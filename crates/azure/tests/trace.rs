//! Messages a module sends to Event Hubs carry the sender's trace.

use ic_azure::eventhubs::traced;
use serde_json::json;

/// Purpose: inside a traced span, a message's properties gain the span's `traceparent`; a
/// `traceparent` already there (a ledger event's, the trace of the command that made it) is
/// kept; properties that are not an object, and any message sent outside a span, are left as
/// they are.
/// Why it matters: this is how a consumer of a module's hub (the documents adapter, the
/// reinsurance notices, the underwriting evidence orders) picks up the trace of whatever sent
/// the message; overwriting an event's own trace would tie it to the relay instead of the
/// command that made it.
#[test]
fn messages_carry_the_senders_trace_unless_they_carry_one() {
    let plain = json!({ "event-id": "e-1" });
    assert_eq!(traced(&plain), plain);
    let settings = ic_telemetry::Settings::from_lookup("ic-azure-test", |_| None).unwrap();
    let (subscriber, _telemetry) = ic_telemetry::subscriber(&settings, ic_telemetry::Output::Stderr).unwrap();
    tracing::subscriber::with_default(subscriber, || {
        let span = tracing::info_span!("send notices");
        let _e = span.enter();
        let tp = ic_telemetry::traceparent().unwrap();
        assert_eq!(traced(&plain), json!({ "event-id": "e-1", "traceparent": tp }));
        let carried = json!({ "traceparent": "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01" });
        assert_eq!(traced(&carried), carried);
        assert_eq!(traced(&json!("text")), json!("text"));
    });
}
