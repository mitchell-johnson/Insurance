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
    let s = desk.approve("r1");
    assert_eq!(ask(&desk, json!({"q": "submission", "submission": s})).unwrap()["status"], json!("queued"));
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
