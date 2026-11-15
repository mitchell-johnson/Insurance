//! Against a real OpenTelemetry Collector, the image and configuration the cluster runs
//! (deploy/k8s/telemetry.yaml), started by `tools/ci/start-collector.sh`, which sends what it
//! receives on to this test. Set `IC_TEST_OTEL_COLLECTOR` and `IC_TEST_OTEL_RETURN` as that
//! script prints them, or `IC_SKIP_COLLECTOR_TESTS=1` where no collector can run.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::http::StatusCode;
use axum::Router;
use ic_telemetry::scrub::REDACTED;
use ic_telemetry::{Output, Settings};
use opentelemetry::trace::TraceContextExt;
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::{any_value, AnyValue, KeyValue};
use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};
use prost::Message;
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// The collector's receiver, and the address this test listens on for what it sends on; `None`
/// when the tests are skipped on purpose.
fn addresses() -> Option<(String, SocketAddr)> {
    if std::env::var("IC_SKIP_COLLECTOR_TESTS").is_ok() {
        return None;
    }
    let collector =
        std::env::var("IC_TEST_OTEL_COLLECTOR").expect("no collector: run tools/ci/start-collector.sh and set what it prints, or IC_SKIP_COLLECTOR_TESTS=1");
    let back = std::env::var("IC_TEST_OTEL_RETURN").expect("IC_TEST_OTEL_RETURN, as start-collector.sh prints it");
    Some((collector, back.parse().expect("IC_TEST_OTEL_RETURN is an address")))
}

/// What the collector sent on, decoded. One listener for the whole file: the collector sends to
/// one port, and each test finds its own by trace ID.
#[derive(Default)]
struct Forwarded {
    traces: Vec<ExportTraceServiceRequest>,
    logs: Vec<ExportLogsServiceRequest>,
}

fn forwarded(back: SocketAddr) -> Arc<Mutex<Forwarded>> {
    static SHARED: OnceLock<Arc<Mutex<Forwarded>>> = OnceLock::new();
    SHARED
        .get_or_init(|| {
            let shared = Arc::new(Mutex::new(Forwarded::default()));
            let into = shared.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
                runtime.block_on(async move {
                    let app = Router::new().fallback(move |uri: axum::http::Uri, body: Bytes| {
                        let into = into.clone();
                        async move {
                            let mut got = into.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                            if uri.path().ends_with("/v1/traces") {
                                got.traces.push(ExportTraceServiceRequest::decode(body).unwrap());
                            } else if uri.path().ends_with("/v1/logs") {
                                got.logs.push(ExportLogsServiceRequest::decode(body).unwrap());
                            }
                            StatusCode::OK
                        }
                    });
                    let listener = tokio::net::TcpListener::bind(back).await.unwrap();
                    tx.send(()).unwrap();
                    axum::serve(listener, app).await.unwrap();
                });
            });
            rx.recv().unwrap();
            shared
        })
        .clone()
}

fn spans_of(got: &Forwarded, trace: &[u8]) -> Vec<(Option<Resource>, Span)> {
    got.traces
        .iter()
        .flat_map(|r| &r.resource_spans)
        .flat_map(|r| r.scope_spans.iter().flat_map(|s| &s.spans).map(move |s| (r.resource.clone(), s.clone())))
        .filter(|(_, s)| s.trace_id == trace)
        .collect()
}

fn records_of(got: &Forwarded, trace: &[u8]) -> Vec<LogRecord> {
    got.logs
        .iter()
        .flat_map(|r| &r.resource_logs)
        .flat_map(|r| &r.scope_logs)
        .flat_map(|s| &s.log_records)
        .filter(|l| l.trace_id == trace)
        .cloned()
        .collect()
}

/// Waits up to a minute (the shipped batching holds data five seconds) for `ready` to hold.
fn wait_for(shared: &Mutex<Forwarded>, ready: impl Fn(&Forwarded) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if ready(&shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!("the collector did not send the data on within a minute");
}

fn value(attributes: &[KeyValue], key: &str) -> Option<String> {
    attributes.iter().find(|a| a.key == key).map(|a| format!("{:?}", a.value))
}

/// Purpose: a process's spans and log records, sent by the real exporter to the real collector
/// running the shipped configuration, come out of the collector with everything that matters:
/// the parent and child spans in one trace, the child's parent being the parent span, the
/// service's resource, the span's own attribute, and the log record carrying the child span's
/// trace and span IDs, its level and its body with the e-mail address already masked.
/// Why it matters: the stand-in collector in tests/telemetry.rs proves what the process sends;
/// this proves a real collector accepts it, keeps the trace together and passes it on intact
/// through the shipped receiver, filter and batching, which is the path every span takes to
/// Application Insights or any other backend.
#[test]
fn a_real_collector_passes_the_trace_and_its_logs_on_intact() {
    let Some((collector, back)) = addresses() else { return };
    let shared = forwarded(back);
    let settings = Settings::from_lookup("ic-collector-test", |key| {
        (key == "OTEL_EXPORTER_OTLP_ENDPOINT").then(|| format!("http://{collector}"))
    })
    .unwrap();
    let (subscriber, telemetry) = ic_telemetry::subscriber(&settings, Output::Stderr).unwrap();
    let (trace, parent, child) = tracing::subscriber::with_default(subscriber, || {
        let outer = tracing::info_span!("close the day");
        let _outer = outer.enter();
        let inner = tracing::info_span!("reconcile partition", partition = 7_u64);
        let _inner = inner.enter();
        tracing::warn!(policy = "P-100", "break found; customer told at jane@example.com");
        let ids = |s: &tracing::Span| {
            let cx = s.context();
            let sc = cx.span().span_context().clone();
            (sc.trace_id().to_bytes().to_vec(), sc.span_id().to_bytes().to_vec())
        };
        let (trace, parent) = ids(&outer);
        let (_, child) = ids(&inner);
        (trace, parent, child)
    });
    telemetry.flush();
    drop(telemetry);

    wait_for(&shared, |got| spans_of(got, &trace).len() >= 2 && !records_of(got, &trace).is_empty());
    let got = shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let spans = spans_of(&got, &trace);
    let (resource, inner) = spans.iter().find(|(_, s)| s.name == "reconcile partition").expect("the child span");
    let (_, outer) = spans.iter().find(|(_, s)| s.name == "close the day").expect("the parent span");
    assert_eq!(outer.span_id, parent);
    assert_eq!(inner.span_id, child);
    assert_eq!(inner.parent_span_id, parent, "the child keeps its parent through the collector");
    assert!(value(&inner.attributes, "partition").unwrap().contains('7'));
    let resource = resource.as_ref().expect("the resource");
    assert!(value(&resource.attributes, "service.name").unwrap().contains("ic-collector-test"));
    assert!(value(&resource.attributes, "service.namespace").unwrap().contains("insurance-core"));

    let record = &records_of(&got, &trace)[0];
    assert_eq!(record.span_id, child, "the log record names the span it was written in");
    assert_eq!(record.severity_text, "WARN");
    let body = format!("{:?}", record.body);
    assert!(body.contains(REDACTED) && !body.contains("jane@example.com"), "{body}");
    assert!(value(&record.attributes, "policy").unwrap().contains("P-100"));
}

fn text(key: &str, value: &str) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: Some(AnyValue {
            value: Some(any_value::Value::StringValue(value.to_string())),
        }),
        ..Default::default()
    }
}

/// Purpose: spans and log records sent straight to the collector by something that does not
/// scrub (a raw OTLP request, as a third-party library or a misconfigured process could send)
/// lose, in the shipped collector's filter, every attribute whose name says it holds personal
/// data or a credential (an e-mail, a phone number, a date of birth, a medical field, the
/// authorization header, the query string, the client's address), and keep the others.
/// Why it matters: the collector is the second line of defence: the processes scrub first, but
/// anything that reaches Azure Monitor stays there for the retention period, so the shipped
/// filter must be shown to work in the collector itself, not only written in its file.
#[test]
fn the_shipped_collector_drops_personal_data_from_whatever_reaches_it() {
    let Some((collector, back)) = addresses() else { return };
    let shared = forwarded(back);
    let trace: Vec<u8> = (1..=16).collect();
    let attributes = vec![
        text("policy", "P-200"),
        text("customer.email", "a@b.com"),
        text("phone_number", "021 555 0101"),
        text("date_of_birth", "1980-01-01"),
        text("medical_history", "asthma"),
        text("http.request.header.authorization", "Bearer abc"),
        text("url.query", "name=Jane"),
        text("client.address", "203.0.113.7"),
    ];
    let resource = Some(Resource {
        attributes: vec![text("service.name", "raw-sender")],
        ..Default::default()
    });
    let spans = ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: resource.clone(),
            scope_spans: vec![ScopeSpans {
                spans: vec![Span {
                    trace_id: trace.clone(),
                    span_id: vec![1; 8],
                    name: "raw".to_string(),
                    kind: 2,
                    start_time_unix_nano: 1_700_000_000_000_000_000,
                    end_time_unix_nano: 1_700_000_000_100_000_000,
                    attributes: attributes.clone(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }],
    };
    let logs = ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            resource,
            scope_logs: vec![ScopeLogs {
                log_records: vec![LogRecord {
                    time_unix_nano: 1_700_000_000_000_000_000,
                    severity_text: "INFO".to_string(),
                    trace_id: trace.clone(),
                    span_id: vec![1; 8],
                    attributes,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }],
    };
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let client = reqwest::Client::new();
        for (path, body) in [("v1/traces", spans.encode_to_vec()), ("v1/logs", logs.encode_to_vec())] {
            let response = client
                .post(format!("http://{collector}/{path}"))
                .header("content-type", "application/x-protobuf")
                .body(body)
                .send()
                .await
                .unwrap();
            assert!(response.status().is_success(), "the collector accepted {path}: {}", response.status());
        }
    });

    wait_for(&shared, |got| !spans_of(got, &trace).is_empty() && !records_of(got, &trace).is_empty());
    let got = shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (_, span) = &spans_of(&got, &trace)[0];
    let record = &records_of(&got, &trace)[0];
    for kept in [&span.attributes, &record.attributes] {
        assert!(value(kept, "policy").unwrap().contains("P-200"));
        let keys: Vec<&str> = kept.iter().map(|a| a.key.as_str()).collect();
        assert_eq!(keys, ["policy"], "only the attribute without personal data is left");
    }
}
