//! A ledger node as a process: one member of every partition's Raft group,
//! served over HTTP.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use ic_azure::blob::{BlobContainer, DirWormStore, WormStore};
use ic_azure::cold_tier::seal_segment;
use ic_azure::credential::AzureCredential;
use ic_azure::eventhubs::{EventHubsSink, EventSink};
use ic_azure::AzureError;
use ic_core::{recon, Envelope, Policy};
use ic_ledger::PartitionId;
use openraft::alias::LogIdOf;
use openraft::async_runtime::WatchReceiver;
use openraft::raft::ReadPolicy;
use openraft::storage::RaftStateMachine;
use openraft::BasicNode;

use crate::access::Guard;
use crate::auth::ClusterAuth;
use crate::client::{BreakView, ClusterClient, Consumed, EndOfDayView, EventPage, MoveLeader, Refusal};
use crate::failback::{self as fb, Failback, FailbackSettings, FailbackStatus, GroupKey};
use crate::group;
use crate::machine::Machine;
use crate::module::{CoreOutcome, CorePosting, ModuleGroup, ModuleMachine, ModuleSpec, ModuleStatus, Outbox, Route};
use crate::network::{self, GroupOf, Network};
use crate::node::{self, Node, NodeStatus, ProposeError};
use crate::peers::Identity;
use crate::query::Query as Question;
use crate::store::LogStore;
use crate::types::{Housekeeping, NodeId, Proposal, Reply, TypeConfig};

/// How a node is configured. Read from the environment by the binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub id: NodeId,
    pub listen: SocketAddr,
    pub data_dir: PathBuf,
    pub partitions: PartitionId,
    /// Every member of the cluster, this one included, as `host:port`.
    pub peers: BTreeMap<NodeId, String>,
    /// Form the groups on first start. Set on exactly one node.
    pub bootstrap: bool,
    pub heartbeat_ms: u64,
    /// The cluster token callers must present, and this node presents.
    pub auth: ClusterAuth,
    /// Where sealed journal segments go.
    pub cold: ColdTarget,
    /// Where events go.
    pub events: EventTarget,
    /// Journal entries per cold-tier segment. Never change it on a live
    /// cluster: segment boundaries are derived from it.
    pub segment_entries: u64,
    /// Log entries between snapshots.
    pub snapshot_every: u64,
    /// The witnesses among the peers: voters that keep the log and no
    /// state ([`crate::witness`]). This node is one if its number is here.
    pub witnesses: BTreeSet<NodeId>,
}

/// Where sealed journal segments are written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColdTarget {
    /// An immutable Blob Storage container: `(endpoint, container)`.
    Azure(String, String),
    /// A directory with the same write-once rule. Off Azure only.
    Dir(PathBuf),
}

/// Where events are published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventTarget {
    /// An Event Hubs namespace and hub: `(endpoint, hub)`.
    EventHubs(String, String),
    /// Nowhere: events are dropped once produced. For a cluster with no
    /// consumers; never for production, where the read models depend on them.
    Discard,
}

impl Settings {
    /// Read settings through `get`, which is `std::env::var` in production.
    ///
    /// - `IC_NODE_ID`: this member's number, unique in the cluster.
    /// - `IC_NODE_PEERS`: `1=host:port,2=host:port,...` for every member.
    /// - `IC_NODE_DATA_DIR`: where the Raft logs live. A persistent disk.
    /// - `IC_NODE_LISTEN`: default `0.0.0.0:7000`.
    /// - `IC_PARTITIONS`: default 1.
    /// - `IC_NODE_BOOTSTRAP`: `true` on the one node that forms the groups.
    /// - `IC_RAFT_HEARTBEAT_MS`: default 250; raise it for cross-region groups.
    /// - `IC_BLOB_ENDPOINT` with `IC_COLD_CONTAINER` (default `cold-segments`),
    ///   or `IC_COLD_DIR`: where sealed journal segments go. One is required.
    /// - `IC_EVENT_HUBS_ENDPOINT` with `IC_EVENT_HUB` (default `policy-events`),
    ///   or `IC_EVENT_SINK=discard`: where events go. One is required.
    /// - `IC_SEGMENT_ENTRIES`: default 1000.
    /// - `IC_RAFT_SNAPSHOT_EVERY`: default 5000 log entries.
    /// - `IC_CLUSTER_TOKEN_FILE`, `IC_CLUSTER_TOKEN`, `IC_CLUSTER_AUTH`: see
    ///   [`ClusterAuth::from_lookup`]. One of them is required.
    /// - `IC_CLUSTER_ACCESS_FILE`: the access map that gives each workload
    ///   its own token and grant ([`crate::access`]). Without it every
    ///   accepted token may do everything.
    /// - `IC_WITNESS_NODES`: `5` — the witnesses among the peers, set the same
    ///   on every node. A witness needs no cold tier and no event hub: it
    ///   seals and publishes nothing.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let auth = ClusterAuth::from_lookup(&get, |path| std::fs::read_to_string(path))?;
        let get = |key: &str| get(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let auth = match get("IC_CLUSTER_ACCESS_FILE") {
            Some(path) => auth.with_access_file(path),
            None => auth,
        };
        let required = |key: &str| get(key).ok_or_else(|| format!("{key} is required"));
        let id: NodeId = required("IC_NODE_ID")?.parse().map_err(|e| format!("IC_NODE_ID: {e}"))?;
        let mut peers = BTreeMap::new();
        for pair in required("IC_NODE_PEERS")?.split(',') {
            let (peer, addr) = pair
                .split_once('=')
                .ok_or_else(|| format!("IC_NODE_PEERS: expected id=host:port, got {pair:?}"))?;
            let peer: NodeId = peer.trim().parse().map_err(|e| format!("IC_NODE_PEERS: {e}"))?;
            if addr.trim().is_empty() || peers.insert(peer, addr.trim().to_string()).is_some() {
                return Err(format!("IC_NODE_PEERS: node {peer} needs exactly one address"));
            }
        }
        if !peers.contains_key(&id) {
            return Err(format!("IC_NODE_PEERS does not list this node ({id})"));
        }
        let partitions: PartitionId = get("IC_PARTITIONS").map_or(Ok(1), |v| v.parse().map_err(|e| format!("IC_PARTITIONS: {e}")))?;
        if partitions == 0 || partitions > ic_ledger::PARTITION_COUNT {
            return Err(format!("IC_PARTITIONS must be between 1 and {}", ic_ledger::PARTITION_COUNT));
        }
        let heartbeat_ms: u64 = get("IC_RAFT_HEARTBEAT_MS").map_or(Ok(250), |v| v.parse().map_err(|e| format!("IC_RAFT_HEARTBEAT_MS: {e}")))?;
        if heartbeat_ms == 0 {
            return Err("IC_RAFT_HEARTBEAT_MS must be positive".to_string());
        }
        let mut witnesses = BTreeSet::new();
        for w in get("IC_WITNESS_NODES").unwrap_or_default().split(',').filter(|w| !w.trim().is_empty()) {
            let w: NodeId = w.trim().parse().map_err(|e| format!("IC_WITNESS_NODES: {e}"))?;
            if !peers.contains_key(&w) {
                return Err(format!("IC_WITNESS_NODES: node {w} is not in IC_NODE_PEERS"));
            }
            witnesses.insert(w);
        }
        if witnesses.len() * 2 >= peers.len() {
            return Err("IC_WITNESS_NODES: the witnesses must be fewer than half the peers, or a majority could hold no data".to_string());
        }
        let witness = witnesses.contains(&id);
        let cold = match (get("IC_BLOB_ENDPOINT"), get("IC_COLD_DIR")) {
            (Some(_), Some(_)) => return Err("set IC_BLOB_ENDPOINT or IC_COLD_DIR, not both".to_string()),
            (Some(endpoint), None) => ColdTarget::Azure(endpoint, get("IC_COLD_CONTAINER").unwrap_or_else(|| "cold-segments".to_string())),
            (None, Some(dir)) => ColdTarget::Dir(PathBuf::from(dir)),
            // A witness seals nothing; the directory is never written.
            (None, None) if witness => ColdTarget::Dir(PathBuf::from(required("IC_NODE_DATA_DIR")?).join("unused")),
            (None, None) => return Err("set IC_BLOB_ENDPOINT or IC_COLD_DIR: the journal must have somewhere to be sealed".to_string()),
        };
        let events = match (get("IC_EVENT_HUBS_ENDPOINT"), get("IC_EVENT_SINK").as_deref()) {
            (Some(endpoint), None) => EventTarget::EventHubs(endpoint, get("IC_EVENT_HUB").unwrap_or_else(|| "policy-events".to_string())),
            (None, Some("discard")) => EventTarget::Discard,
            (Some(_), Some(_)) => return Err("set IC_EVENT_HUBS_ENDPOINT or IC_EVENT_SINK, not both".to_string()),
            (None, Some(other)) => return Err(format!("IC_EVENT_SINK must be discard, not {other:?}")),
            (None, None) if witness => EventTarget::Discard,
            (None, None) => return Err("set IC_EVENT_HUBS_ENDPOINT, or IC_EVENT_SINK=discard to drop events".to_string()),
        };
        let positive = |key: &str, default: u64| -> Result<u64, String> {
            let value = get(key).map_or(Ok(default), |v| v.parse().map_err(|e| format!("{key}: {e}")))?;
            if value == 0 {
                return Err(format!("{key} must be positive"));
            }
            Ok(value)
        };
        let segment_entries = positive("IC_SEGMENT_ENTRIES", 1000)?;
        let snapshot_every = positive("IC_RAFT_SNAPSHOT_EVERY", 5000)?;
        Ok(Self {
            id,
            listen: get("IC_NODE_LISTEN")
                .unwrap_or_else(|| "0.0.0.0:7000".to_string())
                .parse()
                .map_err(|e| format!("IC_NODE_LISTEN: {e}"))?,
            data_dir: PathBuf::from(required("IC_NODE_DATA_DIR")?),
            partitions,
            peers,
            bootstrap: get("IC_NODE_BOOTSTRAP").is_some_and(|v| v == "true"),
            heartbeat_ms,
            auth,
            cold,
            events,
            segment_entries,
            snapshot_every,
            witnesses,
        })
    }
}

/// One node: a member of each partition's group.
pub struct Host {
    pub id: NodeId,
    pub groups: BTreeMap<PartitionId, Node>,
    /// A member of each hosted module's group, by the module's name.
    pub modules: BTreeMap<String, ModuleGroup>,
    /// Hands leadership back to each group's home region. Off unless configured.
    pub failback: Failback,
    /// This node is a witness: it keeps the logs and no state, serves no
    /// caller, and hands over any group it comes to lead.
    pub witness: bool,
    /// The witnesses among the voters.
    witnesses: BTreeSet<NodeId>,
    /// Per group, the last durable mark this node proposed as leader: how far
    /// it said the data voters held the log, and the mark's own log index.
    marks: Mutex<BTreeMap<GroupKey, Mark>>,
    /// The hosted modules that post to the core, and how to read their
    /// outboxes.
    outboxes: BTreeMap<String, Outbox>,
    /// The hosted modules that read the partitions' events.
    readers: Vec<String>,
    partitions: PartitionId,
    peers: BTreeMap<NodeId, String>,
    /// Who may call this node, and what each caller may do there.
    guard: Guard,
    cold: Cold,
    sink: Sink,
    segment_entries: u64,
}

#[derive(Debug, Clone, Copy)]
struct Mark {
    log_index: u64,
    at: Instant,
}

/// How often a leader records how far the data voters hold the log, at most.
const MARK_EVERY: Duration = Duration::from_secs(5);

enum Cold {
    Azure(BlobContainer),
    Dir(DirWormStore),
}

impl WormStore for Cold {
    async fn put_new(&self, name: &str, content_type: &str, body: Vec<u8>) -> Result<(), AzureError> {
        match self {
            Cold::Azure(s) => s.put_new(name, content_type, body).await,
            Cold::Dir(s) => s.put_new(name, content_type, body).await,
        }
    }

    async fn get(&self, name: &str) -> Result<Option<Vec<u8>>, AzureError> {
        match self {
            Cold::Azure(s) => s.get(name).await,
            Cold::Dir(s) => s.get(name).await,
        }
    }
}

enum Sink {
    EventHubs(EventHubsSink),
    Discard,
}

/// Events sent to the stream in one request.
const EVENT_BATCH: usize = 500;
/// How many of a module's core commands one pass carries.
const OUTBOX_BATCH: usize = 64;

impl Host {
    /// Open the logs and start every group's member, and a member of each
    /// module's group besides. Existing logs are replayed, so a restarted
    /// node comes back with the state it had. Every node of a cluster hosts
    /// the same modules. `failback` turns on automatic fail-back to
    /// preferred nodes.
    pub async fn start_full(settings: &Settings, specs: &[ModuleSpec], failback: Option<FailbackSettings>) -> Result<Arc<Self>, String> {
        // Checked before anything is opened: a node with a bad access map does not start.
        let guard = Guard::for_node(&settings.auth, |path| std::fs::read_to_string(path))?;
        let _ = guard.watch();
        let witness = settings.witnesses.contains(&settings.id);
        let timings = |name: &str| {
            if witness {
                node::witness_config(name, settings.heartbeat_ms)
            } else {
                node::config(name, settings.heartbeat_ms, settings.snapshot_every)
            }
        };
        let mut modules = BTreeMap::new();
        for spec in specs {
            let group = ModuleGroup::group_name(&spec.name);
            let dir = settings.data_dir.join(&group);
            let log = LogStore::open(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            let machine = if witness {
                ModuleMachine::open_witness(spec.clone(), &dir)
            } else {
                ModuleMachine::open(spec.clone(), &dir)
            }
            .map_err(|e| format!("{}: {e}", dir.display()))?;
            let config = timings(&group)?;
            let identity = Identity::open(settings.id, &dir).map_err(|e| e.to_string())?;
            let network = Network::http_group(identity.clone(), group, settings.auth.clone());
            let member = ModuleGroup::start(identity, &spec.name, log, machine, network, config).await?;
            if modules.insert(spec.name.clone(), member).is_some() {
                return Err(format!("module {} is listed twice", spec.name));
            }
        }
        let mut groups = BTreeMap::new();
        for partition in 0..settings.partitions {
            let dir = settings.data_dir.join(format!("partition-{partition}"));
            let log = LogStore::open(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            let machine = if witness {
                Machine::open_witness(partition, &dir)
            } else {
                Machine::open(partition, &dir)
            }
            .map_err(|e| format!("{}: {e}", dir.display()))?;
            let config = timings(&format!("partition-{partition}"))?;
            let identity = Identity::open(settings.id, &dir).map_err(|e| e.to_string())?;
            let network = Network::http(identity.clone(), partition, settings.auth.clone());
            let member = Node::start(identity, partition, log, machine, network, config).await?;
            groups.insert(partition, member);
        }
        let credential = || AzureCredential::from_env().map_err(|e| e.to_string());
        let cold = match &settings.cold {
            ColdTarget::Azure(endpoint, container) => Cold::Azure(BlobContainer::new(endpoint, container, credential()?)),
            ColdTarget::Dir(dir) => Cold::Dir(DirWormStore::new(dir)),
        };
        let sink = match &settings.events {
            EventTarget::EventHubs(endpoint, hub) => Sink::EventHubs(EventHubsSink::new(endpoint, hub, credential()?)),
            EventTarget::Discard => Sink::Discard,
        };
        Ok(Arc::new(Self {
            id: settings.id,
            cold,
            sink,
            segment_entries: settings.segment_entries,
            groups,
            modules,
            failback: Failback::new(failback),
            witness,
            witnesses: settings.witnesses.clone(),
            marks: Mutex::new(BTreeMap::new()),
            readers: specs.iter().filter(|s| s.events).map(|s| s.name.clone()).collect(),
            outboxes: specs.iter().filter_map(|s| s.outbox.map(|o| (s.name.clone(), o))).collect(),
            partitions: settings.partitions,
            peers: settings.peers.clone(),
            guard,
        }))
    }

    /// Form every group from the configured peers. A group that already
    /// exists is left as it is.
    pub async fn bootstrap(&self) -> Result<(), String> {
        let members: BTreeMap<NodeId, BasicNode> = self.peers.iter().map(|(id, addr)| (*id, BasicNode::new(addr.clone()))).collect();
        for member in self.groups.values() {
            member.initialize(members.clone()).await?;
        }
        for member in self.modules.values() {
            member.initialize(members.clone()).await?;
        }
        Ok(())
    }

    pub fn statuses(&self) -> Vec<NodeStatus> {
        self.groups.values().map(Node::status).collect()
    }

    pub fn module_statuses(&self) -> Vec<ModuleStatus> {
        self.modules.values().map(ModuleGroup::status).collect()
    }

    pub(crate) fn view(&self, member: &Node) -> EndOfDayView {
        // Asked before the state is read: both take the machine's lock.
        let claims_message_in_flight = member.machine.pending_messages().iter().any(|m| m.body.is_claims());
        member.machine.read(|s| EndOfDayView {
            partition: member.partition,
            business_date: s.business_date,
            last_close: s.last_close.clone(),
            open: s.open_policy_ids(),
            unfinished: s.end_of_day_unfinished(),
            breaks: recon::reconcile(s)
                .into_iter()
                .map(|b| BreakView {
                    check: b.check.to_string(),
                    detail: b.detail,
                })
                .collect(),
            transfer_in_transit: recon::control_totals(s).transfer_in_transit.to_string(),
            transfer_in_flight: s.policies.values().any(Policy::is_frozen),
            claims_offset_in_transit: recon::control_totals(s).claims_offset_in_transit.to_string(),
            claims_message_in_flight,
        })
    }

    /// Deliver a partition's waiting messages to the partitions they are
    /// for, at least once, with the message ID as the receiver's idempotency
    /// key, and record which arrived.
    async fn deliver(&self, member: &Node, client: &ClusterClient) -> Result<usize, String> {
        let mut delivered = Vec::new();
        for message in member.machine.pending_messages() {
            let (caller, command) = message.body.clone().into_delivery();
            let envelope = Envelope {
                key: message.id.clone(),
                caller: caller.to_string(),
                // Drawn fresh for each delivery. Only the first delivery
                // is applied, so only its salt is ever used.
                salt: crate::client::fresh_salt(),
                traceparent: None,
                command,
            };
            match client.propose(message.to_partition, &envelope).await {
                // Applied or refused, the receiver has dealt with it: a
                // refusal comes back to this partition as its own message.
                Ok(_) => delivered.push(message.id),
                Err(e) => tracing::warn!(message = %message.id, error = %e, "message not delivered; will retry"),
            }
        }
        let count = delivered.len();
        if count > 0 {
            member.housekeep(Housekeeping::Delivered { ids: delivered }).await.map_err(|e| e.to_string())?;
        }
        Ok(count)
    }

    /// Have a partition keep its events for every module that reads them.
    /// Done before anything is published, so a module hosted from the
    /// start misses nothing.
    async fn register(&self, member: &Node) -> Result<usize, String> {
        let known = member.machine.consumers();
        for reader in self.readers.iter().filter(|r| !known.contains_key(*r)) {
            let registration = Housekeeping::Consumed {
                consumer: reader.clone(),
                through_seq: 0,
            };
            member.housekeep(registration).await.map_err(|e| e.to_string())?;
        }
        Ok(0)
    }

    /// Bring a module's group up to date with one partition's events, and
    /// tell the partition how far the group has them. At least once: a run
    /// offered twice is taken in once, by the group's own checkpoint.
    async fn feed(&self, module: &ModuleGroup, partition: PartitionId, client: &ClusterClient) -> Result<usize, String> {
        let have = module.machine.cursors().get(&partition).copied();
        let page = client.events(partition, &module.name, have, EVENT_BATCH).await.map_err(|e| e.to_string())?;
        let fed = page.events.len();
        let through = if fed == 0 {
            have
        } else {
            Some(module.feed(partition, page.after, page.events).await.map_err(|e| e.to_string())??)
        };
        if let Some(through) = through.filter(|t| page.acknowledged.is_none_or(|a| a < *t)) {
            client.acknowledge_events(partition, &module.name, through).await.map_err(|e| e.to_string())?;
        }
        Ok(fed)
    }

    /// Carry a module's waiting core commands to their partitions, oldest
    /// first, and tell the module what became of each. Stops at the first
    /// one that could not be offered, or whose outcome the module could not
    /// read, so the order holds; it is offered again on the next pass.
    async fn carry(&self, module: &ModuleGroup, outbox: &Outbox, client: &ClusterClient) -> Result<usize, String> {
        let answer = module.query((outbox.pending)(OUTBOX_BATCH)).await.map_err(|e| e.to_string())??;
        let mut carried = 0;
        for posting in (outbox.read)(answer)? {
            let outcome = match self.offer(&posting, client).await? {
                Some(partition) => match client.propose(partition, &posting.envelope).await.map_err(|e| e.to_string())? {
                    Reply { rejected: Some(reject), .. } => CoreOutcome::Refused(reject),
                    reply => CoreOutcome::Applied {
                        partition,
                        log_index: reply.log_index,
                        duplicate: reply.duplicate,
                    },
                },
                None => CoreOutcome::Unroutable(format!("no partition holds the policy of outbox item {}", posting.id)),
            };
            let told = module.propose((outbox.outcome)(&posting, &outcome)).await.map_err(|e| e.to_string())?;
            if let Err(refusal) = told.result {
                return Err(format!("the module refused the outcome of outbox item {}: {refusal}", posting.id));
            }
            carried += 1;
        }
        Ok(carried)
    }

    /// The partition a posting goes to. `None` when it names a policy that
    /// no partition holds.
    async fn offer(&self, posting: &CorePosting, client: &ClusterClient) -> Result<Option<PartitionId>, String> {
        match &posting.route {
            Route::Partition(partition) => Ok(Some(*partition)),
            Route::Policy(policy) => {
                for partition in 0..self.partitions {
                    if client.locate(partition, policy).await.map_err(|e| e.to_string())?.is_some() {
                        return Ok(Some(partition));
                    }
                }
                Ok(None)
            }
        }
    }

    /// Publish a partition's waiting events, in order, and record how far.
    async fn publish(&self, member: &Node) -> Result<usize, String> {
        let events = member.machine.pending_events(EVENT_BATCH);
        let Some(last) = events.last() else {
            return Ok(0);
        };
        if let Sink::EventHubs(hub) = &self.sink {
            hub.publish(member.partition, &events).await.map_err(|e| e.to_string())?;
        }
        member
            .housekeep(Housekeeping::Published { through_seq: last.seq })
            .await
            .map_err(|e| e.to_string())?;
        Ok(events.len())
    }

    /// Seal every full segment of a partition's journal in the cold tier,
    /// and record how far.
    async fn seal(&self, member: &Node) -> Result<usize, String> {
        let mut sealed = 0;
        while let Some(entries) = member.machine.next_segment(self.segment_entries) {
            let manifest = seal_segment(&self.cold, &entries).await.map_err(|e| e.to_string())?;
            member
                .housekeep(Housekeeping::Sealed {
                    through_seq: manifest.last_seq,
                })
                .await
                .map_err(|e| e.to_string())?;
            sealed += entries.len();
        }
        Ok(sealed)
    }

    /// One pass of the leader's housekeeping for every partition this node
    /// leads: move messages, events and journal entries to where they
    /// belong. Returns how many items moved. A failure is logged and the
    /// items stay with every member until a later pass, by this leader or
    /// the next, succeeds.
    pub async fn housekeep_once(&self, client: &ClusterClient) -> usize {
        if self.witness {
            self.witness_once().await;
            return 0;
        }
        self.mark_durable().await;
        let mut moved = 0;
        for member in self.groups.values() {
            if !member.raft.is_leader() {
                continue;
            }
            for (what, result) in [
                ("readers", self.register(member).await),
                ("messages", self.deliver(member, client).await),
                ("events", self.publish(member).await),
                ("journal", self.seal(member).await),
            ] {
                match result {
                    Ok(n) => moved += n,
                    Err(error) => tracing::warn!(partition = member.partition, what, %error, "housekeeping did not finish; will retry"),
                }
            }
        }
        for module in self.modules.values() {
            if !self.readers.contains(&module.name) || !module.raft.is_leader() {
                continue;
            }
            for partition in 0..self.partitions {
                match self.feed(module, partition, client).await {
                    Ok(n) => moved += n,
                    Err(error) => tracing::warn!(module = %module.name, partition, %error, "events not fed to the module; will retry"),
                }
            }
        }
        for (name, outbox) in &self.outboxes {
            let Some(module) = self.modules.get(name).filter(|m| m.raft.is_leader()) else {
                continue;
            };
            match self.carry(module, outbox, client).await {
                Ok(n) => moved += n,
                Err(error) => tracing::warn!(module = %name, %error, "the module's core commands were not all carried; will retry"),
            }
        }
        moved
    }

    /// How far every data voter holds a group's log, as its leader sees
    /// it: the least of their acknowledged positions, this leader's own
    /// included. `None` when a data voter's position is not known.
    fn durable_of<SM: RaftStateMachine<TypeConfig>>(&self, raft: &GroupOf<SM>) -> Option<LogIdOf<TypeConfig>> {
        let metrics = raft.metrics().borrow_watched().clone();
        let replication = metrics.replication.filter(|_| metrics.state.is_leader())?;
        let mut least: Option<LogIdOf<TypeConfig>> = None;
        for id in raft.voter_ids().filter(|id| !self.witnesses.contains(id)) {
            let at = (*replication.get(&id)?)?;
            least = Some(least.map_or(at, |l| l.min(at)));
        }
        least
    }

    /// Record in the log of every group this node leads how far the data
    /// voters hold it, so the witness can let go of what is behind
    /// ([`crate::witness`]). Only where a witness votes, at most every
    /// [`MARK_EVERY`], and only when something besides the last mark has
    /// been written since, so an idle group is not kept busy by marks.
    async fn mark_durable(&self) {
        if self.witnesses.is_empty() {
            return;
        }
        let mut led: Vec<(GroupKey, Option<LogIdOf<TypeConfig>>, bool)> = Vec::new();
        for (partition, member) in &self.groups {
            let voted = member.raft.voter_ids().any(|id| self.witnesses.contains(&id));
            led.push((GroupKey::Partition(*partition), self.durable_of(&member.raft), voted));
        }
        for (name, module) in &self.modules {
            let voted = module.raft.voter_ids().any(|id| self.witnesses.contains(&id));
            led.push((GroupKey::Module(name.clone()), self.durable_of(&module.raft), voted));
        }
        for (key, durable, voted) in led {
            let Some(through) = durable.filter(|_| voted) else {
                continue;
            };
            let last = self.marks.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&key).copied();
            if last.is_some_and(|m| through.index() <= m.log_index || m.at.elapsed() < MARK_EVERY) {
                continue;
            }
            let proposal = Proposal::Durable { through };
            let written = match &key {
                GroupKey::Partition(p) => group::write(&self.groups[p].raft, proposal).await,
                GroupKey::Module(m) => group::write(&self.modules[m].raft, proposal).await,
            };
            match written {
                Ok(reply) => {
                    let mark = Mark {
                        log_index: reply.log_index,
                        at: Instant::now(),
                    };
                    self.marks.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(key, mark);
                }
                Err(error) => tracing::debug!(group = %key, %error, "durable mark not written; will retry"),
            }
        }
    }

    /// A witness's own pass: take a new marker where the data voters have
    /// moved on, and hand over any group it leads.
    async fn witness_once(&self) {
        for member in self.groups.values() {
            if member.machine.witness().is_some_and(crate::witness::Witness::wants_marker) {
                let _ = member.raft.trigger().snapshot().await;
            }
        }
        for module in self.modules.values() {
            if module.machine.witness().is_some_and(crate::witness::Witness::wants_marker) {
                let _ = module.raft.trigger().snapshot().await;
            }
        }
        self.hand_off_as_witness().await;
    }

    /// A witness leads a group only when it held entries no data voter had
    /// (its leader's region was lost). As leader it sends them on; as soon
    /// as a data voter holds the whole log, the witness hands it the lead,
    /// preferring the group's home. Nothing waits for a quiet window: a
    /// witness serves no caller, so the group takes no writes until it has.
    pub async fn hand_off_as_witness(&self) -> usize {
        let mut handed = 0;
        let mut led = Vec::new();
        for (partition, member) in &self.groups {
            led.extend(fb::lead_of(&member.raft).map(|lead| (GroupKey::Partition(*partition), lead)));
        }
        for (name, module) in &self.modules {
            led.extend(fb::lead_of(&module.raft).map(|lead| (GroupKey::Module(name.clone()), lead)));
        }
        for (key, lead) in led {
            let home = self.failback.home_of(&key);
            let whole: Vec<NodeId> = lead
                .matched
                .iter()
                .filter(|(id, m)| !self.witnesses.contains(id) && **m == lead.last_log_index)
                .map(|(id, _)| *id)
                .collect();
            let Some(to) = whole.iter().find(|id| home.contains(id)).or_else(|| whole.first()).copied() else {
                continue;
            };
            let done = match &key {
                GroupKey::Partition(p) => self.groups[p].raft.trigger().transfer_leader(to).await,
                GroupKey::Module(m) => self.modules[m].raft.trigger().transfer_leader(to).await,
            };
            match done {
                Ok(()) => {
                    tracing::info!(group = %key, to, "the witness handed the lead to a data voter");
                    self.failback.witness_handed();
                    handed += 1;
                }
                Err(error) => tracing::warn!(group = %key, to, %error, "the witness could not hand over the lead; will retry"),
            }
        }
        handed
    }

    pub async fn shutdown(&self) {
        for member in self.groups.values() {
            member.shutdown().await;
        }
        for member in self.modules.values() {
            member.shutdown().await;
        }
    }
}

fn refuse(status: StatusCode, error: ProposeError) -> Response {
    (status, Json(Refusal { error })).into_response()
}

fn no_partition(partition: PartitionId) -> Response {
    refuse(StatusCode::NOT_FOUND, ProposeError::Unknown(format!("this node has no partition {partition}")))
}

fn refusal(error: ProposeError) -> Response {
    let status = match error {
        // 421: the request was sent to a server that cannot answer it.
        ProposeError::NotLeader { .. } => StatusCode::MISDIRECTED_REQUEST,
        ProposeError::Unknown(_) => StatusCode::SERVICE_UNAVAILABLE,
    };
    refuse(status, error)
}

async fn propose(State(host): State<Arc<Host>>, Path(partition): Path<PartitionId>, Json(mut envelope): Json<Envelope>) -> Response {
    let Some(member) = host.groups.get(&partition) else {
        return no_partition(partition);
    };
    // The command, and every event it makes, carries the trace of the
    // request that proposed it (outside the hash chain), so the events on
    // Event Hubs and in the modules' feeds continue that trace.
    if envelope.traceparent.is_none() {
        envelope.traceparent = ic_telemetry::traceparent();
    }
    match member.propose(envelope).await {
        Ok(reply) => Json(reply).into_response(),
        Err(e) => refusal(e),
    }
}

/// Confirm with a majority that this member still leads, then read. A
/// deposed leader therefore never answers from a stale state.
async fn read_as_leader<T: serde::Serialize>(host: &Host, partition: PartitionId, read: impl FnOnce(&Node) -> T) -> Response {
    let Some(member) = host.groups.get(&partition) else {
        return no_partition(partition);
    };
    match member.raft.ensure_linearizable(ReadPolicy::ReadIndex).await {
        Ok(_) => Json(read(member)).into_response(),
        Err(_) => {
            let (leader, addr) = member.leader_hint();
            refusal(ProposeError::NotLeader { leader, addr })
        }
    }
}

async fn end_of_day(State(host): State<Arc<Host>>, Path(partition): Path<PartitionId>) -> Response {
    read_as_leader(&host, partition, |member| host.view(member)).await
}

async fn query(State(host): State<Arc<Host>>, Path(partition): Path<PartitionId>, Json(query): Json<Question>) -> Response {
    read_as_leader(&host, partition, |member| member.machine.query(query)).await
}

async fn init(State(host): State<Arc<Host>>) -> Response {
    match host.bootstrap().await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => refusal(ProposeError::Unknown(e)),
    }
}

/// Set the voters of a partition's group. Asked of the group's leader.
async fn members(State(host): State<Arc<Host>>, Path(partition): Path<PartitionId>, Json(members): Json<BTreeMap<NodeId, String>>) -> Response {
    let Some(member) = host.groups.get(&partition) else {
        return no_partition(partition);
    };
    if members.is_empty() {
        return refuse(StatusCode::BAD_REQUEST, ProposeError::Unknown("a group needs at least one member".to_string()));
    }
    match member.set_members(members).await {
        Ok(()) => Json(()).into_response(),
        Err(e) => refusal(e),
    }
}

#[derive(serde::Deserialize)]
struct EventsQuery {
    consumer: String,
    after: Option<u64>,
    max: Option<usize>,
}

/// A run of the partition's events for a consumer, from the leader.
async fn events(State(host): State<Arc<Host>>, Path(partition): Path<PartitionId>, Query(asked): Query<EventsQuery>) -> Response {
    read_as_leader(&host, partition, |member| {
        let acknowledged = member.machine.consumers().get(&asked.consumer).copied();
        let after = asked.after.or(acknowledged).unwrap_or(0);
        EventPage {
            after,
            events: member.machine.events_after(after, asked.max.unwrap_or(EVENT_BATCH).min(EVENT_BATCH)),
            acknowledged,
        }
    })
    .await
}

async fn consumed(State(host): State<Arc<Host>>, Path((partition, consumer)): Path<(PartitionId, String)>, Json(done): Json<Consumed>) -> Response {
    let Some(member) = host.groups.get(&partition) else {
        return no_partition(partition);
    };
    let through_seq = done.through_seq;
    match member.housekeep(Housekeeping::Consumed { consumer, through_seq }).await {
        Ok(()) => Json(()).into_response(),
        Err(e) => refusal(e),
    }
}

async fn unsubscribe(State(host): State<Arc<Host>>, Path((partition, consumer)): Path<(PartitionId, String)>) -> Response {
    let Some(member) = host.groups.get(&partition) else {
        return no_partition(partition);
    };
    match member.housekeep(Housekeeping::Unsubscribed { consumer }).await {
        Ok(()) => Json(()).into_response(),
        Err(e) => refusal(e),
    }
}

async fn failback_status(State(host): State<Arc<Host>>) -> Json<FailbackStatus> {
    Json(host.failback.status())
}

async fn failback_pause(State(host): State<Arc<Host>>) -> Json<FailbackStatus> {
    host.failback.pause();
    tracing::warn!("fail-back paused by an operator");
    Json(host.failback.status())
}

async fn failback_resume(State(host): State<Arc<Host>>) -> Json<FailbackStatus> {
    host.failback.resume();
    tracing::warn!("fail-back resumed by an operator");
    Json(host.failback.status())
}

/// Hand a partition's leadership to a named voter. Asked of the leader.
/// This is how an operator reverses a fail-back, or makes any planned move.
async fn move_leader(State(host): State<Arc<Host>>, Path(partition): Path<PartitionId>, Json(to): Json<MoveLeader>) -> Response {
    let Some(member) = host.groups.get(&partition) else {
        return no_partition(partition);
    };
    if !member.raft.is_leader() {
        let (leader, addr) = member.leader_hint();
        return refusal(ProposeError::NotLeader { leader, addr });
    }
    if !member.raft.voter_ids().any(|id| id == to.to) {
        return refuse(
            StatusCode::BAD_REQUEST,
            ProposeError::Unknown(format!("node {} is not a voter of partition {partition}", to.to)),
        );
    }
    if host.witnesses.contains(&to.to) {
        return refuse(
            StatusCode::BAD_REQUEST,
            ProposeError::Unknown(format!("node {} is a witness: it holds no state and cannot lead", to.to)),
        );
    }
    match member.raft.trigger().transfer_leader(to.to).await {
        Ok(()) => Json(()).into_response(),
        Err(e) => refusal(ProposeError::Unknown(e.to_string())),
    }
}

fn no_module(module: &str) -> Response {
    refuse(StatusCode::NOT_FOUND, ProposeError::Unknown(format!("this node hosts no module {module}")))
}

async fn module_propose(State(host): State<Arc<Host>>, Path(module): Path<String>, Json(command): Json<serde_json::Value>) -> Response {
    let Some(member) = host.modules.get(&module) else {
        return no_module(&module);
    };
    match member.propose(command).await {
        Ok(reply) => Json(reply).into_response(),
        Err(e) => refusal(e),
    }
}

async fn module_query(State(host): State<Arc<Host>>, Path(module): Path<String>, Json(question): Json<serde_json::Value>) -> Response {
    let Some(member) = host.modules.get(&module) else {
        return no_module(&module);
    };
    match member.query(question).await {
        Ok(answer) => Json(answer).into_response(),
        Err(e) => refusal(e),
    }
}

/// Set the voters of a module's group. Asked of the group's leader.
async fn module_members(State(host): State<Arc<Host>>, Path(module): Path<String>, Json(members): Json<BTreeMap<NodeId, String>>) -> Response {
    let Some(member) = host.modules.get(&module) else {
        return no_module(&module);
    };
    if members.is_empty() {
        return refuse(StatusCode::BAD_REQUEST, ProposeError::Unknown("a group needs at least one member".to_string()));
    }
    match member.set_members(members).await {
        Ok(()) => Json(()).into_response(),
        Err(e) => refusal(e),
    }
}

async fn module_status(State(host): State<Arc<Host>>) -> Json<Vec<ModuleStatus>> {
    Json(host.module_statuses())
}

async fn status(State(host): State<Arc<Host>>) -> Json<Vec<NodeStatus>> {
    Json(host.statuses())
}

/// What a witness answers every caller but an operator asking its status:
/// it holds no state to read and takes no writes, so the caller tries
/// another node.
async fn witness_refuses() -> Response {
    refusal(ProposeError::NotLeader { leader: None, addr: None })
}

/// The node's HTTP surface: client routes and, per partition, Raft's own.
pub fn router(host: Arc<Host>) -> Router {
    let router = if host.witness { witness_router(&host) } else { client_router(&host) };
    raft_routes(router, &host)
}

/// A witness's surface: its status, fail-back's, and nothing a caller could use.
fn witness_router(host: &Arc<Host>) -> Router {
    Router::new()
        .route("/v1/status", get(status))
        .route("/v1/init", post(init))
        .route("/v1/failback", get(failback_status))
        .route("/v1/modules", get(module_status))
        .route("/v1/partitions/{*rest}", axum::routing::any(witness_refuses))
        .route("/v1/modules/{*rest}", axum::routing::any(witness_refuses))
        .layer(axum::middleware::from_fn(ic_telemetry::http::trace))
        .with_state(host.clone())
}

fn client_router(host: &Arc<Host>) -> Router {
    Router::new()
        .route("/v1/status", get(status))
        .route("/v1/init", post(init))
        .route("/v1/partitions/{partition}/commands", post(propose))
        .route("/v1/partitions/{partition}/end-of-day", get(end_of_day))
        .route("/v1/partitions/{partition}/query", post(query))
        .route("/v1/partitions/{partition}/members", axum::routing::put(members))
        .route("/v1/partitions/{partition}/events", get(events))
        .route(
            "/v1/partitions/{partition}/consumers/{consumer}",
            axum::routing::put(consumed).delete(unsubscribe),
        )
        .route("/v1/partitions/{partition}/leader", post(move_leader))
        .route("/v1/failback", get(failback_status))
        .route("/v1/failback/pause", post(failback_pause))
        .route("/v1/failback/resume", post(failback_resume))
        .route("/v1/modules", get(module_status))
        .route("/v1/modules/{module}/commands", post(module_propose))
        .route("/v1/modules/{module}/query", post(module_query))
        .route("/v1/modules/{module}/members", axum::routing::put(module_members))
        // A span for each client request, continuing the caller's trace. Raft's
        // own traffic, nested below, is not traced: it is heartbeats and
        // replication, many a second.
        .layer(axum::middleware::from_fn(ic_telemetry::http::trace))
        .with_state(host.clone())
}

fn raft_routes(mut router: Router, host: &Arc<Host>) -> Router {
    for (partition, member) in &host.groups {
        router = router.nest(&format!("/raft/{partition}"), network::routes(member.raft.clone(), member.identity.clone()));
    }
    for (name, member) in &host.modules {
        let path = format!("/raft/{}", ModuleGroup::group_name(name));
        router = router.nest(&path, network::routes(member.raft.clone(), member.identity.clone()));
    }
    // Everything above needs the cluster token. The probe does not: the
    // kubelet has no token, and the probe says nothing about the ledger.
    router
        .layer(axum::middleware::from_fn_with_state(host.guard.clone(), crate::access::require))
        .route("/healthz", get(|| async { "ok" }))
}

/// A node that is up: its groups, its listener and its relay.
pub struct Running {
    pub host: Arc<Host>,
    pub listen: SocketAddr,
    stop: tokio::sync::oneshot::Sender<()>,
    server: tokio::task::JoinHandle<Result<(), String>>,
    relay: tokio::task::JoinHandle<()>,
}

/// Start a node: open its logs, join its groups and listen.
pub async fn start(settings: Settings) -> Result<Running, String> {
    start_with(settings, Vec::new()).await
}

/// Start a node that also hosts a member of each module's group.
pub async fn start_with(settings: Settings, modules: Vec<ModuleSpec>) -> Result<Running, String> {
    start_full(settings, modules, None).await
}

/// Start a node that also hands leadership back to its preferred nodes.
pub async fn start_full(settings: Settings, modules: Vec<ModuleSpec>, failback: Option<FailbackSettings>) -> Result<Running, String> {
    let host = Host::start_full(&settings, &modules, failback).await?;
    let listener = tokio::net::TcpListener::bind(settings.listen)
        .await
        .map_err(|e| format!("bind {}: {e}", settings.listen))?;
    let listen = listener.local_addr().map_err(|e| e.to_string())?;
    tracing::info!(node = settings.id, %listen, partitions = settings.partitions, "ledger node listening");
    if settings.auth.is_open() {
        tracing::warn!("cluster authentication is off: any caller that reaches this port can write to the ledger");
    }
    if settings.bootstrap {
        host.bootstrap().await?;
    }
    let relay = {
        let host = host.clone();
        let client = ClusterClient::new(settings.peers.values().cloned().collect())
            .with_attempt_timeout(Duration::from_secs(2))
            .with_auth(settings.auth.clone());
        tokio::spawn(async move {
            loop {
                host.housekeep_once(&client).await;
                host.failback.pass(&host, &client, chrono::Utc::now()).await;
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        })
    };
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let app = router(host.clone());
    let server = tokio::spawn(async move {
        let graceful = async {
            let _ = stopped.await;
        };
        axum::serve(listener, app).with_graceful_shutdown(graceful).await.map_err(|e| e.to_string())
    });
    Ok(Running {
        host,
        listen,
        stop,
        server,
        relay,
    })
}

impl Running {
    /// Stop in an orderly way: finish in-flight requests, then leave the groups.
    pub async fn stop(self) -> Result<(), String> {
        let _ = self.stop.send(());
        let served = self.server.await.map_err(|e| e.to_string()).and_then(|r| r);
        self.relay.abort();
        self.host.shutdown().await;
        served
    }

    /// Stop at once, as a crash would: requests in flight are dropped.
    /// Only what reached the disk survives.
    pub async fn kill(self) {
        self.server.abort();
        self.relay.abort();
        self.host.shutdown().await;
    }
}

/// Run a node, its modules and fail-back until `stop` resolves.
pub async fn serve_full(
    settings: Settings,
    modules: Vec<ModuleSpec>,
    failback: Option<FailbackSettings>,
    stop: impl std::future::Future<Output = ()>,
) -> Result<(), String> {
    let running = start_full(settings, modules, failback).await?;
    stop.await;
    tracing::info!("ledger node stopping");
    running.stop().await
}

async fn stop_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { () = ctrl_c => {}, () = term => {} }
}

/// A node binary's whole `main`: log as JSON, read the settings from the
/// environment, host the given modules and run until the process is told
/// to stop.
pub async fn run(modules: Vec<ModuleSpec>) -> Result<(), String> {
    // JSON log lines, and traces and logs over OTLP (crates/telemetry).
    let _telemetry = ic_telemetry::init("ic-node")?;
    let settings = Settings::from_lookup(|key| std::env::var(key).ok())?;
    let failback = FailbackSettings::from_lookup(|key| std::env::var(key).ok())?;
    serve_full(settings, modules, failback, stop_signal()).await
}
