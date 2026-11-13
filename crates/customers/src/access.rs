//! Purpose-based access, decided by Cedar policy.
//!
//! Every call names the calling service (its app role in the Entra ID
//! token), the person it acts for, and a purpose. The policy in
//! `config/cedar/customers.cedar` decides which actions are allowed and,
//! field by field, which values are released. Fields not released are
//! withheld, or shown masked where a masked form says nothing useful. Every
//! decision is written to the audit trail by the service, with the policy
//! version that made it.

use std::str::FromStr;

use cedar_policy::{Authorizer, Context, Decision, Entities, EntityId, EntityTypeName, EntityUid, PolicySet, Request};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::model::Field;

/// The shipped policy.
pub const POLICY: &str = include_str!("../../../config/cedar/customers.cedar");

/// The caller, as far as a decision needs it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Ask {
    /// The calling service's role.
    pub caller: String,
    pub purpose: String,
    /// The person the caller acts for is the party asked about.
    pub self_access: bool,
    /// Kinds of relationship the acting person holds to a policy or scheme
    /// the party is also related to.
    pub actor_roles: Vec<String>,
    /// Field classes a record command touches.
    pub classes: Vec<String>,
}

pub struct Access {
    policies: PolicySet,
    authorizer: Authorizer,
    version: String,
}

impl Access {
    pub fn new(text: &str) -> Result<Self, String> {
        let policies = PolicySet::from_str(text).map_err(|e| format!("the access policy does not parse: {e}"))?;
        let version = hex::encode(&Sha256::digest(text.as_bytes())[..6]);
        Ok(Self {
            policies,
            authorizer: Authorizer::new(),
            version,
        })
    }

    pub fn shipped() -> Self {
        Self::new(POLICY).expect("the shipped policy is checked by its tests")
    }

    /// The version of the policy, for the audit trail.
    pub fn version(&self) -> &str {
        &self.version
    }

    fn decide(&self, ask: &Ask, action: &str, resource: (&str, &str), attrs: Value) -> bool {
        let uid = |ty: &str, id: &str| -> Option<EntityUid> { Some(EntityUid::from_type_name_and_id(EntityTypeName::from_str(ty).ok()?, EntityId::new(id))) };
        let entities = Entities::from_json_value(
            json!([
                { "uid": { "type": "Service", "id": ask.caller }, "attrs": {}, "parents": [] },
                { "uid": { "type": resource.0, "id": resource.1 }, "attrs": attrs, "parents": [] },
            ]),
            None,
        );
        let context = Context::from_json_value(
            json!({
                "purpose": ask.purpose,
                "self": ask.self_access,
                "actor_roles": ask.actor_roles,
                "classes": ask.classes,
            }),
            None,
        );
        let (Ok(entities), Ok(context), Some(principal), Some(action), Some(resource)) = (
            entities,
            context,
            uid("Service", &ask.caller),
            uid("Action", action),
            uid(resource.0, resource.1),
        ) else {
            return false;
        };
        match Request::new(principal, action, resource, context, None) {
            Ok(request) => self.authorizer.is_authorized(&request, &self.policies, &entities).decision() == Decision::Allow,
            Err(_) => false,
        }
    }

    /// May the caller take this action on a party (`match`, `search`,
    /// `relationships`, `export`, `routing`, `merge`, `unmerge`, `reviews`,
    /// `audit`, `operate`)?
    pub fn allows(&self, ask: &Ask, action: &str) -> bool {
        self.decide(ask, action, ("Party", "party"), json!({}))
    }

    /// May the caller send this command (`create`, `record`, ...)?
    pub fn allows_command(&self, ask: &Ask, command: &str) -> bool {
        self.decide(ask, command, ("Command", command), json!({}))
    }

    /// The fields whose values are released to the caller.
    pub fn readable(&self, ask: &Ask, fields: &[Field]) -> Vec<Field> {
        fields
            .iter()
            .copied()
            .filter(|f| self.decide(ask, "read", ("Field", f.as_str()), json!({ "name": f.as_str(), "class": f.class().as_str() })))
            .collect()
    }
}

/// A masked form of a value that is withheld, where one is safe to show:
/// enough to recognise, not enough to use.
pub fn mask(field: Field, value: &Value) -> Option<Value> {
    let s = value.as_str()?;
    let tail = |s: &str, n: usize| s.chars().rev().take(n).collect::<Vec<_>>().into_iter().rev().collect::<String>();
    let masked = match field {
        Field::Email => {
            let (local, domain) = s.split_once('@')?;
            format!("{}***@{domain}", local.chars().next()?)
        }
        Field::MobilePhone => format!("***{}", tail(s, 3)),
        Field::IrdNumber => format!("***{}", tail(s, 3)),
        _ => return None,
    };
    Some(Value::String(masked))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(caller: &str, purpose: &str) -> Ask {
        Ask {
            caller: caller.into(),
            purpose: purpose.into(),
            ..Ask::default()
        }
    }

    /// Purpose: the documents service reading for notices gets names and contact details,
    /// and nothing else; asking for another purpose gets nothing; the vulnerability marker
    /// is read only by the person themself and trained support staff.
    /// Why it matters: purpose limits what a service sees; a notice run must never receive
    /// IRD numbers or dates of birth, and an undeclared purpose must release nothing.
    #[test]
    fn fields_are_released_by_purpose() {
        let access = Access::shipped();
        let got = access.readable(&ask("documents", "notices"), Field::ALL);
        assert!(got.contains(&Field::Email) && got.contains(&Field::FamilyName) && got.contains(&Field::PresentationFormat));
        assert!(!got.contains(&Field::IrdNumber) && !got.contains(&Field::DateOfBirth) && !got.contains(&Field::Vulnerability));
        assert!(access.readable(&ask("documents", "marketing"), Field::ALL).is_empty());
        assert!(access.readable(&ask("unknown", "notices"), Field::ALL).is_empty());
        let payee = access.readable(&ask("payments", "payee_verification"), Field::ALL);
        assert_eq!(payee, vec![Field::GivenName, Field::MiddleNames, Field::FamilyName, Field::OrganisationName]);
        // The vulnerability marker is read only by the person themself in the app and by
        // trained support staff; nobody else, break glass included.
        for caller in ["staff", "claims", "compliance", "break-glass", "portal"] {
            for purpose in ["servicing", "claims", "due_diligence", "break_glass", "self_service", "vulnerability_support"] {
                for me in [false, true] {
                    let mut a = ask(caller, purpose);
                    a.self_access = me;
                    let allowed = (caller == "portal" && purpose == "self_service" && me) || (caller == "staff" && purpose == "vulnerability_support");
                    assert_eq!(
                        access.readable(&a, &[Field::Vulnerability]).contains(&Field::Vulnerability),
                        allowed,
                        "{caller} {purpose} {me}"
                    );
                }
            }
        }
    }

    /// Purpose: the portal releases a record only to the person themself, an adviser on a
    /// shared policy, or an employer administrator for scheme members' names.
    /// Why it matters: relationship-scoped access is how one customer is kept from reading
    /// another's record through the app.
    #[test]
    fn the_portal_is_scoped_by_relationship() {
        let access = Access::shipped();
        let mut a = ask("portal", "self_service");
        assert!(access.readable(&a, Field::ALL).is_empty());
        a.self_access = true;
        assert!(access.readable(&a, Field::ALL).contains(&Field::IrdNumber));
        let mut adviser = ask("portal", "advice");
        assert!(access.readable(&adviser, Field::ALL).is_empty());
        adviser.actor_roles = vec!["adviser".into()];
        let got = access.readable(&adviser, Field::ALL);
        assert!(got.contains(&Field::Email) && !got.contains(&Field::IrdNumber));
        let mut employer = ask("portal", "scheme_administration");
        employer.actor_roles = vec!["employer_administrator".into()];
        assert_eq!(
            access.readable(&employer, Field::ALL),
            vec![Field::GivenName, Field::FamilyName, Field::PreferredName, Field::DateOfBirth]
        );
    }

    /// Purpose: commands are allowed per calling service, contact changes only for the
    /// person themself, and the vulnerability marker only by staff or compliance.
    /// Why it matters: each module may send only its own commands (default deny), and a
    /// marker that routes a person's requests must not be settable from the app.
    #[test]
    fn commands_are_allowed_per_service() {
        let access = Access::shipped();
        assert!(access.allows_command(&ask("claims", "claims"), "mark_deceased"));
        assert!(!access.allows_command(&ask("documents", "notices"), "mark_deceased"));
        assert!(!access.allows_command(&ask("staff", "servicing"), "place_hold"));
        assert!(access.allows_command(&ask("legal", "legal_hold"), "place_hold"));
        let mut me = ask("portal", "self_service");
        me.classes = vec!["contact".into()];
        assert!(!access.allows_command(&me, "record"));
        me.self_access = true;
        assert!(access.allows_command(&me, "record"));
        me.classes = vec!["contact".into(), "identity".into()];
        assert!(!access.allows_command(&me, "record"));
        me.classes = vec!["restricted".into()];
        assert!(!access.allows_command(&me, "record"));
        let mut staff = ask("staff", "servicing");
        staff.classes = vec!["restricted".into()];
        assert!(access.allows_command(&staff, "record"));
        let mut compliance = ask("compliance", "due_diligence");
        compliance.classes = vec!["tax".into()];
        assert!(access.allows_command(&compliance, "record"));
        compliance.classes = vec!["contact".into()];
        assert!(!access.allows_command(&compliance, "record"));
        let mut migration = ask("migration", "migration");
        migration.classes = vec!["restricted".into()];
        assert!(!access.allows_command(&migration, "record"));
        assert!(access.allows_command(&ask("migration", "migration"), "mark_deceased"));
        assert!(access.allows(&ask("group-admin", "scheme_membership"), "match"));
        assert!(!access.allows(&ask("documents", "notices"), "match"));
        assert!(access.allows(&ask("privacy", "audit"), "audit"));
        assert!(access.allows(&ask("operations", "operations"), "operate"));
        assert!(!access.allows(&ask("staff", "servicing"), "operate"));
        assert!(!access.allows(&ask("bad\"name", "x"), "operate"));
        assert!(!access.allows_command(&ask("staff", "servicing"), "no such\"command"));
    }

    /// Purpose: masked forms keep only the first letter of an email and the last three
    /// characters of a phone or IRD number, and other fields have none.
    /// Why it matters: a masked value lets a caller confirm they have the right person
    /// without receiving anything they could use.
    #[test]
    fn masks_hide_the_useful_part() {
        assert_eq!(mask(Field::Email, &json!("aroha@x.nz")), Some(json!("a***@x.nz")));
        assert_eq!(mask(Field::MobilePhone, &json!("+64211234567")), Some(json!("***567")));
        assert_eq!(mask(Field::IrdNumber, &json!("49091850")), Some(json!("***850")));
        assert_eq!(mask(Field::FamilyName, &json!("Ngata")), None);
        assert_eq!(mask(Field::Email, &json!("nope")), None);
        assert_eq!(mask(Field::Email, &json!(3)), None);
    }

    /// Purpose: a policy that does not parse is refused, and the version changes with the
    /// text.
    /// Why it matters: a broken policy must stop the service starting rather than deny or
    /// allow everything, and each audit line must name the exact policy that decided it.
    #[test]
    fn policies_are_checked_and_versioned() {
        assert!(Access::new("permit(").is_err());
        let a = Access::new("permit(principal, action, resource);").unwrap();
        assert_ne!(a.version(), Access::shipped().version());
        assert_eq!(a.version().len(), 12);
    }
}
