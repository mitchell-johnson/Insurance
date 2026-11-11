//! A local stand-in for the Azure endpoints the adapters call, for tests in
//! this workspace: the Entra ID token endpoint, Blob Storage with write-once
//! names, Key Vault signing and Event Hubs. It can be switched off to play
//! an outage. Built only with the `testing` feature.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{post, put};
use axum::{Json, Router};
use serde_json::{json, Value};

/// The token the stand-in issues and then insists on.
pub const TOKEN: &str = "tok-from-entra";

#[derive(Clone, Default)]
pub struct FakeAzure {
    /// `http://127.0.0.1:<port>`: use it as every endpoint and as the authority.
    pub base: String,
    blobs: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    seen: Arc<Mutex<Vec<String>>>,
    down: Arc<AtomicBool>,
}

fn bearer_ok(h: &HeaderMap) -> bool {
    h.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {TOKEN}"))
}

impl FakeAzure {
    pub async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind a local port");
        let fake = Self {
            base: format!("http://{}", listener.local_addr().expect("local address")),
            ..Self::default()
        };
        let app = Router::new()
            .route("/{tenant}/oauth2/v2.0/token", post(token))
            .route("/{container}/{*name}", put(put_blob).get(get_blob))
            .route("/keys/{key}/sign", post(sign))
            .route("/{hub}/partitions/{partition}/messages", post(send))
            .with_state(fake.clone());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        fake
    }

    /// While down, storage, signing and the event stream answer 503. Tokens
    /// are still issued.
    pub fn set_down(&self, down: bool) {
        self.down.store(down, Ordering::SeqCst);
    }

    fn is_down(&self) -> bool {
        self.down.load(Ordering::SeqCst)
    }

    /// One line per call that was served: `token <form>`, `sign <key>`,
    /// `send <hub>/<partition> x<events>`.
    pub fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    fn saw(&self, line: String) {
        self.seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(line);
    }

    /// Every stored blob, by `container/name`.
    pub fn blobs(&self) -> BTreeMap<String, Vec<u8>> {
        self.blobs.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    /// Events accepted for a hub partition, in total.
    pub fn events_sent(&self, hub: &str, partition: u16) -> usize {
        let prefix = format!("send {hub}/{partition} x");
        self.seen().iter().filter_map(|s| s.strip_prefix(&prefix)?.parse::<usize>().ok()).sum()
    }
}

async fn token(State(f): State<FakeAzure>, body: String) -> Json<Value> {
    f.saw(format!("token {body}"));
    Json(json!({ "access_token": TOKEN, "expires_in": 3600, "token_type": "Bearer" }))
}

async fn put_blob(State(f): State<FakeAzure>, Path((container, name)): Path<(String, String)>, headers: HeaderMap, body: Bytes) -> StatusCode {
    if f.is_down() {
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    if !bearer_ok(&headers) || headers.get("x-ms-blob-type").is_none() || headers.get("if-none-match").is_none() {
        return StatusCode::FORBIDDEN;
    }
    let mut blobs = f.blobs.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let key = format!("{container}/{name}");
    if blobs.contains_key(&key) {
        return StatusCode::CONFLICT;
    }
    blobs.insert(key, body.to_vec());
    StatusCode::CREATED
}

async fn get_blob(State(f): State<FakeAzure>, Path((container, name)): Path<(String, String)>) -> Result<Vec<u8>, StatusCode> {
    if f.is_down() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    f.blobs().get(&format!("{container}/{name}")).cloned().ok_or(StatusCode::NOT_FOUND)
}

async fn sign(State(f): State<FakeAzure>, Path(key): Path<String>, headers: HeaderMap, Json(body): Json<Value>) -> Result<Json<Value>, StatusCode> {
    if f.is_down() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    if !bearer_ok(&headers) || body["alg"] != "ES256" {
        return Err(StatusCode::FORBIDDEN);
    }
    f.saw(format!("sign {key}"));
    let digest = body["value"].as_str().unwrap_or_default();
    Ok(Json(
        json!({ "kid": format!("https://fake.vault/keys/{key}/v7"), "value": format!("sig-over-{digest}") }),
    ))
}

async fn send(State(f): State<FakeAzure>, Path((hub, partition)): Path<(String, String)>, headers: HeaderMap, body: String) -> StatusCode {
    if f.is_down() {
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    if !bearer_ok(&headers) || headers.get("content-type").and_then(|v| v.to_str().ok()) != Some("application/vnd.microsoft.servicebus.json") {
        return StatusCode::BAD_REQUEST;
    }
    let Ok(batch) = serde_json::from_str::<Vec<Value>>(&body) else {
        return StatusCode::BAD_REQUEST;
    };
    f.saw(format!("send {hub}/{partition} x{}", batch.len()));
    StatusCode::CREATED
}
