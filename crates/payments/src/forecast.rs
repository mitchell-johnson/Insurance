//! The funding forecast (PAY-013): what is about to leave each fund's
//! accounts, so treasury can fund them before a run is refused for want of
//! money. A claim admitted for at least the pre-advice threshold is flagged
//! to treasury as soon as claims admits it, before any payment is released.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::machine::{PaymentsState, Tx};
use crate::model::{Instruction, OutboxEntry, Reject, Role, RunStatus};

/// A claim claims has admitted and not yet released for payment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Admitted {
    pub amount_minor: i64,
    pub on: NaiveDate,
}

impl PaymentsState {
    /// Claims admitted a claim for a lump sum (`claim_admitted`).
    pub(crate) fn claim_admitted(&mut self, tx: &mut Tx<'_>, claim: &str, amount_minor: i64) -> Result<(), Reject> {
        tx.need(Role::Service)?;
        // A periodic benefit is admitted at nothing; its payments are small
        // and regular, and are seen when released.
        if amount_minor <= 0 || claim.is_empty() || self.admitted.contains_key(claim) {
            return Ok(());
        }
        self.admitted.insert(
            claim.to_string(),
            Admitted {
                amount_minor,
                on: tx.now.date(),
            },
        );
        let threshold = self.cfg()?.rules.pre_advice_threshold_minor;
        if amount_minor >= threshold {
            self.push_outbox(OutboxEntry {
                key: format!("large-claim-{claim}"),
                instruction: Instruction::Notify {
                    to: Role::Treasury,
                    subject: "large_claim_admitted".to_string(),
                    detail: format!("claim {claim} was admitted for {amount_minor}: fund the claims account before it is paid"),
                },
                about: None,
            });
        }
        Ok(())
    }

    /// The claim's money was released, or the claim closed: it is no longer
    /// a forecast.
    pub(crate) fn claim_left_forecast(&mut self, claim: &str) {
        self.admitted.remove(claim);
    }

    /// What is expected to leave between `from` and `to`: runs not yet
    /// settled, by value date, run type and fund; payments released and
    /// waiting for a run; and claims admitted and not yet released, with
    /// those at or above the pre-advice threshold listed.
    pub fn funding_forecast(&self, from: NaiveDate, to: NaiveDate) -> Value {
        let mut runs: BTreeMap<(NaiveDate, String, String), i64> = BTreeMap::new();
        for run in self.runs.values() {
            let open = matches!(run.status, RunStatus::Prepared | RunStatus::Checked | RunStatus::Approved);
            if !open || run.run_type.inbound() || run.value_date < from || run.value_date > to {
                continue;
            }
            let key = (
                run.value_date,
                serde_json::to_value(run.run_type)
                    .map(|v| v.as_str().unwrap_or_default().to_string())
                    .unwrap_or_default(),
                run.fund.clone(),
            );
            *runs.entry(key).or_default() += run.live_total();
        }
        let runs: Vec<Value> = runs
            .into_iter()
            .map(|((value_date, run_type, fund), total)| json!({"value_date": value_date, "run_type": run_type, "fund": fund, "total_minor": total}))
            .collect();
        let threshold = self.config.as_ref().map_or(i64::MAX, |c| c.rules.pre_advice_threshold_minor);
        let large: Vec<Value> = self
            .admitted
            .iter()
            .filter(|(_, a)| a.amount_minor >= threshold)
            .map(|(claim, a)| json!({"claim": claim, "amount_minor": a.amount_minor, "admitted_on": a.on}))
            .collect();
        json!({
            "from": from,
            "to": to,
            "runs": runs,
            "waiting_minor": self.waiting.values().map(|w| w.amount_minor).sum::<i64>(),
            "admitted_minor": self.admitted.values().map(|a| a.amount_minor).sum::<i64>(),
            "large_claims": large,
        })
    }
}
