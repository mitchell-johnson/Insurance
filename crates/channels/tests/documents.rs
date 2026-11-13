//! The documents client against a stand-in HTTP service.

use std::sync::{Arc, Mutex};

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Router;
use ic_azure::credential::AzureCredential;
use ic_channels::documents::{id, Asker, Documents, Hosted};

type Seen = Arc<Mutex<Vec<Vec<(String, String)>>>>;

async fn answer(State(seen): State<Seen>, request: Request) -> Response {
    let headers = request
        .headers()
        .iter()
        .filter(|(n, _)| n.as_str() == "authorization" || n.as_str().starts_with("x-ic-"))
        .map(|(n, v)| (n.to_string(), v.to_str().unwrap().to_string()))
        .collect();
    seen.lock().unwrap().push(headers);
    match request.uri().path() {
        "/v1/documents/doc-1/pdf" => ([("content-type", "application/pdf")], b"%PDF".to_vec()).into_response(),
        "/v1/documents/busy/pdf" => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        "/v1/documents/bare/pdf" => (StatusCode::NOT_FOUND, axum::body::Body::empty()).into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Purpose: a party's download names the party under the role `portal`, a staff download names the actor and purpose under the role
/// `staff`, both with the portal's token; the file comes back with its type; a 503 or an unreachable service is an error to retry; and
/// only plain document IDs are accepted.
/// Why it matters: the documents module's rule is chosen by these headers; a party named wrongly would be refused or, worse, served
/// someone else's letter, and an ID with a path in it must never reach its URL.
#[tokio::test]
async fn documents_are_asked_for_as_the_person() {
    let seen = Seen::default();
    let app = Router::new().fallback(answer).with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let documents = Hosted {
        base,
        scope: "api://ic-documents/.default".into(),
        credential: AzureCredential::static_token("t"),
        http: reqwest::Client::new(),
    };

    let got = documents.pdf("doc-1", Asker::Party("party-1".into())).await.unwrap();
    assert_eq!(
        (got.status, got.content_type.as_str(), got.body.as_slice()),
        (200, "application/pdf", b"%PDF".as_slice())
    );
    let staff = Asker::Staff {
        actor: "rec-1".into(),
        purpose: "complaint".into(),
    };
    assert_eq!(documents.pdf("nope", staff).await.unwrap().status, 404);
    assert_eq!(
        documents.pdf("bare", Asker::Party("p".into())).await.unwrap().content_type,
        "application/octet-stream"
    );
    assert!(documents.pdf("busy", Asker::Party("p".into())).await.unwrap_err().contains("try again"));
    let seen = seen.lock().unwrap().clone();
    let has = |i: usize, n: &str, v: &str| seen[i].iter().any(|(a, b)| a == n && b == v);
    assert!(has(0, "authorization", "Bearer t") && has(0, "x-ic-caller", "portal") && has(0, "x-ic-party", "party-1"));
    assert!(has(1, "x-ic-caller", "staff") && has(1, "x-ic-actor", "rec-1") && has(1, "x-ic-purpose", "complaint"));
    let gone = Hosted {
        base: "http://127.0.0.1:9".into(),
        ..documents
    };
    assert!(gone.pdf("doc-1", Asker::Party("p".into())).await.is_err());

    for ok in ["doc-1", "2027-11/abc".replace('/', "-").as_str(), "n:1.pdf"] {
        assert!(id(ok).is_some(), "{ok}");
    }
    for bad in ["", "a/b", "..", "a b", &"x".repeat(129)] {
        assert!(id(bad).is_none(), "{bad}");
    }
}
