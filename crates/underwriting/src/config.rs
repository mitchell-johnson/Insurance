//! Configuration the module runs on: code lists, question sets, rule sets,
//! authority profiles, authorisation policy, delegated authorities and
//! business settings.
//!
//! Every artefact is versioned and held for good: a decision records the
//! versions it was made under, and an old contract is read under the
//! question set and rules of its own date. A new version becomes active only
//! through an activation command with an author, two other approvers and a
//! change record.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::authority::{Authority, AuthorityProfile, DEFAULT_POLICIES};
use crate::case::Timers;
use crate::codes::{reason, ExclusionLibrary, ReasonLibrary, COVER_TYPES};
use crate::command::{Approval, Reject};
use crate::model::VersionRef;
use crate::questions::{DisclosureRegime, QuestionSet};
use crate::rules::RuleSet;

/// Accidental death cover while a proposal is being underwritten.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterimSettings {
    pub product_code: String,
    pub cover_code: String,
    /// The most interim cover on one life.
    pub max_sum_insured_minor: i64,
}

/// How many decisions in a thousand are drawn for peer review, by outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SampleRates {
    pub standard_per_mille: u32,
    pub non_standard_per_mille: u32,
    pub decline_per_mille: u32,
}

/// Business settings. Each is a decision for the business, not for code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub version: u32,
    /// How long a proposal lives before it lapses.
    pub proposal_days: i64,
    /// How long an offer of terms stays open.
    pub offer_days: i64,
    /// How long a source has to supply evidence.
    pub evidence_days: i64,
    pub chase_every_days: i64,
    pub max_chasers: u32,
    pub timers: Timers,
    /// The rules alone never accept a sum insured above this.
    pub straight_through_limit_minor: i64,
    /// A decision accepting more than this needs a second signature.
    pub countersign_above_minor: i64,
    /// And so does a loading above this.
    pub countersign_loading_above_percent: u32,
    pub interim: Option<InterimSettings>,
    /// Interim cover for a product's proposals in place of `interim`, by
    /// product code, where the product brings its own.
    #[serde(default)]
    pub interim_by_product: BTreeMap<String, InterimSettings>,
    /// What an application must be for each product before it is taken:
    /// occupation tests, minimum terms and limits by age.
    #[serde(default)]
    pub eligibility: Vec<crate::eligibility::Eligibility>,
    pub sample: SampleRates,
    /// Contracts from this date fall under the duty of reasonable care.
    pub reasonable_care_from: NaiveDate,
    /// How far back a delegate's decision may be dated.
    pub delegated_tolerance_days: i64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 1,
            proposal_days: 90,
            offer_days: 30,
            evidence_days: 21,
            chase_every_days: 7,
            max_chasers: 2,
            timers: Timers::default(),
            straight_through_limit_minor: 1_000_000_00,
            countersign_above_minor: 2_000_000_00,
            countersign_loading_above_percent: 100,
            interim: None,
            interim_by_product: crate::eligibility::shipped_interim(),
            eligibility: crate::eligibility::shipped(),
            sample: SampleRates {
                standard_per_mille: 20,
                non_standard_per_mille: 100,
                decline_per_mille: 250,
            },
            // The Contracts of Insurance Act 2024 applies from this date.
            reasonable_care_from: NaiveDate::from_ymd_opt(2027, 11, 15).unwrap_or_default(),
            delegated_tolerance_days: 30,
        }
    }
}

/// An employer's, partner's or administrator's authority to underwrite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelegatedAuthority {
    pub id: String,
    pub version: u32,
    pub delegate: String,
    pub products: BTreeSet<String>,
    /// The most the delegate may bind on one life, by cover type. Below the
    /// treaty's automatic acceptance limit.
    pub per_life_minor: BTreeMap<String, i64>,
    /// The most that may stand accepted under the authority in all.
    pub aggregate_minor: i64,
    pub max_loading_percent: u32,
    pub may_exclude: bool,
    pub may_decline: bool,
    /// The rule set the delegate must use, and its lowest acceptable
    /// version, where we supply the rules.
    pub rule_set: Option<(String, u32)>,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    /// Where the reinsurer's agreement to the authority is on file.
    pub reinsurer_agreement_ref: Option<String>,
}

impl DelegatedAuthority {
    pub fn applies_on(&self, date: NaiveDate) -> bool {
        self.effective_from <= date && self.effective_to.is_none_or(|t| date < t)
    }
}

/// The record of one activation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Activation {
    pub kind: String,
    pub id: String,
    pub version: u32,
    pub content_hash: String,
    pub approval: Approval,
    pub backtest_digest: Option<String>,
    pub on: NaiveDate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub exclusions: Vec<ExclusionLibrary>,
    pub reasons: Vec<ReasonLibrary>,
    pub question_sets: Vec<QuestionSet>,
    pub rule_sets: Vec<RuleSet>,
    pub profiles: BTreeMap<String, AuthorityProfile>,
    pub settings: Settings,
    pub cedar: String,
    /// Every version of every delegated authority, oldest first.
    pub delegated: Vec<DelegatedAuthority>,
    pub activations: Vec<Activation>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            exclusions: vec![],
            reasons: vec![],
            question_sets: vec![],
            rule_sets: vec![],
            profiles: BTreeMap::new(),
            settings: Settings::default(),
            cedar: DEFAULT_POLICIES.to_string(),
            delegated: vec![],
            activations: vec![],
        }
    }
}

fn version_ref(id: &str, version: u32, hash: &str) -> VersionRef {
    VersionRef {
        id: id.to_string(),
        version,
        content_hash: hash.to_string(),
    }
}

impl Approval {
    /// An author and at least two other people, with a change record.
    fn check(&self) -> Result<(), Reject> {
        let distinct: BTreeSet<&String> = self.approvers.iter().filter(|a| !a.is_empty() && **a != self.author).collect();
        if self.author.is_empty() || distinct.len() < 2 || distinct.len() != self.approvers.len() {
            return Err(Reject::new("approval", "a configuration change needs an author and two other approvers"));
        }
        if self.change_ref.is_empty() {
            return Err(Reject::new("approval", "a configuration change needs a change record"));
        }
        Ok(())
    }
}

fn newer(kind: &str, id: &str, version: u32, current: Option<u32>) -> Result<(), Reject> {
    if version == 0 || current.is_some_and(|c| version <= c) {
        return Err(Reject::new(
            "stale_version",
            format!("{kind} {id} v{version} does not follow the active version"),
        ));
    }
    Ok(())
}

impl Config {
    /// The exclusion list in force on a date: the newest version effective by then.
    pub fn exclusions_on(&self, date: NaiveDate) -> Option<&ExclusionLibrary> {
        self.exclusions.iter().rev().find(|l| l.effective_from <= date)
    }

    pub fn reasons_on(&self, date: NaiveDate) -> Option<&ReasonLibrary> {
        self.reasons.iter().rev().find(|l| l.effective_from <= date)
    }

    /// The question set a contract of this date is asked under.
    pub fn question_set_on(&self, id: &str, contract_date: NaiveDate) -> Option<&QuestionSet> {
        self.question_sets.iter().rev().find(|q| q.id == id && q.applies_on(contract_date))
    }

    /// The rule set for a product and contract date.
    pub fn rule_set_on(&self, product: &str, contract_date: NaiveDate) -> Option<&RuleSet> {
        self.rule_sets
            .iter()
            .rev()
            .find(|r| r.applies_on(contract_date) && r.products.iter().any(|p| p == product))
    }

    pub fn question_set(&self, r: &VersionRef) -> Option<&QuestionSet> {
        self.question_sets.iter().find(|q| q.id == r.id && q.version == r.version)
    }

    /// The legal basis of a contract of this date.
    pub fn regime_on(&self, contract_date: NaiveDate) -> DisclosureRegime {
        if contract_date >= self.settings.reasonable_care_from {
            DisclosureRegime::ReasonableCare
        } else {
            DisclosureRegime::DutyOfDisclosure
        }
    }

    pub fn delegated_on(&self, id: &str, date: NaiveDate) -> Option<&DelegatedAuthority> {
        self.delegated
            .iter()
            .rev()
            .find(|d| d.id == id && d.effective_from <= date)
            .filter(|d| d.applies_on(date))
    }

    pub fn exclusions_ref(&self, date: NaiveDate) -> Option<VersionRef> {
        self.exclusions_on(date).map(|l| version_ref(&l.id, l.version, &l.content_hash))
    }

    pub fn reasons_ref(&self, date: NaiveDate) -> Option<VersionRef> {
        self.reasons_on(date).map(|l| version_ref(&l.id, l.version, &l.content_hash))
    }

    pub fn authority(&self) -> Result<Authority, Reject> {
        Authority::new(&self.cedar).map_err(|e| Reject::new("policies", e.to_string()))
    }

    fn record(&mut self, kind: &str, id: &str, version: u32, hash: &str, approval: Approval, backtest_digest: Option<String>, on: NaiveDate) -> Activation {
        let a = Activation {
            kind: kind.to_string(),
            id: id.to_string(),
            version,
            content_hash: hash.to_string(),
            approval,
            backtest_digest,
            on,
        };
        self.activations.push(a.clone());
        a
    }

    pub fn activate_exclusions(&mut self, library: ExclusionLibrary, approval: Approval, on: NaiveDate) -> Result<Activation, Reject> {
        approval.check()?;
        let last = self.exclusions.last();
        newer("exclusion list", &library.id, library.version, last.map(|l| l.version))?;
        if last.is_some_and(|l| library.effective_from < l.effective_from) {
            return Err(Reject::new(
                "effective_date",
                "a new exclusion list cannot take effect before the one it replaces",
            ));
        }
        // A code, once issued, is on policies for decades: it is never removed.
        if let Some(gone) = last.and_then(|l| l.entries.keys().find(|c| !library.contains(c))) {
            return Err(Reject::new(
                "code_removed",
                format!("exclusion {gone} is on file and cannot be removed from the list"),
            ));
        }
        if library
            .entries
            .values()
            .any(|e| e.applies_to.iter().any(|t| !COVER_TYPES.contains(&t.as_str())))
        {
            return Err(Reject::new("cover_type", "an exclusion names a cover type that does not exist"));
        }
        let a = self.record("exclusions", &library.id, library.version, &library.content_hash, approval, None, on);
        self.exclusions.push(library);
        Ok(a)
    }

    pub fn activate_reasons(&mut self, library: ReasonLibrary, approval: Approval, on: NaiveDate) -> Result<Activation, Reject> {
        approval.check()?;
        let last = self.reasons.last();
        newer("reason list", &library.id, library.version, last.map(|l| l.version))?;
        if last.is_some_and(|l| library.effective_from < l.effective_from) {
            return Err(Reject::new("effective_date", "a new reason list cannot take effect before the one it replaces"));
        }
        let gone = last.and_then(|l| l.entries.keys().find(|c| !library.contains(c)));
        if let Some(missing) = gone
            .map(String::as_str)
            .or_else(|| reason::REQUIRED.iter().copied().find(|c| !library.contains(c)))
        {
            return Err(Reject::new("code_removed", format!("reason {missing} must be in the list")));
        }
        let a = self.record("reasons", &library.id, library.version, &library.content_hash, approval, None, on);
        self.reasons.push(library);
        Ok(a)
    }

    pub fn activate_question_set(&mut self, set: QuestionSet, approval: Approval, on: NaiveDate) -> Result<Activation, Reject> {
        approval.check()?;
        let current = self.question_sets.iter().filter(|q| q.id == set.id).map(|q| q.version).max();
        newer("question set", &set.id, set.version, current)?;
        if set.regime != self.regime_on(set.effective_from) || set.effective_to.is_some_and(|t| self.regime_on(t.pred_opt().unwrap_or(t)) != set.regime) {
            return Err(Reject::new("regime", "a question set must sit wholly under the legal basis of its dates"));
        }
        let a = self.record("question_set", &set.id, set.version, &set.content_hash, approval, None, on);
        self.question_sets.push(set);
        Ok(a)
    }

    /// A rule set must agree with the libraries it reads, and a change to an
    /// existing rule set must carry the digest of its backtest.
    pub fn activate_rule_set(&mut self, set: RuleSet, backtest_digest: Option<String>, approval: Approval, on: NaiveDate) -> Result<Activation, Reject> {
        approval.check()?;
        let current = self.rule_sets.iter().filter(|r| r.id == set.id).map(|r| r.version).max();
        newer("rule set", &set.id, set.version, current)?;
        if current.is_some() && backtest_digest.as_deref().is_none_or(str::is_empty) {
            return Err(Reject::new(
                "backtest",
                "a changed rule set is activated only with the backtest its approvers saw",
            ));
        }
        let (Some(questions), Some(exclusions), Some(reasons)) = (
            self.question_set_on(&set.question_set, set.effective_from),
            self.exclusions_on(set.effective_from),
            self.reasons_on(set.effective_from),
        ) else {
            return Err(Reject::new("references", "the rule set's question set and code lists must be active first"));
        };
        set.check_references(questions, exclusions, reasons).map_err(|e| Reject::new("references", e))?;
        let a = self.record("rule_set", &set.id, set.version, &set.content_hash, approval, backtest_digest, on);
        self.rule_sets.push(set);
        Ok(a)
    }

    pub fn activate_profile(&mut self, profile: AuthorityProfile, approval: Approval, on: NaiveDate) -> Result<Activation, Reject> {
        approval.check()?;
        newer(
            "profile",
            &profile.underwriter,
            profile.version,
            self.profiles.get(&profile.underwriter).map(|p| p.version),
        )?;
        // Nobody signs off their own authority.
        if approval.author == profile.underwriter || approval.approvers.contains(&profile.underwriter) {
            return Err(Reject::new("approval", "an underwriter cannot author or approve their own authority"));
        }
        let a = self.record("profile", &profile.underwriter, profile.version, "", approval, None, on);
        self.profiles.insert(profile.underwriter.clone(), profile);
        Ok(a)
    }

    pub fn activate_settings(&mut self, settings: Settings, approval: Approval, on: NaiveDate) -> Result<Activation, Reject> {
        approval.check()?;
        newer("settings", "settings", settings.version, Some(self.settings.version))?;
        let sane = settings.proposal_days > 0
            && settings.offer_days > 0
            && settings.evidence_days > 0
            && settings.chase_every_days > 0
            && settings.timers.first_touch_hours > 0
            && settings.timers.manager_hours >= settings.timers.first_touch_hours
            && settings.timers.work_in_progress_limit > 0
            && settings.straight_through_limit_minor >= 0
            && settings.countersign_above_minor >= 0
            && settings.delegated_tolerance_days >= 0
            && [
                settings.sample.standard_per_mille,
                settings.sample.non_standard_per_mille,
                settings.sample.decline_per_mille,
            ]
            .iter()
            .all(|r| *r <= 1000)
            && settings
                .interim
                .as_ref()
                .is_none_or(|i| i.max_sum_insured_minor > 0 && !i.product_code.is_empty() && !i.cover_code.is_empty());
        if !sane {
            return Err(Reject::new("settings", "a setting is out of range"));
        }
        let a = self.record("settings", "settings", settings.version, "", approval, None, on);
        self.settings = settings;
        Ok(a)
    }

    pub fn activate_policies(&mut self, cedar: String, approval: Approval, on: NaiveDate) -> Result<Activation, Reject> {
        approval.check()?;
        Authority::new(&cedar).map_err(|e| Reject::new("policies", e.to_string()))?;
        let version = self.activations.iter().filter(|a| a.kind == "policies").count() as u32 + 2;
        let hash = crate::load::content_hash(&cedar);
        let a = self.record("policies", "underwriting.cedar", version, &hash, approval, None, on);
        self.cedar = cedar;
        Ok(a)
    }

    pub fn activate_delegated(&mut self, authority: DelegatedAuthority, approval: Approval, on: NaiveDate) -> Result<Activation, Reject> {
        approval.check()?;
        let current = self.delegated.iter().filter(|d| d.id == authority.id).map(|d| d.version).max();
        newer("delegated authority", &authority.id, authority.version, current)?;
        let sane = !authority.delegate.is_empty()
            && authority.aggregate_minor >= 0
            && authority
                .per_life_minor
                .iter()
                .all(|(t, m)| COVER_TYPES.contains(&t.as_str()) && *m >= 0 && *m <= authority.aggregate_minor)
            && authority.effective_to.is_none_or(|t| t > authority.effective_from);
        if !sane {
            return Err(Reject::new("delegated_authority", "the authority's limits or dates are out of range"));
        }
        let a = self.record("delegated_authority", &authority.id, authority.version, "", approval, None, on);
        self.delegated.push(authority);
        Ok(a)
    }
}
