//! The underwriting intake client against a stand-in HTTP service.

use std::sync::{Arc, Mutex};

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Router;
use ic_azure::credential::AzureCredential;
use ic_channels::disclosures::{answers, Hosted, Intake};
use serde_json::{json, Value};

type Seen = Arc<Mutex<Vec<(String, Option<String>)>>>;

async fn answer(State(seen): State<Seen>, request: Request) -> Response {
    let path = request.uri().path().to_string();
    let auth = request.headers().get("authorization").map(|v| v.to_str().unwrap().to_string());
    seen.lock().unwrap().push((path.clone(), auth));
    let body = axum::body::to_bytes(request.into_body(), 1 << 20).await.unwrap();
    let sent: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    match sent["proposal"].as_str() {
        Some("busy") => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Some("bad") => (StatusCode::UNPROCESSABLE_ENTITY, axum::Json(json!({ "problems": ["q-1"] }))).into_response(),
        _ => (StatusCode::OK, axum::Json(json!({ "at": path }))).into_response(),
    }
}

async fn intake() -> (Hosted, Seen) {
    let seen = Seen::default();
    let app = Router::new().fallback(answer).with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let hosted = Hosted {
        base,
        scope: "api://ic-underwriting-intake/.default".into(),
        credential: AzureCredential::static_token("portal-token"),
        http: reqwest::Client::new(),
    };
    (hosted, seen)
}

/// Purpose: each call (to sign, submit, and an underwriter's reading) goes to the intake's own routes with the portal's token; the intake's answer, including a 422 listing the
/// problems, comes back with its status; a 503 or an unreachable intake is an error to retry; and the request carries only the
/// fields the intake takes, with no adviser-entered questions defaulting to none.
/// Why it matters: the intake admits only callers holding the app role `new-business`, and the app must be able to tell answers that
/// do not fit (which the person corrects) from an intake that is busy (which it retries).
#[tokio::test]
async fn the_intake_is_called_as_the_portal() {
    let (intake, seen) = intake().await;
    let body = answers("prop-1", "life-1", &json!({ "step": "interview", "answers": {}, "extra": "dropped" })).unwrap();
    assert_eq!(
        body,
        json!({ "proposal": "prop-1", "life": "life-1", "step": "interview", "answers": {}, "adviser_entered": [] })
    );
    assert_eq!(intake.to_sign(body.clone()).await.unwrap(), (200, json!({ "at": "/v1/disclosures/to-sign" })));
    assert_eq!(intake.submit(body).await.unwrap(), (200, json!({ "at": "/v1/disclosures" })));
    let reading = json!({ "request": "k1", "proposal": "prop-1", "life": "life-1", "underwriter": "amy", "purpose": "assess" });
    assert_eq!(intake.open(reading).await.unwrap(), (200, json!({ "at": "/v1/disclosures/open" })));
    assert!(seen.lock().unwrap().iter().all(|(_, auth)| auth.as_deref() == Some("Bearer portal-token")));

    let bad = answers("bad", "life-1", &json!({ "step": "disclosure", "answers": {} })).unwrap();
    assert_eq!(intake.to_sign(bad).await.unwrap(), (422, json!({ "problems": ["q-1"] })));
    let busy = answers("busy", "life-1", &json!({ "step": "disclosure", "answers": {} })).unwrap();
    assert!(intake.submit(busy.clone()).await.unwrap_err().contains("try again"));
    let gone = Hosted {
        base: "http://127.0.0.1:9".into(),
        ..intake
    };
    assert!(gone.to_sign(busy).await.is_err());
}
