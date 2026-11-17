//! The kinds of document the insurer issues, and what each kind implies.

use serde::{Deserialize, Serialize};

/// What a document is in law and in practice. The class decides whether it
/// is bound to a version and hash, whether delivery must be provable, and
/// how long it is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentClass {
    /// Defines rights and obligations.
    Contractual,
    /// Must be provable, with proof of delivery.
    MandatoryNotice,
    /// Important, but not contractual.
    Informational,
    /// Produced by a remediation, a migration or a complaint.
    GeneratedEvidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentKind {
    PolicyWording,
    PolicySchedule,
    Endorsement,
    OfferOfTerms,
    AcceptanceRecord,
    GroupSchemeTerms,
    AgencyAgreement,

    RenewalNotice,
    PremiumChangeNotice,
    FeeChangeNotice,
    SuspensionNotice,
    LapseNotice,
    DeadlockNotice,
    ClaimDecisionLetter,
    /// The decision to avoid a policy for non-disclosure: treated as never
    /// in force, what happens to the premium, and the right to complain.
    AvoidanceNotice,
    DisclosureStatement,

    PremiumReceipt,
    Statement,
    AdviserStatement,
    ReinsurerStatement,
    Invoice,
    Reminder,
    GeneralCorrespondence,

    RemediationLetter,
    MigrationNotice,
    ComplaintOutcome,
}

impl DocumentKind {
    pub const ALL: [DocumentKind; 26] = [
        DocumentKind::PolicyWording,
        DocumentKind::PolicySchedule,
        DocumentKind::Endorsement,
        DocumentKind::OfferOfTerms,
        DocumentKind::AcceptanceRecord,
        DocumentKind::GroupSchemeTerms,
        DocumentKind::AgencyAgreement,
        DocumentKind::RenewalNotice,
        DocumentKind::PremiumChangeNotice,
        DocumentKind::FeeChangeNotice,
        DocumentKind::SuspensionNotice,
        DocumentKind::LapseNotice,
        DocumentKind::DeadlockNotice,
        DocumentKind::ClaimDecisionLetter,
        DocumentKind::AvoidanceNotice,
        DocumentKind::DisclosureStatement,
        DocumentKind::PremiumReceipt,
        DocumentKind::Statement,
        DocumentKind::AdviserStatement,
        DocumentKind::ReinsurerStatement,
        DocumentKind::Invoice,
        DocumentKind::Reminder,
        DocumentKind::GeneralCorrespondence,
        DocumentKind::RemediationLetter,
        DocumentKind::MigrationNotice,
        DocumentKind::ComplaintOutcome,
    ];

    pub fn class(self) -> DocumentClass {
        use DocumentKind::*;
        match self {
            PolicyWording | PolicySchedule | Endorsement | OfferOfTerms | AcceptanceRecord | GroupSchemeTerms | AgencyAgreement => DocumentClass::Contractual,
            RenewalNotice | PremiumChangeNotice | FeeChangeNotice | SuspensionNotice | LapseNotice | DeadlockNotice | ClaimDecisionLetter | AvoidanceNotice
            | DisclosureStatement => DocumentClass::MandatoryNotice,
            PremiumReceipt | Statement | AdviserStatement | ReinsurerStatement | Invoice | Reminder | GeneralCorrespondence => DocumentClass::Informational,
            RemediationLetter | MigrationNotice | ComplaintOutcome => DocumentClass::GeneratedEvidence,
        }
    }

    /// Whether the insurer must be able to prove the document was delivered.
    /// Every mandatory notice, and the letters a remediation or a complaint
    /// outcome sends, which the design requires to go with proof of delivery.
    pub fn requires_proof(self) -> bool {
        self.class() == DocumentClass::MandatoryNotice || matches!(self, DocumentKind::RemediationLetter | DocumentKind::ComplaintOutcome)
    }

    /// Whether the conduct regime sets requirements for the wording, so that
    /// compliance must approve the template as well as legal and product.
    pub fn needs_compliance_approval(self) -> bool {
        self.requires_proof() || self == DocumentKind::MigrationNotice
    }

    /// The kind in words, as a person reads it: `lapse notice`.
    pub fn words(self) -> String {
        serde_json::to_value(self)
            .ok()
            .and_then(|v| v.as_str().map(|s| s.replace('_', " ")))
            .unwrap_or_default()
    }
}

/// How sensitive a document is. Decides who may open it; a clinical
/// document is never found by search or included in a bulk export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DataClass {
    Ordinary,
    #[default]
    Personal,
    Financial,
    InsuranceSensitive,
    Clinical,
}
