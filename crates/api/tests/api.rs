//! The API end to end against the insurance in a box: in-memory logs and
//! the in-memory stand-ins for the Azure services.

use std::sync::Arc;

mod common;

use axum::http::StatusCode;
use axum::Router;
use common::{d, Call};
use ic_api::auth::Auth;
use ic_api::engine::{AzureConfig, Engine, EngineConfig};
use ic_api::routes::{router, AppState};
use ic_product::{Frequency, Sex};
use ic_readmodel::scope::AudienceScope;
use ic_sim::persona::term_life;
use serde_json::{json, Value};

fn app() -> Router {
    let cfg = EngineConfig {
        partitions: 4,
        data_dir: None,
        opening_date: d(2027, 3, 1),
        load_shipped_config: true,
        azure: AzureConfig::default(),
    };
    let engine = Engine::start(&cfg).unwrap();
    router(Arc::new(AppState {
        backend: ic_api::Backend::single(engine),
        auth: Auth::Dev,
        scope: AudienceScope::shipped().unwrap(),
    }))
}

fn application() -> Value {
    // A 36 year old non-smoker with $500,000 of life cover and a 50% loading.
    serde_json::to_value(term_life(
        1,
        d(2027, 3, 1),
        d(1991, 2, 10),
        Sex::Female,
        false,
        500_000_00,
        None,
        Frequency::Monthly,
        Some("0.5"),
    ))
    .unwrap()
}

async fn issued() -> Router {
    let app = app();
    let (status, body) = Call::new("POST", "/v1/policies")
        .roles("new-business")
        .key("app-1")
        .body(application())
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    app
}

/// Purpose: the health probe answers without authentication, and the readiness probe reports the
/// business date.
/// Why it matters: Kubernetes restarts a container on the first and sends it traffic on the second.
/// Readiness tied to the business date keeps callers away from a ledger that has not been opened.
#[tokio::test]
async fn health_and_readiness() {
    let app = app();
    assert_eq!(Call::new("GET", "/healthz").anonymous().send(&app).await.0, StatusCode::OK);
    let (status, body) = Call::new("GET", "/readyz").anonymous().send(&app).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["business_date"], "2027-03-01");
}

/// Purpose: a quote returns the first period's premium and leaves no policy behind.
/// Why it matters: quotes are asked for far more often than policies are issued. They must be
/// priced exactly as a policy would be and must never reach the log.
#[tokio::test]
async fn a_quote_prices_without_writing_anything() {
    let app = app();
    let (status, quote) = Call::new("POST", "/v1/quotes").body(application()).send(&app).await;
    assert_eq!(status, StatusCode::OK, "{quote}");
    assert!(quote["by_cover"]["life"]["minor"].as_i64().unwrap() > 0, "{quote}");
    // Nothing was issued.
    assert_eq!(
        Call::new("GET", "/v1/policies/KTL00000001").roles("policy-servicing").send(&app).await.0,
        StatusCode::NOT_FOUND
    );
}

/// Purpose: an anonymous caller is refused, a caller without the module's role is forbidden, a
/// write without an idempotency key is a bad request, and the leader's own commands cannot be sent
/// through the API whatever roles are claimed.
/// Why it matters: the API is the boundary other systems cross. Each of these is a way in that must
/// be closed: no identity, the wrong identity, an unrepeatable write, and a caller reaching for the
/// commands that move the business date.
#[tokio::test]
async fn callers_must_be_known_and_hold_the_module_role() {
    let app = app();
    let (status, _) = Call::new("POST", "/v1/policies").anonymous().key("k").body(application()).send(&app).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = Call::new("POST", "/v1/policies")
        .roles("cash-matching")
        .key("k")
        .body(application())
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = Call::new("POST", "/v1/policies").roles("new-business").body(application()).send(&app).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "an idempotency key is required");
    // The leader's commands never arrive over the API, whatever roles the caller claims.
    let advance = json!({ "partition": 0, "command": { "advance_business_date": { "to": "2027-03-02" } } });
    let (status, _) = Call::new("POST", "/v1/commands")
        .roles("leader,operations")
        .key("k")
        .body(advance)
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// Purpose: the same application sent twice with one idempotency key is issued once and reported as
/// a duplicate, and sent under a new key is refused because the policy exists.
/// Why it matters: callers retry on timeouts. A retry must not issue a second policy, and a genuine
/// second attempt must be told clearly that the policy is already there.
#[tokio::test]
async fn issuing_twice_with_one_key_issues_once() {
    let app = issued().await;
    let (status, body) = Call::new("POST", "/v1/policies")
        .roles("new-business")
        .key("app-1")
        .body(application())
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["duplicate"], true);
    // The same application under a new key is refused by the core.
    let (status, body) = Call::new("POST", "/v1/policies")
        .roles("new-business")
        .key("app-2")
        .body(application())
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["title"], "policy_exists");
}

/// Purpose: the same policy read by staff, the policyholder, an adviser, a lender and a stranger
/// gives each exactly the sections the audience rules allow, and the stranger cannot tell it
/// exists.
/// Why it matters: this is the privacy model as callers meet it. An adviser seeing a loading, or a
/// stranger learning that a policy number is real, is a breach.
#[tokio::test]
async fn each_audience_sees_its_own_slice_of_the_policy() {
    let app = issued().await;
    let path = "/v1/policies/KTL00000001";

    let (status, staff) = Call::new("GET", path).roles("policy-servicing").send(&app).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(staff["policy"]["status"], "in_force");
    assert_eq!(staff["covers"][0]["sum_insured_minor"], 500_000_00);
    assert_eq!(staff["underwriting_terms"][0]["loadings"][0], "0.5");

    // The policyholder is also the life insured: she sees her own loading.
    let (_, owner) = Call::new("GET", path).viewer("party-00000001", "policyholder,life_insured").send(&app).await;
    assert!(owner["underwriting_terms"].is_array(), "{owner}");
    assert!(owner["billing"].is_object(), "{owner}");

    // The adviser sees the policy and the cover but never the health-implying terms.
    let (status, adviser) = Call::new("GET", path).viewer("adviser-7", "adviser").send(&app).await;
    assert_eq!(status, StatusCode::OK);
    assert!(adviser["covers"].is_array());
    assert!(adviser.get("underwriting_terms").is_none(), "{adviser}");

    // A lender with an assignment sees that the policy exists and its standing, nothing more.
    let (_, lender) = Call::new("GET", path).viewer("bank-1", "assignee").send(&app).await;
    assert_eq!(lender["policy"]["standing"], "paid");
    assert!(lender.get("covers").is_none() && lender.get("billing").is_none(), "{lender}");

    // Someone with no relationship cannot tell the policy exists.
    assert_eq!(Call::new("GET", path).viewer("stranger", "").send(&app).await.0, StatusCode::NOT_FOUND);
}

/// Purpose: a payment quoting the policy number is matched and applied, the same payment sent again
/// is a duplicate, the balance goes to zero, and a payment nobody can place becomes an exception
/// rather than a guess.
/// Why it matters: payment files are sent twice and references are mistyped. Money must be applied
/// once to the right policy, or held for a person to decide.
#[tokio::test]
async fn a_payment_with_the_policy_number_is_applied_once() {
    let app = issued().await;
    let (_, before) = Call::new("GET", "/v1/policies/KTL00000001").roles("policy-servicing").send(&app).await;
    let due = before["billing"]["outstanding_minor"].as_i64().unwrap();
    assert!(due > 0, "{before}");

    let pay = json!({ "id": "rail-001", "amount": { "minor": due, "currency": "NZD" }, "reference": "ktl 0000 0001", "payer_account": null, "received_on": "2027-03-01" });
    let (status, first) = Call::new("POST", "/v1/payments").roles("cash-matching").body(pay.clone()).send(&app).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{first}");
    assert_eq!(first["status"], "applied", "{first}");
    assert_eq!(first["match"]["rule"], "reference");
    let (_, again) = Call::new("POST", "/v1/payments").roles("cash-matching").body(pay).send(&app).await;
    assert_eq!(again["duplicate"], true);

    let (_, after) = Call::new("GET", "/v1/policies/KTL00000001").roles("policy-servicing").send(&app).await;
    assert_eq!(after["billing"]["outstanding_minor"], 0);

    // A payment nobody can place is an exception item, not a guess.
    let stray =
        json!({ "id": "rail-002", "amount": { "minor": 1234, "currency": "NZD" }, "reference": "thanks", "payer_account": null, "received_on": "2027-03-01" });
    let (_, body) = Call::new("POST", "/v1/payments").roles("cash-matching").body(stray).send(&app).await;
    assert_eq!(body["status"], "exception");
}

/// Purpose: only operations staff can run the close, which advances the business date, reconciles
/// cleanly and publishes the signed root; afterwards a servicing command addressed by policy alone
/// is routed to its partition.
/// Why it matters: the close is the one operation that moves the whole ledger forward. It must be
/// restricted, report honestly, and leave the system serving requests on the new day.
#[tokio::test]
async fn the_close_advances_the_date_reconciles_and_publishes_the_root() {
    let app = issued().await;
    let (status, _) = Call::new("POST", "/v1/admin/end-of-day").roles("policy-servicing").send(&app).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, report) = Call::new("POST", "/v1/admin/end-of-day").roles("operations").send(&app).await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["closed"], "2027-03-01");
    assert_eq!(report["business_date"], "2027-03-02");
    assert_eq!(report["breaks"], json!([]));
    assert!(report["published_root"].is_object(), "{report}");
    let (_, recon) = Call::new("GET", "/v1/admin/reconciliation").roles("operations").send(&app).await;
    assert_eq!(recon["clean"], true);

    // A servicing command routed by the policy directory.
    let cancel = json!({ "policy": "pol-00000001", "command": { "cancel_policy": { "policy": "pol-00000001" } } });
    let (status, body) = Call::new("POST", "/v1/commands")
        .roles("policy-servicing")
        .key("cancel-1")
        .body(cancel)
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let (_, view) = Call::new("GET", "/v1/policies/KTL00000001").roles("policy-servicing").send(&app).await;
    assert_eq!(view["policy"]["status"], "cancelled");
}

/// Purpose: the less travelled answers of the write routes: a quote for an application the
/// product refuses is unprocessable with the rule's code; a command with no partition, group or
/// policy is a bad request; a command addressed by group reaches that group's partition; a
/// configuration change that conflicts with active configuration is refused with the
/// ledger's reason; and a payment matched to a policy that has ended is held as an
/// exception with the match offered as a suggestion.
/// Why it matters: callers act on these answers. A quote that failed must say why, a command
/// that cannot be routed must not be guessed onto a partition, a conflicting change to
/// configuration must come back as a refusal rather than as accepted, and money for an ended policy must stay with
/// cash matching for a person to decide rather than be reported as applied.
#[tokio::test]
async fn refusals_are_reported_as_what_they_are() {
    let app = issued().await;
    let mut unknown = application();
    assert!(unknown["product_code"].is_string(), "{unknown}");
    unknown["product_code"] = json!("no-such-product");
    let (status, body) = Call::new("POST", "/v1/quotes").body(unknown).send(&app).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    let cancel = json!({ "cancel_policy": { "policy": "pol-00000001" } });
    let (status, body) = Call::new("POST", "/v1/commands")
        .roles("policy-servicing")
        .key("c-0")
        .body(json!({ "command": cancel }))
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let rate = json!({ "command": { "activate_index_rate": { "code": "cpi", "effective_from": "2027-01-01", "rate": "0.9" } } });
    let (status, body) = Call::new("POST", "/v1/commands").roles("config").key("cpi-again").body(rate).send(&app).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["title"], "config_conflict", "{body}");

    let group = application()["group"].clone();
    let (status, body) = Call::new("POST", "/v1/commands")
        .roles("policy-servicing")
        .key("c-1")
        .body(json!({ "group": group, "command": cancel }))
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    let pay = json!({ "id": "rail-9", "amount": { "minor": 5000, "currency": "NZD" }, "reference": "KTL00000001", "payer_account": null, "received_on": "2027-03-01" });
    let (status, body) = Call::new("POST", "/v1/payments").roles("cash-matching").body(pay).send(&app).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["status"], "exception", "{body}");
    assert!(body["reason"].as_str().unwrap().starts_with("policy_ended"), "{body}");
    assert_eq!(body["suggestion"]["rule"], "reference", "{body}");
}
