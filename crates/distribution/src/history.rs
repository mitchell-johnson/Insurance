//! Bounding the module's state.
//!
//! The module keeps line by line what it needs to act: open exposures,
//! allocations a reversal or a clawback may still reach, held items not yet
//! released, postings still on their way, and movements not yet on a
//! statement. History older than `Settings::history_days` that nothing can
//! reach any more is let go once a month, when the business date first
//! enters a new month. Movements are folded into a brought-forward balance
//! per party and account, so a position and a proof of debt still add up.
//! The core's ledger keeps every posting, and issued statements are kept by
//! the documents module, so nothing is lost to the business; it simply stops
//! being answerable line by line from here.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{Datelike, NaiveDate};
use ic_core::{PartyId, PolicyId};
use serde::{Deserialize, Serialize};

use ic_core::counterparty::PartyLedger;

use crate::model::PostingState;
use crate::state::DistributionState;
use crate::workbench::ActionState;

/// What has been let go, for the `history` question.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct History {
    /// Line detail before this date has been folded or let go.
    pub compacted_before: Option<NaiveDate>,
    /// Movements folded, per party and account, with the total folded.
    pub brought_forward: BTreeMap<PartyId, BTreeMap<PartyLedger, i64>>,
    /// Counts let go at the last compaction, by kind, for the run book.
    pub last_run: BTreeMap<String, u64>,
    /// Policies let go whole, so that a late event about one is recognised.
    pub policies_let_go: BTreeSet<PolicyId>,
    /// The month (year, month) compaction last ran in.
    pub last_month: Option<(i32, u32)>,
}

impl DistributionState {
    /// Run as core events are taken in: once in each business month.
    pub(crate) fn compact_if_due(&mut self, today: NaiveDate) {
        let month = (today.year(), today.month());
        if self.settings.history_days == 0 || self.history.last_month == Some(month) {
            return;
        }
        self.history.last_month = Some(month);
        if let Some(cutoff) = today.checked_sub_days(chrono::Days::new(u64::from(self.settings.history_days))) {
            self.compact(cutoff);
        }
    }

    /// Let go of everything before `cutoff` that nothing can reach any more.
    pub fn compact(&mut self, cutoff: NaiveDate) {
        let mut counts: BTreeMap<String, u64> = BTreeMap::new();
        let mut count = |kind: &str, n: usize| *counts.entry(kind.to_string()).or_insert(0) += n as u64;

        // Movements already on a statement (or of a party who gets none).
        let cut_of = |party: &PartyId| self.payees.get(party).map_or(u64::MAX, |p| p.statement_cut);
        let (old, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.movements)
            .into_iter()
            .partition(|m| m.date < cutoff && m.seq <= cut_of(&m.party));
        count("movements", old.len());
        for m in old {
            *self.history.brought_forward.entry(m.party).or_default().entry(m.ledger).or_insert(0) += m.amount_minor;
        }
        self.movements = kept;

        // Statements: the latest of each payee stays.
        let mut latest: BTreeMap<PartyId, u32> = BTreeMap::new();
        for s in self.statements.values() {
            let n = latest.entry(s.payee.clone()).or_insert(0);
            *n = (*n).max(s.number);
        }
        let before = self.statements.len();
        self.statements.retain(|_, s| s.date >= cutoff || latest.get(&s.payee) == Some(&s.number));
        count("statements", before - self.statements.len());

        // Policies that ended before the cutoff and that nothing can reach.
        let ended: Vec<PolicyId> = self
            .policies
            .iter()
            .filter(|(_, p)| p.ended.is_some_and(|(d, _)| d < cutoff))
            .map(|(id, _)| id.clone())
            .filter(|id| self.unreachable(id))
            .collect();
        count("policies", ended.len());
        for policy in ended {
            self.let_go_of_policy(&policy);
        }

        // Postings the core confirmed long ago. Adjustment references stay:
        // they are how a reused reference is refused.
        let before = self.requested.len();
        let dated: BTreeMap<String, NaiveDate> = self.allocations.values().map(|a| (a.reference.clone(), a.date)).collect();
        self.requested.retain(|reference, r| {
            reference.starts_with("adj:")
                || !matches!(r.state, PostingState::Posted { .. })
                || r.posting.value_date.or_else(|| dated.get(reference).copied()).is_none_or(|d| d >= cutoff)
        });
        count("postings", before - self.requested.len());

        let before = self.held.len();
        self.held.retain(|h| h.remaining_minor != 0 || h.date >= cutoff);
        count("held_items", before - self.held.len());

        let before = self.actions.len();
        self.actions
            .retain(|_, a| matches!(a.state, ActionState::Proposed | ActionState::Posting) || a.proposed_on >= cutoff);
        count("actions", before - self.actions.len());

        let before = self.cases.len();
        self.cases.retain(|_, c| c.closed.as_ref().is_none_or(|(d, _)| *d >= cutoff));
        count("cases", before - self.cases.len());

        let before = self.tasks.len();
        self.tasks.retain(|_, t| t.done.is_none() || t.due >= cutoff);
        count("tasks", before - self.tasks.len());

        let before = self.controls.len();
        self.controls.retain(|c| c.date >= cutoff);
        count("controls", before - self.controls.len());

        let before = self.memo.len();
        self.memo.retain(|_, m| m.outstanding_minor != 0 || m.as_at >= cutoff);
        count("memo_lines", before - self.memo.len());

        let mut documents = 0;
        for list in self.documents.values_mut() {
            let before = list.len();
            list.retain(|d| d.on >= cutoff);
            documents += before - list.len();
        }
        self.documents.retain(|_, list| !list.is_empty());
        count("documents", documents);

        let before = self.idempotency_len();
        self.forget_keys_before(cutoff);
        count("command_keys", before - self.idempotency_len());

        self.history.compacted_before = Some(self.history.compacted_before.map_or(cutoff, |c| c.max(cutoff)));
        self.history.last_run = counts;
    }

    /// A policy nothing can reach: nothing held from it, nothing on its way
    /// to the core, and no other policy's replacement match or link pointing
    /// at it. Its exposures need not be closed: a policy that ended this long
    /// ago cannot be reinstated, so nothing more can be clawed back or given
    /// back on them.
    fn unreachable(&self, policy: &PolicyId) -> bool {
        let Some(record) = self.policies.get(policy) else { return false };
        let mine: BTreeSet<&String> = self.allocations.values().filter(|a| &a.policy == policy).map(|a| &a.reference).collect();
        let nothing_held = !self.held.iter().any(|h| h.remaining_minor != 0 && mine.contains(&h.reference));
        let nothing_pending = !self
            .requested
            .iter()
            .any(|(reference, r)| r.state == PostingState::Pending && (mine.contains(reference) || r.posting.policy.as_ref() == Some(policy)));
        let prefix = format!("{policy}/");
        let not_linked = !self.policies.iter().any(|(id, p)| id != policy && p.replaces.as_ref() == Some(policy))
            && record.replaces.as_ref().is_none_or(|r| !self.policies.contains_key(r))
            && !self
                .layers
                .values()
                .any(|l| !l.key().starts_with(&prefix) && l.replacement.as_ref().is_some_and(|m| m.replaced.iter().any(|(k, _)| k.starts_with(&prefix))));
        nothing_held && nothing_pending && not_linked
    }

    fn let_go_of_policy(&mut self, policy: &PolicyId) {
        let Some(record) = self.policies.remove(policy) else { return };
        let prefix = format!("{policy}/");
        for key in &record.layers {
            self.layers.remove(key);
        }
        self.layers.retain(|k, _| !k.starts_with(&prefix));
        self.exposures.retain(|_, e| &e.policy != policy);
        let gone: BTreeSet<String> = self.allocations.values().filter(|a| &a.policy == policy).map(|a| a.reference.clone()).collect();
        self.allocations.retain(|r, _| !gone.contains(r));
        for refs in self.allocations_by_date.values_mut() {
            refs.retain(|r| !gone.contains(r));
        }
        self.allocations_by_date.retain(|_, v| !v.is_empty());
        self.receipts.retain(|k, _| !k.starts_with(&prefix));
        self.premiums.remove(policy);
        self.history.policies_let_go.insert(policy.clone());
    }
}
