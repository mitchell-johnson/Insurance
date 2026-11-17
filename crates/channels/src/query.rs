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
    /// Whether a member of staff may download documents.
    DocumentsStaff {
        staff: PartyId,
        at: At,
    },
    /// Whether this member of staff is trained to see and review vulnerability markers.
    CareStaff {
        staff: PartyId,
        at: At,
    },
    /// How many administrators an employer's scheme has.
    SchemeAdmins {
        policy: PolicyId,
    },
    /// Who administers an employer's scheme, and which kinds of change
    /// need a second administrator.
    SchemeAdministration {
        policy: PolicyId,
    },
    /// The policy a person named, by its number or its reference, with
    /// both: `{policy, number}`, or null when no policy is called that.
    PolicyNamed {
        text: String,
    },
    /// Whether this party is the life on an underwriting proposal, and the
    /// enrolled key of the device they proved, if it stands.
    DisclosureSigner {
        party: PartyId,
        device: Option<String>,
        proposal: String,
        life: String,
    },
    /// The proposals this party is a life on, as underwriting published them:
    /// where each stands and any offer of terms waiting for their answer.
    MyProposals {
        party: PartyId,
    },
    /// What the life's progress row holds for a requote: the premium
    /// accepted and the policy the proposal would issue as. For the portal
    /// to price, never shown whole.
    Requote {
        party: PartyId,
        proposal: String,
        life: String,
    },
    /// The offer of terms waiting for a life's answer, as the life's
    /// progress row holds it: the terms, whether they may ask us to look
    /// again, and the policy it would issue as. For the portal to price and
    /// show the life; null if the party is not that life or nothing waits.
    Offer {
        party: PartyId,
        proposal: String,
        life: String,
    },
    /// What a life is shown before they sign that they understand their new
    /// cover replaces cover they hold: what stays, what is added, what is
    /// lost and what it costs, from the proposal underwriting published and
    /// the policy as the core has it. Null if the party is not that life.
    ReplacementStatement {
        party: PartyId,
        proposal: String,
        life: String,
    },
    /// The engagements an adviser has held, current or not, by ID.
    AdviserEngagements {
        party: PartyId,
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
    /// What a representative (an attorney, a guardian) sees of a policy of
    /// the person they act for: that person's own view, without the rows
    /// marked sensitive. The authority is checked by the caller.
    PolicyFor {
        represented: PartyId,
        policy: PolicyId,
        at: At,
    },
    /// An adviser's engagements, standing and book.
    /// What an advice provider or a group sees: each adviser engaged under
    /// it today, their standing, and the policies they service, each as
    /// that adviser sees it. Never a policy's health facts or a claim.
    Practice {
        party: PartyId,
        at: At,
    },
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
    /// The engagement an adviser holds authority on a policy under, if any.
    AdviserEngagement {
        policy: PolicyId,
        adviser: PartyId,
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
    /// Whether a member of staff may take one action on one record, by the
    /// four questions, with the request to write to the access log. Used
    /// where the work itself is done by another service (the core's new
    /// business, the customer master). `about` is the person the record is
    /// about, where there is one: nobody works their own record or that of
    /// someone they are connected to.
    StaffAct {
        staff: PartyId,
        action: String,
        class: DataClass,
        record: String,
        #[serde(default)]
        about: Option<PartyId>,
        #[serde(default)]
        scopes: Vec<String>,
        #[serde(default)]
        purpose: Option<Purpose>,
        at: At,
    },
    /// Staff access as an administrator sees it: grants waiting, alignment with the external source.
    StaffAdmin {
        staff: PartyId,
        at: At,
    },
    /// What a member of staff may do at the complaints workbench, and the
    /// same for one other person a command of theirs names.
    ComplaintsStaff {
        staff: PartyId,
        about: Option<PartyId>,
        at: At,
    },
    /// What a member of staff may do in the finance console.
    FinanceStaff {
        staff: PartyId,
        at: At,
    },
    /// What a member of staff may do in the reinsurance exceptions console,
    /// and the abandonments waiting for a signature.
    ReinsuranceStaff {
        staff: PartyId,
        at: At,
    },
    /// Whether a member of staff may open the payments console, and the
    /// roles they hold in the payments module.
    PaymentsStaff {
        staff: PartyId,
        at: At,
    },
    /// Whether a member of staff may open the group console, and act there.
    GroupStaff {
        staff: PartyId,
        at: At,
    },
    /// Whether a member of staff may open the underwriting console, as a
    /// senior or not, with the non-disclosure remedies waiting for a second
    /// senior.
    UnderwritingStaff {
        staff: PartyId,
        at: At,
    },
    /// What a member of staff may do with replacement matches, and how the
    /// commission module should see them when they decide one.
    ReplacementStaff {
        staff: PartyId,
        at: At,
        purpose: String,
    },
    /// What a member of staff may do in the commission console, with the
    /// commission module's description of them.
    CommissionStaff {
        staff: PartyId,
        at: At,
        purpose: String,
    },
    /// Whether a member of staff may open the claims console.
    ClaimsStaff {
        staff: PartyId,
        at: At,
    },
    /// Whether a member of staff may carry out or decline a customer's
    /// request that the customer master sent to a person first.
    ServicingStaff {
        staff: PartyId,
        at: At,
    },
    /// What a member of staff may do in the collections console.
    CollectionsStaff {
        staff: PartyId,
        at: At,
    },
    /// What a member of staff may do in the documents console.
    CorrespondenceStaff {
        staff: PartyId,
        at: At,
    },
    /// What a member of staff may do in the unit pricing console.
    PricingStaff {
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
    /// Policies a search found: those numbered as typed, and those each
    /// person found in the customer master owns or is insured on.
    /// References, products and standing only (`crate::find`).
    StaffFind {
        number: Option<String>,
        parties: Vec<PartyId>,
    },
    /// Which of these policies are closed to a member of staff: their own,
    /// or one of someone they declared a personal connection with.
    ClosedTo {
        staff: PartyId,
        policies: Vec<PolicyId>,
        at: At,
    },
    /// The people on the staff directory who may be picked for work: who
    /// holds `action` today, or everyone current when none is named. A
    /// person is never offered work they could not take.
    Colleagues {
        action: Option<String>,
        at: At,
    },
    /// Changes colleagues entered for customers that wait for a second
    /// member of staff, other than the asker's own, oldest first.
    SecondApprovals {
        staff: PartyId,
        at: At,
    },
    /// How current the module is, for health checks and the operations console.
    Status,
    /// The books' business date: the latest the core has reported, if any.
    BusinessDate,
    /// What a sign-in may reach, for the tools it is offered: whether it is
    /// an adviser or advice provider, a scheme administrator, and the staff
    /// actions its templates, break-glass and one-off grants hold today.
    Reach {
        party: PartyId,
        at: At,
    },
}

/// A policy by its ID, or by its number typed in any case.
fn known_policy(state: &ChannelState, policy: PolicyId) -> PolicyId {
    if state.data.view.policy(&policy).is_some() {
        return policy;
    }
    state
        .data
        .view
        .directory()
        .find(|e| e.number.eq_ignore_ascii_case(&policy.0))
        .map_or(policy, |e| e.policy.clone())
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
    out.extend(d.directory.policyholders.iter().filter(|(_, h)| h.contains(party)).map(|(p, _)| p.clone()));
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
    Some(
        json!({ "policy": policy, "number": entry.number, "product": entry.product_code, "status": entry.status, "ended": crate::state::ended(entry.status), "relationships": relationships }),
    )
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
                (workbench::actions::APPROVE_CONDUCT, "conduct_officer"),
            ]
            .into_iter()
            .chain(crate::commission::ROLES)
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
        Question::PolicyFor { represented, policy, at } => {
            let is_fresh = fresh(state, at);
            let (relationships, rows) = state.visible_rows(&represented, &policy, state.today(at), is_fresh);
            if relationships.is_empty() {
                return Value::Null;
            }
            let withheld = rows.iter().filter(|r| r.sensitive).count();
            json!({
                "policy": policy,
                "on_behalf_of": represented,
                "relationships": relationships,
                "fresh": is_fresh,
                "withheld": withheld,
                "rows": rows.into_iter().filter(|r| !r.sensitive).map(|r| json!({ "key": r.key, "kind": r.kind, "subject": r.subject, "body": r.body })).collect::<Vec<_>>(),
            })
        }
        Question::Practice { party, at } => {
            let on = state.today(at);
            let advisers: Vec<Value> = d
                .directory
                .practice(&party, on)
                .into_iter()
                .map(|e| {
                    let policies: Vec<Value> = d
                        .directory
                        .policies_of(e)
                        .into_iter()
                        .filter_map(|policy| policy_summary(state, &e.adviser, policy, at))
                        .collect();
                    json!({
                        "engagement": e.id,
                        "adviser": e.adviser,
                        "fap": e.fap,
                        "from": e.from,
                        "standing": standing_json(&d.directory.standing(&e.id, on)),
                        "policies": policies,
                    })
                })
                .collect();
            let policies: usize = advisers.iter().map(|a| a["policies"].as_array().map_or(0, Vec::len)).sum();
            json!({ "party": party, "advisers": advisers, "policies": policies })
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
        // Their own, and the changes others prepared that they approved or declined, so the outcome stays in view.
        Question::Requests { party } => json!(d
            .requests
            .values()
            .filter(|r| r.party == party || r.approved_by.as_ref() == Some(&party) || r.declined_by.as_ref() == Some(&party))
            .map(|r| {
                // The policy's number, which the person knows it by, even after it has left their view.
                let mut row = json!(r);
                row["number"] = json!(r.policy.as_ref().and_then(|p| d.view.policy(p)).map(|e| e.number.clone()));
                row
            })
            .collect::<Vec<_>>()),
        Question::Pending { module } => json!(state.pending_for(&module)),
        Question::AdviserEngagement { policy, adviser } => {
            json!(d.directory.authorities.get(&policy).and_then(|a| a.get(&adviser)).map(|a| a.engagement.clone()))
        }
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
        Question::DocumentsStaff { staff, at } => json!(d.staff.holds_action(&staff, crate::documents::actions::DOWNLOAD, at)),
        Question::CareStaff { staff, at } => json!(d.staff.holds_action(&staff, crate::staff::actions::VULNERABILITY_KIND, at)),
        Question::SchemeAdmins { policy } => json!(d.directory.employer_admins.get(&policy).map_or(0, |admins| admins.len())),
        Question::SchemeAdministration { policy } => json!({
            "admins": d.directory.employer_admins.get(&policy).cloned().unwrap_or_default(),
            "second_admin": d.directory.second_admin.get(&policy).cloned().unwrap_or_default(),
        }),
        Question::PolicyNamed { text } => {
            let want = text.trim().to_lowercase();
            json!(d
                .view
                .directory()
                .find(|e| e.number.to_lowercase() == want || e.policy.0.to_lowercase() == want)
                .map(|e| json!({ "policy": e.policy, "number": e.number })))
        }
        Question::DisclosureSigner { party, device, proposal, life } => {
            let is_life = d.published.values().flat_map(|rows| rows.values()).any(|r| {
                r.kind == "proposal_life" && r.subject.as_ref() == Some(&party) && r.body["proposal"] == proposal.as_str() && r.body["life"] == life.as_str()
            });
            let key = device
                .and_then(|id| d.identity.devices.get(&id))
                .filter(|v| v.party == party && v.revoked.is_none())
                .map(|v| v.public_key.clone());
            json!({ "life": is_life, "public_key": key })
        }
        Question::MyProposals { party } => json!(d
            .published
            .iter()
            .flat_map(|(policy, rows)| rows.values().map(move |r| (policy, r)))
            .filter(|(_, r)| r.kind == "proposal_life" && r.subject.as_ref() == Some(&party))
            .map(
                |(policy, r)| json!({ "policy": policy, "proposal": r.body["proposal"], "life": r.body["life"], "stage": r.body["stage"],
                "waiting_on": r.body["waiting_on"], "offer": r.body.get("offer").map(|o| { let mut o = o.clone(); if let Some(o) = o.as_object_mut() { o.remove("application"); } o }),
                "requote": r.body.get("requote").map(|q| json!({ "code": q["code"], "quoted_minor": q["quoted_minor"] })) })
            )
            .collect::<Vec<_>>()),
        Question::Requote { party, proposal, life } => d
            .published
            .values()
            .flat_map(|rows| rows.values())
            .find(|r| {
                r.kind == "proposal_life" && r.subject.as_ref() == Some(&party) && r.body["proposal"] == proposal.as_str() && r.body["life"] == life.as_str()
            })
            .and_then(|r| r.body.get("requote").cloned())
            .unwrap_or(Value::Null),
        Question::Offer { party, proposal, life } => d
            .published
            .values()
            .flat_map(|rows| rows.values())
            .find(|r| {
                r.kind == "proposal_life" && r.subject.as_ref() == Some(&party) && r.body["proposal"] == proposal.as_str() && r.body["life"] == life.as_str()
            })
            .and_then(|r| r.body.get("offer").cloned())
            .unwrap_or(Value::Null),
        Question::ReplacementStatement { party, proposal, life } => {
            let found = d.published.iter().find_map(|(policy, rows)| {
                rows.values()
                    .find(|r| {
                        r.kind == "proposal_life"
                            && r.subject.as_ref() == Some(&party)
                            && r.body["proposal"] == proposal.as_str()
                            && r.body["life"] == life.as_str()
                    })
                    .map(|r| (policy, &r.body["replacement"]))
            });
            match found {
                None => Value::Null,
                Some((_, offer)) if !offer.is_object() => json!({ "replacement": false }),
                Some((policy, offer)) => {
                    // A top-up is a further layer on the cover held, so what is held stays. A new
                    // policy that replaces another of ours names the old one: its cover is what is lost.
                    let replaced = offer["replaced_policy"].as_str().map(PolicyId::from).unwrap_or_else(|| policy.clone());
                    let held: Vec<Value> = d
                        .view
                        .layers_on_policy(&replaced)
                        .into_iter()
                        .filter(|r| r.in_force && d.view.party_of(&replaced, &r.life) == Some(&party))
                        .filter(|r| &replaced != policy || r.cover == offer["cover"].as_str().unwrap_or_default())
                        .map(|r| json!({ "cover": r.cover, "type": r.cover_type, "layer": r.layer, "sum_insured_minor": r.sum_insured_minor }))
                        .collect();
                    let (kept, lost) = if &replaced == policy { (held, Vec::new()) } else { (Vec::new(), held) };
                    let old = d
                        .extras
                        .get(&replaced)
                        .and_then(|e| e.notices.back())
                        .map(|(_, start, end, total)| json!({ "period_start": start, "period_end": end, "total_minor": total }));
                    json!({
                        "replacement": true,
                        "replaced_policy": replaced,
                        "kept": kept,
                        "gained": { "cover": offer["cover"], "sum_insured_minor": offer["sum_insured_minor"] },
                        "lost": lost,
                        "old_premium": old,
                        "new_premium_minor": offer["premium_minor"],
                    })
                }
            }
        }
        Question::AdviserEngagements { party } => {
            json!(d
                .directory
                .engagements
                .values()
                .filter(|e| e.adviser == party)
                .map(|e| &e.id)
                .collect::<Vec<_>>())
        }
        Question::Authorise { request } => {
            let decision = d.staff.authorise(state.staff_policy(), &request);
            json!({ "decision": decision, "explanation": decision.denied.map(crate::staff::Denial::explain) })
        }
        Question::OwnAccess { staff, at } => {
            let mut own = d.staff.own_access(&staff, at);
            // The books' business date, which every page's dates start from.
            own["business_date"] = json!(d.today);
            own
        }
        Question::StaffAct {
            staff,
            action,
            class,
            record,
            about,
            scopes,
            purpose,
            at,
        } => {
            let (request, mut decision) = staff_check(state, &staff, &action, class, scopes, record, purpose, at);
            let connected = |who: &PartyId| *who == staff || d.staff.connections.get(&staff).is_some_and(|c| c.iter().any(|p| p == who));
            if decision.allowed && about.as_ref().is_some_and(connected) {
                decision = crate::staff::Decision {
                    allowed: false,
                    denied: Some(crate::staff::Denial::Personal),
                    ..decision
                };
            }
            json!({ "request": request, "decision": decision, "explanation": decision.denied.map(crate::staff::Denial::explain) })
        }
        Question::StaffPolicy {
            staff,
            policy,
            scopes,
            purpose,
            at,
        } => {
            // Staff may type the policy's number as the customer reads it out.
            let policy = known_policy(state, policy);
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
            let shown: Vec<Value> = rows
                .values()
                .filter(|r| !r.sensitive || sensitive)
                .map(|r| json!({ "key": r.key, "kind": r.kind, "subject": r.subject, "sensitive": r.sensitive, "body": r.body }))
                .collect();
            let advisers: Vec<Value> = d
                .directory
                .advisers_on(&policy, state.today(at))
                .into_iter()
                .map(|(adviser, standing)| json!({ "adviser": adviser, "standing": standing_json(&standing) }))
                .collect();
            // Every field is added with its class: a field with none does not compile.
            let mut held = decision.classes.clone();
            held.insert(DataClass::Ordinary);
            crate::classes::Shaper::new(&held)
                .field("request", DataClass::Ordinary, &request)
                .field("decision", DataClass::Ordinary, &decision)
                .field("found", DataClass::Ordinary, state.data.view.policy(&policy).is_some())
                .field("policy", DataClass::Ordinary, &policy)
                // Rows about the policy and its people; health-implying rows are kept to those holding that class above.
                .field("rows", DataClass::Personal, shown)
                // The person is told that something was withheld, never what.
                .field("withheld", DataClass::Ordinary, withheld)
                .field("advisers", DataClass::Personal, advisers)
                .field("policyholders", DataClass::Personal, d.directory.owners(&policy))
                .field(
                    "may_see_kind",
                    DataClass::Ordinary,
                    d.staff.holds_action(&staff, crate::staff::actions::VULNERABILITY_KIND, at),
                )
                .finish()
        }
        Question::StaffAdmin { staff, at } => {
            use crate::staff::actions::*;
            let administers = [MANAGE_STAFF, APPROVE_GRANT, CONFIGURE, APPROVE_CONFIGURATION, APPROVE_PROFILE_UPLIFT];
            if !administers.iter().any(|a| d.staff.holds_action(&staff, a, at)) {
                return json!({ "allowed": false });
            }
            json!({
                "allowed": true,
                "grants": d.staff.grants.values().collect::<Vec<_>>(),
                "members": d.staff.members.values().collect::<Vec<_>>(),
                "templates": d.staff.templates.values().collect::<Vec<_>>(),
                // Changes waiting for a second person, and those this person may approve.
                "pending_changes": d.staff.pending.values().collect::<Vec<_>>(),
                "changes_i_may_approve": d.staff.pending.values()
                    .filter(|c| c.proposed_by != staff && d.staff.holds_action(&staff, &c.approver_action, at))
                    .map(|c| c.id.clone())
                    .collect::<Vec<_>>(),
                "decided_changes": d.staff.decided.iter().rev().collect::<Vec<_>>(),
                "connections": d.staff.connections.iter()
                    .flat_map(|(s, people)| people.iter().map(move |p| json!({ "staff": s, "party": p })))
                    .collect::<Vec<_>>(),
                "connections_ended": d.staff.connections_ended.iter().rev().collect::<Vec<_>>(),
                "alignment": d.staff.alignment(),
                "external_source_available": d.staff.external_available,
            })
        }
        Question::ComplaintsStaff { staff, about, at } => json!({
            "me": crate::complaints::standing(&d.staff, &staff, at),
            "other": about.map(|other| crate::complaints::standing(&d.staff, &other, at)),
        }),
        Question::FinanceStaff { staff, at } => crate::finance::standing(&d.staff, &staff, at),
        Question::ClaimsStaff { staff, at } => crate::claims::standing(&d.staff, &staff, at),
        Question::ReinsuranceStaff { staff, at } => {
            let mut standing = crate::reinsurance::standing(&d.staff, &staff, at);
            if standing["view"] == true {
                standing["pending"] = json!(d.countersign.waiting(crate::reinsurance::MODULE));
            }
            standing["today"] = json!(d.today);
            standing
        }
        Question::PaymentsStaff { staff, at } => crate::payments::standing(&d.staff, &staff, at),
        Question::GroupStaff { staff, at } => crate::group::standing(&d.staff, &staff, at),
        Question::UnderwritingStaff { staff, at } => {
            let mut standing = crate::underwriting::standing(&d.staff, &staff, at);
            if standing["view"] == true {
                standing["pending"] = json!(d.countersign.waiting(crate::underwriting::MODULE));
            }
            standing["today"] = json!(d.today);
            standing
        }
        Question::PricingStaff { staff, at } => crate::pricing::standing(&d.staff, &staff, at),
        Question::ServicingStaff { staff, at } => json!(d.staff.holds_action(&staff, crate::servicing::actions::CARRY_OUT, at)),
        Question::CollectionsStaff { staff, at } => crate::collections::standing(&d.staff, &staff, at),
        Question::CorrespondenceStaff { staff, at } => crate::correspondence::standing(&d.staff, &staff, at),
        Question::CommissionStaff { staff, at, purpose } => {
            let mut standing = crate::commission::standing(&d.staff, &staff, at);
            standing["actor"] = commission_staff(state, &staff, at, &purpose);
            standing
        }
        Question::ReplacementStaff { staff, at, purpose } => {
            let mut standing = crate::replacements::standing(&d.staff, &staff, at);
            standing["actor"] = commission_staff(state, &staff, at, &purpose);
            standing
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
        Question::StaffFind { number, parties } => json!({
            "policies": number.as_deref().map(|n| crate::find::numbered(&d.view, n)).unwrap_or_default(),
            "parties": crate::find::policies_of(&d.view, &d.directory, &parties),
        }),
        Question::ClosedTo { staff, policies, at } => {
            let on = state.today(at);
            let related = |who: &PartyId, policy: &PolicyId| !d.directory.relationships(&d.view, who, policy, on).is_empty();
            let connected = d.staff.connections.get(&staff);
            json!(policies
                .into_iter()
                .filter(|policy| related(&staff, policy) || connected.is_some_and(|c| c.iter().any(|who| related(who, policy))))
                .collect::<Vec<_>>())
        }
        Question::Colleagues { action, at } => {
            let people: Vec<Value> = d
                .staff
                .members
                .values()
                .filter(|m| m.ended.is_none_or(|e| at < e))
                .filter(|m| action.as_deref().is_none_or(|a| d.staff.holds_action(&m.party, a, at)))
                .map(|m| {
                    let roles: Vec<&str> = d.staff.templates_of(&m.party, at).iter().map(|t| t.id.as_str()).collect();
                    json!({ "party": m.party, "team": m.team, "roles": roles })
                })
                .collect();
            json!({ "people": people })
        }
        Question::SecondApprovals { staff, at } => {
            if !d.staff.holds_action(&staff, crate::state::assisted::APPROVE, at) {
                return json!({ "may_approve": false, "rows": [] });
            }
            let mut rows: Vec<&crate::state::Request> = d
                .requests
                .values()
                .filter(|r| {
                    r.status
                        == crate::state::RequestStatus::AwaitingApproval {
                            from: crate::state::Approver::SecondStaff,
                        }
                })
                .filter(|r| r.party != staff)
                .collect();
            rows.sort_by(|a, b| a.submitted.cmp(&b.submitted).then_with(|| a.id.cmp(&b.id)));
            let rows: Vec<Value> = rows
                .into_iter()
                .map(|r| {
                    let why = r.assisted.as_ref();
                    let number = r.policy.as_ref().and_then(|p| d.view.policy(p)).map(|e| e.number.clone());
                    json!({ "id": r.id, "policy": r.policy, "number": number, "kind": r.kind, "payload": r.payload, "entered_by": r.party, "entered_at": r.submitted,
                        "reason": why.map(|a| a.reason.clone()), "case": why.map(|a| a.case.clone()),
                        "backdated": why.is_some_and(|a| a.backdated), "above_limit": why.is_some_and(|a| a.above_limit) })
                })
                .collect();
            json!({ "may_approve": true, "rows": rows })
        }
        Question::BusinessDate => json!({ "today": d.today }),
        Question::Reach { party, at } => {
            let on = state.today(at);
            let adviser = d.directory.engagements.values().any(|e| e.adviser == party) || !d.directory.practice(&party, on).is_empty();
            let administers = related(state, &party).iter().any(|policy| {
                d.directory
                    .relationships(&d.view, &party, policy, on)
                    .contains(&ic_readmodel::scope::Relationship::EmployerAdmin)
            });
            let mut actions: BTreeSet<String> = d.staff.templates_of(&party, at).iter().flat_map(|t| t.actions.iter().cloned()).collect();
            for grant in d.staff.active_grants(&party, at) {
                match &grant.kind {
                    crate::staff::GrantKind::OneOff { action, .. } => {
                        actions.insert(action.clone());
                    }
                    crate::staff::GrantKind::BreakGlass { template } => {
                        actions.extend(d.staff.templates.get(template).into_iter().flat_map(|t| t.actions.iter().cloned()))
                    }
                    _ => {}
                }
            }
            json!({ "adviser": adviser, "scheme_admin": administers, "actions": actions })
        }
        Question::Status => json!({
            "business_date": d.today,
            "heard": d.directory.heard,
            // The same moment for a person or an assistant to read.
            "heard_at": chrono::DateTime::from_timestamp(d.directory.heard, 0).filter(|_| d.directory.heard > 0).map(|t| t.to_rfc3339()),
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
