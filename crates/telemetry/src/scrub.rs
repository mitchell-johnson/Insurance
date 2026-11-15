//! Keeping personal data out of logs and traces.
//!
//! Two rules, applied to every log line (on standard output and exported)
//! and every exported span:
//!
//! 1. A field whose **name** says it holds personal data is replaced whole by
//!    [`REDACTED`]: names, contact details, dates of birth, government and
//!    bank identifiers, health details, and credentials ([`sensitive_key`]).
//! 2. Inside any **text**, e-mail addresses, bearer tokens and JWTs, and New
//!    Zealand bank account numbers are masked ([`text`]).
//!
//! The services already log opaque identifiers (party, policy, document
//! IDs) rather than the facts about a person; this is the net under that
//! habit, not a replacement for it.

use std::borrow::Cow;
use std::time::Duration;

use opentelemetry::{Context, KeyValue, Value};
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::trace::{Span, SpanData, SpanProcessor};
use opentelemetry_sdk::Resource;

/// What replaces a field or a piece of text that may be personal data.
pub const REDACTED: &str = "[redacted]";

/// Parts of a field name that mark it as personal data. A field matches when
/// one of these is one of its words (split on `.`, `_`, `-`), or, for the
/// longer entries, appears anywhere in it.
const WORDS: &[&str] = &[
    "name",
    "email",
    "phone",
    "mobile",
    "address",
    "street",
    "suburb",
    "postcode",
    "dob",
    "birth",
    "ird",
    "passport",
    "licence",
    "license",
    "iban",
    "password",
    "secret",
    "token",
    "authorization",
    "cookie",
    "diagnosis",
    "medical",
    "disclosure",
    "symptoms",
    "condition",
    "nhi",
    "gender",
    "sex",
];

const ANYWHERE: &[&str] = &[
    "firstname",
    "lastname",
    "surname",
    "givenname",
    "familyname",
    "fullname",
    "birthdate",
    "dateofbirth",
    "bankaccount",
    "accountnumber",
    "cardnumber",
    "taxnumber",
    "taxid",
    "client_ip",
    "client.address",
    "user_agent",
    "http.request.header",
    "url.query",
];

/// Field names that look sensitive by the rules above but are not: the
/// telemetry's own and OpenTelemetry's conventions.
const ALLOWED: &[&str] = &[
    "otel.name",
    "service.name",
    "span.name",
    "event.name",
    "code.function.name",
    "http.route",
    "rpc.method",
    "module",
    "error.type",
];

/// Whether a field of this name may hold personal data.
pub fn sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    if ALLOWED.contains(&key.as_str()) {
        return false;
    }
    let squashed: String = key.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '_').collect();
    if ANYWHERE.iter().any(|w| squashed.contains(w) || squashed.replace('_', "").contains(w)) {
        return true;
    }
    key.split(['.', '_', '-']).any(|word| WORDS.contains(&word))
}

/// The text with e-mail addresses, bearer tokens, JWTs and bank account
/// numbers masked. Borrowed when there is nothing to mask.
pub fn text(input: &str) -> Cow<'_, str> {
    if !(input.contains('@') || input.contains("eyJ") || input.contains("earer ") || has_account_number(input)) {
        return Cow::Borrowed(input);
    }
    let mut out = String::with_capacity(input.len());
    let tokens: Vec<&str> = split_keep(input);
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i];
        // "Bearer <token>": the token goes, the word stays.
        if t.eq_ignore_ascii_case("bearer") && i + 2 < tokens.len() && tokens[i + 1] == " " && !is_separator(tokens[i + 2]) {
            out.push_str(t);
            out.push(' ');
            out.push_str(REDACTED);
            i += 3;
            continue;
        }
        if is_email(t) || is_jwt(t) || is_account_number(t) {
            out.push_str(REDACTED);
        } else {
            out.push_str(t);
        }
        i += 1;
    }
    Cow::Owned(out)
}

/// Splits text into runs of word characters (letters, digits and `@ . _ - +
/// =`, which addresses, tokens and account numbers are made of) and the
/// separators between them, keeping both.
fn split_keep(input: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut start = 0;
    let mut in_word: Option<bool> = None;
    for (i, c) in input.char_indices() {
        let word = is_word_char(c);
        match in_word {
            Some(w) if w == word => {}
            Some(_) => {
                tokens.push(&input[start..i]);
                start = i;
            }
            None => {}
        }
        in_word = Some(word);
    }
    if start < input.len() {
        tokens.push(&input[start..]);
    }
    tokens
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '@' | '.' | '_' | '-' | '+' | '=' | '/')
}

fn is_separator(t: &str) -> bool {
    !t.chars().next().is_some_and(is_word_char)
}

fn is_email(t: &str) -> bool {
    let t = t.trim_end_matches('.');
    let Some((local, domain)) = t.split_once('@') else {
        return false;
    };
    !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.') && !domain.contains('@')
}

fn is_jwt(t: &str) -> bool {
    t.starts_with("eyJ") && t.matches('.').count() == 2 && t.len() > 20
}

/// A New Zealand bank account number: bank, branch, account, suffix, as
/// `12-3456-7890123-00` (2, 4, 7, 2 or 3 digits).
fn is_account_number(t: &str) -> bool {
    let parts: Vec<&str> = t.trim_end_matches('.').split('-').collect();
    parts.len() == 4
        && parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        && parts[0].len() == 2
        && parts[1].len() == 4
        && (7..=8).contains(&parts[2].len())
        && (2..=3).contains(&parts[3].len())
}

fn has_account_number(input: &str) -> bool {
    input.split(|c: char| !(c.is_ascii_digit() || c == '-')).any(is_account_number)
}

/// The value scrubbed: whole when its key is sensitive, its text masked
/// otherwise. `None` when nothing changes.
pub fn value(key: &str, value: &Value) -> Option<Value> {
    if sensitive_key(key) {
        return Some(Value::from(REDACTED));
    }
    match value {
        Value::String(s) => match text(s.as_str()) {
            Cow::Borrowed(_) => None,
            Cow::Owned(o) => Some(Value::from(o)),
        },
        Value::Array(opentelemetry::Array::String(items)) => {
            let masked: Vec<_> = items.iter().map(|s| text(s.as_str()).into_owned().into()).collect();
            (masked != *items).then_some(Value::Array(opentelemetry::Array::String(masked)))
        }
        _ => None,
    }
}

/// Scrubs a list of attributes in place.
pub fn attributes(list: &mut [KeyValue]) {
    for kv in list.iter_mut() {
        if let Some(v) = value(kv.key.as_str(), &kv.value) {
            kv.value = v;
        }
    }
}

/// Scrubs a whole span: its name, attributes, events and links, and its
/// error description.
pub fn span(data: &mut SpanData) {
    if let Cow::Owned(name) = text(&data.name) {
        data.name = Cow::Owned(name);
    }
    attributes(&mut data.attributes);
    for event in data.events.events.iter_mut() {
        if let Cow::Owned(name) = text(&event.name) {
            event.name = Cow::Owned(name);
        }
        attributes(&mut event.attributes);
    }
    for link in data.links.links.iter_mut() {
        attributes(&mut link.attributes);
    }
    if let opentelemetry::trace::Status::Error { description } = &data.status {
        if let Cow::Owned(d) = text(description) {
            data.status = opentelemetry::trace::Status::error(d);
        }
    }
}

/// A span processor that scrubs each span before handing it on.
#[derive(Debug)]
pub struct ScrubSpans<P> {
    inner: P,
}

impl<P> ScrubSpans<P> {
    pub fn new(inner: P) -> Self {
        Self { inner }
    }
}

impl<P: SpanProcessor> SpanProcessor for ScrubSpans<P> {
    fn on_start(&self, span: &mut Span, cx: &Context) {
        self.inner.on_start(span, cx);
    }

    fn on_end(&self, mut data: SpanData) {
        span(&mut data);
        self.inner.on_end(data);
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.inner.force_flush()
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.inner.shutdown_with_timeout(timeout)
    }

    fn set_resource(&mut self, resource: &Resource) {
        self.inner.set_resource(resource);
    }
}

/// Scrubs a JSON value in place: object members by their key, strings by
/// their text. Used for the log lines, where the members of `span` and
/// `spans` describe the enclosing spans and their `name` is the span's name,
/// not a person's.
pub fn json(value: &mut serde_json::Value) {
    json_in(value, false);
}

fn json_in(value: &mut serde_json::Value, describes_span: bool) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                let span_name = describes_span && k == "name";
                if sensitive_key(k) && !span_name && !v.is_null() {
                    *v = serde_json::Value::String(REDACTED.to_string());
                } else {
                    json_in(v, k == "span" || k == "spans");
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(|v| json_in(v, describes_span)),
        serde_json::Value::String(s) => {
            if let Cow::Owned(o) = text(s) {
                *s = o;
            }
        }
        _ => {}
    }
}
