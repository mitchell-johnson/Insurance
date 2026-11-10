//! Audience scoping. Views are scoped to an audience, not a group: many
//! people see a slice of someone else's policy. Cedar policy maps a viewer's
//! relationships to the rows they may see.

use std::str::FromStr;

use cedar_policy::{Authorizer, Context, Decision, Entities, EntityId, EntityTypeName, EntityUid, PolicySet, Request};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// The policies shipped with the platform. Production loads them from the
/// configuration repository, with its own approvers.
pub const DEFAULT_POLICIES: &str = include_str!("../../../config/cedar/audience.cedar");

#[derive(Debug, thiserror::Error)]
pub enum ScopeError {
    #[error("cedar policies do not parse: {0}")]
    Policies(String),
    #[error("could not build the authorisation request: {0}")]
    Request(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relationship {
    Policyholder,
    LifeInsured,
    Member,
    Adviser,
    EmployerAdmin,
    Assignee,
}

impl Relationship {
    fn key(self) -> &'static str {
        match self {
            Relationship::Policyholder => "policyholder",
            Relationship::LifeInsured => "life_insured",
            Relationship::Member => "member",
            Relationship::Adviser => "adviser",
            Relationship::EmployerAdmin => "employer_admin",
            Relationship::Assignee => "assignee",
        }
    }
}

/// What a read model row is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowScope<'a> {
    /// `policy`, `cover`, `billing`, `scheme`, `member`, `underwriting_terms`, `claim`.
    pub kind: &'a str,
    /// The party the row is about, where it is about one person.
    pub subject: Option<&'a str>,
    /// Carries an insurance fact that implies health.
    pub sensitive: bool,
}

pub struct AudienceScope {
    policies: PolicySet,
    authorizer: Authorizer,
}

impl AudienceScope {
    pub fn new(policies: &str) -> Result<Self, ScopeError> {
        Ok(Self {
            policies: PolicySet::from_str(policies).map_err(|e| ScopeError::Policies(e.to_string()))?,
            authorizer: Authorizer::new(),
        })
    }

    pub fn shipped() -> Result<Self, ScopeError> {
        Self::new(DEFAULT_POLICIES)
    }

    /// May `viewer`, holding `relationships` to the row's policy, see the
    /// row? `fresh` says whether the relationship data is inside its
    /// staleness bound: freshness is a policy input.
    pub fn may_view(&self, viewer: &str, relationships: &[Relationship], row: &RowScope<'_>, fresh: bool) -> Result<bool, ScopeError> {
        let err = |e: String| ScopeError::Request(e);
        let party_type = EntityTypeName::from_str("Party").map_err(|e| err(e.to_string()))?;
        let uid = |ty: &str, id: &str| -> Result<EntityUid, ScopeError> {
            Ok(EntityUid::from_type_name_and_id(
                EntityTypeName::from_str(ty).map_err(|e| err(e.to_string()))?,
                EntityId::new(id),
            ))
        };
        let principal = EntityUid::from_type_name_and_id(party_type, EntityId::new(viewer));
        let action = uid("Action", "view")?;
        let resource = uid("Row", "row")?;
        // A row about nobody in particular has a subject no viewer can equal.
        let subject = json!({ "__entity": { "type": "Party", "id": row.subject.unwrap_or("\u{0}none") } });
        let entities = Entities::from_json_value(
            json!([
                { "uid": { "type": "Row", "id": "row" }, "attrs": { "kind": row.kind, "sensitive": row.sensitive, "subject": subject }, "parents": [] },
                { "uid": { "type": "Party", "id": viewer }, "attrs": {}, "parents": [] }
            ]),
            None,
        )
        .map_err(|e| err(e.to_string()))?;
        let context = Context::from_json_value(
            json!({ "relationships": relationships.iter().map(|r| r.key()).collect::<Vec<_>>(), "fresh": fresh }),
            None,
        )
        .map_err(|e| err(e.to_string()))?;
        let request = Request::new(principal, action, resource, context, None).map_err(|e| err(e.to_string()))?;
        Ok(self.authorizer.is_authorized(&request, &self.policies, &entities).decision() == Decision::Allow)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> AudienceScope {
        AudienceScope::shipped().unwrap()
    }

    #[test]
    fn policyholder_sees_policy_but_not_another_adults_health_facts() {
        let s = scope();
        let cover = RowScope {
            kind: "cover",
            subject: Some("party-partner"),
            sensitive: false,
        };
        let loading = RowScope {
            kind: "underwriting_terms",
            subject: Some("party-partner"),
            sensitive: true,
        };
        let own_loading = RowScope {
            kind: "underwriting_terms",
            subject: Some("party-owner"),
            sensitive: true,
        };
        assert!(s.may_view("party-owner", &[Relationship::Policyholder], &cover, true).unwrap());
        assert!(!s.may_view("party-owner", &[Relationship::Policyholder], &loading, true).unwrap());
        assert!(s
            .may_view("party-owner", &[Relationship::Policyholder, Relationship::LifeInsured], &own_loading, true)
            .unwrap());
    }

    #[test]
    fn life_insured_sees_only_their_own_rows() {
        let s = scope();
        let mine = RowScope {
            kind: "underwriting_terms",
            subject: Some("party-partner"),
            sensitive: true,
        };
        let theirs = RowScope {
            kind: "cover",
            subject: Some("party-owner"),
            sensitive: false,
        };
        let billing = RowScope {
            kind: "billing",
            subject: None,
            sensitive: false,
        };
        assert!(s.may_view("party-partner", &[Relationship::LifeInsured], &mine, true).unwrap());
        assert!(!s.may_view("party-partner", &[Relationship::LifeInsured], &theirs, true).unwrap());
        assert!(!s.may_view("party-partner", &[Relationship::LifeInsured], &billing, true).unwrap());
    }

    #[test]
    fn scheme_member_sees_nothing_else_from_the_scheme() {
        let s = scope();
        let own = RowScope {
            kind: "member",
            subject: Some("party-m1"),
            sensitive: false,
        };
        let other = RowScope {
            kind: "member",
            subject: Some("party-m2"),
            sensitive: false,
        };
        let invoice = RowScope {
            kind: "billing",
            subject: None,
            sensitive: false,
        };
        assert!(s.may_view("party-m1", &[Relationship::Member], &own, true).unwrap());
        assert!(!s.may_view("party-m1", &[Relationship::Member], &other, true).unwrap());
        assert!(!s.may_view("party-m1", &[Relationship::Member], &invoice, true).unwrap());
    }

    #[test]
    fn adviser_and_employer_never_see_sensitive_rows() {
        let s = scope();
        let claim = RowScope {
            kind: "claim",
            subject: Some("party-m1"),
            sensitive: true,
        };
        let invoice = RowScope {
            kind: "billing",
            subject: None,
            sensitive: false,
        };
        let cover = RowScope {
            kind: "cover",
            subject: Some("party-m1"),
            sensitive: false,
        };
        assert!(!s.may_view("party-adv", &[Relationship::Adviser], &claim, true).unwrap());
        assert!(s.may_view("party-adv", &[Relationship::Adviser], &cover, true).unwrap());
        assert!(s.may_view("party-hr", &[Relationship::EmployerAdmin], &invoice, true).unwrap());
        assert!(!s.may_view("party-hr", &[Relationship::EmployerAdmin], &claim, true).unwrap());
        assert!(!s.may_view("party-hr", &[Relationship::EmployerAdmin], &cover, true).unwrap());
    }

    #[test]
    fn no_relationship_no_access_and_stale_data_hides_sensitive_rows() {
        let s = scope();
        let cover = RowScope {
            kind: "cover",
            subject: Some("party-a"),
            sensitive: false,
        };
        let own_loading = RowScope {
            kind: "underwriting_terms",
            subject: Some("party-a"),
            sensitive: true,
        };
        assert!(!s.may_view("party-x", &[], &cover, true).unwrap());
        assert!(s.may_view("party-a", &[Relationship::LifeInsured], &own_loading, true).unwrap());
        assert!(!s.may_view("party-a", &[Relationship::LifeInsured], &own_loading, false).unwrap());
    }
}
