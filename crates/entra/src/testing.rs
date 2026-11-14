//! A stand-in tenant for tests: it publishes a signing key at the discovery
//! path Entra uses, and mints tokens with it. Also an attacker's key that
//! claims the same key ID, for forgery tests.
//!
//! Only for tests (feature `testing`). Making an RSA key is slow, so the two
//! keys are made once per test process.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::routing::get;
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::RsaPrivateKey;
use serde_json::{json, Value};

/// The key ID the stand-in publishes.
pub const KEY_ID: &str = "test-key-1";

struct Key {
    signing: EncodingKey,
    jwk: Value,
}

fn make(kid: &str) -> Key {
    let private = RsaPrivateKey::new(&mut rand::rngs::OsRng, 2048).expect("an RSA key");
    let pem = private.to_pkcs1_pem(rsa::pkcs1::LineEnding::LF).expect("a PEM");
    Key {
        signing: EncodingKey::from_rsa_pem(pem.as_bytes()).expect("a signing key"),
        jwk: json!({
            "kid": kid,
            "kty": "RSA",
            "n": URL_SAFE_NO_PAD.encode(private.n().to_bytes_be()),
            "e": URL_SAFE_NO_PAD.encode(private.e().to_bytes_be()),
        }),
    }
}

fn keys() -> &'static (Key, Key) {
    static KEYS: OnceLock<(Key, Key)> = OnceLock::new();
    KEYS.get_or_init(|| (make(KEY_ID), make(KEY_ID)))
}

/// Seconds since the epoch.
pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// A running stand-in for a tenant's key discovery endpoint.
pub struct Tenant {
    /// What to pass as the authority, such as `http://127.0.0.1:41234`.
    pub authority: String,
    pub tenant: String,
    pub audience: String,
    reads: Arc<AtomicU32>,
}

impl Tenant {
    /// Serve the tenant's keys on a free local port.
    pub async fn start(tenant: &str, audience: &str) -> Self {
        let reads = Arc::new(AtomicU32::new(0));
        let counter = reads.clone();
        let app = Router::new().route(
            "/{tenant}/discovery/v2.0/keys",
            get(move || {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    // A key of another type is published alongside, as Entra may; it is ignored.
                    Json(json!({ "keys": [keys().0.jwk, { "kid": "ec-key", "kty": "EC" }] }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("a local port");
        let authority = format!("http://{}", listener.local_addr().expect("an address"));
        tokio::spawn(async move { axum::serve(listener, app).await });
        Self {
            authority,
            tenant: tenant.to_string(),
            audience: audience.to_string(),
            reads,
        }
    }

    /// How many times the keys were read.
    pub fn reads(&self) -> u32 {
        self.reads.load(Ordering::SeqCst)
    }

    /// A validator for this tenant and audience.
    pub fn validator(&self) -> crate::Validator {
        crate::Validator::new(&self.authority, &self.tenant, &self.audience)
    }

    /// The claims of a good token: this tenant, this audience, in date.
    pub fn claims(&self, subject: &str, roles: &[&str]) -> Value {
        json!({
            "iss": format!("{}/{}/v2.0", self.authority, self.tenant),
            "aud": self.audience,
            "iat": now(),
            "exp": now() + 600,
            "sub": format!("pairwise-{subject}"),
            "oid": subject,
            "roles": roles,
        })
    }

    /// A good token for `subject` with `roles`.
    pub fn token(&self, subject: &str, roles: &[&str]) -> String {
        sign(&self.claims(subject, roles))
    }
}

/// Sign `claims` with the tenant's key under its key ID.
pub fn sign(claims: &Value) -> String {
    sign_with(&keys().0.signing, Some(KEY_ID), claims)
}

/// Sign `claims` with an attacker's key that claims the tenant's key ID.
pub fn forge(claims: &Value) -> String {
    sign_with(&keys().1.signing, Some(KEY_ID), claims)
}

/// Sign `claims` with the tenant's key under another key ID, or none.
pub fn sign_as(kid: Option<&str>, claims: &Value) -> String {
    sign_with(&keys().0.signing, kid, claims)
}

fn sign_with(key: &EncodingKey, kid: Option<&str>, claims: &Value) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = kid.map(str::to_string);
    encode(&header, claims, key).expect("a token")
}
