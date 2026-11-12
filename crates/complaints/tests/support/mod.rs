//! A small harness for driving the complaints state machine in tests.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use ic_complaints::command::{CloseDetail, Intake};
use ic_complaints::model::*;
use ic_complaints::{Applied, Command, ComplaintsState, Envelope, Event, Timers};
use ic_core::{PartyId, PolicyId};
use ic_log::StateMachine;

pub struct Kit {
    pub state: ComplaintsState,
    pub log: Vec<Envelope>,
    pub now: DateTime<Utc>,
}

pub fn party(s: &str) -> PartyId {
    PartyId::from(s)
}

pub fn policy(s: &str) -> PolicyId {
    PolicyId::from(s)
}

/// Timers short enough to walk through in a test: acknowledge in 5 days,
/// update every 10, decide in 20, deadlock at 40.
pub fn timers() -> Timers {
    Timers {
        version: 1,
        acknowledge_days: 5,
        update_every_days: 10,
        decide_days: 20,
        deadlock_days: 40,
        warning_days: 2,
        conduct_officer_after_days: 3,
        complainant_response_days: 10,
        vulnerable_extra_days: 10,
        front_line_limit_minor: 10_000,
        ex_gratia_limits_minor: BTreeMap::from([(1, 50_000), (3, 500_000)]),
        regulator_notification_days: 10,
        retain_closed_days: 100,
        cause_codes: BTreeSet::from([
            "delay".to_string(),
            "wrong_decision".to_string(),
            "poor_explanation".to_string(),
            "adviser_conduct".to_string(),
        ]),
    }
}

/// A claim decision made by `assessor-1` at authority 2 under rule `claims-7`.
pub fn claim_decision() -> DecisionLink {
    DecisionLink {
        decision: "claim-1:decline".into(),
        kind: DecisionKind::Claim,
        decided_by: "assessor-1".into(),
        authority: 2,
        rule_version: Some("claims-7".into()),
        wording_version: Some("wording-3".into()),
    }
}

pub fn intake(id: &str, process: Process) -> Intake {
    Intake {
        complaint: id.to_string(),
        channel: IntakeChannel::Call,
        complainant: party("aroha"),
        complainant_kind: ComplainantKind::Policyholder,
        links: Links {
            policies: vec![policy("pol-1")],
            ..Links::default()
        },
        category: Category {
            product: Some("term-life".into()),
            process,
            cause: None,
        },
        summary_document: format!("doc-summary-{id}"),
        vulnerable: false,
        adjustments: vec![],
        resolved_on_the_spot: None,
        received_on: None,
        acknowledged_on: None,
    }
}

/// A complaint about a declined claim.
pub fn claim_intake(id: &str) -> Intake {
    let mut i = intake(id, Process::Claims);
    i.complainant_kind = ComplainantKind::Claimant;
    i.links.cases = vec!["claim-1".into()];
    i.links.decisions = vec![claim_decision()];
    i
}

pub fn detail(cause: &str) -> CloseDetail {
    CloseDetail {
        cause: cause.into(),
        product_version: None,
        rule_version: None,
        template_version: None,
    }
}

impl Kit {
    /// An empty module at midday on 15 November 2027, New Zealand time.
    pub fn new() -> Self {
        Self {
            state: ComplaintsState::new(),
            log: vec![],
            now: Utc.with_ymd_and_hms(2027, 11, 14, 23, 0, 0).unwrap(),
        }
    }

    /// A module with the test timers active.
    pub fn ready() -> Self {
        let mut kit = Self::new();
        kit.ok("config", "ops", Command::ActivateTimers(timers()));
        kit
    }

    pub fn advance_days(&mut self, days: i64) {
        self.now += Duration::days(days);
    }

    pub fn today(&self) -> NaiveDate {
        ic_complaints::state::nz_date(self.now)
    }

    pub fn send_keyed(&mut self, key: &str, caller: &str, actor: &str, command: Command) -> Applied {
        let envelope = Envelope {
            key: key.to_string(),
            caller: caller.to_string(),
            actor: actor.to_string(),
            at: self.now,
            command,
        };
        // Through JSON, as the log would carry it.
        let envelope: Envelope = serde_json::from_slice(&serde_json::to_vec(&envelope).unwrap()).unwrap();
        self.log.push(envelope.clone());
        self.state.apply(self.log.len() as u64, envelope)
    }

    pub fn send(&mut self, caller: &str, actor: &str, command: Command) -> Applied {
        let key = format!("k{}", self.log.len() + 1);
        self.send_keyed(&key, caller, actor, command)
    }

    /// Send a command that must be accepted; returns its events.
    pub fn ok(&mut self, caller: &str, actor: &str, command: Command) -> Vec<Event> {
        let applied = self.send(caller, actor, command);
        if let Err(e) = &applied.result {
            panic!("the command was refused: {e}");
        }
        applied.events.into_iter().map(|e| e.event).collect()
    }

    /// Send a command that must be refused; returns the refusal's code.
    pub fn err(&mut self, caller: &str, actor: &str, command: Command) -> String {
        let complaints = self.state.complaints.clone();
        let conduct = self.state.conduct.clone();
        let applied = self.send(caller, actor, command);
        let code = applied.result.expect_err("the command should have been refused").code;
        assert!(applied.events.is_empty(), "a refused command produced events");
        assert!(
            self.state.complaints == complaints && self.state.conduct == conduct,
            "a refused command changed state"
        );
        code
    }

    pub fn record(&mut self, intake: Intake) -> Vec<Event> {
        self.ok("intake", "agent-1", Command::RecordComplaint(Box::new(intake)))
    }

    pub fn assign(&mut self, complaint: &str, handler: &str, authority: u8) {
        self.ok(
            "complaints",
            "lead",
            Command::Assign {
                complaint: complaint.into(),
                handler: handler.into(),
                authority,
                trained_for_vulnerability: true,
            },
        );
    }

    pub fn tick(&mut self) -> Vec<Event> {
        self.ok("scheduler", "timer", Command::Tick)
    }

    /// A complaint about a declined claim, assigned to `reviewer-1` at
    /// authority 3 and acknowledged.
    pub fn in_review(&mut self, id: &str) {
        self.record(claim_intake(id));
        self.assign(id, "reviewer-1", 3);
        self.ok(
            "complaints",
            "reviewer-1",
            Command::RecordAcknowledgement {
                complaint: id.into(),
                document: format!("doc-ack-{id}"),
            },
        );
    }

    /// Decide a complaint as its reviewer, with a written outcome.
    pub fn decide(&mut self, id: &str, outcome: OutcomeKind) -> Vec<Event> {
        self.ok(
            "complaints",
            "reviewer-1",
            Command::Decide {
                complaint: id.into(),
                outcome,
                reason_codes: vec!["evidence_reassessed".into()],
                document: Some(format!("doc-outcome-{id}")),
            },
        )
    }

    pub fn delivered(&mut self, id: &str) {
        let today = self.today();
        self.ok(
            "documents",
            "documents-service",
            Command::RecordOutcomeDelivered {
                complaint: id.into(),
                on: today,
            },
        );
    }

    pub fn complaint(&self, id: &str) -> &Complaint {
        &self.state.complaints[id]
    }
}
