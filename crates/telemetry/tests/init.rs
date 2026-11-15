//! Installing telemetry for the whole process. Alone in its file: it sets the global
//! subscriber, which a process can do once, and the process's environment.

/// Purpose: `init` reads the environment, refusing a malformed setting before anything is
/// installed, installs the subscriber and the trace context propagator for the process, and
/// returns a guard that exports to the endpoint set (failing quietly when nothing listens); a
/// second call (a test that
/// starts two services in one process) succeeds and leaves the first in place; `init_stderr`
/// behaves the same; flushing and dropping the guards is safe. A span at info then has a trace
/// even with log lines turned down to warnings (`RUST_LOG=warn`): spans have their own filter.
/// Why it matters: every binary calls this first; it must never panic on a second call, and
/// trace context must be made even with export off so a process still hands the trace on, and
/// quieting a process's logs must not quietly cut its traces into pieces.
#[test]
fn init_installs_once_and_tolerates_a_second_call() {
    // This file's only test, so no other thread reads the environment.
    std::env::set_var("OTEL_TRACES_SAMPLER_ARG", "2");
    assert!(ic_telemetry::init("ic-init-test").unwrap_err().contains("OTEL_TRACES_SAMPLER_ARG"));
    std::env::remove_var("OTEL_TRACES_SAMPLER_ARG");
    std::env::set_var("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:9");
    std::env::set_var("OTEL_EXPORTER_OTLP_TIMEOUT", "300");
    std::env::set_var("RUST_LOG", "warn");
    let first = ic_telemetry::init("ic-init-test").unwrap();
    assert!(first.exporting());
    assert!(format!("{first:?}").contains("exporting"));
    let second = ic_telemetry::init_stderr("ic-init-test").unwrap();
    second.flush();
    drop(second);
    let span = tracing::info_span!("after init");
    let _e = span.enter();
    assert!(ic_telemetry::traceparent().is_some());
    first.flush();
}
