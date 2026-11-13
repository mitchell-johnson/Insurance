//! Paying claims: lump sums, benefits in payment as a book, and payment
//! runs as controlled batches.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;

use crate::command::{Actor, Periodic};
use crate::model::{
    Claim, DecisionKind, Due, EndReason, OverpaymentCause, Payee, PayeeKind, Payment, Period, RecoveryStatus, Run, RunItem, RunKind, RunStatus, Schedule,
    Status, TaskKind, TimerKind,
};
use crate::port::{Instruction, Offset, Reject};
use crate::state::{note, open_task, plus_days, plus_months, ClaimsState, Cx};

/// Payments one run may carry, so approving it stays inside the partition's
/// apply budget. A larger book is paid by more than one run.
pub const MAX_RUN_ITEMS: usize = 250;

/// The benefit for one period under the facts as now known: the monthly
/// benefit less other income declared for the period, by days for a part month.
pub(crate) fn period_gross(s: &Schedule, start: NaiveDate, end: NaiveDate) -> i64 {
    let income = s.income.range(..=start).next_back().map_or(0, |(_, v)| *v);
    let monthly = (s.monthly_minor - income).max(0);
    let full = plus_months(start, 1);
    let gross = if end >= full {
        monthly
    } else {
        monthly * (end - start).num_days() / (full - start).num_days().max(1)
    };
    // Days in custody after sentence are not paid for.
    let span = (end - start).num_days();
    let suspended = s.suspended_days(start, end);
    if suspended == 0 || span <= 0 {
        gross
    } else {
        gross * (span - suspended).max(0) / span
    }
}

/// What replaying the paid periods under `after` instead of `before` comes
/// to: paid too much, and paid too little.
pub(crate) fn replay(before: &Schedule, after: &Schedule) -> (i64, i64) {
    let paid = || before.periods.iter().filter(|(_, p)| !p.returned);
    let change = |start: &NaiveDate, p: &Period| period_gross(before, *start, p.end) - period_gross(after, *start, p.end);
    (
        paid().map(|(s, p)| change(s, p).max(0)).sum(),
        paid().map(|(s, p)| (-change(s, p)).max(0)).sum(),
    )
}

/// The other income taken off one period's benefit: the full benefit for
/// the period less what is paid for it.
pub(crate) fn period_income(s: &Schedule, start: NaiveDate, end: NaiveDate) -> i64 {
    // Days in custody are no benefit at all, not other income: this is the
    // part of the benefit the declared income takes, before any suspension.
    let income = s.income.range(..=start).next_back().map_or(0, |(_, v)| *v);
    let prorate = |monthly: i64| {
        let full = plus_months(start, 1);
        if end >= full {
            monthly
        } else {
            monthly * (end - start).num_days() / (full - start).num_days().max(1)
        }
    };
    (prorate(s.monthly_minor) - prorate((s.monthly_minor - income).max(0))).max(0)
}

/// What was paid for days on or after `end`: the replay of the paid periods
/// under the corrected end date.
pub(crate) fn overpaid_after(s: &Schedule, end: NaiveDate) -> i64 {
    // Arrears paid with a period were owed for earlier ones: never overpaid.
    let after = |start: &NaiveDate, p: &Period| {
        let benefit = p.gross_minor - p.arrears_minor;
        if *start >= end {
            benefit
        } else {
            benefit * (p.end - end).num_days() / (p.end - *start).num_days().max(1)
        }
    };
    s.periods
        .iter()
        .filter(|(_, p)| !p.returned && p.end > end)
        .map(|(start, p)| after(start, p))
        .sum()
}

/// A payment held for a reason the person releasing it is not told.
fn held_payment() -> Reject {
    Reject::new("payment_held", "payment on this claim is held; ask a claims manager")
}

/// The key a provider's debts are held under among offset sources, apart
/// from any person's.
fn provider_key(provider: &str) -> String {
    format!("provider:{provider}")
}

/// An overpayment being recovered by offset, and how much of it is left.
struct OffsetSource {
    claim: String,
    policy: String,
    overpayment: String,
    left_minor: i64,
    pct: u32,
    /// Held on another partition.
    partition: Option<u16>,
}

impl ClaimsState {
    /// Admission of a benefit in payment: the schedule and its review plan.
    /// The reviews are due dates, so nobody has to remember them.
    pub(crate) fn start_benefit(&self, claim: &mut Claim, cx: &Cx<'_>, terms: &Periodic) {
        let mut reviews = BTreeMap::new();
        let plan = [
            (TaskKind::MedicalReview, self.rules.medical_review_months),
            (TaskKind::IncomeDeclaration, self.rules.income_declaration_months),
        ];
        let proof = (self.rules.proof_of_life_months > 0).then_some((TaskKind::ProofOfLife, self.rules.proof_of_life_months));
        for (kind, months) in plan.into_iter().chain(proof) {
            let on = plus_months(terms.first_period, months.max(1));
            if on < terms.end {
                reviews.insert(kind, on);
                claim.due.insert((on, Due::Review(kind)));
            }
        }
        claim.due.insert((terms.expected_end, Due::ExpectedEnd));
        claim.schedule = Some(Schedule {
            monthly_minor: terms.monthly_minor,
            next_period: terms.first_period,
            end: terms.end,
            expected_end: terms.expected_end,
            income: BTreeMap::new(),
            periods: BTreeMap::new(),
            reviews,
            reviews_overdue: 0,
            case_manager: claim.owner.clone(),
            benefit_end: terms.end,
            ended: None,
            arrears_minor: 0,
            suspended: BTreeMap::new(),
        });
        claim.status = Status::InPayment;
        if claim.queue == crate::Queue::IncomeProtectionNew {
            claim.queue = crate::Queue::IncomeProtectionOngoing;
        }
        if claim.claim_type == crate::ClaimType::PremiumWaiver {
            // Nothing is paid out, so there is no payment to time. The
            // waiver itself starts with the day's work.
            claim.waiver_from = Some(terms.first_period);
            claim.due.insert((cx.today, Due::Waiver));
            return;
        }
        // Time from the end of the waiting period to the first payment.
        self.start_timer(claim, TimerKind::Payment, terms.first_period.max(cx.today));
    }

    /// Start waiving premiums beside a benefit in payment. The owner says
    /// from when; it ends when the benefit does.
    pub(crate) fn start_waiver(&mut self, cx: &mut Cx<'_>, id: &str, from: NaiveDate, actor: &Actor) -> Result<(), Reject> {
        let mut claim = self.claim(id)?;
        self.owner(actor, &claim)?;
        let running = claim.status == Status::InPayment && claim.schedule.as_ref().is_some_and(|s| s.ended.is_none() && from < s.end);
        if !running || from < claim.event_date {
            return Err(Reject::new(
                "not_in_payment",
                "premiums are waived beside a benefit in payment, from a date within it",
            ));
        }
        if claim.waiver_from.is_some() {
            return Err(Reject::new("waiver_exists", "premiums are already being waived on this claim"));
        }
        claim.waiver_from = Some(from);
        self.waive(cx, &claim)?;
        claim.due.insert((plus_days(cx.today, 1), Due::Waiver));
        note(&mut claim, cx, Some(actor), format!("waiver_started:{from}"));
        cx.notify("premium_waiver_started", id, &[("policy", claim.policy.clone()), ("from", from.to_string())]);
        self.commit(claim);
        Ok(())
    }

    /// Ask the core to pay from claims expense whatever premium is owing
    /// for periods in the waiver's window.
    fn waive(&self, cx: &mut Cx<'_>, claim: &Claim) -> Result<(), Reject> {
        let (Some(from), Some(s)) = (claim.waiver_from, claim.schedule.as_ref()) else {
            return Ok(());
        };
        cx.core.execute(Instruction::WaivePremium {
            claim: claim.id.clone(),
            policy: claim.policy.clone(),
            cover: claim.cover.clone(),
            from,
            until: s.end,
        })
    }

    /// One day of a waiver: waive what has been billed, and look again
    /// tomorrow while the benefit runs. A claim that is only a waiver has no
    /// payment run to end it, so it ends here when its last day has passed.
    pub(crate) fn waiver_day(&self, cx: &mut Cx<'_>, claim: &mut Claim) {
        if claim.status != Status::InPayment || claim.waiver_from.is_none() {
            return;
        }
        let tomorrow = plus_days(cx.today, 1);
        // A refusal is tried again tomorrow: the premium stays owing meanwhile.
        if self.waive(cx, claim).is_err() {
            note(claim, cx, None, "waiver_refused");
        }
        let only_waiver = claim.claim_type == crate::ClaimType::PremiumWaiver;
        let Some(s) = claim.schedule.as_mut() else { return };
        if only_waiver {
            // The days before today are done; the claim ends on the first day outside the waiver.
            s.next_period = cx.today.min(s.end).max(s.next_period);
            if s.next_period >= s.end {
                s.ended.get_or_insert(EndReason::BenefitPeriodEnd);
                if self.close(cx, claim, Status::Ended, None).is_ok() {
                    return;
                }
            }
        }
        claim.due.insert((tomorrow, Due::Waiver));
    }

    /// End a benefit. If nothing is left to pay the claim closes, and what
    /// was paid past the end is raised as an overpayment in the same step.
    pub(crate) fn end_benefit(
        &self,
        cx: &mut Cx<'_>,
        claim: &mut Claim,
        end: NaiveDate,
        reason: EndReason,
        overpaid_minor: i64,
        cause: OverpaymentCause,
    ) -> Result<(), Reject> {
        let finished = claim.schedule.as_ref().is_some_and(|s| end <= s.next_period);
        if finished {
            let overpayment = Self::overpayment(claim, cx, overpaid_minor, cause, true);
            self.close(cx, claim, Status::Ended, overpayment)?;
        }
        if let Some(s) = claim.schedule.as_mut() {
            s.end = end;
            s.ended = Some(reason);
        }
        Ok(())
    }

    /// A person has died. Every benefit in payment to them ends the day
    /// after, and what was paid past it is an overpayment to recover from
    /// the estate. A claim that cannot be ended here goes to a senior.
    pub(crate) fn record_death(&mut self, cx: &mut Cx<'_>, party: &str, date_of_death: NaiveDate) -> Result<(), Reject> {
        if date_of_death > cx.today {
            return Err(Reject::new("event_in_future", "the date of death is after today"));
        }
        let end = plus_days(date_of_death, 1);
        let late = (cx.today - date_of_death).num_days() > i64::from(self.rules.late_death_days);
        let ids: Vec<String> = self.by_party.get(party).into_iter().flatten().cloned().collect();
        for id in ids {
            let Some(mut claim) = self.claims.get(&id).filter(|c| c.status == Status::InPayment).cloned() else {
                continue;
            };
            let Some(overpaid) = claim.schedule.as_ref().filter(|s| end <= s.end).map(|s| overpaid_after(s, end)) else {
                continue;
            };
            cx.core.work(1);
            let decision = self.decision(&claim, cx, DecisionKind::EndBenefit, overpaid, vec!["death_notified".into()], vec![], None);
            let ended = match claim.in_run {
                Some(_) => Err(Reject::new("in_run", "the claim is in a payment run")),
                None => self.end_benefit(cx, &mut claim, end, EndReason::Death, overpaid, OverpaymentCause::LateDeathNotification),
            };
            match ended {
                Ok(()) => {
                    note(&mut claim, cx, None, format!("decided:EndBenefit:{}", decision.no));
                    claim.decisions.push(decision);
                }
                Err(e) => {
                    open_task(&mut claim, cx, "senior-review/death", TaskKind::SeniorReview, None);
                    note(&mut claim, cx, None, format!("death_not_applied:{}", e.code));
                }
            }
            if late && open_task(&mut claim, cx, "death-register-cadence", TaskKind::DeathRegisterCadence, None) {
                cx.notify("death_register_cadence_check", &id, &[]);
            }
            self.commit(claim);
        }
        Ok(())
    }

    /// A death recorded in error. A benefit it was going to end and that is
    /// still running carries on as if nothing happened. A claim it closed
    /// goes to a senior, who reopens it on the basis `DeathRetracted` and
    /// decides what to do with the overpayment it raised: the money moved,
    /// so a person decides.
    pub(crate) fn retract_death(&mut self, cx: &mut Cx<'_>, party: &str, date_of_death: NaiveDate) -> Result<(), Reject> {
        let end = plus_days(date_of_death, 1);
        let ids: Vec<String> = self.by_party.get(party).into_iter().flatten().cloned().collect();
        for id in ids {
            let Some(mut claim) = self.claims.get(&id).cloned() else { continue };
            let by_this_death = claim.schedule.as_ref().is_some_and(|s| s.ended == Some(EndReason::Death) && s.end == end);
            if !by_this_death {
                continue;
            }
            cx.core.work(1);
            if claim.status == Status::InPayment {
                if let Some(s) = claim.schedule.as_mut() {
                    s.ended = None;
                    s.end = s.benefit_end;
                }
                note(&mut claim, cx, None, format!("death_retracted:{date_of_death}"));
                cx.notify("death_retracted", &id, &[("benefit", "restored".into())]);
            } else {
                open_task(&mut claim, cx, "senior-review/death-retracted", TaskKind::SeniorReview, None);
                note(&mut claim, cx, None, format!("death_retracted:{date_of_death}"));
                cx.notify("death_retracted", &id, &[("benefit", "to_review".into())]);
            }
            self.commit(claim);
        }
        Ok(())
    }

    /// Record who is paid, once identity, account ownership and screening
    /// have passed. An estate needs proof of legal authority above the
    /// simplified limit.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn verify_payee(
        &mut self,
        cx: &mut Cx<'_>,
        id: &str,
        party: String,
        kind: PayeeKind,
        account_ref: String,
        name_check_ref: String,
        screening_ref: String,
        authority_doc: Option<String>,
        actor: &Actor,
    ) -> Result<(), Reject> {
        let mut claim = self.claim(id)?;
        self.owner(actor, &claim)?;
        if [&party, &account_ref, &name_check_ref, &screening_ref].iter().any(|s| s.is_empty()) {
            return Err(Reject::new(
                "payee_checks",
                "a payee needs an account, a passed name check and a passed screening",
            ));
        }
        let amount = claim.claimed_minor.max(claim.payable_minor);
        if kind == PayeeKind::Estate && authority_doc.is_none() && amount > self.rules.estate_simplified_max_minor {
            return Err(Reject::new(
                "authority_document_required",
                "an estate above the simplified limit needs proof of legal authority",
            ));
        }
        claim.payee = Some(Payee {
            party,
            kind,
            account_ref,
            name_check_ref,
            screening_ref,
            authority_doc,
            verified_on: cx.today,
            verified_by: actor.staff.clone(),
        });
        note(&mut claim, cx, Some(actor), "payee_verified");
        self.commit(claim);
        Ok(())
    }

    /// Release a payment from what has been admitted. The money stays in
    /// benefit payable until the payments module settles it at the bank.
    pub(crate) fn pay(
        &mut self,
        cx: &mut Cx<'_>,
        id: &str,
        payment_ref: String,
        amount_minor: i64,
        reissue_of: Option<String>,
        actor: &Actor,
    ) -> Result<(), Reject> {
        let mut claim = self.claim(id)?;
        self.owner(actor, &claim)?;
        let profile = self.authority.profile(&actor.staff)?;
        if !profile.lines.contains(&claim.line()) || amount_minor > profile.max_payment_minor {
            return Err(Reject::new(
                "authority",
                "your authority profile does not cover a payment of this size on this line",
            ));
        }
        if claim.investigation.is_some() {
            return Err(held_payment());
        }
        let Some(payee) = claim.payee.as_ref().map(|p| p.party.clone()) else {
            return Err(Reject::new("payee_not_verified", "verify the payee before the first payment"));
        };
        if amount_minor <= 0 || amount_minor > claim.benefit_payable_minor() || claim.in_run.is_some() {
            return Err(Reject::new(
                "exceeds_payable",
                "the amount is not within what is admitted and unpaid, or the claim is in a run",
            ));
        }
        if amount_minor > claim.benefit_payable_minor() - claim.interests_unpaid_minor() {
            return Err(Reject::new(
                "interest_first",
                "a lender or assignee with a recorded interest is paid before the claimant",
            ));
        }
        let used = claim.payments.iter().any(|p| p.payment_ref == payment_ref)
            || claim.schedule.iter().flat_map(|s| s.periods.values()).any(|p| p.payment_ref == payment_ref);
        if used {
            return Err(Reject::new("payment_exists", "that payment reference has been used on this claim"));
        }
        // A returned benefit period is paid again as that period, in full.
        let period = reissue_of.as_ref().and_then(|r| {
            claim
                .schedule
                .as_ref()?
                .periods
                .iter()
                .find(|(_, p)| p.returned && &p.payment_ref == r)
                .map(|(k, p)| (*k, p.gross_minor - p.offset_minor))
        });
        if period.is_some_and(|(_, net)| net != amount_minor) {
            return Err(Reject::new(
                "reissue_amount",
                "a returned benefit period is paid again for exactly what came back",
            ));
        }
        let close = claim.payable_minor == amount_minor && claim.cost_payable_minor == 0 && claim.status != Status::InPayment;
        // A payment that came back is sent again as it was: its interest is
        // part of what came back, and the delay since is not the insurer's.
        let came_back = |p: &&Payment| p.returned && p.cost_item.is_none() && !p.ex_gratia;
        let carried = reissue_of
            .as_ref()
            .and_then(|r| claim.payments.iter().filter(came_back).find(|p| &p.payment_ref == r))
            .filter(|p| p.amount_minor + p.interest_minor - p.interest_declined_minor == amount_minor)
            .map_or(0, |p| p.interest_minor - p.interest_declined_minor);
        let fresh = match (self.rules.late_interest, claim.admission()) {
            (Some(rate), Some(admission)) if period.is_none() && claim.schedule.is_none() && !claim.payments.iter().any(|p| came_back(&p)) => {
                rate.on(amount_minor, admission.decided_on, cx.today)
            }
            _ => 0,
        };
        // Interest a payee declined and then asked for after all goes with this payment.
        let owed_again = std::mem::take(&mut claim.interest_owed_again_minor);
        let interest_minor = fresh + owed_again;
        // A payee who was told and declined: worked out, posted, and reversed.
        let declined = if claim.interest_declined.is_some() { fresh } else { 0 };
        cx.core.execute(Instruction::Pay {
            claim: id.to_string(),
            policy: claim.policy.clone(),
            payment_ref: payment_ref.clone(),
            amount_minor,
            payee: payee.clone(),
            close,
            interest_minor,
            interest_declined_minor: declined,
            offset: None,
        })?;
        claim.payable_minor -= amount_minor;
        let mut period_end = None;
        match (period, claim.schedule.as_mut()) {
            (Some((start, _)), Some(s)) => {
                if let Some(p) = s.periods.get_mut(&start) {
                    p.returned = false;
                    p.payment_ref = payment_ref.clone();
                    period_end = Some(p.end);
                }
            }
            _ => claim.payments.push(Payment {
                payment_ref: payment_ref.clone(),
                amount_minor: amount_minor - carried,
                paid_on: cx.today,
                ex_gratia: false,
                returned: false,
                released_by: Some(actor.staff.clone()),
                run: None,
                cost_item: None,
                interest_minor: interest_minor + carried,
                interest_declined_minor: declined,
                interested_party: None,
            }),
        }
        Self::meet_timer(&mut claim, TimerKind::Payment, cx.today);
        note(&mut claim, cx, Some(actor), format!("paid:{payment_ref}"));
        cx.paid(
            &claim,
            &payment_ref,
            amount_minor - carried,
            amount_minor + interest_minor - declined,
            &payee,
            period_end,
            false,
            interest_minor + carried - declined,
        );
        if close && claim.status == Status::Admitted {
            claim.status = Status::Paid;
            claim.closed_on = Some(cx.today);
            claim.due.clear();
            cx.notify("claim_closed", id, &[("outcome", "paid".into())]);
        }
        self.commit(claim);
        Ok(())
    }

    /// Pay an interested party what is left of their share, in one payment.
    /// A higher priority is paid first. The payment closes the claim where
    /// it is the last money owed, as a payment to the claimant would.
    pub(crate) fn pay_interest(&mut self, cx: &mut Cx<'_>, id: &str, payment_ref: String, party: &str, actor: &Actor) -> Result<(), Reject> {
        let mut claim = self.claim(id)?;
        self.owner(actor, &claim)?;
        if claim.investigation.is_some() {
            return Err(held_payment());
        }
        let Some(at) = claim.interests.iter().position(|i| i.party == party && i.owed_minor > i.paid_minor) else {
            return Err(Reject::new("no_interest", "that party has no unpaid interest in this claim"));
        };
        let (priority, amount_minor) = (claim.interests[at].priority, claim.interests[at].owed_minor - claim.interests[at].paid_minor);
        if claim.interests.iter().any(|i| i.priority < priority && i.owed_minor > i.paid_minor) {
            return Err(Reject::new("interest_priority", "an interest with a higher priority is paid first"));
        }
        let profile = self.authority.profile(&actor.staff)?;
        if !profile.lines.contains(&claim.line()) || amount_minor > profile.max_payment_minor {
            return Err(Reject::new(
                "authority",
                "your authority profile does not cover a payment of this size on this line",
            ));
        }
        if amount_minor > claim.benefit_payable_minor() || claim.in_run.is_some() {
            return Err(Reject::new("exceeds_payable", "the interest is more than is admitted and unpaid"));
        }
        if claim.payments.iter().any(|p| p.payment_ref == payment_ref) {
            return Err(Reject::new("payment_exists", "that payment reference has been used on this claim"));
        }
        let close = claim.payable_minor == amount_minor && claim.status == Status::Admitted;
        cx.core.execute(Instruction::Pay {
            claim: id.to_string(),
            policy: claim.policy.clone(),
            payment_ref: payment_ref.clone(),
            amount_minor,
            payee: party.to_string(),
            close,
            interest_minor: 0,
            interest_declined_minor: 0,
            offset: None,
        })?;
        claim.payable_minor -= amount_minor;
        claim.interests[at].paid_minor += amount_minor;
        claim.payments.push(Payment {
            payment_ref: payment_ref.clone(),
            amount_minor,
            paid_on: cx.today,
            ex_gratia: false,
            returned: false,
            released_by: Some(actor.staff.clone()),
            run: None,
            cost_item: None,
            interest_minor: 0,
            interest_declined_minor: 0,
            interested_party: Some(party.to_string()),
        });
        Self::meet_timer(&mut claim, TimerKind::Payment, cx.today);
        note(&mut claim, cx, Some(actor), format!("interest_paid:{payment_ref}"));
        cx.paid(&claim, &payment_ref, amount_minor, amount_minor, party, None, false, 0);
        if close {
            claim.status = Status::Paid;
            claim.closed_on = Some(cx.today);
            claim.due.clear();
            cx.notify("claim_closed", id, &[("outcome", "paid".into())]);
        }
        self.commit(claim);
        Ok(())
    }

    /// Pay what a piece of evidence or a rehabilitation task cost to the
    /// provider. The amount is the cost already recorded on the item, so
    /// nothing is paid that was not first recorded, and an item is paid once.
    pub(crate) fn pay_cost(&mut self, cx: &mut Cx<'_>, id: &str, payment_ref: String, item: &str, provider: &str, actor: &Actor) -> Result<(), Reject> {
        let mut claim = self.claim(id)?;
        self.owner(actor, &claim)?;
        let Some(amount_minor) = claim.cost_of(item).filter(|_| !provider.is_empty()) else {
            return Err(Reject::new(
                "no_cost",
                "there is no received evidence or completed rehabilitation task with a cost under that ID",
            ));
        };
        if amount_minor > self.authority.profile(&actor.staff)?.max_payment_minor {
            return Err(Reject::new("authority", "your authority profile does not cover a payment of this size"));
        }
        let mine = |p: &&Payment| p.cost_item.as_deref() == Some(item);
        if claim.payments.iter().filter(mine).any(|p| !p.returned) {
            return Err(Reject::new("cost_paid", "that cost has been paid"));
        }
        let used = claim.payments.iter().any(|p| p.payment_ref == payment_ref)
            || claim.schedule.iter().flat_map(|s| s.periods.values()).any(|p| p.payment_ref == payment_ref);
        if used {
            return Err(Reject::new("payment_exists", "that payment reference has been used on this claim"));
        }
        // A payment the bank sent back left the money in benefit payable:
        // it is released again. Otherwise the cost is expensed now.
        let reissue = claim.payments.iter().any(|p| mine(&p)) && claim.cost_payable_minor >= amount_minor;
        // Where the returned cost was the last money owed on the claim,
        // paying it again closes the claim's ledger, as the last benefit payment does.
        let close = reissue && claim.payable_minor == amount_minor && (claim.closed_on.is_some() || claim.status == Status::Admitted);
        let kind = if claim.evidence.iter().any(|e| e.id == item) {
            "evidence"
        } else {
            "rehabilitation"
        };
        cx.core.execute(if reissue {
            Instruction::Pay {
                claim: id.to_string(),
                policy: claim.policy.clone(),
                payment_ref: payment_ref.clone(),
                amount_minor,
                payee: provider.to_string(),
                close,
                interest_minor: 0,
                interest_declined_minor: 0,
                offset: None,
            }
        } else {
            Instruction::HandlingCost {
                claim: id.to_string(),
                policy: claim.policy.clone(),
                cover: claim.cover.clone(),
                payment_ref: payment_ref.clone(),
                amount_minor,
                payee: provider.to_string(),
                kind: kind.to_string(),
            }
        })?;
        if reissue {
            claim.payable_minor -= amount_minor;
            claim.cost_payable_minor -= amount_minor;
        }
        if close && claim.status == Status::Admitted {
            claim.status = Status::Paid;
            claim.closed_on = Some(cx.today);
            claim.due.clear();
            cx.notify("claim_closed", id, &[("outcome", "paid".into())]);
        }
        claim.payments.push(Payment {
            payment_ref: payment_ref.clone(),
            amount_minor,
            paid_on: cx.today,
            ex_gratia: false,
            returned: false,
            released_by: Some(actor.staff.clone()),
            run: None,
            cost_item: Some(item.to_string()),
            interest_minor: 0,
            interest_declined_minor: 0,
            interested_party: None,
        });
        note(&mut claim, cx, Some(actor), format!("cost_paid:{item}"));
        // The payments module's instruction, apart from `claim_paid` so no
        // reader takes a handling cost for a benefit.
        cx.notify(
            "claim_cost_paid",
            id,
            &[
                ("policy", claim.policy.clone()),
                ("payment_ref", payment_ref),
                ("net_minor", amount_minor.to_string()),
                ("payee", provider.to_string()),
                ("kind", kind.to_string()),
                ("item", item.to_string()),
            ],
        );
        self.commit(claim);
        Ok(())
    }

    /// A payment failed or came back. The money returns to benefit payable,
    /// never to suspense, and a task is raised to pay it again.
    pub(crate) fn payment_returned(&mut self, cx: &mut Cx<'_>, id: &str, payment_ref: &str) -> Result<(), Reject> {
        let mut claim = self.claim(id)?;
        let lump = claim.payments.iter().position(|p| p.payment_ref == payment_ref && !p.returned);
        let period = claim
            .schedule
            .as_ref()
            .and_then(|s| s.periods.iter().find(|(_, p)| p.payment_ref == payment_ref && !p.returned).map(|(k, _)| *k));
        if lump.is_none() && period.is_none() {
            return Err(Reject::new("unknown_payment", format!("claim {id} has no payment {payment_ref} outstanding")));
        }
        cx.core.execute(Instruction::PaymentReturned {
            claim: id.to_string(),
            policy: claim.policy.clone(),
            payment_ref: payment_ref.to_string(),
        })?;
        let mut came_back = (0, None);
        let mut cost = 0;
        let mut kind = (false, 0);
        if let Some(p) = lump.and_then(|i| claim.payments.get_mut(i)) {
            p.returned = true;
            came_back = (p.amount_minor + p.interest_minor - p.interest_declined_minor, p.run.clone());
            kind = (p.ex_gratia, p.interest_minor - p.interest_declined_minor);
            cost = p.cost_item.as_ref().map_or(0, |_| p.amount_minor);
            // An interested party's payment that came back is owed to them again.
            let (party, amount) = (p.interested_party.clone(), p.amount_minor);
            if let Some(i) = claim.interests.iter_mut().find(|i| Some(&i.party) == party.as_ref()) {
                i.paid_minor -= amount;
            }
        }
        claim.cost_payable_minor += cost;
        if let Some(p) = period.and_then(|k| claim.schedule.as_mut()?.periods.get_mut(&k)) {
            p.returned = true;
            came_back = (p.gross_minor - p.offset_minor, Some(p.run.clone()));
        }
        let (amount, run) = came_back;
        claim.payable_minor += amount;
        if let Some(r) = run.and_then(|r| self.runs.get_mut(&r)) {
            r.returned_minor += amount;
        }
        // A returned handling cost is owed to a provider: the claimant's claim stays as it was.
        if claim.status == Status::Paid && cost == 0 {
            claim.status = Status::Admitted;
            claim.closed_on = None;
        }
        open_task(&mut claim, cx, format!("payment-failure/{payment_ref}"), TaskKind::PaymentFailure, None);
        note(&mut claim, cx, None, format!("payment_returned:{payment_ref}"));
        cx.notify(
            "payment_returned",
            id,
            &[
                ("payment_ref", payment_ref.to_string()),
                ("amount_minor", amount.to_string()),
                // What kind of money came back, for readers that keep them apart.
                ("ex_gratia", kind.0.to_string()),
                ("cost", (cost != 0).to_string()),
                ("interest_minor", kind.1.to_string()),
            ],
        );
        self.commit(claim);
        Ok(())
    }

    /// Other income declared for a benefit in payment. Periods already paid
    /// are replayed under the new facts; what was paid too much becomes an
    /// overpayment, and a shortfall goes to a senior to put right.
    pub(crate) fn declare_income(&mut self, cx: &mut Cx<'_>, id: &str, from: NaiveDate, other_income_minor: i64, actor: &Actor) -> Result<(), Reject> {
        let mut claim = self.claim(id)?;
        self.owner(actor, &claim)?;
        let Some(before) = claim
            .schedule
            .clone()
            .filter(|_| claim.status == Status::InPayment && claim.in_run.is_none() && other_income_minor >= 0)
        else {
            return Err(Reject::new(
                "not_in_payment",
                "income is declared on a benefit in payment that is not waiting in a run",
            ));
        };
        let mut after = before.clone();
        after.income.insert(from, other_income_minor);
        let paid = || before.periods.iter().filter(|(_, p)| !p.returned);
        let change = |start: &NaiveDate, p: &Period| period_gross(&before, *start, p.end) - period_gross(&after, *start, p.end);
        let over: i64 = paid().map(|(s, p)| change(s, p).max(0)).sum();
        let under: i64 = paid().map(|(s, p)| (-change(s, p)).max(0)).sum();
        if let Some(o) = Self::overpayment(&claim, cx, over, OverpaymentCause::LateInformation, true) {
            cx.core.execute(Instruction::RaiseOverpayment {
                claim: id.to_string(),
                policy: claim.policy.clone(),
                cover: claim.cover.clone(),
                overpayment: o.id.clone(),
                amount_minor: over,
            })?;
            crate::overpay::publish(cx, "overpayment_raised", &claim, &o, &[("amount_minor", over.to_string())]);
            claim.overpayments.push(o);
        }
        if under > 0 {
            // What was paid too little is owed. It goes out with the next
            // benefit, in a run one person prepares and another approves. A
            // senior looks at it where no further benefit is due, so no run
            // will carry it, or where the same person also owes an
            // overpayment, so that paying one while holding the other is a
            // decision and not an accident.
            after.arrears_minor += under;
            let owes = claim.overpayments.iter().any(|o| o.outstanding_minor() > 0);
            if after.next_period >= after.end || owes {
                open_task(&mut claim, cx, format!("senior-review/underpaid/{from}"), TaskKind::SeniorReview, None);
            }
            cx.notify("underpayment_owed", id, &[("amount_minor", under.to_string())]);
        }
        claim.schedule = Some(after);
        note(&mut claim, cx, Some(actor), format!("income_declared:{from}"));
        self.commit(claim);
        Ok(())
    }

    /// Overpayments being recovered by offset, by who owes them: the life
    /// insured, or for a provider's billing error the provider, under
    /// [`provider_key`].
    fn offset_sources(&self) -> BTreeMap<String, Vec<OffsetSource>> {
        let mut by_party: BTreeMap<String, Vec<OffsetSource>> = BTreeMap::new();
        for c in self.offsetting.iter().filter_map(|id| self.claims.get(id)) {
            let provider = c.health.as_ref().and_then(|h| h.provider.as_deref()).map(provider_key);
            for o in c
                .overpayments
                .iter()
                .filter(|o| o.status == RecoveryStatus::Offset && o.outstanding_minor() > 0)
            {
                let owner = if o.cause == crate::model::OverpaymentCause::ProviderBilling {
                    provider.clone()
                } else {
                    c.life_party.clone()
                };
                let Some(party) = owner else { continue };
                by_party.entry(party).or_default().push(OffsetSource {
                    claim: c.id.clone(),
                    policy: c.policy.clone(),
                    overpayment: o.id.clone(),
                    left_minor: o.outstanding_minor(),
                    // The cap protects a person's income, not a provider's billing.
                    pct: if o.cause == crate::model::OverpaymentCause::ProviderBilling {
                        o.offset_pct.min(100)
                    } else {
                        o.offset_pct.min(self.rules.max_offset_pct)
                    },
                    partition: None,
                });
            }
        }
        // Then what this person owes on other partitions, after what is owed here.
        let parties: Vec<String> = self.remote_offsets.values().map(|r| r.party.clone()).collect();
        for party in parties.into_iter().collect::<BTreeSet<_>>() {
            let remote: Vec<OffsetSource> = self
                .remote_sources(&party)
                .map(|r| OffsetSource {
                    claim: r.claim.clone(),
                    policy: r.policy.clone(),
                    overpayment: r.overpayment.clone(),
                    left_minor: r.available_minor(),
                    pct: r.pct.min(self.rules.max_offset_pct),
                    partition: Some(r.from),
                })
                .collect();
            if !remote.is_empty() {
                by_party.entry(party).or_default().extend(remote);
            }
        }
        by_party
    }

    /// Prepare a run: the payments, a count and a control total, for a
    /// second person to approve.
    pub(crate) fn prepare_run(&mut self, cx: &mut Cx<'_>, run: String, kind: RunKind, benefit_date: NaiveDate, actor: &Actor) -> Result<(), Reject> {
        self.configured()?;
        self.staff(actor)?;
        if run.is_empty() || self.runs.contains_key(&run) {
            return Err(Reject::new("run_exists", format!("run {run} already exists")));
        }
        let mut items: Vec<RunItem> = vec![];
        let mut unpayable: Vec<String> = vec![];
        match kind {
            RunKind::Benefit => {
                let mut sources = self.offset_sources();
                // A claim in a restricted case waits without anyone being told why.
                for c in self
                    .in_payment
                    .iter()
                    .filter_map(|id| self.claims.get(id))
                    .filter(|c| c.in_run.is_none() && c.investigation.is_none())
                {
                    cx.core.work(1);
                    let Some(s) = c.schedule.as_ref() else { continue };
                    // A waiver pays nobody; an overdue proof of life holds the money where it is.
                    if c.claim_type == crate::ClaimType::PremiumWaiver || c.held_for_proof_of_life() {
                        continue;
                    }
                    let Some(payee) = c.payee.as_ref() else {
                        if s.next_period <= benefit_date && s.next_period < s.end {
                            unpayable.push(c.id.clone());
                        }
                        continue;
                    };
                    let mut start = s.next_period;
                    // A shortfall on earlier periods rides the first period paid.
                    let mut arrears = s.arrears_minor;
                    while start <= benefit_date && start < s.end && items.len() < MAX_RUN_ITEMS {
                        let end = plus_months(start, 1).min(s.end);
                        let benefit = period_gross(s, start, end);
                        let gross = benefit + std::mem::take(&mut arrears);
                        let source = c
                            .life_party
                            .as_ref()
                            .and_then(|p| sources.get_mut(p))
                            .and_then(|v| v.iter_mut().find(|o| o.left_minor > 0));
                        let offset = source.and_then(|o| {
                            let take = (benefit * i64::from(o.pct) / 100).min(o.left_minor);
                            o.left_minor -= take;
                            (take > 0).then(|| Offset {
                                claim: o.claim.clone(),
                                policy: o.policy.clone(),
                                overpayment: o.overpayment.clone(),
                                amount_minor: take,
                                partition: o.partition,
                            })
                        });
                        items.push(RunItem {
                            claim: c.id.clone(),
                            period_start: start,
                            period_end: end,
                            gross_minor: gross,
                            offset,
                            payee: payee.party.clone(),
                        });
                        start = end;
                    }
                }
            }
            RunKind::Provider => {
                let mut sources = self.offset_sources();
                for c in self
                    .provider_payable
                    .iter()
                    .filter_map(|id| self.claims.get(id))
                    .filter(|c| c.investigation.is_none())
                    .take(MAX_RUN_ITEMS)
                {
                    cx.core.work(1);
                    let provider = c.health.as_ref().and_then(|h| h.provider.clone());
                    let gross = c.benefit_payable_minor();
                    // A provider's billing error comes back out of what it is paid next.
                    let offset = provider
                        .as_deref()
                        .and_then(|p| sources.get_mut(&provider_key(p)))
                        .and_then(|v| v.iter_mut().find(|o| o.left_minor > 0 && o.claim != c.id))
                        .and_then(|o| {
                            let take = (gross * i64::from(o.pct) / 100).min(o.left_minor);
                            o.left_minor -= take;
                            (take > 0).then(|| Offset {
                                claim: o.claim.clone(),
                                policy: o.policy.clone(),
                                overpayment: o.overpayment.clone(),
                                amount_minor: take,
                                partition: o.partition,
                            })
                        });
                    items.push(RunItem {
                        claim: c.id.clone(),
                        period_start: benefit_date,
                        period_end: benefit_date,
                        gross_minor: gross,
                        offset,
                        payee: provider.unwrap_or_else(|| c.policyholder.clone()),
                    });
                }
            }
        }
        if items.is_empty() && unpayable.is_empty() {
            return Err(Reject::new("nothing_to_pay", "no payment is due for this run"));
        }
        // A benefit that is due and has nobody verified to pay is never
        // skipped in silence.
        for id in unpayable {
            if let Ok(mut claim) = self.claim(&id) {
                open_task(
                    &mut claim,
                    cx,
                    format!("payment-failure/no-payee/{benefit_date}"),
                    TaskKind::PaymentFailure,
                    None,
                );
                self.commit(claim);
            }
        }
        if items.is_empty() {
            // Nothing could be paid, and the tasks say why: no run is made.
            cx.notify("run_prepared", &run, &[("count", "0".into()), ("control_total_minor", "0".into())]);
            return Ok(());
        }
        let claims: BTreeSet<String> = items.iter().map(|i| i.claim.clone()).collect();
        for id in claims {
            if let Ok(mut claim) = self.claim(&id) {
                claim.in_run = Some(run.clone());
                self.commit(claim);
            }
        }
        let prepared = Run {
            id: run.clone(),
            kind,
            benefit_date,
            control_total_minor: items.iter().map(RunItem::net_minor).sum(),
            count: items.len() as u32,
            items,
            prepared_by: actor.staff.clone(),
            approved_by: None,
            status: RunStatus::Prepared,
            submitted_minor: 0,
            exceptions: vec![],
            returned_minor: 0,
            settled_minor: None,
        };
        cx.notify(
            "run_prepared",
            &run,
            &[
                ("count", prepared.count.to_string()),
                ("control_total_minor", prepared.control_total_minor.to_string()),
            ],
        );
        self.runs.insert(run, prepared);
        Ok(())
    }

    /// Post one payment of a run. On a refusal nothing about the claim changes.
    fn pay_item(&mut self, cx: &mut Cx<'_>, run: &Run, item: &RunItem, claim: &mut Claim) -> Result<(), Reject> {
        let payment_ref = item.payment_ref(&run.id);
        let stale = || Reject::new("stale_item", "the claim has moved since the run was prepared");
        let mut closed = false;
        if run.kind == RunKind::Provider {
            if claim.status != Status::Admitted || claim.payable_minor != item.gross_minor || claim.cost_payable_minor != 0 {
                return Err(stale());
            }
            cx.core.execute(Instruction::Pay {
                claim: claim.id.clone(),
                policy: claim.policy.clone(),
                payment_ref: payment_ref.clone(),
                amount_minor: item.gross_minor,
                payee: item.payee.clone(),
                close: true,
                interest_minor: 0,
                interest_declined_minor: 0,
                offset: item.offset.clone(),
            })?;
            if let Some(o) = &item.offset {
                self.take_offset(claim, o);
            }
            claim.payments.push(Payment {
                payment_ref: payment_ref.clone(),
                amount_minor: item.gross_minor,
                paid_on: cx.today,
                ex_gratia: false,
                returned: false,
                released_by: run.approved_by.clone(),
                run: Some(run.id.clone()),
                cost_item: None,
                interest_minor: 0,
                interest_declined_minor: 0,
                interested_party: None,
            });
            claim.payable_minor = 0;
            claim.status = Status::Paid;
            closed = true;
        } else {
            let Some(s) = claim
                .schedule
                .as_ref()
                .filter(|s| claim.status == Status::InPayment && s.next_period == item.period_start)
            else {
                return Err(stale());
            };
            let last = item.period_end >= s.end;
            if item.gross_minor > 0 || last {
                cx.core.execute(Instruction::PayBenefit {
                    claim: claim.id.clone(),
                    policy: claim.policy.clone(),
                    cover: claim.cover.clone(),
                    payment_ref: payment_ref.clone(),
                    period_start: item.period_start,
                    period_end: item.period_end,
                    gross_minor: item.gross_minor,
                    offset: item.offset.clone(),
                    payee: item.payee.clone(),
                    close: last,
                })?;
            }
            claim.reserve_minor -= claim.reserve_minor.min(item.gross_minor);
            match &item.offset {
                Some(o) if o.partition.is_some() => {
                    let life = claim.life_party.clone();
                    self.take_remote(cx, &claim.id, life.as_deref(), o, &payment_ref);
                }
                Some(o) => self.take_offset(claim, o),
                None => {}
            }
            if let Some(s) = claim.schedule.as_mut() {
                let other_income_minor = period_income(s, item.period_start, item.period_end);
                s.periods.insert(
                    item.period_start,
                    Period {
                        other_income_minor,
                        end: item.period_end,
                        gross_minor: item.gross_minor,
                        offset_minor: item.offset.as_ref().map_or(0, |o| o.amount_minor),
                        payment_ref: payment_ref.clone(),
                        run: run.id.clone(),
                        returned: false,
                        arrears_minor: std::mem::take(&mut s.arrears_minor).min(item.gross_minor),
                    },
                );
                s.next_period = item.period_end;
                if last {
                    s.ended.get_or_insert(EndReason::BenefitPeriodEnd);
                    claim.status = Status::Ended;
                    claim.reserve_minor = 0;
                    closed = true;
                }
            }
        }
        Self::meet_timer(claim, TimerKind::Payment, cx.today);
        cx.paid(
            claim,
            &payment_ref,
            item.gross_minor,
            item.net_minor(),
            &item.payee,
            Some(item.period_end),
            false,
            0,
        );
        if closed {
            claim.closed_on = Some(cx.today);
            claim.due.clear();
            cx.notify("claim_closed", &claim.id, &[("outcome", claim.status.code().to_string())]);
        }
        Ok(())
    }

    /// Count an offset against the overpayment it recovers, which may be on
    /// this claim or on another claim of the same person.
    fn take_offset(&mut self, claim: &mut Claim, offset: &Offset) {
        let apply = |c: &mut Claim| {
            if let Some(o) = c.overpayments.iter_mut().find(|o| o.id == offset.overpayment) {
                o.recovered_minor += offset.amount_minor;
                if o.outstanding_minor() <= 0 {
                    o.status = RecoveryStatus::Recovered;
                }
            }
        };
        if offset.claim == claim.id {
            apply(claim);
        } else if let Ok(mut other) = self.claim(&offset.claim) {
            apply(&mut other);
            self.commit(other);
        }
    }

    /// The second person approves the run and it is posted. A payment the
    /// core refuses is an exception with a task; the rest of the run goes on.
    pub(crate) fn approve_run(&mut self, cx: &mut Cx<'_>, id: &str, actor: &Actor) -> Result<(), Reject> {
        let mut run = self
            .runs
            .get(id)
            .cloned()
            .ok_or_else(|| Reject::new("unknown_run", format!("there is no run {id}")))?;
        self.staff(actor)?;
        let largest = run.items.iter().map(RunItem::net_minor).max().unwrap_or(0);
        let profile = self.authority.profile(&actor.staff)?;
        if run.status != RunStatus::Prepared || run.prepared_by == actor.staff || !self.leads(&actor.staff) || largest > profile.max_payment_minor {
            return Err(Reject::new(
                "run_approval",
                "a prepared run is approved by a second person, a lead whose payment authority covers its largest payment",
            ));
        }
        run.approved_by = Some(actor.staff.clone());
        let mut failed: BTreeSet<String> = BTreeSet::new();
        let mut paid = 0u32;
        for item in &run.items {
            cx.core.work(1);
            let Ok(mut claim) = self.claim(&item.claim) else { continue };
            let result = if failed.contains(&item.claim) {
                Err(Reject::new("prior_period_failed", "an earlier period of this claim was not paid"))
            } else {
                self.pay_item(cx, &run, item, &mut claim)
            };
            match result {
                Ok(()) => {
                    run.submitted_minor += item.net_minor();
                    paid += 1;
                }
                Err(e) => {
                    let payment_ref = item.payment_ref(&run.id);
                    failed.insert(item.claim.clone());
                    open_task(&mut claim, cx, format!("payment-failure/{payment_ref}"), TaskKind::PaymentFailure, None);
                    run.exceptions.push((payment_ref, e.code));
                }
            }
            self.commit(claim);
        }
        self.release_from_run(&run);
        run.status = RunStatus::Submitted;
        cx.notify(
            "run_submitted",
            id,
            &[
                ("count", paid.to_string()),
                ("submitted_minor", run.submitted_minor.to_string()),
                ("exceptions", run.exceptions.len().to_string()),
            ],
        );
        self.runs.insert(id.to_string(), run);
        Ok(())
    }

    fn release_from_run(&mut self, run: &Run) {
        let claims: BTreeSet<&String> = run.items.iter().map(|i| &i.claim).collect();
        for id in claims {
            if let Ok(mut claim) = self.claim(id) {
                claim.in_run = None;
                self.commit(claim);
            }
        }
    }

    pub(crate) fn cancel_run(&mut self, cx: &mut Cx<'_>, id: &str, actor: &Actor) -> Result<(), Reject> {
        self.staff(actor)?;
        let Some(mut run) = self.runs.get(id).filter(|r| r.status == RunStatus::Prepared).cloned() else {
            return Err(Reject::new("unknown_run", format!("there is no prepared run {id}")));
        };
        self.release_from_run(&run);
        run.status = RunStatus::Cancelled;
        cx.notify("run_cancelled", id, &[]);
        self.runs.insert(id.to_string(), run);
        Ok(())
    }

    /// The bank statement's total for a run. Payments submitted, less any
    /// that came back, must equal what settled.
    pub(crate) fn confirm_run(&mut self, cx: &mut Cx<'_>, id: &str, settled_minor: i64) -> Result<(), Reject> {
        let Some(run) = self
            .runs
            .get_mut(id)
            .filter(|r| matches!(r.status, RunStatus::Submitted | RunStatus::Unreconciled))
        else {
            return Err(Reject::new("unknown_run", format!("there is no submitted run {id}")));
        };
        let expected = run.submitted_minor - run.returned_minor;
        run.settled_minor = Some(settled_minor);
        if settled_minor == expected {
            run.status = RunStatus::Settled;
        } else {
            run.status = RunStatus::Unreconciled;
            cx.notify(
                "run_unreconciled",
                id,
                &[("expected_minor", expected.to_string()), ("settled_minor", settled_minor.to_string())],
            );
        }
        Ok(())
    }
}

impl ClaimsState {
    /// Work out the late payment interest on what is payable now and tell
    /// the payee. Interest is always offered; it is never waived for them.
    pub(crate) fn offer_late_interest(&mut self, cx: &mut Cx<'_>, id: &str, actor: &Actor) -> Result<(), Reject> {
        let mut claim = self.claim(id)?;
        self.owner(actor, &claim)?;
        let amount = match (self.rules.late_interest, claim.admission()) {
            (Some(rate), Some(admission)) if claim.status == Status::Admitted => rate.on(claim.benefit_payable_minor(), admission.decided_on, cx.today),
            _ => 0,
        };
        if amount <= 0 {
            return Err(Reject::new("no_interest_owed", "no late payment interest is owed on this claim today"));
        }
        claim.interest_offer = Some((amount, cx.today));
        note(&mut claim, cx, Some(actor), format!("interest_offered:{amount}"));
        cx.notify("late_interest_offered", id, &[("amount_minor", amount.to_string())]);
        self.commit(claim);
        Ok(())
    }

    /// The payee was told the interest and declines it, by their own
    /// recorded decision.
    pub(crate) fn decline_late_interest(&mut self, cx: &mut Cx<'_>, id: &str, vault_ref: String, actor: &Actor) -> Result<(), Reject> {
        let mut claim = self.claim(id)?;
        self.owner(actor, &claim)?;
        if claim.interest_offer.is_none() || !claim.status.is_open() {
            return Err(Reject::new(
                "interest_not_offered",
                "a payee can decline interest only on an open claim, after being told the amount",
            ));
        }
        if vault_ref.trim().is_empty() {
            return Err(Reject::new("decision_required", "file the payee's own signed or recorded decision"));
        }
        claim.interest_declined = Some((cx.today, vault_ref));
        note(&mut claim, cx, Some(actor), "interest_declined");
        cx.notify("late_interest_declined", id, &[("reason", "declined_by_payee".into())]);
        self.commit(claim);
        Ok(())
    }

    /// The payee changes their mind before the claim closes: interest is
    /// paid from now on, and what they declined goes with the next payment.
    pub(crate) fn withdraw_interest_decline(&mut self, cx: &mut Cx<'_>, id: &str, actor: &Actor) -> Result<(), Reject> {
        let mut claim = self.claim(id)?;
        self.owner(actor, &claim)?;
        if claim.interest_declined.is_none() || !claim.status.is_open() {
            return Err(Reject::new("no_decline", "there is no decline to withdraw on an open claim"));
        }
        claim.interest_declined = None;
        let declined: i64 = claim.payments.iter_mut().map(|p| std::mem::take(&mut p.interest_declined_minor)).sum();
        claim.interest_owed_again_minor += declined;
        note(&mut claim, cx, Some(actor), "interest_decline_withdrawn");
        cx.notify("late_interest_decline_withdrawn", id, &[("owed_again_minor", declined.to_string())]);
        self.commit(claim);
        Ok(())
    }
}
