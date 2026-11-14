//! A cover's menu of options: the terms a customer chooses for each life's
//! cover, such as an income cover's waiting and benefit periods, its premium
//! structure, or an optional benefit that rides on it.
//!
//! A cover offers either a flat list of excess levels (`excess_options`,
//! one choice) or one or more axes of choices (`option_axes`), never both.
//! With axes, a life's choice names one choice on every axis, in the axes'
//! order: `waiting=w4,period=b2y,premium=stepped`. The premium factor of the
//! choice is the product of each axis choice's factor, and the terms are
//! the union of what each choice sets; no two choices of one selection may
//! set the same term.
//!
//! The core keeps the chosen code on each layer (the field is called
//! `excess_option` for the menu it first served), so a change of terms is a
//! new layer from the day it applies and the past keeps its own.

use std::collections::BTreeSet;

use ic_money::{Dec, DecStr};
use serde::{Deserialize, Serialize};

use crate::{CoverDefinition, PremiumType};

/// How a layer's premium is worked out when an option chooses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PremiumBasis {
    /// Recalculated at each anniversary from the life's age then.
    Stepped,
    /// Fixed at the life's age when the layer was first rated, until this
    /// age; on attained age after it.
    Level { to_age: u32 },
    /// Fixed for blocks of this many years from the layer's rating date,
    /// each block priced at the life's age when it began.
    LevelTerm { years: u32 },
}

/// What one choice sets. Each term is optional; a term no choice sets is
/// the cover's own.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptionTerms {
    /// The excess the customer pays of a claim, replacing the schedule's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excess_minor: Option<i64>,
    /// An income benefit's waiting period, in days.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_days: Option<u32>,
    /// How long an income benefit is paid for one claim, in months.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub benefit_months: Option<u32>,
    /// An income benefit is paid no later than the life's birthday at this age.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub benefit_to_age: Option<u32>,
    /// The cover ends at the anniversary on or after the life reaches this age.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiry_age: Option<u32>,
    /// The cover ends at the anniversary this many years after it began.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiry_after_years: Option<u32>,
    /// The premium structure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub premium: Option<PremiumBasis>,
    /// Optional benefits switched on, by code: built-in benefits that name
    /// one apply only where it is chosen, and claims read them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
}

/// One choice on an axis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptionChoice {
    pub code: String,
    /// The name a quote or schedule shows.
    pub name: String,
    /// What the cover's premium is multiplied by for this choice.
    pub premium_factor: DecStr,
    #[serde(default)]
    pub terms: OptionTerms,
}

/// One axis of a cover's options: a question with a set of answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptionAxis {
    pub code: String,
    pub name: String,
    pub choices: Vec<OptionChoice>,
}

/// A life's option on a cover, resolved from the menu.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CoverOption {
    pub code: String,
    /// What the cover's premium is multiplied by, loadings included.
    pub premium_factor: Dec,
    pub terms: OptionTerms,
}

impl CoverOption {
    pub fn has_feature(&self, feature: &str) -> bool {
        self.terms.features.iter().any(|f| f == feature)
    }
}

/// Why a menu or a choice from it is not sound.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OptionError {
    #[error("the cover offers no such choice: {0}")]
    Unknown(String),
    #[error("{0}")]
    Menu(String),
}

/// Join a selection into the code a layer keeps, in the axes' order.
pub fn option_code(selection: &[(&str, &str)]) -> String {
    selection.iter().map(|(axis, choice)| format!("{axis}={choice}")).collect::<Vec<_>>().join(",")
}

fn plain(code: &str) -> bool {
    !code.is_empty() && code.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// Check a cover's axes when the product is loaded.
pub(crate) fn validate_axes(cover: &CoverDefinition) -> Result<(), OptionError> {
    let menu = |m: String| Err(OptionError::Menu(format!("cover {}: {m}", cover.code)));
    if cover.option_axes.is_empty() {
        return Ok(());
    }
    if !cover.excess_options.is_empty() {
        return menu("a cover offers excess levels or option axes, not both".into());
    }
    let mut axes = BTreeSet::new();
    for axis in &cover.option_axes {
        if !plain(&axis.code) || !axes.insert(axis.code.as_str()) {
            return menu(format!("axis codes are plain and distinct ({})", axis.code));
        }
        if axis.choices.is_empty() {
            return menu(format!("axis {} offers no choice", axis.code));
        }
        let mut choices = BTreeSet::new();
        for c in &axis.choices {
            if !plain(&c.code) || !choices.insert(c.code.as_str()) {
                return menu(format!("axis {}: choice codes are plain and distinct ({})", axis.code, c.code));
            }
            if c.premium_factor.get() <= Dec::from(0i64) {
                return menu(format!("axis {}: choice {} needs a premium factor above zero", axis.code, c.code));
            }
            let t = &c.terms;
            let zero_or_less = [t.waiting_days, t.benefit_months, t.benefit_to_age, t.expiry_age, t.expiry_after_years]
                .into_iter()
                .flatten()
                .any(|v| v == 0);
            let bad_premium = match t.premium {
                Some(PremiumBasis::Level { to_age }) => to_age == 0,
                Some(PremiumBasis::LevelTerm { years }) => years == 0,
                _ => false,
            };
            if zero_or_less || bad_premium || t.excess_minor.is_some_and(|e| e < 0) || t.features.iter().any(|f| !plain(f)) {
                return menu(format!("axis {}: choice {} sets a term out of range", axis.code, c.code));
            }
            let income = matches!(cover.benefit, crate::Benefit::MonthlyBenefit { .. });
            if !income && (t.waiting_days.is_some() || t.benefit_months.is_some() || t.benefit_to_age.is_some()) {
                return menu(format!("axis {}: choice {} sets income terms on a cover that pays no income", axis.code, c.code));
            }
            if t.premium.is_some() && cover.rating.premium_type == PremiumType::GroupUnitRate {
                return menu(format!("axis {}: a scheme's unit rates have no premium structure to choose", axis.code));
            }
        }
    }
    Ok(())
}

/// Merge one choice's terms into what earlier axes set.
fn merge(into: &mut OptionTerms, from: &OptionTerms, code: &str) -> Result<(), OptionError> {
    fn one<T: Clone>(slot: &mut Option<T>, value: &Option<T>, code: &str, term: &str) -> Result<(), OptionError> {
        match (slot.is_some(), value) {
            (true, Some(_)) => Err(OptionError::Menu(format!("two choices in {code} both set {term}"))),
            (_, Some(v)) => {
                *slot = Some(v.clone());
                Ok(())
            }
            (_, None) => Ok(()),
        }
    }
    one(&mut into.excess_minor, &from.excess_minor, code, "the excess")?;
    one(&mut into.waiting_days, &from.waiting_days, code, "the waiting period")?;
    one(&mut into.benefit_months, &from.benefit_months, code, "the benefit period")?;
    one(&mut into.benefit_to_age, &from.benefit_to_age, code, "the benefit period")?;
    one(&mut into.expiry_age, &from.expiry_age, code, "the expiry")?;
    one(&mut into.expiry_after_years, &from.expiry_after_years, code, "the expiry")?;
    one(&mut into.premium, &from.premium, code, "the premium structure")?;
    for f in &from.features {
        if !into.features.contains(f) {
            into.features.push(f.clone());
        }
    }
    Ok(())
}

impl CoverDefinition {
    /// Resolve a life's choice against the cover's menu. A flat excess list
    /// resolves one of its levels; axes resolve one choice on each axis, in
    /// order.
    pub fn option(&self, code: &str) -> Result<CoverOption, OptionError> {
        let unknown = || OptionError::Unknown(code.to_string());
        if !self.excess_options.is_empty() {
            let o = self.excess_option(code).ok_or_else(unknown)?;
            return Ok(CoverOption {
                code: o.code.clone(),
                premium_factor: o.premium_factor.get(),
                terms: OptionTerms {
                    excess_minor: Some(o.excess_minor),
                    ..OptionTerms::default()
                },
            });
        }
        if self.option_axes.is_empty() {
            return Err(unknown());
        }
        let parts: Vec<&str> = code.split(',').collect();
        if parts.len() != self.option_axes.len() {
            return Err(unknown());
        }
        let mut resolved = CoverOption {
            code: code.to_string(),
            premium_factor: Dec::from(1i64),
            terms: OptionTerms::default(),
        };
        for (axis, part) in self.option_axes.iter().zip(parts) {
            let (name, choice) = part.split_once('=').ok_or_else(unknown)?;
            if name != axis.code {
                return Err(unknown());
            }
            let c = axis.choices.iter().find(|c| c.code == choice).ok_or_else(unknown)?;
            resolved.premium_factor *= c.premium_factor.get();
            merge(&mut resolved.terms, &c.terms, code)?;
        }
        Ok(resolved)
    }

    /// Whether the cover offers a menu a life must choose from.
    pub fn has_options(&self) -> bool {
        !self.excess_options.is_empty() || !self.option_axes.is_empty()
    }

    /// The premium structure a layer at this choice is priced on.
    pub fn premium_basis(&self, choice: Option<&str>) -> Option<PremiumBasis> {
        let chosen = choice.and_then(|c| self.option(c).ok()).and_then(|o| o.terms.premium);
        chosen.or(match self.rating.premium_type {
            PremiumType::Stepped => Some(PremiumBasis::Stepped),
            PremiumType::Level => self.rating.level_to_age.map(|to_age| PremiumBasis::Level { to_age }),
            PremiumType::GroupUnitRate => None,
        })
    }

    /// Whether any layer of this cover can be priced on a level basis.
    pub fn may_be_level(&self) -> bool {
        self.rating.premium_type == PremiumType::Level
            || self
                .option_axes
                .iter()
                .flat_map(|a| &a.choices)
                .any(|c| matches!(c.terms.premium, Some(PremiumBasis::Level { .. } | PremiumBasis::LevelTerm { .. })))
    }

    /// Whether any layer of this cover can reach an end by age or term.
    pub fn may_expire(&self) -> bool {
        self.expiry_age.is_some()
            || self
                .option_axes
                .iter()
                .flat_map(|a| &a.choices)
                .any(|c| c.terms.expiry_age.is_some() || c.terms.expiry_after_years.is_some())
    }

    /// The age at which a layer at this choice ends, where one is set.
    pub fn expiry_age_for(&self, choice: Option<&str>) -> Option<u32> {
        choice
            .and_then(|c| self.option(c).ok())
            .and_then(|o| o.terms.expiry_age)
            .or(self.expiry_age)
    }

    /// The years after the cover began at which a layer at this choice ends.
    pub fn expiry_after_years_for(&self, choice: Option<&str>) -> Option<u32> {
        choice.and_then(|c| self.option(c).ok()).and_then(|o| o.terms.expiry_after_years)
    }
}

/// The age a premium is read from: level blocks re-rate at the start of each.
pub fn rating_age(basis: PremiumBasis, age_at_rating_start: u32, attained: u32) -> u32 {
    match basis {
        PremiumBasis::Stepped => attained,
        PremiumBasis::Level { to_age } if attained >= to_age => attained,
        PremiumBasis::Level { .. } => age_at_rating_start,
        PremiumBasis::LevelTerm { years } => {
            let elapsed = attained.saturating_sub(age_at_rating_start);
            age_at_rating_start + elapsed / years * years
        }
    }
}
