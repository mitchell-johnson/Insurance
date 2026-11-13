//! The claims state machine: every claim on a partition, the runs that pay
//! them and the configuration they are decided under.
//!
//! One rule keeps a claim and the ledger in step. A command sends the core at
//! most one instruction, checks everything it can before sending it, and
//! changes its own state only after the core has accepted. A command that is
//! refused, by the module or by the core, leaves nothing behind.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{Days, Months, NaiveDate};
use serde::{Deserialize, Serialize};

use crate::command::{Actor, Command};
use crate::config::{AuthorityBook, Role, Rules, Staff};
use crate::model::{Claim, Due, HistoryItem, PreApproval, Run, Status, Task, TaskKind, Timer, TimerKind};
use crate::port::{Core, Reject};
use crate::{ClaimId, ClaimType, StaffId};

/// Claims whose due items one command works through. The rest wait for the
/// next call, so no single command can hold the partition.
pub const DUE_CHUNK: usize = 100;

/// A configuration version that was activated, and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Activation {
    pub what: String,
    pub version: u32,
    pub content_hash: String,
    pub on: NaiveDate,
}

/// Where the exclusion-match control has got to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Control {
    pub last_run: Option<NaiveDate>,
    /// The last claim looked at, while a pass is part way through.
    pub cursor: Option<String>,
    pub scanned: u64,
    pub hits: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ClaimsState {
    pub authority: AuthorityBook,
    pub rules: Rules,
    pub activations: Vec<Activation>,
    pub claims: BTreeMap<ClaimId, Claim>,
    pub runs: BTreeMap<String, Run>,
    /// Open holds against health limits, by the pre-approval claim.
    pub pre_approvals: BTreeMap<ClaimId, PreApproval>,
    /// Health benefit used: per policy year and over the lifetime.
    pub health_used: BTreeMap<String, i64>,
    pub control: Control,
    /// Claims of a policy that is moving to another partition: out of every
    /// index and unreachable until the move completes or is called off.
    #[serde(default)]
    pub in_transfer: BTreeMap<String, Moved>,
    // Indexes, rebuilt by `commit` from the claims themselves.
    pub due_index: BTreeSet<(NaiveDate, ClaimId)>,
    pub by_policy: BTreeMap<String, BTreeSet<ClaimId>>,
    pub by_party: BTreeMap<String, BTreeSet<ClaimId>>,
    pub in_payment: BTreeSet<ClaimId>,
    pub provider_payable: BTreeSet<ClaimId>,
    /// Claims with an overpayment being recovered by offset.
    pub offsetting: BTreeSet<ClaimId>,
    /// Health claims by the service billed: date, provider, service code.
    #[serde(default)]
    pub billed: BTreeSet<(NaiveDate, String, String, ClaimId)>,
    /// Restricted investigations, by case.
    #[serde(default)]
    pub investigations: BTreeMap<String, crate::investigate::Investigation>,
    /// Overpayments held on other partitions that this partition's benefits
    /// may recover, by `party|partition|overpayment`.
    #[serde(default)]
    pub remote_offsets: BTreeMap<String, crate::offsets::RemoteOffset>,
    /// Offsets taken and returned across partitions that have been dealt
    /// with, so a repeated message changes nothing.
    #[serde(default)]
    pub offset_keys: BTreeSet<String>,
    /// Claims whose overpayments changed in the command being applied, to
    /// be shared at its end. Always empty between commands.
    #[serde(skip)]
    pub(crate) to_share: BTreeSet<ClaimId>,
    pub owned: BTreeMap<StaffId, BTreeSet<ClaimId>>,
}

/// Everything claims holds for one policy, as it travels with the policy.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Moved {
    pub claims: Vec<Claim>,
    pub pre_approvals: BTreeMap<ClaimId, PreApproval>,
    /// Health benefit used by the policy's lives, by the same keys.
    pub health_used: BTreeMap<String, i64>,
}

impl ClaimsState {
    /// What would travel with a policy. Refused while one of its claims is
    /// in a payment run that has not been approved or cancelled.
    pub fn moving(&self, policy: &str) -> Result<Moved, Reject> {
        let ids = self.by_policy.get(policy).cloned().unwrap_or_default();
        let claims: Vec<Claim> = ids.iter().filter_map(|id| self.claims.get(id)).cloned().collect();
        if claims.iter().any(|c| c.in_run.is_some()) {
            return Err(Reject::new(
                "claim_in_run",
                "a claim on this policy is in a payment run; approve or cancel the run first",
            ));
        }
        let prefix = format!("{policy}/");
        Ok(Moved {
            pre_approvals: self
                .pre_approvals
                .iter()
                .filter(|(id, _)| ids.contains(*id))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            health_used: self
                .health_used
                .iter()
                .filter(|(k, _)| k.starts_with(&prefix))
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            claims,
        })
    }

    /// The policy has started to move: its claims leave every index and
    /// wait, so nothing can be done to them on either side meanwhile.
    pub fn detach(&mut self, policy: &str) -> Result<(), Reject> {
        let moved = self.moving(policy)?;
        for c in &moved.claims {
            if let Some(old) = self.claims.remove(&c.id) {
                self.index(&old, false);
            }
            self.pre_approvals.remove(&c.id);
        }
        for key in moved.health_used.keys() {
            self.health_used.remove(key);
        }
        self.by_policy.remove(policy);
        self.in_transfer.insert(policy.to_string(), moved);
        Ok(())
    }

    /// The move was called off: the claims are back as they were.
    pub fn reattach(&mut self, policy: &str) {
        if let Some(moved) = self.in_transfer.remove(policy) {
            self.attach(moved);
        }
    }

    /// The policy has landed elsewhere: this partition lets its claims go.
    pub fn release(&mut self, policy: &str) {
        self.in_transfer.remove(policy);
    }

    /// Claims arriving with a policy: registered again with their timers,
    /// schedules and limits exactly as they left.
    pub fn attach(&mut self, moved: Moved) {
        self.pre_approvals.extend(moved.pre_approvals);
        self.health_used.extend(moved.health_used);
        for claim in moved.claims {
            self.commit(claim);
        }
    }
}

/// What a handler has to hand while one command applies.
pub(crate) struct Cx<'a> {
    pub core: &'a mut dyn Core,
    pub index: u64,
    pub today: NaiveDate,
}

impl Cx<'_> {
    /// A payment was released. This notice is the payments module's
    /// instruction to pay: it carries the payee and the net amount, and the
    /// gross amount the reinsurance module recovers against.
    ///
    /// The policy is the one whose ledger holds the benefit payable. The
    /// account reference is the verified payee's; it is blank for a
    /// provider, whose account the payments module holds.
    pub fn paid(
        &mut self,
        claim: &Claim,
        payment_ref: &str,
        gross_minor: i64,
        net_minor: i64,
        payee: &str,
        period_end: Option<NaiveDate>,
        ex_gratia: bool,
        interest_minor: i64,
    ) {
        let account_ref = claim
            .payee
            .as_ref()
            .filter(|p| p.party == payee)
            .map(|p| p.account_ref.clone())
            .unwrap_or_default();
        // The run it went out in, so the payments module can confirm the run once every item has settled.
        let lump = claim.payments.iter().find(|p| p.payment_ref == payment_ref).and_then(|p| p.run.clone());
        let periods = claim.schedule.iter().flat_map(|s| s.periods.values());
        let run = lump.or_else(|| periods.filter(|p| p.payment_ref == payment_ref).map(|p| p.run.clone()).next());
        let detail = [
            ("policy", claim.policy.clone()),
            ("run", run.unwrap_or_default()),
            ("account_ref", account_ref),
            ("payment_ref", payment_ref.to_string()),
            ("amount_minor", gross_minor.to_string()),
            ("net_minor", net_minor.to_string()),
            ("payee", payee.to_string()),
            ("period_end", period_end.map(|d| d.to_string()).unwrap_or_default()),
            ("ex_gratia", ex_gratia.to_string()),
            ("interest_minor", interest_minor.to_string()),
        ];
        self.notify("claim_paid", &claim.id, &detail);
    }

    pub fn notify(&mut self, kind: &str, claim: &str, detail: &[(&str, String)]) {
        self.core.notify(kind, claim, detail.iter().map(|(k, v)| (k.to_string(), v.clone())).collect());
    }
}

pub(crate) fn plus_days(d: NaiveDate, n: u32) -> NaiveDate {
    d.checked_add_days(Days::new(u64::from(n))).unwrap_or(NaiveDate::MAX)
}

pub(crate) fn plus_months(d: NaiveDate, n: u32) -> NaiveDate {
    d.checked_add_months(Months::new(n)).unwrap_or(NaiveDate::MAX)
}

fn toggle<K: Ord>(set: &mut BTreeSet<K>, key: K, add: bool) {
    if add {
        set.insert(key);
    } else {
        set.remove(&key);
    }
}

/// Add a line to the claim's history. A named person who acted has touched
/// the claim, and so can never be its quality reviewer.
pub(crate) fn note(claim: &mut Claim, cx: &Cx<'_>, by: Option<&Actor>, action: impl Into<String>) {
    if let Some(a) = by {
        claim.touched_by.insert(a.staff.clone());
    }
    claim.history.push(HistoryItem {
        on: cx.today,
        log_index: cx.index,
        by: by.map(|a| a.staff.clone()),
        purpose: by.map(|a| a.purpose.clone()),
        action: action.into(),
    });
}

/// Open a task, unless one with the same ID is already there.
pub(crate) fn open_task(claim: &mut Claim, cx: &Cx<'_>, id: impl Into<String>, kind: TaskKind, due: Option<NaiveDate>) -> bool {
    let id = id.into();
    if claim.tasks.iter().any(|t| t.id == id) {
        return false;
    }
    claim.tasks.push(Task {
        id,
        kind,
        opened_on: cx.today,
        due,
        completed_on: None,
        completed_by: None,
        outcome: None,
        attendees: vec![],
        vault_ref: None,
        spend_minor: 0,
        overdue: false,
    });
    true
}

impl ClaimsState {
    /// Apply one logged command. `index` is its place in the partition's log.
    pub fn apply(&mut self, core: &mut dyn Core, index: u64, command: Command) -> Result<(), Reject> {
        core.work(1);
        let today = core.business_date();
        let cx = &mut Cx { core, index, today };
        self.to_share.clear();
        let result = self.dispatch(cx, index, command);
        if result.is_ok() {
            self.share_offsets(cx);
        }
        self.to_share.clear();
        result
    }

    fn dispatch(&mut self, cx: &mut Cx<'_>, index: u64, command: Command) -> Result<(), Reject> {
        match command {
            Command::ActivateAuthority { book } => self.activate_authority(cx, *book),
            Command::ActivateRules { rules } => self.activate_rules(cx, *rules),
            Command::ApplyValuation { valuation } => self.apply_valuation(cx, *valuation),
            Command::Notify(n) => self.notify(cx, *n),
            Command::LoadMigrated { claim, health_used } => self.load_migrated(cx, *claim, health_used),
            Command::Acknowledge { claim, delivery_ref } => self.acknowledge(cx, &claim, &delivery_ref),
            Command::Assign { claim, to, actor } => self.assign(cx, &claim, &to, &actor),
            Command::RecordContact { claim, actor } => self.record_contact(cx, &claim, &actor),
            Command::Escalate { claim, actor } => self.escalate(cx, &claim, &actor),
            Command::IdentifyLife { claim, party, actor } => self.identify_life(cx, &claim, &party, &actor),
            Command::RecordCause { claim, cause_code, actor } => self.record_cause(cx, &claim, &cause_code, &actor),
            Command::CoverCheck { claim, relied_on, actor } => self.cover_check(cx, &claim, relied_on, &actor),
            Command::RequestEvidence {
                claim,
                evidence,
                kind,
                from,
                actor,
            } => self.request_evidence(cx, &claim, evidence, kind, from, &actor),
            Command::ReceiveEvidence {
                claim,
                evidence,
                vault_ref,
                cost_minor,
            } => self.receive_evidence(cx, &claim, &evidence, vault_ref, cost_minor),
            Command::OpenTask { claim, task, kind, due, actor } => self.open_task_command(cx, &claim, task, kind, due, &actor),
            Command::CompleteTask {
                claim,
                task,
                outcome,
                attendees,
                vault_ref,
                spend_minor,
                actor,
            } => self.complete_task(cx, &claim, &task, outcome, attendees, vault_ref, spend_minor, &actor),
            Command::OpenFile { claim, actor } => self.open_file(cx, &claim, &actor),
            Command::Decide {
                claim,
                request,
                reasons,
                actor,
            } => self.decide(cx, &claim, request, reasons, &actor),
            Command::Approve { claim, actor } => self.approve(cx, &claim, &actor),
            Command::Refuse { claim, actor } => self.refuse(cx, &claim, &actor),
            Command::Reopen {
                claim,
                basis,
                reserve_minor,
                actor,
            } => self.reopen(cx, &claim, basis, reserve_minor, &actor),
            Command::VerifyPayee {
                claim,
                party,
                kind,
                account_ref,
                name_check_ref,
                screening_ref,
                authority_doc,
                actor,
            } => self.verify_payee(cx, &claim, party, kind, account_ref, name_check_ref, screening_ref, authority_doc, &actor),
            Command::Pay {
                claim,
                payment_ref,
                amount_minor,
                reissue_of,
                actor,
            } => self.pay(cx, &claim, payment_ref, amount_minor, reissue_of, &actor),
            Command::PayCost {
                claim,
                payment_ref,
                item,
                provider,
                actor,
            } => self.pay_cost(cx, &claim, payment_ref, &item, &provider, &actor),
            Command::StartWaiver { claim, from, actor } => self.start_waiver(cx, &claim, from, &actor),
            Command::PayInterest {
                claim,
                payment_ref,
                party,
                actor,
            } => self.pay_interest(cx, &claim, payment_ref, &party, &actor),
            Command::PaymentReturned { claim, payment_ref } => self.payment_returned(cx, &claim, &payment_ref),
            Command::DeclareIncome {
                claim,
                from,
                other_income_minor,
                actor,
            } => self.declare_income(cx, &claim, from, other_income_minor, &actor),
            Command::PrepareRun {
                run,
                kind,
                benefit_date,
                actor,
            } => self.prepare_run(cx, run, kind, benefit_date, &actor),
            Command::ApproveRun { run, actor } => self.approve_run(cx, &run, &actor),
            Command::CancelRun { run, actor } => self.cancel_run(cx, &run, &actor),
            Command::ConfirmRun { run, settled_minor } => self.confirm_run(cx, &run, settled_minor),
            Command::RaiseOverpayment {
                claim,
                overpayment,
                amount_minor,
                cause,
                good_faith,
                actor,
            } => self.raise_overpayment(cx, &claim, overpayment, amount_minor, cause, good_faith, &actor),
            Command::RecordRepayment {
                claim,
                overpayment,
                amount_minor,
            } => self.record_repayment(cx, &claim, &overpayment, amount_minor),
            Command::ExclusionControl { after } => self.exclusion_control(cx, after),
            Command::RecordDeath { party, date_of_death } => self.record_death(cx, &party, date_of_death),
            Command::RetractDeath { party, date_of_death } => self.retract_death(cx, &party, date_of_death),
            Command::OffsetSource {
                from,
                claim,
                policy,
                overpayment,
                party,
                outstanding_minor,
                pct,
                seq,
                settled_minor,
            } => self.offset_source(cx, from, claim, policy, overpayment, party, outstanding_minor, pct, seq, settled_minor),
            Command::OffsetTaken {
                from,
                take_ref,
                paid_claim,
                claim,
                overpayment,
                amount_minor,
            } => self.offset_taken(cx, from, &take_ref, &paid_claim, &claim, &overpayment, amount_minor),
            Command::OffsetReturned {
                from,
                take_ref,
                claim,
                amount_minor,
            } => self.offset_returned(cx, from, &take_ref, &claim, amount_minor),
            Command::ReferForInvestigation { case, rule, claims } => self.refer(cx, case, rule, "compliance", claims),
            Command::CloseInvestigation { case, outcome, actor } => self.close_case(cx, &case, outcome, &actor),
            Command::Reinsurance(notice) => self.reinsurance(cx, notice),
            Command::NonDisclosureOutcome { claim, review_ref, outcome } => self.non_disclosure_outcome(cx, &claim, &review_ref, outcome),
            Command::DrainDue => {
                self.drain_due(cx.core, index);
                Ok(())
            }
        }
    }

    fn activate_authority(&mut self, cx: &mut Cx<'_>, book: AuthorityBook) -> Result<(), Reject> {
        book.validate()?;
        if book.version <= self.authority.version {
            return Err(Reject::new(
                "config_conflict",
                format!("authority version {} is not after {}", book.version, self.authority.version),
            ));
        }
        self.activations.push(Activation {
            what: "authority".into(),
            version: book.version,
            content_hash: book.content_hash(),
            on: cx.today,
        });
        self.authority = book;
        Ok(())
    }

    fn activate_rules(&mut self, cx: &mut Cx<'_>, rules: Rules) -> Result<(), Reject> {
        rules.validate()?;
        if rules.version <= self.rules.version {
            return Err(Reject::new(
                "config_conflict",
                format!("rules version {} is not after {}", rules.version, self.rules.version),
            ));
        }
        self.activations.push(Activation {
            what: "rules".into(),
            version: rules.version,
            content_hash: rules.content_hash(),
            on: cx.today,
        });
        self.rules = rules;
        Ok(())
    }

    /// Set each claim in payment held here to the actuary's reserve. A part
    /// seen before is not applied again; a claim not held here belongs to
    /// another partition; a claim that is not in payment keeps its case
    /// reserve and is named back so the actuary can see what was skipped.
    fn apply_valuation(&mut self, cx: &mut Cx<'_>, valuation: crate::config::Valuation) -> Result<(), Reject> {
        valuation.validate()?;
        self.configured()?;
        let what = format!("valuation/{}/{}", valuation.version, valuation.part);
        let hash = valuation.content_hash();
        if let Some(seen) = self.activations.iter().find(|a| a.what == what) {
            if seen.content_hash == hash {
                return Ok(());
            }
            return Err(Reject::new("config_conflict", format!("{what} was applied with other figures")));
        }
        let latest = self
            .activations
            .iter()
            .filter(|a| a.what.starts_with("valuation/"))
            .map(|a| a.version)
            .max()
            .unwrap_or(0);
        if valuation.version < latest {
            return Err(Reject::new(
                "config_conflict",
                format!("valuation {} is older than {latest}", valuation.version),
            ));
        }
        let (mut applied, mut skipped) = (0u32, Vec::new());
        for (id, reserve) in &valuation.reserves {
            let Some(claim) = self.claims.get(id) else { continue };
            cx.core.work(1);
            if claim.status != Status::InPayment {
                skipped.push(id.clone());
                continue;
            }
            let mut claim = claim.clone();
            cx.core.execute(crate::Instruction::SetReserve {
                claim: claim.id.clone(),
                policy: claim.policy.clone(),
                cover: claim.cover.clone(),
                amount_minor: *reserve,
                reason: format!("valuation:{}:{}", valuation.version, valuation.basis),
            })?;
            claim.reserve_minor = *reserve;
            note(&mut claim, cx, None, format!("valuation:{}", valuation.version));
            cx.notify(
                "claim_reserve_changed",
                id,
                &[("gross_reserve_minor", reserve.to_string()), ("valuation", valuation.version.to_string())],
            );
            self.commit(claim);
            applied += 1;
        }
        self.activations.push(Activation {
            what,
            version: valuation.version,
            content_hash: hash,
            on: cx.today,
        });
        cx.notify(
            "valuation_applied",
            "",
            &[
                ("version", valuation.version.to_string()),
                ("part", valuation.part.to_string()),
                ("as_at", valuation.as_at.to_string()),
                ("applied", applied.to_string()),
                ("not_in_payment", skipped.join(",")),
            ],
        );
        Ok(())
    }

    /// Claims cannot be worked until rules and authority have been activated.
    pub(crate) fn configured(&self) -> Result<(), Reject> {
        if self.rules.version == 0 || self.authority.version == 0 {
            return Err(Reject::new(
                "not_configured",
                "claims rules and authority profiles have not been activated on this partition",
            ));
        }
        Ok(())
    }

    /// A working copy of a claim. Handlers change the copy and `commit` it.
    pub(crate) fn claim(&self, id: &str) -> Result<Claim, Reject> {
        self.claims
            .get(id)
            .cloned()
            .ok_or_else(|| Reject::new("unknown_claim", format!("claim {id} is not on this partition")))
    }

    /// Store a claim and bring every index into line with it.
    pub(crate) fn commit(&mut self, claim: Claim) {
        if let Some(old) = self.claims.remove(&claim.id) {
            self.index(&old, false);
        }
        self.index(&claim, true);
        if self.rules.partitions > 1 && !claim.overpayments.is_empty() {
            self.to_share.insert(claim.id.clone());
        }
        self.claims.insert(claim.id.clone(), claim);
    }

    fn index(&mut self, c: &Claim, add: bool) {
        for (date, _) in &c.due {
            toggle(&mut self.due_index, (*date, c.id.clone()), add);
        }
        toggle(self.by_policy.entry(c.policy.clone()).or_default(), c.id.clone(), add);
        if let Some(party) = &c.life_party {
            toggle(self.by_party.entry(party.clone()).or_default(), c.id.clone(), add);
        }
        toggle(&mut self.in_payment, c.id.clone(), add && c.status == Status::InPayment);
        let provider = c.claim_type == ClaimType::Health && c.status == Status::Admitted && c.benefit_payable_minor() > 0 && c.in_run.is_none();
        toggle(&mut self.provider_payable, c.id.clone(), add && provider);
        let offsetting = c
            .overpayments
            .iter()
            .any(|o| o.status == crate::RecoveryStatus::Offset && o.outstanding_minor() > 0);
        toggle(&mut self.offsetting, c.id.clone(), add && offsetting);
        if let Some((date, provider, code)) = crate::investigate::billed(c) {
            toggle(&mut self.billed, (date, provider, code, c.id.clone()), add);
        }
        if let (Some(owner), true) = (&c.owner, c.status.is_open()) {
            toggle(self.owned.entry(owner.clone()).or_default(), c.id.clone(), add);
        }
    }

    /// The actor is known and has said why they are acting.
    pub(crate) fn staff(&self, actor: &Actor) -> Result<&Staff, Reject> {
        if actor.purpose.trim().is_empty() {
            return Err(Reject::new("purpose_required", "say why you are acting on this claim"));
        }
        self.authority.staff(&actor.staff)
    }

    /// Someone who underwrote the life, or has declared a relationship with
    /// anyone on the claim, never works it.
    pub(crate) fn conflicted(&self, staff: &str, claim: &Claim) -> bool {
        let related = self.authority.staff.get(staff).is_some_and(|s| {
            [claim.claimant.as_ref(), Some(&claim.policyholder), claim.life_party.as_ref()]
                .into_iter()
                .flatten()
                .any(|p| s.related_parties.contains(p))
        });
        related || claim.underwriters.contains(staff)
    }

    pub(crate) fn unconflicted(&self, actor: &Actor, claim: &Claim) -> Result<&Staff, Reject> {
        let staff = self.staff(actor)?;
        if self.conflicted(&actor.staff, claim) {
            return Err(Reject::new(
                "conflict_of_interest",
                format!("{} may not act on claim {}", actor.staff, claim.id),
            ));
        }
        Ok(staff)
    }

    /// The actor owns the claim (or manages the benefit) and has no conflict.
    pub(crate) fn owner(&self, actor: &Actor, claim: &Claim) -> Result<&Staff, Reject> {
        let staff = self.unconflicted(actor, claim)?;
        let manages = claim.schedule.as_ref().is_some_and(|s| s.case_manager.as_ref() == Some(&actor.staff));
        if claim.owner.as_ref() != Some(&actor.staff) && !manages {
            return Err(Reject::new("not_owner", format!("claim {} is not assigned to {}", claim.id, actor.staff)));
        }
        Ok(staff)
    }

    /// A queue lead or claims manager.
    pub(crate) fn leads(&self, staff: &str) -> bool {
        self.authority.has_role(staff, Role::QueueLead) || self.authority.has_role(staff, Role::ClaimsManager)
    }

    /// Start a service timer from its target for the claim's line. The
    /// decision target doubles for a vulnerable claimant: the process slows,
    /// rather than fails, for someone who cannot respond quickly.
    pub(crate) fn start_timer(&self, claim: &mut Claim, kind: TimerKind, from: NaiveDate) {
        let targets = self.rules.timers(claim.line());
        let days = match kind {
            TimerKind::Acknowledgement => targets.acknowledge_days,
            TimerKind::FirstContact => targets.first_contact_days,
            TimerKind::Decision if claim.flags.vulnerable => targets.decision_days * 2,
            TimerKind::Decision => targets.decision_days,
            TimerKind::Payment => targets.payment_days,
        };
        let due = plus_days(from, days);
        claim.timers.insert(
            kind,
            Timer {
                due,
                met_on: None,
                escalated: 0,
            },
        );
        for (level, after) in self.rules.escalation_days.iter().enumerate() {
            claim.due.insert((plus_days(due, *after), Due::Timer(kind, level as u8)));
        }
    }

    pub(crate) fn meet_timer(claim: &mut Claim, kind: TimerKind, on: NaiveDate) {
        if let Some(t) = claim.timers.get_mut(&kind) {
            t.met_on.get_or_insert(on);
        }
        claim.due.retain(|(_, d)| !matches!(d, Due::Timer(k, _) if *k == kind));
    }

    /// Claims with something due on or before `today` and not yet handled.
    pub fn due_backlog(&self, today: NaiveDate) -> usize {
        self.due_index.iter().take_while(|(d, _)| *d <= today).count()
    }

    /// Work through what has fallen due: timers that were missed, reviews on
    /// the plan of a claim in payment, late evidence, expired pre-approvals.
    /// Nothing here moves money, and nothing here stops a payment.
    pub fn drain_due(&mut self, core: &mut dyn Core, index: u64) {
        let today = core.business_date();
        let cx = &mut Cx { core, index, today };
        self.to_share.clear();
        let ids: BTreeSet<ClaimId> = self.due_index.iter().take_while(|(d, _)| *d <= today).map(|(_, id)| id.clone()).collect();
        for id in ids.into_iter().take(DUE_CHUNK) {
            let Ok(mut claim) = self.claim(&id) else { continue };
            let due: Vec<(NaiveDate, Due)> = claim.due.iter().take_while(|(d, _)| *d <= today).cloned().collect();
            for item in due {
                cx.core.work(1);
                claim.due.remove(&item);
                self.fall_due(cx, &mut claim, item.0, item.1);
            }
            self.commit(claim);
        }
        self.share_offsets(cx);
        self.to_share.clear();
    }

    fn fall_due(&mut self, cx: &mut Cx<'_>, claim: &mut Claim, on: NaiveDate, due: Due) {
        match due {
            Due::Timer(kind, level) => {
                if let Some(t) = claim.timers.get_mut(&kind).filter(|t| t.met_on.is_none()) {
                    t.escalated = level + 1;
                    // 1 warns the owner, 2 the queue lead, 3 the claims manager.
                    cx.notify(
                        "timer_escalated",
                        &claim.id,
                        &[("timer", format!("{kind:?}")), ("level", (level + 1).to_string())],
                    );
                    note(claim, cx, None, format!("timer_escalated:{kind:?}:{}", level + 1));
                }
            }
            Due::Review(kind) => {
                open_task(claim, cx, format!("review/{kind:?}/{on}"), kind, Some(on));
                let (grace, months) = match kind {
                    TaskKind::ProofOfLife => (self.rules.proof_of_life_grace_days, self.rules.proof_of_life_months),
                    TaskKind::MedicalReview => (self.rules.review_grace_days, self.rules.medical_review_months),
                    _ => (self.rules.review_grace_days, self.rules.income_declaration_months),
                };
                claim.due.insert((plus_days(on, grace), Due::ReviewOverdue(kind)));
                if let Some(s) = claim.schedule.as_mut() {
                    let next = plus_months(on, months.max(1));
                    if next < s.end {
                        s.reviews.insert(kind, next);
                        claim.due.insert((next, Due::Review(kind)));
                    } else {
                        s.reviews.remove(&kind);
                    }
                }
                cx.notify("review_due", &claim.id, &[("review", format!("{kind:?}"))]);
            }
            Due::ReviewOverdue(kind) => {
                // A missed review raises a flag and a senior review. The
                // benefit keeps being paid from the last confirmed inputs.
                let open = claim.tasks.iter_mut().filter(|t| t.kind == kind && t.completed_on.is_none() && !t.overdue);
                let mut missed = 0;
                for t in open {
                    t.overdue = true;
                    missed += 1;
                }
                if missed > 0 {
                    if let Some(s) = claim.schedule.as_mut() {
                        s.reviews_overdue += missed;
                    }
                    open_task(claim, cx, format!("senior-review/overdue/{kind:?}/{on}"), TaskKind::SeniorReview, None);
                    cx.notify("review_overdue", &claim.id, &[("review", format!("{kind:?}"))]);
                }
            }
            Due::Evidence(id) => {
                if claim.evidence.iter().any(|e| e.id == id && e.received_on.is_none()) {
                    cx.notify("evidence_overdue", &claim.id, &[("evidence", id.clone())]);
                    note(claim, cx, None, format!("evidence_overdue:{id}"));
                }
            }
            Due::Waiver => self.waiver_day(cx, claim),
            Due::ExpectedEnd => {
                if claim.status == Status::InPayment {
                    open_task(claim, cx, "senior-review/duration", TaskKind::SeniorReview, None);
                    note(claim, cx, None, "expected_duration_exceeded");
                }
            }
            Due::PreApprovalExpiry => {
                if self.pre_approvals.remove(&claim.id).is_some() {
                    claim.status = Status::Withdrawn;
                    claim.closed_on = Some(cx.today);
                    cx.notify("pre_approval_released", &claim.id, &[("reason", "expired".into())]);
                    note(claim, cx, None, "pre_approval_expired");
                }
            }
        }
    }
}
