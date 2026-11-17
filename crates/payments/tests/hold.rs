//! A hold a customer sets on their own money going out (SVC-022). The
//! customer master owns it; this module enforces what the master says.

mod common;

use chrono::Duration;
use common::*;
use ic_core::PayableKind;
use ic_payments::hold::{releases_value, HoldRecord};
use ic_payments::model::{Counterparty, ItemStatus, NameCheck, NewItem, PayeeKind, RefundDestination, RunType};
use ic_payments::query::answer;
use ic_payments::{Command, Event};
use serde_json::json;

fn record(party: &str, held: bool) -> Command {
    Command::RecordCustomerHold {
        party: party.to_string(),
        held,
    }
}

/// A unit-linked withdrawal or surrender paid to a payee, as the core's
/// `PayoutRaised` releases it.
fn payout(item: &str, payee: &str, amount: i64) -> NewItem {
    NewItem {
        source: format!("payout:pol-1:{item}"),
        policy: Some(policy("pol-1")),
        payable: Some(PayableKind::Benefit),
        ..pay(item, payee, amount)
    }
}

/// A claim payment to a payee.
fn claim(item: &str, payee: &str, amount: i64) -> NewItem {
    NewItem {
        source: format!("claim:cl-1:{item}"),
        policy: Some(policy("pol-1")),
        payable: Some(PayableKind::Benefit),
        ..pay(item, payee, amount)
    }
}

fn new_account() -> Command {
    Command::ChangePayeeAccount {
        payee: "p1".to_string(),
        account: account("p1", "01-0001-9999993-00"),
        name_check: NameCheck::Match,
    }
}

/// Purpose: only the pump, as the service role, records what the customer
/// master says; a change is an event and a repeat of the same answer is not.
/// Why it matters: the master owns the hold and staff cannot lift it there
/// (decision 3). If a person could record "not held" here, they could lift
/// it in payments behind the master's back.
#[test]
fn only_the_service_records_what_the_master_says() {
    let mut desk = Desk::new();
    assert_eq!(desk.code(&pam(), record("party-p1", false)), "not_permitted");
    assert_eq!(desk.code(&lee(), record("party-p1", false)), "not_permitted");
    assert_eq!(desk.code(&svc(), record("", true)), "invalid_party");
    let placed = desk.ok(&svc(), record("party-p1", true));
    assert_eq!(
        placed,
        vec![Event::CustomerHoldRecorded {
            party: "party-p1".to_string(),
            held: true
        }]
    );
    desk.at(desk.now + Duration::minutes(5));
    assert!(desk.ok(&svc(), record("party-p1", true)).is_empty());
    assert_eq!(
        answer(&desk.state, json!({"q": "customer_hold", "party": "party-p1"})).unwrap(),
        json!(HoldRecord {
            held: true,
            checked_at: desk.now
        })
    );
    assert_eq!(desk.ok(&svc(), record("party-p1", false)).len(), 1);
    assert!(!desk.state.held("party-p1"));
}

/// Purpose: while the hold stands, a withdrawal or surrender payout to the
/// customer is refused at run preparation, and their payout account can be
/// neither changed nor added to; a claim payment to them, and a collection
/// from them, go ahead; once the master says it has ended, the payout and
/// the change go through.
/// Why it matters: these are exactly what decision 3 holds and leaves
/// alone. Holding a claim payment or a premium collection would hurt the
/// customer the hold is meant to protect.
#[test]
fn a_hold_stops_value_leaving_and_nothing_else() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Customer, "01-0001-1234579-00");
    desk.authority("a1", "pol-1", "01-0001-7654338-00", false);
    desk.ok(&svc(), record("party-p1", true));
    desk.ok(&svc(), record("party-a1", true));

    desk.prepare("r1", RunType::LumpSumClaims, d(2027, 11, 15));
    let add = Command::AddRunItems {
        run: "r1".to_string(),
        items: vec![payout("w1", "p1", 500_00)],
    };
    assert_eq!(desk.code(&mia(), add.clone()), "customer_hold");
    desk.add("r1", vec![claim("c1", "p1", 900_00)]);

    desk.prepare("r2", RunType::Collection, d(2027, 11, 15));
    desk.add("r2", vec![collect("k1", "a1", "pol-1", 60_00)]);

    assert_eq!(desk.code(&pam(), new_account()), "customer_hold");
    let second = Command::RegisterPayee {
        payee: "p1b".to_string(),
        party: "party-p1".to_string(),
        kind: PayeeKind::Customer,
        account: account("p1", "0108888888"),
        name_check: NameCheck::Match,
    };
    assert_eq!(desk.code(&pam(), second), "customer_hold");

    // Every party whose hold stands is watched, so its end is heard.
    let watch = answer(&desk.state, json!({"q": "hold_watch"})).unwrap();
    assert_eq!(watch.as_object().unwrap().keys().collect::<Vec<_>>(), ["party-a1", "party-p1"]);

    desk.ok(&svc(), record("party-p1", false));
    desk.ok(&mia(), add);
    desk.ok(&pam(), new_account());
}

/// Purpose: a refund, by its refund or by the payable it discharges, takes
/// value out, and so do surrender, withdrawal and loan payouts; a claim
/// payment and an ordinary payment do not. A refund back to the account
/// an authority draws on is caught too.
/// Why it matters: the hold catches money by what it is, so a refund the
/// customer did not ask for cannot slip out through a different route.
#[test]
fn a_refund_is_value_going_out_and_a_claim_is_not() {
    let refund = NewItem {
        refund: Some("rf1".to_string()),
        payable: Some(PayableKind::Refund),
        policy: Some(policy("pol-1")),
        ..pay("x1", "p1", 10_00)
    };
    assert!(releases_value(&refund));
    assert!(releases_value(&NewItem {
        refund: None,
        ..refund.clone()
    }));
    for prefix in ["surrender:", "withdrawal:", "loan:"] {
        assert!(releases_value(&NewItem {
            source: format!("{prefix}pol-1"),
            ..pay("x2", "p1", 10_00)
        }));
    }
    assert!(!releases_value(&claim("x3", "p1", 10_00)));
    assert!(!releases_value(&pay("x4", "p1", 10_00)));
    let to_source = NewItem {
        counterparty: Counterparty::Authority("a1".to_string()),
        ..refund
    };
    let mut desk = Desk::new();
    desk.authority("a1", "pol-1", "01-0001-7654338-00", false);
    desk.ok(&svc(), record("party-a1", true));
    desk.prepare("r1", RunType::PremiumRefunds, d(2027, 11, 15));
    let add = Command::AddRunItems {
        run: "r1".to_string(),
        items: vec![to_source],
    };
    assert_eq!(desk.code(&mia(), add), "customer_hold");
}

/// Purpose: a payout already in a run when the master reports the hold is
/// held when the run is checked, is not sent, and cannot be released.
/// Why it matters: a customer who places a hold after being talked into a
/// withdrawal must still be in time while the run is being prepared.
#[test]
fn a_hold_heard_after_the_item_joined_holds_it_at_the_check() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Customer, "01-0001-1234579-00");
    desk.payee("p2", PayeeKind::Customer, "0102222222");
    desk.prepare("r1", RunType::LumpSumClaims, d(2027, 11, 15));
    desk.add("r1", vec![payout("w1", "p1", 500_00), payout("w2", "p2", 700_00)]);
    let watch = answer(&desk.state, json!({"q": "hold_watch"})).unwrap();
    assert_eq!(watch, json!({"party-p1": null, "party-p2": null}));
    desk.ok(&svc(), record("party-p1", true));
    desk.ok(&mia(), Command::CheckRun { run: "r1".to_string() });
    let items = &desk.state.runs["r1"].items;
    assert_eq!(items["w1"].status, ItemStatus::Held("customer_hold".to_string()));
    assert_eq!(items["w2"].status, ItemStatus::Pending);
    let release = Command::ReleaseItem {
        run: "r1".to_string(),
        item: "w1".to_string(),
    };
    assert_eq!(desk.code(&ben(), release), "not_releasable");
}

/// Purpose: where the configuration insists on a recent answer, a payout
/// whose party the master was not asked about lately is held at the check
/// as unconfirmed, and goes once a fresh answer says there is no hold.
/// Why it matters: a customer master that is down must stop money the hold
/// would have stopped, not wave it through because nobody heard otherwise.
#[test]
fn a_payout_waits_for_a_recent_answer_where_one_is_required() {
    let mut desk = Desk::new();
    let mut c = config();
    c.rules.hold_check_max_age_minutes = Some(30);
    let activate = Command::ActivateConfig {
        version: 2,
        config: Box::new(c),
    };
    desk.ok(&cal(), activate);
    desk.payee("p1", PayeeKind::Customer, "01-0001-1234579-00");
    desk.prepare("r1", RunType::LumpSumClaims, d(2027, 11, 15));
    desk.add("r1", vec![payout("w1", "p1", 500_00), claim("c1", "p1", 900_00)]);
    let check = Command::CheckRun { run: "r1".to_string() };
    desk.ok(&mia(), check.clone());
    let status = |desk: &Desk, item: &str| desk.state.runs["r1"].items[item].status.clone();
    assert_eq!(status(&desk, "w1"), ItemStatus::Held("hold_unconfirmed".to_string()));
    assert_eq!(status(&desk, "c1"), ItemStatus::Pending);

    desk.ok(&svc(), record("party-p1", false));
    desk.at(desk.now + Duration::minutes(30));
    desk.ok(&mia(), check.clone());
    assert_eq!(status(&desk, "w1"), ItemStatus::Pending);
    desk.at(desk.now + Duration::minutes(1));
    desk.ok(&mia(), check);
    assert_eq!(status(&desk, "w1"), ItemStatus::Held("hold_unconfirmed".to_string()));
}

/// Purpose: the parties watched include those with a refund owed or a
/// unit-linked payout waiting; a refund paid with a death claim is not.
/// Why it matters: the pump asks the master only about these, so a party
/// left off this list would have its money go out unasked.
#[test]
fn the_pump_watches_every_party_money_is_about_to_go_to() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Customer, "01-0001-1234579-00");
    desk.payee("p2", PayeeKind::Beneficiary, "0102222222");
    desk.authority("a1", "pol-1", "01-0001-7654338-00", false);
    let refund = |id: &str, pol: &str, destination: RefundDestination| Command::RequestRefund {
        refund: id.to_string(),
        policy: policy(pol),
        amount_minor: 10_00,
        currency: NZD,
        destination,
    };
    desk.ok(&mia(), refund("rf1", "pol-1", RefundDestination::Source { authority: "a1".to_string() }));
    desk.ok(
        &svc(),
        Command::PolicyOwner {
            policy: "pol-2".into(),
            owner: "party-p1".into(),
        },
    );
    desk.ok(&mia(), refund("rf2", "pol-2", RefundDestination::Payee { payee: "p1".to_string() }));
    let with_claim = RefundDestination::WithClaim {
        claim: "cl-9".to_string(),
        payee: "p2".to_string(),
    };
    desk.ok(&mia(), refund("rf3", "pol-3", with_claim));
    for entry in desk.state.outbox.keys().copied().collect::<Vec<_>>() {
        desk.ok(&svc(), Command::CompleteInstruction { entry, refused: None });
    }
    let waiting = ic_payments::intake::Waiting {
        payment: "po-1".to_string(),
        source: "payout:pol-2:po-1".to_string(),
        payee: "party-x".to_string(),
        account_ref: String::new(),
        policy: Some(policy("pol-2")),
        claim: None,
        run: None,
        amount_minor: 5_00,
        payable: Some(PayableKind::Benefit),
    };
    desk.ok(&svc(), Command::OfferPayment { waiting: Box::new(waiting) });
    let watch = answer(&desk.state, json!({"q": "hold_watch"})).unwrap();
    assert_eq!(watch, json!({"party-a1": null, "party-p1": null, "party-x": null}));
}

/// Purpose: the age the check accepts is unset (no master connected) or
/// between a minute and a day, and a configuration written before the
/// setting existed loads with it unset.
/// Why it matters: zero would hold every payout for ever, and an older
/// configuration must still load.
#[test]
fn the_answer_age_is_a_setting_within_sensible_bounds() {
    let mut desk = Desk::new();
    for minutes in [0, 24 * 60 + 1] {
        let mut c = config();
        c.rules.hold_check_max_age_minutes = Some(minutes);
        let activate = Command::ActivateConfig {
            version: 2,
            config: Box::new(c),
        };
        assert_eq!(desk.code(&cal(), activate), "invalid_config");
    }
    let written = serde_json::to_value(config().rules).unwrap();
    assert!(written.get("hold_check_max_age_minutes").is_none());
    let rules: ic_payments::config::Rules = serde_json::from_value(written).unwrap();
    assert_eq!(rules.hold_check_max_age_minutes, None);
}
