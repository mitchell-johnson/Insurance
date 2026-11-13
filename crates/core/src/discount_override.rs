//! Approved overrides: the per-person and one-off discount.
//!
//! An override is a command carrying a percentage, a scope, a reason code,
//! who approved it and a date range. The product says how large an override
//! one approver may give and how large any may be; above the first limit a
//! second, different approver is named, and above the second it is refused.
//! Who checked the approvers' own authority is the caller's business: the
//! core holds the product's limits and the record.
//!
//! An override runs after the product's own discounts, on what they left,
//! and is held to the product's cap and floor like any other. It is read as
//! in force on a billing period's first day; one dated today or earlier
//! adjusts the periods already billed, at its value date. It survives a
//! lapse, and stops only when its end date has passed or is brought forward.

use chrono::NaiveDate;
use ic_ledger::Ledger;
use ic_money::{Dec, DecStr};
use ic_product::discount::{Discount, DiscountBasis, DiscountCondition, Override};
use ic_product::Feature;
use serde::{Deserialize, Serialize};

use crate::catalog::Catalog;
use crate::event::Event;
use crate::machine::{Ctx, Reject};
use crate::policy::{Policy, PolicyStatus};
use crate::{LifeId, PolicyId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscountOverride {
    pub id: String,
    pub percent: DecStr,
    /// The covers it reduces. Empty: every cover.
    #[serde(default)]
    pub covers: Vec<String>,
    /// The one life whose cover it reduces. Absent: every life's.
    #[serde(default)]
    pub life: Option<LifeId>,
    pub reason: String,
    pub from: NaiveDate,
    /// The first day it no longer applies.
    pub until: NaiveDate,
    pub approver: String,
    #[serde(default)]
    pub second_approver: Option<String>,
}

/// Give an approved override, or bring the end date of one already given
/// forward.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetDiscountOverride {
    pub policy: PolicyId,
    pub terms: DiscountOverride,
}

impl SetDiscountOverride {
    pub fn allowed_callers(&self) -> &'static [&'static str] {
        &["collections", "policy-servicing"]
    }
}

/// The overrides in force on a day, as the product's chain reads them.
pub(crate) fn in_force_on(policy: &Policy, day: NaiveDate) -> Vec<Override> {
    policy
        .discount_overrides
        .iter()
        .filter(|o| o.from <= day && day < o.until)
        .map(|o| Override {
            discount: Discount {
                code: format!("override-{}", o.id),
                name: "Approved discount".to_string(),
                posting_type: "discount_override".to_string(),
                covers: o.covers.clone(),
                basis: DiscountBasis::Percent { percent: o.percent.clone() },
                condition: DiscountCondition::Always,
                max_annual: None,
            },
            life: o.life.as_ref().map(|l| l.to_string()),
        })
        .collect()
}

pub(crate) fn apply(ledger: &mut Ledger, catalog: &Catalog, policy: &mut Policy, ctx: &mut Ctx<'_>, command: SetDiscountOverride) -> Result<(), Reject> {
    let terms = command.terms;
    let plain = crate::interest::plain;
    let invalid = |message: &str| Err(Reject::new("invalid_override", message));
    if !plain(&terms.id) || !plain(&terms.reason) || !plain(&terms.approver) || terms.second_approver.as_ref().is_some_and(|a| !plain(a)) {
        return invalid("an override carries a plain reference, reason code and approvers");
    }
    if terms.until <= terms.from || terms.from < policy.inception {
        return invalid("an override runs from a date on or after the policy's start to a later date");
    }
    let before = policy.clone();
    let window = match policy.discount_overrides.iter_mut().find(|o| o.id == terms.id) {
        // Already given: only its end can change, and only to an earlier day.
        Some(given) => {
            let same = DiscountOverride {
                until: given.until,
                ..terms.clone()
            };
            if same != *given || terms.until > given.until {
                return Err(Reject::new(
                    "override_exists",
                    "an override already given can only have its end date brought forward",
                ));
            }
            let was = std::mem::replace(&mut given.until, terms.until);
            (terms.until, was)
        }
        None => {
            let product = catalog
                .product(&policy.product_hash)
                .ok_or_else(|| Reject::new("unknown_product", "product version not active"))?;
            let limits = product.features.iter().find_map(|f| match f {
                Feature::Discounts {
                    override_single_approver_percent,
                    override_max_percent: Some(max),
                    ..
                } => Some((override_single_approver_percent.as_ref().map(DecStr::get), max.get())),
                _ => None,
            });
            let Some((single, max)) = limits else {
                return Err(Reject::new("overrides_not_offered", "this product allows no approved discounts"));
            };
            let percent = terms.percent.get();
            if percent <= Dec::from(0i64) || percent > max {
                return Err(Reject::new(
                    "override_too_large",
                    "an approved discount is above nothing and within the most the product allows",
                ));
            }
            let needs_second = single.is_none_or(|limit| percent > limit);
            let second = terms.second_approver.as_ref().filter(|a| **a != terms.approver);
            if needs_second && second.is_none() {
                return Err(Reject::new(
                    "second_approver_required",
                    "a discount this large needs a second, different approver",
                ));
            }
            if terms.covers.iter().any(|c| product.cover(c).is_none()) {
                return Err(Reject::new("unknown_cover", "the override names a cover the product does not have"));
            }
            if terms.life.as_ref().is_some_and(|l| !policy.lives.contains_key(l)) {
                return Err(Reject::new("unknown_life", "the life is not on the policy"));
            }
            policy.discount_overrides.push(terms.clone());
            (terms.from, terms.until)
        }
    };
    if window.0 == window.1 {
        return Ok(());
    }
    ctx.emit(Event::DiscountOverrideSet {
        policy: policy.id.clone(),
        id: terms.id.clone(),
        percent: terms.percent.clone(),
        reason: terms.reason.clone(),
        from: terms.from,
        until: terms.until,
        approver: terms.approver.clone(),
        second_approver: terms.second_approver.clone(),
    });
    if window.0 <= ctx.date && policy.status == PolicyStatus::InForce {
        if let Some(life) = terms.life.or_else(|| policy.lives.keys().next().cloned()) {
            crate::reduce::adjust_billed(ledger, catalog, &before, policy, ctx, &life, (window.0, Some(window.1)))?;
        }
    }
    Ok(())
}
