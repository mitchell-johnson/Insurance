//! Raft RPCs between the members of a group.
//!
//! Two transports share one connection type. [`Network::http`] is what nodes
//! in separate processes use. [`Network::local`] connects members inside one
//! process and can cut a member off or sever a single link, which is how the
//! tests lose nodes and partition the network without real sockets.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use ic_ledger::PartitionId;
use openraft::alias::{SnapshotOf, VoteOf};
use openraft::errors::{RPCError, ReplicationClosed, StreamingError, Unreachable};
use openraft::network::RPCOption;
use openraft::raft::{AppendEntriesRequest, AppendEntriesResponse, AppendResult, SnapshotResponse, VoteRequest, VoteResponse};
use openraft::{AnyError, BasicNode, RaftNetworkFactory, RaftNetworkV2};

use crate::auth::ClusterAuth;
use crate::machine::Machine;
use crate::types::{NodeId, TypeConfig};

/// A member's handle on its Raft group.
pub type RaftGroup = openraft::Raft<TypeConfig, Machine>;

#[derive(Default)]
struct LocalState {
    members: BTreeMap<NodeId, RaftGroup>,
    /// Members that are down: nothing reaches them and they reach nothing.
    down: BTreeSet<NodeId>,
    /// Severed links, stored with the smaller ID first.
    cut: BTreeSet<(NodeId, NodeId)>,
}

/// An in-process network for one group, with fault injection.
#[derive(Clone, Default)]
pub struct LocalNet {
    state: Arc<Mutex<LocalState>>,
}

impl LocalNet {
    fn lock(&self) -> MutexGuard<'_, LocalState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn register(&self, id: NodeId, raft: RaftGroup) {
        let mut s = self.lock();
        s.members.insert(id, raft);
        s.down.remove(&id);
    }

    /// Take a member off the network, as if its machine had lost power.
    pub fn isolate(&self, id: NodeId) {
        self.lock().down.insert(id);
    }

    /// Put a member back on the network.
    pub fn restore(&self, id: NodeId) {
        self.lock().down.remove(&id);
    }

    /// Sever, or mend, the link between two members in both directions.
    pub fn set_link(&self, a: NodeId, b: NodeId, connected: bool) {
        let key = (a.min(b), a.max(b));
        let mut s = self.lock();
        if connected {
            s.cut.remove(&key);
        } else {
            s.cut.insert(key);
        }
    }

    fn route(&self, from: NodeId, to: NodeId) -> Result<RaftGroup, Unreachable<TypeConfig>> {
        let s = self.lock();
        let blocked = s.down.contains(&from) || s.down.contains(&to) || s.cut.contains(&(from.min(to), from.max(to)));
        match s.members.get(&to) {
            Some(raft) if !blocked => Ok(raft.clone()),
            _ => Err(unreachable(format!("node {to} is unreachable from node {from}"))),
        }
    }
}

fn unreachable(message: impl ToString) -> Unreachable<TypeConfig> {
    Unreachable::new(&AnyError::error(message.to_string()))
}

#[derive(Clone)]
enum Transport {
    Local(LocalNet),
    Http {
        client: reqwest::Client,
        partition: PartitionId,
        auth: ClusterAuth,
    },
}

/// The network factory handed to openraft: it makes one connection per peer.
#[derive(Clone)]
pub struct Network {
    from: NodeId,
    transport: Transport,
}

impl Network {
    pub fn local(from: NodeId, net: LocalNet) -> Self {
        Self {
            from,
            transport: Transport::Local(net),
        }
    }

    /// Peers are reached at `http://{addr}/raft/{partition}/...`.
    pub fn http(from: NodeId, partition: PartitionId, auth: ClusterAuth) -> Self {
        Self {
            from,
            transport: Transport::Http {
                client: reqwest::Client::new(),
                partition,
                auth,
            },
        }
    }
}

/// A connection from one member to one peer.
pub struct Connection {
    from: NodeId,
    to: NodeId,
    addr: String,
    transport: Transport,
}

impl RaftNetworkFactory<TypeConfig> for Network {
    type Network = Connection;

    async fn new_client(&mut self, target: NodeId, node: &BasicNode) -> Connection {
        Connection {
            from: self.from,
            to: target,
            addr: node.addr.clone(),
            transport: self.transport.clone(),
        }
    }
}

/// What a peer answers over HTTP: the RPC's reply, or why it could not be served.
type Wire<T> = Result<T, String>;

impl Connection {
    async fn post<Req, Resp>(
        &self,
        client: &reqwest::Client,
        auth: &ClusterAuth,
        partition: PartitionId,
        path: &str,
        body: &Req,
        option: &RPCOption,
    ) -> Result<Resp, Unreachable<TypeConfig>>
    where
        Req: serde::Serialize,
        Resp: serde::de::DeserializeOwned,
    {
        let url = format!("http://{}/raft/{partition}/{path}", self.addr);
        let response = auth
            .sign(client.post(url))
            .timeout(option.soft_ttl())
            .json(body)
            .send()
            .await
            .map_err(unreachable)?;
        let wire: Wire<Resp> = response.error_for_status().map_err(unreachable)?.json().await.map_err(unreachable)?;
        // A peer that answered with an error is treated as unreachable so the
        // sender backs off instead of retrying at once.
        wire.map_err(unreachable)
    }
}

impl RaftNetworkV2<TypeConfig> for Connection {
    type SnapshotData = ();

    async fn append_entries(&mut self, rpc: AppendEntriesRequest<TypeConfig>, option: RPCOption) -> Result<AppendResult<TypeConfig>, RPCError<TypeConfig>> {
        match &self.transport {
            Transport::Local(net) => Ok(net.route(self.from, self.to)?.append_entries(rpc).await.map_err(unreachable)?),
            Transport::Http { client, partition, auth } => {
                let (prev, last) = (rpc.prev_log_id, rpc.last_log_id());
                let response: AppendEntriesResponse<TypeConfig> = self.post(client, auth, *partition, "append", &rpc, &option).await?;
                Ok(response.into_append_result(prev, last))
            }
        }
    }

    async fn vote(&mut self, rpc: VoteRequest<TypeConfig>, option: RPCOption) -> Result<VoteResponse<TypeConfig>, RPCError<TypeConfig>> {
        match &self.transport {
            Transport::Local(net) => Ok(net.route(self.from, self.to)?.vote(rpc).await.map_err(unreachable)?),
            Transport::Http { client, partition, auth } => Ok(self.post(client, auth, *partition, "vote", &rpc, &option).await?),
        }
    }

    async fn pre_vote(&mut self, rpc: VoteRequest<TypeConfig>, option: RPCOption) -> Result<VoteResponse<TypeConfig>, RPCError<TypeConfig>> {
        match &self.transport {
            Transport::Local(net) => Ok(net.route(self.from, self.to)?.pre_vote(rpc).await.map_err(unreachable)?),
            Transport::Http { client, partition, auth } => Ok(self.post(client, auth, *partition, "pre-vote", &rpc, &option).await?),
        }
    }

    async fn full_snapshot(
        &mut self,
        _vote: VoteOf<TypeConfig>,
        _snapshot: SnapshotOf<TypeConfig, ()>,
        _cancel: impl Future<Output = ReplicationClosed> + Send + 'static,
        _option: RPCOption,
    ) -> Result<SnapshotResponse<TypeConfig>, StreamingError<TypeConfig>> {
        // The log is never purged, so a member is always caught up from the log.
        Err(StreamingError::Unreachable(unreachable("snapshots are not used in this phase")))
    }
}

/// The receiving side: routes that hand a peer's RPC to this member's Raft.
/// Mount under `/raft/{partition}`.
pub fn routes(raft: RaftGroup) -> Router {
    async fn append(State(raft): State<RaftGroup>, Json(rpc): Json<AppendEntriesRequest<TypeConfig>>) -> Json<Wire<AppendEntriesResponse<TypeConfig>>> {
        Json(raft.append_entries(rpc).await.map(AppendEntriesResponse::from).map_err(|e| e.to_string()))
    }
    async fn vote(State(raft): State<RaftGroup>, Json(rpc): Json<VoteRequest<TypeConfig>>) -> Json<Wire<VoteResponse<TypeConfig>>> {
        Json(raft.vote(rpc).await.map_err(|e| e.to_string()))
    }
    async fn pre_vote(State(raft): State<RaftGroup>, Json(rpc): Json<VoteRequest<TypeConfig>>) -> Json<Wire<VoteResponse<TypeConfig>>> {
        Json(raft.pre_vote(rpc).await.map_err(|e| e.to_string()))
    }
    Router::new()
        .route("/append", post(append))
        .route("/vote", post(vote))
        .route("/pre-vote", post(pre_vote))
        .with_state(raft)
}
