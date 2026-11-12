//! A bank for tests, in this process. It speaks the wire form [`HttpBank`]
//! speaks, checks what a bank would check, remembers what it was sent, and
//! can be told to fail in the ways banks and networks fail.
//!
//! [`HttpBank`]: crate::bank::HttpBank

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use base64::Engine;
use chrono::NaiveDate;
use ic_payments::file::{BankMessage, Batch, Manifest};
use ic_payments::iso20022;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::bank::Numbered;

/// What the bank holds under one ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    pub sha256: String,
    pub reference: String,
    /// Item IDs with their amounts, as the bank read them.
    pub items: Vec<(String, i64)>,
}

#[derive(Default)]
pub struct Inner {
    /// Bearer tokens the bank accepts: one per whitelisted region.
    pub tokens: BTreeSet<String>,
    pub held: BTreeMap<String, Held>,
    pub messages: Vec<BankMessage>,
    /// Refuse the next submission with this reason.
    pub refuse_next: Option<String>,
    /// Take the next submission and answer with an error: the sender cannot
    /// tell that it arrived.
    pub swallow_next: bool,
    /// Lose the next submission and answer with an error.
    pub drop_next: bool,
    /// Answer everything with an error.
    pub down: bool,
    /// Items to refuse at acknowledgement, with the reason.
    pub reject_items: BTreeMap<String, String>,
    /// Submissions that reached the bank, repeats included.
    pub arrivals: u64,
    /// The bank speaks ISO 20022 instead of this system's own format.
    pub iso: bool,
}

#[derive(Clone)]
pub struct FakeBank {
    pub url: String,
    inner: Arc<Mutex<Inner>>,
}

type Answer = (StatusCode, Json<Value>);

fn say(status: StatusCode, body: Value) -> Answer {
    (status, Json(body))
}

fn admit(inner: &Inner, headers: &HeaderMap) -> Result<(), Answer> {
    if inner.down {
        return Err(say(StatusCode::SERVICE_UNAVAILABLE, json!("the bank is down")));
    }
    let token = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));
    if token.is_some_and(|t| inner.tokens.contains(t)) {
        Ok(())
    } else {
        Err(say(StatusCode::FORBIDDEN, json!("this address is not on the bank's allow list")))
    }
}

/// Take a submission the bank has parsed and checked.
fn take(inner: &mut Inner, id: &str, sha256: String, items: Vec<(String, i64)>) -> Result<Held, Answer> {
    inner.arrivals += 1;
    if let Some(reason) = inner.refuse_next.take() {
        return Err(say(StatusCode::UNPROCESSABLE_ENTITY, json!(reason)));
    }
    if std::mem::take(&mut inner.drop_next) {
        return Err(say(StatusCode::BAD_GATEWAY, json!("connection reset")));
    }
    let held = match inner.held.get(id) {
        Some(held) if held.sha256 == sha256 => held.clone(),
        Some(_) => return Err(say(StatusCode::CONFLICT, json!("another file was sent under this ID"))),
        None => {
            let held = Held {
                sha256,
                reference: format!("BANK-{}", inner.held.len() + 1),
                items,
            };
            inner.held.insert(id.to_string(), held.clone());
            held
        }
    };
    if std::mem::take(&mut inner.swallow_next) {
        return Err(say(StatusCode::GATEWAY_TIMEOUT, json!("timed out")));
    }
    Ok(held)
}

fn acknowledgement(inner: &Inner, id: &str, held: &Held) -> BankMessage {
    BankMessage::Acknowledgement {
        id: id.to_string(),
        refused: None,
        rejected: held
            .items
            .iter()
            .filter_map(|(item, _)| inner.reject_items.get(item).map(|r| (item.clone(), r.clone())))
            .collect(),
    }
}

async fn put_file(State(bank): State<FakeBank>, Path(id): Path<String>, headers: HeaderMap, body: String) -> Answer {
    let mut inner = bank.lock();
    if let Err(no) = admit(&inner, &headers) {
        return no;
    }
    let bad = |why: &str| say(StatusCode::UNPROCESSABLE_ENTITY, json!(why));
    let manifest = headers.get("x-manifest").and_then(|h| base64::engine::general_purpose::STANDARD.decode(h).ok());
    let Some(manifest) = manifest.and_then(|m| serde_json::from_slice::<Manifest>(&m).ok()) else {
        return bad("no manifest");
    };
    let sha256 = hex::encode(Sha256::digest(body.as_bytes()));
    if manifest.file_id != id || manifest.sha256 != sha256 || manifest.signature.is_empty() {
        return bad("the manifest does not match the file");
    }
    let items = if inner.iso {
        match iso_items(&body, &id) {
            Ok(items) => items,
            Err(why) => return bad(&why),
        }
    } else {
        // The trailer's count and total are checked before a line is touched.
        let mut items = Vec::new();
        let mut trailer = None;
        for line in body.lines() {
            let fields: Vec<&str> = line.split('|').collect();
            match fields.as_slice() {
                ["D", item, _, _, amount] => items.push((item.to_string(), amount.parse::<i64>().unwrap_or(0))),
                ["T", count, total] => trailer = Some((count.parse::<usize>().ok(), total.parse::<i64>().ok())),
                _ => {}
            }
        }
        if trailer != Some((Some(items.len()), Some(items.iter().map(|(_, a)| a).sum()))) {
            return bad("the trailer does not match the lines");
        }
        items
    };
    match take(&mut inner, &id, sha256, items) {
        Ok(held) => say(StatusCode::CREATED, json!({"reference": held.reference})),
        Err(no) => no,
    }
}

/// An ISO 20022 initiation's items, once its header agrees with its lines
/// and its message ID is the one it was sent under.
fn iso_items(body: &str, id: &str) -> Result<Vec<(String, i64)>, String> {
    let initiation = iso20022::read_initiation(body)?;
    if initiation.message != id {
        return Err("the message ID is not the ID it was sent under".to_string());
    }
    if initiation.declared != (initiation.items.len(), initiation.items.iter().map(|(_, a)| a).sum()) {
        return Err("the count and control sum do not match the transactions".to_string());
    }
    Ok(initiation.items)
}

async fn post_batch(State(bank): State<FakeBank>, headers: HeaderMap, body: String) -> Answer {
    let mut inner = bank.lock();
    if let Err(no) = admit(&inner, &headers) {
        return no;
    }
    let key = headers.get("idempotency-key").and_then(|h| h.to_str().ok()).unwrap_or_default().to_string();
    let read = if inner.iso {
        iso_items(&body, &key).map(|items| (key.clone(), hex::encode(Sha256::digest(body.as_bytes())), items))
    } else {
        match serde_json::from_str::<Batch>(&body) {
            Ok(batch) if batch.id == key => {
                let items = batch.items.iter().map(|i| (i.item.clone(), i.amount_minor)).collect();
                batch.content_hash().map(|hash| (batch.id, hash, items))
            }
            Ok(_) => return say(StatusCode::BAD_REQUEST, json!("the idempotency key is the batch's ID")),
            Err(e) => Err(e.to_string()),
        }
    };
    let (id, sha256, items) = match read {
        Ok(read) => read,
        Err(why) => return say(StatusCode::UNPROCESSABLE_ENTITY, json!(why)),
    };
    match take(&mut inner, &id, sha256, items) {
        // An API answers for every item at once.
        Ok(held) => say(
            StatusCode::OK,
            json!({"reference": held.reference, "acknowledgement": acknowledgement(&inner, &id, &held)}),
        ),
        Err(no) => no,
    }
}

async fn get_held(State(bank): State<FakeBank>, Path(id): Path<String>, headers: HeaderMap) -> Answer {
    let inner = bank.lock();
    if let Err(no) = admit(&inner, &headers) {
        return no;
    }
    match inner.held.get(&id) {
        Some(held) => say(StatusCode::OK, json!({"sha256": held.sha256})),
        None => say(StatusCode::NOT_FOUND, json!("not held")),
    }
}

#[derive(Deserialize)]
struct After {
    #[serde(default)]
    after: u64,
}

async fn get_messages(State(bank): State<FakeBank>, Query(q): Query<After>, headers: HeaderMap) -> Answer {
    let inner = bank.lock();
    if let Err(no) = admit(&inner, &headers) {
        return no;
    }
    let messages: Vec<Numbered> = inner
        .messages
        .iter()
        .enumerate()
        .map(|(i, m)| Numbered {
            seq: i as u64 + 1,
            message: m.clone(),
        })
        .filter(|n| n.seq > q.after)
        .collect();
    say(StatusCode::OK, json!(messages))
}

impl FakeBank {
    /// Start a bank on a free local port. It accepts the tokens given.
    pub async fn start(tokens: &[&str]) -> std::io::Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let bank = Self {
            url: format!("http://{}", listener.local_addr()?),
            inner: Arc::new(Mutex::new(Inner {
                tokens: tokens.iter().map(|t| t.to_string()).collect(),
                ..Inner::default()
            })),
        };
        let app = Router::new()
            .route("/files/{id}", put(put_file).get(get_held))
            .route("/batches", post(post_batch))
            .route("/batches/{id}", get(get_held))
            .route("/messages", get(get_messages))
            .with_state(bank.clone());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(bank)
    }

    /// The bank's books, to read or to arrange a failure.
    pub fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().expect("the fake bank's books")
    }

    /// Send the acknowledgement file for something the bank holds.
    pub fn acknowledge(&self, id: &str) {
        let mut inner = self.lock();
        if let Some(held) = inner.held.get(id).cloned() {
            let message = acknowledgement(&inner, id, &held);
            inner.messages.push(message);
        }
    }

    pub fn settle(&self, id: &str, settled_on: NaiveDate) {
        self.push(BankMessage::Settlement {
            id: id.to_string(),
            settled_on,
        });
    }

    pub fn push(&self, message: BankMessage) {
        self.lock().messages.push(message);
    }
}
