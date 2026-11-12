//! Replacements nobody declared: the staff page over the commission
//! module's matches (`docs/distribution.md`, "Replacements nobody declared").
//!
//! When new cover is written on a life soon after cover on that life was
//! given up, the commission module matches the two and pays no initial
//! commission on the matched amount. The writing adviser may dispute the
//! match; a person who manages conduct cases then decides it. This page
//! lists every match and sends that decision, as the person deciding, to
//! the commission module, which checks their authority again by its own
//! policy (`case.manage`, held by the conduct officer).

use serde_json::{json, Value};

use crate::staff::{At, StaffState};
use crate::workbench::actions;

/// The commission module's question listing every match.
pub fn question() -> Value {
    json!("replacement_matches")
}

/// What this person may do here: read the matches with the workbench's
/// view, decide a disputed one as the conduct officer.
pub fn standing(staff: &StaffState, party: &ic_core::PartyId, at: At) -> Value {
    let decide = staff.holds_action(party, actions::APPROVE_CONDUCT, at);
    json!({ "staff": party, "view": decide || staff.holds_action(party, actions::VIEW, at), "decide": decide })
}

/// The decision on a disputed match, in the commission module's own terms,
/// from the page's `{policy, cover, layer, replacement}`.
pub fn decision(body: &Value) -> Result<Value, String> {
    let text = |f: &str| body[f].as_str().filter(|s| !s.is_empty()).ok_or_else(|| format!("a decision names its {f}"));
    let (policy, cover) = (text("policy")?, text("cover")?);
    let layer = body["layer"].as_u64().ok_or("a decision names its layer by number")?;
    let replacement = body["replacement"].as_bool().ok_or("say whether it was a replacement: true or false")?;
    Ok(json!({ "decide_replacement": { "policy": policy, "cover": cover, "layer": layer, "replacement": replacement } }))
}

/// The envelope for one person's decision, sent as them.
pub fn envelope(key: &str, staff: Value, command: Value) -> Value {
    json!({ "key": format!("replacement:{key}"), "actor": { "staff": staff }, "command": command })
}
