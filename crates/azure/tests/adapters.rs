//! The adapters against a local stand-in for the Azure REST endpoints, and
//! the Merkle root and cold tier paths against a real simulated ledger.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{post, put};
use axum::{Json, Router};
use chrono::NaiveDate;
use ic_azure::blob::{BlobContainer, MemoryWormStore, WormStore};
use ic_azure::cold_tier::{fetch_segment, seal_segment};
use ic_azure::credential::AzureCredential;
use ic_azure::eventhubs::{EventHubsSink, EventSink};
use ic_azure::keyvault::{DevSigner, KeyVaultSigner, RootSigner};
use ic_azure::merkle::{publish_root, root_name, verify_published, PublishedRoot};
use ic_azure::AzureError;
use ic_ledger::verify_chain;
use ic_product::{Frequency, Sex};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, term_life};
use ic_sim::Cluster;
use serde_json::{json, Value};

#[derive(Clone, Default)]
struct Fake {
    blobs: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    seen: Arc<Mutex<Vec<String>>>,
}

fn bearer(h: &HeaderMap) -> String {
    h.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string()
}

async fn token(State(f): State<Fake>, body: String) -> Json<Value> {
    f.seen.lock().unwrap().push(format!("token {body}"));
    Json(json!({ "access_token": "tok-from-entra", "expires_in": 3600, "token_type": "Bearer" }))
}

async fn put_blob(State(f): State<Fake>, Path((container, name)): Path<(String, String)>, headers: HeaderMap, body: Bytes) -> StatusCode {
    if bearer(&headers) != "Bearer tok-from-entra" || headers.get("x-ms-blob-type").is_none() || headers.get("if-none-match").is_none() {
        return StatusCode::FORBIDDEN;
    }
    let mut blobs = f.blobs.lock().unwrap();
    let key = format!("{container}/{name}");
    if blobs.contains_key(&key) {
        return StatusCode::CONFLICT;
    }
    blobs.insert(key, body.to_vec());
    StatusCode::CREATED
}

async fn get_blob(State(f): State<Fake>, Path((container, name)): Path<(String, String)>) -> Result<Vec<u8>, StatusCode> {
    f.blobs
        .lock()
        .unwrap()
        .get(&format!("{container}/{name}"))
        .cloned()
        .ok_or(StatusCode::NOT_FOUND)
}

async fn sign(State(f): State<Fake>, Path(key): Path<String>, headers: HeaderMap, Json(body): Json<Value>) -> Result<Json<Value>, StatusCode> {
    if bearer(&headers) != "Bearer tok-from-entra" || body["alg"] != "ES256" {
        return Err(StatusCode::FORBIDDEN);
    }
    f.seen.lock().unwrap().push(format!("sign {key}"));
    Ok(Json(
        json!({ "kid": format!("https://fake.vault/keys/{key}/v7"), "value": format!("sig-over-{}", body["value"].as_str().unwrap()) }),
    ))
}

async fn send(State(f): State<Fake>, Path((hub, partition)): Path<(String, String)>, headers: HeaderMap, body: String) -> StatusCode {
    if headers.get("content-type").and_then(|v| v.to_str().ok()) != Some("application/vnd.microsoft.servicebus.json") {
        return StatusCode::BAD_REQUEST;
    }
    let batch: Vec<Value> = serde_json::from_str(&body).unwrap();
    f.seen.lock().unwrap().push(format!("send {hub}/{partition} x{}", batch.len()));
    StatusCode::CREATED
}

async fn fake_azure() -> (String, Fake) {
    let fake = Fake::default();
    let app = Router::new()
        .route("/{tenant}/oauth2/v2.0/token", post(token))
        .route("/{container}/{*name}", put(put_blob).get(get_blob))
        .route("/keys/{key}/sign", post(sign))
        .route("/{hub}/partitions/{partition}/messages", post(send))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), fake)
}

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn small_ledger() -> Cluster {
    let mut c = Cluster::in_memory(2, 1);
    c.open(d(2027, 11, 15)).unwrap();
    for cmd in shipped_activations().unwrap() {
        c.activate_everywhere(cmd).unwrap();
    }
    for n in 1..=10 {
        let issue = term_life(n, d(2027, 11, 15), d(1985, 1, 1), Sex::Male, false, 300_000_00, None, Frequency::Monthly, None);
        let p = c.partition_of(&issue.group);
        c.submit(p, "new-business", issue_command(issue)).unwrap();
    }
    c
}

#[tokio::test]
async fn workload_identity_blob_key_vault_and_event_hubs_speak_the_expected_protocols() {
    let (base, fake) = fake_azure().await;
    let token_file = std::env::temp_dir().join(format!("ic-azure-federated-{}", std::process::id()));
    std::fs::write(&token_file, "projected-service-account-token\n").unwrap();
    let cred = AzureCredential::workload_identity(&base, "tenant-1", "client-1", token_file.to_str().unwrap());

    // Blob Storage: write once, read back, refuse to overwrite.
    let store = BlobContainer::new(&base, "merkle-roots", cred.clone());
    store
        .put_new("merkle-roots/2027/2027-11-15.json", "application/json", b"{}".to_vec())
        .await
        .unwrap();
    assert_eq!(store.get("merkle-roots/2027/2027-11-15.json").await.unwrap(), Some(b"{}".to_vec()));
    assert!(matches!(
        store.put_new("merkle-roots/2027/2027-11-15.json", "application/json", b"x".to_vec()).await,
        Err(AzureError::AlreadyExists(_))
    ));
    assert_eq!(store.get("nothing-here").await.unwrap(), None);

    // Key Vault: the response names the key version that signed.
    let sig = KeyVaultSigner::new(&base, "merkle-root", cred.clone()).sign(b"root").await.unwrap();
    assert_eq!(sig.key_id, "https://fake.vault/keys/merkle-root/v7");
    assert!(sig.value.starts_with("sig-over-"));

    // Event Hubs: ledger partition n goes to hub partition n.
    let mut c = small_ledger();
    let events = c.take_events();
    let p1: Vec<_> = events.iter().filter(|e| e.partition == 1).cloned().collect();
    EventHubsSink::new(&base, "ledger-events", cred).publish(1, &p1).await.unwrap();

    let seen = fake.seen.lock().unwrap().clone();
    // one token exchange per scope, carrying the federated assertion; later calls use the cache
    let tokens: Vec<_> = seen.iter().filter(|s| s.starts_with("token ")).collect();
    assert_eq!(tokens.len(), 3, "{seen:?}");
    assert!(tokens[0].contains("client_assertion=projected-service-account-token") && tokens[0].contains("client_id=client-1"));
    assert!(tokens[0].contains("scope=https%3A%2F%2Fstorage.azure.com%2F.default"));
    assert!(seen.iter().any(|s| s == "sign merkle-root"));
    assert!(seen.iter().any(|s| s.starts_with("send ledger-events/1 x")));
    let _ = std::fs::remove_file(token_file);
}

#[tokio::test]
async fn the_days_merkle_root_is_signed_published_once_and_verifiable() {
    let mut c = small_ledger();
    let close = c.end_of_day().unwrap();
    let store = MemoryWormStore::default();
    let published = publish_root(&DevSigner, &store, close.closed, &close.heads).await.unwrap();
    assert_eq!(published.root, close.merkle_root);
    assert!(verify_published(&published));
    // publishing the same day again changes nothing
    assert_eq!(publish_root(&DevSigner, &store, close.closed, &close.heads).await.unwrap(), published);
    // a different root for a closed day is refused: closed days never change
    let mut tampered = close.heads.clone();
    tampered.insert(0, ic_ledger::Hash32::ZERO);
    assert!(publish_root(&DevSigner, &store, close.closed, &tampered).await.is_err());
    let stored: PublishedRoot = serde_json::from_slice(&store.get(&root_name(close.closed)).await.unwrap().unwrap()).unwrap();
    assert_eq!(stored, published);
}

#[tokio::test]
async fn sealed_segments_link_to_the_chain_and_replay_from_the_cold_tier() {
    let mut c = small_ledger();
    for _ in 0..40 {
        c.end_of_day().unwrap();
    }
    let journal = c.nodes[&0].journal.clone();
    assert!(journal.len() > 100);
    let store = MemoryWormStore::default();
    let cut = journal.len() / 2;
    let a = seal_segment(&store, &journal[..cut]).await.unwrap();
    let b = seal_segment(&store, &journal[cut..]).await.unwrap();
    // segments link to each other exactly as entries do
    assert_eq!(a.last_hash, b.prev_hash);
    assert_eq!(b.last_hash, c.state(0).ledger.head());
    // sealing the same entries again changes nothing: a new leader repeats
    // what a failed one may already have done
    assert_eq!(seal_segment(&store, &journal[..cut]).await.unwrap(), a);
    // different content over a sealed range is refused by the write-once store
    let mut altered = journal[..cut].to_vec();
    altered[0].salt.push('x');
    assert!(matches!(seal_segment(&store, &altered).await, Err(AzureError::AlreadyExists(_))));
    // a gap is refused
    assert!(seal_segment(&store, &[journal[0].clone(), journal[2].clone()]).await.is_err());
    // fetched back, the two segments verify as one chain
    let mut all = fetch_segment(&store, 0, a.first_seq, a.last_seq).await.unwrap();
    all.extend(fetch_segment(&store, 0, b.first_seq, b.last_seq).await.unwrap());
    assert_eq!(verify_chain(0, &all).unwrap(), c.state(0).ledger.head());
}
