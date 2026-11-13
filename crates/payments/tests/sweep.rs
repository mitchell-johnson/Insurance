//! Cash settled between statutory funds (PAY-010).

mod common;

use common::*;
use ic_log::StateMachine;
use ic_payments::config::BankAccount;
use ic_payments::model::{PayeeKind, Purpose, RunType};
use ic_payments::query::{answer, react};
use ic_payments::{Command, Event};
use serde_json::json;

/// The test configuration with a second statutory fund at both banks.
fn two_funds(desk: &mut Desk) {
    let mut c = config();
    for bank in ["anz", "bnz"] {
        for purpose in [
            Purpose::PremiumCollection,
            Purpose::ClaimsDisbursement,
            Purpose::ThirdPartyDisbursement,
            Purpose::Refunds,
            Purpose::Operating,
        ] {
            c.accounts.insert(
                format!("{bank}-sf2-{}", purpose_code(purpose)),
                BankAccount {
                    bank: bank.to_string(),
                    fund: "sf2".to_string(),
                    purpose,
                    currency: NZD,
                    at_bank: None,
                },
            );
        }
    }
    c.funds.insert("sf2".to_string());
    desk.ok(
        &cal(),
        Command::ActivateConfig {
            version: 2,
            config: Box::new(c),
        },
    );
}

fn core(desk: &mut Desk, id: &str, event: ic_core::Event) -> Vec<Event> {
    let record = ic_core::EventRecord {
        id: id.to_string(),
        partition: 0,
        seq: 1,
        business_date: d(2027, 11, 15),
        log_index: 1,
        traceparent: None,
        event,
    };
    let mut events = Vec::new();
    for envelope in react(&record) {
        let applied = desk
            .state
            .apply(desk.state.event_seq + 1000, envelope)
            .expect("the module takes the core's event");
        events.extend(applied);
    }
    events
}

fn applied(receipt: &str, fund: Option<&str>, amount: i64) -> ic_core::Event {
    ic_core::Event::ReceiptApplied {
        policy: policy("pol-1"),
        receipt: receipt.to_string(),
        allocated_minor: amount,
        to_suspense_minor: 0,
        net_by_cover_type: Default::default(),
        fund: fund.map(str::to_string),
    }
}

fn reversed(receipt: &str, amount: i64) -> ic_core::Event {
    ic_core::Event::ReceiptReversed {
        policy: policy("pol-1"),
        receipt: receipt.to_string(),
        amount_minor: amount,
    }
}

fn sweep_item(item: &str, n: u32, amount: i64) -> ic_payments::model::NewItem {
    let mut new = pay(item, "own-sf2", amount);
    new.source = format!("fund-sweep:anz-sf1-collection:sf2:{n}");
    new
}

/// A desk with two funds, a collection of 100.00 settled into fund one's
/// collection account, and fund two's own account as a payee.
fn collected() -> Desk {
    let mut desk = Desk::new();
    two_funds(&mut desk);
    desk.payee("own-sf2", PayeeKind::Own, "0102222222");
    desk.payee("someone", PayeeKind::Customer, "0103333333");
    desk.authority("a1", "pol-1", "0201234567", false);
    desk.prepare("r1", RunType::Collection, d(2027, 11, 15));
    desk.add("r1", vec![collect("c1", "a1", "pol-1", 100_00)]);
    let submission = desk.approve("r1");
    desk.settle(&submission, d(2027, 11, 15));
    desk
}

/// Purpose: PAY-010. One payment lands in fund one's collection account and
/// the core applies 60.00 of it to a policy in fund two and 40.00 in fund
/// one: the module records that fund one's account owes fund two 60.00,
/// names fund two's collection account at the same bank, keeps the close
/// from going green while it is owed, and clears it when a fund settlement
/// run pays it.
/// Why it matters: each statutory fund's bank accounts must equal its
/// ledger; money left in the wrong fund's account is a breach of the fund
/// separation the regulator relies on, even when the ledger is right.
#[test]
fn money_applied_to_another_fund_is_moved_to_its_account() {
    let mut desk = collected();
    let events = core(&mut desk, "0-1", applied("c1", Some("sf2"), 60_00));
    assert!(events.contains(&Event::FundOwed {
        account: "anz-sf1-collection".into(),
        fund: "sf2".into(),
        amount_minor: 60_00
    }));
    assert!(core(&mut desk, "0-2", applied("c1", Some("sf1"), 40_00)).is_empty());
    // Older events carry no fund, and a receipt this module never brought
    // in has no account to move it from.
    assert!(core(&mut desk, "0-3", applied("c1", None, 1_00)).is_empty());
    assert!(core(&mut desk, "0-4", applied("elsewhere", Some("sf2"), 1_00)).is_empty());

    let sweep = answer(&desk.state, json!({"q": "fund_sweep", "as_at": "2027-11-15"})).unwrap();
    assert_eq!(sweep["items"][0]["to_account"], "anz-sf2-collection");
    assert_eq!(sweep["items"][0]["open_minor"], 60_00);
    let close = desk.state.fund_cash_settlement(d(2027, 11, 16));
    assert_eq!(close["green"], false);
    assert!(close["items"].as_array().unwrap().iter().any(|i| i["sweep"] == "anz-sf1-collection"));

    desk.prepare("s1", RunType::FundSettlement, d(2027, 11, 16));
    desk.add("s1", vec![sweep_item("s1-1", 1, 60_00)]);
    let open = answer(&desk.state, json!({"q": "fund_sweep", "as_at": "2027-11-15"})).unwrap();
    assert_eq!(open["items"][0]["in_flight_minor"], 60_00);
    let submission = desk.approve("s1");
    assert_eq!(desk.state.submissions[&submission].account, "anz-sf1-collection");
    desk.settle(&submission, d(2027, 11, 16));
    assert!(desk.state.sweeps.owed.is_empty());
    assert_eq!(desk.state.fund_cash_settlement(d(2027, 11, 17))["green"], true);
}

/// Purpose: a sweep item is refused unless it is in a fund settlement run of
/// the paying account's fund, pays one of the insurer's own accounts and no
/// policy, and is for no more than is owed less what other sweep items
/// already carry.
/// Why it matters: a sweep moves the insurer's own money between funds
/// without a customer to notice a mistake; moving more than is owed, or
/// from the wrong fund, would break the separation it exists to keep.
#[test]
fn a_sweep_is_only_what_is_owed() {
    let mut desk = collected();
    core(&mut desk, "0-1", applied("c1", Some("sf2"), 60_00));
    let refused = |desk: &mut Desk, run: &str, item| {
        desk.code(
            &mia(),
            Command::AddRunItems {
                run: run.into(),
                items: vec![item],
            },
        )
    };
    desk.prepare("s1", RunType::FundSettlement, d(2027, 11, 16));
    assert_eq!(refused(&mut desk, "s1", sweep_item("x1", 1, 70_00)), "invalid_sweep");
    let mut to_customer = sweep_item("x2", 2, 10_00);
    to_customer.counterparty = ic_payments::model::Counterparty::Payee("someone".into());
    assert_eq!(refused(&mut desk, "s1", to_customer), "invalid_sweep");
    let mut wrong_from = sweep_item("x3", 3, 10_00);
    wrong_from.source = "fund-sweep:anz-sf2-collection:sf2:3".into();
    assert_eq!(refused(&mut desk, "s1", wrong_from), "invalid_sweep");
    desk.prepare("t1", RunType::FundTransfer, d(2027, 11, 16));
    assert_eq!(refused(&mut desk, "t1", sweep_item("x4", 4, 10_00)), "invalid_sweep");

    desk.add("s1", vec![sweep_item("s1-1", 1, 50_00)]);
    assert_eq!(refused(&mut desk, "s1", sweep_item("x5", 5, 20_00)), "invalid_sweep");
    desk.add("s1", vec![sweep_item("s1-2", 6, 10_00)]);
    assert!(ic_payments::sweep::sweep_of("fund-sweep::sf2:1").is_none());
    assert!(ic_payments::sweep::sweep_of("other").is_none());
}

/// Purpose: a collection dishonoured after its money was moved makes the
/// other fund owe it back, from that fund's collection account to the
/// first fund; a reversal before the move just takes back what was owed;
/// a settled sweep the bank returns is owed again.
/// Why it matters: a dishonour takes money out of the account it landed
/// in; if the cash had already followed the receipt to the other fund, the
/// first fund is short until the other fund pays it back.
#[test]
fn a_reversal_or_a_returned_sweep_is_owed_the_right_way() {
    let mut desk = collected();
    core(&mut desk, "0-1", applied("c1", Some("sf2"), 60_00));
    core(&mut desk, "0-2", reversed("c1", 20_00));
    assert_eq!(
        desk.state.sweeps.owed[&("anz-sf1-collection".to_string(), "sf2".to_string())].amount_minor,
        40_00
    );
    assert!(core(&mut desk, "0-3", reversed("not-moved", 5_00)).is_empty());

    desk.prepare("s1", RunType::FundSettlement, d(2027, 11, 16));
    desk.add("s1", vec![sweep_item("s1-1", 1, 40_00)]);
    let submission = desk.approve("s1");
    desk.settle(&submission, d(2027, 11, 16));
    assert!(desk.state.sweeps.owed.is_empty());

    // The rest of the receipt is reversed after the move: fund two owes it
    // back from its own collection account.
    core(&mut desk, "0-4", reversed("c1", 100_00));
    let back = &desk.state.sweeps.owed[&("anz-sf2-collection".to_string(), "sf1".to_string())];
    assert_eq!(back.amount_minor, 40_00);
    assert!(!desk.state.sweeps.moved.contains_key("c1"));

    // The bank sends the settled sweep back: fund one's account still owes.
    desk.ok(
        &svc(),
        Command::RecordReturn {
            item: "s1-1".into(),
            reason: ic_payments::model::ReturnReason::AccountClosed,
            returned_on: d(2027, 11, 18),
        },
    );
    assert_eq!(
        desk.state.sweeps.owed[&("anz-sf1-collection".to_string(), "sf2".to_string())].amount_minor,
        40_00
    );
}

/// Purpose: money that arrives by direct credit and is matched by cash
/// matching is followed the same way: the receipt the core names is the
/// matching instruction's key.
/// Why it matters: most money paid in by bank transfer reaches the core
/// through cash matching; a sweep that only followed direct debits would
/// leave the larger share in the wrong fund.
#[test]
fn a_matched_direct_credit_is_followed_too() {
    let mut desk = Desk::new();
    two_funds(&mut desk);
    desk.statement("anz-sf1-collection", d(2027, 11, 13), 10_000_000_00, &[("line-1", 250_00, "POL-1")]);
    let key = desk.state.outbox.values().find_map(|e| match &e.instruction {
        ic_payments::model::Instruction::Match { exception, .. } => Some(format!("match-{exception}")),
        _ => None,
    });
    let receipt = key.expect("a credit with no expectation goes to cash matching");
    let events = core(&mut desk, "0-9", applied(&receipt, Some("sf2"), 250_00));
    assert!(events.iter().any(|e| matches!(e, Event::FundOwed { amount_minor: 250_00, .. })));
}

/// Purpose: the edges of following receipts: a landing older than the
/// retention is forgotten when a newer one lands; a sweep item drawing on
/// an authority is refused.
/// Why it matters: the landings must not grow without end, and a sweep
/// drawn from a customer's account would take their money.
#[test]
fn the_edges_of_following_receipts() {
    let mut sweeps = ic_payments::sweep::Sweeps::default();
    sweeps.land("old", "anz-sf1-collection", d(2026, 1, 1));
    sweeps.land("new", "anz-sf1-collection", d(2027, 6, 1));
    assert!(!sweeps.landed.contains_key("old") && sweeps.landed.contains_key("new"));
    assert_eq!(sweeps.landed_by_day.len(), 1);

    let mut desk = collected();
    core(&mut desk, "0-1", applied("c1", Some("sf2"), 60_00));
    desk.prepare("s1", RunType::FundSettlement, d(2027, 11, 16));
    let mut drawn = sweep_item("x1", 1, 10_00);
    drawn.counterparty = ic_payments::model::Counterparty::Authority("a1".into());
    assert_eq!(
        desk.code(
            &mia(),
            Command::AddRunItems {
                run: "s1".into(),
                items: vec![drawn]
            }
        ),
        "invalid_sweep"
    );

    assert_eq!(desk.state.fund_sweep(d(2027, 11, 15))["items"][0]["open_minor"], 60_00);
}

/// Purpose: each morning the pump's request prepares one fund settlement
/// run per fund, for the next value date, paying what is open to the own
/// payee whose party is the account it pays into; asking again the same
/// day changes nothing; a sweep with no such payee is left for a person;
/// and the run still needs a person to check it and another to approve it.
/// Why it matters: a sweep nobody remembers to prepare leaves a fund's
/// cash in another fund's account and the close red. Proposing it takes
/// the remembering away without taking away the four eyes.
#[test]
fn the_days_sweep_is_proposed_and_still_needs_people() {
    let mut desk = collected();
    core(&mut desk, "0-1", applied("c1", Some("sf2"), 60_00));
    let propose = Command::ProposeFundSweep { as_at: d(2027, 11, 15) };
    assert_eq!(desk.code(&mia(), propose.clone()), "not_permitted");
    // No own payee for fund two's collection account yet: nothing is made.
    assert!(desk.ok(&svc(), propose.clone()).is_empty());
    desk.ok(
        &pam(),
        Command::RegisterPayee {
            payee: "into-sf2".to_string(),
            party: "anz-sf2-collection".to_string(),
            kind: PayeeKind::Own,
            account: account("Fund two collection", "0102222299"),
            name_check: ic_payments::model::NameCheck::Match,
        },
    );
    desk.ok(&pat(), Command::ConfirmPayee { payee: "into-sf2".to_string() });
    desk.ok(
        &svc(),
        Command::ScreenPayee {
            payee: "into-sf2".to_string(),
            clear: true,
        },
    );
    desk.at(t(2027, 11, 15, 17, 0));
    let events = desk.ok(&svc(), Command::ProposeFundSweep { as_at: d(2027, 11, 16) });
    assert!(events
        .iter()
        .any(|e| matches!(e, Event::RunPrepared { run, .. } if run == "sweep-sf1-20271116")));
    let run = &desk.state.runs["sweep-sf1-20271116"];
    assert_eq!((run.value_date, run.items.len()), (d(2027, 11, 16), 1));
    let item = &run.items["sweep-sf1-20271116-1"];
    assert_eq!(
        (item.new.amount_minor, item.new.source.as_str()),
        (60_00, "fund-sweep:anz-sf1-collection:sf2:20271116")
    );
    // Again the same day: nothing more. Nothing open the next day either.
    assert!(desk.ok(&svc(), Command::ProposeFundSweep { as_at: d(2027, 11, 16) }).is_empty());
    assert!(desk.ok(&svc(), Command::ProposeFundSweep { as_at: d(2027, 11, 17) }).is_empty());
    // The pump is a maker of the run; a person checks, another approves.
    desk.ok(
        &mia(),
        Command::CheckRun {
            run: "sweep-sf1-20271116".to_string(),
        },
    );
    desk.ok(
        &ari(),
        Command::ApproveRun {
            run: "sweep-sf1-20271116".to_string(),
        },
    );
}
