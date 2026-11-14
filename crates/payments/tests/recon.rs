//! Statement reconciliation, exceptions, the region drill and the queries.

mod common;

use common::*;
use ic_payments::model::{ExceptionKind, Instruction, PayeeKind, Probe, RunType, StatementLine};
use ic_payments::query::answer;
use ic_payments::{Command, Event};
use serde_json::json;

fn statement(account: &str, date: chrono::NaiveDate, opening: i64, closing: i64, lines: Vec<StatementLine>) -> Command {
    Command::RecordStatement {
        account: account.to_string(),
        date,
        opening_minor: opening,
        closing_minor: closing,
        lines,
    }
}

fn line(id: &str, amount: i64, reference: &str) -> StatementLine {
    StatementLine {
        id: id.to_string(),
        amount_minor: amount,
        reference: reference.to_string(),
        payer_account: Some("0601111111".to_string()),
    }
}

/// Purpose: the statement line for a settled file and the line for a
/// returned item each match what the module expected and leave nothing
/// outstanding; until then the reconciliation report shows them as not yet
/// on the statement, with their age.
/// Why it matters: every bank account reconciles every day. Money the
/// ledger says has moved must be seen to move at the bank, item for item.
#[test]
fn settled_and_returned_money_matches_the_statement() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Customer, "0101234567");
    desk.prepare("r1", RunType::BenefitPayments, d(2027, 11, 15));
    desk.add("r1", vec![pay("i1", "p1", 300_00), pay("i2", "p1", 500_00)]);
    let s = desk.approve("r1");
    desk.settle(&s, d(2027, 11, 15));
    desk.ok(
        &svc(),
        Command::RecordReturn {
            item: "i2".to_string(),
            reason: ic_payments::model::ReturnReason::InvalidAccount,
            returned_on: d(2027, 11, 16),
        },
    );
    let report = answer(&desk.state, json!({"q": "reconciliation", "account": "anz-sf1-claims", "today": "2027-11-18"})).unwrap();
    assert_eq!(
        report["not_yet_on_statement"],
        json!([{"reference": "i2", "amount_minor": 500_00, "age_days": 2}, {"reference": "r1-1", "amount_minor": -800_00, "age_days": 3}])
    );
    assert_eq!(report["available_minor"], json!(10_000_000_00i64 - 300_00));
    let events = desk.statement(
        "anz-sf1-claims",
        d(2027, 11, 16),
        10_000_000_00,
        &[("l1", -800_00, "r1-1"), ("l2", 500_00, "i2")],
    );
    assert_eq!(
        events,
        vec![Event::StatementReconciled {
            account: "anz-sf1-claims".to_string(),
            date: d(2027, 11, 16),
            matched: 2,
            exceptions: 0
        }]
    );
    let report = desk.state.reconciliation("anz-sf1-claims", d(2027, 11, 18)).unwrap();
    assert_eq!(report["not_yet_on_statement"], json!([]));
    assert_eq!(report["closing_minor"], json!(10_000_000_00i64 - 300_00));
    assert_eq!(report["statement_date"], json!("2027-11-16"));
    assert!(desk.state.exceptions.is_empty());
    assert!(desk.state.reconciliation("nowhere", d(2027, 11, 18)).is_none());
}

/// Purpose: a statement is refused when it does not add up, repeats a line
/// ID, is for an unknown account, or is not later than the last one read.
/// Why it matters: a corrupt or replayed statement would match or break
/// lines that do not exist. It is better not read at all.
#[test]
fn a_statement_that_cannot_be_trusted_is_refused() {
    let mut desk = Desk::new();
    let a = "anz-sf1-claims";
    let open = 10_000_000_00;
    assert_eq!(desk.code(&mia(), statement(a, d(2027, 11, 15), open, open, vec![])), "not_permitted");
    assert_eq!(desk.code(&svc(), statement("zz", d(2027, 11, 15), 0, 0, vec![])), "unknown_account");
    assert_eq!(desk.code(&svc(), statement(a, d(2027, 11, 12), open, open, vec![])), "statement_out_of_order");
    assert_eq!(
        desk.code(&svc(), statement(a, d(2027, 11, 15), open, open + 1, vec![])),
        "statement_does_not_add_up"
    );
    assert_eq!(
        desk.code(&svc(), statement(a, d(2027, 11, 15), i64::MAX, 0, vec![line("l1", 5, "x")])),
        "statement_does_not_add_up"
    );
    assert_eq!(
        desk.code(
            &svc(),
            statement(a, d(2027, 11, 15), open, open + 2, vec![line("l1", 1, "x"), line("l1", 1, "y")])
        ),
        "duplicate_line"
    );
    assert_eq!(
        desk.code(&svc(), statement(a, d(2027, 11, 15), open, open + 1, vec![line("", 1, "x")])),
        "duplicate_line"
    );
}

/// Purpose: a line nobody expected, a line for the wrong amount, and a
/// statement that does not open where the last closed each become an
/// exception; an exception is owned by a named person and resolved only by
/// its owner, once, with a statement of what was done.
/// Why it matters: an unexplained debit on a claims account is either an
/// error or a theft. It needs a name against it the day it appears.
#[test]
fn what_the_statement_cannot_explain_becomes_an_owned_exception() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Customer, "0101234567");
    desk.prepare("r1", RunType::BenefitPayments, d(2027, 11, 15));
    desk.add("r1", vec![pay("i1", "p1", 300_00)]);
    let s = desk.approve("r1");
    desk.settle(&s, d(2027, 11, 15));
    let a = "anz-sf1-claims";
    let lines = [("l1", -299_00, "r1-1"), ("l2", -45_00, "bank fee"), ("l3", 12_00, "interest")];
    let events = desk.statement(a, d(2027, 11, 15), 10_000_000_50, &lines);
    let kinds: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let Event::ExceptionOpened { kind, amount_minor, .. } = e {
                Some((*kind, *amount_minor))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            (ExceptionKind::BalanceBreak, 50),
            (ExceptionKind::AmountMismatch, 1_00),
            (ExceptionKind::UnexplainedDebit, -45_00),
            (ExceptionKind::UnexplainedCredit, 12_00)
        ]
    );
    let report = desk.state.reconciliation(a, d(2027, 11, 20)).unwrap();
    assert_eq!(report["open_exceptions"].as_array().unwrap().len(), 4);
    assert_eq!(report["open_exceptions"][0]["age_days"], json!(5));

    let id = format!("{a}-2027-11-15-l2");
    let assign = |owner: &str| Command::AssignException {
        exception: id.clone(),
        owner: owner.to_string(),
    };
    let resolve = |text: &str| Command::ResolveException {
        exception: id.clone(),
        resolution: text.to_string(),
    };
    assert_eq!(desk.code(&mia(), assign("rae")), "not_permitted");
    assert_eq!(desk.code(&rae(), assign("")), "invalid_owner");
    assert_eq!(
        desk.code(
            &rae(),
            Command::AssignException {
                exception: "zz".to_string(),
                owner: "rae".to_string()
            }
        ),
        "unknown_exception"
    );
    assert_eq!(desk.code(&rae(), resolve("fee agreed with bank")), "not_owner");
    desk.ok(&rae(), assign("rae"));
    assert_eq!(desk.code(&mia(), resolve("x")), "not_permitted");
    assert_eq!(desk.code(&rae(), resolve("  ")), "invalid_resolution");
    desk.ok(&rae(), resolve("bank fee, posted to expenses by finance"));
    assert_eq!(desk.code(&rae(), resolve("again")), "exception_resolved");
    assert_eq!(
        desk.state.reconciliation(a, d(2027, 11, 20)).unwrap()["open_exceptions"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    // Resolved and old, it is forgotten by a prune; the open ones are kept.
    desk.at(t(2028, 1, 10, 9, 0));
    desk.ok(&svc(), Command::Prune { before: d(2028, 1, 1) });
    assert_eq!(desk.state.exceptions.len(), 3);
}

/// Purpose: a credit on a collection account that matches nothing expected
/// is an unidentified receipt: it is handed to cash matching, resolved when
/// cash matching applies it to a policy, and left open for a person when
/// cash matching cannot say whose it is.
/// Why it matters: customers pay by direct credit with poor references.
/// The money must be held as unidentified and worked, never guessed onto a
/// policy and never lost.
#[test]
fn a_direct_credit_goes_to_cash_matching_and_stays_open_until_identified() {
    let mut desk = Desk::new();
    let a = "anz-sf1-collection";
    let open = 10_000_000_00;
    desk.ok(
        &svc(),
        statement(
            a,
            d(2027, 11, 15),
            open,
            open + 350_00,
            vec![line("l1", 150_00, "POL1 SMITH"), line("l2", 200_00, "thanks")],
        ),
    );
    let work: Vec<_> = desk.state.outbox.iter().map(|(k, e)| (*k, e.instruction.clone())).collect();
    assert_eq!(work.len(), 2);
    let Instruction::Match {
        exception,
        amount_minor,
        reference,
        payer_account,
        received_on,
        ..
    } = &work[0].1
    else {
        panic!("a match instruction")
    };
    assert_eq!(
        (amount_minor, reference.as_str(), payer_account.as_deref(), *received_on),
        (&150_00, "POL1 SMITH", Some("0601111111"), d(2027, 11, 15))
    );
    assert_eq!(desk.state.exceptions[exception].kind, ExceptionKind::UnidentifiedReceipt);
    let events = desk.ok(
        &svc(),
        Command::CompleteInstruction {
            entry: work[0].0,
            refused: None,
        },
    );
    assert!(matches!(&events[0], Event::ExceptionResolved { resolution, .. } if resolution == "applied by cash matching"));
    desk.ok(
        &svc(),
        Command::CompleteInstruction {
            entry: work[1].0,
            refused: Some("no_match".to_string()),
        },
    );
    let open: Vec<_> = desk.state.exceptions.values().filter(|e| e.resolution.is_none()).collect();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].reference, "thanks");
    assert!(desk.state.outbox.is_empty());
}

/// Purpose: when the core refuses to post a settlement the bank has already
/// made, an exception is opened naming the instruction and the refusal.
/// Why it matters: at that moment the bank and the ledger disagree about
/// real money. Nothing may quietly drop the instruction.
#[test]
fn a_settlement_the_core_refuses_becomes_an_exception() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Beneficiary, "0101234567");
    desk.prepare("r1", RunType::LumpSumClaims, d(2027, 11, 15));
    let mut item = pay("i1", "p1", 300_00);
    item.policy = Some(policy("pol-1"));
    item.payable = Some(ic_core::PayableKind::Benefit);
    desk.add("r1", vec![item]);
    let s = desk.approve("r1");
    desk.settle(&s, d(2027, 11, 15));
    let events = desk.ok(
        &svc(),
        Command::CompleteInstruction {
            entry: 0,
            refused: Some("insufficient_payable".to_string()),
        },
    );
    assert!(matches!(
        &events[0],
        Event::ExceptionOpened {
            kind: ExceptionKind::CoreRefused,
            ..
        }
    ));
    assert_eq!(desk.state.exceptions["core-i1-0"].reference, "settle-i1: insufficient_payable");
}

fn probe(ok: bool) -> Probe {
    Probe {
        ok,
        evidence_hash: "feed".to_string(),
        detail: "low-value submission".to_string(),
    }
}

/// Purpose: a region is ready only when its endpoints are current and a
/// drill from it has passed recently; a drill passes only if every channel
/// was reached from that region, and says which were not.
/// Why it matters: the addendum requires the evacuation drill to submit a
/// real low-value file and API payment from each secondary region and to
/// fail if it cannot. This is where that result is held and where the
/// fail-over controller reads whether a region can take the leaders.
#[test]
fn a_region_is_ready_only_after_a_drill_reaches_every_channel() {
    let mut desk = Desk::new();
    let ready = |desk: &Desk, today: &str| {
        let all = answer(&desk.state, json!({"q": "readiness", "today": today})).unwrap();
        all.as_array()
            .unwrap()
            .iter()
            .map(|r| (r["region"].as_str().unwrap().to_string(), r["ready"].as_bool().unwrap()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        ready(&desk, "2027-11-15"),
        vec![("akl".to_string(), false), ("mel".to_string(), false), ("syd".to_string(), false)]
    );
    let start = |id: &str, region: &str| Command::StartDrill {
        drill: id.to_string(),
        region: region.to_string(),
    };
    let record = |id: &str, channel: &str, p: Probe| Command::RecordDrillProbe {
        drill: id.to_string(),
        channel: channel.to_string(),
        probe: p,
    };
    assert_eq!(desk.code(&mia(), start("d1", "syd")), "not_permitted");
    assert_eq!(desk.code(&svc(), start("d1", "lax")), "unknown_region");
    desk.ok(&svc(), start("d1", "syd"));
    assert_eq!(desk.code(&svc(), start("d1", "syd")), "duplicate_drill");
    assert_eq!(desk.code(&svc(), record("d1", "nowhere", probe(true))), "unknown_channel");
    assert_eq!(desk.code(&svc(), record("zz", "anz-file", probe(true))), "unknown_drill");
    let mut bare = probe(true);
    bare.evidence_hash = String::new();
    assert_eq!(desk.code(&svc(), record("d1", "anz-file", bare)), "no_evidence");
    assert_eq!(desk.code(&mia(), record("d1", "anz-file", probe(true))), "not_permitted");
    desk.ok(&svc(), record("d1", "anz-file", probe(true)));
    desk.ok(&svc(), record("d1", "anz-api", probe(false)));
    assert_eq!(desk.code(&mia(), Command::CloseDrill { drill: "d1".to_string() }), "not_permitted");
    let events = desk.ok(&svc(), Command::CloseDrill { drill: "d1".to_string() });
    assert_eq!(
        events,
        vec![Event::DrillClosed {
            drill: "d1".to_string(),
            region: "syd".to_string(),
            passed: false,
            unreached: vec!["anz-api".to_string(), "bnz-file".to_string()]
        }]
    );
    assert_eq!(desk.code(&svc(), Command::CloseDrill { drill: "d1".to_string() }), "drill_closed");
    assert_eq!(ready(&desk, "2027-11-15")[2], ("syd".to_string(), false));

    desk.ok(&svc(), start("d2", "syd"));
    for channel in ["anz-file", "anz-api", "bnz-file"] {
        desk.ok(&svc(), record("d2", channel, probe(true)));
    }
    desk.ok(&svc(), Command::CloseDrill { drill: "d2".to_string() });
    assert_eq!(ready(&desk, "2027-11-15")[2], ("syd".to_string(), true));
    // Ninety days on the drill is stale, and later still the certificate has lapsed.
    assert_eq!(ready(&desk, "2028-02-13")[2], ("syd".to_string(), true));
    assert_eq!(ready(&desk, "2028-02-14")[2], ("syd".to_string(), false));
    let late = desk.state.readiness(d(2028, 7, 1));
    assert!(late[2].problems.iter().any(|p| p.contains("certificate in syd has expired")));
    assert_eq!(late[2].last_passed, Some(d(2027, 11, 15)));
    // Old closed drills are pruned; readiness then asks for a new one.
    desk.at(t(2028, 3, 1, 9, 0));
    desk.ok(&svc(), Command::Prune { before: d(2028, 3, 1) });
    assert!(desk.state.drills.is_empty());
    assert!(ic_payments::PaymentsState::default().readiness(d(2028, 1, 1)).is_empty());
}

/// Purpose: each question the host can ask is answered from state, and a
/// question that is malformed or unknown is an error, not a guess.
/// Why it matters: drivers, the fail-back controller and operations screens
/// all read the module through this one function.
#[test]
fn questions_are_answered_from_state() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Customer, "0101234567");
    desk.authority("a1", "pol-1", "0101234567", false);
    desk.prepare("r1", RunType::BenefitPayments, d(2027, 11, 15));
    desk.add("r1", vec![pay("i1", "p1", 300_00)]);
    let ask = |desk: &Desk, q: serde_json::Value| answer(&desk.state, q);
    assert_eq!(ask(&desk, json!({"q": "run", "run": "r1"})).unwrap()["status"], json!("prepared"));
    assert_eq!(ask(&desk, json!({"q": "run", "run": "zz"})).unwrap(), json!(null));
    assert_eq!(ask(&desk, json!({"q": "control_totals", "run": "r1"})).unwrap()["unsent"], json!(300_00));
    assert_eq!(ask(&desk, json!({"q": "control_totals", "run": "zz"})).unwrap(), json!(null));
    assert_eq!(ask(&desk, json!({"q": "payee", "payee": "p1"})).unwrap()["kind"], json!("customer"));
    assert_eq!(ask(&desk, json!({"q": "authority", "authority": "a1"})).unwrap()["status"], json!("active"));
    assert_eq!(ask(&desk, json!({"q": "outbox"})).unwrap(), json!({}));
    assert_eq!(ask(&desk, json!({"q": "config"})).unwrap()["rules"]["cooling_off_hours"], json!(24));
    assert_eq!(ask(&desk, json!({"q": "batch", "submission": "r1-1"})).unwrap(), json!(null));
    let s = desk.approve("r1");
    assert_eq!(ask(&desk, json!({"q": "submission", "submission": s})).unwrap()["status"], json!("queued"));
    assert_eq!(
        ask(&desk, json!({"q": "batch", "submission": s})).unwrap()["items"][0]["number"],
        json!("0101234567")
    );
    let trail = ask(&desk, json!({"q": "trail"})).unwrap();
    assert_eq!(trail["events"], json!(desk.state.event_seq));
    assert_eq!(trail["chain"].as_str().unwrap().len(), 64);
    assert!(ask(&desk, json!({"q": "run"})).unwrap_err().contains("needs run"));
    assert!(ask(&desk, json!({})).unwrap_err().contains("needs q"));
    assert!(ask(&desk, json!({"q": "weather"})).unwrap_err().contains("unknown question"));
    assert!(ask(&desk, json!({"q": "readiness", "today": "soon"})).unwrap_err().contains("date"));
}

/// Purpose: the module's state survives being saved and loaded, and two
/// replicas that apply the same commands hold the same state and the same
/// event chain.
/// Why it matters: the module is replicated. A field that does not survive
/// a snapshot, or an order that differs between replicas, is a divergence
/// that shows up as money handled twice after a failover.
#[test]
fn state_survives_a_snapshot_and_replicas_agree() {
    let build = || {
        let mut desk = Desk::new();
        desk.payee("p1", PayeeKind::Estate, "0101234567");
        desk.authority("a1", "pol-1", "0207654321", true);
        desk.prepare("r1", RunType::BenefitPayments, desk.now.date());
        desk.add("r1", vec![pay("i1", "p1", 300_00)]);
        let s = desk.approve("r1");
        desk.settle(&s, desk.now.date());
        desk.ok(
            &svc(),
            Command::StartDrill {
                drill: "d1".to_string(),
                region: "mel".to_string(),
            },
        );
        desk.ok(
            &svc(),
            Command::PayerDeceased {
                policy: policy("pol-1"),
                claim: "clm-1".to_string(),
            },
        );
        desk.statement("anz-sf1-collection", d(2027, 11, 16), 10_000_000_00, &[("l1", 5_00, "who")]);
        desk.state
    };
    let (one, two) = (build(), build());
    assert_eq!(one, two);
    assert_eq!(one.event_chain, two.event_chain);
    let saved = serde_json::to_string(&one).unwrap();
    let loaded: ic_payments::PaymentsState = serde_json::from_str(&saved).unwrap();
    assert_eq!(loaded, one);
}

/// Purpose: the core's notice that a payer has died becomes the module's
/// own command, keyed by the event so that it is applied once however often
/// the event is offered; other events are ignored.
/// Why it matters: this is the wire between the death claim in the core and
/// the direct debit that must stop. If it drops the event, collection from
/// a dead customer's account carries on.
#[test]
fn the_cores_death_notice_stops_collection() {
    use ic_log::StateMachine;
    let record = |event: serde_json::Value| -> ic_core::EventRecord {
        serde_json::from_value(json!({"id": "3-77", "partition": 3, "seq": 77, "business_date": "2027-11-16", "log_index": 900, "event": event})).unwrap()
    };
    let death = record(json!({"payment_authority_cancelled": {"policy": "pol-1", "claim": "clm-1"}}));
    let mut desk = Desk::new();
    desk.authority("a1", "pol-1", "0101234567", true);
    let commands = ic_payments::query::react(&death);
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].key, "core-3-77");
    for (index, envelope) in commands.iter().chain(&ic_payments::query::react(&death)).enumerate() {
        desk.state.apply(index as u64 + 1000, envelope.clone()).unwrap();
    }
    assert!(desk.state.blocked_accounts.contains("0101234567"));
    assert_eq!(desk.state.clock, Some(monday()), "an event does not move the clock");
    let other = record(json!({"payment_returned": {"policy": "pol-1", "payment": "i1", "payable": "benefit", "amount_minor": 5, "bank_account": "a"}}));
    assert!(ic_payments::query::react(&other).is_empty());
}

/// Purpose: the financial close's two questions. Cash is settled as at a
/// day only when no reconciliation break opened by then is still open and
/// no run valued by then is still waiting for the bank; unidentified
/// receipts are listed oldest first with their total, and show whether
/// anyone owns them.
/// Why it matters: finance signs the month off on these answers. A green
/// that ignored a file the bank has not confirmed, or an aged list that hid
/// an unowned receipt, would close the books on cash nobody can vouch for.
#[test]
fn the_financial_close_is_told_what_cash_is_outstanding() {
    let mut desk = Desk::new();
    let ask = |desk: &Desk, q: &str, as_at: &str| answer(&desk.state, json!({"q": q, "as_at": as_at})).unwrap();
    assert_eq!(ask(&desk, "fund_cash_settlement", "2027-11-30")["green"], json!(true));
    assert_eq!(ask(&desk, "aged_unidentified_receipts", "2027-11-30")["count"], json!(0));
    assert!(answer(&desk.state, json!({"q": "fund_cash_settlement"})).is_err());
    assert!(answer(&desk.state, json!({"q": "aged_unidentified_receipts", "as_at": "month end"})).is_err());

    // Two credits nobody recognises, on different days.
    desk.statement("anz-sf1-collection", d(2027, 11, 15), 10_000_000_00, &[("l1", 5_00, "who")]);
    desk.at(t(2027, 11, 16, 9, 0));
    desk.statement("anz-sf1-collection", d(2027, 11, 16), 10_000_005_00, &[("l2", 7_00, "what")]);
    let aged = ask(&desk, "aged_unidentified_receipts", "2027-11-30");
    assert_eq!((aged["count"].clone(), aged["total_minor"].clone()), (json!(2), json!(12_00)));
    assert_eq!(aged["items"][0]["amount_minor"], json!(5_00), "oldest first");
    assert_eq!(aged["items"][0]["action"], json!(null), "nobody owns it yet");
    // As at the first day only the first had arrived.
    assert_eq!(ask(&desk, "aged_unidentified_receipts", "2027-11-15")["count"], json!(1));
    let first = aged["items"][0]["party"].as_str().unwrap().to_string();
    desk.ok(
        &rae(),
        Command::AssignException {
            exception: first.clone(),
            owner: "rae".to_string(),
        },
    );
    assert_eq!(
        ask(&desk, "aged_unidentified_receipts", "2027-11-30")["items"][0]["action"],
        json!("assigned to rae")
    );

    let check = ask(&desk, "fund_cash_settlement", "2027-11-30");
    assert_eq!((check["green"].clone(), check["count"].clone()), (json!(false), json!(2)));
    assert_eq!(check["items"][0]["fund"], json!("sf1"));

    // A run the bank has not settled is outstanding from its value date.
    desk.payee("p1", PayeeKind::Customer, "0101234567");
    desk.at(t(2027, 11, 17, 9, 0));
    desk.prepare("r1", RunType::BenefitPayments, d(2027, 11, 18));
    desk.add("r1", vec![pay("i1", "p1", 100_00)]);
    let submission = desk.approve("r1");
    assert_eq!(ask(&desk, "fund_cash_settlement", "2027-11-17")["count"], json!(2));
    let check = ask(&desk, "fund_cash_settlement", "2027-11-30");
    assert_eq!(check["count"], json!(3));
    assert!(check["items"].as_array().unwrap().iter().any(|i| i["submission"] == json!(submission)));
    desk.at(t(2027, 11, 18, 9, 0));
    desk.settle(&submission, d(2027, 11, 18));
    assert_eq!(ask(&desk, "fund_cash_settlement", "2027-11-30")["count"], json!(2));
}

/// Purpose: the place a driver has read a bank's messages to is kept per
/// channel and only moves forward; an older place from a slower driver
/// changes nothing; a channel that does not exist and a caller that is not
/// a driver are refused.
/// Why it matters: drivers run in every region at once. If the slower one
/// could move the place back, every restart would re-read what the faster
/// one finished; if a person could move it forward, the bank's answers in
/// between would never be read at all.
#[test]
fn the_place_in_a_banks_messages_only_moves_forward() {
    let mut desk = Desk::new();
    let note = |through| Command::NoteMessagesRead {
        channel: "anz-file".to_string(),
        through,
    };
    assert_eq!(answer(&desk.state, json!({"q": "messages_read"})).unwrap(), json!({}));
    desk.ok(&svc(), note(7));
    desk.ok(&svc(), note(4));
    assert_eq!(answer(&desk.state, json!({"q": "messages_read"})).unwrap(), json!({"anz-file": 7}));
    desk.ok(&svc(), note(9));
    assert_eq!(desk.state.messages_read["anz-file"], 9);
    assert_eq!(desk.code(&mia(), note(99)), "not_permitted");
    let elsewhere = Command::NoteMessagesRead {
        channel: "no-such".to_string(),
        through: 1,
    };
    assert_eq!(desk.code(&svc(), elsewhere), "unknown_channel");
    assert_eq!(desk.state.messages_read["anz-file"], 9);
}

/// Purpose: a credit quoting a reinsurance statement (`ri-stmt-<n>`, in any
/// case, anywhere in the reference) is sent to the reinsurance module as a
/// settlement of that statement, in the statement's sign, and is resolved
/// when that module records it; if that module refuses, the money stays
/// unidentified for a person; a reference that only looks like one, or a
/// debit, is not sent.
/// Why it matters: money a reinsurer owes arrives without anyone in
/// payments knowing to expect it. Matched here, the reinsurance account is
/// cleared the day the money lands instead of when someone notices it in
/// the unidentified list.
#[test]
fn money_a_reinsurer_pays_is_matched_to_its_statement() {
    let mut desk = Desk::new();
    let a = "anz-sf1-operating";
    let open = 10_000_000_00;
    desk.ok(
        &svc(),
        statement(
            a,
            d(2027, 11, 15),
            open,
            open + 1_200_00 + 50_00 + 7_00 - 3_00,
            vec![
                line("l1", 1_200_00, "Swiss Re RI-STMT-42 Q3"),
                line("l2", 50_00, "ri-stmt-7-2"),
                line("l3", 7_00, "ri-stmt-x"),
                line("l4", -3_00, "ri-stmt-9"),
            ],
        ),
    );
    let sent: Vec<(u64, serde_json::Value)> = desk
        .state
        .outbox
        .iter()
        .filter_map(|(k, e)| match &e.instruction {
            Instruction::Module { module, command } if module == "reinsurance" => Some((*k, command.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(sent.len(), 2);
    assert_eq!(
        sent[0].1,
        json!({"key": "payments:received:anz-sf1-operating-2027-11-15-l1", "caller": "payments", "on": "2027-11-15",
               "command": {"record_settlement": {"number": 42, "payment_ref": "Swiss Re RI-STMT-42 Q3", "amount_minor": -1_200_00, "bank_account": "anz-sf1-operating"}}})
    );
    assert_eq!(sent[1].1["command"]["record_settlement"]["number"], 7);
    let unidentified = |desk: &Desk| {
        let mut open: Vec<_> = desk
            .state
            .exceptions
            .values()
            .filter(|e| e.resolution.is_none() && e.kind == ExceptionKind::UnidentifiedReceipt)
            .map(|e| e.reference.clone())
            .collect();
        open.sort();
        open
    };
    assert_eq!(unidentified(&desk), vec!["Swiss Re RI-STMT-42 Q3", "ri-stmt-7-2"]);
    let events = desk.ok(
        &svc(),
        Command::CompleteInstruction {
            entry: sent[0].0,
            refused: None,
        },
    );
    assert!(matches!(&events[0], Event::ExceptionResolved { resolution, .. } if resolution == "recorded by reinsurance against its statement"));
    desk.ok(
        &svc(),
        Command::CompleteInstruction {
            entry: sent[1].0,
            refused: Some("not_instructed".to_string()),
        },
    );
    assert_eq!(unidentified(&desk), vec!["ri-stmt-7-2"], "a refused one stays for a person");
}

/// The payer's policy carried on the match instruction for the first
/// unidentified line in the outbox.
fn payer_policy_of_first_match(desk: &Desk) -> Option<ic_core::PolicyId> {
    desk.state.outbox.values().find_map(|e| match &e.instruction {
        Instruction::Match { payer_policy, .. } => Some(payer_policy.clone()),
        _ => None,
    })?
}

/// Purpose: a direct credit from an account that exactly one policy's
/// authority draws on is handed to cash matching naming that policy, written
/// however the bank spaces the account number; when two policies' authorities
/// draw on the account, or none does, no policy is named.
/// Why it matters: customers who pay by direct debit often top up by direct
/// credit with no usable reference. The account they pay from is the best
/// evidence of whose money it is, but only when it points at one policy;
/// guessing between two would put money on the wrong policy.
#[test]
fn a_direct_credit_from_an_authoritys_account_names_its_policy() {
    let mut desk = Desk::new();
    let a = "anz-sf1-collection";
    let open = 10_000_000_00;
    desk.authority("auth-1", "POL7", "0601111111", true);
    let spaced = StatementLine {
        payer_account: Some("06-0111-1111".to_string()),
        ..line("l1", 150_00, "top up")
    };
    desk.ok(&svc(), statement(a, d(2027, 11, 15), open, open + 150_00, vec![spaced]));
    assert_eq!(payer_policy_of_first_match(&desk), Some(policy("POL7")));

    let mut desk = Desk::new();
    desk.authority("auth-1", "POL7", "0601111111", true);
    desk.authority("auth-2", "POL8", "0601111111", true);
    desk.ok(&svc(), statement(a, d(2027, 11, 15), open, open + 150_00, vec![line("l1", 150_00, "top up")]));
    assert_eq!(payer_policy_of_first_match(&desk), None);

    let mut desk = Desk::new();
    desk.authority("auth-1", "POL7", "0209999999", true);
    desk.ok(&svc(), statement(a, d(2027, 11, 15), open, open + 150_00, vec![line("l1", 150_00, "top up")]));
    assert_eq!(payer_policy_of_first_match(&desk), None);
}

/// Purpose: a drill's token payment that the bank took is expected on the
/// statements: leaving the channel's operating account and, where the drill
/// account is one of the insurer's own, arriving there, so both lines
/// reconcile with no exception; a probe that failed expects nothing.
/// Why it matters: a weekly drill through every channel from every region
/// would otherwise leave reconcilers several exceptions a week for money
/// that only moved between the insurer's own accounts.
#[test]
fn a_drills_token_payment_reconciles_itself() {
    let mut desk = Desk::new();
    let mut c = config();
    c.accounts.get_mut("anz-sf1-claims").unwrap().at_bank = Some(ic_payments::iso20022::OrderingAccount {
        name: "Insurer Operating".into(),
        number: "01-0000-0001".into(),
        bic: None,
    });
    desk.ok(
        &cal(),
        Command::ActivateConfig {
            version: 2,
            config: Box::new(c),
        },
    );
    desk.ok(
        &svc(),
        Command::StartDrill {
            drill: "d1".into(),
            region: "syd".into(),
        },
    );
    for (channel, ok) in [("anz-file", true), ("bnz-file", false)] {
        desk.ok(
            &svc(),
            Command::RecordDrillProbe {
                drill: "d1".into(),
                channel: channel.into(),
                probe: probe(ok),
            },
        );
    }
    let opening = |desk: &Desk, a: &str| desk.state.books.get(a).map_or(0, |b| b.closing_minor);
    let at = opening(&desk, "anz-sf1-operating");
    let out = desk.statement("anz-sf1-operating", d(2027, 11, 15), at, &[("l1", -1_00, "drill-d1-anz-file")]);
    let at = opening(&desk, "anz-sf1-claims");
    let back = desk.statement("anz-sf1-claims", d(2027, 11, 15), at, &[("l1", 1_00, "drill-d1-anz-file")]);
    for events in [out, back] {
        assert!(
            matches!(&events[..], [Event::StatementReconciled { matched: 1, exceptions: 0, .. }]),
            "{events:?}"
        );
    }
    assert!(desk.state.books.values().all(|b| !b.expected.keys().any(|k| k.contains("bnz-file"))));
}
