//! Log events as OpenTelemetry log records, scrubbed of personal data.
//!
//! The same events that become log lines become log records sent over
//! OTLP. Each record carries the trace and span it was written in (the SDK
//! takes them from the context the span activated), its level, its target
//! and its fields as attributes, every field passed through
//! [`crate::scrub`] on the way.

use opentelemetry::logs::{AnyValue, LogRecord as _, Logger as _, LoggerProvider as _, Severity};
use opentelemetry_sdk::logs::{SdkLogger, SdkLoggerProvider};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

use crate::scrub;

/// The layer that turns events into scrubbed log records.
pub struct ScrubbedBridge {
    logger: SdkLogger,
}

impl ScrubbedBridge {
    pub fn new(provider: &SdkLoggerProvider) -> Self {
        Self {
            logger: provider.logger("ic-telemetry"),
        }
    }
}

/// The OpenTelemetry severity of a `tracing` level.
pub fn severity(level: &Level) -> Severity {
    match *level {
        Level::TRACE => Severity::Trace,
        Level::DEBUG => Severity::Debug,
        Level::INFO => Severity::Info,
        Level::WARN => Severity::Warn,
        _ => Severity::Error,
    }
}

/// The fields of one event: the message becomes the body, the others
/// attributes, all scrubbed.
#[derive(Debug, Default, PartialEq)]
pub struct Fields {
    pub body: Option<String>,
    pub attributes: Vec<(String, AnyValue)>,
}

impl Fields {
    fn push(&mut self, field: &Field, value: AnyValue) {
        let key = field.name();
        if key.starts_with("log.") {
            return;
        }
        let value = if scrub::sensitive_key(key) { AnyValue::from(scrub::REDACTED) } else { value };
        self.attributes.push((key.to_string(), value));
    }
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.body = Some(scrub::text(value).into_owned());
        } else {
            self.push(field, AnyValue::from(scrub::text(value).into_owned()));
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.record_str(field, &format!("{value:?}"));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.push(field, AnyValue::from(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        match i64::try_from(value) {
            Ok(v) => self.push(field, AnyValue::from(v)),
            Err(_) => self.push(field, AnyValue::from(value.to_string())),
        }
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.push(field, AnyValue::from(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.push(field, AnyValue::from(value));
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.record_str(field, &value.to_string());
    }
}

impl<S: Subscriber> Layer<S> for ScrubbedBridge {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let mut fields = Fields::default();
        event.record(&mut fields);
        let mut record = self.logger.create_log_record();
        record.set_target(meta.target().to_string());
        record.set_event_name(meta.name());
        record.set_severity_number(severity(meta.level()));
        record.set_severity_text(meta.level().as_str());
        if let Some(body) = fields.body {
            record.set_body(AnyValue::from(body));
        }
        for (k, v) in fields.attributes {
            record.add_attribute(k, v);
        }
        self.logger.emit(record);
    }
}
