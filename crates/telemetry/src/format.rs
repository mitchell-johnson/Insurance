//! The log lines on standard output or error: JSON, one event per line, as
//! the services wrote before, scrubbed of personal data ([`crate::scrub`])
//! and carrying the trace and span they were written in.
//!
//! Each line has `trace_id` and `span_id` at the top level when it was
//! written inside a span, which is what Log Analytics joins on to find the
//! trace in Application Insights (`operation_Id` there is the trace ID).

use std::fmt;

use opentelemetry::trace::TraceContextExt;
use tracing::{Event, Subscriber};
use tracing_subscriber::fmt::format::{Format, Json, JsonFields, Writer};
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields, MakeWriter};
use tracing_subscriber::registry::LookupSpan;

/// The formatting layer for log lines written to `writer`.
pub fn lines<S, W>(writer: W) -> tracing_subscriber::fmt::Layer<S, JsonFields, Traced, W>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    W: for<'w> MakeWriter<'w> + 'static,
{
    tracing_subscriber::fmt::layer().json().event_format(Traced::default()).with_writer(writer)
}

/// The JSON format, with the trace added and personal data scrubbed.
#[derive(Debug)]
pub struct Traced {
    inner: Format<Json>,
}

impl Default for Traced {
    fn default() -> Self {
        Self {
            inner: Format::default().json(),
        }
    }
}

/// The trace and span IDs of the OpenTelemetry context now current, if it
/// is a real one.
pub fn current_ids() -> Option<(String, String)> {
    let cx = opentelemetry::Context::current();
    let span = cx.span();
    let sc = span.span_context();
    sc.is_valid().then(|| (sc.trace_id().to_string(), sc.span_id().to_string()))
}

/// One line as the JSON formatter wrote it, scrubbed and with the trace IDs
/// added. A line that is not JSON (it never is not) is scrubbed as text.
pub fn finish(line: &str, ids: Option<(String, String)>) -> String {
    let body = line.trim_end_matches('\n');
    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(mut value) => {
            crate::scrub::json(&mut value);
            if let (Some((trace, span)), serde_json::Value::Object(map)) = (ids, &mut value) {
                map.insert("trace_id".to_string(), serde_json::Value::String(trace));
                map.insert("span_id".to_string(), serde_json::Value::String(span));
            }
            value.to_string()
        }
        Err(_) => crate::scrub::text(body).into_owned(),
    }
}

impl<S, N> FormatEvent<S, N> for Traced
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(&self, ctx: &FmtContext<'_, S, N>, mut writer: Writer<'_>, event: &Event<'_>) -> fmt::Result {
        let mut line = String::new();
        self.inner.format_event(ctx, Writer::new(&mut line), event)?;
        writeln!(writer, "{}", finish(&line, current_ids()))
    }
}
