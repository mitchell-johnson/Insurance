//! The API reinsurers read their own statements and listings from, beside
//! the SFTP containers (`docs/reinsurance.md`, "Reinsurers").
//!
//! A reinsurer calls with a Microsoft Entra ID token for its own client
//! application, registered in the insurer's tenant and granted the app role
//! [`READ_ROLE`] on this API. The token's client (`azp`, or `appid` in a
//! version 1 token) is the only thing that says who is calling: it is looked
//! up in the configured clients, and every answer is read under that
//! reinsurer's own name. Nothing a caller sends can name another reinsurer.
//! There is no development mode: tests sign their own tokens and serve their
//! own signing keys.
//!
//! The listings carry policy-level facts about lives, so every request is
//! written to the log (target `reinsurance.api`) with the reinsurer, the
//! client, what was asked for and the answer's status. File contents and
//! tokens are never logged.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path, Query as Params, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use ic_azure::blob::WormStore;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::driver::{DriverError, Module};
use crate::host::{Query, MAX_ANSWER};
use crate::ReinsurerId;

/// Where Entra ID issues tokens and publishes its signing keys.
pub use ic_entra::ENTRA_AUTHORITY;

/// The app role a reinsurer's client must hold.
pub const READ_ROLE: &str = "Reinsurer.Statements.Read";

/// The most statements one page lists unless the caller asks for fewer.
pub const PAGE: usize = 100;

/// Settings of the API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiConfig {
    pub listen: SocketAddr,
    pub authority: String,
    pub tenant: String,
    /// The API's own application ID URI or client ID, as tokens name it.
    pub audience: String,
    /// Each reinsurer client application's ID, and the reinsurer it is.
    pub clients: BTreeMap<String, ReinsurerId>,
}

impl ApiConfig {
    /// Read the settings. With `IC_REINSURANCE_API_LISTEN` unset there is no
    /// API. Set, `IC_ENTRA_TENANT`, `IC_REINSURANCE_API_AUDIENCE` and
    /// `IC_REINSURANCE_API_CLIENTS` (`<client id>=<reinsurer>,…`) are all
    /// required: an API that served without knowing its callers would serve
    /// anyone. `IC_ENTRA_AUTHORITY` defaults to Entra's public cloud.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Option<Self>, String> {
        let get = |key: &str| get(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let Some(listen) = get("IC_REINSURANCE_API_LISTEN") else {
            return Ok(None);
        };
        let listen = listen
            .parse()
            .map_err(|_| format!("IC_REINSURANCE_API_LISTEN must be an address and port, not {listen:?}"))?;
        let need = |key: &str| get(key).ok_or_else(|| format!("IC_REINSURANCE_API_LISTEN is set, so set {key} too"));
        let tenant = need("IC_ENTRA_TENANT")?;
        let audience = need("IC_REINSURANCE_API_AUDIENCE")?;
        let mut clients = BTreeMap::new();
        for pair in need("IC_REINSURANCE_API_CLIENTS")?.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let (client, reinsurer) = pair
                .split_once('=')
                .map(|(c, r)| (c.trim(), r.trim()))
                .filter(|(c, r)| !c.is_empty() && !r.is_empty())
                .ok_or_else(|| format!("IC_REINSURANCE_API_CLIENTS lists {pair:?}; each entry is <client id>=<reinsurer>"))?;
            if clients.insert(client.to_string(), ReinsurerId::from(reinsurer)).is_some() {
                return Err(format!("IC_REINSURANCE_API_CLIENTS lists client {client} twice"));
            }
        }
        if clients.is_empty() {
            return Err("IC_REINSURANCE_API_CLIENTS names no client".into());
        }
        Ok(Some(ApiConfig {
            listen,
            authority: get("IC_ENTRA_AUTHORITY").unwrap_or_else(|| ENTRA_AUTHORITY.into()),
            tenant,
            audience,
            clients,
        }))
    }
}

/// Why a request was not let in.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Denied {
    #[error("missing bearer token")]
    Missing,
    #[error("invalid token: {0}")]
    Invalid(String),
    #[error("client {0} is not a reinsurer of this insurer")]
    UnknownClient(String),
    #[error("the token does not carry the role {READ_ROLE}")]
    NoRole,
    #[error("signing keys unavailable: {0}")]
    Keys(String),
}

/// Checks reinsurers' tokens with the shared validator (`crates/entra`)
/// and knows which client is which reinsurer.
#[derive(Clone)]
pub struct ReinsurerAuth {
    validator: ic_entra::Validator,
    clients: BTreeMap<String, ReinsurerId>,
}

impl ReinsurerAuth {
    pub fn new(config: &ApiConfig) -> Self {
        Self {
            validator: ic_entra::Validator::new(&config.authority, &config.tenant, &config.audience),
            clients: config.clients.clone(),
        }
    }

    pub async fn authenticate(&self, headers: &HeaderMap) -> Result<(ReinsurerId, String), Denied> {
        let claims = self.validator.validate(headers).await.map_err(|e| match e {
            ic_entra::AuthError::Missing => Denied::Missing,
            ic_entra::AuthError::Invalid(why) => Denied::Invalid(why),
            ic_entra::AuthError::Keys(why) => Denied::Keys(why),
        })?;
        let client = claims.client.ok_or_else(|| Denied::Invalid("the token names no client".into()))?;
        let reinsurer = self.clients.get(&client).cloned().ok_or_else(|| Denied::UnknownClient(client.clone()))?;
        if !claims.roles.iter().any(|r| r == READ_ROLE) {
            return Err(Denied::NoRole);
        }
        Ok((reinsurer, client))
    }
}

/// What the API reads: the module for the list of statements, and the
/// write-once records container for the files themselves.
pub struct Api<M, W> {
    pub module: M,
    pub records: W,
    pub auth: ReinsurerAuth,
}

fn refusal(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    let mut response = (status, Json(json!({"code": code, "message": message.into()}))).into_response();
    if status == StatusCode::UNAUTHORIZED {
        response.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    }
    response
}

fn denied(why: &Denied) -> Response {
    match why {
        Denied::Missing | Denied::Invalid(_) => refusal(StatusCode::UNAUTHORIZED, "unauthenticated", why.to_string()),
        Denied::UnknownClient(_) | Denied::NoRole => refusal(StatusCode::FORBIDDEN, "forbidden", why.to_string()),
        Denied::Keys(_) => refusal(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "sign-in cannot be checked now; try again later"),
    }
}

/// Who called, or the answer that says why they may not.
async fn caller<M, W>(api: &Api<M, W>, headers: &HeaderMap, asked: &str) -> Result<(ReinsurerId, String), Response> {
    api.auth.authenticate(headers).await.map_err(|why| {
        tracing::warn!(target: "reinsurance.api", asked, refused = %why, "reinsurer request refused");
        denied(&why)
    })
}

fn audit(reinsurer: &ReinsurerId, client: &str, asked: &str, status: StatusCode) {
    tracing::info!(target: "reinsurance.api", reinsurer = %reinsurer, client, asked, status = status.as_u16(), "reinsurer request");
}

#[derive(Debug, Deserialize)]
struct Page {
    #[serde(default)]
    after: Option<u32>,
    #[serde(default)]
    limit: Option<usize>,
}

async fn statements<M: Module, W: WormStore>(State(api): State<Arc<Api<M, W>>>, headers: HeaderMap, Params(page): Params<Page>) -> Response {
    let asked = "statements";
    let (reinsurer, client) = match caller(&api, &headers, asked).await {
        Ok(who) => who,
        Err(response) => return response,
    };
    let limit = page.limit.unwrap_or(PAGE).clamp(1, MAX_ANSWER);
    let query = Query::Statements {
        reinsurer: reinsurer.clone(),
        after: page.after,
        limit,
    };
    let response = match api.module.ask(query).await {
        Ok(listed) => {
            let next = listed
                .as_array()
                .filter(|rows| rows.len() == limit)
                .and_then(|rows| rows.last())
                .and_then(|last| last["number"].as_u64());
            (StatusCode::OK, Json(json!({"statements": listed, "next_after": next}))).into_response()
        }
        Err(DriverError::Unavailable(e)) => {
            tracing::warn!(target: "reinsurance.api", error = %e, "the module did not answer");
            refusal(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "statements cannot be read now; try again later")
        }
        Err(e) => {
            tracing::error!(target: "reinsurance.api", error = %e, "the module refused the statements question");
            refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal", "statements cannot be listed")
        }
    };
    audit(&reinsurer, &client, asked, response.status());
    response
}

/// A path segment a treaty or file name may be: letters, digits, `-`, `_`
/// and `.`, not starting with a dot.
fn segment(text: &str) -> bool {
    !text.is_empty() && text.len() <= 200 && !text.starts_with('.') && text.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

async fn file<M: Module, W: WormStore>(State(api): State<Arc<Api<M, W>>>, headers: HeaderMap, Path((treaty, name)): Path<(String, String)>) -> Response {
    let asked = format!("file {treaty}/{name}");
    let (reinsurer, client) = match caller(&api, &headers, &asked).await {
        Ok(who) => who,
        Err(response) => return response,
    };
    let response = if !segment(&treaty) || !segment(&name) || !name.starts_with("statement-") || !name.ends_with(".json") {
        refusal(StatusCode::NOT_FOUND, "not_found", "no such file")
    } else {
        // The reinsurer's own name from its token, never from the request.
        match api.records.get(&format!("reinsurance/{treaty}/{reinsurer}/{name}")).await {
            Ok(Some(body)) => {
                let sha = hex::encode(Sha256::digest(&body));
                let mut response = (StatusCode::OK, body).into_response();
                let headers = response.headers_mut();
                headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
                headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
                if let Ok(value) = HeaderValue::from_str(&sha) {
                    headers.insert("x-content-sha256", value);
                }
                response
            }
            Ok(None) => refusal(StatusCode::NOT_FOUND, "not_found", "no such file"),
            Err(e) => {
                tracing::warn!(target: "reinsurance.api", error = %e, "the records store did not answer");
                refusal(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "files cannot be read now; try again later")
            }
        }
    };
    audit(&reinsurer, &client, &asked, response.status());
    response
}

/// The API's routes: `GET /v1/statements?after=&limit=`, `GET
/// /v1/files/{treaty}/{file}` and an unauthenticated `GET /healthz`.
pub fn router<M: Module + 'static, W: WormStore + 'static>(api: Api<M, W>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/statements", get(statements::<M, W>))
        .route("/v1/files/{treaty}/{file}", get(file::<M, W>))
        .with_state(Arc::new(api))
}
