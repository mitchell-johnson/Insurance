//! The check with the customer master before a servicing request that moves
//! value or cover, over the API and against a stand-in for the master.

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use common::{d, Call};
use ic_api::auth::Auth;
use ic_api::engine::{AzureConfig, Engine, EngineConfig};
use ic_api::routes::{router, AppState};
use ic_api::servicing::{Hosted, ACTING_PARTY, CONVERSATION, REVIEW};
use ic_azure::credential::AzureCredential;
use ic_product::{Frequency, Sex};
use ic_readmodel::scope::AudienceScope;
use ic_sim::persona::term_life;
use serde_json::{json, Value};

/// What the stand-in master answers, by question, and what it was asked.
#[derive(Default)]
struct Master {
    answers: Mutex<BTreeMap<&'static str, (u16, Value)>>,
    asked: Mutex<Vec<String>>,
}

type Shared = Arc<Master>;

impl Master {
    fn set(&self, question: &'static str, status: u16, body: Value) {
        self.answers.lock().unwrap().insert(question, (status, body));
    }
    fn answer(&self, question: &'static str, headers: &HeaderMap, asked: String) -> (StatusCode, Json<Value>) {
        // The master lets servicing in only by its app role and purpose.
        let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
        if (header("authorization"), header("x-ic-caller"), header("x-ic-purpose"))
            != ("Bearer api-token".into(), "policy-servicing".into(), "servicing".into())
        {
            return (StatusCode::FORBIDDEN, Json(Value::Null));
        }
        self.asked.lock().unwrap().push(asked);
        let (status, body) = self.answers.lock().unwrap().get(question).cloned().unwrap_or((200, json!({})));
        (StatusCode::from_u16(status).unwrap(), Json(body))
    }
}

async fn routing(
    State(m): State<Shared>,
    Path(party): Path<String>,
    Query(q): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> (StatusCode, Json<Value>) {
    m.answer("routing", &headers, format!("routing {party} {}", q["act"]))
}

async fn authority(
    State(m): State<Shared>,
    Path(party): Path<String>,
    Query(q): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> (StatusCode, Json<Value>) {
    m.answer("authority", &headers, format!("authority {party} {} {} {}", q["actor"], q["act"], q["on"]))
}

/// The API in one process with one term life policy, checking servicing
/// with a stand-in master at `base` (or one started here).
async fn app(base: Option<String>) -> (Router, Shared) {
    let master = Shared::default();
    let base = match base {
        Some(base) => base,
        None => {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let stand_in = Router::new()
                .route("/v1/parties/{party}/routing", get(routing))
                .route("/v1/parties/{party}/authority", get(authority))
                .with_state(master.clone());
            tokio::spawn(async move { axum::serve(listener, stand_in).await.unwrap() });
            format!("http://{address}/")
        }
    };
    let cfg = EngineConfig {
        partitions: 2,
        data_dir: None,
        opening_date: d(2027, 3, 1),
        load_shipped_config: true,
        azure: AzureConfig::default(),
    };
    let app = router(Arc::new(AppState {
        backend: ic_api::Backend::single(Engine::start(&cfg).unwrap()),
        auth: Auth::Dev,
        scope: AudienceScope::shipped().unwrap(),
        customers: Some(Arc::new(Hosted {
            base,
            scope: "api://customers/.default".into(),
            credential: AzureCredential::static_token("api-token"),
            http: reqwest::Client::new(),
        })),
    }));
    let application = serde_json::to_value(term_life(
        1,
        d(2027, 3, 1),
        d(1991, 2, 10),
        Sex::Female,
        false,
        500_000_00,
        None,
        Frequency::Monthly,
        None,
    ))
    .unwrap();
    let (status, body) = Call::new("POST", "/v1/policies")
        .roles("new-business")
        .key("app-1")
        .body(application)
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    (app, master)
}

fn cancel<'a>(key: &'a str) -> Call<'a> {
    let body = json!({ "policy": "pol-00000001", "command": { "cancel_policy": { "policy": "pol-00000001" } } });
    Call::new("POST", "/v1/commands").roles("policy-servicing").key(key).body(body)
}

async fn status_of(app: &Router) -> Value {
    let (_, view) = Call::new("GET", "/v1/policies/KTL00000001").roles("policy-servicing").send(app).await;
    view["policy"]["status"].clone()
}

/// Purpose: a cancellation for a policyholder who asked for a pause is held with 409 and a
/// reason until it comes back with the conversation's reference; where a specialist's review
/// is also asked for it is held again until that reference comes too; then it is applied,
/// the person's decision standing. The master is asked about the policyholder and the act,
/// and a held request changes nothing on the policy.
/// Why it matters: persona tests VUL-003 and VUL-005. The pause is the one moment a trained
/// person can ask someone at risk of financial abuse whether this is what they want. A
/// request that slipped past it cannot be taken back once the cover is gone; a pause that
/// became a refusal would take the decision away from the person it belongs to.
#[tokio::test]
async fn a_request_is_held_for_a_conversation_and_then_stands() {
    let (app, master) = app(None).await;
    master.set(
        "routing",
        200,
        json!({ "route_to_person": true, "pause_for_conversation": true, "specialist_review": true }),
    );

    let (status, body) = cancel("c-1").send(&app).await;
    assert_eq!(
        (status, body["title"].as_str()),
        (StatusCode::CONFLICT, Some("pause_for_conversation")),
        "{body}"
    );
    assert_eq!(status_of(&app).await, "in_force");
    assert_eq!(master.asked.lock().unwrap().last().unwrap(), "routing party-00000001 cover");

    let (status, body) = cancel("c-1").header(CONVERSATION, "case-881").send(&app).await;
    assert_eq!((status, body["title"].as_str()), (StatusCode::CONFLICT, Some("specialist_review")), "{body}");
    assert_eq!(status_of(&app).await, "in_force");

    let (status, body) = cancel("c-1").header(CONVERSATION, "case-881").header(REVIEW, "review-12").send(&app).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(status_of(&app).await, "cancelled");
}

/// Purpose: someone other than the policyholder asking is put to the master's authority
/// question for that act on the business date: no authority is refused with 403, an
/// authority in question holds the request with 409, and an authority that allows the act
/// lets it through to the ordinary check.
/// Why it matters: persona tests VUL-008 and VUL-018. An attorney's authority has a scope
/// and dates; a relative with none must not be able to cancel someone's cover by asking
/// confidently, and while capacity is being decided nothing may change on the policy.
#[tokio::test]
async fn a_representative_acts_only_with_an_authority_for_the_act() {
    let (app, master) = app(None).await;
    let asking = |key: &'static str| cancel(key).header(ACTING_PARTY, "party-son");

    master.set("authority", 200, json!({ "decision": "refuse", "reason": "no_authority" }));
    let (status, body) = asking("c-1").send(&app).await;
    assert_eq!((status, body["title"].as_str()), (StatusCode::FORBIDDEN, Some("not_authorised")), "{body}");
    assert_eq!(
        master.asked.lock().unwrap().last().unwrap(),
        "authority party-00000001 party-son cover 2027-03-01"
    );

    master.set("authority", 200, json!({ "decision": "route", "reason": "capacity_in_question" }));
    let (status, body) = asking("c-1").send(&app).await;
    assert_eq!(
        (status, body["title"].as_str()),
        (StatusCode::CONFLICT, Some("authority_in_question")),
        "{body}"
    );
    assert_eq!(status_of(&app).await, "in_force");

    master.set("authority", 200, json!({ "decision": "allow", "relationship": "attorney" }));
    let (status, body) = asking("c-1").send(&app).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(status_of(&app).await, "cancelled");
    // The policyholder named as the one asking is not a representative.
    let asked = master.asked.lock().unwrap().len();
    let (status, _) = cancel("c-2").header(ACTING_PARTY, "party-00000001").send(&app).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "the ledger answers: the policy is already cancelled");
    assert_eq!(master.asked.lock().unwrap().len(), asked + 1, "routing only");
}

/// Purpose: when the master cannot answer (an error status on either question, or no
/// master listening at all) a checked request is answered 503 and nothing changes; a party
/// the master does not hold, a servicing command that is not a checked act, and a command
/// from another module go through without a pause.
/// Why it matters: failing open would mean that an outage of the customer master silently
/// removes the protection for exactly the people it exists for, while failing closed on
/// commands the check does not concern would stop premium collection and claims whenever
/// the master is down.
#[tokio::test]
async fn an_unanswered_check_sends_nothing_on_and_other_commands_are_not_checked() {
    let (app, master) = app(None).await;
    master.set("routing", 500, Value::Null);
    let (status, body) = cancel("c-1").send(&app).await;
    assert_eq!(
        (status, body["title"].as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("customers_unavailable")),
        "{body}"
    );
    master.set("authority", 503, Value::Null);
    let (status, _) = cancel("c-1").header(ACTING_PARTY, "party-son").send(&app).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(status_of(&app).await, "in_force");

    // Not a checked act, and not servicing: the master is not asked.
    let asked = master.asked.lock().unwrap().len();
    let decline = json!({ "policy": "pol-00000001", "command": { "decline_indexation": { "policy": "pol-00000001", "declined": true } } });
    let (status, _) = Call::new("POST", "/v1/commands")
        .roles("policy-servicing")
        .key("d-1")
        .body(decline)
        .send(&app)
        .await;
    assert_ne!(status, StatusCode::SERVICE_UNAVAILABLE);
    let by_underwriting = json!({ "policy": "pol-00000001", "command": { "cancel_policy": { "policy": "pol-00000001" } } });
    let unknown = json!({ "partition": 0, "command": { "cancel_policy": { "policy": "pol-none" } } });
    let (status, _) = Call::new("POST", "/v1/commands")
        .roles("policy-servicing")
        .key("u-1")
        .body(unknown)
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "the ledger answers for a policy nobody holds");
    assert_eq!(master.asked.lock().unwrap().len(), asked);

    // A party the master does not hold has no marker.
    master.set("routing", 404, Value::Null);
    let (status, body) = Call::new("POST", "/v1/commands")
        .roles("underwriting")
        .key("c-9")
        .body(by_underwriting.clone())
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(master.asked.lock().unwrap().len(), asked, "underwriting's cancellation is not servicing");

    // No master listening at all.
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let (app, _) = app_at(closed).await;
    let (status, body) = cancel("c-1").send(&app).await;
    assert_eq!(
        (status, body["title"].as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("customers_unavailable")),
        "{body}"
    );
    let (status, _) = cancel("c-1").header(ACTING_PARTY, "party-son").send(&app).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

async fn app_at(base: String) -> (Router, Shared) {
    app(Some(base)).await
}
