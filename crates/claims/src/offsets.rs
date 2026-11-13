//! Offsets across partitions.
//!
//! A person's claims can sit on more than one partition: each policy lives
//! on its own, and a person can hold several. An overpayment being recovered
//! by offset should come back out of that person's next benefit wherever it
//! is paid, so the partition holding the overpayment shares its position
//! with every other partition, and a benefit run there can keep part of a
//! payment back against it.
//!
//! Three messages carry it, each delivered at least once:
//!
//! - **`OffsetSource`**, from the partition holding the overpayment, each
//!   time its position changes: what is outstanding, the share of a benefit
//!   that may be kept back, and what it has dealt with of what the receiver
//!   took. A later sequence number replaces an earlier one.
//! - **`OffsetTaken`**, from the partition that kept money back. The money
//!   sits in offset in transit until the holder takes it up and reduces the
//!   receivable.
//! - **`OffsetReturned`**, when two partitions took more between them than
//!   was still owed. The excess goes back, out of transit, and is owed to
//!   the claimant again with their next benefit.
//!
//! Across the insurer offset in transit nets to the offsets still on their
//! way. A provider's billing error is not shared: providers are paid by the
//! partition that holds the claim.
//!
//! Limits: the keys that stop a repeated message are kept for ever, and an
//! overpayment on a policy that moves partition is shared again from the new
//! partition only once its position next changes.

use serde::{Deserialize, Serialize};

use crate::command::Command;
use crate::model::{OverpaymentCause, RecoveryStatus, TaskKind};
use crate::port::{Instruction, Offset, Reject};
use crate::state::{note, open_task, ClaimsState, Cx};

/// An overpayment held on another partition, as that partition last said.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteOffset {
    pub party: String,
    pub from: u16,
    pub claim: String,
    pub policy: String,
    pub overpayment: String,
    pub outstanding_minor: i64,
    pub pct: u32,
    pub seq: u64,
    /// Kept back here against it, in all.
    pub sent_minor: i64,
    /// Of that, what the holder has applied or sent back.
    pub settled_minor: i64,
}

impl RemoteOffset {
    /// What may still be kept back here: what is outstanding less what is
    /// already on its way.
    pub fn available_minor(&self) -> i64 {
        (self.outstanding_minor - (self.sent_minor - self.settled_minor)).max(0)
    }
}

fn remote_key(party: &str, from: u16, overpayment: &str) -> String {
    format!("{party}|{from}|{overpayment}")
}

impl ClaimsState {
    /// The overpayments held elsewhere that this person's benefits here may
    /// recover, with what each has left and the share that may be kept.
    pub(crate) fn remote_sources(&self, party: &str) -> impl Iterator<Item = &RemoteOffset> {
        let from = format!("{party}|");
        self.remote_offsets
            .range(from.clone()..)
            .take_while(move |(k, _)| k.starts_with(&from))
            .map(|(_, r)| r)
            .filter(|r| r.pct > 0 && r.available_minor() > 0)
    }

    /// Tell every other partition where each overpayment changed by this
    /// command now stands.
    pub(crate) fn share_offsets(&mut self, cx: &mut Cx<'_>) {
        if self.rules.partitions <= 1 || self.to_share.is_empty() {
            return;
        }
        let here = cx.core.partition();
        let ids = std::mem::take(&mut self.to_share);
        for id in ids {
            let Some(mut claim) = self.claims.get(&id).cloned() else { continue };
            let Some(party) = claim.life_party.clone() else { continue };
            let mut changed = false;
            for o in claim.overpayments.iter_mut().filter(|o| o.cause != OverpaymentCause::ProviderBilling) {
                let outstanding = o.outstanding_minor().max(0);
                let pct = if o.status == RecoveryStatus::Offset && outstanding > 0 {
                    o.offset_pct.min(self.rules.max_offset_pct)
                } else {
                    0
                };
                let received: i64 = o.remote_received.values().sum();
                let position = (outstanding, pct, received);
                // Only what is or was being offset concerns anyone else.
                if o.shared == Some(position) || (o.shared.is_none() && pct == 0) {
                    continue;
                }
                o.shared = Some(position);
                o.shared_seq += 1;
                changed = true;
                for to in (0..self.rules.partitions).filter(|p| *p != here) {
                    cx.core.work(1);
                    let command = Command::OffsetSource {
                        from: here,
                        claim: claim.id.clone(),
                        policy: claim.policy.clone(),
                        overpayment: o.id.clone(),
                        party: party.clone(),
                        outstanding_minor: outstanding,
                        pct,
                        seq: o.shared_seq,
                        settled_minor: o.remote_received.get(&to).copied().unwrap_or(0),
                    };
                    if cx.core.send(to, command).is_err() {
                        cx.notify("offset_unshared", &claim.id, &[("overpayment", o.id.clone()), ("partition", to.to_string())]);
                    }
                }
            }
            if changed {
                // Straight back in: what was shared is not a change to share.
                self.claims.insert(claim.id.clone(), claim);
            }
        }
    }

    /// Another partition says where an overpayment it holds stands.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn offset_source(
        &mut self,
        cx: &mut Cx<'_>,
        from: u16,
        claim: String,
        policy: String,
        overpayment: String,
        party: String,
        outstanding_minor: i64,
        pct: u32,
        seq: u64,
        settled_minor: i64,
    ) -> Result<(), Reject> {
        if from == cx.core.partition() || party.is_empty() || outstanding_minor < 0 || pct > 100 || settled_minor < 0 {
            return Err(Reject::new(
                "invalid_offset",
                "an offset source comes from another partition with a position that can be",
            ));
        }
        let key = remote_key(&party, from, &overpayment);
        let entry = self.remote_offsets.entry(key).or_insert_with(|| RemoteOffset {
            party,
            from,
            claim,
            policy,
            overpayment,
            outstanding_minor,
            pct,
            seq: 0,
            sent_minor: 0,
            settled_minor: 0,
        });
        // An older position, or the same one again, changes nothing.
        if seq > entry.seq {
            entry.seq = seq;
            entry.outstanding_minor = outstanding_minor;
            entry.pct = pct;
            entry.settled_minor = settled_minor.max(entry.settled_minor);
        }
        Ok(())
    }

    /// A benefit here kept back `offset` for an overpayment held elsewhere:
    /// count it as on its way and tell the holder.
    pub(crate) fn take_remote(&mut self, cx: &mut Cx<'_>, paid_claim: &str, life: Option<&str>, offset: &Offset, take_ref: &str) {
        let Some(to) = offset.partition else { return };
        if let Some(r) = life.and_then(|p| self.remote_offsets.get_mut(&remote_key(p, to, &offset.overpayment))) {
            r.sent_minor += offset.amount_minor;
        }
        let command = Command::OffsetTaken {
            from: cx.core.partition(),
            take_ref: take_ref.to_string(),
            paid_claim: paid_claim.to_string(),
            claim: offset.claim.clone(),
            overpayment: offset.overpayment.clone(),
            amount_minor: offset.amount_minor,
        };
        if cx.core.send(to, command).is_err() {
            // Cannot happen for a source another partition shared; the
            // money is in transit and the daily check finds it.
            cx.notify("offset_unsent", paid_claim, &[("payment_ref", take_ref.to_string())]);
        }
    }

    /// Another partition kept money back for an overpayment held here. What
    /// is still owed is applied; anything more goes back.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn offset_taken(
        &mut self,
        cx: &mut Cx<'_>,
        from: u16,
        take_ref: &str,
        paid_claim: &str,
        id: &str,
        overpayment: &str,
        amount_minor: i64,
    ) -> Result<(), Reject> {
        let key = format!("taken:{from}:{take_ref}");
        if self.offset_keys.contains(&key) {
            return Ok(());
        }
        if amount_minor <= 0 || from == cx.core.partition() {
            return Err(Reject::new(
                "invalid_offset",
                "an offset taken elsewhere is positive and from another partition",
            ));
        }
        self.offset_keys.insert(key);
        let mut applied = 0;
        if let Ok(mut claim) = self.claim(id) {
            let owed = claim
                .overpayments
                .iter()
                .find(|o| o.id == overpayment && o.cause != OverpaymentCause::ProviderBilling)
                .map_or(0, |o| o.outstanding_minor().max(0));
            let take = amount_minor.min(owed);
            let received = take > 0
                && cx
                    .core
                    .execute(Instruction::ReceiveOffset {
                        claim: claim.id.clone(),
                        policy: claim.policy.clone(),
                        overpayment: overpayment.to_string(),
                        amount_minor: take,
                    })
                    .is_ok();
            if received {
                applied = take;
            }
            if let Some(o) = claim.overpayments.iter_mut().find(|o| o.id == overpayment) {
                o.recovered_minor += applied;
                if o.outstanding_minor() <= 0 && o.status == RecoveryStatus::Offset {
                    o.status = RecoveryStatus::Recovered;
                }
                // All of it is dealt with here, applied or sent back.
                *o.remote_received.entry(from).or_insert(0) += amount_minor;
                let o = o.clone();
                crate::overpay::publish(
                    cx,
                    "overpayment_offset_received",
                    &claim,
                    &o,
                    &[("amount_minor", applied.to_string()), ("partition", from.to_string())],
                );
            }
            note(&mut claim, cx, None, format!("offset_received:{from}:{take_ref}"));
            self.commit(claim);
        }
        let back = amount_minor - applied;
        if back > 0 {
            let command = Command::OffsetReturned {
                from: cx.core.partition(),
                take_ref: take_ref.to_string(),
                claim: paid_claim.to_string(),
                amount_minor: back,
            };
            cx.core.send(from, command)?;
        }
        Ok(())
    }

    /// Part of what a benefit here kept back was more than was owed on the
    /// other partition. It is the claimant's again, with their next benefit.
    pub(crate) fn offset_returned(&mut self, cx: &mut Cx<'_>, from: u16, take_ref: &str, id: &str, amount_minor: i64) -> Result<(), Reject> {
        let key = format!("returned:{from}:{take_ref}");
        if self.offset_keys.contains(&key) {
            return Ok(());
        }
        if amount_minor <= 0 || from == cx.core.partition() {
            return Err(Reject::new("invalid_offset", "an offset returned is positive and from another partition"));
        }
        let Ok(mut claim) = self.claim(id) else {
            // The claim has gone (its policy moved): the money waits in
            // transit and a person places it.
            self.offset_keys.insert(key);
            cx.notify(
                "offset_return_unplaced",
                id,
                &[("payment_ref", take_ref.to_string()), ("amount_minor", amount_minor.to_string())],
            );
            return Ok(());
        };
        cx.core.execute(Instruction::ReturnOffset {
            claim: claim.id.clone(),
            policy: claim.policy.clone(),
            cover: claim.cover.clone(),
            amount_minor,
        })?;
        self.offset_keys.insert(key);
        let more_due = claim.schedule.as_ref().is_some_and(|s| s.next_period < s.end) && claim.status == crate::Status::InPayment;
        if let Some(s) = claim.schedule.as_mut() {
            s.arrears_minor += amount_minor;
        }
        // No further benefit will carry it: a senior arranges the payment.
        if !more_due {
            open_task(
                &mut claim,
                cx,
                format!("senior-review/offset-returned/{take_ref}"),
                TaskKind::SeniorReview,
                None,
            );
        }
        note(&mut claim, cx, None, format!("offset_returned:{from}:{take_ref}"));
        cx.notify("underpayment_owed", &claim.id, &[("amount_minor", amount_minor.to_string())]);
        self.commit(claim);
        Ok(())
    }
}
