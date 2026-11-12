//! Questions the channel state answers. Reads never change state: the
//! portal service asks the module's leader, which answers from memory.

use std::collections::BTreeSet;

use ic_core::{EventRecord, PartyId, PolicyId};
use ic_raft::{hosted_reading, ModuleSpec, ModuleState};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::classes::DataClass;
use crate::directory::{Actor, AdviserStanding};
use crate::identity::Assurance;
use crate::staff::{AccessRequest, At, Purpose};
use crate::state::{ChannelState, Command, Envelope, EVENT_RELAY};
use crate::workbench;

/// The module's name on the module host. It names its directory on disk
/// and its routes, so it never changes.
pub const MODULE: &str = "channels";
/// Relationship and entitlement data older than this is stale.
pub const STALE_AFTER_SECS: i64 = 120;

/// Proof that the caller holds an enrolled device's key, from the request's headers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceProof {
    pub device: String,
    /// `timestamp.signature`.
    pub proof: String,
    pub method: String,
    pub path: String,
    /// SHA-256 of the request body, in hex.
    pub body_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "q", rename_all = "snake_case")]
pub enum Question {
    /// Who a sign-in is, whether its session still stands, and how sure we are.
    Session {
        subject: String,
        /// When the token was issued.
        issued: At,
        at: At,
        #[serde(default)]
        device: Option<DeviceProof>,
    },
    /// What a device at `have` needs.
    Sync {
        party: PartyId,
        have: Option<u64>,
    },
    /// Every policy a party has a relationship with.
    Policies {
        party: PartyId,
        at: At,
    },
    /// What a viewer may see of one policy.
    Policy {
        viewer: PartyId,
        policy: PolicyId,
        at: At,
    },
    /// An adviser's engagements, standing and book.
    Book {
        adviser: PartyId,
        at: At,
    },
    /// A party's own requests.
    Requests {
        party: PartyId,
    },
    /// Requests accepted and waiting for a module.
    Pending {
        module: String,
    },
    Devices {
        party: PartyId,
    },
    /// A payee's own held money and the reasons.
    HeldBalance {
        party: PartyId,
    },
    /// Answer the four questions for a staff action.
    Authorise {
        request: AccessRequest,
    },
    OwnAccess {
        staff: PartyId,
        at: At,
    },
    /// A policy as a member of staff may see it, shaped to the classes they hold.
    StaffPolicy {
        staff: PartyId,
        policy: PolicyId,
        scopes: Vec<String>,
        purpose: Option<Purpose>,
        at: At,
    },
    /// Staff access as an administrator sees it: grants waiting, alignment with the external source.
    StaffAdmin {
        staff: PartyId,
        at: At,
    },
    /// The workbench's list, or one party's whole position.
    Workbench {
        staff: PartyId,
        party: Option<PartyId>,
        at: At,
    },
    ProofOfDebt {
        staff: PartyId,
        party: PartyId,
        at: At,
    },
    /// Actions approved on the workbench and waiting for `module` to post
    /// them, each with the people who prepared and approved it as the
    /// commission module describes a member of staff.
    WorkbenchApproved {
        module: String,
        at: At,
    },
    /// How current the module is, for health checks and the operations console.
    Status,
}

fn fresh(state: &ChannelState, at: At) -> bool {
    state.data.directory.fresh(at, STALE_AFTER_SECS)
}

fn standing_json(standing: &AdviserStanding) -> Value {
    serde_json::to_value(standing).unwrap_or(Value::Null)
}

/// Every policy a party is related to, from each side of the directory.
fn related(state: &ChannelState, party: &PartyId) -> BTreeSet<PolicyId> {
    let d = &state.data;
    let mut out: BTreeSet<PolicyId> = d.audiences.iter().filter(|(_, a)| a.contains(party)).map(|(p, _)| p.clone()).collect();
    out.extend(d.directory.policyholders.iter().filter(|(_, h)| *h == party).map(|(p, _)| p.clone()));
    out.extend(d.directory.adviser_policies.get(party).into_iter().flatten().cloned());
    out.extend(d.directory.admin_policies.get(party).into_iter().flatten().cloned());
    out
}

fn policy_summary(state: &ChannelState, viewer: &PartyId, policy: &PolicyId, at: At) -> Option<Value> {
    let on = state.today(at);
    let relationships = state.data.directory.relationships(&state.data.view, viewer, policy, on);
    if relationships.is_empty() {
        return None;
    }
    let entry = state.data.view.policy(policy)?;
    Some(json!({ "policy": policy, "number": entry.number, "product": entry.product_code, "status": entry.status, "relationships": relationships }))
}

/// A member of staff as the commission module's own authorisation wants
/// them described: its roles, and how much they may decide. The roles follow
/// from the workbench actions the person's templates hold here, and the one
/// held balance limit stands for each kind of amount there.
fn commission_staff(state: &ChannelState, party: &PartyId, at: At, purpose: &str) -> Value {
    let staff = &state.data.staff;
    let profile = staff.own_access(party, at)["profile"].clone();
    let limit = profile["limits"][workbench::LIMIT].as_i64().unwrap_or(0);
    let amounts = |value: i64| json!({ "release": value, "offset": value, "write_off": value, "split": value, "dividend": value });
    let templates: Vec<Value> = staff
        .templates_of(party, at)
        .into_iter()
        .filter_map(|t| {
            let roles: Vec<&str> = [
                (workbench::actions::PREPARE, "counterparty_finance"),
                (workbench::actions::APPROVE, "counterparty_finance_approver"),
                (workbench::actions::APPROVE_LEGAL, "legal"),
            ]
            .into_iter()
            .filter(|(action, _)| t.actions.contains(*action))
            .map(|(_, role)| role)
            .collect();
            (!roles.is_empty()).then(|| {
                json!({
                    "name": t.id,
                    "roles": roles,
                    "default_profile": { "version": profile["version"].as_u64().unwrap_or(0), "limits": amounts(limit) },
                    "ceiling": amounts(t.ceilings.get(workbench::LIMIT).copied().unwrap_or(0)),
                })
            })
        })
        .collect();
    json!({ "id": party, "templates": templates, "grants": [], "paths": ["*"], "purpose": purpose })
}

fn staff_check(
    state: &ChannelState,
    staff: &PartyId,
    action: &str,
    class: DataClass,
    scopes: Vec<String>,
    record: String,
    purpose: Option<Purpose>,
    at: At,
) -> (AccessRequest, crate::staff::Decision) {
    let request = AccessRequest {
        staff: staff.clone(),
        action: action.to_string(),
        class,
        scopes,
        record: Some(record),
        amount: None,
        purpose,
        decides: false,
        conflict_subject: None,
        maker: None,
        fresh: state.data.staff.external_available,
        at,
    };
    let decision = state.data.staff.authorise(state.staff_policy(), &request);
    (request, decision)
}

/// Answer one question. An `Err` is a question that could not be understood.
pub fn answer(state: &ChannelState, question: Value) -> Result<Value, String> {
    let question: Question = serde_json::from_value(question).map_err(|e| format!("not a question of this module: {e}"))?;
    Ok(ask(state, question))
}

/// Answer a typed question.
pub fn ask(state: &ChannelState, question: Question) -> Value {
    let d = &state.data;
    match question {
        Question::Session { subject, issued, at, device } => {
            let Some(party) = d.directory.party_of_subject(&subject) else {
                return json!({ "known": false });
            };
            let valid = d.identity.session_valid(party, issued);
            let proved = device.as_ref().is_some_and(|p| {
                d.identity
                    .check_proof_hashed(party, &p.device, &p.proof, at, &p.method, &p.path, &p.body_sha256)
            });
            // A device that was enrolled to this person and no longer is must wipe what it holds.
            let revoked = device
                .as_ref()
                .and_then(|p| d.identity.devices.get(&p.device))
                .is_some_and(|known| &known.party == party && known.revoked.is_some());
            json!({
                "known": true,
                "party": party,
                "valid": valid,
                "device_revoked": revoked,
                "assurance": if proved { Assurance::Device } else { Assurance::Token },
                "device": device.filter(|_| proved).map(|p| p.device),
                "staff": !d.staff.templates_of(party, at).is_empty(),
                "fresh": fresh(state, at),
            })
        }
        Question::Sync { party, have } => match d.feeds.get(&party) {
            Some(feed) => json!(feed.sync(have)),
            None => json!(crate::feed::PartyFeed::default().sync(have)),
        },
        Question::Policies { party, at } => {
            json!(related(state, &party)
                .iter()
                .filter_map(|p| policy_summary(state, &party, p, at))
                .collect::<Vec<_>>())
        }
        Question::Policy { viewer, policy, at } => {
            let is_fresh = fresh(state, at);
            let (relationships, rows) = state.visible_rows(&viewer, &policy, state.today(at), is_fresh);
            if relationships.is_empty() {
                // The same answer as for a policy that does not exist.
                return Value::Null;
            }
            json!({
                "policy": policy,
                "relationships": relationships,
                "fresh": is_fresh,
                "rows": rows.into_iter().map(|r| json!({ "key": r.key, "kind": r.kind, "subject": r.subject, "body": r.body })).collect::<Vec<_>>(),
            })
        }
        Question::Book { adviser, at } => {
            let on = state.today(at);
            let engagements: Vec<Value> = d
                .directory
                .engagements
                .values()
                .filter(|e| e.adviser == adviser)
                .map(|e| json!({ "engagement": e.id, "fap": e.fap, "from": e.from, "to": e.to, "standing": standing_json(&d.directory.standing(&e.id, on)) }))
                .collect();
            let policies: Vec<Value> = d
                .directory
                .adviser_policies
                .get(&adviser)
                .into_iter()
                .flatten()
                .filter_map(|policy| {
                    let authority = d.directory.authorities.get(policy)?.get(&adviser)?;
                    let standing = d.directory.standing(&authority.engagement, on);
                    let mut summary = policy_summary(state, &adviser, policy, at)?;
                    summary["engagement"] = json!(authority.engagement);
                    summary["can_transact"] = json!(standing == AdviserStanding::Active);
                    Some(summary)
                })
                .collect();
            let read_only = engagements.iter().any(|e| e["standing"]["state"] == "read_only");
            json!({
                "adviser": adviser,
                "engagements": engagements,
                "policies": policies,
                // Shown as a banner: the book is visible and nothing can be quoted, submitted or changed.
                "read_only": read_only,
                "held": d.workbench.own_held(&adviser),
            })
        }
        Question::Requests { party } => json!(d.requests.values().filter(|r| r.party == party).collect::<Vec<_>>()),
        Question::Pending { module } => json!(state.pending_for(&module)),
        Question::Devices { party } => json!(d
            .identity
            .by_party
            .get(&party)
            .into_iter()
            .flatten()
            .filter_map(|id| d.identity.devices.get(id))
            .map(|v| json!({ "id": v.id, "hardware_backed": v.hardware_backed, "enrolled": v.enrolled, "revoked": v.revoked }))
            .collect::<Vec<_>>()),
        Question::HeldBalance { party } => d.workbench.own_held(&party),
        Question::Authorise { request } => {
            let decision = d.staff.authorise(state.staff_policy(), &request);
            json!({ "decision": decision, "explanation": decision.denied.map(crate::staff::Denial::explain) })
        }
        Question::OwnAccess { staff, at } => d.staff.own_access(&staff, at),
        Question::StaffPolicy {
            staff,
            policy,
            scopes,
            purpose,
            at,
        } => {
            let (request, mut decision) = staff_check(
                state,
                &staff,
                "policy.view",
                DataClass::Personal,
                scopes,
                format!("policy:{policy}"),
                purpose,
                at,
            );
            // Nobody handles their own policy or that of someone they are
            // connected to, whatever their role and path say.
            let on = state.today(at);
            let related = |who: &PartyId| !d.directory.relationships(&d.view, who, &policy, on).is_empty();
            let is_staff = decision.denied != Some(crate::staff::Denial::NotStaff);
            if is_staff && (related(&staff) || d.staff.connections.get(&staff).is_some_and(|c| c.iter().any(related))) {
                decision = crate::staff::Decision {
                    allowed: false,
                    denied: Some(crate::staff::Denial::Personal),
                    ..decision
                };
            }
            if !decision.allowed {
                return json!({ "request": request, "decision": decision, "explanation": decision.denied.map(crate::staff::Denial::explain) });
            }
            let sensitive = decision.classes.contains(&DataClass::InsuranceSensitive);
            let rows = state.rows(&policy, state.today(at), None);
            let withheld = rows.values().filter(|r| r.sensitive && !sensitive).count();
            json!({
                "request": request,
                "decision": decision,
                "found": state.data.view.policy(&policy).is_some(),
                "rows": rows.values().filter(|r| !r.sensitive || sensitive)
                    .map(|r| json!({ "key": r.key, "kind": r.kind, "subject": r.subject, "sensitive": r.sensitive, "body": r.body })).collect::<Vec<_>>(),
                // The person is told that something was withheld, never what.
                "withheld": withheld,
                "advisers": d.directory.advisers_on(&policy, state.today(at)).into_iter()
                    .map(|(adviser, standing)| json!({ "adviser": adviser, "standing": standing_json(&standing) })).collect::<Vec<_>>(),
            })
        }
        Question::StaffAdmin { staff, at } => {
            if !d.staff.holds_action(&staff, crate::staff::actions::MANAGE_STAFF, at) && !d.staff.holds_action(&staff, crate::staff::actions::APPROVE_GRANT, at)
            {
                return json!({ "allowed": false });
            }
            json!({
                "allowed": true,
                "grants": d.staff.grants.values().collect::<Vec<_>>(),
                "members": d.staff.members.values().collect::<Vec<_>>(),
                "templates": d.staff.templates.values().collect::<Vec<_>>(),
                "alignment": d.staff.alignment(),
                "external_source_available": d.staff.external_available,
            })
        }
        Question::Workbench { staff, party, at } => {
            let record = party.as_ref().map_or_else(|| "counterparty:*".to_string(), |p| format!("counterparty:{p}"));
            let (request, decision) = staff_check(
                state,
                &staff,
                workbench::actions::VIEW,
                DataClass::Financial,
                vec![],
                record,
                Some(Purpose {
                    kind: crate::staff::PurposeKind::Task,
                    reference: "workbench".into(),
                }),
                at,
            );
            if !decision.allowed {
                return json!({ "request": request, "decision": decision, "explanation": decision.denied.map(crate::staff::Denial::explain) });
            }
            match party {
                Some(party) => json!({ "request": request, "decision": decision, "position": d.workbench.position(&party) }),
                None => json!({
                    "request": request,
                    "decision": decision,
                    "control": d.workbench.control(state.today(at)),
                    "settings": d.workbench.settings,
                    "parties": d.workbench.positions.iter().map(|(party, p)| json!({
                        "party": party, "status": p.current_status(), "held": p.held(), "net": p.net(),
                        "open_actions": d.workbench.proposals.values().filter(|a| &a.party == party && matches!(a.state, workbench::ProposalState::AwaitingApproval | workbench::ProposalState::Approved)).count(),
                    })).collect::<Vec<_>>(),
                }),
            }
        }
        Question::ProofOfDebt { staff, party, at } => {
            let (request, decision) = staff_check(
                state,
                &staff,
                workbench::actions::VIEW,
                DataClass::Financial,
                vec![],
                format!("counterparty:{party}"),
                Some(Purpose {
                    kind: crate::staff::PurposeKind::Task,
                    reference: "proof-of-debt".into(),
                }),
                at,
            );
            if !decision.allowed {
                return json!({ "request": request, "decision": decision, "explanation": decision.denied.map(crate::staff::Denial::explain) });
            }
            json!({ "request": request, "decision": decision, "extract": d.workbench.proof_of_debt(&party, state.today(at)) })
        }
        Question::WorkbenchApproved { module, at } => {
            let waiting: Vec<Value> = d
                .workbench
                .proposals
                .values()
                .filter(|p| p.module == module && p.state == workbench::ProposalState::Approved)
                .map(|p| {
                    let people: serde_json::Map<String, Value> = std::iter::once(&p.maker)
                        .chain(p.approvals.iter().map(|a| &a.by))
                        .map(|who| (who.to_string(), commission_staff(state, who, at, &p.reason_code)))
                        .collect();
                    json!({ "proposal": p, "people": people })
                })
                .collect();
            json!(waiting)
        }
        Question::Status => json!({
            "business_date": d.today,
            "heard": d.directory.heard,
            "policies": d.view.directory().count(),
            "feeds": d.feeds.len(),
            "requests_pending": d.requests.values().filter(|r| r.status == crate::state::RequestStatus::Pending).count(),
            "state_hash": state.state_hash(),
        }),
    }
}

/// One of the core's events as the module's own command. The key is the
/// event's ID, so an event offered twice is taken in once.
pub fn react(record: &EventRecord) -> Vec<Envelope> {
    vec![Envelope {
        key: format!("event:{}", record.id),
        // An event carries no wall-clock time. Freshness is kept by the
        // gateway's heartbeat, and dates come from the event's business date.
        at: 0,
        actor: Actor::Module(EVENT_RELAY.to_string()),
        command: Command::CoreEvents { records: vec![record.clone()] },
    }]
}

fn make() -> Box<dyn ModuleState> {
    // The shipped policies are compiled in and parsed by the test suite.
    let state = match ChannelState::shipped() {
        Ok(state) => state,
        Err(e) => unreachable!("the shipped policies do not load: {e}"),
    };
    hosted_reading(state, answer, react)
}

/// The channel module, for a ledger node to host.
pub fn module_spec() -> Result<ModuleSpec, String> {
    Ok(ModuleSpec::new(MODULE, make)?.reading_events())
}
