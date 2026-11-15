//! Load Starlark configuration into typed, validated definitions.

use std::collections::{BTreeMap, BTreeSet};

use ic_money::Dec;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use starlark::environment::{Globals, Module};
use starlark::eval::Evaluator;
use starlark::syntax::{AstModule, Dialect};

use crate::discount::{DiscountBasis, DiscountCondition};
use crate::{Benefit, Definition, Feature, PremiumType, ProductDefinition, RateTable, UnmatchedSexRating};

const PRELUDE: &str = include_str!("prelude.star");
const FUND_PRELUDE: &str = include_str!("fund_prelude.star");

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("starlark: {0}")]
    Starlark(String),
    #[error("{file}: `{symbol}` is not defined")]
    MissingSymbol { file: String, symbol: &'static str },
    #[error("{file}: does not match the schema: {error}")]
    Schema { file: String, error: String },
    #[error("{file}: invalid: {error}")]
    Invalid { file: String, error: String },
}

fn content_hash(source: &str) -> String {
    hash_with(PRELUDE, source)
}

fn hash_with(prelude: &str, source: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"ic.config.v1\0");
    // The authoring kit is part of what the source means.
    h.update(Sha256::digest(prelude.as_bytes()));
    h.update(source.as_bytes());
    hex::encode(h.finalize())
}

fn eval<T: DeserializeOwned>(file: &str, source: &str, symbol: &'static str) -> Result<T, LoadError> {
    let json = eval_json(PRELUDE, file, source, symbol)?;
    serde_json::from_value(json).map_err(|e| LoadError::Schema {
        file: file.to_string(),
        error: e.to_string(),
    })
}

/// Load a fund module as data, with the hash of its source. The module must
/// define `FUND`. The investment ledger owns the fund's type and turns this
/// into a definition (`ic_core::invest::load_fund`).
pub fn load_fund_source(file: &str, source: &str) -> Result<(serde_json::Value, String), LoadError> {
    Ok((eval_json(FUND_PRELUDE, file, source, "FUND")?, hash_with(FUND_PRELUDE, source)))
}

fn eval_json(prelude_source: &str, file: &str, source: &str, symbol: &'static str) -> Result<serde_json::Value, LoadError> {
    let sl = |e: starlark::Error| LoadError::Starlark(e.to_string());
    let prelude = AstModule::parse("prelude.star", prelude_source.to_owned(), &Dialect::Standard).map_err(sl)?;
    let ast = AstModule::parse(file, source.to_owned(), &Dialect::Standard).map_err(sl)?;
    let globals = Globals::standard();
    let json = Module::with_temp_heap(|module| -> Result<serde_json::Value, LoadError> {
        {
            let mut ev = Evaluator::new(&module);
            ev.eval_module(prelude, &globals).map_err(sl)?;
            ev.eval_module(ast, &globals).map_err(sl)?;
        }
        let v = module.get(symbol).ok_or_else(|| LoadError::MissingSymbol {
            file: file.to_string(),
            symbol,
        })?;
        v.to_json_value().map_err(|e| LoadError::Starlark(e.to_string()))
    })?;
    Ok(json)
}

fn invalid(file: &str, error: impl Into<String>) -> LoadError {
    LoadError::Invalid {
        file: file.to_string(),
        error: error.into(),
    }
}

/// Load a product module. The module must define `PRODUCT`.
pub fn load_product(file: &str, source: &str) -> Result<ProductDefinition, LoadError> {
    let mut p: ProductDefinition = eval(file, source, "PRODUCT")?;
    p.content_hash = content_hash(source);
    validate_product(file, &mut p)?;
    Ok(p)
}

/// A cover's built-in benefits: distinct codes, a limit wherever money is
/// paid, a monthly multiple only on an income cover, and an optional benefit
/// only behind a feature some choice of the cover's options switches on.
fn validate_built_in(file: &str, c: &crate::CoverDefinition) -> Result<(), LoadError> {
    use crate::BuiltInKind;
    let features: BTreeSet<&str> = c
        .option_axes
        .iter()
        .flat_map(|a| &a.choices)
        .flat_map(|ch| &ch.terms.features)
        .map(String::as_str)
        .collect();
    let mut codes = BTreeSet::new();
    for b in &c.built_in {
        let fail = |why: &str| Err(invalid(file, format!("cover {}: built-in benefit {}: {why}", c.code, b.code)));
        if b.code.is_empty() || b.name.is_empty() || !codes.insert(b.code.as_str()) {
            return fail("needs a name and a code no other built-in benefit of the cover has");
        }
        let pays = !matches!(b.kind, BuiltInKind::Facility | BuiltInKind::PremiumWaiver);
        let limits = [b.max_minor.is_some(), b.percent_of_cover.is_some(), b.multiple_of_cover.is_some()];
        if pays && !limits.contains(&true) {
            return fail("pays money, so it sets the most it pays");
        }
        if !pays && limits.contains(&true) {
            return fail("pays nothing, so it sets no limit");
        }
        let positive = |d: &Option<ic_money::DecStr>| d.as_ref().is_none_or(|v| v.get() > Dec::from(0i64));
        if b.max_minor.is_some_and(|m| m <= 0) || !positive(&b.percent_of_cover) || !positive(&b.multiple_of_cover) {
            return fail("limits are above zero");
        }
        let income = matches!(c.benefit, Benefit::MonthlyBenefit { .. });
        if b.kind == BuiltInKind::MonthlyAddition && !income {
            return fail("is paid each month with an income benefit, so it belongs to an income cover");
        }
        if b.kind == BuiltInKind::EarlyPayment && !matches!(c.benefit, Benefit::LumpSum { .. } | Benefit::MonthlyBenefit { .. }) {
            return fail("is paid early out of the cover's benefit, so it belongs to a lump sum or income cover");
        }
        if b.requires_feature.as_deref().is_some_and(|f| !features.contains(f)) {
            return fail("names a feature none of the cover's options switches on");
        }
    }
    Ok(())
}

fn validate_product(file: &str, p: &mut ProductDefinition) -> Result<(), LoadError> {
    if p.code.is_empty() || p.version == 0 {
        return Err(invalid(file, "product needs a code and a version of at least 1"));
    }
    if p.covers.is_empty() {
        return Err(invalid(file, "product offers no covers"));
    }
    if p.cover_suspension.is_some_and(|s| s.max_months == 0 || s.max_months > 60) {
        return Err(invalid(file, "a cover suspension allows between 1 and 60 months in all"));
    }
    let mut codes = BTreeSet::new();
    for c in &p.covers {
        if !codes.insert(c.code.as_str()) {
            return Err(invalid(file, format!("duplicate cover code {}", c.code)));
        }
        if c.rating.premium_type == PremiumType::Level && c.rating.level_to_age.is_none() {
            return Err(invalid(file, format!("cover {}: level premium needs level_to_age", c.code)));
        }
        if c.rating.premium_type == PremiumType::GroupUnitRate && c.categories.is_empty() {
            return Err(invalid(file, format!("cover {}: group unit rates need benefit categories", c.code)));
        }
        for cat in &c.categories {
            let share = cat.member_share();
            if cat.member_pays_percent.is_some() && !share.is_some_and(|(top, bottom)| top > 0 && top <= bottom) {
                return Err(invalid(
                    file,
                    format!("cover {} category {}: member_pays_percent is above 0 and at most 100", c.code, cat.code),
                ));
            }
            // A member's part is a share of all their automatic cover, so a category pays
            // the same share on every cover that has it.
            let other = p
                .covers
                .iter()
                .flat_map(|o| o.categories.iter())
                .find(|o| o.code == cat.code && o.member_share() != share);
            if let Some(other) = other {
                return Err(invalid(
                    file,
                    format!(
                        "category {}: covers give it different member shares ({:?} and {:?})",
                        cat.code,
                        share,
                        other.member_share()
                    ),
                ));
            }
        }
        if let UnmatchedSexRating::Blended { female_share } = &c.rating.unmatched_sex_rating {
            if female_share.get() < Dec::from(0i64) || female_share.get() > Dec::from(1i64) {
                return Err(invalid(file, format!("cover {}: the blended share is a fraction between 0 and 1", c.code)));
            }
        }
        if let Some(g) = &c.gst_rate {
            if g.get() < Dec::from(0i64) || g.get() >= Dec::from(1i64) {
                return Err(invalid(file, format!("cover {}: gst_rate must be a fraction", c.code)));
            }
        }
        if c.max_sum_insured_minor.is_some_and(|m| m < c.min_sum_insured_minor) || c.min_sum_insured_minor < 0 {
            return Err(invalid(file, format!("cover {}: sum insured limits are inconsistent", c.code)));
        }
        if c.shared_counter
            .as_ref()
            .is_some_and(|name| name.is_empty() || !p.covers.iter().any(|o| o.code != c.code && o.shared_counter == c.shared_counter))
        {
            return Err(invalid(
                file,
                format!("cover {}: a shared counter is named and shared with another cover", c.code),
            ));
        }
        let lump_sum = |code: &str| p.covers.iter().any(|o| o.code == code && matches!(o.benefit, Benefit::LumpSum { .. }));
        if (c.joint_first_event || !c.reduces.is_empty()) && !lump_sum(&c.code) {
            return Err(invalid(file, format!("cover {}: only a lump sum cover is joint or reduces another", c.code)));
        }
        if c.reduces.iter().any(|other| other == &c.code || !lump_sum(other)) {
            return Err(invalid(
                file,
                format!("cover {}: a cover reduces other lump sum covers of the same product", c.code),
            ));
        }
        let mut options = BTreeSet::new();
        for o in &c.excess_options {
            if o.code.is_empty() || !options.insert(o.code.as_str()) {
                return Err(invalid(file, format!("cover {}: excess options need distinct codes", c.code)));
            }
            if o.excess_minor < 0 || o.premium_factor.get() <= Dec::from(0i64) {
                return Err(invalid(
                    file,
                    format!(
                        "cover {}: excess option {} needs an excess of zero or more and a premium factor above zero",
                        c.code, o.code
                    ),
                ));
            }
        }
        crate::options::validate_axes(c).map_err(|e| invalid(file, e.to_string()))?;
        if c.min_entry_age.zip(c.max_entry_age).is_some_and(|(min, max)| min > max) {
            return Err(invalid(file, format!("cover {}: the youngest entry age is above the oldest", c.code)));
        }
        validate_built_in(file, c)?;
        if let Benefit::InvestmentAccount { funds } = &c.benefit {
            if funds.is_empty() {
                return Err(invalid(file, format!("cover {}: investment account needs a fund menu", c.code)));
            }
        }
        if let Some(option) = &c.paid_up_option {
            let (zero, one) = (ic_money::Dec::from(0i64), ic_money::Dec::from(1i64));
            let within = |d: ic_money::Dec| d > zero && d <= one;
            let limited_pay = c
                .option_axes
                .iter()
                .flat_map(|a| &a.choices)
                .any(|ch| matches!(ch.terms.premium, Some(crate::options::PremiumBasis::LimitedPay { .. })));
            if !within(option.min_share_of_term.get()) || !within(option.factor.get()) || !limited_pay || !lump_sum(&c.code) {
                return Err(invalid(
                    file,
                    format!(
                        "cover {}: a paid-up option is on a lump sum cover with a limited-pay premium choice, and its share and factor are above zero and at most one",
                        c.code
                    ),
                ));
            }
        }
        if let Some(option) = &c.insurability_option {
            let plain = |e: &String| !e.is_empty() && e != crate::definition::ANNIVERSARY_EVENT && e.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_');
            let share = option.max_percent_of_original.get();
            let sound = !option.events.is_empty()
                && option.events.iter().all(plain)
                && option.events.iter().collect::<BTreeSet<_>>().len() == option.events.len()
                && share > ic_money::Dec::from(0i64)
                && option.max_increase_minor > 0
                && option.max_uses > 0
                && option.within_days > 0
                && option.to_age > 0
                && option.max_total_percent_of_original.as_ref().is_none_or(|t| t.get() >= share)
                && option.max_total_increase_minor.is_none_or(|t| t >= option.max_increase_minor)
                && option.once_in_days.is_none_or(|d| d > 0)
                && option.uses_in_window.is_none_or(|n| n > 0 && option.once_in_days.is_some())
                && option.days_before.is_none_or(|d| d > 0)
                && option.from_age.is_none_or(|a| a < option.to_age)
                && option.requires_feature.as_ref().is_none_or(|f| !f.is_empty())
                && option.not_before_days.is_none_or(|d| d > 0)
                && option.max_cover_after_minor.is_none_or(|m| m > 0);
            if !sound || !lump_sum(&c.code) {
                return Err(invalid(
                    file,
                    format!(
                        "cover {}: an insurability option is on a lump sum cover, names its events once each, and has limits above zero",
                        c.code
                    ),
                ));
            }
        }
        if let Some(option) = &c.anniversary_option {
            let zero = ic_money::Dec::from(0i64);
            let share = option.max_percent_of_original.get();
            let total = option.max_total_percent_of_original.get();
            let sound = option.every_years > 0
                && share > zero
                && option.at_least_minor > 0
                && total >= share
                && option.max_total_increase_minor >= option.at_least_minor
                && option.within_days > 0
                && option.to_age > 0
                && option.requires_feature.as_ref().is_none_or(|f| !f.is_empty())
                && option.once_in_days.is_none_or(|d| d > 0)
                && option.days_before.is_none_or(|d| d > 0)
                && option.from_age.is_none_or(|a| a < option.to_age);
            if !sound || !lump_sum(&c.code) {
                return Err(invalid(
                    file,
                    format!(
                        "cover {}: an anniversary option is on a lump sum cover, its limits are above zero and its totals at least one use",
                        c.code
                    ),
                ));
            }
        }
        if let Some(option) = &c.adds_cover {
            let plain = |e: &String| !e.is_empty() && e != crate::definition::ANNIVERSARY_EVENT && e.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_');
            let choices_offered = |code: &String, axes: &BTreeMap<String, String>| {
                p.covers.iter().find(|d| &d.code == code).is_some_and(|d| {
                    d.code != c.code
                        && lump_sum(&d.code)
                        && axes
                            .iter()
                            .all(|(axis, choice)| d.option_axes.iter().any(|a| &a.code == axis && a.choices.iter().any(|ch| &ch.code == choice)))
                })
            };
            let sound = !option.events.is_empty()
                && option.events.iter().all(plain)
                && option.events.iter().collect::<BTreeSet<_>>().len() == option.events.len()
                && !option.covers.is_empty()
                && option.covers.iter().all(|(code, axes)| choices_offered(code, axes))
                && option.max_percent_of_original.get() > ic_money::Dec::from(0i64)
                && option.max_increase_minor > 0
                && option.max_uses > 0
                && option.within_days > 0
                && option.to_age > 0
                && option.days_before.is_none_or(|d| d > 0)
                && option.from_age.is_none_or(|a| a < option.to_age)
                && option.once_in_days.is_none_or(|d| d > 0);
            if !sound || !lump_sum(&c.code) {
                return Err(invalid(
                    file,
                    format!(
                        "cover {}: a cover-adding option is on a lump sum cover, names its events once each, adds other lump sum covers of the product with choices they offer, and has limits above zero",
                        c.code
                    ),
                ));
            }
        }
        let mut restored = BTreeSet::new();
        for b in &c.buybacks {
            let sound = (b.restores == c.code || c.reduces.contains(&b.restores))
                && restored.insert(b.restores.as_str())
                && !b.requires_feature.is_empty()
                && !b.after_days.is_empty()
                && b.after_days.iter().all(|d| *d > 0)
                && b.within_days > 0
                && b.max_minor.is_none_or(|m| m > 0);
            if !sound || !lump_sum(&c.code) {
                return Err(invalid(
                    file,
                    format!(
                        "cover {}: a buy-back is on a lump sum cover and writes again that cover or one it reduces, once each, with a feature, waits and a window above zero",
                        c.code
                    ),
                ));
            }
        }
        let mut targets = BTreeSet::new();
        for conv in &c.conversions {
            let within = conv.to == c.code;
            let axis = conv.axis.as_ref().and_then(|a| c.option_axes.iter().find(|x| &x.code == a));
            let sound = targets.insert(conv.to.as_str())
                && conv.before_age > 0
                && if within {
                    axis.is_some_and(|a| {
                        !conv.choices.is_empty()
                            && conv.choices.iter().all(|ch| a.choices.iter().any(|x| &x.code == ch))
                            && conv.choices.iter().collect::<BTreeSet<_>>().len() == conv.choices.len()
                    }) && conv.with_cover.is_none()
                } else {
                    conv.axis.is_none() && conv.choices.is_empty() && lump_sum(&conv.to)
                }
                && conv.with_cover.as_ref().is_none_or(|w| w != &c.code && w != &conv.to && lump_sum(w));
            if !sound || !(within || lump_sum(&c.code)) {
                return Err(invalid(
                    file,
                    format!(
                        "cover {}: a conversion is to choices the cover's own axis offers, or from a lump sum cover to another lump sum cover, once to each cover, with an age above zero",
                        c.code
                    ),
                ));
            }
        }
    }
    let mut kinds = BTreeSet::new();
    for f in &p.features {
        if !kinds.insert(f.order()) {
            return Err(invalid(file, "a lifecycle feature is declared twice"));
        }
        match f {
            Feature::Billing {
                frequencies,
                policy_fee_annual,
                ..
            } => {
                if frequencies.is_empty() {
                    return Err(invalid(file, "billing offers no frequency"));
                }
                if policy_fee_annual.get() < Dec::from(0i64) {
                    return Err(invalid(file, "policy fee is negative"));
                }
            }
            Feature::Arrears {
                grace_days,
                lapse_days,
                reinstatement_days,
                reinstatement_evidence_free_days,
                ..
            } => {
                if lapse_days < grace_days {
                    return Err(invalid(file, "lapse_days is shorter than grace_days"));
                }
                if reinstatement_evidence_free_days.is_some_and(|free| free > *reinstatement_days) {
                    return Err(invalid(file, "reinstatement_evidence_free_days is longer than reinstatement_days"));
                }
            }
            Feature::MultiCoverDiscount { percent, .. } => {
                if percent.get() < Dec::from(0i64) || percent.get() > Dec::from(100i64) {
                    return Err(invalid(file, "discount percent out of range"));
                }
            }
            Feature::Discounts {
                chain,
                max_total_percent,
                min_net_annual,
                override_single_approver_percent,
                override_max_percent,
            } => {
                let pct = |p: &ic_money::DecStr| p.get() >= Dec::from(0i64) && p.get() <= Dec::from(100i64);
                let amount = |a: &ic_money::DecStr| a.get() >= Dec::from(0i64);
                let limits_ok = override_single_approver_percent.as_ref().is_none_or(pct) && override_max_percent.as_ref().is_none_or(pct);
                if !limits_ok || !max_total_percent.as_ref().is_none_or(pct) || !min_net_annual.as_ref().is_none_or(amount) {
                    return Err(invalid(file, "the cap on all discounts is a percentage and the floor is not negative"));
                }
                let mut codes = BTreeSet::new();
                for d in chain.iter().flat_map(|s| s.discounts()) {
                    if d.code.is_empty() || d.name.is_empty() || d.posting_type.is_empty() || !codes.insert(d.code.as_str()) {
                        return Err(invalid(
                            file,
                            format!("discount {}: a discount has its own code, a name and a posting type", d.code),
                        ));
                    }
                    if d.covers.iter().any(|c| !p.covers.iter().any(|o| &o.code == c)) {
                        return Err(invalid(file, format!("discount {}: it names a cover the product does not have", d.code)));
                    }
                    // A component that would increase the premium is a loading or a fee, never a discount.
                    let never_negative = match &d.basis {
                        DiscountBasis::Percent { percent } => pct(percent),
                        DiscountBasis::FixedAnnual { amount: a } => amount(a),
                        DiscountBasis::Tier { percent_by_tier } => {
                            !percent_by_tier.is_empty() && percent_by_tier.values().all(pct) && matches!(d.condition, DiscountCondition::Entitlement { .. })
                        }
                    };
                    if !never_negative || !d.max_annual.as_ref().is_none_or(amount) {
                        return Err(invalid(
                            file,
                            format!(
                                "discount {}: a discount is between nothing and the whole premium, and a tier needs an entitlement",
                                d.code
                            ),
                        ));
                    }
                    let counted = match &d.condition {
                        DiscountCondition::MinCovers { count } | DiscountCondition::MinGroupCovers { count } | DiscountCondition::MinLives { count } => {
                            *count > 0
                        }
                        DiscountCondition::Frequency { frequencies } => !frequencies.is_empty(),
                        DiscountCondition::Entitlement { code } => !code.is_empty(),
                        DiscountCondition::Always => true,
                        DiscountCondition::BenefitCategories { required, categories, count } => {
                            let all: Vec<_> = required.iter().chain(categories).collect();
                            let named = all.iter().flat_map(|c| &c.covers).all(|c| p.covers.iter().any(|o| &o.code == c));
                            let distinct = all.iter().map(|c| c.code.as_str()).collect::<BTreeSet<_>>().len() == all.len();
                            let elsewhere = |e: &String| {
                                e.split_once('/')
                                    .is_some_and(|(product, cover)| !product.is_empty() && !cover.is_empty() && !cover.contains('/'))
                            };
                            let sound = all
                                .iter()
                                .all(|c| !c.code.is_empty() && !c.covers.is_empty() && c.min_sum_insured_minor >= 0 && c.elsewhere.iter().all(elsewhere));
                            if !named {
                                return Err(invalid(
                                    file,
                                    format!("discount {}: a benefit category names a cover the product does not have", d.code),
                                ));
                            }
                            !required.is_empty() && distinct && sound && *count as usize <= categories.len()
                        }
                    };
                    if !counted {
                        return Err(invalid(file, format!("discount {}: its condition can never be told apart from none", d.code)));
                    }
                }
                if chain.iter().any(|s| s.discounts().is_empty()) {
                    return Err(invalid(file, "a set of discounts has at least one member"));
                }
            }
            Feature::Composition {
                core,
                requires,
                exclusive,
                requires_any,
            } => {
                let named = core
                    .iter()
                    .chain(requires.keys())
                    .chain(requires.values().flatten())
                    .chain(requires_any.keys())
                    .chain(requires_any.values().flatten())
                    .chain(exclusive.iter().flatten());
                for code in named {
                    if !codes.contains(code.as_str()) {
                        return Err(invalid(file, format!("composition names cover {code}, which the product does not offer")));
                    }
                }
                if requires.iter().chain(requires_any).any(|(cover, needed)| needed.contains(cover)) {
                    return Err(invalid(file, "a cover cannot require itself"));
                }
                if requires_any.values().any(Vec::is_empty) {
                    return Err(invalid(file, "a cover held with one of a set names at least one cover"));
                }
            }
            Feature::PolicyLoans {
                max_percent_of_value,
                rate_code,
            } => {
                let share = max_percent_of_value.get();
                if share <= ic_money::Dec::from(0i64) || share > ic_money::Dec::from(100i64) || rate_code.is_empty() {
                    return Err(invalid(
                        file,
                        "policy loans are for a share of the value above nothing and up to all of it, at a named rate",
                    ));
                }
            }
            Feature::Earning { .. } | Feature::Indexation { .. } | Feature::CoolingOff { .. } => {}
        }
    }
    for (needed, name) in [(30u8, "billing"), (40, "earning"), (50, "arrears")] {
        if !kinds.contains(&needed) {
            return Err(invalid(file, format!("product has no {name} feature")));
        }
    }
    // Features run in the engine's defined order, whatever order they were listed in.
    p.features.sort_by_key(Feature::order);
    Ok(())
}

/// Load a rate table module. The module must define `RATE_TABLE`.
pub fn load_rate_table(file: &str, source: &str) -> Result<RateTable, LoadError> {
    let mut t: RateTable = eval(file, source, "RATE_TABLE")?;
    t.content_hash = content_hash(source);
    if t.id.is_empty() || t.version == 0 || t.rows.is_empty() {
        return Err(invalid(file, "rate table needs an id, a version and at least one row"));
    }
    for r in &t.rows {
        if r.age_to < r.age_from || r.annual_rate.get() < Dec::from(0i64) {
            return Err(invalid(file, "rate row has an inverted age band or a negative rate"));
        }
    }
    Ok(t)
}

/// Load a definitions library module. The module must define `DEFINITIONS`.
pub fn load_definitions(file: &str, source: &str) -> Result<Vec<Definition>, LoadError> {
    let mut defs: Vec<Definition> = eval(file, source, "DEFINITIONS")?;
    let mut seen = BTreeSet::new();
    for d in &mut defs {
        if d.text.is_empty() {
            return Err(invalid(file, format!("definition {} has no wording", d.id)));
        }
        if !seen.insert((d.id.clone(), d.version)) {
            return Err(invalid(file, format!("definition {} v{} is declared twice", d.id, d.version)));
        }
        d.text_hash = hex::encode(Sha256::digest(d.text.as_bytes()));
    }
    Ok(defs)
}
