//! Authority profiles, enforced by Cedar policy on case actions.
//!
//! Each underwriter has a profile: the most they may accept alone per line of
//! business, whether and how far they may load, whether they may exclude,
//! defer or decline, and which products they are accredited for. A profile is
//! configuration, versioned and changed with approval; every signature
//! records the profile version it was made under.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use cedar_policy::{Authorizer, Context, Decision, Entities, EntityId, EntityTypeName, EntityUid, PolicySet, Request};
use serde::{Deserialize, Serialize};
use serde_json::json;

use ic_core::PartyId;

use crate::case::Queue;
use crate::load::{eval, invalid, LoadError};

/// The policies shipped with the platform.
pub const DEFAULT_POLICIES: &str = include_str!("../../../config/cedar/underwriting.cedar");

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Underwriter,
    QueueLead,
    Manager,
}

impl Role {
    fn key(self) -> &'static str {
        match self {
            Role::Underwriter => "underwriter",
            Role::QueueLead => "queue_lead",
            Role::Manager => "manager",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorityProfile {
    pub underwriter: String,
    pub version: u32,
    /// Maximum sum insured they may accept alone, by cover type, in minor
    /// units. A cover type not listed cannot be decided.
    pub limits: BTreeMap<String, i64>,
    pub products: BTreeSet<String>,
    /// The skill queues they work.
    pub queues: BTreeSet<Queue>,
    /// Zero means they may not apply a loading.
    pub max_loading_percent: u32,
    pub may_exclude: bool,
    pub may_decline: bool,
    pub may_defer: bool,
    /// May give the second signature and review others' decisions.
    pub senior: bool,
    pub role: Role,
    /// The underwriter as a party in the customer master. Nobody acts on an
    /// application on which they are the policyholder or a life, whether or
    /// not they declared it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub party: Option<PartyId>,
}

/// Load authority profiles. The module must define `PROFILES`.
pub fn load_profiles(file: &str, source: &str) -> Result<Vec<AuthorityProfile>, LoadError> {
    let profiles: Vec<AuthorityProfile> = eval(file, source, "PROFILES")?;
    let mut seen = BTreeSet::new();
    for p in &profiles {
        if p.underwriter.is_empty() || p.version == 0 {
            return Err(invalid(file, "a profile needs an underwriter and a version"));
        }
        if !seen.insert(p.underwriter.as_str()) {
            return Err(invalid(file, format!("profile for {} is declared twice", p.underwriter)));
        }
        if p.limits.values().any(|l| *l < 0) || p.max_loading_percent > 500 {
            return Err(invalid(file, format!("profile for {}: a limit is out of range", p.underwriter)));
        }
    }
    Ok(profiles)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseAction {
    Work,
    Decide,
    Countersign,
    PeerReview,
    ReviewNonDisclosure,
    Reassign,
}

impl CaseAction {
    fn key(self) -> &'static str {
        match self {
            CaseAction::Work => "work",
            CaseAction::Decide => "decide",
            CaseAction::Countersign => "countersign",
            CaseAction::PeerReview => "peer_review",
            CaseAction::ReviewNonDisclosure => "review_non_disclosure",
            CaseAction::Reassign => "reassign",
        }
    }
}

/// What is being acted on: one cover's terms, or the case where the action
/// decides nothing (then the sum insured is the largest on the life).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subject<'a> {
    pub product: &'a str,
    pub cover_type: &'a str,
    pub sum_insured_minor: i64,
    pub loading_percent: u32,
    pub has_exclusions: bool,
    /// `standard`, `terms`, `decline`, `defer` or `none`.
    pub outcome: &'a str,
}

/// The facts about this person and this case.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Standing {
    /// Has owned the case or signed a decision on the life.
    pub touched: bool,
    /// Signed the decision now being countersigned.
    pub already_signed: bool,
    /// Has declared a relationship with someone on the application.
    pub conflict: bool,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AuthorityError {
    #[error("cedar policies do not parse: {0}")]
    Policies(String),
    #[error("could not build the authorisation request: {0}")]
    Request(String),
}

pub struct Authority {
    policies: PolicySet,
    authorizer: Authorizer,
}

impl Authority {
    pub fn new(policies: &str) -> Result<Self, AuthorityError> {
        Ok(Self {
            policies: PolicySet::from_str(policies).map_err(|e| AuthorityError::Policies(e.to_string()))?,
            authorizer: Authorizer::new(),
        })
    }

    pub fn shipped() -> Result<Self, AuthorityError> {
        Self::new(DEFAULT_POLICIES)
    }

    /// May this underwriter take this action on this subject?
    pub fn permits(&self, profile: &AuthorityProfile, action: CaseAction, subject: &Subject<'_>, standing: Standing) -> Result<bool, AuthorityError> {
        let err = |e: String| AuthorityError::Request(e);
        let uid = |ty: &str, id: &str| -> Result<EntityUid, AuthorityError> {
            Ok(EntityUid::from_type_name_and_id(
                EntityTypeName::from_str(ty).map_err(|e| err(e.to_string()))?,
                EntityId::new(id),
            ))
        };
        let limit = profile.limits.get(subject.cover_type).copied();
        let entities = Entities::from_json_value(
            json!([
                {
                    "uid": { "type": "Underwriter", "id": profile.underwriter },
                    "attrs": {
                        "products": profile.products,
                        "max_loading_percent": profile.max_loading_percent,
                        "may_exclude": profile.may_exclude,
                        "may_decline": profile.may_decline,
                        "may_defer": profile.may_defer,
                        "senior": profile.senior,
                        "role": profile.role.key(),
                    },
                    "parents": []
                },
                {
                    "uid": { "type": "Subject", "id": "subject" },
                    "attrs": {
                        "product": subject.product,
                        "sum_insured": subject.sum_insured_minor,
                        "loading_percent": subject.loading_percent,
                        "has_exclusions": subject.has_exclusions,
                        "outcome": subject.outcome,
                    },
                    "parents": []
                }
            ]),
            None,
        )
        .map_err(|e| err(e.to_string()))?;
        let context = Context::from_json_value(
            json!({
                "has_limit": limit.is_some(),
                "limit": limit.unwrap_or(0),
                "touched": standing.touched,
                "already_signed": standing.already_signed,
                "conflict": standing.conflict,
            }),
            None,
        )
        .map_err(|e| err(e.to_string()))?;
        let request = Request::new(
            uid("Underwriter", &profile.underwriter)?,
            uid("Action", action.key())?,
            uid("Subject", "subject")?,
            context,
            None,
        )
        .map_err(|e| err(e.to_string()))?;
        Ok(self.authorizer.is_authorized(&request, &self.policies, &entities).decision() == Decision::Allow)
    }
}

/// Why the policies refused this underwriter this action, in words, with
/// who can act instead: the first rule in `config/cedar/underwriting.cedar`
/// that the request falls foul of. The policies decide; this only says why.
pub fn refusal(profile: &AuthorityProfile, action: CaseAction, subject: &Subject<'_>, standing: Standing) -> String {
    let who = &profile.underwriter;
    if standing.conflict {
        return format!(
            "{who} declared a conflict with someone on this application, or is on it: it is closed to them, and another underwriter takes the case"
        );
    }
    let accredited = profile.products.contains(subject.product);
    let not_accredited = || format!("{who} is not accredited for {}: hand the case to an underwriter who is", subject.product);
    let senior = |what: &str| format!("{what} is for a senior underwriter, and {who} is not one: hand the case to a senior underwriter");
    match action {
        CaseAction::Work | CaseAction::Decide | CaseAction::Countersign if !accredited => not_accredited(),
        CaseAction::Countersign if !profile.senior => senior("the second signature"),
        CaseAction::Countersign if standing.already_signed => {
            format!("{who} made the first decision, so another senior underwriter gives the second signature")
        }
        CaseAction::Decide | CaseAction::Countersign => decide_refusal(profile, subject),
        CaseAction::PeerReview | CaseAction::ReviewNonDisclosure if !profile.senior => senior("a review"),
        CaseAction::PeerReview | CaseAction::ReviewNonDisclosure if standing.touched => {
            format!("{who} worked on or decided this case, so another senior underwriter reviews it")
        }
        CaseAction::Reassign => format!("moving a case another underwriter holds is for a queue lead or a manager, and {who} is neither: ask one"),
        _ => format!("{who} is not authorised for this"),
    }
}

/// Why a decision on these terms is beyond this profile.
fn decide_refusal(profile: &AuthorityProfile, subject: &Subject<'_>) -> String {
    let who = &profile.underwriter;
    let cover = subject.cover_type.replace('_', " ");
    match profile.limits.get(subject.cover_type) {
        None => return format!("{who} may not decide {cover} cover: hand the case to an underwriter whose profile includes it, such as a senior underwriter"),
        Some(limit) if subject.sum_insured_minor > *limit => {
            return format!(
                "{cover} cover of {} is above {who}'s limit of {}: hand the case to a senior underwriter whose limit covers it",
                ic_money::dollars(subject.sum_insured_minor),
                ic_money::dollars(*limit)
            )
        }
        Some(_) => {}
    }
    if subject.loading_percent > profile.max_loading_percent {
        return if profile.max_loading_percent == 0 {
            format!(
                "{who} may not apply a loading ({}% was asked for): hand the case to an underwriter who may, such as a senior underwriter",
                subject.loading_percent
            )
        } else {
            format!(
                "a {}% loading is above the most {who} may apply, {}%: hand the case to a senior underwriter",
                subject.loading_percent, profile.max_loading_percent
            )
        };
    }
    let barred = [
        (subject.has_exclusions && !profile.may_exclude, "apply an exclusion"),
        (subject.outcome == "decline" && !profile.may_decline, "decline cover"),
        (subject.outcome == "defer" && !profile.may_defer, "defer cover"),
    ];
    match barred.iter().find(|(barred, _)| *barred) {
        Some((_, what)) => format!("{who} may not {what}: hand the case to an underwriter who may, such as a senior underwriter"),
        None => format!("{who} is not authorised for this"),
    }
}
