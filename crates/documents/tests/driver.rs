//! The driver end to end: issue, store, send, prove, publish, instruct the
//! core and destroy, with every outside dependency failing in turn.

#[path = "support/azure.rs"]
mod azure;
mod support;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};
use ic_documents::channels::{Channels, DeliveryReport, MemoryDirectory, Outbound, SendError};
use ic_documents::command::{Command, HoldScope, NoticeSpec};
use ic_documents::crypto::{self, CryptoError, LocalWrap};
use ic_documents::driver::{CoreLink, Driver, DriverError, EventHubsPublisher, EventPublisher, IssueRequest, LocalModule, MemoryPublisher, Module, Sends};
use ic_documents::model::{AttemptOutcome, Channel, CoreInstruction, DeliveryStatus, RecipientRole, RetentionBasis, RetentionRule};
use ic_documents::module::Query;
use ic_documents::vault::{MemoryStore, ObjectStore, VaultError};
use ic_documents::{DocumentKind, Event, Gate};
use serde_json::json;
use support::*;

/// Channels that do what a test tells them to and remember what they sent.
#[derive(Clone, Default)]
struct Scripted {
    sent: Arc<Mutex<Vec<Outbound>>>,
    fail: Arc<Mutex<BTreeMap<Channel, SendError>>>,
}

impl Scripted {
    fn fail(&self, channel: Channel, error: Option<SendError>) {
        let mut fail = self.fail.lock().unwrap();
        match error {
            Some(e) => fail.insert(channel, e),
            None => fail.remove(&channel),
        };
    }

    fn sent(&self) -> Vec<Outbound> {
        self.sent.lock().unwrap().clone()
    }
}

impl Channels for Scripted {
    async fn send(&self, message: &Outbound) -> Result<String, SendError> {
        if let Some(e) = self.fail.lock().unwrap().get(&message.channel) {
            return Err(e.clone());
        }
        self.sent.lock().unwrap().push(message.clone());
        Ok(format!("ref-{}-{}-{:?}", message.document, message.party, message.channel))
    }
}

/// A core that takes instructions, or refuses those for one policy.
#[derive(Clone, Default)]
struct Core {
    taken: Arc<Mutex<Vec<(String, CoreInstruction)>>>,
    refuse: Arc<Mutex<Option<String>>>,
}

impl CoreLink for Core {
    async fn instruct(&self, id: &str, instruction: &CoreInstruction) -> Result<(), String> {
        if self.refuse.lock().unwrap().as_ref().is_some_and(|p| id.ends_with(p.as_str())) {
            return Err("the partition has no leader".into());
        }
        self.taken.lock().unwrap().push((id.to_string(), instruction.clone()));
        Ok(())
    }
}

type TestDriver = Driver<LocalModule, MemoryStore, LocalWrap, Scripted, MemoryDirectory>;

struct World {
    driver: TestDriver,
    now: Arc<Mutex<DateTime<Utc>>>,
    publisher: MemoryPublisher,
    core: Core,
}

impl World {
    /// A module with the usual templates active, and a driver around it
    /// whose every dependency is in memory.
    async fn new() -> Self {
        let kit = Kit::new();
        let now = Arc::new(Mutex::new(kit.now));
        let module = LocalModule::default();
        // Templates only: the parties get their keys from the driver.
        let ready = Kit::ready();
        for envelope in ready.log.iter().filter(|e| !matches!(e.command, Command::RegisterPartyKey { .. })) {
            assert!(module.propose(envelope.clone()).await.unwrap().result.is_ok());
        }
        let directory = MemoryDirectory::default();
        directory.set("aroha", Channel::Email, "Aroha Ngata", "aroha@example.nz");
        directory.set("aroha", Channel::Post, "Aroha Ngata", "12 Kowhai Street\nWellington 6035");
        let clock = now.clone();
        let driver = Driver {
            module,
            objects: MemoryStore::default(),
            wrap: LocalWrap::new(crypto::new_key()),
            channels: Scripted::default(),
            directory,
            actor: "documents-driver".into(),
            clock: Arc::new(move || *clock.lock().unwrap()),
            batch: 100,
            composer: Default::default(),
        };
        Self {
            driver,
            now,
            publisher: MemoryPublisher::default(),
            core: Core::default(),
        }
    }

    fn advance(&self, duration: Duration) {
        *self.now.lock().unwrap() += duration;
    }

    fn request(&self, id: &str, template: &str) -> IssueRequest {
        IssueRequest {
            document: id.into(),
            caller: "documents".into(),
            template: template.into(),
            template_version: None,
            policy: Some(policy("pol-1")),
            other_policies: vec![],
            data_class: Default::default(),
            lives: vec![],
            prove_delivery: false,
            owner: party("aroha"),
            recipients: vec![recipient("aroha", RecipientRole::Addressee, &[Channel::Email])],
            language: "en".into(),
            format: None,
            data: sample_data(),
            trigger_event: None,
            case: None,
            supersedes: None,
            reason: None,
            wordings: vec![],
            components: vec![],
            notice: None,
            copy_of: None,
        }
    }

    fn renewal(&self, id: &str) -> IssueRequest {
        let mut request = self.request(id, "renewal");
        request.trigger_event = Some("7-100".into());
        request.notice = Some(NoticeSpec {
            purpose: "renewal".into(),
            corrects: None,
        });
        request
    }

    fn state(&self) -> ic_documents::DocumentsState {
        self.driver.module.state()
    }

    async fn cycle(&self) -> ic_documents::driver::Cycle {
        self.driver.cycle(&self.publisher, Some(&self.core)).await
    }
}

/// Purpose: one request renders, encrypts, stores and records a document;
/// the same request again issues nothing new; what is read back is what
/// was rendered, and re-rendering from the stored data gives the same text.
/// Why it matters: a schedule or notice must exist exactly once however
/// often a caller retries, and the insurer must be able to show years later
/// both what it sent and that the stored data and template explain it.
#[tokio::test]
async fn a_document_is_issued_once_and_can_be_read_and_verified() {
    let w = World::new().await;
    let doc = w.driver.issue(w.renewal("n1")).await.unwrap();
    assert_eq!(doc.kind, DocumentKind::RenewalNotice);
    assert_eq!(doc.language, "en");
    assert!(doc.proof);
    assert_eq!(w.driver.objects.names().len(), 1);
    // A mandatory notice is kept from issue, so storage was asked to lock it.
    assert_eq!(w.driver.objects.kept_until(&doc.blob), Some(doc.issued_on + Duration::days(2557)));

    let again = w.driver.issue(w.renewal("n1")).await.unwrap();
    assert_eq!(again, doc);
    assert_eq!(w.driver.objects.names().len(), 1);
    assert_eq!(w.state().party_keys.len(), 1);

    let contents = w.driver.read("n1").await.unwrap();
    assert_eq!(contents.text, "Dear Aroha. Your premium will be $123.45 from 15 January 2028.");
    assert_eq!(contents.data, sample_data());
    assert!(w.driver.verify("n1").await.unwrap());
    assert_eq!(w.driver.read("nothing").await, Err(DriverError::NotFound("document nothing".into())));

    // A language the template does not have is served in its primary one.
    let mut maori = w.request("r1", "receipt");
    maori.language = "mi".into();
    assert_eq!(w.driver.issue(maori).await.unwrap().language, "en");
    // A contractual document's period starts when the policy ends, which
    // is not known yet: storage is given no date.
    let mut schedule = w.request("s1", "schedule");
    schedule.template_version = Some(1);
    schedule.wordings = vec![tref("wording", 1)];
    let schedule = w.driver.issue(schedule).await.unwrap();
    assert_eq!(w.driver.objects.kept_until(&schedule.blob), None);
}

/// Purpose: a request that cannot be issued leaves nothing behind: no
/// document, and no readable object in the vault.
/// Why it matters: a refused or failed issue must not leave a stored
/// document nobody knows about, and must be safe to correct and send again.
#[tokio::test]
async fn a_request_that_cannot_be_issued_leaves_nothing_behind() {
    let w = World::new().await;

    assert_eq!(
        w.driver.issue(w.request("x1", "no-such-template")).await,
        Err(DriverError::NotFound("an active version of template no-such-template".into()))
    );
    let mut versioned = w.request("x2", "renewal");
    versioned.template_version = Some(9);
    assert_eq!(w.driver.issue(versioned).await, Err(DriverError::NotFound("template renewal version 9".into())));

    let mut missing_field = w.renewal("x3");
    missing_field.data = json!({"name": "Aroha"});
    assert!(matches!(w.driver.issue(missing_field).await, Err(DriverError::Template(_))));
    assert!(w.driver.objects.names().is_empty());

    // The module refuses it (a notice with no purpose): the object written
    // for it is removed again.
    let mut refused = w.renewal("x4");
    refused.notice = None;
    assert!(matches!(w.driver.issue(refused).await, Err(DriverError::Refused(_))));
    assert!(w.driver.objects.names().is_empty());
    assert!(w.state().documents.is_empty());

    // Storage is down: nothing is recorded, and the same request works later.
    w.driver.objects.set_down(true);
    assert!(matches!(w.driver.issue(w.renewal("x5")).await, Err(DriverError::Vault(VaultError::Storage(_)))));
    assert!(w.state().documents.is_empty());
    w.driver.objects.set_down(false);
    w.driver.issue(w.renewal("x5")).await.unwrap();

    // The module is down: nothing is known, and the same request works later.
    w.driver.module.set_down(true);
    assert!(matches!(w.driver.issue(w.renewal("x6")).await, Err(DriverError::Unavailable(_))));
    w.driver.module.set_down(false);
    w.driver.issue(w.renewal("x6")).await.unwrap();
    assert_eq!(w.state().documents.len(), 2);
}

/// Purpose: a notice is sent on the first channel with the contact point
/// fetched at that moment, its provider report proves delivery, the clock
/// starts, the core is told, and the module's events reach the stream once.
/// Why it matters: this is the whole path a renewal notice takes. Each
/// link is a separate process step that can be interrupted; together they
/// are what lets the insurer change a premium lawfully.
#[tokio::test]
async fn a_notice_is_sent_proven_and_the_core_is_told() {
    let w = World::new().await;
    w.driver.issue(w.renewal("n1")).await.unwrap();

    let cycle = w.cycle().await;
    assert_eq!(cycle.errors, Vec::<String>::new());
    assert_eq!(cycle.sends, Sends { sent: 1, ..Sends::default() });
    let sent = w.driver.channels.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        (sent[0].channel, sent[0].contact.destination.as_str(), sent[0].title.as_str()),
        (Channel::Email, "aroha@example.nz", "renewal")
    );
    assert!(sent[0].text.contains("$123.45"));
    let attempt = w.state().documents["n1"].deliveries[0].attempts[0].clone();
    assert_eq!(attempt.masked_destination.as_deref(), Some("a***@example.nz"));

    // Nothing new to send while the report is awaited.
    assert_eq!(w.cycle().await.sends, Sends::default());
    assert_eq!(w.driver.channels.sent().len(), 1);

    let report = DeliveryReport {
        provider_reference: "ref-n1-aroha-Email".into(),
        outcome: AttemptOutcome::Delivered,
    };
    assert!(w.driver.report(report.clone()).await.unwrap());
    // Reports arrive more than once, and for messages no longer in progress.
    assert!(!w.driver.report(report).await.unwrap());
    assert!(!w
        .driver
        .report(DeliveryReport {
            provider_reference: "unknown".into(),
            outcome: AttemptOutcome::Delivered
        })
        .await
        .unwrap());

    let today = ic_documents::state::nz_date(*w.now.lock().unwrap());
    assert!(matches!(
        w.state().documents["n1"].deliveries[0].status,
        DeliveryStatus::Delivered { channel: Channel::Email, .. }
    ));
    assert_eq!(
        w.state().notice_gate(&policy("pol-1"), "renewal", today),
        Gate::Waiting(ic_documents::Waiting::PeriodRunning {
            until: today + Duration::days(30)
        })
    );

    let cycle = w.cycle().await;
    assert_eq!(cycle.instructed, 1);
    let taken = w.core.taken.lock().unwrap().clone();
    assert_eq!(taken[0].0, "notice-delivery:n1:pol-1");
    assert!(matches!(&taken[0].1, CoreInstruction::RecordNoticeDelivery { purpose, delivered_on, .. } if purpose == "renewal" && *delivered_on == today));
    assert!(w.state().core_outbox.is_empty());

    let published = w.publisher.seen();
    assert!(published.iter().any(|e| matches!(e.event, Event::NoticeDelivered { .. })));
    let seqs: Vec<u64> = published.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, (1..=seqs.len() as u64).collect::<Vec<_>>(), "every event once, in order");
    assert_eq!(w.cycle().await.published, 0);
}

/// Purpose: a channel that refuses, or a party with no contact point on
/// it, moves the notice to the next channel at once; a channel or customer
/// master that is merely down leaves it waiting; and post is the last
/// resort.
/// Why it matters: a mandatory notice must reach the customer by some
/// provable route. Giving up a channel too early costs money and days;
/// never giving it up leaves the customer without their notice.
#[tokio::test]
async fn a_failed_channel_falls_back_and_an_outage_waits() {
    let w = World::new().await;
    let mut request = w.renewal("n1");
    request.recipients = vec![recipient("aroha", RecipientRole::Addressee, &[Channel::Sms, Channel::Email])];
    w.driver.issue(request).await.unwrap();

    // No mobile number on file: straight to email.
    assert_eq!(
        w.cycle().await.sends,
        Sends {
            no_contact_point: 1,
            ..Sends::default()
        }
    );
    // Email is down, then the customer master is: the notice waits.
    w.driver.channels.fail(Channel::Email, Some(SendError::Temporary("throttled".into())));
    assert_eq!(
        w.cycle().await.sends,
        Sends {
            deferred: 1,
            ..Sends::default()
        }
    );
    w.driver.directory.set_down(true);
    assert_eq!(
        w.cycle().await.sends,
        Sends {
            deferred: 1,
            ..Sends::default()
        }
    );
    w.driver.directory.set_down(false);
    assert_eq!(w.state().documents["n1"].deliveries[0].attempts.len(), 2);
    // Email refuses the address: on to post, which the module added itself.
    w.driver.channels.fail(Channel::Email, Some(SendError::Permanent("address rejected".into())));
    assert_eq!(
        w.cycle().await.sends,
        Sends {
            refused: 1,
            ..Sends::default()
        }
    );
    assert_eq!(w.cycle().await.sends, Sends { sent: 1, ..Sends::default() });
    let sent = w.driver.channels.sent();
    assert_eq!((sent.len(), sent[0].channel), (1, Channel::Post));
    let attempts = &w.state().documents["n1"].deliveries[0].attempts;
    assert_eq!(attempts[0].outcome, Some(AttemptOutcome::NoContactPoint));
    assert_eq!(
        attempts[1].outcome,
        Some(AttemptOutcome::Failed {
            reason: "address rejected".into()
        })
    );
    assert_eq!(attempts[2].masked_destination.as_deref(), Some("*** Wellington 6035"));

    // Post is deemed delivered after the set days, by the passing of time alone.
    w.advance(Duration::days(6));
    w.cycle().await;
    assert!(matches!(
        w.state().documents["n1"].deliveries[0].status,
        DeliveryStatus::Delivered { channel: Channel::Post, .. }
    ));
}

/// Purpose: an in-app document needs no contact point and no send, and
/// while the module cannot be reached nothing is sent at all.
/// Why it matters: the inbox is the default channel for every app user and
/// must not depend on the customer master. And a message sent while its
/// record could not be written would be sent again later as a duplicate.
#[tokio::test]
async fn the_inbox_needs_no_contact_point_and_nothing_is_sent_unrecorded() {
    let w = World::new().await;
    let mut request = w.request("r1", "receipt");
    request.recipients = vec![recipient("aroha", RecipientRole::Addressee, &[Channel::InApp])];
    w.driver.issue(request).await.unwrap();
    assert_eq!(w.cycle().await.sends.sent, 1);
    assert_eq!(w.driver.channels.sent()[0].contact.destination, "");
    assert_eq!(w.state().documents["r1"].deliveries[0].attempts[0].masked_destination.as_deref(), Some("app"));

    w.driver.issue(w.request("r2", "receipt")).await.unwrap();
    w.driver.module.set_down(true);
    assert!(matches!(w.driver.deliver().await, Err(DriverError::Unavailable(_))));
    assert_eq!(w.driver.channels.sent().len(), 1);
    w.driver.module.set_down(false);
    assert_eq!(w.driver.deliver().await.unwrap().sent, 1);
    let sent = w.driver.channels.sent();
    assert_eq!(sent.iter().filter(|m| m.document == "r2").count(), 1);
    assert_eq!(sent[1].attempt, 1);
}

/// Purpose: when a document's retention ends its key is dropped and its
/// vault object deleted, an object storage still locks is left for later
/// without failing, and a held document is untouched.
/// Why it matters: keeping personal documents past their period breaks the
/// privacy law; destroying one under a legal hold destroys evidence. And
/// the deletion must finish even though storage and the module are
/// different systems that fail separately.
#[tokio::test]
async fn expired_documents_are_destroyed_and_held_ones_kept() {
    let w = World::new().await;
    let rules: BTreeMap<_, _> = DocumentKind::ALL
        .iter()
        .map(|k| {
            (
                *k,
                RetentionRule {
                    basis: RetentionBasis::Issue,
                    days: 30,
                },
            )
        })
        .collect();
    let module = &w.driver.module;
    let envelope = |key: &str, caller: &str, command| ic_documents::Envelope {
        key: key.into(),
        caller: caller.into(),
        actor: "ops".into(),
        at: *w.now.lock().unwrap(),
        command,
    };
    assert!(module
        .propose(envelope("ret", "config", Command::ActivateRetention { version: 1, rules }))
        .await
        .unwrap()
        .result
        .is_ok());

    let kept = w.driver.issue(w.request("r1", "receipt")).await.unwrap();
    let held = w.driver.issue(w.request("r2", "receipt")).await.unwrap();
    assert!(module
        .propose(envelope(
            "hold",
            "legal",
            Command::PlaceLegalHold {
                scope: HoldScope::Document("r2".into()),
                case: "lit-1".into()
            }
        ))
        .await
        .unwrap()
        .result
        .is_ok());

    w.advance(Duration::days(40));
    // Storage is down when the deletion is first tried.
    w.driver.objects.set_down(true);
    let cycle = w.cycle().await;
    assert!(cycle.errors.iter().any(|e| e.starts_with("delete: ")), "{:?}", cycle.errors);
    assert_eq!(w.state().pending_deletes.len(), 1);
    w.driver.objects.set_down(false);
    let cycle = w.cycle().await;
    assert_eq!(cycle.deleted, 1);
    assert!(w.state().pending_deletes.is_empty());
    assert_eq!(w.driver.objects.names(), vec![held.blob.clone()]);
    assert_eq!(w.driver.read("r1").await, Err(DriverError::Vault(VaultError::Destroyed)));
    assert_eq!(w.driver.read("r2").await.unwrap().text, "We received $123.45. Thank you.");
    assert!(!w.driver.objects.names().contains(&kept.blob));
    assert_eq!(w.cycle().await.deleted, 0);
}

/// Purpose: storage that still locks an object does not stop the
/// destruction of others, and a person's erasure makes their documents
/// unreadable even while the objects remain.
/// Why it matters: erasure cannot wait for a storage lock set years ago.
/// Dropping the key is what makes it immediate; the leftover object is
/// noise that is swept up when the lock ends.
#[tokio::test]
async fn a_locked_object_waits_and_an_erased_person_is_unreadable() {
    struct Locking(MemoryStore);
    impl ObjectStore for Locking {
        async fn put_new(&self, name: &str, body: Vec<u8>, keep_until: Option<chrono::NaiveDate>) -> Result<(), VaultError> {
            self.0.put_new(name, body, keep_until).await
        }
        async fn get(&self, name: &str) -> Result<Option<Vec<u8>>, VaultError> {
            self.0.get(name).await
        }
        async fn delete(&self, name: &str) -> Result<(), VaultError> {
            Err(VaultError::Locked(name.to_string()))
        }
    }
    let w = World::new().await;
    let World { driver, now, .. } = w;
    let driver = Driver {
        module: driver.module,
        objects: Locking(driver.objects),
        wrap: driver.wrap,
        channels: driver.channels,
        directory: driver.directory,
        actor: driver.actor,
        clock: driver.clock,
        batch: driver.batch,
        composer: Default::default(),
    };
    let request = World::new().await.request("r1", "receipt");
    driver.issue(request).await.unwrap();
    assert!(driver.read("r1").await.is_ok());

    let shred = ic_documents::Envelope {
        key: "shred".into(),
        caller: "privacy".into(),
        actor: "privacy-officer".into(),
        at: *now.lock().unwrap(),
        command: Command::ShredParty { party: party("aroha") },
    };
    assert!(driver.module.propose(shred).await.unwrap().result.is_ok());
    assert_eq!(driver.read("r1").await, Err(DriverError::Vault(VaultError::Destroyed)));
    assert!(driver.module.state().party_keys.is_empty());
    assert_eq!(driver.delete_destroyed().await.unwrap(), 0);
    assert_eq!(driver.module.state().pending_deletes.len(), 1, "the object is deleted when its lock ends");
    assert_eq!(driver.objects.0.names().len(), 1);
}

/// Purpose: when the stream, the core or the module is unavailable the
/// cycle says so, does what it still can, loses nothing, and finishes the
/// work once they return.
/// Why it matters: these are four systems that fail independently. A
/// cycle that stopped at the first failure would let an Event Hubs outage
/// stop notices being sent; one that dropped work would lose a proof of
/// delivery the core is waiting for.
#[tokio::test]
async fn an_outage_of_any_dependency_delays_and_loses_nothing() {
    let w = World::new().await;
    w.driver.issue(w.renewal("n1")).await.unwrap();
    let mut second = w.renewal("n2");
    second.policy = Some(policy("pol-2"));
    w.driver.issue(second).await.unwrap();

    w.publisher.set_down(true);
    let cycle = w.cycle().await;
    assert_eq!(cycle.sends.sent, 2, "the stream being down does not stop delivery");
    assert_eq!(cycle.errors.len(), 1);
    assert!(cycle.errors[0].starts_with("publish: "));
    assert!(w.publisher.seen().is_empty());

    for doc in ["n1", "n2"] {
        let reference = format!("ref-{doc}-aroha-Email");
        assert!(w
            .driver
            .report(DeliveryReport {
                provider_reference: reference,
                outcome: AttemptOutcome::Delivered
            })
            .await
            .unwrap());
    }
    // The core takes one instruction and not the other.
    *w.core.refuse.lock().unwrap() = Some("pol-2".into());
    let cycle = w.cycle().await;
    assert!(cycle.errors.iter().any(|e| e.starts_with("core: ")));
    assert_eq!(w.core.taken.lock().unwrap().len(), 1);
    assert_eq!(w.state().core_outbox.len(), 1, "the refused instruction is kept");

    w.driver.module.set_down(true);
    let cycle = w.cycle().await;
    assert_eq!(cycle.errors.len(), 5, "{:?}", cycle.errors);
    assert!(matches!(
        w.driver
            .report(DeliveryReport {
                provider_reference: "x".into(),
                outcome: AttemptOutcome::Delivered
            })
            .await,
        Err(DriverError::Unavailable(_))
    ));
    w.driver.module.set_down(false);

    *w.core.refuse.lock().unwrap() = None;
    w.publisher.set_down(false);
    w.advance(Duration::seconds(1));
    let cycle = w.cycle().await;
    assert_eq!(cycle.errors, Vec::<String>::new());
    assert_eq!(cycle.instructed, 1);
    assert!(w.state().core_outbox.is_empty());
    let seqs: Vec<u64> = w.publisher.seen().iter().map(|e| e.seq).collect();
    assert_eq!(seqs, (1..=seqs.len() as u64).collect::<Vec<_>>());
    // Without a link to the core, instructions simply wait.
    let cycle = w.driver.cycle(&w.publisher, None::<&Core>).await;
    assert_eq!((cycle.instructed, cycle.errors.len()), (0, 0));
}

/// Purpose: a stored document that no longer matches what was issued fails
/// verification or cannot be read, and the owner's key service failing is
/// reported as that.
/// Why it matters: verification is the control that shows the vault still
/// holds what customers were sent. It has to fail loudly on a changed
/// object, and must not mistake a Key Vault outage for a damaged document.
#[tokio::test]
async fn verification_detects_a_changed_document() {
    let w = World::new().await;
    let doc = w.driver.issue(w.renewal("n1")).await.unwrap();
    assert!(w.driver.verify("n1").await.unwrap());

    // A legacy document has no template to render again.
    let legacy = ic_documents::command::LegacyImport {
        document: "old-1".into(),
        kind: DocumentKind::PolicySchedule,
        policy: Some(policy("pol-1")),
        data_class: Default::default(),
        owner: party("aroha"),
        issued_on: date("2019-03-01"),
        language: "en".into(),
        content_sha256: doc.content_sha256.clone(),
        blob: doc.blob.clone(),
        wrapped_key: doc.wrapped_key.clone().unwrap(),
        bytes: doc.bytes,
    };
    let envelope = ic_documents::Envelope {
        key: "legacy".into(),
        caller: "migration".into(),
        actor: "migration".into(),
        at: *w.now.lock().unwrap(),
        command: Command::ImportLegacy(Box::new(legacy)),
    };
    assert!(w.driver.module.propose(envelope).await.unwrap().result.is_ok());
    // Its key was wrapped for another document ID, so it cannot be opened.
    assert_eq!(
        w.driver.verify("old-1").await,
        Err(DriverError::Vault(VaultError::Crypto(CryptoError::Unreadable)))
    );

    assert_eq!(
        w.driver.pdf("old-1", &ic_documents::pdf::Fonts::bundled()).await,
        Err(DriverError::Vault(VaultError::Crypto(CryptoError::Unreadable)))
    );

    w.driver.objects.corrupt(&doc.blob);
    assert_eq!(
        w.driver.verify("n1").await,
        Err(DriverError::Vault(VaultError::Crypto(CryptoError::Unreadable)))
    );

    let other = Driver {
        module: w.driver.module.clone(),
        objects: w.driver.objects.clone(),
        wrap: LocalWrap::new(crypto::new_key()),
        channels: Scripted::default(),
        directory: MemoryDirectory::default(),
        actor: "other".into(),
        clock: w.driver.clock.clone(),
        batch: 10,
        composer: Default::default(),
    };
    assert_eq!(other.read("n1").await, Err(DriverError::Crypto(CryptoError::Unreadable)));
}

/// Purpose: the module's events are put on Event Hubs as one ordered
/// batch with the properties consumers de-duplicate on, and a refusal or
/// an unreachable namespace is an error that leaves them unpublished.
/// Why it matters: claims, the portal and complaints learn that a notice
/// was delivered or found defective only from this stream. An event marked
/// published that never arrived is a consumer that never finds out.
#[tokio::test]
async fn events_are_published_to_event_hubs() {
    let fake = azure::Fake::start().await;
    let w = World::new().await;
    w.driver.issue(w.renewal("n1")).await.unwrap();
    let hub = EventHubsPublisher::new(&format!("{}/eh/", fake.base), "documents-events", azure::Fake::credential());

    fake.force("eventhubs", 503);
    assert_eq!(w.driver.publish(&hub).await, Err(DriverError::Unavailable("Event Hubs returned 503".into())));
    fake.force("eventhubs", 0);
    let pending: Vec<ic_documents::EventRecord> = serde_json::from_value(w.driver.module.ask(Query::PendingEvents { limit: 500 }).await.unwrap()).unwrap();
    let published = w.driver.publish(&hub).await.unwrap();
    assert_eq!(published as usize, pending.len());
    {
        let inner = fake.lock();
        assert_eq!(inner.events.len(), pending.len());
        assert_eq!(inner.events[0]["UserProperties"]["event-id"], json!(pending[0].id));
        let body: ic_documents::EventRecord = serde_json::from_str(inner.events[0]["Body"].as_str().unwrap()).unwrap();
        assert_eq!(body, pending[0]);
    }
    assert_eq!(w.driver.publish(&hub).await.unwrap(), 0);

    let nowhere = EventHubsPublisher::new("http://127.0.0.1:1", "h", azure::Fake::credential());
    assert!(nowhere.publish(&pending).await.is_err());
}

/// Purpose: a document from a template composed of a shared part is
/// rendered with the part's wording by the driver, and verifies against
/// the stored data years later using the same part version.
/// Why it matters: shared wording such as the cooling-off paragraph is in
/// most customer documents. If the driver rendered without the parts, or
/// verified against a different version, every such document would be
/// wrong or would fail its own integrity check.
#[tokio::test]
async fn a_template_with_a_shared_part_is_rendered_and_verified_with_it() {
    let w = World::new().await;
    let mut kit = Kit::ready();
    let before = kit.log.len();
    kit.ok(
        "config",
        "author",
        Command::RegisterPart(ic_documents::template::Part {
            id: "cooling-off".into(),
            version: 1,
            bodies: std::collections::BTreeMap::from([("en".to_string(), "You may cancel within 14 days.".to_string())]),
        }),
    );
    let mut welcome = template("welcome", DocumentKind::PolicyWording, "Welcome {{name}}. {{> cooling-off}}");
    welcome.parts.push(ic_documents::template::PartRef {
        id: "cooling-off".into(),
        version: 1,
    });
    kit.activate(welcome, &sample_data());
    for envelope in &kit.log[before..] {
        assert!(w.driver.module.propose(envelope.clone()).await.unwrap().result.is_ok());
    }

    w.driver.issue(w.request("w1", "welcome")).await.unwrap();
    assert_eq!(w.driver.read("w1").await.unwrap().text, "Welcome Aroha. You may cancel within 14 days.");
    assert!(w.driver.verify("w1").await.unwrap());
}

/// Purpose: an issued document can be fetched as a PDF of the text on
/// record, in large print when it was issued in that format, and a
/// document that does not exist is reported as not found.
/// Why it matters: this is the copy a customer downloads from the portal.
/// It must be the stored text, not a fresh rendering from today's data,
/// and a customer who asked for large print must get it every time.
#[tokio::test]
async fn an_issued_document_can_be_fetched_as_a_pdf() {
    let w = World::new().await;
    let fonts = ic_documents::pdf::Fonts::bundled();
    w.driver.issue(w.renewal("n1")).await.unwrap();
    let file = w.driver.pdf("n1", &fonts).await.unwrap();
    assert!(file.starts_with(b"%PDF-"));
    assert_eq!(w.driver.pdf("n1", &fonts).await.unwrap(), file);
    let title = w.driver.read("n1").await.unwrap();
    assert!(!title.text.is_empty());

    let mut large = w.renewal("n2");
    large.format = Some(ic_documents::template::AlternateFormat::LargePrint);
    w.driver.issue(large).await.unwrap();
    let bigger = w.driver.pdf("n2", &fonts).await.unwrap();
    assert_ne!(bigger, file);
    // The text is set at 18 point, which the content stream says.
    let has_size = |file: &[u8], size: &str| {
        use std::io::Read;
        let start = file.windows(7).position(|w| w == b"stream\n").unwrap() + 7;
        let end = start + file[start..].windows(10).position(|w| w == b"\nendstream").unwrap();
        let mut content = String::new();
        flate2::read::ZlibDecoder::new(&file[start..end]).read_to_string(&mut content).unwrap();
        content.contains(&format!("/F1 {size} Tf"))
    };
    assert!(has_size(&bigger, "18") && !has_size(&file, "18") && has_size(&file, "11"));

    assert_eq!(w.driver.pdf("nope", &fonts).await, Err(DriverError::NotFound("document nope".into())));
}

/// Purpose: nothing is sent on any channel to a person the customer master
/// says has died; each channel is recorded as having no contact point.
/// Why it matters: a lapse notice or a renewal addressed to someone who
/// has died reaches a grieving family. The document must instead show as
/// undelivered so that staff send it to whoever acts for the estate.
#[tokio::test]
async fn nothing_is_sent_to_a_person_who_has_died() {
    let w = World::new().await;
    for (channel, destination) in [(Channel::Email, "aroha@example.nz"), (Channel::Post, "12 Kowhai Street\nWellington 6035")] {
        w.driver.directory.set_contact(
            "aroha",
            channel,
            ic_documents::channels::Contact {
                name: "Aroha Ngata".into(),
                destination: destination.into(),
                deceased: true,
                ..Default::default()
            },
        );
    }
    let mut request = w.renewal("n1");
    request.recipients = vec![recipient("aroha", RecipientRole::Addressee, &[Channel::Email])];
    w.driver.issue(request).await.unwrap();
    let mut total = Sends::default();
    for _ in 0..3 {
        let sends = w.cycle().await.sends;
        total.no_contact_point += sends.no_contact_point;
        total.sent += sends.sent;
    }
    assert_eq!((total.sent, total.no_contact_point), (0, 2), "email, then post, and no more");
    assert!(w.driver.channels.sent().is_empty());
    let attempts = &w.state().documents["n1"].deliveries[0].attempts;
    assert!(attempts.iter().all(|a| a.outcome == Some(AttemptOutcome::NoContactPoint)));
}
