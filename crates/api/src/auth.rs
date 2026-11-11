//! Who is calling.
//!
//! Production: OAuth 2.0 bearer tokens issued by Microsoft Entra ID, verified
//! against the tenant's published signing keys. A caller's app roles name the
//! modules it may act as (`new-business`, `cash-matching`, and so on).
//! Customers and partners sign in through Microsoft Entra External ID and
//! carry no module roles; what they may see is decided by audience scoping.
//!
//! Development: with no tenant configured, identity comes from headers. That
//! mode refuses to start outside the insurance in a box.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::HeaderMap;
use ic_readmodel::scope::Relationship;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use tokio::sync::RwLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    /// Opaque subject: the Entra object ID, which the customer master maps to a party.
    pub subject: String,
    pub roles: Vec<String>,
    /// Relationships asserted in development mode only. In production they
    /// come from the gateway's read model of the customer master.
    pub dev_relationships: Vec<Relationship>,
}

impl Caller {
    pub fn has_role(&self, role: &str) -> bool {
        self.roles.iter().any(|r| r == role)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("missing bearer token")]
    Missing,
    #[error("invalid token: {0}")]
    Invalid(String),
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

/// Where Entra ID issues tokens and publishes its signing keys.
pub const ENTRA_AUTHORITY: &str = "https://login.microsoftonline.com";

#[derive(Clone)]
pub enum Auth {
    /// Identity from `x-dev-subject`, `x-dev-roles` and `x-dev-relationships`.
    Dev,
    Entra(Arc<Entra>),
}

pub struct Entra {
    issuer: String,
    audience: String,
    jwks_url: String,
    http: reqwest::Client,
    keys: RwLock<(BTreeMap<String, DecodingKey>, Option<Instant>)>,
}

impl Auth {
    /// Validate tokens issued by `tenant` at the global Entra ID authority.
    pub fn entra(tenant: &str, audience: &str) -> Self {
        Self::entra_at(ENTRA_AUTHORITY, tenant, audience)
    }

    /// Validate tokens issued by `tenant` at another authority: a sovereign
    /// cloud's, such as `https://login.microsoftonline.us`.
    pub fn entra_at(authority: &str, tenant: &str, audience: &str) -> Self {
        let authority = authority.trim_end_matches('/');
        Auth::Entra(Arc::new(Entra {
            issuer: format!("{authority}/{tenant}/v2.0"),
            audience: audience.to_string(),
            jwks_url: format!("{authority}/{tenant}/discovery/v2.0/keys"),
            http: reqwest::Client::new(),
            keys: RwLock::new((BTreeMap::new(), None)),
        }))
    }

    pub async fn authenticate(&self, headers: &HeaderMap) -> Result<Caller, AuthError> {
        let header = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).map(str::to_string);
        match self {
            Auth::Dev => {
                let subject = header("x-dev-subject").ok_or(AuthError::Missing)?;
                let list = |k: &str| {
                    header(k)
                        .map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>())
                        .unwrap_or_default()
                };
                let dev_relationships = list("x-dev-relationships")
                    .iter()
                    .filter_map(|r| serde_json::from_value(serde_json::Value::String(r.clone())).ok())
                    .collect();
                Ok(Caller {
                    subject,
                    roles: list("x-dev-roles"),
                    dev_relationships,
                })
            }
            Auth::Entra(e) => {
                let token = header("authorization")
                    .and_then(|v| v.strip_prefix("Bearer ").map(str::to_string))
                    .ok_or(AuthError::Missing)?;
                let kid = decode_header(&token)
                    .map_err(|e| AuthError::Invalid(e.to_string()))?
                    .kid
                    .ok_or_else(|| AuthError::Invalid("no key id".to_string()))?;
                let key = e.key(&kid).await?;
                let mut validation = Validation::new(Algorithm::RS256);
                validation.set_audience(&[&e.audience]);
                validation.set_issuer(&[&e.issuer]);
                let claims = decode::<Claims>(&token, &key, &validation)
                    .map_err(|e| AuthError::Invalid(e.to_string()))?
                    .claims;
                Ok(Caller {
                    subject: claims.oid.unwrap_or(claims.sub),
                    roles: claims.roles,
                    dev_relationships: vec![],
                })
            }
        }
    }
}

impl Entra {
    /// The signing key for a key ID. Keys are cached for an hour and
    /// refetched once if an unknown key ID appears (Entra rotates keys).
    async fn key(&self, kid: &str) -> Result<DecodingKey, AuthError> {
        {
            let cache = self.keys.read().await;
            let fresh = cache.1.is_some_and(|t| t.elapsed() < Duration::from_secs(3600));
            if let (Some(k), true) = (cache.0.get(kid), fresh) {
                return Ok(k.clone());
            }
        }
        let mut cache = self.keys.write().await;
        if cache.1.is_none_or(|t| t.elapsed() > Duration::from_secs(60)) || cache.0.is_empty() {
            let jwks: Jwks = self
                .http
                .get(&self.jwks_url)
                .send()
                .await
                .map_err(|e| AuthError::Keys(e.to_string()))?
                .json()
                .await
                .map_err(|e| AuthError::Keys(e.to_string()))?;
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
