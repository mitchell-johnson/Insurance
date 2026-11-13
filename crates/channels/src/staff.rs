//! Staff authorisation.
//!
//! A person's entitlements are their role templates plus their active
//! grants, inside the templates' ceilings. Every action answers four
//! questions: what (is the action in a template the person holds), whose (do
//! they have a path to the record), how much (is the amount inside their
//! authority profile), and why (is an acceptable purpose declared). The
//! facts are worked out here and decided by Cedar policy
//! (`config/cedar/staff.cedar`).
//!
//! Nothing here reads a clock. Every command and every question carries the
//! time it is asked at, in seconds since the epoch, supplied by the caller.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::str::FromStr;

use cedar_policy::{Authorizer, Context, Decision as CedarDecision, Entities, EntityId, EntityTypeName, EntityUid, PolicySet, Request};
use ic_core::{PartyId, Reject};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::classes::DataClass;

/// Seconds since the Unix epoch, supplied by whoever proposes a command.
pub type At = i64;

/// The policy shipped with the platform.
pub const DEFAULT_POLICIES: &str = include_str!("../../../config/cedar/staff.cedar");

/// How many of a person's own recent decisions are kept for their access view.
pub const ACCESS_VIEW_LEN: usize = 200;

/// Actions that govern staff access itself.
pub mod actions {
    pub const MANAGE_STAFF: &str = "staff.manage";
    pub const CONFIGURE: &str = "staff.configure";
    pub const APPROVE_CONFIGURATION: &str = "staff.approve_configuration";
    pub const APPROVE_GRANT: &str = "staff.approve_grant";
    pub const APPROVE_PROFILE_UPLIFT: &str = "staff.approve_profile_uplift";
    pub const REVIEW_BREAK_GLASS: &str = "staff.review_break_glass";
}

/// A role template: what kind of action a person may take, which data
/// classes they see, and the most any authority limit may ever be for them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Template {
    pub id: String,
    pub actions: BTreeSet<String>,
    pub classes: BTreeSet<DataClass>,
    /// The authority profile a holder starts with.
    pub default_profile: String,
    /// The ceiling per limit. No grant raises a limit above it.
    #[serde(default)]
    pub ceilings: BTreeMap<String, i64>,
    /// May be taken under break-glass.
    #[serde(default)]
    pub break_glass: bool,
}

/// How much a person may decide. Versioned: a decision records the version
/// it was made under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub version: u32,
    pub limits: BTreeMap<String, i64>,
}

/// Where a template holding or a grant came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    BuiltIn,
    /// An external identity governance system, with its own reference.
    External {
        reference: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holding {
    pub from: At,
    pub to: Option<At>,
    pub source: Source,
}

impl Holding {
    fn active(&self, at: At) -> bool {
        self.from <= at && self.to.is_none_or(|t| at < t)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaffMember {
    pub party: PartyId,
    pub team: String,
    pub manager: Option<PartyId>,
    pub templates: BTreeMap<String, Holding>,
    /// The authority profile, where it differs from the templates' default.
    pub profile: Option<String>,
    pub started: At,
    pub ended: Option<At>,
    /// When the person's manager last confirmed their roles and profile.
    pub recertified: At,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GrantKind {
    /// Inherit another person's paths (queues, cases, schemes, books) for a
    /// period. Not their authority.
    Cover { covered: PartyId },
    /// Raise limits in the holder's profile for a period.
    ProfileUplift { limits: BTreeMap<String, i64> },
    /// One action on one record, consumed on use.
    OneOff { action: String, record: String },
    /// Emergency access to a wider template. Alerts at once and is reviewed.
    BreakGlass { template: String },
}

impl GrantKind {
    pub fn key(&self) -> &'static str {
        match self {
            GrantKind::Cover { .. } => "cover",
            GrantKind::ProfileUplift { .. } => "profile_uplift",
            GrantKind::OneOff { .. } => "one_off",
            GrantKind::BreakGlass { .. } => "break_glass",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantState {
    Requested,
    Active,
    Revoked,
    Consumed,
    Declined,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub id: String,
    pub holder: PartyId,
    pub kind: GrantKind,
    pub reason: String,
    pub requested_by: PartyId,
    pub requested_at: At,
    pub approver: Option<PartyId>,
    pub from: At,
    pub to: At,
    pub state: GrantState,
    pub source: Source,
    /// Break-glass only: who reviewed the use, and when.
    #[serde(default)]
    pub reviewed: Option<(PartyId, At)>,
}

impl Grant {
    pub fn active(&self, at: At) -> bool {
        self.state == GrantState::Active && self.from <= at && at < self.to
    }
}

/// Two actions one person may not both take on the same subject: underwriting
/// and claims on the same life, payment entry and payment approval.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Conflict {
    pub a: String,
    pub b: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// The longest a grant of each kind may live, in seconds.
    pub max_life: BTreeMap<String, i64>,
    /// Roles not confirmed by the manager within this many seconds lapse.
    pub recertify_every: i64,
}

impl Default for Settings {
    fn default() -> Self {
        const DAY: i64 = 86_400;
        Self {
            max_life: [("cover", 30 * DAY), ("profile_uplift", 14 * DAY), ("one_off", DAY), ("break_glass", 4 * 3600)]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            recertify_every: 180 * DAY,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PurposeKind {
    /// A case the person owns or is reviewing.
    Case,
    /// A held or review task.
    Task,
    /// A reason code for an ad hoc lookup.
    Reason,
}

impl PurposeKind {
    fn key(self) -> &'static str {
        match self {
            PurposeKind::Case => "case",
            PurposeKind::Task => "task",
            PurposeKind::Reason => "reason",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Purpose {
    pub kind: PurposeKind,
    pub reference: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Amount {
    /// The name of the limit in the authority profile.
    pub limit: String,
    pub value: i64,
}

/// One action by one member of staff, as the four questions see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessRequest {
    pub staff: PartyId,
    pub action: String,
    /// The class of data the action needs to be worth doing.
    pub class: DataClass,
    /// The scope keys that reach the record: `case:…`, `queue:…`, `team:…`,
    /// `scheme:…`, `book:…`. Empty for an action that concerns no record.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// The record acted on, for the log and for one-off grants.
    #[serde(default)]
    pub record: Option<String>,
    #[serde(default)]
    pub amount: Option<Amount>,
    #[serde(default)]
    pub purpose: Option<Purpose>,
    /// The action decides something, as opposed to reading.
    #[serde(default)]
    pub decides: bool,
    /// What separation of duties is judged on: `life:…`, `payment:…`.
    #[serde(default)]
    pub conflict_subject: Option<String>,
    /// Who prepared the thing being approved, when this is an approval.
    #[serde(default)]
    pub maker: Option<PartyId>,
    /// Whether entitlement data is inside its staleness bound.
    pub fresh: bool,
    pub at: At,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Denial {
    /// Not a current member of staff.
    NotStaff,
    /// The action is in none of the person's templates.
    What,
    /// No path to this record.
    Whose,
    /// Above the person's authority.
    HowMuch,
    /// No purpose, or one that is not acceptable for this data.
    Why,
    /// The person's templates do not hold the data class.
    Class,
    /// A declared separation of duties conflict.
    Conflict,
    /// The person prepared what they are being asked to approve.
    MakerChecker,
    /// Entitlement data is too old for this action.
    Stale,
    /// Refused by policy for a reason the facts above do not name.
    Policy,
    /// The record is the person's own, or belongs to someone they have a
    /// declared personal connection with.
    Personal,
}

impl Denial {
    /// What the person is told, so a gap becomes a request to a manager.
    pub fn explain(self) -> &'static str {
        match self {
            Denial::NotStaff => "You are not a current member of staff on this platform.",
            Denial::What => "None of your roles allows this action. Ask your manager for the role or a one-off grant.",
            Denial::Whose => "You have no path to this record: it is not in a case, queue, team, scheme or book assigned to you.",
            Denial::HowMuch => "This is above your authority. Refer it up, or ask for a profile uplift.",
            Denial::Why => "A purpose is required: the case or task you are working on, or a reason code this data accepts.",
            Denial::Class => "Your roles do not include the class of data this action needs.",
            Denial::Conflict => "Separation of duties: you have already acted on this in a conflicting role.",
            Denial::MakerChecker => "You prepared this, so someone else must approve it.",
            Denial::Stale => "Your entitlements could not be confirmed as current. Try again shortly.",
            Denial::Policy => "Policy refuses this action.",
            Denial::Personal => {
                "This record is your own or belongs to someone you have a personal connection with. Use the customer app for your own policies; a colleague must handle this one."
            }
        }
    }
}

/// The answer, with everything a later audit needs to see why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub allowed: bool,
    pub denied: Option<Denial>,
    /// The template the action was allowed under.
    pub template: Option<String>,
    /// The authority profile and version.
    pub profile: Option<(String, u32)>,
    /// The grant relied on, if any.
    pub grant: Option<String>,
    /// The data classes the person holds, for shaping the response.
    pub classes: BTreeSet<DataClass>,
}

impl Decision {
    fn deny(reason: Denial) -> Self {
        Self {
            allowed: false,
            denied: Some(reason),
            template: None,
            profile: None,
            grant: None,
            classes: BTreeSet::new(),
        }
    }
}

/// One line of the access log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessRecord {
    pub at: At,
    pub staff: PartyId,
    pub action: String,
    pub class: DataClass,
    pub record: Option<String>,
    pub purpose: Option<Purpose>,
    pub decision: Decision,
    /// Why this line is outside the normal pattern, if it is: `no_path` for
    /// an attempt on a record the person had no path to, `personal` for
    /// their own record or a connected person's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flag: Option<String>,
}

/// What an external entitlement maps to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ElevationRule {
    /// `cover`, `profile_uplift` or `one_off`.
    pub kind: String,
    pub max_secs: i64,
}

/// Versioned configuration connecting an external entitlement source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mapping {
    pub version: u32,
    pub effective_from: At,
    /// External role or entitlement to templates.
    pub roles: BTreeMap<String, Vec<String>>,
    /// External elevation type to a grant kind and its cap.
    #[serde(default)]
    pub elevations: BTreeMap<String, ElevationRule>,
    /// Templates come from the external source.
    #[serde(default)]
    pub templates_external: bool,
    /// Grant kinds that come from the external source. The rest are local.
    #[serde(default)]
    pub grants_external: BTreeSet<String>,
    /// The member of staff who works the alignment report's exceptions,
    /// such as an entitlement with no mapping. Every mapping names one.
    #[serde(default)]
    pub alignment_owner: Option<PartyId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalHolding {
    pub person: PartyId,
    pub entitlement: String,
    pub reference: String,
    #[serde(default)]
    pub to: Option<At>,
}

/// Differences between the external source and the platform, each an
/// exception for someone to own.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alignment {
    /// External entitlements with no mapping. They confer nothing.
    pub unmapped: BTreeSet<String>,
    /// Templates no external entitlement maps to.
    pub templates_without_source: BTreeSet<String>,
    /// People holding a template locally that the source does not grant.
    pub local_only: BTreeSet<(PartyId, String)>,
    /// Who works these exceptions, as the mapping in force names them.
    #[serde(default)]
    pub owner: Option<PartyId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StaffCommand {
    /// The first administrator and templates, on an empty platform only.
    Bootstrap {
        templates: Vec<Template>,
        profiles: Vec<Profile>,
        admin: PartyId,
        admin_templates: Vec<String>,
    },
    DefineTemplate {
        template: Template,
        approved_by: PartyId,
    },
    ActivateProfile {
        profile: Profile,
        approved_by: PartyId,
    },
    DeclareConflict {
        conflict: Conflict,
        approved_by: PartyId,
    },
    /// A member of staff declares a personal connection: a relative, a
    /// friend, anyone whose records they must not handle.
    DeclareConnection {
        party: PartyId,
    },
    /// A staff administrator removes a connection that no longer holds.
    EndConnection {
        staff: PartyId,
        party: PartyId,
    },
    ChangeSettings {
        settings: Settings,
        approved_by: PartyId,
    },
    Employ {
        party: PartyId,
        team: String,
        manager: Option<PartyId>,
        templates: Vec<String>,
    },
    EndEmployment {
        party: PartyId,
    },
    AssignTemplate {
        party: PartyId,
        template: String,
    },
    RemoveTemplate {
        party: PartyId,
        template: String,
    },
    SetProfile {
        party: PartyId,
        profile: Option<String>,
        approved_by: PartyId,
    },
    AssignPath {
        party: PartyId,
        scope: String,
    },
    RemovePath {
        party: PartyId,
        scope: String,
    },
    RequestGrant {
        id: String,
        holder: PartyId,
        kind: GrantKind,
        reason: String,
        from: At,
        to: At,
    },
    ApproveGrant {
        id: String,
    },
    DeclineGrant {
        id: String,
    },
    RevokeGrant {
        id: String,
    },
    BreakGlass {
        id: String,
        template: String,
        reason: String,
    },
    ReviewBreakGlass {
        id: String,
    },
    Recertify {
        party: PartyId,
        /// The templates the manager confirms. Any other holding ends.
        confirmed: Vec<String>,
    },
    /// Write a decision to the access log, consume a one-off grant it relied
    /// on, and remember the act for separation of duties.
    Record {
        request: AccessRequest,
        decision: Decision,
    },
    ActivateMapping {
        mapping: Mapping,
        approved_by: PartyId,
    },
    /// What the external source says people hold. `full` is a reconciliation
    /// pull: anything the source no longer grants is revoked.
    ExternalSync {
        holdings: Vec<ExternalHolding>,
        full: bool,
    },
    /// Just-in-time elevation approved in the external tool.
    ExternalElevation {
        reference: String,
        person: PartyId,
        elevation: String,
        kind: GrantKind,
        to: At,
    },
    ExternalElevationEnded {
        reference: String,
    },
    ExternalSourceAvailable {
        available: bool,
    },
}

/// Something the rest of the platform must act on at once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StaffEvent {
    /// Close this person's sessions now: the priority revocation channel.
    SessionsRevoked {
        party: PartyId,
    },
    /// Break-glass was used. Raises an alert immediately.
    BreakGlassUsed {
        grant: String,
        party: PartyId,
        template: String,
        reason: String,
    },
    GrantRequested {
        grant: String,
        approver_action: String,
    },
    GrantChanged {
        grant: String,
        state: GrantState,
    },
    /// The external entitlement source went away or came back.
    ExternalSource {
        available: bool,
    },
    /// An attempt outside the normal pattern, for the people who watch for misuse.
    AccessFlagged {
        staff: PartyId,
        action: String,
        record: Option<String>,
        reason: String,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaffState {
    pub templates: BTreeMap<String, Template>,
    /// Every version of every profile, oldest first.
    pub profiles: BTreeMap<String, Vec<Profile>>,
    pub members: BTreeMap<PartyId, StaffMember>,
    pub paths: BTreeMap<PartyId, BTreeSet<String>>,
    pub grants: BTreeMap<String, Grant>,
    pub conflicts: BTreeSet<Conflict>,
    /// subject -> action -> who has taken it.
    pub acts: BTreeMap<String, BTreeMap<String, BTreeSet<PartyId>>>,
    pub settings: Settings,
    pub recent: BTreeMap<PartyId, VecDeque<AccessRecord>>,
    pub mapping: Option<Mapping>,
    /// False while the external source cannot be reached. Existing grants
    /// run to their end; nothing new is created from it.
    #[serde(default = "yes")]
    pub external_available: bool,
    #[serde(default)]
    pub unmapped: BTreeSet<String>,
    /// Staff member -> the people they have a personal connection with.
    #[serde(default)]
    pub connections: BTreeMap<PartyId, BTreeSet<PartyId>>,
}

fn yes() -> bool {
    true
}

fn reject(code: &str, message: impl Into<String>) -> Reject {
    Reject::new(code, message)
}

/// The facts behind one decision.
struct Facts {
    template: Option<String>,
    profile: Option<(String, u32)>,
    grant: Option<String>,
    classes: BTreeSet<DataClass>,
    what: bool,
    whose: bool,
    how_much: bool,
    why: bool,
    class_held: bool,
    conflict: bool,
    is_maker: bool,
    break_glass: bool,
}

impl StaffState {
    pub fn new() -> Self {
        Self {
            external_available: true,
            ..Self::default()
        }
    }

    fn current(&self, party: &PartyId, at: At) -> Option<&StaffMember> {
        self.members.get(party).filter(|m| m.started <= at && m.ended.is_none_or(|e| at < e))
    }

    /// The templates a person holds at a time: holdings in their window and
    /// confirmed by a manager recently enough.
    pub fn templates_of(&self, party: &PartyId, at: At) -> Vec<&Template> {
        let Some(m) = self.current(party, at) else { return vec![] };
        if at - m.recertified > self.settings.recertify_every {
            return vec![];
        }
        m.templates
            .iter()
            .filter(|(_, h)| h.active(at))
            .filter_map(|(id, _)| self.templates.get(id))
            .collect()
    }

    pub fn active_grants(&self, party: &PartyId, at: At) -> Vec<&Grant> {
        self.grants.values().filter(|g| &g.holder == party && g.active(at)).collect()
    }

    fn latest_profile(&self, id: &str) -> Option<&Profile> {
        self.profiles.get(id).and_then(|v| v.last())
    }

    /// Whether a person holds an action at a time, through a template.
    pub fn holds_action(&self, party: &PartyId, action: &str, at: At) -> bool {
        self.templates_of(party, at).iter().any(|t| t.actions.contains(action))
    }

    /// The most a limit may be for someone holding these templates.
    fn ceiling(templates: &[&Template], limit: &str) -> i64 {
        templates.iter().filter_map(|t| t.ceilings.get(limit)).copied().max().unwrap_or(0)
    }

    fn facts(&self, req: &AccessRequest) -> Option<Facts> {
        let member = self.current(&req.staff, req.at)?;
        let held = self.templates_of(&req.staff, req.at);
        let grants = self.active_grants(&req.staff, req.at);
        let mut classes: BTreeSet<DataClass> = held.iter().flat_map(|t| t.classes.iter().copied()).collect();
        let mut grant = None;

        // What.
        let mut template = held.iter().find(|t| t.actions.contains(&req.action)).map(|t| t.id.clone());
        let mut break_glass = false;
        let mut one_off = false;
        if template.is_none() {
            for g in &grants {
                match &g.kind {
                    GrantKind::OneOff { action, record } if action == &req.action && Some(record) == req.record.as_ref() => {
                        grant = Some(g.id.clone());
                        one_off = true;
                    }
                    GrantKind::BreakGlass { template: t } => {
                        if let Some(t) = self.templates.get(t).filter(|t| t.actions.contains(&req.action)) {
                            template = Some(t.id.clone());
                            grant = Some(g.id.clone());
                            break_glass = true;
                            // Break-glass widens what may be seen, never to clinical.
                            classes.extend(t.classes.iter().copied().filter(|c| *c != DataClass::Clinical));
                        }
                    }
                    _ => {}
                }
                if grant.is_some() {
                    break;
                }
            }
        }
        let what = template.is_some() || one_off;

        // Whose.
        let own = self.paths.get(&req.staff);
        let reaches = |paths: Option<&BTreeSet<String>>| paths.is_some_and(|p| req.scopes.iter().any(|s| p.contains(s)));
        let mut whose = req.scopes.is_empty() || reaches(own) || one_off || break_glass;
        if !whose {
            for g in &grants {
                if let GrantKind::Cover { covered } = &g.kind {
                    if reaches(self.paths.get(covered)) {
                        whose = true;
                        grant = Some(g.id.clone());
                        break;
                    }
                }
            }
        }

        // How much.
        let profile_id = member
            .profile
            .clone()
            .or_else(|| template.as_ref().and_then(|t| self.templates.get(t)).map(|t| t.default_profile.clone()))
            .or_else(|| held.first().map(|t| t.default_profile.clone()));
        let profile = profile_id.as_deref().and_then(|id| self.latest_profile(id));
        let how_much = match &req.amount {
            None => true,
            Some(amount) => {
                let base = profile.and_then(|p| p.limits.get(&amount.limit)).copied().unwrap_or(0);
                let mut limit = base;
                let mut via = None;
                for g in &grants {
                    if let GrantKind::ProfileUplift { limits } = &g.kind {
                        if let Some(l) = limits.get(&amount.limit).filter(|l| **l > limit) {
                            limit = *l;
                            via = Some(g.id.clone());
                        }
                    }
                }
                // The ceiling is the template's, whatever a grant says.
                let limit = limit.min(Self::ceiling(&held, &amount.limit));
                if amount.value > base && amount.value <= limit {
                    grant = via.or(grant);
                }
                amount.value <= limit
            }
        };

        // Why.
        let needs_purpose = req.decides || req.class != DataClass::Ordinary;
        let why = !needs_purpose || req.purpose.as_ref().is_some_and(|p| !p.reference.trim().is_empty());

        let class_held = classes.contains(&req.class);
        let conflict = req.conflict_subject.as_ref().is_some_and(|subject| {
            let done = self.acts.get(subject);
            self.conflicts.iter().any(|c| {
                let other = if c.a == req.action {
                    Some(&c.b)
                } else if c.b == req.action {
                    Some(&c.a)
                } else {
                    None
                };
                other.is_some_and(|o| done.and_then(|d| d.get(o)).is_some_and(|who| who.contains(&req.staff)))
            })
        });
        Some(Facts {
            template,
            profile: profile.map(|p| (p.id.clone(), p.version)),
            grant,
            classes,
            what,
            whose,
            how_much,
            why,
            class_held,
            conflict,
            is_maker: req.maker.as_ref() == Some(&req.staff),
            break_glass,
        })
    }

    /// Answer the four questions. Pure: nothing is recorded until the
    /// decision is logged with [`StaffCommand::Record`].
    pub fn authorise(&self, policy: &StaffPolicy, req: &AccessRequest) -> Decision {
        let Some(f) = self.facts(req) else {
            return Decision::deny(Denial::NotStaff);
        };
        let allowed = policy.decide(req, &f);
        let denied = if allowed {
            None
        } else if !f.what {
            Some(Denial::What)
        } else if !f.whose {
            Some(Denial::Whose)
        } else if !f.how_much {
            Some(Denial::HowMuch)
        } else if !f.why {
            Some(Denial::Why)
        } else if !f.class_held {
            Some(Denial::Class)
        } else if f.conflict {
            Some(Denial::Conflict)
        } else if f.is_maker {
            Some(Denial::MakerChecker)
        } else if req.class == DataClass::Clinical
            && (req.purpose.as_ref().map(|p| p.kind) != Some(PurposeKind::Task) || f.grant.is_some() || req.record.is_none())
        {
            Some(Denial::Why)
        } else if !req.fresh {
            Some(Denial::Stale)
        } else {
            Some(Denial::Policy)
        };
        Decision {
            allowed,
            denied,
            template: f.template,
            profile: f.profile,
            grant: f.grant,
            classes: f.classes,
        }
    }

    /// What a person sees of their own access.
    pub fn own_access(&self, party: &PartyId, at: At) -> serde_json::Value {
        let templates: Vec<&str> = self.templates_of(party, at).iter().map(|t| t.id.as_str()).collect();
        let member = self.current(party, at);
        let profile = member
            .and_then(|m| m.profile.clone())
            .or_else(|| self.templates_of(party, at).first().map(|t| t.default_profile.clone()))
            .and_then(|id| self.latest_profile(&id).cloned());
        json!({
            "staff": member.is_some(),
            "team": member.map(|m| m.team.clone()),
            "templates": templates,
            "profile": profile,
            "paths": self.paths.get(party).cloned().unwrap_or_default(),
            "grants": self.grants.values().filter(|g| &g.holder == party && (g.active(at) || g.state == GrantState::Requested)).collect::<Vec<_>>(),
            "recertification_due": member.map(|m| m.recertified + self.settings.recertify_every),
            "recent": self.recent.get(party).map(|r| r.iter().rev().collect::<Vec<_>>()).unwrap_or_default(),
            "connections": self.connections.get(party).cloned().unwrap_or_default(),
        })
    }

    /// Differences between the external source's view and the platform's.
    pub fn alignment(&self) -> Alignment {
        let mut out = Alignment {
            unmapped: self.unmapped.clone(),
            owner: self.mapping.as_ref().and_then(|m| m.alignment_owner.clone()),
            ..Alignment::default()
        };
        let Some(mapping) = self.mapping.as_ref().filter(|m| m.templates_external) else {
            return out;
        };
        let sourced: BTreeSet<&String> = mapping.roles.values().flatten().collect();
        out.templates_without_source = self.templates.keys().filter(|t| !sourced.contains(t)).cloned().collect();
        for m in self.members.values().filter(|m| m.ended.is_none()) {
            for (t, h) in &m.templates {
                if h.source == Source::BuiltIn && h.to.is_none() {
                    out.local_only.insert((m.party.clone(), t.clone()));
                }
            }
        }
        out
    }

    fn require_config_approval(&self, actor: &PartyId, approved_by: &PartyId, at: At) -> Result<(), Reject> {
        if actor == approved_by {
            return Err(reject("maker_checker", "configuration needs a second person: the proposer cannot approve"));
        }
        if !self.holds_action(actor, actions::CONFIGURE, at) {
            return Err(reject("forbidden", "the proposer may not change staff configuration"));
        }
        if !self.holds_action(approved_by, actions::APPROVE_CONFIGURATION, at) {
            return Err(reject("forbidden", "the approver may not approve staff configuration"));
        }
        Ok(())
    }

    fn require(&self, actor: &PartyId, action: &str, at: At) -> Result<(), Reject> {
        if self.holds_action(actor, action, at) {
            Ok(())
        } else {
            Err(reject("forbidden", format!("{action} is required")))
        }
    }

    fn check_templates(&self, ids: &[String]) -> Result<(), Reject> {
        match ids.iter().find(|t| !self.templates.contains_key(*t)) {
            Some(t) => Err(reject("unknown_template", format!("there is no template {t}"))),
            None => Ok(()),
        }
    }

    fn max_life(&self, kind: &str) -> i64 {
        self.settings.max_life.get(kind).copied().unwrap_or(0)
    }

    /// Check a grant against the ceilings. A grant never exceeds a template
    /// ceiling and never names something that does not exist.
    fn check_grant(&self, holder: &PartyId, kind: &GrantKind, from: At, to: At, at: At) -> Result<(), Reject> {
        if self.current(holder, at).is_none() {
            return Err(reject("not_staff", "the holder is not a current member of staff"));
        }
        if to <= from || to <= at {
            return Err(reject("bad_period", "a grant must end after it starts and after now"));
        }
        if to - from.max(at) > self.max_life(kind.key()) {
            return Err(reject(
                "too_long",
                format!("a {} grant may last at most {} seconds", kind.key(), self.max_life(kind.key())),
            ));
        }
        match kind {
            GrantKind::Cover { covered } => {
                if covered == holder || !self.members.contains_key(covered) {
                    return Err(reject("bad_cover", "cover is for another member of staff"));
                }
            }
            GrantKind::ProfileUplift { limits } => {
                let held = self.templates_of(holder, at);
                if let Some((name, value)) = limits.iter().find(|(name, value)| **value > Self::ceiling(&held, name)) {
                    return Err(reject(
                        "above_ceiling",
                        format!("{name} of {value} is above the ceiling for the holder's roles"),
                    ));
                }
            }
            GrantKind::OneOff { action, record } => {
                if action.trim().is_empty() || record.trim().is_empty() {
                    return Err(reject("bad_one_off", "a one-off grant names one action and one record"));
                }
            }
            GrantKind::BreakGlass { template } => {
                if !self.templates.get(template).is_some_and(|t| t.break_glass) {
                    return Err(reject("not_break_glass", "that template cannot be taken under break-glass"));
                }
            }
        }
        Ok(())
    }

    fn is_external(&self, kind: &str) -> bool {
        self.mapping.as_ref().is_some_and(|m| m.grants_external.contains(kind))
    }

    /// Apply a command sent by `actor` at `at`. A rejected command changes nothing.
    pub fn apply(&mut self, actor: &PartyId, at: At, command: StaffCommand) -> Result<Vec<StaffEvent>, Reject> {
        let mut events = Vec::new();
        match command {
            StaffCommand::Bootstrap {
                templates,
                profiles,
                admin,
                admin_templates,
            } => {
                if !self.members.is_empty() || !self.templates.is_empty() {
                    return Err(reject("already_bootstrapped", "staff access is already set up"));
                }
                for p in profiles {
                    self.profiles.entry(p.id.clone()).or_default().push(p);
                }
                for t in templates {
                    self.templates.insert(t.id.clone(), t);
                }
                if let Err(e) = self.check_templates(&admin_templates) {
                    *self = Self::new();
                    return Err(e);
                }
                self.members.insert(
                    admin.clone(),
                    StaffMember {
                        party: admin,
                        team: "administration".to_string(),
                        manager: None,
                        templates: admin_templates
                            .into_iter()
                            .map(|t| {
                                (
                                    t,
                                    Holding {
                                        from: at,
                                        to: None,
                                        source: Source::BuiltIn,
                                    },
                                )
                            })
                            .collect(),
                        profile: None,
                        started: at,
                        ended: None,
                        recertified: at,
                    },
                );
            }
            StaffCommand::DefineTemplate { template, approved_by } => {
                self.require_config_approval(actor, &approved_by, at)?;
                if !self.profiles.contains_key(&template.default_profile) {
                    return Err(reject("unknown_profile", "the template's default profile does not exist"));
                }
                self.templates.insert(template.id.clone(), template);
            }
            StaffCommand::ActivateProfile { profile, approved_by } => {
                self.require_config_approval(actor, &approved_by, at)?;
                let versions = self.profiles.entry(profile.id.clone()).or_default();
                let next = versions.last().map_or(1, |p| p.version + 1);
                if profile.version != next {
                    return Err(reject("bad_version", format!("the next version of {} is {next}", profile.id)));
                }
                versions.push(profile);
            }
            StaffCommand::DeclareConflict { conflict, approved_by } => {
                self.require_config_approval(actor, &approved_by, at)?;
                self.conflicts.insert(conflict);
            }
            StaffCommand::DeclareConnection { party } => {
                if self.current(actor, at).is_none() {
                    return Err(reject("forbidden", "only current staff declare a connection"));
                }
                self.connections.entry(actor.clone()).or_default().insert(party);
            }
            StaffCommand::EndConnection { staff, party } => {
                // Declaring is the person's own act; undoing it is not, or the declaration would mean nothing.
                if &staff == actor || !self.holds_action(actor, actions::MANAGE_STAFF, at) {
                    return Err(reject(
                        "forbidden",
                        "a connection is removed by a staff administrator, never by the person it concerns",
                    ));
                }
                if !self.connections.get_mut(&staff).is_some_and(|c| c.remove(&party)) {
                    return Err(reject("unknown_connection", "no such connection is declared"));
                }
            }
            StaffCommand::ChangeSettings { settings, approved_by } => {
                self.require_config_approval(actor, &approved_by, at)?;
                self.settings = settings;
            }
            StaffCommand::Employ {
                party,
                team,
                manager,
                templates,
            } => {
                self.require(actor, actions::MANAGE_STAFF, at)?;
                self.check_templates(&templates)?;
                if self.current(&party, at).is_some() {
                    return Err(reject("already_staff", "that person is already a member of staff"));
                }
                let holding = Holding {
                    from: at,
                    to: None,
                    source: Source::BuiltIn,
                };
                self.members.insert(
                    party.clone(),
                    StaffMember {
                        party,
                        team,
                        manager,
                        templates: templates.into_iter().map(|t| (t, holding.clone())).collect(),
                        profile: None,
                        started: at,
                        ended: None,
                        recertified: at,
                    },
                );
            }
            StaffCommand::EndEmployment { party } => {
                self.require(actor, actions::MANAGE_STAFF, at)?;
                let member = self.members.get_mut(&party).ok_or_else(|| reject("not_staff", "no such member of staff"))?;
                // Leaving ends every relationship on the leaving date.
                member.ended = Some(at);
                for h in member.templates.values_mut() {
                    h.to = Some(h.to.map_or(at, |t| t.min(at)));
                }
                self.paths.remove(&party);
                for g in self
                    .grants
                    .values_mut()
                    .filter(|g| g.holder == party && matches!(g.state, GrantState::Active | GrantState::Requested))
                {
                    g.state = GrantState::Revoked;
                    events.push(StaffEvent::GrantChanged {
                        grant: g.id.clone(),
                        state: GrantState::Revoked,
                    });
                }
                events.push(StaffEvent::SessionsRevoked { party });
            }
            StaffCommand::AssignTemplate { party, template } => {
                self.require(actor, actions::MANAGE_STAFF, at)?;
                self.check_templates(std::slice::from_ref(&template))?;
                if self.mapping.as_ref().is_some_and(|m| m.templates_external && m.effective_from <= at) {
                    return Err(reject("external_source", "templates come from the external entitlement source"));
                }
                if &party == actor {
                    return Err(reject("self_service", "nobody assigns a role to themselves"));
                }
                let member = self
                    .members
                    .get_mut(&party)
                    .filter(|m| m.ended.is_none())
                    .ok_or_else(|| reject("not_staff", "no such member of staff"))?;
                member.templates.insert(
                    template,
                    Holding {
                        from: at,
                        to: None,
                        source: Source::BuiltIn,
                    },
                );
            }
            StaffCommand::RemoveTemplate { party, template } => {
                self.require(actor, actions::MANAGE_STAFF, at)?;
                let holding = self
                    .members
                    .get_mut(&party)
                    .and_then(|m| m.templates.get_mut(&template))
                    .ok_or_else(|| reject("not_held", "that person does not hold that template"))?;
                holding.to = Some(at);
                events.push(StaffEvent::SessionsRevoked { party });
            }
            StaffCommand::SetProfile { party, profile, approved_by } => {
                // A profile changes with the manager's and the function head's approval.
                self.require(actor, actions::MANAGE_STAFF, at)?;
                self.require(&approved_by, actions::APPROVE_PROFILE_UPLIFT, at)?;
                if actor == &approved_by || &party == actor || party == approved_by {
                    return Err(reject("maker_checker", "a profile change needs two people, neither of them its subject"));
                }
                if let Some(p) = &profile {
                    let chosen = self.latest_profile(p).ok_or_else(|| reject("unknown_profile", "no such profile"))?;
                    let held = self.templates_of(&party, at);
                    if let Some((name, value)) = chosen.limits.iter().find(|(name, value)| **value > Self::ceiling(&held, name)) {
                        return Err(reject(
                            "above_ceiling",
                            format!("{name} of {value} is above the ceiling for the person's roles"),
                        ));
                    }
                }
                let member = self
                    .members
                    .get_mut(&party)
                    .filter(|m| m.ended.is_none())
                    .ok_or_else(|| reject("not_staff", "no such member of staff"))?;
                member.profile = profile;
            }
            StaffCommand::AssignPath { party, scope } => {
                self.require(actor, actions::MANAGE_STAFF, at)?;
                if self.current(&party, at).is_none() {
                    return Err(reject("not_staff", "no such member of staff"));
                }
                self.paths.entry(party).or_default().insert(scope);
            }
            StaffCommand::RemovePath { party, scope } => {
                self.require(actor, actions::MANAGE_STAFF, at)?;
                if let Some(p) = self.paths.get_mut(&party) {
                    p.remove(&scope);
                }
            }
            StaffCommand::RequestGrant {
                id,
                holder,
                kind,
                reason,
                from,
                to,
            } => {
                if self.grants.contains_key(&id) {
                    return Err(reject("duplicate", "a grant with that ID exists"));
                }
                if matches!(kind, GrantKind::BreakGlass { .. }) {
                    return Err(reject("bad_kind", "break-glass is taken, not requested"));
                }
                if self.is_external(kind.key()) {
                    return Err(reject("external_source", "grants of this kind come from the external entitlement source"));
                }
                if self.current(actor, at).is_none() {
                    return Err(reject("not_staff", "only staff request grants"));
                }
                if reason.trim().is_empty() {
                    return Err(reject("no_reason", "a grant needs a reason"));
                }
                self.check_grant(&holder, &kind, from, to, at)?;
                let approver_action = match kind {
                    GrantKind::ProfileUplift { .. } => actions::APPROVE_PROFILE_UPLIFT,
                    _ => actions::APPROVE_GRANT,
                };
                events.push(StaffEvent::GrantRequested {
                    grant: id.clone(),
                    approver_action: approver_action.to_string(),
                });
                self.grants.insert(
                    id.clone(),
                    Grant {
                        id,
                        holder,
                        kind,
                        reason,
                        requested_by: actor.clone(),
                        requested_at: at,
                        approver: None,
                        from,
                        to,
                        state: GrantState::Requested,
                        source: Source::BuiltIn,
                        reviewed: None,
                    },
                );
            }
            StaffCommand::ApproveGrant { id } => {
                let g = self
                    .grants
                    .get(&id)
                    .filter(|g| g.state == GrantState::Requested)
                    .ok_or_else(|| reject("not_requested", "no such grant awaiting approval"))?;
                if &g.holder == actor || &g.requested_by == actor {
                    return Err(reject("maker_checker", "a grant is approved by someone other than its holder and requester"));
                }
                let needed = match g.kind {
                    GrantKind::ProfileUplift { .. } => actions::APPROVE_PROFILE_UPLIFT,
                    _ => actions::APPROVE_GRANT,
                };
                self.require(actor, needed, at)?;
                // Ceilings are checked again: roles may have changed since the request.
                self.check_grant(&g.holder, &g.kind, g.from, g.to, at)?;
                if let Some(g) = self.grants.get_mut(&id) {
                    g.state = GrantState::Active;
                    g.approver = Some(actor.clone());
                }
                events.push(StaffEvent::GrantChanged {
                    grant: id,
                    state: GrantState::Active,
                });
            }
            StaffCommand::DeclineGrant { id } => {
                let g = self
                    .grants
                    .get(&id)
                    .filter(|g| g.state == GrantState::Requested)
                    .ok_or_else(|| reject("not_requested", "no such grant awaiting approval"))?;
                let needed = match g.kind {
                    GrantKind::ProfileUplift { .. } => actions::APPROVE_PROFILE_UPLIFT,
                    _ => actions::APPROVE_GRANT,
                };
                if &g.requested_by != actor {
                    self.require(actor, needed, at)?;
                }
                if let Some(g) = self.grants.get_mut(&id) {
                    g.state = GrantState::Declined;
                    g.approver = Some(actor.clone());
                }
                events.push(StaffEvent::GrantChanged {
                    grant: id,
                    state: GrantState::Declined,
                });
            }
            StaffCommand::RevokeGrant { id } => {
                let g = self
                    .grants
                    .get(&id)
                    .filter(|g| g.state == GrantState::Active)
                    .ok_or_else(|| reject("not_active", "no such active grant"))?;
                if &g.holder != actor && self.members.get(&g.holder).and_then(|m| m.manager.as_ref()) != Some(actor) {
                    self.require(actor, actions::APPROVE_GRANT, at)?;
                }
                let holder = g.holder.clone();
                if let Some(g) = self.grants.get_mut(&id) {
                    g.state = GrantState::Revoked;
                }
                events.push(StaffEvent::GrantChanged {
                    grant: id,
                    state: GrantState::Revoked,
                });
                events.push(StaffEvent::SessionsRevoked { party: holder });
            }
            StaffCommand::BreakGlass { id, template, reason } => {
                if self.grants.contains_key(&id) {
                    return Err(reject("duplicate", "a grant with that ID exists"));
                }
                if reason.trim().is_empty() {
                    return Err(reject("no_reason", "break-glass needs a reason"));
                }
                let kind = GrantKind::BreakGlass { template: template.clone() };
                let to = at + self.max_life("break_glass");
                self.check_grant(actor, &kind, at, to, at)?;
                events.push(StaffEvent::BreakGlassUsed {
                    grant: id.clone(),
                    party: actor.clone(),
                    template,
                    reason: reason.clone(),
                });
                self.grants.insert(
                    id.clone(),
                    Grant {
                        id,
                        holder: actor.clone(),
                        kind,
                        reason,
                        requested_by: actor.clone(),
                        requested_at: at,
                        approver: None,
                        from: at,
                        to,
                        state: GrantState::Active,
                        source: Source::BuiltIn,
                        reviewed: None,
                    },
                );
            }
            StaffCommand::ReviewBreakGlass { id } => {
                self.require(actor, actions::REVIEW_BREAK_GLASS, at)?;
                let g = self
                    .grants
                    .get_mut(&id)
                    .filter(|g| matches!(g.kind, GrantKind::BreakGlass { .. }) && g.reviewed.is_none())
                    .ok_or_else(|| reject("not_reviewable", "no such break-glass use awaiting review"))?;
                if &g.holder == actor {
                    return Err(reject("maker_checker", "break-glass is reviewed by someone else"));
                }
                g.reviewed = Some((actor.clone(), at));
            }
            StaffCommand::Recertify { party, confirmed } => {
                let member = self
                    .members
                    .get_mut(&party)
                    .filter(|m| m.ended.is_none())
                    .ok_or_else(|| reject("not_staff", "no such member of staff"))?;
                if member.manager.as_ref() != Some(actor) {
                    return Err(reject("forbidden", "only the person's manager recertifies their access"));
                }
                // Anything unconfirmed lapses.
                for (id, h) in member.templates.iter_mut() {
                    if h.active(at) && !confirmed.contains(id) {
                        h.to = Some(at);
                    }
                }
                member.recertified = at;
                events.push(StaffEvent::SessionsRevoked { party });
            }
            StaffCommand::Record { request, decision } => {
                if &request.staff != actor {
                    return Err(reject("forbidden", "a decision is recorded for the person who made the request"));
                }
                if decision.allowed {
                    if let Some(g) = decision.grant.as_ref().and_then(|id| self.grants.get_mut(id)) {
                        if matches!(g.kind, GrantKind::OneOff { .. }) {
                            if g.state != GrantState::Active {
                                return Err(reject("consumed", "that one-off grant has been used"));
                            }
                            g.state = GrantState::Consumed;
                            events.push(StaffEvent::GrantChanged {
                                grant: g.id.clone(),
                                state: GrantState::Consumed,
                            });
                        }
                    }
                    if let (Some(subject), true) = (&request.conflict_subject, request.decides) {
                        self.acts
                            .entry(subject.clone())
                            .or_default()
                            .entry(request.action.clone())
                            .or_default()
                            .insert(request.staff.clone());
                    }
                }
                let flag = match decision.denied {
                    Some(Denial::Whose) => Some("no_path"),
                    Some(Denial::Personal) => Some("personal"),
                    _ => None,
                };
                if let Some(reason) = flag {
                    events.push(StaffEvent::AccessFlagged {
                        staff: request.staff.clone(),
                        action: request.action.clone(),
                        record: request.record.clone(),
                        reason: reason.to_string(),
                    });
                }
                let recent = self.recent.entry(request.staff.clone()).or_default();
                recent.push_back(AccessRecord {
                    at,
                    staff: request.staff,
                    action: request.action,
                    class: request.class,
                    record: request.record,
                    purpose: request.purpose,
                    decision,
                    flag: flag.map(str::to_string),
                });
                while recent.len() > ACCESS_VIEW_LEN {
                    recent.pop_front();
                }
            }
            StaffCommand::ActivateMapping { mapping, approved_by } => {
                self.require_config_approval(actor, &approved_by, at)?;
                let next = self.mapping.as_ref().map_or(1, |m| m.version + 1);
                if mapping.version != next {
                    return Err(reject("bad_version", format!("the next mapping version is {next}")));
                }
                let mapped: Vec<String> = mapping.roles.values().flatten().cloned().collect();
                self.check_templates(&mapped)?;
                if let Some((name, _)) = mapping
                    .elevations
                    .iter()
                    .find(|(_, r)| !["cover", "profile_uplift", "one_off"].contains(&r.kind.as_str()) || r.max_secs <= 0)
                {
                    return Err(reject("bad_elevation", format!("elevation {name} maps to no grant kind")));
                }
                let owned = mapping
                    .alignment_owner
                    .as_ref()
                    .and_then(|o| self.members.get(o))
                    .is_some_and(|m| m.ended.is_none());
                if !owned {
                    return Err(reject(
                        "no_alignment_owner",
                        "a mapping names a member of staff who works the alignment report's exceptions",
                    ));
                }
                self.mapping = Some(mapping);
            }
            StaffCommand::ExternalSync { holdings, full } => {
                let mapping = self
                    .mapping
                    .clone()
                    .filter(|m| m.templates_external && m.effective_from <= at)
                    .ok_or_else(|| reject("no_mapping", "no mapping makes an external source the source of templates"))?;
                let mut granted: BTreeMap<PartyId, BTreeMap<String, (String, Option<At>)>> = BTreeMap::new();
                let mut unmapped = BTreeSet::new();
                for h in holdings {
                    match mapping.roles.get(&h.entitlement) {
                        // Anything unmapped confers nothing.
                        None => {
                            unmapped.insert(h.entitlement);
                        }
                        Some(templates) => {
                            let person = granted.entry(h.person).or_default();
                            for t in templates {
                                person.insert(t.clone(), (h.reference.clone(), h.to));
                            }
                        }
                    }
                }
                if full {
                    self.unmapped = unmapped;
                } else {
                    self.unmapped.extend(unmapped);
                }
                for member in self.members.values_mut().filter(|m| m.ended.is_none()) {
                    let now_granted = granted.remove(&member.party).unwrap_or_default();
                    let mut lost = false;
                    if full {
                        // Anything the source no longer grants is revoked.
                        for (id, h) in member.templates.iter_mut() {
                            if matches!(h.source, Source::External { .. }) && h.active(at) && !now_granted.contains_key(id) {
                                h.to = Some(at);
                                lost = true;
                            }
                        }
                    }
                    for (template, (reference, to)) in now_granted {
                        let source = Source::External { reference };
                        match member.templates.get_mut(&template).filter(|h| h.active(at)) {
                            // The source owns the holding and its duration, including one
                            // that was assigned here before the source took over.
                            Some(h) => {
                                h.source = source;
                                h.to = to;
                            }
                            None => {
                                member.templates.insert(template, Holding { from: at, to, source });
                            }
                        }
                    }
                    if lost {
                        events.push(StaffEvent::SessionsRevoked { party: member.party.clone() });
                    }
                }
            }
            StaffCommand::ExternalElevation {
                reference,
                person,
                elevation,
                kind,
                to,
            } => {
                let mapping = self.mapping.as_ref().ok_or_else(|| reject("no_mapping", "no external source is mapped"))?;
                let rule = mapping
                    .elevations
                    .get(&elevation)
                    .ok_or_else(|| reject("unmapped", "that elevation type maps to nothing, so it confers nothing"))?;
                if rule.kind != kind.key() || !mapping.grants_external.contains(kind.key()) {
                    return Err(reject("bad_kind", "the elevation does not map to that grant kind"));
                }
                if !self.external_available {
                    return Err(reject("external_source", "the external source is unavailable: elevation fails closed"));
                }
                let id = format!("ext:{reference}");
                if self.grants.contains_key(&id) {
                    return Err(reject("duplicate", "that elevation has already been applied"));
                }
                // The platform may shorten the source's duration, never lengthen it.
                let to = to.min(at + rule.max_secs).min(at + self.max_life(kind.key()));
                self.check_grant(&person, &kind, at, to, at)?;
                self.grants.insert(
                    id.clone(),
                    Grant {
                        id: id.clone(),
                        holder: person.clone(),
                        kind,
                        reason: format!("external elevation {elevation}"),
                        requested_by: person,
                        requested_at: at,
                        approver: None,
                        from: at,
                        to,
                        state: GrantState::Active,
                        source: Source::External { reference },
                        reviewed: None,
                    },
                );
                events.push(StaffEvent::GrantChanged {
                    grant: id,
                    state: GrantState::Active,
                });
            }
            StaffCommand::ExternalElevationEnded { reference } => {
                let id = format!("ext:{reference}");
                let g = self
                    .grants
                    .get_mut(&id)
                    .filter(|g| g.state == GrantState::Active)
                    .ok_or_else(|| reject("not_active", "no such active elevation"))?;
                g.state = GrantState::Revoked;
                events.push(StaffEvent::GrantChanged {
                    grant: id,
                    state: GrantState::Revoked,
                });
                events.push(StaffEvent::SessionsRevoked { party: g.holder.clone() });
            }
            StaffCommand::ExternalSourceAvailable { available } => {
                if self.external_available != available {
                    self.external_available = available;
                    events.push(StaffEvent::ExternalSource { available });
                }
            }
        }
        Ok(events)
    }
}

/// The Cedar policy that decides staff actions.
pub struct StaffPolicy {
    policies: PolicySet,
    authorizer: Authorizer,
}

impl std::fmt::Debug for StaffPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StaffPolicy").finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("staff policies do not parse: {0}")]
pub struct PolicyError(pub String);

impl StaffPolicy {
    pub fn new(policies: &str) -> Result<Self, PolicyError> {
        Ok(Self {
            policies: PolicySet::from_str(policies).map_err(|e| PolicyError(e.to_string()))?,
            authorizer: Authorizer::new(),
        })
    }

    pub fn shipped() -> Result<Self, PolicyError> {
        Self::new(DEFAULT_POLICIES)
    }

    /// Decide from the facts. Anything that cannot be expressed to Cedar is a denial.
    fn decide(&self, req: &AccessRequest, f: &Facts) -> bool {
        let uid = |ty: &str, id: &str| {
            EntityTypeName::from_str(ty)
                .ok()
                .map(|t| EntityUid::from_type_name_and_id(t, EntityId::new(id)))
        };
        let (Some(principal), Some(action), Some(resource)) = (
            uid("Staff", &req.staff.0),
            uid("Action", &req.action),
            uid("Record", req.record.as_deref().unwrap_or("none")),
        ) else {
            return false;
        };
        let context = Context::from_json_value(
            json!({
                "what": f.what,
                "whose": f.whose,
                "how_much": f.how_much,
                "why": f.why,
                "class_held": f.class_held,
                "class": req.class.key(),
                "purpose_kind": req.purpose.as_ref().map_or("none", |p| p.kind.key()),
                "conflict": f.conflict,
                "is_maker": f.is_maker,
                "break_glass": f.break_glass,
                "granted": f.grant.is_some(),
                "one_record": req.record.is_some(),
                "decides": req.decides,
                "fresh": req.fresh,
            }),
            None,
        );
        let Ok(context) = context else { return false };
        let Ok(request) = Request::new(principal, action, resource, context, None) else {
            return false;
        };
        self.authorizer.is_authorized(&request, &self.policies, &Entities::empty()).decision() == CedarDecision::Allow
    }
}
