//! The API as a gateway to replicated ledger nodes: the production
//! arrangement, with real nodes, real Raft and real HTTP between them, in
//! this process.

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use axum::Router;
use common::{d, Call, Ledger};
use ic_api::auth::Auth;
use ic_api::backend::{Backend, Gateway};
use ic_api::routes::{router, AppState};
use ic_cashmatch::Matcher;
use ic_core::{home_partition, Command, GroupId};
use ic_ledger::PartitionId;
use ic_product::{Frequency, Sex};
use ic_readmodel::scope::AudienceScope;
use ic_sim::config::shipped_activations;
use ic_sim::persona::term_life;
use serde_json::{json, Value};

const PARTITIONS: PartitionId = 2;

fn opening() -> chrono::NaiveDate {
    d(2027, 11, 15)
}

fn gateway(ledger: &Ledger, patience: Duration) -> Gateway {
    Gateway {
        client: ledger.client.clone(),
        partitions: ledger.partitions,
        patience,
        matcher: Matcher::default(),
    }
}

fn app(ledger: &Ledger, patience: Duration) -> Router {
    router(Arc::new(AppState {
        backend: Backend::cluster(gateway(ledger, patience)),
        auth: Auth::Dev,
        scope: AudienceScope::shipped().unwrap(),
    }))
}

/// The application numbered `n`: identical terms, its own group and party.
fn application(n: u64) -> Value {
    serde_json::to_value(term_life(
        n,
        opening(),
        d(1991, 2, 10),
        Sex::Female,
        false,
        500_000_00,
        None,
        Frequency::Monthly,
        None,
    ))
    .unwrap()
}

fn partition_of(n: u64) -> PartitionId {
    home_partition(&GroupId(format!("grp-{n:08}"))) % PARTITIONS
}

/// The lowest application number at or above `from` that lives on `partition`.
fn on_partition(partition: PartitionId, from: u64) -> u64 {
    (from..).find(|n| partition_of(*n) == partition).unwrap()
}

async fn issue(app: &Router, n: u64) -> Value {
    let (status, body) = Call::new("POST", "/v1/policies")
        .roles("new-business")
        .key(&format!("app-{n}"))
        .body(application(n))
        .send(app)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    body
}

async fn view(app: &Router, n: u64) -> (StatusCode, Value) {
    Call::new("GET", &format!("/v1/policies/KTL{n:08}")).roles("policy-servicing").send(app).await
}

/// Open the book on every partition and activate the shipped configuration
/// through the API.
async fn open(ledger: &Ledger, app: &Router) {
    for p in 0..PARTITIONS {
        ledger
            .run(p, &format!("open-{p}"), "leader", Command::OpenBusinessDate { date: opening() })
            .await;
    }
    for (n, activation) in shipped_activations().unwrap().into_iter().enumerate() {
        let (status, body) = Call::new("POST", "/v1/commands")
            .roles("config")
            .key(&format!("activate-{n}"))
            .body(json!({ "command": activation }))
            .send(app)
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    }
}

/// Purpose: drive the whole public API through a gateway in front of a three-node,
/// two-partition ledger: readiness, configuration activated on every partition, quotes,
/// policies issued onto the partition their group hashes to, policy views, payments matched
/// across partitions and applied once, a command routed by policy alone, reconciliation,
/// a claims question put to every partition, and the close, which a gateway declines because it runs as a workflow.
/// Why it matters: this is what an insurer's other systems actually call in production.
/// The single-process tests prove the handlers; this proves the same handlers give the
/// same answers when the state lives in Raft groups on other machines, including the
/// questions that span partitions (which partition holds this policy number, which policy
/// does this payment belong to), where a gateway that asked only one partition would
/// answer wrongly.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_gateway_serves_the_api_from_replicated_nodes() {
    let ledger = Ledger::start("gateway", 3, PARTITIONS).await;
    let app = app(&ledger, Duration::from_secs(20));

    // Not ready until every partition has a business date.
    assert_eq!(Call::new("GET", "/readyz").anonymous().send(&app).await.0, StatusCode::SERVICE_UNAVAILABLE);
    let (status, _) = Call::new("POST", "/v1/quotes").body(application(1)).send(&app).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "a quote before the book is open");
    open(&ledger, &app).await;
    let (status, ready) = Call::new("GET", "/readyz").anonymous().send(&app).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ready["business_date"], "2027-11-15");

    // A quote prices on the nodes and writes nothing.
    let (status, quote) = Call::new("POST", "/v1/quotes").body(application(1)).send(&app).await;
    assert_eq!(status, StatusCode::OK, "{quote}");
    assert!(quote["by_cover"]["life"]["minor"].as_i64().unwrap() > 0, "{quote}");
    assert_eq!(view(&app, 1).await.0, StatusCode::NOT_FOUND);

    // Two identical policies, one on each partition.
    let (a, b) = (on_partition(0, 1), on_partition(1, 1));
    assert_eq!(issue(&app, a).await["partition"], 0);
    assert_eq!(issue(&app, b).await["partition"], 1);
    assert_eq!(issue(&app, a).await["duplicate"], true, "the same key issued twice");
    let (status, refused) = Call::new("POST", "/v1/policies")
        .roles("new-business")
        .key("another-key")
        .body(application(a))
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(refused["title"], "policy_exists");

    // Each is found by its number, whichever partition holds it.
    let (status, on_a) = view(&app, a).await;
    assert_eq!(status, StatusCode::OK, "{on_a}");
    assert_eq!(on_a["policy"]["status"], "in_force");
    assert_eq!(on_a["covers"][0]["sum_insured_minor"], 500_000_00);
    let (_, on_b) = view(&app, b).await;
    let due = on_b["billing"]["outstanding_minor"].as_i64().unwrap();
    assert!(due > 0, "{on_b}");
    assert_eq!(on_a["billing"]["outstanding_minor"], due);
    // The audience rules are the gateway's own and apply as in one process.
    let path = format!("/v1/policies/KTL{a:08}");
    assert_eq!(Call::new("GET", &path).viewer("stranger", "").send(&app).await.0, StatusCode::NOT_FOUND);
    let (_, lender) = Call::new("GET", &path).viewer("bank-1", "assignee").send(&app).await;
    assert!(lender["policy"].is_object() && lender.get("billing").is_none(), "{lender}");

    // A payment with no reference fits both policies, one on each partition:
    // nobody is guessed at.
    let unreferenced =
        |id: &str| json!({ "id": id, "amount": { "minor": due, "currency": "NZD" }, "reference": null, "payer_account": null, "received_on": "2027-11-15" });
    let (_, ambiguous) = Call::new("POST", "/v1/payments")
        .roles("cash-matching")
        .body(unreferenced("rail-1"))
        .send(&app)
        .await;
    assert_eq!(ambiguous["status"], "exception", "{ambiguous}");
    assert_eq!(ambiguous["suggestion"], Value::Null, "{ambiguous}");

    // A payment quoting the policy number on partition 1 is applied there, once.
    let referenced = json!({ "id": "rail-2", "amount": { "minor": due, "currency": "NZD" }, "reference": format!("ktl {b:08}"), "payer_account": null, "received_on": "2027-11-15" });
    let (status, paid) = Call::new("POST", "/v1/payments")
        .roles("cash-matching")
        .body(referenced.clone())
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{paid}");
    assert_eq!(paid["status"], "applied", "{paid}");
    assert_eq!(paid["match"]["partition"], 1);
    assert_eq!(paid["duplicate"], false);
    let (_, again) = Call::new("POST", "/v1/payments").roles("cash-matching").body(referenced).send(&app).await;
    assert_eq!(again["duplicate"], true, "{again}");
    assert_eq!(view(&app, b).await.1["billing"]["outstanding_minor"], 0);
    assert_eq!(view(&app, a).await.1["billing"]["outstanding_minor"], due);

    // Now only one policy owes that amount, and it is the suggestion.
    let (_, single) = Call::new("POST", "/v1/payments")
        .roles("cash-matching")
        .body(unreferenced("rail-3"))
        .send(&app)
        .await;
    assert_eq!(single["status"], "exception", "{single}");
    assert_eq!(single["suggestion"]["policy"], format!("pol-{a:08}"));
    assert_eq!(single["suggestion"]["partition"], 0);

    // A servicing command addressed by policy alone finds its partition.
    let policy_b = format!("pol-{b:08}");
    let cancel = json!({ "policy": policy_b, "command": { "cancel_policy": { "policy": policy_b } } });
    let (status, cancelled) = Call::new("POST", "/v1/commands")
        .roles("policy-servicing")
        .key("cancel-1")
        .body(cancel)
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{cancelled}");
    assert_eq!(cancelled["partition"], 1);
    assert_eq!(view(&app, b).await.1["policy"]["status"], "cancelled");
    let nowhere = json!({ "policy": "pol-99999999", "command": { "cancel_policy": { "policy": "pol-99999999" } } });
    let (status, _) = Call::new("POST", "/v1/commands")
        .roles("policy-servicing")
        .key("cancel-2")
        .body(nowhere)
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let beyond = json!({ "partition": PARTITIONS, "command": { "cancel_policy": { "policy": policy_b } } });
    let (status, _) = Call::new("POST", "/v1/commands")
        .roles("policy-servicing")
        .key("cancel-3")
        .body(beyond)
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // The books reconcile across both partitions.
    let (status, recon) = Call::new("GET", "/v1/admin/reconciliation").roles("operations").send(&app).await;
    assert_eq!(status, StatusCode::OK, "{recon}");
    assert_eq!(recon["clean"], true, "{recon}");
    assert_eq!(recon["business_date"], "2027-11-15");

    // Claims questions are asked of every partition. Nothing has been notified, so each
    // partition answers with an empty report and no partition knows the claim.
    let (status, report) = Call::new("GET", "/v1/claims/report").roles("claims").send(&app).await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["partitions"].as_array().map(Vec::len), Some(PARTITIONS as usize), "{report}");
    let (status, _) = Call::new("GET", "/v1/claims/clm-none").roles("claims").send(&app).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The close of a replicated ledger is the workflow's job, and the gateway says so.
    let (status, close) = Call::new("POST", "/v1/admin/end-of-day").roles("operations").send(&app).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(close["title"], "close_runs_as_workflow");
    assert_eq!(view(&app, a).await.1["policy"]["status"], "in_force");

    ledger.stop().await;
}

/// Purpose: kill the node leading a partition and keep using the API; then kill a second
/// node, leaving no majority, and check that a write is answered 503 with a Retry-After
/// rather than a guess; then bring a node back and repeat the same request with the same
/// idempotency key.
/// Why it matters: the gateway is where a caller meets the cluster's failures. Losing one
/// node of three must be invisible apart from a pause. Losing the majority must produce an
/// honest "not confirmed, try again", and the retry must not issue the policy twice. A
/// restarted node must still answer policy reads, which means its read models came back
/// from its snapshot and log.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_gateway_rides_out_a_lost_leader_and_is_honest_without_a_majority() {
    let mut ledger = Ledger::start("gateway-faults", 3, PARTITIONS).await;
    let app = app(&ledger, Duration::from_secs(20));
    open(&ledger, &app).await;
    let first = on_partition(0, 1);
    issue(&app, first).await;

    // One node down, the leader of partition 0: the API carries on.
    let leader = ledger.leader_of(0).expect("partition 0 has no leader");
    ledger.kill(leader).await;
    let second = on_partition(0, first + 1);
    assert_eq!(issue(&app, second).await["duplicate"], false);
    assert_eq!(view(&app, first).await.0, StatusCode::OK);
    assert_eq!(view(&app, second).await.0, StatusCode::OK);

    // A second node down: no majority. A gateway with little patience says so.
    let survivor = *ledger.running.keys().next().unwrap();
    let other = *ledger.running.keys().find(|id| **id != survivor).unwrap();
    ledger.kill(other).await;
    let impatient = common::Call::new("POST", "/v1/policies");
    let third = on_partition(0, second + 1);
    let hasty = self::app(&ledger, Duration::from_millis(1500));
    let (status, headers, problem) = impatient.roles("new-business").key("app-late").body(application(third)).send_full(&hasty).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{problem}");
    assert_eq!(problem["title"], "ledger_unavailable");
    assert_eq!(headers.get("retry-after").and_then(|v| v.to_str().ok()), Some("1"));
    let (status, _) = view(&hasty, first).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "a read was answered without a majority");
    let (status, _) = Call::new("GET", "/readyz").anonymous().send(&hasty).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let (status, _) = Call::new("GET", "/v1/admin/reconciliation").roles("operations").send(&hasty).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);

    // The first node returns from its disk. The same request, same key, now
    // succeeds, and the policy exists once.
    ledger.boot(leader).await;
    let (status, body) = Call::new("POST", "/v1/policies")
        .roles("new-business")
        .key("app-late")
        .body(application(third))
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let (status, body) = Call::new("POST", "/v1/policies")
        .roles("new-business")
        .key("app-late")
        .body(application(third))
        .send(&app)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["duplicate"], true);
    for n in [first, second, third] {
        let (status, body) = view(&app, n).await;
        assert_eq!(status, StatusCode::OK, "policy {n} after the restart: {body}");
        assert!(body["billing"]["outstanding_minor"].as_i64().unwrap() > 0, "{body}");
    }

    ledger.stop().await;
}

/// Purpose: a gateway checks at start-up that the nodes hold as many partitions as it was
/// configured for, and that some node answers at all.
/// Why it matters: the partition count decides which partition a group's policies live on.
/// A gateway rolled out with a different count from the nodes would route new business to
/// the wrong partitions and fail to find existing policies, silently. It must refuse to
/// start instead, and a gateway that cannot reach the ledger must not report itself up.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_gateway_refuses_to_start_against_a_ledger_of_another_shape() {
    let ledger = Ledger::start("gateway-shape", 1, PARTITIONS).await;
    let brief = Duration::from_millis(500);
    assert_eq!(gateway(&ledger, brief).verify(brief).await, Ok(()));

    let mut wrong = gateway(&ledger, brief);
    wrong.partitions = PARTITIONS + 1;
    let error = wrong.verify(brief).await.unwrap_err();
    assert!(error.contains("holds 2 partitions") && error.contains("IC_PARTITIONS"), "{error}");

    let nodes = ledger.nodes();
    let auth = ledger.auth.clone();
    ledger.stop().await;
    let gone = Gateway::new(&ic_api::settings::ClusterSettings {
        nodes,
        auth,
        partitions: PARTITIONS,
        patience: brief,
    });
    let error = gone.verify(brief).await.unwrap_err();
    assert!(error.contains("no ledger node answered"), "{error}");
}
