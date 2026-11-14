//! Claims work arriving over the API: the generic command route carries the
//! claims module's commands to the partition that holds the policy.

use std::sync::Arc;

mod common;

use axum::http::StatusCode;
use axum::Router;
use common::{d, Call};
use ic_api::auth::Auth;
use ic_api::engine::{AzureConfig, Engine, EngineConfig};
use ic_api::routes::{router, AppState};
use ic_core::claims_module::{AuthorityBook, Rules};
use ic_product::{Frequency, Sex};
use ic_readmodel::scope::AudienceScope;
use ic_sim::persona::term_life;
use serde_json::json;

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
        customers: None,
    }))
}

/// Purpose: a claim notification sent to `/v1/commands` needs the claims role, is routed by its
/// policy to the right partition, is refused with the module's own reason until claims rules
/// and authority profiles have been activated by the configuration role on every partition,
/// and is then accepted.
/// Why it matters: this is how every channel (app, portals, providers, phone) reaches claims.
/// A notification accepted on the wrong partition, or by a caller without the claims role,
/// or decided under rules nobody activated, would be a claim outside every control.
#[tokio::test]
async fn a_claim_notification_arrives_over_the_command_route() {
    notified(&app()).await;
}

/// Issue a policy, activate the claims configuration and notify claim `clm-1` on it, checking
/// each step of the way.
async fn notified(app: &Router) {
    let application = serde_json::to_value(term_life(
        1,
        d(2027, 3, 1),
        d(1991, 2, 10),
        Sex::Female,
        false,
        500_000_00,
        Some(100_000_00),
        Frequency::Monthly,
        None,
    ))
    .unwrap();
    let (status, body) = Call::new("POST", "/v1/policies")
        .roles("new-business")
        .key("app-1")
        .body(application)
        .send(app)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    let notify = json!({ "policy": "pol-00000001", "command": { "claims": { "notify": {
        "claim": "clm-1", "policy": "pol-00000001", "cover": "trauma", "life": "life-1", "claim_type": "trauma",
        "event_date": "2027-03-01", "channel": "app", "notifier": "policyholder"
    } } } });
    let (status, body) = Call::new("POST", "/v1/commands")
        .roles("payments")
        .key("n-0")
        .body(notify.clone())
        .send(app)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = Call::new("POST", "/v1/commands")
        .roles("claims")
        .key("n-1")
        .body(notify.clone())
        .send(app)
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["title"], "not_configured", "{body}");

    let approvals = (String::from("config-ann"), vec![String::from("config-bob"), String::from("config-cat")]);
    let rules = Rules {
        version: 1,
        large_claim_minor: i64::MAX,
        two_signature_admission_minor: 1,
        two_signature_ex_gratia_minor: 1,
        two_signature_write_off_minor: 1,
        prepared_by: approvals.0.clone(),
        approved_by: approvals.1.clone(),
        ..Rules::default()
    };
    let book = AuthorityBook {
        version: 1,
        prepared_by: approvals.0,
        approved_by: approvals.1,
        ..AuthorityBook::default()
    };
    for (key, command) in [
        ("rules-1", json!({ "claims": { "activate_rules": { "rules": rules } } })),
        ("authority-1", json!({ "claims": { "activate_authority": { "book": book } } })),
    ] {
        let (status, body) = Call::new("POST", "/v1/commands")
            .roles("claims")
            .key(key)
            .body(json!({ "command": command }))
            .send(app)
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        let (status, body) = Call::new("POST", "/v1/commands")
            .roles("config")
            .key(key)
            .body(json!({ "command": command }))
            .send(app)
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    }

    let (status, body) = Call::new("POST", "/v1/commands")
        .roles("claims")
        .key("n-2")
        .body(notify.clone())
        .send(app)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    // The channel retrying under the same key gets the same answer, and a second key is told
    // the claim exists.
    let (status, _) = Call::new("POST", "/v1/commands")
        .roles("claims")
        .key("n-2")
        .body(notify.clone())
        .send(app)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (status, body) = Call::new("POST", "/v1/commands").roles("claims").key("n-3").body(notify).send(app).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["title"], "claim_exists", "{body}");
    let (_, recon) = Call::new("GET", "/v1/admin/reconciliation").roles("operations").send(app).await;
    assert_eq!(recon["clean"], true, "{recon}");
}

/// Purpose: a claim is read over the API by claims staff and by the person it is about, and
/// by nobody else: an adviser, the policy's owner when the claim is another person's, and a
/// stranger all get the same answer as for a claim that does not exist. Queues, the book, the
/// report and a run's remittance advice answer only to the claims role.
/// Why it matters: that a claim exists says something about a person's health. The read
/// routes are what the portals and the staff screens sit on, so a route that answered an
/// adviser or an employer would be a privacy breach at the first request.
#[tokio::test]
async fn a_claim_is_read_only_by_claims_staff_and_the_person_it_is_about() {
    let app = app();
    notified(&app).await;

    let (status, body) = Call::new("GET", "/v1/claims/clm-1").roles("claims").send(&app).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!((body["claim"].as_str(), body["status"].as_str()), (Some("clm-1"), Some("open")), "{body}");
    assert!(body.get("cause_code").is_none() && body.get("policy").is_none(), "{body}");

    // The life insured sees their own claim. Owning the policy is not enough when the claim is
    // about someone else, and an adviser never sees it.
    let (status, own) = Call::new("GET", "/v1/claims/clm-1").viewer("party-00000001", "policyholder").send(&app).await;
    assert_eq!(status, StatusCode::OK, "{own}");
    assert_eq!(own, body);
    for (subject, relationships) in [
        ("party-other", "policyholder"),
        ("adviser-1", "adviser"),
        ("party-other", ""),
        ("emp-1", "employer_admin"),
    ] {
        let (status, _) = Call::new("GET", "/v1/claims/clm-1").viewer(subject, relationships).send(&app).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{subject} as {relationships}");
    }
    let (status, _) = Call::new("GET", "/v1/claims/clm-9").roles("claims").send(&app).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The working view is for claims staff alone, and carries no clinical field.
    let (status, work) = Call::new("GET", "/v1/claims/clm-1/work").roles("claims").send(&app).await;
    assert_eq!(status, StatusCode::OK, "{work}");
    assert_eq!((work["claim"].as_str(), work["queue"].as_str()), (Some("clm-1"), Some("lump_sum")), "{work}");
    assert!(
        work["timers"].is_object() && work["tasks"].is_array() && work["reserve_minor"].is_i64(),
        "{work}"
    );
    assert!(work.get("cause_code").is_none() && work.get("decisions").is_none(), "{work}");
    let (status, _) = Call::new("GET", "/v1/claims/clm-1/work")
        .viewer("party-00000001", "policyholder")
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "the person it is about has the claimant's view, not the staff's");
    let (status, _) = Call::new("GET", "/v1/claims/clm-9/work").roles("claims").send(&app).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Staff views.
    let (status, queue) = Call::new("GET", "/v1/claims/queues/lump_sum").roles("claims").send(&app).await;
    assert_eq!(status, StatusCode::OK, "{queue}");
    assert_eq!(queue["rows"][0]["claim"], "clm-1", "{queue}");
    let (status, _) = Call::new("GET", "/v1/claims/queues/nonsense").roles("claims").send(&app).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, report) = Call::new("GET", "/v1/claims/report").roles("claims").send(&app).await;
    assert_eq!(status, StatusCode::OK, "{report}");
    let notified: u64 = report["partitions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["notified"]["trauma"].as_u64().unwrap_or(0))
        .sum();
    assert_eq!(notified, 1, "{report}");
    let (status, book) = Call::new("GET", "/v1/claims/book").roles("claims").send(&app).await;
    assert_eq!(status, StatusCode::OK, "{book}");
    assert!(book["partitions"].as_array().unwrap().iter().all(|p| p["in_payment"] == 0), "{book}");
    let (status, advice) = Call::new("GET", "/v1/claims/runs/run-1/remittance").roles("claims").send(&app).await;
    assert_eq!(status, StatusCode::OK, "{advice}");
    assert!(
        advice["partitions"].as_array().unwrap().iter().all(|p| p.as_object().unwrap().is_empty()),
        "{advice}"
    );
    for path in [
        "/v1/claims/queues/lump_sum",
        "/v1/claims/report",
        "/v1/claims/book",
        "/v1/claims/runs/run-1/remittance",
    ] {
        let (status, _) = Call::new("GET", path).roles("policy-servicing").send(&app).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
    }
}
