//! A local stand-in for the Azure services the documents driver calls:
//! Blob Storage (with deletion and retention locks), Key Vault key
//! wrapping, Communication Services email and SMS, Event Hubs, and the
//! customer master's contact-point route. Any of them can be made to
//! answer with a chosen status to play an outage or a refusal.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ic_azure::credential::AzureCredential;
use serde_json::{json, Value};

pub const TOKEN: &str = "tok";

#[derive(Default)]
pub struct Inner {
    pub blobs: BTreeMap<String, Vec<u8>>,
    /// Blob name to the `until` header it was written with.
    pub locks: BTreeMap<String, String>,
    /// Service name to the status it answers with instead of working.
    pub force: BTreeMap<String, u16>,
    /// Service name to a body it answers with instead of its own.
    pub bodies: BTreeMap<String, Value>,
    pub emails: Vec<(String, Value)>,
    pub sms: Vec<Value>,
    pub events: Vec<Value>,
    pub contacts: BTreeMap<String, Value>,
    pub contact_queries: Vec<BTreeMap<String, String>>,
    /// `target` (`party:<id>`) to the customer master's answer.
    pub relationships: BTreeMap<String, Value>,
    pub calls: Vec<String>,
}

#[derive(Clone, Default)]
pub struct Fake {
    pub base: String,
    pub inner: Arc<Mutex<Inner>>,
}

type Reply = Result<Json<Value>, StatusCode>;

impl Fake {
    pub async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let fake = Self {
            base: format!("http://{}", listener.local_addr().unwrap()),
            ..Self::default()
        };
        let app = Router::new()
            .route("/blob/{container}/{*name}", put(put_blob).get(get_blob).delete(delete_blob))
            .route("/keys/{key}/wrapkey", post(wrap))
            .route("/keys/{key}/{version}/unwrapkey", post(unwrap))
            .route("/emails:send", post(email))
            .route("/sms", post(sms))
            .route("/eh/{hub}/partitions/{partition}/messages", post(events))
            .route("/v1/parties/{party}/contact-points", get(contact))
            .route("/v1/relationships", get(relationships))
            .with_state(fake.clone());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        fake
    }

    pub fn credential() -> AzureCredential {
        AzureCredential::static_token(TOKEN)
    }

    pub fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap()
    }

    /// Make a service answer with a status; `0` puts it back to work.
    pub fn force(&self, service: &str, status: u16) {
        if status == 0 {
            self.lock().force.remove(service);
        } else {
            self.lock().force.insert(service.to_string(), status);
        }
    }

    pub fn body(&self, service: &str, body: Value) {
        self.lock().bodies.insert(service.to_string(), body);
    }

    fn gate(&self, service: &str, headers: &HeaderMap) -> Result<(), StatusCode> {
        let mut inner = self.lock();
        inner.calls.push(service.to_string());
        if let Some(status) = inner.force.get(service) {
            return Err(StatusCode::from_u16(*status).unwrap());
        }
        if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some(&format!("Bearer {TOKEN}")) {
            return Err(StatusCode::UNAUTHORIZED);
        }
        Ok(())
    }

    fn canned(&self, service: &str) -> Option<Value> {
        self.lock().bodies.get(service).cloned()
    }
}

async fn put_blob(State(f): State<Fake>, Path((container, name)): Path<(String, String)>, headers: HeaderMap, body: Bytes) -> StatusCode {
    if let Err(status) = f.gate("blob", &headers) {
        return status;
    }
    if headers.get("x-ms-blob-type").is_none() || headers.get("if-none-match").is_none() {
        return StatusCode::BAD_REQUEST;
    }
    let key = format!("{container}/{name}");
    let mut inner = f.lock();
    if inner.blobs.contains_key(&key) {
        return StatusCode::CONFLICT;
    }
    if let Some(until) = headers.get("x-ms-immutability-policy-until-date") {
        if headers.get("x-ms-immutability-policy-mode").and_then(|v| v.to_str().ok()) != Some("Locked") {
            return StatusCode::BAD_REQUEST;
        }
        inner.locks.insert(key.clone(), until.to_str().unwrap().to_string());
    }
    inner.blobs.insert(key, body.to_vec());
    StatusCode::CREATED
}

async fn get_blob(State(f): State<Fake>, Path((container, name)): Path<(String, String)>, headers: HeaderMap) -> Result<Vec<u8>, StatusCode> {
    f.gate("blob", &headers)?;
    f.lock().blobs.get(&format!("{container}/{name}")).cloned().ok_or(StatusCode::NOT_FOUND)
}

async fn delete_blob(State(f): State<Fake>, Path((container, name)): Path<(String, String)>, headers: HeaderMap) -> StatusCode {
    if let Err(status) = f.gate("blob", &headers) {
        return status;
    }
    let key = format!("{container}/{name}");
    let mut inner = f.lock();
    if inner.locks.contains_key(&key) {
        return StatusCode::CONFLICT;
    }
    match inner.blobs.remove(&key) {
        Some(_) => StatusCode::ACCEPTED,
        None => StatusCode::NOT_FOUND,
    }
}

fn flip(value: &Value) -> String {
    let mut bytes = URL_SAFE_NO_PAD.decode(value.as_str().unwrap_or_default()).unwrap_or_default();
    bytes.reverse();
    URL_SAFE_NO_PAD.encode(bytes)
}

async fn wrap(State(f): State<Fake>, Path(key): Path<String>, headers: HeaderMap, Json(body): Json<Value>) -> Reply {
    f.gate("keyvault", &headers)?;
    if body["alg"] != "RSA-OAEP-256" {
        return Err(StatusCode::BAD_REQUEST);
    }
    if let Some(canned) = f.canned("wrap") {
        return Ok(Json(canned));
    }
    Ok(Json(json!({ "kid": format!("{}/keys/{key}/v7", f.base), "value": flip(&body["value"]) })))
}

async fn unwrap(State(f): State<Fake>, Path((key, version)): Path<(String, String)>, headers: HeaderMap, Json(body): Json<Value>) -> Reply {
    f.gate("keyvault", &headers)?;
    if version != "v7" {
        return Err(StatusCode::NOT_FOUND);
    }
    if let Some(canned) = f.canned("unwrap") {
        return Ok(Json(canned));
    }
    Ok(Json(
        json!({ "kid": format!("{}/keys/{key}/{version}", f.base), "value": flip(&body["value"]) }),
    ))
}

async fn email(State(f): State<Fake>, headers: HeaderMap, Json(body): Json<Value>) -> Result<(StatusCode, Json<Value>), StatusCode> {
    f.gate("email", &headers)?;
    let id = headers.get("operation-id").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
    let reply = f.canned("email").unwrap_or(json!({ "id": id, "status": "Running" }));
    let mut inner = f.lock();
    // The provider sends a repeated operation once.
    if !inner.emails.iter().any(|(seen, _)| *seen == id) {
        inner.emails.push((id, body));
    }
    Ok((StatusCode::ACCEPTED, Json(reply)))
}

async fn sms(State(f): State<Fake>, headers: HeaderMap, Json(body): Json<Value>) -> Result<(StatusCode, Json<Value>), StatusCode> {
    f.gate("sms", &headers)?;
    let to = body["smsRecipients"][0]["to"].clone();
    let reply = f
        .canned("sms")
        .unwrap_or(json!({ "value": [{ "to": to, "messageId": format!("sms-{}", f.lock().sms.len() + 1), "httpStatusCode": 202, "successful": true }] }));
    f.lock().sms.push(body);
    Ok((StatusCode::ACCEPTED, Json(reply)))
}

async fn events(State(f): State<Fake>, Path((_hub, partition)): Path<(String, String)>, headers: HeaderMap, body: String) -> StatusCode {
    if let Err(status) = f.gate("eventhubs", &headers) {
        return status;
    }
    if partition != "0" || headers.get("content-type").and_then(|v| v.to_str().ok()) != Some("application/vnd.microsoft.servicebus.json") {
        return StatusCode::BAD_REQUEST;
    }
    let batch: Vec<Value> = serde_json::from_str(&body).unwrap_or_default();
    f.lock().events.extend(batch);
    StatusCode::CREATED
}

async fn contact(State(f): State<Fake>, Path(party): Path<String>, Query(query): Query<BTreeMap<String, String>>, headers: HeaderMap) -> Reply {
    f.gate("directory", &headers)?;
    let key = format!("{party}/{}", query.get("channel").cloned().unwrap_or_default());
    let mut inner = f.lock();
    inner.contact_queries.push(query);
    inner.contacts.get(&key).cloned().map(Json).ok_or(StatusCode::NOT_FOUND)
}

async fn relationships(State(f): State<Fake>, Query(query): Query<BTreeMap<String, String>>, headers: HeaderMap) -> Reply {
    f.gate("directory", &headers)?;
    let target = query.get("target").cloned().unwrap_or_default();
    let answer = f.lock().relationships.get(&target).cloned();
    Ok(Json(answer.unwrap_or_else(|| json!({ "target": target, "relationships": [] }))))
}
