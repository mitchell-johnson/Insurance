//! A server span for every HTTP request a service answers.
//!
//! Add it to a router as its last layer, so it wraps every route:
//!
//! ```ignore
//! router.layer(axum::middleware::from_fn(ic_telemetry::http::trace))
//! ```
//!
//! The span continues the caller's trace from its `traceparent` header (a
//! browser page, the portal, another service), is named after the method
//! and the route template (`POST /v1/partitions/{partition}/commands`), and
//! records the status code. It records nothing else about the request: not
//! the path with its identifiers, the query, the headers or the caller's
//! address. A response of 500 or more marks the span as failed.

use axum::extract::{MatchedPath, Request};
use axum::middleware::Next;
use axum::response::Response;
use tracing::Instrument;
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// Health and readiness probes, polled every few seconds: they get no span,
/// so they do not crowd out the traces that matter.
pub fn is_probe(route: &str) -> bool {
    matches!(route, "/healthz" | "/readyz" | "/livez")
}

/// The middleware. See the module documentation.
pub async fn trace(request: Request, next: Next) -> Response {
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| "unmatched".to_string());
    if is_probe(&route) {
        return next.run(request).await;
    }
    let method = request.method().as_str().to_string();
    let span = tracing::info_span!(
        "http.server",
        otel.name = %format!("{method} {route}"),
        otel.kind = "server",
        http.request.method = %method,
        http.route = %route,
        http.response.status_code = tracing::field::Empty,
        otel.status_code = tracing::field::Empty,
    );
    let parent = crate::propagate::extract(request.headers());
    let _ = span.set_parent(parent);
    let response = next.run(request).instrument(span.clone()).await;
    let status = response.status().as_u16();
    span.record("http.response.status_code", status);
    if status >= 500 {
        span.record("otel.status_code", "ERROR");
    }
    response
}
