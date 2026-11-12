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
use crate::event::Event;
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

/// A policy stays on its partition while a claim on it is open or has money
/// outstanding: the claim record lives here.
pub(crate) fn may_transfer(policy: &Policy) -> Result<(), Reject> {
    let busy = |c: &ClaimLedger| c.open || c.reserve_minor != 0 || c.payable_minor != 0 || c.receivable_minor != 0;
    if policy.claims.values().any(busy) {
        return Err(Reject::new(
            "open_claim",
            "the policy has a claim that is open or has money outstanding; it cannot be transferred yet",
        ));
    }
    Ok(())
}

fn status_code(status: PolicyStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The cover check: what the policy record says about a cover and a life on
/// the event date, as known now. Facts only.
fn cover_facts(state: &CoreState, policy: &str, cover: &str, life: Option<&str>, on: NaiveDate) -> Result<CoverFacts, Reject> {
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
        LifeKind::Pool { .. } => None,
    });
    // Standing on the event date: what is unpaid now was unpaid then. A lapse
    // wrote the unpaid premium off, so for a lapsed policy the question is
    // asked of the receivables as they stood when it lapsed.
    let lapsed = p.lapse.as_ref().filter(|_| p.status == PolicyStatus::Lapsed);
    let unpaid: Vec<&crate::policy::Receivable> = match lapsed {
        Some(l) => l.receivables_before.iter().filter(|r| !r.closed && r.outstanding_minor > 0).collect(),
        None => p.receivables.iter().filter(|r| !r.closed && r.outstanding_minor > 0).collect(),
    };
    // Premium paid since the event for a period that was due by it: owed on
    // the event date, though nothing is owed on it now.
    let settled_since: Vec<&crate::policy::Receivable> = p
        .receivables
        .iter()
        .filter(|r| !r.closed && r.outstanding_minor == 0 && r.total_minor > 0 && r.due <= on && r.settled_on.is_some_and(|s| s > on))
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
        ends_on_death: p.lives.len() == 1 && p.lives.values().all(|l| matches!(l.kind, LifeKind::Named { .. })) && p.covers.values().all(|c| !c.formula_driven),
        policyholder: p.policyholder.0.clone(),
        product_code: p.product_code.clone(),
        product_version: p.product_hash.clone(),
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
    let mut covered = !at_event.is_empty();
    if c.formula_driven {
        // Scheme cover moves with the member's details as they were on the date.
        let factors = record.filter(|_| life_on_policy).and_then(|r| r.factors.as_of(on));
        let amount = match (product, life_id.as_ref(), factors) {
            (Some(pr), Some(id), Some(f)) => lifecycle::member_covers(pr, p, id, f)
                .into_iter()
                .find(|m| m.cover == cover)
                .map(|m| m.sum_insured_minor),
            _ => None,
        };
        sum_insured = amount.unwrap_or(0);
        covered = amount.is_some();
    }
    let mut exclusions: Vec<String> = at_event.iter().flat_map(|l| l.exclusions.iter().cloned()).collect();
    exclusions.sort();
    exclusions.dedup();
    facts.exclusions = exclusions;
    facts.sum_insured_minor = sum_insured;
    facts.in_force = covered && c.start <= on && policy_live && !matches!(standing, Some("suspended" | "lapsed"));
    // Whoever holds an assignment standing on the event date is paid first.
    let standing_interests = crate::interest::ranked(p, on).into_iter().map(|i| ic_claims::port::Interest {
        party: i.party.to_string(),
        extent_minor: i.extent_minor,
        priority: i.priority,
    });
    facts.interests = standing_interests.collect();
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
            terminate,
            ..
        } => admit(s, p, ctx, &claim, cover, amount_minor, release_reserve, deduct_premium_minor, terminate),
        Instruction::Pay {
            claim,
            payment_ref,
            amount_minor,
            payee,
            close,
            interest_minor,
            ..
        } => pay(&mut s.ledger, p, ctx, &claim, payment_ref, amount_minor, payee, close, interest_minor),
        Instruction::PayBenefit {
            claim,
            payment_ref,
            period_start,
            period_end,
            gross_minor,
            offset,
            payee,
            close,
            ..
        } => pay_benefit(s, p, ctx, &claim, payment_ref, (period_start, period_end), gross_minor, offset, payee, close),
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
        Instruction::WaivePremium { claim, cover, from, until, .. } => waive(s, p, ctx, &claim, &cover, from, until),
        Instruction::Close {
            claim, outcome, overpayment, ..
        } => close(&mut s.ledger, p, ctx, &claim, outcome, overpayment),
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
    deduct: i64,
    terminate: Option<Termination>,
) -> Result<(), Reject> {
    let existing = policy.claims.get(claim);
    if amount <= 0 || !(0..=amount).contains(&deduct) || existing.is_some_and(|c| !c.open) {
        return invalid("an admission is a positive amount on an open claim, less no more premium than it pays");
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
    record.payable_minor += amount - deduct;
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
    ledger: &mut Ledger,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    claim: &str,
    payment_ref: String,
    amount: i64,
    payee: String,
    close: bool,
    interest: i64,
) -> Result<(), Reject> {
    let record = known(policy, claim)?;
    let free = record.payable_minor - record.instructed_minor;
    if interest < 0 || amount <= 0 || amount > free || record.payments.contains_key(&payment_ref) || (close && amount != free) {
        return invalid("a payment is new, positive and within what is admitted and not yet released");
    }
    let c = policy.currency;
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
    post(
        ledger,
        policy,
        ctx,
        if interest > 0 { "claim_late_interest" } else { "claim_reserve_released" },
        set,
        claim,
        &[("payment_ref", payment_ref.clone()), ("payee", payee), ("interest_minor", interest.to_string())],
    )?;
    if let Some(record) = policy.claims.get_mut(claim) {
        record.reserve_minor -= reserve;
        record.payable_minor += interest;
        record.open = !close;
    }
    instruct(policy, claim, payment_ref, amount + interest, false);
    Ok(())
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
    gross: i64,
    offset: Option<Offset>,
    payee: String,
    close: bool,
) -> Result<(), Reject> {
    let record = known(policy, claim)?;
    let taken = offset.as_ref().map_or(0, |o| o.amount_minor);
    if gross < 0 || !(0..=gross).contains(&taken) || !record.open || record.payments.contains_key(&payment_ref) {
        return invalid("a benefit period is paid once, on an open claim, with an offset no larger than the benefit");
    }
    let c = policy.currency;
    // The overpayment an offset recovers may be on another policy of the
    // same person on this partition.
    let other = offset.as_ref().map(|o| PolicyId(o.policy.clone())).filter(|id| *id != policy.id);
    if let Some(o) = &offset {
        let owed = match &other {
            Some(id) => state
                .policies
                .get(id)
                .filter(|p| p.currency == c && !p.is_frozen())
                .and_then(|p| p.claims.get(&o.claim)),
            None => policy.claims.get(&o.claim),
        };
        if owed.is_none_or(|l| l.receivable_minor < o.amount_minor) {
            return Err(Reject::new("offset", "the overpayment does not have that much outstanding on this partition"));
        }
    }
    let drawn = record.reserve_minor.min(gross);
    let released = if close { record.reserve_minor - drawn } else { 0 };
    let (expense, d) = incurred(policy, &record.cover, "benefit_paid");
    let mut set = PostingSet::new();
    set.debit(accounts::claim_reserve(&policy.id, c), i128::from(drawn + released), d.clone());
    set.debit(expense, i128::from(gross - drawn - released), d.clone());
    set.credit(accounts::benefit_payable(&policy.id, c), i128::from(gross - taken), d.clone());
    if let Some(o) = &offset {
        set.credit(accounts::claim_recovery_receivable(&PolicyId(o.policy.clone()), c), i128::from(taken), d);
    }
    let extra = [
        ("payment_ref", payment_ref.clone()),
        ("payee", payee),
        ("period_start", period.0.to_string()),
        ("period_end", period.1.to_string()),
    ];
    post(&mut state.ledger, policy, ctx, "benefit_paid", set, claim, &extra)?;
    if let Some(record) = policy.claims.get_mut(claim) {
        record.reserve_minor -= drawn + released;
        record.payable_minor += gross - taken;
        record.open = !close;
    }
    instruct(policy, claim, payment_ref, gross - taken, false);
    if let Some(o) = &offset {
        let owing = match other {
            Some(id) => state.policies.get_mut(&id).and_then(|p| p.claims.get_mut(&o.claim)),
            None => policy.claims.get_mut(&o.claim),
        };
        if let Some(l) = owing {
            l.receivable_minor -= o.amount_minor;
        }
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

/// Premium waiver: the premium was billed as usual, and what is owing for
/// periods starting in the window is paid from claims expense, so earned
/// premium and claims cost are both recognised. Older arrears are not
/// touched, and nothing is posted where nothing is owing.
fn waive(state: &mut CoreState, policy: &mut Policy, ctx: &mut Ctx<'_>, claim: &str, cover: &str, from: NaiveDate, until: NaiveDate) -> Result<(), Reject> {
    known(policy, claim)?;
    let mine = |r: &&crate::policy::Receivable| r.outstanding_minor > 0 && !r.closed && r.period_start >= from && r.period_start < until;
    let total: i64 = policy.receivables.iter().filter(mine).map(|r| r.outstanding_minor).sum();
    if total == 0 || policy.status != PolicyStatus::InForce {
        return Ok(());
    }
    // The part of what is waived that pays for cover, by cover type, as a
    // receipt reports it: commission agreements may earn on waived premium.
    let waived: BTreeMap<u32, i64> = policy.receivables.iter().filter(mine).map(|r| (r.id, r.outstanding_minor)).collect();
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
    for r in policy.receivables.iter_mut().filter(|r| mine(&&**r)) {
        r.outstanding_minor = 0;
    }
    if let Some(record) = policy.claims.get_mut(claim) {
        record.waived_minor += total;
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
