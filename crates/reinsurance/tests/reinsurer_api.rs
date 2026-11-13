//! The API reinsurers read their own statements and listings from: who may
//! call, what each reinsurer sees, and how an outage reads.

mod common;

use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::routing::get;
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use common::*;
use ic_azure::blob::{MemoryWormStore, WormStore};
use ic_azure::AzureError;
use ic_raft::ClusterAuth;
use ic_reinsurance::driver::{DriverError, LocalModule, Module};
use ic_reinsurance::host::Query;
use ic_reinsurance::reinsurer_api::{router, Api, ApiConfig, ReinsurerAuth, READ_ROLE};
use ic_reinsurance::{Applied, Command, Envelope, Notice};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::RsaPrivateKey;
use serde_json::{json, Value};

const TENANT: &str = "tenant-1";
const AUDIENCE: &str = "api://ic-reinsurance";
const NORTH_APP: &str = "app-north";
const SOUTH_APP: &str = "app-south";

struct Signing {
    key: EncodingKey,
    jwk: Value,
}

fn signing() -> &'static Signing {
    static KEY: OnceLock<Signing> = OnceLock::new();
    KEY.get_or_init(|| {
        let private = RsaPrivateKey::new(&mut rand::rngs::OsRng, 2048).unwrap();
        let pem = private.to_pkcs1_pem(rsa::pkcs1::LineEnding::LF).unwrap();
        Signing {
            key: EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap(),
            jwk: json!({
                "kid": "k1",
                "kty": "RSA",
                "n": URL_SAFE_NO_PAD.encode(private.n().to_bytes_be()),
                "e": URL_SAFE_NO_PAD.encode(private.e().to_bytes_be()),
            }),
        }
    })
}

/// A stand-in tenant that publishes the signing key.
async fn tenant() -> String {
    let app = Router::new().route("/{tenant}/discovery/v2.0/keys", get(|| async { Json(json!({ "keys": [signing().jwk] })) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let authority = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    authority
}

/// A token with these claims over the usual ones.
fn token_with(authority: &str, kid: &str, overrides: Value) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let mut claims = json!({
        "iss": format!("{authority}/{TENANT}/v2.0"),
        "aud": AUDIENCE,
        "exp": now + 600,
        "sub": "service-principal",
        "azp": NORTH_APP,
        "roles": [READ_ROLE],
    });
    for (k, v) in overrides.as_object().unwrap() {
        if v.is_null() {
            claims.as_object_mut().unwrap().remove(k);
        } else {
            claims[k] = v.clone();
        }
    }
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(kid.into());
    encode(&header, &claims, &signing().key).unwrap()
}

fn token(authority: &str, app: &str) -> String {
    token_with(authority, "k1", json!({"azp": app}))
}

fn config(authority: &str) -> ApiConfig {
    ApiConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        authority: authority.into(),
        tenant: TENANT.into(),
        audience: AUDIENCE.into(),
        clients: BTreeMap::from([(NORTH_APP.into(), north()), (SOUTH_APP.into(), south())]),
    }
}

/// North's first statement, cut in a module, and the module.
fn module_with_a_statement() -> (LocalModule, Notice) {
    let mut g = Bench::new();
    g.issue_scheme(2, "S1", d(2027, 2, 1), vec![member_cover("M1", 200 * K)]);
    g.premium_due(2, "S1", 1, d(2027, 2, 1), d(2027, 3, 1), &[("group_life", 100_00)]);
    g.today = d(2027, 3, 2);
    let cut = Command::CutStatement {
        treaty: group(),
        reinsurer: north(),
        period_end: d(2027, 2, 28),
    };
    let document = g
        .ok("month-end", cut)
        .notices
        .into_iter()
        .find(|n| matches!(n, Notice::Document { .. }))
        .unwrap();
    let module = LocalModule::default();
    for envelope in g.log.clone() {
        module.apply(envelope);
    }
    (module, document)
}

/// The API on a local port, and its address.
async fn site<M: Module + 'static, W: WormStore + 'static>(module: M, records: W, authority: &str) -> String {
    let app = router(Api {
        module,
        records,
        auth: ReinsurerAuth::new(&config(authority)),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

async fn fetch(url: &str, bearer: Option<&str>) -> (u16, reqwest::header::HeaderMap, Vec<u8>) {
    let mut request = reqwest::Client::new().get(url);
    if let Some(t) = bearer {
        request = request.bearer_auth(t);
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    (status, headers, response.bytes().await.unwrap().to_vec())
}

async fn records_holding(document: &Notice) -> MemoryWormStore {
    let Notice::Document { name, body, .. } = document else { unreachable!() };
    let records = MemoryWormStore::default();
    records.put_new(name, "application/json", body.clone().into_bytes()).await.unwrap();
    // South's own statement under the same treaty, and something of the insurer's own.
    records
        .put_new(
            "reinsurance/group-life-surplus/reinsurer-south/statement-9.json",
            "application/json",
            b"{\"south\":true}".to_vec(),
        )
        .await
        .unwrap();
    records
        .put_new("reinsurance/group-life-surplus/reinsurer-north/notes.json", "application/json", b"{}".to_vec())
        .await
        .unwrap();
    records
}

/// Purpose: a reinsurer signed in with its own client lists its statements, with the file
/// name, hash, period, net and status of each, pages through them, and reads its statement
/// byte for byte with the file's hash beside it. Another reinsurer asking the same paths
/// sees none of it, and its own file under the same treaty is out of the first reinsurer's
/// reach; names that are not a statement's, or that try to leave the reinsurer's folder,
/// are not found.
/// Why it matters: listings carry policy-level facts about lives. Which reinsurer is asking
/// must come from the signed token alone, so no request can be shaped to read another
/// reinsurer's book or the insurer's own records.
#[tokio::test]
async fn a_reinsurer_reads_only_its_own_statements_and_files() {
    let authority = tenant().await;
    let (module, document) = module_with_a_statement();
    let Notice::Document { body, sha256, .. } = &document else { unreachable!() };
    let base = site(module, records_holding(&document).await, &authority).await;
    let north_token = token(&authority, NORTH_APP);
    let south_token = token(&authority, SOUTH_APP);

    let (status, _, listed) = fetch(&format!("{base}/v1/statements"), Some(&north_token)).await;
    assert_eq!(status, 200);
    let listed: Value = serde_json::from_slice(&listed).unwrap();
    let statements = listed["statements"].as_array().unwrap();
    assert_eq!(statements.len(), 1);
    let first = &statements[0];
    assert_eq!(first["number"], 1);
    assert_eq!(first["treaty"], "group-life-surplus");
    assert_eq!(first["file"], "group-life-surplus/statement-1.json");
    assert_eq!(first["sha256"], sha256.as_str());
    assert_eq!(first["status"], "issued");
    assert_eq!(first["period_end"], "2027-02-28");
    assert_eq!(first["supplementary"], json!([]));
    assert_eq!(listed["next_after"], Value::Null, "a short page is the last");

    // Paging: a full page names where the next begins.
    let (_, _, page) = fetch(&format!("{base}/v1/statements?limit=1"), Some(&north_token)).await;
    let page: Value = serde_json::from_slice(&page).unwrap();
    assert_eq!(page["next_after"], 1);
    let (_, _, rest) = fetch(&format!("{base}/v1/statements?after=1&limit=0"), Some(&north_token)).await;
    assert_eq!(serde_json::from_slice::<Value>(&rest).unwrap(), json!({"statements": [], "next_after": null}));

    let (status, headers, file) = fetch(&format!("{base}/v1/files/group-life-surplus/statement-1.json"), Some(&north_token)).await;
    assert_eq!(status, 200);
    assert_eq!(file, body.as_bytes());
    assert_eq!(headers["x-content-sha256"], sha256.as_str());
    assert_eq!(headers["content-type"], "application/json");
    assert_eq!(headers["cache-control"], "no-store");

    // South sees no statements of north's, cannot read north's file, and reads its own.
    let (_, _, theirs) = fetch(&format!("{base}/v1/statements"), Some(&south_token)).await;
    assert_eq!(serde_json::from_slice::<Value>(&theirs).unwrap()["statements"], json!([]));
    let (status, _, _) = fetch(&format!("{base}/v1/files/group-life-surplus/statement-1.json"), Some(&south_token)).await;
    assert_eq!(status, 404);
    let (status, _, own) = fetch(&format!("{base}/v1/files/group-life-surplus/statement-9.json"), Some(&south_token)).await;
    assert_eq!((status, own), (200, b"{\"south\":true}".to_vec()));
    let (status, _, _) = fetch(&format!("{base}/v1/files/group-life-surplus/statement-9.json"), Some(&north_token)).await;
    assert_eq!(status, 404, "south's statement is not north's to read");

    for path in [
        "group-life-surplus/notes.json",
        "group-life-surplus/statement-1.txt",
        "..%2Freinsurer-south/statement-9.json",
        "group-life-surplus/..%2F..%2Freleases%2Fstatement-1.json",
        ".hidden/statement-1.json",
        "group-life-surplus/statement-2.json",
    ] {
        let (status, _, refused) = fetch(&format!("{base}/v1/files/{path}"), Some(&north_token)).await;
        assert_eq!(status, 404, "{path}");
        assert_eq!(serde_json::from_slice::<Value>(&refused).unwrap()["code"], "not_found");
    }
    let (status, _, ok) = fetch(&format!("{base}/healthz"), None).await;
    assert_eq!((status, ok), (200, b"ok".to_vec()));
}

/// Purpose: a request with no token, a token that is not one, one for another audience or
/// issuer, an expired one, one signed by a key the tenant does not publish, or one that
/// names no client is refused as unauthenticated and asks for a bearer token; a valid token
/// for a client that is not a configured reinsurer, or without the read role, is forbidden.
/// A version 1 token naming its client as `appid` is read the same way as `azp`.
/// Why it matters: this API is reachable from outside the insurer. Any gap in checking the
/// token would hand a stranger the listings.
#[tokio::test]
async fn requests_without_a_good_token_are_refused() {
    let authority = tenant().await;
    let (module, document) = module_with_a_statement();
    let base = site(module, records_holding(&document).await, &authority).await;
    let url = format!("{base}/v1/statements");

    let (status, headers, body) = fetch(&url, None).await;
    assert_eq!(status, 401);
    assert_eq!(headers["www-authenticate"], "Bearer");
    assert_eq!(serde_json::from_slice::<Value>(&body).unwrap()["code"], "unauthenticated");
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let unauthenticated = [
        "not.a.token".to_string(),
        token_with(&authority, "k1", json!({"aud": "api://someone-else"})),
        token_with(&authority, "k1", json!({"iss": "https://elsewhere/v2.0"})),
        token_with(&authority, "k1", json!({"exp": now - 3600})),
        token_with(&authority, "k9", json!({})),
        token_with(&authority, "k1", json!({"azp": null})),
    ];
    for bad in &unauthenticated {
        let (status, _, _) = fetch(&url, Some(bad)).await;
        assert_eq!(status, 401, "{bad}");
    }
    // A token with no key ID at all.
    let header = Header::new(Algorithm::RS256);
    let no_kid = encode(&header, &json!({"aud": AUDIENCE, "exp": now + 600}), &signing().key).unwrap();
    assert_eq!(fetch(&url, Some(&no_kid)).await.0, 401);

    for forbidden in [
        token_with(&authority, "k1", json!({"azp": "app-stranger"})),
        token_with(&authority, "k1", json!({"roles": ["Something.Else"]})),
        token_with(&authority, "k1", json!({"roles": null})),
    ] {
        let (status, _, body) = fetch(&url, Some(&forbidden)).await;
        assert_eq!(status, 403);
        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap()["code"], "forbidden");
    }
    let v1 = token_with(&authority, "k1", json!({"azp": null, "appid": NORTH_APP}));
    assert_eq!(fetch(&url, Some(&v1)).await.0, 200);
    let file = format!("{base}/v1/files/group-life-surplus/statement-1.json");
    assert_eq!(fetch(&file, None).await.0, 401);
    assert_eq!(fetch(&file, Some(&token_with(&authority, "k1", json!({"azp": "app-stranger"})))).await.0, 403);
}

/// A records store that cannot be reached.
#[derive(Clone)]
struct Away;

impl WormStore for Away {
    async fn put_new(&self, _: &str, _: &str, _: Vec<u8>) -> Result<(), AzureError> {
        Err(AzureError::Response("away".into()))
    }
    async fn get(&self, _: &str) -> Result<Option<Vec<u8>>, AzureError> {
        Err(AzureError::Response("away".into()))
    }
}

/// A module that answers every question with a refusal.
struct Confused;

impl Module for Confused {
    async fn propose(&self, _: Envelope) -> Result<Applied, DriverError> {
        Err(DriverError::Answer("no".into()))
    }
    async fn ask(&self, _: Query) -> Result<Value, DriverError> {
        Err(DriverError::Answer("not a question of this module".into()))
    }
}

/// Purpose: while the reinsurance module, the records store or the tenant's signing keys
/// cannot be reached the API answers "unavailable" (503); a module that refuses the
/// question is an internal error (500), not an empty list.
/// Why it matters: a reinsurer's system reconciling statements must be able to tell "try
/// later" from "there is nothing", or an outage would read as statements withdrawn.
#[tokio::test]
async fn outages_read_as_outages() {
    let authority = tenant().await;
    let (module, document) = module_with_a_statement();
    let north = token(&authority, NORTH_APP);

    module.set_down(true);
    let base = site(module, Away, &authority).await;
    let (status, _, body) = fetch(&format!("{base}/v1/statements"), Some(&north)).await;
    assert_eq!(status, 503);
    assert_eq!(serde_json::from_slice::<Value>(&body).unwrap()["code"], "unavailable");
    let (status, _, _) = fetch(&format!("{base}/v1/files/group-life-surplus/statement-1.json"), Some(&north)).await;
    assert_eq!(status, 503);

    let base = site(Confused, records_holding(&document).await, &authority).await;
    let (status, _, body) = fetch(&format!("{base}/v1/statements"), Some(&north)).await;
    assert_eq!(status, 500);
    assert_eq!(serde_json::from_slice::<Value>(&body).unwrap()["code"], "internal");
    assert!(Confused
        .propose(Envelope {
            key: "k".into(),
            caller: "c".into(),
            on: d(2027, 1, 1),
            command: Command::CoreEvents { events: vec![] },
        })
        .await
        .is_err());

    // A tenant that is not there: sign-in cannot be checked.
    let gone = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let (module, document) = module_with_a_statement();
    let base = site(module, records_holding(&document).await, &gone).await;
    let (status, _, _) = fetch(&format!("{base}/v1/statements"), Some(&token(&gone, NORTH_APP))).await;
    assert_eq!(status, 503);
    // A tenant that answers with something other than keys.
    let wrong = {
        let app = Router::new().route("/{tenant}/discovery/v2.0/keys", get(|| async { "not keys" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let authority = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        authority
    };
    let (module, document) = module_with_a_statement();
    let base = site(module, records_holding(&document).await, &wrong).await;
    assert_eq!(fetch(&format!("{base}/v1/statements"), Some(&token(&wrong, NORTH_APP))).await.0, 503);
}

fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: BTreeMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    move |key| map.get(key).cloned()
}

/// Purpose: the API's settings: absent unless its address is set; once set, the tenant, the
/// audience and at least one client are required, each client entry must be `<client
/// id>=<reinsurer>`, and no client may be named twice. The authority defaults to Entra's.
/// Why it matters: an API started without knowing its callers would either serve nobody or,
/// worse, someone it should not; a client listed twice could map to two reinsurers.
#[test]
fn the_api_settings_are_whole_or_absent() {
    assert_eq!(ApiConfig::from_lookup(lookup(&[])), Ok(None));
    let listen = ("IC_REINSURANCE_API_LISTEN", "0.0.0.0:8443");
    let tenant = ("IC_ENTRA_TENANT", TENANT);
    let audience = ("IC_REINSURANCE_API_AUDIENCE", AUDIENCE);
    let clients = ("IC_REINSURANCE_API_CLIENTS", " app-north = reinsurer-north , app-south=reinsurer-south,");
    let read = ApiConfig::from_lookup(lookup(&[listen, tenant, audience, clients])).unwrap().unwrap();
    assert_eq!(read.listen, "0.0.0.0:8443".parse().unwrap());
    assert_eq!(read.authority, "https://login.microsoftonline.com");
    assert_eq!(read.clients, BTreeMap::from([("app-north".into(), north()), ("app-south".into(), south())]));
    let err = |pairs: &[(&str, &str)]| ApiConfig::from_lookup(lookup(pairs)).unwrap_err();
    assert!(err(&[("IC_REINSURANCE_API_LISTEN", "everywhere")]).contains("address and port"));
    assert!(err(&[listen]).contains("IC_ENTRA_TENANT"));
    assert!(err(&[listen, tenant]).contains("IC_REINSURANCE_API_AUDIENCE"));
    assert!(err(&[listen, tenant, audience]).contains("IC_REINSURANCE_API_CLIENTS"));
    assert!(err(&[listen, tenant, audience, ("IC_REINSURANCE_API_CLIENTS", "app-north")]).contains("<client id>=<reinsurer>"));
    assert!(err(&[listen, tenant, audience, ("IC_REINSURANCE_API_CLIENTS", "=reinsurer-north")]).contains("<client id>=<reinsurer>"));
    assert!(err(&[listen, tenant, audience, ("IC_REINSURANCE_API_CLIENTS", " , ")]).contains("names no client"));
    assert!(err(&[listen, tenant, audience, ("IC_REINSURANCE_API_CLIENTS", "a=reinsurer-north,a=reinsurer-south")]).contains("twice"));
    let elsewhere = ApiConfig::from_lookup(lookup(&[listen, tenant, audience, clients, ("IC_ENTRA_AUTHORITY", "https://login.example")]))
        .unwrap()
        .unwrap();
    assert_eq!(elsewhere.authority, "https://login.example");
}

/// Purpose: the driver process serves the API beside its loop when the API is configured:
/// it answers on its address while the driver runs, both stop together when told, and a
/// driver whose API address is already taken refuses to start, naming the address.
/// Why it matters: this is how the API is deployed. An API left running after its driver
/// was told to stop would hold the pod open; one that silently failed to listen would leave
/// reinsurers with nothing to call while the pod looked healthy.
#[tokio::test]
async fn the_driver_process_serves_the_api_and_stops_it() {
    let authority = tenant().await;
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut api = config(&authority);
    api.listen = format!("127.0.0.1:{port}").parse().unwrap();
    let settings = ic_reinsurance::service::Config {
        cluster: "127.0.0.1:1".into(),
        cycle_every: Duration::from_millis(5),
        patience: Duration::from_millis(20),
        batch: 10,
        records_endpoint: String::new(),
        records_container: String::new(),
        event_hubs: String::new(),
        event_hub: String::new(),
        drops_endpoint: None,
        api: Some(api),
    };
    let staff = || ic_azure::eventhubs::EventHubsSink::new("http://127.0.0.1:1", "h", ic_azure::credential::AzureCredential::static_token("t"));
    let auth = || ClusterAuth::tokens(vec!["test-cluster-token-0123456789abcdef".to_string()]).unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let served = tokio::spawn(ic_reinsurance::service::serve(
        settings.clone(),
        auth(),
        MemoryWormStore::default(),
        staff(),
        ic_reinsurance::carriers::ReinsurerDrops::Off,
        async {
            let _ = stopped.await;
        },
    ));
    let health = format!("http://127.0.0.1:{port}/healthz");
    let mut up = false;
    for _ in 0..100 {
        if reqwest::get(&health).await.is_ok_and(|r| r.status().is_success()) {
            up = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(up, "the API answers while the driver runs");
    // With no cluster behind it, the list is an outage, not an empty list.
    let (status, _, _) = fetch(&format!("http://127.0.0.1:{port}/v1/statements"), Some(&token(&authority, NORTH_APP))).await;
    assert_eq!(status, 503);

    // The address is taken while it runs.
    let taken = ic_reinsurance::service::serve(
        settings.clone(),
        auth(),
        MemoryWormStore::default(),
        staff(),
        ic_reinsurance::carriers::ReinsurerDrops::Off,
        std::future::ready(()),
    )
    .await
    .unwrap_err();
    assert!(taken.contains(&format!("127.0.0.1:{port}")), "{taken}");

    stop.send(()).unwrap();
    assert_eq!(served.await.unwrap(), Ok(()));
    assert!(reqwest::get(&health).await.is_err(), "the API stops with the driver");
}
