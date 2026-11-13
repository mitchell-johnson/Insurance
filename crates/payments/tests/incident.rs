//! Collections that fail together for a reason that is not the payers'
//! doing (COL-024).

mod common;

use std::collections::BTreeSet;

use common::*;
use ic_payments::model::{AuthorityStatus, ReturnReason, RunType};
use ic_payments::query::answer;
use ic_payments::Command;
use serde_json::json;

fn back(item: &str, reason: ReturnReason) -> Command {
    Command::RecordReturn {
        item: item.to_string(),
        reason,
        returned_on: d(2027, 11, 16),
    }
}

fn declare(id: &str, authorities: &[&str], reason: &str) -> Command {
    Command::DeclareIncident {
        incident: id.to_string(),
        authorities: authorities.iter().map(|a| a.to_string()).collect::<BTreeSet<_>>(),
        from: d(2027, 11, 15),
        to: d(2027, 11, 15),
        reason: reason.to_string(),
    }
}

/// Purpose: COL-024. Failures on the authorities an incident names, inside
/// its dates, are tagged to it whether they came back before or after it
/// was declared: no dishonour is counted, nothing is suspended, ended or
/// presented again, collections is asked to get new details from each
/// payer, and the incident lists the policies for the remediation case. A
/// failure outside the incident is a dishonour as before.
/// Why it matters: three hundred customers whose details the provider
/// invalidated did nothing wrong. Counting it against them would suspend
/// their authorities and push their policies towards lapse.
#[test]
fn failures_in_an_incident_are_not_held_against_the_payers() {
    let mut desk = Desk::new();
    desk.authority("a1", "pol-1", "0101234567", false);
    desk.authority("a2", "pol-2", "0201234567", false);
    desk.authority("a3", "pol-3", "0301234567", false);
    desk.prepare("r1", RunType::Collection, d(2027, 11, 15));
    desk.add(
        "r1",
        vec![
            collect("c1", "a1", "pol-1", 50_00),
            collect("c2", "a2", "pol-2", 60_00),
            collect("c3", "a3", "pol-3", 70_00),
        ],
    );
    let s = desk.approve("r1");
    desk.settle(&s, d(2027, 11, 15));
    // One comes back before anyone sees the pattern.
    desk.ok(&svc(), back("c1", ReturnReason::InvalidAccount));
    assert_eq!(desk.state.authorities["a1"].status, AuthorityStatus::Cancelled);

    assert_eq!(desk.code(&mia(), declare("inc-1", &["a1", "a2"], "provider reissued tokens")), "not_permitted");
    assert_eq!(desk.code(&rae(), declare("inc-1", &["a1", "a2"], " ")), "invalid_incident");
    assert_eq!(desk.code(&rae(), declare("inc-1", &["zz"], "x")), "unknown_authority");
    desk.ok(&rae(), declare("inc-1", &["a1", "a2"], "provider reissued tokens"));
    assert_eq!(desk.code(&rae(), declare("inc-1", &["a1"], "again")), "duplicate_incident");
    assert_eq!(desk.state.authorities["a1"].dishonours, 0);

    // After it is declared: not a dishonour, not presented again.
    desk.ok(&svc(), back("c2", ReturnReason::InsufficientFunds));
    assert_eq!(desk.state.authorities["a2"].dishonours, 0);
    assert_eq!(desk.state.authorities["a2"].status, AuthorityStatus::Active);
    assert!(!desk.state.represent.contains("c2"));
    // Outside the incident: a dishonour as before.
    desk.ok(&svc(), back("c3", ReturnReason::InsufficientFunds));
    assert_eq!(desk.state.authorities["a3"].dishonours, 1);
    assert!(desk.state.represent.contains("c3"));

    let incident = answer(&desk.state, json!({"q": "incident", "incident": "inc-1"})).unwrap();
    assert_eq!(incident["items"]["c1"], json!(["pol-1", "2027-11-15"]));
    assert_eq!(incident["items"]["c2"], json!(["pol-2", "2027-11-15"]));
    let asks = answer(&desk.state, json!({"q": "notices", "role": "collections"})).unwrap();
    assert_eq!(asks.as_array().unwrap().len(), 2);
    assert_eq!(asks[0][1]["subject"], "new_details_needed");
}
