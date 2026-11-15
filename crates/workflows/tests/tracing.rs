//! Trace context through Temporal: the worker's client carries the SDK's OpenTelemetry plugin.

use ic_workflows::temporal::{telemetry_custom, Tracing};
use temporalio_client::envconfig::LoadClientConfigProfileOptions;
use temporalio_client::ClientOptions;

/// Purpose: loading the client's configuration with the `Tracing` extension registers exactly
/// one plugin, the SDK's OpenTelemetry plugin on the process's own tracer, and the worker's
/// telemetry settings give the tracer provider the SDK's replay-safe ID generator, which still
/// makes real trace context.
/// Why it matters: without the plugin a close's steps would each start a trace of their own and
/// nothing would join the close job, the workflow and the ledger commands it sends; without the
/// replay-safe IDs, every replay of a workflow on another worker would give its spans new IDs.
#[test]
fn the_worker_client_carries_the_tracing_plugin() {
    let settings = ic_telemetry::Settings::from_lookup("ic-worker-test", |_| None).unwrap();
    let (subscriber, telemetry) = ic_telemetry::subscriber_with(&settings, ic_telemetry::Output::Stderr, telemetry_custom()).unwrap();
    let tracing = Tracing { tracer: telemetry.tracer() };
    let mut options = LoadClientConfigProfileOptions::default();
    options.disable_file = true;
    let (_, client) = ClientOptions::load_from_config_with_extensions(options, &[&tracing]).unwrap();
    assert_eq!(client.plugins().len(), 1);
    tracing::subscriber::with_default(subscriber, || {
        let span = tracing::info_span!("close");
        let _e = span.enter();
        assert!(ic_telemetry::traceparent().is_some());
    });
}
