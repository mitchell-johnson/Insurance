//! The remedies for non-disclosure: avoiding a policy, and varying its terms.
//!
//! Both are decided by an approved non-disclosure case in the underwriting
//! module and reach the core as commands naming that case. The core does
//! not judge the remedy; it carries it out and keeps the books whole.
//!
//! **Avoid.** The policy is treated as never having been in force: every
//! layer ends on the day it started. The case says what becomes of the
//! premiums. Refunded, every premium billed since inception is undone
//! (earned, unearned, tax and levies) and what the policyholder paid goes to
//! policy suspense, from where it is refunded like any other credit. Kept,
//! what was paid stays with the insurer: premium not yet earned is taken to
//! its own income account, tax already collected stays collected, and what
//! was still owed is written off.
//!
//! **Vary.** The layers named read as written on the varied terms from their
//! start. Nothing already billed is billed again; later periods price on the
//! varied terms. What each layer was written on is kept.

use std::collections::BTreeMap;

use ic_ledger::{Dimensions, Ledger};
use ic_money::AccrualAmount;

use crate::accounts::{self, dims};
use crate::arrears;
use crate::catalog::Catalog;
use crate::event::Event;
use crate::lifecycle::close_out;
use crate::machine::{Ctx, Reject};
use crate::policy::{AvoidanceRecord, CoverStatus, LayerTerms, Policy, PolicyStatus, PremiumOnAvoidance, Variation};
use crate::postings::PostingSet;
use crate::{earning, LifeId};

/// A case reference: short and plain, never free text.
fn plain(s: &str) -> bool {
    !s.is_empty() && s.len() <= 120 && s.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':' | '/'))
}

fn retail(policy: &Policy) -> Result<(), Reject> {
    if policy.covers.values().any(|c| c.formula_driven) {
        return Err(Reject::new(
            "not_retail",
            "a scheme is not avoided or varied as a whole; the member's own cover is ended or altered",
        ));
    }
    Ok(())
}

pub(crate) fn avoid(
    ledger: &mut Ledger,
    catalog: &Catalog,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    case: String,
    premium: PremiumOnAvoidance,
) -> Result<(), Reject> {
    if !plain(&case) {
        return Err(Reject::new("invalid_case", "an avoidance names the non-disclosure case that decided it"));
    }
    // The same decision arriving again changes nothing.
    if policy.avoidance.as_ref().is_some_and(|a| a.case == case && a.premium == premium) {
        return Ok(());
    }
    if policy.status != PolicyStatus::InForce {
        return Err(Reject::new("not_in_force", "only a policy in force can be avoided"));
    }
    retail(policy)?;
    let c = policy.currency;
    if ledger.natural_balance(&accounts::benefit_payable(&policy.id, c).id) != 0 {
        return Err(Reject::new(
            "claim_in_payment",
            "a benefit is owed on this policy; the claim is settled or set aside before the policy is avoided",
        ));
    }
    // A policy in force has no closed receivables. One that does was closed
    // by something this command does not know how to undo.
    if policy.receivables.iter().any(|r| r.closed) {
        return Err(Reject::new(
            "avoidance_needs_review",
            "part of this policy's billing was closed earlier; a person works it out",
        ));
    }
    let product = catalog
        .product(&policy.product_hash)
        .ok_or_else(|| Reject::new("unknown_product", "product version not active"))?
        .clone();
    let suspense = accounts::policy_suspense(&policy.id, c);
    let held_before = ledger.natural_balance(&suspense.id);
    let mut detail = BTreeMap::new();
    detail.insert("case".to_string(), case.clone());
    match premium {
        PremiumOnAvoidance::Refund => {
            // Everything still on the policy is undone as in the cooling-off
            // period: earned premium and its tax are returned with the rest.
            close_out(
                ledger,
                &product,
                policy,
                ctx,
                true,
                "policy_avoided",
                ctx.date,
                policy.inception,
                detail.clone(),
            )?;
            // And everything billed and earned before that, from the totals
            // kept when those receivables left the policy's state.
            let pruned = std::mem::take(&mut policy.pruned);
            let mut set = PostingSet::new();
            let mut back_fine: i128 = 0;
            for (cover_type, fine) in &pruned.earned_fine {
                set.debit(
                    accounts::earned_premium(&policy.product_code, cover_type, c),
                    *fine,
                    dims(policy, Some(cover_type), "avoidance"),
                );
                back_fine += fine;
            }
            let mut back_minor = pruned.gst_minor as i128;
            set.debit(accounts::gst_payable(c), pruned.gst_minor as i128, dims(policy, None, "gst"));
            for (code, v) in &pruned.levies_minor {
                set.debit(accounts::levy_payable(code, c), *v as i128, dims(policy, None, "levy"));
                back_minor += *v as i128;
            }
            let (net, rem) = AccrualAmount::new(back_fine, c).round_to_money(product.rounding)?;
            set.credit(suspense.clone(), net.minor as i128 + back_minor, Dimensions::default());
            set.credit(accounts::rounding(c), rem.fine, dims(policy, None, "rounding"));
            let postings = set.into_postings(ledger)?;
            if !postings.is_empty() {
                ctx.post(ledger, "policy_avoided_earlier_premium", Some(policy), ctx.date, postings, detail)?;
            }
        }
        PremiumOnAvoidance::Retain => {
            if let Some(yesterday) = ctx.date.pred_opt() {
                earning::earn_through(ledger, policy, ctx, yesterday)?;
            }
            let mut set = PostingSet::new();
            let (_, kept_fine) = arrears::write_off(policy, &mut set, |_| true);
            set.credit(
                accounts::avoidance_premium_retained(&policy.product_code, c),
                kept_fine,
                dims(policy, None, "avoidance"),
            );
            let postings = set.into_postings(ledger)?;
            if !postings.is_empty() {
                ctx.post(ledger, "policy_avoided", Some(policy), ctx.date, postings, detail)?;
            }
            for r in policy.receivables.iter_mut() {
                r.outstanding_minor = 0;
                r.closed = true;
            }
            for cover in policy.covers.values_mut() {
                cover.status = CoverStatus::Ended;
                for l in cover.layers.iter_mut().filter(|l| l.end.is_none()) {
                    l.end = Some(l.start);
                }
            }
        }
    }
    let refunded_minor = i64::try_from(ledger.natural_balance(&suspense.id) - held_before).unwrap_or(0);
    policy.avoidance = Some(AvoidanceRecord {
        case: case.clone(),
        avoided_on: ctx.date,
        premium,
        refunded_minor,
    });
    arrears::set_standing(policy, ctx, None);
    ctx.emit(Event::PolicyStatusChanged {
        policy: policy.id.clone(),
        from: policy.status,
        to: PolicyStatus::Avoided,
    });
    policy.status = PolicyStatus::Avoided;
    ctx.emit(Event::PolicyAvoided {
        policy: policy.id.clone(),
        case,
        premium,
        refunded_minor,
    });
    Ok(())
}

pub(crate) fn vary(policy: &mut Policy, ctx: &mut Ctx<'_>, case: String, terms: Vec<LayerTerms>) -> Result<(), Reject> {
    if !plain(&case) {
        return Err(Reject::new("invalid_case", "a variation names the non-disclosure case that decided it"));
    }
    if policy.variations.iter().any(|v| v.case == case) {
        return Ok(());
    }
    if policy.status != PolicyStatus::InForce {
        return Err(Reject::new("not_in_force", "only a policy in force can be varied"));
    }
    retail(policy)?;
    if terms.iter().any(|t| t.loadings.iter().any(|l| l.get().is_negative())) {
        return Err(Reject::new("invalid_loading", "a loading cannot be negative"));
    }
    // Worked on a copy, so a refusal part way leaves the policy as it was.
    let mut draft = policy.clone();
    let mut varied: Vec<(String, LifeId, u32)> = Vec::new();
    for t in &terms {
        let cover = draft
            .covers
            .get_mut(&t.cover)
            .filter(|c| c.status == CoverStatus::InForce)
            .ok_or_else(|| Reject::new("unknown_cover", format!("{} is not a cover in force on this policy", t.cover)))?;
        // Every layer that gave the life cover for at least a day.
        let mut found = false;
        for l in cover.layers.iter_mut().filter(|l| l.life == t.life && l.end.is_none_or(|e| e > l.start)) {
            found = true;
            if l.loadings == t.loadings && l.exclusions == t.exclusions {
                continue;
            }
            draft.variations.push(Variation {
                case: case.clone(),
                recorded_on: ctx.date,
                cover: t.cover.clone(),
                layer: l.id,
                loadings_before: std::mem::replace(&mut l.loadings, t.loadings.clone()),
                exclusions_before: std::mem::replace(&mut l.exclusions, t.exclusions.clone()),
            });
            varied.push((t.cover.clone(), t.life.clone(), l.id));
        }
        if !found {
            return Err(Reject::new("unknown_life", format!("life {} has no {} cover", t.life, t.cover)));
        }
    }
    if varied.is_empty() {
        return Err(Reject::new("no_change", "the cover is already on these terms"));
    }
    *policy = draft;
    ctx.emit(Event::TermsVaried {
        policy: policy.id.clone(),
        case,
        layers: varied,
    });
    Ok(())
}
