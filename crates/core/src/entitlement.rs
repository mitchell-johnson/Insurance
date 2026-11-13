//! Entitlements: what a life or a policy holds that a discount is given for.
//!
//! A module that judges outside data (a wellness programme, an employer or
//! affinity membership, a loyalty scheme) tells the core one thing per
//! change: this life, or this policy, holds this entitlement at this tier
//! from this date to that date. The core never sees the data behind it. A
//! manual award ends in the same command, with its reason as the reference,
//! so who holds a discount and why can be reported from the record.
//!
//! The product's discounts read the entitlements held on the first day of
//! a billing period, so a change dated in the future takes effect from the
//! next billing and nothing already billed is touched. A change dated today
//! or in the past is the programme's own decision to reach back: the
//! periods already billed are adjusted for the days from that date, each
//! adjustment posted at its value date.

use chrono::NaiveDate;
use ic_ledger::Ledger;
use ic_product::discount::{DiscountCondition, Held};
use ic_product::{Feature, ProductDefinition};
use serde::{Deserialize, Serialize};

use crate::catalog::Catalog;
use crate::event::Event;
use crate::machine::{Ctx, Reject};
use crate::policy::{Policy, PolicyStatus};
use crate::{LifeId, PolicyId};

/// One holding, kept for good: a later change ends it, never edits it away.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entitlement {
    pub code: String,
    /// Empty where the entitlement has no tiers.
    pub tier: String,
    /// The life that holds it. Absent: the policy holds it, for every life.
    pub life: Option<LifeId>,
    pub from: NaiveDate,
    /// The first day it is no longer held.
    pub until: Option<NaiveDate>,
    /// The module that awarded it and its own reference or reason.
    pub source: String,
    pub reference: String,
    pub recorded_on: NaiveDate,
}

impl Entitlement {
    fn held_on(&self, day: NaiveDate) -> bool {
        self.from <= day && self.until.is_none_or(|u| u > day)
    }
}

/// A module says what is held from a date: a tier awarded, changed or lost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetEntitlement {
    pub policy: PolicyId,
    pub code: String,
    /// The tier held, for an entitlement that has tiers.
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub life: Option<LifeId>,
    /// False: from the date it is no longer held.
    pub held: bool,
    pub from: NaiveDate,
    #[serde(default)]
    pub until: Option<NaiveDate>,
    pub reference: String,
}

impl SetEntitlement {
    pub fn allowed_callers(&self) -> &'static [&'static str] {
        &["entitlements", "wellness"]
    }
}

/// The entitlement codes a product's discounts are given for.
fn codes(product: &ProductDefinition) -> Vec<&str> {
    let chains = product.features.iter().filter_map(|f| match f {
        Feature::Discounts { chain, .. } => Some(chain),
        _ => None,
    });
    chains
        .flatten()
        .flat_map(|step| step.discounts())
        .filter_map(|d| match &d.condition {
            DiscountCondition::Entitlement { code } => Some(code.as_str()),
            _ => None,
        })
        .collect()
}

/// What the policy and its lives hold on a day, as the discounts read it.
pub(crate) fn held_on(policy: &Policy, day: NaiveDate) -> Vec<Held> {
    policy
        .entitlements
        .iter()
        .filter(|e| e.held_on(day))
        .map(|e| Held {
            code: e.code.clone(),
            tier: e.tier.clone(),
            life: e.life.as_ref().map(|l| l.to_string()),
        })
        .collect()
}

pub(crate) fn apply(ledger: &mut Ledger, catalog: &Catalog, policy: &mut Policy, ctx: &mut Ctx<'_>, command: SetEntitlement) -> Result<(), Reject> {
    let plain = crate::interest::plain;
    let invalid = |message: &str| Err(Reject::new("invalid_entitlement", message));
    if !plain(&command.code) || !plain(&command.reference) || command.tier.as_ref().is_some_and(|t| !plain(t)) {
        return invalid("an entitlement carries a plain code, tier and reference, never the data behind them");
    }
    if command.until.is_some_and(|u| u <= command.from) || (!command.held && (command.until.is_some() || command.tier.is_some())) {
        return invalid("an entitlement is held from a date to a later one, and one that is lost has neither a tier nor an end");
    }
    if command.from < policy.inception {
        return invalid("an entitlement starts no earlier than the policy");
    }
    if command.life.as_ref().is_some_and(|l| !policy.lives.contains_key(l)) {
        return Err(Reject::new("unknown_life", "the life is not on the policy"));
    }
    let product = catalog
        .product(&policy.product_hash)
        .ok_or_else(|| Reject::new("unknown_product", "product version not active"))?;
    if !codes(product).contains(&command.code.as_str()) {
        return Err(Reject::new(
            "unknown_entitlement",
            format!("no discount of this product is given for {}", command.code),
        ));
    }

    let before = policy.clone();
    // From the date on, this is what is held: what was recorded for later is
    // replaced, and what was running is ended there.
    let same = |e: &Entitlement| e.code == command.code && e.life == command.life;
    policy.entitlements.retain(|e| !(same(e) && e.from >= command.from));
    for e in policy.entitlements.iter_mut().filter(|e| same(e) && e.until.is_none_or(|u| u > command.from)) {
        e.until = Some(command.from);
    }
    if command.held {
        policy.entitlements.push(Entitlement {
            code: command.code.clone(),
            tier: command.tier.clone().unwrap_or_default(),
            life: command.life.clone(),
            from: command.from,
            until: command.until,
            source: ctx.caller.to_string(),
            reference: command.reference.clone(),
            recorded_on: ctx.date,
        });
    }
    if before.entitlements == policy.entitlements {
        return Ok(());
    }
    ctx.emit(Event::EntitlementChanged {
        policy: policy.id.clone(),
        code: command.code.clone(),
        tier: command.tier.clone(),
        life: command.life.clone(),
        held: command.held,
        from: command.from,
        until: command.until,
        reference: command.reference,
    });
    // A change dated today or earlier reaches the periods already billed.
    if command.from <= ctx.date && policy.status == PolicyStatus::InForce {
        let life = command.life.or_else(|| policy.lives.keys().next().cloned());
        if let Some(life) = life {
            let window = (command.from, command.until);
            crate::reduce::adjust_billed(ledger, catalog, &before, policy, ctx, &life, window)?;
        }
    }
    Ok(())
}
