//! The forwarder that carries the commission module's facts to the channels.

mod common;

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use common::*;
use ic_channels::forward::{commands_for, pass, Source, PUBLISHER, STRUCTURE};
use ic_channels::http::{Channel, Local};
use ic_channels::query::Question;
use ic_channels::state::Command;
use ic_channels::workbench::{PartyStatus, WorkbenchCommand};
use ic_distribution::rules::HeldReason;
use ic_distribution::structure::{CeasedReason, LicenceRecord, LicenceState};
use ic_distribution::workbench::{DocumentRecord, HeldLine, Position, Summary};
use ic_distribution::{BookId, DistributionState};
use serde_json::{json, Value};

/// A commission module that answers from a script: question (as JSON text) to answer.
#[derive(Default)]
struct Scripted {
    answers: Mutex<BTreeMap<String, Result<Value, String>>>,
    asked: Mutex<Vec<String>>,
}

impl Scripted {
    fn set(&self, question: Value, answer: Result<Value, String>) {
        self.answers.lock().unwrap().insert(question.to_string(), answer);
    }
}

impl Source for Scripted {
    fn ask(&self, question: Value) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send + '_>> {
        let key = question.to_string();
        self.asked.lock().unwrap().push(key.clone());
        let answer = self
            .answers
            .lock()
            .unwrap()
            .get(&key)
            .cloned()
            .unwrap_or_else(|| Err(format!("unscripted: {key}")));
        Box::pin(async move { answer })
    }
}

fn summary(receivable: i64, advance: i64) -> Summary {
    Summary {
        held_minor: 0,
        commission_minor: 0,
        receivable_minor: receivable,
        retention_minor: 0,
        advance_minor: advance,
        clearing_minor: 0,
        net_minor: 0,
    }
}

/// A position as the commission module itself builds and serialises it.
fn position(party: &str, held: Vec<HeldLine>, licences: Vec<LicenceRecord>, ceased: Option<(chrono::NaiveDate, CeasedReason)>, summary: Summary) -> Value {
    serde_json::to_value(Position {
        party: p(party),
        summary,
        held,
        clawback_at_risk_minor: 0,
        instructions: vec![],
        payment_hold: None,
        documents: vec![DocumentRecord {
            kind: "regulator_notice".into(),
            document_ref: "vault-1".into(),
            on: d(2027, 11, 20),
        }],
        licence_history: licences,
        ceased,
        open_cases: vec!["case-9".into()],
        notes: vec![],
        actions_in_progress: vec![],
    })
    .unwrap()
}

fn held(period: &str, book: Option<&str>, reason: Option<HeldReason>, amount: i64) -> HeldLine {
    HeldLine {
        period: period.into(),
        book: book.map(|b| BookId(b.into())),
        reason,
        amount_minor: amount,
        entries: vec![],
    }
}

fn licence(state: LicenceState, effective: chrono::NaiveDate) -> LicenceRecord {
    LicenceRecord {
        state,
        effective,
        source_ref: format!("notice-{effective}"),
    }
}

fn engagements(rows: Value) -> Value {
    json!({ "as_at": "2027-12-01", "engagements": rows })
}

/// Purpose: every question the forwarder asks is one the commission module's own code understands, and every answer that code gives,
/// from an empty state and from its own types filled in, is one the forwarder can read.
/// Why it matters: the two modules share no types on purpose, so nothing but this test stops one side renaming a field and the
/// adviser portal quietly going stale.
#[test]
fn the_questions_and_answers_match_the_commission_modules_own() {
    let theirs = DistributionState::default();
    let today = d(2027, 12, 1);
    for question in [
        json!({ "changed_parties": { "since": 0 } }),
        json!("status"),
        json!({ "position": { "party": "adv-1", "as_at": today } }),
        json!({ "engagements": { "adviser": "adv-1", "as_at": today } }),
    ] {
        ic_distribution::host::answer(&theirs, question.clone()).unwrap_or_else(|e| panic!("{question}: {e}"));
    }
    let changed = ic_distribution::host::answer(&theirs, json!({ "changed_parties": { "since": 0 } })).unwrap();
    assert_eq!((changed["seq"].as_u64(), changed["parties"].as_array().map(Vec::len)), (Some(0), Some(0)));
    assert!(ic_distribution::host::answer(&theirs, json!("status")).unwrap().get("today").is_some());

    // An empty position and an empty engagement list make one command: an empty publication.
    let empty = ic_distribution::host::answer(&theirs, json!({ "position": { "party": "adv-1", "as_at": today } })).unwrap();
    let none = ic_distribution::host::answer(&theirs, json!({ "engagements": { "adviser": "adv-1", "as_at": today } })).unwrap();
    let commands = commands_for(&p("adv-1"), empty, none, today, "NZD").unwrap();
    assert!(matches!(&commands[..], [(PUBLISHER, Command::Workbench(WorkbenchCommand::Publish { items, .. }))] if items.is_empty()));

    // Every state the commission module can report reads as the workbench status it means.
    for (state, expected) in [
        (LicenceState::Active, PartyStatus::Normal),
        (LicenceState::Suspended, PartyStatus::LicenceSuspended),
        (LicenceState::Revoked, PartyStatus::LicenceRevoked),
        (LicenceState::Lapsed, PartyStatus::LicenceRevoked),
    ] {
        // A change dated in the future is recorded for the directory but is not yet the party's status.
        let history = vec![licence(state, d(2027, 11, 1)), licence(LicenceState::Revoked, d(2028, 1, 1))];
        let commands = commands_for(
            &p("x"),
            position("x", vec![], history, None, summary(0, 0)),
            engagements(json!([])),
            today,
            "NZD",
        )
        .unwrap();
        let status: Vec<PartyStatus> = commands
            .iter()
            .filter_map(|(_, c)| match c {
                Command::Workbench(WorkbenchCommand::RecordStatus { status, .. }) => Some(*status),
                _ => None,
            })
            .collect();
        assert_eq!(status, [expected]);
        assert_eq!(commands.iter().filter(|(m, _)| *m == STRUCTURE).count(), 3);
    }
    for (reason, expected) in [
        (CeasedReason::Deceased, PartyStatus::Deceased),
        (CeasedReason::Liquidation, PartyStatus::InLiquidation),
        (CeasedReason::WoundUp, PartyStatus::RunOff),
        (CeasedReason::Retired, PartyStatus::RunOff),
    ] {
        let ceased = Some((d(2027, 11, 5), reason));
        let history = vec![licence(LicenceState::Active, d(2027, 1, 1))];
        let commands = commands_for(
            &p("x"),
            position("x", vec![], history, ceased, summary(0, 0)),
            engagements(json!([])),
            today,
            "NZD",
        )
        .unwrap();
        assert!(commands
            .iter()
            .any(|(_, c)| matches!(c, Command::Workbench(WorkbenchCommand::RecordStatus { status, .. }) if *status == expected)));
    }
    for reason in [
        HeldReason::SuspendedLicence,
        HeldReason::Dispute,
        HeldReason::MissingPaymentDetails,
        HeldReason::LegalHold,
    ] {
        let line = held("2027-11", None, Some(reason), 1);
        commands_for(
            &p("x"),
            position("x", vec![line], vec![], None, summary(0, 0)),
            engagements(json!([])),
            today,
            "NZD",
        )
        .unwrap();
    }
    assert!(commands_for(&p("x"), json!({ "held": 3 }), engagements(json!([])), today, "NZD")
        .unwrap_err()
        .contains("position"));
    assert!(
        commands_for(&p("x"), position("x", vec![], vec![], None, summary(0, 0)), json!({}), today, "NZD")
            .unwrap_err()
            .contains("engagements")
    );
}

/// Purpose: one pass carries an adviser's engagement, their provider's suspension and their held commission into the channels, so the
/// adviser's book goes read-only, their own held balance shows with its reason, and the workbench has the position; a second pass
/// from the start changes nothing, and a failed pass leaves the cursor where it was.
/// Why it matters: this is addendum entry 4 end to end. Without the forwarder the commission module knows a licence is suspended and
/// the portal goes on letting the adviser transact.
#[tokio::test]
async fn a_pass_carries_engagements_licences_and_held_money() {
    let mut w = World::new();
    w.dir(
        module("identity"),
        ic_channels::directory::DirectoryCommand::BindSubject {
            subject: "sub-adv".into(),
            party: p("adv-1"),
        },
    )
    .unwrap();
    let at = w.at;
    let local = Arc::new(Local::new(w.ch));
    let today = "2027-12-01";
    let source = Scripted::default();
    source.set(
        json!({ "changed_parties": { "since": 0 } }),
        Ok(json!({ "seq": 7, "parties": ["fap-1", "adv-1"] })),
    );
    source.set(json!("status"), Ok(json!({ "today": today, "seq": 7 })));
    source.set(
        json!({ "position": { "party": "fap-1", "as_at": today } }),
        Ok(position(
            "fap-1",
            vec![],
            vec![licence(LicenceState::Suspended, d(2027, 11, 10))],
            None,
            summary(0, 0),
        )),
    );
    source.set(json!({ "engagements": { "adviser": "fap-1", "as_at": today } }), Ok(engagements(json!([]))));
    let lines = vec![
        held("2027-11", Some("book-a"), Some(HeldReason::SuspendedLicence), 900_00),
        held("2027-10", None, None, 50_00),
        held("2027-09", None, Some(HeldReason::Dispute), 0),
    ];
    source.set(
        json!({ "position": { "party": "adv-1", "as_at": today } }),
        Ok(position("adv-1", lines, vec![], None, summary(-300_00, 120_00))),
    );
    source.set(
        json!({ "engagements": { "adviser": "adv-1", "as_at": today } }),
        Ok(engagements(
            json!([{ "id": "e1", "adviser": "adv-1", "with": "fap-1", "from": "2025-01-01", "to": null, "accreditations": [], "impairment": null }]),
        )),
    );

    assert_eq!(pass(&source, local.as_ref(), 0, at, "NZD").await, Ok(7));
    let book = local.ask(Question::Book { adviser: p("adv-1"), at }).await.unwrap();
    assert_eq!((&book["read_only"], &book["engagements"][0]["fap"]), (&json!(true), &json!("fap-1")));
    assert_eq!(book["engagements"][0]["standing"]["state"], "read_only");
    let own = local.ask(Question::HeldBalance { party: p("adv-1") }).await.unwrap();
    assert_eq!(own["held"]["NZD"], 950_00);
    let reasons: Vec<&str> = own["entries"].as_array().unwrap().iter().map(|e| e["reason"].as_str().unwrap()).collect();
    assert_eq!(reasons.len(), 2);
    assert!(reasons.contains(&"suspended_licence") && reasons.contains(&"legal_hold"));
    local.with(|s| {
        let position = &s.data.workbench.positions[&p("adv-1")];
        assert_eq!(position.held()["NZD"], 950_00);
        // What the party owes is carried as a debt, whichever sign the commission module gave it.
        assert_eq!(position.net()["NZD"], 950_00 - 300_00 - 120_00);
        assert_eq!((position.documents.len(), position.cases.len()), (1, 1));
        assert_eq!(s.data.workbench.positions[&p("fap-1")].current_status(), PartyStatus::LicenceSuspended);
    });

    // After a restart the same facts are sent again and nothing doubles.
    let before = local.with(|s| serde_json::to_value(&s.data.workbench).unwrap());
    source.set(
        json!({ "changed_parties": { "since": 0 } }),
        Ok(json!({ "seq": 9, "parties": ["fap-1", "adv-1"] })),
    );
    assert_eq!(pass(&source, local.as_ref(), 0, at + 60, "NZD").await, Ok(9));
    assert_eq!(local.with(|s| serde_json::to_value(&s.data.workbench).unwrap()), before);
    assert_eq!(local.with(|s| s.data.directory.licences[&p("fap-1")].len()), 1);

    // Nothing changed: one question, and the cursor follows the module.
    source.set(json!({ "changed_parties": { "since": 9 } }), Ok(json!({ "seq": 12, "parties": [] })));
    source.asked.lock().unwrap().clear();
    assert_eq!(pass(&source, local.as_ref(), 9, at, "NZD").await, Ok(12));
    assert_eq!(source.asked.lock().unwrap().len(), 1);

    // The licence is restored: service resumes.
    source.set(json!({ "changed_parties": { "since": 12 } }), Ok(json!({ "seq": 13, "parties": ["fap-1"] })));
    let history = vec![
        licence(LicenceState::Suspended, d(2027, 11, 10)),
        licence(LicenceState::Active, d(2027, 11, 14)),
    ];
    source.set(
        json!({ "position": { "party": "fap-1", "as_at": today } }),
        Ok(position("fap-1", vec![], history, None, summary(0, 0))),
    );
    assert_eq!(pass(&source, local.as_ref(), 12, at + 120, "NZD").await, Ok(13));
    let book = local
        .ask(Question::Book {
            adviser: p("adv-1"),
            at: at + 120,
        })
        .await
        .unwrap();
    assert_eq!(book["read_only"], false);

    // A pass that cannot finish reports why and moves nothing.
    source.set(json!({ "changed_parties": { "since": 13 } }), Ok(json!({ "seq": 20, "parties": ["ghost"] })));
    assert!(pass(&source, local.as_ref(), 13, at, "NZD").await.unwrap_err().contains("unscripted"));
    source.set(json!("status"), Ok(json!({ "today": null })));
    assert!(pass(&source, local.as_ref(), 13, at, "NZD").await.unwrap_err().contains("business date"));
    source.set(json!("status"), Ok(json!({ "today": today })));
    source.set(json!({ "changed_parties": { "since": 13 } }), Err("no leader".into()));
    assert_eq!(pass(&source, local.as_ref(), 13, at, "NZD").await, Err("no leader".to_string()));
    // A fact the channel module refuses stops the pass, so it is seen and not skipped.
    source.set(json!({ "changed_parties": { "since": 13 } }), Ok(json!({ "seq": 21, "parties": ["adv-1"] })));
    source.set(
        json!({ "engagements": { "adviser": "adv-1", "as_at": today } }),
        Ok(engagements(
            json!([{ "id": "e1", "adviser": "adv-1", "with": "fap-1", "from": "2027-01-01", "to": "2026-01-01" }]),
        )),
    );
    assert!(pass(&source, local.as_ref(), 13, at, "NZD").await.unwrap_err().contains("refused"));
}
