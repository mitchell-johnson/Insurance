//! Treaties: the typed terms a Starlark treaty module is validated into.
//!
//! A treaty is versioned and effective-dated. New business follows the
//! version in force when the cover starts; cover already in force stays on
//! the version it was written under until a recapture or a negotiated change
//! moves it by command.

use chrono::{Datelike, NaiveDate};
use ic_money::Currency;
use ic_product::Sex;
use serde::{Deserialize, Serialize};

use crate::math::{de_ppm, de_rate, Ppm, WHOLE};
use crate::{ReinsurerId, TreatyId, TreatyRef};

/// The products and cover types a treaty applies to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub product: String,
    pub cover_types: Vec<String>,
}

/// How a cover is shared between the insurer and the treaty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Basis {
    /// A fixed share of every cover, up to a total sum at risk per life that
    /// the treaty accepts automatically.
    QuotaShare {
        #[serde(deserialize_with = "de_ppm")]
        share: Ppm,
        automatic_limit_minor: i64,
    },
    /// Everything above the insurer's retention per life, up to the amount
    /// per life the treaty accepts automatically.
    Surplus { retention_minor: i64, automatic_limit_minor: i64 },
    /// Excess of loss per life: the loss on a life above the retention, up
    /// to a limit. A claim recovers what it costs above the retention, not a
    /// proportion.
    ExcessOfLoss { retention_minor: i64, limit_minor: i64 },
}

impl Basis {
    /// Whether a claim recovers in proportion to the cession.
    pub fn proportional(&self) -> bool {
        !matches!(self, Basis::ExcessOfLoss { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelShare {
    pub reinsurer: ReinsurerId,
    #[serde(deserialize_with = "de_ppm")]
    pub share: Ppm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginalPremium {
    /// Before discounts.
    Gross,
    /// After discounts.
    Net,
}

/// What the reinsurer is paid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PremiumBasis {
    /// The ceded sum at risk at the reinsurer's own rates.
    RateTable { table: String },
    /// The ceded proportion of the customer's premium.
    OriginalPremium { of: OriginalPremium },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimTerms {
    /// A claim at or above this gross amount is notified to the reinsurers.
    #[serde(default)]
    pub notification_threshold_minor: Option<i64>,
    /// At or above this gross amount the reinsurers have a say before the
    /// claim is admitted.
    #[serde(default)]
    pub consent_threshold_minor: Option<i64>,
    /// How long the reinsurers have to answer.
    #[serde(default)]
    pub consent_days: u32,
    #[serde(default)]
    pub ex_gratia_recoverable: bool,
}

/// How members who are not linked to a party are ceded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UnnamedBasis {
    /// A share of every member's cover.
    SchemeShare {
        #[serde(deserialize_with = "de_ppm")]
        share: Ppm,
    },
    /// The excess over a limit per member.
    ExcessPerMember { limit_minor: i64 },
}

/// What happens to cover on a scheme member that a membership file wrote
/// without underwriting and that takes the life over the retention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExcessPath {
    /// Ceded facultatively under the treaty's standing terms for group
    /// business.
    Facultative,
    /// Flagged for the scheme's underwritten-excess path.
    UnderwrittenExcess,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupTerms {
    pub unnamed: UnnamedBasis,
    /// Used unless the scheme's own terms say otherwise.
    pub excess_path: ExcessPath,
    /// Who takes the standing facultative cession, and how much per life.
    pub facultative_reinsurer: ReinsurerId,
    pub facultative_limit_minor: i64,
    /// Annual rate per thousand of the facultative cession, in millionths.
    #[serde(deserialize_with = "de_rate")]
    pub facultative_rate: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatementPeriod {
    Monthly,
    Quarterly,
    Yearly,
}

impl StatementPeriod {
    /// The first day of the statement period that ends on `period_end`.
    pub fn start_for(self, period_end: NaiveDate) -> NaiveDate {
        let months = match self {
            StatementPeriod::Monthly => 1,
            StatementPeriod::Quarterly => 3,
            StatementPeriod::Yearly => 12,
        };
        let next = period_end.succ_opt().unwrap_or(period_end);
        next.checked_sub_months(chrono::Months::new(months)).unwrap_or(next)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatementTerms {
    pub period: StatementPeriod,
    /// The reinsurer agrees the statement before anything is paid.
    #[serde(default)]
    pub agree_before_payment: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfitCommission {
    /// The insurer's share of the treaty's profit.
    #[serde(deserialize_with = "de_ppm")]
    pub share: Ppm,
    /// The reinsurer's expenses, as a share of ceded premium.
    #[serde(deserialize_with = "de_ppm")]
    pub expense_allowance: Ppm,
}

/// What a listing may tell a reinsurer about a life. Names and contact
/// details are not on this list and are never sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListingField {
    Age,
    Sex,
    Smoker,
}

/// The clauses a disputed item cites.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clauses {
    #[serde(default)]
    pub errors_and_omissions: String,
    #[serde(default)]
    pub claims_cooperation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Treaty {
    pub id: TreatyId,
    pub version: u32,
    pub name: String,
    pub effective_from: NaiveDate,
    /// From this date the version takes no new cover. Cover already on it
    /// stays on it.
    #[serde(default)]
    pub closed_to_new_business_from: Option<NaiveDate>,
    pub currency: Currency,
    pub scope: Vec<Scope>,
    pub basis: Basis,
    /// Treaties that name the same group share one retention per life: what
    /// the insurer keeps under one counts against the retention of the
    /// others. Without it the treaty's retention is its own.
    #[serde(default)]
    pub retention_group: Option<String>,
    pub panel: Vec<PanelShare>,
    pub premium: PremiumBasis,
    /// Reinsurance commission as a share of ceded premium.
    #[serde(default, deserialize_with = "de_ppm")]
    pub commission: Ppm,
    pub claims: ClaimTerms,
    /// Whether retention freed when a cover ends is given to later covers on
    /// the same life.
    #[serde(default)]
    pub reallocate_freed_retention: bool,
    /// Risk codes the treaty does not accept automatically, in underwriting's
    /// cause tags (`pursuit.aviation`, `occupation.class_4`, `travel.*`). A
    /// code ending in `.*` excludes the whole family.
    #[serde(default)]
    pub exclusions: Vec<String>,
    #[serde(default)]
    pub group: Option<GroupTerms>,
    /// The insurer may take cover back after this many years.
    #[serde(default)]
    pub recapture_after_years: Option<u32>,
    pub statement: StatementTerms,
    #[serde(default)]
    pub profit_commission: Option<ProfitCommission>,
    #[serde(default)]
    pub listing_fields: Vec<ListingField>,
    #[serde(default)]
    pub clauses: Clauses,
    #[serde(default)]
    pub content_hash: String,
}

impl Treaty {
    pub fn reference(&self) -> TreatyRef {
        TreatyRef {
            id: self.id.clone(),
            version: self.version,
        }
    }

    /// The retention this treaty's cover counts against.
    pub fn retention_key(&self) -> &str {
        self.retention_group.as_deref().unwrap_or(&self.id.0)
    }

    pub fn covers(&self, product: &str, cover_type: &str) -> bool {
        self.scope
            .iter()
            .any(|s| (s.product == "*" || s.product == product) && s.cover_types.iter().any(|c| c == "*" || c == cover_type))
    }

    /// Whether any of the risk codes underwriting sent for a life is one the
    /// treaty excludes.
    pub fn excludes(&self, risk_codes: &[String]) -> bool {
        self.exclusions.iter().any(|x| match x.strip_suffix('*') {
            Some(family) => risk_codes.iter().any(|c| c.starts_with(family)),
            None => risk_codes.contains(x),
        })
    }

    /// Whether cover starting on `on` is written under this version.
    pub fn open_on(&self, on: NaiveDate) -> bool {
        self.effective_from <= on && self.closed_to_new_business_from.is_none_or(|c| on < c)
    }

    /// Check the terms hang together. The message says what is wrong.
    pub fn validate(&self) -> Result<(), String> {
        if self.id.0.is_empty() || self.version == 0 {
            return Err("a treaty needs an id and a version of at least 1".into());
        }
        if self.scope.is_empty() || self.scope.iter().any(|s| s.product.is_empty() || s.cover_types.is_empty()) {
            return Err("a treaty must say which products and cover types it applies to".into());
        }
        check_panel(&self.panel)?;
        if let Some(bad) = self.exclusions.iter().find(|x| !exclusion_well_formed(x)) {
            return Err(format!(
                "the exclusion {bad:?} is not a risk code: use a tag such as pursuit.aviation, or a family such as pursuit.*"
            ));
        }
        match &self.basis {
            Basis::QuotaShare { share, automatic_limit_minor } => {
                if *share == 0 || *automatic_limit_minor <= 0 {
                    return Err("a quota share needs a share above zero and an automatic limit".into());
                }
            }
            Basis::Surplus {
                retention_minor,
                automatic_limit_minor,
            } => {
                if *retention_minor < 0 || *automatic_limit_minor <= 0 {
                    return Err("a surplus treaty needs a retention of zero or more and an automatic limit".into());
                }
            }
            Basis::ExcessOfLoss { retention_minor, limit_minor } => {
                if *retention_minor < 0 || *limit_minor <= 0 {
                    return Err("an excess of loss treaty needs a retention of zero or more and a limit".into());
                }
            }
        }
        if let PremiumBasis::RateTable { table } = &self.premium {
            if table.is_empty() {
                return Err("the premium basis names no rate table".into());
            }
        }
        let c = &self.claims;
        if c.notification_threshold_minor.is_some_and(|t| t <= 0) || c.consent_threshold_minor.is_some_and(|t| t <= 0) {
            return Err("claim thresholds must be above zero".into());
        }
        if c.consent_threshold_minor.is_some() && c.consent_days == 0 {
            return Err("a consent threshold needs the number of days the reinsurer has to answer".into());
        }
        if let Some(g) = &self.group {
            if g.facultative_limit_minor < 0 || g.facultative_reinsurer.0.is_empty() {
                return Err("group terms need a facultative reinsurer and a limit of zero or more".into());
            }
            if matches!(g.unnamed, UnnamedBasis::ExcessPerMember { limit_minor } if limit_minor < 0) {
                return Err("the limit per member cannot be negative".into());
            }
        }
        if self.closed_to_new_business_from.is_some_and(|c| c < self.effective_from) {
            return Err("a treaty cannot close to new business before it starts".into());
        }
        Ok(())
    }
}

fn check_panel(panel: &[PanelShare]) -> Result<(), String> {
    if panel.is_empty() || panel.iter().any(|p| p.reinsurer.0.is_empty() || p.share == 0) {
        return Err("the panel needs at least one reinsurer, each with a share above zero".into());
    }
    let mut seen = std::collections::BTreeSet::new();
    if !panel.iter().all(|p| seen.insert(&p.reinsurer)) {
        return Err("a reinsurer appears twice on the panel".into());
    }
    if panel.iter().map(|p| p.share as u64).sum::<u64>() != WHOLE as u64 {
        return Err("the panel's shares must add up to 1".into());
    }
    Ok(())
}

/// A catastrophe or stop loss cover: it responds to the portfolio, not to
/// one policy. Its premium is a finance ledger journal, not a posting here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AggregateTreaty {
    pub id: TreatyId,
    pub version: u32,
    pub name: String,
    /// The cover period, both ends included.
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub currency: Currency,
    pub scope: Vec<Scope>,
    pub kind: AggregateKind,
    pub panel: Vec<PanelShare>,
    #[serde(default)]
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AggregateKind {
    /// Many lives lost in one event: the retained loss from the event above
    /// a retention, once at least `min_lives` are involved.
    Catastrophe { min_lives: u32, retention_minor: i64, limit_minor: i64 },
    /// The retained claims of the whole period above an attachment point.
    StopLoss { attachment_minor: i64, limit_minor: i64 },
}

impl AggregateTreaty {
    pub fn reference(&self) -> TreatyRef {
        TreatyRef {
            id: self.id.clone(),
            version: self.version,
        }
    }

    pub fn covers(&self, product: &str, cover_type: &str, event_date: NaiveDate) -> bool {
        self.period_start <= event_date
            && event_date <= self.period_end
            && self
                .scope
                .iter()
                .any(|s| (s.product == "*" || s.product == product) && s.cover_types.iter().any(|c| c == "*" || c == cover_type))
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.id.0.is_empty() || self.version == 0 || self.scope.is_empty() {
            return Err("an aggregate treaty needs an id, a version of at least 1 and a scope".into());
        }
        if self.period_end < self.period_start {
            return Err("the cover period ends before it starts".into());
        }
        check_panel(&self.panel)?;
        let (floor, limit) = match &self.kind {
            AggregateKind::Catastrophe {
                retention_minor, limit_minor, ..
            } => (*retention_minor, *limit_minor),
            AggregateKind::StopLoss { attachment_minor, limit_minor } => (*attachment_minor, *limit_minor),
        };
        if floor < 0 || limit <= 0 {
            return Err("an aggregate treaty needs a retention of zero or more and a limit".into());
        }
        Ok(())
    }
}

/// Rating facts about a life. Insurance facts, not identity: this is all the
/// module ever holds about a person.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskFactors {
    #[serde(default)]
    pub date_of_birth: Option<NaiveDate>,
    #[serde(default)]
    pub sex: Option<Sex>,
    #[serde(default)]
    pub smoker: Option<bool>,
}

impl RiskFactors {
    /// Age last birthday.
    pub fn age_on(&self, on: NaiveDate) -> Option<u32> {
        let dob = self.date_of_birth?;
        let mut age = on.year() - dob.year();
        if (on.month(), on.day()) < (dob.month(), dob.day()) {
            age -= 1;
        }
        u32::try_from(age).ok()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReinsurerRate {
    #[serde(default)]
    pub age_from: Option<u32>,
    #[serde(default)]
    pub age_to: Option<u32>,
    #[serde(default)]
    pub sex: Option<Sex>,
    #[serde(default)]
    pub smoker: Option<bool>,
    /// Annual rate per thousand of ceded sum at risk, in millionths.
    #[serde(deserialize_with = "de_rate")]
    pub rate: i64,
}

/// A reinsurer's rate table: versioned data, activated through the log like
/// the insurer's own tables.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReinsurerRateTable {
    pub id: String,
    pub version: u32,
    pub effective_from: NaiveDate,
    pub rows: Vec<ReinsurerRate>,
    #[serde(default)]
    pub content_hash: String,
}

impl ReinsurerRateTable {
    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty() || self.version == 0 || self.rows.is_empty() {
            return Err("a rate table needs an id, a version of at least 1 and at least one row".into());
        }
        if self.rows.iter().any(|r| matches!((r.age_from, r.age_to), (Some(a), Some(b)) if b < a)) {
            return Err("a rate row has an inverted age band".into());
        }
        Ok(())
    }

    /// The rate for a life on a date. A row that asks about a fact the
    /// module does not hold does not match: a rate is never guessed.
    pub fn rate(&self, factors: Option<&RiskFactors>, on: NaiveDate) -> Option<i64> {
        let age = factors.and_then(|f| f.age_on(on));
        let sex = factors.and_then(|f| f.sex);
        let smoker = factors.and_then(|f| f.smoker);
        self.rows
            .iter()
            .find(|r| {
                r.age_from.is_none_or(|a| age.is_some_and(|x| x >= a))
                    && r.age_to.is_none_or(|b| age.is_some_and(|x| x <= b))
                    && r.sex.is_none_or(|s| sex == Some(s))
                    && r.smoker.is_none_or(|s| smoker == Some(s))
            })
            .map(|r| r.rate)
    }
}

/// A risk code, or a family of them: lower-case words joined by dots, with
/// `*` allowed only as the whole of the last part.
fn exclusion_well_formed(code: &str) -> bool {
    let word = |p: &str| !p.is_empty() && p.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    let (rest, last) = code.rsplit_once('.').unwrap_or(("", code));
    let rest_ok = rest.is_empty() || rest.split('.').all(word);
    if last == "*" {
        !rest.is_empty() && rest_ok
    } else {
        word(last) && rest_ok
    }
}
