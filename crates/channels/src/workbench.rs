//! The held balance workbench: one screen for every held balance and every
//! counterparty position.
//!
//! The modules that own the money (commission, finance, reinsurance, group
//! administration) publish each party's position here. Staff prepare an
//! action on a held balance with its reason and document; other staff
//! approve it, with legal as a named role where the rules call for it. An
//! approved action goes to the owning module, which posts it through the
//! core's ordinary commands. Nothing here moves money.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use ic_core::{PartyId, Reject};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::classes::DataClass;
use crate::directory::Actor;
use crate::staff::{AccessRequest, Amount, At, Purpose, PurposeKind, StaffPolicy, StaffState};

/// Staff actions the workbench asks the four questions about.
pub mod actions {
    pub const VIEW: &str = "held_balance.view";
    pub const PREPARE: &str = "held_balance.prepare";
    pub const APPROVE: &str = "held_balance.approve";
    pub const APPROVE_LEGAL: &str = "held_balance.approve_legal";
    pub const CONFIGURE: &str = "held_balance.configure";
}

/// The authority limit a prepared or approved amount is judged against.
pub const LIMIT: &str = "held_balance";
/// The modules that may publish positions.
pub const PUBLISHERS: [&str; 5] = ["commission", "finance", "reinsurance", "group-admin", "claims"];
/// The modules that may record a party's status.
pub const STATUS_SOURCES: [&str; 3] = ["compliance", "customer-master", "distribution"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    /// Money owed to the party and held back.
    Held,
    ClawbackReceivable,
    Overpayment,
    PremiumArrears,
    PaymentInstruction,
    Assignment,
    Advance,
    ReinsuranceRecovery,
    ReinsurancePremium,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldReason {
    SuspendedLicence,
    Dispute,
    MissingPaymentDetails,
    LegalHold,
}

/// One line of a party's position. A positive amount is owed to the party;
/// a negative amount is owed by them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub kind: ItemKind,
    pub amount_minor: i64,
    pub currency: String,
    /// The period the entry belongs to, such as `2027-11`.
    #[serde(default)]
    pub period: Option<String>,
    /// The book of business it arose on.
    #[serde(default)]
    pub book: Option<String>,
    /// Why it is held. Required on a held item.
    #[serde(default)]
    pub reason: Option<HoldReason>,
    pub since: NaiveDate,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Document {
    /// The vault reference.
    pub reference: String,
    /// `agreement`, `regulator_notice`, `liquidator_appointment`, `legal_advice`, `demand`, …
    pub kind: String,
    pub filed: NaiveDate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartyStatus {
    Normal,
    LicenceSuspended,
    LicenceRevoked,
    InLiquidation,
    Deceased,
    RunOff,
    Untraced,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusChange {
    pub status: PartyStatus,
    pub effective: NaiveDate,
    pub source: String,
    pub recorded: At,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    /// module -> item id -> item. Each module replaces only its own.
    pub items: BTreeMap<String, BTreeMap<String, Item>>,
    pub documents: BTreeSet<Document>,
    pub status: Vec<StatusChange>,
    /// Cases open against the party.
    pub cases: BTreeSet<String>,
    /// A hold pending review: the date and the legal reference.
    pub review: Option<(NaiveDate, String)>,
}

impl Position {
    pub fn all(&self) -> impl Iterator<Item = (&String, &Item)> {
        self.items.iter().flat_map(|(module, items)| items.values().map(move |i| (module, i)))
    }

    /// The held balance by currency.
    pub fn held(&self) -> BTreeMap<String, i64> {
        let mut out = BTreeMap::new();
        for (_, i) in self.all().filter(|(_, i)| i.kind == ItemKind::Held) {
            *out.entry(i.currency.clone()).or_default() += i.amount_minor;
        }
        out
    }

    /// Everything netted, by currency.
    pub fn net(&self) -> BTreeMap<String, i64> {
        let mut out = BTreeMap::new();
        for (_, i) in self
            .all()
            .filter(|(_, i)| !matches!(i.kind, ItemKind::PaymentInstruction | ItemKind::Assignment))
        {
            *out.entry(i.currency.clone()).or_default() += i.amount_minor;
        }
        out
    }

    pub fn current_status(&self) -> PartyStatus {
        self.status.last().map_or(PartyStatus::Normal, |s| s.status)
    }

    /// The module that owns the party's held money in a currency: the one
    /// holding the most of it.
    fn owner(&self, currency: &str) -> Option<&String> {
        let mut by_module: BTreeMap<&String, i64> = BTreeMap::new();
        for (module, i) in self.all().filter(|(_, i)| i.kind == ItemKind::Held && i.currency == currency) {
            *by_module.entry(module).or_default() += i.amount_minor;
        }
        by_module
            .into_iter()
            .max_by_key(|(module, total)| (*total, std::cmp::Reverse(*module)))
            .map(|(m, _)| m)
    }
}

/// What may be done with a held balance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    /// Pay all or part to a named verified account.
    Release {
        amount_minor: i64,
        account: String,
        new_account: bool,
    },
    /// Net the held balance against a receivable, in one entry.
    Offset {
        amount_minor: i64,
        receivable: String,
    },
    /// Keep held, with a review date and a legal reference.
    HoldPending {
        review_on: NaiveDate,
        legal_reference: String,
    },
    WriteOff {
        amount_minor: i64,
    },
    /// Allocate a disputed share between parties, each with their agreement reference.
    Split {
        shares: Vec<Share>,
    },
    /// A receipt from a liquidator against a receivable, the remainder written off.
    RecordDividend {
        receivable: String,
        received_minor: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Share {
    pub party: PartyId,
    pub amount_minor: i64,
    pub agreement: String,
}

impl Action {
    /// The amount authority is judged on.
    pub fn amount(&self) -> i64 {
        match self {
            Action::Release { amount_minor, .. } | Action::Offset { amount_minor, .. } | Action::WriteOff { amount_minor } => *amount_minor,
            Action::HoldPending { .. } => 0,
            Action::Split { shares } => shares.iter().map(|s| s.amount_minor).sum(),
            Action::RecordDividend { received_minor, .. } => *received_minor,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Needs {
    /// Approvers other than legal.
    pub approvers: u8,
    pub legal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    pub by: PartyId,
    pub legal: bool,
    pub at: At,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProposalState {
    AwaitingApproval,
    /// Approved and waiting for the owning module to post it.
    Approved,
    Applied {
        reference: String,
    },
    Failed {
        code: String,
        message: String,
    },
    Declined {
        by: PartyId,
        reason: String,
    },
    Withdrawn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub id: String,
    pub party: PartyId,
    pub currency: String,
    #[serde(flatten)]
    pub action: Action,
    pub reason_code: String,
    pub document: String,
    /// The module that posts it.
    pub module: String,
    pub maker: PartyId,
    pub prepared: At,
    pub needs: Needs,
    pub approvals: Vec<Approval>,
    #[serde(flatten)]
    pub state: ProposalState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// Above this, two approvers plus legal.
    pub two_approver_above_minor: i64,
    /// A write-off above this needs a second approver.
    pub write_off_second_above_minor: i64,
    /// The age bands of the daily control, in days.
    pub age_bands: Vec<i64>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            two_approver_above_minor: 50_000_00,
            write_off_second_above_minor: 1_000_00,
            age_bands: vec![30, 90, 180, 365],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkbenchCommand {
    /// A module replaces its own items for a party, and adds documents and cases.
    Publish {
        party: PartyId,
        items: Vec<Item>,
        #[serde(default)]
        documents: Vec<Document>,
        #[serde(default)]
        cases: Vec<String>,
    },
    RecordStatus {
        party: PartyId,
        status: PartyStatus,
        effective: NaiveDate,
        source: String,
    },
    Prepare {
        id: String,
        party: PartyId,
        currency: String,
        action: Action,
        reason_code: String,
        document: String,
    },
    Approve {
        id: String,
        /// Approving as legal, the named role.
        legal: bool,
    },
    Decline {
        id: String,
        reason: String,
    },
    Withdraw {
        id: String,
    },
    /// The owning module reports the outcome.
    Resolve {
        id: String,
        reference: Option<String>,
        failed: Option<(String, String)>,
    },
    ChangeSettings {
        settings: Settings,
        approved_by: PartyId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkbenchEvent {
    PositionChanged {
        party: PartyId,
    },
    Prepared {
        id: String,
    },
    /// Send it to the module to post.
    Ready {
        id: String,
        module: String,
    },
    Closed {
        id: String,
    },
    /// A hold was set with a review date: open a task for it.
    ReviewSet {
        party: PartyId,
        on: NaiveDate,
        legal_reference: String,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workbench {
    pub positions: BTreeMap<PartyId, Position>,
    pub proposals: BTreeMap<String, Proposal>,
    #[serde(default)]
    pub settings: Settings,
}

fn reject(code: &str, message: impl Into<String>) -> Reject {
    Reject::new(code, message)
}

/// What the staff state and policy say about one workbench action.
pub struct Authority<'a> {
    pub staff: &'a StaffState,
    pub policy: &'a StaffPolicy,
    pub fresh: bool,
}

impl Authority<'_> {
    fn check(&self, who: &PartyId, action: &str, party: &PartyId, amount: i64, maker: Option<&PartyId>, case: &str, at: At) -> Result<(), Reject> {
        let decision = self.staff.authorise(
            self.policy,
            &AccessRequest {
                staff: who.clone(),
                action: action.to_string(),
                class: DataClass::Financial,
                scopes: vec![],
                record: Some(format!("counterparty:{party}")),
                amount: Some(Amount {
                    limit: LIMIT.to_string(),
                    value: amount,
                }),
                purpose: Some(Purpose {
                    kind: PurposeKind::Task,
                    reference: case.to_string(),
                }),
                decides: true,
                conflict_subject: None,
                maker: maker.cloned(),
                fresh: self.fresh,
                at,
            },
        );
        match decision.denied {
            None => Ok(()),
            Some(denial) => Err(reject("not_authorised", denial.explain())),
        }
    }
}

impl Workbench {
    /// What approval an action needs, from the party's status and the amount.
    pub fn needs(&self, position: &Position, action: &Action) -> Needs {
        let amount = action.amount();
        let above = amount > self.settings.two_approver_above_minor;
        let new_account_at_risk = matches!(action, Action::Release { new_account: true, .. })
            && matches!(position.current_status(), PartyStatus::InLiquidation | PartyStatus::Deceased);
        let second_for_write_off = matches!(action, Action::WriteOff { .. }) && amount > self.settings.write_off_second_above_minor;
        Needs {
            approvers: if above || second_for_write_off { 2 } else { 1 },
            legal: above || new_account_at_risk,
        }
    }

    fn validate(&self, party: &PartyId, position: &Position, currency: &str, action: &Action) -> Result<(), Reject> {
        let held = position.held().get(currency).copied().unwrap_or(0);
        let committed: i64 = self
            .proposals
            .values()
            .filter(|p| &p.party == party && p.currency == currency && matches!(p.state, ProposalState::AwaitingApproval | ProposalState::Approved))
            .filter(|p| !matches!(p.action, Action::HoldPending { .. } | Action::RecordDividend { .. }))
            .map(|p| p.action.amount())
            .sum();
        let receivable = |id: &str| {
            position
                .all()
                .map(|(_, i)| i)
                .find(|i| i.id == id && i.currency == currency && i.amount_minor < 0)
                .map(|i| -i.amount_minor)
                .ok_or_else(|| reject("unknown_receivable", "the party has no such receivable in that currency"))
        };
        let within_held = |amount: i64| {
            if amount <= 0 {
                Err(reject("bad_amount", "the amount must be more than zero"))
            } else if amount > held - committed {
                Err(reject(
                    "exceeds_held",
                    "the amount is more than the held balance not already committed to another action",
                ))
            } else {
                Ok(())
            }
        };
        match action {
            Action::Release { amount_minor, account, .. } => {
                if account.trim().is_empty() {
                    return Err(reject("no_account", "a release names the verified account it pays"));
                }
                within_held(*amount_minor)
            }
            Action::Offset { amount_minor, receivable: id } => {
                within_held(*amount_minor)?;
                if *amount_minor > receivable(id)? {
                    return Err(reject("exceeds_receivable", "the offset is more than the receivable"));
                }
                Ok(())
            }
            Action::HoldPending { legal_reference, .. } => {
                if legal_reference.trim().is_empty() {
                    return Err(reject("no_legal_reference", "a hold pending names its legal reference"));
                }
                Ok(())
            }
            Action::WriteOff { amount_minor } => within_held(*amount_minor),
            Action::Split { shares } => {
                if shares.len() < 2 || shares.iter().any(|s| s.amount_minor <= 0 || s.agreement.trim().is_empty()) {
                    return Err(reject(
                        "bad_split",
                        "a split names at least two parties, each with an amount and their agreement reference",
                    ));
                }
                within_held(action.amount())
            }
            Action::RecordDividend {
                receivable: id,
                received_minor,
            } => {
                if *received_minor < 0 || *received_minor > receivable(id)? {
                    return Err(reject("bad_amount", "a dividend is between nothing and the whole receivable"));
                }
                Ok(())
            }
        }
    }

    /// Apply a command. A rejected command changes nothing.
    pub fn apply(&mut self, authority: &Authority<'_>, actor: &Actor, at: At, command: WorkbenchCommand) -> Result<Vec<WorkbenchEvent>, Reject> {
        let staff = |actor: &Actor| match actor {
            Actor::Staff(p) => Ok(p.clone()),
            _ => Err(reject("forbidden", "the workbench is used by staff")),
        };
        let module = |actor: &Actor, allowed: &[&str]| match actor {
            Actor::Module(m) if allowed.contains(&m.as_str()) => Ok(m.clone()),
            _ => Err(reject("forbidden", format!("only {allowed:?} may send this"))),
        };
        match command {
            WorkbenchCommand::Publish {
                party,
                items,
                documents,
                cases,
            } => {
                let module = module(actor, &PUBLISHERS)?;
                if let Some(bad) = items.iter().find(|i| i.kind == ItemKind::Held && (i.reason.is_none() || i.amount_minor < 0)) {
                    return Err(reject(
                        "bad_item",
                        format!("held item {} needs a reason and an amount owed to the party", bad.id),
                    ));
                }
                if items.iter().any(|i| i.id.trim().is_empty() || i.currency.len() != 3) {
                    return Err(reject("bad_item", "an item has an ID and a three-letter currency"));
                }
                let position = self.positions.entry(party.clone()).or_default();
                if items.is_empty() {
                    position.items.remove(&module);
                } else {
                    position.items.insert(module, items.into_iter().map(|i| (i.id.clone(), i)).collect());
                }
                position.documents.extend(documents);
                position.cases.extend(cases);
                Ok(vec![WorkbenchEvent::PositionChanged { party }])
            }
            WorkbenchCommand::RecordStatus {
                party,
                status,
                effective,
                source,
            } => {
                module(actor, &STATUS_SOURCES)?;
                if source.trim().is_empty() {
                    return Err(reject("no_source", "a status change names its source"));
                }
                self.positions.entry(party.clone()).or_default().status.push(StatusChange {
                    status,
                    effective,
                    source,
                    recorded: at,
                });
                Ok(vec![WorkbenchEvent::PositionChanged { party }])
            }
            WorkbenchCommand::Prepare {
                id,
                party,
                currency,
                action,
                reason_code,
                document,
            } => {
                let maker = staff(actor)?;
                if self.proposals.contains_key(&id) {
                    return Err(reject("duplicate", "an action with that ID exists"));
                }
                if reason_code.trim().is_empty() || document.trim().is_empty() {
                    return Err(reject("undocumented", "an action carries a reason code and a document reference"));
                }
                let position = self
                    .positions
                    .get(&party)
                    .ok_or_else(|| reject("unknown_party", "no position is held for that party"))?;
                self.validate(&party, position, &currency, &action)?;
                authority.check(&maker, actions::PREPARE, &party, action.amount(), None, &id, at)?;
                let module = position
                    .owner(&currency)
                    .cloned()
                    .ok_or_else(|| reject("nothing_held", "nothing is held for that party in that currency"))?;
                let needs = self.needs(position, &action);
                self.proposals.insert(
                    id.clone(),
                    Proposal {
                        id: id.clone(),
                        party,
                        currency,
                        action,
                        reason_code,
                        document,
                        module,
                        maker,
                        prepared: at,
                        needs,
                        approvals: vec![],
                        state: ProposalState::AwaitingApproval,
                    },
                );
                Ok(vec![WorkbenchEvent::Prepared { id }])
            }
            WorkbenchCommand::Approve { id, legal } => {
                let by = staff(actor)?;
                let proposal = self.proposals.get(&id).ok_or_else(|| reject("not_found", "no such action"))?;
                if proposal.state != ProposalState::AwaitingApproval {
                    return Err(reject("not_awaiting", "that action is not awaiting approval"));
                }
                if proposal.approvals.iter().any(|a| a.by == by) {
                    return Err(reject(
                        "already_approved",
                        "you have already approved this; each approval is a different person",
                    ));
                }
                let have_legal = proposal.approvals.iter().any(|a| a.legal);
                let have_others = proposal.approvals.iter().filter(|a| !a.legal).count();
                if legal && (!proposal.needs.legal || have_legal) {
                    return Err(reject("not_needed", "legal approval is not outstanding on this action"));
                }
                if !legal && have_others >= usize::from(proposal.needs.approvers) {
                    return Err(reject("not_needed", "this action is waiting for legal, not for another approver"));
                }
                let action = if legal { actions::APPROVE_LEGAL } else { actions::APPROVE };
                authority.check(&by, action, &proposal.party, proposal.action.amount(), Some(&proposal.maker), &id, at)?;
                let mut events = Vec::new();
                if let Some(p) = self.proposals.get_mut(&id) {
                    p.approvals.push(Approval { by, legal, at });
                    let others = p.approvals.iter().filter(|a| !a.legal).count();
                    let complete = others >= usize::from(p.needs.approvers) && (!p.needs.legal || p.approvals.iter().any(|a| a.legal));
                    if complete {
                        p.state = ProposalState::Approved;
                        events.push(WorkbenchEvent::Ready {
                            id: id.clone(),
                            module: p.module.clone(),
                        });
                        if let Action::HoldPending { review_on, legal_reference } = &p.action {
                            events.push(WorkbenchEvent::ReviewSet {
                                party: p.party.clone(),
                                on: *review_on,
                                legal_reference: legal_reference.clone(),
                            });
                            if let Some(position) = self.positions.get_mut(&p.party) {
                                position.review = Some((*review_on, legal_reference.clone()));
                            }
                        }
                    }
                }
                Ok(events)
            }
            WorkbenchCommand::Decline { id, reason } => {
                let by = staff(actor)?;
                let proposal = self.proposals.get(&id).ok_or_else(|| reject("not_found", "no such action"))?;
                if proposal.state != ProposalState::AwaitingApproval {
                    return Err(reject("not_awaiting", "that action is not awaiting approval"));
                }
                if reason.trim().is_empty() {
                    return Err(reject("no_reason", "a decline gives its reason"));
                }
                authority.check(&by, actions::APPROVE, &proposal.party, 0, Some(&proposal.maker), &id, at)?;
                if let Some(p) = self.proposals.get_mut(&id) {
                    p.state = ProposalState::Declined { by, reason };
                }
                Ok(vec![WorkbenchEvent::Closed { id }])
            }
            WorkbenchCommand::Withdraw { id } => {
                let by = staff(actor)?;
                let proposal = self
                    .proposals
                    .get_mut(&id)
                    .filter(|p| p.maker == by)
                    .ok_or_else(|| reject("not_found", "no such action"))?;
                if proposal.state != ProposalState::AwaitingApproval {
                    return Err(reject("not_awaiting", "that action is not awaiting approval"));
                }
                proposal.state = ProposalState::Withdrawn;
                Ok(vec![WorkbenchEvent::Closed { id }])
            }
            WorkbenchCommand::Resolve { id, reference, failed } => {
                let proposal = self.proposals.get_mut(&id).ok_or_else(|| reject("not_found", "no such action"))?;
                match actor {
                    Actor::Module(m) if m == &proposal.module => {}
                    _ => return Err(reject("forbidden", "an action is resolved by the module that posts it")),
                }
                if proposal.state != ProposalState::Approved {
                    return Err(reject("not_approved", "that action is not waiting to be posted"));
                }
                proposal.state = match (failed, reference) {
                    (Some((code, message)), _) => ProposalState::Failed { code, message },
                    (None, Some(reference)) => ProposalState::Applied { reference },
                    (None, None) => return Err(reject("no_reference", "a posted action carries the core's reference")),
                };
                Ok(vec![WorkbenchEvent::Closed { id }])
            }
            WorkbenchCommand::ChangeSettings { settings, approved_by } => {
                let by = staff(actor)?;
                if approved_by == by {
                    return Err(reject("maker_checker", "a change to the thresholds needs a second person"));
                }
                if settings.two_approver_above_minor < 0 || settings.write_off_second_above_minor < 0 || settings.age_bands.windows(2).any(|w| w[0] >= w[1]) {
                    return Err(reject("bad_settings", "thresholds are not negative and age bands rise"));
                }
                for who in [&by, &approved_by] {
                    if !authority.staff.holds_action(who, actions::CONFIGURE, at) {
                        return Err(reject("not_authorised", "both people need the role that configures the workbench"));
                    }
                }
                self.settings = settings;
                Ok(vec![])
            }
        }
    }

    /// The daily control: the held total across all parties, aged, by currency.
    pub fn control(&self, today: NaiveDate) -> Value {
        let bands = &self.settings.age_bands;
        let label = |i: usize| match (i.checked_sub(1).and_then(|j| bands.get(j)), bands.get(i)) {
            (None, Some(to)) => format!("0-{to}"),
            (Some(from), Some(to)) => format!("{}-{to}", from + 1),
            (Some(from), None) => format!("over {from}"),
            (None, None) => "all".to_string(),
        };
        let mut totals: BTreeMap<String, (i64, Vec<i64>)> = BTreeMap::new();
        let mut parties = BTreeSet::new();
        for (party, position) in &self.positions {
            for (_, item) in position.all().filter(|(_, i)| i.kind == ItemKind::Held) {
                let age = (today - item.since).num_days();
                let band = bands.iter().position(|b| age <= *b).unwrap_or(bands.len());
                let entry = totals.entry(item.currency.clone()).or_insert_with(|| (0, vec![0; bands.len() + 1]));
                entry.0 += item.amount_minor;
                entry.1[band] += item.amount_minor;
                parties.insert(party);
            }
        }
        json!({
            "as_at": today,
            "parties": parties.len(),
            "held": totals.into_iter().map(|(currency, (total, by_band))| json!({
                "currency": currency,
                "total_minor": total,
                "aged": by_band.iter().enumerate().map(|(i, v)| json!({ "days": label(i), "minor": v })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "reviews_due": self.positions.iter()
                .filter_map(|(party, p)| p.review.as_ref().filter(|(on, _)| *on <= today).map(|(on, reference)| json!({ "party": party, "on": on, "legal_reference": reference })))
                .collect::<Vec<_>>(),
        })
    }

    /// The whole position of one party, as the approvers see it before signing.
    pub fn position(&self, party: &PartyId) -> Option<Value> {
        let p = self.positions.get(party)?;
        Some(json!({
            "party": party,
            "status": p.current_status(),
            "status_history": p.status,
            "held": p.held(),
            "net": p.net(),
            "items": p.all().map(|(module, i)| json!({ "module": module, "item": i })).collect::<Vec<_>>(),
            "documents": p.documents,
            "cases": p.cases,
            "review": p.review.as_ref().map(|(on, reference)| json!({ "on": on, "legal_reference": reference })),
            "actions": self.proposals.values().filter(|a| &a.party == party).collect::<Vec<_>>(),
        }))
    }

    /// What a payee sees of their own held money: the amounts and the reasons.
    pub fn own_held(&self, party: &PartyId) -> Value {
        let Some(p) = self.positions.get(party) else {
            return json!({ "held": {}, "entries": [] });
        };
        json!({
            "held": p.held(),
            "entries": p.all().filter(|(_, i)| i.kind == ItemKind::Held)
                .map(|(_, i)| json!({ "amount_minor": i.amount_minor, "currency": i.currency, "period": i.period, "book": i.book, "reason": i.reason, "since": i.since }))
                .collect::<Vec<_>>(),
        })
    }

    /// A statement of what the party owes and is owed, by period, with the
    /// entry each line comes from, in a form that can be lodged.
    pub fn proof_of_debt(&self, party: &PartyId, today: NaiveDate) -> Option<Value> {
        let p = self.positions.get(party)?;
        let mut lines: Vec<(&Option<String>, &String, &Item)> = p
            .all()
            .filter(|(_, i)| !matches!(i.kind, ItemKind::PaymentInstruction | ItemKind::Assignment))
            .map(|(module, i)| (&i.period, module, i))
            .collect();
        lines.sort_by(|a, b| (a.0, a.1, &a.2.id).cmp(&(b.0, b.1, &b.2.id)));
        let owed_by: BTreeMap<String, i64> = lines.iter().filter(|l| l.2.amount_minor < 0).fold(BTreeMap::new(), |mut m, l| {
            *m.entry(l.2.currency.clone()).or_default() -= l.2.amount_minor;
            m
        });
        let owed_to: BTreeMap<String, i64> = lines.iter().filter(|l| l.2.amount_minor > 0).fold(BTreeMap::new(), |mut m, l| {
            *m.entry(l.2.currency.clone()).or_default() += l.2.amount_minor;
            m
        });
        Some(json!({
            "party": party,
            "as_at": today,
            "status": p.current_status(),
            "owed_by_party": owed_by,
            "owed_to_party": owed_to,
            "net": p.net(),
            "lines": lines.iter().map(|(period, module, i)| json!({
                "period": period, "kind": i.kind, "amount_minor": i.amount_minor, "currency": i.currency,
                "book": i.book, "lineage": format!("{module}:{}", i.id),
            })).collect::<Vec<_>>(),
            "documents": p.documents,
        }))
    }
}
