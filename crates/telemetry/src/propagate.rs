//! W3C trace context across process boundaries.
//!
//! Outgoing: [`Traced::traced`] on a `reqwest` request adds `traceparent`
//! (and `tracestate`) for the current span; [`traceparent`] gives the same
//! value as text, for a ledger command's envelope, an Event Hubs message's
//! properties or a Temporal workflow's input.
//!
//! Incoming: [`crate::http`] continues the caller's trace for each request;
//! [`continue_from`] makes a span the child of a `traceparent` carried in a
//! message or an input.

use std::collections::HashMap;

use opentelemetry::propagation::TextMapPropagator;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::Context;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// The trace context headers of `cx`: `traceparent`, and `tracestate` when
/// it has one. Empty when `cx` holds no real span.
pub fn headers_of(cx: &Context) -> HashMap<String, String> {
    let mut carrier = HashMap::new();
    if cx.span().span_context().is_valid() {
        TraceContextPropagator::new().inject_context(cx, &mut carrier);
    }
    carrier
}

/// The trace context headers of the current span: the current `tracing`
/// span's, or, outside any, the OpenTelemetry context a library made
/// current (the Temporal SDK does, around each activity).
pub fn headers() -> HashMap<String, String> {
    let traced = headers_of(&tracing::Span::current().context());
    if traced.is_empty() {
        headers_of(&Context::current())
    } else {
        traced
    }
}

/// The current span's `traceparent`, or `None` outside any span.
pub fn traceparent() -> Option<String> {
    headers().remove("traceparent")
}

/// The context a `traceparent` (and `tracestate`) names. An absent or
/// malformed value gives an empty context, so the span begins a trace of
/// its own: a bad header from a caller never fails the call.
pub fn context_from(traceparent: Option<&str>, tracestate: Option<&str>) -> Context {
    let mut carrier = HashMap::new();
    if let Some(tp) = traceparent {
        carrier.insert("traceparent".to_string(), tp.to_string());
    }
    if let Some(ts) = tracestate {
        carrier.insert("tracestate".to_string(), ts.to_string());
    }
    TraceContextPropagator::new().extract_with_context(&Context::new(), &carrier)
}

/// Makes `span` a child of the trace `traceparent` names, if it names one.
/// Returns whether it did.
pub fn continue_from(span: &tracing::Span, traceparent: Option<&str>) -> bool {
    let cx = context_from(traceparent, None);
    if !cx.span().span_context().is_valid() {
        return false;
    }
    span.set_parent(cx).is_ok()
}

/// The context carried by HTTP headers.
pub fn extract(headers: &axum::http::HeaderMap) -> Context {
    let get = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    context_from(get("traceparent"), get("tracestate"))
}

/// Adds the current span's trace context to an outgoing request.
pub trait Traced {
    fn traced(self) -> Self;
}

impl Traced for reqwest::RequestBuilder {
    fn traced(self) -> Self {
        headers().into_iter().fold(self, |request, (k, v)| request.header(k, v))
    }
}
