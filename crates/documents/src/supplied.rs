//! What a product's documents are given to render with.
//!
//! The documents the core's events require (`required_documents.rs`) are
//! rendered from the policy: its number, product, status, covers and
//! premium (`required_documents::policy_data`, or `member_data` for a
//! scheme member's own cover), the addressee's name, the business date,
//! and the facts of the event that asked for the document
//! (`triggers.rs`). A template bound to a product, of a kind those events
//! ask for, is the one `template_for_kind` picks for them, so it may use
//! those fields and no others.
//!
//! [`sample`] is that data for a synthetic persona: what "render a sample"
//! shows a template author, and the source of the field list a rendering
//! test is checked against. A test in `tests/required_documents.rs` keeps
//! it in step with what a real policy gives.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::kinds::DocumentKind;
use crate::render;

/// The kinds the core's events ask for (`triggers::triggers`).
pub const REQUIRED_KINDS: [DocumentKind; 9] = [
    DocumentKind::PolicySchedule,
    DocumentKind::LapseNotice,
    DocumentKind::SuspensionNotice,
    DocumentKind::Endorsement,
    DocumentKind::GeneralCorrespondence,
    DocumentKind::AvoidanceNotice,
    DocumentKind::PremiumReceipt,
    DocumentKind::Invoice,
    DocumentKind::PremiumChangeNotice,
];

/// Whether the core's events ask for documents of this kind, so that a
/// template of it bound to a product answers them.
pub fn answers_events(kind: DocumentKind) -> bool {
    REQUIRED_KINDS.contains(&kind)
}

/// The data a product's document of `kind` is rendered with, for a
/// synthetic persona (never a real customer), or `None` for a kind the
/// core's events never ask for: such a document is given whatever the
/// module that asks for it sends.
pub fn sample(kind: DocumentKind) -> Option<Value> {
    if !answers_events(kind) {
        return None;
    }
    let mut data = json!({
        "name": "Aroha Sample",
        "date": "2027-11-15",
        "policy": "SAMPLE-1",
        "product": "sample-product",
        "status": "in_force",
        "inception": "2027-01-15",
        "frequency": "monthly",
        "currency": "NZD",
        "covers": [{
            "cover": "life",
            "type": "life",
            "status": "in_force",
            "start": "2027-01-15",
            "sum_insured": 50_000_000,
            "layers": [{ "layer": "layer-1", "life": "life-1", "start": "2027-01-15", "sum_insured": 50_000_000 }],
        }],
        "premium": 12_345,
        "outstanding": 12_345,
        "oldest_unpaid_due": "2027-10-15",
    });
    let detail = match kind {
        DocumentKind::Endorsement => json!({
            "member": "life-1", "reference": "REINSTATE-1", "effective": "2027-11-01", "arrears": 4_500, "billed": 4_500,
        }),
        DocumentKind::GeneralCorrespondence => json!({ "ended": "lapsed" }),
        DocumentKind::PremiumReceipt => json!({ "receipt": "RC-1", "allocated": 12_345, "to_suspense": 0 }),
        DocumentKind::Invoice => json!({
            "receivable": "RV-1", "period_start": "2027-11-15", "period_end": "2027-12-14", "base": 13_000,
            "discounts": [{ "name": "Multi-policy", "amount": 655 }], "net_premium": 12_345, "charges": 0, "total": 12_345,
        }),
        DocumentKind::PremiumChangeNotice => json!({
            "entitlement": "wellness", "tier": "gold", "life": "life-1", "from": "2028-01-15", "reference": "ENT-1",
        }),
        _ => json!({}),
    };
    for (key, value) in detail.as_object().into_iter().flatten() {
        data[key] = value.clone();
    }
    Some(data)
}

/// Every field a product's document of `kind` may use, in the renderer's
/// path notation, or `None` for a kind the core's events never ask for.
/// A scheme member's own notice is also told which member it is about.
pub fn fields(kind: DocumentKind) -> Option<BTreeSet<String>> {
    let mut out = render::supplied(&sample(kind)?);
    out.insert(crate::triggers::MEMBER.to_string());
    Some(out)
}
