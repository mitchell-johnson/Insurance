//! The claims module's seat in the partition.
//!
//! The claims module ([`ic_claims`]) decides; the core posts. This file is
//! the port between them: it answers the cover check from the policy record
//! and turns each claims instruction into one balanced journal entry. The
//! core never judges a claim. It checks that the claim's money is where the
//! instruction says it is, and keeps its own record of each claim's reserve,
//! payable and receivable so the two sides can be reconciled every day.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_claims::port::{CoverFacts, Instruction, Offset, Termination};
use ic_ledger::{Dimensions, Ledger};

use crate::accounts::{self, dims};
use crate::arrears;
use crate::event::{Event, MessageBody, OutboundMessage};
use crate::machine::{CoreState, Ctx, Reject};
use crate::policy::{ClaimLedger, ClaimPayment, ClaimPaymentState, LayerOrigin, LifeKind, Policy, PolicyStatus, UnwindCause};
use crate::postings::PostingSet;
use crate::{cash, group, lifecycle, unwind, LifeId, PartyId, PolicyId};

const MODULE: &str = "claims";

fn outward(e: Reject) -> ic_claims::Reject {
    ic_claims::Reject::new(&e.code, e.message)
}

struct Port<'a, 'b> {
    state: &'a mut CoreState,
    ctx: &'a mut Ctx<'b>,
}

impl ic_claims::Core for Port<'_, '_> {
    fn business_date(&self) -> NaiveDate {
        // The cutoff moves the date before it lets claims catch up.
        self.state.business_date.unwrap_or(self.ctx.date)
    }
    fn cover_facts(&self, policy: &str, cover: &str, life: Option<&str>, event_date: NaiveDate) -> Result<CoverFacts, ic_claims::Reject> {
        cover_facts(self.state, policy, cover, life, event_date).map_err(outward)
    }
    fn execute(&mut self, instruction: Instruction) -> Result<(), ic_claims::Reject> {
        execute(self.state, self.ctx, instruction).map_err(outward)
    }
    fn notify(&mut self, kind: &str, claim: &str, detail: BTreeMap<String, String>) {
        self.ctx.emit(Event::Module {
            module: MODULE.to_string(),
            kind: kind.to_string(),
            subject: claim.to_string(),
            detail,
        });
    }
    fn work(&mut self, units: u32) {
        self.ctx.work += units;
    }
    fn partition(&self) -> u16 {
        self.state.partition
    }
    fn send(&mut self, to_partition: u16, command: ic_claims::Command) -> Result<(), ic_claims::Reject> {
        send(self.state, self.ctx, to_partition, command).map_err(outward)
    }
}

/// Apply one claims command. The module's state is part of the partition's
/// state, so it is taken out while the module works on the rest and always
/// put back.
pub(crate) fn apply(state: &mut CoreState, ctx: &mut Ctx<'_>, command: ic_claims::Command) -> Result<(), Reject> {
    let mut module = std::mem::take(&mut state.claims);
    let index = ctx.index;
    let result = module.apply(&mut Port { state, ctx }, index, command);
    state.claims = module;
    result.map_err(|e| Reject::new(&e.code, e.message))
}

/// Queue a claims command for another partition. It leaves through the
/// outbox in the same step as whatever this command posted, and the relay
/// delivers it at least once as caller `partition`. The receiver must treat
/// a repeat as already done: the message ID stops a repeat only for as long
/// as the receiver keeps its idempotency keys.
///
/// The core does not know how many partitions there are, so the caller
/// names each one. Whoever drives the claims module knows the topology.
pub(crate) fn send(state: &CoreState, ctx: &mut Ctx<'_>, to_partition: ic_ledger::PartitionId, command: ic_claims::Command) -> Result<(), Reject> {
    if to_partition == state.partition {
        return Err(Reject::new("same_partition", "a partition does not send itself a message"));
    }
    let id = format!("claims:{}:{}:{}", state.partition, ctx.index, ctx.messages.len());
    ctx.messages.push(OutboundMessage {
        id,
        to_partition,
        body: MessageBody::Claims(Box::new(command)),
    });
    Ok(())
}

/// Let claims work through what has fallen due: at the cutoff and after each
/// end-of-day chunk.
pub(crate) fn drain_due(state: &mut CoreState, ctx: &mut Ctx<'_>) {
    let today = state.business_date.unwrap_or(ctx.date);
    if state.claims.due_backlog(today) == 0 {
        return;
    }
    let mut module = std::mem::take(&mut state.claims);
    let index = ctx.index;
    module.drain_due(&mut Port { state, ctx }, index);
    state.claims = module;
}

/// Policies with claims work that has fallen due and not been handled. The
/// end-of-day run is not finished until it has visited them.
pub(crate) fn policies_with_work_due(state: &CoreState, today: NaiveDate) -> Vec<PolicyId> {
    let due = state.claims.due_index.iter().take_while(|(d, _)| *d <= today);
    let mut policies: Vec<PolicyId> = due
        .filter_map(|(_, id)| state.claims.claims.get(id))
        .map(|c| PolicyId(c.policy.clone()))
        .collect();
    policies.sort();
    policies.dedup();
    policies.truncate(crate::machine::MAX_EOD_CHUNK);
    policies
}

/// What claims holds for a policy, to travel with it. Open claims, benefit
/// schedules and timers move with the policy; only a claim sitting in a
/// payment run holds it back, until the run is approved or cancelled.
pub(crate) fn moving(state: &CoreState, policy: &Policy) -> Result<ic_claims::Moved, Reject> {
    state.claims.moving(&policy.id.to_string()).map_err(|e| Reject::new(&e.code, e.message))
}

/// How many months in all an income benefit is paid for mental health
/// causes where the chosen option carries the limit: the two years of the
/// AIA Living wording. The products name the feature, not the period.
const MENTAL_HEALTH_LIMIT_MONTHS: u32 = 24;

/// The first day a life is the given age: the birthday, or 1 March for a
/// life born on 29 February in a year without one, as age is counted
/// everywhere else.
fn birthday_at(born: NaiveDate, age: u32) -> Option<NaiveDate> {
    let day = born.checked_add_months(chrono::Months::new(age.checked_mul(12)?))?;
    if ic_product::schedule::age_on(born, day) < age {
        day.succ_opt()
    } else {
        Some(day)
    }
}

fn status_code(status: PolicyStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The cover check: what the policy record says about a cover and a life on
/// the event date, as known now. Facts only.
pub fn cover_facts(state: &CoreState, policy: &str, cover: &str, life: Option<&str>, on: NaiveDate) -> Result<CoverFacts, Reject> {
    let p = state
        .policies
        .get(&PolicyId(policy.to_string()))
        .filter(|p| p.status != PolicyStatus::Transferred)
        .ok_or_else(|| Reject::new("unknown_policy", format!("policy {policy} is not on this partition")))?;
    let product = state.catalog.product(&p.product_hash);
    let life_id = life
        .map(|l| LifeId(l.to_string()))
        .or_else(|| p.lives.keys().next().filter(|_| p.lives.len() == 1).cloned());
    let record = life_id.as_ref().and_then(|l| p.lives.get(l));
    let life_on_policy = record.is_some_and(|r| r.in_force_on(on));
    let life_party = record.and_then(|r| match &r.kind {
        LifeKind::Named { party } => Some(party.0.clone()),
        LifeKind::UnnamedMember { linked_party, .. } => linked_party.as_ref().map(|p| p.0.clone()),
        LifeKind::Dependant { party, .. } => party.as_ref().map(|p| p.0.clone()),
        LifeKind::Pool { .. } => None,
    });
    // Standing on the event date: what is unpaid now was unpaid then. A lapse
    // wrote the unpaid premium off, so for a lapsed policy the question is
    // asked of the receivables as they stood when it lapsed.
    let lapsed = p.lapse.as_ref().filter(|_| p.status == PolicyStatus::Lapsed);
    // On a scheme, what one member owes for cover of their own (or of their
    // dependants) is that member's standing and nobody else's.
    let payer = life_id.as_ref().map(|l| p.payer_of(l));
    let theirs = |r: &&crate::policy::Receivable| r.payer.is_none() || r.payer == payer;
    let unpaid: Vec<&crate::policy::Receivable> = match lapsed {
        Some(l) => l.receivables_before.iter().filter(|r| !r.closed && r.outstanding_minor > 0).collect(),
        None => p.receivables.iter().filter(|r| !r.closed && r.outstanding_minor > 0).filter(theirs).collect(),
    };
    // Premium paid since the event for a period that was due by it: owed on
    // the event date, though nothing is owed on it now.
    let settled_since: Vec<&crate::policy::Receivable> = p
        .receivables
        .iter()
        .filter(|r| !r.closed && r.outstanding_minor == 0 && r.total_minor > 0 && r.due <= on && r.settled_on.is_some_and(|s| s > on))
        .filter(theirs)
        .collect();
    let oldest = unpaid.iter().chain(&settled_since).map(|r| r.due).min();
    let standing = product.and_then(|pr| pr.arrears()).map(|rules| {
        let age = oldest.map_or(0, |due| (on - due).num_days());
        if lapsed.is_some_and(|l| l.lapsed_on <= on) || age > i64::from(rules.lapse_days) {
            "lapsed"
        } else if age <= 0 {
            "paid"
        } else if age <= i64::from(rules.grace_days) || !rules.suspend_after_grace {
            "in_grace"
        } else {
            "suspended"
        }
    });
    // Suspended at the owner's request on the day: not covered, whatever is paid.
    let standing = if p.suspended_on(on) { Some("suspended") } else { standing };
    let policy_live = match p.status {
        PolicyStatus::Lapsed => p.lapse.as_ref().is_some_and(|l| l.lapsed_on > on),
        PolicyStatus::Quoted | PolicyStatus::Proposed => false,
        _ => p.inception <= on,
    };
    let mut facts = CoverFacts {
        policy_status: status_code(p.status),
        standing: standing.map(str::to_string),
        outstanding_premium_minor: unpaid.iter().filter(|r| r.due <= on).map(|r| r.outstanding_minor).sum(),
        premium_paid_since_event_minor: settled_since.iter().map(|r| r.total_minor).sum(),
        life_on_policy,
        life_party,
        policy_year_start: ic_product::schedule::anniversary(p.inception, ic_product::schedule::policy_year(p.inception, on)),
        ends_on_death: p.lives.len() == 1 && p.lives.values().all(|l| matches!(l.kind, LifeKind::Named { .. })) && !p.is_scheme(),
        policy_loan_minor: p.loan.as_ref().map_or(0, crate::loan::Loan::outstanding_minor),
        policyholder: p.policyholder.0.clone(),
        product_code: p.product_code.clone(),
        product_version: p.product_hash.clone(),
        continuation_of: p.continuation_of.as_ref().map(|c| ic_claims::port::Continuation {
            policy: c.policy.to_string(),
            life: c.life.to_string(),
        }),
        ..CoverFacts::default()
    };
    // A claim is registered even where the cover is not on the policy: the
    // facts then say so, and the assessor decides.
    let Some(c) = p.covers.get(cover) else {
        return Ok(facts);
    };
    facts.cover_type = c.cover_type.clone();
    facts.cover_start = Some(c.start);
    facts.definitions = c.definitions.iter().map(|d| format!("{}@{}", d.id, d.version)).collect();
    let mine = |l: &&crate::policy::Layer| life_id.as_ref().is_none_or(|id| &l.life == id);
    let at_event: Vec<&crate::policy::Layer> = c.layers.iter().filter(mine).filter(|l| l.in_force_on(on)).collect();
    // A life given the cover after the policy began has it from its own
    // first layer, and waiting periods run from there.
    if let Some(first) = at_event.iter().map(|l| l.start).min().filter(|_| life_id.is_some()) {
        facts.cover_start = Some(first.max(c.start));
    }
    // The excess level the life held the cover at on the event date.
    facts.premium_waiver = product.and_then(|pr| pr.cover(cover)).and_then(|def| def.premium_waiver);
    facts.shared_counter = product.and_then(|pr| pr.cover(cover)).and_then(|def| def.shared_counter.clone());
    // An income benefit's waiting period and what its wording does in custody.
    let mut own_benefit_months = None;
    if let Some(ic_product::Benefit::MonthlyBenefit {
        waiting_days,
        benefit_months,
        benefit_while_imprisoned,
    }) = product.and_then(|pr| pr.cover(cover)).map(|def| &def.benefit)
    {
        own_benefit_months = Some(*benefit_months).filter(|m| *m > 0);
        facts.waiting_days = *waiting_days;
        facts.benefit_while_imprisoned = match benefit_while_imprisoned {
            ic_product::WhileImprisoned::Suspend => ic_claims::port::Imprisonment::Suspend,
            ic_product::WhileImprisoned::Pay => ic_claims::port::Imprisonment::Pay,
        };
    }
    // A lump sum's survival period, and whether paying it uses the cover up.
    // On total and permanent disablement the product's survival period is the
    // qualifying period (disabled for it, and alive at its end), so claims
    // also takes it as the waiting period before a lump sum may be paid.
    if let Some(ic_product::Benefit::LumpSum { survival_days, ends_cover }) = product.and_then(|pr| pr.cover(cover)).map(|def| &def.benefit) {
        facts.survival_days = *survival_days;
        facts.ends_cover = Some(*ends_cover);
        if c.cover_type == "tpd" {
            facts.waiting_days = *survival_days;
        }
    }
    // The business's peak season on the event date, as its schedule names it.
    facts.peak_months = crate::peak_season::months_on(p, cover, on);
    // The option the life held the cover at on the event date: its excess,
    // and the waiting period an income cover's option sets.
    let chosen = at_event
        .iter()
        .find_map(|l| l.excess_option.as_deref())
        .and_then(|option| product.and_then(|pr| pr.cover(cover)).and_then(|def| def.option(option).ok()));
    facts.chosen_excess_minor = chosen.as_ref().and_then(|o| o.terms.excess_minor);
    if let Some(days) = chosen.as_ref().and_then(|o| o.terms.waiting_days) {
        facts.waiting_days = days;
    }
    // What else the chosen option sets for a claim: how long an income
    // benefit runs, and the limits and indexation its features switch on.
    if let Some(option) = &chosen {
        facts.benefit_months = option.terms.benefit_months;
        let born = record.and_then(|r| r.factors.as_of(on)).and_then(|f| f.date_of_birth);
        facts.benefit_to = option.terms.benefit_to_age.zip(born).and_then(|(age, born)| birthday_at(born, age));
        facts.mental_health_months = option.has_feature("mental-health-limit").then_some(MENTAL_HEALTH_LIMIT_MONTHS);
        facts.claim_indexation = option.has_feature("claim-indexation");
    }
    if let Some(ic_product::Benefit::HealthSchedule { limits }) = product.and_then(|pr| pr.cover(cover)).map(|def| &def.benefit) {
        facts.health_limits = limits
            .iter()
            .map(|l| ic_claims::port::HealthLimit {
                category: l.category.clone(),
                per_event_minor: l.per_event_minor,
                per_year_minor: l.per_year_minor,
                per_lifetime_minor: l.per_lifetime_minor,
                excess_minor: Some(l.excess_minor),
            })
            .collect();
    }
    // Where the option sets no end to the benefit, by months or by age, the
    // cover's own benefit period stands.
    if facts.benefit_months.is_none() && facts.benefit_to.is_none() && chosen.as_ref().is_none_or(|o| o.terms.benefit_to_age.is_none()) {
        facts.benefit_months = own_benefit_months;
    }
    facts.event_cap_minor = product.and_then(|pr| pr.event_cap_minor);
    let mut sum_insured: i64 = at_event.iter().map(|l| l.sum_insured_minor).sum();
    if product.is_some_and(|pr| pr.pays_step_applied_after_event()) {
        // The wording pays an indexation step applied after the event.
        let later = c
            .layers
            .iter()
            .filter(mine)
            .filter(|l| l.origin == LayerOrigin::Indexation && l.start > on && l.end.is_none_or(|e| e > l.start));
        sum_insured += later.map(|l| l.sum_insured_minor).sum::<i64>();
    }
    // Cover an earlier claim took off by the event date is still counted:
    // the owner of the claim takes earlier admissions off itself.
    sum_insured += p
        .claims
        .values()
        .flat_map(|k| &k.cuts)
        .filter(|x| x.cover == cover && x.on <= on && life_id.as_ref().is_none_or(|id| &x.life == id))
        .map(|x| x.amount_minor)
        .sum::<i64>();
    if let Some(pr) = product {
        if pr.cover(cover).is_some_and(|d| d.joint_first_event) {
            let mut lives: Vec<String> = c.layers.iter().map(|l| l.life.to_string()).collect();
            lives.sort();
            lives.dedup();
            facts.joint_lives = lives;
        }
        facts.reduced_by = pr
            .covers
            .iter()
            .filter(|d| d.reduces.iter().any(|r| r == cover))
            .map(|d| d.code.clone())
            .collect();
    }
    let mut covered = !at_event.is_empty();
    if c.formula_driven {
        // Scheme cover moves with the member's details as they were on the date.
        let factors = record.filter(|_| life_on_policy).and_then(|r| r.factors.as_of(on));
        let amount = match (product, life_id.as_ref(), factors) {
            (Some(pr), Some(id), Some(f)) => lifecycle::member_covers_on(pr, p, id, f, Some(on))
                .into_iter()
                .find(|m| m.cover == cover)
                .map(|m| m.sum_insured_minor),
            _ => None,
        };
        // Cover above the automatic amount is paid only where underwriting
        // accepted it and it was written as its own layer.
        let excess: i64 = at_event
            .iter()
            .filter(|l| l.origin == LayerOrigin::UnderwrittenExcess)
            .map(|l| l.sum_insured_minor)
            .sum();
        sum_insured = amount.map_or(0, |a| a + excess);
        covered = amount.is_some();
    }
    let mut exclusions: Vec<String> = at_event.iter().flat_map(|l| l.exclusions.iter().cloned()).collect();
    exclusions.sort();
    exclusions.dedup();
    facts.exclusions = exclusions;
    facts.sum_insured_minor = sum_insured;
    // The benefits built into the cover, each with its limit against the
    // sum insured on the event date and whether the chosen option has it.
    if let Some(def) = product.and_then(|pr| pr.cover(cover)) {
        facts.built_in = def
            .built_in
            .iter()
            .map(|b| ic_claims::port::BuiltIn {
                code: b.code.clone(),
                kind: serde_json::to_value(b.kind)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default(),
                limit_minor: b.limit_minor(sum_insured),
                applies: b.applies(chosen.as_ref()),
            })
            .collect();
    }
    facts.in_force = covered && c.start <= on && policy_live && !matches!(standing, Some("suspended" | "lapsed"));
    // The same question as the record stood on the event date: a leave
    // date or a change of details recorded since is left out. Given only
    // where the answer differs from today's.
    if let (Some(r), Some(id)) = (record, life_id.as_ref()) {
        let left_then = r.left.filter(|_| r.left_recorded_on.is_none_or(|recorded| recorded <= on));
        // A migrated policy was not on this record before it was loaded: what it
        // was loaded with stands for what was known.
        let factors_then = r.factors.as_known(on, on).or_else(|| r.factors.as_of(on).filter(|_| p.migrated));
        let on_policy_then = factors_then.is_some() && r.joined <= on && left_then.is_none_or(|l| l > on);
        // Layers the later leave ended were still running as far as anyone knew.
        let ended_since = |l: &&crate::policy::Layer| left_then.is_none() && r.left.is_some() && l.end == r.left.map(|left| left.max(l.start));
        let layers_then: Vec<&crate::policy::Layer> = c
            .layers
            .iter()
            .filter(|l| &l.life == id && l.start <= on && (l.end.is_none_or(|e| e > on) || ended_since(l)))
            .collect();
        let (sum_then, covered_then) = if c.formula_driven {
            let amount = match (product, factors_then.filter(|_| on_policy_then)) {
                (Some(pr), Some(f)) => lifecycle::member_covers_on(pr, p, id, f, Some(on))
                    .into_iter()
                    .find(|m| m.cover == cover)
                    .map(|m| m.sum_insured_minor),
                _ => None,
            };
            let excess: i64 = layers_then
                .iter()
                .filter(|l| l.origin == LayerOrigin::UnderwrittenExcess)
                .map(|l| l.sum_insured_minor)
                .sum();
            (amount.map_or(0, |a| a + excess), amount.is_some())
        } else {
            let now: i64 = at_event.iter().map(|l| l.sum_insured_minor).sum();
            let then: i64 = layers_then.iter().map(|l| l.sum_insured_minor).sum();
            (sum_insured - now + then, !layers_then.is_empty())
        };
        let then = ic_claims::port::KnownThen {
            in_force: covered_then && c.start <= on && policy_live && !matches!(standing, Some("suspended" | "lapsed")),
            life_on_policy: on_policy_then,
            sum_insured_minor: sum_then,
        };
        if (then.in_force, then.life_on_policy, then.sum_insured_minor) != (facts.in_force, facts.life_on_policy, facts.sum_insured_minor) {
            facts.known_then = Some(then);
        }
    }
    // Whoever holds an assignment standing on the event date is paid first.
    let standing_interests = crate::interest::ranked(p, on).into_iter().map(|i| ic_claims::port::Interest {
        party: i.party.to_string(),
        extent_minor: i.extent_minor,
        priority: i.priority,
        insolvency: i.kind == crate::interest::InterestKind::Insolvency,
    });
    facts.interests = standing_interests.collect();
    // Then whoever was nominated on the event date, as the record knows it now.
    facts.nominations = crate::nomination::nominees(p, life_id.as_ref(), on)
        .iter()
        .map(|n| ic_claims::port::Nominee {
            party: n.party.to_string(),
            share_pct: n.share_pct,
        })
        .collect();
    Ok(facts)
}

fn execute(state: &mut CoreState, ctx: &mut Ctx<'_>, instruction: Instruction) -> Result<(), Reject> {
    let id = PolicyId(instruction.policy().to_string());
    state.with_policy_tracked(ctx, &id, true, |s, ctx, p| match instruction {
        Instruction::SetReserve {
            claim,
            cover,
            amount_minor,
            reason,
            ..
        } => set_reserve(&mut s.ledger, p, ctx, &claim, cover, amount_minor, reason),
        Instruction::Admit {
            claim,
            cover,
            amount_minor,
            release_reserve,
            deduct_premium_minor,
            repay_loan_minor,
            terminate,
            life,
            ..
        } => {
            let ends = terminate.is_some();
            let deductions = (deduct_premium_minor, repay_loan_minor);
            admit(s, p, ctx, &claim, cover.clone(), amount_minor, release_reserve, deductions, terminate)?;
            // A policy the claim ended has no cover left to reduce.
            match life.filter(|_| !ends) {
                Some(life) => {
                    let total = p.claims.get(&claim).map_or(0, |c| c.admitted_minor);
                    crate::reduce::for_claim(&mut s.ledger, &s.catalog, p, ctx, &claim, &cover, &LifeId(life), total)
                }
                None => Ok(()),
            }
        }
        Instruction::Pay {
            claim,
            payment_ref,
            amount_minor,
            payee,
            close,
            interest_minor,
            interest_declined_minor,
            interest_withheld_minor,
            offset,
            ..
        } => pay(
            s,
            p,
            ctx,
            &claim,
            payment_ref,
            amount_minor,
            payee,
            close,
            (interest_minor, interest_declined_minor, interest_withheld_minor),
            offset,
        ),
        Instruction::PayBenefit {
            claim,
            payment_ref,
            period_start,
            period_end,
            gross_minor,
            offset,
            payee,
            close,
            tax_withheld_minor,
            ..
        } => pay_benefit(
            s,
            p,
            ctx,
            &claim,
            payment_ref,
            (period_start, period_end),
            (gross_minor, tax_withheld_minor),
            offset,
            payee,
            close,
        ),
        Instruction::PaymentReturned { claim, payment_ref, .. } => payment_returned(p, &claim, &payment_ref),
        Instruction::ExGratia {
            claim,
            cover,
            payment_ref,
            amount_minor,
            payee,
            ..
        } => goodwill_or_cost(&mut s.ledger, p, ctx, &claim, &cover, payment_ref, amount_minor, payee, None),
        Instruction::HandlingCost {
            claim,
            cover,
            payment_ref,
            amount_minor,
            payee,
            kind,
            ..
        } => goodwill_or_cost(&mut s.ledger, p, ctx, &claim, &cover, payment_ref, amount_minor, payee, Some(kind)),
        Instruction::OpenMigrated {
            claim,
            cover,
            reserve_minor,
            admitted_minor,
            payable_minor,
            receivable_minor,
            open,
            ..
        } => open_migrated(
            &mut s.ledger,
            p,
            ctx,
            &claim,
            cover,
            [reserve_minor, admitted_minor, payable_minor, receivable_minor],
            open,
        ),
        Instruction::WaivePremium {
            claim,
            cover,
            only_cover,
            from,
            until,
            ..
        } => waive(s, p, ctx, &claim, &cover, only_cover, from, until),
        Instruction::UnwaivePremium { claim, cover, from, .. } => unwaive(s, p, ctx, &claim, &cover, from),
        Instruction::Close {
            claim, outcome, overpayment, ..
        } => {
            let reversed = outcome == "admission_reversed";
            close(&mut s.ledger, p, ctx, &claim, outcome, overpayment)?;
            if reversed {
                crate::reduce::restore_for_claim(&mut s.ledger, &s.catalog, p, ctx, &claim)?;
            }
            Ok(())
        }
        Instruction::Reopen { claim, reserve_minor, .. } => reopen(&mut s.ledger, p, ctx, &claim, reserve_minor),
        Instruction::RaiseOverpayment {
            claim,
            overpayment,
            amount_minor,
            ..
        } => move_receivable(&mut s.ledger, p, ctx, &claim, "claim_overpayment_raised", overpayment, amount_minor),
        Instruction::RecoverOverpayment {
            claim,
            overpayment,
            amount_minor,
            ..
        } => move_receivable(&mut s.ledger, p, ctx, &claim, "claim_overpayment_recovered", overpayment, amount_minor),
        Instruction::ReceiveOffset {
            claim,
            overpayment,
            amount_minor,
            ..
        } => move_receivable(&mut s.ledger, p, ctx, &claim, "claim_offset_received", overpayment, amount_minor),
        Instruction::ReturnOffset { claim, amount_minor, .. } => return_offset(&mut s.ledger, p, ctx, &claim, amount_minor),
        Instruction::WriteOffOverpayment {
            claim,
            overpayment,
            amount_minor,
            ..
        } => move_receivable(&mut s.ledger, p, ctx, &claim, "claim_overpayment_written_off", overpayment, amount_minor),
        Instruction::IdentifyMember { life, party, .. } => group::identify(p, ctx, &LifeId(life), PartyId(party)),
    })
}

fn known<'a>(policy: &'a Policy, claim: &str) -> Result<&'a ClaimLedger, Reject> {
    policy
        .claims
        .get(claim)
        .ok_or_else(|| Reject::new("unknown_claim", format!("claim {claim} has no ledger record on policy {}", policy.id)))
}

fn invalid<T>(message: &str) -> Result<T, Reject> {
    Err(Reject::new("invalid_amount", message))
}

/// The expense account and dimensions a claim on this cover posts to.
fn incurred(policy: &Policy, cover: &str, posting_type: &str) -> (ic_ledger::Account, Dimensions) {
    let cover_type = policy.covers.get(cover).map_or("unknown", |c| c.cover_type.as_str());
    (
        accounts::claims_incurred(&policy.product_code, cover_type, policy.currency),
        dims(policy, Some(cover_type), posting_type),
    )
}

/// Post a claim's entry. Every entry names its claim; none carries a cause.
fn post(ledger: &mut Ledger, policy: &Policy, ctx: &mut Ctx<'_>, kind: &str, set: PostingSet, claim: &str, extra: &[(&str, String)]) -> Result<(), Reject> {
    let postings = set.into_postings(ledger)?;
    if postings.is_empty() {
        return Ok(());
    }
    let mut detail: BTreeMap<String, String> = extra.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    detail.insert("claim".to_string(), claim.to_string());
    ctx.post(ledger, kind, Some(policy), ctx.date, postings, detail)
}

/// Set the case reserve to an amount: the first estimate or an adjustment.
fn set_reserve(ledger: &mut Ledger, policy: &mut Policy, ctx: &mut Ctx<'_>, claim: &str, cover: String, amount: i64, reason: String) -> Result<(), Reject> {
    let existing = policy.claims.get(claim);
    if amount < 0 || existing.is_some_and(|c| !c.open) {
        return invalid("a reserve is never negative and is set on an open claim");
    }
    let c = policy.currency;
    let delta = i128::from(amount - existing.map_or(0, |c| c.reserve_minor));
    let (expense, d) = incurred(policy, &cover, "claim_reserve");
    let mut set = PostingSet::new();
    set.debit(expense, delta, d.clone());
    set.credit(accounts::claim_reserve(&policy.id, c), delta, d);
    post(ledger, policy, ctx, "claim_reserve_set", set, claim, &[("reason", reason)])?;
    let record = policy.claims.entry(claim.to_string()).or_insert_with(|| ClaimLedger {
        cover,
        open: true,
        ..ClaimLedger::default()
    });
    record.reserve_minor = amount;
    Ok(())
}

/// Admit: the reserve moves to benefit payable, with the difference from the
/// estimate going to claims expense. Premium owing is taken from the claim,
/// and a death claim ends the policy as at the event.
#[allow(clippy::too_many_arguments)]
fn admit(
    state: &mut CoreState,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    claim: &str,
    cover: String,
    amount: i64,
    release_reserve: bool,
    (deduct, repay_loan): (i64, i64),
    terminate: Option<Termination>,
) -> Result<(), Reject> {
    let existing = policy.claims.get(claim);
    let taken = deduct.checked_add(repay_loan).filter(|_| deduct >= 0 && repay_loan >= 0);
    if amount <= 0 || !taken.is_some_and(|t| t <= amount) || existing.is_some_and(|c| !c.open) {
        return invalid("an admission is a positive amount on an open claim, less no more premium and loan than it pays");
    }
    if repay_loan > policy.loan.as_ref().map_or(0, crate::loan::Loan::outstanding_minor) {
        return Err(Reject::new("loan_not_owing", "the policy does not owe that much on a loan"));
    }
    if deduct > policy.outstanding_minor() {
        return Err(Reject::new("premium_not_owing", "the policy does not owe that much premium"));
    }
    if let Some(t) = &terminate {
        // What the termination will check, checked before anything is posted.
        let held_elsewhere = policy.unwind.as_ref().is_some_and(|h| h.after != t.event_date || h.cause != UnwindCause::Claim);
        if policy.status != PolicyStatus::InForce || t.event_date > ctx.date || t.event_date < policy.inception || held_elsewhere {
            return Err(Reject::new("cannot_terminate", "the policy cannot be ended by this claim as at the event date"));
        }
    }
    let c = policy.currency;
    let released = if release_reserve { existing.map_or(0, |c| c.reserve_minor) } else { 0 };
    let (expense, d) = incurred(policy, &cover, "claim_admitted");
    let mut set = PostingSet::new();
    set.debit(accounts::claim_reserve(&policy.id, c), i128::from(released), d.clone());
    set.debit(expense, i128::from(amount - released), d.clone());
    set.credit(accounts::benefit_payable(&policy.id, c), i128::from(amount - deduct), d);
    set.credit(accounts::policy_suspense(&policy.id, c), i128::from(deduct), Dimensions::default());
    post(
        &mut state.ledger,
        policy,
        ctx,
        "claim_admitted",
        set,
        claim,
        &[("deducted_premium_minor", deduct.to_string())],
    )?;
    let record = policy.claims.entry(claim.to_string()).or_insert_with(|| ClaimLedger {
        cover,
        open: true,
        ..ClaimLedger::default()
    });
    record.reserve_minor -= released;
    record.admitted_minor += amount;
    record.payable_minor += amount - deduct - repay_loan;
    // The loan is met out of what the claim would have paid.
    let payable = accounts::benefit_payable(&policy.id, c);
    crate::loan::deduct(&mut state.ledger, policy, ctx, payable, repay_loan, "claim", claim)?;
    if deduct > 0 {
        cash::apply_suspense(&mut state.ledger, policy, ctx)?;
        if let Some(product) = state.catalog.product(&policy.product_hash) {
            arrears::refresh(product, policy, ctx);
        }
    }
    if let Some(t) = terminate {
        unwind::terminate_on_claim(&mut state.ledger, &state.catalog, policy, ctx, t.event_date, claim, t.payer_deceased)?;
    }
    Ok(())
}

/// Record a payment against a claim that the payments module is to make.
fn instruct(policy: &mut Policy, claim: &str, payment_ref: String, amount: i64, ex_gratia: bool) {
    if let Some(record) = policy.claims.get_mut(claim) {
        record.instructed_minor += amount;
        record.payments.insert(
            payment_ref,
            ClaimPayment {
                amount_minor: amount,
                ex_gratia,
                state: ClaimPaymentState::Instructed,
            },
        );
    }
}

/// Release a payment from what has been admitted. The money stays in
/// benefit payable until the payments module settles it at the bank. On the
/// last payment the claim closes and any reserve left is released.
#[allow(clippy::too_many_arguments)]
fn pay(
    state: &mut CoreState,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    claim: &str,
    payment_ref: String,
    amount: i64,
    payee: String,
    close: bool,
    (interest, declined, withheld): (i64, i64, i64),
    offset: Option<Offset>,
) -> Result<(), Reject> {
    let record = known(policy, claim)?;
    if !(0..=(interest - declined).max(0)).contains(&withheld) {
        return invalid("tax is withheld from the interest paid and is no more than it");
    }
    if !(0..=interest.max(0)).contains(&declined) {
        return invalid("interest declined is no more than the interest owed");
    }
    let free = record.payable_minor - record.instructed_minor;
    let taken = offset.as_ref().map_or(0, |o| o.amount_minor);
    if interest < 0 || amount <= 0 || amount > free || !(0..=amount).contains(&taken) || record.payments.contains_key(&payment_ref) || (close && amount != free)
    {
        return invalid("a payment is new, positive and within what is admitted and not yet released, with an offset no larger than it");
    }
    let c = policy.currency;
    let other = offset_policy(state, policy, offset.as_ref())?;
    let reserve = if close { record.reserve_minor } else { 0 };
    let (expense, d) = incurred(policy, &record.cover, "claim_closed");
    let mut set = PostingSet::new();
    set.debit(accounts::claim_reserve(&policy.id, c), i128::from(reserve), d.clone());
    set.credit(expense, i128::from(reserve), d);
    // Interest for late payment is its own expense, owed with the payment
    // and by value date: the date of this entry.
    let late = dims(policy, None, "claim_late_interest");
    set.debit(accounts::claim_interest(&policy.product_code, c), i128::from(interest), late.clone());
    set.credit(accounts::benefit_payable(&policy.id, c), i128::from(interest), late);
    // What is kept back is no longer owed to the payee: it pays down what
    // the payee owes on another claim.
    if let Some(o) = &offset {
        let kept = dims(policy, None, "claim_offset");
        set.debit(accounts::benefit_payable(&policy.id, c), i128::from(taken), kept.clone());
        set.credit(accounts::claim_recovery_receivable(&PolicyId(o.policy.clone()), c), i128::from(taken), kept);
    }
    let kind = match (interest > 0, taken > 0) {
        (true, _) => "claim_late_interest",
        (false, true) => "claim_offset",
        (false, false) => "claim_reserve_released",
    };
    post(
        &mut state.ledger,
        policy,
        ctx,
        kind,
        set,
        claim,
        &[("payment_ref", payment_ref.clone()), ("payee", payee), ("interest_minor", interest.to_string())],
    )?;
    // A payee who was told the interest and declined it: the interest is
    // reversed by its own entry, so the decline stays visible.
    if declined > 0 {
        let reason = dims(policy, None, "claim_late_interest_declined");
        let mut set = PostingSet::new();
        set.debit(accounts::benefit_payable(&policy.id, c), i128::from(declined), reason.clone());
        set.credit(accounts::claim_interest(&policy.product_code, c), i128::from(declined), reason);
        post(
            &mut state.ledger,
            policy,
            ctx,
            "claim_late_interest_declined",
            set,
            claim,
            &[("payment_ref", payment_ref.clone()), ("reason", "declined_by_payee".to_string())],
        )?;
    }
    withhold(state, policy, ctx, claim, &payment_ref, "rwt", withheld)?;
    if let Some(record) = policy.claims.get_mut(claim) {
        record.reserve_minor -= reserve;
        record.payable_minor += interest - declined - taken - withheld;
        record.open = !close;
    }
    instruct(policy, claim, payment_ref, amount - taken + interest - declined - withheld, false);
    reduce_receivable(state, policy, other, offset.as_ref());
    Ok(())
}

/// Tax kept back from a payment: out of benefit payable, owed to the revenue
/// authority, in its own entry under its own posting type so the tax
/// reconciliation can agree the payable to what was withheld.
fn withhold(state: &mut CoreState, policy: &mut Policy, ctx: &mut Ctx<'_>, claim: &str, payment_ref: &str, code: &str, amount: i64) -> Result<(), Reject> {
    if amount == 0 {
        return Ok(());
    }
    let c = policy.currency;
    let d = dims(policy, None, &format!("withholding_{code}"));
    let mut set = PostingSet::new();
    set.debit(accounts::benefit_payable(&policy.id, c), i128::from(amount), d.clone());
    set.credit(accounts::withholding_tax_payable(code, c), i128::from(amount), d);
    let detail = [("payment_ref", payment_ref.to_string()), ("tax", code.to_string())];
    post(&mut state.ledger, policy, ctx, "tax_withheld", set, claim, &detail)
}

/// Where the overpayment an offset recovers is held, when it is on another
/// policy of this partition, after checking it has that much outstanding.
fn offset_policy(state: &CoreState, policy: &Policy, offset: Option<&Offset>) -> Result<Option<PolicyId>, Reject> {
    let Some(o) = offset else { return Ok(None) };
    let other = Some(PolicyId(o.policy.clone())).filter(|id| *id != policy.id);
    let owed = match &other {
        Some(id) => state
            .policies
            .get(id)
            .filter(|p| p.currency == policy.currency && !p.is_frozen())
            .and_then(|p| p.claims.get(&o.claim)),
        None => policy.claims.get(&o.claim),
    };
    if owed.is_none_or(|l| l.receivable_minor < o.amount_minor) {
        return Err(Reject::new("offset", "the overpayment does not have that much outstanding on this partition"));
    }
    Ok(other)
}

/// Count an offset against the receivable it recovers.
fn reduce_receivable(state: &mut CoreState, policy: &mut Policy, other: Option<PolicyId>, offset: Option<&Offset>) {
    let Some(o) = offset else { return };
    let owing = match other {
        Some(id) => state.policies.get_mut(&id).and_then(|p| p.claims.get_mut(&o.claim)),
        None => policy.claims.get_mut(&o.claim),
    };
    if let Some(l) = owing {
        l.receivable_minor -= o.amount_minor;
    }
}

/// One period of a benefit in payment, in one entry: drawn from the reserve
/// as far as it goes and from claims expense beyond it, owed to the claimant
/// less any offset, which reduces the overpayment it recovers.
#[allow(clippy::too_many_arguments)]
fn pay_benefit(
    state: &mut CoreState,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    claim: &str,
    payment_ref: String,
    period: (NaiveDate, NaiveDate),
    (gross, withheld): (i64, i64),
    offset: Option<Offset>,
    payee: String,
    close: bool,
) -> Result<(), Reject> {
    let record = known(policy, claim)?;
    let taken = offset.as_ref().map_or(0, |o| o.amount_minor);
    if gross < 0 || !(0..=gross).contains(&taken) || !record.open || record.payments.contains_key(&payment_ref) {
        return invalid("a benefit period is paid once, on an open claim, with an offset no larger than the benefit");
    }
    if !(0..=gross - taken).contains(&withheld) {
        return invalid("tax is withheld from the benefit paid and is no more than it");
    }
    let c = policy.currency;
    // The overpayment an offset recovers may be on another policy of the
    // same person on this partition, or on another partition: then the
    // money goes to offset in transit and that partition takes it up.
    let remote = offset.as_ref().is_some_and(|o| o.partition.is_some_and(|p| p != state.partition));
    let other = if remote { None } else { offset_policy(state, policy, offset.as_ref())? };
    let drawn = record.reserve_minor.min(gross);
    let released = if close { record.reserve_minor - drawn } else { 0 };
    let (expense, d) = incurred(policy, &record.cover, "benefit_paid");
    let mut set = PostingSet::new();
    set.debit(accounts::claim_reserve(&policy.id, c), i128::from(drawn + released), d.clone());
    set.debit(expense, i128::from(gross - drawn - released), d.clone());
    set.credit(accounts::benefit_payable(&policy.id, c), i128::from(gross - taken), d.clone());
    match &offset {
        Some(_) if remote => set.credit(accounts::claims_offset_in_transit(c), i128::from(taken), Dimensions::default()),
        Some(o) => set.credit(accounts::claim_recovery_receivable(&PolicyId(o.policy.clone()), c), i128::from(taken), d),
        None => {}
    }
    let extra = [
        ("payment_ref", payment_ref.clone()),
        ("payee", payee),
        ("period_start", period.0.to_string()),
        ("period_end", period.1.to_string()),
    ];
    post(&mut state.ledger, policy, ctx, "benefit_paid", set, claim, &extra)?;
    withhold(state, policy, ctx, claim, &payment_ref, "benefit", withheld)?;
    if let Some(record) = policy.claims.get_mut(claim) {
        record.reserve_minor -= drawn + released;
        record.payable_minor += gross - taken - withheld;
        record.open = !close;
    }
    instruct(policy, claim, payment_ref, gross - taken - withheld, false);
    if !remote {
        reduce_receivable(state, policy, other, offset.as_ref());
    }
    Ok(())
}

/// A payment failed before the bank settled it, or the bank sent it back
/// (which the core has by then posted through `ReturnPayment`). Either way
/// the money is in benefit payable, owed and no longer with the bank.
fn payment_returned(policy: &mut Policy, claim: &str, payment_ref: &str) -> Result<(), Reject> {
    known(policy, claim)?;
    let record = policy.claims.get_mut(claim).filter(|r| r.payments.contains_key(payment_ref));
    let Some((record, payment)) = record.and_then(|r| {
        let p = r.payments.get(payment_ref)?.clone();
        Some((r, p))
    }) else {
        return Err(Reject::new("unknown_payment", format!("claim {claim} has no payment {payment_ref}")));
    };
    match payment.state {
        ClaimPaymentState::Instructed => {
            record.instructed_minor -= payment.amount_minor;
            record.open = true;
            if let Some(p) = record.payments.get_mut(payment_ref) {
                p.state = ClaimPaymentState::Returned;
            }
            Ok(())
        }
        // The bank's return already put it back.
        ClaimPaymentState::Returned => Ok(()),
        ClaimPaymentState::Settled => Err(Reject::new(
            "payment_settled",
            "the bank settled this payment; a return comes through the payments module",
        )),
    }
}

/// A goodwill payment, or with `cost` a cost of handling the claim owed to a
/// third party. Each has its own posting type and its own expense account,
/// and neither is ever mixed with what is paid under the contract.
#[allow(clippy::too_many_arguments)]
fn goodwill_or_cost(
    ledger: &mut Ledger,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    claim: &str,
    cover: &str,
    payment_ref: String,
    amount: i64,
    payee: String,
    cost: Option<String>,
) -> Result<(), Reject> {
    let record = known(policy, claim)?;
    if amount <= 0 || record.payments.contains_key(&payment_ref) {
        return invalid("an ex gratia or handling cost payment is new and positive");
    }
    let c = policy.currency;
    let (entry, expense) = match &cost {
        None => ("claim_ex_gratia", accounts::ex_gratia(&policy.product_code, c)),
        Some(_) => ("claim_handling_cost", accounts::claims_handling(&policy.product_code, c)),
    };
    let (_, d) = incurred(policy, cover, cost.as_deref().unwrap_or("ex_gratia"));
    let mut set = PostingSet::new();
    set.debit(expense, i128::from(amount), d.clone());
    set.credit(accounts::benefit_payable(&policy.id, c), i128::from(amount), d);
    post(
        ledger,
        policy,
        ctx,
        entry,
        set,
        claim,
        &[("payment_ref", payment_ref.clone()), ("payee", payee)],
    )?;
    if let Some(record) = policy.claims.get_mut(claim) {
        record.payable_minor += amount;
    }
    instruct(policy, claim, payment_ref, amount, cost.is_none());
    Ok(())
}

/// A claim loaded from another system. Nothing is expensed here: the cost
/// was recognised there. The reserve, the unpaid admitted money and the
/// overpayment are opened against the migration control account.
fn open_migrated(ledger: &mut Ledger, policy: &mut Policy, ctx: &mut Ctx<'_>, claim: &str, cover: String, money: [i64; 4], open: bool) -> Result<(), Reject> {
    let [reserve, admitted, payable, receivable] = money;
    if policy.claims.contains_key(claim) || money.iter().any(|m| *m < 0) {
        return invalid("a migrated claim is new to the policy and its amounts are not negative");
    }
    let c = policy.currency;
    let d = dims(policy, policy.covers.get(&cover).map(|c| c.cover_type.as_str()), "claim_migrated");
    let control = accounts::claims_migration_control(c);
    let mut set = PostingSet::new();
    set.debit(control.clone(), i128::from(reserve + payable), d.clone());
    set.credit(accounts::claim_reserve(&policy.id, c), i128::from(reserve), d.clone());
    set.credit(accounts::benefit_payable(&policy.id, c), i128::from(payable), d.clone());
    set.debit(accounts::claim_recovery_receivable(&policy.id, c), i128::from(receivable), d.clone());
    set.credit(control, i128::from(receivable), d);
    post(ledger, policy, ctx, "claim_migrated", set, claim, &[])?;
    policy.claims.insert(
        claim.to_string(),
        ClaimLedger {
            cover,
            open,
            reserve_minor: reserve,
            admitted_minor: admitted,
            payable_minor: payable,
            receivable_minor: receivable,
            ..ClaimLedger::default()
        },
    );
    Ok(())
}

/// Premium waiver: the premium was billed as usual, and what is owing for
/// periods starting in the window is paid from claims expense, so earned
/// premium and claims cost are both recognised. Older arrears are not
/// touched, and nothing is posted where nothing is owing.
/// What a waiver pays on each receivable in its window: all that is owing,
/// or, where it waives only its own cover's premium, that cover's share of
/// each premium (by what the cover earns in it, GST and levies alike) less
/// what this claim has already waived on it, never more than is owing.
fn to_waive(policy: &Policy, claim: &str, cover: &str, only_cover: bool, from: NaiveDate, until: NaiveDate) -> BTreeMap<u32, i64> {
    let record = policy.claims.get(claim);
    let cover_type = policy.covers.get(cover).map(|c| c.cover_type.as_str());
    policy
        .receivables
        .iter()
        .filter(|r| r.outstanding_minor > 0 && !r.closed && r.period_start >= from && r.period_start < until)
        .filter_map(|r| {
            if !only_cover {
                return Some((r.id, r.outstanding_minor));
            }
            let all: i128 = r.net_fine.values().sum();
            let mine = cover_type.and_then(|t| r.net_fine.get(t)).copied().unwrap_or(0);
            if all <= 0 || mine <= 0 {
                return None;
            }
            let share = i64::try_from(crate::postings::div_half_even(i128::from(r.total_minor) * mine, all)).unwrap_or(0);
            let already = record.and_then(|k| k.waived.get(&r.id)).copied().unwrap_or(0);
            Some((r.id, (share - already).clamp(0, r.outstanding_minor)))
        })
        .filter(|(_, amount)| *amount > 0)
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn waive(
    state: &mut CoreState,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    claim: &str,
    cover: &str,
    only_cover: bool,
    from: NaiveDate,
    until: NaiveDate,
) -> Result<(), Reject> {
    known(policy, claim)?;
    // The part of what is waived that pays for cover, by cover type, as a
    // receipt reports it: commission agreements may earn on waived premium.
    let waived = to_waive(policy, claim, cover, only_cover, from, until);
    let total: i64 = waived.values().sum();
    if total == 0 || policy.status != PolicyStatus::InForce {
        return Ok(());
    }
    let net = crate::counterparty::net_by_cover_type(&policy.receivables, &waived);
    let c = policy.currency;
    let (expense, d) = incurred(policy, cover, "premium_waiver");
    let mut set = PostingSet::new();
    set.debit(expense, i128::from(total), d.clone());
    set.credit(accounts::premium_receivable(&policy.id, c), i128::from(total), d);
    post(
        &mut state.ledger,
        policy,
        ctx,
        "premium_waived",
        set,
        claim,
        &[("amount_minor", total.to_string())],
    )?;
    for r in policy.receivables.iter_mut() {
        if let Some(amount) = waived.get(&r.id) {
            r.outstanding_minor -= amount;
        }
    }
    if let Some(record) = policy.claims.get_mut(claim) {
        record.waived_minor += total;
        for (id, amount) in &waived {
            *record.waived.entry(*id).or_default() += amount;
        }
    }
    ctx.emit(Event::Module {
        module: MODULE.to_string(),
        kind: "premium_waived".to_string(),
        subject: claim.to_string(),
        detail: [("policy".to_string(), policy.id.to_string()), ("amount_minor".to_string(), total.to_string())]
            .into_iter()
            .chain(net.into_iter().map(|(cover_type, amount)| (format!("net:{cover_type}"), amount.to_string())))
            .collect(),
    });
    if let Some(product) = state.catalog.product(&policy.product_hash) {
        arrears::refresh(product, policy, ctx);
    }
    Ok(())
}

/// Bill back what a claim's waiver paid for periods starting on or after a
/// date: the reverse of the waiver's entry, with the receivables outstanding
/// again and the policy's standing worked out afresh. Asked twice, the
/// second finds nothing left to bill back.
fn unwaive(state: &mut CoreState, policy: &mut Policy, ctx: &mut Ctx<'_>, claim: &str, cover: &str, from: NaiveDate) -> Result<(), Reject> {
    let record = known(policy, claim)?;
    let back: BTreeMap<u32, i64> = record
        .waived
        .iter()
        .filter(|(id, amount)| **amount > 0 && policy.receivables.iter().any(|r| r.id == **id && r.period_start >= from))
        .map(|(id, amount)| (*id, *amount))
        .collect();
    let total: i64 = back.values().sum();
    if total == 0 {
        return Ok(());
    }
    let net = crate::counterparty::net_by_cover_type(&policy.receivables, &back);
    let c = policy.currency;
    let (expense, d) = incurred(policy, cover, "premium_waiver_reversed");
    let mut set = PostingSet::new();
    set.debit(accounts::premium_receivable(&policy.id, c), i128::from(total), d.clone());
    set.credit(expense, i128::from(total), d);
    post(
        &mut state.ledger,
        policy,
        ctx,
        "premium_waiver_reversed",
        set,
        claim,
        &[("amount_minor", total.to_string()), ("from", from.to_string())],
    )?;
    for r in policy.receivables.iter_mut() {
        if let Some(amount) = back.get(&r.id) {
            r.outstanding_minor += amount;
        }
    }
    if let Some(record) = policy.claims.get_mut(claim) {
        record.waived_minor -= total;
        record.waived.retain(|id, _| !back.contains_key(id));
    }
    ctx.emit(Event::Module {
        module: MODULE.to_string(),
        kind: "premium_waiver_reversed".to_string(),
        subject: claim.to_string(),
        detail: [("policy".to_string(), policy.id.to_string()), ("amount_minor".to_string(), total.to_string())]
            .into_iter()
            .chain(net.into_iter().map(|(cover_type, amount)| (format!("net:{cover_type}"), amount.to_string())))
            .collect(),
    });
    if let Some(product) = state.catalog.product(&policy.product_hash) {
        arrears::refresh(product, policy, ctx);
    }
    Ok(())
}

/// Before the payments module settles a benefit payment: it must be one
/// claims released, for exactly that amount. Where the policy does not owe
/// the money at all, the settlement's own check says so.
pub(crate) fn check_settlement(ledger: &Ledger, policy: &Policy, payment: &str, amount: i64) -> Result<(), Reject> {
    let instructed = policy
        .claims
        .values()
        .filter_map(|c| c.payments.get(payment))
        .any(|p| p.state == ClaimPaymentState::Instructed && p.amount_minor == amount);
    let owed = ledger.available(&accounts::benefit_payable(&policy.id, policy.currency).id) >= i128::from(amount);
    if owed && !instructed {
        return Err(Reject::new(
            "payment_not_instructed",
            format!("no claim has released a payment {payment} of that amount"),
        ));
    }
    Ok(())
}

/// The bank settled a benefit payment: the claim no longer owes it.
pub(crate) fn settled(policy: &mut Policy, payment: &str) {
    for record in policy.claims.values_mut() {
        if let Some(p) = record.payments.get_mut(payment).filter(|p| p.state == ClaimPaymentState::Instructed) {
            p.state = ClaimPaymentState::Settled;
            record.payable_minor -= p.amount_minor;
            record.instructed_minor -= p.amount_minor;
        }
    }
}

/// The bank sent a settled benefit payment back. The core has posted it to
/// benefit payable; the claim owes it again and the claims module is told in
/// the same step, so it raises its task and the two never disagree.
pub(crate) fn bank_returned(state: &mut CoreState, ctx: &mut Ctx<'_>, policy: &PolicyId, payment: &str) -> Result<(), Reject> {
    let mut claim = None;
    for (id, record) in state.policies.get_mut(policy).into_iter().flat_map(|p| p.claims.iter_mut()) {
        if let Some(p) = record.payments.get_mut(payment).filter(|p| p.state == ClaimPaymentState::Settled) {
            p.state = ClaimPaymentState::Returned;
            record.payable_minor += p.amount_minor;
            record.open = true;
            claim = Some(id.clone());
        }
    }
    match claim {
        Some(claim) => apply(
            state,
            ctx,
            ic_claims::Command::PaymentReturned {
                claim,
                payment_ref: payment.to_string(),
            },
        ),
        None => Ok(()),
    }
}

/// Close a claim: whatever reserve and unpaid admission remain are released
/// to claims expense, and anything overpaid becomes a receivable, in one entry.
fn close(ledger: &mut Ledger, policy: &mut Policy, ctx: &mut Ctx<'_>, claim: &str, outcome: String, overpayment: Option<(String, i64)>) -> Result<(), Reject> {
    let record = known(policy, claim)?;
    let overpaid = overpayment.as_ref().map_or(0, |o| o.1);
    if overpaid < 0 {
        return invalid("an overpayment is positive");
    }
    let c = policy.currency;
    // What has been released for payment is still owed; the rest is not.
    let (reserve, payable) = (record.reserve_minor, record.payable_minor - record.instructed_minor);
    let (expense, d) = incurred(policy, &record.cover, "claim_closed");
    let mut set = PostingSet::new();
    set.debit(accounts::claim_reserve(&policy.id, c), i128::from(reserve), d.clone());
    set.debit(accounts::benefit_payable(&policy.id, c), i128::from(payable), d.clone());
    set.debit(accounts::claim_recovery_receivable(&policy.id, c), i128::from(overpaid), d.clone());
    set.credit(expense, i128::from(reserve + payable + overpaid), d);
    let extra = [("outcome", outcome), ("overpayment", overpayment.map(|o| o.0).unwrap_or_default())];
    post(ledger, policy, ctx, "claim_closed", set, claim, &extra)?;
    if let Some(record) = policy.claims.get_mut(claim) {
        record.reserve_minor = 0;
        record.payable_minor -= payable;
        record.receivable_minor += overpaid;
        record.open = false;
    }
    Ok(())
}

/// Reopen a closed claim with a new reserve. Nothing posted is edited.
fn reopen(ledger: &mut Ledger, policy: &mut Policy, ctx: &mut Ctx<'_>, claim: &str, reserve: i64) -> Result<(), Reject> {
    let record = known(policy, claim)?;
    if reserve < 0 || record.open {
        return invalid("a closed claim is reopened with a reserve that is not negative");
    }
    let c = policy.currency;
    let (expense, d) = incurred(policy, &record.cover, "claim_reopened");
    let mut set = PostingSet::new();
    set.debit(expense, i128::from(reserve), d.clone());
    set.credit(accounts::claim_reserve(&policy.id, c), i128::from(reserve), d);
    post(ledger, policy, ctx, "claim_reopened", set, claim, &[])?;
    if let Some(record) = policy.claims.get_mut(claim) {
        record.reserve_minor += reserve;
        record.open = true;
    }
    Ok(())
}

/// Part of an offset kept back here for another partition was more than was
/// owed there. It comes out of transit and the benefit it was kept from is
/// owed again, so the expense it was drawn from is given back; paying it
/// with the next benefit draws it again.
fn return_offset(ledger: &mut Ledger, policy: &mut Policy, ctx: &mut Ctx<'_>, claim: &str, amount: i64) -> Result<(), Reject> {
    let record = known(policy, claim)?;
    if amount <= 0 {
        return invalid("an offset returned is positive");
    }
    let c = policy.currency;
    let (expense, d) = incurred(policy, &record.cover, "claim_offset_returned");
    let mut set = PostingSet::new();
    set.debit(accounts::claims_offset_in_transit(c), i128::from(amount), Dimensions::default());
    set.credit(expense, i128::from(amount), d);
    post(ledger, policy, ctx, "claim_offset_returned", set, claim, &[])
}

/// Raise, recover or write off an overpayment. The receivable is on the
/// claim's policy; recovery is money in, a write-off is its own expense.
fn move_receivable(
    ledger: &mut Ledger,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    claim: &str,
    kind: &str,
    overpayment: String,
    amount: i64,
) -> Result<(), Reject> {
    let record = known(policy, claim)?;
    let raising = kind == "claim_overpayment_raised";
    if amount <= 0 || (!raising && amount > record.receivable_minor) {
        return invalid("the amount is positive and no more than is outstanding on the overpayment");
    }
    let c = policy.currency;
    let (expense, d) = incurred(policy, &record.cover, kind);
    let receivable = accounts::claim_recovery_receivable(&policy.id, c);
    let mut set = PostingSet::new();
    match kind {
        "claim_overpayment_raised" => {
            set.debit(receivable, i128::from(amount), d.clone());
            set.credit(expense, i128::from(amount), d);
        }
        "claim_overpayment_recovered" => {
            set.debit(accounts::settlement(c), i128::from(amount), Dimensions::default());
            set.credit(receivable, i128::from(amount), d);
        }
        // Kept back from a benefit on another partition, on its way here.
        "claim_offset_received" => {
            set.debit(accounts::claims_offset_in_transit(c), i128::from(amount), Dimensions::default());
            set.credit(receivable, i128::from(amount), d);
        }
        _ => {
            set.debit(accounts::claim_recovery_write_off(&policy.product_code, c), i128::from(amount), d.clone());
            set.credit(receivable, i128::from(amount), d);
        }
    }
    post(ledger, policy, ctx, kind, set, claim, &[("overpayment", overpayment)])?;
    if let Some(record) = policy.claims.get_mut(claim) {
        record.receivable_minor += if raising { amount } else { -amount };
    }
    Ok(())
}

/// The daily control: for every policy, the ledger's claim reserve, benefit
/// payable and recovery receivable equal the core's claim records, and those
/// equal the claims module's own figures.
pub(crate) fn reconcile(state: &CoreState, breaks: &mut Vec<crate::recon::Break>) {
    let module = state.claims.totals_by_policy();
    for p in state.policies.values().filter(|p| !p.is_frozen() && p.status != PolicyStatus::Transferred) {
        let c = p.currency;
        let sum = |f: fn(&ClaimLedger) -> i64| -> i128 { p.claims.values().map(|l| i128::from(f(l))).sum() };
        let core = (sum(|l| l.reserve_minor), sum(|l| l.payable_minor), sum(|l| l.receivable_minor));
        let ledger = (
            state.ledger.natural_balance(&accounts::claim_reserve(&p.id, c).id),
            // Benefit payable also carries unit-linked money on its way out.
            state.ledger.natural_balance(&accounts::benefit_payable(&p.id, c).id) - crate::invest::payouts_outstanding(p),
            state.ledger.natural_balance(&accounts::claim_recovery_receivable(&p.id, c).id),
        );
        if ledger != core {
            breaks.push(crate::recon::Break {
                check: "claims_ledger",
                detail: format!("policy {}: ledger {ledger:?}, claim records {core:?} (reserve, payable, receivable)", p.id),
            });
        }
        // The module counts what it has still to release; the ledger also
        // holds what is released and waiting for the bank.
        let waiting = sum(|l| l.instructed_minor);
        let m = module.get(&p.id.0).copied().unwrap_or_default();
        let claims = (
            i128::from(m.reserve_minor),
            i128::from(m.payable_minor) + waiting,
            i128::from(m.receivable_minor),
        );
        if claims != core {
            breaks.push(crate::recon::Break {
                check: "claims_control",
                detail: format!(
                    "policy {}: claims module {claims:?}, claim records {core:?} (reserve, payable, receivable)",
                    p.id
                ),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Ctx<'static> {
        Ctx {
            index: 7,
            date: NaiveDate::MIN,
            caller: "claims",
            salt: "",
            events: vec![],
            entries: vec![],
            messages: vec![],
            work: 1,
        }
    }

    /// A partition with the shipped term life product and one policy on it,
    /// open on 15 November 2027.
    fn book() -> (CoreState, NaiveDate) {
        book_of(include_str!("../../../products/term_life.star"), &[("life", "life-1", 400_000_00)])
    }

    /// The same, on a product source of the test's own, with the covers given
    /// as (cover, life, sum insured).
    fn book_of(source: &str, covers: &[(&str, &str, i64)]) -> (CoreState, NaiveDate) {
        use crate::command::{CoverSpec, Envelope, IssuePolicy, LifeSpec};
        let today = NaiveDate::from_ymd_opt(2027, 11, 15).unwrap();
        let mut state = CoreState::new(0);
        let definitions = ic_product::load_definitions("library.star", include_str!("../../../products/definitions/library.star")).unwrap();
        let rates = [
            include_str!("../../../products/rates/life_base.star"),
            include_str!("../../../products/rates/trauma_base.star"),
        ];
        let product = ic_product::load_product("term_life.star", source).unwrap();
        let mut lives: Vec<&str> = covers.iter().map(|c| c.1).collect();
        lives.dedup();
        let issue = IssuePolicy {
            policy: PolicyId("pol-1".to_string()),
            number: "KTL1".to_string(),
            group: crate::GroupId("grp-1".to_string()),
            policyholder: crate::PartyId("party-1".to_string()),
            product_code: "term-life".to_string(),
            inception: today,
            frequency: ic_product::Frequency::Monthly,
            lives: lives
                .iter()
                .map(|id| LifeSpec {
                    id: LifeId(id.to_string()),
                    kind: LifeKind::Named {
                        party: crate::PartyId(id.replace("life", "party")),
                    },
                    factors: crate::LifeFactors {
                        date_of_birth: NaiveDate::from_ymd_opt(1988, 6, 2),
                        sex: Some(ic_product::Sex::Female),
                        smoker: Some(false),
                        ..Default::default()
                    },
                })
                .collect(),
            covers: covers
                .iter()
                .map(|(code, life, sum)| CoverSpec {
                    code: code.to_string(),
                    life: Some(LifeId(life.to_string())),
                    sum_insured_minor: *sum,
                    loadings: vec![],
                    exclusions: vec![],
                    excess_option: None,
                })
                .collect(),
            migrated: false,
            continuation_of: None,
        };
        let mut commands = vec![
            ("leader", crate::Command::OpenBusinessDate { date: today }),
            ("config", crate::Command::ActivateDefinitions { definitions }),
        ];
        for source in rates {
            let table = Box::new(ic_product::load_rate_table("rates.star", source).unwrap());
            commands.push(("config", crate::Command::ActivateRateTable { table }));
        }
        commands.push(("config", crate::Command::ActivateProduct { product: Box::new(product) }));
        commands.push(("new-business", crate::Command::IssuePolicy(Box::new(issue))));
        for (index, (caller, command)) in commands.into_iter().enumerate() {
            let envelope = Envelope {
                key: format!("k{index}"),
                caller: caller.to_string(),
                salt: "s".to_string(),
                traceparent: None,
                command,
            };
            let applied = state.apply_envelope(index as u64 + 1, envelope);
            assert_eq!(applied.result, Ok(()), "command {index}");
        }
        (state, today)
    }

    /// Purpose: tax withheld from late-payment interest on a claim, and from an income
    /// benefit, leaves benefit payable for the withholding tax payable account of its kind in
    /// its own entry under its own posting type, and the bank is asked for the rest; tax
    /// larger than the interest or the benefit it is withheld from is refused with nothing
    /// posted.
    /// Why it matters: the design posts tax on claim payments at the event. Finance's close
    /// reconciles the withholding payable to these posting types, and the payee's payment
    /// plus the tax must equal what the claim cost. Tax netted silently out of a payment
    /// could never be agreed to a return, and tax above its base would underpay a claimant.
    #[test]
    fn tax_withheld_from_interest_and_benefits_is_owed_to_the_revenue_in_its_own_entry() {
        let (mut state, today) = book();
        let mut ctx = Ctx { date: today, ..ctx() };
        let run = |state: &mut CoreState, ctx: &mut Ctx<'_>, instruction| execute(state, ctx, instruction);
        let pol = PolicyId("pol-1".to_string());
        let admit = Instruction::Admit {
            life: None,
            claim: "clm-1".into(),
            policy: "pol-1".into(),
            cover: "life".into(),
            amount_minor: 10_000_00,
            release_reserve: false,
            deduct_premium_minor: 0,
            terminate: None,
            repay_loan_minor: 0,
        };
        run(&mut state, &mut ctx, admit).unwrap();
        let pay = |withheld: i64| Instruction::Pay {
            claim: "clm-1".into(),
            policy: "pol-1".into(),
            payment_ref: "pay-1".into(),
            amount_minor: 4_000_00,
            payee: "party-1".into(),
            close: false,
            interest_minor: 100_00,
            interest_declined_minor: 0,
            interest_withheld_minor: withheld,
            offset: None,
        };
        let entries = ctx.entries.len();
        assert_eq!(run(&mut state, &mut ctx, pay(100_01)).unwrap_err().code, "invalid_amount");
        assert_eq!(ctx.entries.len(), entries, "a refusal posts nothing");
        run(&mut state, &mut ctx, pay(33_00)).unwrap();
        let rwt = accounts::withholding_tax_payable("rwt", ic_money::Currency::NZD);
        assert_eq!(state.ledger.natural_balance(&rwt.id), 33_00);
        let record = &state.policies[&pol].claims["clm-1"];
        assert_eq!(record.payments["pay-1"].amount_minor, 4_000_00 + 100_00 - 33_00);
        let withheld = ctx.entries.last().unwrap();
        assert_eq!((withheld.body.kind.as_str(), withheld.body.detail["tax"].as_str()), ("tax_withheld", "rwt"));
        assert!(withheld.body.postings.iter().all(|p| p.dims.posting_type.as_deref() == Some("withholding_rwt")));

        let benefit = |withheld: i64| Instruction::PayBenefit {
            claim: "clm-1".into(),
            policy: "pol-1".into(),
            cover: "life".into(),
            payment_ref: "ben-1".into(),
            period_start: today,
            period_end: today,
            gross_minor: 1_000_00,
            offset: None,
            payee: "party-1".into(),
            close: false,
            tax_withheld_minor: withheld,
        };
        assert_eq!(run(&mut state, &mut ctx, benefit(1_000_01)).unwrap_err().code, "invalid_amount");
        run(&mut state, &mut ctx, benefit(175_00)).unwrap();
        assert_eq!(
            state
                .ledger
                .natural_balance(&accounts::withholding_tax_payable("benefit", ic_money::Currency::NZD).id),
            175_00
        );
        let record = &state.policies[&pol].claims["clm-1"];
        assert_eq!(record.payments["ben-1"].amount_minor, 825_00);
        // What is owed to payees and the bank is asked for agree: nothing is left over.
        let payable = state.ledger.natural_balance(&accounts::benefit_payable(&pol, ic_money::Currency::NZD).id);
        assert_eq!(payable, i128::from(record.payable_minor));
    }

    /// Purpose: premium a claim's waiver paid is billed back from a date: the waiver's entry
    /// is reversed under its own posting type, the receivable is outstanding again for
    /// exactly what was waived, the claim no longer counts it, and commission is told with
    /// the same breakdown the waiver gave; a date after the waived period, or asking twice,
    /// bills nothing back.
    /// Why it matters: an admission reversed on a false declaration leaves the policyholder
    /// owing the premium the insurer paid for them. Billed back by hand it would miss the
    /// receivable, the expense or the commission; billed back twice it would charge them
    /// double.
    #[test]
    fn a_waived_premium_is_billed_back_once_for_the_periods_asked() {
        let (mut state, today) = book();
        let mut ctx = Ctx { date: today, ..ctx() };
        let pol = PolicyId("pol-1".to_string());
        let owed = state.policies[&pol].outstanding_minor();
        assert!(owed > 0, "the first premium is billed and unpaid");
        let admit = Instruction::Admit {
            life: None,
            claim: "clm-1".into(),
            policy: "pol-1".into(),
            cover: "life".into(),
            amount_minor: 1_000_00,
            release_reserve: false,
            deduct_premium_minor: 0,
            terminate: None,
            repay_loan_minor: 0,
        };
        execute(&mut state, &mut ctx, admit).unwrap();
        let waive = Instruction::WaivePremium {
            claim: "clm-1".into(),
            policy: "pol-1".into(),
            cover: "life".into(),
            only_cover: false,
            from: today,
            until: today + chrono::Days::new(31),
        };
        execute(&mut state, &mut ctx, waive).unwrap();
        let expense = accounts::claims_incurred("term-life", "life", ic_money::Currency::NZD);
        let spent = state.ledger.natural_balance(&expense.id);
        assert_eq!(
            (state.policies[&pol].outstanding_minor(), state.policies[&pol].claims["clm-1"].waived_minor),
            (0, owed)
        );

        let unwaive = |from: NaiveDate| Instruction::UnwaivePremium {
            claim: "clm-1".into(),
            policy: "pol-1".into(),
            cover: "life".into(),
            from,
        };
        let entries = ctx.entries.len();
        execute(&mut state, &mut ctx, unwaive(today + chrono::Days::new(1))).unwrap();
        assert_eq!(ctx.entries.len(), entries, "the waived period began before that date");
        execute(&mut state, &mut ctx, unwaive(today)).unwrap();
        let p = &state.policies[&pol];
        assert_eq!((p.outstanding_minor(), p.claims["clm-1"].waived_minor), (owed, 0));
        assert!(p.claims["clm-1"].waived.is_empty());
        assert_eq!(spent - state.ledger.natural_balance(&expense.id), i128::from(owed));
        let reversal = ctx.entries.last().unwrap();
        assert_eq!(reversal.body.kind, "premium_waiver_reversed");
        assert!(reversal
            .body
            .postings
            .iter()
            .all(|p| p.dims.posting_type.as_deref() == Some("premium_waiver_reversed")));
        let told = ctx.events.iter().rev().find_map(|e| match e {
            Event::Module { kind, detail, .. } if kind == "premium_waiver_reversed" => Some(detail.clone()),
            _ => None,
        });
        assert_eq!(told.unwrap()["amount_minor"], owed.to_string());
        let entries = ctx.entries.len();
        execute(&mut state, &mut ctx, unwaive(today)).unwrap();
        assert_eq!(ctx.entries.len(), entries, "asked twice, billed back once");
    }

    /// Purpose: a waiver that belongs to one cover waives that cover's share of each premium
    /// in its window (what the cover earns in it over everything the premium pays for), and
    /// asked again the next day waives nothing more; a cover the premium does not pay for
    /// waives nothing.
    /// Why it matters: AIA's pregnancy waiver waives the income cover's premium only. Waiving
    /// the whole premium would give away the other covers' premium, and a waiver run every
    /// day that forgot what it had already waived would waive the rest a share at a time.
    #[test]
    fn a_waiver_for_one_cover_waives_only_that_covers_share_once() {
        let (mut state, today) = book();
        let mut ctx = Ctx { date: today, ..ctx() };
        let pol = PolicyId("pol-1".to_string());
        let admit = Instruction::Admit {
            life: None,
            claim: "clm-1".into(),
            policy: "pol-1".into(),
            cover: "life".into(),
            amount_minor: 1_000_00,
            release_reserve: false,
            deduct_premium_minor: 0,
            terminate: None,
            repay_loan_minor: 0,
        };
        execute(&mut state, &mut ctx, admit).unwrap();
        let p = &state.policies[&pol];
        let r = p.receivables.iter().find(|r| r.outstanding_minor > 0).unwrap();
        let cover_type = p.covers["life"].cover_type.clone();
        let all: i128 = r.net_fine.values().sum();
        let share = i64::try_from(crate::postings::div_half_even(i128::from(r.total_minor) * r.net_fine[&cover_type], all)).unwrap();
        let owed = p.outstanding_minor();
        let waive = |cover: &str| Instruction::WaivePremium {
            claim: "clm-1".into(),
            policy: "pol-1".into(),
            cover: cover.into(),
            only_cover: true,
            from: today,
            until: today + chrono::Days::new(31),
        };
        execute(&mut state, &mut ctx, waive("life")).unwrap();
        let p = &state.policies[&pol];
        assert!(share > 0 && share <= owed);
        assert_eq!((p.claims["clm-1"].waived_minor, p.outstanding_minor()), (share, owed - share));
        let entries = ctx.entries.len();
        execute(&mut state, &mut ctx, waive("life")).unwrap();
        execute(&mut state, &mut ctx, waive("no-such-cover")).unwrap();
        assert_eq!(ctx.entries.len(), entries, "nothing more to waive");
        assert_eq!(state.policies[&pol].claims["clm-1"].waived_minor, share);
    }

    /// Purpose: a claims command queued for another partition leaves with an
    /// ID that names the sender, the log entry and its place in that entry,
    /// and the relay delivers it as a claims command from caller `partition`.
    /// Why it matters: the ID is the receiver's idempotency key, so two
    /// messages from one entry must never share one, and the caller is what
    /// stops a partition message reaching a command meant for staff.
    #[test]
    fn a_claims_message_for_another_partition_is_queued_and_delivered_as_partition() {
        let state = CoreState::new(1);
        let mut ctx = ctx();
        send(&state, &mut ctx, 2, ic_claims::Command::DrainDue).unwrap();
        send(&state, &mut ctx, 3, ic_claims::Command::DrainDue).unwrap();
        let ids: Vec<&str> = ctx.messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["claims:1:7:0", "claims:1:7:1"]);
        assert_eq!(ctx.messages[1].to_partition, 3);
        let (caller, command) = ctx.messages.remove(0).body.into_delivery();
        assert_eq!(caller, "partition");
        assert_eq!(command, crate::Command::Claims(Box::new(ic_claims::Command::DrainDue)));
    }

    /// Purpose: a partition cannot queue a claims message to itself.
    /// Why it matters: the relay only carries messages between partitions,
    /// so a message addressed home would mean the sender chose the wrong
    /// partition, and applying it would hide that.
    #[test]
    fn a_claims_message_to_the_same_partition_is_refused() {
        let state = CoreState::new(1);
        let mut ctx = ctx();
        let refused = send(&state, &mut ctx, 1, ic_claims::Command::DrainDue).unwrap_err();
        assert_eq!(refused.code, "same_partition");
        assert!(ctx.messages.is_empty());
    }
    fn lump_sum(claim: &str, cover: &str, life: Option<&str>, amount_minor: i64) -> Instruction {
        Instruction::Admit {
            claim: claim.into(),
            policy: "pol-1".into(),
            cover: cover.into(),
            amount_minor,
            release_reserve: false,
            deduct_premium_minor: 0,
            terminate: None,
            repay_loan_minor: 0,
            life: life.map(str::to_string),
        }
    }

    fn held(state: &CoreState, cover: &str, life: &str, on: NaiveDate) -> i64 {
        let layers = &state.policies[&PolicyId("pol-1".to_string())].covers[cover].layers;
        layers
            .iter()
            .filter(|l| l.life.0 == life && l.in_force_on(on))
            .map(|l| l.sum_insured_minor)
            .sum()
    }

    /// Purpose: on a product whose trauma cover is declared to reduce the life cover, a trauma
    /// lump sum admitted for a life takes the amount off both covers from the business date
    /// and hands back the premium billed for the days ahead; a second admission on the claim
    /// takes only its own amount; the cover check still reports the sum insured before the
    /// claim and names the cover that reduces it; and reversing the admission writes both
    /// covers back and bills the premium again, to the cent.
    /// Why it matters: accelerated trauma is sold as an advance of the life cover. If the life
    /// cover stayed whole the insurer would pay the same sum twice and bill premium for cover
    /// already paid out; if the cover check reported the reduced amount, claims, which takes
    /// earlier admissions off itself, would deduct the first payment twice.
    #[test]
    fn a_lump_sum_reduces_its_cover_and_the_covers_linked_to_it_until_reversed() {
        let source = include_str!("../../../products/term_life.star").replace("code = \"trauma\",", "code = \"trauma\",\n            reduces = [\"life\"],");
        let (mut state, today) = book_of(&source, &[("life", "life-1", 400_000_00), ("trauma", "life-1", 100_000_00)]);
        let mut ctx = Ctx { date: today, ..ctx() };
        let pol = PolicyId("pol-1".to_string());
        let owed = state.policies[&pol].outstanding_minor();

        execute(&mut state, &mut ctx, lump_sum("clm-1", "trauma", Some("life-1"), 60_000_00)).unwrap();
        assert_eq!(
            (held(&state, "trauma", "life-1", today), held(&state, "life", "life-1", today)),
            (40_000_00, 340_000_00)
        );
        let after_first = state.policies[&pol].outstanding_minor();
        assert!(after_first < owed, "premium billed for cover no longer held is handed back");
        execute(&mut state, &mut ctx, lump_sum("clm-1", "trauma", Some("life-1"), 40_000_00)).unwrap();
        assert_eq!(
            (held(&state, "trauma", "life-1", today), held(&state, "life", "life-1", today)),
            (0, 300_000_00)
        );

        let trauma = cover_facts(&state, "pol-1", "trauma", Some("life-1"), today).unwrap();
        let life = cover_facts(&state, "pol-1", "life", Some("life-1"), today).unwrap();
        assert_eq!((trauma.sum_insured_minor, life.sum_insured_minor), (100_000_00, 400_000_00));
        assert_eq!((life.reduced_by, trauma.reduced_by), (vec!["trauma".to_string()], vec![]));
        assert!(life.joint_lives.is_empty());

        let close = Instruction::Close {
            claim: "clm-1".into(),
            policy: "pol-1".into(),
            cover: "trauma".into(),
            outcome: "admission_reversed".into(),
            overpayment: None,
        };
        execute(&mut state, &mut ctx, close).unwrap();
        assert_eq!(
            (held(&state, "trauma", "life-1", today), held(&state, "life", "life-1", today)),
            (100_000_00, 400_000_00)
        );
        assert_eq!(state.policies[&pol].outstanding_minor(), owed);
        assert!(state.policies[&pol].claims["clm-1"].cuts.is_empty());
        let life = cover_facts(&state, "pol-1", "life", Some("life-1"), today).unwrap();
        assert_eq!(life.sum_insured_minor, 400_000_00, "a restored cover is not counted twice");
    }

    /// Purpose: on a joint first-event cover a lump sum admitted for one life ends the cover
    /// for every life on it, and the cover check names them all; an advance admitted without
    /// a life leaves the cover alone until the admission that names the life, which then takes
    /// the whole of what the claim admitted; and a cover that is not joint is reduced for the
    /// claimant's life only.
    /// Why it matters: a joint cover carries one sum insured, paid once. Left in force for the
    /// survivor it would be paid a second time. A funeral advance on a claim later declined
    /// must not cost the family their cover, and a couple with a cover each must not lose the
    /// partner's when one of them claims.
    #[test]
    fn a_joint_cover_ends_for_every_life_on_the_first_event() {
        let covers = [("life", "life-1", 400_000_00), ("life", "life-2", 400_000_00)];
        let joint = include_str!("../../../products/term_life.star").replace("code = \"life\",", "code = \"life\",\n            joint_first_event = True,");
        let (mut state, today) = book_of(&joint, &covers);
        let mut ctx = Ctx { date: today, ..ctx() };
        execute(&mut state, &mut ctx, lump_sum("clm-1", "life", None, 10_000_00)).unwrap();
        assert_eq!(held(&state, "life", "life-1", today), 400_000_00, "an advance leaves the cover as it is");
        execute(&mut state, &mut ctx, lump_sum("clm-1", "life", Some("life-1"), 390_000_00)).unwrap();
        assert_eq!((held(&state, "life", "life-1", today), held(&state, "life", "life-2", today)), (0, 0));
        let facts = cover_facts(&state, "pol-1", "life", Some("life-2"), today).unwrap();
        assert_eq!(facts.joint_lives, vec!["life-1".to_string(), "life-2".to_string()]);
        assert_eq!(facts.sum_insured_minor, 400_000_00);

        let (mut state, today) = book_of(include_str!("../../../products/term_life.star"), &covers);
        let mut ctx = Ctx { date: today, ..self::ctx() };
        execute(&mut state, &mut ctx, lump_sum("clm-1", "life", Some("life-1"), 400_000_00)).unwrap();
        assert_eq!((held(&state, "life", "life-1", today), held(&state, "life", "life-2", today)), (0, 400_000_00));
    }

    /// Purpose: the cover check reports what is owed on a loan against the policy, and an
    /// admission that says to repay it moves that much of the benefit from benefit payable
    /// to the loan, interest first, in its own entry, leaving the rest payable on the claim;
    /// it is refused where the policy does not owe that much on a loan, and where premium
    /// and loan together would take more than the claim pays.
    /// Why it matters: a death benefit paid in full on a policy with a loan pays the estate
    /// money the owner already had. The claims module decides the deduction from the fact
    /// the core reports and relies on the core to refuse one that is not owed; a loan taken
    /// off without its own entry could not be agreed to the loan account at the close.
    #[test]
    fn an_admission_repays_the_policy_loan_out_of_the_benefit() {
        let (mut state, today) = book();
        let mut ctx = Ctx { date: today, ..ctx() };
        let pol = PolicyId("pol-1".to_string());
        let c = ic_money::Currency::NZD;
        // A loan of NZD 3,000 with NZD 50 of interest, lent and paid out earlier.
        let policy = state.policies[&pol].clone();
        let mut set = PostingSet::new();
        set.debit(accounts::policy_loan(&pol, c), 3_050_00, Dimensions::default());
        set.credit(accounts::settlement(c), 3_050_00, Dimensions::default());
        let postings = set.into_postings(&mut state.ledger).unwrap();
        ctx.post(&mut state.ledger, "fixture", Some(&policy), today, postings, Default::default())
            .unwrap();
        state.policies.get_mut(&pol).unwrap().loan = Some(crate::loan::Loan {
            principal_minor: 3_000_00,
            interest_minor: 50_00,
            ..Default::default()
        });
        assert_eq!(cover_facts(&state, "pol-1", "life", None, today).unwrap().policy_loan_minor, 3_050_00);

        let admit = |deduct: i64, repay: i64| Instruction::Admit {
            life: None,
            claim: "clm-1".into(),
            policy: "pol-1".into(),
            cover: "life".into(),
            amount_minor: 10_000_00,
            release_reserve: false,
            deduct_premium_minor: deduct,
            terminate: None,
            repay_loan_minor: repay,
        };
        let entries = ctx.entries.len();
        assert_eq!(execute(&mut state, &mut ctx, admit(0, 3_050_01)).unwrap_err().code, "loan_not_owing");
        assert_eq!(execute(&mut state, &mut ctx, admit(0, -1)).unwrap_err().code, "invalid_amount");
        assert_eq!(execute(&mut state, &mut ctx, admit(7_000_00, 3_050_00)).unwrap_err().code, "invalid_amount");
        assert_eq!(ctx.entries.len(), entries, "a refusal posts nothing");

        execute(&mut state, &mut ctx, admit(0, 3_050_00)).unwrap();
        assert_eq!(state.ledger.balance(&accounts::policy_loan(&pol, c).id), 0);
        assert_eq!(state.ledger.natural_balance(&accounts::benefit_payable(&pol, c).id), 6_950_00);
        let record = &state.policies[&pol];
        assert_eq!(
            (record.claims["clm-1"].admitted_minor, record.claims["clm-1"].payable_minor),
            (10_000_00, 6_950_00)
        );
        assert_eq!(record.loan.as_ref().map(crate::loan::Loan::outstanding_minor), Some(0));
        let repaid = ctx.entries.last().unwrap();
        assert_eq!(
            (repaid.body.kind.as_str(), repaid.body.detail["claim"].as_str()),
            ("policy_loan_repaid", "clm-1")
        );
        assert!(ctx.events.iter().any(|e| matches!(
            e,
            Event::PolicyLoanRepaid { amount_minor: 3_050_00, interest_minor: 50_00, principal_minor: 3_000_00, outstanding_minor: 0, from, .. } if from == "claim"
        )));
        assert_eq!(cover_facts(&state, "pol-1", "life", None, today).unwrap().policy_loan_minor, 0);
    }

    /// Purpose: where a claim ends the policy and its admission repaid only part of the
    /// loan, the rest is written off to the product's loan loss account in its own entry,
    /// with an event, as the policy ends; the loan account is left at nothing.
    /// Why it matters: once the policy has ended there is no value and no benefit left to
    /// repay a loan from. Left on the books it is an asset that will never be collected,
    /// and the daily reconciliation of loan accounts would carry it for ever.
    #[test]
    fn a_loan_the_claim_did_not_repay_is_written_off_when_the_claim_ends_the_policy() {
        let (mut state, today) = book();
        let mut ctx = Ctx { date: today, ..ctx() };
        let pol = PolicyId("pol-1".to_string());
        let c = ic_money::Currency::NZD;
        let policy = state.policies[&pol].clone();
        let mut set = PostingSet::new();
        set.debit(accounts::policy_loan(&pol, c), 3_000_00, Dimensions::default());
        set.credit(accounts::settlement(c), 3_000_00, Dimensions::default());
        let postings = set.into_postings(&mut state.ledger).unwrap();
        ctx.post(&mut state.ledger, "fixture", Some(&policy), today, postings, Default::default())
            .unwrap();
        state.policies.get_mut(&pol).unwrap().loan = Some(crate::loan::Loan {
            principal_minor: 3_000_00,
            ..Default::default()
        });
        let admit = Instruction::Admit {
            life: None,
            claim: "clm-1".into(),
            policy: "pol-1".into(),
            cover: "life".into(),
            amount_minor: 10_000_00,
            release_reserve: false,
            deduct_premium_minor: 0,
            terminate: Some(Termination {
                event_date: today,
                payer_deceased: false,
            }),
            repay_loan_minor: 1_000_00,
        };
        execute(&mut state, &mut ctx, admit).unwrap();
        assert_eq!(state.policies[&pol].status, PolicyStatus::Terminated);
        assert_eq!(state.ledger.balance(&accounts::policy_loan(&pol, c).id), 0);
        assert_eq!(state.ledger.natural_balance(&accounts::policy_loan_written_off("term-life", c).id), 2_000_00);
        assert_eq!(state.policies[&pol].loan.as_ref().map(crate::loan::Loan::outstanding_minor), Some(0));
        assert!(ctx.events.iter().any(|e| matches!(
            e,
            Event::PolicyLoanWrittenOff { amount_minor: 2_000_00, cause, .. } if cause == "terminated_on_claim"
        )));
    }
    /// Purpose: the first day a life is a given age is its birthday, and for a life born on
    /// 29 February it is the first day age counts it as that old, in a leap year or not; the
    /// day before, the life is still a year younger.
    /// Why it matters: an income benefit to age 65 stops on this day. A day early takes a
    /// day's benefit from every claimant who reaches the age on claim; a date that disagrees
    /// with how age is counted for premiums would have the two halves of the system
    /// disagree about how old someone is.
    #[test]
    fn the_day_a_benefit_to_an_age_ends_is_the_first_day_at_that_age() {
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();
        assert_eq!(birthday_at(d(1987, 6, 1), 65), Some(d(2052, 6, 1)));
        for (born, age) in [(d(1988, 2, 29), 65), (d(1988, 2, 29), 64), (d(1990, 12, 31), 70)] {
            let day = birthday_at(born, age).unwrap();
            assert_eq!(ic_product::schedule::age_on(born, day), age, "{born} at {age}");
            assert_eq!(
                ic_product::schedule::age_on(born, day.pred_opt().unwrap()),
                age - 1,
                "{born} the day before {age}"
            );
        }
        assert_eq!(birthday_at(d(1988, 2, 29), 64), Some(d(2052, 2, 29)));
        assert_eq!(birthday_at(d(1987, 6, 1), u32::MAX), None, "an age no calendar reaches is no date");
    }
}
