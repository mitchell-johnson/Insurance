//! Intake, triage and assessment: everything up to the decision.

use std::collections::BTreeMap;

use chrono::NaiveDate;

use crate::command::{Actor, Notification};
use crate::config::{Match, Queue, Role};
use crate::model::{Claim, ClaimType, CoverCheck, Due, Evidence, PreApproval, Reinsurance, Status, TaskKind, TimerKind};
use crate::port::{CoverFacts, Instruction, Reject};
use crate::state::{note, open_task, plus_days, ClaimsState, Cx};

/// Kinds of task a person may open. The rest are opened by the module itself.
const OPENABLE: &[TaskKind] = &[
    TaskKind::ClaimantConversation,
    TaskKind::CaseConference,
    TaskKind::MedicalOpinion,
    TaskKind::MembershipEvidence,
    TaskKind::Rehabilitation,
    TaskKind::ReturnToWork,
    TaskKind::NonDisclosureReview,
    TaskKind::RepaymentArrangement,
    TaskKind::SeniorReview,
];

/// What a health item may be paid, and the counters it is counted against.
pub(crate) struct HealthAmount {
    pub amount_minor: i64,
    pub year_key: String,
    pub lifetime_key: String,
}

impl ClaimsState {
    /// Price a health item against the limits of its benefit category:
    /// excess, then the member's share, then per event, per policy year and
    /// lifetime, less what is already used or held for a pre-approval.
    pub(crate) fn health_amount(&self, claim: &Claim, facts: &CoverFacts) -> Option<HealthAmount> {
        let item = claim.health.as_ref()?;
        let benefit = self.rules.health_benefits.get(&item.category)?;
        let life = claim.life.clone().unwrap_or_default();
        // Each cover counts on its own, unless the product's shell declares
        // a counter that covers share.
        let counter = facts.shared_counter.clone().unwrap_or_else(|| claim.cover.clone());
        let lifetime_key = format!("{}/{}/{}/{}", claim.policy, life, counter, item.category);
        let year_key = format!("{lifetime_key}/{}", facts.policy_year_start.map_or_else(|| "-".to_string(), |d| d.to_string()));
        let held: i64 = self
            .pre_approvals
            .values()
            .filter(|p| p.policy == claim.policy && p.life == life && p.category == item.category && Some(&p.claim) != item.settles_pre_approval.as_ref())
            .filter(|p| {
                let cover = self.claims.get(&p.claim).map(|c| c.cover.clone()).unwrap_or_default();
                facts.shared_counter.is_some() || cover == claim.cover
            })
            .map(|p| p.amount_minor)
            .sum();
        let used = |key: &String| self.health_used.get(key).copied().unwrap_or(0);
        // The excess the life chose replaces the schedule's, where the cover offers a choice.
        let excess = facts.chosen_excess_minor.unwrap_or(benefit.excess_minor);
        let share = (item.invoice_minor - excess).max(0) * i64::from(100 - benefit.copay_pct) / 100;
        let amount_minor = share
            .min(benefit.per_event_minor)
            .min(benefit.per_year_minor - used(&year_key) - held)
            .min(benefit.lifetime_minor - used(&lifetime_key) - held)
            .max(0);
        Some(HealthAmount {
            amount_minor,
            year_key,
            lifetime_key,
        })
    }

    /// Count an admitted health amount against the limits, and release the
    /// pre-approval hold the invoice settles.
    pub(crate) fn use_health(&mut self, cx: &mut Cx<'_>, claim: &Claim, priced: &HealthAmount, amount_minor: i64) {
        *self.health_used.entry(priced.year_key.clone()).or_default() += amount_minor;
        *self.health_used.entry(priced.lifetime_key.clone()).or_default() += amount_minor;
        let settles = claim.health.as_ref().and_then(|h| h.settles_pre_approval.clone());
        if let Some(pre) = settles.filter(|p| self.pre_approvals.remove(p).is_some()) {
            if let Ok(mut held) = self.claim(&pre) {
                held.status = Status::Withdrawn;
                held.closed_on = Some(cx.today);
                held.due.retain(|(_, d)| *d != Due::PreApprovalExpiry);
                note(&mut held, cx, None, format!("pre_approval_settled_by:{}", claim.id));
                self.commit(held);
            }
            cx.notify("pre_approval_released", &pre, &[("reason", "settled".into())]);
        }
    }

    /// The amount the rules may admit without a person, if they may.
    fn straight_through(&self, claim: &Claim, facts: &CoverFacts) -> Option<i64> {
        let max = *self.rules.straight_through_max_minor.get(&claim.claim_type)?;
        let standing_clean = facts.standing.as_deref().is_none_or(|s| s == "paid");
        let simple = !claim.flags.complex() && !claim.flags.group && !claim.claim_type.periodic() && claim.claim_type != ClaimType::Death;
        let (exclusions, _) = self.rules.exclusion_check(&facts.exclusions, &facts.cover_type, claim.cause_code.as_deref());
        if !(facts.in_force && facts.life_on_policy && standing_clean && simple && exclusions == Match::DoesNotApply) {
            return None;
        }
        let amount = match claim.claim_type {
            ClaimType::Health => self.health_amount(claim, facts)?.amount_minor,
            _ => Some(claim.claimed_minor).filter(|a| *a <= facts.sum_insured_minor)?,
        };
        (amount > 0 && amount <= max).then_some(amount)
    }

    fn triage(&self, claim: &Claim) -> Queue {
        if claim.flags.complex() || claim.claimed_minor >= self.rules.large_claim_minor {
            Queue::ComplexLarge
        } else if claim.flags.group {
            Queue::Group
        } else {
            match claim.claim_type {
                ClaimType::Health => Queue::Health,
                t if t.periodic() => Queue::IncomeProtectionNew,
                _ => Queue::LumpSum,
            }
        }
    }

    /// Higher is more urgent: amount, vulnerability, hardship and a provider
    /// waiting to treat. The time since the event is added when a queue is
    /// read, so it keeps counting.
    fn priority(&self, claim: &Claim) -> u32 {
        let flags = &claim.flags;
        let large = if claim.claimed_minor >= self.rules.large_claim_minor { 20 } else { 0 };
        large + [flags.vulnerable, flags.hardship, flags.provider_waiting].into_iter().filter(|f| *f).count() as u32 * 30
    }

    /// Register a claim. Whatever the channel, and however doubtful cover
    /// looks, the claim is opened: a decline is a decision with reasons.
    pub(crate) fn notify(&mut self, cx: &mut Cx<'_>, n: Notification) -> Result<(), Reject> {
        self.configured()?;
        if self.claims.contains_key(&n.claim) {
            return Err(Reject::new("claim_exists", format!("claim {} is already registered", n.claim)));
        }
        if n.event_date > cx.today {
            return Err(Reject::new("event_in_future", "the event date is after today"));
        }
        let facts = cx.core.cover_facts(&n.policy, &n.cover, n.life.as_deref(), n.event_date)?;
        let cause_known = n.cause_code.as_ref().is_some_and(|c| self.rules.cause_codes.contains_key(c));
        let mut claim = Claim {
            id: n.claim.clone(),
            policy: n.policy,
            cover: n.cover,
            life: n.life,
            claim_type: n.claim_type,
            event_date: n.event_date,
            notified_on: cx.today,
            channel: n.channel,
            notifier: n.notifier,
            claimant: n.claimant,
            claimed_minor: n.claimed_minor.unwrap_or(facts.sum_insured_minor).max(0),
            flags: n.flags,
            health: n.health,
            status: Status::Open,
            queue: Queue::LumpSum,
            owner: None,
            priority: 0,
            underwriters: n.underwriters.into_iter().collect(),
            touched_by: Default::default(),
            cause_code: n.cause_code.clone().filter(|_| cause_known),
            cause_history: vec![],
            cover_check: None,
            policyholder: facts.policyholder.clone(),
            life_party: facts.life_party.clone(),
            reserve_minor: 0,
            admitted_minor: 0,
            loan_repaid_minor: 0,
            advanced_minor: 0,
            interest_offer: None,
            interest_declined: None,
            interest_owed_again_minor: 0,
            insolvency_hold: false,
            directed_to: None,
            payable_minor: 0,
            cost_payable_minor: 0,
            waiver_from: None,
            interests: vec![],
            migrated: false,
            investigation: None,
            continued_from: facts.continuation_of.clone(),
            in_run: None,
            decisions: vec![],
            pending: None,
            tasks: vec![],
            evidence: vec![],
            timers: BTreeMap::new(),
            payee: None,
            payments: vec![],
            schedule: None,
            overpayments: vec![],
            reinsurance: Reinsurance::default(),
            catastrophe_event: n.catastrophe_event,
            overturns: 0,
            closed_on: None,
            due: Default::default(),
            history: vec![],
        };
        if let Some(health) = claim.health.as_ref() {
            claim.claimed_minor = n.claimed_minor.unwrap_or(health.invoice_minor).max(0);
        }
        claim.cause_history.extend(claim.cause_code.clone().map(|c| (cx.today, c)));
        claim.queue = self.triage(&claim);
        claim.priority = self.priority(&claim);
        self.start_timer(&mut claim, TimerKind::Acknowledgement, cx.today);
        note(&mut claim, cx, None, "notified");
        if n.cause_code.is_some() && !cause_known {
            // A cause the rules cannot read never clears an exclusion, so the
            // claim goes to a person with the cause still to record.
            note(&mut claim, cx, None, "cause_not_recognised");
        }
        if claim.queue == Queue::ComplexLarge {
            open_task(&mut claim, cx, "case-conference/1", TaskKind::CaseConference, None);
        }
        let late_death = claim.claim_type == ClaimType::Death && (cx.today - claim.event_date).num_days() > i64::from(self.rules.late_death_days);
        if late_death {
            // An earlier match on the death register would have shortened the window.
            open_task(&mut claim, cx, "death-register-cadence", TaskKind::DeathRegisterCadence, None);
        }

        let periodic = claim.claim_type.periodic();
        // A case opened by a rule takes the claim out of the rules' hands
        // without saying so: it waits for a person like any other.
        let held = self.screen(cx, &mut claim, facts.cover_start);
        let straight = self.straight_through(&claim, &facts).filter(|_| !held);
        let pre_approval = claim.health.as_ref().is_some_and(|h| h.pre_approval_request);
        let reserved = |claim: &Claim, gross: i64| {
            vec![
                ("claim", claim.id.clone()),
                ("policy", claim.policy.clone()),
                ("cover", claim.cover.clone()),
                ("life", claim.life.clone().unwrap_or_default()),
                ("event_date", claim.event_date.to_string()),
                ("gross_reserve_minor", gross.to_string()),
                ("periodic", periodic.to_string()),
                ("catastrophe_event", claim.catastrophe_event.clone().unwrap_or_default()),
                // A continuation policy's claim stays with the scheme's treaty history.
                (
                    "continued_from_policy",
                    claim.continued_from.as_ref().map(|c| c.policy.clone()).unwrap_or_default(),
                ),
                ("continued_from_life", claim.continued_from.as_ref().map(|c| c.life.clone()).unwrap_or_default()),
            ]
        };
        match (straight, pre_approval) {
            (Some(amount), true) => {
                // A hold against the limits, like a card authorisation. No posting.
                let item = claim.health.as_ref().map(|h| h.category.clone()).unwrap_or_default();
                let expires = plus_days(cx.today, self.rules.pre_approval_days);
                self.pre_approvals.insert(
                    claim.id.clone(),
                    PreApproval {
                        claim: claim.id.clone(),
                        policy: claim.policy.clone(),
                        life: claim.life.clone().unwrap_or_default(),
                        category: item,
                        amount_minor: amount,
                        expires,
                    },
                );
                claim.due.insert((expires, Due::PreApprovalExpiry));
                note(&mut claim, cx, None, "pre_approved");
                cx.notify(
                    "pre_approval_given",
                    &claim.id,
                    &[("amount_minor", amount.to_string()), ("expires", expires.to_string())],
                );
            }
            (Some(amount), false) => {
                cx.core.execute(Instruction::Admit {
                    // A lump sum comes off the life's cover; a health invoice does not.
                    life: claim.life.clone().filter(|_| claim.claim_type != ClaimType::Health),
                    claim: claim.id.clone(),
                    policy: claim.policy.clone(),
                    cover: claim.cover.clone(),
                    amount_minor: amount,
                    release_reserve: true,
                    deduct_premium_minor: 0,
                    repay_loan_minor: 0,
                    terminate: None,
                })?;
                claim.status = Status::Admitted;
                claim.admitted_minor = amount;
                claim.payable_minor = amount;
                let decision = self.decision(
                    &claim,
                    cx,
                    crate::DecisionKind::Admit,
                    amount,
                    vec!["straight_through".into()],
                    vec![],
                    Some(&facts),
                );
                claim.decisions.push(decision);
                if let Some(priced) = self.health_amount(&claim, &facts) {
                    self.use_health(cx, &claim, &priced, amount);
                }
                self.start_timer(&mut claim, TimerKind::Payment, cx.today);
                note(&mut claim, cx, None, "admitted_straight_through");
                cx.lifecycle(&self.rules, "claim_reserved", &claim, &reserved(&claim, amount));
                cx.lifecycle(&self.rules, "claim_admitted", &claim, &[("admitted_minor", amount.to_string())]);
            }
            (None, _) => {
                let pct = self.rules.initial_reserve_pct.get(&claim.claim_type).copied().unwrap_or(100);
                let reserve = claim.claimed_minor.saturating_mul(i64::from(pct)) / 100;
                cx.core.execute(Instruction::SetReserve {
                    claim: claim.id.clone(),
                    policy: claim.policy.clone(),
                    cover: claim.cover.clone(),
                    amount_minor: reserve,
                    reason: "notification".into(),
                })?;
                claim.reserve_minor = reserve;
                cx.lifecycle(&self.rules, "claim_reserved", &claim, &reserved(&claim, reserve));
            }
        }
        if claim.claim_type == ClaimType::Death {
            // The customer master marks the life deceased and starts the estate process.
            cx.notify(
                "death_notified",
                &claim.id,
                &[
                    ("policy", claim.policy.clone()),
                    ("life", claim.life.clone().unwrap_or_default()),
                    ("date_of_death", claim.event_date.to_string()),
                ],
            );
        }
        if late_death {
            cx.notify("death_register_cadence_check", &claim.id, &[]);
        }
        self.commit(claim);
        Ok(())
    }

    /// The acknowledgement was delivered. The service timers start here.
    pub(crate) fn acknowledge(&mut self, cx: &mut Cx<'_>, id: &str, delivery_ref: &str) -> Result<(), Reject> {
        let mut claim = self.claim(id)?;
        let acknowledged = claim.timers.get(&TimerKind::Acknowledgement).is_some_and(|t| t.met_on.is_some());
        if acknowledged || delivery_ref.is_empty() {
            return Err(Reject::new(
                "acknowledgement",
                "the claim is already acknowledged, or there is no proof of delivery",
            ));
        }
        Self::meet_timer(&mut claim, TimerKind::Acknowledgement, cx.today);
        if claim.status == Status::Open {
            self.start_timer(&mut claim, TimerKind::FirstContact, cx.today);
            self.start_timer(&mut claim, TimerKind::Decision, cx.today);
        }
        note(&mut claim, cx, None, format!("acknowledged:{delivery_ref}"));
        self.commit(claim);
        Ok(())
    }

    /// Whether a person may be given a claim: skill, authority, capacity and
    /// no conflict.
    fn eligible(&self, to: &str, claim: &Claim) -> Result<(), Reject> {
        let staff = self.authority.staff(to)?;
        let profile = self.authority.profile(to)?;
        let refuse = |code: &str, why: &str| Err(Reject::new(code, format!("{to} {why}")));
        if !claim.status.is_open() {
            return refuse("claim_closed", "cannot be given a closed claim");
        }
        if !profile.lines.contains(&claim.line()) || !staff.queues.contains(&claim.queue) {
            return refuse("not_accredited", "is not accredited for this line and queue");
        }
        if claim.queue == Queue::ComplexLarge && !profile.senior {
            return refuse("not_senior", "does not work the senior queue");
        }
        if profile.max_admission_minor.get(&claim.claim_type).copied().unwrap_or(0) < claim.claimed_minor {
            return refuse("authority", "has no authority for a claim of this type and amount");
        }
        if self.conflicted(to, claim) {
            return refuse("conflict_of_interest", "underwrote the life or is related to someone on the claim");
        }
        if claim.flags.vulnerable && !staff.vulnerable_trained {
            return refuse("not_trained", "is not trained to work with vulnerable claimants");
        }
        let load = self.owned.get(to).map_or(0, |s| s.iter().filter(|c| *c != &claim.id).count());
        if staff.wip_limit > 0 && load >= staff.wip_limit as usize {
            return refuse("wip_limit", "is at their work in progress limit");
        }
        Ok(())
    }

    /// One owner at a time. A person takes a claim from their queue, or a
    /// lead hands it over; either way the handover is on the record.
    pub(crate) fn assign(&mut self, cx: &mut Cx<'_>, id: &str, to: &str, actor: &Actor) -> Result<(), Reject> {
        let mut claim = self.claim(id)?;
        self.staff(actor)?;
        if actor.staff != to && !self.leads(&actor.staff) {
            return Err(Reject::new(
                "not_authorised",
                "only a queue lead or claims manager assigns a claim to someone else",
            ));
        }
        self.eligible(to, &claim)?;
        let previous = claim.owner.replace(to.to_string());
        if let Some(s) = claim.schedule.as_mut() {
            s.case_manager = Some(to.to_string());
        }
        note(&mut claim, cx, Some(actor), format!("assigned:{to}"));
        let handover = previous.as_deref().is_some_and(|p| p != to);
        if handover && claim.flags.communication_support {
            // The new owner tells the claimant themselves, and says what was handed over.
            note(&mut claim, cx, Some(actor), format!("handover:{}:{to}", previous.clone().unwrap_or_default()));
            open_task(
                &mut claim,
                cx,
                format!("handover/{to}/{}", cx.today),
                TaskKind::ClaimantConversation,
                Some(plus_days(cx.today, 2)),
            );
        }
        // The claimant is told who their owner is, and when it changes.
        cx.notify(
            "claim_assigned",
            id,
            &[
                ("handover", previous.is_some().to_string()),
                ("owner", to.to_string()),
                ("communication_support", claim.flags.communication_support.to_string()),
            ],
        );
        self.commit(claim);
        Ok(())
    }

    /// Record a communication marker, or its end. Turned on, every deadline
    /// still open for the claimant moves out by the rules' extension; turned
    /// off, nothing already given is shortened.
    pub(crate) fn record_communication_need(&mut self, cx: &mut Cx<'_>, id: &str, needed: bool, actor: &Actor) -> Result<(), Reject> {
        let mut claim = self.claim(id)?;
        if !self.leads(&actor.staff) {
            self.owner(actor, &claim)?;
        }
        if claim.flags.communication_support == needed {
            return Ok(());
        }
        claim.flags.communication_support = needed;
        let extra = self.rules.communication_extra_days;
        if needed && extra > 0 {
            let claimant = claim.claimant.clone();
            let theirs: Vec<(NaiveDate, Due)> = claim
                .due
                .iter()
                .filter(|(_, d)| match d {
                    Due::Evidence(e) => claim
                        .evidence
                        .iter()
                        .any(|x| &x.id == e && x.received_on.is_none() && customer_side(&x.from, claimant.as_deref())),
                    Due::ReviewOverdue(_) => true,
                    _ => false,
                })
                .cloned()
                .collect();
            for (on, due) in theirs {
                claim.due.remove(&(on, due.clone()));
                let later = plus_days(on, extra);
                if let Due::Evidence(e) = &due {
                    if let Some(x) = claim.evidence.iter_mut().find(|x| &x.id == e) {
                        x.due = later;
                    }
                }
                claim.due.insert((later, due));
            }
        }
        note(&mut claim, cx, Some(actor), format!("communication_need:{needed}"));
        cx.notify("communication_need_recorded", id, &[("needed", needed.to_string())]);
        self.commit(claim);
        Ok(())
    }

    pub(crate) fn record_contact(&mut self, cx: &mut Cx<'_>, id: &str, actor: &Actor) -> Result<(), Reject> {
        let mut claim = self.claim(id)?;
        self.owner(actor, &claim)?;
        Self::meet_timer(&mut claim, TimerKind::FirstContact, cx.today);
        note(&mut claim, cx, Some(actor), "contact");
        self.commit(claim);
        Ok(())
    }

    /// Move a claim to the senior queue, with a case conference to hold.
    pub(crate) fn escalate(&mut self, cx: &mut Cx<'_>, id: &str, actor: &Actor) -> Result<(), Reject> {
        let mut claim = self.claim(id)?;
        if !self.leads(&actor.staff) {
            self.owner(actor, &claim)?;
        }
        self.unconflicted(actor, &claim)?;
        claim.queue = Queue::ComplexLarge;
        let owner_senior = claim.owner.as_ref().is_some_and(|o| self.authority.profile(o).is_ok_and(|p| p.senior));
        if !owner_senior {
            claim.owner = None;
        }
        let n = claim.task(TaskKind::CaseConference).count() + 1;
        open_task(&mut claim, cx, format!("case-conference/{n}"), TaskKind::CaseConference, None);
        note(&mut claim, cx, Some(actor), "escalated");
        self.commit(claim);
        Ok(())
    }

    /// Identification at claim: an unnamed scheme member becomes a person.
    pub(crate) fn identify_life(&mut self, cx: &mut Cx<'_>, id: &str, party: &str, actor: &Actor) -> Result<(), Reject> {
        let mut claim = self.claim(id)?;
        self.owner(actor, &claim)?;
        let life = claim
            .life
            .clone()
            .filter(|_| !party.is_empty())
            .ok_or_else(|| Reject::new("no_life", "the claim names no life record, or no party was given"))?;
        cx.core.execute(Instruction::IdentifyMember {
            policy: claim.policy.clone(),
            life,
            party: party.to_string(),
        })?;
        claim.life_party = Some(party.to_string());
        note(&mut claim, cx, Some(actor), "life_identified");
        self.commit(claim);
        Ok(())
    }

    /// Record the cause of claim, from the controlled list. The code stays
    /// inside the module.
    pub(crate) fn record_cause(&mut self, cx: &mut Cx<'_>, id: &str, cause_code: &str, actor: &Actor) -> Result<(), Reject> {
        let mut claim = self.claim(id)?;
        self.owner(actor, &claim)?;
        if !self.rules.cause_codes.contains_key(cause_code) {
            return Err(Reject::new("unknown_cause", "that cause code is not on the controlled list"));
        }
        claim.cause_code = Some(cause_code.to_string());
        claim.cause_history.push((cx.today, cause_code.to_string()));
        note(&mut claim, cx, Some(actor), "cause_recorded");
        self.commit(claim);
        Ok(())
    }

    /// Read the facts about the cover on the event date and record which of
    /// them the assessor relies on.
    pub(crate) fn cover_check(&mut self, cx: &mut Cx<'_>, id: &str, relied_on: Vec<String>, actor: &Actor) -> Result<(), Reject> {
        let mut claim = self.claim(id)?;
        self.owner(actor, &claim)?;
        let facts = cx.core.cover_facts(&claim.policy, &claim.cover, claim.life.as_deref(), claim.event_date)?;
        claim.policyholder = facts.policyholder.clone();
        claim.life_party = facts.life_party.clone().or(claim.life_party);
        claim.cover_check = Some(CoverCheck {
            checked_on: cx.today,
            facts,
            relied_on,
            by: actor.staff.clone(),
        });
        note(&mut claim, cx, Some(actor), "cover_checked");
        self.commit(claim);
        Ok(())
    }

    pub(crate) fn request_evidence(&mut self, cx: &mut Cx<'_>, id: &str, evidence: String, kind: String, from: String, actor: &Actor) -> Result<(), Reject> {
        let mut claim = self.claim(id)?;
        self.owner(actor, &claim)?;
        if claim.evidence.iter().any(|e| e.id == evidence) {
            return Err(Reject::new("evidence_exists", format!("evidence request {evidence} already exists")));
        }
        let mut days = self.rules.timers(claim.line()).evidence_days;
        if claim.flags.communication_support && customer_side(&from, claim.claimant.as_deref()) {
            days += self.rules.communication_extra_days;
        }
        let due = plus_days(cx.today, days);
        claim.due.insert((due, Due::Evidence(evidence.clone())));
        claim.evidence.push(Evidence {
            id: evidence.clone(),
            kind,
            from,
            requested_on: cx.today,
            due,
            received_on: None,
            vault_ref: None,
            cost_minor: 0,
        });
        note(&mut claim, cx, Some(actor), format!("evidence_requested:{evidence}"));
        cx.notify("evidence_requested", id, &[("evidence", evidence), ("due", due.to_string())]);
        self.commit(claim);
        Ok(())
    }

    pub(crate) fn receive_evidence(&mut self, cx: &mut Cx<'_>, id: &str, evidence: &str, vault_ref: String, cost_minor: i64) -> Result<(), Reject> {
        let mut claim = self.claim(id)?;
        let item = claim
            .evidence
            .iter_mut()
            .find(|e| e.id == evidence && e.received_on.is_none())
            .filter(|_| !vault_ref.is_empty() && cost_minor >= 0)
            .ok_or_else(|| Reject::new("evidence", "no such evidence is outstanding, or it has no place in the vault"))?;
        item.received_on = Some(cx.today);
        item.vault_ref = Some(vault_ref);
        item.cost_minor = cost_minor;
        claim.due.retain(|(_, d)| !matches!(d, Due::Evidence(e) if e == evidence));
        note(&mut claim, cx, None, format!("evidence_received:{evidence}"));
        cx.notify("evidence_received", id, &[("evidence", evidence.to_string())]);
        self.commit(claim);
        Ok(())
    }

    pub(crate) fn open_task_command(
        &mut self,
        cx: &mut Cx<'_>,
        id: &str,
        task: String,
        kind: TaskKind,
        due: Option<NaiveDate>,
        actor: &Actor,
    ) -> Result<(), Reject> {
        let mut claim = self.claim(id)?;
        if !self.leads(&actor.staff) {
            self.owner(actor, &claim)?;
        }
        self.unconflicted(actor, &claim)?;
        if !OPENABLE.contains(&kind) {
            return Err(Reject::new("system_task", "tasks of this kind are opened by the claims module itself"));
        }
        if !open_task(&mut claim, cx, task.clone(), kind, due) {
            return Err(Reject::new("task_exists", format!("task {task} already exists")));
        }
        if kind == TaskKind::NonDisclosureReview {
            // Worked by underwriting, by someone else, with its own approvals.
            cx.notify(
                "non_disclosure_review_requested",
                id,
                &[("review_ref", task.clone()), ("policy", claim.policy.clone())],
            );
        }
        note(&mut claim, cx, Some(actor), format!("task_opened:{task}"));
        self.commit(claim);
        Ok(())
    }

    /// Who may complete a task depends on what it is.
    fn may_complete(&self, claim: &Claim, kind: TaskKind, attendees: usize, vault_ref: bool, spend_minor: i64, actor: &Actor) -> Result<(), Reject> {
        self.unconflicted(actor, claim)?;
        let profile = self.authority.profile(&actor.staff)?;
        let who = &actor.staff;
        let owns = self.owner(actor, claim).is_ok();
        let ok = match kind {
            TaskKind::NonDisclosureReview | TaskKind::ReinsurerConsent => {
                return Err(Reject::new("system_task", "this task is completed by the module that owns the answer"))
            }
            // By a reviewer who did not touch the case.
            TaskKind::QualityReview => self.authority.has_role(who, Role::QualityReviewer) && !claim.touched_by.contains(who),
            // The question and the opinion are both held in the clinical store.
            TaskKind::MedicalOpinion => self.authority.has_role(who, Role::MedicalOfficer) && vault_ref,
            // The certificate, declaration or register match is on file.
            TaskKind::ProofOfLife => owns && vault_ref,
            TaskKind::CaseConference => (owns || profile.senior) && attendees >= 2,
            TaskKind::SeniorReview | TaskKind::ExclusionMatch => profile.senior,
            TaskKind::DeathRegisterCadence => self.leads(who),
            TaskKind::RepaymentArrangement => owns || self.authority.staff(who)?.queues.contains(&Queue::Recoveries),
            TaskKind::Rehabilitation => owns && spend_minor <= profile.max_payment_minor,
            _ => owns,
        };
        if ok && spend_minor >= 0 {
            Ok(())
        } else {
            Err(Reject::new("not_authorised", format!("{who} may not complete this task as it stands")))
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn complete_task(
        &mut self,
        cx: &mut Cx<'_>,
        id: &str,
        task: &str,
        outcome: String,
        attendees: Vec<String>,
        vault_ref: Option<String>,
        spend_minor: i64,
        actor: &Actor,
    ) -> Result<(), Reject> {
        let mut claim = self.claim(id)?;
        let kind = claim
            .tasks
            .iter()
            .find(|t| t.id == task && t.completed_on.is_none())
            .map(|t| t.kind)
            .ok_or_else(|| Reject::new("unknown_task", format!("claim {id} has no open task {task}")))?;
        self.may_complete(&claim, kind, attendees.len(), vault_ref.is_some(), spend_minor, actor)?;
        let was_overdue = finish_task(
            &mut claim,
            task,
            cx.today,
            Some(actor.staff.clone()),
            outcome,
            attendees,
            vault_ref,
            spend_minor,
        );
        if let (true, Some(s)) = (was_overdue, claim.schedule.as_mut()) {
            s.reviews_overdue = s.reviews_overdue.saturating_sub(1);
        }
        if kind == TaskKind::QualityReview {
            // The reviewer stays someone who did not touch the case.
            claim.history.push(crate::HistoryItem {
                on: cx.today,
                log_index: cx.index,
                by: Some(actor.staff.clone()),
                purpose: Some(actor.purpose.clone()),
                action: format!("task_completed:{task}"),
            });
        } else {
            note(&mut claim, cx, Some(actor), format!("task_completed:{task}"));
        }
        self.commit(claim);
        Ok(())
    }

    /// Whether someone other than the owner may open the clinical file:
    /// only through a review they are the right person to do.
    fn review_access(&self, staff: &str, claim: &Claim) -> bool {
        let senior = self.authority.profile(staff).is_ok_and(|p| p.senior) || self.authority.has_role(staff, Role::ClaimsManager);
        let open = |kind| claim.has_open_task(kind);
        claim.pending.as_ref().is_some_and(|p| p.proposer != staff)
            || (self.authority.has_role(staff, Role::MedicalOfficer) && open(TaskKind::MedicalOpinion))
            || (self.authority.has_role(staff, Role::QualityReviewer) && open(TaskKind::QualityReview))
            || (senior && (open(TaskKind::SeniorReview) || open(TaskKind::CaseConference) || open(TaskKind::ExclusionMatch)))
    }

    /// Open the clinical file. Every view is logged with its purpose, and a
    /// view is refused unless the person holds the claim or is reviewing it.
    pub(crate) fn open_file(&mut self, cx: &mut Cx<'_>, id: &str, actor: &Actor) -> Result<(), Reject> {
        let mut claim = self.claim(id)?;
        self.unconflicted(actor, &claim)?;
        if self.owner(actor, &claim).is_err() && !self.review_access(&actor.staff, &claim) {
            return Err(Reject::new("no_access", format!("{} neither holds nor reviews claim {id}", actor.staff)));
        }
        claim.history.push(crate::HistoryItem {
            on: cx.today,
            log_index: cx.index,
            by: Some(actor.staff.clone()),
            purpose: Some(actor.purpose.clone()),
            action: "file_opened".into(),
        });
        self.commit(claim);
        Ok(())
    }
}

/// Mark a task done. Returns whether it had been flagged overdue.
#[allow(clippy::too_many_arguments)]
pub(crate) fn finish_task(
    claim: &mut Claim,
    task: &str,
    on: NaiveDate,
    by: Option<String>,
    outcome: String,
    attendees: Vec<String>,
    vault_ref: Option<String>,
    spend_minor: i64,
) -> bool {
    let Some(t) = claim.tasks.iter_mut().find(|t| t.id == task && t.completed_on.is_none()) else {
        return false;
    };
    t.completed_on = Some(on);
    t.completed_by = by;
    t.outcome = Some(outcome);
    t.attendees = attendees;
    t.vault_ref = vault_ref;
    t.spend_minor = spend_minor;
    std::mem::take(&mut t.overdue)
}

impl ClaimsState {
    /// Load one claim from the old system. Its money is opened in the core
    /// against the migration control account in the same step, so the
    /// record and the ledger arrive together or not at all. What cannot be
    /// carried consistently is refused, never repaired by guessing.
    pub(crate) fn load_migrated(&mut self, cx: &mut Cx<'_>, mut claim: Claim, health_used: BTreeMap<String, i64>) -> Result<(), Reject> {
        self.configured()?;
        let bad = |why: &str| Err(Reject::new("invalid_migration", why));
        if self.claims.contains_key(&claim.id) || self.in_transfer.values().any(|m| m.claims.iter().any(|c| c.id == claim.id)) {
            return Err(Reject::new("claim_exists", format!("claim {} is already here", claim.id)));
        }
        // The policy is on this partition and the cover is one it has or had.
        cx.core.cover_facts(&claim.policy, &claim.cover, claim.life.as_deref(), claim.event_date)?;
        let receivable: i64 = claim.overpayments.iter().map(crate::Overpayment::outstanding_minor).sum();
        let money = [claim.reserve_minor, claim.admitted_minor, claim.payable_minor, receivable];
        if money.iter().any(|m| *m < 0) || claim.cost_payable_minor != 0 || claim.payable_minor > claim.admitted_minor {
            return bad("reserve, admitted, payable and overpayments are not negative, and no more is payable than was admitted");
        }
        if claim.in_run.is_some() || claim.pending.is_some() {
            return bad("a claim is loaded outside any payment run and with no decision waiting for a signature");
        }
        let open = claim.status.is_open();
        let fits = match claim.status {
            Status::InPayment => claim.schedule.as_ref().is_some_and(|s| s.ended.is_none() && s.next_period < s.end),
            _ if open => claim.schedule.is_none() || claim.status == Status::Admitted,
            _ => claim.reserve_minor == 0 && claim.payable_minor == 0 && claim.closed_on.is_some(),
        };
        if !fits {
            return bad("the status does not fit the schedule, the money or the closing date");
        }
        if health_used.keys().any(|k| !k.starts_with(&format!("{}/", claim.policy))) {
            return bad("health limits used are keyed by the claim's own policy");
        }
        cx.core.execute(Instruction::OpenMigrated {
            claim: claim.id.clone(),
            policy: claim.policy.clone(),
            cover: claim.cover.clone(),
            reserve_minor: claim.reserve_minor,
            admitted_minor: claim.admitted_minor,
            payable_minor: claim.payable_minor,
            receivable_minor: receivable,
            open,
        })?;
        claim.migrated = true;
        // What the old system would have remembered is put on the calendar here.
        if let Some(s) = claim.schedule.as_ref().filter(|_| claim.status == Status::InPayment) {
            let reviews: Vec<_> = s
                .reviews
                .iter()
                .map(|(kind, on)| ((*on).max(cx.today), crate::model::Due::Review(*kind)))
                .collect();
            claim.due.extend(reviews);
            claim.due.insert((s.expected_end.max(cx.today), crate::model::Due::ExpectedEnd));
            if claim.waiver_from.is_some() {
                claim.due.insert((cx.today, crate::model::Due::Waiver));
            }
        }
        for (key, used) in health_used {
            *self.health_used.entry(key).or_default() += used;
        }
        note(&mut claim, cx, None, "migrated");
        cx.notify(
            "claim_migrated",
            &claim.id,
            &[
                ("policy", claim.policy.clone()),
                ("status", claim.status.code().to_string()),
                ("reserve_minor", claim.reserve_minor.to_string()),
                ("payable_minor", claim.payable_minor.to_string()),
                ("receivable_minor", receivable.to_string()),
            ],
        );
        self.commit(claim);
        Ok(())
    }
}

/// Whether evidence asked `from` someone is the claimant's to give: asked of
/// "claimant" or of the claimant's own party.
fn customer_side(from: &str, claimant: Option<&str>) -> bool {
    from == "claimant" || Some(from) == claimant
}
