//! The module as a ledger node hosts it: events in through `on_event`,
//! commands and questions as JSON, postings out through the driver's envelopes.

mod common;

use std::collections::BTreeMap;

use common::*;
use ic_core::counterparty::CounterpartyPosting;
use ic_core::{Command as CoreCommand, Reject};
use ic_distribution::config::{BookOutcome, Change, MoveRequest};
use ic_distribution::host::{ask, module_spec, posting_envelope, refusal, Question, CORE_CALLER, MODULE};
use ic_distribution::rules::Share;
use ic_distribution::structure::{LicenceState, PartyKind};
use ic_distribution::workbench::Action;
use ic_distribution::{Actor, BookId, Command, Envelope};
use ic_money::{Currency::NZD, Money};
use ic_raft::ModuleState;
use ic_sim::Cluster;
use serde_json::{json, Value};

struct Hosted {
    cluster: Cluster,
    module: Box<dyn ModuleState>,
    index: u64,
}

impl Hosted {
    fn new() -> Self {
        let mut cluster = Cluster::in_memory(1, 7);
        cluster.open(d(START.0, START.1, START.2)).unwrap();
        for activation in ic_sim::config::shipped_activations().unwrap() {
            cluster.activate_everywhere(activation).unwrap();
        }
        let mut h = Self {
            cluster,
            module: (module_spec().unwrap().make)(),
            index: 0,
        };
        h.pump();
        h
    }

    /// What the node and the driver do between them: feed events, send what
    /// is unconfirmed to the core, until nothing is left.
    fn pump(&mut self) {
        loop {
            let events = self.cluster.take_events();
            for e in &events {
                self.index += 1;
                self.module.on_event(self.index, e);
            }
            let unconfirmed: Vec<(u16, CounterpartyPosting)> = serde_json::from_value(self.ask(json!("unconfirmed"))).unwrap();
            if events.is_empty() && unconfirmed.is_empty() {
                break;
            }
            for (partition, posting) in unconfirmed {
                let envelope = posting_envelope(posting, "salt".into());
                assert_eq!(envelope.caller, CORE_CALLER);
                let applied = self.cluster.submit_keyed(partition, &envelope.key, &envelope.caller, envelope.command);
                applied.result.unwrap();
            }
        }
    }

    fn ask(&self, question: Value) -> Value {
        self.module.query(question).unwrap()
    }

    fn send(&mut self, key: &str, actor: Actor, command: Command) -> Value {
        self.index += 1;
        let envelope = Envelope {
            key: key.into(),
            actor,
            command,
        };
        let out = self.module.apply(self.index, serde_json::to_value(envelope).unwrap()).unwrap();
        assert_eq!(out["result"], json!({ "Ok": null }), "{key}: {out}");
        out
    }
}

/// Purpose: hosted the way a ledger node hosts it, the module follows the core's events, takes
/// JSON commands, has its postings delivered by the driver's envelopes, answers every question
/// the portals, the workbench and finance's month-end ask, and comes back from a snapshot
/// identical.
/// Why it matters: this is the production wiring. If the hosted form dropped an event, could
/// not decode what the gateway sends, or lost anything across a snapshot, commission would be
/// wrong for every payee at once and no unit test of the state machine would show it.
#[test]
fn the_hosted_module_earns_holds_answers_and_survives_a_snapshot() {
    let spec = module_spec().unwrap();
    assert_eq!((spec.name.as_str(), spec.events), (MODULE, true));
    let mut h = Hosted::new();
    for (id, kind) in [("adviser-a", PartyKind::Adviser), ("a-co", PartyKind::Company), ("fap-one", PartyKind::Fap)] {
        h.send(&format!("party-{id}"), admin(), Command::AddParty { party: party(id), kind });
    }
    for id in ["a-co", "fap-one"] {
        h.send(
            &format!("payee-{id}"),
            admin(),
            Command::RegisterPayee {
                party: party(id),
                agreement_ref: "agreement".into(),
                advance_recovery_ppm: 0,
            },
        );
    }
    h.send(
        "engage",
        admin(),
        Command::StartEngagement {
            id: "eng-a-one".into(),
            engagement: ic_distribution::structure::Engagement {
                adviser: party("adviser-a"),
                with: party("fap-one"),
                from: d(2020, 1, 1),
                to: None,
                accreditations: ["term-life".to_string()].into(),
            },
        },
    );
    let lines = vec![
        base("a-co", Share::OfRemainder(pct(75.0)), true),
        base("fap-one", Share::OfRemainder(pct(25.0)), false),
    ];
    let changes = vec![
        Change::ActivateSchedule(schedule()),
        Change::CreateBook {
            id: BookId::from("book"),
            name: "Book".into(),
            engagement: Some("eng-a-one".into()),
            owner: Some(party("fap-one")),
            house: false,
        },
        Change::ActivateRule(rule("book", 1, d(START.0, START.1, START.2), lines, vec![liable("fap-one")])),
    ];
    h.send(
        "propose",
        ops(),
        Command::ProposeChange {
            id: "setup".into(),
            changes,
            instructions_ref: "doc".into(),
        },
    );
    h.send(
        "approve",
        approver(),
        Command::ApproveChange {
            id: "setup".into(),
            compliance: false,
        },
    );
    h.send(
        "comply",
        compliance(),
        Command::ApproveChange {
            id: "setup".into(),
            compliance: true,
        },
    );

    let issue = ic_sim::persona::term_life(
        1,
        d(START.0, START.1, START.2),
        d(1990, 3, 10),
        ic_product::Sex::Female,
        false,
        500_000_00,
        None,
        ic_product::Frequency::Monthly,
        None,
    );
    let policy = issue.policy.clone();
    h.cluster.submit(0, "new-business", ic_sim::persona::issue_command(issue)).unwrap();
    h.pump();
    h.send(
        "write",
        module("new-business"),
        Command::WriteLayer(Box::new(ic_distribution::state::WriteLayer {
            policy: policy.clone(),
            cover: "life".into(),
            layer: 1,
            writing: vec![("eng-a-one".into(), 1_000_000)],
            books: vec![("book".into(), 1_000_000)],
            schedule: "standard".into(),
            option: "upfront".into(),
            annual_premium_minor: 1_200_00,
            replacement: false,
        })),
    );
    let question = |q: Question| serde_json::to_value(q).unwrap();
    let today = h.cluster.business_date().unwrap();
    let disclosure = h.ask(question(Question::DryRun {
        policy: policy.clone(),
        date: today,
        net_by_cover_type: BTreeMap::from([("life".to_string(), 100_00)]),
    }));
    assert_eq!(
        disclosure["a-co"],
        json!(900_00 + 7_50),
        "initial on the annual premium and renewal on this one"
    );

    let owing = h.cluster.state(0).policies[&policy].outstanding_minor();
    let receipt = CoreCommand::ApplyReceipt {
        policy: policy.clone(),
        receipt: "r1".into(),
        amount: Money::new(owing, NZD),
    };
    h.cluster.submit(0, "cash-matching", receipt).unwrap();
    h.pump();
    let position = h.ask(question(Question::Position {
        party: party("fap-one"),
        as_at: today,
    }));
    let earned = position["summary"]["commission_minor"].as_i64().unwrap();
    assert!(earned > 300_00);
    assert_eq!(position["clawback_at_risk_minor"], json!(1_200_00));
    assert_eq!(h.ask(question(Question::ClawbackExposure { date: today }))["fap-one"], json!(1_200_00));

    // The provider's licence is suspended from the start: what it was paid moves to held.
    h.send(
        "licence",
        compliance(),
        Command::RecordLicence {
            party: party("fap-one"),
            state: LicenceState::Suspended,
            effective: today,
            source_ref: "notice-1".into(),
        },
    );
    h.pump();
    let aged = h.ask(question(Question::AgedHeld { as_at: today }));
    assert_eq!(
        (aged["kind"].as_str(), aged["count"].as_u64(), aged["total_minor"].as_i64()),
        (Some("held_balance"), Some(1), Some(earned))
    );
    let item = &aged["items"][0];
    assert_eq!(
        (item["party"].as_str(), item["reason"].as_str(), item["action"].is_null()),
        (Some("fap-one"), Some("SuspendedLicence"), true)
    );
    assert_eq!(
        h.ask(question(Question::AgedHeld {
            as_at: today.pred_opt().unwrap()
        }))["count"],
        json!(0)
    );
    assert_eq!(h.ask(question(Question::HeldControl { date: today }))["held_total_minor"], json!(earned));
    assert_eq!(h.ask(question(Question::MonthEndBlocks { date: today })), json!([]));
    let case = h.ask(question(Question::Case {
        id: format!("conduct-fap-one-{today}"),
    }));
    assert_eq!(case["owner"], json!("conduct_officer"));

    // A documented decision to keep it held shows on the aged report as the action taken.
    let hold = Action::HoldPending {
        review_on: today.checked_add_days(chrono::Days::new(30)).unwrap(),
        legal_ref: "legal-1".into(),
    };
    h.send(
        "hold",
        finance(),
        Command::ProposeAction {
            id: "hold-1".into(),
            party: party("fap-one"),
            action: hold,
            reason_code: "licence_suspended".into(),
            document_ref: "vault-doc-7".into(),
        },
    );
    h.send(
        "hold-a",
        finance_approver(),
        Command::ApproveAction {
            id: "hold-1".into(),
            as_role: ic_distribution::authority::Role::CounterpartyFinanceApprover,
        },
    );
    h.send(
        "hold-l",
        legal(),
        Command::ApproveAction {
            id: "hold-1".into(),
            as_role: ic_distribution::authority::Role::Legal,
        },
    );
    assert_eq!(h.ask(question(Question::AgedHeld { as_at: today }))["items"][0]["action"], json!("vault-doc-7"));
    assert_eq!(h.ask(question(Question::Action { id: "hold-1".into() }))["state"], json!("completed"));
    assert_eq!(h.ask(question(Question::OpenTasks)).as_array().unwrap().len(), 1, "the review");
    assert_eq!(h.ask(question(Question::SweepCheck)), json!({ "green": true, "items": [] }));
    assert!(h.ask(question(Question::ProofOfDebt {
        party: party("fap-one"),
        as_at: today
    }))["hash"]
        .is_string());
    assert!(h
        .ask(question(Question::Statement {
            payee: party("a-co"),
            number: 1
        }))
        .is_null());
    let status = h.ask(question(Question::Status));
    assert_eq!(
        (status["unconfirmed"].as_u64(), status["today"].as_str()),
        (Some(0), Some(today.to_string().as_str()))
    );

    // Planning a move is a question: it changes nothing and says why it cannot be done.
    let request = MoveRequest {
        from_engagement: "eng-a-one".into(),
        new_engagement: "eng-a-two".into(),
        to: party("fap-one"),
        on: today,
        accreditations: Default::default(),
        books: vec![BookOutcome::Stays {
            book: BookId::from("book"),
            new_servicing: "eng-a-one".into(),
        }],
    };
    assert_eq!(
        h.ask(question(Question::PlanMove(Box::new(request.clone()))))["changes"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    let unknown = MoveRequest {
        from_engagement: "ghost".into(),
        ..request
    };
    assert_eq!(
        h.ask(question(Question::PlanMove(Box::new(unknown))))["refused"]["code"],
        json!("unknown_engagement")
    );

    // What the host refuses, and what it keeps across a snapshot.
    assert!(h.module.query(json!({ "no_such_question": 1 })).unwrap_err().contains("not a question"));
    assert!(h.module.apply(999, json!({ "not": "an envelope" })).is_err());
    let saved = h.module.save().unwrap();
    let mut restored = (module_spec().unwrap().make)();
    restored.load(saved.clone()).unwrap();
    assert_eq!(restored.save().unwrap(), saved);
    assert_eq!(restored.query(question(Question::Status)).unwrap(), status);

    // The driver's report of a refusal is an ordinary command with a stable key.
    let envelope = refusal("wb:x:1", &Reject::new("ledger_closed", "closed"));
    assert_eq!(envelope.key, "refused:wb:x:1");
    assert!(matches!(envelope.command, Command::PostingRefused { ref code, .. } if code == "ledger_closed"));
    let state: ic_distribution::DistributionState = serde_json::from_value(saved).unwrap();
    assert!(ask(&state, Question::Unconfirmed).unwrap().as_array().unwrap().is_empty());
}
