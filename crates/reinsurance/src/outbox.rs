//! The outbox: instructions for the core and notices for others, kept in the
//! module's replicated state until the driver has carried them.
//!
//! A command proposed by the event feed has nobody waiting for its reply, so
//! what it produced cannot travel in the reply alone. Everything the module
//! issues is numbered and kept here, in order, and leaves only when the
//! driver acknowledges it. A new leader, or a restarted driver, carries on
//! from the oldest entry; the core applies an instruction once however often
//! it is sent.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::machine::{Audience, Ctx, Instruction, Notice, Reinsurance, Reject};

/// A refused instruction a person decided not to send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Abandoned {
    pub seq: u64,
    pub instruction: Instruction,
    pub reason: String,
    pub approved_by: String,
    pub on: NaiveDate,
}

/// A refused notice a person decided not to send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AbandonedNotice {
    pub seq: u64,
    pub notice: Notice,
    pub reason: String,
    pub approved_by: String,
    pub on: NaiveDate,
}

fn notice_refusal(seq: u64) -> String {
    format!("notice:{seq}")
}

fn refusal(seq: u64) -> String {
    format!("delivery:{seq}")
}

impl Reinsurance {
    /// The oldest instructions not yet acknowledged, with their numbers.
    pub fn outbox(&self, limit: usize) -> Vec<(u64, &Instruction)> {
        self.outbox.iter().take(limit).map(|(seq, i)| (*seq, i)).collect()
    }

    /// The oldest notices not yet handed on, with their numbers.
    pub fn notices(&self, limit: usize) -> Vec<(u64, &Notice)> {
        self.notice_box.iter().take(limit).map(|(seq, n)| (*seq, n)).collect()
    }

    /// The oldest notices for one audience still to be handed on. Those the
    /// audience refused are left out until a person sends them again.
    pub fn notices_for(&self, audience: Audience, limit: usize) -> Vec<(u64, &Notice)> {
        self.notice_box
            .iter()
            .filter(|(seq, n)| n.audience() == audience && !self.notices_refused.contains(seq))
            .take(limit)
            .map(|(seq, n)| (*seq, n))
            .collect()
    }

    /// Refused notices that were abandoned, oldest first.
    pub fn notices_abandoned(&self) -> &[AbandonedNotice] {
        &self.notices_abandoned
    }

    /// Refused instructions that were abandoned, oldest first.
    pub fn abandoned(&self) -> &[Abandoned] {
        &self.abandoned
    }

    pub(crate) fn acknowledge_instructions(&mut self, ctx: &mut Ctx, through: u64) -> Result<(), Reject> {
        if through > self.issued {
            return Err(Reject::new(
                "unknown_instruction",
                format!("instruction {through} has not been issued; the last is {}", self.issued),
            ));
        }
        let done: Vec<u64> = self.outbox.range(..=through).map(|(seq, _)| *seq).collect();
        for seq in done {
            self.outbox.remove(&seq);
            self.set_exception(ctx, refusal(seq), "instruction_refused", None);
        }
        Ok(())
    }

    pub(crate) fn instruction_refused(&mut self, ctx: &mut Ctx, seq: u64, code: &str, message: &str) -> Result<(), Reject> {
        let Some(instruction) = self.outbox.get(&seq) else {
            return Err(Reject::new("unknown_instruction", format!("instruction {seq} is not waiting to be delivered")));
        };
        if code.trim().is_empty() {
            return Err(Reject::new("reason_required", "a refusal carries the core's code"));
        }
        let detail = format!(
            "the core refused instruction {seq} ({}) for partition {}: {code}: {message}. Nothing after it is delivered until this is resolved",
            instruction.id, instruction.partition
        );
        self.set_exception(ctx, refusal(seq), "instruction_refused", Some(detail));
        Ok(())
    }

    pub(crate) fn abandon_instruction(&mut self, ctx: &mut Ctx, seq: u64, reason: String, approved_by: String) -> Result<(), Reject> {
        if !self.exceptions.contains_key(&refusal(seq)) {
            return Err(Reject::new("not_refused", format!("instruction {seq} has not been refused by the core")));
        }
        if reason.trim().is_empty() || approved_by.trim().is_empty() {
            return Err(Reject::new(
                "reason_required",
                "abandoning an instruction needs a reason and the person who approved it",
            ));
        }
        // The exception exists only while the instruction waits.
        let Some(instruction) = self.outbox.remove(&seq) else {
            return Err(Reject::new("unknown_instruction", format!("instruction {seq} is not waiting to be delivered")));
        };
        self.abandoned.push(Abandoned {
            seq,
            instruction,
            reason,
            approved_by,
            on: ctx.on,
        });
        self.set_exception(ctx, refusal(seq), "instruction_refused", None);
        Ok(())
    }

    pub(crate) fn acknowledge_notices(&mut self, seqs: &[u64]) -> Result<(), Reject> {
        if let Some(ahead) = seqs.iter().find(|seq| **seq > self.noticed) {
            return Err(Reject::new(
                "unknown_notice",
                format!("notice {ahead} has not been raised; the last is {}", self.noticed),
            ));
        }
        for seq in seqs {
            // One set aside as refused leaves only by a person's decision.
            if !self.notices_refused.contains(seq) {
                self.notice_box.remove(seq);
            }
        }
        Ok(())
    }

    pub(crate) fn notice_refused(&mut self, ctx: &mut Ctx, seq: u64, code: &str, message: &str) -> Result<(), Reject> {
        let Some(notice) = self.notice_box.get(&seq) else {
            return Err(Reject::new("unknown_notice", format!("notice {seq} is not waiting to be handed on")));
        };
        if code.trim().is_empty() {
            return Err(Reject::new("reason_required", "a refusal carries the refuser's code"));
        }
        let detail = format!("notice {seq} for {:?} was refused: {code}: {message}", notice.audience());
        self.notices_refused.insert(seq);
        self.set_exception(ctx, notice_refusal(seq), "notice_refused", Some(detail));
        Ok(())
    }

    pub(crate) fn retry_notice(&mut self, ctx: &mut Ctx, seq: u64) -> Result<(), Reject> {
        if !self.notices_refused.remove(&seq) {
            return Err(Reject::new("not_refused", format!("notice {seq} has not been refused")));
        }
        self.set_exception(ctx, notice_refusal(seq), "notice_refused", None);
        Ok(())
    }

    pub(crate) fn abandon_notice(&mut self, ctx: &mut Ctx, seq: u64, reason: String, approved_by: String) -> Result<(), Reject> {
        if !self.notices_refused.contains(&seq) {
            return Err(Reject::new("not_refused", format!("notice {seq} has not been refused")));
        }
        if reason.trim().is_empty() || approved_by.trim().is_empty() {
            return Err(Reject::new(
                "reason_required",
                "abandoning a notice needs a reason and the person who approved it",
            ));
        }
        self.notices_refused.remove(&seq);
        if let Some(notice) = self.notice_box.remove(&seq) {
            self.notices_abandoned.push(AbandonedNotice {
                seq,
                notice,
                reason,
                approved_by,
                on: ctx.on,
            });
        }
        self.set_exception(ctx, notice_refusal(seq), "notice_refused", None);
        Ok(())
    }
}
