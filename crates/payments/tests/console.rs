//! The lists the staff payments console reads.

mod common;

use common::*;
use ic_payments::model::{PayeeKind, RunType};
use ic_payments::query::answer;
use ic_payments::Command;
use serde_json::json;

fn ask(desk: &Desk, question: serde_json::Value) -> serde_json::Value {
    answer(&desk.state, question).unwrap()
}

/// Purpose: open runs are listed by value date with their counts and live
/// total; closed and cancelled runs are not.
/// Why it matters: the console's run list is where makers and approvers
/// find their work. A run missing from it is a payment nobody approves.
#[test]
fn open_runs_are_listed_with_their_totals() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Customer, "01-0001-1234579-00");
    desk.prepare("r2", RunType::LumpSumClaims, d(2027, 11, 16));
    desk.add("r2", vec![pay("i1", "p1", 100_00), pay("i2", "p1", 250_00)]);
    desk.prepare("r1", RunType::Commission, d(2027, 11, 15));
    desk.prepare("r3", RunType::Commission, d(2027, 11, 15));
    desk.ok(&mia(), Command::CancelRun { run: "r3".to_string() });
    let runs = ask(&desk, json!({"q": "open_runs"}));
    assert_eq!(runs.as_array().unwrap().len(), 2);
    assert_eq!(runs[0]["run"], "r1");
    assert_eq!(
        runs[1],
        json!({
            "run": "r2", "run_type": "lump_sum_claims", "fund": "sf1", "status": "prepared",
            "value_date": "2027-11-16", "urgent": false, "makers": ["mia"], "approvers": [],
            "items": 2, "held": 0, "total_minor": 350_00
        })
    );
}

/// Purpose: unresolved exceptions are listed oldest first, on every account
/// or one, with their owner; a resolved one drops off.
/// Why it matters: reconcilers work this list each morning, and money
/// nobody can see on it ages unidentified.
#[test]
fn open_exceptions_are_listed_across_accounts() {
    let mut desk = Desk::new();
    desk.statement("anz-sf1-collection", d(2027, 11, 15), 10_000_000_00, &[("l1", 5_00, "who")]);
    desk.at(t(2027, 11, 16, 9, 0));
    desk.statement("bnz-sf1-collection", d(2027, 11, 16), 10_000_000_00, &[("l2", 7_00, "what")]);
    let all = ask(&desk, json!({"q": "open_exceptions"}));
    assert_eq!(all.as_array().unwrap().len(), 2);
    assert_eq!((all[0]["amount_minor"].clone(), all[0]["owner"].clone()), (json!(5_00), json!(null)));
    let first = all[0]["exception"].as_str().unwrap().to_string();
    desk.ok(
        &rae(),
        Command::AssignException {
            exception: first.clone(),
            owner: "rae".to_string(),
        },
    );
    let one = ask(&desk, json!({"q": "open_exceptions", "account": "anz-sf1-collection"}));
    assert_eq!((one.as_array().unwrap().len(), one[0]["owner"].clone()), (1, json!("rae")));
    desk.ok(
        &rae(),
        Command::ResolveException {
            exception: first,
            resolution: "returned to payer".to_string(),
        },
    );
    assert_eq!(ask(&desk, json!({"q": "open_exceptions", "account": "anz-sf1-collection"})), json!([]));
}

/// Purpose: a party's payees and authorities are listed with what stops
/// each, and no account number appears anywhere in the answer.
/// Why it matters: staff look a customer up by party; showing account
/// numbers on a list would spread them to everyone who can open it.
#[test]
fn a_party_is_shown_without_its_account_numbers() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Customer, "01-0001-1234579-00");
    desk.authority("a1", "pol-1", "01-0001-7654338-00", false);
    let party = ask(&desk, json!({"q": "party", "party": "party-p1"}));
    assert_eq!(party["payees"][0]["payee"], "p1");
    assert_eq!(party["payees"][0]["blocked"], json!(null));
    assert_eq!(party["authorities"], json!([]));
    assert_eq!(party["customer_hold"], json!(null));
    let payer = ask(&desk, json!({"q": "party", "party": "party-a1"}));
    assert_eq!(
        (payer["authorities"][0]["authority"].clone(), payer["authorities"][0]["status"].clone()),
        (json!("a1"), json!("active"))
    );
    for text in [party.to_string(), payer.to_string()] {
        assert!(!text.contains("01-0001-1234579-00") && !text.contains("01-0001-7654338-00"), "{text}");
    }
    assert!(answer(&desk.state, json!({"q": "party"})).is_err());
}

/// A collection run whose file the channel took after the cutoff: makers
/// are told it went for a later day.
fn deferred_collection(desk: &mut Desk, run: &str, day: chrono::NaiveDate) {
    desk.at(day.and_hms_opt(9, 0, 0).unwrap());
    desk.prepare(run, ic_payments::model::RunType::Collection, day);
    desk.add(run, vec![collect(&format!("{run}-c1"), "a1", "pol-1", 150_00)]);
    let s = desk.approve(run);
    desk.at(day.and_hms_opt(20, 0, 0).unwrap());
    desk.deliver(&s);
}

/// Purpose: a notice the module raises is kept for its role until someone
/// holding that role closes it with what they did; nobody else can close
/// it, a closed one is not reopened, and closed ones are forgotten after
/// the keeping period.
/// Why it matters: a deferred file, a large payment or a refused posting
/// needs a person. If the only trace is a log line, the work depends on
/// someone happening to read the log.
#[test]
fn notices_wait_for_someone_with_the_role() {
    let mut desk = Desk::new();
    desk.authority("a1", "pol-1", "01-0001-1234579-00", false);
    deferred_collection(&mut desk, "d1", d(2027, 11, 19));
    let open = ask(&desk, json!({"q": "notices", "role": "maker"}));
    assert_eq!(open[0][1]["subject"], "submission_deferred");
    let id = open[0][0].as_u64().unwrap();
    assert_eq!(ask(&desk, json!({"q": "notices", "role": "approver"})), json!([]));
    assert!(answer(&desk.state, json!({"q": "notices"})).unwrap_err().contains("role"));
    let close = |action: &str| Command::CloseNotice {
        notice: id,
        action: action.to_string(),
    };
    assert_eq!(desk.code(&ari(), close("seen")), "not_permitted");
    assert_eq!(desk.code(&mia(), close(" ")), "action_required");
    assert_eq!(
        desk.code(
            &mia(),
            Command::CloseNotice {
                notice: 999,
                action: "x".into()
            }
        ),
        "unknown_notice"
    );
    desk.ok(&mia(), close("customers told; no fees charged"));
    assert_eq!(desk.code(&mia(), close("again")), "notice_closed");
    assert_eq!(ask(&desk, json!({"q": "notices", "role": "maker"})), json!([]));
    assert!(desk.state.notices.items.contains_key(&id));

    // Once a closed notice is older than the keeping period, the next
    // notice raised forgets it.
    if let Some(closed) = desk.state.notices.items.get_mut(&id).and_then(|n| n.closed.as_mut()) {
        closed.1 -= chrono::Duration::days(ic_payments::notices::KEEP_CLOSED_DAYS);
    }
    deferred_collection(&mut desk, "d2", d(2027, 11, 26));
    assert!(!desk.state.notices.items.contains_key(&id));
    assert_eq!(ask(&desk, json!({"q": "notices", "role": "maker"})).as_array().unwrap().len(), 1);
}
