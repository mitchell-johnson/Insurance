//! The channel module's replicated state machine.
//!
//! One deterministic state machine holds everything the channels need to
//! answer a request: the read models followed from the core's event stream,
//! who is who and how they relate to each policy, enrolled devices, staff
//! entitlements, each party's device feed, and the requests in flight. It
//! changes only by applying a logged command, and never reads a clock: every
//! command carries the time it was proposed.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use chrono::{DateTime, NaiveDate};
use ic_core::{EventRecord, LifeId, PartyId, PolicyId, Reject};
use ic_ledger::PartitionId;
use ic_readmodel::scope::{AudienceScope, Relationship};
use ic_readmodel::{ProjectionError, Projections};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::directory::{Actor, AdviserStanding, Directory, DirectoryCommand};
use crate::feed::{visible, PartyFeed, Row};
use crate::identity::{IdentityCommand, IdentityEvent, IdentityState};
use crate::staff::{At, PolicyError, StaffCommand, StaffEvent, StaffPolicy, StaffState};
use crate::workbench::{Authority, Workbench, WorkbenchCommand, WorkbenchEvent};

/// How many idempotency keys are remembered.
pub const KEY_MEMORY: usize = 100_000;
/// How many premium notices a policy's view keeps.
pub const NOTICES_KEPT: usize = 6;
/// The module that relays the core's event stream.
pub const EVENT_RELAY: &str = "event-relay";
/// The channel gateway itself, when it resolves a request it dispatched.
pub const GATEWAY: &str = "portal-gateway";

/// What goes into the channel module's log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    /// Idempotency key. A duplicate changes nothing.
    pub key: String,
    /// When it was proposed, in seconds since the epoch.
    pub at: At,
    pub actor: Actor,
    pub command: Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Approver {
    /// The policyholder approves what an adviser or staff member prepared.
    Customer,
    /// A second administrator of the scheme.
    SecondAdministrator,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RequestStatus {
    AwaitingApproval {
        from: Approver,
    },
    /// Accepted and waiting for the owning module.
    Pending,
    Applied {
        reference: Option<String>,
    },
    Rejected {
        code: String,
        message: String,
    },
    Cancelled,
}

/// Something a user asked for. Writes are asynchronous by contract: the
/// request is recorded, shown as pending, and resolved by an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub id: String,
    /// Who asked.
    pub party: PartyId,
    pub policy: Option<PolicyId>,
    /// What is asked for: `decline_indexation`, `cancel_policy`, `notify_claim`, …
    pub kind: String,
    /// The module that owns the outcome.
    pub module: String,
    /// IDs and choices only. Never clinical content.
    pub payload: Value,
    /// The requester's device signed this exact request.
    pub signed: bool,
    pub submitted: At,
    #[serde(flatten)]
    pub status: RequestStatus,
    /// Who approved it, where it needed approval.
    pub approved_by: Option<PartyId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    /// A batch of the core's events, in order per partition.
    CoreEvents {
        records: Vec<EventRecord>,
    },
    /// The relay has nothing new: relationship data is still current.
    Heartbeat,
    Directory(DirectoryCommand),
    Identity(IdentityCommand),
    Staff(StaffCommand),
    /// The held balance workbench.
    Workbench(WorkbenchCommand),
    /// A module publishes rows of its own into the audience-scoped views:
    /// a claim's status, a held balance, a proposal's progress.
    PublishRows {
        rows: Vec<Row>,
        remove: Vec<String>,
    },
    SubmitRequest {
        id: String,
        policy: Option<PolicyId>,
        kind: String,
        module: String,
        payload: Value,
        signed: bool,
    },
    ApproveRequest {
        id: String,
        signed: bool,
    },
    CancelRequest {
        id: String,
    },
    ResolveRequest {
        id: String,
        reference: Option<String>,
        rejected: Option<(String, String)>,
    },
}

/// What applying a command produced, for the host to act on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    Identity(IdentityEvent),
    Staff(StaffEvent),
    Workbench(WorkbenchEvent),
    /// Re-check this party's open sessions and streams now.
    Revoked {
        party: PartyId,
    },
    /// A party's feed moved: wake their sync streams.
    FeedMoved {
        party: PartyId,
        version: u64,
    },
    /// The event stream has a hole. Ask for the partition's events from `expected`.
    Gap {
        partition: PartitionId,
        expected: u64,
    },
    /// A request is ready for its owning module.
    RequestReady {
        id: String,
        module: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Output {
    pub result: Result<Vec<Effect>, Reject>,
    pub duplicate: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyExtras {
    /// (receivable, period start, period end, total).
    pub notices: VecDeque<(u32, NaiveDate, NaiveDate, i64)>,
    pub indexation_declined: Option<bool>,
    pub last_anniversary: Option<(u32, NaiveDate)>,
}

/// Everything replicated. A pure function of the log.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Data {
    pub view: Projections,
    pub directory: Directory,
    pub identity: IdentityState,
    pub staff: StaffState,
    pub extras: BTreeMap<PolicyId, PolicyExtras>,
    /// Rows other modules published, by policy then key.
    pub published: BTreeMap<PolicyId, BTreeMap<String, Row>>,
    pub feeds: BTreeMap<PartyId, PartyFeed>,
    /// Who holds rows of each policy in their feed.
    pub audiences: BTreeMap<PolicyId, BTreeSet<PartyId>>,
    pub requests: BTreeMap<String, Request>,
    #[serde(default)]
    pub workbench: Workbench,
    /// The latest business date any partition has reported.
    pub today: Option<NaiveDate>,
    keys: BTreeSet<String>,
    key_order: VecDeque<String>,
}

pub struct ChannelState {
    pub data: Data,
    scope: AudienceScope,
    staff_policy: StaffPolicy,
}

/// The shipped policies do not load. A build that passes its tests cannot
/// produce this.
#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error(transparent)]
    Audience(#[from] ic_readmodel::scope::ScopeError),
    #[error(transparent)]
    Staff(#[from] PolicyError),
}

// The replicated state is `data`. The audience policy is configuration every
// replica loads alike, so a saved state is the data alone.
impl Serialize for ChannelState {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.data.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ChannelState {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let data = Data::deserialize(deserializer)?;
        let scope = AudienceScope::shipped().map_err(serde::de::Error::custom)?;
        let staff_policy = StaffPolicy::shipped().map_err(serde::de::Error::custom)?;
        Ok(Self { data, scope, staff_policy })
    }
}

fn reject(code: &str, message: impl Into<String>) -> Reject {
    Reject::new(code, message)
}

/// The policy an event concerns, and the life where it concerns one member.
/// Read from the event's own fields, so events added by other modules are
/// followed without a change here.
fn subject_of(record: &EventRecord) -> Option<(PolicyId, Option<LifeId>)> {
    let value = serde_json::to_value(&record.event).ok()?;
    let fields = value.as_object()?.values().next()?;
    let policy = fields.get("policy")?.as_str()?;
    let life = fields.get("life").and_then(Value::as_str).map(LifeId::from);
    Some((PolicyId::from(policy), life))
}

impl ChannelState {
    pub fn new(scope: AudienceScope, staff_policy: StaffPolicy) -> Self {
        Self {
            data: Data {
                staff: StaffState::new(),
                ..Data::default()
            },
            scope,
            staff_policy,
        }
    }

    /// With the audience and staff policies shipped with the platform.
    pub fn shipped() -> Result<Self, SetupError> {
        Ok(Self::new(AudienceScope::shipped()?, StaffPolicy::shipped()?))
    }

    pub fn staff_policy(&self) -> &StaffPolicy {
        &self.staff_policy
    }

    pub fn scope(&self) -> &AudienceScope {
        &self.scope
    }

    /// Hash of the whole replicated state. Replicas that applied the same
    /// log report the same hash.
    pub fn state_hash(&self) -> String {
        // Serialising plain maps and strings cannot fail.
        hex::encode(Sha256::digest(serde_json::to_vec(&self.data).unwrap_or_default()))
    }

    /// The date relationships and licences are judged on: the business date
    /// where known, otherwise the calendar date of `at`.
    pub fn today(&self, at: At) -> NaiveDate {
        self.data
            .today
            .or_else(|| DateTime::from_timestamp(at, 0).map(|t| t.date_naive()))
            .unwrap_or_default()
    }

    /// Every row about a policy, before audience scoping. `only` limits the
    /// per-person rows to one person.
    pub fn rows(&self, policy: &PolicyId, on: NaiveDate, only: Option<&PartyId>) -> BTreeMap<String, Row> {
        let d = &self.data;
        let mut rows = BTreeMap::new();
        let Some(entry) = d.view.policy(policy) else { return rows };
        let mut add = |suffix: String, kind: &str, subject: Option<PartyId>, sensitive: bool, body: Value| {
            let key = format!("{policy}/{suffix}");
            rows.insert(
                key.clone(),
                Row {
                    key,
                    policy: policy.clone(),
                    kind: kind.to_string(),
                    subject,
                    sensitive,
                    body,
                },
            );
        };
        let extras = d.extras.get(policy);
        add(
            "policy".to_string(),
            "policy",
            None,
            false,
            json!({
                "number": entry.number, "product": entry.product_code, "status": entry.status, "standing": entry.standing,
                "indexation_declined": extras.and_then(|e| e.indexation_declined),
                "last_anniversary": extras.and_then(|e| e.last_anniversary).map(|(n, on)| json!({ "number": n, "on": on })),
            }),
        );
        if let Some(b) = d.view.billing(policy) {
            add("billing".to_string(), "billing", None, false, json!(b));
        }
        for (receivable, start, end, total) in extras.into_iter().flat_map(|e| e.notices.iter()) {
            add(
                format!("notice/{receivable}"),
                "billing",
                None,
                false,
                json!({ "receivable": receivable, "period_start": start, "period_end": end, "total_minor": total }),
            );
        }
        let members = d.directory.scheme_members.get(policy);
        let (mut count, mut total) = (BTreeSet::new(), 0i64);
        for r in d.view.layers_on_policy(policy) {
            let party = d.view.party_of(policy, &r.life).cloned();
            let is_member = members.is_some_and(|m| m.contains(&r.life));
            if is_member {
                count.insert(&r.life);
                total += r.sum_insured_minor;
            }
            if only.is_some() && party.as_ref() != only {
                continue;
            }
            let body = json!({ "cover": r.cover, "type": r.cover_type, "life": r.life, "layer": r.layer, "sum_insured_minor": r.sum_insured_minor, "in_force": r.in_force });
            if is_member {
                add(format!("member/{}/{}", r.life, r.cover), "member", party, false, body);
            } else {
                add(format!("cover/{}/{}", r.cover, r.layer), "cover", party, false, body);
            }
        }
        if members.is_some() {
            add(
                "scheme".to_string(),
                "scheme",
                None,
                false,
                json!({ "members": count.len(), "sum_insured_minor": total }),
            );
        }
        for (adviser, standing) in d.directory.advisers_on(policy, on) {
            let body = match standing {
                AdviserStanding::Active => json!({ "adviser": adviser, "state": "available" }),
                // The customer is told the adviser cannot act, and given a
                // direct route to the insurer. They may appoint another at any time.
                AdviserStanding::ReadOnly { since, .. } => json!({ "adviser": adviser, "state": "unavailable", "since": since, "contact": "insurer" }),
                AdviserStanding::Ended => continue,
            };
            add(format!("adviser/{adviser}"), "adviser", None, false, body);
        }
        for r in d.requests.values().filter(|r| r.policy.as_ref() == Some(policy)) {
            if only.is_some_and(|p| p != &r.party) {
                continue;
            }
            add(
                format!("request/{}", r.id),
                "request",
                Some(r.party.clone()),
                false,
                json!({ "id": r.id, "kind": r.kind, "submitted": r.submitted, "state": r.status }),
            );
        }
        for (key, row) in d.published.get(policy).into_iter().flatten() {
            if only.is_none() || row.subject.as_ref() == only || row.subject.is_none() {
                rows.insert(key.clone(), row.clone());
            }
        }
        rows
    }

    /// The rows of a policy a viewer may see, with their relationships.
    /// This is what the portals read, and what feeds are built from.
    pub fn visible_rows(&self, viewer: &PartyId, policy: &PolicyId, on: NaiveDate, fresh: bool) -> (Vec<Relationship>, Vec<Row>) {
        let relationships = self.data.directory.relationships(&self.data.view, viewer, policy, on);
        if relationships.is_empty() {
            return (relationships, vec![]);
        }
        let narrow = relationships.iter().all(|r| matches!(r, Relationship::LifeInsured | Relationship::Member));
        let none = BTreeSet::new();
        let consented = self.data.directory.family_consent.get(policy).unwrap_or(&none);
        let rows = self
            .rows(policy, on, narrow.then_some(viewer))
            .into_values()
            // A row that cannot be judged is not shown.
            .filter(|row| visible(&self.scope, viewer, &relationships, row, consented, fresh).unwrap_or(false))
            .collect();
        (relationships, rows)
    }

    /// Bring device feeds up to date for a policy. `only` limits the work to
    /// one party, for a change that concerns one member of a large scheme.
    fn refresh(&mut self, policy: &PolicyId, on: NaiveDate, only: Option<&PartyId>, effects: &mut Vec<Effect>) {
        let mut now = self.data.directory.feed_audience(&self.data.view, policy);
        // An employer's scheme is served by the portal, not by a device feed.
        if self.data.directory.scheme_members.contains_key(policy) {
            if let Some(holder) = self.data.directory.policyholders.get(policy) {
                now.remove(holder);
            }
        }
        let before = self.data.audiences.get(policy).cloned().unwrap_or_default();
        let targets: Vec<PartyId> = match only {
            Some(p) => vec![p.clone()],
            None => before.union(&now).cloned().collect(),
        };
        let all = self.rows(policy, on, only);
        for party in targets {
            let (_, rows) = self.visible_rows(&party, policy, on, true);
            let visible: BTreeMap<String, Value> = rows.into_iter().map(|r| (r.key, r.body)).collect();
            if visible.is_empty() && !self.data.feeds.contains_key(&party) {
                continue;
            }
            let feed = self.data.feeds.entry(party.clone()).or_default();
            if feed.reconcile(policy, &visible, |key| all.contains_key(key)) > 0 {
                effects.push(Effect::FeedMoved { party, version: feed.version });
            }
        }
        match only {
            Some(p) if now.contains(p) => {
                self.data.audiences.entry(policy.clone()).or_default().insert(p.clone());
            }
            Some(_) => {}
            None if now.is_empty() => {
                self.data.audiences.remove(policy);
            }
            None => {
                self.data.audiences.insert(policy.clone(), now);
            }
        }
    }

    fn follow(&mut self, record: &EventRecord, at: At, touched: &mut BTreeMap<PolicyId, Option<BTreeSet<PartyId>>>) -> Result<(), ProjectionError> {
        use ic_core::Event;
        if !self.data.view.apply(record)? {
            return Ok(());
        }
        self.data.directory.follow(&record.event);
        self.data.directory.heard = self.data.directory.heard.max(at);
        if self.data.today.is_none_or(|t| t < record.business_date) {
            self.data.today = Some(record.business_date);
        }
        match &record.event {
            Event::PremiumDue {
                policy,
                receivable,
                period_start,
                period_end,
                total_minor,
                ..
            } => {
                let notices = &mut self.data.extras.entry(policy.clone()).or_default().notices;
                notices.push_back((*receivable, *period_start, *period_end, *total_minor));
                while notices.len() > NOTICES_KEPT {
                    notices.pop_front();
                }
            }
            Event::IndexationChoiceRecorded { policy, declined } => {
                self.data.extras.entry(policy.clone()).or_default().indexation_declined = Some(*declined);
            }
            Event::AnniversaryReached { policy, number, on } => {
                self.data.extras.entry(policy.clone()).or_default().last_anniversary = Some((*number, *on));
            }
            _ => {}
        }
        if let Some((policy, life)) = subject_of(record) {
            // A change to one scheme member concerns that member's feed only.
            let member = life
                .filter(|l| self.data.directory.scheme_members.get(&policy).is_some_and(|m| m.contains(l)))
                .map(|l| self.data.view.party_of(&policy, &l).cloned());
            match (touched.get_mut(&policy), member) {
                (Some(None), _) => {}
                (Some(Some(parties)), Some(party)) => parties.extend(party),
                (None, Some(party)) => {
                    touched.insert(policy, Some(party.into_iter().collect()));
                }
                (_, None) => {
                    touched.insert(policy, None);
                }
            }
        }
        Ok(())
    }

    fn submit(&mut self, actor: &Actor, at: At, on: NaiveDate, request: Request) -> Result<Request, Reject> {
        let d = &self.data;
        let mut request = request;
        if d.requests.contains_key(&request.id) {
            return Err(reject("duplicate", "a request with that ID exists"));
        }
        if request.kind.trim().is_empty() || request.module.trim().is_empty() {
            return Err(reject("bad_request", "a request names what is asked and the module that owns it"));
        }
        request.status = match (actor, &request.policy) {
            (Actor::Module(_), _) => return Err(reject("forbidden", "requests come from people")),
            // Staff act for a customer who called; the customer approves the prompt.
            (Actor::Staff(_), Some(_)) => RequestStatus::AwaitingApproval { from: Approver::Customer },
            (Actor::Staff(_), None) => return Err(reject("bad_request", "a staff request names the policy it is for")),
            (Actor::Party(_), None) => RequestStatus::Pending,
            (Actor::Party(party), Some(policy)) => {
                let relationships = d.directory.relationships(&d.view, party, policy, on);
                if relationships.is_empty() {
                    // The same answer as for a policy that does not exist.
                    return Err(reject("not_found", "no such policy"));
                }
                if relationships.contains(&Relationship::Policyholder) {
                    RequestStatus::Pending
                } else if relationships.contains(&Relationship::EmployerAdmin) {
                    if d.directory.second_admin.get(policy).is_some_and(|kinds| kinds.contains(&request.kind)) {
                        RequestStatus::AwaitingApproval {
                            from: Approver::SecondAdministrator,
                        }
                    } else {
                        RequestStatus::Pending
                    }
                } else if relationships.contains(&Relationship::Adviser) {
                    let standing = d
                        .directory
                        .authorities
                        .get(policy)
                        .and_then(|a| a.get(party))
                        .map_or(AdviserStanding::Ended, |a| d.directory.standing(&a.engagement, on));
                    if standing != AdviserStanding::Active {
                        return Err(reject(
                            "adviser_read_only",
                            "your licence status allows you to see your book, and nothing can be quoted, submitted or changed",
                        ));
                    }
                    // An adviser prepares; the customer approves in the app.
                    RequestStatus::AwaitingApproval { from: Approver::Customer }
                } else if relationships.iter().any(|r| matches!(r, Relationship::LifeInsured | Relationship::Member)) {
                    // A person insured acts on their own matters: a claim, a consent.
                    RequestStatus::Pending
                } else {
                    return Err(reject("forbidden", "your relationship to this policy does not allow requests"));
                }
            }
        };
        request.submitted = at;
        Ok(request)
    }

    fn apply_command(&mut self, env: Envelope) -> Result<Vec<Effect>, Reject> {
        let Envelope { at, actor, command, .. } = env;
        let on = self.today(at);
        let mut effects = Vec::new();
        match command {
            Command::CoreEvents { records } => {
                if actor != Actor::Module(EVENT_RELAY.to_string()) {
                    return Err(reject("forbidden", "only the event relay delivers core events"));
                }
                let mut touched = BTreeMap::new();
                for record in &records {
                    if let Err(ProjectionError::Gap { partition, expected, .. }) = self.follow(record, at, &mut touched) {
                        effects.push(Effect::Gap { partition, expected });
                        break;
                    }
                }
                let on = self.today(at);
                for (policy, only) in touched {
                    match only {
                        None => self.refresh(&policy, on, None, &mut effects),
                        Some(parties) => {
                            for party in parties {
                                self.refresh(&policy, on, Some(&party), &mut effects);
                            }
                        }
                    }
                }
            }
            Command::Heartbeat => {
                if actor != Actor::Module(EVENT_RELAY.to_string()) {
                    return Err(reject("forbidden", "only the event relay reports the stream is current"));
                }
                self.data.directory.heard = self.data.directory.heard.max(at);
            }
            Command::Directory(command) => {
                let touched = self.data.directory.apply(&actor, at, &self.data.view, command)?;
                for policy in &touched.policies {
                    self.refresh(policy, on, None, &mut effects);
                }
                effects.extend(touched.revoked.into_iter().map(|party| Effect::Revoked { party }));
            }
            Command::Identity(command) => {
                // A person manages their own devices; enrolment and
                // revocation of sessions come from the identity service.
                let allowed = match (&actor, &command) {
                    (Actor::Module(m), _) => m == "identity",
                    (Actor::Party(p) | Actor::Staff(p), IdentityCommand::RevokeDevice { device }) => self.data.identity.active_device(device, p).is_some(),
                    (Actor::Party(p) | Actor::Staff(p), IdentityCommand::EnrolDevice { party, enrolment, .. }) => {
                        p == party && matches!(enrolment, crate::identity::Enrolment::ExistingDevice { .. })
                    }
                    (Actor::Party(p) | Actor::Staff(p), IdentityCommand::IssueChallenge { device, .. }) => {
                        self.data.identity.active_device(device, p).is_some()
                    }
                    (Actor::Party(p) | Actor::Staff(p), IdentityCommand::RedeemChallenge { id, .. }) => self
                        .data
                        .identity
                        .challenges
                        .get(id)
                        .is_some_and(|c| self.data.identity.active_device(&c.device, p).is_some()),
                    (_, IdentityCommand::RevokeSessions { .. }) => false,
                };
                if !allowed {
                    return Err(reject("forbidden", "that is not yours to do"));
                }
                for event in self.data.identity.apply(at, command)? {
                    if let IdentityEvent::DeviceRevoked { party, .. } | IdentityEvent::SessionsRevoked { party } = &event {
                        effects.push(Effect::Revoked { party: party.clone() });
                    }
                    effects.push(Effect::Identity(event));
                }
            }
            Command::Staff(command) => {
                let staff = match (&actor, &command) {
                    (Actor::Staff(p), _) => p.clone(),
                    // The entitlement source and the operator's bootstrap job are modules.
                    (
                        Actor::Module(m),
                        StaffCommand::Bootstrap { .. }
                        | StaffCommand::ExternalSync { .. }
                        | StaffCommand::ExternalElevation { .. }
                        | StaffCommand::ExternalElevationEnded { .. }
                        | StaffCommand::ExternalSourceAvailable { .. },
                    ) if m == "entitlements" => PartyId::from("module:entitlements"),
                    _ => return Err(reject("forbidden", "staff commands come from staff")),
                };
                for event in self.data.staff.apply(&staff, at, command)? {
                    if let StaffEvent::SessionsRevoked { party } = &event {
                        effects.push(Effect::Revoked { party: party.clone() });
                    }
                    effects.push(Effect::Staff(event));
                }
            }
            Command::Workbench(command) => {
                let authority = Authority {
                    staff: &self.data.staff,
                    policy: &self.staff_policy,
                    fresh: self.data.staff.external_available,
                };
                let events = self.data.workbench.apply(&authority, &actor, at, command)?;
                effects.extend(events.into_iter().map(Effect::Workbench));
            }
            Command::PublishRows { rows, remove } => {
                let Actor::Module(module) = &actor else {
                    return Err(reject("forbidden", "rows are published by modules"));
                };
                // A module writes only under its own name, so one module
                // cannot overwrite another's rows or the channel's own.
                let owned = |policy: &PolicyId, key: &str| key.starts_with(&format!("{policy}/{module}/"));
                if let Some(bad) = rows.iter().find(|r| !owned(&r.policy, &r.key)) {
                    return Err(reject("bad_key", format!("{} is not a key of module {module}", bad.key)));
                }
                let policy_of = |key: &str| PolicyId::from(key.split_once('/').map_or("", |(policy, _)| policy));
                if let Some(bad) = remove.iter().find(|key| !owned(&policy_of(key), key)) {
                    return Err(reject("bad_key", format!("{bad} is not a key of module {module}")));
                }
                let mut touched = BTreeSet::new();
                for key in remove {
                    let policy = policy_of(&key);
                    if let Some(rows) = self.data.published.get_mut(&policy) {
                        rows.remove(&key);
                        if rows.is_empty() {
                            self.data.published.remove(&policy);
                        }
                    }
                    touched.insert(policy);
                }
                for row in rows {
                    touched.insert(row.policy.clone());
                    self.data.published.entry(row.policy.clone()).or_default().insert(row.key.clone(), row);
                }
                for policy in touched {
                    self.refresh(&policy, on, None, &mut effects);
                }
            }
            Command::SubmitRequest {
                id,
                policy,
                kind,
                module,
                payload,
                signed,
            } => {
                let party = actor.party().cloned().ok_or_else(|| reject("forbidden", "requests come from people"))?;
                let request = self.submit(
                    &actor,
                    at,
                    on,
                    Request {
                        id: id.clone(),
                        party,
                        policy,
                        kind,
                        module,
                        payload,
                        signed,
                        submitted: at,
                        status: RequestStatus::Pending,
                        approved_by: None,
                    },
                )?;
                if request.status == RequestStatus::Pending {
                    effects.push(Effect::RequestReady {
                        id: id.clone(),
                        module: request.module.clone(),
                    });
                }
                let policy = request.policy.clone();
                self.data.requests.insert(id, request);
                if let Some(policy) = policy {
                    self.refresh(&policy, on, None, &mut effects);
                }
            }
            Command::ApproveRequest { id, signed } => {
                let Actor::Party(party) = &actor else {
                    return Err(reject("forbidden", "an approval comes from a person"));
                };
                let request = self.data.requests.get(&id).ok_or_else(|| reject("not_found", "no such request"))?;
                let RequestStatus::AwaitingApproval { from } = request.status else {
                    return Err(reject("not_awaiting", "that request is not awaiting approval"));
                };
                let policy = request.policy.clone().ok_or_else(|| reject("not_found", "no such request"))?;
                let relationships = self.data.directory.relationships(&self.data.view, party, &policy, on);
                let allowed = match from {
                    Approver::Customer => relationships.contains(&Relationship::Policyholder),
                    // The administrator who asked cannot be the second.
                    Approver::SecondAdministrator => relationships.contains(&Relationship::EmployerAdmin) && party != &request.party,
                };
                if !allowed {
                    return Err(reject("not_found", "no such request"));
                }
                if from == Approver::Customer && !signed {
                    return Err(reject("step_up_required", "approving this needs your device's signature"));
                }
                let module = request.module.clone();
                if let Some(r) = self.data.requests.get_mut(&id) {
                    r.status = RequestStatus::Pending;
                    r.approved_by = Some(party.clone());
                    r.signed = signed;
                }
                effects.push(Effect::RequestReady { id, module });
                self.refresh(&policy, on, None, &mut effects);
            }
            Command::CancelRequest { id } => {
                let request = self.data.requests.get(&id).ok_or_else(|| reject("not_found", "no such request"))?;
                let by_requester = actor.party() == Some(&request.party);
                let by_owner = request.policy.as_ref().is_some_and(|policy| {
                    actor
                        .party()
                        .is_some_and(|p| matches!(actor, Actor::Party(_)) && self.data.directory.policyholders.get(policy) == Some(p))
                });
                if !by_requester && !by_owner {
                    return Err(reject("not_found", "no such request"));
                }
                if !matches!(request.status, RequestStatus::AwaitingApproval { .. } | RequestStatus::Pending) {
                    return Err(reject("already_resolved", "that request has already been resolved"));
                }
                let policy = request.policy.clone();
                if let Some(r) = self.data.requests.get_mut(&id) {
                    r.status = RequestStatus::Cancelled;
                }
                if let Some(policy) = policy {
                    self.refresh(&policy, on, None, &mut effects);
                }
            }
            Command::ResolveRequest { id, reference, rejected } => {
                let request = self.data.requests.get(&id).ok_or_else(|| reject("not_found", "no such request"))?;
                match &actor {
                    Actor::Module(m) if m == &request.module || m == GATEWAY => {}
                    _ => return Err(reject("forbidden", "a request is resolved by the module that owns it")),
                }
                if request.status != RequestStatus::Pending {
                    return Err(reject("not_pending", "that request is not pending"));
                }
                let policy = request.policy.clone();
                if let Some(r) = self.data.requests.get_mut(&id) {
                    r.status = match rejected {
                        Some((code, message)) => RequestStatus::Rejected { code, message },
                        None => RequestStatus::Applied { reference },
                    };
                }
                if let Some(policy) = policy {
                    self.refresh(&policy, on, None, &mut effects);
                }
            }
        }
        Ok(effects)
    }

    /// Requests accepted and waiting for a module, oldest first.
    pub fn pending_for(&self, module: &str) -> Vec<&Request> {
        let mut out: Vec<&Request> = self
            .data
            .requests
            .values()
            .filter(|r| r.module == module && r.status == RequestStatus::Pending)
            .collect();
        out.sort_by_key(|r| (r.submitted, r.id.clone()));
        out
    }
}

impl ic_log::StateMachine for ChannelState {
    type Command = Envelope;
    type Output = Output;

    fn apply(&mut self, _index: u64, env: Envelope) -> Output {
        if self.data.keys.contains(&env.key) {
            return Output {
                result: Ok(vec![]),
                duplicate: true,
            };
        }
        let key = env.key.clone();
        // Every command validates before it writes, so a rejected command
        // leaves no trace and is not remembered: a corrected retry under the
        // same key is applied.
        let result = self.apply_command(env);
        if result.is_ok() {
            self.data.keys.insert(key.clone());
            self.data.key_order.push_back(key);
            while self.data.key_order.len() > KEY_MEMORY {
                if let Some(old) = self.data.key_order.pop_front() {
                    self.data.keys.remove(&old);
                }
            }
        }
        Output { result, duplicate: false }
    }
}
