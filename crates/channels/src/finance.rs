//! The finance console: how staff reach the finance ledger.
//!
//! That module (`finance` on the nodes) keeps the general ledger, the
//! month-end close, manual journals and regulatory returns, and takes
//! everything a person does from the caller `finance-console`, with the
//! person and the roles this service states. The roles follow from the
//! staff actions a person's templates hold here.
//!
//! The two modules share no code. `tests/http.rs` runs the console against
//! that module's own state machine.

use std::collections::BTreeMap;

use ic_core::PartyId;
use serde_json::{json, Map, Value};

use crate::staff::{At, StaffState};

/// The finance ledger's name on the nodes.
pub const MODULE: &str = "finance";
/// The caller that module takes people's commands from.
pub const CALLER: &str = "finance-console";
/// A journal line's amount there is in millionths of a minor unit.
const FINE_PER_MINOR: i64 = 1_000_000;

/// Staff actions that open the console and give that module's roles.
pub mod actions {
    pub const VIEW: &str = "finance.view";
    pub const OPERATE: &str = "finance.operate";
    pub const APPROVE: &str = "finance.approve";
    pub const LEAD: &str = "finance.lead";
    pub const ACTUARY: &str = "finance.actuary";
}

const ROLES: [(&str, &str); 4] = [
    (actions::OPERATE, "finance_operator"),
    (actions::APPROVE, "finance_approver"),
    (actions::LEAD, "finance_lead"),
    (actions::ACTUARY, "appointed_actuary"),
];

/// Commands that module takes from its own services and feeds, never from
/// a person at the console.
const SERVICE_ONLY: [&str; 9] = [
    // A chart and a close configuration are reviewed source, proposed by an
    // operator's program; the lead approves them here.
    "propose_chart",
    "propose_close_config",
    "configure",
    "ingest",
    "core_event",
    "record_merkle_root",
    "register_extract",
    "record_conduct_facts",
    "report_aged_items",
];

/// A question's fields: text that must be there, text that may be, and
/// whole numbers that may be.
type Fields = (&'static [&'static str], &'static [&'static str], &'static [&'static str]);

const QUESTIONS: [(&str, Fields); 18] = [
    ("status", (&[], &[], &[])),
    ("work", (&[], &[], &[])),
    ("dashboard", (&["period", "today"], &[], &[])),
    ("checks", (&["period"], &[], &[])),
    ("trial_balance", (&["period"], &["fund"], &[])),
    ("movements", (&["period"], &[], &[])),
    ("sources", (&["period"], &[], &[])),
    ("backdated", (&["period"], &["origin"], &[])),
    ("journal", (&["id"], &[], &[])),
    ("conduct", (&["period"], &["product"], &[])),
    ("return", (&["id"], &[], &[])),
    // One figure on a filed return and what is behind it.
    ("return_line", (&["id", "code"], &[], &["version"])),
    ("tax_reconciliation", (&["period"], &[], &[])),
    // Charts and close configurations waiting for the lead's approval.
    ("proposals", (&[], &[], &[])),
    ("aged_suspense", (&["as_at"], &[], &[])),
    ("extract", (&["kind", "period"], &[], &["offset", "limit"])),
    // How many policies are in each accounting group, and one policy's group
    // (the partition is a number; the module refuses a question without it).
    ("accounting_groups", (&[], &[], &[])),
    ("accounting_group", (&["policy"], &[], &["partition"])),
];

/// The return definitions shipped in this repository. A deployment adds its
/// own with `IC_RETURN_DEFINITIONS`.
pub const SHIPPED_RETURNS: [(&str, &str); 1] = [(
    "returns/insurer_quarterly.star",
    include_str!("../../../config/finance/returns/insurer_quarterly.star"),
)];

/// The return definitions a person may file a return on. They are written
/// as reviewed source, never typed at the console: the person names one and
/// the service sends the definition as it was loaded, with the hash of its
/// source.
#[derive(Debug, Default)]
pub struct Returns {
    held: BTreeMap<(String, u32), Value>,
}

impl Returns {
    /// Load definitions, each a file name and its source, with that
    /// module's own loader and checks.
    pub fn load<'a>(files: impl IntoIterator<Item = (&'a str, &'a str)>) -> Result<Self, String> {
        let mut held = BTreeMap::new();
        for (file, source) in files {
            let d = ic_finance::returns::ReturnDefinition::load(file, source).map_err(|e| format!("{file}: {e}"))?;
            let key = (d.id.clone(), d.version);
            let value = serde_json::to_value(&d).map_err(|e| format!("{file}: {e}"))?;
            if held.get(&key).is_some_and(|had: &Value| had["content_hash"] != value["content_hash"]) {
                return Err(format!(
                    "{file}: return definition {} version {} is already loaded from different source",
                    d.id, d.version
                ));
            }
            held.insert(key, value);
        }
        Ok(Returns { held })
    }

    /// What may be filed on: each definition's name as the console sends it,
    /// without its lines' mappings.
    pub fn list(&self) -> Vec<Value> {
        self.held
            .iter()
            .map(|((id, version), d)| {
                json!({ "name": format!("{id}@{version}"), "id": id, "version": version, "title": d["title"], "currency": d["currency"],
                    "effective_from": d["effective_from"], "effective_to": d["effective_to"], "hash": d["content_hash"],
                    "lines": d["lines"].as_array().map(|lines| lines.iter().map(|l| json!({ "code": l["code"], "label": l["label"] })).collect::<Vec<_>>()) })
            })
            .collect()
    }

    /// Put the named definition into a command that files or restates a
    /// return. A definition given any other way is refused.
    fn fill(&self, command: &mut Value) -> Result<(), String> {
        for name in ["file_return", "restate_return"] {
            let Some(fields) = command.get_mut(name).and_then(Value::as_object_mut) else {
                continue;
            };
            let found = fields
                .get("definition")
                .and_then(Value::as_str)
                .and_then(|named| named.rsplit_once('@'))
                .and_then(|(id, version)| self.held.get(&(id.to_string(), version.parse().ok()?)))
                .ok_or("name the return definition as it is listed, such as insurer-quarterly@1: a definition is not written at the console")?;
            fields.insert("definition".to_string(), found.clone());
        }
        Ok(())
    }
}

/// What a member of staff may do in the console: whether they may open it,
/// and the roles they act in.
pub fn standing(staff: &StaffState, party: &PartyId, at: At) -> Value {
    let roles: Vec<&str> = ROLES
        .iter()
        .filter(|(action, _)| staff.holds_action(party, action, at))
        .map(|(_, role)| *role)
        .collect();
    json!({ "view": !roles.is_empty() || staff.holds_action(party, actions::VIEW, at), "roles": roles })
}

/// A question for the finance ledger, from its name and the fields the
/// page sent. Only the console's questions pass, each with its own fields.
pub fn question(name: &str, fields: &BTreeMap<String, String>) -> Result<Value, String> {
    let (_, (needed, optional, numbers)) = QUESTIONS
        .iter()
        .find(|(q, _)| *q == name)
        .ok_or_else(|| format!("{name} is not a question the console asks"))?;
    let given = |field: &str| fields.get(field).map(|v| v.trim()).filter(|v| !v.is_empty());
    let mut out = Map::new();
    out.insert("q".to_string(), json!(name));
    for field in *needed {
        out.insert((*field).to_string(), json!(given(field).ok_or_else(|| format!("{name} needs {field}"))?));
    }
    for field in *optional {
        if let Some(value) = given(field) {
            out.insert((*field).to_string(), json!(value));
        }
    }
    for field in *numbers {
        if let Some(value) = given(field) {
            let number: u64 = value.parse().map_err(|_| format!("{field} is a whole number"))?;
            out.insert((*field).to_string(), json!(number));
        }
    }
    Ok(Value::Object(out))
}

/// A journal line may give its amount as `signed_minor`, in minor units:
/// debits positive, credits negative. That module counts in millionths of
/// a minor unit, which a browser cannot hold exactly, so the conversion is
/// made here. A cut-over total may likewise come as `total_minor`.
fn exact_lines(command: &mut Value) -> Result<(), String> {
    // A cut-over total is on the trial balance's scale, so it comes as minor units too.
    if let Some(fields) = command.pointer_mut("/record_cutover_total").and_then(Value::as_object_mut) {
        if let Some(minor) = fields.remove("total_minor") {
            let fine = minor
                .as_i64()
                .and_then(|m| m.checked_mul(FINE_PER_MINOR))
                .ok_or("a cut-over total_minor is a whole number of minor units, within range")?;
            fields.insert("total".to_string(), json!(fine));
        }
    }
    // By pointer, not by index: indexing a command of another name would add the key.
    let Some(lines) = command.pointer_mut("/propose_journal/journal/lines").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    for line in lines {
        let Some(fields) = line.as_object_mut() else { continue };
        if let Some(minor) = fields.remove("signed_minor") {
            let fine = minor
                .as_i64()
                .and_then(|m| m.checked_mul(FINE_PER_MINOR))
                .ok_or("a journal line's signed_minor is a whole number of minor units, within range")?;
            fields.insert("signed_fine".to_string(), json!(fine));
        }
    }
    Ok(())
}

/// Who may price a proposed rate table against the book.
pub const DRY_RUN_ROLES: [&str; 2] = ["finance_lead", "appointed_actuary"];

/// The ledger's dry-run question from what the page sent: the date, the
/// cover codes the table rates, how it rates them, and the two versions as
/// their reviewed Starlark sources (the files under `products/rates`),
/// which are loaded here exactly as the product loader reads them. A table
/// is never typed in as JSON. `loadings` are the core's rows for the date.
pub fn dry_run(body: &Value, loadings: Vec<Value>) -> Result<Value, String> {
    let as_at = body["as_at"]
        .as_str()
        .and_then(|d| d.parse::<chrono::NaiveDate>().ok())
        .ok_or("give the date to price the book on")?;
    let covers: Vec<&str> = body["covers"]
        .as_array()
        .map(|c| c.iter().filter_map(Value::as_str).filter(|c| !c.trim().is_empty()).collect())
        .unwrap_or_default();
    if covers.is_empty() {
        return Err("name at least one cover code the table rates".to_string());
    }
    let table = |which: &str| -> Result<Value, String> {
        let source = body[which]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or(format!("attach the {which} version's .star file"))?;
        let table = ic_product::load_rate_table(&format!("{which}.star"), source).map_err(|e| format!("the {which} version does not load: {e}"))?;
        serde_json::to_value(table).map_err(|e| e.to_string())
    };
    Ok(serde_json::json!({ "q": "table_dry_run", "as_at": as_at, "request": {
        "covers": covers,
        "per_life": body["per_life"] == true,
        "age_at_start": body["age_at_start"] == true,
        "current": table("current")?,
        "proposed": table("proposed")?,
        "loadings": loadings,
    } }))
}

/// The envelope that carries one person's command to the finance ledger:
/// who they are and the roles they hold. Commands that module takes only
/// from its own services are refused.
pub fn envelope(key: &str, actor: &PartyId, roles: &Value, returns: &Returns, mut command: Value) -> Result<Value, String> {
    let name = match &command {
        Value::Object(fields) if fields.len() == 1 => fields.keys().next().cloned(),
        // A command with no fields may be sent by its name alone.
        Value::String(name) => Some(name.clone()),
        _ => None,
    }
    .ok_or("a command is one name with its fields")?;
    if SERVICE_ONLY.contains(&name.as_str()) {
        return Err(format!("{name} is not sent by a person"));
    }
    exact_lines(&mut command)?;
    returns.fill(&mut command)?;
    Ok(json!({ "key": key, "caller": CALLER, "actor": actor, "roles": roles, "command": command }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Purpose: return definitions load with the finance module's own
    /// loader, and a file that does not pass it, or a second source for a
    /// name and version already held, stops the load with the file named.
    /// Why it matters: a return is filed on the definition the service
    /// holds. Two sources under one name would let a return be filed on a
    /// mapping nobody reviewed, with nothing on screen to show it.
    #[test]
    fn return_definitions_are_one_reviewed_source_each() {
        let (file, shipped) = SHIPPED_RETURNS[0];
        let held = Returns::load(SHIPPED_RETURNS).unwrap();
        assert_eq!(held.list()[0]["name"], "insurer-quarterly@1");
        assert_eq!(Returns::load([(file, shipped), ("again.star", shipped)]).unwrap().list().len(), 1);
        let changed = shipped.replace("Quarterly insurer return", "Another title");
        assert_eq!(
            Returns::load([(file, shipped), ("changed.star", changed.as_str())]).unwrap_err(),
            "changed.star: return definition insurer-quarterly version 1 is already loaded from different source"
        );
        assert!(Returns::load([("bad.star", "RETURN = {")]).unwrap_err().starts_with("bad.star: "));
        assert!(Returns::default().list().is_empty());
    }
}
