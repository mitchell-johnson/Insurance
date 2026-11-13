//! A module's commands for the core, carried there by the host: three nodes
//! in this process over real HTTP, hosting a small module that keeps an
//! outbox beside a partition.

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use common::{d, scratch, start_date};
use ic_core::{Command, Envelope, Event};
use ic_log::StateMachine;
use ic_product::{Frequency, Sex};
use ic_raft::client::fresh_salt;
use ic_raft::server::{self, ColdTarget, EventTarget, Running, Settings};
use ic_raft::{hosted, ClusterAuth, ClusterClient, CoreOutcome, CorePosting, ModuleSpec, ModuleState, NodeId, Outbox, Route};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, term_life};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const TOKEN: &str = "test-cluster-token-0123456789abcdef";

/// A module that only posts: an outbox in arrival order, and what it was
/// told became of each item.
#[derive(Default, Serialize, Deserialize)]
struct Poster {
    next: u64,
    outbox: BTreeMap<u64, CorePosting>,
    queued: BTreeMap<String, u64>,
    outcomes: BTreeMap<String, Vec<CoreOutcome>>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Step {
    /// Put an item in the outbox. An item already queued or settled is left alone.
    Post(Box<CorePosting>),
    /// The host's report of what became of an item.
    Settle { id: String, outcome: CoreOutcome },
}

impl StateMachine for Poster {
    type Command = Step;
    type Output = Result<(), String>;

    fn apply(&mut self, _index: u64, step: Step) -> Result<(), String> {
        match step {
            Step::Post(posting) => {
                if !self.queued.contains_key(&posting.id) {
                    self.next += 1;
                    self.queued.insert(posting.id.clone(), self.next);
                    self.outbox.insert(self.next, *posting);
                }
                Ok(())
            }
            Step::Settle { id, outcome } => {
                if id == "never-settles" {
                    return Err("this item's outcome is never accepted".to_string());
                }
                // An outcome reported twice is recorded once.
                if let Some(seq) = self.queued.get(&id) {
                    if self.outbox.remove(seq).is_some() {
                        self.outcomes.entry(id).or_default().push(outcome);
                    }
                }
                Ok(())
            }
        }
    }
}

fn ask(poster: &Poster, question: Value) -> Result<Value, String> {
    if let Some(limit) = question["pending"].as_u64() {
        if poster.outbox.values().any(|p| p.id == "unreadable") {
            return Ok(json!("not a list of postings"));
        }
        return Ok(json!(poster.outbox.values().take(limit as usize).collect::<Vec<_>>()));
    }
    Ok(json!({ "outcomes": poster.outcomes, "waiting": poster.outbox.len() }))
}

fn poster() -> Box<dyn ModuleState> {
    hosted(Poster::default(), ask)
}

fn pending(limit: usize) -> Value {
    json!({ "pending": limit })
}

fn read(answer: Value) -> Result<Vec<CorePosting>, String> {
    serde_json::from_value(answer).map_err(|e| format!("the outbox could not be read: {e}"))
}

fn outcome(posting: &CorePosting, outcome: &CoreOutcome) -> Value {
    if posting.id == "garbled" {
        return json!({ "nonsense": true });
    }
    json!({ "settle": { "id": posting.id, "outcome": outcome } })
}

fn specs() -> Vec<ModuleSpec> {
    vec![ModuleSpec::new("poster", poster).unwrap().posting_to_core(Outbox { pending, read, outcome })]
}

struct Cluster {
    dir: std::path::PathBuf,
    peers: BTreeMap<NodeId, String>,
    auth: ClusterAuth,
    running: BTreeMap<NodeId, Running>,
    client: ClusterClient,
}

impl Cluster {
    async fn start(name: &str) -> Self {
        let dir = scratch(name);
        let peers: BTreeMap<NodeId, String> = (1..=3u64)
            .map(|id| {
                let port = common::free_port();
                (id, format!("127.0.0.1:{port}"))
            })
            .collect();
        let auth = ClusterAuth::tokens(vec![TOKEN.to_string()]).unwrap();
        let client = ClusterClient::new(peers.values().cloned().collect())
            .with_attempt_timeout(Duration::from_secs(2))
            .with_auth(auth.clone());
        let mut cluster = Self {
            dir,
            peers,
            auth,
            running: BTreeMap::new(),
            client,
        };
        for id in 1..=3 {
            cluster.boot(id).await;
        }
        cluster
    }

    async fn boot(&mut self, id: NodeId) {
        let settings = Settings {
            id,
            listen: self.peers[&id].parse().unwrap(),
            data_dir: self.dir.join(format!("node-{id}")),
            partitions: 1,
            peers: self.peers.clone(),
            bootstrap: id == 1,
            heartbeat_ms: 50,
            auth: self.auth.clone(),
            cold: ColdTarget::Dir(self.dir.join("cold")),
            events: EventTarget::Discard,
            segment_entries: 10,
            snapshot_every: 1000,
        };
        self.running.insert(id, server::start_with(settings, specs()).await.unwrap());
    }

    fn envelope(key: &str, command: Command) -> Envelope {
        Envelope {
            key: key.to_string(),
            caller: "new-business".to_string(),
            salt: fresh_salt(),
            traceparent: None,
            command,
        }
    }

    fn issue(n: u64) -> Command {
        issue_command(term_life(
            n,
            start_date(),
            d(1990, 3, 10),
            Sex::Female,
            false,
            500_000_00,
            None,
            Frequency::Monthly,
            None,
        ))
    }

    /// Put a core command in the module's outbox.
    async fn post(&self, id: &str, route: Route, envelope: Envelope) {
        let posting = CorePosting {
            id: id.to_string(),
            route,
            envelope,
        };
        let reply = self
            .client
            .propose_module_until("poster", &json!({ "post": posting }), Duration::from_secs(30))
            .await
            .unwrap();
        assert_eq!(reply.result, Ok(json!({ "Ok": null })));
    }

    /// What the module has been told, once nothing but `left` items wait.
    async fn settled(&self, left: u64) -> BTreeMap<String, Vec<CoreOutcome>> {
        let mut last = Value::Null;
        for _ in 0..300 {
            if let Ok(Ok(answer)) = self.client.query_module("poster", &json!({})).await {
                if answer["waiting"].as_u64() == Some(left) {
                    return serde_json::from_value(answer["outcomes"].clone()).unwrap();
                }
                last = answer;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("the outbox did not drain to {left}: {last}");
    }

    async fn leader(&self) -> NodeId {
        for _ in 0..100 {
            for id in self.running.keys() {
                if self.client.module_status(&self.peers[id]).await.unwrap()[0].role == "leader" {
                    return *id;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("the module's group elected no leader");
    }

    async fn stop(self) {
        for (_, node) in self.running {
            node.stop().await.unwrap();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Purpose: a core command a module leaves in its outbox is proposed to its partition by
/// the host and the module is told what became of it: applied, refused by the core with
/// the core's own reason, or addressed to a policy no partition holds. A command addressed
/// by policy reaches the partition that holds the policy. Items are carried oldest first.
/// The same command queued under a second item is applied once by the core and reported
/// as a repeat. Items queued while the node carrying them is lost are still carried, by
/// the module's next leader, and each outcome is recorded once. An outcome the module
/// cannot read holds back the items behind it, and an outbox the host cannot read is
/// retried without harm. A question for the investment ledger is answered by the partition.
/// Why it matters: commission, reinsurance and group billing change the ledger only
/// through this path. An item dropped when a node dies is commission earned and never
/// posted; an item posted twice is money counted twice; an item the core refuses and
/// nobody hears about is a module that believes in a posting the ledger does not have.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_modules_core_commands_are_carried_and_their_outcomes_reported() {
    let mut cluster = Cluster::start("module-outbox").await;
    let open = || Command::OpenBusinessDate { date: start_date() };
    let mut opening = Cluster::envelope("open", open());
    opening.caller = "leader".to_string();
    cluster.post("open", Route::Partition(0), opening).await;
    for (n, activation) in shipped_activations().unwrap().into_iter().enumerate() {
        let mut envelope = Cluster::envelope(&format!("activate-{n}"), activation);
        envelope.caller = "config".to_string();
        cluster.post(&format!("activate-{n}"), Route::Partition(0), envelope).await;
    }
    cluster
        .post("issue-1", Route::Partition(0), Cluster::envelope("issue-1", Cluster::issue(1)))
        .await;
    let outcomes = cluster.settled(0).await;
    // Had the issue overtaken the opening of the day or the product's activation, the
    // core would have refused it.
    for (id, outcome) in &outcomes {
        assert!(
            matches!(
                outcome[..],
                [CoreOutcome::Applied {
                    partition: 0,
                    duplicate: false,
                    ..
                }]
            ),
            "{id}: {outcome:?}"
        );
    }
    assert!(outcomes.contains_key("issue-1"));

    // The policy the core issued, as a reader of its events sees it.
    let page = cluster.client.events(0, "test", Some(0), 500).await.unwrap();
    let policy = page
        .events
        .iter()
        .find_map(|record| match &record.event {
            Event::PolicyIssued { policy, .. } => Some(policy.clone()),
            _ => None,
        })
        .expect("the issue produced a PolicyIssued event");

    // The investment ledger answers through the same door as every other question.
    let nothing = cluster.client.investment(0, &ic_core::invest::Question::Executable).await.unwrap();
    assert_eq!(nothing, ic_core::invest::Reply::Executable(vec![]));
    // So does the loadings question: this policy was issued on standard terms.
    let loadings = cluster.client.loadings(0, None, 100, None).await.unwrap();
    assert_eq!((loadings.rows.len(), loadings.next), (0, None));

    // Refused by the core, unroutable, routed by policy, and a repeat of an earlier command.
    let mut refused = Cluster::envelope("unknown-caller", open());
    refused.caller = "nobody".to_string();
    cluster.post("refused", Route::Partition(0), refused).await;
    let nowhere: ic_core::PolicyId = serde_json::from_value(json!("no-such-policy")).unwrap();
    cluster
        .post("lost", Route::Policy(nowhere), Cluster::envelope("issue-9", Cluster::issue(9)))
        .await;
    cluster
        .post("issue-2", Route::Policy(policy), Cluster::envelope("issue-2", Cluster::issue(2)))
        .await;
    cluster
        .post("issue-1-again", Route::Partition(0), Cluster::envelope("issue-1", Cluster::issue(1)))
        .await;
    let outcomes = cluster.settled(0).await;
    assert!(matches!(outcomes["refused"][..], [CoreOutcome::Refused(_)]), "{:?}", outcomes["refused"]);
    assert!(matches!(outcomes["lost"][..], [CoreOutcome::Unroutable(_)]), "{:?}", outcomes["lost"]);
    assert!(
        matches!(
            outcomes["issue-2"][..],
            [CoreOutcome::Applied {
                partition: 0,
                duplicate: false,
                ..
            }]
        ),
        "{:?}",
        outcomes["issue-2"]
    );
    assert!(
        matches!(outcomes["issue-1-again"][..], [CoreOutcome::Applied { duplicate: true, .. }]),
        "{:?}",
        outcomes["issue-1-again"]
    );

    // The node carrying the outbox is lost with work still arriving.
    let carrier = cluster.leader().await;
    cluster
        .post("issue-3", Route::Partition(0), Cluster::envelope("issue-3", Cluster::issue(3)))
        .await;
    cluster.running.remove(&carrier).unwrap().kill().await;
    for n in 4..=6 {
        cluster
            .post(
                &format!("issue-{n}"),
                Route::Partition(0),
                Cluster::envelope(&format!("issue-{n}"), Cluster::issue(n)),
            )
            .await;
    }
    let outcomes = cluster.settled(0).await;
    for n in 3..=6 {
        let outcome = &outcomes[&format!("issue-{n}")];
        assert!(matches!(outcome[..], [CoreOutcome::Applied { .. }]), "issue-{n}: {outcome:?}");
    }
    cluster.boot(carrier).await;

    // An item whose outcome the module answers with a refusal of its own stays in the
    // outbox and is offered again each pass; the core applies it once however often.
    cluster
        .post("never-settles", Route::Partition(0), Cluster::envelope("issue-7", Cluster::issue(7)))
        .await;
    cluster
        .post("after", Route::Partition(0), Cluster::envelope("issue-8", Cluster::issue(8)))
        .await;
    let outcomes = cluster.settled(1).await;
    assert!(matches!(outcomes["after"][..], [CoreOutcome::Applied { duplicate: false, .. }]));

    // An outcome the module cannot read as a command stops the pass there: what is
    // behind it waits its turn rather than overtaking.
    cluster
        .post("garbled", Route::Partition(0), Cluster::envelope("issue-9", Cluster::issue(9)))
        .await;
    cluster
        .post("behind", Route::Partition(0), Cluster::envelope("issue-10", Cluster::issue(10)))
        .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let held = cluster.settled(3).await;
    assert!(!held.contains_key("behind"));

    // An outbox the host cannot read is reported and retried, and takes nothing down.
    cluster
        .post("unreadable", Route::Partition(0), Cluster::envelope("issue-11", Cluster::issue(11)))
        .await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(cluster.settled(4).await.len(), held.len());
    cluster.stop().await;
}
