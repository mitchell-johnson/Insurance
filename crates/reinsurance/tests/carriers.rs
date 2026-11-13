//! The carriers for notices that leave the cluster: statements, listings and
//! the release log filed in write-once storage, and notices for staff on the
//! module's event hub.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use chrono::NaiveDate;
use common::*;
use ic_azure::blob::{BlobContainer, MemoryWormStore, WormStore};
use ic_azure::credential::AzureCredential;
use ic_azure::eventhubs::EventHubsSink;
use ic_azure::testing::{FakeAzure, TOKEN};
use ic_azure::AzureError;
use ic_reinsurance::carriers::{drop_container, kind, release_record, Carriers, MemoryDrops, Outbound, ReinsurerDrops, StaffFeed};
use ic_reinsurance::driver::{Audiences, DriverError, Handed};
use ic_reinsurance::machine::Audience;
use ic_reinsurance::{Command, Notice};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

fn outage() -> AzureError {
    AzureError::Status {
        service: "test",
        status: 503,
        body: "away".into(),
    }
}

/// A write-once store that can be away, or can lose sight of what it holds.
#[derive(Clone, Default)]
struct Shelf {
    kept: MemoryWormStore,
    down: Arc<AtomicBool>,
    blind: Arc<AtomicBool>,
    unreadable: Arc<AtomicBool>,
}

impl WormStore for Shelf {
    async fn put_new(&self, name: &str, content_type: &str, body: Vec<u8>) -> Result<(), AzureError> {
        if self.down.load(Ordering::SeqCst) {
            return Err(outage());
        }
        assert_eq!(content_type, "application/json");
        self.kept.put_new(name, content_type, body).await
    }

    async fn get(&self, name: &str) -> Result<Option<Vec<u8>>, AzureError> {
        if self.unreadable.load(Ordering::SeqCst) {
            return Err(outage());
        }
        if self.blind.load(Ordering::SeqCst) {
            return Ok(None);
        }
        self.kept.get(name).await
    }
}

/// A feed for staff that records what it is sent, and can be away.
#[derive(Clone, Default)]
struct Feed {
    sent: Arc<Mutex<Vec<(String, String, Value)>>>,
    down: Arc<AtomicBool>,
}

impl StaffFeed for Feed {
    async fn publish(&self, id: &str, kind: &str, body: Value) -> Result<(), AzureError> {
        if self.down.load(Ordering::SeqCst) {
            return Err(outage());
        }
        self.sent.lock().unwrap().push((id.to_string(), kind.to_string(), body));
        Ok(())
    }
}

/// The other modules, standing in for the cluster.
#[derive(Clone, Default)]
struct Modules {
    taken: Arc<Mutex<Vec<u64>>>,
}

impl Audiences for Modules {
    fn reached(&self) -> Vec<Audience> {
        vec![Audience::Group]
    }

    async fn hand_on(&self, seq: u64, _notice: &Notice, _on: NaiveDate) -> Result<Handed, DriverError> {
        self.taken.lock().unwrap().push(seq);
        Ok(Handed::Taken)
    }
}

fn carriers() -> Carriers<Modules, Shelf, Feed> {
    Carriers {
        modules: Modules::default(),
        records: Shelf::default(),
        staff: Feed::default(),
        outbound: ReinsurerDrops::Off,
    }
}

/// The notices a real statement raises: its release log entry and its document.
fn statement_notices() -> Vec<Notice> {
    let mut g = Bench::new();
    g.issue_scheme(2, "S1", d(2027, 2, 1), vec![member_cover("M1", 200 * K)]);
    g.premium_due(2, "S1", 1, d(2027, 2, 1), d(2027, 3, 1), &[("group_life", 100_00)]);
    g.today = d(2027, 3, 2);
    let cut = Command::CutStatement {
        treaty: group(),
        reinsurer: north(),
        period_end: d(2027, 2, 28),
    };
    let notices = g.ok("month-end", cut).notices;
    assert_eq!(notices.iter().map(kind).collect::<Vec<_>>(), vec!["release_logged", "document"]);
    notices
}

fn refusal(handed: Result<Handed, DriverError>) -> String {
    match handed {
        Ok(Handed::Refused(reject)) => reject.code,
        other => panic!("expected a refusal, not {other:?}"),
    }
}

/// Purpose: a statement's document is filed under the name the module gave it, byte for
/// byte, with the hash the statement records; the entry in the release log is filed under
/// the notice's own number with the date it was filed; and filing either again, on a later
/// day, is accepted and changes nothing.
/// Why it matters: the filed statement is what the insurer shows a reinsurer, an auditor or
/// a court as what was sent. A driver that died after writing and before acknowledging will
/// file it again; if that failed the notice would be set aside every time a pod restarted,
/// and if it overwrote, the record would no longer be evidence.
#[tokio::test]
async fn statements_and_the_release_log_are_filed_once() {
    let c = carriers();
    let notices = statement_notices();
    let (release, document) = (&notices[0], &notices[1]);
    let Notice::Document { name, sha256, body } = document else { unreachable!() };
    assert_eq!(name, "reinsurance/group-life-surplus/reinsurer-north/statement-1.json");
    assert_eq!(c.reached(), vec![Audience::Group, Audience::Records, Audience::Staff]);

    assert_eq!(c.hand_on(4, release, d(2027, 3, 2)).await, Ok(Handed::Taken));
    assert_eq!(c.hand_on(5, document, d(2027, 3, 2)).await, Ok(Handed::Taken));
    assert_eq!(c.records.kept.names(), vec![name.clone(), release_record(4)]);
    assert_eq!(release_record(4), "reinsurance/releases/000000000004.json");
    let filed = c.records.kept.get(name).await.unwrap().unwrap();
    assert_eq!(filed, body.as_bytes());
    assert_eq!(&hex::encode(Sha256::digest(&filed)), sha256);
    let logged: Value = serde_json::from_slice(&c.records.kept.get(&release_record(4)).await.unwrap().unwrap()).unwrap();
    assert_eq!(
        logged,
        json!({"seq": 4, "filed_on": "2027-03-02", "release": {"release_logged": {
            "subject": "statement 1", "treaty": "group-life-surplus", "reinsurer": "reinsurer-north",
            "purpose": "treaty statement and listing", "documents": [name],
        }}})
    );

    // Again the next day, as after a restart between writing and acknowledging.
    assert_eq!(c.hand_on(4, release, d(2027, 3, 3)).await, Ok(Handed::Taken));
    assert_eq!(c.hand_on(5, document, d(2027, 3, 3)).await, Ok(Handed::Taken));
    assert_eq!(c.records.kept.names().len(), 2);
    let again: Value = serde_json::from_slice(&c.records.kept.get(&release_record(4)).await.unwrap().unwrap()).unwrap();
    assert_eq!(again["filed_on"], "2027-03-02", "the first filing stands");
    assert!(c.modules.taken.lock().unwrap().is_empty() && c.staff.sent.lock().unwrap().is_empty());
}

/// Purpose: a record whose name is already filed with other contents is refused as a
/// conflict and the filed one is left alone; a document that does not have the hash it was
/// issued with is refused and never written; and while the store is away, or cannot show
/// what it holds, the notice is not refused but waits for the next pass.
/// Why it matters: a refusal sets the notice aside for a person, which is right when two
/// different statements claim one name and wrong for an outage: an hour without storage
/// must not turn every statement of a month-end into an exception to clear by hand. And a
/// conflict that was silently accepted would leave the insurer holding a filed statement
/// that is not the one it sent.
#[tokio::test]
async fn a_record_that_conflicts_is_refused_and_an_outage_only_waits() {
    let c = carriers();
    let notices = statement_notices();
    let (release, document) = (&notices[0], &notices[1]);
    let Notice::Document { name, sha256, body } = document else { unreachable!() };
    let on = d(2027, 3, 2);

    c.records.down.store(true, Ordering::SeqCst);
    assert!(matches!(c.hand_on(5, document, on).await, Err(DriverError::Unavailable(why)) if why.contains("503")));
    assert!(c.records.kept.names().is_empty());
    c.records.down.store(false, Ordering::SeqCst);

    let damaged = Notice::Document {
        name: name.clone(),
        sha256: sha256.clone(),
        body: format!("{body} "),
    };
    assert_eq!(refusal(c.hand_on(5, &damaged, on).await), "record_damaged");
    assert!(c.records.kept.names().is_empty());

    assert_eq!(c.hand_on(4, release, on).await, Ok(Handed::Taken));
    assert_eq!(c.hand_on(5, document, on).await, Ok(Handed::Taken));
    let other_body = body.replace("\"statement\":1", "\"statement\":2");
    assert_ne!(&other_body, body);
    let other = Notice::Document {
        name: name.clone(),
        sha256: hex::encode(Sha256::digest(other_body.as_bytes())),
        body: other_body,
    };
    assert_eq!(refusal(c.hand_on(6, &other, on).await), "record_conflict");
    assert_eq!(c.records.kept.get(name).await.unwrap().unwrap(), body.as_bytes());
    let other_release = Notice::ReleaseLogged {
        subject: "claim C-1".into(),
        treaty: group(),
        reinsurer: north(),
        purpose: "large claim notification".into(),
        documents: vec![],
    };
    assert_eq!(refusal(c.hand_on(4, &other_release, on).await), "record_conflict");
    // Something under the name that is not a release at all is a conflict too.
    c.records.kept.put_new(&release_record(9), "text/plain", b"not json".to_vec()).await.unwrap();
    assert_eq!(refusal(c.hand_on(9, release, on).await), "record_conflict");

    // The store says the name is taken but cannot show it: not a refusal.
    c.records.blind.store(true, Ordering::SeqCst);
    assert!(matches!(c.hand_on(5, document, on).await, Err(DriverError::Unavailable(why)) if why.contains("cannot be read")));
    c.records.unreadable.store(true, Ordering::SeqCst);
    assert!(matches!(c.hand_on(5, document, on).await, Err(DriverError::Unavailable(why)) if why.contains("503")));
}

/// Purpose: a notice for staff is published with an ID that is the same on every try, the
/// notice's kind, and a body carrying its number, the business date and the notice itself;
/// a feed that is away loses nothing; and notices for other modules go to the carrier for
/// modules untouched.
/// Why it matters: an exception nobody is told about is the failure this module exists to
/// prevent: a life over its retention with no reinsurance, found at claim time. The stable
/// ID is what lets a consumer drop the repeat a retry sends rather than raise two tasks.
#[tokio::test]
async fn staff_are_told_on_the_feed_and_modules_through_their_own_carrier() {
    let c = carriers();
    let on = d(2027, 3, 2);
    let opened = Notice::ExceptionOpened {
        id: "over_retention:life-1".into(),
        code: "over_retention".into(),
        detail: "life-1 holds 1,200,000 against a retention of 500,000".into(),
    };
    c.staff.down.store(true, Ordering::SeqCst);
    assert!(matches!(c.hand_on(9, &opened, on).await, Err(DriverError::Unavailable(_))));
    c.staff.down.store(false, Ordering::SeqCst);
    assert_eq!(c.hand_on(9, &opened, on).await, Ok(Handed::Taken));
    let due = Notice::SupplementaryListingDue { statement: 3 };
    assert_eq!(c.hand_on(10, &due, on).await, Ok(Handed::Taken));
    assert_eq!(
        *c.staff.sent.lock().unwrap(),
        vec![
            (
                "reinsurance-notice-9".to_string(),
                "exception_opened".to_string(),
                json!({"seq": 9, "on": "2027-03-02", "notice": {"exception_opened": {
                    "id": "over_retention:life-1", "code": "over_retention",
                    "detail": "life-1 holds 1,200,000 against a retention of 500,000"}}}),
            ),
            (
                "reinsurance-notice-10".to_string(),
                "supplementary_listing_due".to_string(),
                json!({"seq": 10, "on": "2027-03-02", "notice": {"supplementary_listing_due": {"statement": 3}}}),
            ),
        ]
    );

    let for_group = Notice::RequireUnderwrittenExcess {
        policy: "S1".into(),
        life: "M1".into(),
        cover: "group-life".into(),
        excess_minor: 1,
        reference: "r".into(),
    };
    assert_eq!(c.hand_on(11, &for_group, on).await, Ok(Handed::Taken));
    assert_eq!(*c.modules.taken.lock().unwrap(), vec![11]);
    assert_eq!(c.staff.sent.lock().unwrap().len(), 2);
    assert!(c.records.kept.names().is_empty());
}

/// Purpose: with the real Blob Storage and Event Hubs clients against a stand-in for
/// Azure, a statement lands in the records container under its name, a repeat is accepted
/// because the service refuses the second write and the stored copy matches, a staff notice
/// is accepted by the module's hub on partition 0, and an outage of either service leaves
/// the notice waiting.
/// Why it matters: the carriers are only as good as the calls the Azure clients make. The
/// "already exists" answer in particular comes from the service as an HTTP status; were it
/// read as a failure, every statement would be re-sent for ever after the first restart.
#[tokio::test]
async fn the_azure_clients_file_and_publish() {
    let fake = FakeAzure::start().await;
    let credential = AzureCredential::static_token(TOKEN);
    let c = Carriers {
        modules: Modules::default(),
        records: BlobContainer::new(&fake.base, "reinsurance-records", credential.clone()),
        staff: EventHubsSink::new(&fake.base, "reinsurance-events", credential.clone()),
        outbound: ReinsurerDrops::Azure {
            endpoint: fake.base.clone(),
            credential,
        },
    };
    let notices = statement_notices();
    let Notice::Document { name, body, .. } = &notices[1] else { unreachable!() };
    let on = d(2027, 3, 2);
    for _ in 0..2 {
        assert_eq!(c.hand_on(4, &notices[0], on).await, Ok(Handed::Taken));
        assert_eq!(c.hand_on(5, &notices[1], on).await, Ok(Handed::Taken));
    }
    let blobs = fake.blobs();
    assert_eq!(blobs.len(), 3);
    assert_eq!(blobs[&format!("reinsurance-records/{name}")], body.as_bytes());
    // The statement is also left in the reinsurer's own container, for it to collect.
    assert_eq!(blobs["ri-reinsurer-north/group-life-surplus/statement-1.json"], body.as_bytes());
    assert!(blobs.contains_key(&format!("reinsurance-records/{}", release_record(4))));

    let closed = Notice::ExceptionClosed {
        id: "over_retention:life-1".into(),
    };
    assert_eq!(c.hand_on(6, &closed, on).await, Ok(Handed::Taken));
    assert_eq!(fake.events_sent("reinsurance-events", 0), 1);

    fake.set_down(true);
    assert!(matches!(c.hand_on(7, &closed, on).await, Err(DriverError::Unavailable(_))));
    let Notice::Document { sha256, .. } = &notices[1] else { unreachable!() };
    let later = Notice::Document {
        name: "reinsurance/group-life-surplus/reinsurer-north/statement-2.json".into(),
        sha256: sha256.clone(),
        body: body.clone(),
    };
    assert!(matches!(c.hand_on(8, &later, on).await, Err(DriverError::Unavailable(_))));
    assert_eq!(fake.blobs().len(), 3);
    assert_eq!(fake.events_sent("reinsurance-events", 0), 1);
}

/// Purpose: once filed, a statement or listing part is also left in its reinsurer's own
/// container as `<treaty>/<file>`, once however often it is handed on; the release log and
/// other records are not; a different document under a name already left is refused; a
/// reinsurer whose ID cannot name a container is refused for a person; and with no SFTP account
/// configured nothing is left anywhere but the record.
/// Why it matters: this is how a reinsurer receives policy-level data. Each reinsurer must see
/// only its own files, a resend after a restart must not duplicate or alter what it has, and
/// the insurer's own release log must never leave.
#[tokio::test]
async fn statements_and_listings_are_left_for_their_reinsurer_to_collect() {
    let drops = MemoryDrops::default();
    let c = Carriers {
        modules: Modules::default(),
        records: Shelf::default(),
        staff: Feed::default(),
        outbound: drops.clone(),
    };
    let notices = statement_notices();
    let on = d(2027, 3, 2);
    for _ in 0..2 {
        for (seq, n) in notices.iter().enumerate() {
            assert_eq!(c.hand_on(seq as u64, n, on).await, Ok(Handed::Taken));
        }
    }
    let north = drops.store("ri-reinsurer-north");
    assert_eq!(north.names(), vec!["group-life-surplus/statement-1.json".to_string()]);
    let Notice::Document { sha256, body, .. } = &notices[1] else { unreachable!() };
    assert_eq!(
        north.get("group-life-surplus/statement-1.json").await.unwrap().as_deref(),
        Some(body.as_bytes())
    );
    assert!(drops.store("ri-reinsurer-south").names().is_empty());

    // Another document under a name the reinsurer already has.
    let other = r#"{"statement":1,"changed":true}"#.to_string();
    let rewritten = Notice::Document {
        name: "reinsurance/group-life-surplus/reinsurer-north/statement-1.json".into(),
        sha256: hex::encode(Sha256::digest(other.as_bytes())),
        body: other,
    };
    let fresh = Carriers {
        modules: Modules::default(),
        records: Shelf::default(),
        staff: Feed::default(),
        outbound: drops.clone(),
    };
    assert_eq!(refusal(fresh.hand_on(9, &rewritten, on).await), "record_conflict");

    // A reinsurer ID no container can be named after.
    assert_eq!(drop_container("reinsurer-north").as_deref(), Some("ri-reinsurer-north"));
    assert_eq!(drop_container("North_Re").as_deref(), Some("ri-north-re"));
    for bad in ["a--b", "b-", "x".repeat(61).as_str(), "a.b"] {
        assert_eq!(drop_container(bad), None, "{bad}");
    }
    let odd = Notice::Document {
        name: "reinsurance/group-life-surplus/a--b/statement-3.json".into(),
        sha256: sha256.clone(),
        body: body.clone(),
    };
    assert_eq!(refusal(c.hand_on(10, &odd, on).await), "no_drop_for_reinsurer");
    let release_only = Notice::Document {
        name: "reinsurance/releases/other.json".into(),
        sha256: sha256.clone(),
        body: body.clone(),
    };
    assert_eq!(c.hand_on(11, &release_only, on).await, Ok(Handed::Taken));
    let not_a_statement = Notice::Document {
        name: "reinsurance/group-life-surplus/reinsurer-north/notes.json".into(),
        sha256: sha256.clone(),
        body: body.clone(),
    };
    assert_eq!(c.hand_on(12, &not_a_statement, on).await, Ok(Handed::Taken));
    assert_eq!(drops.store("ri-reinsurer-north").names().len(), 1, "only statements and listings are left");
    assert!(ReinsurerDrops::Off.drop_for("ri-reinsurer-north").is_none());
}
