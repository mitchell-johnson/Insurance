//! Who is calling the service: Microsoft Entra ID bearer tokens.
//!
//! Every caller is a service with a workload identity (the portal, the
//! staff console's backend, another module's driver). Its token is issued
//! for this service's application, and the token's app roles are the
//! caller names it may act as (`group-admin`, `employer-portal`,
//! `member-portal`, ...). There is no shared key and no development mode:
//! tests sign their own tokens and serve their own signing keys.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::http::HeaderMap;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use tokio::sync::RwLock;
use tokio::time::Instant;

/// Where Entra ID issues tokens and publishes its signing keys.
pub const ENTRA_AUTHORITY: &str = "https://login.microsoftonline.com";

/// How long fetched signing keys are trusted before they are fetched again.
const KEYS_FRESH_FOR: Duration = Duration::from_secs(3600);
/// The least time between fetches, however many unknown key IDs arrive.
const KEYS_FETCH_EVERY: Duration = Duration::from_secs(60);

/// A caller whose token was accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    /// The caller names this workload may act as.
    pub roles: Vec<String>,
    /// The Entra object ID of the calling workload, for the log.
    pub subject: String,
}

impl Caller {
    pub fn may_act_as(&self, caller: &str) -> bool {
        self.roles.iter().any(|r| r == caller)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    #[error("missing bearer token")]
    Missing,
    #[error("invalid token: {0}")]
    Invalid(String),
    /// Entra ID's keys could not be fetched. Nothing is known about the
    /// token: the caller tries again.
    #[error("signing keys unavailable: {0}")]
    Keys(String),
}

#[derive(Debug, Deserialize)]
struct Claims {
    #[serde(default)]
    oid: Option<String>,
    sub: String,
    #[serde(default)]
    roles: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Debug, Deserialize)]
struct Jwk {
    kid: String,
    #[serde(default)]
    n: String,
    #[serde(default)]
    e: String,
}

type KeyCache = (BTreeMap<String, DecodingKey>, Option<Instant>);

/// Validates tokens issued by one tenant for one audience.
#[derive(Clone)]
pub struct Entra {
    issuer: String,
    audience: String,
    jwks_url: String,
    http: reqwest::Client,
    keys: Arc<RwLock<KeyCache>>,
}

impl Entra {
    /// Validate tokens for `audience` issued by `tenant` at `authority`.
    pub fn new(authority: &str, tenant: &str, audience: &str) -> Self {
        let authority = authority.trim_end_matches('/');
        Self {
            issuer: format!("{authority}/{tenant}/v2.0"),
            audience: audience.to_string(),
            jwks_url: format!("{authority}/{tenant}/discovery/v2.0/keys"),
            http: ic_azure::http_client(),
            keys: Arc::new(RwLock::new((BTreeMap::new(), None))),
        }
    }

    /// The caller behind a request's bearer token: signed by the tenant,
    /// issued for this service, not expired, and carrying at least one role.
    pub async fn authenticate(&self, headers: &HeaderMap) -> Result<Caller, AuthError> {
        let token = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or(AuthError::Missing)?;
        let kid = decode_header(token)
            .map_err(|e| AuthError::Invalid(e.to_string()))?
            .kid
            .ok_or_else(|| AuthError::Invalid("no key id".to_string()))?;
        let key = self.key(&kid).await?;
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[&self.audience]);
        validation.set_issuer(&[&self.issuer]);
        let claims = decode::<Claims>(token, &key, &validation)
            .map_err(|e| AuthError::Invalid(e.to_string()))?
            .claims;
        if claims.roles.is_empty() {
            return Err(AuthError::Invalid("the token carries no role".to_string()));
        }
        Ok(Caller {
            roles: claims.roles,
            subject: claims.oid.unwrap_or(claims.sub),
        })
    }

    /// The signing key for a key ID. Keys are kept for an hour and fetched
    /// again, at most once a minute, when an unknown key ID appears (Entra
    /// ID rotates its keys).
    async fn key(&self, kid: &str) -> Result<DecodingKey, AuthError> {
        {
            let cache = self.keys.read().await;
            let fresh = cache.1.is_some_and(|t| t.elapsed() < KEYS_FRESH_FOR);
            if let (Some(k), true) = (cache.0.get(kid), fresh) {
                return Ok(k.clone());
            }
        }
        let mut cache = self.keys.write().await;
        if cache.1.is_none_or(|t| t.elapsed() > KEYS_FETCH_EVERY) || cache.0.is_empty() {
            let resp = self.http.get(&self.jwks_url).send().await.map_err(|e| AuthError::Keys(e.to_string()))?;
            let jwks: Jwks = resp.json().await.map_err(|e| AuthError::Keys(e.to_string()))?;
            cache.0 = jwks
                .keys
                .iter()
                .filter_map(|k| DecodingKey::from_rsa_components(&k.n, &k.e).ok().map(|d| (k.kid.clone(), d)))
                .collect();
            cache.1 = Some(Instant::now());
        }
        cache
            .0
            .get(kid)
            .cloned()
            .ok_or_else(|| AuthError::Invalid("token signed by an unknown key".to_string()))
    }
}
