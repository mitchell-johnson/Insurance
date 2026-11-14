//! What the customer master records: parties, the facts about them on two
//! timelines, and their relationships.

use std::fmt;

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A party's stable opaque ID. This is the customer ID the core and every
/// module see; nothing about the person can be read from it.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PartyId(pub String);

impl PartyId {
    /// A new random ID.
    pub fn generate() -> Self {
        Self(format!("pty_{}", uuid::Uuid::new_v4().simple()))
    }

    /// Whether `text` has the shape of an ID this service issues. IDs loaded
    /// from a legacy system keep their own form, so this is only used to
    /// refuse obvious garbage in a path.
    pub fn plausible(text: &str) -> bool {
        (3..=64).contains(&text.len()) && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    }
}

impl fmt::Display for PartyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartyKind {
    Person,
    Organisation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    /// Known, with no relationship that makes them a customer yet.
    Prospect,
    Active,
    /// Death has been notified. Starts the estate process and restricts the
    /// person's policies.
    Deceased,
    /// Every relationship has ended. The retention clock is running.
    Exited,
}

impl Lifecycle {
    pub fn as_str(self) -> &'static str {
        match self {
            Lifecycle::Prospect => "prospect",
            Lifecycle::Active => "active",
            Lifecycle::Deceased => "deceased",
            Lifecycle::Exited => "exited",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        [Lifecycle::Prospect, Lifecycle::Active, Lifecycle::Deceased, Lifecycle::Exited]
            .into_iter()
            .find(|l| l.as_str() == text)
    }
}

/// The class a field belongs to. Classes decide who may read a field and
/// how long it is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldClass {
    /// Names and dates of birth and death.
    Identity,
    /// Addresses, email, phone and how the person wants to be written to.
    Contact,
    /// IRD number, tax residency and rates.
    Tax,
    /// Organisation details.
    Organisation,
    /// A vulnerability marker. Read only by policy to route actions to a
    /// person; never shown as a label.
    Restricted,
}

impl FieldClass {
    pub fn as_str(self) -> &'static str {
        match self {
            FieldClass::Identity => "identity",
            FieldClass::Contact => "contact",
            FieldClass::Tax => "tax",
            FieldClass::Organisation => "organisation",
            FieldClass::Restricted => "restricted",
        }
    }
}

macro_rules! fields {
    ($($variant:ident => $name:literal, $class:ident;)*) => {
        /// A fact the customer master holds about a party.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum Field { $($variant),* }

        impl Field {
            pub const ALL: &'static [Field] = &[$(Field::$variant),*];

            pub fn as_str(self) -> &'static str {
                match self { $(Field::$variant => $name),* }
            }

            pub fn class(self) -> FieldClass {
                match self { $(Field::$variant => FieldClass::$class),* }
            }

            pub fn parse(text: &str) -> Option<Field> {
                match text { $($name => Some(Field::$variant),)* _ => None }
            }
        }
    };
}

fields! {
    GivenName => "given_name", Identity;
    MiddleNames => "middle_names", Identity;
    FamilyName => "family_name", Identity;
    PreferredName => "preferred_name", Identity;
    DateOfBirth => "date_of_birth", Identity;
    DateOfDeath => "date_of_death", Identity;
    // What the person tells us, in their words. Never a rating factor:
    // the rating sex factor is underwriting's, kept apart (docs/decisions.md).
    Gender => "gender", Identity;
    // Right to live in New Zealand, dated: cover and tax rules turn on it
    // (`RESIDENCY_STATUSES`). `non_resident` once the person has left.
    ResidencyStatus => "residency_status", Identity;
    Email => "email", Contact;
    MobilePhone => "mobile_phone", Contact;
    PostalAddress => "postal_address", Contact;
    ResidentialAddress => "residential_address", Contact;
    PreferredLanguage => "preferred_language", Contact;
    PresentationFormat => "presentation_format", Contact;
    NoticeChannel => "notice_channel", Contact;
    // Contact points chosen for one relationship: `[{relationship, channel,
    // destination}]`, used for notices about that relationship only.
    RelationshipContacts => "relationship_contacts", Contact;
    IrdNumber => "ird_number", Tax;
    TaxResidency => "tax_residency", Tax;
    PrescribedInvestorRate => "prescribed_investor_rate", Tax;
    ResidentWithholdingRate => "resident_withholding_rate", Tax;
    OrganisationName => "organisation_name", Organisation;
    RegistrationNumber => "registration_number", Organisation;
    Vulnerability => "vulnerability", Restricted;
}

impl fmt::Display for Field {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One statement of a fact: what was true from `valid_from` (the world's
/// timeline), as learned at `recorded_at` (ours). On any date, the
/// statement in effect is the one recorded last among those covering that
/// date, so a later statement overrides earlier knowledge from its
/// `valid_from` on, and a statement about the past only carries `valid_to`. A value of `None` says the
/// fact stopped being held from that date, such as a phone number given up.
/// A correction is a later statement for the same date; nothing earlier is
/// ever changed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assertion {
    pub field: Field,
    pub value: Option<Value>,
    pub valid_from: NaiveDate,
    /// The first day it no longer held, for a fact stated about the past
    /// only (a former address). `None`: until stated otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_to: Option<NaiveDate>,
    pub recorded_at: DateTime<Utc>,
    /// Why, for a correction.
    #[serde(default)]
    pub reason: Option<String>,
    /// Where the fact came from: `self_service`, `group-scheme:S1`, `staff`,
    /// `migration`, `claims`.
    pub source: String,
}

/// What a relationship connects the party to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum Target {
    Party(PartyId),
    Policy(String),
    Scheme(String),
}

impl Target {
    pub fn kind(&self) -> &'static str {
        match self {
            Target::Party(_) => "party",
            Target::Policy(_) => "policy",
            Target::Scheme(_) => "scheme",
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Target::Party(p) => &p.0,
            Target::Policy(id) | Target::Scheme(id) => id,
        }
    }

    pub fn from_parts(kind: &str, id: &str) -> Option<Target> {
        match kind {
            "party" => Some(Target::Party(PartyId(id.to_string()))),
            "policy" => Some(Target::Policy(id.to_string())),
            "scheme" => Some(Target::Scheme(id.to_string())),
            _ => None,
        }
    }
}

macro_rules! relationships {
    ($($variant:ident => $name:literal, $target:literal;)*) => {
        /// A typed, directional relationship from the party to its target.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum RelationshipKind { $($variant),* }

        impl RelationshipKind {
            pub const ALL: &'static [RelationshipKind] = &[$(RelationshipKind::$variant),*];

            pub fn as_str(self) -> &'static str {
                match self { $(RelationshipKind::$variant => $name),* }
            }

            /// The kind of target the relationship points at.
            pub fn target_kind(self) -> &'static str {
                match self { $(RelationshipKind::$variant => $target),* }
            }

            pub fn parse(text: &str) -> Option<Self> {
                match text { $($name => Some(RelationshipKind::$variant),)* _ => None }
            }
        }
    };
}

relationships! {
    // To a policy.
    Owner => "owner", "policy";
    LifeInsured => "life_insured", "policy";
    Beneficiary => "beneficiary", "policy";
    Dependant => "dependant", "policy";
    Payer => "payer", "policy";
    InterestedParty => "interested_party", "policy";
    Assignee => "assignee", "policy";
    Claimant => "claimant", "policy";
    Adviser => "adviser", "policy";
    // To a scheme.
    EmployerAdministrator => "employer_administrator", "scheme";
    SchemeMember => "scheme_member", "scheme";
    Broker => "broker", "scheme";
    // To another party.
    Director => "director", "party";
    Trustee => "trustee", "party";
    Signatory => "signatory", "party";
    BeneficialOwner => "beneficial_owner", "party";
    Employee => "employee", "party";
    Attorney => "attorney", "party";
    Guardian => "guardian", "party";
    AuthorisedRepresentative => "authorised_representative", "party";
    Executor => "executor", "party";
    Partner => "partner", "party";
    Parent => "parent", "party";
    Staff => "staff", "party";
    // Pays the premium on the target's policies: a company for its director,
    // a parent for a child. Payments names it to clear a third-party payer.
    PremiumPayer => "premium_payer", "party";
}

impl RelationshipKind {
    /// Kinds by which one person acts for another, within a scope.
    pub fn represents(self) -> bool {
        matches!(
            self,
            RelationshipKind::Attorney | RelationshipKind::Guardian | RelationshipKind::AuthorisedRepresentative
        )
    }
}

/// What a representative may be authorised to do for someone.
pub const REPRESENTATIVE_SCOPES: [&str; 11] = [
    "view",
    "pay_premiums",
    "change_payment_method",
    "change_contact_details",
    "change_bank_details",
    "change_cover",
    "change_beneficiary",
    "surrender",
    "claim",
    "complain",
    "all",
];

/// On what footing a representative acts.
pub const REPRESENTATIVE_BASES: [&str; 3] = ["enduring_power_of_attorney", "court_order", "written_authority"];

/// Acts that move money or who receives it. A representative doing one of
/// these for their own benefit goes to a person first.
pub const SELF_BENEFIT_ACTS: [&str; 4] = ["change_beneficiary", "change_bank_details", "surrender", "claim"];

impl fmt::Display for RelationshipKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A relationship as recorded. It holds IDs, the kind, dates and a few
/// non-personal attributes (a beneficiary's share, the date a dependant
/// controls their own health information, a signing rule).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Relationship {
    pub id: String,
    pub kind: RelationshipKind,
    pub target: Target,
    pub valid_from: NaiveDate,
    #[serde(default)]
    pub valid_to: Option<NaiveDate>,
    pub recorded_at: DateTime<Utc>,
    #[serde(default)]
    pub attributes: serde_json::Map<String, Value>,
}

impl Relationship {
    /// In force on `day`: started on or before it and not ended by it.
    /// `valid_to` is the first day it no longer holds.
    pub fn in_force(&self, day: NaiveDate) -> bool {
        self.valid_from <= day && self.valid_to.is_none_or(|end| day < end)
    }
}

/// A consent the party has given or withdrawn, for one purpose.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Consent {
    pub purpose: String,
    pub granted: bool,
    pub valid_from: NaiveDate,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationOutcome {
    Passed,
    Failed,
    Referred,
}

/// An identity verification outcome from a provider. The evidence itself is
/// in the document vault; this holds its reference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verification {
    pub method: String,
    pub provider: String,
    pub outcome: VerificationOutcome,
    pub evidence_ref: String,
    pub verified_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskRating {
    Low,
    Medium,
    High,
}

/// A party's due diligence risk rating, set by the compliance module.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rating {
    pub rating: RiskRating,
    pub review_by: NaiveDate,
    pub recorded_at: DateTime<Utc>,
}

/// A statement of correction the person asked to be attached when their
/// request to correct a fact was declined.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeclinedCorrection {
    pub field: Field,
    pub statement: String,
    pub recorded_at: DateTime<Utc>,
}
