//! The gateway's read model of who is who: which sign-in is which party,
//! and what each party's relationship is to each policy.
//!
//! Relationships that the core's events carry (policyholder, life insured,
//! scheme member) are followed from the stream. The rest arrive as commands:
//! a customer authorising an adviser, an employer naming an administrator, a
//! lender's interest, an adviser's engagement with a financial advice
//! provider, and the licence status of advisers and the bodies above them.
//!
//! A revocation is a command applied at once, ahead of any event stream: the
//! priority revocation channel.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use ic_core::{LifeId, PartyId, PolicyId, Reject};
use ic_readmodel::scope::Relationship;
use ic_readmodel::Projections;
use serde::{Deserialize, Serialize};

use crate::staff::At;

/// How deep the structure above an adviser is followed. The model allows
/// any depth; this bounds the walk against a cycle entered in error.
const MAX_STRUCTURE_DEPTH: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LicenceStatus {
    Active,
    Suspended,
    Revoked,
    Lapsed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LicenceChange {
    pub status: LicenceStatus,
    pub effective: NaiveDate,
    /// The regulator's notice, or the insurer's own record.
    pub source: String,
    pub recorded: At,
}

/// An adviser's dated relationship with a financial advice provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Engagement {
    pub id: String,
    pub adviser: PartyId,
    pub fap: PartyId,
    pub from: NaiveDate,
    pub to: Option<NaiveDate>,
}

/// One body operating under another: an authorised body under a provider, a
/// provider under a group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructureLink {
    pub parent: PartyId,
    pub from: NaiveDate,
    pub to: Option<NaiveDate>,
}

/// A customer's authority for an adviser to see and service a policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Authority {
    /// The engagement the access belongs to. It does not pass to others at
    /// the advice business.
    pub engagement: String,
    pub granted_by: PartyId,
    pub granted: At,
}

/// Whether an adviser may act under an engagement on a date.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AdviserStanding {
    /// Licensed and engaged: may quote, submit and change.
    Active,
    /// A licence the engagement depends on is suspended, revoked or lapsed.
    /// The book is visible; nothing can be quoted, submitted or changed.
    ReadOnly {
        since: NaiveDate,
        /// The party whose licence it is: the adviser, or a body above them.
        licence_of: PartyId,
        status: LicenceStatus,
    },
    /// No such engagement on that date. Access has ended.
    Ended,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectoryCommand {
    /// Tie a sign-in subject to a party, after identity verification.
    BindSubject {
        subject: String,
        party: PartyId,
    },
    UnbindSubject {
        subject: String,
    },
    /// Ownership changed in the customer master.
    RecordOwnership {
        policy: PolicyId,
        policyholder: PartyId,
    },
    /// An owner's relationship ended in the customer master with nobody
    /// named in their place, as when the owner has died and the estate is
    /// being settled. The policy then has no owner in the app; staff act
    /// for the estate. Ignored when someone else already owns it, since the
    /// new owner's `RecordOwnership` may arrive first.
    EndOwnership {
        policy: PolicyId,
        party: PartyId,
    },
    /// The policyholder authorises an adviser, under one engagement.
    GrantAdviserAuthority {
        policy: PolicyId,
        adviser: PartyId,
        engagement: String,
    },
    /// The policyholder ends an adviser's access. Takes effect at once.
    WithdrawAdviserAuthority {
        policy: PolicyId,
        adviser: PartyId,
    },
    SetEmployerAdmin {
        policy: PolicyId,
        party: PartyId,
        admin: bool,
    },
    /// Which kinds of change need a second administrator on this scheme.
    RequireSecondAdmin {
        policy: PolicyId,
        changes: BTreeSet<String>,
    },
    RecordAssignment {
        policy: PolicyId,
        party: PartyId,
        active: bool,
    },
    /// The policyholder lets someone look at the policy, or stops it. That
    /// person can read what their kind of access is for and can change nothing.
    SetDelegate {
        policy: PolicyId,
        party: PartyId,
        /// `None` ends the access.
        access: Option<DelegateAccess>,
    },
    /// An adult on a policy lets the policyholder see their own sensitive rows, or stops it.
    SetFamilyConsent {
        policy: PolicyId,
        consent: bool,
    },
    /// A child insured under a policy, and the date from which their health
    /// information is theirs to control: the day they reach the age the
    /// product sets. Until then the policyholder sees it as a parent does.
    RecordDependant {
        policy: PolicyId,
        party: PartyId,
        controls_from: NaiveDate,
    },
    /// The customer master no longer records this person as a dependant on
    /// the policy. What they see follows from their cover alone.
    EndDependant {
        policy: PolicyId,
        party: PartyId,
    },
    RecordEngagement {
        engagement: Engagement,
    },
    RecordStructure {
        child: PartyId,
        link: StructureLink,
    },
    RecordLicence {
        party: PartyId,
        status: LicenceStatus,
        effective: NaiveDate,
        source: String,
    },
}

/// What someone the policyholder lets in may see.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegateAccess {
    /// A relative or friend: the policy, without anything that implies health.
    Viewer,
    /// An accountant: the policy, its billing and its documents.
    Financial,
    /// A support person, who helps the policyholder use the service.
    Support,
}

impl DelegateAccess {
    fn relationship(self) -> Relationship {
        match self {
            DelegateAccess::Viewer => Relationship::Viewer,
            DelegateAccess::Financial => Relationship::FinancialViewer,
            DelegateAccess::Support => Relationship::SupportPerson,
        }
    }
}

/// Who a directory command comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Actor {
    /// A signed-in customer, adviser or employer administrator.
    Party(PartyId),
    /// A member of staff.
    Staff(PartyId),
    /// Another module of the platform, by name.
    Module(String),
}

impl Actor {
    pub fn party(&self) -> Option<&PartyId> {
        match self {
            Actor::Party(p) | Actor::Staff(p) => Some(p),
            Actor::Module(_) => None,
        }
    }
}

/// What a command changed, so views can be brought up to date.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Touched {
    pub policies: BTreeSet<PolicyId>,
    /// Parties whose sessions must be re-checked now.
    pub revoked: BTreeSet<PartyId>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Directory {
    pub subjects: BTreeMap<String, PartyId>,
    /// The owners of each policy: one, or several jointly.
    #[serde(deserialize_with = "owners_of")]
    pub policyholders: BTreeMap<PolicyId, BTreeSet<PartyId>>,
    /// Lives that are scheme members, as opposed to lives individually insured.
    pub scheme_members: BTreeMap<PolicyId, BTreeSet<LifeId>>,
    /// policy -> adviser -> authority.
    pub authorities: BTreeMap<PolicyId, BTreeMap<PartyId, Authority>>,
    pub adviser_policies: BTreeMap<PartyId, BTreeSet<PolicyId>>,
    pub employer_admins: BTreeMap<PolicyId, BTreeSet<PartyId>>,
    pub admin_policies: BTreeMap<PartyId, BTreeSet<PolicyId>>,
    pub second_admin: BTreeMap<PolicyId, BTreeSet<String>>,
    pub assignees: BTreeMap<PolicyId, BTreeSet<PartyId>>,
    /// Adults who let the policyholder see their sensitive rows.
    pub family_consent: BTreeMap<PolicyId, BTreeSet<PartyId>>,
    /// Children insured under a policy, each with the date they take
    /// control of their own health information.
    #[serde(default)]
    pub dependants: BTreeMap<PolicyId, BTreeMap<PartyId, NaiveDate>>,
    /// People the policyholder lets look at a policy.
    #[serde(default)]
    pub delegates: BTreeMap<PolicyId, BTreeMap<PartyId, DelegateAccess>>,
    pub engagements: BTreeMap<String, Engagement>,
    pub structure: BTreeMap<PartyId, Vec<StructureLink>>,
    pub licences: BTreeMap<PartyId, Vec<LicenceChange>>,
    /// When the directory last heard from the event stream or a command.
    pub heard: At,
}

fn reject(code: &str, message: impl Into<String>) -> Reject {
    Reject::new(code, message)
}

fn in_window(from: NaiveDate, to: Option<NaiveDate>, on: NaiveDate) -> bool {
    from <= on && to.is_none_or(|t| on < t)
}

/// Owners as recorded: a set per policy, or one party per policy as state
/// written before joint ownership was.
fn owners_of<'de, D: serde::Deserializer<'de>>(d: D) -> Result<BTreeMap<PolicyId, BTreeSet<PartyId>>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Owners {
        Set(BTreeSet<PartyId>),
        One(PartyId),
    }
    let raw = BTreeMap::<PolicyId, Owners>::deserialize(d)?;
    Ok(raw
        .into_iter()
        .map(|(p, o)| match o {
            Owners::Set(set) => (p, set),
            Owners::One(one) => (p, [one].into()),
        })
        .collect())
}

impl Directory {
    /// Whether a party is an owner of a policy.
    pub fn owns(&self, policy: &PolicyId, party: &PartyId) -> bool {
        self.policyholders.get(policy).is_some_and(|o| o.contains(party))
    }

    /// The owners of a policy, none if it has none recorded.
    pub fn owners(&self, policy: &PolicyId) -> BTreeSet<PartyId> {
        self.policyholders.get(policy).cloned().unwrap_or_default()
    }

    pub fn party_of_subject(&self, subject: &str) -> Option<&PartyId> {
        self.subjects.get(subject)
    }

    /// Whether relationship data is inside its staleness bound at a time.
    pub fn fresh(&self, at: At, bound_secs: i64) -> bool {
        at - self.heard <= bound_secs
    }

    /// The licence status of one party on a date. No record means active:
    /// most parties never have a licence event.
    pub fn licence_on(&self, party: &PartyId, on: NaiveDate) -> Option<&LicenceChange> {
        self.licences
            .get(party)
            .and_then(|changes| changes.iter().filter(|c| c.effective <= on).max_by_key(|c| (c.effective, c.recorded)))
            .filter(|c| c.status != LicenceStatus::Active)
    }

    /// Whether an adviser may act under an engagement on a date. A licence
    /// lost anywhere above the engagement cascades down to it.
    pub fn standing(&self, engagement: &str, on: NaiveDate) -> AdviserStanding {
        let Some(e) = self.engagements.get(engagement).filter(|e| in_window(e.from, e.to, on)) else {
            return AdviserStanding::Ended;
        };
        let mut chain = vec![e.adviser.clone(), e.fap.clone()];
        let mut frontier = vec![e.fap.clone()];
        for _ in 0..MAX_STRUCTURE_DEPTH {
            let mut next = Vec::new();
            for body in &frontier {
                for link in self.structure.get(body).into_iter().flatten().filter(|l| in_window(l.from, l.to, on)) {
                    if !chain.contains(&link.parent) {
                        chain.push(link.parent.clone());
                        next.push(link.parent.clone());
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
        chain
            .iter()
            .filter_map(|p| self.licence_on(p, on).map(|c| (p, c)))
            .min_by_key(|(_, c)| c.effective)
            .map_or(AdviserStanding::Active, |(p, c)| AdviserStanding::ReadOnly {
                since: c.effective,
                licence_of: p.clone(),
                status: c.status,
            })
    }

    /// Every engagement that depends on a party's licence: the party's own,
    /// and those under any body below which the party sits.
    fn dependents(&self, party: &PartyId) -> Vec<&Engagement> {
        let mut bodies: BTreeSet<&PartyId> = [party].into();
        for _ in 0..MAX_STRUCTURE_DEPTH {
            let before = bodies.len();
            for (child, links) in &self.structure {
                if links.iter().any(|l| bodies.contains(&l.parent)) {
                    bodies.insert(child);
                }
            }
            if bodies.len() == before {
                break;
            }
        }
        self.engagements
            .values()
            .filter(|e| bodies.contains(&e.adviser) || bodies.contains(&e.fap))
            .collect()
    }

    /// The engagements a body answers for on a date: advisers engaged with
    /// it, or with any body linked below it on that date. A provider sees
    /// its advisers; a group sees every provider under it and theirs.
    pub fn practice(&self, party: &PartyId, on: NaiveDate) -> Vec<&Engagement> {
        let mut bodies: BTreeSet<&PartyId> = [party].into();
        for _ in 0..MAX_STRUCTURE_DEPTH {
            let before = bodies.len();
            for (child, links) in &self.structure {
                if links.iter().any(|l| in_window(l.from, l.to, on) && bodies.contains(&l.parent)) {
                    bodies.insert(child);
                }
            }
            if bodies.len() == before {
                break;
            }
        }
        self.engagements
            .values()
            .filter(|e| bodies.contains(&e.fap) && in_window(e.from, e.to, on))
            .collect()
    }

    /// The policies an adviser holds authority on under one engagement.
    pub fn policies_of(&self, engagement: &Engagement) -> Vec<&PolicyId> {
        self.authorities
            .iter()
            .filter(|(_, by_adviser)| by_adviser.get(&engagement.adviser).is_some_and(|a| a.engagement == engagement.id))
            .map(|(p, _)| p)
            .collect()
    }

    fn policies_under(&self, engagements: &[&Engagement]) -> BTreeSet<PolicyId> {
        let ids: BTreeSet<&str> = engagements.iter().map(|e| e.id.as_str()).collect();
        self.authorities
            .iter()
            .filter(|(_, by_adviser)| by_adviser.values().any(|a| ids.contains(a.engagement.as_str())))
            .map(|(p, _)| p.clone())
            .collect()
    }

    /// The servicing adviser on a policy, with their standing, as the
    /// customer needs to know it.
    pub fn advisers_on(&self, policy: &PolicyId, on: NaiveDate) -> Vec<(&PartyId, AdviserStanding)> {
        self.authorities
            .get(policy)
            .into_iter()
            .flatten()
            .map(|(adviser, a)| (adviser, self.standing(&a.engagement, on)))
            .collect()
    }

    /// A party's relationships to a policy on a date.
    pub fn relationships(&self, view: &Projections, party: &PartyId, policy: &PolicyId, on: NaiveDate) -> Vec<Relationship> {
        let mut out = Vec::new();
        if self.owns(policy, party) {
            out.push(Relationship::Policyholder);
        }
        let members = self.scheme_members.get(policy);
        let mut life = false;
        let mut member = false;
        for row in view.layers_on_policy(policy) {
            if view.party_of(policy, &row.life) == Some(party) {
                if members.is_some_and(|m| m.contains(&row.life)) {
                    member = true;
                } else {
                    life = true;
                }
            }
        }
        if life {
            out.push(Relationship::LifeInsured);
        }
        if member {
            out.push(Relationship::Member);
        }
        // An adviser under a lost licence still sees the book; an ended
        // engagement ends the access with it.
        if self
            .authorities
            .get(policy)
            .and_then(|a| a.get(party))
            .is_some_and(|a| self.standing(&a.engagement, on) != AdviserStanding::Ended)
        {
            out.push(Relationship::Adviser);
        }
        if self.employer_admins.get(policy).is_some_and(|a| a.contains(party)) {
            out.push(Relationship::EmployerAdmin);
        }
        if self.assignees.get(policy).is_some_and(|a| a.contains(party)) {
            out.push(Relationship::Assignee);
        }
        if let Some(access) = self.delegates.get(policy).and_then(|d| d.get(party)) {
            out.push(access.relationship());
        }
        out
    }

    /// The parties whose device feeds follow a policy: its owner, the people
    /// insured under it and anyone with a recorded interest. Advisers and
    /// employer administrators use online portals instead.
    pub fn feed_audience(&self, view: &Projections, policy: &PolicyId) -> BTreeSet<PartyId> {
        let mut out: BTreeSet<PartyId> = self.owners(policy);
        for row in view.layers_on_policy(policy) {
            out.extend(view.party_of(policy, &row.life).cloned());
        }
        out.extend(self.assignees.get(policy).into_iter().flatten().cloned());
        out.extend(self.delegates.get(policy).into_iter().flat_map(|d| d.keys()).cloned());
        out
    }

    /// The people on a policy whose sensitive rows the policyholder may see
    /// on a date: adults who consent, and children who do not yet control
    /// their own health information.
    pub fn consenting(&self, policy: &PolicyId, on: NaiveDate) -> BTreeSet<PartyId> {
        let mut out = self.family_consent.get(policy).cloned().unwrap_or_default();
        out.extend(
            self.dependants
                .get(policy)
                .into_iter()
                .flatten()
                .filter(|(_, from)| on < **from)
                .map(|(party, _)| party.clone()),
        );
        out
    }

    /// The children who took control of their own health information after
    /// `after` and on or before `on`, with their policy.
    pub fn came_of_age(&self, after: NaiveDate, on: NaiveDate) -> Vec<(PolicyId, PartyId)> {
        self.dependants
            .iter()
            .flat_map(|(policy, children)| {
                children
                    .iter()
                    .filter(move |(_, from)| after < **from && **from <= on)
                    .map(move |(c, _)| (policy.clone(), c.clone()))
            })
            .collect()
    }

    /// Follow one core event.
    pub fn follow(&mut self, event: &ic_core::Event) {
        use ic_core::Event;
        match event {
            Event::PolicyIssued {
                policy, policyholder, members, ..
            } => {
                self.policyholders.insert(policy.clone(), [policyholder.clone()].into());
                if !members.is_empty() {
                    self.scheme_members
                        .entry(policy.clone())
                        .or_default()
                        .extend(members.iter().map(|m| m.life.clone()));
                }
            }
            Event::MemberJoined { policy, life, .. } => {
                self.scheme_members.entry(policy.clone()).or_default().insert(life.clone());
            }
            Event::TransferLanded { policy, members, .. } if !members.is_empty() => {
                self.scheme_members
                    .entry(policy.clone())
                    .or_default()
                    .extend(members.iter().map(|m| m.life.clone()));
            }
            _ => {}
        }
    }

    fn require_policyholder(&self, actor: &Actor, policy: &PolicyId) -> Result<PartyId, Reject> {
        match actor {
            Actor::Party(p) if self.owns(policy, p) => Ok(p.clone()),
            _ => Err(reject("forbidden", "only the policyholder may do this")),
        }
    }

    /// What an owner gave ends with their ownership: every adviser they
    /// authorised, and anyone let look at the policy (who let them in is not
    /// kept, so a joint owner who stays lets them in again if they wish).
    fn owner_gone(&mut self, policy: &PolicyId, gone: &PartyId, touched: &mut Touched) {
        touched.revoked.extend(self.delegates.remove(policy).unwrap_or_default().into_keys());
        let all = !self.policyholders.contains_key(policy);
        let authorities = self.authorities.entry(policy.clone()).or_default();
        let ended: Vec<PartyId> = authorities
            .iter()
            .filter(|(_, a)| all || &a.granted_by == gone)
            .map(|(adviser, _)| adviser.clone())
            .collect();
        for adviser in ended {
            authorities.remove(&adviser);
            if let Some(p) = self.adviser_policies.get_mut(&adviser) {
                p.remove(policy);
            }
            touched.revoked.insert(adviser);
        }
        if authorities.is_empty() {
            self.authorities.remove(policy);
        }
        touched.policies.insert(policy.clone());
    }

    fn require_module(actor: &Actor, modules: &[&str]) -> Result<(), Reject> {
        match actor {
            Actor::Module(m) if modules.contains(&m.as_str()) => Ok(()),
            _ => Err(reject("forbidden", format!("only {modules:?} may send this"))),
        }
    }

    /// Apply a command. A rejected command changes nothing.
    pub fn apply(&mut self, actor: &Actor, at: At, view: &Projections, command: DirectoryCommand) -> Result<Touched, Reject> {
        let mut touched = Touched::default();
        match command {
            DirectoryCommand::BindSubject { subject, party } => {
                Self::require_module(actor, &["identity", "customer-master"])?;
                if self.subjects.get(&subject).is_some_and(|p| p != &party) {
                    return Err(reject("already_bound", "that sign-in belongs to another party"));
                }
                self.subjects.insert(subject, party);
            }
            DirectoryCommand::UnbindSubject { subject } => {
                Self::require_module(actor, &["identity", "customer-master"])?;
                if let Some(party) = self.subjects.remove(&subject) {
                    touched.revoked.insert(party);
                }
            }
            DirectoryCommand::RecordOwnership { policy, policyholder } => {
                Self::require_module(actor, &["customer-master"])?;
                if view.policy(&policy).is_none() {
                    return Err(reject("unknown_policy", "no such policy"));
                }
                // An owner is added: a joint owner, or a new owner whose
                // predecessor's ownership the customer master ends separately.
                if self.policyholders.entry(policy.clone()).or_default().insert(policyholder) {
                    touched.policies.insert(policy);
                }
            }
            DirectoryCommand::EndOwnership { policy, party } => {
                Self::require_module(actor, &["customer-master"])?;
                let owners = self.policyholders.entry(policy.clone()).or_default();
                let removed = owners.remove(&party);
                if owners.is_empty() {
                    self.policyholders.remove(&policy);
                }
                if removed {
                    self.owner_gone(&policy, &party, &mut touched);
                    touched.revoked.insert(party);
                }
            }
            DirectoryCommand::GrantAdviserAuthority { policy, adviser, engagement } => {
                let granted_by = self.require_policyholder(actor, &policy)?;
                if !self.engagements.get(&engagement).is_some_and(|e| e.adviser == adviser) {
                    return Err(reject("unknown_engagement", "that adviser has no such engagement"));
                }
                self.authorities.entry(policy.clone()).or_default().insert(
                    adviser.clone(),
                    Authority {
                        engagement,
                        granted_by,
                        granted: at,
                    },
                );
                self.adviser_policies.entry(adviser).or_default().insert(policy.clone());
                touched.policies.insert(policy);
            }
            DirectoryCommand::WithdrawAdviserAuthority { policy, adviser } => {
                self.require_policyholder(actor, &policy)?;
                if self.authorities.get_mut(&policy).and_then(|a| a.remove(&adviser)).is_none() {
                    return Err(reject("not_authorised", "that adviser holds no authority on this policy"));
                }
                if let Some(p) = self.adviser_policies.get_mut(&adviser) {
                    p.remove(&policy);
                }
                touched.revoked.insert(adviser);
                touched.policies.insert(policy);
            }
            DirectoryCommand::SetEmployerAdmin { policy, party, admin } => {
                // The first administrator is set up by group administration;
                // after that the employer's own administrators manage the list.
                let by_admin = matches!(actor, Actor::Party(p) if self.employer_admins.get(&policy).is_some_and(|a| a.contains(p)));
                if !by_admin {
                    Self::require_module(actor, &["group-admin"])?;
                }
                if view.policy(&policy).is_none() {
                    return Err(reject("unknown_policy", "no such scheme"));
                }
                // A colleague is named once they have signed in, so a slip of
                // the keyboard cannot make someone nobody knows an administrator.
                if admin && by_admin && !self.subjects.values().any(|p| *p == party) {
                    return Err(reject(
                        "unknown_person",
                        "nobody with that reference has signed in; ask your colleague to sign in to the employer portal once and give you the reference it shows them",
                    ));
                }
                let last = self.employer_admins.get(&policy).is_some_and(|a| a.len() == 1 && a.contains(&party));
                if !admin && by_admin && last {
                    return Err(reject("last_admin", "a scheme keeps at least one administrator"));
                }
                let admins = self.employer_admins.entry(policy.clone()).or_default();
                if admin {
                    admins.insert(party.clone());
                    self.admin_policies.entry(party).or_default().insert(policy.clone());
                } else {
                    admins.remove(&party);
                    if let Some(p) = self.admin_policies.get_mut(&party) {
                        p.remove(&policy);
                    }
                    touched.revoked.insert(party);
                }
                touched.policies.insert(policy);
            }
            DirectoryCommand::RequireSecondAdmin { policy, changes } => {
                match actor {
                    Actor::Party(p) if self.employer_admins.get(&policy).is_some_and(|a| a.contains(p)) => {}
                    _ => Self::require_module(actor, &["group-admin"])?,
                }
                self.second_admin.insert(policy, changes);
            }
            DirectoryCommand::RecordAssignment { policy, party, active } => {
                Self::require_module(actor, &["customer-master", "policy-servicing"])?;
                let set = self.assignees.entry(policy.clone()).or_default();
                if active {
                    set.insert(party);
                } else {
                    set.remove(&party);
                    touched.revoked.insert(party);
                }
                touched.policies.insert(policy);
            }
            DirectoryCommand::SetDelegate { policy, party, access } => {
                let owner = self.require_policyholder(actor, &policy)?;
                if owner == party {
                    return Err(reject("policyholder", "the policyholder already sees their own policy"));
                }
                let on_policy = self.delegates.entry(policy.clone()).or_default();
                match access {
                    Some(access) => {
                        on_policy.insert(party, access);
                    }
                    None => {
                        if on_policy.remove(&party).is_none() {
                            return Err(reject("not_authorised", "that person has no access to this policy"));
                        }
                        touched.revoked.insert(party);
                    }
                }
                touched.policies.insert(policy);
            }
            DirectoryCommand::SetFamilyConsent { policy, consent } => {
                let Actor::Party(party) = actor else {
                    return Err(reject("forbidden", "consent is given by the person themselves"));
                };
                if !view.layers_on_policy(&policy).iter().any(|r| view.party_of(&policy, &r.life) == Some(party)) {
                    return Err(reject("not_insured", "only a person insured under the policy can consent"));
                }
                let set = self.family_consent.entry(policy.clone()).or_default();
                if consent {
                    set.insert(party.clone());
                } else {
                    set.remove(party);
                }
                touched.policies.insert(policy);
            }
            DirectoryCommand::RecordDependant { policy, party, controls_from } => {
                Self::require_module(actor, &["customer-master"])?;
                if !view.layers_on_policy(&policy).iter().any(|r| view.party_of(&policy, &r.life) == Some(&party)) {
                    return Err(reject("not_insured", "a dependant is a person insured under the policy"));
                }
                if self.owns(&policy, &party) {
                    return Err(reject("policyholder", "the policyholder is not their own dependant"));
                }
                self.dependants.entry(policy.clone()).or_default().insert(party, controls_from);
                touched.policies.insert(policy);
            }
            DirectoryCommand::EndDependant { policy, party } => {
                Self::require_module(actor, &["customer-master"])?;
                if let Some(set) = self.dependants.get_mut(&policy) {
                    set.remove(&party);
                    if set.is_empty() {
                        self.dependants.remove(&policy);
                    }
                }
                touched.policies.insert(policy);
            }
            DirectoryCommand::RecordEngagement { engagement } => {
                Self::require_module(actor, &["distribution"])?;
                if engagement.to.is_some_and(|t| t < engagement.from) {
                    return Err(reject("bad_period", "an engagement cannot end before it starts"));
                }
                if self.engagements.get(&engagement.id).is_some_and(|e| e.adviser != engagement.adviser) {
                    return Err(reject("engagement_reused", "that engagement belongs to another adviser"));
                }
                touched.policies = self.policies_under(&[&engagement]);
                touched.revoked.insert(engagement.adviser.clone());
                self.engagements.insert(engagement.id.clone(), engagement);
            }
            DirectoryCommand::RecordStructure { child, link } => {
                Self::require_module(actor, &["distribution"])?;
                if link.parent == child {
                    return Err(reject("bad_structure", "a body cannot sit under itself"));
                }
                let links = self.structure.entry(child.clone()).or_default();
                links.retain(|l| !(l.parent == link.parent && l.from == link.from));
                links.push(link);
                touched.policies = self.policies_under(&self.dependents(&child));
            }
            DirectoryCommand::RecordLicence {
                party,
                status,
                effective,
                source,
            } => {
                Self::require_module(actor, &["distribution", "compliance"])?;
                if source.trim().is_empty() {
                    return Err(reject(
                        "no_source",
                        "a licence change names its source: the regulator's notice or our own record",
                    ));
                }
                // The same change told twice, as a forwarder may after a restart, is one change.
                let known = self.licences.entry(party.clone()).or_default();
                if known.iter().any(|c| c.status == status && c.effective == effective && c.source == source) {
                    return Ok(touched);
                }
                known.push(LicenceChange {
                    status,
                    effective,
                    source,
                    recorded: at,
                });
                // The cascade: every engagement that depends on this licence.
                let dependents = self.dependents(&party);
                touched.revoked = dependents.iter().map(|e| e.adviser.clone()).collect();
                touched.policies = self.policies_under(&dependents);
            }
        }
        // `heard` is not moved here: only the event stream says the
        // relationships the core carries are current.
        Ok(touched)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Purpose: a directory written when a policy had one owner reads back with that owner as the policy's only one, and the
    /// joint form reads as written.
    /// Why it matters: state written before joint ownership must load after it; a node that could not read its own snapshot
    /// would have to rebuild from the whole log, and a misread owner would show a policy to the wrong person.
    #[test]
    fn a_single_owner_from_before_joint_ownership_still_reads() {
        let with = |owners: serde_json::Value| -> Directory {
            let mut written = serde_json::to_value(Directory::default()).unwrap();
            written["policyholders"] = owners;
            serde_json::from_value(written).unwrap()
        };
        let old = with(json!({ "pol-1": "party-a" }));
        assert_eq!(old.owners(&PolicyId::from("pol-1")), [PartyId::from("party-a")].into());
        let joint = with(json!({ "pol-1": ["party-a", "party-b"] }));
        assert!(joint.owns(&PolicyId::from("pol-1"), &PartyId::from("party-b")));
        assert!(joint.owners(&PolicyId::from("pol-2")).is_empty());
    }
}
