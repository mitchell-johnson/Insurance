//! Policy suspense aged from the ledger's own feed, and documented actions.

mod common;

use common::*;
use ic_finance::books::FinEvent;
use ic_finance::close::CheckStatus;
use ic_finance::{host, FinCommand};
use ic_money::FINE_PER_MINOR;
use serde_json::{json, Value};

const CHECK: &str = "aged:policy_suspense";

fn aged(w: &World, as_at: chrono::NaiveDate) -> Value {
    host::answer(&w.books.state, json!({ "q": "aged_suspense", "as_at": as_at })).unwrap()
}

fn report(w: &mut World, as_at: chrono::NaiveDate) {
    let answer = aged(w, as_at);
    let command = FinCommand::ReportAgedItems {
        kind: "policy_suspense".to_string(),
        as_at: serde_json::from_value(answer["as_at"].clone()).unwrap(),
        items: serde_json::from_value(answer["items"].clone()).unwrap(),
        total_minor: answer["total_minor"].as_i64().unwrap(),
        count: answer["count"].as_u64().unwrap() as u32,
    };
    w.books.send("close-workflow", None, command).unwrap();
}

fn status(w: &World, p: ic_finance::PeriodId) -> CheckStatus {
    w.books.state.checks(p).into_iter().find(|c| c.id == CHECK).unwrap().status
}

/// Purpose: two policies pay ahead on 15 November 2027. One pays exactly two months, so its
/// suspense is used up when December's premium falls due and it pays again on 20 December;
/// the other pays far more than it owes and the excess sits. The ledger's own answer lists
/// each policy with what is held and the date it has been held since (20 December for the
/// first: the earlier holding ended), and the total equals the policy suspense control
/// account to the cent. At the December close the item held since November is too old and
/// turns the check red; a documented action turns it green, stays with the item when the
/// same holding is reported again, and nobody without the role, and no item that is not in
/// the report, can be documented.
/// Why it matters: unapplied customer money is the balance an auditor asks about first. It
/// has to come from the same postings as the ledger so the two cannot disagree, age from
/// when the money actually arrived, and stay red until a named person has written down
/// what is being done about it.
#[test]
fn suspense_is_aged_from_the_feed_and_needs_a_documented_action() {
    let start = d(2027, 11, 15);
    let mut w = World::new(2, start);
    let (p1, used) = w.issue(1, start);
    let (p2, sits) = w.issue(2, start);
    w.pay(p1, &used, "two-months", 2 * MONTHLY);
    w.pay(p2, &sits, "far-too-much", 40 * MONTHLY);
    w.until(d(2027, 12, 3));

    let november = aged(&w, d(2027, 11, 30));
    assert_eq!(november["as_at"], json!("2027-12-02"), "the balances are as at the latest day closed");
    assert_eq!(november["count"], json!(2));
    let items = november["items"].as_array().unwrap();
    assert!(items
        .iter()
        .all(|i| i["since"] == json!("2027-11-15") && i["reason"] == json!("policy_suspense")));
    let of = |answer: &Value, policy: &str| answer["items"].as_array().unwrap().iter().find(|i| i["party"] == json!(policy)).cloned();
    assert_eq!(of(&november, &used.0).unwrap()["amount_minor"], json!(MONTHLY));
    assert_eq!(of(&november, &sits.0).unwrap()["amount_minor"], json!(39 * MONTHLY));

    w.until(d(2027, 12, 18));
    assert!(of(&aged(&w, d(2027, 12, 17)), &used.0).is_none(), "December's premium used the money up");
    w.until(d(2027, 12, 20));
    w.pay(p1, &used, "again", MONTHLY);
    w.until(d(2028, 1, 4));

    let dec = period(2027, 12);
    let december = aged(&w, dec.end());
    assert_eq!(of(&december, &used.0).unwrap()["since"], json!("2027-12-20"));
    let old = of(&december, &sits.0).unwrap();
    assert_eq!(old["since"], json!("2027-11-15"));
    // Oldest first.
    assert_eq!(december["items"][0]["party"], json!(sits.0));
    // The same money as the control account, to the cent.
    let control: i128 = w
        .books
        .state
        .trial_balance(period(2028, 1), None)
        .iter()
        .filter(|r| r.account == "2050")
        .map(|r| r.balance)
        .sum();
    assert_eq!(i128::from(december["total_minor"].as_i64().unwrap()) * FINE_PER_MINOR, -control);

    assert!(matches!(status(&w, dec), CheckStatus::Red { ref items } if items[0].contains("no policy_suspense report")));
    report(&mut w, dec.end());
    let CheckStatus::Red { items } = status(&w, dec) else {
        panic!("money held for seven weeks with no action is not red");
    };
    assert_eq!(items.len(), 1, "{items:?}");
    assert!(items[0].starts_with(&format!("{} (policy_suspense):", sits.0)) && items[0].contains("with no documented action"));

    let document = |party: &str, action: &str, owner: &str| FinCommand::DocumentAgedItem {
        kind: "policy_suspense".to_string(),
        party: party.to_string(),
        since: start,
        action: action.to_string(),
        owner: owner.to_string(),
    };
    assert_eq!(w.books.console_code("ana", document(&sits.0, "case-77", "oscar")), "authority");
    assert_eq!(w.books.console_code("olive", document(&sits.0, " ", "oscar")), "aged_action");
    assert_eq!(w.books.console_code("olive", document(&used.0, "case-77", "oscar")), "aged_item");
    assert_eq!(w.books.code("collections", None, document(&sits.0, "case-77", "oscar")), "caller");
    w.books.console("olive", document(&sits.0, "case-77: refund offered", "oscar")).unwrap();
    assert!(matches!(
        w.books.applied.last().unwrap().events[0].event,
        FinEvent::AgedItemDocumented { ref owner, ref by, .. } if owner == "oscar" && by == "olive"
    ));
    assert_eq!(status(&w, dec), CheckStatus::Green);

    // The same holding reported again the next day keeps its action.
    w.days(1);
    report(&mut w, dec.end());
    assert_eq!(status(&w, dec), CheckStatus::Green);
    let stored = &w.books.state.aged["policy_suspense"];
    let item = stored.items.iter().find(|i| i.party == sits.0).unwrap();
    assert_eq!(
        (item.action.as_deref(), item.owner.as_deref()),
        (Some("case-77: refund offered"), Some("oscar"))
    );
    assert!(stored.items.iter().find(|i| i.party == used.0).unwrap().action.is_none());
}
