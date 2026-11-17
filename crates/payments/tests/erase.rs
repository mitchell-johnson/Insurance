//! Bank details of a person the customer master has erased (`erase.rs`):
//! retention shredding in the master does not reach the account names and
//! numbers this module keeps in clear, so the module forgets them itself
//! once the master says the person is gone.

mod common;

use common::*;
use ic_payments::aml::PayerCase;
use ic_payments::erase::{ERASED, RECHECK_DAYS};
use ic_payments::model::{AuthorityStatus, NameCheck, PayeeKind, RunType};
use ic_payments::query::answer;
use ic_payments::{Command, Event};
use serde_json::json;

fn watch(desk: &Desk) -> Vec<String> {
    serde_json::from_value(answer(&desk.state, json!({"q": "erasure_watch", "now": desk.now})).unwrap()).unwrap()
}

fn checked(party: &str, erased: bool) -> Command {
    Command::PartyChecked {
        party: party.to_string(),
        erased,
    }
}

fn case(owner: &str, payer: &str) -> PayerCase {
    PayerCase {
        owner: owner.to_string(),
        payer: payer.to_string(),
        policies: vec!["pol-1".to_string()],
        opened: d(2027, 11, 1),
        last_paid: d(2027, 11, 1),
        cleared: None,
        proposed: None,
    }
}

/// Purpose: every party holding bank details here is asked about, and asked
/// again only after the recheck period; when the master says a person is
/// erased, their payee is closed and blanked, their finished payments lose
/// the account, and a payment still with the bank keeps it until the bank
/// has finished, when the next daily check blanks the rest.
/// Why it matters: the Privacy Act's retention promise is that a person's
/// details go when the master's schedule says so; account numbers left here
/// in clear would break it, and blanking a payment the bank is still
/// processing would leave a return with nothing to reconcile against.
#[test]
fn an_erased_payees_details_are_forgotten_once_the_bank_has_finished() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Customer, "0201111111");
    desk.payee("p2", PayeeKind::Customer, "0202222222");
    assert_eq!(watch(&desk), ["party-p1", "party-p2"]);
    desk.prepare("r1", RunType::LumpSumClaims, d(2027, 11, 15));
    desk.add("r1", vec![pay("i1", "p1", 100_00)]);
    let s1 = desk.approve("r1");
    desk.settle(&s1, d(2027, 11, 15));
    desk.prepare("r2", RunType::PremiumRefunds, d(2027, 11, 15));
    desk.add("r2", vec![pay("i2", "p1", 50_00)]);
    let s2 = desk.approve("r2");
    desk.deliver(&s2);

    assert!(desk.ok(&svc(), checked("party-p2", false)).is_empty());
    assert_eq!(watch(&desk), ["party-p1"], "p2 is not asked again yet");
    let events = desk.ok(&svc(), checked("party-p1", true));
    assert!(events.contains(&Event::PartyErased { party: "party-p1".to_string() }));
    let p1 = &desk.state.payees["p1"];
    assert!(p1.closed && p1.account.number.is_empty() && p1.account.name.is_empty());
    assert_eq!(desk.state.runs["r1"].items["i1"].account, None);
    assert!(desk.state.runs["r2"].items["i2"].account.is_some(), "the bank still has it");
    assert!(watch(&desk).is_empty(), "asked just now");

    desk.ok(
        &svc(),
        Command::RecordAcknowledgement {
            submission: s2.clone(),
            refused: None,
            rejected: Default::default(),
        },
    );
    desk.ok(
        &svc(),
        Command::RecordSettlement {
            submission: s2,
            settled_on: d(2027, 11, 15),
        },
    );
    desk.at(monday() + chrono::Duration::days(1));
    desk.ok(&svc(), checked("party-p2", false));
    assert_eq!(watch(&desk), ["party-p1"], "an unfinished forgetting is asked daily");
    assert!(desk.ok(&svc(), checked("party-p1", true)).is_empty(), "erased once");
    assert!(desk.state.erasure.unfinished.is_empty());
    assert_eq!(desk.state.runs["r2"].items["i2"].account, None);
    assert!(watch(&desk).is_empty());

    desk.at(monday() + chrono::Duration::days(1 + RECHECK_DAYS));
    assert_eq!(watch(&desk), ["party-p2"], "the rest are asked again after the recheck period");
    let by_own_clock: Vec<String> = serde_json::from_value(answer(&desk.state, json!({"q": "erasure_watch"})).unwrap()).unwrap();
    assert!(by_own_clock.is_empty(), "without a clock the module's own is used");
}

/// Purpose: an erased authority holder's authority is cancelled and
/// blanked, the AML records about them as owner, as a party payer or by
/// their account go, and no new payee or authority is taken for them; only
/// the service may say a party is erased, and it must name one.
/// Why it matters: payer cases and holds name the person too, and a new
/// record for someone the master has erased would bring their details back.
#[test]
fn an_erased_payer_is_forgotten_everywhere_and_not_taken_again() {
    let mut desk = Desk::new();
    desk.authority("a1", "pol-1", "02-0001-3333358-00", false);
    {
        let cases = &mut desk.state.payer_checks.cases;
        cases.insert("party-a1|party:x".to_string(), case("party-a1", "party:x"));
        cases.insert("own|party:party-a1".to_string(), case("own", "party:party-a1"));
        cases.insert("own|account:020001333335800".to_string(), case("own", "account:020001333335800"));
        cases.insert("own|party:y".to_string(), case("own", "party:y"));
    }
    assert_eq!(desk.code(&rae(), checked("party-a1", true)), "not_permitted");
    assert_eq!(desk.code(&svc(), checked("", true)), "invalid_party");

    desk.ok(&svc(), checked("party-a1", true));
    let a1 = &desk.state.authorities["a1"];
    assert_eq!(a1.status, AuthorityStatus::Cancelled);
    assert_eq!(a1.reason.as_deref(), Some(ERASED));
    assert!(a1.account.number.is_empty() && a1.account.name.is_empty());
    let left: Vec<&String> = desk.state.payer_checks.cases.keys().collect();
    assert_eq!(left, ["own|party:y"]);

    let payee = Command::RegisterPayee {
        payee: "p9".to_string(),
        party: "party-a1".to_string(),
        kind: PayeeKind::Customer,
        account: account("a1", "02-0001-4444445-00"),
        name_check: NameCheck::Match,
    };
    assert_eq!(desk.code(&pam(), payee), ERASED);
    let authority = Command::CreateAuthority {
        authority: "a9".to_string(),
        policy: policy("pol-1"),
        party: "party-a1".to_string(),
        life: None,
        account: account("a1", "02-0001-4444445-00"),
        sole_name: false,
    };
    assert_eq!(desk.code(&pam(), authority), ERASED);
}
