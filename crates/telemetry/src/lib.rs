//! OpenTelemetry for every process in the insurance core.
//!
//! One call at the top of `main`, [`init`] (or [`init_stderr`] for a process
//! whose standard output is a report), sets up the same three things in
//! every service, driver, job and worker:
//!
//! - **Logs** as JSON lines on standard output or error, as before, each now
//!   carrying the `trace_id` and `span_id` of the span it was written in, so
//!   the container log in Log Analytics joins to the trace.
//! - **Traces**: every `tracing` span is an OpenTelemetry span. Trace context
//!   (W3C `traceparent`) is carried across HTTP ([`http`], [`propagate`]),
//!   into ledger commands and their events (the envelope's `traceparent`),
//!   onto Event Hubs messages, and into Temporal workflows (their input), so
//!   a command can be followed from the browser to the ledger and on.
//! - **Export over OTLP** (HTTP and protobuf) of both traces and logs to the
//!   collector named by `OTEL_EXPORTER_OTLP_ENDPOINT`, which sends them to
//!   Application Insights (`deploy/k8s/telemetry.yaml`). Without that
//!   variable nothing is exported, but trace context is still made and
//!   carried, so a process behind one that exports still joins its trace.
//!
//! Personal data stays out of both ([`scrub`]): a field whose name says it
//! holds a name, contact detail, identity or health detail is replaced by
//! `[redacted]` and e-mail addresses, bearer tokens and bank account numbers
//! inside any text are masked, on standard output and in what is exported.
//! Spans made by [`http`] record the route template, never the path, query,
//! headers or the caller's address.

pub mod format;
pub mod http;
pub mod logs;
pub mod propagate;
pub mod scrub;

use std::time::Duration;

use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_otlp::{Protocol, WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::logs::{BatchLogProcessor, SdkLoggerProvider};
use opentelemetry_sdk::trace::{BatchSpanProcessor, IdGenerator, Sampler, SdkTracer, SdkTracerProvider};
use opentelemetry_sdk::Resource;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, Registry};

pub use propagate::{continue_from, traceparent, Traced};
/// The OpenTelemetry API and SDK at the versions this crate uses, for a
/// process that hands a tracer to a library (the Temporal worker).
pub use {opentelemetry, opentelemetry_sdk};

/// The `service.namespace` of every process.
pub const NAMESPACE: &str = "insurance-core";

/// Where a process's log lines go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    Stdout,
    /// For a process whose standard output is its report (the close, the
    /// payments pump's commands, ic-admin).
    Stderr,
}

/// How a signal is sent: OTLP over HTTP, as protobuf or as JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    /// `http/protobuf`, the default.
    Protobuf,
    /// `http/json`.
    Json,
}

/// Headers sent with every export: an API key or a dataset name a service
/// asks for. Their values are never printed.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Headers(pub Vec<(String, String)>);

impl std::fmt::Debug for Headers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.0.iter().map(|(k, _)| format!("{k}=…"))).finish()
    }
}

/// Where and how one signal (traces or logs) is exported.
#[derive(Debug, Clone, PartialEq)]
pub struct Signal {
    /// The full URL the signal is posted to.
    pub endpoint: String,
    pub wire: Wire,
    pub headers: Headers,
    /// `gzip`, or nothing.
    pub gzip: bool,
}

/// What [`init`] reads from the environment: the standard OpenTelemetry
/// variables, so the same image exports to the collector in the cluster
/// (the default, which sends on to Azure Monitor) or straight to any
/// service that takes OTLP over HTTP (docs/operations.md, "Telemetry").
///
/// | Variable | Meaning |
/// | --- | --- |
/// | `OTEL_EXPORTER_OTLP_ENDPOINT` | Base URL; `/v1/traces` and `/v1/logs` are added |
/// | `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`, `..._LOGS_ENDPOINT` | One signal's full URL, used as written |
/// | `OTEL_EXPORTER_OTLP_PROTOCOL`, `..._TRACES_PROTOCOL`, `..._LOGS_PROTOCOL` | `http/protobuf` (default) or `http/json` |
/// | `OTEL_EXPORTER_OTLP_HEADERS`, `..._TRACES_HEADERS`, `..._LOGS_HEADERS` | `key=value,key=value`, values URL-encoded; a signal's own add to and override the general ones |
/// | `OTEL_EXPORTER_OTLP_COMPRESSION`, `..._TRACES_COMPRESSION`, `..._LOGS_COMPRESSION` | `gzip` or `none` |
/// | `OTEL_EXPORTER_OTLP_TIMEOUT`, `..._TRACES_TIMEOUT`, `..._LOGS_TIMEOUT` | Milliseconds, default 10000 |
/// | `OTEL_TRACES_EXPORTER`, `OTEL_LOGS_EXPORTER` | `otlp` (default) or `none` |
/// | `OTEL_SDK_DISABLED` | `true` exports nothing |
/// | `OTEL_SERVICE_NAME`, `OTEL_RESOURCE_ATTRIBUTES`, `OTEL_TRACES_SAMPLER_ARG` | As named |
///
/// gRPC is not spoken by the services; a service that only takes gRPC is
/// reached through the collector, which converts.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// `service.name`: `OTEL_SERVICE_NAME`, or the name the process gives.
    pub service: String,
    /// `service.version`: `IC_VERSION` (the image tag), or this crate's version.
    pub version: String,
    /// Where traces go; `None` exports none.
    pub traces: Option<Signal>,
    /// Where logs go; `None` exports none (the lines on standard output stay).
    pub logs: Option<Signal>,
    /// The share of new traces kept, from `OTEL_TRACES_SAMPLER_ARG` (0 to 1,
    /// default 1). A trace begun upstream keeps that decision.
    pub sample_ratio: f64,
    /// How long one export may take, per signal.
    pub traces_timeout: Duration,
    pub logs_timeout: Duration,
    /// `OTEL_RESOURCE_ATTRIBUTES`, as `key=value` pairs: the pod, node and
    /// zone the manifests set from the downward API.
    pub attributes: Vec<(String, String)>,
}

/// Decodes `%XX` escapes, as the specification asks of header values.
fn percent_decode(value: &str) -> Result<String, String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = value
                .get(i + 1..i + 3)
                .ok_or_else(|| format!("a % not followed by two hex digits in {value:?}"))?;
            out.push(u8::from_str_radix(hex, 16).map_err(|_| format!("a % not followed by two hex digits in {value:?}"))?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| format!("header value {value:?} is not UTF-8 once decoded"))
}

/// `key=value,key=value`, as `OTEL_EXPORTER_OTLP_HEADERS` is written.
fn parse_headers(var: &str, text: &str) -> Result<Vec<(String, String)>, String> {
    let mut headers = Vec::new();
    for pair in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        match pair.split_once('=') {
            Some((k, v)) if !k.trim().is_empty() => {
                let value = percent_decode(v.trim()).map_err(|e| format!("{var}: {e}"))?;
                headers.push((k.trim().to_ascii_lowercase(), value));
            }
            _ => return Err(format!("{var} must be key=value pairs, and one is not")),
        }
    }
    Ok(headers)
}

fn is_url(e: &str) -> bool {
    e.starts_with("http://") || e.starts_with("https://")
}

impl Settings {
    /// Reads the settings, or says which one is wrong.
    pub fn from_lookup(service: &str, get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let get = |key: &str| get(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let base = get("OTEL_EXPORTER_OTLP_ENDPOINT").map(|e| e.trim_end_matches('/').to_string());
        if let Some(e) = &base {
            if !is_url(e) {
                return Err(format!("OTEL_EXPORTER_OTLP_ENDPOINT must be an http or https URL, not {e:?}"));
            }
        }
        let disabled = match get("OTEL_SDK_DISABLED").as_deref() {
            None | Some("false") => false,
            Some("true") => true,
            Some(v) => return Err(format!("OTEL_SDK_DISABLED must be true or false, not {v:?}")),
        };
        let timeout_of = |var: &str, fallback: Duration| -> Result<Duration, String> {
            match get(var) {
                None => Ok(fallback),
                Some(v) => match v.parse::<u64>() {
                    Ok(ms) if ms > 0 => Ok(Duration::from_millis(ms)),
                    _ => Err(format!("{var} must be a whole number of milliseconds, not {v:?}")),
                },
            }
        };
        let timeout = timeout_of("OTEL_EXPORTER_OTLP_TIMEOUT", Duration::from_secs(10))?;
        let general_headers = parse_headers("OTEL_EXPORTER_OTLP_HEADERS", &get("OTEL_EXPORTER_OTLP_HEADERS").unwrap_or_default())?;
        let signal = |name: &str, path: &str| -> Result<Option<Signal>, String> {
            let upper = name.to_ascii_uppercase();
            match get(&format!("OTEL_{upper}_EXPORTER")).as_deref() {
                None | Some("otlp") => {}
                Some("none") => return Ok(None),
                Some(v) => return Err(format!("OTEL_{upper}_EXPORTER must be otlp or none, not {v:?}")),
            }
            let own = format!("OTEL_EXPORTER_OTLP_{upper}_ENDPOINT");
            let endpoint = match (get(&own), &base) {
                (Some(e), _) if is_url(&e) => e,
                (Some(e), _) => return Err(format!("{own} must be an http or https URL, not {e:?}")),
                (None, Some(b)) => format!("{b}{path}"),
                (None, None) => return Ok(None),
            };
            let protocol_var = format!("OTEL_EXPORTER_OTLP_{upper}_PROTOCOL");
            let protocol = get(&protocol_var).or_else(|| get("OTEL_EXPORTER_OTLP_PROTOCOL"));
            let wire = match protocol.as_deref() {
                None | Some("http/protobuf") => Wire::Protobuf,
                Some("http/json") => Wire::Json,
                Some("grpc") => {
                    return Err(format!(
                        "{name}: the services export OTLP over HTTP only (http/protobuf or http/json); send gRPC through the collector"
                    ))
                }
                Some(v) => return Err(format!("{name}: the protocol must be http/protobuf or http/json, not {v:?}")),
            };
            let mut headers = general_headers.clone();
            let own_headers = format!("OTEL_EXPORTER_OTLP_{upper}_HEADERS");
            for (k, v) in parse_headers(&own_headers, &get(&own_headers).unwrap_or_default())? {
                headers.retain(|(existing, _)| *existing != k);
                headers.push((k, v));
            }
            let compression_var = format!("OTEL_EXPORTER_OTLP_{upper}_COMPRESSION");
            let gzip = match get(&compression_var).or_else(|| get("OTEL_EXPORTER_OTLP_COMPRESSION")).as_deref() {
                None | Some("none") => false,
                Some("gzip") => true,
                Some(v) => return Err(format!("{name}: the compression must be gzip or none, not {v:?}")),
            };
            Ok(Some(Signal {
                endpoint,
                wire,
                headers: Headers(headers),
                gzip,
            }))
        };
        let (traces, logs) = if disabled {
            (None, None)
        } else {
            (signal("traces", "/v1/traces")?, signal("logs", "/v1/logs")?)
        };
        let sample_ratio = match get("OTEL_TRACES_SAMPLER_ARG") {
            None => 1.0,
            Some(v) => match v.parse::<f64>() {
                Ok(r) if (0.0..=1.0).contains(&r) => r,
                _ => return Err(format!("OTEL_TRACES_SAMPLER_ARG must be a number from 0 to 1, not {v:?}")),
            },
        };
        let mut attributes = Vec::new();
        for pair in get("OTEL_RESOURCE_ATTRIBUTES")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            match pair.split_once('=') {
                Some((k, v)) if !k.trim().is_empty() => attributes.push((k.trim().to_string(), v.trim().to_string())),
                _ => return Err(format!("OTEL_RESOURCE_ATTRIBUTES must be key=value pairs, not {pair:?}")),
            }
        }
        Ok(Self {
            service: get("OTEL_SERVICE_NAME").unwrap_or_else(|| service.to_string()),
            version: get("IC_VERSION").unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string()),
            traces,
            logs,
            sample_ratio,
            traces_timeout: timeout_of("OTEL_EXPORTER_OTLP_TRACES_TIMEOUT", timeout)?,
            logs_timeout: timeout_of("OTEL_EXPORTER_OTLP_LOGS_TIMEOUT", timeout)?,
            attributes,
        })
    }

    /// Whether anything is exported.
    pub fn exporting(&self) -> bool {
        self.traces.is_some() || self.logs.is_some()
    }

    /// The resource every span and log record of the process carries.
    pub fn resource(&self) -> Resource {
        let mut attributes = vec![
            KeyValue::new("service.namespace", NAMESPACE),
            KeyValue::new("service.version", self.version.clone()),
        ];
        for (k, v) in &self.attributes {
            if k != "service.name" {
                attributes.push(KeyValue::new(k.clone(), v.clone()));
            }
        }
        Resource::builder_empty()
            .with_service_name(self.service.clone())
            .with_attributes(attributes)
            .build()
    }
}

/// Keeps the exporters running. Dropping it sends what is still buffered,
/// so it is held until `main` returns.
#[must_use = "dropping the guard stops export"]
pub struct Telemetry {
    traces: SdkTracerProvider,
    logs: Option<SdkLoggerProvider>,
    exporting: bool,
}

impl std::fmt::Debug for Telemetry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Telemetry").field("exporting", &self.exporting).finish()
    }
}

impl Telemetry {
    /// Whether traces and logs are being sent to a collector.
    pub fn exporting(&self) -> bool {
        self.exporting
    }

    /// A tracer on this process's provider, for a library that makes its
    /// own spans with OpenTelemetry directly (the Temporal SDK's plugin).
    pub fn tracer(&self) -> SdkTracer {
        self.traces.tracer("ic-telemetry")
    }

    /// Sends everything buffered now. Used before a job exits on its own.
    pub fn flush(&self) {
        let _ = self.traces.force_flush();
        if let Some(logs) = &self.logs {
            let _ = logs.force_flush();
        }
    }
}

impl Drop for Telemetry {
    fn drop(&mut self) {
        let _ = self.traces.shutdown();
        if let Some(logs) = &self.logs {
            let _ = logs.shutdown();
        }
    }
}

/// How a process changes the providers, where it must.
#[derive(Default)]
pub struct Custom {
    /// Makes span and trace IDs. The Temporal worker passes the SDK's
    /// `WorkflowIdGenerator`, which gives a workflow's spans the same IDs
    /// when Temporal replays it.
    pub id_generator: Option<Box<dyn IdGenerator>>,
}

/// The providers for these settings: traces always (so context is made and
/// carried), logs only when exporting.
pub fn providers(settings: &Settings) -> Result<(SdkTracerProvider, Option<SdkLoggerProvider>), String> {
    providers_with(settings, Custom::default())
}

/// As [`providers`], changed as `custom` says.
pub fn providers_with(settings: &Settings, custom: Custom) -> Result<(SdkTracerProvider, Option<SdkLoggerProvider>), String> {
    let resource = settings.resource();
    let sampler = Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(settings.sample_ratio)));
    let mut traces = SdkTracerProvider::builder().with_sampler(sampler).with_resource(resource.clone());
    if let Some(ids) = custom.id_generator {
        traces = traces.with_id_generator(BoxedIds(ids));
    }
    let mut logs = None;
    if let Some(signal) = &settings.traces {
        let mut spans = opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_protocol(protocol(signal))
            .with_endpoint(signal.endpoint.clone())
            .with_timeout(settings.traces_timeout)
            .with_headers(signal.headers.0.iter().cloned().collect());
        if signal.gzip {
            spans = spans.with_compression(opentelemetry_otlp::Compression::Gzip);
        }
        let spans = spans.build().map_err(|e| format!("trace exporter: {e}"))?;
        traces = traces.with_span_processor(scrub::ScrubSpans::new(BatchSpanProcessor::builder(spans).build()));
    }
    if let Some(signal) = &settings.logs {
        let mut records = opentelemetry_otlp::LogExporter::builder()
            .with_http()
            .with_protocol(protocol(signal))
            .with_endpoint(signal.endpoint.clone())
            .with_timeout(settings.logs_timeout)
            .with_headers(signal.headers.0.iter().cloned().collect());
        if signal.gzip {
            records = records.with_compression(opentelemetry_otlp::Compression::Gzip);
        }
        let records = records.build().map_err(|e| format!("log exporter: {e}"))?;
        logs = Some(
            SdkLoggerProvider::builder()
                .with_resource(resource)
                .with_log_processor(BatchLogProcessor::builder(records).build())
                .build(),
        );
    }
    Ok((traces.build(), logs))
}

fn protocol(signal: &Signal) -> Protocol {
    match signal.wire {
        Wire::Protobuf => Protocol::HttpBinary,
        Wire::Json => Protocol::HttpJson,
    }
}

/// A boxed ID generator, as the provider's builder takes one.
#[derive(Debug)]
struct BoxedIds(Box<dyn IdGenerator>);

impl IdGenerator for BoxedIds {
    fn new_trace_id(&self) -> opentelemetry::trace::TraceId {
        self.0.new_trace_id()
    }

    fn new_span_id(&self) -> opentelemetry::trace::SpanId {
        self.0.new_span_id()
    }
}

/// The filter for log lines and log records: `RUST_LOG`, or `info`.
fn filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
}

/// The filter for spans: `IC_TRACE_FILTER`, or `info`. Separate from `RUST_LOG`, so quieter log
/// lines (`RUST_LOG=warn`) do not drop the spans a trace is made of.
fn trace_filter() -> EnvFilter {
    EnvFilter::try_from_env("IC_TRACE_FILTER").unwrap_or_else(|_| EnvFilter::new("info"))
}

/// Builds the subscriber for these settings and output, without installing it.
pub fn subscriber(settings: &Settings, output: Output) -> Result<(impl tracing::Subscriber + Send + Sync, Telemetry), String> {
    subscriber_with(settings, output, Custom::default())
}

/// As [`subscriber`], with the providers changed as `custom` says.
pub fn subscriber_with(settings: &Settings, output: Output, custom: Custom) -> Result<(impl tracing::Subscriber + Send + Sync, Telemetry), String> {
    let (traces, logs) = providers_with(settings, custom)?;
    let tracer = traces.tracer("ic-telemetry");
    let lines = match output {
        Output::Stdout => format::lines(std::io::stdout).with_filter(filter()).boxed(),
        Output::Stderr => format::lines(std::io::stderr).with_filter(filter()).boxed(),
    };
    let exported = logs.as_ref().map(|p| logs::ScrubbedBridge::new(p).with_filter(filter()).boxed());
    let spans = tracing_opentelemetry::layer()
        .with_tracer(tracer)
        .with_threads(false)
        .with_location(false)
        .with_filter(trace_filter());
    let subscriber = Registry::default().with(spans).with(lines).with(exported);
    let exporting = settings.exporting();
    Ok((subscriber, Telemetry { traces, logs, exporting }))
}

/// Sets up logs and traces for the process named `service`, logging to
/// standard output. Hold the result until `main` returns.
///
/// A second call in one process (a test running two services) keeps the
/// first one's subscriber and returns a guard that exports nothing more.
pub fn init(service: &str) -> Result<Telemetry, String> {
    init_to(service, Output::Stdout)
}

/// As [`init`], logging to standard error.
pub fn init_stderr(service: &str) -> Result<Telemetry, String> {
    init_to(service, Output::Stderr)
}

/// As [`init`], with the output chosen.
pub fn init_to(service: &str, output: Output) -> Result<Telemetry, String> {
    init_with(service, output, Custom::default())
}

/// As [`init`], with the output chosen and the providers changed.
pub fn init_with(service: &str, output: Output, custom: Custom) -> Result<Telemetry, String> {
    let settings = Settings::from_lookup(service, |key| std::env::var(key).ok())?;
    let (subscriber, telemetry) = subscriber_with(&settings, output, custom)?;
    if subscriber.try_init().is_ok() {
        opentelemetry::global::set_text_map_propagator(opentelemetry_sdk::propagation::TraceContextPropagator::new());
        if telemetry.exporting {
            tracing::info!(service = %settings.service, "telemetry is exported over OTLP");
        }
    }
    Ok(telemetry)
}
