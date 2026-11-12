//! Reconciliation rules for one partition. Run at the end of every business
//! day; any break stops the line.

use ic_ledger::BalanceFamily;
use serde::Serialize;

use crate::accounts::{self, role};
use crate::machine::CoreState;
use crate::policy::PolicyStatus;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Break {
    pub check: &'static str,
    pub detail: String,
}

/// Totals a control account in the finance ledger is checked against.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ControlTotals {
    pub premium_receivable_minor: i128,
    pub unearned_premium_fine: i128,
    pub policy_suspense_minor: i128,
    pub earned_premium_fine: i128,
    pub transfer_in_transit: i128,
}

pub fn control_totals(state: &CoreState) -> ControlTotals {
    ControlTotals {
        premium_receivable_minor: state.ledger.role_total(role::PREMIUM_RECEIVABLE),
        unearned_premium_fine: -state.ledger.role_total(role::UNEARNED_PREMIUM),
        policy_suspense_minor: -state.ledger.role_total(role::POLICY_SUSPENSE),
        earned_premium_fine: -state.ledger.role_total(role::EARNED_PREMIUM),
        transfer_in_transit: state.ledger.role_total(role::TRANSFER_IN_TRANSIT),
    }
}

pub fn reconcile(state: &CoreState) -> Vec<Break> {
    let mut breaks = Vec::new();
    // The partition is balanced on its own at all times, in every currency
    // and every fund's units.
    for (family, net) in state.ledger.trial_balance() {
        if net != 0 {
            let name = match family {
                BalanceFamily::Currency(c) => c.to_string(),
                BalanceFamily::Fund(f) => f.0,
            };
            breaks.push(Break {
                check: "trial_balance",
                detail: format!("{name} is off by {net}"),
            });
        }
    }
    let mut receivable_total: i128 = 0;
    let mut unearned_total: i128 = 0;
    for p in state.policies.values() {
        // A policy in flight has its balances in the in-transit accounts; a
        // transferred one has none here.
        if p.is_frozen() || p.status == PolicyStatus::Transferred {
            continue;
        }
        let c = p.currency;
        // Premium receivable equals the sum of outstanding invoices.
        let recv = state.ledger.balance(&accounts::premium_receivable(&p.id, c).id);
        let outstanding = p.outstanding_minor() as i128;
        if recv != outstanding {
            breaks.push(Break {
                check: "premium_receivable",
                detail: format!("policy {}: ledger {recv}, invoices {outstanding}", p.id),
            });
        }
        // Unearned premium equals the per-policy unearned balance, recomputed.
        let unearned = state.ledger.natural_balance(&accounts::unearned_premium(&p.id, c).id);
        if unearned != p.unearned_fine() {
            breaks.push(Break {
                check: "unearned_premium",
                detail: format!("policy {}: ledger {unearned}, recomputed {}", p.id, p.unearned_fine()),
            });
        }
        if state.ledger.natural_balance(&accounts::policy_suspense(&p.id, c).id) < 0 {
            breaks.push(Break {
                check: "policy_suspense",
                detail: format!("policy {} has a debit balance in suspense", p.id),
            });
        }
        // Refund payable equals the refunds raised and not yet paid or released.
        let refund_payable = state.ledger.natural_balance(&accounts::refund_payable(&p.id, c).id);
        let owed: i128 = p.refunds_raised.values().map(|v| *v as i128).sum();
        if refund_payable != owed {
            breaks.push(Break {
                check: "refund_payable",
                detail: format!("policy {}: ledger {refund_payable}, refunds owed {owed}", p.id),
            });
        }
        // Only a policy in force has a standing.
        if (p.status == PolicyStatus::InForce) != p.standing.is_some() {
            breaks.push(Break {
                check: "standing",
                detail: format!("policy {} is {:?} with standing {:?}", p.id, p.status, p.standing),
            });
        }
        receivable_total += outstanding;
        unearned_total += p.unearned_fine();
    }
    let frozen = state.policies.values().any(|p| p.is_frozen());
    let totals = control_totals(state);
    if !frozen && totals.premium_receivable_minor != receivable_total {
        breaks.push(Break {
            check: "receivable_control",
            detail: format!("control {}, policies {receivable_total}", totals.premium_receivable_minor),
        });
    }
    if !frozen && totals.unearned_premium_fine != unearned_total {
        breaks.push(Break {
            check: "unearned_control",
            detail: format!("control {}, policies {unearned_total}", totals.unearned_premium_fine),
        });
    }
    crate::claims::reconcile(state, &mut breaks);
    breaks
}
