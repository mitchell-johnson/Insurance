//! Cover a scheme member pays for themselves.
//!
//! A member-paid layer, or the loading on a layer where the member pays only
//! that, is billed to the member and not to the employer: it has its own
//! receivables on the scheme policy, marked with the member as payer, and
//! the member's own standing. Money the member pays is set only against what
//! the member owes, and what is left over is held as the member's credit in
//! an account of its own. Nothing of it reaches the scheme invoice, the
//! scheme's balance or the scheme's standing.

use std::collections::{BTreeMap, BTreeSet};

use ic_ledger::{Account, Dimensions, Ledger};
use ic_money::{AccrualAmount, Money};
use ic_product::ProductDefinition;

use crate::accounts::{self, dims};
use crate::arrears::{self, Derived};
use crate::cash::{allocate, apply_allocations};
use crate::event::Event;
use crate::machine::{Ctx, Reject};
use crate::policy::{MemberLapse, Policy, PolicyStatus, ReceiptRecord, Receivable, Standing};
use crate::postings::PostingSet;
use crate::LifeId;

/// The account holding a member's own money, noted on the policy so that a
/// transfer knows it exists.
fn credit_account(policy: &mut Policy, life: &LifeId) -> Account {
    policy.member_accounts.insert(life.clone());
    accounts::member_suspense(&policy.id, life, policy.currency)
}

/// A member's credit balance.
pub fn credit_minor(ledger: &Ledger, policy: &Policy, life: &LifeId) -> i128 {
    ledger.available(&accounts::member_suspense(&policy.id, life, policy.currency).id)
}

fn member(policy: &Policy, life: &LifeId) -> Result<(), Reject> {
    if policy.lives.contains_key(life) {
        Ok(())
    } else {
        Err(Reject::new("unknown_member", format!("{life} is not a member of this scheme")))
    }
}

/// Money from a member for cover the member pays for themselves.
pub(crate) fn apply_receipt(
    ledger: &mut Ledger,
    catalog: &crate::catalog::Catalog,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    life: LifeId,
    receipt: String,
    amount: Money,
) -> Result<(), Reject> {
    member(policy, &life)?;
    if amount.currency != policy.currency || amount.minor <= 0 {
        return Err(Reject::new("invalid_amount", "a receipt must be a positive amount in the policy's currency"));
    }
    if policy.receipts.contains_key(&receipt) {
        return Err(Reject::new("duplicate_receipt", format!("receipt {receipt} has already been applied")));
    }
    let c = policy.currency;
    let allocations = if policy.status == PolicyStatus::InForce {
        allocate(policy, amount.minor, Some(&life))
    } else {
        BTreeMap::new()
    };
    let allocated: i64 = allocations.values().sum();
    let to_credit = amount.minor - allocated;
    let mut set = PostingSet::new();
    set.debit(accounts::settlement(c), amount.minor as i128, Dimensions::default());
    set.credit(accounts::premium_receivable(&policy.id, c), allocated as i128, Dimensions::default());
    if to_credit > 0 {
        set.credit(credit_account(policy, &life), to_credit as i128, Dimensions::default());
    }
    let mut detail = BTreeMap::new();
    detail.insert("receipt".to_string(), receipt.clone());
    let postings = set.into_postings(ledger)?;
    ctx.post(ledger, "member_premium_received", Some(policy), ctx.date, postings, detail)?;
    let net_by_cover_type = crate::counterparty::net_by_cover_type(&policy.receivables, &allocations);
    apply_allocations(policy, &allocations, 1);
    policy.receipts.insert(
        receipt.clone(),
        ReceiptRecord {
            received_on: ctx.date,
            amount_minor: amount.minor,
            allocations,
            to_suspense_minor: to_credit,
            reversed: false,
            member: Some(life.clone()),
        },
    );
    ctx.emit(Event::MemberReceiptApplied {
        policy: policy.id.clone(),
        life: life.clone(),
        receipt,
        allocated_minor: allocated,
        to_credit_minor: to_credit,
        net_by_cover_type,
    });
    if let Some(product) = catalog.product(&policy.product_hash) {
        refresh(product, policy, ctx, &life);
    }
    Ok(())
}

/// Set a member's credit against what the member owes, oldest first.
pub(crate) fn apply_credit(ledger: &mut Ledger, policy: &mut Policy, ctx: &mut Ctx<'_>, life: &LifeId) -> Result<(), Reject> {
    if policy.status != PolicyStatus::InForce || !policy.member_accounts.contains(life) {
        return Ok(());
    }
    let balance = credit_minor(ledger, policy, life);
    if balance <= 0 {
        return Ok(());
    }
    let allocations = allocate(policy, i64::try_from(balance).unwrap_or(i64::MAX), Some(life));
    let allocated: i64 = allocations.values().sum();
    if allocated == 0 {
        return Ok(());
    }
    let c = policy.currency;
    let mut set = PostingSet::new();
    set.debit(accounts::member_suspense(&policy.id, life, c), allocated as i128, Dimensions::default());
    set.credit(accounts::premium_receivable(&policy.id, c), allocated as i128, Dimensions::default());
    let postings = set.into_postings(ledger)?;
    ctx.post(ledger, "member_credit_applied", Some(policy), ctx.date, postings, BTreeMap::new())?;
    let net_by_cover_type = crate::counterparty::net_by_cover_type(&policy.receivables, &allocations);
    apply_allocations(policy, &allocations, 1);
    ctx.emit(Event::MemberCreditApplied {
        policy: policy.id.clone(),
        life: life.clone(),
        amount_minor: allocated,
        net_by_cover_type,
    });
    Ok(())
}

/// Pay a member's credit back to the member.
pub(crate) fn refund_credit(ledger: &mut Ledger, policy: &mut Policy, ctx: &mut Ctx<'_>, life: LifeId, amount: Money) -> Result<(), Reject> {
    member(policy, &life)?;
    let c = policy.currency;
    if amount.currency != c || amount.minor <= 0 {
        return Err(Reject::new("invalid_amount", "a refund must be a positive amount in the policy's currency"));
    }
    if credit_minor(ledger, policy, &life) < amount.minor as i128 {
        return Err(Reject::new("insufficient_credit", "the member does not hold that much in credit"));
    }
    let mut set = PostingSet::new();
    set.debit(accounts::member_suspense(&policy.id, &life, c), amount.minor as i128, Dimensions::default());
    set.credit(accounts::settlement(c), amount.minor as i128, Dimensions::default());
    let postings = set.into_postings(ledger)?;
    ctx.post(ledger, "member_credit_refunded", Some(policy), ctx.date, postings, BTreeMap::new())?;
    ctx.emit(Event::MemberCreditRefunded {
        policy: policy.id.clone(),
        life,
        amount_minor: amount.minor,
    });
    Ok(())
}

/// Close what one member owes, as a lapse closes a policy's receivables:
/// what is unpaid is written off, and premium the member paid for days not
/// yet covered goes back to the member's credit. Returns what was owed.
pub(crate) fn close(ledger: &mut Ledger, product: &ProductDefinition, policy: &mut Policy, ctx: &mut Ctx<'_>, life: &LifeId) -> Result<i64, Reject> {
    Ok(close_keeping(ledger, product, policy, ctx, life)?.0)
}

/// [`close`], returning also the entry's postings, for a lapse to keep.
fn close_keeping(
    ledger: &mut Ledger,
    product: &ProductDefinition,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    life: &LifeId,
) -> Result<(i64, Vec<ic_ledger::Posting>), Reject> {
    let c = policy.currency;
    let theirs = |r: &crate::policy::Receivable| r.payer.as_ref() == Some(life);
    if !policy.receivables.iter().any(|r| !r.closed && theirs(r)) {
        return Ok((0, vec![]));
    }
    let mut set = PostingSet::new();
    let (owed, leftover_fine) = arrears::write_off(policy, &mut set, theirs);
    let (refund, rem) = AccrualAmount::new(leftover_fine, c).round_to_money(product.rounding)?;
    if refund.minor != 0 {
        set.credit(credit_account(policy, life), refund.minor as i128, Dimensions::default());
    }
    set.credit(accounts::rounding(c), rem.fine, dims(policy, None, "rounding"));
    let postings = set.into_postings(ledger)?;
    if !postings.is_empty() {
        ctx.post(ledger, "member_cover_closed", Some(policy), ctx.date, postings.clone(), BTreeMap::new())?;
    }
    for r in policy.receivables.iter_mut().filter(|r| !r.closed && r.payer.as_ref() == Some(life)) {
        r.outstanding_minor = 0;
        r.closed = true;
    }
    Ok((owed, postings))
}

/// Close what every member owes, before the scheme itself lapses or ends:
/// the scheme's close-out then deals only with the employer's money.
pub(crate) fn close_all(ledger: &mut Ledger, product: &ProductDefinition, policy: &mut Policy, ctx: &mut Ctx<'_>) -> Result<(), Reject> {
    let payers: BTreeSet<LifeId> = policy.receivables.iter().filter(|r| !r.closed).filter_map(|r| r.payer.clone()).collect();
    for life in payers {
        close(ledger, product, policy, ctx, &life)?;
    }
    for life in std::mem::take(&mut policy.member_standing).into_keys() {
        announce(policy, ctx, &life, None);
    }
    Ok(())
}

/// The purpose under which documents records delivery of a member's lapse
/// notice (`RecordNoticeDelivery`).
pub fn member_lapse_notice(life: &LifeId) -> String {
    format!("member_lapse:{life}")
}

/// Whether a member in force on `on` pays a share of their automatic cover.
fn part_pays(product: &ProductDefinition, policy: &Policy, life: &LifeId, on: chrono::NaiveDate) -> bool {
    policy
        .lives
        .get(life)
        .filter(|l| l.in_force_on(on))
        .and_then(|l| crate::premium::factors_on(l, on).ok())
        .is_some_and(|f| crate::premium::member_share(product, f).is_some())
}

/// Tell payments that nothing more is to be collected from this member.
pub(crate) fn end_collection(policy: &Policy, ctx: &mut Ctx<'_>, life: LifeId, reason: &str) {
    ctx.emit(Event::PaymentAuthorityCancelled {
        policy: policy.id.clone(),
        claim: reason.to_string(),
        life: Some(life),
    });
}

/// A member who has left, was billed for cover of their own and owes
/// nothing more: their collection can end now. One who still owes is left to
/// the arrears run, which ends it when it writes the debt off.
pub(crate) fn end_collection_on_leaving(policy: &Policy, ctx: &mut Ctx<'_>, life: &LifeId) {
    let theirs = |r: &&crate::policy::Receivable| r.payer.as_ref() == Some(life);
    let billed = policy.receivables.iter().any(|r| theirs(&r));
    let owing = policy.receivables.iter().filter(theirs).any(|r| !r.closed && r.outstanding_minor > 0);
    if billed && !owing {
        end_collection(policy, ctx, life.clone(), "member_left");
    }
}

/// Record a member's standing and say so when it changed.
fn announce(policy: &mut Policy, ctx: &mut Ctx<'_>, life: &LifeId, to: Option<Standing>) {
    let from = match to {
        Some(s) => policy.member_standing.insert(life.clone(), s),
        None => policy.member_standing.remove(life),
    };
    if from != to {
        ctx.emit(Event::MemberStandingChanged {
            policy: policy.id.clone(),
            life: life.clone(),
            from,
            to,
            oldest_unpaid_due: policy.member_oldest_unpaid_due(life),
        });
    }
}

/// Re-derive one member's standing after a bill, a payment or a reversal,
/// without lapsing: that happens only in the end-of-day run.
pub(crate) fn refresh(product: &ProductDefinition, policy: &mut Policy, ctx: &mut Ctx<'_>, life: &LifeId) {
    let Some(rules) = product.arrears().filter(|_| policy.status == PolicyStatus::InForce) else {
        return;
    };
    let billed = policy.receivables.iter().any(|r| !r.closed && r.payer.as_ref() == Some(life));
    if !billed && !policy.member_standing.contains_key(life) {
        return;
    }
    let to = match arrears::derive_from(policy.member_oldest_unpaid_due(life), rules, ctx.date) {
        Derived::Standing(s) => s,
        Derived::Lapse => policy.member_standing.get(life).copied().unwrap_or(Standing::InGrace),
    };
    announce(policy, ctx, life, Some(to));
}

/// The end-of-day arrears run for the members of a scheme who are billed
/// for cover of their own. A member who has not paid within the product's
/// lapse period loses that cover and nothing else: the cover the employer
/// pays for, and every other member, are untouched.
pub(crate) fn run_arrears(
    ledger: &mut Ledger,
    catalog: &crate::catalog::Catalog,
    product: &ProductDefinition,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
) -> Result<(), Reject> {
    let Some(rules) = product.arrears().filter(|_| policy.status == PolicyStatus::InForce) else {
        return Ok(());
    };
    let mut members: BTreeSet<LifeId> = policy.member_standing.keys().cloned().collect();
    members.extend(policy.receivables.iter().filter(|r| !r.closed).filter_map(|r| r.payer.clone()));
    for life in members {
        match arrears::derive_from(policy.member_oldest_unpaid_due(&life), rules, ctx.date) {
            Derived::Standing(s) => announce(policy, ctx, &life, Some(s)),
            // As a policy waits for its lapse notice, a member's own cover waits
            // for theirs, recorded by documents under `member_lapse:<life>`.
            Derived::Lapse if policy.notices.get(&member_lapse_notice(&life)).is_some_and(|n| n.earliest_effect > ctx.date) => {}
            Derived::Lapse => {
                ctx.work += 40;
                let (layers, effective) = crate::group::end_member_paid_layers(ledger, catalog, product, policy, ctx, &life)?;
                // On a part-pay scheme the member's automatic cover is theirs in part:
                // not paying for it ends it, so they leave the scheme's cover today.
                let left = part_pays(product, policy, &life, effective);
                if left {
                    let leave = crate::command::MemberChange::Left { life: life.clone(), effective };
                    crate::group::apply_membership(ledger, catalog, policy, ctx, vec![leave])?;
                }
                let receivables_before: Vec<Receivable> = policy
                    .receivables
                    .iter()
                    .filter(|r| !r.closed && r.payer.as_ref() == Some(&life))
                    .cloned()
                    .collect();
                let (arrears_minor, postings) = close_keeping(ledger, product, policy, ctx, &life)?;
                if rules.reinstatement_days > 0 {
                    policy.member_lapses.insert(
                        life.clone(),
                        MemberLapse {
                            lapsed_on: effective,
                            layers: layers.clone(),
                            postings,
                            receivables_before,
                            arrears_minor,
                            left,
                        },
                    );
                }
                announce(policy, ctx, &life, None);
                ctx.emit(Event::MemberCoverLapsed {
                    policy: policy.id.clone(),
                    life: life.clone(),
                    layers,
                    effective,
                    arrears_minor,
                });
                end_collection(policy, ctx, life, "member_lapsed");
            }
        }
    }
    Ok(())
}

/// Reinstate a member's own cover that lapsed for non-payment, inside the
/// product's reinstatement window. The member must hold, as credit, what the
/// lapse wrote off and the premium for every period billed since: the
/// write-off is reversed, the cover is in force again from the day it ended
/// (no gap), the periods since are billed to the member, and the credit pays
/// all of it at once. After the product's evidence-free period it needs
/// underwriting, which is not built for a member's cover, so it is refused.
pub(crate) fn reinstate(
    ledger: &mut Ledger,
    catalog: &crate::catalog::Catalog,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    life: LifeId,
    reference: String,
) -> Result<(), Reject> {
    member(policy, &life)?;
    if policy.status != PolicyStatus::InForce {
        return Err(Reject::new("not_in_force", "the scheme is not in force"));
    }
    if reference.trim().is_empty() {
        return Err(Reject::new("invalid_request", "a reinstatement names its reference"));
    }
    let Some(lapse) = policy.member_lapses.get(&life).cloned() else {
        return Err(Reject::new(
            "not_lapsed",
            format!("member {life} has no lapsed cover of their own to reinstate"),
        ));
    };
    let product = catalog
        .product(&policy.product_hash)
        .ok_or_else(|| Reject::new("unknown_product", "product version not active"))?
        .clone();
    let rules = product
        .arrears()
        .ok_or_else(|| Reject::new("window_closed", "the product has no reinstatement"))?;
    let since = (ctx.date - lapse.lapsed_on).num_days();
    if since > i64::from(rules.reinstatement_days) {
        return Err(Reject::new(
            "window_closed",
            "the reinstatement window has closed; the member must apply for cover again",
        ));
    }
    if rules.reinstatement_evidence_free_days.is_some_and(|free| since > i64::from(free)) {
        return Err(Reject::new("underwriting_required", "lapsed too long to reinstate on payment alone"));
    }
    // Priced first, with nothing written, so a refusal leaves no trace.
    let billed = if lapse.left {
        crate::group::rejoin_cost(catalog, policy, ctx.date, ctx.index, &life)?
    } else {
        crate::group::restore_member_paid_layers(ledger, catalog, &product, policy, ctx, &lapse.layers, lapse.lapsed_on, false)?
    };
    let required = lapse.arrears_minor + billed.max(0);
    let held = credit_minor(ledger, policy, &life);
    if held < i128::from(required) {
        return Err(Reject::new(
            "arrears_unpaid",
            format!("reinstatement needs {required} in the member's credit; the member holds {held}"),
        ));
    }
    if !lapse.postings.is_empty() {
        let reversal = lapse.postings.iter().map(ic_ledger::Posting::reversed).collect();
        let mut detail = BTreeMap::new();
        detail.insert("reference".to_string(), reference.clone());
        ctx.post(ledger, "member_lapse_reversed", Some(policy), ctx.date, reversal, detail)?;
    }
    for before in lapse.receivables_before {
        if let Some(r) = policy.receivables.iter_mut().find(|r| r.id == before.id) {
            *r = before;
        }
    }
    if lapse.left {
        // Back as a member from the day they left: their automatic cover and any cover
        // they pay for, every period since billed as of today.
        let rejoin = crate::command::MemberChange::Reinstated { life: life.clone() };
        crate::group::apply_membership(ledger, catalog, policy, ctx, vec![rejoin])?;
    } else {
        crate::group::restore_member_paid_layers(ledger, catalog, &product, policy, ctx, &lapse.layers, lapse.lapsed_on, true)?;
    }
    policy.member_lapses.remove(&life);
    apply_credit(ledger, policy, ctx, &life)?;
    ctx.emit(Event::MemberCoverReinstated {
        policy: policy.id.clone(),
        life: life.clone(),
        reference,
        layers: lapse.layers,
        effective: lapse.lapsed_on,
        arrears_minor: lapse.arrears_minor,
        billed_minor: billed,
    });
    refresh(&product, policy, ctx, &life);
    Ok(())
}
