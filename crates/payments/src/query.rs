//! Questions the module answers from its state. Queries never change it.

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

fn found<T: serde::Serialize>(what: Option<T>) -> Result<Value, String> {
    serde_json::to_value(what).map_err(|e| e.to_string())
}

/// Answer one question. This is the function the module host calls.
pub fn answer(state: &PaymentsState, question: Value) -> Result<Value, String> {
    match text(&question, "q")? {
        "submission_in_progress" => Ok(json!(state.submission_in_progress())),
        "outbox" => found(Some(&state.outbox)),
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
        "refund" => found(state.refunds.get(text(&question, "refund")?)),
        "represent" => found(Some(&state.represent)),
        "reconciliation" => found(state.reconciliation(text(&question, "account")?, day(&question)?)),
        "readiness" => found(Some(state.readiness(day(&question)?))),
        "trail" => Ok(json!({"events": state.event_seq, "chain": state.event_chain})),
        other => Err(format!("unknown question {other}")),
    }
}
