//! Microsoft Entra access tokens, checked the same way by every service.
//!
//! A token is accepted only when it is signed by one of the tenant's
//! published keys (RS256), was issued by that tenant, names this service as
//! its audience and is in date. The tenant's keys are fetched from its
//! discovery endpoint, kept for an hour, and fetched again (at most once a
//! minute) when a token names a key ID not yet seen, because Entra rotates
//! its keys.
//!
//! Workforce tenants (Microsoft Entra ID) issue from
//! `https://login.microsoftonline.com/<tenant>/v2.0`. Customer tenants
//! (Microsoft Entra External ID) are reached at their own
//! `https://<name>.ciamlogin.com` authority but issue from
//! `https://<tenant>.ciamlogin.com/<tenant>/v2.0`; [`Validator::new`] works
//! that out from the authority. Access tokens must be version 2
//! (`requestedAccessTokenVersion: 2` on the app registration), which the
//! scripts in `deploy/entra` set.
//!
//! What a caller may do is never decided here: this says who the token is
//! for, which roles it carries and which client application asked for it.
//! Each service decides from that.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::http::HeaderMap;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use tokio::sync::RwLock;
use tokio::time::Instant;

#[cfg(feature = "testing")]
pub mod testing;

/// Where Entra ID issues workforce tokens and publishes their signing keys.
pub const ENTRA_AUTHORITY: &str = "https://login.microsoftonline.com";

/// How long fetched signing keys are trusted before they are fetched again.
pub const KEYS_FRESH_FOR: Duration = Duration::from_secs(3600);
/// The least time between fetches, however many unknown key IDs arrive.
pub const KEYS_FETCH_EVERY: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    #[error("missing bearer token")]
    Missing,
    #[error("invalid token: {0}")]
    Invalid(String),
    /// The tenant's keys could not be fetched. Nothing is known about the
    /// token: the caller tries again.
    #[error("signing keys unavailable: {0}")]
    Keys(String),
}

/// What an accepted token says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claims {
    /// The Entra object ID of the person or workload, or the token's
    /// subject where it carries none.
    pub subject: String,
    /// The app roles granted to the caller on this service's registration.
    pub roles: Vec<String>,
    /// The client application the token was issued to (`azp`, or `appid`
    /// in a version 1 token), where the token names one.
    pub client: Option<String>,
    /// When the token was issued, in seconds since the epoch, where it says.
    pub issued: Option<i64>,
}

impl Claims {
    pub fn has_role(&self, role: &str) -> bool {
        self.roles.iter().any(|r| r == role)
    }

    /// The one role a caller acts as. A token with several roles names the
    /// one it uses in `x-ic-caller`; a token with one role needs not.
    pub fn acting_as(&self, headers: &HeaderMap) -> Result<String, AuthError> {
        match header(headers, "x-ic-caller") {
            Some(asked) if self.has_role(asked) => Ok(asked.to_string()),
            Some(asked) => Err(AuthError::Invalid(format!("the token does not carry the role {asked}"))),
            None => match self.roles.as_slice() {
                [one] => Ok(one.clone()),
                [] => Err(AuthError::Invalid("the token carries no role".into())),
                _ => Err(AuthError::Invalid("the token carries several roles; name one in x-ic-caller".into())),
            },
        }
    }
}

#[derive(Debug, Deserialize)]
struct Raw {
    #[serde(default)]
    oid: Option<String>,
    sub: String,
    #[serde(default)]
    roles: Vec<String>,
    #[serde(default)]
    azp: Option<String>,
    #[serde(default)]
    appid: Option<String>,
    #[serde(default)]
    iat: Option<i64>,
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

/// Checks tokens issued by one tenant for one audience.
#[derive(Clone)]
pub struct Validator {
    issuer: String,
    audience: String,
    jwks_url: String,
    http: reqwest::Client,
    keys: Arc<RwLock<KeyCache>>,
}

impl std::fmt::Debug for Validator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Validator")
            .field("issuer", &self.issuer)
            .field("audience", &self.audience)
            .finish_non_exhaustive()
    }
}

/// A header's value, trimmed, if it is present and not empty.
pub fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok()).map(str::trim).filter(|v| !v.is_empty())
}

/// The bearer token in a request's `Authorization` header.
pub fn bearer(headers: &HeaderMap) -> Option<&str> {
    header(headers, "authorization")
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

/// The issuer a tenant's version 2 tokens carry, reached at `authority`.
pub fn issuer_for(authority: &str, tenant: &str) -> String {
    let authority = authority.trim_end_matches('/');
    // External ID: the issuer is the tenant's own ciamlogin.com host,
    // whatever name the tenant is reached by.
    if let Some(scheme_end) = authority.find("://") {
        let host = &authority[scheme_end + 3..];
        if host.ends_with(".ciamlogin.com") && !host.contains('/') {
            return format!("{}{tenant}.ciamlogin.com/{tenant}/v2.0", &authority[..scheme_end + 3]);
        }
    }
    format!("{authority}/{tenant}/v2.0")
}

impl Validator {
    /// Check tokens for `audience` issued by `tenant`, reached at `authority`.
    pub fn new(authority: &str, tenant: &str, audience: &str) -> Self {
        let base = authority.trim_end_matches('/');
        Self {
            issuer: issuer_for(authority, tenant),
            audience: audience.to_string(),
            jwks_url: format!("{base}/{tenant}/discovery/v2.0/keys"),
            http: ic_azure::http_client(),
            keys: Arc::new(RwLock::new((BTreeMap::new(), None))),
        }
    }

    /// The issuer tokens must carry.
    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    /// The audience tokens must carry.
    pub fn audience(&self) -> &str {
        &self.audience
    }

    /// Check the bearer token on a request.
    pub async fn validate(&self, headers: &HeaderMap) -> Result<Claims, AuthError> {
        self.validate_token(bearer(headers).ok_or(AuthError::Missing)?).await
    }

    /// Check one token.
    pub async fn validate_token(&self, token: &str) -> Result<Claims, AuthError> {
        let kid = decode_header(token)
            .map_err(|e| AuthError::Invalid(e.to_string()))?
            .kid
            .ok_or_else(|| AuthError::Invalid("no key id".to_string()))?;
        let key = self.key(&kid).await?;
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[&self.audience]);
        validation.set_issuer(&[&self.issuer]);
        validation.set_required_spec_claims(&["exp", "iss", "aud"]);
        let raw = decode::<Raw>(token, &key, &validation).map_err(|e| AuthError::Invalid(e.to_string()))?.claims;
        Ok(Claims {
            subject: raw.oid.unwrap_or(raw.sub),
            roles: raw.roles,
            client: raw.azp.or(raw.appid),
            issued: raw.iat,
        })
    }

    /// The signing key for a key ID.
    async fn key(&self, kid: &str) -> Result<DecodingKey, AuthError> {
        {
            let cache = self.keys.read().await;
            let fresh = cache.1.is_some_and(|t| t.elapsed() < KEYS_FRESH_FOR);
            if let (Some(k), true) = (cache.0.get(kid), fresh) {
                return Ok(k.clone());
            }
        }
        let mut cache = self.keys.write().await;
        let stale = cache.1.is_none_or(|t| t.elapsed() >= KEYS_FRESH_FOR);
        if stale || (!cache.0.contains_key(kid) && cache.1.is_none_or(|t| t.elapsed() > KEYS_FETCH_EVERY)) {
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
