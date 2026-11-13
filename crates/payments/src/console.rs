//! Lists for the staff payments console. Each answer is built from state
//! alone and leaves out account numbers and names: a person looks an item
//! up by ID for anything more.

use serde_json::{json, Value};

use crate::machine::PaymentsState;
use crate::model::{ItemStatus, RunStatus};

impl PaymentsState {
    /// Runs not yet closed or cancelled, oldest value date first.
    pub fn open_runs(&self) -> Value {
        let mut runs: Vec<_> = self
            .runs
            .iter()
            .filter(|(_, r)| matches!(r.status, RunStatus::Prepared | RunStatus::Checked | RunStatus::Approved))
            .collect();
        runs.sort_by_key(|(id, r)| (r.value_date, (*id).clone()));
        let rows: Vec<Value> = runs
            .into_iter()
            .map(|(id, r)| {
                let held = r.items.values().filter(|i| matches!(i.status, ItemStatus::Held(_))).count();
                json!({
                    "run": id,
                    "run_type": r.run_type,
                    "fund": r.fund,
                    "status": r.status,
                    "value_date": r.value_date,
                    "urgent": r.urgent,
                    "makers": r.makers,
                    "approvers": r.approvers,
                    "items": r.items.len(),
                    "held": held,
                    "total_minor": r.live_total(),
                })
            })
            .collect();
        json!(rows)
    }

    /// Reconciliation exceptions not yet resolved, oldest first, on one
    /// account or on all of them.
    pub fn open_exceptions(&self, account: Option<&str>) -> Value {
        let mut open: Vec<_> = self
            .exceptions
            .iter()
            .filter(|(_, e)| e.resolution.is_none() && account.is_none_or(|a| e.account == a))
            .collect();
        open.sort_by_key(|(id, e)| (e.opened_on, (*id).clone()));
        let rows: Vec<Value> = open
            .into_iter()
            .map(|(id, e)| {
                json!({
                    "exception": id,
                    "account": e.account,
                    "kind": e.kind,
                    "amount_minor": e.amount_minor,
                    "reference": e.reference,
                    "owner": e.owner,
                    "opened_on": e.opened_on,
                })
            })
            .collect();
        json!(rows)
    }

    /// A party's payees and authorities, with what stops each being paid
    /// or collected from now, and what the customer master last said about
    /// the party's own hold.
    pub fn party_summary(&self, party: &str) -> Value {
        let now = self.clock.unwrap_or_default();
        let payees: Vec<Value> = self
            .payees
            .iter()
            .filter(|(_, p)| p.party == party)
            .map(|(id, p)| {
                json!({
                    "payee": id,
                    "kind": p.kind,
                    "blocked": p.blocked(now),
                    "cooling_off_until": p.cooling_off_until,
                })
            })
            .collect();
        let authorities: Vec<Value> = self
            .authorities
            .iter()
            .filter(|(_, a)| a.party == party)
            .map(|(id, a)| {
                json!({
                    "authority": id,
                    "policy": a.policy,
                    "life": a.life,
                    "status": a.status,
                    "reason": a.reason,
                    "dishonours": a.dishonours,
                    "hardship": a.hardship,
                    "sole_name": a.sole_name,
                })
            })
            .collect();
        json!({
            "party": party,
            "payees": payees,
            "authorities": authorities,
            "customer_hold": self.customer_holds.get(party),
        })
    }
}
