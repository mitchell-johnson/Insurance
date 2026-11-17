//! Decisions: who may make them, who must sign with them, and what each does.
//!
//! A decision is proposed by one person. Where the rules ask for more
//! signatures it waits, and takes effect only when the last one is given.
//! The checks are run again at that moment, because the claim may have moved.

use chrono::NaiveDate;

use crate::command::{Actor, NonDisclosureOutcome, ReopenBasis, Request};
use crate::config::{Match, Profile, Queue, Role};
use crate::intake::{finish_task, HealthAmount};
use crate::model::{
    Claim, ClaimType, Decision, DecisionKind, Due, Finding, Need, Overpayment, OverpaymentCause, Payment, Pending, PreApproval, RecoveryStatus, Status,
    TaskKind, TimerKind,
};
use crate::port::{CoverFacts, Instruction, Reject, Termination};
use crate::state::{note, open_task, plus_days, plus_months, ClaimsState, Cx};
use crate::StaffId;

/// What a request comes to once it has been checked against the claim.
#[derive(Default)]
pub(crate) struct Plan {
    /// The amount the decision is for, which authority is measured against.
    pub amount_minor: i64,
    pub deduct_minor: i64,
    /// A policy loan repaid out of a death benefit.
    pub loan_minor: i64,
    pub terminate: Option<Termination>,
    /// Money already paid that the decision makes an overpayment.
    pub overpaid_minor: i64,
    pub health: Option<HealthAmount>,
    /// What each interested party takes of a lump sum before the claimant.
    pub interests: Vec<crate::model::InterestDue>,
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
    /// What lump sums already admitted have used of this cover: on the cover
    /// for this life, or for any life where the cover pays once on the first
    /// event, and on the covers whose payments reduce it.
    fn earlier_admitted(&self, claim: &Claim, facts: &CoverFacts) -> i64 {
        if facts.ends_cover == Some(false) {
            // A cover a claim leaves in place is not used up by earlier claims.
            return 0;
        }
        let others = self.by_policy.get(&claim.policy).into_iter().flatten().filter_map(|id| self.claims.get(id));
        let joint = |c: &Claim| c.life.as_ref().is_some_and(|l| facts.joint_lives.contains(l));
        others
            .filter(|c| c.id != claim.id && !c.claim_type.periodic() && c.status != Status::Reversed)
            .filter(|c| (c.cover == claim.cover && (c.life == claim.life || joint(c))) || (c.life == claim.life && facts.reduced_by.contains(&c.cover)))
            .map(|c| c.admitted_minor - Self::on_top(c, facts))
            .sum()
    }

    /// What a claim admitted under built-in benefits paid on top of the
    /// cover: everything but early payments.
    fn on_top(claim: &Claim, facts: &CoverFacts) -> i64 {
        claim
            .built_in
            .iter()
            .filter(|(code, _)| !facts.built_in.iter().any(|b| &b.code == *code && b.reduces_cover()))
            .map(|(_, a)| a)
            .sum()
    }

    /// What is left of the policy's cap for one event, where it has one:
    /// the cap less what every other claim on the policy for the same life
    /// and event date has admitted, less what this claim already has.
    fn event_room(&self, claim: &Claim, facts: &CoverFacts) -> Option<i64> {
        let cap = facts.event_cap_minor?;
        let others: i64 = self
            .by_policy
            .get(&claim.policy)
            .into_iter()
            .flatten()
            .filter_map(|id| self.claims.get(id))
            .filter(|c| c.id != claim.id && c.life == claim.life && c.event_date == claim.event_date && c.status != Status::Reversed)
            .map(|c| c.admitted_minor)
            .sum();
        Some(cap - others - claim.admitted_minor)
    }

    /// The day the waiting period runs from: the event date, or, under the
    /// Concurrent Waiting Period Benefit, the earlier disablement date of a
    /// business cover claim for the same person and cause on another policy
    /// that is still being paid or ended within the last 12 months, so the
    /// waiting already served there counts here.
    fn waiting_from(&self, claim: &Claim, facts: &CoverFacts) -> NaiveDate {
        let (Some(_), Some(party)) = (facts.has_built_in("concurrent-waiting-period"), claim.life_party.as_ref()) else {
            return claim.event_date;
        };
        self.by_party
            .get(party)
            .into_iter()
            .flatten()
            .filter_map(|id| self.claims.get(id))
            .filter(|c| c.id != claim.id && c.policy != claim.policy && c.life_party.as_ref() == Some(party))
            .filter(|c| c.claim_type.periodic() && c.cause_code.is_some() && c.cause_code == claim.cause_code)
            .filter(|c| c.event_date < claim.event_date)
            .filter(|c| match c.status {
                Status::InPayment => true,
                Status::Ended => c.closed_on.is_some_and(|closed| claim.event_date <= plus_months(closed, 12)),
                _ => false,
            })
            .map(|c| c.event_date)
            .min()
            .unwrap_or(claim.event_date)
    }

    /// Days of income benefit already paid for a mental health cause on
    /// this cover for this life, on other claims.
    fn mental_health_days(&self, claim: &Claim) -> i64 {
        self.by_policy
            .get(&claim.policy)
            .into_iter()
            .flatten()
            .filter_map(|id| self.claims.get(id))
            .filter(|c| c.id != claim.id && c.cover == claim.cover && c.life == claim.life && c.status != Status::Reversed)
            .filter(|c| self.rules.cause_in(c.cause_code.as_deref(), "mental"))
            .filter_map(|c| c.schedule.as_ref())
            .flat_map(|s| s.periods.iter().filter(|(_, p)| !p.returned).map(|(start, p)| (p.end - *start).num_days()))
            .sum()
    }

    /// Check the cover for a decision that pays from it: checked, in force
    /// for a member, and with a cause recorded.
    fn covered(claim: &Claim) -> Result<&CoverFacts, Reject> {
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
        Ok(facts)
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
                let facts = Self::covered(claim)?;
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
                        // A premium waiver claim pays no money to anyone: its benefit is the premium.
                        if claim.claim_type == crate::ClaimType::PremiumWaiver && facts.premium_waiver == Some(false) {
                            return refuse("no_waiver_cover", "the product does not waive premium on this cover");
                        }
                        let amount_fits = match claim.claim_type {
                            crate::ClaimType::PremiumWaiver => p.monthly_minor == 0,
                            _ => p.monthly_minor > 0,
                        };
                        let sound = amount_fits
                            && p.monthly_minor <= facts.sum_insured_minor
                            && p.first_period >= plus_days(self.waiting_from(claim, facts), facts.waiting_days)
                            && p.end > p.first_period
                            && p.expected_end > p.first_period;
                        if !sound {
                            return refuse("invalid_schedule", "the benefit terms do not fit the cover");
                        }
                        // The option the life chose sets the longest benefit period and the age it stops at.
                        let by_period = facts.benefit_months.map(|m| plus_months(p.first_period, m));
                        if [by_period, facts.benefit_to].into_iter().flatten().any(|latest| p.end > latest) {
                            return refuse("beyond_benefit_period", "the benefit runs past the period or the age the chosen option pays to");
                        }
                        let mental = self.rules.cause_in(claim.cause_code.as_deref(), "mental");
                        if let Some(months) = facts.mental_health_months.filter(|_| mental) {
                            // A limit in all: months already paid on earlier mental health claims count.
                            let used = u64::try_from(self.mental_health_days(claim)).unwrap_or(0);
                            let latest = plus_months(p.first_period, months)
                                .checked_sub_days(chrono::Days::new(used))
                                .unwrap_or(p.first_period);
                            if p.end > latest {
                                return refuse("mental_health_limit", "the benefit runs past what the cover pays in all for mental health");
                            }
                        }
                        plan.amount_minor = claim.reserve_minor.max(p.monthly_minor);
                    }
                    (false, None) => {
                        // A disablement claim's qualifying period runs before it is paid, unless waived.
                        let qualifying = claim.claim_type == ClaimType::TotalPermanentDisability && facts.waiting_days > 0;
                        if qualifying && claim.waiting_waived.is_none() && cx.today < plus_days(claim.event_date, facts.waiting_days) {
                            return refuse("qualifying_period", "the incapacity has not yet lasted the cover's qualifying period");
                        }
                        // A lump sum with a survival period is paid only once the life has survived it.
                        if claim.health.is_none() && facts.survival_days > 0 && cx.today < plus_days(claim.event_date, facts.survival_days) {
                            return refuse("survival_period", "the life has not yet survived the cover's survival period after the event");
                        }
                        plan.health = self.health_amount(claim, facts);
                        let mut cap = match &plan.health {
                            Some(h) => h.amount_minor,
                            // An early payment built into the cover was paid out of the sum insured.
                            None => {
                                facts.sum_insured_minor
                                    - self.earlier_admitted(claim, facts)
                                    - (claim.admitted_minor - claim.advanced_minor - Self::on_top(claim, facts))
                            }
                        };
                        if let Some(room) = self.event_room(claim, facts) {
                            // The advance is part of this benefit, so it is room already used.
                            cap = cap.min(room + claim.advanced_minor);
                        }
                        let amount = amount_minor.unwrap_or(cap);
                        if amount <= 0 || amount > cap {
                            return refuse("amount_exceeds_cover", "the amount is more than the cover pays for this event");
                        }
                        if amount <= claim.advanced_minor {
                            return refuse("below_advance", "the benefit admitted is not more than was already advanced on it");
                        }
                        // The advance is already admitted and paid out of this benefit.
                        let amount = amount - claim.advanced_minor;
                        plan.amount_minor = amount;
                        if facts.standing.as_deref() == Some("in_grace") {
                            // A claim in grace is payable less the premium owing.
                            plan.deduct_minor = facts.outstanding_premium_minor.clamp(0, amount);
                        }
                        if plan.health.is_none() {
                            // A loan against the policy is repaid out of a death benefit first.
                            if claim.claim_type == ClaimType::Death {
                                plan.loan_minor = facts.policy_loan_minor.clamp(0, amount - plan.deduct_minor);
                            }
                            // Then recorded interests, in priority order, each up to its extent.
                            let mut left = amount - plan.deduct_minor - plan.loan_minor;
                            let mut recorded: Vec<_> = facts.interests.iter().collect();
                            recorded.sort_by(|a, b| (a.priority, &a.party).cmp(&(b.priority, &b.party)));
                            // The Official Assignee's interest is not paid out: it holds what the
                            // owner or the estate would get until a legal direction.
                            for i in recorded.into_iter().filter(|i| !i.insolvency) {
                                let owed = i.extent_minor.map_or(left, |e| e.clamp(0, left));
                                left -= owed;
                                if owed > 0 {
                                    plan.interests.push(crate::model::InterestDue {
                                        party: i.party.clone(),
                                        priority: i.priority,
                                        owed_minor: owed,
                                        paid_minor: 0,
                                        nominated: false,
                                    });
                                }
                            }
                            if claim.claim_type == ClaimType::Death {
                                // What is left goes to the nomination in force on the date of
                                // death, in its shares, after every interest; anything not
                                // nominated goes to the estate.
                                let after = facts.interests.iter().map(|i| i.priority).max().unwrap_or(0) + 1;
                                let total: u32 = facts.nominations.iter().map(|n| n.share_pct).sum();
                                if total > 100 || facts.nominations.iter().any(|n| n.share_pct == 0) {
                                    return refuse("invalid_nomination", "the nomination's shares are not between 1 and 100 percent in all");
                                }
                                let pool = left;
                                let mut shares: Vec<(String, i64)> = facts
                                    .nominations
                                    .iter()
                                    .map(|n| (n.party.clone(), pool * i64::from(n.share_pct) / 100))
                                    .collect();
                                if total == 100 {
                                    // Whole shares leave a few cents unshared; the last nominee has them.
                                    let rounding = pool - shares.iter().map(|(_, a)| a).sum::<i64>();
                                    if let Some(last) = shares.last_mut() {
                                        last.1 += rounding;
                                    }
                                }
                                for (party, owed) in shares.into_iter().filter(|(_, a)| *a > 0) {
                                    plan.interests.push(crate::model::InterestDue {
                                        party,
                                        priority: after,
                                        owed_minor: owed,
                                        paid_minor: 0,
                                        nominated: true,
                                    });
                                }
                            }
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
                if matches!(request, Request::Decline) && claim.has_open_task(TaskKind::NonDisclosureReview) {
                    return refuse(
                        "awaiting_non_disclosure_review",
                        "a non-disclosure review is open on this claim: underwriting decides the remedy first",
                    );
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
            Request::AdmitBuiltIn { code, amount_minor, settles } => {
                let open = claim.status == Status::Open || (claim.status == Status::InPayment && !settles);
                if !open {
                    return refuse("not_open", "a built-in benefit is admitted on an open claim, or beside a benefit in payment");
                }
                let facts = Self::covered(claim)?;
                let Some(benefit) = facts.built_in.iter().find(|b| &b.code == code) else {
                    return refuse("unknown_benefit", "the cover has no built-in benefit with that code");
                };
                if !benefit.applies {
                    return refuse("built_in_not_chosen", "the built-in benefit needs a feature the life did not choose");
                }
                // A business mentor is the one facility paid for: the rest are
                // services arranged for the claimant, or ride with the benefit.
                let mentors = code == "business-mentors";
                match benefit.kind.as_str() {
                    "monthly_addition" => {
                        return refuse(
                            "paid_with_the_benefit",
                            "this benefit is added to each monthly payment, not admitted on its own",
                        )
                    }
                    "facility" if mentors => {
                        if claim.status != Status::InPayment || *settles {
                            return refuse("not_in_payment", "a business mentor is paid for while the income benefit is being paid");
                        }
                    }
                    "facility" | "premium_waiver" => return refuse("not_payable", "this benefit is a service or a premium waiver, not a payment"),
                    _ => {}
                }
                let limit = if mentors {
                    match benefit.limit_minor.or(Some(self.rules.business_mentors_minor).filter(|m| *m > 0)) {
                        Some(limit) => Some(limit),
                        None => return refuse("not_configured", "the most paid for a business mentor is not set in the claims rules"),
                    }
                } else {
                    benefit.limit_minor
                };
                let used: i64 = self
                    .by_policy
                    .get(&claim.policy)
                    .into_iter()
                    .flatten()
                    .filter_map(|id| self.claims.get(id))
                    // One business mentor for each business: the policy, whichever life claims.
                    .filter(|c| (mentors || c.life == claim.life) && c.status != Status::Reversed)
                    .filter_map(|c| c.built_in.get(code))
                    .sum();
                let mut room = limit.map_or(i64::MAX, |l| l - used);
                if benefit.reduces_cover() {
                    let taken = self.earlier_admitted(claim, facts) + claim.admitted_minor - Self::on_top(claim, facts);
                    room = room.min(facts.sum_insured_minor - taken);
                }
                if let Some(event) = self.event_room(claim, facts) {
                    room = room.min(event);
                }
                if *amount_minor <= 0 || *amount_minor > room {
                    return refuse("built_in_limit", "the amount is more than the built-in benefit has left to pay");
                }
                plan.amount_minor = *amount_minor;
            }
            Request::FuneralAdvance { amount_minor } => {
                if claim.claim_type != ClaimType::Death || claim.status != Status::Open {
                    return refuse("not_open", "an advance is made on an open death claim, before it is decided");
                }
                let room = self.rules.funeral_advance_max_minor - claim.advanced_minor;
                if *amount_minor <= 0 || *amount_minor > room {
                    return Err(Reject::new(
                        "advance_limit",
                        format!(
                            "an advance before a decision is at most {} in all, and {} of it is left on this claim",
                            ic_money::dollars(self.rules.funeral_advance_max_minor),
                            ic_money::dollars(room.max(0))
                        ),
                    ));
                }
                if claim
                    .cover_check
                    .as_ref()
                    .is_some_and(|c| !c.facts.in_force || *amount_minor > c.facts.sum_insured_minor)
                {
                    return refuse("cover_not_confirmed", "the cover check found no cover for this advance");
                }
                plan.amount_minor = *amount_minor;
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
            Request::SuspendBenefit { from, evidence } => {
                let Some(schedule) = claim.schedule.as_ref().filter(|_| claim.status == Status::InPayment) else {
                    return refuse("not_in_payment", "only a benefit in payment can be suspended");
                };
                let wording = claim.cover_check.as_ref().map(|c| c.facts.benefit_while_imprisoned);
                if wording == Some(crate::port::Imprisonment::Pay) {
                    return refuse("paid_in_custody", "the product's wording pays this benefit in custody");
                }
                if schedule.open_suspension().is_some() || *from >= schedule.end || schedule.suspended.keys().any(|k| k >= from) {
                    return refuse("invalid_suspension", "the benefit is already suspended, or the date is outside it");
                }
                if evidence.trim().is_empty() {
                    return refuse("evidence_required", "file the evidence of custody after sentence first");
                }
                if !claim.task(TaskKind::ClaimantConversation).any(|t| t.completed_on.is_some()) {
                    return refuse(
                        "conversation_required",
                        "talk to the claimant or their representative first, and record it as a task",
                    );
                }
            }
            Request::PermanentDisablement { ends } => {
                let Some(s) = claim.schedule.as_ref().filter(|_| claim.status == Status::InPayment) else {
                    return refuse("not_in_payment", "the benefit is added to a benefit in payment");
                };
                if claim.cover_check.as_ref().and_then(|c| c.facts.has_built_in("permanent-disablement")).is_none() {
                    return refuse("no_permanent_benefit", "the cover has no Permanent Disablement Benefit");
                }
                if *ends {
                    if s.permanent_from.is_none() || s.permanent_until.is_some() {
                        return refuse("not_running", "the Permanent Disablement Benefit is not being paid");
                    }
                } else {
                    if s.permanent_from.is_some() {
                        return refuse("already_started", "the Permanent Disablement Benefit has already been accepted on this claim");
                    }
                    // 24 consecutive months paid: from the first period, with nothing returned and no suspension.
                    let first = s.periods.keys().next().copied();
                    let consecutive = first.is_some_and(|first| {
                        let through = plus_months(first, 24);
                        s.next_period >= through && !s.periods.range(..through).any(|(_, p)| p.returned) && s.suspended_days(first, through) == 0
                    });
                    if !consecutive {
                        return refuse("not_24_months", "the benefit has not been paid for 24 consecutive months");
                    }
                }
                plan.amount_minor = s.monthly_on(cx.today) / 3;
            }
            Request::BuiltInWaiver { code, from } => {
                if claim.claim_type != ClaimType::PremiumWaiver || claim.status != Status::Open {
                    return refuse("not_open", "a built-in premium waiver is admitted on an open waiver claim");
                }
                let facts = Self::covered(claim)?;
                let Some(benefit) = facts.built_in.iter().find(|b| &b.code == code && b.kind == "premium_waiver") else {
                    return refuse("unknown_benefit", "the cover has no built-in premium waiver with that code");
                };
                if !benefit.applies {
                    return refuse("built_in_not_chosen", "the built-in benefit needs a feature the life did not choose");
                }
                if !claim.evidence.iter().any(|e| e.received_on.is_some()) {
                    return refuse("evidence_required", "the doctor's confirmation or the death certificate must be on file");
                }
                if waiver_months(code).is_none() {
                    return refuse("no_waiver_terms", "the claims module does not know how long this waiver runs");
                }
                if code == PREGNANCY_WAIVER {
                    // The event is the start of the second trimester; the birth, about six
                    // months later, must be at least 12 months after the cover started.
                    if facts.cover_start.is_none_or(|start| claim.event_date < plus_months(start, 6)) {
                        return refuse("too_soon_after_start", "the birth must be at least 12 months after the cover started");
                    }
                }
                // From the event to six months after the pregnancy ends, or after the death.
                if *from < claim.event_date || *from > plus_months(claim.event_date, 12) {
                    return refuse("outside_waiver_window", "the waiver starts outside the window the benefit allows");
                }
                // Nothing is paid: the premium is the benefit.
                plan.amount_minor = 0;
            }
            Request::ResumeBenefit { from } => {
                let open = claim
                    .schedule
                    .as_ref()
                    .filter(|_| claim.status == Status::InPayment)
                    .and_then(|s| s.open_suspension());
                if !open.is_some_and(|start| *from > start) {
                    return refuse("not_suspended", "the benefit is not suspended, or resumes before it was suspended");
                }
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
                // Measured against the whole benefit, the advance with it.
                amount + claim.advanced_minor <= profile.max_admission_minor.get(&claim.claim_type).copied().unwrap_or(0)
                    && (exclusion_override.is_none() || profile.may_override_exclusion)
            }
            Request::Decline => profile.may_decline,
            Request::ReverseAdmission { .. } => profile.may_reverse_admission && profile.senior,
            Request::ExGratia { .. } => amount <= profile.max_ex_gratia_minor,
            Request::FuneralAdvance { .. } | Request::AdmitBuiltIn { .. } | Request::PermanentDisablement { .. } => {
                amount <= profile.max_admission_minor.get(&claim.claim_type).copied().unwrap_or(0)
            }
            Request::Withdraw | Request::EndBenefit { .. } | Request::SuspendBenefit { .. } | Request::ResumeBenefit { .. } | Request::BuiltInWaiver { .. } => {
                true
            }
        }
    }

    /// The further signatures a decision needs before it takes effect.
    fn needs(&self, profile: &Profile, claim: &Claim, request: &Request, plan: &Plan) -> Vec<Need> {
        let amount = plan.amount_minor;
        let two_signatures = match request {
            Request::Decline | Request::ReverseAdmission { .. } => true,
            Request::Admit { .. } => amount + claim.advanced_minor >= self.rules.two_signature_admission_minor,
            Request::AdmitBuiltIn { .. } => amount >= self.rules.two_signature_admission_minor,
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
        if kind == DecisionKind::Decline && reasons.iter().any(|r| self.rules.non_disclosure_reasons.contains(r)) {
            let avoided = claim
                .task(TaskKind::NonDisclosureReview)
                .any(|t| t.completed_on.is_some() && t.outcome.as_deref() == Some("Avoid"));
            if !avoided {
                return refuse(
                    "no_non_disclosure_remedy",
                    "a decline for non-disclosure stands on underwriting's remedy: refer the claim for a non-disclosure review, and decline only once it has avoided the policy",
                );
            }
        }
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
            crate::overpay::publish(cx, "overpayment_raised", claim, &o, &[("amount_minor", o.amount_minor.to_string())]);
            claim.overpayments.push(o);
        }
        cx.lifecycle(&self.rules, "claim_closed", claim, &[("outcome", status.code().to_string())]);
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
            shared_seq: 0,
            shared: None,
            remote_received: Default::default(),
            letters: vec![],
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
                let insolvent = claim.cover_check.as_ref().is_some_and(|c| c.facts.interests.iter().any(|i| i.insolvency));
                if insolvent && claim.claim_type != ClaimType::Health {
                    claim.insolvency_hold = true;
                    cx.notify("proceeds_held", &id, &[("reason", "insolvency_interest".into())]);
                }
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
                    cx.lifecycle(
                        &self.rules,
                        "claim_admitted",
                        claim,
                        &[("admitted_minor", "0".into()), ("monthly_minor", terms.monthly_minor.to_string())],
                    );
                } else {
                    // The core takes a lump sum off the life's cover and the covers linked to it.
                    // A cover a claim leaves in place is not reduced by it.
                    let stays = claim.cover_check.as_ref().is_some_and(|c| c.facts.ends_cover == Some(false));
                    let life = claim.life.clone().filter(|_| plan.health.is_none() && !stays);
                    cx.core.execute(Instruction::Admit {
                        life,
                        claim: id.clone(),
                        policy,
                        cover,
                        amount_minor: plan.amount_minor,
                        release_reserve: true,
                        deduct_premium_minor: plan.deduct_minor,
                        terminate: plan.terminate,
                        repay_loan_minor: plan.loan_minor,
                    })?;
                    claim.status = Status::Admitted;
                    claim.admitted_minor += plan.amount_minor;
                    claim.payable_minor += plan.amount_minor - plan.deduct_minor - plan.loan_minor;
                    claim.loan_repaid_minor += plan.loan_minor;
                    claim.reserve_minor = 0;
                    let advanced = claim.advanced_minor;
                    for due in &plan.interests {
                        let basis = if due.nominated { "nomination" } else { "interest" };
                        cx.notify(
                            "interest_to_be_paid",
                            &id,
                            &[
                                ("party", due.party.clone()),
                                ("owed_minor", due.owed_minor.to_string()),
                                ("basis", basis.to_string()),
                            ],
                        );
                    }
                    claim.interests.extend(plan.interests.iter().cloned());
                    if let Some(priced) = &plan.health {
                        self.use_health(cx, claim, priced, plan.amount_minor);
                    }
                    self.start_timer(claim, TimerKind::Payment, cx.today);
                    cx.lifecycle(
                        &self.rules,
                        "claim_admitted",
                        claim,
                        &[
                            // The whole benefit: what was advanced was part of it.
                            ("admitted_minor", (plan.amount_minor + advanced).to_string()),
                            ("advanced_minor", advanced.to_string()),
                            ("loan_repaid_minor", plan.loan_minor.to_string()),
                        ],
                    );
                }
                Self::meet_timer(claim, TimerKind::Decision, cx.today);
            }
            Request::AdmitBuiltIn { code, amount_minor, settles } => {
                let reduces = claim
                    .cover_check
                    .as_ref()
                    .and_then(|c| c.facts.built_in.iter().find(|b| &b.code == code))
                    .is_some_and(crate::port::BuiltIn::reduces_cover);
                cx.core.execute(Instruction::Admit {
                    // Only an early payment comes off the cover.
                    life: claim.life.clone().filter(|_| reduces),
                    claim: id.clone(),
                    policy,
                    cover,
                    amount_minor: *amount_minor,
                    release_reserve: *settles,
                    deduct_premium_minor: 0,
                    terminate: None,
                    repay_loan_minor: 0,
                })?;
                claim.admitted_minor += amount_minor;
                claim.payable_minor += amount_minor;
                *claim.built_in.entry(code.clone()).or_default() += amount_minor;
                if *settles {
                    claim.status = Status::Admitted;
                    claim.reserve_minor = 0;
                    self.start_timer(claim, TimerKind::Payment, cx.today);
                    Self::meet_timer(claim, TimerKind::Decision, cx.today);
                    cx.lifecycle(
                        &self.rules,
                        "claim_admitted",
                        claim,
                        &[("admitted_minor", amount_minor.to_string()), ("built_in", code.clone())],
                    );
                } else {
                    cx.notify("built_in_admitted", &id, &[("code", code.clone()), ("amount_minor", amount_minor.to_string())]);
                }
            }
            Request::FuneralAdvance { amount_minor } => {
                // The reserve falls by what is advanced, so the claim's cost is unchanged.
                let reserve = (claim.reserve_minor - amount_minor).max(0);
                cx.core.execute(Instruction::SetReserve {
                    claim: id.clone(),
                    policy: policy.clone(),
                    cover: cover.clone(),
                    amount_minor: reserve,
                    reason: "funeral_advance".into(),
                })?;
                cx.core.execute(Instruction::Admit {
                    life: None,
                    claim: id.clone(),
                    policy,
                    cover,
                    amount_minor: *amount_minor,
                    release_reserve: false,
                    deduct_premium_minor: 0,
                    terminate: None,
                    repay_loan_minor: 0,
                })?;
                claim.reserve_minor = reserve;
                claim.admitted_minor += amount_minor;
                claim.payable_minor += amount_minor;
                claim.advanced_minor += amount_minor;
                cx.notify("funeral_advance_admitted", &id, &[("amount_minor", amount_minor.to_string())]);
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
                if let (true, Some(from)) = (*false_declaration, claim.waiver_from) {
                    // Premium waived on a false declaration is owed again.
                    cx.core.execute(Instruction::UnwaivePremium {
                        claim: id.clone(),
                        policy: claim.policy.clone(),
                        cover: claim.cover.clone(),
                        from,
                    })?;
                    note(claim, cx, None, "waiver_billed_back");
                }
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
                    cost_item: None,
                    interest_minor: 0,
                    interest_declined_minor: 0,
                    interest_withheld_minor: 0,
                    interested_party: None,
                });
                cx.paid(claim, payment_ref, *amount_minor, *amount_minor, &payee, None, true, 0);
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
            Request::SuspendBenefit { from, .. } => self.suspend_benefit(cx, claim, *from)?,
            Request::ResumeBenefit { from } => self.resume_benefit(cx, claim, *from),
            Request::BuiltInWaiver { code, from } => {
                let end = plus_months(*from, waiver_months(code).unwrap_or(0));
                let terms = crate::command::Periodic {
                    monthly_minor: 0,
                    first_period: *from,
                    end,
                    expected_end: end,
                };
                // Recorded as a built-in admitted for nothing, so the waiver knows its basis.
                claim.built_in.entry(code.clone()).or_default();
                self.start_benefit(claim, cx, &terms);
                Self::meet_timer(claim, TimerKind::Decision, cx.today);
                cx.lifecycle(
                    &self.rules,
                    "claim_admitted",
                    claim,
                    &[("admitted_minor", "0".into()), ("built_in", code.clone()), ("until", end.to_string())],
                );
            }
            Request::PermanentDisablement { ends } => {
                for s in claim.schedule.iter_mut() {
                    if *ends {
                        s.permanent_until = Some(cx.today);
                    } else {
                        s.permanent_from = Some(cx.today);
                    }
                }
                let kind = if *ends {
                    "permanent_disablement_ended"
                } else {
                    "permanent_disablement_accepted"
                };
                cx.notify(kind, &id, &[("monthly_minor", plan.amount_minor.to_string()), ("from", cx.today.to_string())]);
            }
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
        // The product's Recurrent Disablement Benefit sets the window: 12
        // months after payments ceased. A product that lists its benefits
        // without it has none; one that says nothing takes the rules' window.
        let facts = claim.cover_check.as_ref().map(|c| &c.facts).filter(|f| f.lists_built_ins());
        let window_end = |closed: NaiveDate| match facts {
            Some(f) => f.has_built_in("recurrent-disablement").map(|_| plus_months(closed, 12)),
            None => Some(plus_days(closed, self.rules.recurrence_days)),
        };
        let in_window = claim.status == Status::Ended && claim.closed_on.and_then(window_end).is_some_and(|end| cx.today <= end);
        if recurrence && !(in_window && claim.schedule.is_some()) {
            return refuse("recurrence_window", "the claim is not an ended benefit inside the recurrence window");
        }
        let undeath = basis == ReopenBasis::DeathRetracted;
        let ended_by_death = claim.status == Status::Ended && claim.schedule.as_ref().is_some_and(|s| s.ended == Some(crate::EndReason::Death));
        if undeath && !(ended_by_death && claim.history.iter().any(|h| h.action.starts_with("death_retracted:"))) {
            return refuse("death_not_retracted", "the claim was not ended by a death that has since been retracted");
        }
        cx.core.execute(Instruction::Reopen {
            claim: id.to_string(),
            policy: claim.policy.clone(),
            reserve_minor,
        })?;
        claim.reserve_minor = reserve_minor;
        claim.closed_on = None;
        claim.owner = Some(actor.staff.clone());
        match claim.schedule.as_mut().filter(|_| recurrence || undeath) {
            Some(s) => {
                // The same condition, back inside the window: the same claim
                // carries on, with no new waiting period. After a retracted
                // death it carries on from the last period paid, so the gap is paid.
                s.ended = None;
                s.end = s.benefit_end;
                if recurrence {
                    s.next_period = cx.today;
                }
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

impl ClaimsState {
    /// No benefit from `from` while the life is in custody. Periods already
    /// paid for those days become an overpayment held as a fact. The review
    /// timers wait; the claim, the waiver and the cover carry on.
    fn suspend_benefit(&mut self, cx: &mut Cx<'_>, claim: &mut Claim, from: NaiveDate) -> Result<(), Reject> {
        let Some(before) = claim.schedule.clone() else { return Ok(()) };
        let mut after = before.clone();
        after.suspended.insert(from, NaiveDate::MAX);
        let (over, _) = crate::book::replay(&before, &after);
        if let Some(o) = Self::overpayment(claim, cx, over, OverpaymentCause::LateInformation, true) {
            cx.core.execute(Instruction::RaiseOverpayment {
                claim: claim.id.clone(),
                policy: claim.policy.clone(),
                cover: claim.cover.clone(),
                overpayment: o.id.clone(),
                amount_minor: over,
            })?;
            crate::overpay::publish(cx, "overpayment_raised", claim, &o, &[("amount_minor", over.to_string())]);
            claim.overpayments.push(o);
        }
        // The reviews wait for the release; they are put back then, as far on.
        claim.due.retain(|(_, d)| !matches!(d, Due::Review(_)));
        claim.schedule = Some(after);
        cx.notify("benefit_suspended", &claim.id, &[("from", from.to_string())]);
        Ok(())
    }

    /// Released: the benefit is payable again from `from`, with no new
    /// waiting period. Periods paid with nothing for days after `from` are
    /// owed and go with the next benefit.
    fn resume_benefit(&mut self, cx: &mut Cx<'_>, claim: &mut Claim, from: NaiveDate) {
        let Some(before) = claim.schedule.clone() else { return };
        let Some(start) = before.open_suspension() else { return };
        let mut after = before.clone();
        after.suspended.insert(start, from);
        let (_, under) = crate::book::replay(&before, &after);
        after.arrears_minor += under;
        // The review plan moves on by the time it waited.
        let paused = chrono::Days::new(u64::try_from((from - start).num_days()).unwrap_or(0));
        for (kind, on) in after.reviews.iter_mut() {
            *on = on.checked_add_days(paused).unwrap_or(NaiveDate::MAX);
            if *on < after.end {
                claim.due.insert((*on, Due::Review(*kind)));
            }
        }
        claim.schedule = Some(after);
        if under > 0 {
            cx.notify("underpayment_owed", &claim.id, &[("amount_minor", under.to_string())]);
        }
        cx.notify("benefit_resumed", &claim.id, &[("from", from.to_string())]);
    }
}

/// AIA's pregnancy premium waiver on its income covers.
pub(crate) const PREGNANCY_WAIVER: &str = "pregnancy-premium-waiver";

/// How many months a built-in premium waiver runs, by its code.
fn waiver_months(code: &str) -> Option<u32> {
    match code {
        PREGNANCY_WAIVER => Some(6),
        "waiver-on-death" => Some(24),
        _ => None,
    }
}
