//! Where the adapters get their Entra ID tokens: the choice of source from
//! the environment, the two token protocols, caching and refusals, against a
//! local stand-in for the token endpoints.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use ic_azure::credential::{AzureCredential, IMDS_ENDPOINT, SCOPE_KEY_VAULT, SCOPE_STORAGE};
use ic_azure::AzureError;
use serde_json::{json, Value};

type Seen = Arc<Mutex<Vec<String>>>;

/// The metadata service: it insists on the `Metadata` header, and returns
/// the lifetime as a string, as the real one does. Tokens for Key Vault are
/// about to expire.
async fn imds(State(seen): State<Seen>, headers: HeaderMap, Query(q): Query<BTreeMap<String, String>>) -> Result<Json<Value>, StatusCode> {
    if headers.get("metadata").and_then(|v| v.to_str().ok()) != Some("true") {
        return Err(StatusCode::BAD_REQUEST);
    }
    let mut seen = seen.lock().unwrap();
    seen.push(format!("{} as {}", q["resource"], q.get("client_id").map_or("the machine", String::as_str)));
    let lifetime = if q["resource"].contains("vault") { "120" } else { "3600" };
    Ok(Json(json!({ "access_token": format!("imds-token-{}", seen.len()), "expires_in": lifetime })))
}

async fn refusing() -> (StatusCode, &'static str) {
    (StatusCode::UNAUTHORIZED, "AADSTS70021: No matching federated identity record found")
}

async fn endpoints() -> (String, Seen) {
    let seen = Seen::default();
    let app = Router::new()
        .route("/metadata/identity/oauth2/token", get(imds))
        .route("/{tenant}/oauth2/v2.0/token", post(refusing))
        .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, seen)
}

/// Purpose: the credential source is chosen from the variables the AKS workload identity
/// webhook injects: all three give workload identity (at a sovereign cloud's authority if
/// one is named), none gives the machine's managed identity or the one `AZURE_CLIENT_ID`
/// names, and a token file without a tenant or client is an error. Blank values count as
/// unset.
/// Why it matters: each process on AKS runs as its own identity with its own grants. A
/// half-injected pod that quietly fell back to the node's managed identity would run with
/// the wrong permissions, or with none and fail much later on its first write to storage.
#[test]
fn the_credential_source_follows_the_workload_identity_variables() {
    let from = |vars: &[(&str, &str)]| {
        let vars: BTreeMap<String, String> = vars.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
        AzureCredential::from_lookup(|k| vars.get(k).cloned()).map(|c| c.describe())
    };
    let injected = [
        ("AZURE_FEDERATED_TOKEN_FILE", "/var/run/secrets/azure/tokens/azure-identity-token"),
        ("AZURE_TENANT_ID", "tenant-1"),
        ("AZURE_CLIENT_ID", " client-1 "),
    ];
    assert_eq!(
        from(&injected).unwrap(),
        "workload identity client-1 in tenant tenant-1 at https://login.microsoftonline.com"
    );
    let mut sovereign = injected.to_vec();
    sovereign.push(("AZURE_AUTHORITY_HOST", "https://login.microsoftonline.us/"));
    assert!(from(&sovereign).unwrap().ends_with("at https://login.microsoftonline.us"));

    assert_eq!(from(&[]).unwrap(), format!("the machine's managed identity from {IMDS_ENDPOINT}"));
    assert_eq!(
        from(&[("AZURE_CLIENT_ID", "client-2"), ("AZURE_FEDERATED_TOKEN_FILE", "  ")]).unwrap(),
        format!("managed identity client-2 from {IMDS_ENDPOINT}")
    );
    let half = from(&[("AZURE_FEDERATED_TOKEN_FILE", "/token"), ("AZURE_CLIENT_ID", "client-1")]);
    assert!(matches!(half, Err(AzureError::NoCredential(m)) if m.contains("AZURE_TENANT_ID")));
    // The process environment of a test run has no federated token.
    assert!(AzureCredential::from_env().is_ok());
    assert_eq!(AzureCredential::static_token("secret-value").describe(), "a fixed token");
}

/// Purpose: a managed identity token is asked for with the `Metadata` header, the resource
/// without its `/.default` suffix and the chosen client; it is cached per scope and reused;
/// and a token with under five minutes left is fetched again rather than reused.
/// Why it matters: the ledger nodes call storage on every sealed segment. Without the
/// cache each call would cost a round trip to the metadata service, which throttles; with
/// a cache that ignored expiry, calls would start failing with 401 an hour after start and
/// keep failing until the process restarted.
#[tokio::test]
async fn managed_identity_tokens_are_cached_until_they_near_expiry() {
    let (base, seen) = endpoints().await;
    let endpoint = format!("{base}/metadata/identity/oauth2/token");
    let named = AzureCredential::managed_identity(&endpoint, Some("client-7"));
    assert_eq!(named.token(SCOPE_STORAGE).await.unwrap(), "imds-token-1");
    assert_eq!(named.token(SCOPE_STORAGE).await.unwrap(), "imds-token-1", "the cached token was not reused");
    assert_eq!(named.clone().token(SCOPE_STORAGE).await.unwrap(), "imds-token-1", "clones share the cache");

    // Two minutes of life is inside the refresh margin: asked for every time.
    assert_eq!(named.token(SCOPE_KEY_VAULT).await.unwrap(), "imds-token-2");
    assert_eq!(named.token(SCOPE_KEY_VAULT).await.unwrap(), "imds-token-3");

    let machine = AzureCredential::managed_identity(&endpoint, None);
    assert_eq!(machine.token(SCOPE_STORAGE).await.unwrap(), "imds-token-4");
    assert_eq!(
        *seen.lock().unwrap(),
        [
            "https://storage.azure.com as client-7",
            "https://vault.azure.net as client-7",
            "https://vault.azure.net as client-7",
            "https://storage.azure.com as the machine",
        ]
    );
    assert_eq!(AzureCredential::static_token("fixed").token(SCOPE_STORAGE).await.unwrap(), "fixed");
}

/// Purpose: when Entra ID refuses the token exchange the caller gets the service, the
/// status and the start of the response; a missing projected token file is reported as
/// such; a metadata service that cannot be reached is an error, not a hang; and nothing
/// from a failed attempt is cached.
/// Why it matters: a wrong federation subject is the commonest fault in a new AKS
/// deployment, and the only clue is the AADSTS code in the response body. An adapter that
/// swallowed it would leave an operator with "storage write failed" and nothing to act on.
#[tokio::test]
async fn a_refused_token_exchange_says_who_refused_and_why() {
    let (base, _) = endpoints().await;
    let dir = std::env::temp_dir().join(format!("ic-azure-credential-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let token_file = dir.join("token");
    std::fs::write(&token_file, "projected").unwrap();
    let cred = AzureCredential::workload_identity(&base, "tenant-1", "client-1", token_file.to_str().unwrap());
    for _ in 0..2 {
        match cred.token(SCOPE_STORAGE).await {
            Err(AzureError::Status { service, status, body }) => {
                assert_eq!((service, status), ("Entra ID", 401));
                assert!(body.starts_with("AADSTS70021"), "{body}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    let missing = AzureCredential::workload_identity(&base, "tenant-1", "client-1", dir.join("absent").to_str().unwrap());
    assert!(matches!(missing.token(SCOPE_STORAGE).await, Err(AzureError::Io(_))));

    // Nothing listens here.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    let unreachable = AzureCredential::managed_identity(&format!("http://{closed}/token"), None);
    assert!(matches!(unreachable.token(SCOPE_STORAGE).await, Err(AzureError::Http(_))));

    // A refusal from the metadata service is passed on the same way.
    let wrong_path = AzureCredential::managed_identity(&format!("{base}/nowhere"), None);
    assert!(matches!(
        wrong_path.token(SCOPE_STORAGE).await,
        Err(AzureError::Status {
            service: "managed identity",
            status: 404,
            ..
        })
    ));
    let _ = std::fs::remove_dir_all(&dir);
}
