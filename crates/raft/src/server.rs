//! A ledger node as a process: one member of every partition's Raft group,
//! served over HTTP.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use ic_core::event::MessageBody;
use ic_core::{recon, Command, Envelope, Policy};
use ic_ledger::PartitionId;
use openraft::raft::ReadPolicy;
use openraft::BasicNode;

use crate::client::{BreakView, ClusterClient, EndOfDayView, Refusal};
use crate::network::{self, Network};
use crate::node::{self, Node, NodeStatus, ProposeError};
use crate::store::LogStore;
use crate::types::NodeId;

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
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let get = |key: &str| get(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
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
        })
    }
}

/// One node: a member of each partition's group.
pub struct Host {
    pub id: NodeId,
    pub groups: BTreeMap<PartitionId, Node>,
    peers: BTreeMap<NodeId, String>,
}

impl Host {
    /// Open the logs and start every group's member. Existing logs are
    /// replayed, so a restarted node comes back with the state it had.
    pub async fn start(settings: &Settings) -> Result<Arc<Self>, String> {
        let mut groups = BTreeMap::new();
        for partition in 0..settings.partitions {
            let dir = settings.data_dir.join(format!("partition-{partition}"));
            let log = LogStore::open(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            let config = node::config(&format!("partition-{partition}"), settings.heartbeat_ms)?;
            let member = Node::start(settings.id, partition, log, Network::http(settings.id, partition), config).await?;
            groups.insert(partition, member);
        }
        Ok(Arc::new(Self {
            id: settings.id,
            groups,
            peers: settings.peers.clone(),
        }))
    }

    /// Form every group from the configured peers. A group that already
    /// exists is left as it is.
    pub async fn bootstrap(&self) -> Result<(), String> {
        let members: BTreeMap<NodeId, BasicNode> = self.peers.iter().map(|(id, addr)| (*id, BasicNode::new(addr.clone()))).collect();
        for member in self.groups.values() {
            member.initialize(members.clone()).await?;
        }
        Ok(())
    }

    pub fn statuses(&self) -> Vec<NodeStatus> {
        self.groups.values().map(Node::status).collect()
    }

    fn view(&self, member: &Node) -> EndOfDayView {
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
        })
    }

    /// One pass of the cross-partition relay. The leader of a partition
    /// delivers the messages its commands produced, at least once, with the
    /// message ID as the receiver's idempotency key. Returns how many were
    /// delivered.
    pub async fn relay_once(&self, client: &ClusterClient) -> usize {
        let mut delivered = 0;
        for member in self.groups.values() {
            if !member.raft.is_leader() {
                continue;
            }
            let pending = member.machine.take_messages();
            let mut undelivered = Vec::new();
            for message in pending {
                let command = match message.body.clone() {
                    MessageBody::TransferRecord(record) => Command::LandTransfer { record },
                    MessageBody::TransferLanded { policy, transfer_id } => Command::CompleteTransfer { policy, transfer_id },
                    MessageBody::TransferRejected { policy, transfer_id, .. } => Command::AbortTransfer { policy, transfer_id },
                };
                let envelope = Envelope {
                    key: message.id.clone(),
                    caller: "transfer".to_string(),
                    // Derived from the message so a redelivery is byte-identical.
                    salt: format!("relay:{}", message.id),
                    traceparent: None,
                    command,
                };
                match client.propose(message.to_partition, &envelope).await {
                    Ok(_) => delivered += 1,
                    Err(e) => {
                        tracing::warn!(message = %message.id, error = %e, "relay delivery failed; will retry");
                        undelivered.push(message);
                    }
                }
            }
            member.machine.return_messages(undelivered);
        }
        delivered
    }

    pub async fn shutdown(&self) {
        for member in self.groups.values() {
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

async fn propose(State(host): State<Arc<Host>>, Path(partition): Path<PartitionId>, Json(envelope): Json<Envelope>) -> Response {
    let Some(member) = host.groups.get(&partition) else {
        return no_partition(partition);
    };
    match member.propose(envelope).await {
        Ok(reply) => Json(reply).into_response(),
        Err(e) => refusal(e),
    }
}

async fn end_of_day(State(host): State<Arc<Host>>, Path(partition): Path<PartitionId>) -> Response {
    let Some(member) = host.groups.get(&partition) else {
        return no_partition(partition);
    };
    // Confirm with a majority that this member still leads before reading,
    // so a deposed leader never reports a stale day as finished.
    match member.raft.ensure_linearizable(ReadPolicy::ReadIndex).await {
        Ok(_) => Json(host.view(member)).into_response(),
        Err(_) => {
            let status = member.status();
            let addr = status.leader.and_then(|l| host.peers.get(&l).cloned());
            refusal(ProposeError::NotLeader { leader: status.leader, addr })
        }
    }
}

async fn init(State(host): State<Arc<Host>>) -> Response {
    match host.bootstrap().await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => refusal(ProposeError::Unknown(e)),
    }
}

async fn status(State(host): State<Arc<Host>>) -> Json<Vec<NodeStatus>> {
    Json(host.statuses())
}

/// The node's HTTP surface: client routes and, per partition, Raft's own.
pub fn router(host: Arc<Host>) -> Router {
    let mut router = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/status", get(status))
        .route("/v1/init", post(init))
        .route("/v1/partitions/{partition}/commands", post(propose))
        .route("/v1/partitions/{partition}/end-of-day", get(end_of_day))
        .with_state(host.clone());
    for (partition, member) in &host.groups {
        router = router.nest(&format!("/raft/{partition}"), network::routes(member.raft.clone()));
    }
    router
}

/// Run a node until `stop` resolves.
pub async fn serve(settings: Settings, stop: impl std::future::Future<Output = ()> + Send + 'static) -> Result<(), String> {
    let host = Host::start(&settings).await?;
    let listener = tokio::net::TcpListener::bind(settings.listen)
        .await
        .map_err(|e| format!("bind {}: {e}", settings.listen))?;
    tracing::info!(node = settings.id, listen = %settings.listen, partitions = settings.partitions, "ledger node listening");
    if settings.bootstrap {
        host.bootstrap().await?;
    }
    let relay = {
        let host = host.clone();
        let client = ClusterClient::new(settings.peers.values().cloned().collect()).with_attempt_timeout(Duration::from_secs(2));
        tokio::spawn(async move {
            loop {
                host.relay_once(&client).await;
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        })
    };
    let served = axum::serve(listener, router(host.clone()))
        .with_graceful_shutdown(stop)
        .await
        .map_err(|e| e.to_string());
    relay.abort();
    host.shutdown().await;
    served
}
