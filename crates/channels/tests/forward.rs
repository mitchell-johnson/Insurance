//! The forwarder that carries the commission module's facts to the channels.

mod common;

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use common::*;
use ic_channels::directory::{DirectoryCommand, StructureLink};
use ic_channels::forward::{commands_for, conduct_by_party, pass, structure_for, Source, PUBLISHER, STRUCTURE};
use ic_channels::http::{Channel, Local};
use ic_channels::query::Question;
use ic_channels::state::Command;
use ic_channels::workbench::{PartyStatus, WorkbenchCommand};
use ic_distribution::rules::HeldReason;
use ic_distribution::structure::{CeasedReason, LicenceRecord, LicenceState, Relation, RelationKind};
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

    fn tell(&self, envelope: Value) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send + '_>> {
        Box::pin(async move { Err(format!("nothing is sent to the commission module in these tests: {envelope}")) })
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
        json!("relations"),
        json!("conduct_cases"),
    ] {
        ic_distribution::host::answer(&theirs, question.clone()).unwrap_or_else(|e| panic!("{question}: {e}"));
    }
    let changed = ic_distribution::host::answer(&theirs, json!({ "changed_parties": { "since": 0 } })).unwrap();
    assert_eq!((changed["seq"].as_u64(), changed["parties"].as_array().map(Vec::len)), (Some(0), Some(0)));
    assert!(ic_distribution::host::answer(&theirs, json!("status")).unwrap().get("today").is_some());

    // No structure and no conduct cases read as none; that module's own relation type reads as a link, except who is paid for whom.
    assert_eq!(
        structure_for(ic_distribution::host::answer(&theirs, json!("relations")).unwrap(), None),
        Ok(vec![])
    );
    assert_eq!(
        conduct_by_party(ic_distribution::host::answer(&theirs, json!("conduct_cases")).unwrap()),
        Ok(BTreeMap::new())
    );
    let relation = |kind| Relation {
        child: p("fap-1"),
        parent: p("grp-1"),
        kind,
        from: d(2026, 1, 1),
        to: Some(d(2028, 1, 1)),
    };
    let theirs_now = serde_json::to_value([
        relation(RelationKind::Under),
        relation(RelationKind::LicensedBy),
        relation(RelationKind::PayeeFor),
    ])
    .unwrap();
    let link = Command::Directory(DirectoryCommand::RecordStructure {
        child: p("fap-1"),
        link: StructureLink {
            parent: p("grp-1"),
            from: d(2026, 1, 1),
            to: Some(d(2028, 1, 1)),
        },
    });
    assert_eq!(structure_for(theirs_now.clone(), None), Ok(vec![link.clone(), link.clone()]));
    assert_eq!(structure_for(theirs_now.clone(), Some(&[p("grp-1")])).map(|c| c.len()), Ok(2));
    assert_eq!(structure_for(theirs_now, Some(&[p("adv-9")])), Ok(vec![]));
    assert!(structure_for(json!({}), None).unwrap_err().contains("relations"));
    assert!(conduct_by_party(json!([{ "case": 4 }])).unwrap_err().contains("conduct_cases"));

    // An empty position and an empty engagement list make one command: an empty publication.
    let empty = ic_distribution::host::answer(&theirs, json!({ "position": { "party": "adv-1", "as_at": today } })).unwrap();
    let none = ic_distribution::host::answer(&theirs, json!({ "engagements": { "adviser": "adv-1", "as_at": today } })).unwrap();
    let commands = commands_for(&p("adv-1"), empty, none, today, "NZD", vec![]).unwrap();
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
            vec![],
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
            vec![],
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
            vec![],
        )
        .unwrap();
    }
    assert!(commands_for(&p("x"), json!({ "held": 3 }), engagements(json!([])), today, "NZD", vec![])
        .unwrap_err()
        .contains("position"));
    assert!(
        commands_for(&p("x"), position("x", vec![], vec![], None, summary(0, 0)), json!({}), today, "NZD", vec![])
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
    source.set(json!("relations"), Ok(json!([])));
    source.set(json!("conduct_cases"), Ok(json!([])));
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

/// Purpose: a pass carries the structure above the advice business and the open conduct cases: a group's licence is revoked and the
/// adviser two levels below goes read-only naming the group; a conduct case reaches the body it was opened on and the advisers and
/// payees it names, and leaves when it closes; a later pass sends only the links that touch a party that changed.
/// Why it matters: addendum entry 4 follows a licence down the structure. Without the links the portal would cut off advisers under a
/// suspended provider but go on serving those under a provider whose parent lost its licence, and the workbench would not know to
/// bring the conduct officer in.
#[tokio::test]
async fn a_pass_carries_the_structure_and_the_conduct_cases() {
    let w = World::new();
    let at = w.at;
    let local = Arc::new(Local::new(w.ch));
    let today = "2027-12-01";
    let source = Scripted::default();
    source.set(json!("status"), Ok(json!({ "today": today })));
    let quiet = |party: &str, licences: Vec<LicenceRecord>| {
        source.set(
            json!({ "position": { "party": party, "as_at": today } }),
            Ok(position(party, vec![], licences, None, summary(0, 0))),
        );
        source.set(json!({ "engagements": { "adviser": party, "as_at": today } }), Ok(engagements(json!([]))));
    };
    quiet("grp-1", vec![licence(LicenceState::Revoked, d(2027, 11, 12))]);
    quiet("fap-1", vec![]);
    quiet("pay-1", vec![]);
    source.set(
        json!({ "position": { "party": "adv-1", "as_at": today } }),
        Ok(position("adv-1", vec![], vec![], None, summary(0, 0))),
    );
    source.set(
        json!({ "engagements": { "adviser": "adv-1", "as_at": today } }),
        Ok(engagements(
            json!([{ "id": "e1", "adviser": "adv-1", "with": "fap-1", "from": "2025-01-01", "to": null }]),
        )),
    );
    source.set(
        json!("relations"),
        Ok(json!([
            { "child": "fap-1", "parent": "grp-1", "kind": "licensed_by", "from": "2025-01-01", "to": null },
            { "child": "pay-1", "parent": "adv-1", "kind": "payee_for", "from": "2025-01-01", "to": null },
        ])),
    );
    source.set(
        json!("conduct_cases"),
        Ok(json!([
            { "case": "cc-1", "licence_event": "notice-1", "party": "grp-1", "advisers": ["adv-1"], "payees": ["pay-1", "adv-1"], "policies": [], "closed": null },
            { "case": "cc-0", "party": "adv-1", "closed": "2027-10-01" },
        ])),
    );
    source.set(
        json!({ "changed_parties": { "since": 0 } }),
        Ok(json!({ "seq": 4, "parties": ["grp-1", "fap-1", "adv-1", "pay-1"] })),
    );
    assert_eq!(pass(&source, local.as_ref(), 0, at, "NZD").await, Ok(4));

    let book = local.ask(Question::Book { adviser: p("adv-1"), at }).await.unwrap();
    assert_eq!(book["read_only"], true);
    assert_eq!(book["engagements"][0]["standing"]["licence_of"], "grp-1");
    local.with(|s| {
        // Who is paid for whom is not a link a licence follows.
        assert_eq!(s.data.directory.structure.keys().collect::<Vec<_>>(), [&p("fap-1")]);
        let conduct = |party: &str| s.data.workbench.positions[&p(party)].conduct.iter().cloned().collect::<Vec<_>>();
        assert_eq!(
            (conduct("grp-1"), conduct("adv-1"), conduct("pay-1")),
            (vec!["cc-1".to_string()], vec!["cc-1".to_string()], vec!["cc-1".to_string()])
        );
        assert!(conduct("fap-1").is_empty());
    });

    // Later: the case closes, and a new link appears between parties that did not change. Only adv-1 is forwarded.
    source.set(
        json!("conduct_cases"),
        Ok(json!([{ "case": "cc-1", "party": "grp-1", "advisers": ["adv-1"], "closed": "2027-12-01" }])),
    );
    source.set(
        json!("relations"),
        Ok(json!([
            { "child": "fap-1", "parent": "grp-1", "kind": "licensed_by", "from": "2025-01-01", "to": "2027-11-01" },
            { "child": "fap-2", "parent": "grp-2", "kind": "under", "from": "2025-01-01", "to": null },
        ])),
    );
    source.set(
        json!({ "changed_parties": { "since": 4 } }),
        Ok(json!({ "seq": 5, "parties": ["adv-1", "fap-1"] })),
    );
    assert_eq!(pass(&source, local.as_ref(), 4, at + 60, "NZD").await, Ok(5));
    local.with(|s| {
        assert!(s.data.workbench.positions[&p("adv-1")].conduct.is_empty());
        assert!(!s.data.directory.structure.contains_key(&p("fap-2")));
        // The link ended before the revocation, replacing the open one: the group's licence no longer reaches the adviser.
        assert_eq!(s.data.directory.structure[&p("fap-1")].len(), 1);
    });
    let book = local
        .ask(Question::Book {
            adviser: p("adv-1"),
            at: at + 60,
        })
        .await
        .unwrap();
    assert_eq!(book["read_only"], false);

    // A structure the directory refuses stops the pass.
    source.set(
        json!("relations"),
        Ok(json!([{ "child": "fap-1", "parent": "fap-1", "kind": "under", "from": "2025-01-01", "to": null }])),
    );
    source.set(json!({ "changed_parties": { "since": 5 } }), Ok(json!({ "seq": 6, "parties": ["fap-1"] })));
    assert!(pass(&source, local.as_ref(), 5, at, "NZD").await.unwrap_err().contains("the structure"));
}
