//! The placeholder wording: every template the service issues from has
//! one, each renders from the data its letter is really given, reads
//! plainly enough to pass the module's check, and none can be activated.

mod support;

use std::collections::BTreeSet;

use ic_documents::command::{ApprovalRole, Command};
use ic_documents::complaint_letters::LetterTemplates;
use ic_documents::model::REQUESTED_KINDS;
use ic_documents::placeholders::{is_placeholder, Placeholder, MARKER, PLACEHOLDERS};
use ic_documents::DocumentKind;
use serde_json::{json, Value};
use support::*;

fn placeholder(id: &str) -> &'static Placeholder {
    PLACEHOLDERS.iter().find(|p| p.id == id).unwrap_or_else(|| panic!("no placeholder for {id}"))
}

/// What a letter request gives a template (`requested_letters.rs`): the
/// name, the subject as `reference`, the date, the reasons, and each detail
/// as text.
fn requested(detail: Value) -> Value {
    let mut data = json!({ "name": "Aroha Ngata", "reference": "prop-1", "date": "2027-11-15", "reasons": [] });
    for (k, v) in detail.as_object().unwrap() {
        data[k] = v.clone();
    }
    data
}

/// What the required documents step gives (`required_documents::policy_data`).
fn policy(extra: Value) -> Value {
    let mut data = json!({
        "name": "Aroha Ngata", "date": "2027-11-15", "policy": "KTL00000001", "product": "term-life",
        "status": "in_force", "inception": "2027-11-15", "frequency": "monthly", "currency": "NZD",
        "covers": [{ "cover": "life", "type": "life", "status": "in_force", "start": "2027-11-15", "sum_insured": 50_000_000, "layers": [] }],
        "premium": 4_635, "outstanding": null, "oldest_unpaid_due": null,
    });
    for (k, v) in extra.as_object().unwrap() {
        data[k] = v.clone();
    }
    data
}

/// The data each placeholder is given in the service, by template id.
fn sample(id: &str) -> Value {
    match id {
        "uw-declined" | "uw-offer-made" => requested(json!({ "reasons": ["Your blood pressure readings were above our limit"] })),
        "uw-replacement-disclosure" => requested(json!({
            "replaced_policy": "OLD-1", "kept": "Life cover of $500,000", "gained": "Trauma cover of $100,000",
            "lost": "Cover for a condition you have now", "old_premium": 9_000, "new_premium": 11_000,
        })),
        "ri-reinsurance-statement" => requested(json!({
            "treaty": "T-1", "reinsurer": "Southern Re", "period": "2027-10-01 to 2027-12-31", "currency": "NZD",
            "net_owed_to_reinsurer_minor": "123456", "payment_reference": "ri-stmt-9", "record": "listing-9",
        })),
        "grp-absence-started" | "grp-absence-limit-reached" => requested(json!({ "since": "2027-11-01", "cover_ends": "2028-11-01" })),
        "cl-overpayment-repayment" => requested(json!({ "repayment_reference": "OP123456", "amount": "1250.00" })),
        "complaint-acknowledgement" | "complaint-deadlock-notice" => json!({ "name": "Aroha Ngata", "reference": "c-1", "date": "2027-11-15" }),
        "complaint-update" => json!({ "name": "Aroha Ngata", "reference": "c-1", "date": "2027-11-15", "due": "2027-12-15", "number": "1" }),
        "licence-event-notice" => json!({ "name": "Aroha Ngata", "reference": "lc-1", "date": "2027-11-15", "status": "suspended", "effective": "2027-11-10" }),
        id if id.starts_with("collections/") => json!({
            "name": "Aroha Ngata", "reference": "n-1", "date": "2027-11-15", "step": id.trim_start_matches("collections/"), "policies": ["KTL00000001"],
        }),
        "lapse-notice" | "suspension-notice" => policy(json!({ "outstanding": 4_635, "oldest_unpaid_due": "2027-11-15" })),
        "policy-schedule" | "avoidance-notice" => policy(json!({})),
        "policy-ended" => policy(json!({ "ended": "lapsed" })),
        "premium-receipt" => policy(json!({ "receipt": "rc-1", "allocated": 4_635, "to_suspense": 0 })),
        "premium-notice" => policy(json!({
            "receivable": 2, "period_start": "2027-11-15", "period_end": "2027-12-14", "base": 5_000,
            "discounts": [{ "name": "Multi-policy", "amount": 500 }], "net_premium": 4_500, "charges": 135, "total": 4_635,
        })),
        "premium-change-notice" => policy(json!({ "entitlement": "wellness", "tier": "gold", "life": null, "from": "2028-01-15", "reference": "wl-7" })),
        _ => requested(json!({})),
    }
}

/// Purpose: every template the service issues from has a placeholder:
/// each letter other modules may ask for, each letter the complaints
/// register owes, each collections step, and each kind of document the
/// core's events require.
/// Why it matters: a letter whose template has no wording waits for ever;
/// legal, product and compliance need a draft of every one to start from.
#[test]
fn every_template_the_service_issues_from_has_a_placeholder() {
    for kind in REQUESTED_KINDS {
        placeholder(kind.template);
    }
    let letters = LetterTemplates::default();
    for id in [&letters.acknowledgement, &letters.deadlock_notice, &letters.update, &letters.licence_notice] {
        placeholder(id);
    }
    for step in ["reminder", "suspension_notice", "final_notice"] {
        placeholder(&format!("collections/{step}"));
    }
    let kinds: BTreeSet<DocumentKind> = PLACEHOLDERS.iter().map(|p| p.kind).collect();
    for kind in [
        DocumentKind::PolicySchedule,
        DocumentKind::LapseNotice,
        DocumentKind::SuspensionNotice,
        DocumentKind::GeneralCorrespondence,
        DocumentKind::AvoidanceNotice,
        DocumentKind::PremiumReceipt,
        DocumentKind::Invoice,
        DocumentKind::PremiumChangeNotice,
    ] {
        assert!(kinds.contains(&kind), "{kind:?}");
    }
    let ids: BTreeSet<&str> = PLACEHOLDERS.iter().map(|p| p.id).collect();
    assert_eq!(ids.len(), PLACEHOLDERS.len(), "a template id is used twice");
}

/// Purpose: each placeholder is accepted by the module (plain enough to
/// read, at least one alternate format), renders from the data its letter
/// is really given with no field missing, and says on its face that it is
/// a placeholder; the decline letter gives the reasons and how to ask for
/// a review, and says what happens when the reasons cannot be told.
/// Why it matters: the placeholders also fix the data contract each
/// letter is written against, so real wording can be dropped in without
/// a letter failing for a missing field.
#[test]
fn each_placeholder_renders_from_its_real_data() {
    let mut kit = Kit::new();
    for p in PLACEHOLDERS {
        let t = p.template("author", date("2028-11-15"));
        assert!(is_placeholder(&t));
        kit.ok("config", "author", Command::SubmitTemplate(Box::new(t.clone())));
        let text = t.render(&kit.state.parts, "en", &sample(p.id)).unwrap_or_else(|e| panic!("{}: {e}", p.id));
        assert!(text.starts_with(MARKER), "{}", p.id);
        assert!(!text.contains("{{"), "{}: {text}", p.id);
    }
    let declined = placeholder("uw-declined").template("author", date("2028-11-15"));
    let text = declined.render(&kit.state.parts, "en", &sample("uw-declined")).unwrap();
    assert!(text.contains("- Your blood pressure readings were above our limit"), "{text}");
    assert!(text.contains("ask us to review this decision"), "{text}");
    let withheld = declined.render(&kit.state.parts, "en", &requested(json!({}))).unwrap();
    assert!(withheld.contains("send the reasons to a doctor you choose"), "{withheld}");
    let notice = placeholder("premium-notice").template("author", date("2028-11-15"));
    let text = notice.render(&kit.state.parts, "en", &sample("premium-notice")).unwrap();
    assert!(text.contains("Less Multi-policy: $5.00") && text.contains("Total due: $46.35"), "{text}");
    let statement = placeholder("ri-reinsurance-statement").template("author", date("2028-11-15"));
    let text = statement.render(&kit.state.parts, "en", &sample("ri-reinsurance-statement")).unwrap();
    assert!(text.contains("NZD $1,234.56"), "{text}");
}

/// Purpose: a placeholder can be approved and tested like any template
/// but never activated, and the same template with the marker taken out
/// activates.
/// Why it matters: placeholder wording must never reach a customer,
/// however many approvals it gathers by mistake.
#[test]
fn a_placeholder_cannot_be_activated() {
    let mut kit = Kit::new();
    let p = placeholder("uw-policy-issued");
    let t = p.template("author", date("2028-11-15"));
    let r = t.reference();
    kit.ok("config", "author", Command::SubmitTemplate(Box::new(t.clone())));
    kit.approve(&r, &[ApprovalRole::Legal, ApprovalRole::Product, ApprovalRole::Compliance]);
    let case = kit.case_for(&t, &sample(p.id));
    kit.ok(
        "config",
        "author",
        Command::RunRenderingTests {
            template: r.clone(),
            cases: vec![case],
        },
    );
    let today = kit.today();
    let refused = kit.err(
        "config",
        "product",
        Command::ActivateTemplate {
            template: r,
            effective_from: today,
        },
    );
    assert!(refused.contains("placeholder_wording"), "{refused}");

    let mut written = t.clone();
    written.version = 2;
    written.bodies.insert("en".into(), p.body.replace(MARKER, "").trim().to_string());
    assert!(!is_placeholder(&written));
    kit.activate(written, &sample(p.id));
}
