//! The customer master client against a stand-in HTTP service: what the
//! portal sends, and how it reads what comes back.

use std::sync::{Arc, Mutex};

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Router;
use ic_azure::credential::AzureCredential;
use ic_channels::customers::{check, Hosted, Master};
use ic_core::PartyId;
use serde_json::{json, Value};

type Seen = Arc<Mutex<Vec<(String, String, Vec<(String, String)>)>>>;

async fn answer(State(seen): State<Seen>, request: Request) -> Response {
    let path = request.uri().path_and_query().map(|p| p.to_string()).unwrap_or_default();
    let headers = request
        .headers()
        .iter()
        .filter(|(n, _)| n.as_str() == "authorization" || n.as_str().starts_with("x-ic-") || ["if-match", "idempotency-key"].contains(&n.as_str()))
        .map(|(n, v)| (n.to_string(), v.to_str().unwrap().to_string()))
        .collect();
    seen.lock().unwrap().push((request.method().to_string(), path.clone(), headers));
    if path.contains("busy") {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if path.contains("bare") {
        return StatusCode::NO_CONTENT.into_response();
    }
    (StatusCode::OK, axum::Json(json!({ "ok": path }))).into_response()
}

async fn master() -> (Hosted, Seen) {
    let seen = Seen::default();
    let app = Router::new().fallback(answer).with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let hosted = Hosted {
        base,
        scope: "api://ic-customers/.default".into(),
        credential: AzureCredential::static_token("portal-token"),
        http: reqwest::Client::new(),
    };
    (hosted, seen)
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
}

/// Purpose: each of the three calls goes to the master's route for the person signed in, with the portal's token, the purpose the
/// master's policy keys on, the person as both party and actor, and on a change the version and idempotency key.
/// Why it matters: the master decides what the portal may see by these headers alone; a wrong purpose or party would either be
/// refused or, worse, read as someone else.
#[tokio::test]
async fn the_master_is_asked_about_the_person_signed_in() {
    let (master, seen) = master().await;
    let kiri = PartyId("party-1".into());
    assert_eq!(
        master.record(&kiri).await.unwrap(),
        (200, json!({ "ok": "/v1/parties/party-1?purpose=self_service" }))
    );
    let change = json!({ "command": "record", "fields": { "email": "k@new.nz" } });
    assert_eq!(master.command(&kiri, Some("3".into()), "k1".into(), change).await.unwrap().0, 200);
    assert_eq!(
        master.command(&kiri, None, "k2".into(), json!({ "command": "set_consent" })).await.unwrap().0,
        200
    );
    assert_eq!(master.export(&kiri).await.unwrap().0, 200);

    let seen = seen.lock().unwrap();
    let routes: Vec<_> = seen.iter().map(|(m, p, _)| format!("{m} {p}")).collect();
    assert_eq!(
        routes,
        [
            "GET /v1/parties/party-1?purpose=self_service",
            "POST /v1/parties/party-1/commands",
            "POST /v1/parties/party-1/commands",
            "GET /v1/parties/party-1/export?purpose=subject_access",
        ]
    );
    for (_, _, headers) in seen.iter() {
        assert_eq!(header(headers, "authorization"), Some("Bearer portal-token"));
        assert_eq!(
            (header(headers, "x-ic-caller"), header(headers, "x-ic-party"), header(headers, "x-ic-actor")),
            (Some("portal"), Some("party-1"), Some("party-1"))
        );
    }
    assert_eq!(header(&seen[1].2, "x-ic-purpose"), Some("self_service"));
    assert_eq!((header(&seen[1].2, "if-match"), header(&seen[1].2, "idempotency-key")), (Some("3"), Some("k1")));
    assert_eq!((header(&seen[2].2, "if-match"), header(&seen[2].2, "idempotency-key")), (None, Some("k2")));
    assert_eq!(header(&seen[3].2, "x-ic-purpose"), Some("subject_access"));
}

/// Purpose: a party ID cannot steer the request elsewhere on the master; a 503 is an error to retry; a body that is not JSON is
/// read as null with its status; an unreachable master is an error; and the portal's own check lets through only the two commands
/// a person sends about themselves.
/// Why it matters: party IDs come from the directory, but the URL is built from them, so a stray `/` or `?` must not reach another
/// route; and the app must be able to tell "try again" from "refused".
#[tokio::test]
async fn the_client_reads_every_kind_of_answer() {
    let (master, seen) = master().await;
    master.record(&PartyId("p/../../admin?x=1".into())).await.unwrap();
    assert_eq!(seen.lock().unwrap()[0].1, "/v1/parties/padminx1?purpose=self_service");
    assert!(master.record(&PartyId("busy".into())).await.unwrap_err().contains("try again"));
    assert_eq!(master.export(&PartyId("bare".into())).await.unwrap(), (204, Value::Null));

    let gone = Hosted {
        base: "http://127.0.0.1:9".into(),
        ..master
    };
    assert!(gone.record(&PartyId("p".into())).await.is_err());

    assert!(check(&json!({ "command": "record" })).is_ok());
    assert!(check(&json!({ "command": "set_consent" })).is_ok());
    assert!(check(&json!({ "command": "mark_deceased" }))
        .unwrap_err()
        .contains("not something you change yourself"));
    assert!(check(&json!({})).unwrap_err().contains("names itself"));
}
