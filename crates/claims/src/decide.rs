//! Decisions: who may make them, who must sign with them, and what each does.
//!
//! A decision is proposed by one person. Where the rules ask for more
//! signatures it waits, and takes effect only when the last one is given.
//! The checks are run again at that moment, because the claim may have moved.

use crate::command::{Actor, NonDisclosureOutcome, ReopenBasis, Request};
use crate::config::{Match, Profile, Queue, Role};
use crate::intake::{finish_task, HealthAmount};
use crate::model::{
    Claim, ClaimType, Decision, DecisionKind, Due, Finding, Need, Overpayment, OverpaymentCause, Payment, Pending, PreApproval, RecoveryStatus, Status,
    TaskKind, TimerKind,
};
use crate::port::{CoverFacts, Instruction, Reject, Termination};
use crate::state::{note, open_task, plus_days, ClaimsState, Cx};
use crate::StaffId;

/// What a request comes to once it has been checked against the claim.
#[derive(Default)]
pub(crate) struct Plan {
    /// The amount the decision is for, which authority is measured against.
    pub amount_minor: i64,
    pub deduct_minor: i64,
    pub terminate: Option<Termination>,
    /// Money already paid that the decision makes an overpayment.
    pub overpaid_minor: i64,
    pub health: Option<HealthAmount>,
}

fn refuse<T>(code: &str, message: &str) -> Result<T, Reject> {
    Err(Reject::new(code, message))
}

impl ClaimsState {
    /// Build the record of a decision: everything needed to reproduce it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn decision(
        &self,
        claim: &Claim,
        cx: &Cx<'_>,
        kind: DecisionKind,
        amount_minor: i64,
        reasons: Vec<String>,
        signers: Vec<StaffId>,
        facts: Option<&CoverFacts>,
    ) -> Decision {
        let exclusion_outcomes = facts
            .map(|f| self.rules.exclusion_check(&f.exclusions, &f.cover_type, claim.cause_code.as_deref()).1)
            .unwrap_or_default();
        Decision {
            no: claim.decisions.len() as u32 + 1,
            kind,
            amount_minor,
            reasons,
            decided_on: cx.today,
            straight_through: signers.is_empty(),
            rules_version: self.rules.version,
            authority_version: self.authority.version,
            profile: signers
                .first()
                .and_then(|s| self.authority.staff.get(s))
                .map(|s| s.profile.clone())
                .unwrap_or_default(),
            signers,
            cause_code: claim.cause_code.clone(),
            exclusion_outcomes,
            exclusion_override: None,
            finding: None,
            definitions: facts.map(|f| f.definitions.clone()).unwrap_or_default(),
            relied_on: claim.cover_check.as_ref().map(|c| c.relied_on.clone()).unwrap_or_default(),
        }
    }

    /// Lump sums already admitted on the same cover for the same life.
    fn earlier_admitted(&self, claim: &Claim) -> i64 {
        let others = self.by_policy.get(&claim.policy).into_iter().flatten().filter_map(|id| self.claims.get(id));
        others
            .filter(|c| c.cover == claim.cover && c.life == claim.life && !c.claim_type.periodic() && c.status != Status::Reversed)
            .map(|c| c.admitted_minor)
            .sum()
    }

    /// Check a request against the claim as it stands. Changes nothing.
    pub(crate) fn plan(&self, cx: &Cx<'_>, claim: &Claim, request: &Request) -> Result<Plan, Reject> {
        let mut plan = Plan::default();
        let closing = matches!(
            request,
            Request::Decline | Request::Withdraw | Request::ReverseAdmission { .. } | Request::EndBenefit { .. }
        );
        if closing && claim.in_run.is_some() {
            return refuse("in_run", "the claim is in a payment run that has not been approved or cancelled");
        }
        match request {
            Request::Reserve { amount_minor } => {
                if !claim.status.is_open() || *amount_minor < 0 {
                    return refuse("invalid_reserve", "a reserve is set on an open claim and is never negative");
                }
                plan.amount_minor = *amount_minor;
            }
            Request::Admit {
                amount_minor,
                periodic,
                exclusion_override,
                exclusion_reviewed,
                payer_deceased,
            } => {
                if claim.status != Status::Open {
                    return refuse("not_open", "only an open claim can be admitted");
                }
                if claim.has_open_task(TaskKind::NonDisclosureReview) {
                    return refuse("awaiting_non_disclosure_review", "a non-disclosure review is open on this claim");
                }
                let Some(check) = claim.cover_check.as_ref() else {
                    return refuse("cover_not_checked", "record the cover check before deciding");
                };
                let facts = &check.facts;
                let member = facts.life_on_policy || claim.task(TaskKind::MembershipEvidence).any(|t| t.outcome.as_deref() == Some("confirmed"));
                if !facts.in_force || !member {
                    return refuse("cover_not_confirmed", "cover was not in force for this life on the event date");
                }
                if claim.cause_code.is_none() {
                    return refuse("cause_required", "record the cause of claim before admitting");
                }
                let (exclusions, _) = self.rules.exclusion_check(&facts.exclusions, &facts.cover_type, claim.cause_code.as_deref());
                match (exclusions, exclusion_override) {
                    (Match::Applies, None) => return refuse("exclusion_applies", "an exclusion on the cover matches the cause of claim"),
                    (Match::NeedsAssessor, None) if !exclusion_reviewed => {
                        return refuse(
                            "exclusion_needs_assessor",
                            "an exclusion on the cover could not be decided by the rules: read it and say so",
                        );
                    }
                    (_, Some(code)) => self.rules.check_reasons(DecisionKind::Admit, std::slice::from_ref(code))?,
                    _ => {}
                }
                let awaiting: Vec<&String> = claim.reinsurance.consent_required.iter().collect();
                if !awaiting.is_empty() {
                    return refuse("awaiting_reinsurer_consent", "a reinsurer must consent before this claim is admitted");
                }
                if claim.queue == Queue::ComplexLarge && !claim.task(TaskKind::CaseConference).any(|t| t.completed_on.is_some()) {
                    return refuse("case_conference_required", "a claim on the senior queue needs its case conference first");
                }
                match (claim.claim_type.periodic(), periodic) {
                    (true, Some(p)) => {
                        let sound = p.monthly_minor > 0
                            && p.monthly_minor <= facts.sum_insured_minor
                            && p.first_period >= claim.event_date
                            && p.end > p.first_period
                            && p.expected_end > p.first_period;
                        if !sound {
                            return refuse("invalid_schedule", "the benefit terms do not fit the cover");
                        }
                        plan.amount_minor = claim.reserve_minor.max(p.monthly_minor);
                    }
                    (false, None) => {
                        plan.health = self.health_amount(claim, facts);
                        let cap = match &plan.health {
                            Some(h) => h.amount_minor,
                            None => facts.sum_insured_minor - self.earlier_admitted(claim),
                        };
                        let amount = amount_minor.unwrap_or(cap);
                        if amount <= 0 || amount > cap {
                            return refuse("amount_exceeds_cover", "the amount is more than the cover pays for this event");
                        }
                        plan.amount_minor = amount;
                        if facts.standing.as_deref() == Some("in_grace") {
                            // A claim in grace is payable less the premium owing.
                            plan.deduct_minor = facts.outstanding_premium_minor.clamp(0, amount);
                        }
                        if claim.claim_type == ClaimType::Death && facts.ends_on_death && facts.policy_status == "in_force" {
                            plan.terminate = Some(Termination {
                                event_date: claim.event_date,
                                payer_deceased: *payer_deceased,
                            });
                        }
                    }
                    _ => return refuse("invalid_schedule", "a benefit in payment needs its terms, and a lump sum has none"),
                }
            }
            Request::Decline | Request::Withdraw => {
                if claim.status != Status::Open {
                    return refuse("not_open", "only an open claim can be declined or withdrawn");
                }
                let spoken = claim.task(TaskKind::ClaimantConversation).any(|t| t.completed_on.is_some());
                if matches!(request, Request::Decline) && claim.claim_type != ClaimType::Health && !spoken {
                    return refuse("conversation_required", "talk to the claimant before declining, and record it as a task");
                }
            }
            Request::ReverseAdmission { .. } => {
                let admitted = matches!(claim.status, Status::Admitted | Status::InPayment | Status::Paid | Status::Ended);
                let Some(admission) = claim.admission().filter(|_| admitted) else {
                    return refuse("not_admitted", "there is no admission in force to reverse");
                };
                if claim.queue != Queue::ComplexLarge {
                    return refuse(
                        "senior_queue_required",
                        "a reversal of admission is worked on the senior queue: escalate the claim first",
                    );
                }
                if !claim.task_done_since(TaskKind::CaseConference, admission.decided_on) {
                    return refuse(
                        "case_conference_required",
                        "a reversal of admission needs a case conference held since the admission",
                    );
                }
                if !claim.task_done_since(TaskKind::ClaimantConversation, admission.decided_on) {
                    return refuse(
                        "conversation_required",
                        "talk to the claimant before reversing an admission, and record it as a task",
                    );
                }
                let already: i64 = claim.overpayments.iter().map(|o| o.amount_minor).sum();
                plan.overpaid_minor = (claim.paid_minor(false) - already).max(0);
                plan.amount_minor = claim.admitted_minor.max(plan.overpaid_minor);
            }
            Request::ExGratia { amount_minor, payment_ref } => {
                if *amount_minor <= 0 || claim.payee.is_none() {
                    return refuse("payee_not_verified", "an ex gratia payment needs an amount and a verified payee");
                }
                if claim.payments.iter().any(|p| &p.payment_ref == payment_ref) {
                    return refuse("payment_exists", "that payment reference has been used on this claim");
                }
                plan.amount_minor = *amount_minor;
            }
            Request::EndBenefit { end, .. } => {
                let Some(schedule) = claim.schedule.as_ref().filter(|s| claim.status == Status::InPayment && *end <= s.end) else {
                    return refuse("invalid_end", "the claim is not in payment, or the benefit already ends sooner");
                };
                plan.overpaid_minor = crate::book::overpaid_after(schedule, *end);
            }
            Request::RecoveryStatus {
                overpayment,
                status,
                offset_pct,
                instalment_minor,
            } => {
                let Some(op) = claim
                    .overpayment(overpayment)
                    .filter(|o| !matches!(o.status, RecoveryStatus::WrittenOff | RecoveryStatus::Recovered))
                else {
                    return refuse("unknown_overpayment", "there is no such overpayment outstanding on this claim");
                };
                let sound = match status {
                    RecoveryStatus::Held | RecoveryStatus::Pursued => true,
                    RecoveryStatus::Offset => (1..=self.rules.max_offset_pct).contains(offset_pct),
                    RecoveryStatus::Arrangement => *instalment_minor > 0,
                    RecoveryStatus::WrittenOff | RecoveryStatus::Recovered => false,
                };
                if !sound {
                    return refuse(
                        "invalid_recovery_status",
                        "that status cannot be set this way, or its terms are outside the rules",
                    );
                }
                plan.amount_minor = op.outstanding_minor();
            }
            Request::WriteOff { overpayment } => {
                let Some(op) = claim
                    .overpayment(overpayment)
                    .filter(|o| o.outstanding_minor() > 0 && o.status != RecoveryStatus::WrittenOff)
                else {
                    return refuse("unknown_overpayment", "there is no such overpayment outstanding on this claim");
                };
                plan.amount_minor = op.outstanding_minor();
            }
        }
        let _ = cx;
        Ok(plan)
    }

    /// Whether a profile covers the whole of a decision.
    fn covers(&self, profile: &Profile, claim: &Claim, request: &Request, plan: &Plan) -> bool {
        let amount = plan.amount_minor;
        match request {
            Request::RecoveryStatus { .. } => profile.may_change_recovery_status,
            Request::WriteOff { .. } => amount <= profile.max_write_off_minor,
            _ if !profile.lines.contains(&claim.line()) => false,
            Request::Reserve { .. } => amount <= profile.max_reserve_minor,
            Request::Admit { exclusion_override, .. } => {
                amount <= profile.max_admission_minor.get(&claim.claim_type).copied().unwrap_or(0)
                    && (exclusion_override.is_none() || profile.may_override_exclusion)
            }
            Request::Decline => profile.may_decline,
            Request::ReverseAdmission { .. } => profile.may_reverse_admission && profile.senior,
            Request::ExGratia { .. } => amount <= profile.max_ex_gratia_minor,
            Request::Withdraw | Request::EndBenefit { .. } => true,
        }
    }

    /// The further signatures a decision needs before it takes effect.
    fn needs(&self, profile: &Profile, claim: &Claim, request: &Request, plan: &Plan) -> Vec<Need> {
        let amount = plan.amount_minor;
        let two_signatures = match request {
            Request::Decline | Request::ReverseAdmission { .. } => true,
            Request::Admit { .. } => amount >= self.rules.two_signature_admission_minor,
            Request::ExGratia { .. } => amount >= self.rules.two_signature_ex_gratia_minor,
            Request::WriteOff { .. } => amount >= self.rules.two_signature_write_off_minor,
            _ => false,
        };
        if let Request::RecoveryStatus { overpayment, status, .. } = request {
            // The insurer's own error and an innocent recipient: recovery is
            // a conduct decision, for the conduct officer and legal together.
            let protected = claim.overpayment(overpayment).is_some_and(Overpayment::protected);
            if protected && *status != RecoveryStatus::Held {
                return vec![Need::ConductOfficer, Need::Legal];
            }
        }
        if two_signatures || !self.covers(profile, claim, request, plan) {
            vec![Need::Authority]
        } else {
            vec![]
        }
    }

    pub(crate) fn decide(&mut self, cx: &mut Cx<'_>, id: &str, request: Request, reasons: Vec<String>, actor: &Actor) -> Result<(), Reject> {
        self.configured()?;
        let mut claim = self.claim(id)?;
        if claim.pending.is_some() {
            return refuse("decision_pending", "another decision on this claim is waiting for a signature");
        }
        let kind = request.kind();
        let recovery = matches!(kind, DecisionKind::RecoveryStatus | DecisionKind::WriteOff);
        if recovery {
            // Recoveries and the held balance workbench act on closed claims too.
            self.unconflicted(actor, &claim)?;
        } else {
            self.owner(actor, &claim)?;
        }
        let profile = self.authority.profile(&actor.staff)?;
        self.rules.check_reasons(kind, &reasons)?;
        let plan = self.plan(cx, &claim, &request)?;
        // Some authority cannot be borrowed from a second signer.
        let holds = match &request {
            Request::Decline => profile.may_decline,
            Request::ReverseAdmission { .. } => profile.may_reverse_admission && profile.senior,
            Request::RecoveryStatus { .. } => profile.may_change_recovery_status,
            Request::Admit {
                exclusion_override: Some(_), ..
            } => profile.may_override_exclusion,
            _ => true,
        };
        if !holds || !(recovery || profile.lines.contains(&claim.line())) {
            return refuse("authority", "your authority profile does not let you propose this decision");
        }
        let needs = self.needs(profile, &claim, &request, &plan);
        note(&mut claim, cx, Some(actor), format!("decision_proposed:{kind:?}"));
        if needs.is_empty() {
            self.execute(cx, &mut claim, &request, reasons, vec![actor.staff.clone()], plan)?;
        } else {
            claim.pending = Some(Pending {
                request,
                reasons,
                proposer: actor.staff.clone(),
                proposed_on: cx.today,
                needs,
                signed: vec![],
            });
            cx.notify("decision_pending", id, &[("kind", format!("{kind:?}"))]);
        }
        self.commit(claim);
        Ok(())
    }

    /// Which outstanding signature this person can give, if any.
    fn can_sign(&self, staff: &str, claim: &Claim, pending: &Pending, plan: &Plan) -> Option<usize> {
        let profile = self.authority.profile(staff).ok()?;
        pending.needs.iter().position(|need| match need {
            Need::Authority => self.covers(profile, claim, &pending.request, plan),
            Need::ConductOfficer => self.authority.has_role(staff, Role::ConductOfficer),
            Need::Legal => self.authority.has_role(staff, Role::Legal),
        })
    }

    pub(crate) fn approve(&mut self, cx: &mut Cx<'_>, id: &str, actor: &Actor) -> Result<(), Reject> {
        let mut claim = self.claim(id)?;
        let Some(mut pending) = claim.pending.take() else {
            return refuse("nothing_pending", "no decision on this claim is waiting for a signature");
        };
        self.unconflicted(actor, &claim)?;
        if pending.proposer == actor.staff || pending.signed.contains(&actor.staff) {
            return refuse("second_signer", "each signature must come from a different person");
        }
        let plan = self.plan(cx, &claim, &pending.request)?;
        let Some(position) = self.can_sign(&actor.staff, &claim, &pending, &plan) else {
            return refuse("authority", "you cannot give any signature this decision still needs");
        };
        pending.needs.remove(position);
        pending.signed.push(actor.staff.clone());
        note(&mut claim, cx, Some(actor), "decision_signed");
        if pending.needs.is_empty() {
            let signers = std::iter::once(pending.proposer).chain(pending.signed).collect();
            self.execute(cx, &mut claim, &pending.request, pending.reasons, signers, plan)?;
        } else {
            claim.pending = Some(pending);
        }
        self.commit(claim);
        Ok(())
    }

    pub(crate) fn refuse(&mut self, cx: &mut Cx<'_>, id: &str, actor: &Actor) -> Result<(), Reject> {
        let mut claim = self.claim(id)?;
        let Some(pending) = claim.pending.take() else {
            return refuse("nothing_pending", "no decision on this claim is waiting for a signature");
        };
        self.unconflicted(actor, &claim)?;
        let may = self.leads(&actor.staff) || self.can_sign(&actor.staff, &claim, &pending, &Plan::default()).is_some();
        if pending.proposer == actor.staff || !may {
            return refuse("authority", "a proposal is refused by someone who could have signed it");
        }
        note(&mut claim, cx, Some(actor), "decision_refused");
        cx.notify("decision_refused", id, &[("kind", format!("{:?}", pending.request.kind()))]);
        self.commit(claim);
        Ok(())
    }

    /// Close the claim in the ledger and in the module, in that order.
    pub(crate) fn close(&self, cx: &mut Cx<'_>, claim: &mut Claim, status: Status, overpayment: Option<Overpayment>) -> Result<(), Reject> {
        cx.core.execute(Instruction::Close {
            claim: claim.id.clone(),
            policy: claim.policy.clone(),
            cover: claim.cover.clone(),
            outcome: status.code().to_string(),
            overpayment: overpayment.as_ref().map(|o| (o.id.clone(), o.amount_minor)),
        })?;
        claim.reserve_minor = 0;
        claim.payable_minor = 0;
        claim.status = status;
        claim.closed_on = Some(cx.today);
        claim.due.clear();
        if let Some(o) = overpayment {
            cx.notify(
                "overpayment_raised",
                &claim.id,
                &[("amount_minor", o.amount_minor.to_string()), ("cause", format!("{:?}", o.cause))],
            );
            claim.overpayments.push(o);
        }
        cx.notify("claim_closed", &claim.id, &[("outcome", status.code().to_string())]);
        Ok(())
    }

    /// An overpayment found by replay: a fact, recorded as held.
    pub(crate) fn overpayment(claim: &Claim, cx: &Cx<'_>, amount_minor: i64, cause: OverpaymentCause, good_faith: bool) -> Option<Overpayment> {
        (amount_minor > 0).then(|| Overpayment {
            id: format!("{}/overpayment/{}", claim.id, claim.overpayments.len() + 1),
            raised_on: cx.today,
            amount_minor,
            recovered_minor: 0,
            written_off_minor: 0,
            status: RecoveryStatus::Held,
            cause,
            good_faith,
            offset_pct: 0,
            instalment_minor: 0,
            history: vec![(cx.today, RecoveryStatus::Held, claim.decisions.len() as u32 + 1)],
        })
    }

    /// Carry out a decision that has every signature it needs.
    fn execute(
        &mut self,
        cx: &mut Cx<'_>,
        claim: &mut Claim,
        request: &Request,
        reasons: Vec<String>,
        signers: Vec<StaffId>,
        plan: Plan,
    ) -> Result<(), Reject> {
        let kind = request.kind();
        let facts = claim.cover_check.as_ref().map(|c| c.facts.clone());
        let reason = reasons.first().cloned().unwrap_or_default();
        let mut decision = self.decision(claim, cx, kind, plan.amount_minor, reasons, signers.clone(), facts.as_ref());
        let (id, policy, cover) = (claim.id.clone(), claim.policy.clone(), claim.cover.clone());
        match request {
            Request::Reserve { amount_minor } => {
                cx.core.execute(Instruction::SetReserve {
                    claim: id.clone(),
                    policy,
                    cover,
                    amount_minor: *amount_minor,
                    reason,
                })?;
                let before = std::mem::replace(&mut claim.reserve_minor, *amount_minor);
                let moved = (amount_minor - before).abs().saturating_mul(100);
                if before > 0 && moved >= before.saturating_mul(i64::from(self.rules.sharp_reserve_move_pct)) {
                    // The reserve drives the books: a sharp move gets a senior's eye.
                    open_task(claim, cx, format!("senior-review/reserve/{}", decision.no), TaskKind::SeniorReview, None);
                }
                cx.notify("claim_reserve_changed", &id, &[("gross_reserve_minor", amount_minor.to_string())]);
            }
            Request::Admit {
                periodic, exclusion_override, ..
            } => {
                decision.exclusion_override = exclusion_override.clone();
                let pre_approval = claim.health.as_ref().filter(|h| h.pre_approval_request).map(|h| h.category.clone());
                if let Some(category) = pre_approval {
                    let expires = plus_days(cx.today, self.rules.pre_approval_days);
                    self.pre_approvals.insert(
                        id.clone(),
                        PreApproval {
                            claim: id.clone(),
                            policy,
                            life: claim.life.clone().unwrap_or_default(),
                            category,
                            amount_minor: plan.amount_minor,
                            expires,
                        },
                    );
                    claim.due.insert((expires, Due::PreApprovalExpiry));
                    cx.notify(
                        "pre_approval_given",
                        &id,
                        &[("amount_minor", plan.amount_minor.to_string()), ("expires", expires.to_string())],
                    );
                } else if let Some(terms) = periodic {
                    self.start_benefit(claim, cx, terms);
                    cx.notify(
                        "claim_admitted",
                        &id,
                        &[("admitted_minor", "0".into()), ("monthly_minor", terms.monthly_minor.to_string())],
                    );
                } else {
                    cx.core.execute(Instruction::Admit {
                        claim: id.clone(),
                        policy,
                        cover,
                        amount_minor: plan.amount_minor,
                        release_reserve: true,
                        deduct_premium_minor: plan.deduct_minor,
                        terminate: plan.terminate,
                    })?;
                    claim.status = Status::Admitted;
                    claim.admitted_minor += plan.amount_minor;
                    claim.payable_minor += plan.amount_minor - plan.deduct_minor;
                    claim.reserve_minor = 0;
                    if let Some(priced) = &plan.health {
                        self.use_health(cx, claim, priced, plan.amount_minor);
                    }
                    self.start_timer(claim, TimerKind::Payment, cx.today);
                    cx.notify("claim_admitted", &id, &[("admitted_minor", plan.amount_minor.to_string())]);
                }
                Self::meet_timer(claim, TimerKind::Decision, cx.today);
            }
            Request::Decline => self.close(cx, claim, Status::Declined, None)?,
            Request::Withdraw => self.close(cx, claim, Status::Withdrawn, None)?,
            Request::ReverseAdmission { false_declaration } => {
                // Why the admission was wrong, read from its own record.
                let finding = match claim.admission() {
                    Some(a) if a.exclusion_override.is_some() => Finding::AssessorOverride,
                    Some(a) if a.cause_code != claim.cause_code => Finding::CauseMiscoded,
                    _ => Finding::RulesUnmapped,
                };
                decision.finding = Some(finding);
                let cause = if *false_declaration {
                    OverpaymentCause::ClaimantFalseDeclaration
                } else {
                    OverpaymentCause::InsurerError
                };
                let overpayment = Self::overpayment(claim, cx, plan.overpaid_minor, cause, !false_declaration);
                self.close(cx, claim, Status::Reversed, overpayment)?;
                if let Some(s) = claim.schedule.as_mut() {
                    // The benefit stops from the decision date, never before it.
                    s.end = s.end.min(cx.today);
                }
                open_task(claim, cx, format!("finding/{}", finding.code()), TaskKind::QualityReview, None);
                cx.notify(
                    "admission_reversed",
                    &id,
                    &[
                        ("decided_on", cx.today.to_string()),
                        ("reason_code", reason),
                        ("cause", finding.code().to_string()),
                    ],
                );
            }
            Request::ExGratia { amount_minor, payment_ref } => {
                let payee = claim.payee.as_ref().map(|p| p.party.clone()).unwrap_or_default();
                cx.core.execute(Instruction::ExGratia {
                    claim: id.clone(),
                    policy,
                    cover,
                    payment_ref: payment_ref.clone(),
                    amount_minor: *amount_minor,
                    payee: payee.clone(),
                })?;
                claim.payments.push(Payment {
                    payment_ref: payment_ref.clone(),
                    amount_minor: *amount_minor,
                    paid_on: cx.today,
                    ex_gratia: true,
                    returned: false,
                    released_by: signers.first().cloned(),
                    run: None,
                });
                cx.paid(&id, payment_ref, *amount_minor, *amount_minor, &payee, None, true);
            }
            Request::EndBenefit { end, reason } => {
                let cause = if *reason == crate::EndReason::Death {
                    OverpaymentCause::LateDeathNotification
                } else {
                    OverpaymentCause::LateInformation
                };
                self.end_benefit(cx, claim, *end, *reason, plan.overpaid_minor, cause)?;
            }
            Request::RecoveryStatus {
                overpayment,
                status,
                offset_pct,
                instalment_minor,
            } => Self::set_recovery_status(cx, claim, overpayment, *status, *offset_pct, *instalment_minor, decision.no),
            Request::WriteOff { overpayment } => Self::write_off(cx, claim, overpayment, decision.no)?,
        }
        note(claim, cx, None, format!("decided:{kind:?}:{}", decision.no));
        self.sample(claim, cx, &decision);
        claim.decisions.push(decision);
        Ok(())
    }

    /// Open a closed claim again. History is never edited: the claim keeps
    /// its decisions and gains a new reserve.
    pub(crate) fn reopen(&mut self, cx: &mut Cx<'_>, id: &str, basis: ReopenBasis, reserve_minor: i64, actor: &Actor) -> Result<(), Reject> {
        self.configured()?;
        let mut claim = self.claim(id)?;
        self.unconflicted(actor, &claim)?;
        let profile = self.authority.profile(&actor.staff)?;
        if claim.status.is_open() {
            return refuse("not_closed", "the claim is still open");
        }
        if !profile.lines.contains(&claim.line()) || !(0..=profile.max_reserve_minor).contains(&reserve_minor) {
            return refuse("authority", "your authority profile does not cover this claim and reserve");
        }
        if basis == ReopenBasis::InternalReview && claim.decisions.iter().any(|d| d.signers.contains(&actor.staff)) {
            return refuse("independent_review", "an internal review is worked by someone who did not make the decision");
        }
        let recurrence = basis == ReopenBasis::Recurrence;
        let in_window = claim.status == Status::Ended && claim.closed_on.is_some_and(|c| cx.today <= plus_days(c, self.rules.recurrence_days));
        if recurrence && !(in_window && claim.schedule.is_some()) {
            return refuse("recurrence_window", "the claim is not an ended benefit inside the recurrence window");
        }
        cx.core.execute(Instruction::Reopen {
            claim: id.to_string(),
            policy: claim.policy.clone(),
            reserve_minor,
        })?;
        claim.reserve_minor = reserve_minor;
        claim.closed_on = None;
        claim.owner = Some(actor.staff.clone());
        match claim.schedule.as_mut().filter(|_| recurrence) {
            Some(s) => {
                // The same condition, back inside the window: the same claim
                // carries on, with no new waiting period.
                s.ended = None;
                s.end = s.benefit_end;
                s.next_period = cx.today;
                s.case_manager = Some(actor.staff.clone());
                claim.status = Status::InPayment;
            }
            None => {
                claim.status = Status::Open;
                self.start_timer(&mut claim, TimerKind::Decision, cx.today);
            }
        }
        if matches!(basis, ReopenBasis::InternalReview | ReopenBasis::DisputeScheme) {
            // Linked back to the assessor, the rule version and the wording.
            claim.overturns += 1;
        }
        note(&mut claim, cx, Some(actor), format!("reopened:{basis:?}"));
        cx.notify("claim_reopened", id, &[("basis", format!("{basis:?}"))]);
        cx.notify("claim_reserve_changed", id, &[("gross_reserve_minor", reserve_minor.to_string())]);
        self.commit(claim);
        Ok(())
    }

    /// Underwriting has decided a non-disclosure review opened from a claim.
    pub(crate) fn non_disclosure_outcome(&mut self, cx: &mut Cx<'_>, id: &str, review_ref: &str, outcome: NonDisclosureOutcome) -> Result<(), Reject> {
        let mut claim = self.claim(id)?;
        if !claim
            .tasks
            .iter()
            .any(|t| t.id == review_ref && t.kind == TaskKind::NonDisclosureReview && t.completed_on.is_none())
        {
            return refuse("unknown_task", "no such non-disclosure review is open on this claim");
        }
        finish_task(&mut claim, review_ref, cx.today, None, format!("{outcome:?}"), vec![], None, 0);
        note(&mut claim, cx, None, format!("non_disclosure_outcome:{outcome:?}"));
        self.commit(claim);
        Ok(())
    }
}
