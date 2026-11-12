//! The held balance workbench: one place for every held balance and every
//! counterparty position. A liquidation is the hardest use of it; a disputed
//! share, an estate, an employer in arrears, a reinsurer in run-off and an
//! overpaid claimant are the same screen.
//!
//! Every action is a typed command with a reason code and a document
//! reference. The preparer builds it; approvers see the full position before
//! signing and can never be the preparer. Every action posts through the
//! core's ordinary counterparty command, so the finance ledger, the party's
//! statement and the audit trail follow with nothing done outside the system.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_core::counterparty::{CounterpartyAccount, CounterpartyLine, InternalLedger, PartyLedger};
use ic_core::{PartyId, Reject};
use ic_ledger::PartitionId;
use ic_money::Currency;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::authority::{Ask, DecisionRecord, LimitKind, Role};
use crate::model::*;
use crate::rules::{portion, HeldReason};
use crate::state::{reject, Actor, Ctx, DistributionState, Event};
use crate::statement::{simple_posting, PaymentOwner, PaymentPart, PaymentRecord};
use crate::structure::{CeasedReason, LicenceRecord};
use crate::BookId;

/// Which of the party's accounts a debt sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Debt {
    /// A debit balance on commission: clawback or a backdated correction.
    Commission,
    /// A receivable another module raised: an overpayment, premium arrears,
    /// a recovery due from a reinsurer.
    Receivable,
}

impl Debt {
    fn ledger(self) -> PartyLedger {
        match self {
            Debt::Commission => PartyLedger::CommissionPayable,
            Debt::Receivable => PartyLedger::Receivable,
        }
    }
}

/// Where the recovery of an overpayment stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryStatus {
    /// Not pursued. The default where the insurer made the error and the
    /// recipient acted in good faith.
    Held,
    Pursued,
    Arrangement,
    Offset,
    WrittenOff,
}

/// What another module says about a receivable it posted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PositionNote {
    /// `claim_overpayment`, `scheme_premium_arrears`, `reinsurance_recovery`, and so on.
    pub class: String,
    pub recovery: RecoveryStatus,
    pub insurer_error: bool,
    /// The case in the owning module.
    pub case_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplitShare {
    pub party: PartyId,
    pub amount_minor: i64,
    /// That party's agreement to the split.
    pub agreement_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Pay all or part of the held balance to a named verified account.
    Release { amount_minor: i64, account_ref: String },
    /// Net the held balance against what the party owes.
    Offset { amount_minor: i64, against: Debt },
    /// Keep held, with a review date and a legal reference.
    HoldPending { review_on: NaiveDate, legal_ref: String },
    /// Write off a held balance above an age or below a floor.
    WriteOffHeld { amount_minor: i64 },
    /// Write off what the party owes.
    WriteOffDebt { amount_minor: i64, debt: Debt },
    /// Allocate a disputed held amount between parties.
    Split { shares: Vec<SplitShare> },
    /// A receipt from a liquidator against the debt, with the remainder
    /// written off, as one command.
    RecordDividend { debt: Debt, received_minor: i64, receipt_ref: String },
    /// Move an aged debt to the party that guaranteed it.
    MoveToGuarantor { guarantor: PartyId, amount_minor: i64 },
    /// Change the recovery status of a receivable.
    SetRecovery { reference: String, to: RecoveryStatus },
}

impl Action {
    pub fn kind(&self) -> &'static str {
        match self {
            Action::Release { .. } => "release",
            Action::Offset { .. } => "offset",
            Action::HoldPending { .. } => "hold_pending",
            Action::WriteOffHeld { .. } => "write_off_held",
            Action::WriteOffDebt { .. } => "write_off_debt",
            Action::Split { .. } => "split",
            Action::RecordDividend { .. } => "record_dividend",
            Action::MoveToGuarantor { .. } => "move_to_guarantor",
            Action::SetRecovery { .. } => "set_recovery",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionState {
    Proposed,
    /// Approved; its postings are on their way to the ledger.
    Posting,
    Completed,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionRecord {
    pub id: String,
    pub party: PartyId,
    pub action: Action,
    pub reason_code: String,
    pub document_ref: String,
    pub amount_minor: i64,
    pub maker: DecisionRecord,
    pub proposed_on: NaiveDate,
    /// The approvals still needed, by role. One entry per signature.
    pub needed: Vec<Role>,
    pub approvals: Vec<(Role, DecisionRecord)>,
    pub state: ActionState,
    /// The net position the approvers were shown.
    pub position_shown: Summary,
    pub failed: Option<String>,
    pub rejected_because: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentRecord {
    /// `agreement`, `regulator_notice`, `liquidator_appointment`, `legal_advice`, `demand`.
    pub kind: String,
    pub document_ref: String,
    pub on: NaiveDate,
}

/// A party's balances. Positive is in the party's favour.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub held_minor: i64,
    pub commission_minor: i64,
    pub receivable_minor: i64,
    pub retention_minor: i64,
    pub advance_minor: i64,
    pub clearing_minor: i64,
    pub net_minor: i64,
}

/// How part of a held balance built up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldLine {
    /// The month it was held in, as `YYYY-MM`.
    pub period: String,
    pub book: Option<BookId>,
    pub reason: Option<HeldReason>,
    pub amount_minor: i64,
    pub entries: Vec<String>,
}

/// A receivable another module keeps in its own ledger, shown here so that
/// one screen has everything a party owes. It is not in this module's
/// accounts or its control totals, and it is worked in the module that owns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoLine {
    /// The module that owns it: `claims`.
    pub owner: String,
    /// The owner's reference, for example `clm-1/overpayment/1`.
    pub reference: String,
    pub party: PartyId,
    /// The owner's case: a claim ID.
    pub case_ref: String,
    pub policy: Option<ic_core::PolicyId>,
    pub outstanding_minor: i64,
    /// The owner's own status code.
    pub status: String,
    pub cause: String,
    pub insurer_error: bool,
    pub good_faith: bool,
    /// The business date of the owner's latest word on it.
    pub as_at: NaiveDate,
}

/// Everything the workbench shows for one party.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Position {
    pub party: PartyId,
    pub summary: Summary,
    pub held: Vec<HeldLine>,
    pub clawback_at_risk_minor: i64,
    pub instructions: Vec<PaymentInstruction>,
    pub payment_hold: Option<String>,
    pub documents: Vec<DocumentRecord>,
    pub licence_history: Vec<LicenceRecord>,
    pub ceased: Option<(NaiveDate, CeasedReason)>,
    pub open_cases: Vec<String>,
    pub notes: Vec<(String, PositionNote)>,
    pub actions_in_progress: Vec<String>,
    /// Owed to the insurer in another module's ledger and still outstanding.
    pub memo: Vec<MemoLine>,
}

/// A statement of what a party owes and is owed, by period, with lineage,
/// in a form that can be lodged with a liquidator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProofOfDebt {
    pub party: PartyId,
    pub as_at: NaiveDate,
    /// Period, account, amount, and the ledger references behind it.
    pub lines: Vec<(String, PartyLedger, i64, Vec<String>)>,
    pub owed_by_party_minor: i64,
    pub owed_to_party_minor: i64,
    pub net_minor: i64,
    pub hash: String,
}

/// The daily control: the held total across all parties, aged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlSnapshot {
    pub date: NaiveDate,
    pub held_total_minor: i64,
    pub parties: u32,
    /// Up to 30 days, 31 to 90, 91 to 365, older.
    pub aged_minor: [i64; 4],
    /// The total agrees with the held accounts as the ledger confirmed them.
    pub agrees_with_ledger: bool,
}

fn account(p: &PartyId, ledger: PartyLedger) -> CounterpartyAccount {
    CounterpartyAccount::Party { party: p.clone(), ledger }
}

fn line(account: CounterpartyAccount, amount_minor: i64) -> CounterpartyLine {
    CounterpartyLine { account, amount_minor }
}

/// Take `amount` from per-partition balances, in partition order.
fn take(balances: &[(PartitionId, i64)], amount: i64) -> Vec<(PartitionId, i64)> {
    let mut left = amount;
    let mut out = vec![];
    for (p, b) in balances.iter().filter(|(_, b)| *b > 0) {
        let t = left.min(*b);
        if t > 0 {
            out.push((*p, t));
            left -= t;
        }
    }
    out
}

impl DistributionState {
    pub fn summary(&self, party: &PartyId) -> Summary {
        let b = |l| self.balance(party, l);
        let mut s = Summary {
            held_minor: b(PartyLedger::Held),
            commission_minor: b(PartyLedger::CommissionPayable),
            receivable_minor: b(PartyLedger::Receivable),
            retention_minor: b(PartyLedger::Retention),
            advance_minor: b(PartyLedger::Advance),
            clearing_minor: b(PartyLedger::Clearing),
            net_minor: 0,
        };
        s.net_minor = s.held_minor + s.commission_minor + s.receivable_minor + s.retention_minor + s.advance_minor + s.clearing_minor;
        s
    }

    /// What the workbench shows for a party.
    pub fn position(&self, party: &PartyId, as_at: NaiveDate) -> Position {
        let mut held: BTreeMap<(String, Option<BookId>, Option<HeldReason>), (i64, Vec<String>)> = BTreeMap::new();
        for h in self.held.iter().filter(|h| &h.party == party && h.remaining_minor > 0) {
            let e = held.entry((h.date.format("%Y-%m").to_string(), h.book.clone(), h.reason)).or_default();
            e.0 += h.remaining_minor;
            e.1.push(h.reference.clone());
        }
        let node = self.config.structure.parties.get(party);
        let payee = self.payees.get(party);
        Position {
            party: party.clone(),
            summary: self.summary(party),
            held: held
                .into_iter()
                .map(|((period, book, reason), (amount_minor, entries))| HeldLine {
                    period,
                    book,
                    reason,
                    amount_minor,
                    entries,
                })
                .collect(),
            clawback_at_risk_minor: self.clawback_exposure(as_at).get(party).copied().unwrap_or(0),
            instructions: payee.map(|p| p.instructions.clone()).unwrap_or_default(),
            payment_hold: payee.and_then(|p| p.payment_hold.clone()),
            documents: self.documents.get(party).cloned().unwrap_or_default(),
            licence_history: node.map(|n| n.licence.clone()).unwrap_or_default(),
            ceased: node.and_then(|n| n.ceased),
            open_cases: self
                .cases
                .values()
                .filter(|c| c.closed.is_none() && (&c.subject == party || c.payees.contains(party)))
                .map(|c| c.id.clone())
                .collect(),
            notes: self
                .notes
                .iter()
                .filter(|(_, (p, _))| p == party)
                .map(|(r, (_, n))| (r.clone(), n.clone()))
                .collect(),
            actions_in_progress: self
                .actions
                .values()
                .filter(|a| &a.party == party && matches!(a.state, ActionState::Proposed | ActionState::Posting))
                .map(|a| a.id.clone())
                .collect(),
            memo: self.memo.values().filter(|m| &m.party == party && m.outstanding_minor != 0).cloned().collect(),
        }
    }

    /// The proof of debt extract for a party.
    pub fn proof_of_debt(&self, party: &PartyId, as_at: NaiveDate) -> ProofOfDebt {
        let mut by: BTreeMap<(String, PartyLedger), (i64, Vec<String>)> = BTreeMap::new();
        for m in self
            .movements
            .iter()
            .filter(|m| &m.party == party && m.date <= as_at && m.ledger != PartyLedger::Clearing)
        {
            let e = by.entry((m.date.format("%Y-%m").to_string(), m.ledger)).or_default();
            e.0 += m.amount_minor;
            e.1.push(m.reference.clone());
        }
        let lines: Vec<(String, PartyLedger, i64, Vec<String>)> = by
            .into_iter()
            .map(|((period, ledger), (amount, refs))| (period, ledger, amount, refs))
            .collect();
        let mut totals: BTreeMap<PartyLedger, i64> = BTreeMap::new();
        for (_, ledger, amount, _) in &lines {
            *totals.entry(*ledger).or_insert(0) += amount;
        }
        let owed_by: i64 = totals.values().filter(|v| **v < 0).map(|v| -v).sum();
        let owed_to: i64 = totals.values().filter(|v| **v > 0).sum();
        let mut h = Sha256::new();
        h.update(b"ic.distribution.proof_of_debt.v1");
        h.update(serde_json::to_vec(&(party, as_at, &lines)).unwrap_or_default());
        ProofOfDebt {
            party: party.clone(),
            as_at,
            lines,
            owed_by_party_minor: owed_by,
            owed_to_party_minor: owed_to,
            net_minor: owed_to - owed_by,
            hash: hex::encode(h.finalize()),
        }
    }

    /// The held total across all parties, aged, as at a day.
    pub fn held_control(&self, date: NaiveDate) -> ControlSnapshot {
        let mut aged = [0i64; 4];
        let mut parties = std::collections::BTreeSet::new();
        for h in self.held.iter().filter(|h| h.remaining_minor > 0) {
            let bucket = match (date - h.date).num_days() {
                d if d <= 30 => 0,
                d if d <= 90 => 1,
                d if d <= 365 => 2,
                _ => 3,
            };
            aged[bucket] += h.remaining_minor;
            parties.insert(&h.party);
        }
        let total: i64 = aged.iter().sum();
        let ledger: i64 = self.positions.keys().map(|p| self.balance(p, PartyLedger::Held)).sum();
        ControlSnapshot {
            date,
            held_total_minor: total,
            parties: parties.len() as u32,
            aged_minor: aged,
            agrees_with_ledger: total == ledger,
        }
    }

    /// The document behind a standing decision to keep a party's balance
    /// held: a completed hold whose review date is still ahead.
    pub fn held_decision(&self, party: &PartyId, date: NaiveDate) -> Option<String> {
        self.actions
            .values()
            .find(|a| &a.party == party && a.state == ActionState::Completed && matches!(a.action, Action::HoldPending { review_on, .. } if review_on > date))
            .map(|a| a.document_ref.clone())
    }

    /// Whether a payment for the party is with the payments module.
    pub fn payment_outstanding(&self, party: &PartyId) -> bool {
        self.payments.values().any(|p| &p.party == party)
    }

    /// Month-end: parties whose held balance older than the threshold age is
    /// above the threshold amount. Each blocks sign-off until it has a
    /// decision in the workbench with a review date still ahead.
    pub fn month_end_blocks(&self, date: NaiveDate) -> Vec<(PartyId, i64)> {
        let mut old: BTreeMap<PartyId, i64> = BTreeMap::new();
        for h in self
            .held
            .iter()
            .filter(|h| h.remaining_minor > 0 && (date - h.date).num_days() > self.settings.month_end_held_age_days as i64)
        {
            *old.entry(h.party.clone()).or_insert(0) += h.remaining_minor;
        }
        old.into_iter()
            .filter(|(party, amount)| *amount > self.settings.month_end_held_threshold_minor && self.held_decision(party, date).is_none())
            .collect()
    }

    pub(crate) fn record_daily_control(&mut self, ctx: &mut Ctx) -> Result<(), Reject> {
        let date = self.date()?;
        let snapshot = self.held_control(date);
        if !snapshot.agrees_with_ledger {
            self.task(ctx, "held_control_break", "held", date, "held items do not sum to the held accounts".into());
        }
        // Reviews that fall due today become tasks.
        let due: Vec<(String, PartyId)> = self
            .actions
            .values()
            .filter(|a| a.state == ActionState::Completed && matches!(a.action, Action::HoldPending { review_on, .. } if review_on == date))
            .map(|a| (a.id.clone(), a.party.clone()))
            .collect();
        for (id, party) in due {
            self.task(ctx, "held_review", &party.0, date, format!("review of held balance under action {id}"));
        }
        ctx.emit(Event::ControlRecorded {
            date,
            held_total_minor: snapshot.held_total_minor,
            parties: snapshot.parties,
        });
        self.controls.retain(|c| c.date != date);
        self.controls.push(snapshot);
        Ok(())
    }

    /// Held money not already spoken for by an action on its way to the ledger.
    fn held_available(&self, party: &PartyId) -> Vec<(PartitionId, i64)> {
        let mut balances = self.by_partition(party, PartyLedger::Held);
        for r in self.requested.values().filter(|r| r.state == PostingState::Pending) {
            for l in &r.posting.lines {
                if matches!(&l.account, CounterpartyAccount::Party { party: p, ledger: PartyLedger::Held } if p == party) && l.amount_minor > 0 {
                    if let Some((_, b)) = balances.iter_mut().find(|(p, _)| *p == r.partition) {
                        *b -= l.amount_minor;
                    }
                }
            }
        }
        balances
    }

    /// What the party owes on an account, by partition, as positive amounts.
    fn owed(&self, party: &PartyId, debt: Debt) -> Vec<(PartitionId, i64)> {
        self.by_partition(party, debt.ledger())
            .into_iter()
            .filter(|(_, v)| *v < 0)
            .map(|(p, v)| (p, -v))
            .collect()
    }

    fn amount_and_limit(action: &Action) -> (i64, LimitKind) {
        match action {
            Action::Release { amount_minor, .. } => (*amount_minor, LimitKind::Release),
            Action::Offset { amount_minor, .. } | Action::MoveToGuarantor { amount_minor, .. } => (*amount_minor, LimitKind::Offset),
            Action::WriteOffHeld { amount_minor } | Action::WriteOffDebt { amount_minor, .. } => (*amount_minor, LimitKind::WriteOff),
            Action::Split { shares } => (shares.iter().map(|s| s.amount_minor).sum(), LimitKind::Split),
            Action::RecordDividend { received_minor, .. } => (*received_minor, LimitKind::Dividend),
            Action::HoldPending { .. } | Action::SetRecovery { .. } => (0, LimitKind::Offset),
        }
    }

    /// Everything that must be true for an action to go ahead. Checked when
    /// it is proposed and again when the last approval arrives.
    fn check_action(&self, party: &PartyId, action: &Action, today: NaiveDate) -> Result<(), Reject> {
        let held: i64 = self.held_available(party).iter().map(|(_, v)| *v).sum();
        let need_held = |amount: i64| -> Result<(), Reject> {
            if amount <= 0 {
                return reject("invalid_amount", "the amount must be positive");
            }
            if amount > held {
                return reject("insufficient_held", format!("{party} has {held} held and not already committed"));
            }
            Ok(())
        };
        let need_debt = |amount: i64, debt: Debt| -> Result<i64, Reject> {
            let owed: i64 = self.owed(party, debt).iter().map(|(_, v)| *v).sum();
            if amount <= 0 || amount > owed {
                return reject("insufficient_debt", format!("{party} owes {owed} on that account"));
            }
            Ok(owed)
        };
        let recovery_held = self.notes.values().any(|(p, n)| p == party && n.recovery == RecoveryStatus::Held);
        match action {
            Action::Release { amount_minor, account_ref } => {
                need_held(*amount_minor)?;
                let payee = self
                    .payees
                    .get(party)
                    .ok_or_else(|| Reject::new("unknown_payee", format!("{party} is not a registered payee")))?;
                if payee.account_on(account_ref, today).is_none() {
                    return reject("account_not_verified", "the account is not on the party, or is still inside its cooling-off");
                }
                Ok(())
            }
            Action::Offset { amount_minor, against } => {
                need_held(*amount_minor)?;
                need_debt(*amount_minor, *against)?;
                if *against == Debt::Receivable && recovery_held {
                    return reject(
                        "recovery_held",
                        "no offset while the recovery status is held; moving it on needs the conduct officer and legal",
                    );
                }
                Ok(())
            }
            Action::HoldPending { review_on, legal_ref } => {
                if *review_on <= today || legal_ref.trim().is_empty() {
                    return reject("invalid_hold", "a hold has a review date ahead and a legal reference");
                }
                if held <= 0 {
                    return reject("insufficient_held", format!("{party} has nothing held"));
                }
                Ok(())
            }
            Action::WriteOffHeld { amount_minor } => {
                need_held(*amount_minor)?;
                let oldest = self.held.iter().filter(|h| &h.party == party && h.remaining_minor > 0).map(|h| h.date).min();
                let aged = oldest.is_some_and(|d| (today - d).num_days() >= self.settings.write_off_age_days as i64);
                if !(aged || held <= self.settings.write_off_floor_minor) {
                    return reject("write_off_not_allowed", "a held balance is written off only above the age or below the floor");
                }
                Ok(())
            }
            Action::WriteOffDebt { amount_minor, debt } => need_debt(*amount_minor, *debt).map(|_| ()),
            Action::Split { shares } => {
                let total: i64 = shares.iter().map(|s| s.amount_minor).sum();
                if shares.is_empty() || shares.iter().any(|s| s.amount_minor <= 0 || s.agreement_ref.trim().is_empty()) {
                    return reject("invalid_split", "each party to a split has a positive amount and its agreement reference");
                }
                if let Some(s) = shares.iter().find(|s| !self.payees.contains_key(&s.party)) {
                    return reject("unknown_payee", format!("{} is not a registered payee", s.party));
                }
                need_held(total)
            }
            Action::RecordDividend {
                debt,
                received_minor,
                receipt_ref,
            } => {
                if receipt_ref.trim().is_empty() || *received_minor < 0 {
                    return reject("invalid_dividend", "a dividend has a receipt reference and is not negative");
                }
                let owed: i64 = self.owed(party, *debt).iter().map(|(_, v)| *v).sum();
                if owed <= 0 || *received_minor > owed {
                    return reject("insufficient_debt", format!("{party} owes {owed} on that account"));
                }
                Ok(())
            }
            Action::MoveToGuarantor { guarantor, amount_minor } => {
                need_debt(*amount_minor, Debt::Commission)?;
                let guaranteed = self
                    .config
                    .books
                    .values()
                    .flat_map(|b| b.rules.iter())
                    .flat_map(|r| r.liability.iter())
                    .any(|l| &l.liable == party && l.guarantor.as_ref() == Some(guarantor))
                    || self
                        .config
                        .fragments
                        .values()
                        .flatten()
                        .flat_map(|f| f.liability.iter())
                        .any(|l| &l.liable == party && l.guarantor.as_ref() == Some(guarantor));
                if !guaranteed {
                    return reject("not_a_guarantor", format!("no rule names {guarantor} as guarantor of {party}"));
                }
                Ok(())
            }
            Action::SetRecovery { reference, to } => {
                let (p, note) = self
                    .notes
                    .get(reference)
                    .ok_or_else(|| Reject::new("unknown_reference", format!("{reference} is not a described position")))?;
                if p != party || note.recovery == *to {
                    return reject("invalid_recovery", "the status must change, on a position of this party");
                }
                Ok(())
            }
        }
    }

    /// The signatures an action needs. Above the threshold: two approvers
    /// and legal. A release to an account held under an appointment for a
    /// party in liquidation or deceased: legal, whatever the amount. A larger
    /// write-off: a second approver. Pursuing an overpayment that is held:
    /// the conduct officer and legal.
    fn signatures(&self, party: &PartyId, action: &Action, amount: i64) -> Vec<Role> {
        let mut needed = vec![Role::CounterpartyFinanceApprover];
        let mut legal = false;
        if amount >= self.settings.two_approver_threshold_minor {
            needed.push(Role::CounterpartyFinanceApprover);
            legal = true;
        }
        match action {
            Action::Release { account_ref, .. } => {
                let ceased = self
                    .config
                    .structure
                    .parties
                    .get(party)
                    .and_then(|n| n.ceased)
                    .is_some_and(|(_, r)| matches!(r, CeasedReason::Liquidation | CeasedReason::Deceased));
                let appointed = self
                    .payees
                    .get(party)
                    .is_some_and(|p| p.accounts.iter().any(|a| &a.account_ref == account_ref && a.appointment_ref.is_some()));
                if ceased || appointed {
                    legal = true;
                }
            }
            Action::WriteOffHeld { .. } | Action::WriteOffDebt { .. } | Action::RecordDividend { .. } => {
                if amount >= self.settings.write_off_second_approver_minor && needed.len() < 2 {
                    needed.push(Role::CounterpartyFinanceApprover);
                }
            }
            Action::SetRecovery { reference, to } => {
                let from_held = self.notes.get(reference).is_some_and(|(_, n)| n.recovery == RecoveryStatus::Held);
                if from_held && *to != RecoveryStatus::WrittenOff {
                    needed.push(Role::ConductOfficer);
                    legal = true;
                }
            }
            Action::HoldPending { .. } => legal = true,
            _ => {}
        }
        if legal {
            needed.push(Role::Legal);
        }
        needed
    }

    pub(crate) fn propose_action(
        &mut self,
        ctx: &mut Ctx,
        actor: &Actor,
        id: String,
        party: PartyId,
        action: Action,
        reason_code: String,
        document_ref: String,
    ) -> Result<(), Reject> {
        let staff = actor.person()?;
        let today = self.date()?;
        if self.actions.contains_key(&id) {
            return reject("duplicate_action", format!("action {id} already exists"));
        }
        if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') || id.is_empty() {
            return reject("invalid_action", "an action ID is letters, digits and hyphens");
        }
        if reason_code.trim().is_empty() || document_ref.trim().is_empty() {
            return reject("invalid_action", "every action carries a reason code and a document reference");
        }
        let (amount, limit) = Self::amount_and_limit(&action);
        let maker = self.authorise(staff, &Ask::new("workbench.prepare", &party.0).amount(limit, amount))?;
        self.check_action(&party, &action, today)?;
        ctx.emit(Event::ActionProposed {
            id: id.clone(),
            party: party.clone(),
            kind: action.kind().to_string(),
            amount_minor: amount,
        });
        let record = ActionRecord {
            id: id.clone(),
            needed: self.signatures(&party, &action, amount),
            position_shown: self.summary(&party),
            party,
            action,
            reason_code,
            document_ref,
            amount_minor: amount,
            maker,
            proposed_on: today,
            approvals: vec![],
            state: ActionState::Proposed,
            failed: None,
            rejected_because: None,
        };
        self.actions.insert(id, record);
        Ok(())
    }

    pub(crate) fn reject_action(&mut self, ctx: &mut Ctx, actor: &Actor, id: &str, reason: String) -> Result<(), Reject> {
        let staff = actor.person()?;
        let record = self
            .actions
            .get(id)
            .ok_or_else(|| Reject::new("unknown_action", format!("action {id} is not known")))?;
        if record.state != ActionState::Proposed {
            return reject("action_settled", "the action has already been approved or rejected");
        }
        self.authorise(staff, &Ask::new("workbench.view", &record.party.0))?;
        if let Some(a) = self.actions.get_mut(id) {
            a.state = ActionState::Rejected;
            a.rejected_because = Some(reason);
        }
        ctx.emit(Event::ActionRejected { id: id.to_string() });
        Ok(())
    }

    pub(crate) fn approve_action(&mut self, ctx: &mut Ctx, actor: &Actor, id: &str, as_role: Role) -> Result<(), Reject> {
        let staff = actor.person()?;
        let today = self.date()?;
        let record = self
            .actions
            .get(id)
            .ok_or_else(|| Reject::new("unknown_action", format!("action {id} is not known")))?
            .clone();
        if record.state != ActionState::Proposed {
            return reject("action_settled", "the action has already been approved or rejected");
        }
        let Some(at) = record.needed.iter().position(|r| *r == as_role) else {
            return reject("approval_not_needed", format!("the action is not waiting for a {} signature", as_role.key()));
        };
        if record.approvals.iter().any(|(_, d)| d.staff == staff.id) {
            return reject("not_authorised", "each signature comes from a different person");
        }
        let (amount, limit) = Self::amount_and_limit(&record.action);
        let is_maker = record.maker.staff == staff.id;
        let ask = match as_role {
            Role::CounterpartyFinanceApprover => Ask::new("workbench.approve", &record.party.0).amount(limit, amount),
            Role::Legal => Ask::new("workbench.approve_legal", &record.party.0),
            Role::ConductOfficer => Ask::new("workbench.approve_conduct", &record.party.0),
            _ => return reject("approval_not_needed", "that role does not sign workbench actions"),
        }
        .maker(is_maker);
        let decision = self.authorise(staff, &ask)?;
        let last = record.needed.len() == 1;
        if last {
            // The position may have moved since the action was prepared.
            self.check_action(&record.party, &record.action, today)?;
        }
        if let Some(a) = self.actions.get_mut(id) {
            a.needed.remove(at);
            a.approvals.push((as_role, decision));
        }
        ctx.emit(Event::ActionApproved {
            id: id.to_string(),
            by: staff.id.clone(),
            role: as_role,
        });
        if last {
            self.execute(ctx, &record, today);
        }
        Ok(())
    }

    /// Carry out an approved action. Everything was checked; this only posts.
    fn execute(&mut self, ctx: &mut Ctx, record: &ActionRecord, today: NaiveDate) {
        let c = Currency::NZD;
        let party = &record.party;
        let id = &record.id;
        let mut postings: Vec<(PartitionId, &'static str, Vec<CounterpartyLine>)> = vec![];
        let held = self.held_available(party);
        match &record.action {
            Action::Release { amount_minor, account_ref } => {
                let taken = take(&held, *amount_minor);
                for (p, a) in &taken {
                    postings.push((
                        *p,
                        "held_released",
                        vec![line(account(party, PartyLedger::Held), *a), line(account(party, PartyLedger::Clearing), -*a)],
                    ));
                }
                let part = PaymentPart {
                    reference: format!("pay:{id}"),
                    account_ref: account_ref.clone(),
                    amount_minor: *amount_minor,
                    settled: None,
                };
                ctx.emit(Event::PaymentRequested {
                    reference: part.reference.clone(),
                    party: party.clone(),
                    account_ref: account_ref.clone(),
                    amount_minor: *amount_minor,
                    currency: c,
                });
                self.payments.insert(
                    format!("action-{id}"),
                    PaymentRecord {
                        owner: PaymentOwner::Action,
                        owner_id: id.clone(),
                        party: party.clone(),
                        currency: c,
                        parts: vec![part],
                        clearing: taken,
                        return_to: PartyLedger::Held,
                    },
                );
            }
            Action::Offset { amount_minor, against } => {
                // One netting entry per partition reduces both sides; where
                // the two sit on different partitions the difference passes
                // through the party's clearing account, which nets to zero.
                let from_held: BTreeMap<PartitionId, i64> = take(&held, *amount_minor).into_iter().collect();
                let from_debt: BTreeMap<PartitionId, i64> = take(&self.owed(party, *against), *amount_minor).into_iter().collect();
                let partitions: std::collections::BTreeSet<PartitionId> = from_held.keys().chain(from_debt.keys()).copied().collect();
                for p in partitions {
                    let h = from_held.get(&p).copied().unwrap_or(0);
                    let d = from_debt.get(&p).copied().unwrap_or(0);
                    let mut lines = vec![];
                    if h != 0 {
                        lines.push(line(account(party, PartyLedger::Held), h));
                    }
                    if d != 0 {
                        lines.push(line(account(party, against.ledger()), -d));
                    }
                    if h != d {
                        lines.push(line(account(party, PartyLedger::Clearing), d - h));
                    }
                    postings.push((p, "held_offset", lines));
                }
            }
            Action::HoldPending { review_on, legal_ref } => {
                self.task(ctx, "held_review", &party.0, *review_on, format!("held pending under {legal_ref}; action {id}"));
            }
            Action::WriteOffHeld { amount_minor } => {
                for (p, a) in take(&held, *amount_minor) {
                    let contra = CounterpartyAccount::Internal(InternalLedger::WriteOff { class: "held".into() });
                    postings.push((p, "held_written_off", vec![line(account(party, PartyLedger::Held), a), line(contra, -a)]));
                }
            }
            Action::WriteOffDebt { amount_minor, debt } => {
                for (p, a) in take(&self.owed(party, *debt), *amount_minor) {
                    postings.push((
                        p,
                        "debt_written_off",
                        vec![line(self.write_off_account(party, *debt), a), line(account(party, debt.ledger()), -a)],
                    ));
                }
            }
            Action::Split { shares } => {
                let total: i64 = shares.iter().map(|s| s.amount_minor).sum();
                let taken = take(&held, total);
                // Each party's amount is drawn across the partitions in turn.
                let mut left: Vec<i64> = shares.iter().map(|s| s.amount_minor).collect();
                for (p, a) in taken {
                    let mut lines = vec![line(account(party, PartyLedger::Held), a)];
                    let mut room = a;
                    for (share, l) in shares.iter().zip(left.iter_mut()) {
                        let t = room.min(*l);
                        if t > 0 {
                            lines.push(line(account(&share.party, PartyLedger::CommissionPayable), -t));
                            *l -= t;
                            room -= t;
                        }
                    }
                    postings.push((p, "held_split", lines));
                }
            }
            Action::RecordDividend { debt, received_minor, .. } => {
                let owed = self.owed(party, *debt);
                let total: i64 = owed.iter().map(|(_, v)| *v).sum();
                let mut received_left = *received_minor;
                let count = owed.len();
                for (n, (p, d)) in owed.into_iter().enumerate() {
                    // Cents in the dollar on each partition; the last takes the odd cent.
                    let r = if n + 1 == count {
                        received_left
                    } else {
                        portion(d, ((*received_minor as i128 * 1_000_000) / total as i128) as u32).min(received_left)
                    };
                    received_left -= r;
                    let mut lines = vec![line(account(party, debt.ledger()), -d)];
                    if r != 0 {
                        lines.push(line(CounterpartyAccount::Internal(InternalLedger::Settlement), r));
                    }
                    if d - r != 0 {
                        lines.push(line(self.write_off_account(party, *debt), d - r));
                    }
                    postings.push((p, "dividend_received", lines));
                }
            }
            Action::MoveToGuarantor { guarantor, amount_minor } => {
                for (p, a) in take(&self.owed(party, Debt::Commission), *amount_minor) {
                    postings.push((
                        p,
                        "debt_moved_to_guarantor",
                        vec![
                            line(account(guarantor, PartyLedger::CommissionPayable), a),
                            line(account(party, PartyLedger::CommissionPayable), -a),
                        ],
                    ));
                }
            }
            Action::SetRecovery { reference, to } => {
                if let Some((_, n)) = self.notes.get_mut(reference) {
                    n.recovery = *to;
                }
            }
        }
        let none = postings.is_empty();
        for (n, (partition, kind, lines)) in postings.into_iter().enumerate() {
            let mut posting = simple_posting(format!("wb:{id}:{}", n + 1), kind, c, lines, id);
            posting.detail.insert("reason_code".into(), record.reason_code.clone());
            posting.detail.insert("document".into(), record.document_ref.clone());
            self.request(ctx, partition, posting, Some(id.clone()));
        }
        if let Some(a) = self.actions.get_mut(id) {
            a.state = if none { ActionState::Completed } else { ActionState::Posting };
        }
        if none {
            ctx.emit(Event::ActionCompleted { id: id.clone() });
        }
        let _ = today;
    }

    fn write_off_account(&self, party: &PartyId, debt: Debt) -> CounterpartyAccount {
        match debt {
            Debt::Commission => CounterpartyAccount::Internal(InternalLedger::CommissionBadDebt),
            Debt::Receivable => {
                let class = self
                    .notes
                    .values()
                    .find(|(p, _)| p == party)
                    .map_or("receivable".to_string(), |(_, n)| n.class.clone());
                CounterpartyAccount::Internal(InternalLedger::WriteOff { class })
            }
        }
    }

    /// A posting that belongs to an action or a change landed. The action is
    /// complete when all of its postings have, and no payment is outstanding.
    pub(crate) fn posting_confirmed_for(&mut self, ctx: &mut Ctx, owner: &str) {
        let outstanding = self
            .requested
            .values()
            .any(|r| r.owner.as_deref() == Some(owner) && r.state == PostingState::Pending)
            || self.payments.values().any(|p| p.owner == PaymentOwner::Action && p.owner_id == owner);
        if outstanding {
            return;
        }
        if let Some(a) = self.actions.get_mut(owner).filter(|a| a.state == ActionState::Posting) {
            a.state = ActionState::Completed;
            ctx.emit(Event::ActionCompleted { id: owner.to_string() });
        }
    }
}
