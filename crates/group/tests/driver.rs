//! The driver between the module and the world: an uploaded file from
//! quarantine to members in the core, and every outside party failing in
//! turn without anything being lost or done twice.

mod common;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use chrono::NaiveDate;
use common::*;
use ic_core::{Command, GroupId};
use ic_group::command::{caller, Envelope, GroupCommand, Resolution};
use ic_group::driver::{content_hash, Core, CustomerMaster, Driver, DriverError, FileStore, Module, Peers, Scan};
use ic_group::event::{ExceptionKind, GroupEvent, ReinsuranceNotice, UnderwritingRequest};
use ic_group::mapping::{Columns, FileKind, FileMapping, SalaryPeriod, SalaryUnit};
use ic_group::record::MemberIdentity;
use ic_group::state::{FileStatus, GroupState, Outcome};
use ic_group::Question;
use ic_sim::Cluster;
use serde_json::Value;

#[derive(Clone, Default)]
struct Flag(Arc<AtomicBool>);

impl Flag {
    fn set(&self, on: bool) {
        self.0.store(on, Ordering::SeqCst);
    }
    fn check(&self, what: &str) -> Result<(), String> {
        if self.0.load(Ordering::SeqCst) {
            Err(format!("{what} is down"))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone)]
struct Local {
    state: Arc<Mutex<GroupState>>,
    down: Flag,
}

impl Module for Local {
    async fn propose(&self, envelope: Envelope) -> Result<Outcome, DriverError> {
        self.down.check("module").map_err(DriverError::Unavailable)?;
        Ok(self.state.lock().unwrap().apply_envelope(envelope))
    }
    async fn ask(&self, question: Question) -> Result<Value, DriverError> {
        self.down.check("module").map_err(DriverError::Unavailable)?;
        ic_group::answer(&self.state.lock().unwrap(), serde_json::to_value(question).unwrap()).map_err(DriverError::Answer)
    }
}

#[derive(Clone)]
struct SimCore {
    cluster: Arc<Mutex<Cluster>>,
    down: Flag,
    sent: Arc<Mutex<Vec<String>>>,
}

impl Core for SimCore {
    async fn submit(&self, key: &str, group: &GroupId, command: &Command) -> Result<Option<String>, String> {
        self.down.check("core")?;
        self.sent.lock().unwrap().push(key.to_string());
        let mut c = self.cluster.lock().unwrap();
        let p = c.partition_of(group);
        Ok(c.submit_keyed(p, key, "group-admin", command.clone()).result.err().map(|e| e.code))
    }
    async fn business_date(&self) -> Result<Option<NaiveDate>, String> {
        self.down.check("core")?;
        Ok(self.cluster.lock().unwrap().business_date())
    }
}

#[derive(Clone, Default)]
struct Sink {
    down: Flag,
    underwriting_down: Flag,
    notices: Arc<Mutex<Vec<(String, ReinsuranceNotice)>>>,
    requests: Arc<Mutex<Vec<UnderwritingRequest>>>,
    events: Arc<Mutex<Vec<(u64, GroupEvent)>>>,
}

impl Peers for Sink {
    async fn reinsurance(&self, key: &str, _on: NaiveDate, notice: &ReinsuranceNotice) -> Result<(), String> {
        self.down.check("reinsurance")?;
        self.notices.lock().unwrap().push((key.to_string(), notice.clone()));
        Ok(())
    }
    async fn underwriting(&self, _key: &str, request: &UnderwritingRequest) -> Result<(), String> {
        self.down.check("underwriting")?;
        self.underwriting_down.check("underwriting")?;
        self.requests.lock().unwrap().push(request.clone());
        Ok(())
    }
    async fn publish(&self, seq: u64, _date: NaiveDate, event: &GroupEvent) -> Result<(), String> {
        self.down.check("events")?;
        self.events.lock().unwrap().push((seq, event.clone()));
        Ok(())
    }
}

#[derive(Clone, Default)]
struct Directory {
    down: Flag,
    asked: Arc<Mutex<Vec<MemberIdentity>>>,
}

impl CustomerMaster for Directory {
    async fn resolve(&self, _scheme: &str, identity: &MemberIdentity) -> Result<Resolution, String> {
        self.down.check("customer master")?;
        self.asked.lock().unwrap().push(identity.clone());
        Ok(if identity.family_name == "Ngata" {
            Resolution::Possible {
                candidates: vec![party("party-ngata-1")],
            }
        } else {
            Resolution::Created {
                party: party(&format!("party-{}", identity.employee_id)),
            }
        })
    }
}

#[derive(Clone, Default)]
struct Store {
    down: Flag,
    blobs: Arc<Mutex<BTreeMap<String, (Vec<u8>, Scan)>>>,
}

impl Store {
    fn mark(&self, scheme: &str, file: &str, scan: Scan) {
        self.blobs.lock().unwrap().get_mut(&format!("{scheme}/{file}")).unwrap().1 = scan;
    }
}

impl FileStore for Store {
    async fn put(&self, scheme: &str, file: &str, bytes: Vec<u8>) -> Result<(), String> {
        self.down.check("storage")?;
        self.blobs.lock().unwrap().entry(format!("{scheme}/{file}")).or_insert((bytes, Scan::Pending));
        Ok(())
    }
    async fn scan(&self, scheme: &str, file: &str) -> Result<Scan, String> {
        self.down.check("storage")?;
        Ok(self.blobs.lock().unwrap().get(&format!("{scheme}/{file}")).map_or(Scan::Pending, |b| b.1))
    }
    async fn read(&self, scheme: &str, file: &str) -> Result<Option<Vec<u8>>, String> {
        self.down.check("storage")?;
        Ok(self.blobs.lock().unwrap().get(&format!("{scheme}/{file}")).map(|b| b.0.clone()))
    }
}

fn named_mapping() -> FileMapping {
    FileMapping {
        id: "harbour-freight".to_string(),
        version: 2,
        kind: FileKind::Full,
        delimiter: ',',
        columns: Columns {
            employee_id: "Emp No".to_string(),
            given_name: Some("First".to_string()),
            family_name: Some("Last".to_string()),
            date_of_birth: Some("DOB".to_string()),
            category: Some("Category".to_string()),
            salary: Some("Salary".to_string()),
            billing_group: Some("Site".to_string()),
            ..Columns::default()
        },
        date_format: "%d/%m/%Y".to_string(),
        salary_unit: SalaryUnit::Major,
        salary_period: SalaryPeriod::Annual,
        categories: BTreeMap::new(),
        change_values: BTreeMap::new(),
        content_hash: String::new(),
    }
    .seal()
    .unwrap()
}

const FILE: &str = "Emp No,First,Last,DOB,Category,Salary,Site\n\
E001,Ana,Founder,01/05/1980,staff,60000,akl\n\
E002,Bo,Ngata,02/06/1985,staff,60000,wlg\n\
E003,Cy,Patel,03/07/1975,executive,240000,wlg\n\
E004,Di,Broken,not-a-date,staff,50000,akl\n";

type Rig = Driver<Local, SimCore, Sink, Directory, Store>;

/// The harness's scheme, with the module and the core behind the driver's ports.
fn rig() -> Rig {
    let mut h = Harness::new(terms());
    h.staff(GroupCommand::SetMapping {
        scheme: SCHEME.into(),
        mapping: Box::new(named_mapping()),
    })
    .unwrap();
    let module = Local {
        state: Arc::new(Mutex::new(h.g)),
        down: Flag::default(),
    };
    let core = SimCore {
        cluster: Arc::new(Mutex::new(h.c)),
        down: Flag::default(),
        sent: Arc::default(),
    };
    Driver::new(module, core, Sink::default(), Directory::default(), Store::default(), "test-salt")
}

/// Run the driver, and the host's event feed, until neither has anything to do.
async fn settle(d: &Rig) {
    for _ in 0..200 {
        let pass = d.run_once().await.unwrap();
        let records = d.core.cluster.lock().unwrap().take_events();
        let mut fed = 0;
        for env in records.iter().flat_map(ic_group::react) {
            fed += 1;
            assert!(d.module.state.lock().unwrap().apply_envelope(env).result.is_ok());
        }
        if pass.done == 0 && fed == 0 {
            return;
        }
    }
    panic!("the driver never settled");
}

fn file_status(d: &Rig, file: &str) -> FileStatus {
    d.module.state.lock().unwrap().schemes[SCHEME].files[file].status
}

async fn approve(d: &Rig, file: &str) {
    let today = d.core.business_date().await.unwrap().unwrap();
    let env = Envelope {
        key: format!("approve-{file}"),
        caller: caller::EMPLOYER.into(),
        actor: "admin-cy".into(),
        today,
        command: GroupCommand::ApproveFile {
            scheme: SCHEME.into(),
            file: file.into(),
        },
    };
    d.module.propose(env).await.unwrap().result.unwrap();
}

/// Purpose: an uploaded file is stored before the module hears of it, is not read until
/// the scan passes it, is mapped with the scheme's mapping, and after approval its joiners
/// are matched by name with the customer master and applied to the core; names go to the
/// customer master and nowhere else.
/// Why it matters: this is the path every member's cover arrives by. It is also the path
/// by which an outside party's file gets inside, and the one place personal details pass
/// through, so both the order of the steps and where the names end up are the control.
#[tokio::test]
async fn an_uploaded_file_goes_from_quarantine_to_the_core() {
    let d = rig();
    d.upload(caller::EMPLOYER, "admin-bo", SCHEME, "nov.csv", FILE.as_bytes().to_vec())
        .await
        .unwrap();
    settle(&d).await;
    assert_eq!(file_status(&d, "nov.csv"), FileStatus::Quarantined, "not read before the scan");
    d.files.mark(SCHEME, "nov.csv", Scan::Clean);
    settle(&d).await;
    assert_eq!(file_status(&d, "nov.csv"), FileStatus::AwaitingApproval);
    {
        let g = d.module.state.lock().unwrap();
        let f = &g.schemes[SCHEME].files["nov.csv"];
        assert_eq!((f.summary.joiners, f.summary.rejected), (2, 1));
        assert!(f
            .rejected
            .iter()
            .any(|r| r.employee_id.as_deref() == Some("E004") && r.field.as_deref() == Some("date_of_birth")));
    }
    assert!(d.directory.asked.lock().unwrap().is_empty(), "nobody is matched before the employer approves");

    approve(&d, "nov.csv").await;
    settle(&d).await;
    assert_eq!(file_status(&d, "nov.csv"), FileStatus::Applied);
    let asked: Vec<String> = d
        .directory
        .asked
        .lock()
        .unwrap()
        .iter()
        .map(|i| format!("{} {}", i.given_name, i.family_name))
        .collect();
    assert_eq!(asked, vec!["Bo Ngata", "Cy Patel"]);
    {
        let g = d.module.state.lock().unwrap();
        let s = &g.schemes[SCHEME];
        assert_eq!(s.membership.active("E003").unwrap().party, Some(party("party-E003")));
        assert_eq!(s.membership.active("E002").unwrap().party, None, "a possible match is not a link");
        assert!(g.exceptions.values().any(|e| e.kind == ExceptionKind::PossibleMatch));
        assert!(g.outbox.is_empty());
        // Nothing the module holds or sent out carries a name.
        let everything = format!(
            "{}{}{}",
            serde_json::to_string(&*g).unwrap(),
            serde_json::to_string(&*d.peers.events.lock().unwrap()).unwrap(),
            serde_json::to_string(&*d.peers.notices.lock().unwrap()).unwrap()
        );
        for name in ["Ngata", "Patel", "Founder", "Broken"] {
            assert!(!everything.contains(name), "{name} left the file");
        }
    }
    let c = d.core.cluster.lock().unwrap();
    assert_eq!(c.state(0).policies.values().next().unwrap().lives.len(), 3);
    drop(c);
    assert_eq!(d.peers.requests.lock().unwrap().len(), 1, "the executive's excess went to underwriting");
    assert!(d
        .peers
        .notices
        .lock()
        .unwrap()
        .iter()
        .any(|(_, n)| matches!(n, ReinsuranceNotice::NotePossibleMatch { .. })));
    let events = d.peers.events.lock().unwrap();
    assert!(events.iter().any(|(_, e)| matches!(e, GroupEvent::FileApplied { applied: 2, .. })));
    let mut seqs: Vec<u64> = events.iter().map(|(s, _)| *s).collect();
    let n = seqs.len();
    seqs.dedup();
    assert_eq!(seqs.len(), n, "each event published once");
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "in order");
}

/// Purpose: a file that fails the scan, was changed in storage after upload, has gone
/// missing, is not text, or lacks a mapped column is refused with a code that says which,
/// and the scheme is free for the next file.
/// Why it matters: each of these is a file that must never reach membership, and each
/// must tell the employer what to fix. A file altered between upload and reading is the
/// tampering case: the hash taken at upload is what the employer's approval refers to.
#[tokio::test]
async fn a_bad_file_is_refused_with_its_reason() {
    let d = rig();
    let refused = |d: &Rig, file: &str| d.module.state.lock().unwrap().schemes[SCHEME].files[file].code.clone();
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        ("virus.csv", FILE.as_bytes().to_vec(), "malware"),
        ("changed.csv", FILE.replace("60000", "60001").into_bytes(), "content_changed"),
        ("gone.csv", FILE.replace("60000", "60002").into_bytes(), "file_missing"),
        ("binary.csv", vec![0xff, 0xfe, 0x00, 0x41], "not_text"),
        ("columns.csv", b"Emp No,First\nE009,Zed\n".to_vec(), "missing_column"),
        ("empty.csv", vec![], "no_header"),
        ("twice.csv", b"Emp No,Emp No\nE009,E009\n".to_vec(), "duplicate_column"),
        ("quote.csv", b"Emp No,First\n\"E009,Zed\n".to_vec(), "unclosed_quote"),
    ];
    for (file, bytes, code) in cases {
        d.upload(caller::EMPLOYER, "admin-bo", SCHEME, file, bytes).await.unwrap();
        match code {
            "malware" => d.files.mark(SCHEME, file, Scan::Malicious),
            _ => d.files.mark(SCHEME, file, Scan::Clean),
        }
        if code == "content_changed" {
            d.files.blobs.lock().unwrap().get_mut(&format!("{SCHEME}/{file}")).unwrap().0 = FILE.replace("60000", "99999").into_bytes();
        }
        if code == "file_missing" {
            // The scan result is recorded, then the blob disappears.
            d.read_files().await.unwrap();
            d.files.blobs.lock().unwrap().remove(&format!("{SCHEME}/{file}"));
        }
        settle(&d).await;
        assert_eq!(file_status(&d, file), FileStatus::Refused, "{file}");
        assert_eq!(refused(&d, file).as_deref(), Some(code), "{file}");
    }
    // The same upload sent twice is one upload; other content under a used name is refused,
    // and so is a file for a scheme that is not there.
    d.upload(caller::EMPLOYER, "admin-bo", SCHEME, "virus.csv", FILE.as_bytes().to_vec())
        .await
        .unwrap();
    assert_eq!(file_status(&d, "virus.csv"), FileStatus::Refused);
    let other = d.upload(caller::EMPLOYER, "admin-bo", SCHEME, "virus.csv", b"Emp No\nE1\n".to_vec()).await;
    assert!(matches!(other, Err(DriverError::Refused(r)) if r.code == "file_exists"));
    let nowhere = d.upload(caller::EMPLOYER, "admin-bo", "nowhere", "a.csv", vec![1]).await;
    assert!(matches!(nowhere, Err(DriverError::Refused(r)) if r.code == "unknown_scheme"));
    assert_eq!(content_hash(b"abc").len(), 64);
}

/// Purpose: with storage, the customer master, the core, reinsurance, underwriting, the
/// event stream and the module itself each down in turn, the driver reports the step as
/// not done, leaves the work where it was, and finishes it exactly once when they return.
/// Why it matters: these are seven separate services and any of them can be away when a
/// file arrives. Work must wait in the module's outbox, never in the driver's memory, and
/// a repeat after recovery must not join a member or publish an event twice.
#[tokio::test]
async fn every_outside_party_can_fail_and_nothing_is_lost_or_repeated() {
    let d = rig();
    d.files.down.set(true);
    let up = d.upload(caller::EMPLOYER, "admin-bo", SCHEME, "nov.csv", FILE.as_bytes().to_vec()).await;
    assert!(matches!(up, Err(DriverError::Unavailable(_))));
    assert!(
        d.module.state.lock().unwrap().schemes[SCHEME].files.is_empty(),
        "no record of a file storage does not hold"
    );
    d.files.down.set(false);
    d.upload(caller::EMPLOYER, "admin-bo", SCHEME, "nov.csv", FILE.as_bytes().to_vec())
        .await
        .unwrap();

    // Storage away again: the scan cannot be read, then the file cannot.
    d.files.down.set(true);
    assert_eq!(d.read_files().await.unwrap().failed, 1);
    d.files.down.set(false);
    d.files.mark(SCHEME, "nov.csv", Scan::Clean);
    d.read_files().await.unwrap();
    d.files.down.set(true);
    assert_eq!(d.read_files().await.unwrap().failed, 1);
    assert_eq!(file_status(&d, "nov.csv"), FileStatus::Scanned);
    d.files.down.set(false);
    settle(&d).await;
    approve(&d, "nov.csv").await;

    // Everything outside is down. The outbox keeps all of it.
    d.directory.down.set(true);
    d.peers.down.set(true);
    let pass = d.deliver().await.unwrap();
    assert_eq!(pass.done, 0);
    assert!(pass.failed > 0);
    let waiting = d.module.state.lock().unwrap().outbox.len();
    assert_eq!(d.deliver().await.unwrap().failed, pass.failed);
    assert_eq!(d.module.state.lock().unwrap().outbox.len(), waiting);
    assert_eq!(file_status(&d, "nov.csv"), FileStatus::ResolvingParties);

    // The customer master returns; now the core is away, and with it the business date.
    d.directory.down.set(false);
    d.peers.down.set(false);
    d.core.down.set(true);
    assert!(matches!(d.deliver().await, Err(DriverError::Unavailable(_))));
    d.core.down.set(false);
    // The module is away: nothing can be recorded, so nothing is assumed.
    d.module.down.set(true);
    assert!(matches!(d.run_once().await, Err(DriverError::Unavailable(_))));
    d.module.down.set(false);

    settle(&d).await;
    assert_eq!(file_status(&d, "nov.csv"), FileStatus::Applied);
    let c = d.core.cluster.lock().unwrap();
    assert_eq!(c.state(0).policies.values().next().unwrap().lives.len(), 3);
    drop(c);
    let events = d.peers.events.lock().unwrap();
    let mut seqs: Vec<u64> = events.iter().map(|(s, _)| *s).collect();
    seqs.sort_unstable();
    let n = seqs.len();
    seqs.dedup();
    assert_eq!(seqs.len(), n, "no event published twice");
    assert_eq!(d.peers.requests.lock().unwrap().len(), 1);
    let sent = d.core.sent.lock().unwrap();
    let mut keys = sent.clone();
    keys.dedup();
    assert_eq!(*sent, keys, "no command sent to the core twice");
}

/// Purpose: a pass takes at most its batch from the outbox, and a file's joiners are
/// answered in parts when there are many.
/// Why it matters: an employer of ten thousand sends ten thousand rows. Commands and
/// passes must stay a bounded size or one large file stalls the module's group for
/// everyone.
#[tokio::test]
async fn large_work_is_taken_in_bounded_pieces() {
    let d = rig().with_batch(50);
    let mut file = String::from("Emp No,First,Last,DOB,Category,Salary,Site\nE001,Ana,Founder,01/05/1980,staff,60000,akl\n");
    for i in 0..1_100 {
        file.push_str(&format!("N{i:04},Given{i},Family{i},02/06/1985,staff,60000,wlg\n"));
    }
    d.upload(caller::EMPLOYER, "admin-bo", SCHEME, "big.csv", file.into_bytes()).await.unwrap();
    d.files.mark(SCHEME, "big.csv", Scan::Clean);
    let first = d.run_once().await.unwrap();
    assert!(first.done <= 1 + 50, "one file step and one batch: {first:?}");
    settle(&d).await;
    approve(&d, "big.csv").await;
    settle(&d).await;
    assert_eq!(file_status(&d, "big.csv"), FileStatus::Applied);
    assert_eq!(d.directory.asked.lock().unwrap().len(), 1_100);
    assert_eq!(d.module.state.lock().unwrap().schemes[SCHEME].membership.active_count(), 1_101);
    assert_eq!(d.core.cluster.lock().unwrap().state(0).policies.values().next().unwrap().lives.len(), 1_101);
}

/// Purpose: an outbox entry that cannot be delivered, here a request to an underwriting
/// service that is away for days, stays in the outbox and does not stop the entries
/// behind it, even when a pass takes only one entry at a time.
/// Why it matters: the outbox is one ordered list. If the oldest entry blocked the rest,
/// one peer being away would stop members' cover reaching the core.
#[tokio::test]
async fn a_stuck_entry_does_not_hold_up_the_rest() {
    let d = rig().with_batch(1);
    d.peers.underwriting_down.set(true);
    d.upload(caller::EMPLOYER, "admin-bo", SCHEME, "nov.csv", FILE.as_bytes().to_vec())
        .await
        .unwrap();
    d.files.mark(SCHEME, "nov.csv", Scan::Clean);
    settle(&d).await;
    approve(&d, "nov.csv").await;
    settle(&d).await;
    assert_eq!(file_status(&d, "nov.csv"), FileStatus::Applied);
    assert_eq!(d.module.state.lock().unwrap().outbox.len(), 1, "only the request to underwriting waits");
    assert_eq!(d.deliver().await.unwrap().failed, 1);
    // A later file goes through behind it.
    let next = FILE.replace("E003,Cy,Patel,03/07/1975,executive,240000", "E003,Cy,Patel,03/07/1975,executive,250000");
    d.upload(caller::EMPLOYER, "admin-bo", SCHEME, "dec.csv", next.into_bytes()).await.unwrap();
    d.files.mark(SCHEME, "dec.csv", Scan::Clean);
    settle(&d).await;
    approve(&d, "dec.csv").await;
    settle(&d).await;
    assert_eq!(file_status(&d, "dec.csv"), FileStatus::Applied);
    d.peers.underwriting_down.set(false);
    settle(&d).await;
    assert!(d.module.state.lock().unwrap().outbox.is_empty());
    assert_eq!(d.peers.requests.lock().unwrap().len(), 2);
}
