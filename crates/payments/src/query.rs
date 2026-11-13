//! Questions the module answers from its state. Queries never change it.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde_json::{json, Value};

use crate::machine::PaymentsState;
use crate::model::{ItemStatus, SubmissionStatus};

impl PaymentsState {
    /// A submission is queued or with a driver: the bank may be receiving
    /// it. The fail-back controller does not move leaders while this holds.
    pub fn submission_in_progress(&self) -> bool {
        self.submissions
            .values()
            .any(|s| matches!(s.status, SubmissionStatus::Queued | SubmissionStatus::Sending))
    }

    /// The control totals of one run: submitted is acknowledged plus
    /// rejected, and acknowledged is settled plus returned plus whatever
    /// the bank still holds.
    pub fn control_totals(&self, run: &str) -> Option<Value> {
        let run = self.runs.get(run)?;
        let sum = |pick: fn(&ItemStatus) -> bool| -> i64 { run.items.values().filter(|i| pick(&i.status)).map(|i| i.new.amount_minor).sum() };
        let unsent = sum(|s| matches!(s, ItemStatus::Pending | ItemStatus::Held(_)));
        let at_bank = sum(|s| *s == ItemStatus::Submitted);
        let rejected = sum(|s| matches!(s, ItemStatus::Rejected(_)));
        let awaiting = sum(|s| *s == ItemStatus::Acknowledged);
        let settled = sum(|s| *s == ItemStatus::Settled);
        let returned = sum(|s| matches!(s, ItemStatus::Returned(_)));
        Some(json!({
            "unsent": unsent,
            "awaiting_acknowledgement": at_bank,
            "rejected": rejected,
            "acknowledged": awaiting + settled + returned,
            "awaiting_settlement": awaiting,
            "settled": settled,
            "returned": returned,
        }))
    }

    /// Where one bank account stands against its last statement: what the
    /// bank has still to show, and what it showed that nobody has explained.
    pub fn reconciliation(&self, account: &str, today: NaiveDate) -> Option<Value> {
        let book = self.books.get(account)?;
        let outstanding: Vec<Value> = book
            .expected
            .iter()
            .map(|(reference, e)| json!({"reference": reference, "amount_minor": e.amount_minor, "age_days": (today - e.since).num_days()}))
            .collect();
        let open: Vec<Value> = self
            .exceptions
            .iter()
            .filter(|(_, e)| e.account == account && e.resolution.is_none())
            .map(|(id, e)| json!({"exception": id, "kind": e.kind, "amount_minor": e.amount_minor, "owner": e.owner, "age_days": (today - e.opened_on).num_days()}))
            .collect();
        Some(json!({
            "statement_date": book.statement_date,
            "closing_minor": book.closing_minor,
            "available_minor": self.available(account),
            "not_yet_on_statement": outstanding,
            "open_exceptions": open,
        }))
    }
}

fn text<'a>(question: &'a Value, field: &str) -> Result<&'a str, String> {
    question[field].as_str().ok_or_else(|| format!("the question needs {field}"))
}

fn day(question: &Value) -> Result<NaiveDate, String> {
    text(question, "today")?.parse().map_err(|_| "today is a date like 2026-11-11".to_string())
}

fn date(question: &Value, field: &str) -> Result<NaiveDate, String> {
    text(question, field)?.parse().map_err(|_| format!("{field} is a date like 2026-11-30"))
}

fn as_at(question: &Value) -> Result<NaiveDate, String> {
    date(question, "as_at")
}

fn found<T: serde::Serialize>(what: Option<T>) -> Result<Value, String> {
    serde_json::to_value(what).map_err(|e| e.to_string())
}

/// Answer one question. This is the function the module host calls.
pub fn answer(state: &PaymentsState, question: Value) -> Result<Value, String> {
    // The fail-back controller's question, asked of every hosted module
    // before a leader is handed back: is it quiet here? Not while a
    // submission is queued or with a driver.
    if question.get("failback_quiet").is_some() {
        return Ok(json!(!state.submission_in_progress()));
    }
    match text(&question, "q")? {
        "submission_in_progress" => Ok(json!(state.submission_in_progress())),
        "outbox" => found(Some(&state.outbox)),
        "config" => found(state.config.as_ref()),
        "batch" => found(state.batch(text(&question, "submission")?)),
        "queued_submissions" => {
            let queued: Vec<_> = state.submissions.iter().filter(|(_, s)| s.status == SubmissionStatus::Queued).collect();
            found(Some(queued))
        }
        "unconfirmed_submissions" => {
            let unsure: Vec<_> = state.submissions.iter().filter(|(_, s)| s.status == SubmissionStatus::Sending).collect();
            found(Some(unsure))
        }
        "run" => found(state.runs.get(text(&question, "run")?)),
        "submission" => found(state.submissions.get(text(&question, "submission")?)),
        "control_totals" => found(state.control_totals(text(&question, "run")?)),
        "authority" => found(state.authorities.get(text(&question, "authority")?)),
        "payee" => found(state.payees.get(text(&question, "payee")?)),
        "customer_hold" => found(state.customer_holds.get(text(&question, "party")?)),
        "hold_watch" => found(Some(state.hold_watch())),
        "open_runs" => Ok(state.open_runs()),
        "funding_forecast" => Ok(state.funding_forecast(date(&question, "from")?, date(&question, "to")?)),
        "notices" => {
            let role = serde_json::from_value(question["role"].clone()).map_err(|_| "the question needs a role".to_string())?;
            found(Some(state.open_notices(role)))
        }
        "next_value_date" => {
            let run_type = serde_json::from_value(question["run_type"].clone()).map_err(|_| "the question needs a run_type".to_string())?;
            let at = question["at"]
                .as_str()
                .map(|at| at.parse().map_err(|_| "at is a time like 2026-11-30T16:30:00".to_string()));
            found(state.next_value_date(run_type, at.transpose()?))
        }
        "expected_payment" => Ok(state.expected_payment(text(&question, "payment")?)),
        "open_exceptions" => Ok(state.open_exceptions(question["account"].as_str())),
        "party" => Ok(state.party_summary(text(&question, "party")?)),
        "refund" => found(state.refunds.get(text(&question, "refund")?)),
        "messages_read" => found(Some(&state.messages_read)),
        "awaiting_conversation" => found(Some(&state.awaiting_conversation)),
        // The population delayed between two value dates, by item, for
        // putting right what the delay caused.
        "deferred" => {
            let (from, to) = (date(&question, "from")?, date(&question, "to")?);
            Ok(serde_json::json!(state
                .deferred
                .iter()
                .filter(|(_, d)| d.due >= from && d.due <= to)
                .collect::<BTreeMap<_, _>>()))
        }
        "represent" => found(Some(&state.represent)),
        "waiting" => found(Some(state.waiting_payments())),
        "fund_cash_settlement" => Ok(state.fund_cash_settlement(as_at(&question)?)),
        "fund_sweep" => Ok(state.fund_sweep(as_at(&question)?)),
        "aged_unidentified_receipts" => Ok(state.aged_unidentified_receipts(as_at(&question)?)),
        "reconciliation" => found(state.reconciliation(text(&question, "account")?, day(&question)?)),
        "readiness" => found(Some(state.readiness(day(&question)?))),
        "trail" => Ok(json!({"events": state.event_seq, "chain": state.event_chain})),
        other => Err(format!("unknown question {other}")),
    }
}

fn payout_waiting(policy: &ic_core::PolicyId, policyholder: &ic_core::PartyId, payout: &str, payment: &str, amount_minor: i64) -> crate::Command {
    crate::Command::OfferPayment {
        waiting: Box::new(crate::intake::Waiting {
            payment: payment.to_string(),
            source: format!("payout:{policy}:{payout}"),
            payee: policyholder.to_string(),
            account_ref: String::new(),
            policy: Some(policy.clone()),
            claim: None,
            run: None,
            amount_minor,
            payable: Some(ic_core::PayableKind::Benefit),
        }),
    }
}

/// What the module does about one of the core's events. A pure function of
/// the event: the host shows every member the same events in the same order.
pub fn react(record: &ic_core::EventRecord) -> Vec<crate::Envelope> {
    let command = match &record.event {
        ic_core::Event::PaymentAuthorityCancelled { policy, claim } => crate::Command::PayerDeceased {
            policy: policy.clone(),
            claim: claim.clone(),
        },
        // Which fund each dollar of a receipt went to, so cash can follow it
        // between the funds' bank accounts (PAY-010).
        ic_core::Event::ReceiptApplied {
            receipt,
            allocated_minor,
            fund: Some(fund),
            ..
        } => crate::Command::ReceiptFunded {
            receipt: receipt.clone(),
            fund: fund.clone(),
            amount_minor: *allocated_minor,
        },
        ic_core::Event::ReceiptReversed { receipt, amount_minor, .. } => crate::Command::ReceiptUnfunded {
            receipt: receipt.clone(),
            amount_minor: *amount_minor,
        },
        // Claims released a payment: that is the instruction to pay it. A
        // benefit (`claim_paid`) and a provider's evidence or rehabilitation
        // cost (`claim_cost_paid`) both leave the policy's benefit payable.
        ic_core::Event::Module { module, kind, subject, detail } if module == "claims" && (kind == "claim_paid" || kind == "claim_cost_paid") => {
            let what = if kind == "claim_paid" { "claim" } else { "claim-cost" };
            let field = |name: &str| detail.get(name).cloned().unwrap_or_default();
            crate::Command::OfferPayment {
                waiting: Box::new(crate::intake::Waiting {
                    payment: field("payment_ref"),
                    source: format!("{what}:{subject}:{}", field("payment_ref")),
                    payee: field("payee"),
                    account_ref: field("account_ref"),
                    policy: Some(ic_core::PolicyId::from(field("policy").as_str())),
                    claim: Some(subject.clone()),
                    run: Some(field("run")).filter(|run| !run.is_empty()),
                    // An amount that cannot be read is refused by the
                    // module as not positive; nothing is guessed.
                    amount_minor: field("net_minor").parse().unwrap_or(0),
                    payable: Some(ic_core::PayableKind::Benefit),
                }),
            }
        }
        // Claims admitted a claim, or closed one: the funding forecast.
        ic_core::Event::Module { module, kind, subject, detail } if module == "claims" && kind == "claim_admitted" => crate::Command::ClaimAdmitted {
            claim: subject.clone(),
            amount_minor: detail.get("admitted_minor").and_then(|v| v.parse().ok()).unwrap_or(0),
        },
        ic_core::Event::Module { module, kind, subject, .. } if module == "claims" && kind == "claim_closed" => {
            crate::Command::ClaimClosed { claim: subject.clone() }
        }
        // Claims submitted a run, with the net total it released.
        ic_core::Event::Module { module, kind, subject, detail } if module == "claims" && kind == "run_submitted" => crate::Command::ClaimsRunSubmitted {
            run: subject.clone(),
            // Unreadable is unknown: the run is then confirmed as before,
            // when nothing of it is outstanding.
            submitted_minor: detail.get("submitted_minor").and_then(|v| v.parse().ok()).unwrap_or(0),
        },
        // A unit-linked payout is owed (a withdrawal, a surrender, a price
        // remediation), or owed again under a new reference after the bank
        // sent it back. It is paid to the policyholder: a payee of that
        // party, verified and screened like any other.
        ic_core::Event::Investment(ic_core::invest::InvestEvent::PayoutRaised {
            policy,
            policyholder,
            payout,
            amount_minor,
            ..
        }) => payout_waiting(policy, policyholder, payout, payout, *amount_minor),
        ic_core::Event::Investment(ic_core::invest::InvestEvent::PayoutReinstructed {
            policy,
            policyholder,
            payout,
            payment,
            amount_minor,
        }) => payout_waiting(policy, policyholder, payout, payment, *amount_minor),
        // A payout held while the owner is bankrupt is owed to whoever the
        // legal direction named, a payee verified and screened like any other.
        ic_core::Event::Investment(ic_core::invest::InvestEvent::PayoutDirected {
            policy,
            payee,
            payout,
            payment,
            amount_minor,
        }) => payout_waiting(policy, payee, payout, payment, *amount_minor),
        _ => return Vec::new(),
    };
    vec![crate::Envelope {
        // The event's ID, so an event offered twice is taken in once.
        key: format!("core-{}", record.id),
        actor: crate::Actor {
            id: "core-events".to_string(),
            roles: [crate::Role::Service].into(),
        },
        // An event carries no wall-clock time, and the core's business date
        // can run ahead of the wall clock. The module's clock never runs
        // backwards, so the epoch leaves it exactly where it was.
        at: chrono::DateTime::UNIX_EPOCH.naive_utc(),
        command,
    }]
}
