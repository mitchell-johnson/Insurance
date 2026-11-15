//! The end-of-day workflow on a real Temporal server, with real `ic-worker`
//! processes killed underneath it.
//!
//! The server is the one at `IC_TEST_TEMPORAL_ADDRESS` if that is set (the
//! Docker Compose cluster and CI set it). Otherwise the Temporal SDK's test
//! support starts the dev server executable named by `IC_TEST_TEMPORAL_EXE`.
//! With neither set the tests fail and say so: the SDK's own download of a
//! dev server does not work in this build (its HTTP client has no TLS), and
//! a test run should not depend on fetching an executable anyway. Set
//! `IC_SKIP_TEMPORAL_TESTS=1` to leave the server tests out on a machine
//! that has neither; they are never skipped silently.

mod common;

use std::path::Path;
use std::process::{Child, Command as Process, Output, Stdio};
use std::time::Duration;

use common::{day, free_port, scratch, simulated, Ledger};
use ic_azure::merkle::{root_name, verify_published, PublishedRoot};
use ic_workflows::EndOfDayReport;
use temporalio_sdk::testing::{EphemeralExe, LocalServer, LocalWorkflowEnvironmentOptions, WorkflowEnvironment};

const WORKER: &str = env!("CARGO_BIN_EXE_ic-worker");

/// A Temporal server for the test, and the local one's handle if we own it.
struct Temporal {
    address: String,
    local: Option<WorkflowEnvironment<LocalServer>>,
}

async fn temporal() -> Temporal {
    if let Ok(address) = std::env::var("IC_TEST_TEMPORAL_ADDRESS") {
        return Temporal { address, local: None };
    }
    let port = free_port();
    let options = LocalWorkflowEnvironmentOptions::builder().port(port).startup_timeout(Duration::from_secs(60));
    let exe = std::env::var("IC_TEST_TEMPORAL_EXE").expect(
        "no Temporal server: set IC_TEST_TEMPORAL_ADDRESS to a running server, IC_TEST_TEMPORAL_EXE to a dev server executable, or IC_SKIP_TEMPORAL_TESTS=1",
    );
    let options = options.server_executable(EphemeralExe::ExistingPath(exe)).build();
    let local = WorkflowEnvironment::start_local(options)
        .await
        .expect("could not start a Temporal dev server; see this file's header");
    Temporal {
        address: format!("127.0.0.1:{port}"),
        local: Some(local),
    }
}

/// How the worker processes of one test are configured.
struct Fleet {
    temporal: String,
    task_queue: String,
    nodes: String,
    roots: std::path::PathBuf,
}

impl Fleet {
    fn command(&self, nodes: &str) -> Process {
        let mut command = Process::new(WORKER);
        command
            .env_clear()
            .envs(std::env::vars().filter(|(k, _)| k == "LLVM_PROFILE_FILE"))
            .env("TEMPORAL_ADDRESS", &self.temporal)
            .env("TEMPORAL_NAMESPACE", "default")
            .env("IC_NODES", nodes)
            .env("IC_CLUSTER_TOKEN", common::TOKEN)
            .env("IC_ROOT_DIR", &self.roots)
            .env("IC_TASK_QUEUE", &self.task_queue)
            // Short, so a stopped worker's step is handed on within seconds.
            .env("IC_STEP_PATIENCE_SECS", "2")
            .env("IC_STEP_TIMEOUT_SECS", "4")
            .env("RUST_LOG", "warn")
            .stdin(Stdio::null());
        command
    }

    /// The same, with its traces sent to `collector` and `role` on its resource.
    fn traced(&self, nodes: &str, collector: &str, role: &str) -> Process {
        let mut command = self.command(nodes);
        command
            .env("OTEL_EXPORTER_OTLP_ENDPOINT", format!("http://{collector}"))
            .env("OTEL_LOGS_EXPORTER", "none")
            .env("OTEL_RESOURCE_ATTRIBUTES", format!("test.role={role}"));
        command
    }

    fn worker(&self) -> Child {
        self.command(&self.nodes).stdout(Stdio::null()).stderr(worker_log()).spawn().unwrap()
    }

    /// A worker that can reach Temporal but not the ledger: it takes steps
    /// and never finishes them.
    fn stranded_worker(&self) -> Child {
        self.command("127.0.0.1:1").stdout(Stdio::null()).stderr(worker_log()).spawn().unwrap()
    }

    /// Start `ic-worker close` for a day; it waits for the run to finish.
    fn close(&self, date: chrono::NaiveDate, partitions: u16) -> Child {
        self.command(&self.nodes)
            .args(["close", &date.to_string(), &partitions.to_string()])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }
}

/// Where the worker processes of this test run write what they log, so a
/// failure can show it.
fn worker_log_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("temporal-workers-{}.log", std::process::id()))
}

fn worker_log() -> Stdio {
    Stdio::from(std::fs::OpenOptions::new().create(true).append(true).open(worker_log_path()).unwrap())
}

/// Stop a worker process abruptly, with no chance to finish or report.
fn crash(mut worker: Child) {
    worker.kill().unwrap();
    worker.wait().unwrap();
}

async fn finish(child: Child) -> Output {
    let waiting = tokio::task::spawn_blocking(move || child.wait_with_output().unwrap());
    tokio::time::timeout(Duration::from_secs(120), waiting)
        .await
        .expect("the run did not finish")
        .unwrap()
}

async fn report(child: Child) -> EndOfDayReport {
    let output = finish(child).await;
    assert!(
        output.status.success(),
        "the run failed: {}\nworkers logged:\n{}",
        String::from_utf8_lossy(&output.stderr),
        std::fs::read_to_string(worker_log_path()).unwrap_or_default()
    );
    serde_json::from_slice(&output.stdout).expect("the report is not JSON")
}

fn published(roots: &Path, date: chrono::NaiveDate) -> PublishedRoot {
    serde_json::from_slice(&std::fs::read(roots.join(root_name(date))).expect("no root was published")).unwrap()
}

/// Purpose: run the real workflow on a real Temporal server while worker processes are
/// stopped abruptly: one that had taken a step and could not finish it, one between days,
/// and one in the middle of a day. Every day must still close, once, with the book equal
/// to the simulator's and a verifiable root on disk; a run for the wrong day must fail for
/// good rather than retry; and a worker asked to stop must exit cleanly.
/// Why it matters: this is the production arrangement end to end: Temporal's history, the
/// SDK's replay, the activity timeouts and retries, the worker binary and its settings.
/// The step-level tests prove the steps are safe to repeat; this proves Temporal really
/// does hand a dead worker's step to a live one, and that our retryable and non-retryable
/// markings are honoured.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_workflow_survives_workers_dying() {
    if std::env::var("IC_SKIP_TEMPORAL_TESTS").is_ok_and(|v| v == "1") {
        eprintln!("IC_SKIP_TEMPORAL_TESTS=1: the Temporal workflow test was NOT run");
        return;
    }
    let server = temporal().await;
    let ledger = Ledger::start("temporal", 3, 2).await;
    let fleet = Fleet {
        temporal: server.address.clone(),
        // A queue of its own, so a shared server's other workers stay out.
        task_queue: format!("ic-test-{}-{}", std::process::id(), free_port()),
        nodes: ledger.client.nodes().join(","),
        roots: scratch("temporal-roots"),
    };

    // Day 0. The only worker alive cannot reach the ledger: it takes the
    // first step and sits on it. Stop it there, then bring up two good ones.
    let stranded = fleet.stranded_worker();
    let closing = fleet.close(day(0), 2);
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(ledger.books().await, simulated(2, 0), "a worker with no ledger moved the ledger");
    crash(stranded);
    let first = fleet.worker();
    let second = fleet.worker();
    let day0 = report(closing).await;
    assert_eq!(day0.closed, day(0));
    assert_eq!(day0.breaks, vec![]);
    assert_eq!(ledger.books().await, simulated(2, 1));
    let root = published(&fleet.roots, day(0));
    assert!(verify_published(&root));
    assert_eq!(root.root, day0.published.root);

    // Day 1. One of the two workers dies while idle; the other carries on.
    crash(first);
    let day1 = report(fleet.close(day(1), 2)).await;
    assert_eq!(day1.breaks, vec![]);
    assert_eq!(ledger.books().await, simulated(2, 2));

    // Day 2. The last worker dies as the run starts; a new one finishes it.
    let closing = fleet.close(day(2), 2);
    tokio::time::sleep(Duration::from_millis(300)).await;
    crash(second);
    let mut third = fleet.worker();
    let day2 = report(closing).await;
    assert_eq!(day2.closed, day(2));
    assert_eq!(day2.breaks, vec![]);
    assert_eq!(ledger.books().await, simulated(2, 3));
    assert!(verify_published(&published(&fleet.roots, day(2))));

    // Asking for a closed day again gives the same answer and changes nothing.
    let again = report(fleet.close(day(2), 2)).await;
    assert_eq!(again, day2);
    assert_eq!(ledger.books().await, simulated(2, 3));

    // A run for a day the ledger is not on fails once and for all.
    let wrong = finish(fleet.close(day(400), 2)).await;
    assert!(!wrong.status.success());
    assert!(
        String::from_utf8_lossy(&wrong.stderr).contains("refused"),
        "{}",
        String::from_utf8_lossy(&wrong.stderr)
    );
    assert_eq!(ledger.books().await, simulated(2, 3));

    // The stop signal Kubernetes sends on a rollout: a clean exit.
    assert!(Process::new("kill").args(["-TERM", &third.id().to_string()]).status().unwrap().success());
    let exit = tokio::task::spawn_blocking(move || third.wait().unwrap()).await.unwrap();
    assert!(exit.success(), "the worker did not stop cleanly: {exit:?}");

    ledger.stop().await;
    if let Some(local) = server.local {
        local.shutdown().await.unwrap();
    }
    let _ = std::fs::remove_dir_all(&fleet.roots);
}

/// A stand-in collector for the worker processes' traces: each span with the `test.role` of
/// the process that sent it.
fn span_collector() -> (
    String,
    std::sync::Arc<std::sync::Mutex<Vec<(String, opentelemetry_proto::tonic::trace::v1::Span)>>>,
) {
    use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
    use prost::Message;
    let spans = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let into = spans.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    listener.set_nonblocking(true).unwrap();
    tokio::spawn(async move {
        let app = axum::Router::new().fallback(move |uri: axum::http::Uri, body: axum::body::Bytes| {
            let into = into.clone();
            async move {
                if uri.path().ends_with("/v1/traces") {
                    let request = ExportTraceServiceRequest::decode(body).unwrap();
                    let mut got = into.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    for resource in request.resource_spans {
                        let role = resource
                            .resource
                            .iter()
                            .flat_map(|r| &r.attributes)
                            .find(|a| a.key == "test.role")
                            .map(|a| format!("{:?}", a.value))
                            .unwrap_or_default();
                        for span in resource.scope_spans.into_iter().flat_map(|s| s.spans) {
                            got.push((role.clone(), span));
                        }
                    }
                }
                axum::http::StatusCode::OK
            }
        });
        let listener = tokio::net::TcpListener::from_std(listener).unwrap();
        axum::serve(listener, app).await.unwrap();
    });
    (address, spans)
}

/// Purpose: a close run by `ic-worker close` and carried out by a separate worker process is
/// one trace: the close command's own span, and the workflow and activity spans the worker
/// process makes as Temporal hands it the steps, all share the close's trace ID, with the
/// worker's spans exported by the worker process itself.
/// Why it matters: the close crosses processes only through Temporal, so this is the proof
/// that the trace survives Temporal; without it the steps of a failed close could not be
/// followed back to the run that started them, and the commands each step sends the ledger
/// would start traces of their own.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_close_is_one_trace_across_temporal() {
    if std::env::var("IC_SKIP_TEMPORAL_TESTS").is_ok_and(|v| v == "1") {
        eprintln!("IC_SKIP_TEMPORAL_TESTS=1: the Temporal trace test was NOT run");
        return;
    }
    let server = temporal().await;
    // A day of its own: Temporal names the close's workflow by its date, and the other tests on
    // this server close days 0 to 2 and the days before today.
    let ledger = Ledger::start_empty("traced", 3, 1).await;
    let date = day(100);
    ledger.run(0, "open", "leader", ic_core::Command::OpenBusinessDate { date }).await;
    let (collector, spans) = span_collector();
    let fleet = Fleet {
        temporal: server.address.clone(),
        task_queue: format!("ic-test-{}-{}", std::process::id(), free_port()),
        nodes: ledger.client.nodes().join(","),
        roots: scratch("traced-roots"),
    };
    let mut worker = fleet
        .traced(&fleet.nodes, &collector, "worker")
        .stdout(Stdio::null())
        .stderr(worker_log())
        .spawn()
        .unwrap();
    let closing = fleet
        .traced(&fleet.nodes, &collector, "close")
        .args(["close", &date.to_string(), "1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let closed = report(closing).await;
    assert_eq!(closed.closed, date);
    // A clean stop, so the worker flushes what it holds.
    assert!(Process::new("kill").args(["-TERM", &worker.id().to_string()]).status().unwrap().success());
    tokio::task::spawn_blocking(move || worker.wait().unwrap()).await.unwrap();

    let got = spans.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    let (_, root) = got
        .iter()
        .find(|(role, s)| role.contains("close") && s.name == "ic-worker")
        .unwrap_or_else(|| panic!("the close command's span: {:?}", got.iter().map(|(r, s)| (r, &s.name)).collect::<Vec<_>>()));
    let in_trace: Vec<_> = got.iter().filter(|(_, s)| s.trace_id == root.trace_id).collect();
    let names = |role: &str| {
        in_trace
            .iter()
            .filter(|(r, _)| r.contains(role))
            .map(|(_, s)| s.name.clone())
            .collect::<Vec<_>>()
    };
    let worker_names = names("worker");
    assert!(
        worker_names.iter().any(|n| n.contains("Workflow")) && worker_names.iter().any(|n| n.contains("Activity")),
        "the worker's workflow and activity spans are in the close's trace: worker {worker_names:?}, close {:?}, all {:?}",
        names("close"),
        got.iter().map(|(r, s)| (r, &s.name)).collect::<Vec<_>>()
    );

    drop(server.local);
    ledger.stop().await;
    let _ = std::fs::remove_dir_all(&fleet.roots);
}

fn run(args: &[&str], env: &[(&str, &str)]) -> (bool, String) {
    let output = Process::new(WORKER)
        .env_clear()
        .envs(std::env::vars().filter(|(k, _)| k == "LLVM_PROFILE_FILE"))
        .envs(env.iter().copied())
        .args(args)
        .output()
        .unwrap();
    (output.status.success(), String::from_utf8_lossy(&output.stderr).into_owned())
}

/// Purpose: the command the nightly schedule runs, against a real Temporal server and a
/// ledger that is two days behind the calendar: it closes both overdue days, oldest
/// first, stops at today, reports what it closed, and when run again closes nothing.
/// Why it matters: this is how the insurer's day actually ends in production. After an
/// outage the ledger is days behind; the job must catch up without a person, must not run
/// past today into a day still in progress, and must be harmless when the scheduler fires
/// it twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_scheduled_close_catches_up_to_today_and_stops() {
    if std::env::var("IC_SKIP_TEMPORAL_TESTS").is_ok_and(|v| v == "1") {
        eprintln!("IC_SKIP_TEMPORAL_TESTS=1: the scheduled close test was NOT run");
        return;
    }
    let server = temporal().await;
    let ledger = Ledger::start_empty("scheduled", 3, 2).await;
    let today = chrono::Utc::now().with_timezone(&chrono_tz::Pacific::Auckland).date_naive();
    let behind = today - chrono::Days::new(2);
    for partition in 0..2 {
        let open = ic_core::Command::OpenBusinessDate { date: behind };
        ledger.run(partition, &format!("open-{partition}"), "leader", open).await;
    }
    let fleet = Fleet {
        temporal: server.address.clone(),
        task_queue: format!("ic-test-{}-{}", std::process::id(), free_port()),
        nodes: ledger.client.nodes().join(","),
        roots: scratch("scheduled-roots"),
    };
    let mut worker = fleet.worker();
    let close_due = || {
        fleet
            .command(&fleet.nodes)
            .arg("close-due")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };

    let output = finish(close_due()).await;
    assert!(
        output.status.success(),
        "the scheduled close failed: {}\nworkers logged:\n{}",
        String::from_utf8_lossy(&output.stderr),
        std::fs::read_to_string(worker_log_path()).unwrap_or_default()
    );
    let said: serde_json::Value = serde_json::from_slice(&output.stdout).expect("the report is not JSON");
    let yesterday = today - chrono::Days::new(1);
    assert_eq!(said["closed"], serde_json::json!([behind.to_string(), yesterday.to_string()]));
    assert_eq!(said["business_date"], today.to_string());
    for date in [behind, yesterday] {
        assert!(verify_published(&published(&fleet.roots, date)), "{date}");
    }
    let on = ic_workflows::due::ledger_day(&ledger.client, Duration::from_secs(20)).await.unwrap();
    assert_eq!(on.business_date, today);

    // Fired again the same night: nothing to do, and today stays open.
    let again = finish(close_due()).await;
    assert!(again.status.success());
    let said: serde_json::Value = serde_json::from_slice(&again.stdout).unwrap();
    assert_eq!(said["closed"], serde_json::json!([]));
    assert!(!fleet.roots.join(root_name(today)).exists(), "today was closed while still in progress");

    assert!(Process::new("kill").args(["-TERM", &worker.id().to_string()]).status().unwrap().success());
    tokio::task::spawn_blocking(move || worker.wait().unwrap()).await.unwrap();
    drop(server.local);
    ledger.stop().await;
}

/// Purpose: the worker binary refuses to start with missing settings, an unknown command,
/// a malformed `close` request or an unreachable Temporal server, and says which.
/// Why it matters: these are the mistakes made in a deployment manifest or a runbook
/// command at two in the morning. Each must fail at once with the cause named, not hang
/// or start a run for a date nobody meant.
#[test]
fn the_worker_binary_names_what_is_wrong() {
    let nodes = [("IC_NODES", "127.0.0.1:1"), ("IC_CLUSTER_AUTH", "open")];
    let failing = |args: &[&str], env: &[(&str, &str)], names: &str| {
        let (ok, stderr) = run(args, env);
        assert!(!ok, "{args:?} should have failed");
        assert!(stderr.contains(names), "{stderr:?} does not mention {names:?}");
    };
    failing(&[], &[], "IC_CLUSTER_TOKEN_FILE");
    failing(&[], &[("IC_CLUSTER_AUTH", "open")], "IC_NODES is required");
    failing(&["reopen"], &nodes, "unknown command");
    failing(&["close"], &nodes, "usage: ic-worker close");
    failing(&["close", "2027-11-15"], &nodes, "usage: ic-worker close");
    failing(&["close", "yesterday", "2"], &nodes, "date:");
    failing(&["close", "2027-11-15", "many"], &nodes, "partitions:");
    let unreachable = [("IC_NODES", "127.0.0.1:1"), ("IC_CLUSTER_AUTH", "open"), ("TEMPORAL_ADDRESS", "127.0.0.1:1")];
    failing(&["close", "2027-11-15", "1"], &unreachable, "temporal:");
    failing(&[], &unreachable, "temporal:");
}
