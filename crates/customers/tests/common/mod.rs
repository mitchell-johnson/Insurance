//! A customer master over a real PostgreSQL server, the Azure stand-in
//! (Key Vault, Blob Storage, Event Hubs) and a stand-in Entra tenant that
//! signs tokens with a key it publishes.
//!
//! The server is named by `IC_TEST_POSTGRES_URL` (CI starts one). Each
//! test gets a database of its own, dropped when the test ends. Without
//! the variable the tests say so and pass, so a workspace test run on a
//! machine without PostgreSQL is not blocked; CI always sets it.

#![allow(dead_code)]

use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use http_body_util::BodyExt;
use ic_azure::blob::MemoryWormStore;
use ic_azure::credential::AzureCredential;
use ic_azure::keywrap::KeyVaultWrapper;
use ic_azure::testing::{FakeAzure, TOKEN};
use ic_customers::access::Access;
use ic_customers::auth::Entra;
use ic_customers::crypto::IndexKey;
use ic_customers::db::{self, Connector};
use ic_customers::keys::KeyRing;
use ic_customers::retention::Register;
use ic_customers::service::{router, App};
use ic_customers::store::Store;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::RsaPrivateKey;
use serde_json::{json, Value};
use tower::ServiceExt;

pub const TENANT: &str = "tenant-1";
pub const AUDIENCE: &str = "api://customer-master";
pub const INDEX_KEY: &str = "an-index-key-of-at-least-32-bytes!!";

pub type TestApp = App<KeyVaultWrapper, MemoryWormStore>;

struct Key {
    signing: EncodingKey,
    jwk: Value,
}

fn key() -> &'static Key {
    static KEY: OnceLock<Key> = OnceLock::new();
    KEY.get_or_init(|| {
        let private = RsaPrivateKey::new(&mut rand::rngs::OsRng, 2048).unwrap();
        let pem = private.to_pkcs1_pem(rsa::pkcs1::LineEnding::LF).unwrap();
        Key {
            signing: EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap(),
            jwk: json!({
                "kid": "k1",
                "kty": "RSA",
                "n": URL_SAFE_NO_PAD.encode(private.n().to_bytes_be()),
                "e": URL_SAFE_NO_PAD.encode(private.e().to_bytes_be()),
            }),
        }
    })
}

/// The stand-in tenant: publishes the signing key.
pub async fn tenant() -> String {
    let app = Router::new().route("/{tenant}/discovery/v2.0/keys", get(|| async { Json(json!({ "keys": [key().jwk] })) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let authority = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    authority
}

/// A token for a service holding `roles`.
pub fn token_with(authority: &str, roles: &[&str], audience: &str) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let claims = json!({
        "iss": format!("{authority}/{TENANT}/v2.0"),
        "aud": audience,
        "exp": now + 600,
        "sub": "workload",
        "oid": "object-1",
        "roles": roles,
    });
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("k1".into());
    encode(&header, &claims, &key().signing).unwrap()
}

/// The PostgreSQL server for tests, or `None` to skip.
pub fn server() -> Option<Connector> {
    let url = std::env::var("IC_TEST_POSTGRES_URL").ok().filter(|u| !u.is_empty());
    if url.is_none() {
        eprintln!("IC_TEST_POSTGRES_URL is not set: skipping a test that needs PostgreSQL");
    }
    url.map(|u| Connector::local(&u).unwrap())
}

/// A fresh, empty database on the test server, dropped on drop.
pub struct Database {
    pub connector: Connector,
    admin: Connector,
    name: String,
}

impl Database {
    pub async fn new(server: Connector) -> Self {
        let name = format!("cm_test_{}", uuid::Uuid::new_v4().simple());
        let admin = server.clone();
        admin.connect().await.unwrap().execute(&format!("create database {name}"), &[]).await.unwrap();
        Self {
            connector: server.with_database(&name),
            admin,
            name,
        }
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        let admin = self.admin.clone();
        let name = self.name.clone();
        // Dropped from a fresh runtime: the test's runtime may be ending.
        let _ = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async {
                if let Ok(c) = admin.connect().await {
                    let _ = c.execute(&format!("drop database if exists {name} with (force)"), &[]).await;
                }
            });
        })
        .join();
    }
}

pub struct World {
    pub app: Arc<TestApp>,
    pub router: Router,
    pub azure: FakeAzure,
    pub authority: String,
    pub db: Database,
}

pub async fn world() -> Option<World> {
    let server = server()?;
    let db = Database::new(server).await;
    let pool = db::open(db.connector.clone(), 4).await.unwrap();
    let azure = FakeAzure::start().await;
    let authority = tenant().await;
    let credential = AzureCredential::static_token(TOKEN);
    let app = Arc::new(App {
        store: Store::new(
            pool,
            KeyRing::new(KeyVaultWrapper::new(&azure.base, "customers-kek", credential)),
            IndexKey::new(INDEX_KEY.as_bytes()).unwrap(),
        ),
        access: Access::shipped(),
        auth: Entra::new(&authority, TENANT, AUDIENCE),
        register: Register::new(MemoryWormStore::default()),
    });
    Some(World {
        router: router(app.clone()),
        app,
        azure,
        authority,
        db,
    })
}

/// One request.
pub struct Call<'a> {
    pub method: &'a str,
    pub path: String,
    pub role: &'a str,
    pub purpose: Option<&'a str>,
    pub body: Option<Value>,
    pub headers: Vec<(&'a str, String)>,
}

impl World {
    pub fn token(&self, role: &str) -> String {
        token_with(&self.authority, &[role], AUDIENCE)
    }

    pub async fn send(&self, call: Call<'_>) -> (StatusCode, Value) {
        let mut req = Request::builder()
            .method(call.method)
            .uri(&call.path)
            .header("authorization", format!("Bearer {}", self.token(call.role)))
            .header("content-type", "application/json");
        if let Some(p) = call.purpose {
            req = req.header("x-ic-purpose", p);
        }
        for (k, v) in &call.headers {
            req = req.header(*k, v);
        }
        let body = call.body.map(|b| Body::from(b.to_string())).unwrap_or_else(Body::empty);
        let resp = self.router.clone().oneshot(req.body(body).unwrap()).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    pub async fn get(&self, path: &str, role: &str, purpose: &str) -> (StatusCode, Value) {
        self.send(Call {
            method: "GET",
            path: path.to_string(),
            role,
            purpose: Some(purpose),
            body: None,
            headers: vec![],
        })
        .await
    }

    pub async fn post(&self, path: &str, role: &str, body: Value) -> (StatusCode, Value) {
        self.send(Call {
            method: "POST",
            path: path.to_string(),
            role,
            purpose: None,
            body: Some(body),
            headers: vec![],
        })
        .await
    }

    /// Create a person through the API as staff, and return their ID.
    pub async fn person(&self, given: &str, family: &str, dob: &str, more: &[(&str, Value)]) -> String {
        let mut facts = vec![
            json!({ "field": "given_name", "value": given }),
            json!({ "field": "family_name", "value": family }),
            json!({ "field": "date_of_birth", "value": dob }),
        ];
        for (f, v) in more {
            facts.push(json!({ "field": f, "value": v }));
        }
        let (status, body) = self
            .post("/v1/parties", "staff", json!({ "kind": "person", "facts": facts, "allow_duplicate": true }))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["party"].as_str().unwrap().to_string()
    }

    pub async fn command(&self, party: &str, role: &str, command: Value) -> (StatusCode, Value) {
        self.post(&format!("/v1/parties/{party}/commands"), role, command).await
    }

    /// Run a statement on the test database directly, as an attacker or a
    /// faulty process with database access would.
    pub async fn sql(&self, statement: &str) -> Result<u64, String> {
        let c = self.db.connector.connect().await.unwrap();
        c.execute(statement, &[]).await.map_err(|e| e.to_string())
    }
}

/// The core as the death relay sees it, in memory: what was proposed, and
/// partitions that refuse or cannot be reached.
#[derive(Default)]
pub struct FakeCore {
    pub partitions: u16,
    pub refuse: std::sync::Mutex<Option<u16>>,
    pub unreachable: std::sync::Mutex<bool>,
    /// Proposals go unanswered.
    pub lost: std::sync::Mutex<bool>,
    /// The directory refuses commands that mention this party.
    pub refuse_party: std::sync::Mutex<Option<String>>,
    pub directed: std::sync::Mutex<Vec<(String, Value)>>,
    pub proposed: std::sync::Mutex<Vec<(u16, String, Value)>>,
}

impl ic_customers::relay::Core for FakeCore {
    async fn partitions(&self) -> Result<u16, String> {
        if *self.unreachable.lock().unwrap() {
            return Err("no cluster".into());
        }
        Ok(self.partitions)
    }

    async fn propose(&self, partition: u16, key: &str, command: &ic_core::Command) -> Result<Option<String>, String> {
        self.proposed
            .lock()
            .unwrap()
            .push((partition, key.to_string(), serde_json::to_value(command).unwrap()));
        if *self.lost.lock().unwrap() {
            return Err("no answer".into());
        }
        if *self.refuse.lock().unwrap() == Some(partition) {
            return Ok(Some("not_open".into()));
        }
        Ok(None)
    }
}

impl ic_customers::relay::Directory for FakeCore {
    async fn direct(&self, key: &str, command: &ic_channels::directory::DirectoryCommand) -> Result<Option<String>, String> {
        if *self.lost.lock().unwrap() {
            return Err("no answer".into());
        }
        let command = serde_json::to_value(command).unwrap();
        if let Some(p) = self.refuse_party.lock().unwrap().as_deref() {
            if command.to_string().contains(p) {
                return Ok(Some("unknown_policy: no such policy".into()));
            }
        }
        self.directed.lock().unwrap().push((key.to_string(), command));
        Ok(None)
    }
}
