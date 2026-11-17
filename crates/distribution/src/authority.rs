//! Staff authorisation for this module: roles as templates, an authority
//! profile that says how much a person may decide, grants that sit on top
//! for a period, and the four questions asked on every action.
//!
//! The policy is Cedar, shipped in `config/cedar/distribution.cedar`. This
//! file turns a caller and an action into a Cedar request and records what
//! the decision relied on, so a later question about whether someone was
//! allowed to do something is answered from the record.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use cedar_policy::{Authorizer, Context, Decision, Entities, EntityId, EntityTypeName, EntityUid, PolicySet, Request};
use chrono::NaiveDate;
use ic_core::PartyId;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// The policies shipped with the platform.
pub const SHIPPED_POLICIES: &str = include_str!("../../../config/cedar/distribution.cedar");

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Manages advisers, providers, engagements and payees.
    DistributionAdmin,
    /// Proposes rules, schedules, books and statements.
    CommissionOps,
    CommissionApprover,
    Compliance,
    /// Prepares workbench actions. Holds the financial data class.
    CounterpartyFinance,
    CounterpartyFinanceApprover,
    Legal,
    ConductOfficer,
}

impl Role {
    pub fn key(self) -> &'static str {
        match self {
            Role::DistributionAdmin => "distribution_admin",
            Role::CommissionOps => "commission_ops",
            Role::CommissionApprover => "commission_approver",
            Role::Compliance => "compliance",
            Role::CounterpartyFinance => "counterparty_finance",
            Role::CounterpartyFinanceApprover => "counterparty_finance_approver",
            Role::Legal => "legal",
            Role::ConductOfficer => "conduct_officer",
        }
    }
    /// The data classes the role is granted. None of these roles holds a
    /// customer's personal, insurance-sensitive or clinical data.
    pub fn data_classes(self) -> &'static [&'static str] {
        match self {
            Role::DistributionAdmin | Role::Compliance | Role::ConductOfficer => &["ordinary"],
            _ => &["ordinary", "financial"],
        }
    }
}

/// The kinds of amount an authority profile limits, in minor units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitKind {
    Release,
    Offset,
    WriteOff,
    Split,
    Dividend,
    /// Book-level items and other commission adjustments.
    Adjustment,
}

/// How much a person may decide. Versioned configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityProfile {
    pub version: u32,
    pub limits: BTreeMap<LimitKind, i64>,
}

impl AuthorityProfile {
    pub fn limit(&self, kind: LimitKind) -> i64 {
        self.limits.get(&kind).copied().unwrap_or(0)
    }
}

/// A role template: the baseline everyone holds at least one of. The ceiling
/// bounds what any profile or uplift can reach for that template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Template {
    pub name: String,
    pub roles: BTreeSet<Role>,
    pub default_profile: AuthorityProfile,
    pub ceiling: BTreeMap<LimitKind, i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantKind {
    /// Inherit another person's paths (their counterparties and books) for a
    /// period. Their authority is not inherited.
    Cover { of: PartyId },
    /// Raise limits for a period. Capped by the template's ceiling.
    ProfileUplift { limits: BTreeMap<LimitKind, i64> },
    /// One named action on one record, consumed on use.
    OneOff { action: String, record: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub id: String,
    pub kind: GrantKind,
    pub reason: String,
    pub approver: PartyId,
    pub from: NaiveDate,
    /// The last day the grant is good for.
    pub until: NaiveDate,
    /// The reference in an external identity governance system, if it came from one.
    #[serde(default)]
    pub external_ref: Option<String>,
}

impl Grant {
    pub fn active(&self, day: NaiveDate) -> bool {
        self.from <= day && day <= self.until
    }
}

/// Who is calling, as the gateway's internal token describes them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Staff {
    pub id: PartyId,
    pub templates: Vec<Template>,
    #[serde(default)]
    pub grants: Vec<Grant>,
    /// The records the person has a path to: counterparties and books they
    /// are assigned. `*` is an assignment to the whole workbench queue.
    pub paths: BTreeSet<String>,
    /// The case, task or reason code for this call.
    pub purpose: String,
}

/// What a decision relied on. Stored with the decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionRecord {
    pub staff: PartyId,
    pub action: String,
    pub roles: Vec<Role>,
    pub profile_version: u32,
    pub limit_minor: i64,
    pub purpose: String,
    /// Grants the decision relied on.
    pub grants: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthorityError {
    #[error("cedar policies do not parse: {0}")]
    Policies(String),
    #[error("could not build the authorisation request: {0}")]
    Request(String),
    /// The reason is shown to the person, so an authority gap becomes a
    /// request to a manager rather than a workaround.
    #[error("not authorised: {0}")]
    Denied(String),
}

/// What is being asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask<'a> {
    pub action: &'a str,
    /// The record: a party ID, a book, a change set.
    pub record: &'a str,
    pub amount_minor: i64,
    pub limit: Option<LimitKind>,
    /// The caller prepared the thing they are now asked to approve.
    pub is_maker: bool,
}

impl<'a> Ask<'a> {
    pub fn new(action: &'a str, record: &'a str) -> Self {
        Self {
            action,
            record,
            amount_minor: 0,
            limit: None,
            is_maker: false,
        }
    }
    pub fn amount(mut self, kind: LimitKind, amount_minor: i64) -> Self {
        self.limit = Some(kind);
        self.amount_minor = amount_minor;
        self
    }
    pub fn maker(mut self, is_maker: bool) -> Self {
        self.is_maker = is_maker;
        self
    }
}

impl Staff {
    pub fn roles(&self) -> BTreeSet<Role> {
        self.templates.iter().flat_map(|t| t.roles.iter().copied()).collect()
    }

    /// The limit for a kind of amount on a day: the best of the templates'
    /// profiles, raised by any active uplift, and never above a ceiling.
    pub fn limit(&self, kind: LimitKind, day: NaiveDate) -> (i64, u32, Vec<String>) {
        let mut best = 0;
        let mut version = 0;
        let mut ceiling = 0;
        for t in &self.templates {
            let l = t.default_profile.limit(kind);
            if l >= best {
                best = l;
                version = t.default_profile.version;
            }
            ceiling = ceiling.max(t.ceiling.get(&kind).copied().unwrap_or(l));
        }
        let mut used = vec![];
        for g in self.grants.iter().filter(|g| g.active(day)) {
            if let GrantKind::ProfileUplift { limits } = &g.kind {
                if let Some(l) = limits.get(&kind).copied().filter(|l| *l > best) {
                    best = l;
                    used.push(g.id.clone());
                }
            }
        }
        (best.min(ceiling), version, used)
    }

    fn path_to(&self, record: &str, action: &str, day: NaiveDate) -> (bool, Vec<String>) {
        if self.paths.contains("*") || self.paths.contains(record) {
            return (true, vec![]);
        }
        for g in self.grants.iter().filter(|g| g.active(day)) {
            match &g.kind {
                GrantKind::OneOff { action: a, record: r } if a == action && r == record => return (true, vec![g.id.clone()]),
                _ => {}
            }
        }
        (false, vec![])
    }
}

pub struct Authority {
    policies: PolicySet,
    authorizer: Authorizer,
}

impl Authority {
    pub fn new(policies: &str) -> Result<Self, AuthorityError> {
        Ok(Self {
            policies: PolicySet::from_str(policies).map_err(|e| AuthorityError::Policies(e.to_string()))?,
            authorizer: Authorizer::new(),
        })
    }

    pub fn shipped() -> Result<Self, AuthorityError> {
        Self::new(SHIPPED_POLICIES)
    }

    /// Ask the four questions. `covered` lists the paths of people this
    /// caller is covering under a grant, resolved by the caller of this
    /// function from its own records.
    pub fn decide(&self, staff: &Staff, ask: &Ask<'_>, day: NaiveDate, covered: &BTreeSet<String>) -> Result<DecisionRecord, AuthorityError> {
        let err = |e: String| AuthorityError::Request(e);
        let uid = |ty: &str, id: &str| -> Result<EntityUid, AuthorityError> {
            Ok(EntityUid::from_type_name_and_id(
                EntityTypeName::from_str(ty).map_err(|e| err(e.to_string()))?,
                EntityId::new(id),
            ))
        };
        let roles = staff.roles();
        let (limit, profile_version, mut grants) = match ask.limit {
            Some(kind) => staff.limit(kind, day),
            None => (0, staff.templates.iter().map(|t| t.default_profile.version).max().unwrap_or(0), vec![]),
        };
        let (own_path, path_grants) = staff.path_to(ask.record, ask.action, day);
        let cover_grant = staff
            .grants
            .iter()
            .find(|g| g.active(day) && matches!(g.kind, GrantKind::Cover { .. }) && (covered.contains("*") || covered.contains(ask.record)));
        let has_path = own_path || cover_grant.is_some();
        grants.extend(path_grants);
        if !own_path {
            grants.extend(cover_grant.map(|g| g.id.clone()));
        }
        let entities = Entities::from_json_value(
            json!([
                { "uid": { "type": "Staff", "id": staff.id.0 }, "attrs": { "roles": roles.iter().map(|r| r.key()).collect::<Vec<_>>() }, "parents": [] },
                { "uid": { "type": "Record", "id": ask.record }, "attrs": {}, "parents": [] }
            ]),
            None,
        )
        .map_err(|e| err(e.to_string()))?;
        let allowed = |amount: i64| -> Result<bool, AuthorityError> {
            let context = Context::from_json_value(
                json!({
                    "amount": amount,
                    "limit": limit,
                    "purpose": staff.purpose.trim(),
                    "has_path": has_path,
                    "is_maker": ask.is_maker,
                }),
                None,
            )
            .map_err(|e| err(e.to_string()))?;
            let request = Request::new(
                uid("Staff", &staff.id.0)?,
                uid("Action", ask.action)?,
                uid("Record", ask.record)?,
                context,
                None,
            )
            .map_err(|e| err(e.to_string()))?;
            Ok(self.authorizer.is_authorized(&request, &self.policies, &entities).decision() == Decision::Allow)
        };
        if allowed(ask.amount_minor)? {
            return Ok(DecisionRecord {
                staff: staff.id.clone(),
                action: ask.action.to_string(),
                roles: roles.into_iter().collect(),
                profile_version,
                limit_minor: limit,
                purpose: staff.purpose.trim().to_string(),
                grants,
            });
        }
        // Say which question failed, most basic first.
        let why = if staff.purpose.trim().is_empty() {
            "a purpose must be declared".to_string()
        } else if !has_path {
            format!("you have no path to {}", ask.record)
        } else if ask.is_maker {
            "you prepared this, so someone else must approve it".to_string()
        } else if ask.limit.is_some() && ask.amount_minor > limit && allowed(limit)? {
            // The same action for an amount inside the limit would be allowed.
            format!("the amount {} is above your limit of {limit}", ask.amount_minor)
        } else if let Some(conflict) = conflict(ask.action, &roles) {
            conflict.to_string()
        } else {
            match described(ask.action) {
                Some((what, who)) => format!("none of your roles allows {} ({what}): that is done by {who}", ask.action),
                None => format!("none of your roles allows {}", ask.action),
            }
        };
        Err(AuthorityError::Denied(why))
    }
}

/// A staff action in words, and who does it, for a refusal people read.
fn described(action: &str) -> Option<(&'static str, &'static str)> {
    Some(match action {
        "structure.manage" => ("changing the distribution structure", "the distribution administrator"),
        "payee.manage" => ("managing payees and their accounts", "the distribution administrator"),
        "payee.hold" => ("holding or releasing a payee's payments", "the distribution administrator or compliance"),
        "licence.record" => ("recording a licence state", "compliance or the distribution administrator"),
        "config.propose" => ("proposing or withdrawing a change to the commission rules", "commission operations"),
        "statement.cut" => ("cutting a payee's statement", "commission operations"),
        "policy.assign" => ("assigning business to an adviser", "commission operations"),
        "config.approve" => ("approving a change to the commission rules", "a commission approver"),
        "config.approve_compliance" => ("approving a change to the commission rules as compliance", "compliance"),
        "workbench.prepare" => ("preparing a held balance action", "counterparty finance"),
        "workbench.approve" => ("approving a held balance action", "a counterparty finance approver"),
        "workbench.approve_legal" => ("approving a held balance action as legal", "legal"),
        "workbench.approve_conduct" => ("approving a held balance action as the conduct officer", "the conduct officer"),
        "workbench.view" | "workbench.document" => (
            "working the held balance workbench",
            "counterparty finance, its approvers, legal or the conduct officer",
        ),
        "payee.restricted_instruction" => ("entering a court order or statutory deduction", "legal"),
        "case.manage" => ("working a conduct case", "the conduct officer or compliance"),
        _ => return None,
    })
}

/// A declared conflict of duties that refuses the action whatever else the person holds.
fn conflict(action: &str, roles: &BTreeSet<Role>) -> Option<&'static str> {
    match action {
        "config.approve" if roles.contains(&Role::DistributionAdmin) => {
            Some("someone who manages the distribution structure does not approve what it is paid: another commission approver must")
        }
        "workbench.approve" if roles.contains(&Role::CounterpartyFinance) => Some(
            "someone who prepares held balance actions does not approve them, even another person's: a counterparty finance approver who prepares none must",
        ),
        _ => None,
    }
}

/// The templates shipped for this module. Limits are business settings; the
/// figures here are the defaults a function head signs off or changes.
pub fn shipped_templates() -> Vec<Template> {
    let limits = |pairs: &[(LimitKind, i64)]| pairs.iter().copied().collect::<BTreeMap<_, _>>();
    let all = |v: i64| {
        limits(&[
            (LimitKind::Release, v),
            (LimitKind::Offset, v),
            (LimitKind::WriteOff, v),
            (LimitKind::Split, v),
            (LimitKind::Dividend, v),
            (LimitKind::Adjustment, v),
        ])
    };
    let template = |name: &str, roles: &[Role], profile: BTreeMap<LimitKind, i64>, ceiling: BTreeMap<LimitKind, i64>| Template {
        name: name.to_string(),
        roles: roles.iter().copied().collect(),
        default_profile: AuthorityProfile { version: 1, limits: profile },
        ceiling,
    };
    vec![
        template("distribution administrator", &[Role::DistributionAdmin], BTreeMap::new(), BTreeMap::new()),
        template("commission operations", &[Role::CommissionOps], BTreeMap::new(), BTreeMap::new()),
        template(
            "commission approver",
            &[Role::CommissionApprover],
            limits(&[(LimitKind::Adjustment, 50_000_00)]),
            limits(&[(LimitKind::Adjustment, 250_000_00)]),
        ),
        template("compliance", &[Role::Compliance], BTreeMap::new(), BTreeMap::new()),
        template("counterparty finance", &[Role::CounterpartyFinance], all(1_000_000_00), all(5_000_000_00)),
        template(
            "counterparty finance approver",
            &[Role::CounterpartyFinanceApprover],
            {
                let mut l = all(250_000_00);
                l.insert(LimitKind::WriteOff, 25_000_00);
                l
            },
            all(5_000_000_00),
        ),
        template("legal", &[Role::Legal], BTreeMap::new(), BTreeMap::new()),
        template("conduct officer", &[Role::ConductOfficer], BTreeMap::new(), BTreeMap::new()),
    ]
}
