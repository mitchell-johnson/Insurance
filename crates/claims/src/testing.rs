//! A stand-in for the core, for tests of the claims module and of anything
//! that drives it. It records what it was asked to do and can be told to
//! refuse the next instruction.

use std::collections::BTreeMap;

use chrono::NaiveDate;

use crate::port::{Core, CoverFacts, Instruction, Reject};

#[derive(Debug, Clone, Default)]
pub struct FakeCore {
    pub today: NaiveDate,
    /// Cover facts by `policy/cover`. A policy with no entry is unknown.
    pub covers: BTreeMap<String, CoverFacts>,
    pub instructions: Vec<Instruction>,
    pub notices: Vec<(String, String, BTreeMap<String, String>)>,
    /// Refuse instructions while this is above zero, counting down.
    pub refuse_next: u32,
    pub work: u32,
    /// The partition the fake stands for.
    pub partition: u16,
    /// Commands queued for other partitions, in order.
    pub sent: Vec<(u16, crate::Command)>,
}

impl FakeCore {
    pub fn new(today: NaiveDate) -> Self {
        Self { today, ..Self::default() }
    }

    pub fn cover(&mut self, policy: &str, cover: &str, facts: CoverFacts) {
        self.covers.insert(format!("{policy}/{cover}"), facts);
    }

    /// Notices of one kind, in the order they were published.
    pub fn noticed(&self, kind: &str) -> Vec<&BTreeMap<String, String>> {
        self.notices.iter().filter(|(k, _, _)| k == kind).map(|(_, _, d)| d).collect()
    }
}

impl Core for FakeCore {
    fn business_date(&self) -> NaiveDate {
        self.today
    }

    fn cover_facts(&self, policy: &str, cover: &str, _life: Option<&str>, _event_date: NaiveDate) -> Result<CoverFacts, Reject> {
        self.covers
            .get(&format!("{policy}/{cover}"))
            .cloned()
            .ok_or_else(|| Reject::new("unknown_policy", format!("policy {policy} is not on this partition")))
    }

    fn execute(&mut self, instruction: Instruction) -> Result<(), Reject> {
        if self.refuse_next > 0 {
            self.refuse_next -= 1;
            return Err(Reject::new("ledger", "the core refused the instruction"));
        }
        self.instructions.push(instruction);
        Ok(())
    }

    fn notify(&mut self, kind: &str, claim: &str, detail: BTreeMap<String, String>) {
        self.notices.push((kind.to_string(), claim.to_string(), detail));
    }

    fn work(&mut self, units: u32) {
        self.work += units;
    }

    fn partition(&self) -> u16 {
        self.partition
    }

    fn send(&mut self, to_partition: u16, command: crate::Command) -> Result<(), Reject> {
        if to_partition == self.partition {
            return Err(Reject::new("same_partition", "a partition does not send itself a message"));
        }
        self.sent.push((to_partition, command));
        Ok(())
    }
}
