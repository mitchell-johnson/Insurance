//! Letters asking for an overpaid claim to be repaid: pulled from the
//! claims on each partition, quoting the reference payments holds, and
//! issued like any other requested letter.

mod support;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ic_core::claims_module::report::LetterDue;
use ic_core::claims_module::Command as ClaimsCommand;
use ic_documents::channels::{Channels, MemoryDirectory, Outbound, SendError};
use ic_documents::complaint_letters::LetterPass;
use ic_documents::crypto::{self, LocalWrap};
use ic_documents::driver::{Driver, LocalModule, Module};
use ic_documents::model::{LetterRequestStatus, REQUESTED_KINDS};
use ic_documents::repayment_letters::{amount, reference_of, run_repayment_letters, ClaimsLetters, RepaymentReferences, RepaymentRelay};
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

/// Two partitions of claims: what each says is due, what was recorded,
/// and switches for an outage or a refusal.
#[derive(Clone, Default)]
struct Partitions {
    due: Arc<Mutex<BTreeMap<u16, Option<Vec<LetterDue>>>>>,
    recorded: Arc<Mutex<Vec<(u16, ClaimsCommand)>>>,
    down: Arc<Mutex<Option<u16>>>,
    refuse: Arc<Mutex<Option<String>>>,
}

impl ClaimsLetters for Partitions {
    fn partitions(&self) -> u16 {
        2
    }

    async fn due(&self, partition: u16) -> Result<Option<Vec<LetterDue>>, String> {
        if *self.down.lock().unwrap() == Some(partition) {
            return Err("no leader".into());
        }
        Ok(self.due.lock().unwrap().get(&partition).cloned().unwrap_or(Some(vec![])))
    }

    async fn record(&self, partition: u16, _key: String, command: ClaimsCommand) -> Result<Option<String>, String> {
        if let Some(code) = self.refuse.lock().unwrap().clone() {
            return Ok(Some(code));
        }
        // Taken: it leaves the list, as claims does.
        let ClaimsCommand::RecordRepaymentLetter { request, .. } = &command else {
            panic!("only repayment letters are recorded");
        };
        if let Some(Some(due)) = self.due.lock().unwrap().get_mut(&partition) {
            due.retain(|l| &l.request != request);
        }
        self.recorded.lock().unwrap().push((partition, command));
        Ok(None)
    }
}

/// Payments' references by overpayment; a missing one is nothing owed.
#[derive(Clone, Default)]
struct References {
    by_overpayment: Arc<Mutex<BTreeMap<String, String>>>,
    down: Arc<Mutex<bool>>,
}

impl RepaymentReferences for References {
    async fn reference(&self, overpayment: &str) -> Result<Option<String>, String> {
        if *self.down.lock().unwrap() {
            return Err("payments is unavailable".into());
        }
        Ok(self.by_overpayment.lock().unwrap().get(overpayment).cloned())
    }
}

const REPAY: &str = "Dear {{name}}. We overpaid claim {{reference}} by ${{amount}}. Please repay it quoting {{repayment_reference}}.";

fn due(request: &str, overpayment: &str, amount_minor: i64) -> LetterDue {
    LetterDue {
        request: request.into(),
        claim: "claim-1".into(),
        policy: "pol-1".into(),
        party: "aroha".into(),
        life: Some("life-1".into()),
        overpayment: overpayment.into(),
        amount_minor,
        asked_on: date("2027-11-15"),
    }
}

struct World {
    relay: RepaymentRelay<Partitions, References>,
    driver: Arc<TestDriver>,
}

impl World {
    async fn new(wording: bool) -> Self {
        let mut kit = Kit::new();
        let now = kit.now;
        if wording {
            let data = json!({ "name": "Aroha", "reference": "claim-1", "amount": "1250.00", "repayment_reference": "OP123456" });
            kit.activate(template("cl-overpayment-repayment", DocumentKind::GeneralCorrespondence, REPAY), &data);
        }
        let module = LocalModule::default();
        for envelope in &kit.log {
            assert!(module.propose(envelope.clone()).await.unwrap().result.is_ok());
        }
        let directory = MemoryDirectory::default();
        directory.set("aroha", ic_documents::model::Channel::Email, "Aroha Ngata", "aroha@example.nz");
        let driver = Arc::new(Driver {
            module,
            objects: MemoryStore::default(),
            wrap: LocalWrap::new(crypto::new_key()),
            channels: Quiet,
            directory,
            actor: "documents-driver".into(),
            clock: Arc::new(move || now),
            batch: 100,
        });
        let relay = RepaymentRelay {
            claims: Partitions::default(),
            references: References::default(),
            clock: Arc::new(move || now),
        };
        Self { relay, driver }
    }

    fn owes(&self, partition: u16, letters: Vec<LetterDue>) {
        self.relay.claims.due.lock().unwrap().insert(partition, Some(letters));
    }

    fn reference(&self, overpayment: &str, reference: &str) {
        self.relay
            .references
            .by_overpayment
            .lock()
            .unwrap()
            .insert(overpayment.into(), reference.into());
    }

    async fn pass(&self) -> LetterPass {
        self.relay.pass(&self.driver).await
    }

    fn state(&self) -> ic_documents::DocumentsState {
        self.driver.module.state()
    }
}

/// Purpose: a repayment letter due on any partition is recorded as a
/// request from claims, with the repayment reference payments holds and
/// the amount owed; the partition is told it is taken; the requested
/// letters step then issues it quoting the reference; and the next pass
/// has nothing to do.
/// Why it matters: claims lives inside the partitions and cannot ask for
/// a letter itself. A credit that quotes the reference is matched to the
/// overpayment automatically, so a letter without it, or one sent twice,
/// leaves a person's repayment unmatched or chases them twice.
#[tokio::test]
async fn a_repayment_letter_is_pulled_from_claims_and_quotes_the_reference() {
    let w = World::new(true).await;
    w.owes(1, vec![due("cl-op:op-1:1", "op-1", 125_000)]);
    w.reference("op-1", "OP123456");

    let pass = w.pass().await;
    assert_eq!((pass.issued, pass.left, pass.errors), (1, 0, vec![]));
    let recorded = w.relay.claims.recorded.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec![(
            1,
            ClaimsCommand::RecordRepaymentLetter {
                claim: "claim-1".into(),
                overpayment: "op-1".into(),
                request: "cl-op:op-1:1".into()
            }
        )]
    );
    let record = w.state().letter_requests["cl-op:op-1:1"].clone();
    assert_eq!(record.caller, "claims");
    assert_eq!(record.request.kind, "overpayment_repayment");
    assert_eq!(record.request.detail["repayment_reference"], "OP123456");
    assert_eq!(record.request.detail["amount"], "1250.00");

    let issued = w.driver.requested_letters().await;
    assert_eq!((issued.issued, issued.errors), (1, vec![]));
    assert_eq!(
        w.driver.read("letter:cl-op:op-1:1").await.unwrap().text,
        "Dear Aroha Ngata. We overpaid claim claim-1 by $1250.00. Please repay it quoting OP123456."
    );
    assert!(matches!(w.state().letter_requests["cl-op:op-1:1"].status, LetterRequestStatus::Issued { .. }));
    assert!(REQUESTED_KINDS.iter().any(|k| k.kind == "overpayment_repayment" && k.proven));

    assert_eq!(w.pass().await, LetterPass::default());
}

/// Purpose: a letter for an overpayment payments says nothing is owed on
/// is skipped and not recorded; a partition that is down or not open is
/// passed over while the others are worked; payments being down and a
/// refusal from claims are reported; a letter recorded on an earlier pass
/// for a different amount stands, and the partition is still told.
/// Why it matters: these are the ordinary states of a running cluster.
/// Each must end with exactly one letter asked for, never one for money
/// already repaid and never a pass that stops on the first fault.
#[tokio::test]
async fn faults_and_settled_overpayments_are_handled() {
    let w = World::new(false).await;
    w.owes(0, vec![due("cl-op:op-0:1", "op-0", 5_000)]);
    w.owes(1, vec![due("cl-op:op-1:1", "op-1", 125_000)]);
    w.reference("op-1", "OP123456");

    // Partition 0's overpayment is settled: skipped, and left for claims.
    *w.relay.claims.down.lock().unwrap() = Some(1);
    let pass = w.pass().await;
    assert_eq!((pass.issued, pass.left), (0, 1));
    assert_eq!(pass.errors, vec!["partition 1: repayment letters: no leader".to_string()]);
    *w.relay.claims.down.lock().unwrap() = None;

    *w.relay.references.down.lock().unwrap() = true;
    let pass = w.pass().await;
    assert!(pass.errors.iter().any(|e| e == "cl-op:op-1:1: payments is unavailable"), "{:?}", pass.errors);
    *w.relay.references.down.lock().unwrap() = false;

    // Claims refuses the record: the request stands, and is reported.
    *w.relay.claims.refuse.lock().unwrap() = Some("unknown_letter".into());
    let pass = w.pass().await;
    assert_eq!(pass.issued, 0);
    assert!(
        pass.errors.iter().any(|e| e.contains("claims refused the record: unknown_letter")),
        "{:?}",
        pass.errors
    );
    assert!(w.state().letter_requests.contains_key("cl-op:op-1:1"));
    *w.relay.claims.refuse.lock().unwrap() = None;

    // Since then some was repaid: the letter asked for stands.
    w.owes(1, vec![due("cl-op:op-1:1", "op-1", 100_000)]);
    let pass = w.pass().await;
    assert_eq!((pass.issued, pass.errors), (1, vec![]));
    assert_eq!(w.state().letter_requests["cl-op:op-1:1"].request.detail["amount"], "1250.00");

    // A partition whose business date is not open is passed over.
    w.relay.claims.due.lock().unwrap().insert(1, None);
    assert_eq!(w.pass().await.errors, Vec::<String>::new());

    // No wording yet: the request waits in the documents module.
    let issued = w.driver.requested_letters().await;
    assert_eq!(issued.issued, 0);
    assert!(issued.errors[0].contains("cl-overpayment-repayment"), "{:?}", issued.errors);
}

/// Purpose: amounts are written with two decimal places and a sign, and
/// payments' answer is read as a reference, as nothing owed, or as an
/// error when it is anything else.
/// Why it matters: the letter states the money a person must repay; a
/// wrong amount or an empty reference on it is the insurer's error, and
/// an answer payments did not mean must not become a reference.
#[test]
fn amounts_and_references_are_read_exactly() {
    assert_eq!(amount(125_000), "1250.00");
    assert_eq!(amount(5), "0.05");
    assert_eq!(amount(-1_999), "-19.99");
    assert_eq!(reference_of(json!("OP123456")), Ok(Some("OP123456".to_string())));
    assert_eq!(reference_of(Value::Null), Ok(None));
    assert!(reference_of(json!(" ")).is_err());
    assert!(reference_of(json!({ "reference": "OP1" })).is_err());
}

/// Purpose: the step runs a pass straight away and on every interval
/// until it is stopped.
/// Why it matters: a step that quietly stopped would leave overpayments
/// unchased while claims believes the letters were asked for.
#[tokio::test]
async fn the_step_runs_until_stopped() {
    let w = World::new(true).await;
    w.owes(0, vec![due("cl-op:op-1:1", "op-1", 125_000)]);
    w.reference("op-1", "OP123456");
    let passes = Arc::new(Mutex::new(vec![]));
    let seen = passes.clone();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let World { relay, driver } = w;
    let task = tokio::spawn(run_repayment_letters(
        relay,
        driver,
        Duration::from_millis(5),
        async move {
            let _ = rx.await;
        },
        move |pass: &LetterPass| seen.lock().unwrap().push(pass.clone()),
    ));
    tokio::time::sleep(Duration::from_millis(40)).await;
    tx.send(()).unwrap();
    task.await.unwrap();
    let passes = passes.lock().unwrap().clone();
    assert!(passes.len() >= 2, "{}", passes.len());
    assert_eq!(passes[0].issued, 1);
    assert_eq!(passes.iter().map(|p| p.issued).sum::<u32>(), 1);
}

/// Purpose: in production the step reaches the partitions and payments
/// over the cluster: a partition's letters, a partition not yet open, an
/// answer to another question, a record taken or refused, payments'
/// reference, nothing owed or a refusal, and a cluster nobody answers,
/// each read as what it is.
/// Why it matters: every repayment letter goes over these routes. An
/// unreachable cluster must read as "try again" and an odd answer as an
/// error, never as no letters due or as a letter taken.
#[tokio::test]
async fn the_step_reaches_claims_and_payments_over_the_cluster() {
    use axum::extract::Path;
    use axum::routing::post;
    use axum::{Json, Router};
    use ic_core::claims_module::report::Reply;
    use ic_documents::repayment_letters::{ClusterClaimsLetters, ClusterReferences};
    use ic_raft::ClusterClient;

    async fn query(Path(partition): Path<u16>) -> Json<Value> {
        Json(match partition {
            0 => serde_json::to_value(Some(Reply::RepaymentLetters(vec![due("cl-op:op-1:1", "op-1", 125_000)]))).unwrap(),
            1 => Value::Null,
            _ => serde_json::to_value(Some(Reply::Queue(vec![]))).unwrap(),
        })
    }
    async fn command(Json(envelope): Json<Value>) -> Json<Value> {
        let rejected = if envelope["key"] == "refuse" {
            json!({ "code": "unknown_letter", "message": "no such letter" })
        } else {
            Value::Null
        };
        Json(json!({ "log_index": 3, "rejected": rejected, "duplicate": false, "events": [], "messages": [], "entries": 0, "work_units": 0 }))
    }
    async fn payments(Json(question): Json<Value>) -> Json<Value> {
        Json(match question["overpayment"].as_str() {
            Some("op-1") => json!({ "Ok": "OP123456" }),
            Some("op-2") => json!({ "Ok": null }),
            _ => json!({ "Err": "refused" }),
        })
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let app = Router::new()
        .route("/v1/partitions/{partition}/query", post(query))
        .route("/v1/partitions/{partition}/commands", post(command))
        .route("/v1/modules/payments/query", post(payments));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let patience = Duration::from_millis(200);
    let claims = ClusterClaimsLetters::new(ClusterClient::new(vec![addr.clone()]), 3, patience);
    assert_eq!(claims.partitions(), 3);
    let letters = claims.due(0).await.unwrap().unwrap();
    assert_eq!(letters[0].request, "cl-op:op-1:1");
    assert_eq!(claims.due(1).await, Ok(None));
    assert!(claims.due(2).await.unwrap_err().contains("another question"));
    let taken = || ClaimsCommand::RecordRepaymentLetter {
        claim: "claim-1".into(),
        overpayment: "op-1".into(),
        request: "cl-op:op-1:1".into(),
    };
    assert_eq!(claims.record(0, "ok".into(), taken()).await, Ok(None));
    assert_eq!(claims.record(0, "refuse".into(), taken()).await, Ok(Some("unknown_letter".into())));

    let references = ClusterReferences::new(ClusterClient::new(vec![addr]));
    assert_eq!(references.reference("op-1").await, Ok(Some("OP123456".into())));
    assert_eq!(references.reference("op-2").await, Ok(None));
    assert_eq!(references.reference("op-3").await, Err("refused".into()));

    let nowhere = ClusterClient::new(vec!["127.0.0.1:1".into()]);
    let lost = ClusterClaimsLetters::new(nowhere.clone(), 1, Duration::from_millis(50));
    assert!(lost.due(0).await.is_err());
    assert!(lost.record(0, "k".into(), taken()).await.is_err());
    assert!(ClusterReferences::new(nowhere).reference("op-1").await.is_err());
}

/// Purpose: a request the documents module refuses for any reason but an
/// earlier, different version of itself is reported, and the partition is
/// not told the letter was taken.
/// Why it matters: telling claims a letter was taken when it was not
/// would drop the overpayment from the list with nobody ever asked to
/// repay it.
#[tokio::test]
async fn a_refused_request_is_not_recorded_as_taken() {
    let w = World::new(true).await;
    let mut bad = due("cl-op:op-1:1", "op-1", 125_000);
    bad.claim = " ".into();
    w.owes(0, vec![bad]);
    w.reference("op-1", "OP123456");
    let pass = w.pass().await;
    assert_eq!(pass.issued, 0);
    assert!(pass.errors[0].contains("no_request"), "{:?}", pass.errors);
    assert!(w.relay.claims.recorded.lock().unwrap().is_empty());
}
