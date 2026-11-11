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
    let app = app();
    let application = serde_json::to_value(term_life(1, d(2027, 3, 1), d(1991, 2, 10), Sex::Female, false, 500_000_00, Some(100_000_00), Frequency::Monthly, None)).unwrap();
    let (status, body) = Call::new("POST", "/v1/policies").roles("new-business").key("app-1").body(application).send(&app).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    let notify = json!({ "policy": "pol-00000001", "command": { "claims": { "notify": {
        "claim": "clm-1", "policy": "pol-00000001", "cover": "trauma", "life": "life-1", "claim_type": "trauma",
        "event_date": "2027-03-01", "channel": "app", "notifier": "policyholder"
    } } } });
    let (status, body) = Call::new("POST", "/v1/commands").roles("payments").key("n-0").body(notify.clone()).send(&app).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = Call::new("POST", "/v1/commands").roles("claims").key("n-1").body(notify.clone()).send(&app).await;
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
        let (status, body) = Call::new("POST", "/v1/commands").roles("claims").key(key).body(json!({ "command": command })).send(&app).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        let (status, body) = Call::new("POST", "/v1/commands").roles("config").key(key).body(json!({ "command": command })).send(&app).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    }

    let (status, body) = Call::new("POST", "/v1/commands").roles("claims").key("n-2").body(notify.clone()).send(&app).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    // The channel retrying under the same key gets the same answer, and a second key is told
    // the claim exists.
    let (status, _) = Call::new("POST", "/v1/commands").roles("claims").key("n-2").body(notify.clone()).send(&app).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (status, body) = Call::new("POST", "/v1/commands").roles("claims").key("n-3").body(notify).send(&app).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["title"], "claim_exists", "{body}");
    let (_, recon) = Call::new("GET", "/v1/admin/reconciliation").roles("operations").send(&app).await;
    assert_eq!(recon["clean"], true, "{recon}");
}
