//! The driver between the module and the world: an uploaded file from
//! quarantine to members in the core, and every outside party failing in
//! turn without anything being lost or done twice.

mod common;
/// The customer master's own test harness: the real service over PostgreSQL.
// It reads the clock to date its tokens, which this crate forbids only for the state machine.
#[allow(clippy::disallowed_methods)]
#[path = "../../customers/tests/common/mod.rs"]
mod customer_master;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::NaiveDate;
use common::*;
use ic_core::{Command, GroupId};
use ic_group::command::{caller, Envelope, ExcessDecision, GroupCommand, MatchDecision, Resolution};
use ic_group::driver::{content_hash, Answers, Core, CustomerMaster, Driver, DriverError, ExcessAnswer, FileStore, Module, Peers, Scan};
use ic_group::event::{CommissionLayer, ContinuationExcess, ExceptionKind, GroupEvent, IssueOutcome, MemberLetter, ReinsuranceNotice, UnderwritingRequest};
use ic_group::mapping::{Columns, FileKind, FileMapping, SalaryPeriod, SalaryUnit};
use ic_group::record::MemberIdentity;
use ic_group::state::{CaseStatus, FileStatus, GroupState, Outcome};
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
    withdrawals: Arc<Mutex<Vec<String>>>,
    outcomes: Arc<Mutex<Vec<(String, IssueOutcome)>>>,
    /// Underwriting's outbox: numbered effects, of which some answer us.
    effects: Arc<Mutex<Vec<(u64, Option<ExcessAnswer>)>>>,
    events: Arc<Mutex<Vec<(u64, GroupEvent)>>>,
    /// Layers written in the commission module, and the answers it gives
    /// next (written when there are none left).
    layers: Arc<Mutex<Vec<(String, CommissionLayer)>>>,
    commission_answers: Arc<Mutex<std::collections::VecDeque<Result<Option<String>, String>>>>,
    /// Letters the documents module took, and the answers it gives next
    /// (taken when there are none left).
    letters: Arc<Mutex<Vec<MemberLetter>>>,
    letter_answers: Arc<Mutex<std::collections::VecDeque<Result<Option<String>, String>>>>,
    /// Continuations asked of underwriting, the answers it gives to the
    /// asking, and the outcomes waiting in its outbox.
    continuations: Arc<Mutex<Vec<ContinuationExcess>>>,
    continuation_answers: Arc<Mutex<std::collections::VecDeque<Result<Option<String>, String>>>>,
    continued: Arc<Mutex<Vec<(String, String, u64, String)>>>,
}

impl Peers for Sink {
    async fn continue_excess(&self, _today: NaiveDate, request: &ContinuationExcess) -> Result<Option<String>, String> {
        let answer = self.continuation_answers.lock().unwrap().pop_front().unwrap_or(Ok(None));
        if answer == Ok(None) {
            self.continuations.lock().unwrap().push(request.clone());
        }
        answer
    }
    async fn letter(&self, letter: &MemberLetter) -> Result<Option<String>, String> {
        let answer = self.letter_answers.lock().unwrap().pop_front().unwrap_or(Ok(None));
        if answer == Ok(None) {
            self.letters.lock().unwrap().push(letter.clone());
        }
        answer
    }
    async fn commission(&self, key: &str, layer: &CommissionLayer) -> Result<Option<String>, String> {
        let answer = self.commission_answers.lock().unwrap().pop_front().unwrap_or(Ok(None));
        if answer == Ok(None) {
            self.layers.lock().unwrap().push((key.to_string(), layer.clone()));
        }
        answer
    }
    async fn reinsurance(&self, key: &str, _on: NaiveDate, notice: &ReinsuranceNotice) -> Result<(), String> {
        self.down.check("reinsurance")?;
        self.notices.lock().unwrap().push((key.to_string(), notice.clone()));
        Ok(())
    }
    async fn underwriting(&self, _key: &str, _today: NaiveDate, request: &UnderwritingRequest) -> Result<(), String> {
        self.down.check("underwriting")?;
        self.underwriting_down.check("underwriting")?;
        self.requests.lock().unwrap().push(request.clone());
        Ok(())
    }
    async fn withdraw_underwriting(&self, _key: &str, _today: NaiveDate, request_key: &str) -> Result<(), String> {
        self.down.check("underwriting")?;
        self.underwriting_down.check("underwriting")?;
        self.withdrawals.lock().unwrap().push(request_key.to_string());
        Ok(())
    }
    async fn underwriting_outcome(&self, _key: &str, _today: NaiveDate, request_key: &str, outcome: &IssueOutcome) -> Result<(), String> {
        self.down.check("underwriting")?;
        self.underwriting_down.check("underwriting")?;
        self.outcomes.lock().unwrap().push((request_key.to_string(), outcome.clone()));
        Ok(())
    }
    async fn underwriting_answers(&self, after: u64, max: usize) -> Result<Answers, String> {
        self.down.check("underwriting")?;
        self.underwriting_down.check("underwriting")?;
        let page: Vec<_> = self.effects.lock().unwrap().iter().filter(|(n, _)| *n > after).take(max).cloned().collect();
        Ok(Answers {
            through: page.last().map_or(after, |(n, _)| *n),
            more: page.len() >= max,
            answers: page.into_iter().filter_map(|(_, a)| a).collect(),
            continued: std::mem::take(&mut *self.continued.lock().unwrap()),
        })
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

type RigWith<D> = Driver<Local, SimCore, Sink, D, Store>;
type Rig = RigWith<Directory>;

/// The harness's scheme, with the module and the core behind the driver's ports.
fn rig() -> Rig {
    rig_with(Directory::default(), named_mapping())
}

fn rig_with<D: CustomerMaster>(directory: D, mapping: FileMapping) -> RigWith<D> {
    let mut h = Harness::new(terms());
    h.staff(GroupCommand::SetMapping {
        scheme: SCHEME.into(),
        mapping: Box::new(mapping),
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
    Driver::new(module, core, Sink::default(), directory, Store::default(), "test-salt")
}

/// Run the driver, and the host's event feed, until neither has anything to do.
async fn settle<D: CustomerMaster>(d: &RigWith<D>) {
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

fn file_status<D: CustomerMaster>(d: &RigWith<D>, file: &str) -> FileStatus {
    d.module.state.lock().unwrap().schemes[SCHEME].files[file].status
}

async fn approve<D: CustomerMaster>(d: &RigWith<D>, file: &str) {
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
        (
            "wide.csv",
            format!("Emp No,First\n{}\n", vec!["x"; 201].join(",")).into_bytes(),
            "too_many_fields",
        ),
        ("long.csv", "x\n".repeat(250_002).into_bytes(), "too_many_rows"),
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

async fn through<D: CustomerMaster>(d: &RigWith<D>, name: &str, content: &str) {
    d.upload(caller::EMPLOYER, "admin-bo", SCHEME, name, content.as_bytes().to_vec()).await.unwrap();
    d.files.mark(SCHEME, name, Scan::Clean);
    settle(d).await;
    approve(d, name).await;
    settle(d).await;
    assert_eq!(file_status(d, name), FileStatus::Applied);
}

fn case_status(d: &Rig, case: u64) -> CaseStatus {
    d.module.state.lock().unwrap().schemes[SCHEME].cases[&case].status
}

/// Purpose: a request for the excess of a member the customer master could not identify
/// is neither sent nor counted as a failure, holds up nothing behind it, and goes with the
/// person's ID as soon as staff say who the member is.
/// Why it matters: underwriting asks a person for a health disclosure. Sending it a member
/// who may be one of two people would put one person's answers on another's record
/// (addendum entry 2); dropping the request would leave the member under-covered for good.
#[tokio::test]
async fn a_request_waits_until_the_member_is_a_known_person() {
    let d = rig().with_batch(1);
    // Ngata, whom the customer master cannot place, is the executive here.
    let file = FILE
        .replace("E002,Bo,Ngata,02/06/1985,staff,60000", "E002,Bo,Ngata,02/06/1985,executive,240000")
        .replace("E003,Cy,Patel,03/07/1975,executive,240000", "E003,Cy,Patel,03/07/1975,staff,60000");
    through(&d, "nov.csv", &file).await;
    assert!(d.peers.requests.lock().unwrap().is_empty());
    {
        let g = d.module.state.lock().unwrap();
        assert_eq!(g.outbox.len(), 1, "only the request waits: {:?}", g.outbox);
    }
    assert_eq!(d.deliver().await.unwrap(), Default::default(), "waiting is not a failure");

    let today = d.core.business_date().await.unwrap().unwrap();
    let decide = Envelope {
        key: "match-e002".into(),
        caller: caller::STAFF.into(),
        actor: "staff-ana".into(),
        today,
        command: GroupCommand::ResolveMatch {
            scheme: SCHEME.into(),
            employee_id: "E002".into(),
            decision: MatchDecision::SamePerson { party: party("party-ngata-1") },
        },
    };
    d.module.propose(decide).await.unwrap().result.unwrap();
    settle(&d).await;
    let requests = d.peers.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        (requests[0].party.clone(), requests[0].life.clone(), requests[0].cover_type.as_str()),
        (Some(party("party-ngata-1")), life("E002"), "group_life")
    );
    assert_eq!(
        (requests[0].date_of_birth, requests[0].policyholder.clone(), requests[0].product_code.as_str()),
        (
            NaiveDate::from_ymd_opt(1985, 6, 2),
            d.module.state.lock().unwrap().schemes[SCHEME].employer.clone(),
            "group-life"
        )
    );
    assert!(d.module.state.lock().unwrap().outbox.is_empty());
}

/// Purpose: underwriting's answers are read from its outbox in pages, each becomes a
/// decision on its case exactly once however often it is read, effects that are not
/// answers to this module are passed over, an answer for a case already closed is left
/// alone without stopping the ones behind it, and underwriting being away is a pass that
/// waits.
/// Why it matters: the answer is the only way accepted cover reaches the member's policy.
/// Read twice it must not write two layers; unreadable it must not be skipped; and one
/// stale answer must not strand every later member's decision behind it.
#[tokio::test]
async fn underwriting_answers_become_decisions_exactly_once() {
    let d = rig().with_batch(1);
    through(&d, "nov.csv", FILE).await;
    let request = d.peers.requests.lock().unwrap()[0].clone();
    let (case, excess) = (request.case, request.excess_minor);
    let answer = |key: &str, decision: ExcessDecision| {
        Some(ExcessAnswer {
            key: key.to_string(),
            scheme: SCHEME.to_string(),
            case,
            decision,
        })
    };
    let accepted = ExcessDecision::Accepted {
        sum_insured_minor: excess,
        loadings: vec![],
        exclusions: vec!["EX-BACK".into()],
        member_consents_to_itemising: false,
        member_pays_loading: false,
    };
    *d.peers.effects.lock().unwrap() = vec![
        (1, None),
        (2, answer("grp-uw:harbour:1/terms", accepted)),
        (3, None),
        // Too late: the case was decided by the answer before it.
        (4, answer("grp-uw:harbour:1/ended", ExcessDecision::Declined)),
    ];

    d.peers.underwriting_down.set(true);
    let waiting = d.collect_answers().await.unwrap();
    assert_eq!((waiting.done, waiting.failed), (0, 1));
    assert_eq!(case_status(&d, case), CaseStatus::Requested);
    d.peers.underwriting_down.set(false);

    settle(&d).await;
    assert_eq!(case_status(&d, case), CaseStatus::Accepted);
    let layers = |d: &Rig| {
        let c = d.core.cluster.lock().unwrap();
        let policy = c.state(0).policies.values().next().unwrap();
        let underwritten = |l: &&ic_core::policy::Layer| l.origin == ic_core::LayerOrigin::UnderwrittenExcess && l.sum_insured_minor == excess;
        policy.covers.values().flat_map(|c| c.layers.iter()).filter(underwritten).count()
    };
    assert_eq!(layers(&d), 1);
    // Underwriting hears which layer the core wrote, under the proposal's name.
    {
        let outcomes = d.peers.outcomes.lock().unwrap();
        assert_eq!(outcomes.len(), 1, "{outcomes:?}");
        assert_eq!(outcomes[0].0, format!("grp-uw:{SCHEME}:{case}"));
        assert!(matches!(&outcomes[0].1, IssueOutcome::Issued { life: l, cover, .. } if *l == request.life && *cover == request.cover));
    }
    assert_eq!(d.collect_answers().await.unwrap(), Default::default(), "nothing is read twice by one driver");

    // A driver started afresh reads everything again and changes nothing.
    let again = Driver::new(
        d.module.clone(),
        d.core.clone(),
        d.peers.clone(),
        Directory::default(),
        Store::default(),
        "test-salt",
    );
    assert_eq!(again.collect_answers().await.unwrap().failed, 0);
    settle(&again).await;
    assert_eq!((case_status(&again, case), layers(&again)), (CaseStatus::Accepted, 1));
    assert_eq!(again.peers.outcomes.lock().unwrap().len(), 1);
}

/// Purpose: when the cover a request was for is no longer wanted, a request underwriting
/// already has is withdrawn there, and one that never left is simply dropped.
/// Why it matters: an open proposal keeps chasing the member for a health disclosure. A
/// member whose salary fell back under the automatic amount, or who has left, must not be
/// asked for medical details for cover that will never be written.
#[tokio::test]
async fn a_request_no_longer_wanted_is_withdrawn_or_never_sent() {
    let lower = FILE.replace("executive,240000", "executive,150000");

    // Sent, then not wanted: withdrawn under the name it was sent by.
    let d = rig();
    through(&d, "nov.csv", FILE).await;
    let case = d.peers.requests.lock().unwrap()[0].case;
    through(&d, "dec.csv", &lower).await;
    assert_eq!(case_status(&d, case), CaseStatus::NotProceeded);
    assert_eq!(*d.peers.withdrawals.lock().unwrap(), vec![format!("grp-uw:{SCHEME}:{case}")]);
    assert!(d.module.state.lock().unwrap().outbox.is_empty());

    // Never sent: nothing goes to underwriting at all.
    let d = rig();
    d.peers.underwriting_down.set(true);
    through(&d, "nov.csv", FILE).await;
    through(&d, "dec.csv", &lower).await;
    d.peers.underwriting_down.set(false);
    settle(&d).await;
    assert!(d.peers.requests.lock().unwrap().is_empty() && d.peers.withdrawals.lock().unwrap().is_empty());
    assert!(d.module.state.lock().unwrap().outbox.is_empty());
}

/// Purpose: with the real customer master service behind the driver, over HTTP with a
/// signed Entra ID token and PostgreSQL underneath, a file's joiners are matched as the
/// addendum requires: a joiner whose email is a known person's is linked to that person, one found only
/// by her mobile number is put to staff as a possible match and not created again,
/// a namesake with nothing stronger than a name and a birth date is left unlinked as a
/// possible match for a person to decide, and someone unknown is created there and
/// linked; a retried file asks again and gets the same people.
/// Why it matters: the two services were built apart against a shape agreed in prose.
/// This is the only test in which one actually answers the other, and a wrong link here
/// joins two people's cover in accumulation and shows one person's records to another.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn joiners_are_matched_by_the_real_customer_master() {
    use ic_group::service::HttpCustomerMaster;

    let Some(world) = customer_master::world().await else { return };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let routes = world.router.clone();
    tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });

    let known = world
        .person("Bo", "Ngata", "1985-06-02", &[("email", serde_json::json!("bo.ngata@example.test"))])
        .await;
    let namesake = world.person("Cy", "Patel", "1975-07-03", &[]).await;
    let by_phone = world
        .person("Fay", "Lim", "1992-09-05", &[("mobile_phone", serde_json::json!("+64215550134"))])
        .await;

    let credential = ic_azure::credential::AzureCredential::static_token(&world.token("group-admin"));
    let client = HttpCustomerMaster::new(&base, customer_master::AUDIENCE, credential);
    let mut mapping = named_mapping();
    mapping.columns.email = Some("Email".to_string());
    mapping.columns.mobile_phone = Some("Mobile".to_string());
    mapping.content_hash = String::new();
    let d = rig_with(client, mapping.seal().unwrap());

    let file = "Emp No,First,Last,DOB,Category,Salary,Site,Email,Mobile\n\
E001,Ana,Founder,01/05/1980,staff,60000,akl,,\n\
E002,Bo,Ngata,02/06/1985,staff,60000,wlg,bo.ngata@example.test,\n\
E003,Cy,Patel,03/07/1975,staff,60000,wlg,,\n\
E005,Eru,Walker,04/08/1990,staff,60000,akl,eru.walker@example.test,\n\
E006,Fay,Tane,05/09/1992,staff,60000,akl,,021 555 0134\n";
    through(&d, "nov.csv", file).await;

    let party_of = |id: &str| d.module.state.lock().unwrap().schemes[SCHEME].membership.active(id).unwrap().party.clone();
    assert_eq!(party_of("E002"), Some(party(&known)), "the known email links");
    assert_eq!(party_of("E003"), None, "a name and a birth date are not enough to link");
    // Fay's surname has changed since the customer master last heard of her. Her mobile,
    // written for dialling at home, still finds her; a mobile is not enough to link on, so
    // she is put to staff as a possible match and not created a second time.
    assert_eq!(party_of("E006"), None, "a mobile alone does not link");
    assert_eq!(
        d.module.state.lock().unwrap().schemes[SCHEME].possible.get("E006"),
        Some(&vec![party(&by_phone)])
    );
    {
        let g = d.module.state.lock().unwrap();
        assert_eq!(g.schemes[SCHEME].possible.get("E003"), Some(&vec![party(&namesake)]));
        assert!(g.exceptions.values().any(|e| e.kind == ExceptionKind::PossibleMatch && e.subject == "E003"));
    }
    let created = party_of("E005").expect("someone unknown is created and linked");
    assert!(created != party(&known) && created != party(&namesake));
    // The person is really there, under the name the file gave.
    let (status, record) = world.get(&format!("/v1/parties/{}", created.0), "staff", "servicing").await;
    assert_eq!(status, 200, "{record}");
    assert!(record.to_string().contains("Walker"), "{record}");

    // Asked again for the same joiners, the customer master gives the same people.
    let again = HttpCustomerMaster::new(
        &base,
        customer_master::AUDIENCE,
        ic_azure::credential::AzureCredential::static_token(&world.token("group-admin")),
    );
    let eru = MemberIdentity {
        row: 5,
        employee_id: "E005".into(),
        given_name: "Eru".into(),
        family_name: "Walker".into(),
        date_of_birth: NaiveDate::from_ymd_opt(1990, 8, 4),
        email: Some("eru.walker@example.test".into()),
        national_id: None,
        mobile_phone: None,
    };
    let answer = again.resolve(SCHEME, &eru).await.unwrap();
    assert!(
        matches!(&answer, Resolution::Linked { party: p } | Resolution::Created { party: p } if *p == created),
        "{answer:?}"
    );
    // A caller without the role is refused, and the error does not carry the person's details.
    let stranger = HttpCustomerMaster::new(
        &base,
        customer_master::AUDIENCE,
        ic_azure::credential::AzureCredential::static_token(&world.token("documents")),
    );
    let refused = stranger.resolve(SCHEME, &eru).await.unwrap_err();
    assert!(refused.contains("403") && !refused.contains("Walker"), "{refused}");
}

const GROUP_AUDIENCE: &str = "api://group-schemes";

/// The service behind a stand-in Entra ID tenant, and that tenant's address.
async fn served(d: Arc<Rig>) -> (String, String) {
    let authority = customer_master::tenant().await;
    let entra = ic_group::auth::Entra::new(&authority, customer_master::TENANT, GROUP_AUDIENCE);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(ic_group::service::serve(
        listener,
        ic_group::service::router(d, entra, Default::default()),
        std::future::pending(),
    ));
    (base, authority)
}

/// Purpose: the service's routes take an upload, a command and a question only with an
/// Entra ID token issued for this service, and only under a caller name the token carries
/// as a role; an upload is stored and quarantined under the uploader's name; a command
/// is applied on the core's business date under a key that cannot collide with the
/// driver's; refusals come back with the module's own code; and nobody outside can send
/// the driver's commands, read the outbox, or announce a file without uploading it.
/// Why it matters: these routes are how the employer portal, staff screens and the other
/// services reach the module. The driver's commands (scan results, core answers) are what
/// make the module trust a file, so they must not be reachable from a portal.
#[tokio::test]
async fn the_service_routes_admit_only_what_a_portal_may_do() {
    let d = Arc::new(rig());
    let (base, authority) = served(d.clone()).await;
    let token = |roles: &[&str]| customer_master::token_with(&authority, roles, GROUP_AUDIENCE);
    // A service holding every caller name the routes take, so that what follows tests the
    // routes' own rules; what a narrower token may do is tested at the end.
    let all = token(&[
        "group-admin",
        "employer-portal",
        "member-portal",
        "underwriting",
        "reinsurance",
        "cash-matching",
        "group-driver",
    ]);
    let http = reqwest::Client::new();
    let post = |path: &str, key: Option<&str>, headers: &[(&str, &str)], body: Vec<u8>| {
        let mut r = http.post(format!("{base}{path}")).body(body);
        if let Some(key) = key {
            r = r.bearer_auth(key);
        }
        for (k, v) in headers {
            r = r.header(*k, *v);
        }
        async move {
            let resp = r.send().await.unwrap();
            let status = resp.status().as_u16();
            (status, resp.json::<Value>().await.unwrap_or(Value::Null))
        }
    };
    let who = [("x-ic-caller", "employer-portal"), ("x-ic-actor", "admin-bo")];
    let upload = "/v1/schemes/harbour/files/nov.csv";
    assert_eq!(http.get(format!("{base}/healthz")).send().await.unwrap().text().await.unwrap(), "ok");
    // Ready only while the cluster answers; no token is needed to ask.
    assert_eq!(http.get(format!("{base}/readyz")).send().await.unwrap().status().as_u16(), 200);
    d.core.down.set(true);
    assert_eq!(http.get(format!("{base}/readyz")).send().await.unwrap().status().as_u16(), 503);
    d.core.down.set(false);
    let metrics = http.get(format!("{base}/metrics")).send().await.unwrap().text().await.unwrap();
    assert!(metrics.contains("ic_group_passes_total 0\n"), "{metrics}");

    // The token: missing, not a token, for another service, or from another tenant.
    let elsewhere = customer_master::token_with(&authority, &["group-admin"], "api://customer-master");
    let forged = customer_master::token_with("https://login.example.test", &["group-admin"], GROUP_AUDIENCE);
    for key in [None, Some("wrong-key"), Some(""), Some(elsewhere.as_str()), Some(forged.as_str())] {
        assert_eq!(post(upload, key, &who, FILE.into()).await.0, 401);
        assert_eq!(post("/v1/commands", key, &[], b"{}".to_vec()).await.0, 401);
        assert_eq!(post("/v1/questions", key, &[], b"{}".to_vec()).await.0, 401);
    }
    // Uploads.
    assert_eq!(post(upload, Some(all.as_str()), &[], FILE.into()).await.1["error"]["code"], "missing_identity");
    let member = [("x-ic-caller", "member-portal"), ("x-ic-actor", "party-9")];
    assert_eq!(post(upload, Some(all.as_str()), &member, FILE.into()).await.0, 403);
    assert_eq!(post(upload, Some(all.as_str()), &who, vec![]).await.1["error"]["code"], "empty_file");
    assert_eq!(
        post("/v1/schemes/nowhere/files/a.csv", Some(all.as_str()), &who, FILE.into()).await,
        (
            422,
            serde_json::json!({"error": {"code": "unknown_scheme", "message": "scheme nowhere is not registered"}})
        )
    );
    let (status, body) = post(upload, Some(all.as_str()), &who, FILE.into()).await;
    assert_eq!((status, body["status"].as_str()), (202, Some("quarantined")));
    assert_eq!(d.module.state.lock().unwrap().schemes[SCHEME].files["nov.csv"].uploaded_by, "admin-bo");
    d.files.down.set(true);
    assert_eq!(post("/v1/schemes/harbour/files/dec.csv", Some(all.as_str()), &who, FILE.into()).await.0, 503);
    d.files.down.set(false);

    // Commands.
    let command = |caller: &str, key: &str, command: Value| {
        serde_json::to_vec(&serde_json::json!({"key": key, "caller": caller, "actor": "staff-ana", "command": command})).unwrap()
    };
    let freeze = serde_json::json!({"freeze": {"scheme": SCHEME, "reason": "review"}});
    assert_eq!(
        post("/v1/commands", Some(all.as_str()), &[], b"not json".to_vec()).await.1["error"]["code"],
        "invalid_command"
    );
    assert_eq!(
        post(
            "/v1/commands",
            Some(all.as_str()),
            &[],
            command("group-driver", "k1", serde_json::json!("tick"))
        )
        .await
        .0,
        403
    );
    let scan = serde_json::json!({"record_scan": {"scheme": SCHEME, "file": "nov.csv", "clean": true}});
    assert_eq!(
        post("/v1/commands", Some(all.as_str()), &[], command("group-admin", "k1", scan)).await.1["error"]["code"],
        "caller_not_allowed"
    );
    let receive = serde_json::json!({"receive_file": {"scheme": SCHEME, "file": "x.csv", "content_hash": "h"}});
    assert_eq!(
        post("/v1/commands", Some(all.as_str()), &[], command("employer-portal", "k1", receive)).await.1["error"]["code"],
        "use_upload"
    );
    assert_eq!(
        post("/v1/commands", Some(all.as_str()), &[], command("group-admin", "", freeze.clone()))
            .await
            .0,
        403
    );
    assert_eq!(
        post("/v1/commands", Some(all.as_str()), &[], command("group-admin", "k2", freeze.clone())).await,
        (200, serde_json::json!({"applied": true, "duplicate": false}))
    );
    assert_eq!(
        post("/v1/commands", Some(all.as_str()), &[], command("group-admin", "k2", freeze)).await.1["duplicate"],
        true
    );
    let unfreeze = serde_json::json!({"unfreeze": {"scheme": "nowhere"}});
    assert_eq!(
        post("/v1/commands", Some(all.as_str()), &[], command("group-admin", "k3", unfreeze)).await.0,
        422
    );
    d.core.down.set(true);
    let again = serde_json::json!({"unfreeze": {"scheme": SCHEME}});
    assert_eq!(
        post("/v1/commands", Some(all.as_str()), &[], command("group-admin", "k4", again.clone()))
            .await
            .0,
        503
    );
    d.core.down.set(false);
    d.module.down.set(true);
    assert_eq!(
        post("/v1/commands", Some(all.as_str()), &[], command("group-admin", "k4", again.clone()))
            .await
            .0,
        503
    );
    assert_eq!(
        post("/v1/questions", Some(all.as_str()), &[], br#"{"ask":"files_to_read"}"#.to_vec()).await.0,
        503
    );
    d.module.down.set(false);
    assert_eq!(
        post("/v1/commands", Some(all.as_str()), &[], command("group-admin", "k4", again)).await.0,
        200,
        "the same key after a 503 is safe"
    );

    // Questions.
    let (status, scheme) = post("/v1/questions", Some(all.as_str()), &[], br#"{"ask":"scheme","scheme":"harbour"}"#.to_vec()).await;
    assert_eq!((status, scheme["open_file"].as_str(), scheme["frozen"].as_str()), (200, Some("nov.csv"), None));
    assert_eq!(
        post("/v1/questions", Some(all.as_str()), &[], br#"{"ask":"scheme","scheme":"nowhere"}"#.to_vec())
            .await
            .0,
        404
    );
    assert_eq!(
        post("/v1/questions", Some(all.as_str()), &[], br#"{"ask":"outbox","limit":10}"#.to_vec())
            .await
            .0,
        403
    );
    assert_eq!(
        post("/v1/questions", Some(all.as_str()), &[], br#"{"ask":"weather"}"#.to_vec()).await.1["error"]["code"],
        "invalid_question"
    );

    // A token is good only for the caller names it carries.
    let members_only = token(&["member-portal"]);
    let as_staff = command("group-admin", "k9", serde_json::json!({"unfreeze": {"scheme": SCHEME}}));
    assert_eq!(
        post("/v1/commands", Some(members_only.as_str()), &[], as_staff.clone()).await.1["error"]["code"],
        "role_not_held"
    );
    assert_eq!(
        post(upload, Some(members_only.as_str()), &who, FILE.into()).await.1["error"]["code"],
        "role_not_held"
    );
    assert_eq!(
        post(
            "/v1/questions",
            Some(members_only.as_str()),
            &[],
            br#"{"ask":"scheme","scheme":"harbour"}"#.to_vec()
        )
        .await
        .0,
        200
    );
    // A service this one does not know, or one with no role at all, gets nothing.
    let stranger = token(&["documents"]);
    assert_eq!(
        post(
            "/v1/questions",
            Some(stranger.as_str()),
            &[],
            br#"{"ask":"scheme","scheme":"harbour"}"#.to_vec()
        )
        .await
        .0,
        403
    );
    assert_eq!(post("/v1/commands", Some(token(&[]).as_str()), &[], as_staff.clone()).await.0, 401);

    // When Entra ID's signing keys cannot be fetched nothing is known about the token, so
    // the caller is told to try again and is not told it is refused.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dark = format!("http://{}", listener.local_addr().unwrap());
    let unreachable = ic_group::auth::Entra::new("http://127.0.0.1:1", customer_master::TENANT, GROUP_AUDIENCE);
    tokio::spawn(ic_group::service::serve(
        listener,
        ic_group::service::router(d.clone(), unreachable, Default::default()),
        std::future::pending(),
    ));
    let resp = http.post(format!("{dark}/v1/commands")).bearer_auth(&all).body(as_staff).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 503);
}

/// Purpose: the service loop keeps running passes until told to stop, whatever a pass
/// meets: work to do, nothing to do, or the module away; and the counters served at
/// `/metrics` say how many passes ran, how many could not, and what is left waiting.
/// Why it matters: the loop is the only thing that moves a file forward in production. If
/// one failed pass ended it, a brief outage of any dependency would stop all schemes
/// until someone restarted the service.
#[tokio::test]
async fn the_service_loop_survives_failed_passes() {
    let d = Arc::new(rig());
    d.upload(caller::EMPLOYER, "admin-bo", SCHEME, "nov.csv", FILE.as_bytes().to_vec())
        .await
        .unwrap();
    d.files.mark(SCHEME, "nov.csv", Scan::Clean);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let (log, down) = (seen.clone(), d.module.down.clone());
    let health = Arc::new(ic_group::service::Health::default());
    let counted = health.clone();
    let running = tokio::spawn(ic_group::service::run_passes(
        d.clone(),
        Duration::from_millis(5),
        async {
            let _ = stop_rx.await;
        },
        move |pass| {
            counted.record(pass);
            let mut log = log.lock().unwrap();
            log.push(pass.as_ref().map(|p| p.done).map_err(|e| e.to_string()));
            // The module goes away for the third and fourth passes.
            down.set(matches!(log.len(), 2 | 3));
        },
    ));
    for _ in 0..400 {
        if file_status(&d, "nov.csv") == FileStatus::AwaitingApproval && seen.lock().unwrap().len() > 6 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    stop_tx.send(()).unwrap();
    running.await.unwrap();
    let seen = seen.lock().unwrap();
    assert!(seen.iter().any(|p| p.is_err()), "{seen:?}");
    assert!(seen.iter().any(|p| matches!(p, Ok(n) if *n > 0)));
    assert!(matches!(seen.last(), Some(Ok(0))), "idle at the end: {seen:?}");
    assert_eq!(file_status(&d, "nov.csv"), FileStatus::AwaitingApproval);
    // What an operator's dashboard is told about the same run.
    let metrics = health.metrics();
    let value = |name: &str| -> u64 {
        let line = metrics.lines().find(|l| l.starts_with(&format!("{name} "))).unwrap();
        line.rsplit(' ').next().unwrap().parse().unwrap()
    };
    assert_eq!(value("ic_group_passes_total"), seen.len() as u64);
    assert_eq!(value("ic_group_passes_failed_total"), seen.iter().filter(|p| p.is_err()).count() as u64);
    assert!(value("ic_group_steps_total") > 0 && value("ic_group_waiting") == 0, "{metrics}");
    assert!(metrics.contains("# TYPE ic_group_waiting gauge"));
}

/// Purpose: the customer master client sends a joiner's identity with an Entra ID token
/// and reads the three answers, and treats an error status, an unreadable answer, a
/// "possible" with no candidates, and no connection as "not answered" without repeating
/// the person's details in the error.
/// Why it matters: this is the one call that carries names out of the module. Its errors
/// are logged, so they must not carry the names with them; and a wrong reading of the
/// answer is a wrong link between two people.
#[tokio::test]
async fn the_customer_master_client_reads_every_answer() {
    use axum::http::HeaderMap;
    use axum::routing::post;
    use ic_group::service::HttpCustomerMaster;
    let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
    let log = seen.clone();
    let app = axum::Router::new().route(
        "/v1/parties/match",
        post(move |headers: HeaderMap, axum::Json(body): axum::Json<Value>| {
            let log = log.clone();
            async move {
                assert_eq!(headers["authorization"], "Bearer tok-1");
                log.lock().unwrap().push(body.clone());
                match body["family_name"].as_str().unwrap() {
                    "Linked" => (axum::http::StatusCode::OK, r#"{"outcome":"linked","party":"p-1","ignored":["email"]}"#),
                    "Created" => (axum::http::StatusCode::CREATED, r#"{"outcome":"created","party":"p-2"}"#),
                    "Possible" => (axum::http::StatusCode::OK, r#"{"outcome":"possible","candidates":["p-3","p-4"]}"#),
                    "Nobody" => (axum::http::StatusCode::OK, r#"{"outcome":"possible","candidates":[]}"#),
                    "Garbled" => (axum::http::StatusCode::OK, "<html>"),
                    _ => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom"),
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    let identity = |family: &str| MemberIdentity {
        row: 1,
        employee_id: "E002".into(),
        given_name: "Bo".into(),
        family_name: family.into(),
        date_of_birth: Some(d(1985, 6, 2)),
        email: Some("bo@example.test".into()),
        national_id: None,
        mobile_phone: None,
    };
    let cm = HttpCustomerMaster::new(
        &base,
        "api://customer-master/.default",
        ic_azure::credential::AzureCredential::static_token("tok-1"),
    );
    assert_eq!(
        cm.resolve(SCHEME, &identity("Linked")).await.unwrap(),
        Resolution::Linked { party: party("p-1") }
    );
    assert_eq!(
        cm.resolve(SCHEME, &identity("Created")).await.unwrap(),
        Resolution::Created { party: party("p-2") }
    );
    assert_eq!(
        cm.resolve(SCHEME, &identity("Possible")).await.unwrap(),
        Resolution::Possible {
            candidates: vec![party("p-3"), party("p-4")]
        }
    );
    {
        let sent = seen.lock().unwrap();
        assert_eq!(
            (
                sent[0]["source"].as_str(),
                sent[0]["date_of_birth"].as_str(),
                sent[0]["create_if_none"].as_bool()
            ),
            (Some("group-scheme:harbour"), Some("1985-06-02"), Some(true))
        );
    }
    let gone = HttpCustomerMaster::new("http://127.0.0.1:1", "scope", ic_azure::credential::AzureCredential::static_token("tok-1"));
    for (client, family) in [(&cm, "Nobody"), (&cm, "Garbled"), (&cm, "Failing"), (&gone, "Unreachable")] {
        let error = client.resolve(SCHEME, &identity(family)).await.unwrap_err();
        assert!(!error.contains(family) && !error.contains("Bo") && !error.contains("example.test"), "{error}");
    }
}

/// Purpose: the service refuses to start without its storage, event stream, customer
/// master, tenant or audience, or with a short salt, says which setting is wrong, takes the documented
/// defaults for the rest, and never prints a secret.
/// Why it matters: a service that started half-configured would accept uploads it cannot
/// process, and one without its tenant or audience could not tell who is calling. And the
/// fingerprint salt ends up in logs if the configuration's debug form includes it.
#[test]
fn the_service_configuration_is_checked_at_start() {
    use ic_group::service::Config;
    let full: BTreeMap<&str, &str> = [
        ("IC_CLUSTER_NODES", "node-1:7000,node-2:7000"),
        ("IC_GROUP_QUARANTINE_ENDPOINT", "https://stgroup.blob.core.windows.net"),
        ("IC_GROUP_EVENT_HUBS_NAMESPACE", "https://evh.servicebus.windows.net"),
        ("IC_GROUP_CUSTOMER_MASTER", "https://customers.internal"),
        ("IC_GROUP_CUSTOMER_MASTER_SCOPE", "api://customers/.default"),
        ("IC_GROUP_FINGERPRINT_SALT", "salt-0123456789abcdef0123456789abcdef"),
        ("AZURE_TENANT_ID", "tenant-1"),
        ("IC_GROUP_AUDIENCE", "api://group-schemes"),
    ]
    .into();
    let read = |vars: &BTreeMap<&str, &str>| {
        Config::from_lookup(
            |k| vars.get(k).map(|v| v.to_string()),
            |path| match path {
                "/mnt/secrets/salt" => Ok("salt-from-key-vault-0123456789abcdef\n".to_string()),
                _ => Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no such file")),
            },
        )
    };
    let config = read(&full).unwrap();
    assert_eq!(
        (
            config.quarantine_container.as_str(),
            config.event_hub.as_str(),
            config.reinsurance_module.as_str()
        ),
        ("group-quarantine", "group-events", "reinsurance")
    );
    assert_eq!((config.listen.as_str(), config.cycle_every), ("0.0.0.0:8080", Duration::from_secs(5)));
    assert_eq!(
        (config.tenant.as_str(), config.audience.as_str(), config.entra_authority.as_str()),
        ("tenant-1", "api://group-schemes", "https://login.microsoftonline.com")
    );
    // The authority is the one workload identity names, unless this service is given its own.
    let mut vars = full.clone();
    vars.insert("AZURE_AUTHORITY_HOST", "https://login.microsoftonline.us/");
    assert_eq!(read(&vars).unwrap().entra_authority, "https://login.microsoftonline.us/");
    vars.insert("IC_GROUP_ENTRA_AUTHORITY", "https://login.partner.example");
    assert_eq!(read(&vars).unwrap().entra_authority, "https://login.partner.example");
    let shown = format!("{config:?}");
    assert!(shown.contains("node-1:7000") && !shown.contains("salt-0123"), "{shown}");
    for missing in full.keys() {
        let mut vars = full.clone();
        vars.insert(missing, "  ");
        assert!(read(&vars).unwrap_err().contains(missing), "{missing}");
    }
    for (key, value, says) in [
        ("IC_GROUP_FINGERPRINT_SALT", "short", "at least 32"),
        ("IC_GROUP_CYCLE_SECONDS", "0", "above zero"),
        ("IC_GROUP_CYCLE_SECONDS", "soon", "above zero"),
    ] {
        let mut vars = full.clone();
        vars.insert(key, value);
        assert!(read(&vars).unwrap_err().contains(says), "{key}");
    }
    let mut vars = full.clone();
    vars.extend([
        ("IC_GROUP_CYCLE_SECONDS", "30"),
        ("IC_GROUP_LISTEN", "127.0.0.1:9000"),
        ("IC_GROUP_REINSURANCE_MODULE", "ri"),
    ]);
    let config = read(&vars).unwrap();
    assert_eq!(
        (config.cycle_every, config.listen.as_str(), config.reinsurance_module.as_str()),
        (Duration::from_secs(30), "127.0.0.1:9000", "ri")
    );
    // Secrets mounted from Key Vault arrive as files, and the node list
    // may come from the name the other services use.
    let mut vars = full.clone();
    vars.remove("IC_GROUP_FINGERPRINT_SALT");
    vars.remove("IC_CLUSTER_NODES");
    vars.extend([("IC_GROUP_FINGERPRINT_SALT_FILE", "/mnt/secrets/salt"), ("IC_NODES", "node-9:7000")]);
    let config = read(&vars).unwrap();
    assert_eq!(
        (config.fingerprint_salt.as_str(), config.cluster.as_str()),
        ("salt-from-key-vault-0123456789abcdef", "node-9:7000")
    );
    vars.insert("IC_GROUP_FINGERPRINT_SALT_FILE", "/mnt/secrets/absent");
    assert!(read(&vars).unwrap_err().contains("IC_GROUP_FINGERPRINT_SALT_FILE cannot be read"));
}

/// Purpose: an employer who sends a monthly full file can also have payroll send changes in
/// between. The scheme holds a layout for each; a change file is read with the change
/// layout and a full file with the full one, whichever order they arrive in; a payroll
/// change is previewed, approved and applied by the same steps as any file; and when the
/// payroll export changes its column order and date format, a file in the new format has
/// its rows refused under the old layout and is read only once a later version of the
/// layout is set, an earlier version being refused.
/// Why it matters: a change read with the full-file layout would look like a file naming
/// one member, and everyone else would be taken off cover. And a payroll feed that skipped
/// the preview and approval would let a salary typed wrong at the employer change cover
/// and premium with nobody having looked at it.
#[tokio::test]
async fn payroll_changes_and_the_monthly_file_each_use_their_own_layout() {
    use ic_group::record::RowChange;

    let d = rig();
    let payroll = |version: u32, columns: [&str; 2], date_format: &str| {
        FileMapping {
            id: "harbour-payroll".into(),
            version,
            kind: FileKind::Changes,
            delimiter: ',',
            columns: Columns {
                employee_id: "Emp No".into(),
                category: Some("Category".into()),
                salary: Some("Salary".into()),
                billing_group: Some("Site".into()),
                change: Some(columns[0].into()),
                effective: Some(columns[1].into()),
                ..Columns::default()
            },
            date_format: date_format.into(),
            salary_unit: SalaryUnit::Major,
            salary_period: SalaryPeriod::Annual,
            categories: BTreeMap::new(),
            change_values: [("UPD".to_string(), RowChange::Change), ("TERM".to_string(), RowChange::Leave)].into(),
            content_hash: String::new(),
        }
        .seal()
        .unwrap()
    };
    let set = |d: &Rig, key: &str, mapping: FileMapping, today: NaiveDate| {
        let env = Envelope {
            key: key.into(),
            caller: caller::STAFF.into(),
            actor: "ops-ana".into(),
            today,
            command: GroupCommand::SetMapping {
                scheme: SCHEME.into(),
                mapping: Box::new(mapping),
            },
        };
        d.module.state.lock().unwrap().apply_envelope(env).result.map(|_| ()).map_err(|r| r.code)
    };
    let salary = |d: &Rig, id: &str| {
        d.module.state.lock().unwrap().schemes[SCHEME]
            .membership
            .active(id)
            .and_then(|m| m.attrs.salary_minor)
    };
    let members = |d: &Rig| d.module.state.lock().unwrap().schemes[SCHEME].membership.active_count();
    let file = |d: &Rig, name: &str| d.module.state.lock().unwrap().schemes[SCHEME].files[name].clone();
    let today = d.core.business_date().await.unwrap().unwrap();
    through(&d, "nov.csv", FILE).await;
    assert_eq!((members(&d), salary(&d, "E002")), (3, Some(60_000_00)));

    // Before there is a change layout, a payroll change does not fit and is refused whole.
    let change = format!(
        "Emp No,Event,From,Category,Salary,Site\nE002,UPD,{},staff,65000,wlg\n",
        today.format("%Y-%m-%d")
    );
    d.upload(caller::EMPLOYER, "payroll-feed", SCHEME, "early.csv", change.clone().into_bytes())
        .await
        .unwrap();
    d.files.mark(SCHEME, "early.csv", Scan::Clean);
    settle(&d).await;
    assert_eq!(
        (file(&d, "early.csv").status, file(&d, "early.csv").code.as_deref()),
        (FileStatus::Refused, Some("missing_column"))
    );

    // With one, it is previewed as one change, waits for approval, and is applied.
    set(&d, "map-1", payroll(1, ["Event", "From"], "%Y-%m-%d"), today).unwrap();
    d.upload(
        caller::EMPLOYER,
        "payroll-feed",
        SCHEME,
        "pay-1.csv",
        change.replace("65000", "66000").into_bytes(),
    )
    .await
    .unwrap();
    d.files.mark(SCHEME, "pay-1.csv", Scan::Clean);
    settle(&d).await;
    let preview = file(&d, "pay-1.csv");
    assert_eq!(
        (preview.status, preview.summary.changes, preview.summary.leavers),
        (FileStatus::AwaitingApproval, 1, 0)
    );
    assert_eq!(salary(&d, "E002"), Some(60_000_00), "nothing changes before approval");
    approve(&d, "pay-1.csv").await;
    settle(&d).await;
    assert_eq!(
        (file_status(&d, "pay-1.csv"), salary(&d, "E002"), members(&d)),
        (FileStatus::Applied, Some(66_000_00), 3)
    );

    // The next monthly file is still read as a full file.
    through(
        &d,
        "dec.csv",
        &FILE.replace("E002,Bo,Ngata,02/06/1985,staff,60000", "E002,Bo,Ngata,02/06/1985,staff,67000"),
    )
    .await;
    assert_eq!((members(&d), salary(&d, "E002")), (3, Some(67_000_00)));

    // Payroll changes its export: columns renamed and moved, dates written another way.
    let moved = format!(
        "Emp No,Site,Category,Salary,Effective,Type\nE002,wlg,staff,70000,{},UPD\n",
        today.format("%d/%m/%Y")
    );
    d.upload(caller::EMPLOYER, "payroll-feed", SCHEME, "pay-2.csv", moved.clone().into_bytes())
        .await
        .unwrap();
    d.files.mark(SCHEME, "pay-2.csv", Scan::Clean);
    settle(&d).await;
    assert_eq!(file_status(&d, "pay-2.csv"), FileStatus::Refused, "the old layouts do not read the new export");
    assert_eq!(
        set(&d, "map-old", payroll(1, ["Type", "Effective"], "%d/%m/%Y"), today),
        Err("mapping_version".to_string())
    );
    set(&d, "map-2", payroll(2, ["Type", "Effective"], "%d/%m/%Y"), today).unwrap();
    through(&d, "pay-3.csv", &moved.replace("70000", "71000")).await;
    assert_eq!(salary(&d, "E002"), Some(71_000_00));
    // The old export no longer fits either layout.
    d.upload(
        caller::EMPLOYER,
        "payroll-feed",
        SCHEME,
        "pay-4.csv",
        change.replace("65000", "72000").into_bytes(),
    )
    .await
    .unwrap();
    d.files.mark(SCHEME, "pay-4.csv", Scan::Clean);
    settle(&d).await;
    assert_eq!(file_status(&d, "pay-4.csv"), FileStatus::Refused);
    assert_eq!(salary(&d, "E002"), Some(71_000_00));
}

/// Purpose: staff try a proposed mapping version against a sample of the employer's file
/// before setting it. The answer says what the file would do (joiners, leavers, changes,
/// rows refused by number and code, whether the safety check would hold it) and which
/// mapping version was tried; nothing is stored, no file appears, the scheme's mapping and
/// members are untouched and no name leaves the service. A mapping that cannot be sealed, a
/// sample that does not fit it, a sample longer than one part of a real file, an unknown
/// scheme, and a caller who is not staff are each refused.
/// Why it matters: a mapping is trusted for every file once set. Finding out that the new
/// version reads the wrong column from the first live file means the employer's real month
/// is refused, or worse, half read; trying it first on a sample costs nothing.
#[tokio::test]
async fn a_new_mapping_version_is_tried_on_a_sample_before_it_is_set() {
    let d = Arc::new(rig());
    through(&d, "nov.csv", FILE).await;
    let (base, authority) = served(d.clone()).await;
    let staff = customer_master::token_with(&authority, &["group-admin"], GROUP_AUDIENCE);
    let employer = customer_master::token_with(&authority, &["employer-portal"], GROUP_AUDIENCE);
    let http = reqwest::Client::new();
    let attempt = |scheme: &str, token: &str, body: Value| {
        let r = http
            .post(format!("{base}/v1/schemes/{scheme}/mappings/try"))
            .bearer_auth(token)
            .body(serde_json::to_vec(&body).unwrap());
        async move {
            let resp = r.send().await.unwrap();
            (resp.status().as_u16(), resp.json::<Value>().await.unwrap_or(Value::Null))
        }
    };
    let before = d.module.state.lock().unwrap().clone();

    // The export now heads its columns differently and writes dates year first. E002 is
    // gone, E005 is new, E003's pay has changed and one date cannot be read.
    let mut next = named_mapping();
    next.version = 3;
    next.columns.employee_id = "Staff ID".into();
    next.date_format = "%Y-%m-%d".into();
    next.content_hash = String::new();
    let sample = "Staff ID,First,Last,DOB,Category,Salary,Site\n\
        E001,Ana,Founder,1980-05-01,staff,60000,akl\n\
        E003,Cy,Patel,1975-07-03,executive,250000,wlg\n\
        E005,Eve,Tane,1990-01-09,staff,55000,akl\n\
        E006,Fa,Broken,09/01/1990,staff,55000,akl\n";
    let (status, tried) = attempt(SCHEME, &staff, serde_json::json!({"mapping": next, "sample": sample})).await;
    assert_eq!(status, 200, "{tried}");
    let summary = &tried["summary"];
    assert_eq!(
        (
            summary["joiners"].as_u64(),
            summary["leavers"].as_u64(),
            summary["changes"].as_u64(),
            summary["rejected"].as_u64()
        ),
        (Some(1), Some(1), Some(1), Some(1))
    );
    assert_eq!(
        (tried["rejected"][0]["row"].as_u64(), tried["rejected"][0]["code"].as_str()),
        (Some(4), Some("bad_date"))
    );
    assert_eq!(
        (tried["mapping"]["version"].as_u64(), tried["mapping"]["kind"].as_str()),
        (Some(3), Some("full"))
    );
    assert_eq!(tried["would_be_held"], true, "one of three members leaving trips the safety check");
    let text = tried.to_string();
    assert!(!text.contains("Tane") && !text.contains("Eve"), "{text}");
    assert!(*d.module.state.lock().unwrap() == before, "a trial changes nothing");
    assert!(d.files.blobs.lock().unwrap().keys().all(|k| k.ends_with("nov.csv")));

    // What cannot be tried.
    let old_sample = serde_json::json!({"mapping": next, "sample": FILE});
    assert_eq!(attempt(SCHEME, &staff, old_sample).await.1["error"]["code"], "missing_column");
    let mut unusable = next.clone();
    unusable.date_format = "%m/%Y".into();
    let bad = attempt(SCHEME, &staff, serde_json::json!({"mapping": unusable, "sample": sample})).await;
    assert_eq!((bad.0, bad.1["error"]["code"].as_str()), (422, Some("invalid_mapping")));
    assert_eq!(attempt("nowhere", &staff, serde_json::json!({"mapping": next, "sample": sample})).await.0, 404);
    assert_eq!(
        attempt(SCHEME, &staff, serde_json::json!({"sample": sample})).await.1["error"]["code"],
        "invalid_trial"
    );
    assert_eq!(attempt(SCHEME, &employer, serde_json::json!({"mapping": next, "sample": sample})).await.0, 403);
    assert_eq!(
        attempt(SCHEME, "not-a-token", serde_json::json!({"mapping": next, "sample": sample})).await.0,
        401
    );
    // A sample is a sample: more rows than one part of a real file is refused, here and by
    // the module itself if it is asked directly.
    let long: String = std::iter::once("Staff ID,First,Last,DOB,Category,Salary,Site\n".to_string())
        .chain((0..2_001).map(|n| format!("X{n},A,B,1990-01-09,staff,55000,akl\n")))
        .collect();
    let (status, body) = attempt(SCHEME, &staff, serde_json::json!({"mapping": next, "sample": long})).await;
    assert_eq!((status, body["error"]["code"].as_str()), (422, Some("too_many_rows")));
    let rows: Vec<Value> = (0..2_001)
        .map(|n| serde_json::json!({"row": n, "employee_id": format!("X{n}"), "attrs": {}}))
        .collect();
    let direct = ic_group::answer(&before, serde_json::json!({"ask": "preview", "scheme": SCHEME, "kind": "full", "rows": rows}));
    assert!(direct.unwrap_err().contains("at most"));
    assert!(*d.module.state.lock().unwrap() == before);
}

/// Purpose: the driver carries a scheme's broker to the commission module and acts on the
/// three kinds of answer: one to try again (the module has not seen the scheme yet) leaves
/// the layer in the outbox for the next pass; a written layer leaves it; and on the other
/// cover a refusal a person must put right takes it out and raises an exception for staff.
/// Why it matters: the commission module learns of a new scheme from its own feed of the
/// core, which may lag this one. Treating that lag as a failure would leave the broker
/// unpaid, and retrying a real refusal for ever would hide it from everyone.
#[tokio::test]
async fn the_driver_writes_a_schemes_broker_and_reports_a_refusal() {
    let d = rig();
    let today = d.core.business_date().await.unwrap().unwrap();
    let mut t = terms();
    t.broker = Some(ic_group::terms::SchemeBroker {
        engagement: "eng-broker-x".into(),
        book: "x-schemes".into(),
        schedule: "standard".into(),
        option: "level".into(),
        expected_annual_premium_minor: [("group-life".to_string(), 24_000_00)].into(),
    });
    let env = Envelope {
        key: "broker".into(),
        caller: caller::STAFF.into(),
        actor: "ops-ana".into(),
        today,
        command: GroupCommand::AmendTerms {
            scheme: SCHEME.into(),
            terms: Box::new(t),
        },
    };
    d.module.state.lock().unwrap().apply_envelope(env).result.unwrap();
    let waiting = |d: &Rig| {
        d.module
            .state
            .lock()
            .unwrap()
            .outbox
            .values()
            .filter(|o| matches!(o, ic_group::event::Outbound::Commission { .. }))
            .count()
    };
    let refused = |d: &Rig| {
        d.module
            .state
            .lock()
            .unwrap()
            .exceptions
            .values()
            .filter(|e| e.kind == ExceptionKind::BrokerNotRecorded)
            .count()
    };
    assert_eq!(waiting(&d), 1);

    // Not seen yet: the layer waits for the next pass.
    d.peers
        .commission_answers
        .lock()
        .unwrap()
        .push_back(Err("commission does not know the layer yet".into()));
    let pass = d.deliver().await.unwrap();
    assert!(pass.failed >= 1);
    assert_eq!((waiting(&d), refused(&d)), (1, 0));
    // Written on the next.
    d.deliver().await.unwrap();
    assert_eq!(waiting(&d), 0);
    assert_eq!(d.peers.layers.lock().unwrap()[0].1.engagement, "eng-broker-x");

    // A refusal for a person, on a scheme whose broker is added later.
    let d = rig();
    let mut t = terms();
    t.broker = Some(ic_group::terms::SchemeBroker {
        engagement: "eng-unlicensed".into(),
        book: "u-schemes".into(),
        schedule: "standard".into(),
        option: "level".into(),
        expected_annual_premium_minor: [("group-life".to_string(), 24_000_00)].into(),
    });
    let env = Envelope {
        key: "broker".into(),
        caller: caller::STAFF.into(),
        actor: "ops-ana".into(),
        today,
        command: GroupCommand::AmendTerms {
            scheme: SCHEME.into(),
            terms: Box::new(t),
        },
    };
    d.module.state.lock().unwrap().apply_envelope(env).result.unwrap();
    d.peers
        .commission_answers
        .lock()
        .unwrap()
        .push_back(Ok(Some("not_authorised_to_advise".into())));
    d.deliver().await.unwrap();
    assert_eq!((waiting(&d), refused(&d)), (0, 1));
    assert!(d.peers.layers.lock().unwrap().is_empty());
}

/// Purpose: the driver carries a member's letter to the documents module and acts on its
/// three kinds of answer: one to try again leaves the letter in the outbox; a letter taken
/// leaves it; and a refusal takes it out and raises `letter_not_requested` for staff.
/// Why it matters: the leave of absence letters (GRP-038) tell a member abroad the day
/// their cover ends. A letter dropped on a passing fault would leave them uninsured without
/// knowing, and one retried for ever after a refusal would hide that nobody wrote to them.
#[tokio::test]
async fn the_driver_asks_for_a_members_letter_and_reports_a_refusal() {
    let d = rig();
    let policy = d.module.state.lock().unwrap().schemes[SCHEME].policy.clone();
    let letter = |n: u32| MemberLetter {
        request: format!("grp:absence_started:{SCHEME}:E001:2027-11-0{n}"),
        kind: "absence_started".into(),
        party: ic_core::PartyId("party-1".into()),
        policy: policy.clone(),
        scheme: SCHEME.into(),
        life: ic_core::LifeId("E001".into()),
        since: chrono::NaiveDate::from_ymd_opt(2027, 11, n).unwrap(),
        cover_ends: chrono::NaiveDate::from_ymd_opt(2028, 1, 1).unwrap(),
        business_date: chrono::NaiveDate::from_ymd_opt(2027, 11, 1).unwrap(),
    };
    let waiting = |d: &Rig| {
        d.module
            .state
            .lock()
            .unwrap()
            .outbox
            .values()
            .filter(|o| matches!(o, ic_group::event::Outbound::Letter(_)))
            .count()
    };
    // A letter is put in the outbox as the module puts it there when a leave starts.
    let entry = ic_group::event::Outbound::Letter(letter(1));
    d.module.state.lock().unwrap().outbox.insert(9_001, entry);
    d.peers.letter_answers.lock().unwrap().push_back(Err("documents unavailable".into()));
    let pass = d.deliver().await.unwrap();
    assert!(pass.failed >= 1);
    assert_eq!(waiting(&d), 1, "kept for the next pass");
    d.deliver().await.unwrap();
    assert_eq!(waiting(&d), 0);
    assert_eq!(d.peers.letters.lock().unwrap()[0], letter(1));

    let entry = ic_group::event::Outbound::Letter(letter(2));
    d.module.state.lock().unwrap().outbox.insert(9_002, entry);
    d.peers.letter_answers.lock().unwrap().push_back(Ok(Some("unknown_letter_kind".into())));
    d.deliver().await.unwrap();
    assert_eq!(waiting(&d), 0);
    let g = d.module.state.lock().unwrap();
    let raised = g.exceptions.values().find(|e| e.kind == ExceptionKind::LetterNotRequested).unwrap();
    assert_eq!(raised.subject, letter(2).request);
    assert_eq!(d.peers.letters.lock().unwrap().len(), 1);
}

/// Purpose: the driver carries a leaver's underwritten cover to underwriting and acts on the
/// answers: a passing fault leaves it for the next pass; underwriting's refusal (`continuation`:
/// the earlier terms do not cover it) is recorded as the outcome; and an outcome read from
/// underwriting's outbox is recorded on the offer.
/// Why it matters: the continued cover exists only once underwriting writes it. A request lost
/// on a fault would leave the leaver believing they held cover they do not, and a refusal never
/// recorded would leave staff unable to tell them.
#[tokio::test]
async fn the_driver_asks_underwriting_to_continue_a_leavers_cover() {
    use ic_group::state::{ContinuedExcess, ContinuedStatus, Offer, OfferStatus};
    let d = rig();
    let policy = d.module.state.lock().unwrap().schemes[SCHEME].policy.clone();
    let day = chrono::NaiveDate::from_ymd_opt(2027, 11, 1).unwrap();
    let request = |offer: u64| ContinuationExcess {
        key: format!("grp-cont:{SCHEME}:{offer}"),
        policy: ic_core::PolicyId(format!("pol-cont-{offer}")),
        group: ic_core::GroupId(format!("grp-cont-{offer}")),
        party: ic_core::PartyId("party-1".into()),
        product_code: "term-life".into(),
        cover: "life".into(),
        cover_type: "life".into(),
        date_of_birth: None,
        sex: None,
        smoker: None,
        amount_minor: 150_000_00,
        inception: day,
        scheme_policy: policy.clone(),
        scheme_life: ic_core::LifeId("E001".into()),
        earlier: vec![format!("grp-uw:{SCHEME}:1")],
    };
    // Two offers taken up with an underwritten part, as the module leaves them.
    {
        let mut g = d.module.state.lock().unwrap();
        let s = g.schemes.get_mut(SCHEME).unwrap();
        for offer in [7, 8] {
            s.offers.insert(
                offer,
                Offer {
                    id: offer,
                    employee_id: "E001".into(),
                    life: ic_core::LifeId("E001".into()),
                    left: day,
                    expires: day,
                    max_sum_insured_minor: 900_000_00,
                    status: OfferStatus::TakenUp {
                        policy: ic_core::PolicyId(format!("pol-cont-{offer}")),
                    },
                    underwritten_minor: 150_000_00,
                    earlier: vec![],
                    continued_excess: Some(ContinuedExcess {
                        amount_minor: 150_000_00,
                        cover: "life".into(),
                        cover_type: "life".into(),
                        product_code: "term-life".into(),
                        status: ContinuedStatus::Asked,
                    }),
                },
            );
        }
        g.outbox.insert(9_101, ic_group::event::Outbound::ContinueExcess(request(7)));
    }
    let status = |d: &Rig, offer: u64| {
        d.module.state.lock().unwrap().schemes[SCHEME].offers[&offer]
            .continued_excess
            .as_ref()
            .unwrap()
            .status
            .clone()
    };
    d.peers.continuation_answers.lock().unwrap().push_back(Err("underwriting unavailable".into()));
    assert!(d.deliver().await.unwrap().failed >= 1);
    d.deliver().await.unwrap();
    assert_eq!(d.peers.continuations.lock().unwrap()[0], request(7));

    // The other is refused there: recorded as its outcome.
    let entry = ic_group::event::Outbound::ContinueExcess(request(8));
    d.module.state.lock().unwrap().outbox.insert(9_102, entry);
    d.peers.continuation_answers.lock().unwrap().push_back(Ok(Some("continuation".into())));
    d.deliver().await.unwrap();
    assert_eq!(
        status(&d, 8),
        ContinuedStatus::NotContinued {
            outcome: "refused:continuation".into()
        }
    );

    // Underwriting writes the first and says so in its outbox.
    d.peers.continued.lock().unwrap().push(("eff-1".into(), SCHEME.into(), 7, "issued".into()));
    d.peers.effects.lock().unwrap().push((50, None));
    d.collect_answers().await.unwrap();
    assert_eq!(status(&d, 7), ContinuedStatus::InPlace);
}
