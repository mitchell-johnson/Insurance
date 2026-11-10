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
}

/// What a cover pays. Claims assessment is outside the core; the benefit only
/// says what is payable once an event meets the definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Benefit {
    LumpSum { survival_days: u32, ends_cover: bool },
    MonthlyBenefit { waiting_days: u32, benefit_months: u32 },
    HealthSchedule { limits: Vec<BenefitLimit> },
    InvestmentAccount { funds: Vec<String> },
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DayCount {
    Actual,
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
    },
    Earning {
        day_count: DayCount,
    },
    Arrears {
        grace_days: u32,
        lapse_days: u32,
        suspend_after_grace: bool,
        reinstatement_days: u32,
    },
    Indexation {
        rate_code: String,
        opt_out: bool,
    },
    MultiCoverDiscount {
        min_covers: u32,
        percent: DecStr,
    },
    CoolingOff {
        days: u32,
    },
}

impl Feature {
    /// The defined order features run in.
    pub fn order(&self) -> u8 {
        match self {
            Feature::MultiCoverDiscount { .. } => 10,
            Feature::Indexation { .. } => 20,
            Feature::Billing { .. } => 30,
            Feature::Earning { .. } => 40,
            Feature::Arrears { .. } => 50,
            Feature::CoolingOff { .. } => 60,
        }
    }
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
}

impl ProductDefinition {
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
            } => Some(ArrearsRules {
                grace_days: *grace_days,
                lapse_days: *lapse_days,
                suspend_after_grace: *suspend_after_grace,
                reinstatement_days: *reinstatement_days,
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
    pub fn indexation(&self) -> Option<&str> {
        self.features.iter().find_map(|f| match f {
            Feature::Indexation { rate_code, .. } => Some(rate_code.as_str()),
            _ => None,
        })
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
