//! What the API tests share: a way to call the router as each kind of
//! caller, and a cluster of ledger nodes in this process.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::Router;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use ic_core::{Command, Envelope};
use ic_ledger::PartitionId;
use ic_raft::client::fresh_salt;
use ic_raft::server::{self, ColdTarget, EventTarget, Running, Settings};
use ic_raft::{ClusterAuth, ClusterClient, NodeId};
use serde_json::Value;
use tower::ServiceExt;

pub fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

pub struct Call<'a> {
    method: &'a str,
    path: &'a str,
    subject: Option<&'a str>,
    roles: &'a str,
    relationships: &'a str,
    key: Option<&'a str>,
    body: Option<Value>,
    headers: Vec<(&'a str, &'a str)>,
}

impl<'a> Call<'a> {
    pub fn new(method: &'a str, path: &'a str) -> Self {
        Self {
            method,
            path,
            subject: Some("svc"),
            roles: "",
            relationships: "",
            key: None,
            body: None,
            headers: Vec::new(),
        }
    }
    pub fn roles(mut self, r: &'a str) -> Self {
        self.roles = r;
        self
    }
    pub fn viewer(mut self, subject: &'a str, relationships: &'a str) -> Self {
        self.subject = Some(subject);
        self.relationships = relationships;
        self
    }
    pub fn anonymous(mut self) -> Self {
        self.subject = None;
        self
    }
    pub fn header(mut self, name: &'a str, value: &'a str) -> Self {
        self.headers.push((name, value));
        self
    }
    pub fn key(mut self, k: &'a str) -> Self {
        self.key = Some(k);
        self
    }
    pub fn body(mut self, b: Value) -> Self {
        self.body = Some(b);
        self
    }
    pub async fn send(self, app: &Router) -> (StatusCode, Value) {
        let (status, _, body) = self.send_full(app).await;
        (status, body)
    }

    /// The call with the response's headers as well.
    pub async fn send_full(self, app: &Router) -> (StatusCode, HeaderMap, Value) {
        let mut req = Request::builder()
            .method(self.method)
            .uri(self.path)
            .header("content-type", "application/json")
            .header("x-dev-roles", self.roles)
            .header("x-dev-relationships", self.relationships);
        if let Some(s) = self.subject {
            req = req.header("x-dev-subject", s);
        }
        if let Some(k) = self.key {
            req = req.header("idempotency-key", k);
        }
        for (name, value) in &self.headers {
            req = req.header(*name, *value);
        }
        let body = self.body.map_or_else(Body::empty, |b| Body::from(b.to_string()));
        let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (status, headers, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }
}

/// The cluster token of the test ledgers.
pub const TOKEN: &str = "test-cluster-token-0123456789abcdef";

pub const PATIENCE: Duration = Duration::from_secs(30);

/// A port below the range the operating system gives outgoing connections,
/// so no client socket can take it between choosing it and binding it.
pub fn free_port() -> u16 {
    static NEXT: AtomicU16 = AtomicU16::new(0);
    loop {
        let offset = NEXT.fetch_add(1, Ordering::Relaxed);
        let port = 10_000 + ((std::process::id() as u16).wrapping_mul(59).wrapping_add(offset) % 9_000);
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
}

pub fn scratch(name: &str) -> PathBuf {
    static RUN: AtomicU64 = AtomicU64::new(0);
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{}-{}", std::process::id(), RUN.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A cluster of ledger nodes in this process, each with its own disk.
pub struct Ledger {
    pub partitions: PartitionId,
    pub settings: BTreeMap<NodeId, Settings>,
    pub running: BTreeMap<NodeId, Running>,
    pub client: ClusterClient,
    pub auth: ClusterAuth,
    pub dir: PathBuf,
    /// The modules each node hosts.
    pub modules: fn() -> Vec<ic_raft::ModuleSpec>,
}

impl Ledger {
    pub async fn start(name: &str, size: u64, partitions: PartitionId) -> Self {
        Self::start_hosting(name, size, partitions, Vec::new).await
    }

    /// The same, with each node hosting `modules`.
    pub async fn start_hosting(name: &str, size: u64, partitions: PartitionId, modules: fn() -> Vec<ic_raft::ModuleSpec>) -> Self {
        let dir = scratch(name);
        let peers: BTreeMap<NodeId, String> = (1..=size).map(|id| (id, format!("127.0.0.1:{}", free_port()))).collect();
        let auth = ClusterAuth::tokens(vec![TOKEN.to_string()]).unwrap();
        let settings = peers
            .iter()
            .map(|(id, addr)| {
                let s = Settings {
                    id: *id,
                    listen: addr.parse().unwrap(),
                    data_dir: dir.join(format!("node-{id}")),
                    partitions,
                    peers: peers.clone(),
                    bootstrap: *id == 1,
                    heartbeat_ms: 50,
                    auth: auth.clone(),
                    cold: ColdTarget::Dir(dir.join("cold")),
                    events: EventTarget::Discard,
                    segment_entries: 50,
                    // Small, so the read models cross a snapshot in the tests.
                    snapshot_every: 20,
                };
                (*id, s)
            })
            .collect();
        let client = ClusterClient::new(peers.values().cloned().collect())
            .with_attempt_timeout(Duration::from_secs(2))
            .with_auth(auth.clone());
        let mut ledger = Self {
            partitions,
            settings,
            running: BTreeMap::new(),
            client,
            auth,
            dir,
            modules,
        };
        for id in 1..=size {
            ledger.boot(id).await;
        }
        ledger
    }

    pub fn nodes(&self) -> Vec<String> {
        self.client.nodes().to_vec()
    }

    pub async fn boot(&mut self, id: NodeId) {
        let running = server::start_with(self.settings[&id].clone(), (self.modules)()).await.unwrap();
        self.running.insert(id, running);
    }

    /// Crash a node: its requests are dropped and its memory is gone.
    pub async fn kill(&mut self, id: NodeId) {
        self.running.remove(&id).expect("the node is not running").kill().await;
    }

    pub fn leader_of(&self, partition: PartitionId) -> Option<NodeId> {
        self.running.iter().find(|(_, r)| r.host.groups[&partition].raft.is_leader()).map(|(id, _)| *id)
    }

    /// Put a command to a partition as one of the core's own callers, which
    /// the API never lets in from outside.
    pub async fn run(&self, partition: PartitionId, key: &str, caller: &str, command: Command) {
        let envelope = Envelope {
            key: key.to_string(),
            caller: caller.to_string(),
            salt: fresh_salt(),
            traceparent: None,
            command,
        };
        let reply = self.client.propose_until(partition, &envelope, PATIENCE).await.unwrap();
        assert_eq!(reply.rejected, None, "{key}");
    }

    pub async fn stop(mut self) {
        for id in self.running.keys().copied().collect::<Vec<_>>() {
            self.kill(id).await;
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
