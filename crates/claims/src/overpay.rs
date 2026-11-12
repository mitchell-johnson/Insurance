//! Overpayments and their recovery.
//!
//! An overpayment is a receivable on the claim, raised as a fact with the
//! status `held`. What happens next is a decision. Where the insurer made
//! the mistake and the recipient acted in good faith, nothing is asked of
//! them unless the conduct officer and legal both sign; the decision engine
//! enforces that, and an offset is only ever taken from an overpayment whose
//! status is `offset`.

use crate::command::Actor;
use crate::config::Queue;
use crate::model::{Claim, Overpayment, OverpaymentCause, RecoveryStatus, TaskKind};
use crate::port::{Instruction, Reject};
use crate::state::{note, open_task, ClaimsState, Cx};

/// Publish a change to an overpayment with its whole position, so a reader
/// such as the held balance workbench needs no other source: who owes it,
/// what is left, how it is being recovered and whose mistake it was.
pub(crate) fn publish(cx: &mut Cx<'_>, kind: &str, claim: &Claim, o: &Overpayment, extra: &[(&str, String)]) {
    let party = claim
        .claimant
        .clone()
        .or_else(|| claim.life_party.clone())
        .unwrap_or_else(|| claim.policyholder.clone());
    let mut detail = vec![
        ("claim", claim.id.clone()),
        ("policy", claim.policy.clone()),
        ("overpayment", o.id.clone()),
        ("party", party),
        ("outstanding_minor", o.outstanding_minor().to_string()),
        ("status", o.status.code().to_string()),
        ("cause", format!("{:?}", o.cause)),
        ("insurer_error", (o.cause == OverpaymentCause::InsurerError).to_string()),
        ("good_faith", o.good_faith.to_string()),
    ];
    detail.extend(extra.iter().cloned());
    cx.notify(kind, &claim.id, &detail);
}

impl ClaimsState {
    /// Record an overpayment found outside a replay: a provider billing
    /// twice, for example.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn raise_overpayment(
        &mut self,
        cx: &mut Cx<'_>,
        id: &str,
        overpayment: String,
        amount_minor: i64,
        cause: OverpaymentCause,
        good_faith: bool,
        actor: &Actor,
    ) -> Result<(), Reject> {
        let mut claim = self.claim(id)?;
        let recoveries = self.unconflicted(actor, &claim)?.queues.contains(&Queue::Recoveries);
        if !recoveries {
            self.owner(actor, &claim)?;
        }
        let raised: i64 = claim.overpayments.iter().map(|o| o.amount_minor).sum();
        let paid = claim.paid_minor(false) + claim.paid_minor(true);
        if amount_minor <= 0 || raised + amount_minor > paid || claim.overpayment(&overpayment).is_some() {
            return Err(Reject::new(
                "invalid_overpayment",
                "an overpayment is new, positive and no more than was paid on the claim",
            ));
        }
        let mut o = Self::overpayment(&claim, cx, amount_minor, cause, good_faith).ok_or_else(|| Reject::new("invalid_overpayment", "nothing was overpaid"))?;
        o.id = overpayment;
        cx.core.execute(Instruction::RaiseOverpayment {
            claim: id.to_string(),
            policy: claim.policy.clone(),
            cover: claim.cover.clone(),
            overpayment: o.id.clone(),
            amount_minor,
        })?;
        // A false declaration also goes to the compliance module, which reads this.
        publish(cx, "overpayment_raised", &claim, &o, &[("amount_minor", amount_minor.to_string())]);
        claim.overpayments.push(o);
        note(&mut claim, cx, Some(actor), "overpayment_raised");
        self.commit(claim);
        Ok(())
    }

    /// Money received against an overpayment.
    pub(crate) fn record_repayment(&mut self, cx: &mut Cx<'_>, id: &str, overpayment: &str, amount_minor: i64) -> Result<(), Reject> {
        let mut claim = self.claim(id)?;
        let policy = claim.policy.clone();
        let Some(o) = claim
            .overpayments
            .iter_mut()
            .find(|o| o.id == overpayment && amount_minor > 0 && amount_minor <= o.outstanding_minor())
        else {
            return Err(Reject::new("invalid_repayment", "there is no such overpayment with that much outstanding"));
        };
        cx.core.execute(Instruction::RecoverOverpayment {
            claim: id.to_string(),
            policy,
            overpayment: overpayment.to_string(),
            amount_minor,
        })?;
        o.recovered_minor += amount_minor;
        if o.outstanding_minor() == 0 {
            o.status = RecoveryStatus::Recovered;
            o.history.push((cx.today, RecoveryStatus::Recovered, 0));
        }
        let o = o.clone();
        note(&mut claim, cx, None, "repayment_recorded");
        publish(cx, "overpayment_recovered", &claim, &o, &[("amount_minor", amount_minor.to_string())]);
        self.commit(claim);
        Ok(())
    }

    /// Change how an overpayment is being recovered. Every signature the
    /// change needs has been given by the time this runs.
    pub(crate) fn set_recovery_status(
        cx: &mut Cx<'_>,
        claim: &mut Claim,
        overpayment: &str,
        status: RecoveryStatus,
        offset_pct: u32,
        instalment_minor: i64,
        decision: u32,
    ) {
        let mut changed = None;
        if let Some(o) = claim.overpayments.iter_mut().find(|o| o.id == overpayment) {
            o.status = status;
            o.offset_pct = if status == RecoveryStatus::Offset { offset_pct } else { 0 };
            o.instalment_minor = if status == RecoveryStatus::Arrangement { instalment_minor } else { 0 };
            o.history.push((cx.today, status, decision));
            changed = Some(o.clone());
        }
        if status == RecoveryStatus::Arrangement {
            // A repayment arrangement is a case of its own.
            open_task(claim, cx, format!("repayment-arrangement/{decision}"), TaskKind::RepaymentArrangement, None);
        }
        if let Some(o) = changed {
            publish(cx, "recovery_status_changed", claim, &o, &[]);
        }
    }

    /// Write off what is left of an overpayment.
    pub(crate) fn write_off(cx: &mut Cx<'_>, claim: &mut Claim, overpayment: &str, decision: u32) -> Result<(), Reject> {
        let (id, policy) = (claim.id.clone(), claim.policy.clone());
        let Some(o) = claim.overpayments.iter_mut().find(|o| o.id == overpayment) else {
            return Err(Reject::new("unknown_overpayment", "there is no such overpayment on this claim"));
        };
        let amount = o.outstanding_minor();
        cx.core.execute(Instruction::WriteOffOverpayment {
            claim: id.clone(),
            policy,
            overpayment: overpayment.to_string(),
            amount_minor: amount,
        })?;
        o.written_off_minor += amount;
        o.status = RecoveryStatus::WrittenOff;
        o.history.push((cx.today, RecoveryStatus::WrittenOff, decision));
        // Whether a reinsurer shares the cost is agreed under the treaty.
        let o = o.clone();
        publish(cx, "overpayment_written_off", claim, &o, &[("amount_minor", amount.to_string())]);
        Ok(())
    }
}
