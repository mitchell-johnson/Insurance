//! A small harness for driving the documents state machine in tests.
#![allow(dead_code)]

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use ic_core::{PartyId, PolicyId};
use ic_documents::command::{ApprovalRole, Command, Envelope, Issue, NoticeSpec, RenderCase};
use ic_documents::model::{Channel, Recipient, RecipientRole};
use ic_documents::template::{sha256_hex, AlternateFormat, ProductBinding, Template, TemplateRef};
use ic_documents::{Applied, DocumentKind, DocumentsState, Event};
use ic_log::StateMachine;
use serde_json::{json, Value};

pub struct Kit {
    pub state: DocumentsState,
    pub log: Vec<Envelope>,
    pub now: DateTime<Utc>,
}

pub fn date(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

pub fn party(s: &str) -> PartyId {
    PartyId::from(s)
}

pub fn policy(s: &str) -> PolicyId {
    PolicyId::from(s)
}

pub fn tref(id: &str, version: u32) -> TemplateRef {
    TemplateRef { id: id.to_string(), version }
}

pub fn template(id: &str, kind: DocumentKind, body: &str) -> Template {
    Template {
        id: id.to_string(),
        version: 1,
        kind,
        title: id.to_string(),
        products: vec![ProductBinding { code: "term-life".into(), version: 1 }],
        definitions_version: Some("defs-2027".into()),
        primary_language: "en".into(),
        bodies: BTreeMap::from([("en".to_string(), body.to_string())]),
        controlling_statements: BTreeMap::new(),
        parts: vec![],
        alternate_formats: vec![AlternateFormat::LargePrint],
        minimum_notice_days: None,
        review_by: date("2028-11-15"),
        author: "author".into(),
    }
}

pub fn recipient(p: &str, role: RecipientRole, channels: &[Channel]) -> Recipient {
    Recipient {
        party: party(p),
        role,
        relationship: Some("policyholder".into()),
        channels: channels.to_vec(),
    }
}

pub fn sample_data() -> Value {
    json!({"name": "Aroha", "premium": 12345, "anniversary": "2028-01-15"})
}

impl Kit {
    /// An empty module at midday on 15 November 2027, New Zealand time.
    pub fn new() -> Self {
        Self {
            state: DocumentsState::new(),
            log: vec![],
            now: Utc.with_ymd_and_hms(2027, 11, 14, 23, 0, 0).unwrap(),
        }
    }

    pub fn advance_hours(&mut self, hours: i64) {
        self.now += Duration::hours(hours);
    }

    pub fn advance_days(&mut self, days: i64) {
        self.now += Duration::days(days);
    }

    pub fn today(&self) -> NaiveDate {
        ic_documents::state::nz_date(self.now)
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
        let documents = self.state.documents.clone();
        let templates = self.state.templates.clone();
        let defects = self.state.defects.clone();
        let applied = self.send(caller, actor, command);
        let code = applied.result.expect_err("the command should have been refused").code;
        assert!(applied.events.is_empty(), "a refused command produced events");
        // A refusal leaves nothing behind but its stored result.
        assert!(self.state.documents == documents && self.state.templates == templates && self.state.defects == defects, "a refused command changed state");
        code
    }

    pub fn approve(&mut self, r: &TemplateRef, roles: &[ApprovalRole]) {
        for role in roles {
            let actor = format!("{role:?}").to_lowercase();
            self.ok("config", &actor, Command::ApproveTemplate { template: r.clone(), role: *role });
        }
    }

    pub fn case_for(&self, t: &Template, data: &Value) -> RenderCase {
        let text = t.render(&self.state.parts, "en", data).unwrap();
        RenderCase {
            product_code: "term-life".into(),
            product_version: 1,
            persona: "aroha".into(),
            language: "en".into(),
            data: data.clone(),
            product_fields: ic_documents::render::supplied(data).into_iter().collect(),
            expected_sha256: sha256_hex(text.as_bytes()),
        }
    }

    /// Take a template from submission to active today.
    pub fn activate(&mut self, t: Template, data: &Value) -> TemplateRef {
        let r = t.reference();
        self.ok("config", "author", Command::SubmitTemplate(Box::new(t.clone())));
        self.approve(&r, &[ApprovalRole::Legal, ApprovalRole::Product, ApprovalRole::Compliance]);
        let case = self.case_for(&t, data);
        self.ok("config", "author", Command::RunRenderingTests { template: r.clone(), cases: vec![case] });
        let today = self.today();
        self.ok("config", "product", Command::ActivateTemplate { template: r.clone(), effective_from: today });
        r
    }

    /// A module with a wording, a schedule, a renewal notice (30 days), a
    /// lapse notice and a receipt active, and keys for the usual parties.
    pub fn ready() -> Self {
        let mut kit = Self::new();
        let data = sample_data();
        kit.activate(template("wording", DocumentKind::PolicyWording, "This policy pays the sum insured."), &data);
        kit.activate(template("schedule", DocumentKind::PolicySchedule, "Schedule for {{name}}. Premium {{premium|money}}."), &data);
        let mut renewal = template("renewal", DocumentKind::RenewalNotice, "Dear {{name}}. Your premium will be {{premium|money}} from {{anniversary|date}}.");
        renewal.minimum_notice_days = Some(30);
        kit.activate(renewal, &data);
        let mut lapse = template("lapse", DocumentKind::LapseNotice, "Dear {{name}}. Your policy will end if the premium is not paid.");
        lapse.minimum_notice_days = Some(14);
        kit.activate(lapse, &data);
        kit.activate(template("receipt", DocumentKind::PremiumReceipt, "We received {{premium|money}}. Thank you."), &data);
        kit.activate(template("decision", DocumentKind::ClaimDecisionLetter, "Dear {{name}}. We have made a decision on your claim."), &data);
        for p in ["aroha", "ben", "lender"] {
            kit.ok("documents", "svc", Command::RegisterPartyKey { party: party(p), wrapped_key: format!("wrapped-{p}") });
        }
        kit
    }

    pub fn issue_spec(&self, id: &str, kind: DocumentKind, template_id: &str) -> Issue {
        Issue {
            document: id.to_string(),
            kind,
            template: tref(template_id, 1),
            policy: Some(policy("pol-1")),
            other_policies: vec![],
            data_class: Default::default(),
            lives: vec![],
            prove_delivery: false,
            owner: party("aroha"),
            recipients: vec![recipient("aroha", RecipientRole::Addressee, &[Channel::Email])],
            language: "en".into(),
            format: None,
            content_sha256: sha256_hex(id.as_bytes()),
            data_sha256: sha256_hex(b"data"),
            blob: format!("docs/{id}"),
            wrapped_key: format!("dek-{id}"),
            bytes: 100,
            trigger_event: None,
            case: None,
            supersedes: None,
            reason: None,
            wordings: vec![],
            components: vec![],
            notice: None,
        }
    }

    pub fn renewal_spec(&self, id: &str) -> Issue {
        let mut issue = self.issue_spec(id, DocumentKind::RenewalNotice, "renewal");
        issue.trigger_event = Some("7-100".into());
        issue.notice = Some(NoticeSpec { purpose: "renewal".into(), corrects: None });
        issue
    }

    pub fn issue(&mut self, issue: Issue) -> Vec<Event> {
        self.ok("documents", "svc", Command::IssueDocument(Box::new(issue)))
    }

    pub fn issue_err(&mut self, issue: Issue) -> String {
        self.err("documents", "svc", Command::IssueDocument(Box::new(issue)))
    }

    pub fn sent(&mut self, document: &str, p: &str, channel: Channel) {
        self.ok(
            "delivery",
            "svc",
            Command::RecordSend {
                document: document.into(),
                party: party(p),
                channel,
                masked_destination: "a***@example.test".into(),
                provider_reference: "ref-1".into(),
            },
        );
    }

    pub fn outcome(&mut self, document: &str, p: &str, channel: Channel, outcome: ic_documents::model::AttemptOutcome) -> Vec<Event> {
        self.ok("delivery", "svc", Command::RecordOutcome { document: document.into(), party: party(p), channel, outcome })
    }

    /// Send and confirm on one channel.
    pub fn deliver(&mut self, document: &str, p: &str, channel: Channel) -> Vec<Event> {
        self.sent(document, p, channel);
        self.outcome(document, p, channel, ic_documents::model::AttemptOutcome::Delivered)
    }

    pub fn tick(&mut self) -> Vec<Event> {
        self.ok("delivery", "svc", Command::Tick)
    }
}
