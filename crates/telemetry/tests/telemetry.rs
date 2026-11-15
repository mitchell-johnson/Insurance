//! Traces and logs leave a process over OTLP, carry their trace across calls, and say nothing
//! about a person on the way. Each test installs the subscriber for its own thread only.

use std::io::Write;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode};
use axum::routing::get;
use axum::Router;
use http_body_util::BodyExt;
use ic_telemetry::format::finish;
use ic_telemetry::propagate::{context_from, headers_of};
use ic_telemetry::scrub::REDACTED;
use ic_telemetry::{continue_from, traceparent, Output, Settings, Traced, Wire};
use opentelemetry::trace::TraceContextExt;
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_sdk::trace::RandomIdGenerator;
use prost::Message;
use tower::ServiceExt;
use tracing_opentelemetry::OpenTelemetrySpanExt;

const PARENT: &str = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";
const PARENT_TRACE: &str = "0af7651916cd43dd8448eb211c80319c";

fn settings(endpoint: Option<String>) -> Settings {
    Settings::from_lookup("ic-test", |key| match key {
        "OTEL_EXPORTER_OTLP_ENDPOINT" => endpoint.clone(),
        _ => None,
    })
    .unwrap()
}

/// A stand-in collector: every request it was sent (path, headers, body), and what came on
/// each signal, decoded.
#[derive(Default)]
struct Received {
    requests: Vec<(String, axum::http::HeaderMap, Vec<u8>)>,
    traces: Vec<ExportTraceServiceRequest>,
    logs: Vec<ExportLogsServiceRequest>,
}

fn collector() -> (SocketAddr, Arc<Mutex<Received>>) {
    let received = Arc::new(Mutex::new(Received::default()));
    let (tx, rx) = std::sync::mpsc::channel();
    let shared = received.clone();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async move {
            let app = Router::new().fallback(move |uri: axum::http::Uri, headers: axum::http::HeaderMap, body: Bytes| {
                let shared = shared.clone();
                async move {
                    let mut got = shared.lock().unwrap();
                    let protobuf = headers.get("content-type").is_some_and(|v| v == "application/x-protobuf");
                    let plain = headers.get("content-encoding").is_none();
                    if protobuf && plain && uri.path().ends_with("/v1/traces") {
                        got.traces.push(ExportTraceServiceRequest::decode(body.clone()).unwrap());
                    }
                    if protobuf && plain && uri.path().ends_with("/v1/logs") {
                        got.logs.push(ExportLogsServiceRequest::decode(body.clone()).unwrap());
                    }
                    got.requests.push((uri.path().to_string(), headers, body.to_vec()));
                    StatusCode::OK
                }
            });
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });
    (rx.recv().unwrap(), received)
}

/// Purpose: with a collector configured, a span and a log event written inside it reach the
/// collector over OTLP/HTTP: the span under its name with the service's resource (name,
/// namespace, version), the log record with its level, body and fields, and the record's trace
/// and span IDs equal to the span's. Personal data in the event (an e-mail in the message, a
/// phone field) arrives masked, and a span field holding a name arrives redacted.
/// Why it matters: this is the whole point of the crate, end to end through the real exporter
/// and wire format the collector reads: if the IDs did not match, Application Insights could
/// not show a request's logs beside its trace; if scrubbing were skipped on export, personal
/// data would sit in Azure Monitor for the retention period.
#[test]
fn spans_and_logs_reach_the_collector_correlated_and_scrubbed() {
    let (addr, received) = collector();
    let (subscriber, telemetry) = ic_telemetry::subscriber(&settings(Some(format!("http://{addr}/"))), Output::Stderr).unwrap();
    assert!(telemetry.exporting());
    let ids = tracing::subscriber::with_default(subscriber, || {
        // A span renamed, failed and linked with text that holds personal data: all scrubbed.
        let other = tracing::info_span!("other");
        let linked = other.context().span().span_context().clone();
        let odd = tracing::info_span!(
            "odd",
            otel.name = "mail to a@b.com",
            otel.status_code = "ERROR",
            otel.status_description = "refused x@y.com"
        );
        odd.add_link_with_attributes(linked, vec![opentelemetry::KeyValue::new("email", "a@b.com")]);
        drop(odd);
        drop(other);
        let span = tracing::info_span!("issue policy", party = "P-1", full_name = "Jane Doe");
        let _entered = span.enter();
        let error = std::io::Error::other("no route for a@b.com");
        tracing::warn!(
            phone = "021 555 0101",
            count = 3_u64,
            big = u64::MAX,
            ratio = 0.5,
            ok = true,
            delta = -2_i64,
            log.file = "dropped.rs",
            error = &error as &(dyn std::error::Error + 'static),
            "letter sent to jane@example.com"
        );
        let cx = span.context();
        let sc = cx.span().span_context().clone();
        (sc.trace_id().to_string(), sc.span_id().to_string())
    });
    telemetry.flush();
    drop(telemetry);

    let got = received.lock().unwrap();
    let spans: Vec<_> = got.traces.iter().flat_map(|r| &r.resource_spans).collect();
    let resource = spans[0].resource.as_ref().unwrap();
    let attr = |key: &str| resource.attributes.iter().find(|a| a.key == key).map(|a| format!("{:?}", a.value));
    assert!(attr("service.name").unwrap().contains("ic-test"));
    assert!(attr("service.namespace").unwrap().contains("insurance-core"));
    assert!(attr("service.version").is_some());
    let span = spans
        .iter()
        .flat_map(|r| &r.scope_spans)
        .flat_map(|s| &s.spans)
        .find(|s| s.name == "issue policy")
        .expect("the span was exported");
    assert_eq!(hex(&span.trace_id), ids.0);
    let span_attr = |key: &str| span.attributes.iter().find(|a| a.key == key).map(|a| format!("{:?}", a.value));
    assert!(span_attr("party").unwrap().contains("P-1"));
    assert!(span_attr("full_name").unwrap().contains(REDACTED));

    let record = got
        .logs
        .iter()
        .flat_map(|r| &r.resource_logs)
        .flat_map(|r| &r.scope_logs)
        .flat_map(|s| &s.log_records)
        .next()
        .expect("the log was exported");
    assert_eq!(hex(&record.trace_id), ids.0);
    assert_eq!(hex(&record.span_id), ids.1);
    assert_eq!(record.severity_text, "WARN");
    assert!(format!("{:?}", record.body).contains(&format!("letter sent to {REDACTED}")));
    let log_attr = |key: &str| record.attributes.iter().find(|a| a.key == key).map(|a| format!("{:?}", a.value));
    assert!(log_attr("phone").unwrap().contains(REDACTED));
    assert!(log_attr("count").unwrap().contains('3'));
    assert!(log_attr("big").unwrap().contains(&u64::MAX.to_string()));
    assert!(log_attr("ratio").is_some() && log_attr("ok").is_some() && log_attr("delta").is_some());
    assert!(log_attr("log.file").is_none(), "the log crate's own fields are not repeated");
    assert!(log_attr("error").unwrap().contains(&format!("no route for {REDACTED}")));
    let all_spans: Vec<_> = spans.iter().flat_map(|r| &r.scope_spans).flat_map(|s| &s.spans).collect();
    let odd = all_spans
        .iter()
        .find(|s| s.name == format!("mail to {REDACTED}"))
        .expect("the renamed span, scrubbed");
    assert!(format!("{:?}", odd.status).contains(&format!("refused {REDACTED}")));
    assert!(format!("{:?}", odd.links).contains(REDACTED) && !format!("{:?}", odd.links).contains("a@b.com"));
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Purpose: the OpenTelemetry severity of each `tracing` level is the matching one.
/// Why it matters: alert rules and Application Insights' views filter on severity; an error
/// logged as information would never page anyone.
#[test]
fn levels_map_to_their_severities() {
    use ic_telemetry::logs::severity;
    use opentelemetry::logs::Severity;
    use tracing::Level;
    assert_eq!(severity(&Level::TRACE), Severity::Trace);
    assert_eq!(severity(&Level::DEBUG), Severity::Debug);
    assert_eq!(severity(&Level::INFO), Severity::Info);
    assert_eq!(severity(&Level::WARN), Severity::Warn);
    assert_eq!(severity(&Level::ERROR), Severity::Error);
}

fn lookup<'a>(vars: &'a [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> + 'a {
    move |key: &str| vars.iter().find(|(k, _)| *k == key).map(|(_, v)| v.to_string())
}

/// Purpose: the settings default to no export, sampling everything and a ten-second timeout;
/// a base endpoint gives both signals their standard paths over protobuf; each signal's own
/// endpoint, protocol, headers, compression and timeout override the general ones; `none` and
/// `OTEL_SDK_DISABLED` turn export off; the service name, version and resource attributes are
/// read; header values are percent-decoded and never shown by `Debug`.
/// Why it matters: these are the standard OpenTelemetry variables, so the same image can send to
/// the collector in the cluster, or to Grafana Cloud, Honeycomb or Datadog directly, by changing
/// settings alone; an API key in a header must not end up in a log line that prints the settings.
#[test]
fn settings_are_read_from_the_standard_variables() {
    let none = Settings::from_lookup("ic-api", |_| None).unwrap();
    assert_eq!(none.service, "ic-api");
    assert_eq!((none.traces.clone(), none.logs.clone()), (None, None));
    assert!(!none.exporting());
    assert_eq!(none.sample_ratio, 1.0);
    assert_eq!(none.traces_timeout, Duration::from_secs(10));
    assert!(none.attributes.is_empty());

    let base = Settings::from_lookup("ic-api", lookup(&[("OTEL_EXPORTER_OTLP_ENDPOINT", " http://ic-otel-collector:4318/ ")])).unwrap();
    let traces = base.traces.clone().unwrap();
    assert_eq!(traces.endpoint, "http://ic-otel-collector:4318/v1/traces");
    assert_eq!((traces.wire, traces.gzip, traces.headers.0.len()), (Wire::Protobuf, false, 0));
    assert_eq!(base.logs.unwrap().endpoint, "http://ic-otel-collector:4318/v1/logs");

    let vars = [
        ("OTEL_SERVICE_NAME", "renamed"),
        ("IC_VERSION", "abc123"),
        ("OTEL_EXPORTER_OTLP_ENDPOINT", "https://otlp.example.com"),
        ("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT", "https://logs.example.com/ingest"),
        ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/json"),
        ("OTEL_EXPORTER_OTLP_TRACES_PROTOCOL", "http/protobuf"),
        ("OTEL_EXPORTER_OTLP_HEADERS", "x-api-key=secret%20one,x-team=core"),
        ("OTEL_EXPORTER_OTLP_LOGS_HEADERS", "x-api-key=logs-key,x-dataset=logs"),
        ("OTEL_EXPORTER_OTLP_COMPRESSION", "gzip"),
        ("OTEL_EXPORTER_OTLP_LOGS_COMPRESSION", "none"),
        ("OTEL_EXPORTER_OTLP_TIMEOUT", "2500"),
        ("OTEL_EXPORTER_OTLP_LOGS_TIMEOUT", "700"),
        ("OTEL_TRACES_SAMPLER_ARG", "0.25"),
        (
            "OTEL_RESOURCE_ATTRIBUTES",
            "k8s.pod.name=ic-api-1, cloud.availability_zone=2,service.name=ignored",
        ),
    ];
    let set = Settings::from_lookup("ic-api", lookup(&vars)).unwrap();
    assert_eq!((set.service.as_str(), set.version.as_str()), ("renamed", "abc123"));
    let traces = set.traces.clone().unwrap();
    assert_eq!(traces.endpoint, "https://otlp.example.com/v1/traces");
    assert_eq!((traces.wire, traces.gzip), (Wire::Protobuf, true));
    assert_eq!(
        traces.headers.0,
        vec![("x-api-key".to_string(), "secret one".to_string()), ("x-team".to_string(), "core".to_string())]
    );
    let logs = set.logs.clone().unwrap();
    assert_eq!(logs.endpoint, "https://logs.example.com/ingest");
    assert_eq!((logs.wire, logs.gzip), (Wire::Json, false));
    assert_eq!(
        logs.headers.0,
        vec![
            ("x-team".to_string(), "core".to_string()),
            ("x-api-key".to_string(), "logs-key".to_string()),
            ("x-dataset".to_string(), "logs".to_string())
        ]
    );
    assert_eq!(
        (set.traces_timeout, set.logs_timeout),
        (Duration::from_millis(2500), Duration::from_millis(700))
    );
    assert_eq!(set.sample_ratio, 0.25);
    assert!(
        !format!("{set:?}").contains("secret") && !format!("{set:?}").contains("logs-key"),
        "header values are not shown"
    );
    let resource = set.resource();
    assert_eq!(resource.get(&"k8s.pod.name".into()).map(|v| v.to_string()), Some("ic-api-1".to_string()));
    assert_eq!(resource.get(&"service.name".into()).map(|v| v.to_string()), Some("renamed".to_string()));

    let only_logs = Settings::from_lookup(
        "ic-api",
        lookup(&[("OTEL_EXPORTER_OTLP_ENDPOINT", "http://c:4318"), ("OTEL_TRACES_EXPORTER", "none")]),
    )
    .unwrap();
    assert!(only_logs.traces.is_none() && only_logs.logs.is_some() && only_logs.exporting());
    let off = Settings::from_lookup(
        "ic-api",
        lookup(&[("OTEL_EXPORTER_OTLP_ENDPOINT", "http://c:4318"), ("OTEL_SDK_DISABLED", "true")]),
    )
    .unwrap();
    assert!(!off.exporting());
    let on = Settings::from_lookup(
        "ic-api",
        lookup(&[("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", "http://t/x"), ("OTEL_SDK_DISABLED", "false")]),
    )
    .unwrap();
    assert_eq!((on.traces.unwrap().endpoint.as_str(), on.logs.is_none()), ("http://t/x", true));
    // Blank values count as unset.
    let blank = Settings::from_lookup("ic-api", |_| Some(" ".to_string())).unwrap();
    assert_eq!((blank.exporting(), blank.service.as_str()), (false, "ic-api"));

    for (key, value, says) in [
        ("OTEL_EXPORTER_OTLP_ENDPOINT", "collector:4318", "OTEL_EXPORTER_OTLP_ENDPOINT"),
        ("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", "collector:4318", "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT"),
        ("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc", "through the collector"),
        ("OTEL_EXPORTER_OTLP_PROTOCOL", "thrift", "http/protobuf or http/json"),
        ("OTEL_EXPORTER_OTLP_COMPRESSION", "zstd", "gzip or none"),
        ("OTEL_EXPORTER_OTLP_HEADERS", "novalue", "OTEL_EXPORTER_OTLP_HEADERS"),
        ("OTEL_EXPORTER_OTLP_HEADERS", "k=%zz", "two hex digits"),
        ("OTEL_EXPORTER_OTLP_HEADERS", "k=%4", "two hex digits"),
        ("OTEL_EXPORTER_OTLP_HEADERS", "k=%ff", "UTF-8"),
        ("OTEL_EXPORTER_OTLP_LOGS_HEADERS", "=v", "OTEL_EXPORTER_OTLP_LOGS_HEADERS"),
        ("OTEL_LOGS_EXPORTER", "jaeger", "OTEL_LOGS_EXPORTER"),
        ("OTEL_SDK_DISABLED", "yes", "OTEL_SDK_DISABLED"),
        ("OTEL_TRACES_SAMPLER_ARG", "1.5", "OTEL_TRACES_SAMPLER_ARG"),
        ("OTEL_TRACES_SAMPLER_ARG", "half", "OTEL_TRACES_SAMPLER_ARG"),
        ("OTEL_EXPORTER_OTLP_TIMEOUT", "0", "OTEL_EXPORTER_OTLP_TIMEOUT"),
        ("OTEL_EXPORTER_OTLP_LOGS_TIMEOUT", "soon", "OTEL_EXPORTER_OTLP_LOGS_TIMEOUT"),
        ("OTEL_RESOURCE_ATTRIBUTES", "zone", "OTEL_RESOURCE_ATTRIBUTES"),
        ("OTEL_RESOURCE_ATTRIBUTES", "=2", "OTEL_RESOURCE_ATTRIBUTES"),
    ] {
        let err = Settings::from_lookup("ic-api", |k| match k {
            "OTEL_EXPORTER_OTLP_ENDPOINT" if key != k => Some("http://c:4318".to_string()),
            _ => (k == key).then(|| value.to_string()),
        })
        .unwrap_err();
        assert!(err.contains(says), "{key}={value}: {err}");
    }
}

/// Purpose: the exporters honour the settings on the wire: traces go to the base endpoint's
/// path as protobuf with the general headers, logs go to their own URL as JSON with their own
/// headers, and a gzip signal is sent compressed.
/// Why it matters: a backend such as Honeycomb, Grafana Cloud or Datadog authenticates by a
/// header and may want logs at a URL of its own; parsing the settings right is no use if the
/// exporter then sends somewhere else, unauthenticated, or in a form the backend refuses.
#[test]
fn exporters_send_where_and_how_the_settings_say() {
    let (addr, received) = collector();
    let base = format!("http://{addr}");
    let logs_url = format!("http://{addr}/custom/logs");
    let vars = [
        ("OTEL_EXPORTER_OTLP_ENDPOINT", base.as_str()),
        ("OTEL_EXPORTER_OTLP_HEADERS", "x-api-key=general"),
        ("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT", logs_url.as_str()),
        ("OTEL_EXPORTER_OTLP_LOGS_PROTOCOL", "http/json"),
        ("OTEL_EXPORTER_OTLP_LOGS_HEADERS", "x-api-key=for-logs"),
    ];
    let settings = Settings::from_lookup("ic-test", |k| vars.iter().find(|(key, _)| *key == k).map(|(_, v)| v.to_string())).unwrap();
    let (subscriber, telemetry) = ic_telemetry::subscriber(&settings, Output::Stderr).unwrap();
    tracing::subscriber::with_default(subscriber, || {
        let span = tracing::info_span!("routed");
        let _e = span.enter();
        tracing::info!("to its own place");
    });
    drop(telemetry);
    {
        let got = received.lock().unwrap();
        let traces = got.requests.iter().find(|(p, _, _)| p == "/v1/traces").expect("traces at the base endpoint");
        assert_eq!(traces.1.get("x-api-key").unwrap(), "general");
        assert_eq!(traces.1.get("content-type").unwrap(), "application/x-protobuf");
        let logs = got.requests.iter().find(|(p, _, _)| p == "/custom/logs").expect("logs at their own endpoint");
        assert_eq!(logs.1.get("x-api-key").unwrap(), "for-logs");
        assert_eq!(logs.1.get("content-type").unwrap(), "application/json");
        assert!(String::from_utf8_lossy(&logs.2).contains("to its own place"));
    }

    let gz = [("OTEL_EXPORTER_OTLP_ENDPOINT", base.as_str()), ("OTEL_EXPORTER_OTLP_COMPRESSION", "gzip")];
    let settings = Settings::from_lookup("ic-test", |k| gz.iter().find(|(key, _)| *key == k).map(|(_, v)| v.to_string())).unwrap();
    let before = received.lock().unwrap().requests.len();
    let (subscriber, telemetry) = ic_telemetry::subscriber(&settings, Output::Stderr).unwrap();
    tracing::subscriber::with_default(subscriber, || {
        let _s = tracing::info_span!("compressed").entered();
        tracing::info!("compressed too");
    });
    drop(telemetry);
    let got = received.lock().unwrap();
    let sent: Vec<_> = got.requests[before..].iter().collect();
    assert!(
        sent.iter().all(|(_, h, _)| h.get("content-encoding").is_some_and(|v| v == "gzip")),
        "everything gzipped"
    );
    assert!(sent.iter().any(|(p, _, _)| p == "/v1/traces") && sent.iter().any(|(p, _, _)| p == "/v1/logs"));
}

/// Purpose: a request through the HTTP layer gets a server span that continues the caller's
/// `traceparent` (same trace, new span), so a handler's own outgoing calls and log lines carry
/// that trace; a request with no header begins a new trace; a malformed header is ignored and
/// the request still answers; probes get no span; and a 500 is answered unchanged.
/// Why it matters: this layer is on every service's router, so it is what joins the browser,
/// the portal, the API and the ledger nodes into one trace; and a bad or hostile header must
/// never make a request fail, nor the readiness probe flood the traces.
#[test]
fn the_http_layer_continues_the_callers_trace() {
    let (subscriber, _telemetry) = ic_telemetry::subscriber(&settings(None), Output::Stderr).unwrap();
    let _default = tracing::subscriber::set_default(subscriber);
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let app = Router::new()
        .route("/v1/policies/{id}", get(|| async { traceparent().unwrap_or_default() }))
        .route("/readyz", get(|| async { traceparent().unwrap_or_else(|| "none".to_string()) }))
        .route("/fail", get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "broken") }))
        .layer(axum::middleware::from_fn(ic_telemetry::http::trace));
    let call = |path: &str, header: Option<&str>| {
        let mut request = Request::builder().uri(path);
        if let Some(h) = header {
            request = request.header("traceparent", h);
        }
        let response = runtime.block_on(app.clone().oneshot(request.body(Body::empty()).unwrap())).unwrap();
        let status = response.status();
        let body = runtime.block_on(response.into_body().collect()).unwrap().to_bytes();
        (status, String::from_utf8(body.to_vec()).unwrap())
    };

    let (status, seen) = call("/v1/policies/P-1", Some(PARENT));
    assert_eq!(status, StatusCode::OK);
    assert!(seen.starts_with(&format!("00-{PARENT_TRACE}-")), "{seen}");
    assert_ne!(seen, PARENT, "the server span is a new span in the caller's trace");

    let (_, fresh) = call("/v1/policies/P-1", None);
    assert!(fresh.starts_with("00-") && !fresh.contains(PARENT_TRACE));
    let (status, bad) = call("/v1/policies/P-1", Some("not-a-traceparent"));
    assert_eq!(status, StatusCode::OK);
    assert!(bad.starts_with("00-"));

    assert_eq!(call("/readyz", Some(PARENT)).1, "none");
    assert_eq!(call("/fail", None), (StatusCode::INTERNAL_SERVER_ERROR, "broken".to_string()));
    assert_eq!(call("/nowhere", None).0, StatusCode::NOT_FOUND);
    assert!(ic_telemetry::http::is_probe("/healthz") && !ic_telemetry::http::is_probe("/v1/status"));
}

/// Purpose: outside any span there is no `traceparent` and nothing is added to a request;
/// inside one, `traced` adds the span's `traceparent`; `continue_from` makes a span the child of
/// a carried `traceparent` and refuses an absent or malformed one; `tracestate` travels with it.
/// Why it matters: these are what the ledger client, the Event Hubs sink and the Temporal
/// worker call to hand a trace on; a missing parent must not invent a broken one, and a
/// caller's trace state (a vendor's sampling hints) must survive the hop.
#[test]
fn trace_context_is_handed_on_and_picked_up() {
    let (subscriber, _telemetry) = ic_telemetry::subscriber(&settings(None), Output::Stdout).unwrap();
    let _default = tracing::subscriber::set_default(subscriber);
    let client = reqwest::Client::new();
    assert_eq!(traceparent(), None);
    let bare = client.get("http://localhost/").traced().build().unwrap();
    assert!(bare.headers().get("traceparent").is_none());

    let span = tracing::info_span!("consume event");
    assert!(continue_from(&span, Some(PARENT)));
    let _entered = span.enter();
    let tp = traceparent().unwrap();
    assert!(tp.starts_with(&format!("00-{PARENT_TRACE}-")));
    let sent = client.get("http://localhost/").traced().build().unwrap();
    assert_eq!(sent.headers().get("traceparent").unwrap().to_str().unwrap(), tp);

    assert!(!continue_from(&tracing::info_span!("x"), None));
    assert!(!continue_from(&tracing::info_span!("y"), Some("00-zz-zz-01")));

    let cx = context_from(Some(PARENT), Some("vendor=1"));
    let carried = headers_of(&cx);
    assert_eq!(carried.get("traceparent").map(String::as_str), Some(PARENT));
    assert_eq!(carried.get("tracestate").map(String::as_str), Some("vendor=1"));
    assert!(headers_of(&opentelemetry::Context::new()).is_empty());
}

#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

impl Write for Buffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Purpose: the log lines are one JSON object per line, scrubbed, with `trace_id` and `span_id`
/// at the top level when written inside a span and without them outside one; the alert rules'
/// phrases survive as written; a line that is not JSON is scrubbed as text.
/// Why it matters: standard output is what Container Insights keeps and what the alert rules
/// search (`Line has "..."`), so the message must come through unchanged, and the trace ID on
/// each line is how an on-call person goes from an alert to the trace.
#[test]
fn log_lines_carry_their_trace_and_are_scrubbed() {
    use tracing_subscriber::layer::SubscriberExt;
    let buffer = Buffer::default();
    let writer = buffer.clone();
    let (traces, _) = ic_telemetry::providers(&settings(None)).unwrap();
    use opentelemetry::trace::TracerProvider as _;
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(traces.tracer("t")))
        .with(ic_telemetry::format::lines(move || writer.clone()));
    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(email = "a@b.com", "outside any span");
        let span = tracing::info_span!("close", partition = 3);
        let _e = span.enter();
        tracing::error!(target: "ic_worker", "a close failed with reconciliation breaks");
    });
    let text = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    let lines: Vec<serde_json::Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["fields"]["email"], REDACTED);
    assert!(lines[0].get("trace_id").is_none());
    assert_eq!(lines[1]["fields"]["message"], "a close failed with reconciliation breaks");
    assert_eq!(lines[1]["trace_id"].as_str().unwrap().len(), 32);
    assert_eq!(lines[1]["span_id"].as_str().unwrap().len(), 16);
    assert_eq!(lines[1]["span"]["name"], "close");

    assert_eq!(finish("not json a@b.com\n", None), format!("not json {REDACTED}"));
    assert_eq!(finish("[1]", Some(("t".into(), "s".into()))), "[1]");
}

/// Purpose: a process may give the tracer provider its own ID generator (the Temporal worker
/// does), whose IDs then appear on its spans, and may take a tracer from its telemetry for a
/// library that makes OpenTelemetry spans itself.
/// Why it matters: the Temporal SDK's plugin needs both: replay-safe IDs, and a tracer on the
/// process's own provider so its workflow and activity spans are exported with the rest.
#[test]
fn a_process_can_bring_its_own_ids_and_take_a_tracer() {
    #[derive(Debug)]
    struct Fixed;
    impl opentelemetry_sdk::trace::IdGenerator for Fixed {
        fn new_trace_id(&self) -> opentelemetry::trace::TraceId {
            opentelemetry::trace::TraceId::from_hex(PARENT_TRACE).unwrap()
        }
        fn new_span_id(&self) -> opentelemetry::trace::SpanId {
            RandomIdGenerator::default().new_span_id()
        }
    }
    use opentelemetry::trace::{Span as _, Tracer as _};
    let custom = ic_telemetry::Custom {
        id_generator: Some(Box::new(Fixed)),
    };
    let (subscriber, telemetry) = ic_telemetry::subscriber_with(&settings(None), Output::Stderr, custom).unwrap();
    tracing::subscriber::with_default(subscriber, || {
        let _s = tracing::info_span!("fixed").entered();
        assert!(traceparent().unwrap().contains(PARENT_TRACE));
    });
    let mut span = telemetry.tracer().start("from a library");
    assert_eq!(span.span_context().trace_id().to_string(), PARENT_TRACE);
    span.end();
}
