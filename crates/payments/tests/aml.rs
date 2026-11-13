//! Premium paid by someone other than the policy owner (FIN-015, FIN-016).

mod common;

use common::*;
use ic_log::StateMachine;
use ic_payments::model::{Role, RunType, StatementLine};
use ic_payments::query::{answer, react};
use ic_payments::{Command, Event};
use serde_json::{json, Value};

fn carla() -> ic_payments::Actor {
    person("carla", &[Role::Compliance])
}

fn core_event(id: &str, event: Value) -> ic_core::EventRecord {
    serde_json::from_value(json!({"id": id, "partition": 1, "seq": 1, "business_date": "2027-11-15", "log_index": 1, "event": event})).unwrap()
}

fn hear(desk: &mut Desk, record: &ic_core::EventRecord) -> Vec<Event> {
    let mut events = Vec::new();
    for envelope in react(record) {
        desk.sent += 1;
        events.extend(desk.state.apply(desk.sent, envelope).unwrap());
    }
    events
}

fn issued(desk: &mut Desk, policy: &str, owner: &str) {
    let event = json!({"policy_issued": {
        "policy": policy, "number": format!("N-{policy}"), "group": "g1", "policyholder": owner,
        "product_code": "term", "product_hash": "h", "inception": "2027-01-01", "lives": [], "layers": []
    }});
    hear(desk, &core_event(&format!("issue-{policy}"), event));
}

fn applied(desk: &mut Desk, policy: &str, receipt: &str) -> Vec<Event> {
    let event = json!({"receipt_applied": {"policy": policy, "receipt": receipt, "allocated_minor": 150_00, "to_suspense_minor": 0, "fund": "sf1"}});
    hear(desk, &core_event(&format!("applied-{receipt}"), event))
}

/// Read a statement of one direct credit from an account.
fn direct_credit(desk: &mut Desk, line: &str, on: chrono::NaiveDate, from: &str) -> String {
    let a = "anz-sf1-collection";
    let opening = desk.state.books.get(a).map_or(10_000_000_00, |b| b.closing_minor);
    desk.ok(
        &svc(),
        Command::RecordStatement {
            account: a.into(),
            date: on,
            opening_minor: opening,
            closing_minor: opening + 150_00,
            lines: vec![StatementLine {
                id: line.into(),
                amount_minor: 150_00,
                reference: "premium".into(),
                payer_account: Some(from.into()),
            }],
        },
    );
    format!("match-{a}-{on}-{line}")
}

fn compliance_notices(desk: &Desk) -> usize {
    answer(&desk.state, json!({"q": "notices", "role": "compliance"}))
        .unwrap()
        .as_array()
        .unwrap()
        .len()
}

/// Purpose: a premium collected from someone other than the policy owner
/// opens one case and tells compliance once; the owner's own direct debit
/// and a credit from an account the owner's authority uses open nothing; a
/// case cleared without a relationship is good for that month only, and a
/// payment in a later month raises it again.
/// Why it matters: a third party paying someone's premium is a money
/// laundering indicator that must be looked at, but raising it on every
/// payment buries compliance in the same case each month, and the owner
/// paying from their own account is not a third party at all.
#[test]
fn a_payer_who_is_not_the_owner_is_raised_once() {
    let mut desk = Desk::new();
    issued(&mut desk, "pol-1", "party-own");
    issued(&mut desk, "pol-9", "party-a9");
    desk.authority("a1", "pol-1", "0101111111", false);
    desk.authority("a9", "pol-9", "0109999999", true);
    desk.prepare("d1", RunType::Collection, d(2027, 11, 15));
    desk.add("d1", vec![collect("c1", "a1", "pol-1", 150_00), collect("c9", "a9", "pol-9", 150_00)]);
    let s = desk.approve("d1");
    desk.settle(&s, d(2027, 11, 15));

    assert!(applied(&mut desk, "pol-9", "c9").is_empty(), "the owner's own direct debit");
    let events = applied(&mut desk, "pol-1", "c1");
    let case = "party-own|party:party-a1";
    assert_eq!(events, [Event::PayerCaseOpened { case: case.into() }]);
    assert_eq!(compliance_notices(&desk), 1);
    let receipt = direct_credit(&mut desk, "l1", d(2027, 11, 16), "01-0999-9999");
    assert!(applied(&mut desk, "pol-9", &receipt).is_empty(), "from the owner's own account");
    assert!(applied(&mut desk, "pol-9", "never-seen").is_empty());

    let receipt = direct_credit(&mut desk, "l2", d(2027, 11, 17), "0101111111");
    desk.state.payer_checks.pending.insert(
        "c1-again".into(),
        ic_payments::aml::Payer {
            payer: "party:party-a1".into(),
            seen: d(2027, 11, 20),
        },
    );
    assert!(applied(&mut desk, "pol-1", "c1-again").is_empty(), "joins the open case");
    assert_eq!(compliance_notices(&desk), 1);
    let open = answer(&desk.state, json!({"q": "payer_cases"})).unwrap();
    assert_eq!(open.as_array().unwrap().len(), 1);
    assert_eq!(open[0]["case"], case);

    let clear = |note: &str| Command::ClearPayer {
        case: case.into(),
        relationship: None,
        note: note.into(),
    };
    assert_eq!(desk.code(&mia(), clear("spouse pays")), "not_permitted");
    assert_eq!(desk.code(&carla(), clear(" ")), "invalid_clearance");
    let unknown = Command::ClearPayer {
        case: "nobody".into(),
        relationship: None,
        note: "x".into(),
    };
    assert_eq!(desk.code(&carla(), unknown), "unknown_case");
    desk.at(t(2027, 11, 21, 9, 0));
    assert_eq!(
        desk.ok(&carla(), clear("spouse pays; checked")),
        [Event::PayerCaseCleared { case: case.into() }]
    );
    assert_eq!(desk.code(&carla(), clear("again")), "case_cleared");

    assert!(
        applied(&mut desk, "pol-1", &receipt).is_empty(),
        "the same payer by the account their authority uses, cleared this month"
    );
    desk.state.payer_checks.pending.insert(
        "dec".into(),
        ic_payments::aml::Payer {
            payer: "party:party-a1".into(),
            seen: d(2027, 12, 15),
        },
    );
    assert_eq!(applied(&mut desk, "pol-1", "dec"), [Event::PayerCaseOpened { case: case.into() }]);
}

/// Purpose: a business account paying the owner's personal premium opens a
/// case; a clearance naming the customer master's relationship takes effect
/// only once the master confirms it (an unconfirmed one leaves the case open
/// and tells compliance); once it does, later payments from that account, in later months and to the
/// owner's other policies, raise nothing.
/// Why it matters: a director's company paying their cover is ordinary and
/// is explained once. If the rule kept firing every month after the
/// relationship was recorded, compliance would learn to ignore it.
#[test]
fn a_recorded_relationship_stops_the_rule() {
    let mut desk = Desk::new();
    issued(&mut desk, "pol-1", "party-own");
    issued(&mut desk, "pol-2", "party-own");
    let business = "02-0500-0012345-00";
    let receipt = direct_credit(&mut desk, "l1", d(2027, 11, 15), business);
    let case = "party-own|account:020500001234500";
    assert_eq!(applied(&mut desk, "pol-1", &receipt), [Event::PayerCaseOpened { case: case.into() }]);
    let blank = Command::ClearPayer {
        case: case.into(),
        relationship: Some(String::new()),
        note: "employer".into(),
    };
    assert_eq!(desk.code(&carla(), blank), "invalid_clearance");
    let propose = |relationship: &str| Command::ClearPayer {
        case: case.into(),
        relationship: Some(relationship.into()),
        note: "the owner's company; relationship recorded in the customer master".into(),
    };
    assert_eq!(desk.code(&carla(), propose("rel-77")), "invalid_clearance", "the reference is party/id");
    let confirm = |confirmed: bool| Command::ConfirmRelationship {
        case: case.into(),
        confirmed,
        reason: if confirmed {
            String::new()
        } else {
            "the relationship links other parties".into()
        },
    };
    assert_eq!(desk.code(&carla(), confirm(true)), "not_permitted");
    assert!(desk.ok(&svc(), confirm(true)).is_empty(), "nothing proposed yet");

    // Not cleared until the customer master confirms the relationship.
    assert!(desk.ok(&carla(), propose("party-biz/rel-1")).is_empty());
    let to_check = answer(&desk.state, json!({"q": "payer_relationships_to_check"})).unwrap();
    assert_eq!(to_check, json!([[case, "party-own", "account:020500001234500", "party-biz/rel-1"]]));
    desk.ok(&svc(), confirm(false));
    assert_eq!(compliance_notices(&desk), 2, "told the relationship was not confirmed");
    assert_eq!(answer(&desk.state, json!({"q": "payer_cases"})).unwrap().as_array().unwrap().len(), 1);

    desk.ok(&carla(), propose("party-biz/rel-77"));
    assert_eq!(desk.ok(&svc(), confirm(true)), [Event::PayerCaseCleared { case: case.into() }]);
    let detail = answer(&desk.state, json!({"q": "payer_case", "case": case})).unwrap();
    assert_eq!(detail["cleared"]["relationship"], "party-biz/rel-77");

    let receipt = direct_credit(&mut desk, "l2", d(2028, 1, 15), business);
    assert!(applied(&mut desk, "pol-2", &receipt).is_empty());
    assert_eq!(compliance_notices(&desk), 2);
    assert_eq!(desk.state.payer_checks.cases[case].policies, ["pol-1", "pol-2"]);
    assert!(answer(&desk.state, json!({"q": "payer_cases"})).unwrap().as_array().unwrap().is_empty());
}
