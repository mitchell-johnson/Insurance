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
use openraft::alias::{SnapshotMetaOf, SnapshotOf, VoteOf};
use openraft::errors::{RPCError, ReplicationClosed, StreamingError, Unreachable};
use openraft::network::RPCOption;
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, AppendResult, SnapshotResponse, TransferLeaderRequest, TransferLeaderResponse, VoteRequest, VoteResponse,
};
use openraft::storage::RaftStateMachine;
use openraft::{AnyError, BasicNode, RaftNetworkFactory, RaftNetworkV2};

use crate::auth::ClusterAuth;
use crate::machine::Machine;
use crate::peers::{Identity, Impostor, EXPECTS_HEADER, INCARNATION_HEADER, NODE_HEADER};
use crate::types::{NodeId, TypeConfig};

/// A member's handle on its Raft group.
pub type RaftGroup = openraft::Raft<TypeConfig, Machine>;

/// A member's handle on a group over any state machine whose snapshot is its
/// own JSON text: a partition's, or a module's.
pub type GroupOf<SM> = openraft::Raft<TypeConfig, SM>;

#[derive(Default)]
struct LocalState {
    members: BTreeMap<NodeId, (RaftGroup, Identity)>,
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

    pub fn register(&self, identity: Identity, raft: RaftGroup) {
        let mut s = self.lock();
        let id = identity.id;
        s.members.insert(id, (raft, identity));
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

    fn route(&self, from: &Identity, to: NodeId) -> Result<RaftGroup, Unreachable<TypeConfig>> {
        let s = self.lock();
        let blocked = s.down.contains(&from.id) || s.down.contains(&to) || s.cut.contains(&(from.id.min(to), from.id.max(to)));
        match s.members.get(&to) {
            Some((raft, target)) if !blocked => {
                // Each end checks the other, as the HTTP transport does.
                target.admit(from.id, &from.incarnation).map_err(unreachable)?;
                from.admit(to, &target.incarnation).map_err(unreachable)?;
                Ok(raft.clone())
            }
            _ => Err(unreachable(format!("node {to} is unreachable from node {}", from.id))),
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
        /// The group's name in the path: a partition's number, or `module-<name>`.
        group: String,
        auth: ClusterAuth,
    },
}

/// The network factory handed to openraft: it makes one connection per peer.
#[derive(Clone)]
pub struct Network {
    from: Identity,
    transport: Transport,
}

impl Network {
    pub fn local(from: Identity, net: LocalNet) -> Self {
        Self {
            from,
            transport: Transport::Local(net),
        }
    }

    /// Peers are reached at `http://{addr}/raft/{partition}/...`.
    pub fn http(from: Identity, partition: PartitionId, auth: ClusterAuth) -> Self {
        Self::http_group(from, partition.to_string(), auth)
    }

    /// Peers are reached at `http://{addr}/raft/{group}/...`.
    pub fn http_group(from: Identity, group: String, auth: ClusterAuth) -> Self {
        Self {
            from,
            transport: Transport::Http {
                client: reqwest::Client::new(),
                group,
                auth,
            },
        }
    }
}

/// A connection from one member to one peer.
pub struct Connection {
    from: Identity,
    to: NodeId,
    addr: String,
    transport: Transport,
}

impl RaftNetworkFactory<TypeConfig> for Network {
    type Network = Connection;

    async fn new_client(&mut self, target: NodeId, node: &BasicNode) -> Connection {
        Connection {
            from: self.from.clone(),
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
        group: &str,
        path: &str,
        body: &Req,
        option: &RPCOption,
    ) -> Result<Resp, Unreachable<TypeConfig>>
    where
        Req: serde::Serialize,
        Resp: serde::de::DeserializeOwned,
    {
        let url = format!("http://{}/raft/{group}/{path}", self.addr);
        let mut request = auth
            .sign(client.post(url))
            .header(NODE_HEADER, self.from.id)
            .header(INCARNATION_HEADER, &self.from.incarnation);
        // Say which incarnation of the peer this message is meant for, so a
        // node that has lost its disk does not act on it before we find out.
        if let Some(expected) = self.from.known(self.to) {
            request = request.header(EXPECTS_HEADER, expected);
        }
        let response = request.timeout(option.soft_ttl()).json(body).send().await.map_err(unreachable)?;
        // Nothing a peer says reaches Raft until the peer is known to be the
        // member it claims to be.
        let incarnation = response.headers().get(INCARNATION_HEADER).and_then(|v| v.to_str().ok()).map(str::to_string);
        match incarnation {
            Some(incarnation) => self.from.admit(self.to, &incarnation).map_err(|impostor| {
                tracing::error!(peer = self.to, "{impostor}");
                unreachable(impostor)
            })?,
            None => return Err(unreachable(format!("node {} did not identify itself", self.to))),
        }
        let wire: Wire<Resp> = response.error_for_status().map_err(unreachable)?.json().await.map_err(unreachable)?;
        // A peer that answered with an error is treated as unreachable so the
        // sender backs off instead of retrying at once.
        wire.map_err(unreachable)
    }
}

impl RaftNetworkV2<TypeConfig> for Connection {
    type SnapshotData = Vec<u8>;

    async fn append_entries(&mut self, rpc: AppendEntriesRequest<TypeConfig>, option: RPCOption) -> Result<AppendResult<TypeConfig>, RPCError<TypeConfig>> {
        match &self.transport {
            Transport::Local(net) => Ok(net.route(&self.from, self.to)?.append_entries(rpc).await.map_err(unreachable)?),
            Transport::Http { client, group, auth } => {
                let (prev, last) = (rpc.prev_log_id, rpc.last_log_id());
                let response: AppendEntriesResponse<TypeConfig> = self.post(client, auth, group, "append", &rpc, &option).await?;
                Ok(response.into_append_result(prev, last))
            }
        }
    }

    async fn vote(&mut self, rpc: VoteRequest<TypeConfig>, option: RPCOption) -> Result<VoteResponse<TypeConfig>, RPCError<TypeConfig>> {
        match &self.transport {
            Transport::Local(net) => Ok(net.route(&self.from, self.to)?.vote(rpc).await.map_err(unreachable)?),
            Transport::Http { client, group, auth } => Ok(self.post(client, auth, group, "vote", &rpc, &option).await?),
        }
    }

    async fn pre_vote(&mut self, rpc: VoteRequest<TypeConfig>, option: RPCOption) -> Result<VoteResponse<TypeConfig>, RPCError<TypeConfig>> {
        match &self.transport {
            Transport::Local(net) => Ok(net.route(&self.from, self.to)?.pre_vote(rpc).await.map_err(unreachable)?),
            Transport::Http { client, group, auth } => Ok(self.post(client, auth, group, "pre-vote", &rpc, &option).await?),
        }
    }

    /// A leader handing over tells every member who is to lead next, so the
    /// chosen one stands at once and the others do not wait out the lease.
    async fn transfer_leader(
        &mut self,
        rpc: TransferLeaderRequest<TypeConfig>,
        option: RPCOption,
    ) -> Result<TransferLeaderResponse<TypeConfig>, RPCError<TypeConfig>> {
        match &self.transport {
            Transport::Local(net) => Ok(net.route(&self.from, self.to)?.handle_transfer_leader(rpc).await.map_err(unreachable)?),
            Transport::Http { client, group, auth } => Ok(self.post(client, auth, group, "transfer-leader", &rpc, &option).await?),
        }
    }

    async fn full_snapshot(
        &mut self,
        vote: VoteOf<TypeConfig>,
        snapshot: SnapshotOf<TypeConfig, Vec<u8>>,
        _cancel: impl Future<Output = ReplicationClosed> + Send + 'static,
        option: RPCOption,
    ) -> Result<SnapshotResponse<TypeConfig>, StreamingError<TypeConfig>> {
        match &self.transport {
            Transport::Local(net) => Ok(net
                .route(&self.from, self.to)?
                .install_full_snapshot(vote, snapshot)
                .await
                .map_err(unreachable)?),
            Transport::Http { client, group, auth } => {
                let wire = SnapshotWire {
                    vote,
                    meta: snapshot.meta,
                    state: String::from_utf8(snapshot.snapshot).map_err(unreachable)?,
                };
                Ok(self.post(client, auth, group, "snapshot", &wire, &option).await?)
            }
        }
    }
}

/// A snapshot on the wire. The state is the snapshot file's own JSON text.
#[derive(serde::Serialize, serde::Deserialize)]
struct SnapshotWire {
    vote: VoteOf<TypeConfig>,
    meta: SnapshotMetaOf<TypeConfig>,
    state: String,
}

/// Raft messages carry batches of entries and whole snapshots.
const MAX_RAFT_BODY: usize = 1 << 30;

/// Check who is calling before a Raft message is handled, and tell the
/// caller who answered.
async fn identify(State(me): State<Identity>, request: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
    use axum::response::IntoResponse;
    let caller = {
        let header = |name: &str| request.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
        header(NODE_HEADER).and_then(|id| id.parse::<NodeId>().ok()).zip(header(INCARNATION_HEADER))
    };
    let meant_for_another = request
        .headers()
        .get(EXPECTS_HEADER)
        .is_some_and(|expected| expected.as_bytes() != me.incarnation.as_bytes());
    let mut response = match caller {
        None => (axum::http::StatusCode::BAD_REQUEST, "Raft callers must identify themselves").into_response(),
        Some((id, _)) if meant_for_another => {
            tracing::error!(
                peer = id,
                "node {id} knows this node by another incarnation: this disk is not the one the group knows. Replace this node as a new member with a new number."
            );
            (axum::http::StatusCode::CONFLICT, "this node is not the member the caller knows").into_response()
        }
        Some((id, incarnation)) => match me.admit(id, &incarnation) {
            Ok(()) => next.run(request).await,
            Err(impostor @ Impostor { .. }) => {
                tracing::error!(peer = id, "{impostor}");
                (axum::http::StatusCode::CONFLICT, impostor.to_string()).into_response()
            }
        },
    };
    if let Ok(value) = me.incarnation.parse() {
        response.headers_mut().insert(INCARNATION_HEADER, value);
    }
    response
}

/// The receiving side: routes that hand a peer's RPC to this member's Raft.
/// Mount under `/raft/{group}`.
pub fn routes<SM>(raft: GroupOf<SM>, identity: Identity) -> Router
where
    SM: RaftStateMachine<TypeConfig, SnapshotData = Vec<u8>>,
{
    async fn append<SM>(State(raft): State<GroupOf<SM>>, Json(rpc): Json<AppendEntriesRequest<TypeConfig>>) -> Json<Wire<AppendEntriesResponse<TypeConfig>>>
    where
        SM: RaftStateMachine<TypeConfig, SnapshotData = Vec<u8>>,
    {
        Json(raft.append_entries(rpc).await.map(AppendEntriesResponse::from).map_err(|e| e.to_string()))
    }
    async fn vote<SM>(State(raft): State<GroupOf<SM>>, Json(rpc): Json<VoteRequest<TypeConfig>>) -> Json<Wire<VoteResponse<TypeConfig>>>
    where
        SM: RaftStateMachine<TypeConfig, SnapshotData = Vec<u8>>,
    {
        Json(raft.vote(rpc).await.map_err(|e| e.to_string()))
    }
    async fn pre_vote<SM>(State(raft): State<GroupOf<SM>>, Json(rpc): Json<VoteRequest<TypeConfig>>) -> Json<Wire<VoteResponse<TypeConfig>>>
    where
        SM: RaftStateMachine<TypeConfig, SnapshotData = Vec<u8>>,
    {
        Json(raft.pre_vote(rpc).await.map_err(|e| e.to_string()))
    }
    async fn snapshot<SM>(State(raft): State<GroupOf<SM>>, Json(wire): Json<SnapshotWire>) -> Json<Wire<SnapshotResponse<TypeConfig>>>
    where
        SM: RaftStateMachine<TypeConfig, SnapshotData = Vec<u8>>,
    {
        let snapshot = SnapshotOf::<TypeConfig, Vec<u8>> {
            meta: wire.meta,
            snapshot: wire.state.into_bytes(),
        };
        Json(raft.install_full_snapshot(wire.vote, snapshot).await.map_err(|e| e.to_string()))
    }
    async fn transfer_leader<SM>(
        State(raft): State<GroupOf<SM>>,
        Json(rpc): Json<TransferLeaderRequest<TypeConfig>>,
    ) -> Json<Wire<TransferLeaderResponse<TypeConfig>>>
    where
        SM: RaftStateMachine<TypeConfig, SnapshotData = Vec<u8>>,
    {
        Json(raft.handle_transfer_leader(rpc).await.map_err(|e| e.to_string()))
    }
    Router::new()
        .route("/transfer-leader", post(transfer_leader::<SM>))
        .route("/append", post(append::<SM>))
        .route("/vote", post(vote::<SM>))
        .route("/pre-vote", post(pre_vote::<SM>))
        .route("/snapshot", post(snapshot::<SM>))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_RAFT_BODY))
        .layer(axum::middleware::from_fn_with_state(identity, identify))
        .with_state(raft)
}
