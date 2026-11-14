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

/// Purpose: each call (record, commands, export, routing, spending hold) goes to the master's route for the person signed in, with the portal's token, the purpose the
/// master's policy keys on, the person as both party and actor, and on a change the version and idempotency key; routing is
/// asked under `self_service` too.
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
    assert_eq!(master.routing(&kiri).await.unwrap(), (200, json!({ "ok": "/v1/parties/party-1/routing" })));
    assert_eq!(
        master.spending_hold(&kiri).await.unwrap(),
        (200, json!({ "ok": "/v1/parties/party-1/spending-hold" }))
    );

    let seen = seen.lock().unwrap();
    let routes: Vec<_> = seen.iter().map(|(m, p, _)| format!("{m} {p}")).collect();
    assert_eq!(
        routes,
        [
            "GET /v1/parties/party-1?purpose=self_service",
            "POST /v1/parties/party-1/commands",
            "POST /v1/parties/party-1/commands",
            "GET /v1/parties/party-1/export?purpose=subject_access",
            "GET /v1/parties/party-1/routing",
            "GET /v1/parties/party-1/spending-hold",
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

/// Purpose: what a trained member of staff asks about markers goes to the master as staff, under the purpose its policy keeps for
/// vulnerability support, with the member of staff as the actor and no party header; a marker change carries its idempotency key.
/// Why it matters: the master shows a marker's kind and takes a change to it only for that purpose and a staff caller. Sent as the
/// portal's self-service it would be refused, and sent with the customer as actor the master's record of who changed it would be wrong.
#[tokio::test]
async fn the_master_is_asked_about_markers_as_the_trained_person() {
    use ic_channels::customers::Care;
    let (master, seen) = master().await;
    let staff = PartyId("hcs-1".into());
    let body = json!({ "command": "record", "reason": "remove at review rv-1", "facts": [{ "field": "vulnerability", "value": false }] });
    for call in [
        Care::Reviews,
        Care::Routing(PartyId("party-1".into())),
        Care::Marker {
            party: PartyId("party-1".into()),
            key: "k9".into(),
            body,
        },
        Care::Dismiss("rv/1".into()),
    ] {
        assert_eq!(master.care(&staff, call).await.unwrap().0, 200);
    }
    let seen = seen.lock().unwrap().clone();
    let routes: Vec<_> = seen.iter().map(|(m, p, _)| format!("{m} {p}")).collect();
    assert_eq!(
        routes,
        [
            "GET /v1/reviews",
            "GET /v1/parties/party-1/routing",
            "POST /v1/parties/party-1/commands",
            "POST /v1/reviews/rv1/dismiss"
        ]
    );
    for (_, _, headers) in seen.iter() {
        assert_eq!(
            (
                header(headers, "x-ic-caller"),
                header(headers, "x-ic-purpose"),
                header(headers, "x-ic-actor"),
                header(headers, "x-ic-party")
            ),
            (Some("staff"), Some("vulnerability_support"), Some("hcs-1"), None)
        );
    }
    assert_eq!(header(&seen[2].2, "idempotency-key"), Some("k9"));

    // Whether a caller may be told about someone: asked as staff too, for the act `view`.
    let (master, seen) = self::master().await;
    assert_eq!(
        master
            .speaks_for(&staff, &PartyId("party-1".into()), &PartyId("party/2".into()))
            .await
            .unwrap()
            .0,
        200
    );
    // An attorney's own authority: asked as the portal, for and about the person signed in.
    assert_eq!(master.represents(&PartyId("party-9".into()), &PartyId("party-1".into())).await.unwrap().0, 200);
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].1, "/v1/parties/party-1/authority?actor=party2&act=view");
    assert_eq!(seen[1].1, "/v1/parties/party-1/authority?actor=party-9&act=view");
    assert_eq!(
        (
            header(&seen[1].2, "x-ic-caller"),
            header(&seen[1].2, "x-ic-purpose"),
            header(&seen[1].2, "x-ic-actor"),
            header(&seen[1].2, "x-ic-party")
        ),
        (Some("portal"), Some("representative"), Some("party-9"), Some("party-9"))
    );
    assert_eq!(
        (
            header(&seen[0].2, "x-ic-caller"),
            header(&seen[0].2, "x-ic-purpose"),
            header(&seen[0].2, "x-ic-actor")
        ),
        (Some("staff"), Some("caller_check"), Some("hcs-1"))
    );
}

/// Purpose: a party ID cannot steer the request elsewhere on the master; a 503 is an error to retry; a body that is not JSON is
/// read as null with its status; an unreachable master is an error; and the portal's own check lets through only the two commands
/// a person sends about themselves; and a request goes to a person first when the master says so or cannot say.
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

    // Whether a request goes to a person first: as the master says; to a person when it cannot say; not with no master.
    use ic_channels::customers::route_to_person;
    assert!(route_to_person(Some(Ok((200, json!({ "route_to_person": true }))))));
    assert!(!route_to_person(Some(Ok((200, json!({ "route_to_person": false }))))));
    assert!(route_to_person(Some(Ok((403, json!({}))))));
    assert!(route_to_person(Some(Err("down".into()))));
    assert!(!route_to_person(None));
}
