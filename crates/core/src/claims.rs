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
use ic_money::Money;

use crate::accounts::{self, dims};
use crate::arrears::{self, Derived};
use crate::event::Event;
use crate::machine::{CoreState, Ctx, Reject};
use crate::policy::{ClaimLedger, ClaimPayment, LayerOrigin, LifeKind, Policy, PolicyStatus, Standing, UnwindCause};
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
    let mut policies: Vec<PolicyId> = due.filter_map(|(_, id)| state.claims.claims.get(id)).map(|c| PolicyId(c.policy.clone())).collect();
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
        return Err(Reject::new("open_claim", "the policy has a claim that is open or has money outstanding; it cannot be transferred yet"));
    }
    Ok(())
}

fn status_code(status: PolicyStatus) -> String {
    serde_json::to_value(status).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
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
    let life_id = life.map(|l| LifeId(l.to_string())).or_else(|| Some(p.lives.keys().next()?.clone()).filter(|_| p.lives.len() == 1));
    let record = life_id.as_ref().and_then(|l| p.lives.get(l));
    let life_on_policy = record.is_some_and(|r| r.in_force_on(on));
    let life_party = record.and_then(|r| match &r.kind {
        LifeKind::Named { party } => Some(party.0.clone()),
        LifeKind::UnnamedMember { linked_party, .. } => linked_party.as_ref().map(|p| p.0.clone()),
        LifeKind::Pool { .. } => None,
    });
    // Standing on the event date: what is unpaid now was unpaid then.
    let standing = product.and_then(|pr| pr.arrears()).map(|rules| match arrears::derive(p, rules, on) {
        Derived::Standing(Standing::Paid) => "paid",
        Derived::Standing(Standing::InGrace) => "in_grace",
        Derived::Standing(Standing::Suspended) => "suspended",
        Derived::Lapse => "lapsed",
    });
    let policy_live = match p.status {
        PolicyStatus::Lapsed => p.lapse.as_ref().is_some_and(|l| l.lapsed_on > on),
        PolicyStatus::Quoted | PolicyStatus::Proposed => false,
        _ => p.inception <= on,
    };
    let mut facts = CoverFacts {
        policy_status: status_code(p.status),
        standing: standing.map(str::to_string),
        outstanding_premium_minor: p.receivables.iter().filter(|r| !r.closed && r.due <= on).map(|r| r.outstanding_minor.max(0)).sum(),
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
        let later = c.layers.iter().filter(mine).filter(|l| l.origin == LayerOrigin::Indexation && l.start > on && l.end.is_none_or(|e| e > l.start));
        sum_insured += later.map(|l| l.sum_insured_minor).sum::<i64>();
    }
    let mut covered = !at_event.is_empty();
    if c.formula_driven {
        // Scheme cover moves with the member's details as they were on the date.
        let factors = record.filter(|_| life_on_policy).and_then(|r| r.factors.as_of(on));
        let amount = match (product, life_id.as_ref(), factors) {
            (Some(pr), Some(id), Some(f)) => lifecycle::member_covers(pr, p, id, f).into_iter().find(|m| m.cover == cover).map(|m| m.sum_insured_minor),
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
    Ok(facts)
}

fn execute(state: &mut CoreState, ctx: &mut Ctx<'_>, instruction: Instruction) -> Result<(), Reject> {
    let id = PolicyId(instruction.policy().to_string());
    state.with_policy_tracked(ctx, &id, true, |s, ctx, p| match instruction {
        Instruction::SetReserve {
            claim, cover, amount_minor, reason, ..
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
            ..
        } => pay(&mut s.ledger, p, ctx, &claim, payment_ref, amount_minor, payee, close),
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
        Instruction::PaymentReturned { claim, payment_ref, .. } => payment_returned(&mut s.ledger, p, ctx, &claim, &payment_ref),
        Instruction::ExGratia {
            claim,
            cover,
            payment_ref,
            amount_minor,
            payee,
            ..
        } => ex_gratia(&mut s.ledger, p, ctx, &claim, &cover, payment_ref, amount_minor, payee),
        Instruction::Close { claim, outcome, overpayment, .. } => close(&mut s.ledger, p, ctx, &claim, outcome, overpayment),
        Instruction::Reopen { claim, reserve_minor, .. } => reopen(&mut s.ledger, p, ctx, &claim, reserve_minor),
        Instruction::RaiseOverpayment {
            claim, overpayment, amount_minor, ..
        } => move_receivable(&mut s.ledger, p, ctx, &claim, "claim_overpayment_raised", overpayment, amount_minor),
        Instruction::RecoverOverpayment {
            claim, overpayment, amount_minor, ..
        } => move_receivable(&mut s.ledger, p, ctx, &claim, "claim_overpayment_recovered", overpayment, amount_minor),
        Instruction::WriteOffOverpayment {
            claim, overpayment, amount_minor, ..
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
    post(&mut state.ledger, policy, ctx, "claim_admitted", set, claim, &[("deducted_premium_minor", deduct.to_string())])?;
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

/// Pay from benefit payable. On the last payment the claim closes and any
/// reserve left is released. Where the premium payer was the deceased, what
/// the policy holds in suspense is paid with the claim.
#[allow(clippy::too_many_arguments)]
fn pay(ledger: &mut Ledger, policy: &mut Policy, ctx: &mut Ctx<'_>, claim: &str, payment_ref: String, amount: i64, payee: String, close: bool) -> Result<(), Reject> {
    let record = known(policy, claim)?;
    if amount <= 0 || amount > record.payable_minor || record.payments.contains_key(&payment_ref) || (close && amount != record.payable_minor) {
        return invalid("a payment is new, positive and within what is admitted and unpaid");
    }
    let c = policy.currency;
    let reserve = if close { record.reserve_minor } else { 0 };
    let (expense, d) = incurred(policy, &record.cover, "claim_paid");
    let mut set = PostingSet::new();
    set.debit(accounts::benefit_payable(&policy.id, c), i128::from(amount), d.clone());
    set.credit(accounts::settlement(c), i128::from(amount), Dimensions::default());
    set.debit(accounts::claim_reserve(&policy.id, c), i128::from(reserve), d.clone());
    set.credit(expense, i128::from(reserve), d);
    post(ledger, policy, ctx, "claim_paid", set, claim, &[("payment_ref", payment_ref.clone()), ("payee", payee)])?;
    if let Some(record) = policy.claims.get_mut(claim) {
        record.payable_minor -= amount;
        record.reserve_minor -= reserve;
        record.open = !close;
        record.payments.insert(
            payment_ref,
            ClaimPayment {
                amount_minor: amount,
                ex_gratia: false,
                returned: false,
            },
        );
    }
    let suspense = ledger.available(&accounts::policy_suspense(&policy.id, c).id);
    if policy.refund_with_claim.as_deref() == Some(claim) && suspense > 0 {
        let refund = Money::new(i64::try_from(suspense).unwrap_or(i64::MAX), c);
        cash::refund_suspense(ledger, policy, ctx, refund, Some(claim.to_string()))?;
    }
    Ok(())
}

/// One period of a benefit in payment, in one entry: drawn from the reserve
/// as far as it goes and from claims expense beyond it, paid to the claimant
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
            Some(id) => state.policies.get(id).filter(|p| p.currency == c && !p.is_frozen()).and_then(|p| p.claims.get(&o.claim)),
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
    set.credit(accounts::settlement(c), i128::from(gross - taken), Dimensions::default());
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
        record.open = !close;
        record.payments.insert(
            payment_ref,
            ClaimPayment {
                amount_minor: gross - taken,
                ex_gratia: false,
                returned: false,
            },
        );
    }
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

/// A payment failed or came back: the money returns to benefit payable,
/// never to suspense, and the claim is open again until it is paid.
fn payment_returned(ledger: &mut Ledger, policy: &mut Policy, ctx: &mut Ctx<'_>, claim: &str, payment_ref: &str) -> Result<(), Reject> {
    let record = known(policy, claim)?;
    let Some(amount) = record.payments.get(payment_ref).filter(|p| !p.returned).map(|p| p.amount_minor) else {
        return Err(Reject::new("unknown_payment", format!("claim {claim} has no payment {payment_ref} outstanding")));
    };
    let c = policy.currency;
    let (_, d) = incurred(policy, &record.cover, "claim_payment_returned");
    let mut set = PostingSet::new();
    set.debit(accounts::settlement(c), i128::from(amount), Dimensions::default());
    set.credit(accounts::benefit_payable(&policy.id, c), i128::from(amount), d);
    post(ledger, policy, ctx, "claim_payment_returned", set, claim, &[("payment_ref", payment_ref.to_string())])?;
    if let Some(record) = policy.claims.get_mut(claim) {
        record.payable_minor += amount;
        record.open = true;
        if let Some(p) = record.payments.get_mut(payment_ref) {
            p.returned = true;
        }
    }
    Ok(())
}

/// A goodwill payment: its own posting type and its own expense account.
#[allow(clippy::too_many_arguments)]
fn ex_gratia(ledger: &mut Ledger, policy: &mut Policy, ctx: &mut Ctx<'_>, claim: &str, cover: &str, payment_ref: String, amount: i64, payee: String) -> Result<(), Reject> {
    let record = known(policy, claim)?;
    if amount <= 0 || record.payments.contains_key(&payment_ref) {
        return invalid("an ex gratia payment is new and positive");
    }
    let c = policy.currency;
    let (_, d) = incurred(policy, cover, "ex_gratia");
    let mut set = PostingSet::new();
    set.debit(accounts::ex_gratia(&policy.product_code, c), i128::from(amount), d);
    set.credit(accounts::settlement(c), i128::from(amount), Dimensions::default());
    post(ledger, policy, ctx, "claim_ex_gratia", set, claim, &[("payment_ref", payment_ref.clone()), ("payee", payee)])?;
    if let Some(record) = policy.claims.get_mut(claim) {
        record.payments.insert(
            payment_ref,
            ClaimPayment {
                amount_minor: amount,
                ex_gratia: true,
                returned: false,
            },
        );
    }
    Ok(())
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
    let (reserve, payable) = (record.reserve_minor, record.payable_minor);
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
        record.payable_minor = 0;
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
fn move_receivable(ledger: &mut Ledger, policy: &mut Policy, ctx: &mut Ctx<'_>, claim: &str, kind: &str, overpayment: String, amount: i64) -> Result<(), Reject> {
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
            state.ledger.natural_balance(&accounts::benefit_payable(&p.id, c).id),
            state.ledger.natural_balance(&accounts::claim_recovery_receivable(&p.id, c).id),
        );
        if ledger != core {
            breaks.push(crate::recon::Break {
                check: "claims_ledger",
                detail: format!("policy {}: ledger {ledger:?}, claim records {core:?} (reserve, payable, receivable)", p.id),
            });
        }
        let m = module.get(&p.id.0).copied().unwrap_or_default();
        let claims = (i128::from(m.reserve_minor), i128::from(m.payable_minor), i128::from(m.receivable_minor));
        if claims != core {
            breaks.push(crate::recon::Break {
                check: "claims_control",
                detail: format!("policy {}: claims module {claims:?}, claim records {core:?} (reserve, payable, receivable)", p.id),
            });
        }
    }
}
