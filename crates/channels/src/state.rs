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
    /// A second member of staff, for a change staff entered that is above
    /// the enterer's limit or dated in the past. The customer approves
    /// after.
    SecondStaff,
}

/// Staff actions and the authority limit for changes staff enter for a
/// customer who rang (SVC-018).
/// Requests either owner of a jointly owned policy makes alone. Anything
/// else on such a policy waits until every owner has signed it.
pub const EITHER_OWNER: &[&str] = &["notify_claim", "update_contact_details"];

/// What a cover is held at, from the layer's option code, for a person to
/// read: each axis with the choice made on it (`waiting=4w,benefit=to65`),
/// or the one level of a flat list such as a health excess. It is what was
/// bought, not a health fact, so it goes wherever the cover row goes.
pub fn chosen(code: &str) -> Option<Value> {
    let code = code.trim();
    if code.is_empty() {
        return None;
    }
    let mut out = serde_json::Map::new();
    for part in code.split(',') {
        match part.split_once('=') {
            Some((axis, choice)) => out.insert(axis.trim().to_string(), json!(choice.trim())),
            None => out.insert("level".to_string(), json!(part.trim())),
        };
    }
    Some(Value::Object(out))
}

/// Requests that move money or change where it goes. A person's own
/// request of one of these kinds is taken only signed on their enrolled
/// device, which needs a connection for the challenge: a payment keyed in
/// with no signal is not queued and sent later as if it had been signed.
pub const MONEY_KINDS: &[&str] = &["make_payment", "change_payment_method", "change_bank_account", "withdrawal", "surrender"];

/// Requests that give up cover or change what is collected and when. Like
/// money, a person's own request of one of these kinds is taken only signed
/// on their enrolled device, so a stolen session or an assistant acting on a
/// sign-in alone cannot end someone's cover or change their debits. Declining
/// an indexation is not among them: it is taken back as easily as it is made.
pub const CONTRACT_KINDS: &[&str] = &["cancel_policy", "change_frequency", "reduce_cover", "switch_funds"];

/// Whether a person's own request of this kind needs their device's signature.
pub fn needs_signature(kind: &str) -> bool {
    MONEY_KINDS.contains(&kind) || CONTRACT_KINDS.contains(&kind)
}

pub mod assisted {
    /// Enter a change for a customer, for them to approve in the app.
    pub const ENTER: &str = "service.enter_change";
    /// Be the second person on a change another entered.
    pub const APPROVE: &str = "service.approve_change";
    /// The authority profile limit on the amount a change moves.
    pub const LIMIT: &str = "assisted_change";
    /// The authority profile limit on the cover a reduction gives up. A
    /// profile without it sends every reduction to a second person.
    pub const COVER_LIMIT: &str = "assisted_cover_reduction";
}

/// What a change staff entered for a customer carries: why, the case it
/// belongs to, and the second member of staff where one was needed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assisted {
    pub reason: String,
    pub case: String,
    /// The payload's `effective` date was before today.
    #[serde(default)]
    pub backdated: bool,
    /// The payload's `amount_minor`, or the cover a reduction gives up, was
    /// above the enterer's limit.
    #[serde(default)]
    pub above_limit: bool,
    /// For a reduction: the cover given up, which the limit was judged on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_given_up_minor: Option<i64>,
    #[serde(default)]
    pub second: Option<PartyId>,
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
    /// On a policy owned jointly, the owners who have signed it so far. It
    /// goes ahead once every owner has.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub signed_by: BTreeSet<PartyId>,
    /// The customer master says this person's requests go to a person
    /// first (a vulnerability marker). The marker itself is never here.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub route_to_person: bool,
    /// Entered by staff for a customer who rang.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assisted: Option<Assisted>,
    /// Cancelled by the policyholder rather than by whoever asked: the
    /// customer declined a change prepared for them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declined_by: Option<PartyId>,
    /// The books' business date it was asked on, which is the date people
    /// are shown; `submitted` is the clock's moment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<NaiveDate>,
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
    /// A command for another module that two people put their names to.
    Countersign(crate::countersign::CountersignCommand),
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
        /// Set by the gateway from the customer master's routing answer.
        #[serde(default)]
        route_to_person: bool,
        /// Staff entering a change for a customer give why and the case.
        #[serde(default)]
        assisted: Option<Assisted>,
    },
    /// A second member of staff agrees a change another entered, which
    /// then goes to the customer.
    SecondApproveRequest {
        id: String,
    },
    ApproveRequest {
        id: String,
        signed: bool,
        /// Set by the gateway from the customer master's routing answer for
        /// the person approving. A request someone prepared for a customer
        /// is routed as that customer's once they approve it.
        #[serde(default)]
        route_to_person: bool,
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
    /// A child insured under a policy now controls their own health
    /// information: the policyholder no longer sees it. Tell the child, who
    /// may now enrol a device of their own, and the policyholder.
    ControlPassed {
        policy: PolicyId,
        party: PartyId,
        /// Every owner, each of whom is told.
        #[serde(default)]
        policyholders: BTreeSet<PartyId>,
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
    /// Units held in each fund, from the investment ledger's executed and
    /// replayed orders.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub holdings: BTreeMap<String, Holding>,
    /// Orders placed and not yet executed or cancelled, by order ID.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub orders: BTreeMap<String, PendingOrder>,
    /// How often premium falls due, once the owner has changed it, and from when.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency: Option<(ic_product::Frequency, NaiveDate)>,
}

/// A policy's units in one fund, in millionths of a unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holding {
    pub units_micro: i64,
    /// The pricing date of the last order that moved it.
    pub as_at: NaiveDate,
}

/// A fund's latest price: the highest version of the latest pricing date.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FundPrice {
    pub pricing_date: NaiveDate,
    pub version: u32,
    /// What a seller receives for a unit: what a withdrawal or surrender
    /// pays, so the value shown is what the customer would receive.
    pub sell: ic_money::DecStr,
}

/// An order waiting for its price.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingOrder {
    pub fund: String,
    pub direction: ic_core::invest::Direction,
    pub amount: ic_core::invest::OrderAmount,
    pub kind: ic_core::invest::OrderKind,
    pub pricing_date: NaiveDate,
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
    /// Commands for other modules waiting for a second person.
    #[serde(default)]
    pub countersign: crate::countersign::Countersign,
    /// The latest business date any partition has reported.
    pub today: Option<NaiveDate>,
    /// Each fund's latest unit price, for valuing holdings.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub prices: BTreeMap<String, FundPrice>,
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

/// Whether a policy has ended, whatever the reason: lapsed, surrendered,
/// matured, expired, cancelled, ended by a claim, or avoided for
/// non-disclosure (treated as never in force). Pages show it as ended.
pub fn ended(status: ic_core::PolicyStatus) -> bool {
    use ic_core::PolicyStatus as S;
    !matches!(status, S::Quoted | S::Proposed | S::InForce | S::PaidUp)
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
                "number": entry.number, "product": entry.product_code, "status": entry.status, "ended": ended(entry.status), "standing": entry.standing,
                "indexation_declined": extras.and_then(|e| e.indexation_declined),
                "last_anniversary": extras.and_then(|e| e.last_anniversary).map(|(n, on)| json!({ "number": n, "on": on })),
                // Changed by the owner: what they now pay by and from when. Until then the bills show it.
                "frequency": extras.and_then(|e| e.frequency).map(|(f, from)| json!({ "frequency": f, "from": from })),
            }),
        );
        if let Some(b) = d.view.billing(policy) {
            add("billing".to_string(), "billing", None, false, json!(b));
        }
        for (fund, held) in extras.into_iter().flat_map(|e| e.holdings.iter()) {
            let mut body = json!({ "fund": fund, "units_micro": held.units_micro, "as_at": held.as_at });
            // A value is never shown without the date of the price it is at.
            if let Some(price) = d.prices.get(fund) {
                let value = ic_money::UnitQty::new(held.units_micro, ic_money::FundId(fund.clone())).value_at(
                    price.sell.get(),
                    ic_money::Currency::NZD,
                    ic_money::RoundingMode::HalfEven,
                );
                if let Ok(value) = value {
                    body["price"] = json!(price.sell);
                    body["price_date"] = json!(price.pricing_date);
                    body["value_minor"] = json!(value.minor);
                }
            }
            add(format!("holding/{fund}"), "holding", None, false, body);
        }
        for (order, o) in extras.into_iter().flat_map(|e| e.orders.iter()) {
            add(
                format!("unit_order/{order}"),
                "unit_order",
                None,
                false,
                json!({ "order": order, "fund": o.fund, "direction": o.direction, "amount": o.amount, "kind": o.kind, "pricing_date": o.pricing_date }),
            );
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
            let mut body = json!({ "cover": r.cover, "type": r.cover_type, "life": r.life, "layer": r.layer, "sum_insured_minor": r.sum_insured_minor, "in_force": r.in_force });
            if let Some(options) = r.option.as_deref().and_then(chosen) {
                body["options"] = options;
            }
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
                // Whose licence it is: the adviser's own, or their provider's (F2-20-11).
                AdviserStanding::ReadOnly { since, licence_of, .. } => json!({
                    "adviser": adviser, "state": "unavailable", "since": since, "contact": "insurer",
                    "licence": if licence_of == *adviser { "adviser" } else { "provider" },
                }),
                // An engagement that has ended is said so, with the day, so the
                // customer knows to appoint another (F2-20-1); one not yet begun
                // is not shown.
                AdviserStanding::Ended => {
                    let Some(to) = d.directory.engagement_of(policy, adviser).and_then(|e| e.to).filter(|to| *to <= on) else {
                        continue;
                    };
                    json!({ "adviser": adviser, "state": "ended", "since": to, "contact": "insurer" })
                }
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
                json!({
                    "id": r.id,
                    "kind": r.kind,
                    "submitted": r.submitted,
                    "state": r.status,
                    // What a servicing change is, so whoever approves it reads it in words: choices only, never a claim's content.
                    "payload": (r.module == crate::servicing::MODULE).then(|| r.payload.clone()),
                    "prepared_by": if r.assisted.is_some() {
                        "staff"
                    } else if d.directory.owns(policy, &r.party) {
                        "owner"
                    } else if d.directory.authorities.get(policy).is_some_and(|a| a.contains_key(&r.party)) {
                        "adviser"
                    } else {
                        "support"
                    },
                    "approved_by": r.approved_by,
                    "declined": r.declined_by.is_some(),
                    // Who asked, who has signed and who declined, so each owner reads where it stands (F2-04-4, F2-04-5).
                    "asked_by": r.party,
                    "signed_by": r.signed_by,
                    "declined_by": r.declined_by,
                    "on": r.on,
                    "context": self.change_context(r),
                }),
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
        self.rows_for(viewer, policy, on, fresh, relationships)
    }

    /// What a person's phone holds of a policy: what they see, less what
    /// they see only as a scheme's administrator. The employer's view is
    /// the portal's, so a director who is also a member, or holds their
    /// own policies, never carries colleagues' cover on their phone.
    pub fn device_rows(&self, viewer: &PartyId, policy: &PolicyId, on: NaiveDate) -> Vec<Row> {
        let mut relationships = self.data.directory.relationships(&self.data.view, viewer, policy, on);
        relationships.retain(|r| *r != Relationship::EmployerAdmin);
        self.rows_for(viewer, policy, on, true, relationships).1
    }

    fn rows_for(&self, viewer: &PartyId, policy: &PolicyId, on: NaiveDate, fresh: bool, relationships: Vec<Relationship>) -> (Vec<Relationship>, Vec<Row>) {
        if relationships.is_empty() {
            return (relationships, vec![]);
        }
        let narrow = relationships.iter().all(|r| matches!(r, Relationship::LifeInsured | Relationship::Member));
        let consented = &self.data.directory.consenting(policy, on);
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
            for holder in self.data.directory.owners(policy) {
                now.remove(&holder);
            }
        }
        let before = self.data.audiences.get(policy).cloned().unwrap_or_default();
        let targets: Vec<PartyId> = match only {
            Some(p) => vec![p.clone()],
            None => before.union(&now).cloned().collect(),
        };
        let all = self.rows(policy, on, only);
        for party in targets {
            let rows = self.device_rows(&party, policy, on);
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
            Event::BillingChanged {
                policy, frequency, first_day, ..
            } => {
                self.data.extras.entry(policy.clone()).or_default().frequency = Some((*frequency, *first_day));
            }
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
            Event::Investment(ic_core::invest::InvestEvent::UnitPriceActivated {
                fund,
                pricing_date,
                version,
                sell: Some(sell),
                ..
            }) => {
                let newer = self
                    .data
                    .prices
                    .get(&fund.0)
                    .is_none_or(|p| (*pricing_date, *version) > (p.pricing_date, p.version));
                if newer {
                    let price = FundPrice {
                        pricing_date: *pricing_date,
                        version: *version,
                        sell: sell.clone(),
                    };
                    self.data.prices.insert(fund.0.clone(), price);
                    // Every policy holding the fund: its value moved.
                    for (policy, extras) in &self.data.extras {
                        if extras.holdings.contains_key(&fund.0) {
                            touched.insert(policy.clone(), None);
                        }
                    }
                }
            }
            Event::Investment(invest) => {
                if let Some(policy) = self.follow_investment(invest) {
                    touched.insert(policy, None);
                }
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

    /// Keep a policy's units and waiting orders as the investment ledger
    /// reports them. The policy whose rows changed, if any.
    fn follow_investment(&mut self, event: &ic_core::invest::InvestEvent) -> Option<PolicyId> {
        use ic_core::invest::{Direction, InvestEvent};
        let (policy, extras) = match event {
            InvestEvent::UnitOrderPlaced { policy, .. }
            | InvestEvent::UnitOrderCancelled { policy, .. }
            | InvestEvent::UnitOrderExecuted { policy, .. }
            | InvestEvent::UnitOrderReplayed { policy, .. } => (policy, self.data.extras.entry(policy.clone()).or_default()),
            _ => return None,
        };
        let mut moved = |fund: &str, delta: i64, on: NaiveDate| {
            let held = extras.holdings.entry(fund.to_string()).or_insert(Holding { units_micro: 0, as_at: on });
            held.units_micro = held.units_micro.saturating_add(delta);
            held.as_at = held.as_at.max(on);
        };
        match event {
            InvestEvent::UnitOrderPlaced {
                order_id,
                fund,
                direction,
                amount,
                kind,
                pricing_date,
                ..
            } => {
                let order = PendingOrder {
                    fund: fund.0.clone(),
                    direction: *direction,
                    amount: *amount,
                    kind: *kind,
                    pricing_date: *pricing_date,
                };
                extras.orders.insert(order_id.clone(), order);
            }
            InvestEvent::UnitOrderCancelled { order_id, .. } => {
                extras.orders.remove(order_id);
            }
            InvestEvent::UnitOrderExecuted {
                order_id,
                fund,
                direction,
                units_micro,
                pricing_date,
                ..
            } => {
                extras.orders.remove(order_id);
                let delta = match direction {
                    Direction::Buy => *units_micro,
                    Direction::Sell => -units_micro.abs(),
                };
                moved(&fund.0, delta, *pricing_date);
            }
            InvestEvent::UnitOrderReplayed {
                fund,
                units_delta_micro,
                pricing_date,
                ..
            } => moved(&fund.0, *units_delta_micro, *pricing_date),
            _ => {}
        }
        Some(policy.clone())
    }

    /// The cover a life holds now under a cover code: its layers in force, added up.
    pub fn cover_now(&self, policy: &PolicyId, cover: &str, life: &str) -> Option<i64> {
        let layers: Vec<i64> = self
            .data
            .view
            .layers_on_policy(policy)
            .into_iter()
            .filter(|r| r.in_force && r.cover == cover && r.life.0 == life)
            .map(|r| r.sum_insured_minor)
            .collect();
        (!layers.is_empty()).then(|| layers.iter().sum())
    }

    /// What a change is about, for the people who approve it to read: for a
    /// reduction, the cover the life holds now and whose life it is (F2-04-1,
    /// F2-04-3, F2-04-7). Nothing for other kinds.
    pub fn change_context(&self, r: &Request) -> Value {
        let (Some(policy), "reduce_cover") = (&r.policy, r.kind.as_str()) else {
            return Value::Null;
        };
        let (cover, life) = (r.payload["cover"].as_str().unwrap_or_default(), r.payload["life"].as_str().unwrap_or_default());
        serde_json::json!({
            "from_minor": self.cover_now(policy, cover, life),
            "life_party": self.data.view.party_of(policy, &ic_core::LifeId::from(life)),
        })
    }

    /// Check a change staff entered for a customer: it says why and names
    /// the case, the enterer may enter changes and, where it moves money,
    /// the amount is within their limit; and say whether a second member of
    /// staff must agree it first (above the limit, or dated in the past).
    fn assisted_check(&self, staff: &PartyId, at: At, on: NaiveDate, request: &mut Request) -> Result<bool, Reject> {
        let a = request
            .assisted
            .as_mut()
            .filter(|a| !a.reason.trim().is_empty() && !a.case.trim().is_empty())
            .ok_or_else(|| reject("bad_request", "a change entered for a customer says why and names the case"))?;
        let amount = request.payload["amount_minor"].as_i64();
        let check = |limit: &str, amount: Option<i64>| {
            self.data.staff.authorise(
                &self.staff_policy,
                &crate::staff::AccessRequest {
                    staff: staff.clone(),
                    action: assisted::ENTER.to_string(),
                    class: crate::classes::DataClass::Ordinary,
                    scopes: vec![],
                    record: request.policy.as_ref().map(|p| format!("policy:{p}")),
                    amount: amount.map(|value| crate::staff::Amount {
                        limit: limit.to_string(),
                        value,
                    }),
                    purpose: Some(crate::staff::Purpose {
                        kind: crate::staff::PurposeKind::Case,
                        reference: a.case.clone(),
                    }),
                    decides: true,
                    conflict_subject: None,
                    maker: None,
                    fresh: self.data.staff.external_available,
                    at,
                },
            )
        };
        let plain = check(assisted::LIMIT, None);
        if !plain.allowed {
            let why = plain.denied.map_or("refused", crate::staff::Denial::explain);
            return Err(reject("forbidden", why));
        }
        let above = |limit: &str, value: i64| check(limit, Some(value)).denied == Some(crate::staff::Denial::HowMuch);
        let mut above_limit = amount.is_some_and(|value| above(assisted::LIMIT, value));
        // A reduction is measured by the cover given up, against its own limit:
        // a $300,000 cut is not a $0 change because no money moves (F2-04-6).
        let mut given_up = None;
        if request.kind == "reduce_cover" {
            let (cover, life) = (
                request.payload["cover"].as_str().unwrap_or_default(),
                request.payload["life"].as_str().unwrap_or_default(),
            );
            let now = request.policy.as_ref().and_then(|policy| self.cover_now(policy, cover, life)).ok_or_else(|| {
                reject(
                    "bad_request",
                    format!("the policy has no {cover} cover in force on {life}; pick the cover to reduce"),
                )
            })?;
            let kept = request.payload["sum_insured_minor"].as_i64().unwrap_or(now);
            if kept >= now {
                return Err(reject("bad_request", "a reduction keeps less cover than the life holds now"));
            }
            given_up = Some(now - kept);
            above_limit |= above(assisted::COVER_LIMIT, now - kept);
        }
        a.above_limit = above_limit;
        a.cover_given_up_minor = given_up;
        a.backdated = request.payload["effective"]
            .as_str()
            .and_then(|d| d.parse::<NaiveDate>().ok())
            .is_some_and(|d| d < on);
        Ok(a.above_limit || a.backdated)
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
            // Staff act for a customer who called; the customer approves the
            // prompt, after a second member of staff where one is needed.
            (Actor::Staff(staff), Some(_)) => {
                let second = self.assisted_check(staff, at, on, &mut request)?;
                RequestStatus::AwaitingApproval {
                    from: if second { Approver::SecondStaff } else { Approver::Customer },
                }
            }
            (Actor::Staff(_), None) => return Err(reject("bad_request", "a staff request names the policy it is for")),
            (Actor::Party(_), None) => RequestStatus::Pending,
            (Actor::Party(party), Some(policy)) => {
                let relationships = d.directory.relationships(&d.view, party, policy, on);
                if relationships.is_empty() {
                    // The same answer as for a policy that does not exist.
                    return Err(reject("not_found", "no such policy"));
                }
                let owners = d.directory.owners(policy);
                if relationships.contains(&Relationship::Policyholder) {
                    if owners.len() > 1 && !EITHER_OWNER.contains(&request.kind.as_str()) {
                        // Owned jointly: every owner signs, this one first if they did.
                        if request.signed {
                            request.signed_by.insert(party.clone());
                        }
                        RequestStatus::AwaitingApproval { from: Approver::Customer }
                    } else {
                        RequestStatus::Pending
                    }
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
                } else if relationships.contains(&Relationship::SupportPerson) {
                    // A support person helps prepare; the step is the customer's own, signed on their device.
                    RequestStatus::AwaitingApproval { from: Approver::Customer }
                } else if relationships.iter().any(|r| matches!(r, Relationship::LifeInsured | Relationship::Member)) {
                    // A person insured acts on their own matters: a claim, a consent.
                    RequestStatus::Pending
                } else {
                    return Err(reject("forbidden", "your relationship to this policy does not allow requests"));
                }
            }
        };
        // A change of a kind nobody carries out is refused now, once the asker may ask at all, naming the kinds
        // there are, rather than accepted and failed later (F2-05-1).
        if request.module == crate::servicing::MODULE && !crate::servicing::KINDS.iter().any(|(k, _)| *k == request.kind) {
            return Err(reject("unknown_kind", crate::servicing::unknown_kind(&request.kind)));
        }
        // Someone who prepares a change for the customer to approve (an adviser,
        // a support person) is not the one whose circumstances decide how it is
        // handled: the customer's routing is taken when they approve it.
        if matches!(actor, Actor::Party(_)) && request.status == (RequestStatus::AwaitingApproval { from: Approver::Customer }) {
            request.route_to_person = false;
        }
        // Where others must still approve, the customer's signature is taken at approval.
        if matches!(actor, Actor::Party(_)) && request.status == RequestStatus::Pending && needs_signature(&request.kind) && !request.signed {
            return Err(reject("step_up_required", "sign this on your device while connected; nothing has been sent"));
        }
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
                let was = self.data.today;
                for record in &records {
                    if let Err(ProjectionError::Gap { partition, expected, .. }) = self.follow(record, at, &mut touched) {
                        effects.push(Effect::Gap { partition, expected });
                        break;
                    }
                }
                let on = self.today(at);
                // A new business date: children who reached the age of control leave their parents' view.
                if let Some(was) = was.filter(|was| *was < on) {
                    for (policy, party) in self.data.directory.came_of_age(was, on) {
                        effects.push(Effect::ControlPassed {
                            policyholders: self.data.directory.owners(&policy),
                            policy: policy.clone(),
                            party,
                        });
                        touched.insert(policy, None);
                    }
                }
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
                if !matches!(&actor, Actor::Module(m) if m == EVENT_RELAY || m == GATEWAY) {
                    return Err(reject("forbidden", "only the event relay reports the stream is current"));
                }
                self.data.directory.heard = self.data.directory.heard.max(at);
            }
            Command::Directory(command) => {
                let touched = self.data.directory.apply(&actor, at, on, &self.data.view, command)?;
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
                    // A signature over a challenge the device was never given reads as a forgery, not a missing permission.
                    let why = if matches!(command, IdentityCommand::RedeemChallenge { .. }) {
                        "that signature does not answer a challenge your enrolled device was given; sign it again on your phone"
                    } else {
                        "that is not yours to do"
                    };
                    return Err(reject("forbidden", why));
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
            Command::Countersign(command) => {
                self.data.countersign.apply(&self.data.staff, &actor, at, command)?;
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
                route_to_person,
                assisted,
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
                        signed_by: BTreeSet::new(),
                        route_to_person,
                        assisted,
                        declined_by: None,
                        on: Some(on),
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
            Command::SecondApproveRequest { id } => {
                let Actor::Staff(staff) = &actor else {
                    return Err(reject("forbidden", "a second approval comes from staff"));
                };
                let request = self.data.requests.get(&id).ok_or_else(|| reject("not_found", "no such request"))?;
                if request.status != (RequestStatus::AwaitingApproval { from: Approver::SecondStaff }) {
                    return Err(reject("not_awaiting", "that request is not waiting for a second member of staff"));
                }
                if &request.party == staff {
                    return Err(reject("maker_checker", "the person who entered a change is not its second approver"));
                }
                if !self.data.staff.holds_action(staff, assisted::APPROVE, at) {
                    return Err(reject("forbidden", "your roles do not include approving changes others entered"));
                }
                let policy = request.policy.clone();
                if let Some(r) = self.data.requests.get_mut(&id) {
                    r.status = RequestStatus::AwaitingApproval { from: Approver::Customer };
                    if let Some(a) = r.assisted.as_mut() {
                        a.second = Some(staff.clone());
                    }
                }
                if let Some(policy) = policy {
                    self.refresh(&policy, on, None, &mut effects);
                }
            }
            Command::ApproveRequest { id, signed, route_to_person } => {
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
                    Approver::SecondStaff => false,
                    // The administrator who asked cannot be the second.
                    Approver::SecondAdministrator => relationships.contains(&Relationship::EmployerAdmin) && party != &request.party,
                };
                if !allowed {
                    return Err(reject("not_found", "no such request"));
                }
                if from == Approver::Customer && !signed {
                    return Err(reject("step_up_required", "approving this needs your device's signature"));
                }
                let mut signed_by = request.signed_by.clone();
                if from == Approver::Customer {
                    // Every owner signs; on a policy with one owner, that is this one.
                    if !signed_by.insert(party.clone()) {
                        return Err(reject("already_signed", "you have signed this; it waits for the other owners"));
                    }
                }
                let complete = from != Approver::Customer || self.data.directory.owners(&policy).is_subset(&signed_by);
                let module = request.module.clone();
                if let Some(r) = self.data.requests.get_mut(&id) {
                    if from == Approver::Customer {
                        r.signed_by = signed_by;
                        // The customer's own circumstances decide how it is handled;
                        // a second administrator's do not, the scheme is the employer's.
                        r.route_to_person |= route_to_person;
                    }
                    if complete {
                        r.status = RequestStatus::Pending;
                        r.approved_by = Some(party.clone());
                        r.signed = signed;
                    }
                }
                if complete {
                    effects.push(Effect::RequestReady { id, module });
                }
                self.refresh(&policy, on, None, &mut effects);
            }
            Command::CancelRequest { id } => {
                let request = self.data.requests.get(&id).ok_or_else(|| reject("not_found", "no such request"))?;
                let by_requester = actor.party() == Some(&request.party);
                let by_owner = request.policy.as_ref().is_some_and(|policy| {
                    actor
                        .party()
                        .is_some_and(|p| matches!(actor, Actor::Party(_)) && self.data.directory.owns(policy, p))
                });
                if !by_requester && !by_owner {
                    return Err(reject("not_found", "no such request"));
                }
                if !matches!(request.status, RequestStatus::AwaitingApproval { .. } | RequestStatus::Pending) {
                    return Err(reject("already_resolved", "that request has already been resolved"));
                }
                // Staff take back a change they entered by mistake only until an owner has signed it (F2-04-2).
                if matches!(actor, Actor::Staff(_)) && (request.status == RequestStatus::Pending || !request.signed_by.is_empty()) {
                    return Err(reject(
                        "already_signed",
                        "the customer has signed it, so it is theirs now: they can decline it in the app, or ask for it to be undone",
                    ));
                }
                let policy = request.policy.clone();
                if let Some(r) = self.data.requests.get_mut(&id) {
                    r.status = RequestStatus::Cancelled;
                    r.declined_by = actor.party().filter(|_| !by_requester).cloned();
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
