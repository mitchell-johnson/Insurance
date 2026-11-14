//! Which workload is calling a node, and what it may do there.
//!
//! Without an access map every holder of the cluster token may do
//! everything: propose any core command under any caller name, write to any
//! module, move leaders. With one (`IC_CLUSTER_ACCESS_FILE`), each workload
//! presents its own token and is held to its grant:
//!
//! - **peer**: the ledger nodes themselves. Everything, including Raft's own
//!   RPCs between members.
//! - **callers**: the caller names it may put on a core command
//!   (`Envelope::caller`), which is what the core's `allowed_callers`
//!   decides on. A workload that may not name `claims` cannot send a claims
//!   command, whatever its token.
//! - **write_modules**, **read_modules**: the modules it may propose to and
//!   ask. Writing a module includes reading it.
//! - **read_partitions**: whether it may read the partitions (queries,
//!   events, end-of-day views, consumer positions).
//! - **admin**: forming groups, changing members, moving leaders, pausing
//!   fail-back.
//!
//! Every workload may read the node's status. Nothing but a peer reaches
//! Raft's RPCs.
//!
//! The map holds SHA-256 digests of the tokens, never the tokens: the
//! tokens stay in Key Vault, one secret per workload, mounted only into that
//! workload's pods. Two digests per workload let a token be rotated with no
//! restart, and the file is read again every [`RELOAD_EVERY`], so adding a
//! workload or a digest needs no restart either.
//!
//! `mode: report` lets everything through and logs what enforcement would
//! refuse (target `cluster.access`), so a map can be checked against a live
//! system before it is enforced. `shared_token: accept` keeps the old shared
//! token working, with full access, while workloads move to their own; it is
//! logged on every use.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::auth::ClusterAuth;

/// How often the map is read again from its file.
pub const RELOAD_EVERY: Duration = Duration::from_secs(30);

/// The most a core command may be, when its caller has to be read.
pub const MAX_COMMAND_BODY: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Enforce,
    Report,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SharedToken {
    #[default]
    Refuse,
    Accept,
}

/// What one workload may do.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    /// Hex SHA-256 of each token this workload may present.
    pub token_sha256: Vec<String>,
    #[serde(default)]
    pub peer: bool,
    #[serde(default)]
    pub admin: bool,
    #[serde(default)]
    pub read_partitions: bool,
    #[serde(default)]
    pub callers: BTreeSet<String>,
    #[serde(default)]
    pub write_modules: BTreeSet<String>,
    #[serde(default)]
    pub read_modules: BTreeSet<String>,
}

/// The access map.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Access {
    #[serde(default)]
    pub mode: Mode,
    #[serde(default)]
    pub shared_token: SharedToken,
    pub workloads: BTreeMap<String, Grant>,
}

/// Who a request is from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Who {
    /// A workload in the map.
    Workload(String),
    /// The shared cluster token, while it is still accepted.
    Shared,
}

/// What a request asks to do, worked out from its method and path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ask {
    Status,
    Raft,
    Admin,
    ReadPartition,
    /// A core command; the caller it names is read from the body.
    Command,
    WriteModule(String),
    ReadModule(String),
    Unknown,
}

impl Ask {
    pub fn of(method: &Method, path: &str) -> Ask {
        let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
        match (method.as_str(), parts.as_slice()) {
            (_, ["raft", ..]) => Ask::Raft,
            ("GET", ["v1", "status"]) | ("GET", ["v1", "modules"]) | ("GET", ["v1", "failback"]) => Ask::Status,
            ("POST", ["v1", "init"])
            | ("PUT", ["v1", "partitions", _, "members"])
            | ("POST", ["v1", "partitions", _, "leader"])
            | ("POST", ["v1", "failback", "pause" | "resume"])
            | ("PUT", ["v1", "modules", _, "members"]) => Ask::Admin,
            ("POST", ["v1", "partitions", _, "commands"]) => Ask::Command,
            ("POST", ["v1", "partitions", _, "query"])
            | ("GET", ["v1", "partitions", _, "end-of-day" | "events"])
            | ("PUT" | "DELETE", ["v1", "partitions", _, "consumers", _]) => Ask::ReadPartition,
            ("POST", ["v1", "modules", module, "commands"]) => Ask::WriteModule((*module).to_string()),
            ("POST", ["v1", "modules", module, "query"]) => Ask::ReadModule((*module).to_string()),
            _ => Ask::Unknown,
        }
    }
}

fn digest(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

impl Access {
    /// Read and check a map.
    pub fn parse(text: &str) -> Result<Self, String> {
        let access: Access = serde_json::from_str(text).map_err(|e| format!("the access map is not valid: {e}"))?;
        let mut seen = BTreeMap::new();
        for (name, grant) in &access.workloads {
            if grant.token_sha256.is_empty() {
                return Err(format!("workload {name} lists no token digest"));
            }
            for d in &grant.token_sha256 {
                if d.len() != 64 || !d.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
                    return Err(format!("workload {name}: {d:?} is not a lower-case hex SHA-256"));
                }
                if let Some(other) = seen.insert(d.clone(), name.clone()) {
                    return Err(format!("workloads {other} and {name} share a token"));
                }
            }
        }
        if !access.workloads.values().any(|g| g.peer) {
            return Err("no workload is a peer: the nodes could not reach each other".to_string());
        }
        Ok(access)
    }

    /// Whose token this is. `shared` is the node's own accepted shared tokens.
    pub fn who(&self, token: &str, shared: &ClusterAuth) -> Option<Who> {
        let presented = digest(token);
        for (name, grant) in &self.workloads {
            if grant.token_sha256.contains(&presented) {
                return Some(Who::Workload(name.clone()));
            }
        }
        (self.shared_token == SharedToken::Accept && shared.admits(Some(&format!("Bearer {token}")))).then_some(Who::Shared)
    }

    /// Whether `who` may do `ask`; for a core command, naming `caller`.
    pub fn decide(&self, who: &Who, ask: &Ask, caller: Option<&str>) -> Result<(), String> {
        let Who::Workload(name) = who else {
            return Ok(());
        };
        let Some(grant) = self.workloads.get(name) else {
            return Err(format!("{name} is no longer in the access map"));
        };
        if grant.peer {
            return Ok(());
        }
        let allowed = match ask {
            Ask::Status => true,
            Ask::Raft | Ask::Unknown => false,
            Ask::Admin => grant.admin,
            Ask::ReadPartition => grant.read_partitions,
            Ask::Command => caller.is_some_and(|c| grant.callers.contains(c)),
            Ask::WriteModule(m) => grant.write_modules.contains(m),
            Ask::ReadModule(m) => grant.read_modules.contains(m) || grant.write_modules.contains(m),
        };
        if allowed {
            Ok(())
        } else {
            Err(match ask {
                Ask::Command => format!("{name} may not send core commands as {}", caller.unwrap_or("(no caller)")),
                other => format!("{name} may not {other:?}"),
            })
        }
    }
}

/// The map a node enforces, read again from its file now and then.
#[derive(Clone)]
pub struct Guard {
    shared: ClusterAuth,
    map: Option<Arc<RwLock<Arc<Access>>>>,
    file: Option<PathBuf>,
}

impl Guard {
    /// The shared token only: the behaviour before access maps.
    pub fn shared(shared: ClusterAuth) -> Self {
        Self { shared, map: None, file: None }
    }

    /// Enforce `access`, falling back to `shared` only as the map allows.
    pub fn with_map(shared: ClusterAuth, access: Access) -> Self {
        Self {
            shared,
            map: Some(Arc::new(RwLock::new(Arc::new(access)))),
            file: None,
        }
    }

    /// A node's guard: its shared tokens, and the access map its settings
    /// name (`IC_CLUSTER_ACCESS_FILE`), read through `read`.
    pub fn for_node(shared: &ClusterAuth, read: impl Fn(&str) -> std::io::Result<String>) -> Result<Self, String> {
        let Some(path) = shared.access_file() else {
            return Ok(Self::shared(shared.clone()));
        };
        if shared.is_open() {
            return Err("IC_CLUSTER_ACCESS_FILE needs the node's own token in IC_CLUSTER_TOKEN_FILE, not IC_CLUSTER_AUTH=open".to_string());
        }
        let access = Access::parse(&read(path).map_err(|e| format!("IC_CLUSTER_ACCESS_FILE {path}: {e}"))?)?;
        let own = shared.outgoing().unwrap_or_default();
        match access.who(own, &ClusterAuth::open()) {
            Some(Who::Workload(name)) if access.workloads[&name].peer => {}
            _ => return Err("this node's own token (the first in IC_CLUSTER_TOKEN_FILE) is not a peer in the access map".to_string()),
        }
        let mut guard = Self::with_map(shared.clone(), access);
        guard.file = Some(PathBuf::from(path));
        Ok(guard)
    }

    pub fn map(&self) -> Option<Arc<Access>> {
        self.map.as_ref().map(|m| m.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone())
    }

    /// Read the file again. A map that does not parse is logged and the
    /// last good one kept.
    pub fn reload(&self) -> Result<bool, String> {
        let (Some(map), Some(file)) = (&self.map, &self.file) else {
            return Ok(false);
        };
        let access = std::fs::read_to_string(file)
            .map_err(|e| format!("{}: {e}", file.display()))
            .and_then(|text| Access::parse(&text));
        match access {
            Ok(access) => {
                let mut current = map.write().unwrap_or_else(std::sync::PoisonError::into_inner);
                let changed = **current != access;
                if changed {
                    *current = Arc::new(access);
                    tracing::info!(target: "cluster.access", "access map reloaded");
                }
                Ok(changed)
            }
            Err(e) => {
                tracing::error!(target: "cluster.access", error = %e, "access map not reloaded; the last good one stays");
                Err(e)
            }
        }
    }

    /// Read the file again every [`RELOAD_EVERY`] for as long as the node runs.
    pub fn watch(&self) -> Option<tokio::task::JoinHandle<()>> {
        self.file.as_ref()?;
        let guard = self.clone();
        Some(tokio::spawn(async move {
            let mut every = tokio::time::interval(RELOAD_EVERY);
            every.tick().await;
            loop {
                every.tick().await;
                let _ = guard.reload();
            }
        }))
    }
}

fn refuse(status: StatusCode, why: &str) -> Response {
    let mut response = (status, why.to_string()).into_response();
    if status == StatusCode::UNAUTHORIZED {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, "Bearer".parse().expect("a header value"));
    }
    response
}

#[derive(Deserialize)]
struct Caller {
    caller: String,
}

/// Middleware: who is calling, and may they do this.
pub async fn require(State(guard): State<Guard>, request: Request, next: Next) -> Response {
    let authorization = request.headers().get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).map(str::to_string);
    let Some(access) = guard.map() else {
        return if guard.shared.admits(authorization.as_deref()) {
            next.run(request).await
        } else {
            refuse(StatusCode::UNAUTHORIZED, "a cluster token is required")
        };
    };
    let Some(who) = authorization
        .as_deref()
        .and_then(|v| v.strip_prefix("Bearer "))
        .and_then(|token| access.who(token, &guard.shared))
    else {
        return refuse(StatusCode::UNAUTHORIZED, "a cluster token is required");
    };
    if who == Who::Shared {
        tracing::warn!(target: "cluster.access", path = %request.uri().path(), "the shared cluster token was used");
    }
    let ask = Ask::of(request.method(), request.uri().path());
    let (request, caller) = if ask == Ask::Command && matches!(&who, Who::Workload(name) if !access.workloads[name].peer) {
        let (parts, body) = request.into_parts();
        let Ok(bytes) = axum::body::to_bytes(body, MAX_COMMAND_BODY).await else {
            return refuse(StatusCode::PAYLOAD_TOO_LARGE, "the command is too large");
        };
        let caller = serde_json::from_slice::<Caller>(&bytes).ok().map(|c| c.caller);
        (Request::from_parts(parts, Body::from(bytes)), caller)
    } else {
        (request, None)
    };
    match access.decide(&who, &ask, caller.as_deref()) {
        Ok(()) => next.run(request).await,
        Err(why) if access.mode == Mode::Report => {
            tracing::warn!(target: "cluster.access", path = %request.uri().path(), refused = %why, "would be refused");
            next.run(request).await
        }
        Err(why) => {
            tracing::warn!(target: "cluster.access", path = %request.uri().path(), refused = %why, "refused");
            refuse(StatusCode::FORBIDDEN, &why)
        }
    }
}
