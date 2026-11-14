use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_money::{Currency, DecStr, RoundingMode};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineOfBusiness {
    Life,
    Health,
    Group,
    Investment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Frequency {
    Fortnightly,
    Monthly,
    Quarterly,
    HalfYearly,
    Yearly,
}

impl Frequency {
    pub fn periods_per_year(self) -> i64 {
        match self {
            Frequency::Fortnightly => 26,
            Frequency::Monthly => 12,
            Frequency::Quarterly => 4,
            Frequency::HalfYearly => 2,
            Frequency::Yearly => 1,
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Frequency::Fortnightly => "fortnightly",
            Frequency::Monthly => "monthly",
            Frequency::Quarterly => "quarterly",
            Frequency::HalfYearly => "half_yearly",
            Frequency::Yearly => "yearly",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sex {
    Female,
    Male,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenefitLimit {
    pub category: String,
    pub per_event_minor: Option<i64>,
    pub per_year_minor: Option<i64>,
    #[serde(default)]
    pub excess_minor: i64,
    /// The most paid for the life over the whole life of the policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_lifetime_minor: Option<i64>,
}

/// An income benefit while the life is in custody after sentence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WhileImprisoned {
    /// No benefit for whole days in custody; the claim, the premium waiver
    /// and the cover carry on.
    #[default]
    Suspend,
    /// The benefit is paid as if the life were not in custody.
    Pay,
}

/// What a cover pays. Claims assessment is outside the core; the benefit only
/// says what is payable once an event meets the definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Benefit {
    LumpSum {
        survival_days: u32,
        ends_cover: bool,
    },
    MonthlyBenefit {
        waiting_days: u32,
        benefit_months: u32,
        /// What the wording does with the benefit while the life is in
        /// custody after sentence (`docs/decisions.md`, decision 5).
        #[serde(default)]
        benefit_while_imprisoned: WhileImprisoned,
    },
    HealthSchedule {
        limits: Vec<BenefitLimit>,
    },
    InvestmentAccount {
        funds: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PremiumType {
    /// Recalculated at each anniversary from current age.
    Stepped,
    /// Fixed to a set age.
    Level,
    /// Unit rates per benefit category, recalculated at scheme renewal.
    GroupUnitRate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RateBasis {
    /// Annual rate per thousand of sum insured.
    PerThousand,
    /// Annual rate per life.
    PerLife,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rating {
    pub premium_type: PremiumType,
    /// Rate table ID. The version in force is resolved through activation.
    pub table: String,
    pub basis: RateBasis,
    #[serde(default)]
    pub level_to_age: Option<u32>,
    /// How a life is rated when the table rates by sex and the life's
    /// rating sex is not recorded, as for a life who is neither male nor
    /// female (`docs/decisions.md`, decision 1).
    #[serde(default)]
    pub unmatched_sex_rating: UnmatchedSexRating,
}

/// The basis for rating a life whose sex a sex-rated table does not hold.
/// Gender, what the person tells us, is kept by the customer master and is
/// never a rating factor; this is about the rating factor being absent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UnmatchedSexRating {
    /// The lower of the table's female and male rates for the life's other
    /// facts: nobody pays more for not picking a category.
    #[default]
    LowerOf,
    /// `female_share` of the female rate and the rest of the male rate.
    Blended { female_share: DecStr },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Levy {
    pub code: String,
    pub rate: DecStr,
}

/// A group scheme benefit category: a formula over member data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenefitCategory {
    pub code: String,
    pub salary_multiple: DecStr,
    pub max_sum_insured_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverDefinition {
    pub code: String,
    /// `life`, `trauma`, `income_protection`, `health`, `group_life`, `investment`.
    pub cover_type: String,
    pub benefit: Benefit,
    pub rating: Rating,
    /// Definition IDs from the definitions library. A cover is bound to the
    /// version in force at its start date.
    #[serde(default)]
    pub definitions: Vec<String>,
    /// GST rate where the cover is taxable (health), none for life.
    #[serde(default)]
    pub gst_rate: Option<DecStr>,
    #[serde(default)]
    pub levies: Vec<Levy>,
    #[serde(default)]
    pub expiry_age: Option<u32>,
    #[serde(default)]
    pub min_sum_insured_minor: i64,
    #[serde(default)]
    pub max_sum_insured_minor: Option<i64>,
    #[serde(default)]
    pub categories: Vec<BenefitCategory>,
    #[serde(default)]
    pub automatic_acceptance_limit_minor: Option<i64>,
    /// The excess levels a customer may choose between on this cover. Where
    /// there are any, each life's cover carries one, and it prices the cover
    /// and replaces the schedule's excess at claim.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excess_options: Vec<ExcessOption>,
    /// Whether premium is waived while a benefit is paid on this cover.
    /// Absent: the product does not say, and the wording is read.
    #[serde(default)]
    pub premium_waiver: Option<bool>,
    /// A name two or more health covers share where their benefit limits
    /// draw on one counter, as a base plan and its top-up do. Absent: the
    /// cover's limits count on their own.
    #[serde(default)]
    pub shared_counter: Option<String>,
    /// Covers on the same policy that a lump sum paid on this one reduces,
    /// by the amount paid and for the same life: trauma cover accelerated
    /// from life cover names the life cover here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reduces: Vec<String>,
    /// One sum insured across every life on the cover, paid once on the
    /// first event to any of them. A payment reduces or ends the cover for
    /// all of them. A cover that carries on in full for the survivor is not
    /// joint: it is a single cover on each life.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub joint_first_event: bool,
    /// The right to more of this cover without evidence of health after an
    /// event in the life's own life. Absent: the cover has no such option.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insurability_option: Option<InsurabilityOption>,
    /// The right to more of this cover without evidence of health at set
    /// anniversaries. Absent: the cover has no such option.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anniversary_option: Option<AnniversaryOption>,
    /// The right to stop paying a limited-pay premium early and keep a
    /// reduced sum insured for life. Absent: the cover has no such option.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paid_up_option: Option<PaidUpOption>,
    /// The axes of the cover's options menu (see [`crate::options`]). Where
    /// there are any, each life's cover carries one choice on every axis.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub option_axes: Vec<crate::options::OptionAxis>,
    /// The youngest a life may be, at its age last birthday, when the cover
    /// starts for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_entry_age: Option<u32>,
    /// The oldest a life may be when the cover starts for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_entry_age: Option<u32>,
    /// What the cover pays or allows besides its main benefit, as its
    /// wording sets out. Claims and servicing read these; the core does not.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub built_in: Vec<BuiltInBenefit>,
}

/// A limited-pay cover made paid up: no more premium, and a sum insured
/// reduced to the share of the premium term paid for, times a factor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaidUpOption {
    /// The least share of the premium term that must have been paid for,
    /// as a fraction: `0.5` is half the term.
    pub min_share_of_term: DecStr,
    /// What the proportionate sum insured is multiplied by, `0.95` say.
    pub factor: DecStr,
}

/// How a built-in benefit relates to the cover's sum insured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltInKind {
    /// Paid early out of the sum insured, which it reduces.
    EarlyPayment,
    /// A lump sum paid on top of the sum insured, which it leaves alone.
    Additional,
    /// Costs the owner has paid, refunded on receipts, on top of the sum insured.
    Reimbursement,
    /// An amount paid each month on top of an income benefit.
    MonthlyAddition,
    /// A right the owner may use, such as more cover without evidence or a
    /// change of premium structure; nothing is paid.
    Facility,
    /// Premium is not collected while it applies.
    PremiumWaiver,
}

/// One built-in or optional benefit of a cover, and the most it pays: the
/// lowest of the limits it sets. An optional benefit names the option
/// feature that switches it on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuiltInBenefit {
    pub code: String,
    pub name: String,
    pub kind: BuiltInKind,
    /// The most it pays, in minor units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_minor: Option<i64>,
    /// The most it pays as a percentage of the life's sum insured on the cover.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent_of_cover: Option<DecStr>,
    /// The most it pays as a multiple of the life's sum insured on the
    /// cover: of the monthly benefit on an income cover, or of the benefit
    /// amount a cover such as accidental injury pays in multiples of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multiple_of_cover: Option<DecStr>,
    /// Only where the life's option has this feature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_feature: Option<String>,
    /// Definition ids the claim is assessed against.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub definitions: Vec<String>,
}

impl BuiltInBenefit {
    /// The most this benefit pays on a cover with this sum insured, in minor
    /// units, the lowest of its limits. `None` where it sets no limit.
    pub fn limit_minor(&self, sum_insured_minor: i64) -> Option<i64> {
        // A share of the cover is cut down to whole minor units: a limit
        // never rounds up past what the wording allows.
        let of = |share: &DecStr, divisor: i64| {
            let v = ic_money::Dec::from(sum_insured_minor) * share.get() / ic_money::Dec::from(divisor);
            ic_money::round_integer(v, RoundingMode::Down).ok().and_then(|m| i64::try_from(m).ok())
        };
        [
            self.max_minor,
            self.percent_of_cover.as_ref().and_then(|p| of(p, 100)),
            self.multiple_of_cover.as_ref().and_then(|m| of(m, 1)),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Whether this benefit applies to a life at this option.
    pub fn applies(&self, option: Option<&crate::options::CoverOption>) -> bool {
        self.requires_feature.as_ref().is_none_or(|f| option.is_some_and(|o| o.has_feature(f)))
    }
}

/// A future insurability option on a lump sum cover: after one of the named
/// events the life may add cover on the terms it already has, without
/// underwriting, inside these limits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InsurabilityOption {
    /// The events that give the right: `marriage`, `birth_or_adoption`, ...
    pub events: Vec<String>,
    /// The most one exercise adds, as a share of the cover the life had at
    /// the start, in percent.
    pub max_percent_of_original: DecStr,
    /// And in money.
    pub max_increase_minor: i64,
    /// How many times a life may use the option on this cover.
    pub max_uses: u32,
    /// Days after the event within which it must be used.
    pub within_days: u32,
    /// The life must be younger than this when it is used.
    pub to_age: u32,
    /// The most all uses add together, as a share of the cover the life had
    /// at the start, in percent. Absent: only `max_uses` bounds the total.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_total_percent_of_original: Option<DecStr>,
    /// And in money. Where both are set the lower applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_total_increase_minor: Option<i64>,
    /// No use within this many days of an earlier increase of the cover for
    /// the life without evidence, under this option or the anniversary one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub once_in_days: Option<u32>,
}

/// The event an anniversary option is used under, in place of one of the
/// insurability option's events: the date is the anniversary itself.
pub const ANNIVERSARY_EVENT: &str = "anniversary";

/// A future insurability option at set anniversaries: every `every_years`
/// years from the day the life's cover started, the life may add cover on
/// the terms it already has, without underwriting, inside these limits. A
/// right not used in the window after its anniversary lapses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnniversaryOption {
    pub every_years: u32,
    /// The most one use adds, as a share of the cover the life had at the
    /// start, in percent ...
    pub max_percent_of_original: DecStr,
    /// ... or this much, whichever is the greater.
    pub at_least_minor: i64,
    /// The most all uses add together: the lower of this share of the
    /// original cover and `max_total_increase_minor`.
    pub max_total_percent_of_original: DecStr,
    pub max_total_increase_minor: i64,
    /// Days after the anniversary within which it must be used.
    pub within_days: u32,
    /// The life must be younger than this when it is used.
    pub to_age: u32,
    /// The option feature the life's original cover must have chosen.
    /// Absent: every life on the cover has the option.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_feature: Option<String>,
    /// No use within this many days of an earlier increase of the cover for
    /// the life without evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub once_in_days: Option<u32>,
}

/// One excess level on a cover's menu.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExcessOption {
    pub code: String,
    /// What the customer pays of a claim before the cover does.
    pub excess_minor: i64,
    /// What the cover's premium is multiplied by at this excess.
    pub premium_factor: DecStr,
}

impl CoverDefinition {
    pub fn excess_option(&self, code: &str) -> Option<&ExcessOption> {
        self.excess_options.iter().find(|o| o.code == code)
    }

    /// Whether a choice of excess is one this cover accepts: one of its
    /// options where it has any, and none where it has none.
    pub fn accepts_excess(&self, choice: Option<&str>) -> bool {
        match choice {
            None => !self.has_options(),
            Some(code) => self.option(code).is_ok(),
        }
    }

    /// Whether a life of this age may start this cover.
    pub fn accepts_entry_age(&self, age: u32) -> bool {
        self.min_entry_age.is_none_or(|m| age >= m) && self.max_entry_age.is_none_or(|m| age <= m)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DayCount {
    Actual,
}

/// When a change of billing frequency takes effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrequencyChange {
    /// From the first billing period on or after the next policy anniversary.
    Anniversary,
    /// From the next period not yet billed.
    NextPeriod,
}

/// Lifecycle features. Each is an enum variant with typed parameters, so the
/// compiler forces every part of the engine to handle every feature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Feature {
    Billing {
        frequencies: Vec<Frequency>,
        policy_fee_annual: DecStr,
        /// Loading by frequency key for paying more often than yearly.
        #[serde(default)]
        modal_loading: BTreeMap<String, DecStr>,
        /// When a change of frequency the owner asks for takes effect.
        /// Absent: the frequency chosen at issue cannot be changed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        frequency_change: Option<FrequencyChange>,
        /// Whether the owner may move the day of the month premium falls
        /// due on.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        billing_day_change: bool,
    },
    Earning {
        day_count: DayCount,
    },
    Arrears {
        grace_days: u32,
        lapse_days: u32,
        suspend_after_grace: bool,
        reinstatement_days: u32,
        /// Days after the lapse within which paying the arrears is enough.
        /// Later than this, inside the window, the policy is reinstated only
        /// on underwriting's decision. Not set: the whole window is free
        /// of evidence.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reinstatement_evidence_free_days: Option<u32>,
    },
    Indexation {
        rate_code: String,
        opt_out: bool,
        /// What a claim pays when an indexation step was applied after the
        /// insured event but before anyone knew of it. By default the step
        /// is treated as never having started and the claim pays the sum
        /// insured as at the event. A product whose wording promises the
        /// indexed amount sets this.
        #[serde(default)]
        pays_step_applied_after_event: bool,
    },
    MultiCoverDiscount {
        min_covers: u32,
        percent: DecStr,
    },
    /// The product's chain of discounts, in order, and the limits on it
    /// (see `discount`). Each discount is its own line on the bill.
    Discounts {
        chain: Vec<crate::discount::DiscountStep>,
        /// The most all discounts together take, as a percentage of base premium.
        #[serde(default)]
        max_total_percent: Option<DecStr>,
        /// The least the net annual premium can be, in major currency units.
        #[serde(default)]
        min_net_annual: Option<DecStr>,
        /// The largest approved override, as a percentage, that one
        /// approver can give. Above it a second approver is needed.
        /// Absent: every override needs two.
        #[serde(default)]
        override_single_approver_percent: Option<DecStr>,
        /// The largest approved override anyone can give, as a percentage.
        /// Absent: the product allows no overrides.
        #[serde(default)]
        override_max_percent: Option<DecStr>,
    },
    CoolingOff {
        days: u32,
    },
    /// The owner may borrow against the policy's value.
    PolicyLoans {
        /// The most that may be owed, as a percentage of the policy's
        /// value on the day more is borrowed.
        max_percent_of_value: DecStr,
        /// The code of the yearly interest rate, activated through the log
        /// as an index rate and read each day as in force that day.
        rate_code: String,
    },
    /// The rules for combining covers on a mix-and-match product. They hold
    /// for each life's own set of covers, so a family policy can give each
    /// life a different set.
    Composition {
        /// A life with any cover holds at least one of these.
        #[serde(default)]
        core: Vec<String>,
        /// A cover and the covers the same life must also hold to have it.
        #[serde(default)]
        requires: BTreeMap<String, Vec<String>>,
        /// Sets of covers of which a life holds at most one.
        #[serde(default)]
        exclusive: Vec<Vec<String>>,
        /// A cover and covers of which the same life must also hold at
        /// least one to have it.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        requires_any: BTreeMap<String, Vec<String>>,
    },
}

impl Feature {
    /// The defined order features run in.
    pub fn order(&self) -> u8 {
        match self {
            Feature::Composition { .. } => 5,
            Feature::MultiCoverDiscount { .. } => 10,
            Feature::Discounts { .. } => 11,
            Feature::Indexation { .. } => 20,
            Feature::Billing { .. } => 30,
            Feature::Earning { .. } => 40,
            Feature::Arrears { .. } => 50,
            Feature::CoolingOff { .. } => 60,
            Feature::PolicyLoans { .. } => 70,
        }
    }
}

/// A policy's cover and premium suspended at the owner's request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuspensionOption {
    /// The most months of suspension over the policy's whole life.
    pub max_months: u32,
    /// How long the policy must have been in force before the first one.
    pub min_months_in_force: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductDefinition {
    pub code: String,
    pub name: String,
    pub version: u32,
    pub effective_from: NaiveDate,
    pub line: LineOfBusiness,
    pub currency: Currency,
    pub statutory_fund: String,
    pub rounding: RoundingMode,
    pub covers: Vec<CoverDefinition>,
    pub features: Vec<Feature>,
    /// The most the policy pays for one claim event across all its covers,
    /// in minor units. Absent: each cover's own limit and nothing more.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_cap_minor: Option<i64>,
    /// The owner's right to suspend the whole policy for a time: no
    /// premium, and no cover for anything that happens meanwhile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_suspension: Option<SuspensionOption>,
    /// SHA-256 of the Starlark source. Filled by the loader.
    #[serde(default)]
    pub content_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArrearsRules {
    pub grace_days: u32,
    pub lapse_days: u32,
    pub suspend_after_grace: bool,
    pub reinstatement_days: u32,
    pub reinstatement_evidence_free_days: Option<u32>,
}

impl ProductDefinition {
    /// Whether one life may hold exactly this set of covers under the
    /// product's composition rules. A product that declares none allows any
    /// set, and a life with no covers breaks no rule.
    pub fn check_composition(&self, held: &std::collections::BTreeSet<&str>) -> Result<(), String> {
        let Some((core, requires, exclusive, requires_any)) = self.features.iter().find_map(|f| match f {
            Feature::Composition {
                core,
                requires,
                exclusive,
                requires_any,
            } => Some((core, requires, exclusive, requires_any)),
            _ => None,
        }) else {
            return Ok(());
        };
        if held.is_empty() {
            return Ok(());
        }
        if !core.is_empty() && !core.iter().any(|c| held.contains(c.as_str())) {
            return Err(format!("at least one of {} is held", core.join(", ")));
        }
        for (cover, needed) in requires {
            if let Some(missing) = needed.iter().find(|n| held.contains(cover.as_str()) && !held.contains(n.as_str())) {
                return Err(format!("{cover} is held only with {missing}"));
            }
        }
        for (cover, any) in requires_any {
            if held.contains(cover.as_str()) && !any.iter().any(|n| held.contains(n.as_str())) {
                return Err(format!("{cover} is held only with one of {}", any.join(", ")));
            }
        }
        for set in exclusive {
            let chosen: Vec<&str> = set.iter().map(String::as_str).filter(|c| held.contains(c)).collect();
            if chosen.len() > 1 {
                return Err(format!("only one of {} is held", chosen.join(", ")));
            }
        }
        Ok(())
    }

    pub fn cover(&self, code: &str) -> Option<&CoverDefinition> {
        self.covers.iter().find(|c| c.code == code)
    }
    pub fn arrears(&self) -> Option<ArrearsRules> {
        self.features.iter().find_map(|f| match f {
            Feature::Arrears {
                grace_days,
                lapse_days,
                suspend_after_grace,
                reinstatement_days,
                reinstatement_evidence_free_days,
            } => Some(ArrearsRules {
                grace_days: *grace_days,
                lapse_days: *lapse_days,
                suspend_after_grace: *suspend_after_grace,
                reinstatement_days: *reinstatement_days,
                reinstatement_evidence_free_days: *reinstatement_evidence_free_days,
            }),
            _ => None,
        })
    }
    pub fn cooling_off_days(&self) -> Option<u32> {
        self.features.iter().find_map(|f| match f {
            Feature::CoolingOff { days } => Some(*days),
            _ => None,
        })
    }
    /// The product's policy loan terms: the share of value that may be
    /// owed, as a percentage, and the interest rate's code.
    pub fn policy_loans(&self) -> Option<(&DecStr, &str)> {
        self.features.iter().find_map(|f| match f {
            Feature::PolicyLoans {
                max_percent_of_value,
                rate_code,
            } => Some((max_percent_of_value, rate_code.as_str())),
            _ => None,
        })
    }
    pub fn indexation(&self) -> Option<&str> {
        self.features.iter().find_map(|f| match f {
            Feature::Indexation { rate_code, .. } => Some(rate_code.as_str()),
            _ => None,
        })
    }
    /// Whether an indexation step applied after an insured event that was
    /// not yet known stands at claim. See [`Feature::Indexation`].
    pub fn pays_step_applied_after_event(&self) -> bool {
        self.features.iter().any(|f| {
            matches!(
                f,
                Feature::Indexation {
                    pays_step_applied_after_event: true,
                    ..
                }
            )
        })
    }
    /// What the product lets the owner change about billing after issue:
    /// when a new frequency takes effect, if it may change at all, and
    /// whether the day premium falls due may move.
    pub fn billing_changes(&self) -> (Option<FrequencyChange>, bool) {
        self.features
            .iter()
            .find_map(|f| match f {
                Feature::Billing {
                    frequency_change,
                    billing_day_change,
                    ..
                } => Some((*frequency_change, *billing_day_change)),
                _ => None,
            })
            .unwrap_or((None, false))
    }
    pub fn allows_frequency(&self, f: Frequency) -> bool {
        self.features
            .iter()
            .any(|x| matches!(x, Feature::Billing { frequencies, .. } if frequencies.contains(&f)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateRow {
    #[serde(default)]
    pub age_from: u32,
    #[serde(default = "max_age")]
    pub age_to: u32,
    #[serde(default)]
    pub sex: Option<Sex>,
    #[serde(default)]
    pub smoker: Option<bool>,
    #[serde(default)]
    pub category: Option<String>,
    /// The occupation class the row applies to. A row without one applies
    /// to every class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occupation_class: Option<String>,
    pub annual_rate: DecStr,
}

fn max_age() -> u32 {
    150
}

/// Versioned rate data, referenced by hash and activated through the log like
/// a product. Actuarial judgement stays outside the core.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateTable {
    pub id: String,
    pub version: u32,
    pub effective_from: NaiveDate,
    pub rows: Vec<RateRow>,
    /// Activation scope: none is global; otherwise the named groups only (a
    /// scheme's own table stays on that scheme's partition).
    #[serde(default)]
    pub scope: Option<Vec<String>>,
    #[serde(default)]
    pub content_hash: String,
}

/// A versioned medical or event definition a cover is bound to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub id: String,
    pub version: u32,
    pub effective_from: NaiveDate,
    pub title: String,
    /// The wording. Hashed into `text_hash`; the core keeps only the hash.
    #[serde(default, skip_serializing)]
    pub text: String,
    #[serde(default)]
    pub text_hash: String,
}

/// The binding of a cover to a definition version. Frozen forever once made:
/// a later definition change never silently alters an existing cover.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DefinitionBinding {
    pub id: String,
    pub version: u32,
    pub text_hash: String,
}
