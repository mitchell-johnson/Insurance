//! The letters the complaints register says are owed, issued by the
//! documents driver and recorded on the complaint or the case.

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use ic_complaints::command::Intake;
use ic_complaints::model::{
    Category, ComplainantKind, ConductKind, Contact as Told, IntakeChannel, LetterDue, LetterKind, Links, OutcomeKind, Process, Representative,
};
use ic_complaints::module::Query;
use ic_complaints::{Applied, Command as Cmd, Envelope as Env, Timers};
use ic_core::PartyId;
use ic_documents::channels::{Channels, Contact, MemoryDirectory, Outbound, SendError};
use ic_documents::complaint_letters::{document_id, run_letters, LetterIssuer, LetterPass, LetterTemplates};
use ic_documents::complaints_relay::{Complaints, LocalComplaints};
use ic_documents::crypto::{self, LocalWrap};
use ic_documents::driver::{Driver, DriverError, LocalModule, Module};
use ic_documents::model::{Channel, RecipientRole};
use ic_documents::template::AlternateFormat;
use ic_documents::vault::MemoryStore;
use ic_documents::DocumentKind;
use serde_json::{json, Value};
use support::*;

#[derive(Clone, Default)]
struct Quiet;

impl Channels for Quiet {
    async fn send(&self, message: &Outbound) -> Result<String, SendError> {
        Ok(format!("ref-{}", message.document))
    }
}

type TestDriver = Driver<LocalModule, MemoryStore, LocalWrap, Quiet, MemoryDirectory>;

/// The complaints module behind a switch that fails the next command, or
/// answers the letters question with a list a test fixed.
#[derive(Clone, Default)]
struct Shaky {
    inner: LocalComplaints,
    fail_next_command: Arc<Mutex<bool>>,
    fixed: Arc<Mutex<Option<Value>>>,
    /// Refuse the question about outcomes waiting for delivery.
    outcomes_down: Arc<Mutex<bool>>,
}

impl Complaints for Shaky {
    async fn propose(&self, envelope: Env) -> Result<Applied, DriverError> {
        if std::mem::take(&mut *self.fail_next_command.lock().unwrap()) {
            return Err(DriverError::Unavailable("no leader".into()));
        }
        self.inner.propose(envelope).await
    }

    async fn ask(&self, query: Query) -> Result<Value, DriverError> {
        if matches!(query, Query::OutcomesAwaitingDelivery { .. }) && *self.outcomes_down.lock().unwrap() {
            return Err(DriverError::Unavailable("no leader".into()));
        }
        let fixed = self.fixed.lock().unwrap().clone();
        match (&query, fixed) {
            (Query::LettersDue { .. }, Some(fixed)) => Ok(fixed),
            _ => self.inner.ask(query).await,
        }
    }
}

struct World {
    issuer: LetterIssuer<Shaky, Arc<TestDriver>>,
    driver: Arc<TestDriver>,
    now: DateTime<Utc>,
}

const ACK: &str = "Dear {{name}}. We got your complaint {{reference}} on {{date|date}}. We will write by {{due|date}}.";
const DEADLOCK: &str = "Dear {{name}}. We could not agree on complaint {{reference}}. You can take it to the scheme.";
const UPDATE: &str = "Dear {{name}}. This is update {{number}} on complaint {{reference}}. We will write again by {{due|date}}.";
const LICENCE: &str = "Dear {{name}}. The licence of your adviser was {{status}} on {{effective|date}}.";

fn letter_data() -> Value {
    json!({ "name": "Aroha", "reference": "c1", "date": "2027-11-15", "due": "2027-11-20", "status": "revoked", "effective": "2027-11-12", "number": "1" })
}

fn timers() -> Timers {
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
        ex_gratia_limits_minor: BTreeMap::from([(1, 50_000)]),
        regulator_notification_days: 10,
        retain_closed_days: 100,
        cause_codes: BTreeSet::from(["delay".to_string()]),
    }
}

fn intake(id: &str, who: &str) -> Cmd {
    Cmd::RecordComplaint(Box::new(Intake {
        complaint: id.into(),
        channel: IntakeChannel::Call,
        complainant: PartyId::from(who),
        complainant_kind: ComplainantKind::Policyholder,
        links: Links::default(),
        category: Category {
            product: None,
            process: Process::Service,
            cause: None,
        },
        summary_document: "doc-summary".into(),
        vulnerable: false,
        adjustments: vec![],
        resolved_on_the_spot: None,
        received_on: None,
        acknowledged_on: None,
    }))
}

impl World {
    /// A documents module with the templates named active, a complaints
    /// module with its timers, and a customer master that knows Aroha.
    async fn new(templates: &[(&str, DocumentKind, &str)]) -> Self {
        let mut kit = Kit::new();
        let now = kit.now;
        for (id, kind, body) in templates {
            let mut t = template(id, *kind, body);
            t.alternate_formats = vec![AlternateFormat::LargePrint];
            kit.activate(t, &letter_data());
        }
        let module = LocalModule::default();
        for envelope in &kit.log {
            assert!(module.propose(envelope.clone()).await.unwrap().result.is_ok());
        }
        let directory = MemoryDirectory::default();
        directory.set("aroha", Channel::Email, "Aroha Ngata", "aroha@example.nz");
        let driver = Arc::new(Driver {
            module,
            objects: MemoryStore::default(),
            wrap: LocalWrap::new(crypto::new_key()),
            channels: Quiet,
            directory,
            actor: "documents-driver".into(),
            clock: Arc::new(move || now),
            batch: 100,
            composer: Default::default(),
        });
        let issuer = LetterIssuer {
            complaints: Shaky::default(),
            letters: driver.clone(),
            templates: LetterTemplates::default(),
            actor: "documents-service".into(),
            clock: Arc::new(move || now),
            batch: 100,
        };
        let world = Self { issuer, driver, now };
        world.complaints("config", Cmd::ActivateTimers(timers())).await;
        world
    }

    async fn all() -> Self {
        Self::new(&[
            ("complaint-acknowledgement", DocumentKind::GeneralCorrespondence, ACK),
            ("complaint-deadlock-notice", DocumentKind::DeadlockNotice, DEADLOCK),
            ("licence-event-notice", DocumentKind::GeneralCorrespondence, LICENCE),
            ("complaint-update", DocumentKind::GeneralCorrespondence, UPDATE),
        ])
        .await
    }

    /// Send the complaints module a command that must be accepted.
    async fn complaints(&self, caller: &str, command: Cmd) {
        let n = self.issuer.complaints.inner.read(|s| s.remembered_keys());
        let applied = self
            .issuer
            .complaints
            .inner
            .propose(Env {
                key: format!("setup-{n}"),
                caller: caller.into(),
                actor: if caller == "complaints" { "lead".into() } else { "setup".into() },
                at: self.now,
                command,
            })
            .await
            .unwrap();
        assert_eq!(applied.result, Ok(()));
    }

    /// A handler declares deadlock once the date to decide by (20 days on
    /// these timers) has passed without an outcome, as the register asks.
    async fn declare_deadlock(&self, complaint: &str) {
        let applied = self
            .issuer
            .complaints
            .inner
            .propose(Env {
                key: format!("deadlock-{complaint}"),
                caller: "complaints".into(),
                actor: "lead".into(),
                at: self.now + chrono::Duration::days(21),
                command: Cmd::DeclareDeadlock { complaint: complaint.into() },
            })
            .await
            .unwrap();
        assert_eq!(applied.result, Ok(()));
    }

    /// The scheduler's tick, `days` after the world's day.
    async fn tick(&self, days: i64) {
        let at = self.now + chrono::Duration::days(days);
        let applied = self
            .issuer
            .complaints
            .inner
            .propose(Env {
                key: format!("tick-{days}"),
                caller: "scheduler".into(),
                actor: "scheduler".into(),
                at,
                command: Cmd::Tick,
            })
            .await
            .unwrap();
        assert_eq!(applied.result, Ok(()));
    }

    /// Hand complaint `id` to the lead and decide it, with `document` as the
    /// written outcome.
    async fn decide(&self, id: &str, document: &str) {
        let assign = Cmd::Assign {
            complaint: id.into(),
            handler: "lead".into(),
            authority: 3,
            trained_for_vulnerability: true,
        };
        self.complaints("complaints", assign).await;
        let decide = Cmd::Decide {
            complaint: id.into(),
            outcome: OutcomeKind::NotUpheld,
            reason_codes: vec!["policy_terms".into()],
            document: Some(document.into()),
        };
        self.complaints("complaints", decide).await;
    }

    fn today(&self) -> NaiveDate {
        ic_complaints::state::nz_date(self.now)
    }

    async fn text(&self, document: &str) -> String {
        self.driver.read(document).await.unwrap().text
    }
}

/// Purpose: a complaint nobody has acknowledged gets its acknowledgement
/// from the template, addressed to the complainant and whoever acts for
/// them, in the format the customer master says they need; the complaint
/// records the document; and the next pass has nothing to do.
/// Why it matters: the acknowledgement timer is a regulatory promise. This
/// is what keeps it when no handler has got to the complaint, and a letter
/// issued twice or not recorded would either pester the complainant or
/// leave the timer escalating on a complaint that was answered.
#[tokio::test]
async fn an_acknowledgement_is_issued_and_recorded_once() {
    let w = World::all().await;
    w.driver.directory.set_contact(
        "aroha",
        Channel::Email,
        Contact {
            name: "Aroha Ngata".into(),
            destination: "aroha@example.nz".into(),
            language: Some("mi".into()),
            format: Some("large_print".into()),
            deceased: false,
        },
    );
    w.complaints("intake", intake("c1", "aroha")).await;
    for who in ["lawyer-1", "aroha"] {
        let representative = Representative {
            party: PartyId::from(who),
            relationship: "lawyer".into(),
            authority_document: "doc-authority".into(),
        };
        let command = Cmd::AddRepresentative {
            complaint: "c1".into(),
            representative,
        };
        w.complaints("complaints", command).await;
    }

    assert_eq!(
        w.issuer.pass().await,
        LetterPass {
            issued: 1,
            left: 0,
            reported: 0,
            errors: vec![]
        }
    );
    let document = w.driver.document("complaint-ack:c1").await.unwrap();
    assert_eq!(document.kind, DocumentKind::GeneralCorrespondence);
    assert_eq!(document.format, Some(AlternateFormat::LargePrint));
    // The template has no te reo version yet: its primary language is used.
    assert_eq!(document.language, "en");
    assert!(document.proof);
    assert_eq!(document.case.as_deref(), Some("c1"));
    let to: Vec<(String, RecipientRole)> = document
        .deliveries
        .iter()
        .map(|d| (d.recipient.party.0.to_string(), d.recipient.role))
        .collect();
    assert_eq!(
        to,
        vec![
            ("aroha".to_string(), RecipientRole::Addressee),
            ("lawyer-1".to_string(), RecipientRole::Representative)
        ],
        "a complainant who is also listed as acting for themselves is written to once"
    );
    assert_eq!(
        w.text("complaint-ack:c1").await,
        "Dear Aroha Ngata. We got your complaint c1 on 15 November 2027. We will write by 20 November 2027."
    );
    let complaint = w.issuer.complaints.inner.read(|s| s.complaints["c1"].clone());
    assert_eq!(complaint.acknowledgement.as_deref(), Some("complaint-ack:c1"));
    assert_eq!(complaint.acknowledged_on, Some(w.today()));

    assert_eq!(w.issuer.pass().await, LetterPass::default());
    assert_eq!(w.driver.module.state().documents.len(), 1);
}

/// Purpose: a deadlock gets its notice and each customer of a completed
/// licence case their letter, each recorded where it belongs.
/// Why it matters: a deadlocked complaint cannot close until the
/// complainant has been told of their right to go to the scheme, and a
/// licence case cannot close while any customer is untold. Both would
/// otherwise wait on someone writing each letter by hand.
#[tokio::test]
async fn deadlock_notices_and_licence_letters_are_issued() {
    let w = World::all().await;
    w.driver.directory.set("cust-1", Channel::Post, "Ben Tane", "1 High Street\nNapier 4110");
    w.complaints("intake", intake("c1", "aroha")).await;
    let ack = Cmd::RecordAcknowledgement {
        complaint: "c1".into(),
        document: "by-hand".into(),
    };
    w.complaints("complaints", ack).await;
    let assign = Cmd::Assign {
        complaint: "c1".into(),
        handler: "lead".into(),
        authority: 3,
        trained_for_vulnerability: true,
    };
    w.complaints("complaints", assign).await;
    w.declare_deadlock("c1").await;
    let kind = ConductKind::LicenceEvent {
        licence_event: "ev-1".into(),
        party: PartyId::from("fap-1"),
        status: "revoked".into(),
        effective: NaiveDate::from_ymd_opt(2027, 11, 12).unwrap(),
    };
    let open = Cmd::OpenConductCase {
        case: "licence:ev-1".into(),
        kind,
        owner: "conduct-officer".into(),
        summary_document: None,
        notify_regulator: false,
    };
    w.complaints("distribution", open).await;
    let attach = Cmd::AttachPopulation {
        case: "licence:ev-1".into(),
        customers: vec![PartyId::from("cust-1")],
        advisers: vec![],
        policies: vec![],
    };
    w.complaints("distribution", attach).await;
    let complete = Cmd::CompletePopulation {
        case: "licence:ev-1".into(),
        customers: 1,
        policies: 0,
    };
    w.complaints("distribution", complete).await;

    let pass = w.issuer.pass().await;
    assert_eq!((pass.issued, pass.left, pass.errors), (2, 0, vec![]));
    let notice = w.driver.document("complaint-deadlock:c1").await.unwrap();
    assert_eq!(notice.kind, DocumentKind::DeadlockNotice);
    assert_eq!(notice.trigger_event.as_deref(), Some("complaints:c1:DeadlockNotice"));
    assert_eq!(
        w.text("licence-notice:licence:ev-1:cust-1").await,
        "Dear Ben Tane. The licence of your adviser was revoked on 12 November 2027."
    );
    let (deadlock, told) = w.issuer.complaints.inner.read(|s| {
        (
            s.complaints["c1"].deadlock.clone().unwrap(),
            s.conduct["licence:ev-1"].customers[&PartyId::from("cust-1")].clone(),
        )
    });
    assert_eq!(deadlock.notice.as_deref(), Some("complaint-deadlock:c1"));
    assert_eq!(
        told,
        Told::Notified {
            document: "licence-notice:licence:ev-1:cust-1".into(),
            on: w.today()
        }
    );
    assert_eq!(w.issuer.pass().await, LetterPass::default());
}

/// Purpose: a kind of letter with no active template is left for a person
/// and said once per pass, while the kinds that have wording still go; a
/// complainant with no contact point, or who has died, is left too.
/// Why it matters: the service must never invent wording, and must not
/// address a letter to nobody or to someone who has died. But one missing
/// template must not stop every other letter, and the gap has to be
/// visible to whoever runs the service.
#[tokio::test]
async fn letters_that_cannot_be_written_are_left_for_a_person() {
    let w = World::new(&[("complaint-deadlock-notice", DocumentKind::DeadlockNotice, DEADLOCK)]).await;
    w.complaints("intake", intake("c1", "aroha")).await;
    w.complaints("intake", intake("c2", "aroha")).await;
    let assign = Cmd::Assign {
        complaint: "c2".into(),
        handler: "lead".into(),
        authority: 3,
        trained_for_vulnerability: true,
    };
    w.complaints("complaints", assign).await;
    w.declare_deadlock("c2").await;

    let pass = w.issuer.pass().await;
    assert_eq!((pass.issued, pass.left), (1, 2));
    assert_eq!(
        pass.errors,
        vec!["template complaint-acknowledgement has no active version; its letters are left for a person".to_string()]
    );
    assert!(w.driver.document("complaint-deadlock:c2").await.is_ok());

    let w = World::all().await;
    w.complaints("intake", intake("c1", "stranger")).await;
    w.complaints("intake", intake("c2", "ben")).await;
    w.driver.directory.set_contact(
        "ben",
        Channel::Post,
        Contact {
            name: "Ben".into(),
            destination: "1 High Street".into(),
            deceased: true,
            ..Contact::default()
        },
    );
    let pass = w.issuer.pass().await;
    assert_eq!((pass.issued, pass.left), (0, 2));
    assert_eq!(
        pass.errors,
        vec![
            "c1: stranger has no contact point; the letter is left for a person".to_string(),
            "c2: ben has died; the letter is left for a person".to_string()
        ]
    );
    assert!(w.driver.module.state().documents.is_empty());
}

/// Purpose: a format the template is not offered in does not stop the
/// letter, an outage of the customer master or of either module is
/// reported and the letter goes on a later pass, and a pass cut short
/// between issuing and recording issues nothing twice.
/// Why it matters: these are the ordinary faults of a running service. Each
/// must end with exactly one letter on record, not none and not two.
#[tokio::test]
async fn faults_delay_a_letter_and_never_duplicate_it() {
    let w = World::all().await;
    w.driver.directory.set_contact(
        "aroha",
        Channel::Email,
        Contact {
            name: "Aroha Ngata".into(),
            destination: "aroha@example.nz".into(),
            format: Some("braille".into()),
            ..Contact::default()
        },
    );
    w.complaints("intake", intake("c1", "aroha")).await;

    w.driver.directory.set_down(true);
    let pass = w.issuer.pass().await;
    assert_eq!((pass.issued, pass.left, pass.errors.len()), (0, 0, 1));
    assert!(
        pass.errors[0].starts_with("complaint-ack:c1: the documents module is unavailable"),
        "{:?}",
        pass.errors
    );
    w.driver.directory.set_down(false);

    w.issuer.complaints.inner.set_down(true);
    let pass = w.issuer.pass().await;
    assert!(pass.errors[0].starts_with("letters due: "), "{:?}", pass.errors);
    w.issuer.complaints.inner.set_down(false);

    // Issued, but the record did not reach the complaints module.
    *w.issuer.complaints.fail_next_command.lock().unwrap() = true;
    let pass = w.issuer.pass().await;
    assert_eq!((pass.issued, pass.errors.len()), (0, 1), "{:?}", pass.errors);
    assert!(pass.errors[0].contains("no leader"), "{:?}", pass.errors);
    let document = w.driver.document("complaint-ack:c1").await.unwrap();
    assert_eq!(document.format, None, "the template has no braille version");
    assert_eq!(w.issuer.complaints.inner.read(|s| s.complaints["c1"].acknowledged_on), None);

    assert_eq!(w.issuer.pass().await.issued, 1);
    assert_eq!(w.driver.module.state().documents.len(), 1);
    assert_eq!(
        w.issuer.complaints.inner.read(|s| s.complaints["c1"].acknowledgement.clone()).as_deref(),
        Some("complaint-ack:c1")
    );
}

/// Purpose: a letter someone recorded while the pass was running counts as
/// done; a refusal that means something else, and an answer that is not a
/// list of letters, are reported.
/// Why it matters: handlers and this service work the same register at the
/// same time. Losing that race is normal and must not raise an alarm, but
/// a real refusal must not be swallowed with it.
#[tokio::test]
async fn a_letter_recorded_by_someone_else_is_done() {
    let w = World::all().await;
    w.complaints("intake", intake("c1", "aroha")).await;
    let stale = w.issuer.complaints.inner.read(|s| s.letters_due(10));
    let by_hand = Cmd::RecordAcknowledgement {
        complaint: "c1".into(),
        document: "by-hand".into(),
    };
    w.complaints("complaints", by_hand).await;
    *w.issuer.complaints.fixed.lock().unwrap() = Some(serde_json::to_value(&stale).unwrap());
    assert_eq!(w.issuer.pass().await.issued, 1);
    assert_eq!(
        w.issuer.complaints.inner.read(|s| s.complaints["c1"].acknowledgement.clone()).as_deref(),
        Some("by-hand"),
        "the handler's record stands"
    );

    let unknown = LetterDue {
        kind: LetterKind::Acknowledgement,
        subject: "nope".into(),
        party: PartyId::from("aroha"),
        representatives: vec![],
        since: w.today(),
        due: None,
        detail: BTreeMap::new(),
    };
    assert_eq!(document_id(&unknown), "complaint-ack:nope");
    *w.issuer.complaints.fixed.lock().unwrap() = Some(serde_json::to_value(vec![unknown]).unwrap());
    let pass = w.issuer.pass().await;
    assert_eq!(pass.issued, 0);
    // The template needs a date the register did not give: the letter is
    // not written with a blank in it.
    assert!(pass.errors[0].starts_with("complaint-ack:nope: "), "{:?}", pass.errors);

    *w.issuer.complaints.fixed.lock().unwrap() = Some(json!({ "not": "a list" }));
    assert!(w.issuer.pass().await.errors[0].starts_with("letters due: "));
}

/// Purpose: the step runs a pass at once and then every interval until it
/// is told to stop.
/// Why it matters: this loop is what the deployed service runs. If it did
/// not stop when asked, a rolling update would wait out its grace period
/// on every pod.
#[tokio::test]
async fn the_step_runs_until_stopped() {
    let w = World::all().await;
    w.complaints("intake", intake("c1", "aroha")).await;
    w.complaints("intake", intake("c2", "stranger")).await;
    let World { issuer, driver, .. } = w;
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let seen = Arc::new(Mutex::new(vec![]));
    let record = seen.clone();
    let mut stop = Some(stop);
    run_letters(
        issuer,
        Duration::from_millis(5),
        async {
            let _ = stopped.await;
        },
        move |pass| {
            let mut seen = record.lock().unwrap();
            seen.push(pass.clone());
            if seen.len() == 3 {
                if let Some(stop) = stop.take() {
                    let _ = stop.send(());
                }
            }
        },
    )
    .await;
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 3);
    assert_eq!((seen[0].issued, seen[0].left), (1, 1));
    assert_eq!((seen[1].issued, seen[1].left), (0, 1));
    assert!(driver.document("complaint-ack:c1").await.is_ok());
}

/// Purpose: a progress update goes when the update timer reaches its
/// warning and nobody has written, numbered and recorded on the complaint;
/// it is not sent before the warning, not sent twice, and the same update
/// recorded again is refused.
/// Why it matters: complainants must hear how their complaint is going at
/// the interval the rules set. Missing one is a breach the regulator counts;
/// sending one early or twice is noise, and counting one twice would hide
/// the next missed update.
#[tokio::test]
async fn a_progress_update_goes_when_its_timer_warns() {
    let w = World::all().await;
    w.complaints("intake", intake("c1", "aroha")).await;
    assert_eq!(w.issuer.pass().await.issued, 1, "the acknowledgement goes first");

    // Updates are every ten days with two days' warning.
    w.tick(7).await;
    assert_eq!(w.issuer.pass().await, LetterPass::default());
    w.tick(8).await;
    let pass = w.issuer.pass().await;
    assert_eq!((pass.issued, pass.left, pass.errors), (1, 0, vec![]));
    let due = w.today() + chrono::Duration::days(10);
    assert_eq!(
        w.text("complaint-update:c1:1").await,
        format!(
            "Dear Aroha Ngata. This is update 1 on complaint c1. We will write again by {}.",
            due.format("%-d %B %Y")
        )
    );
    let complaint = w.issuer.complaints.inner.read(|s| s.complaints["c1"].clone());
    // The register's day is the latest it has seen: the tick's.
    assert_eq!(complaint.updates, vec![w.today() + chrono::Duration::days(8)]);
    assert_eq!(complaint.update_documents, vec!["complaint-update:c1:1".to_string()]);
    assert_eq!(w.issuer.pass().await, LetterPass::default());

    let again = w
        .issuer
        .complaints
        .inner
        .propose(Env {
            key: "again".into(),
            caller: "documents".into(),
            actor: "documents-service".into(),
            at: w.now,
            command: Cmd::RecordUpdate {
                complaint: "c1".into(),
                document: "complaint-update:c1:1".into(),
            },
        })
        .await
        .unwrap();
    assert_eq!(again.result.unwrap_err().code, "already_recorded");
}

/// Purpose: a written outcome is recorded as delivered once every addressee
/// has it, on the day the last did; one still on its way waits, one that
/// cannot be delivered or cannot be found is reported and left, and an
/// outage of the register is reported without stopping the letters.
/// Why it matters: the complaint cannot close, and the time to go to the
/// scheme does not start, until the outcome is proven delivered. A handler
/// should not have to copy that date from one module to another, and a
/// false proof would cut short the complainant's right to escalate.
#[tokio::test]
async fn a_written_outcome_is_proven_from_its_deliveries() {
    let w = World::all().await;
    w.driver.directory.set("rua", Channel::Email, "Rua Kahu", "rua@example.nz");
    w.complaints("intake", intake("c1", "aroha")).await;
    w.complaints("intake", intake("c2", "rua")).await;
    w.complaints("intake", intake("c3", "aroha")).await;
    assert_eq!(w.issuer.pass().await.issued, 3);
    // The acknowledgements stand in for the written outcomes.
    w.decide("c1", "complaint-ack:c1").await;
    w.decide("c2", "complaint-ack:c2").await;
    w.decide("c3", "nowhere").await;

    let pass = w.issuer.pass().await;
    assert_eq!(pass.reported, 0, "nothing is delivered yet");
    assert_eq!(pass.errors.len(), 1, "{:?}", pass.errors);
    assert!(pass.errors[0].starts_with("nowhere: "), "{:?}", pass.errors);

    w.driver.deliver().await.unwrap();
    use ic_documents::channels::DeliveryReport;
    use ic_documents::model::AttemptOutcome;
    let delivered = DeliveryReport {
        provider_reference: "ref-complaint-ack:c1".into(),
        outcome: AttemptOutcome::Delivered,
    };
    assert!(w.driver.report(delivered).await.unwrap());
    let failed = DeliveryReport {
        provider_reference: "ref-complaint-ack:c2".into(),
        outcome: AttemptOutcome::Failed {
            reason: "no such mailbox".into(),
        },
    };
    assert!(w.driver.report(failed).await.unwrap());
    w.driver.deliver().await.unwrap();

    *w.issuer.complaints.outcomes_down.lock().unwrap() = true;
    let pass = w.issuer.pass().await;
    assert!(pass.errors[0].starts_with("outcomes awaiting delivery: "), "{:?}", pass.errors);
    *w.issuer.complaints.outcomes_down.lock().unwrap() = false;

    let pass = w.issuer.pass().await;
    assert_eq!(pass.reported, 1, "{:?}", pass.errors);
    assert!(
        pass.errors
            .iter()
            .any(|e| e == "complaint-ack:c2: the outcome of c2 could not be delivered; left for the handler"),
        "{:?}",
        pass.errors
    );
    let delivered_on = w.issuer.complaints.inner.read(|s| s.complaints["c1"].outcome.clone().unwrap().delivered_on);
    assert_eq!(delivered_on, Some(w.today()));
    let pass = w.issuer.pass().await;
    assert_eq!(pass.reported, 0, "proof is recorded once");
}
