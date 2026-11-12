//! The documents service: its configuration, the delivery-report webhook
//! Event Grid calls, and the cycle loop.

mod support;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ic_documents::channels::{Channels, MemoryDirectory, Outbound, SendError};
use ic_documents::command::{Command, NoticeSpec};
use ic_documents::crypto::{self, LocalWrap};
use ic_documents::driver::{Driver, IssueRequest, LocalModule, MemoryPublisher, Module};
use ic_documents::model::{Channel, DeliveryStatus, RecipientRole};
use ic_documents::service::{router, run, run_cycles, serve, stop_signal, Config, NoCore};
use ic_documents::vault::MemoryStore;
use serde_json::{json, Value};
use support::*;

const KEY: &str = "0123456789abcdef0123456789abcdef";

/// Every message is accepted and given a reference from its document.
#[derive(Clone, Default)]
struct Accepting;

impl Channels for Accepting {
    async fn send(&self, message: &Outbound) -> Result<String, SendError> {
        Ok(format!("ref-{}", message.document))
    }
}

type TestDriver = Driver<LocalModule, MemoryStore, LocalWrap, Accepting, MemoryDirectory>;

/// A driver over an in-memory module with the usual templates, and one
/// renewal notice issued to a party reachable by email.
async fn driver_with_notice() -> Arc<TestDriver> {
    let module = LocalModule::default();
    let ready = Kit::ready();
    for envelope in &ready.log {
        assert!(module.propose(envelope.clone()).await.unwrap().result.is_ok());
    }
    let directory = MemoryDirectory::default();
    directory.set("aroha", Channel::Email, "Aroha Ngata", "aroha@example.nz");
    let now = ready.now;
    let driver = Arc::new(Driver {
        module,
        objects: MemoryStore::default(),
        wrap: LocalWrap::new(crypto::new_key()),
        channels: Accepting,
        directory,
        actor: "documents-service".into(),
        clock: Arc::new(move || now),
        batch: 50,
    });
    // The parties' keys in the prepared kit are placeholders: use a party
    // whose key the driver makes itself.
    driver.directory.set("mere", Channel::Email, "Mere Parata", "mere@example.nz");
    let request = IssueRequest {
        document: "n1".into(),
        caller: "documents".into(),
        template: "renewal".into(),
        template_version: None,
        policy: Some(policy("pol-1")),
        other_policies: vec![],
        data_class: Default::default(),
        lives: vec![],
        prove_delivery: false,
        owner: party("mere"),
        recipients: vec![recipient("mere", RecipientRole::Addressee, &[Channel::Email])],
        language: "en".into(),
        format: None,
        data: sample_data(),
        trigger_event: Some("7-100".into()),
        case: None,
        supersedes: None,
        reason: None,
        wordings: vec![],
        components: vec![],
        notice: Some(NoticeSpec {
            purpose: "renewal".into(),
            corrects: None,
        }),
    };
    driver.issue(request).await.unwrap();
    driver
}

fn settings() -> BTreeMap<&'static str, String> {
    BTreeMap::from([
        ("IC_CLUSTER_NODES", "node-0:7000,node-1:7000".to_string()),
        ("IC_DOCUMENTS_VAULT_ENDPOINT", "https://stdocs.blob.core.windows.net".to_string()),
        ("IC_DOCUMENTS_KEY_VAULT", "https://kv-docs.vault.azure.net".to_string()),
        ("IC_COMMUNICATION_ENDPOINT", "https://acs-docs.communication.azure.com".to_string()),
        ("IC_DOCUMENTS_EMAIL_FROM", "notices@insurer.example".to_string()),
        ("IC_DOCUMENTS_SMS_FROM", "+6421000000".to_string()),
        ("IC_CUSTOMER_MASTER_URL", "https://customers.internal".to_string()),
        ("IC_CUSTOMER_MASTER_SCOPE", "api://customer-master/.default".to_string()),
        ("IC_EVENT_HUBS_NAMESPACE", "https://evh-core.servicebus.windows.net".to_string()),
        ("IC_DOCUMENTS_WEBHOOK_KEY", format!("  {KEY}  ")),
    ])
}

/// Purpose: the service reads its settings from the environment, applies
/// the documented defaults, refuses to start when a required one is
/// missing or unusable, and never prints the webhook key.
/// Why it matters: a service that started without its vault or key would
/// fail at the first customer document instead of at deployment, and a
/// webhook key in a log is a key anyone with log access can use to forge
/// proof of delivery.
#[test]
fn settings_are_required_defaulted_and_never_printed() {
    let env = settings();
    let config = Config::from_lookup(|k| env.get(k).cloned()).unwrap();
    assert_eq!(config.cluster, "node-0:7000,node-1:7000");
    assert_eq!(config.webhook_key, KEY);
    assert_eq!(
        (config.vault_container.as_str(), config.print_container.as_str()),
        ("documents-vault", "print-outbox")
    );
    assert_eq!((config.key_name.as_str(), config.event_hub.as_str()), ("documents-kek", "documents-events"));
    assert_eq!(config.complaints_event_hub, "complaints-events");
    assert_eq!(config.conduct_officer, "conduct-officer");
    assert_eq!((config.listen.as_str(), config.cycle_every), ("0.0.0.0:8080", Duration::from_secs(15)));
    let printed = format!("{config:?}");
    assert!(printed.contains("node-0:7000") && !printed.contains(KEY));

    let mut custom = env.clone();
    custom.insert("IC_DOCUMENTS_CYCLE_SECONDS", "60".into());
    custom.insert("IC_DOCUMENTS_LISTEN", "127.0.0.1:9000".into());
    custom.insert("IC_COMPLAINTS_EVENT_HUB", "complaints-test".into());
    let config = Config::from_lookup(|k| custom.get(k).cloned()).unwrap();
    assert_eq!(config.complaints_event_hub, "complaints-test");
    assert_eq!((config.listen.as_str(), config.cycle_every), ("127.0.0.1:9000", Duration::from_secs(60)));

    for key in env.keys() {
        let mut without = env.clone();
        without.insert(key, "   ".into());
        assert_eq!(Config::from_lookup(|k| without.get(k).cloned()).unwrap_err(), format!("{key} is not set"));
    }
    let mut short = env.clone();
    short.insert("IC_DOCUMENTS_WEBHOOK_KEY", "short".into());
    assert!(Config::from_lookup(|k| short.get(k).cloned()).unwrap_err().contains("at least 32"));
    for bad in ["0", "soon", "-5"] {
        let mut wrong = env.clone();
        wrong.insert("IC_DOCUMENTS_CYCLE_SECONDS", bad.into());
        assert!(Config::from_lookup(|k| wrong.get(k).cloned())
            .unwrap_err()
            .contains("IC_DOCUMENTS_CYCLE_SECONDS"));
    }
}

/// Purpose: the webhook records a provider's delivery report as proof of
/// delivery only when the caller presents the key; it answers Event Grid's
/// handshake, refuses what it cannot read, and asks for redelivery when
/// the report could not be recorded.
/// Why it matters: this route turns an outside message into legal proof
/// that a notice was delivered. Unauthenticated, anyone could start a
/// notice clock; and a report dropped during an outage would leave a
/// delivered notice to time out and be posted again.
#[tokio::test]
async fn the_webhook_records_authenticated_delivery_reports() {
    let driver = driver_with_notice().await;
    assert_eq!(driver.deliver().await.unwrap().sent, 1);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(serve(listener, router(driver.clone(), KEY), async {
        let _ = stopped.await;
    }));
    let http = reqwest::Client::new();
    let url = format!("{base}/v1/delivery-reports");
    let report = json!([{ "eventType": "Microsoft.Communication.EmailDeliveryReportReceived", "data": { "messageId": "ref-n1", "status": "Delivered" } }]);
    let post = |key: Option<&'static str>, body: Value| {
        let mut request = http.post(&url).json(&body);
        if let Some(key) = key {
            request = request.header("x-webhook-key", key);
        }
        async move {
            let response = request.send().await.unwrap();
            (response.status().as_u16(), response.json::<Value>().await.unwrap())
        }
    };

    assert_eq!(http.get(format!("{base}/healthz")).send().await.unwrap().text().await.unwrap(), "ok");
    assert_eq!(post(None, report.clone()).await.0, 401);
    assert_eq!(post(Some("0123456789abcdef0123456789abcdeX"), report.clone()).await.0, 401);
    let in_progress = |d: &TestDriver| d.module.state().documents["n1"].deliveries[0].status == DeliveryStatus::InProgress;
    assert!(in_progress(&driver), "an unauthenticated report proved nothing");

    let handshake = json!([{ "eventType": "Microsoft.EventGrid.SubscriptionValidationEvent", "data": { "validationCode": "code-1" } }]);
    assert_eq!(post(Some(KEY), handshake).await, (200, json!({ "validationResponse": "code-1" })));
    assert_eq!(post(Some(KEY), json!({ "not": "a delivery" })).await.0, 400);

    // The module cannot be reached: Event Grid is asked to deliver again.
    driver.module.set_down(true);
    assert_eq!(post(Some(KEY), report.clone()).await.0, 503);
    driver.module.set_down(false);
    assert!(in_progress(&driver));

    assert_eq!(post(Some(KEY), report.clone()).await, (200, json!({ "recorded": 1 })));
    assert!(matches!(
        driver.module.state().documents["n1"].deliveries[0].status,
        DeliveryStatus::Delivered { channel: Channel::Email, .. }
    ));
    // The same report again changes nothing.
    assert_eq!(post(Some(KEY), report).await, (200, json!({ "recorded": 0 })));

    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
}

/// Purpose: the loop runs the cycle until it is told to stop, keeps going
/// when a cycle fails, and without a link to the core leaves instructions
/// waiting in the module.
/// Why it matters: this loop is what makes notices go out at all. If one
/// failed cycle ended it, a brief outage would stop all correspondence
/// until someone noticed; and an instruction dropped for want of a core
/// link would be a proof of delivery the core never hears of.
#[tokio::test]
async fn the_loop_keeps_cycling_until_stopped() {
    let driver = driver_with_notice().await;
    let publisher = MemoryPublisher::default();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let record = seen.clone();
    let looping = tokio::spawn({
        let driver = driver.clone();
        let publisher = publisher.clone();
        async move {
            let stop = async {
                let _ = stopped.await;
            };
            run_cycles(driver, publisher, Some(NoCore), Duration::from_millis(5), stop, move |cycle| {
                record.lock().unwrap().push(cycle.clone())
            })
            .await;
        }
    });
    let wait_for = |n: usize| {
        let seen = seen.clone();
        async move {
            for _ in 0..400 {
                if seen.lock().unwrap().len() >= n {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            panic!("the loop did not reach {n} cycles");
        }
    };
    wait_for(1).await;
    assert_eq!(seen.lock().unwrap()[0].sends.sent, 1);

    // Delivery is proven; the core has no notice commands, so the
    // instruction stays in the module and the cycle reports it.
    let delivered = Command::RecordOutcome {
        document: "n1".into(),
        party: party("mere"),
        channel: Channel::Email,
        outcome: ic_documents::model::AttemptOutcome::Delivered,
    };
    let envelope = ic_documents::Envelope {
        key: "proof".into(),
        caller: "delivery".into(),
        actor: "svc".into(),
        at: Kit::new().now,
        command: delivered,
    };
    assert!(driver.module.propose(envelope).await.unwrap().result.is_ok());
    let before = seen.lock().unwrap().len();
    wait_for(before + 2).await;
    assert!(seen.lock().unwrap().last().unwrap().errors.iter().any(|e| e.starts_with("core: ")));
    assert_eq!(driver.module.state().core_outbox.len(), 1);

    // The module goes away and comes back: the loop is still running.
    driver.module.set_down(true);
    let before = seen.lock().unwrap().len();
    wait_for(before + 2).await;
    assert!(seen.lock().unwrap().last().unwrap().errors.len() >= 4);
    driver.module.set_down(false);

    stop.send(()).unwrap();
    looping.await.unwrap();
    assert!(!publisher.seen().is_empty());

    // A listener that is already gone is reported, not hung on.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    serve(listener, router(driver, KEY), async {}).await.unwrap();
}

/// A stand-in ledger node: no partitions, and every module accepts every
/// command and answers every question with an empty list.
async fn quiet_cluster() -> (String, Arc<Mutex<Vec<String>>>) {
    use axum::extract::{Path, State};
    use axum::routing::{get, post};
    let calls = Arc::new(Mutex::new(Vec::new()));
    async fn command(State(calls): State<Arc<Mutex<Vec<String>>>>, Path(module): Path<String>, axum::Json(body): axum::Json<Value>) -> axum::Json<Value> {
        calls.lock().unwrap().push(format!("{module}:{}", body["caller"].as_str().unwrap_or_default()));
        axum::Json(json!({ "log_index": 1, "result": { "Ok": { "index": 1, "result": { "Ok": null }, "events": [], "duplicate": false } } }))
    }
    async fn query(State(calls): State<Arc<Mutex<Vec<String>>>>, Path(module): Path<String>) -> axum::Json<Value> {
        calls.lock().unwrap().push(format!("{module}?"));
        axum::Json(json!({ "Ok": [] }))
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let app = axum::Router::new()
        .route("/v1/status", get(|| async { axum::Json(json!([])) }))
        .route("/v1/modules/{module}/commands", post(command))
        .route("/v1/modules/{module}/query", post(query))
        .with_state(calls.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (addr, calls)
}

fn run_config(cluster: &str) -> Config {
    let mut env = settings();
    env.insert("IC_CLUSTER_NODES", cluster.to_string());
    env.insert("IC_DOCUMENTS_LISTEN", "127.0.0.1:0".into());
    let mut config = Config::from_lookup(|k| env.get(k).cloned()).unwrap();
    config.cycle_every = Duration::from_millis(10);
    config.cluster_wait = Duration::from_millis(300);
    config
}

/// Purpose: the service as deployed starts against the cluster, runs the
/// documents cycle and the complaints relay side by side, and stops all
/// of it when asked; with no ledger node answering, or its address taken,
/// it fails at start with the reason.
/// Why it matters: this is the function the container runs. If the relay
/// were not started here no complaint timer would ever move in
/// production, and a service that started without a cluster would sit
/// looking healthy while sending nothing.
#[tokio::test]
async fn the_service_runs_both_loops_and_stops_when_asked() {
    let (cluster, calls) = quiet_cluster().await;
    let credential = ic_azure::credential::AzureCredential::static_token("tok");
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let running = tokio::spawn(run(run_config(&cluster), credential.clone(), ic_raft::ClusterAuth::open(), async move {
        let _ = stopped.await;
    }));
    let seen = |what: &'static str| {
        let calls = calls.clone();
        async move {
            for _ in 0..400 {
                if calls.lock().unwrap().iter().any(|c| c == what) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            panic!("the service never sent {what}");
        }
    };
    seen("documents:delivery").await;
    seen("complaints:scheduler").await;
    seen("distribution?").await;
    stop.send(()).unwrap();
    assert_eq!(tokio::time::timeout(Duration::from_secs(10), running).await.unwrap().unwrap(), Ok(()));

    let nowhere = run(run_config("127.0.0.1:1"), credential.clone(), ic_raft::ClusterAuth::open(), async {}).await;
    assert_eq!(nowhere, Err("no ledger node answered".to_string()));

    let taken = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = run_config(&cluster);
    config.listen = taken.local_addr().unwrap().to_string();
    let error = run(config, credential, ic_raft::ClusterAuth::open(), async {}).await.unwrap_err();
    assert!(error.starts_with("cannot listen on 127.0.0.1:"), "{error}");
}

/// Purpose: the stop signal completes when the process is sent SIGTERM.
/// Why it matters: Kubernetes stops a pod with SIGTERM. A service that
/// ignored it would be killed mid-cycle after the grace period on every
/// deployment, instead of finishing the step it was on.
#[tokio::test]
async fn sigterm_stops_the_service() {
    let waiting = tokio::spawn(stop_signal());
    // Let the handler be installed before the signal is sent.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!waiting.is_finished());
    let sent = std::process::Command::new("kill")
        .args(["-TERM", &std::process::id().to_string()])
        .status()
        .unwrap();
    assert!(sent.success());
    tokio::time::timeout(Duration::from_secs(10), waiting).await.unwrap().unwrap();
}
