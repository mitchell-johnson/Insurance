//! The customer master for the portal's tests: a stand-in that keeps one
//! record per party, notes every call the portal makes, and answers as the
//! master's HTTP service does. A version mismatch is a 412, and `down`
//! makes every call fail as an unreachable master would.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use ic_channels::customers::{Answered, Care, Desk, Master};
use ic_core::PartyId;
use serde_json::{json, Value};

type Asked<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<Answered, String>> + Send + 'a>>;

/// One call as the master saw it.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub what: &'static str,
    pub party: String,
    pub version: Option<String>,
    pub key: Option<String>,
    pub body: Option<Value>,
}

#[derive(Default)]
pub struct MasterDouble {
    pub calls: Mutex<Vec<Call>>,
    records: Mutex<BTreeMap<String, (u64, Value)>>,
    pub down: AtomicBool,
    /// Parties whose requests go to a person first.
    pub vulnerable: Mutex<Vec<String>>,
    /// Each party's own spending hold, as the master answers for it.
    holds: Mutex<BTreeMap<String, Value>>,
    /// Open prompts to review a marker: (review, party, reason).
    pub reviews: Mutex<Vec<(String, String, String)>>,
    /// Markers as staff last set them, by party; `false` once removed.
    pub markers: Mutex<BTreeMap<String, Value>>,
    /// What trained staff asked, with who asked.
    pub care: Mutex<Vec<(String, Care)>>,
    /// The master's decision for (party, caller): `allow` or `route`. Anyone else is refused.
    pub authorities: Mutex<BTreeMap<(String, String), &'static str>>,
    /// Who was asked about: (staff, party, caller).
    pub asked_for: Mutex<Vec<(String, String, String)>>,
    /// Staff and operations work asked of the master, with who for.
    pub desks: Mutex<Vec<(String, Desk)>>,
}

impl MasterDouble {
    /// Add a person to the double, as `with` does for the first.
    pub fn add(&self, party: &str, record: Value) {
        self.records.lock().unwrap().insert(party.to_string(), (1, record));
    }

    pub fn with(party: &str, record: Value) -> Self {
        let double = Self::default();
        double.records.lock().unwrap().insert(party.to_string(), (1, record));
        double
    }

    fn note(&self, call: Call) -> Result<(), String> {
        self.calls.lock().unwrap().push(call);
        if self.down.load(Ordering::SeqCst) {
            return Err("connection refused".to_string());
        }
        Ok(())
    }

    fn found(&self, party: &PartyId) -> Option<(u64, Value)> {
        self.records.lock().unwrap().get(&party.0).cloned()
    }
}

/// A record as the master answers a read: each released fact with its
/// dates, nothing masked or withheld in the double.
fn answer(party: &str, version: u64, record: &Value) -> Value {
    let facts: serde_json::Map<String, Value> = record
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, v)| {
            (
                k.clone(),
                json!({ "value": v, "valid_from": "2020-01-01", "recorded_at": "2020-01-01T00:00:00Z" }),
            )
        })
        .collect();
    json!({ "party": party, "kind": "person", "lifecycle": "active", "version": version, "facts": facts, "masked": {}, "withheld": [], "consents": {} })
}

fn missing() -> Answered {
    (404, json!({ "title": "unknown_party" }))
}

impl Master for MasterDouble {
    fn record(&self, party: &PartyId) -> Asked<'_> {
        let party = party.clone();
        Box::pin(async move {
            self.note(Call {
                what: "record",
                party: party.0.clone(),
                version: None,
                key: None,
                body: None,
            })?;
            Ok(self.found(&party).map_or_else(missing, |(v, r)| (200, answer(&party.0, v, &r))))
        })
    }

    fn command(&self, party: &PartyId, version: Option<String>, key: String, body: Value) -> Asked<'_> {
        let party = party.clone();
        Box::pin(async move {
            self.note(Call {
                what: "command",
                party: party.0.clone(),
                version: version.clone(),
                key: Some(key),
                body: Some(body.clone()),
            })?;
            let mut records = self.records.lock().unwrap();
            let Some((current, record)) = records.get_mut(&party.0) else {
                return Ok(missing());
            };
            if version.is_some_and(|v| v != current.to_string()) {
                return Ok((412, json!({ "title": "version_mismatch" })));
            }
            // A spending hold, as the master keeps it: removal needs a passed identity check.
            let mut holds = self.holds.lock().unwrap();
            let hold = holds
                .entry(party.0.clone())
                .or_insert_with(|| json!({ "held": false, "placed_at": null, "removal": null }));
            match body["command"].as_str() {
                Some("place_spending_hold") => *hold = json!({ "held": true, "placed_at": "2027-02-01T09:00:00Z", "removal": null }),
                Some("request_hold_removal") if body["verification"].as_str().is_none() => {
                    return Ok((422, json!({ "title": "verification_needed" })));
                }
                Some("request_hold_removal") if hold["held"] == true => {
                    hold["removal"] = json!({ "requested_at": "2027-02-02T09:00:00Z", "ends_at": "2027-02-09T09:00:00Z" });
                }
                Some("cancel_hold_removal") => hold["removal"] = Value::Null,
                Some("request_hold_removal") => return Ok((409, json!({ "title": "not_held" }))),
                _ => {}
            }
            drop(holds);
            for fact in body["facts"].as_array().into_iter().flatten() {
                if let Some(name) = fact["field"].as_str() {
                    record[name] = fact["value"].clone();
                }
            }
            *current += 1;
            Ok((200, json!({ "version": current.to_string() })))
        })
    }

    fn export(&self, party: &PartyId) -> Asked<'_> {
        let party = party.clone();
        Box::pin(async move {
            self.note(Call {
                what: "export",
                party: party.0.clone(),
                version: None,
                key: None,
                body: None,
            })?;
            Ok(self
                .found(&party)
                .map_or_else(missing, |(_, r)| (200, json!({ "party": party.0, "fields": r, "history": [] }))))
        })
    }

    fn routing(&self, party: &PartyId) -> Asked<'_> {
        let party = party.clone();
        Box::pin(async move {
            self.note(Call {
                what: "routing",
                party: party.0.clone(),
                version: None,
                key: None,
                body: None,
            })?;
            let to_person = self.vulnerable.lock().unwrap().contains(&party.0);
            let adjustments = if to_person { json!(["plain_language", "more_time"]) } else { json!([]) };
            let mut answer = json!({ "route_to_person": to_person, "adjustments": adjustments, "review_on": null });
            if to_person {
                answer["kind"] = json!("bereavement");
            }
            Ok((200, answer))
        })
    }

    fn spending_hold(&self, party: &PartyId) -> Asked<'_> {
        let party = party.clone();
        Box::pin(async move {
            self.note(Call {
                what: "spending_hold",
                party: party.0.clone(),
                version: None,
                key: None,
                body: None,
            })?;
            let hold = self.holds.lock().unwrap().get(&party.0).cloned();
            Ok((200, hold.unwrap_or_else(|| json!({ "held": false, "placed_at": null, "removal": null }))))
        })
    }

    fn represents(&self, representative: &PartyId, party: &PartyId) -> Asked<'_> {
        let (who, party) = (representative.clone(), party.clone());
        Box::pin(async move {
            if self.down.load(Ordering::SeqCst) {
                return Err("connection refused".to_string());
            }
            if party.0 == "nobody" {
                return Ok((404, json!({ "title": "not_found" })));
            }
            Ok(match self.authorities.lock().unwrap().get(&(party.0, who.0)) {
                Some(decision) => (
                    200,
                    json!({ "decision": decision, "relationship": { "kind": "attorney", "basis": "enduring_power_of_attorney", "scope": ["view"], "valid_from": "2027-10-01", "valid_to": null } }),
                ),
                None => (200, json!({ "decision": "refuse", "relationship": null })),
            })
        })
    }

    fn speaks_for(&self, staff: &PartyId, party: &PartyId, caller: &PartyId) -> Asked<'_> {
        let (staff, party, caller) = (staff.clone(), party.clone(), caller.clone());
        Box::pin(async move {
            self.asked_for.lock().unwrap().push((staff.0, party.0.clone(), caller.0.clone()));
            if self.down.load(Ordering::SeqCst) {
                return Err("connection refused".to_string());
            }
            let decision = self.authorities.lock().unwrap().get(&(party.0, caller.0)).cloned().unwrap_or("refuse");
            Ok((200, json!({ "decision": decision })))
        })
    }

    /// Answers with the request the master would have been sent, so a
    /// test sees the path, the role, the purpose and the key. A party
    /// called `nobody` is not known.
    fn desk(&self, actor: &PartyId, desk: Desk) -> Asked<'_> {
        let actor = actor.clone();
        Box::pin(async move {
            self.desks.lock().unwrap().push((actor.0.clone(), desk.clone()));
            if self.down.load(Ordering::SeqCst) {
                return Err("connection refused".to_string());
            }
            if desk.about().is_some_and(|p| p.0 == "nobody") {
                return Ok((404, json!({ "title": "not_found", "detail": "no such party" })));
            }
            let (method, path, key, body) = desk.request();
            let (role, purpose) = desk.role();
            let mut echo = json!({ "method": method.as_str(), "path": path, "key": key, "body": body, "as": role, "purpose": purpose });
            match &desk {
                // A search scores the double's records as the master would at its simplest: the family
                // name must agree, and a given name or birth date that agrees makes it strong.
                Desk::Search(probe) => {
                    let lower = |v: &Value| v.as_str().map(str::to_lowercase);
                    let results: Vec<Value> = self
                        .records
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|(_, (_, r))| lower(&r["family_name"]).is_some() && lower(&r["family_name"]) == lower(&probe["family_name"]))
                        .map(|(party, (_, r))| {
                            let strong = (probe["given_name"].is_string() && lower(&r["given_name"]) == lower(&probe["given_name"]))
                                || (probe["date_of_birth"].is_string() && r["date_of_birth"] == probe["date_of_birth"]);
                            json!({ "party": party, "score": if strong { 90 } else { 40 }, "strong": strong })
                        })
                        .collect();
                    echo["results"] = json!(results);
                }
                Desk::Read { party, .. } => {
                    if let Some((version, record)) = self.found(party) {
                        let read = answer(&party.0, version, &record);
                        for (k, v) in read.as_object().into_iter().flatten() {
                            echo[k] = v.clone();
                        }
                    }
                }
                _ => {}
            }
            Ok((200, echo))
        })
    }

    fn care(&self, staff: &PartyId, call: Care) -> Asked<'_> {
        let staff = staff.clone();
        Box::pin(async move {
            self.care.lock().unwrap().push((staff.0.clone(), call.clone()));
            if self.down.load(Ordering::SeqCst) {
                return Err("connection refused".to_string());
            }
            Ok(match call {
                Care::Reviews => {
                    let open: Vec<Value> = self
                        .reviews
                        .lock()
                        .unwrap()
                        .iter()
                        .map(|(id, party, reason)| json!({ "id": id, "kind": if reason == "duplicate" { "duplicate" } else { "consider_marker" }, "party": party, "candidates": [], "reason": reason, "status": "open", "created_at": "2027-11-01T00:00:00Z" }))
                        .collect();
                    (200, json!({ "reviews": open }))
                }
                Care::Routing(party) => match self.markers.lock().unwrap().get(&party.0) {
                    Some(m) if m.is_object() => (
                        200,
                        json!({ "route_to_person": true, "kind": m["kind"], "adjustments": m["adjustments"], "review_on": m["review_on"] }),
                    ),
                    _ => (200, json!({ "route_to_person": false, "adjustments": [], "review_on": null })),
                },
                Care::Marker { party, body, .. } => {
                    let value = body["facts"][0]["value"].clone();
                    // As the master does: a kind outside its list is refused.
                    if value.is_object() && value["kind"] == "diagnosis" {
                        return Ok((422, json!({ "title": "invalid", "detail": "kind is not one of the marker kinds" })));
                    }
                    self.markers.lock().unwrap().insert(party.0, value);
                    (200, json!({ "version": 2 }))
                }
                Care::Dismiss(review) => {
                    let mut open = self.reviews.lock().unwrap();
                    let before = open.len();
                    open.retain(|(id, _, _)| *id != review);
                    if open.len() == before {
                        (404, json!({ "title": "not_found", "detail": "no such review" }))
                    } else {
                        (200, json!({ "review": review, "status": "dismissed" }))
                    }
                }
            })
        })
    }
}
