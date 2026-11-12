//! Who is calling: Microsoft Entra ID bearer tokens.
//!
//! Every caller is a service with a workload identity. Its token's app
//! roles name what it may act as (`group-admin`, `documents`, `claims`,
//! `portal`, `staff`, ...). A token with several roles names the one it
//! acts as in `x-ic-caller`. There is no development mode: tests sign their
//! own tokens and serve their own signing keys.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::HeaderMap;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use tokio::sync::RwLock;

/// Where Entra ID issues tokens and publishes its signing keys.
pub const ENTRA_AUTHORITY: &str = "https://login.microsoftonline.com";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    /// The calling service's role, which the access policy decides on.
    pub role: String,
    /// The Entra object ID of the calling workload.
    pub subject: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
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

type KeyCache = (BTreeMap<String, DecodingKey>, Option<Instant>);

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

    pub async fn authenticate(&self, headers: &HeaderMap) -> Result<Caller, AuthError> {
        let header = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).map(str::trim).filter(|v| !v.is_empty());
        let token = header("authorization").and_then(|v| v.strip_prefix("Bearer ")).ok_or(AuthError::Missing)?;
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
        let role = match header("x-ic-caller") {
            Some(asked) if claims.roles.iter().any(|r| r == asked) => asked.to_string(),
            Some(asked) => return Err(AuthError::Invalid(format!("the token does not carry the role {asked}"))),
            None => match claims.roles.as_slice() {
                [one] => one.clone(),
                [] => return Err(AuthError::Invalid("the token carries no role".into())),
                _ => return Err(AuthError::Invalid("the token carries several roles; name one in x-ic-caller".into())),
            },
        };
        Ok(Caller {
            role,
            subject: claims.oid.unwrap_or(claims.sub),
        })
    }

    /// The signing key for a key ID. Keys are cached for an hour and
    /// fetched again, at most once a minute, when an unknown key ID appears
    /// (Entra rotates its keys).
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
