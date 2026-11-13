//! The controlled code lists: exclusion codes and decision reason codes.
//!
//! An exclusion code names a clause, never a condition in plain words. The
//! core stores codes on a cover layer; the wording behind a code stays here,
//! split off from the library into [`ExclusionWordings`], which has restricted
//! access. The claims module matches a claim's coded cause against the codes
//! on the layer with [`ExclusionLibrary::matches`], so both modules apply one
//! rule.
//!
//! The library is plain data: it can be activated through a log, serialised,
//! compared and recorded on a decision. Matching is a pure function with no
//! clock and no Starlark.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::load::{content_hash, eval, invalid, LoadError};

/// The roots of the cause-of-claim tag taxonomy, owned by the claims module.
/// A tag is lowercase dotted segments; it matches itself and its descendants.
pub const CAUSE_ROOTS: &[&str] = &[
    "musculoskeletal",
    "mental",
    "cardiovascular",
    "cancer",
    "neurological",
    "respiratory",
    "digestive",
    "endocrine",
    "pregnancy",
    "infection",
    "injury",
    "pursuit",
    "occupation",
    "travel",
    "pre_existing",
    "unknown",
];

/// The cover types an exclusion can apply to.
pub const COVER_TYPES: &[&str] = &["life", "trauma", "income_protection", "health", "group_life"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExclusionCategory {
    Condition,
    BodySystem,
    MentalHealth,
    Pursuit,
    Occupation,
    Travel,
    PreExisting,
    /// A migrated clause carried word for word. Never matched automatically.
    Verbatim,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExclusionEntry {
    pub code: String,
    pub version: u32,
    pub category: ExclusionCategory,
    pub applies_to: BTreeSet<String>,
    pub cause_tags: Vec<String>,
    /// SHA-256 of the clause wording. The wording itself is not held here.
    pub wording_hash: String,
}

/// The exclusion code list as plain data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExclusionLibrary {
    pub id: String,
    pub version: u32,
    pub effective_from: NaiveDate,
    pub content_hash: String,
    pub entries: BTreeMap<String, ExclusionEntry>,
}

/// The clause titles and wordings. Restricted: underwriters, claims assessors
/// on an open claim, and the person the clause applies to.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct ExclusionWordings {
    by_code: BTreeMap<String, (String, String)>,
}

impl std::fmt::Debug for ExclusionWordings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Wordings never reach a log line.
        write!(f, "ExclusionWordings({} clauses)", self.by_code.len())
    }
}

impl ExclusionWordings {
    /// The clause title and wording for a code.
    pub fn clause(&self, code: &str) -> Option<(&str, &str)> {
        self.by_code.get(code).map(|(t, w)| (t.as_str(), w.as_str()))
    }
}

/// An exclusion as it sits on a cover layer: the code and its dates.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AppliedExclusion {
    pub code: String,
    /// First day the exclusion applies.
    pub from: NaiveDate,
    /// First day it no longer applies. None for a permanent exclusion.
    #[serde(default)]
    pub until: Option<NaiveDate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchOutcome {
    Applies,
    DoesNotApply,
    /// The code or the cause could not be judged mechanically.
    NeedsAssessor,
}

/// Whether a tag is well formed: dotted segments of `[a-z0-9_]`, none empty.
pub fn well_formed_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag
            .split('.')
            .all(|seg| !seg.is_empty() && seg.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'))
}

fn root_of(tag: &str) -> &str {
    tag.split('.').next().unwrap_or(tag)
}

/// `tag` equals `ancestor` or is a dotted descendant of it.
fn within(tag: &str, ancestor: &str) -> bool {
    tag == ancestor || (tag.len() > ancestor.len() && tag.starts_with(ancestor) && tag.as_bytes()[ancestor.len()] == b'.')
}

impl ExclusionLibrary {
    pub fn contains(&self, code: &str) -> bool {
        self.entries.contains_key(code)
    }

    /// The library as the claims rules carry it. Claims cannot depend on
    /// this crate (its state lives in the core, which this crate depends
    /// on), so its rules hold the same entries as data and are activated
    /// from this. The two matchers are held equal by a test.
    pub fn for_claims(&self) -> BTreeMap<String, ic_core::claims_module::config::Exclusion> {
        self.entries
            .iter()
            .map(|(code, e)| {
                let exclusion = ic_core::claims_module::config::Exclusion {
                    applies_to: e.applies_to.clone(),
                    cause_tags: e.cause_tags.clone(),
                    verbatim: e.category == ExclusionCategory::Verbatim,
                };
                (code.clone(), exclusion)
            })
            .collect()
    }

    /// Does this exclusion catch a claim with these coded causes on this
    /// date? Errs towards a person: anything not judged with certainty goes
    /// to an assessor and is never silently treated as not applying.
    pub fn matches(&self, applied: &AppliedExclusion, cover_type: &str, cause_tags: &[String], event_date: NaiveDate) -> MatchOutcome {
        let Some(entry) = self.entries.get(&applied.code) else {
            return MatchOutcome::NeedsAssessor;
        };
        if entry.category == ExclusionCategory::Verbatim {
            return MatchOutcome::NeedsAssessor;
        }
        if event_date < applied.from || applied.until.is_some_and(|u| event_date >= u) {
            return MatchOutcome::DoesNotApply;
        }
        if !entry.applies_to.contains(cover_type) {
            return MatchOutcome::DoesNotApply;
        }
        let recognised = |t: &String| well_formed_tag(t) && root_of(t) != "unknown" && CAUSE_ROOTS.contains(&root_of(t));
        if cause_tags.is_empty() || !cause_tags.iter().all(recognised) {
            return MatchOutcome::NeedsAssessor;
        }
        if cause_tags.iter().any(|t| entry.cause_tags.iter().any(|x| within(t, x))) {
            MatchOutcome::Applies
        } else {
            MatchOutcome::DoesNotApply
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawExclusion {
    code: String,
    version: u32,
    category: ExclusionCategory,
    applies_to: BTreeSet<String>,
    cause_tags: Vec<String>,
    title: String,
    wording: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLibrary<E> {
    id: String,
    version: u32,
    effective_from: NaiveDate,
    entries: Vec<E>,
}

fn well_formed_code(code: &str, prefix: &str) -> bool {
    code.strip_prefix(prefix).is_some_and(|d| d.len() == 4 && d.bytes().all(|b| b.is_ascii_digit()))
}

/// Load the exclusion library. The module must define `EXCLUSIONS`.
pub fn load_exclusions(file: &str, source: &str) -> Result<(ExclusionLibrary, ExclusionWordings), LoadError> {
    let raw: RawLibrary<RawExclusion> = eval(file, source, "EXCLUSIONS")?;
    if raw.id.is_empty() || raw.version == 0 || raw.entries.is_empty() {
        return Err(invalid(file, "exclusion library needs an id, a version and at least one code"));
    }
    let mut entries = BTreeMap::new();
    let mut wordings = ExclusionWordings::default();
    for e in raw.entries {
        if !well_formed_code(&e.code, "EX-") {
            return Err(invalid(file, format!("exclusion code {} is not EX- and four digits", e.code)));
        }
        let verbatim = e.category == ExclusionCategory::Verbatim;
        if verbatim != e.code.starts_with("EX-9") {
            return Err(invalid(
                file,
                format!("{}: the EX-9xxx range is for verbatim clauses, and only for them", e.code),
            ));
        }
        if e.version == 0 || e.wording.is_empty() || e.title.is_empty() {
            return Err(invalid(file, format!("{}: needs a version, a title and wording", e.code)));
        }
        if e.applies_to.is_empty() || e.applies_to.iter().any(|c| !COVER_TYPES.contains(&c.as_str())) {
            return Err(invalid(file, format!("{}: applies_to must name known cover types", e.code)));
        }
        if !verbatim && e.cause_tags.is_empty() {
            return Err(invalid(file, format!("{}: an exclusion that is matched needs cause tags", e.code)));
        }
        if let Some(bad) = e
            .cause_tags
            .iter()
            .find(|t| !well_formed_tag(t) || !CAUSE_ROOTS.contains(&root_of(t)) || root_of(t) == "unknown")
        {
            return Err(invalid(file, format!("{}: cause tag {bad} is not in the claims taxonomy", e.code)));
        }
        let entry = ExclusionEntry {
            code: e.code.clone(),
            version: e.version,
            category: e.category,
            applies_to: e.applies_to,
            cause_tags: e.cause_tags,
            wording_hash: hex::encode(Sha256::digest(e.wording.as_bytes())),
        };
        wordings.by_code.insert(e.code.clone(), (e.title, e.wording));
        if entries.insert(e.code.clone(), entry).is_some() {
            return Err(invalid(file, format!("exclusion code {} is declared twice", e.code)));
        }
    }
    Ok((
        ExclusionLibrary {
            id: raw.id,
            version: raw.version,
            effective_from: raw.effective_from,
            content_hash: content_hash(source),
            entries,
        },
        wordings,
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonKind {
    /// Why a rule or a person reached a medical, financial or occupational view.
    Assessment,
    /// Why a case was routed where it was.
    Routing,
    /// Why evidence was asked for or could not be had.
    Evidence,
    /// Why a proposal ended.
    Closure,
    /// A decision that arrived as data: migrated or delegated.
    Source,
}

/// One entry in the controlled reason list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReasonEntry {
    pub code: String,
    pub kind: ReasonKind,
    /// Whether the customer may be told this reason. Reasons that may not be
    /// disclosed are held under the stricter access class.
    pub disclosable: bool,
    /// The plain words the customer is told. Empty when not disclosable.
    pub text: String,
}

/// The reason code list. Reasons imply health, so they stay in this module:
/// they are never sent to the core or put in an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReasonLibrary {
    pub id: String,
    pub version: u32,
    pub effective_from: NaiveDate,
    pub content_hash: String,
    pub entries: BTreeMap<String, ReasonEntry>,
}

/// Reasons the module itself gives. The shipped library must define them.
pub mod reason {
    pub const EVIDENCE_NOT_OBTAINED: &str = "RC-0901";
    pub const EVIDENCE_COMPLETE: &str = "RC-0902";
    pub const PROPOSAL_EXPIRED: &str = "RC-0910";
    pub const OFFER_EXPIRED: &str = "RC-0911";
    pub const OFFER_DECLINED: &str = "RC-0912";
    pub const WITHDRAWN: &str = "RC-0913";
    pub const COUNTER_OFFER: &str = "RC-0920";
    pub const FACULTATIVE_REQUIRED: &str = "RC-0930";
    pub const FACULTATIVE_DECLINED: &str = "RC-0931";
    pub const FACULTATIVE_TERMS: &str = "RC-0932";
    pub const RISK_EXCLUDED_BY_TREATY: &str = "RC-0933";
    pub const RULES_CLEAN: &str = "RC-0001";
    pub const COUNTERSIGN_REFUSED: &str = "RC-0940";
    pub const INTERVIEW_CONFIRMED: &str = "RC-0950";
    pub const MIGRATED: &str = "RC-0960";
    pub const MIGRATED_UNKNOWN_TERMS: &str = "RC-0961";
    pub const DELEGATED: &str = "RC-0970";
    pub const TAKEOVER: &str = "RC-0975";
    pub const DELEGATED_REJECTED: &str = "RC-0971";
    pub const NON_DISCLOSURE_REVIEW: &str = "RC-0980";
    pub const QUOTE_SUPERSEDED: &str = "RC-0990";

    pub const REQUIRED: &[&str] = &[
        EVIDENCE_NOT_OBTAINED,
        EVIDENCE_COMPLETE,
        PROPOSAL_EXPIRED,
        OFFER_EXPIRED,
        OFFER_DECLINED,
        WITHDRAWN,
        COUNTER_OFFER,
        FACULTATIVE_REQUIRED,
        FACULTATIVE_DECLINED,
        FACULTATIVE_TERMS,
        RISK_EXCLUDED_BY_TREATY,
        RULES_CLEAN,
        COUNTERSIGN_REFUSED,
        INTERVIEW_CONFIRMED,
        MIGRATED,
        MIGRATED_UNKNOWN_TERMS,
        DELEGATED,
        TAKEOVER,
        DELEGATED_REJECTED,
        NON_DISCLOSURE_REVIEW,
        QUOTE_SUPERSEDED,
    ];
}

impl ReasonLibrary {
    pub fn contains(&self, code: &str) -> bool {
        self.entries.contains_key(code)
    }

    /// The first of `codes` that is not in the list.
    pub fn first_unknown<'a>(&self, codes: impl IntoIterator<Item = &'a String>) -> Option<&'a String> {
        codes.into_iter().find(|c| !self.contains(c))
    }

    /// Split reasons into the plain words the customer may be told and the
    /// codes that may not be disclosed.
    pub fn for_customer<'a>(&self, codes: impl IntoIterator<Item = &'a String>) -> (Vec<String>, Vec<String>) {
        let mut told = Vec::new();
        let mut withheld = Vec::new();
        for c in codes {
            match self.entries.get(c) {
                Some(e) if e.disclosable => told.push(e.text.clone()),
                _ => withheld.push(c.clone()),
            }
        }
        (told, withheld)
    }
}

/// Load the reason library. The module must define `REASONS`.
pub fn load_reasons(file: &str, source: &str) -> Result<ReasonLibrary, LoadError> {
    let raw: RawLibrary<ReasonEntry> = eval(file, source, "REASONS")?;
    if raw.id.is_empty() || raw.version == 0 {
        return Err(invalid(file, "reason library needs an id and a version"));
    }
    let mut entries = BTreeMap::new();
    for e in raw.entries {
        if !well_formed_code(&e.code, "RC-") {
            return Err(invalid(file, format!("reason code {} is not RC- and four digits", e.code)));
        }
        if e.disclosable == e.text.is_empty() {
            return Err(invalid(
                file,
                format!("{}: a disclosable reason needs its plain words, and a withheld one must have none", e.code),
            ));
        }
        if let Some(old) = entries.insert(e.code.clone(), e) {
            return Err(invalid(file, format!("reason code {} is declared twice", old.code)));
        }
    }
    if let Some(missing) = reason::REQUIRED.iter().find(|c| !entries.contains_key(**c)) {
        return Err(invalid(file, format!("the list must define {missing}, which the module itself uses")));
    }
    Ok(ReasonLibrary {
        id: raw.id,
        version: raw.version,
        effective_from: raw.effective_from,
        content_hash: content_hash(source),
        entries,
    })
}
