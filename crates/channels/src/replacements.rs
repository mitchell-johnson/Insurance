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

/// The matches on business an adviser wrote, under any of `engagements`,
/// as they may see them: their own layer, the amount, the reason and
/// where it stands. What it replaced is left out: that cover may be
/// another adviser's business.
pub fn for_adviser(matches: &Value, engagements: &Value) -> Value {
    let theirs = |m: &&Value| {
        m["writing"]
            .as_array()
            .is_some_and(|w| w.iter().any(|e| engagements.as_array().is_some_and(|mine| mine.contains(e))))
    };
    let rows: Vec<Value> = matches
        .as_array()
        .into_iter()
        .flatten()
        .filter(theirs)
        .map(|m| {
            json!({
                "layer": m["layer"], "policy": m["policy"], "matched_minor": m["matched_minor"],
                "sum_insured_minor": m["sum_insured_minor"], "reason": m["reason"], "on": m["on"],
                "disputed": m["disputed"], "overturned": m["overturned"],
            })
        })
        .collect();
    Value::Array(rows)
}

/// The request kind new business takes an adviser's dispute from. It is
/// raised only through the adviser's own route, which checks they wrote
/// the layer; the general request route refuses it.
pub const DISPUTE: &str = "dispute_replacement";
/// The module that carries a dispute to the commission module.
pub const DISPUTE_MODULE: &str = "new-business";

/// An adviser's dispute of one of their matches, as the request new
/// business acts on: the layer and the adviser's reason. Refused unless
/// the layer is among `theirs` and stands undisputed.
pub fn dispute(body: &Value, theirs: &Value) -> Result<(String, Value), String> {
    let layer = body["layer"].as_str().filter(|s| !s.is_empty()).ok_or("a dispute names the layer")?;
    let reason = body["reason"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("say why it was not a replacement")?;
    let found = theirs
        .as_array()
        .into_iter()
        .flatten()
        .find(|m| m["layer"] == layer)
        .ok_or("that layer is not one of your matches")?;
    if !found["disputed"].is_null() || found["overturned"] == true {
        return Err("that match is already disputed or decided".to_string());
    }
    let (policy, rest) = layer.split_once('/').ok_or("that layer is not one of your matches")?;
    let (cover, number) = rest.rsplit_once('/').ok_or("that layer is not one of your matches")?;
    let number: u32 = number.parse().map_err(|_| "that layer is not one of your matches")?;
    Ok((
        format!("dispute:{layer}"),
        json!({ "policy": policy, "cover": cover, "layer": number, "reason": reason.chars().take(500).collect::<String>() }),
    ))
}
