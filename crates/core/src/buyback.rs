//! Buy-back: cover a claim took off written again without evidence.
//!
//! A lump sum cover may declare buy-backs (`buyback` in the product): after
//! a claim on it took cover off a life (its own sum insured, or the cover it
//! is accelerated against), the life may have that cover written again,
//! once, on the terms the claim took it from, at today's rates. The request
//! names the claim, the cover written again and which survival period the
//! condition claimed for carries; the core checks the life chose the option,
//! the claim is closed, the window, the amount and that it is the first.

use chrono::NaiveDate;
use ic_ledger::Ledger;
use serde::{Deserialize, Serialize};

use crate::catalog::Catalog;
use crate::command::UnderwrittenLayer;
use crate::event::Event;
use crate::interest::plain;
use crate::machine::{Ctx, Reject};
use crate::policy::{Policy, PolicyStatus};
use crate::{LifeId, PolicyId};

/// The owner asks for cover a claim took off to be written again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuyBack {
    pub policy: PolicyId,
    /// The claim whose payment took the cover off.
    pub claim: String,
    /// The cover written again: the claimed cover, or one it is accelerated against.
    pub restores: String,
    pub life: LifeId,
    /// The survival period the condition claimed for carries, in days: one
    /// of the product's.
    pub after_days: u32,
    /// The request's own reference. The same one again changes nothing.
    pub reference: String,
}

/// A buy-back made, kept on the policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuybackRecord {
    pub claim: String,
    /// The cover the claim was on.
    pub cover: String,
    pub restores: String,
    pub life: LifeId,
    pub on: NaiveDate,
    pub sum_insured_minor: i64,
    pub layer: u32,
}

pub(crate) fn buy_back(ledger: &mut Ledger, catalog: &Catalog, policy: &mut Policy, ctx: &mut Ctx<'_>, spec: BuyBack) -> Result<(), Reject> {
    if !plain(&spec.reference) || !plain(&spec.claim) {
        return Err(Reject::new("invalid_request", "a request has a plain reference and names a claim"));
    }
    if let Some(done) = policy.buybacks.get(&spec.reference) {
        return if done.claim == spec.claim && done.restores == spec.restores && done.life == spec.life {
            Ok(())
        } else {
            Err(Reject::new("duplicate_request", format!("reference {} bought back other cover", spec.reference)))
        };
    }
    if policy.status != PolicyStatus::InForce {
        return Err(Reject::new("not_in_force", "cover is written again only on a policy in force"));
    }
    let Some(record) = policy.claims.get(&spec.claim) else {
        return Err(Reject::new("unknown_claim", format!("claim {} is not on the policy", spec.claim)));
    };
    if record.open {
        return Err(Reject::new("claim_open", "cover is bought back once the claim that took it is closed"));
    }
    let claimed = record.cover.clone();
    let product = catalog
        .product(&policy.product_hash)
        .cloned()
        .ok_or_else(|| Reject::new("unknown_product", "the policy's product version is not active"))?;
    let def = product
        .cover(&claimed)
        .ok_or_else(|| Reject::new("unknown_cover", format!("the product has no cover {claimed}")))?;
    let Some(option) = def.buybacks.iter().find(|b| b.restores == spec.restores) else {
        return Err(Reject::new(
            "no_buyback",
            format!("a claim on {claimed} gives no right to buy back {}", spec.restores),
        ));
    };
    // The option is chosen on the life's cover that was claimed on.
    let chose = policy
        .covers
        .get(&claimed)
        .and_then(|c| c.layers.iter().filter(|l| l.life == spec.life).max_by_key(|l| (l.start, l.id)))
        .and_then(|l| l.excess_option.as_deref())
        .and_then(|code| def.option(code).ok())
        .is_some_and(|o| o.has_feature(&option.requires_feature));
    if !chose {
        return Err(Reject::new(
            "no_buyback",
            format!("the cover for life {} was taken without {}", spec.life, option.requires_feature),
        ));
    }
    if policy.buybacks.values().any(|b| b.life == spec.life && b.restores == spec.restores && b.cover == claimed) {
        return Err(Reject::new("buyback_used", "the cover is bought back only once"));
    }
    let cuts: Vec<_> = record.cuts.iter().filter(|c| c.cover == spec.restores && c.life == spec.life).cloned().collect();
    let taken: i64 = cuts.iter().map(|c| c.amount_minor).sum();
    let Some(off) = cuts.iter().map(|c| c.on).min() else {
        return Err(Reject::new(
            "nothing_taken",
            format!("claim {} took no {} off life {}", spec.claim, spec.restores, spec.life),
        ));
    };
    if !option.after_days.contains(&spec.after_days) {
        return Err(Reject::new("buyback_window", format!("the survival period is one of {:?} days", option.after_days)));
    }
    let from = off + chrono::Days::new(u64::from(spec.after_days));
    if ctx.date < from || (ctx.date - from).num_days() > i64::from(option.within_days) {
        return Err(Reject::new(
            "buyback_window",
            format!("cover is bought back from {from} for {} days", option.within_days),
        ));
    }
    let held: i64 = policy
        .covers
        .get(&spec.restores)
        .map(|c| c.layers.iter().filter(|l| l.life == spec.life && l.in_force_on(ctx.date)).map(|l| l.sum_insured_minor).sum())
        .unwrap_or(0);
    if option.only_after_nil && held > 0 {
        return Err(Reject::new("not_nil", "this buy-back follows only a claim that took the whole cover"));
    }
    let amount = option.max_minor.map_or(taken, |m| taken.min(m));
    // The terms are those of the layer the claim took the cover from.
    let source = policy
        .covers
        .get(&spec.restores)
        .and_then(|c| c.layers.iter().find(|l| cuts.iter().any(|x| x.layer == l.id)))
        .cloned()
        .ok_or_else(|| Reject::new("nothing_taken", "the layer the claim took is not on the policy"))?;
    crate::underwritten::add_layer(
        ledger,
        catalog,
        policy,
        ctx,
        UnderwrittenLayer {
            policy: spec.policy.clone(),
            cover: spec.restores.clone(),
            life: spec.life.clone(),
            sum_insured_minor: amount,
            loadings: source.loadings.clone(),
            exclusions: source.exclusions.clone(),
            reference: spec.reference.clone(),
            start: None,
        },
    )?;
    let layer = policy
        .covers
        .get(&spec.restores)
        .and_then(|c| c.layers.iter().map(|l| l.id).max())
        .unwrap_or(0);
    ctx.emit(Event::CoverBoughtBack {
        policy: policy.id.clone(),
        claim: spec.claim.clone(),
        cover: claimed.clone(),
        restores: spec.restores.clone(),
        life: spec.life.clone(),
        layer,
        sum_insured_minor: amount,
        reference: spec.reference.clone(),
    });
    policy.buybacks.insert(
        spec.reference,
        BuybackRecord {
            claim: spec.claim,
            cover: claimed,
            restores: spec.restores,
            life: spec.life,
            on: ctx.date,
            sum_insured_minor: amount,
            layer,
        },
    );
    Ok(())
}
