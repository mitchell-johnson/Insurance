//! Microsoft Entra ID token validation, against a stand-in for the tenant's
//! published signing keys.

mod common;

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use ic_api::auth::{Auth, AuthError};
use ic_api::engine::{AzureConfig, Engine, EngineConfig};
use ic_api::routes::{router, AppState};
use ic_readmodel::scope::AudienceScope;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::RsaPrivateKey;
use serde_json::{json, Value};

const TENANT: &str = "tenant-1";
const AUDIENCE: &str = "api://insurance-core";

/// A signing key: what signs tokens, and what the tenant publishes about it.
struct Key {
    signing: EncodingKey,
    jwk: Value,
}

fn key(kid: &str) -> Key {
    let private = RsaPrivateKey::new(&mut rand::rngs::OsRng, 2048).unwrap();
    let pem = private.to_pkcs1_pem(rsa::pkcs1::LineEnding::LF).unwrap();
    Key {
        signing: EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap(),
        jwk: json!({
            "kid": kid,
            "kty": "RSA",
            "n": URL_SAFE_NO_PAD.encode(private.n().to_bytes_be()),
            "e": URL_SAFE_NO_PAD.encode(private.e().to_bytes_be()),
        }),
    }
}

/// The tenant's key, and an attacker's key that claims the same key ID.
/// Generated once: making an RSA key is slow.
fn keys() -> &'static (Key, Key) {
    static KEYS: OnceLock<(Key, Key)> = OnceLock::new();
    KEYS.get_or_init(|| (key("key-1"), key("key-1")))
}

/// A stand-in for the tenant's key discovery endpoint. Counts how often it is read.
async fn tenant() -> (String, Arc<AtomicU32>) {
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
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let authority = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (authority, reads)
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn claims(authority: &str) -> Value {
    json!({
        "iss": format!("{authority}/{TENANT}/v2.0"),
        "aud": AUDIENCE,
        "exp": now() + 600,
        "sub": "subject-pairwise",
        "oid": "object-1",
        "roles": ["new-business", "policy-servicing"],
    })
}

fn token(key: &Key, kid: Option<&str>, claims: &Value) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = kid.map(str::to_string);
    encode(&header, claims, &key.signing).unwrap()
}

fn bearer(token: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
    headers
}

/// Purpose: a token signed by the tenant's key, for this API, in date, is accepted and yields
/// the caller's object ID and app roles; and every other token is refused: no token, another
/// scheme, garbage, no key ID, an unknown key, the wrong audience, the wrong issuer, expired,
/// an unsigned token, and one signed by somebody else's key under the tenant's key ID.
/// Why it matters: this is the front door of the production system. Each refusal here is an
/// attack that works against a careless validator: a token minted for another application
/// replayed here, a token from another tenant, a stolen expired token, or a forged one. The
/// roles in an accepted token decide which commands the caller may send to the ledger.
#[tokio::test]
async fn only_a_token_from_the_tenant_for_this_api_is_accepted() {
    let (authority, reads) = tenant().await;
    let auth = Auth::entra_at(&format!("{authority}/"), TENANT, AUDIENCE);
    let (tenant_key, forger) = keys();
    let good = claims(&authority);

    let caller = auth.authenticate(&bearer(&token(tenant_key, Some("key-1"), &good))).await.unwrap();
    assert_eq!(caller.subject, "object-1");
    assert_eq!(caller.roles, vec!["new-business", "policy-servicing"]);
    assert!(caller.has_role("new-business") && !caller.has_role("operations"));
    assert!(caller.dev_relationships.is_empty());

    // A token with no object ID is known by its subject, and may have no roles.
    let mut plain = good.clone();
    plain.as_object_mut().unwrap().remove("oid");
    plain.as_object_mut().unwrap().remove("roles");
    let caller = auth.authenticate(&bearer(&token(tenant_key, Some("key-1"), &plain))).await.unwrap();
    assert_eq!((caller.subject.as_str(), caller.roles.len()), ("subject-pairwise", 0));

    // The keys were fetched once and then remembered.
    assert_eq!(reads.load(Ordering::SeqCst), 1);

    assert!(matches!(auth.authenticate(&HeaderMap::new()).await, Err(AuthError::Missing)));
    let mut basic = HeaderMap::new();
    basic.insert("authorization", "Basic dXNlcjpwYXNz".parse().unwrap());
    assert!(matches!(auth.authenticate(&basic).await, Err(AuthError::Missing)));

    let with = |change: &dyn Fn(&mut Value)| {
        let mut c = good.clone();
        change(&mut c);
        c
    };
    let unsigned = format!(
        "{}.{}.",
        URL_SAFE_NO_PAD.encode(r#"{"alg":"none","kid":"key-1"}"#),
        URL_SAFE_NO_PAD.encode(good.to_string())
    );
    let refused: Vec<(&str, String)> = vec![
        ("garbage", "not-a-token".to_string()),
        ("no key ID", token(tenant_key, None, &good)),
        ("an unknown key", token(tenant_key, Some("key-9"), &good)),
        ("a key type this API does not use", token(tenant_key, Some("ec-key"), &good)),
        (
            "another audience",
            token(tenant_key, Some("key-1"), &with(&|c| c["aud"] = json!("api://another-app"))),
        ),
        (
            "another tenant",
            token(tenant_key, Some("key-1"), &with(&|c| c["iss"] = json!(format!("{authority}/tenant-2/v2.0")))),
        ),
        ("expired", token(tenant_key, Some("key-1"), &with(&|c| c["exp"] = json!(now() - 3600)))),
        (
            "no expiry",
            token(tenant_key, Some("key-1"), &with(&|c| drop(c.as_object_mut().unwrap().remove("exp")))),
        ),
        ("unsigned", unsigned),
        ("a forger's key under the tenant's key ID", token(forger, Some("key-1"), &good)),
    ];
    for (what, token) in &refused {
        match auth.authenticate(&bearer(token)).await {
            Err(AuthError::Invalid(_)) => {}
            other => panic!("a token with {what} got {other:?}"),
        }
    }
}

/// Purpose: through the real routes, an Entra token with the right role issues a policy, one
/// without it is forbidden, a bad token is 401, and when the tenant's keys cannot be fetched
/// the answer is 503, not 401 and never a pass.
/// Why it matters: the routes turn authentication results into what callers see. If Entra ID
/// is unreachable the API cannot know who anyone is: it must fail closed, and say "try again"
/// (503) rather than "you are not allowed" (401), or every client would discard good tokens
/// during an outage.
#[tokio::test]
async fn the_routes_enforce_entra_tokens_and_fail_closed_without_the_keys() {
    let (authority, _) = tenant().await;
    let app = |auth: Auth| {
        let engine = Engine::start(&EngineConfig {
            partitions: 1,
            data_dir: None,
            opening_date: common::d(2027, 3, 1),
            load_shipped_config: true,
            azure: AzureConfig::default(),
        })
        .unwrap();
        router(Arc::new(AppState {
            backend: ic_api::Backend::single(engine),
            auth,
            scope: AudienceScope::shipped().unwrap(),
            customers: None,
        }))
    };
    let application = serde_json::to_value(ic_sim::persona::term_life(
        1,
        common::d(2027, 3, 1),
        common::d(1991, 2, 10),
        ic_product::Sex::Female,
        false,
        500_000_00,
        None,
        ic_product::Frequency::Monthly,
        None,
    ))
    .unwrap();
    let issue = |app: Router, token: Option<String>| {
        let application = application.clone();
        async move {
            use tower::ServiceExt;
            let mut request = axum::http::Request::builder()
                .method("POST")
                .uri("/v1/policies")
                .header("content-type", "application/json")
                .header("idempotency-key", "app-1")
                // Development headers are ignored when Entra ID is configured.
                .header("x-dev-subject", "intruder")
                .header("x-dev-roles", "new-business");
            if let Some(token) = token {
                request = request.header("authorization", format!("Bearer {token}"));
            }
            let response = app
                .oneshot(request.body(axum::body::Body::from(application.to_string())).unwrap())
                .await
                .unwrap();
            response.status()
        }
    };
    let (tenant_key, _) = keys();
    let good = claims(&authority);
    let mut servicing_only = good.clone();
    servicing_only["roles"] = json!(["policy-servicing"]);

    let entra = app(Auth::entra_at(&authority, TENANT, AUDIENCE));
    assert_eq!(issue(entra.clone(), None).await, StatusCode::UNAUTHORIZED);
    assert_eq!(issue(entra.clone(), Some("forged".to_string())).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        issue(entra.clone(), Some(token(tenant_key, Some("key-1"), &servicing_only))).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(issue(entra, Some(token(tenant_key, Some("key-1"), &good))).await, StatusCode::ACCEPTED);

    // Nothing is listening at this authority: the keys cannot be fetched.
    let unreachable = app(Auth::entra_at("http://127.0.0.1:1", TENANT, AUDIENCE));
    assert_eq!(
        issue(unreachable, Some(token(tenant_key, Some("key-1"), &good))).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
}
