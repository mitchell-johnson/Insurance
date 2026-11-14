//! Placeholder wording for every template the service issues from.
//!
//! No wording has been written by legal, product or compliance yet. These
//! are best estimates, so that each letter has something to review and
//! the data it is given can be checked against real wording. Every body
//! starts with [`MARKER`], and a template whose wording carries it can be
//! submitted and tested but never activated (`placeholder_wording`): a
//! placeholder cannot reach a customer. To use one, rewrite it, remove the
//! marker and submit it as a new version.
//!
//! The wording is in `crates/documents/templates/<id>.txt`, one file a
//! template (a `/` in the id is a `-` in the file name).

use std::collections::BTreeMap;

use chrono::NaiveDate;

use crate::kinds::DocumentKind;
use crate::template::{AlternateFormat, Template};

/// The first line of every placeholder.
pub const MARKER: &str = "[Placeholder wording for legal review. Not for issue.]";

/// One placeholder: its template id, kind, title, notice period and wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placeholder {
    pub id: &'static str,
    pub kind: DocumentKind,
    pub title: &'static str,
    /// Best estimate of the notice period, for legal to confirm.
    pub notice_days: Option<u32>,
    pub body: &'static str,
}

macro_rules! placeholder {
    ($id:literal, $file:literal, $kind:ident, $title:literal, $days:expr) => {
        Placeholder {
            id: $id,
            kind: DocumentKind::$kind,
            title: $title,
            notice_days: $days,
            body: include_str!(concat!("../templates/", $file, ".txt")),
        }
    };
}

/// Every placeholder, by template id.
pub const PLACEHOLDERS: &[Placeholder] = &[
    // Letters underwriting asks for (`model::REQUESTED_KINDS`).
    placeholder!("uw-offer-made", "uw-offer-made", OfferOfTerms, "Offer of cover", None),
    placeholder!("uw-declined", "uw-declined", GeneralCorrespondence, "Cover declined", None),
    placeholder!("uw-proposal-lapsed", "uw-proposal-lapsed", GeneralCorrespondence, "Application closed", None),
    placeholder!(
        "uw-proposal-withdrawn",
        "uw-proposal-withdrawn",
        GeneralCorrespondence,
        "Application withdrawn",
        None
    ),
    placeholder!("uw-policy-issued", "uw-policy-issued", GeneralCorrespondence, "Cover started", None),
    placeholder!("uw-adviser-unavailable", "uw-adviser-unavailable", GeneralCorrespondence, "Your adviser", None),
    placeholder!("uw-interview-to-confirm", "uw-interview-to-confirm", GeneralCorrespondence, "Interview", None),
    // The text the life signs before a replacement goes ahead (ADV-028).
    placeholder!(
        "uw-replacement-disclosure",
        "uw-replacement-disclosure",
        DisclosureStatement,
        "Replacement disclosure",
        None
    ),
    // Letters other modules ask for.
    placeholder!(
        "ri-reinsurance-statement",
        "ri-reinsurance-statement",
        ReinsurerStatement,
        "Reinsurance statement",
        None
    ),
    placeholder!(
        "grp-absence-started",
        "grp-absence-started",
        GeneralCorrespondence,
        "Cover while on leave",
        None
    ),
    placeholder!(
        "grp-absence-limit-reached",
        "grp-absence-limit-reached",
        GeneralCorrespondence,
        "Cover ended while on leave",
        None
    ),
    placeholder!(
        "cl-overpayment-repayment",
        "cl-overpayment-repayment",
        GeneralCorrespondence,
        "Claim overpayment",
        None
    ),
    // Letters the complaints register owes.
    placeholder!(
        "complaint-acknowledgement",
        "complaint-acknowledgement",
        GeneralCorrespondence,
        "Complaint received",
        None
    ),
    placeholder!("complaint-deadlock-notice", "complaint-deadlock-notice", DeadlockNotice, "Final response", None),
    placeholder!("complaint-update", "complaint-update", GeneralCorrespondence, "Complaint update", None),
    placeholder!(
        "licence-event-notice",
        "licence-event-notice",
        GeneralCorrespondence,
        "Your adviser's licence",
        None
    ),
    // Dunning notices from collections (`collections/<step>`).
    placeholder!("collections/reminder", "collections-reminder", Reminder, "Premium reminder", None),
    placeholder!(
        "collections/suspension_notice",
        "collections-suspension_notice",
        SuspensionNotice,
        "Cover on hold",
        None
    ),
    placeholder!("collections/final_notice", "collections-final_notice", LapseNotice, "Final notice", None),
    // Documents the core's events require (`required_documents.rs`).
    placeholder!("policy-schedule", "policy-schedule", PolicySchedule, "Policy schedule", None),
    placeholder!("lapse-notice", "lapse-notice", LapseNotice, "Premium overdue", Some(14)),
    placeholder!("suspension-notice", "suspension-notice", SuspensionNotice, "Cover suspended", Some(14)),
    placeholder!("policy-ended", "policy-ended", GeneralCorrespondence, "Policy ended", None),
    placeholder!("avoidance-notice", "avoidance-notice", AvoidanceNotice, "Policy avoided", None),
    placeholder!("premium-receipt", "premium-receipt", PremiumReceipt, "Receipt", None),
    placeholder!("premium-notice", "premium-notice", Invoice, "Premium notice", None),
    placeholder!(
        "premium-change-notice",
        "premium-change-notice",
        PremiumChangeNotice,
        "Premium change",
        Some(30)
    ),
];

impl Placeholder {
    /// The placeholder as a template, version 1, in English, written by
    /// `author` and due for review by `review_by`. It describes no product:
    /// a template answering a trigger is bound to its products when its
    /// real wording is submitted.
    pub fn template(&self, author: &str, review_by: NaiveDate) -> Template {
        Template {
            id: self.id.to_string(),
            version: 1,
            kind: self.kind,
            title: format!("{} (placeholder)", self.title),
            products: vec![],
            definitions_version: None,
            primary_language: "en".into(),
            bodies: BTreeMap::from([("en".to_string(), self.body.to_string())]),
            controlling_statements: BTreeMap::new(),
            parts: vec![],
            alternate_formats: vec![AlternateFormat::LargePrint],
            minimum_notice_days: self.notice_days,
            review_by,
            author: author.to_string(),
        }
    }
}

/// Whether a template's wording is still a placeholder.
pub fn is_placeholder(template: &Template) -> bool {
    template.bodies.values().any(|b| b.contains(MARKER))
}
