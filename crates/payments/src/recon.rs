//! Reconciliation: every bank account, every day. Each statement line is
//! matched to something the module expected, or becomes an exception with
//! an owner until someone accountable resolves it.

use std::collections::BTreeSet;

use chrono::NaiveDate;

use crate::machine::{Event, PaymentsState, Tx};
use crate::model::{About, Exception, ExceptionKind, Id, Instruction, OutboxEntry, Purpose, Reject, Role, StatementLine, SubmissionStatus};

/// The outbox key prefix for money a reinsurer paid in.
const RECEIVED_FROM_REINSURER: &str = "reinsurance-received-";

/// The reinsurance statement a bank reference quotes, as `ri-stmt-<n>` in
/// any case, anywhere in the reference.
fn reinsurer_statement(reference: &str) -> Option<u64> {
    let lower = reference.to_ascii_lowercase();
    let at = lower.find("ri-stmt-")? + "ri-stmt-".len();
    let digits: String = lower[at..].chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

impl PaymentsState {
    /// The place only moves forward: a slower driver reporting an older
    /// place changes nothing.
    pub(crate) fn note_messages_read(&mut self, tx: &mut Tx<'_>, channel: Id, through: u64) -> Result<(), Reject> {
        tx.need(Role::Service)?;
        if !self.cfg()?.channels.contains_key(&channel) {
            return Err(Reject::new("unknown_channel", format!("there is no channel {channel}")));
        }
        let place = self.messages_read.entry(channel).or_default();
        *place = (*place).max(through);
        Ok(())
    }

    pub(crate) fn open_exception(&mut self, tx: &mut Tx<'_>, id: Id, exception: Exception) {
        tx.emit(Event::ExceptionOpened {
            exception: id.clone(),
            kind: exception.kind,
            amount_minor: exception.amount_minor,
        });
        self.exceptions.insert(id, exception);
    }

    pub(crate) fn record_statement(
        &mut self,
        tx: &mut Tx<'_>,
        account: &str,
        date: NaiveDate,
        opening_minor: i64,
        closing_minor: i64,
        lines: Vec<StatementLine>,
    ) -> Result<(), Reject> {
        tx.need(Role::Service)?;
        let cfg = self.cfg()?;
        let purpose = cfg
            .accounts
            .get(account)
            .map(|a| a.purpose)
            .ok_or_else(|| Reject::new("unknown_account", format!("there is no account {account}")))?;
        let currency = cfg.accounts[account].currency;
        let book = self.books.get(account).cloned().unwrap_or_default();
        if book.statement_date.is_some_and(|last| date <= last) {
            return Err(Reject::new(
                "statement_out_of_order",
                format!("a statement for {account} up to {date} has been read"),
            ));
        }
        let movement = lines.iter().try_fold(0i64, |sum, l| sum.checked_add(l.amount_minor));
        if movement.and_then(|m| opening_minor.checked_add(m)) != Some(closing_minor) {
            return Err(Reject::new(
                "statement_does_not_add_up",
                "opening balance plus the lines is not the closing balance",
            ));
        }
        let mut ids = BTreeSet::new();
        if lines.iter().any(|l| l.id.is_empty() || !ids.insert(&l.id)) {
            return Err(Reject::new("duplicate_line", "every statement line has its own ID"));
        }
        let today = tx.now.date();
        let exception = |kind, amount_minor, reference: &str| Exception {
            kind,
            account: account.to_string(),
            amount_minor,
            reference: reference.to_string(),
            opened_on: today,
            owner: None,
            resolution: None,
        };
        if book.statement_date.is_some() && book.closing_minor != opening_minor {
            let gap = opening_minor - book.closing_minor;
            self.open_exception(
                tx,
                format!("{account}-{date}-opening"),
                exception(ExceptionKind::BalanceBreak, gap, "opening balance"),
            );
        }
        let (mut matched, mut broken) = (0, 0);
        for line in &lines {
            let id = format!("{account}-{date}-{}", line.id);
            let expected = self.books.get_mut(account).and_then(|b| b.expected.remove(&line.reference));
            let reinsurer = (line.amount_minor > 0).then(|| reinsurer_statement(&line.reference)).flatten();
            match expected {
                Some(e) if e.amount_minor == line.amount_minor => {
                    matched += 1;
                    continue;
                }
                Some(e) => self.open_exception(
                    tx,
                    id,
                    exception(ExceptionKind::AmountMismatch, line.amount_minor - e.amount_minor, &line.reference),
                ),
                // A reinsurer paid what a statement says it owes, quoting
                // the statement. The reinsurance module is told; until it
                // records it, the money is unidentified.
                None if reinsurer.is_some() => {
                    self.open_exception(
                        tx,
                        id.clone(),
                        exception(ExceptionKind::UnidentifiedReceipt, line.amount_minor, &line.reference),
                    );
                    self.push_outbox(OutboxEntry {
                        key: format!("{RECEIVED_FROM_REINSURER}{id}"),
                        instruction: Instruction::Module {
                            module: "reinsurance".to_string(),
                            command: serde_json::json!({
                                "key": format!("payments:received:{id}"),
                                "caller": "payments",
                                "on": date,
                                "command": {"record_settlement": {"number": reinsurer, "payment_ref": line.reference, "amount_minor": -line.amount_minor, "bank_account": account}},
                            }),
                        },
                        about: Some(About::Exception(id)),
                    });
                }
                None if line.amount_minor > 0 && purpose == Purpose::PremiumCollection => {
                    // A customer paid by direct credit. Cash matching decides
                    // whose it is; until it does, the money is unidentified.
                    self.open_exception(
                        tx,
                        id.clone(),
                        exception(ExceptionKind::UnidentifiedReceipt, line.amount_minor, &line.reference),
                    );
                    // Cash matching applies it under the outbox key: that is
                    // the receipt the core names when it says which fund
                    // the money went to.
                    self.sweeps.land(&format!("match-{id}"), account, date);
                    self.push_outbox(OutboxEntry {
                        key: format!("match-{id}"),
                        instruction: Instruction::Match {
                            exception: id.clone(),
                            account: account.to_string(),
                            amount_minor: line.amount_minor,
                            currency,
                            reference: line.reference.clone(),
                            payer_account: line.payer_account.clone(),
                            received_on: date,
                        },
                        about: Some(About::Exception(id)),
                    });
                }
                None if line.amount_minor > 0 => self.open_exception(tx, id, exception(ExceptionKind::UnexplainedCredit, line.amount_minor, &line.reference)),
                None => self.open_exception(tx, id, exception(ExceptionKind::UnexplainedDebit, line.amount_minor, &line.reference)),
            }
            broken += 1;
        }
        let book = self.books.entry(account.to_string()).or_default();
        book.statement_date = Some(date);
        book.closing_minor = closing_minor;
        tx.emit(Event::StatementReconciled {
            account: account.to_string(),
            date,
            matched,
            exceptions: broken,
        });
        Ok(())
    }

    fn open(&mut self, id: &str) -> Result<&mut Exception, Reject> {
        match self.exceptions.get_mut(id) {
            Some(e) if e.resolution.is_none() => Ok(e),
            Some(_) => Err(Reject::new("exception_resolved", format!("exception {id} is already resolved"))),
            None => Err(Reject::new("unknown_exception", format!("there is no exception {id}"))),
        }
    }

    pub(crate) fn assign_exception(&mut self, tx: &mut Tx<'_>, id: &str, owner: String) -> Result<(), Reject> {
        tx.need(Role::Reconciler)?;
        if owner.is_empty() {
            return Err(Reject::new("invalid_owner", "an exception is owned by a named person"));
        }
        self.open(id)?.owner = Some(owner.clone());
        tx.emit(Event::ExceptionAssigned {
            exception: id.to_string(),
            owner,
        });
        Ok(())
    }

    pub(crate) fn resolve_exception(&mut self, tx: &mut Tx<'_>, id: &str, resolution: String) -> Result<(), Reject> {
        tx.need(Role::Reconciler)?;
        let me = tx.actor.id.clone();
        let e = self.open(id)?;
        if e.owner.as_ref() != Some(&me) {
            return Err(Reject::new("not_owner", "an exception is resolved by the person who owns it"));
        }
        if resolution.trim().is_empty() {
            return Err(Reject::new("invalid_resolution", "a resolution says what was done"));
        }
        e.resolution = Some(resolution.clone());
        tx.emit(Event::ExceptionResolved {
            exception: id.to_string(),
            resolution,
        });
        Ok(())
    }

    /// A driver finished an outbox entry, or the other side refused it.
    pub(crate) fn complete_instruction(&mut self, tx: &mut Tx<'_>, entry: u64, refused: Option<String>) -> Result<(), Reject> {
        tx.need(Role::Service)?;
        let done = self
            .outbox
            .remove(&entry)
            .ok_or_else(|| Reject::new("unknown_instruction", format!("outbox entry {entry} is not waiting")))?;
        match (done.about, refused) {
            (Some(About::RefundRaise(id)), refused) => self.refund_raised(tx, &id, refused),
            (Some(About::RefundRelease(id)), None) => self.refund_released(tx, &id),
            // Cash matching applied the receipt: it is identified.
            // So did the reinsurance module, for money a reinsurer paid.
            (Some(About::Exception(id)), None) => {
                let resolution = if done.key.starts_with(RECEIVED_FROM_REINSURER) {
                    "recorded by reinsurance against its statement"
                } else {
                    "applied by cash matching"
                };
                if let Some(e) = self.exceptions.get_mut(&id) {
                    e.resolution = Some(resolution.to_string());
                    tx.emit(Event::ExceptionResolved {
                        exception: id,
                        resolution: resolution.to_string(),
                    });
                }
            }
            // Nobody could say whose it is. It stays open for a person.
            (Some(About::Exception(_)), Some(_)) | (None, _) | (Some(About::Item(_)), None) => {}
            // The core refused something the bank has already done. The
            // ledger and the bank now disagree until a person sorts it out.
            (Some(About::Item(reference) | About::RefundRelease(reference)), Some(code)) => {
                let exception = Exception {
                    kind: ExceptionKind::CoreRefused,
                    account: String::new(),
                    amount_minor: 0,
                    reference: format!("{}: {code}", done.key),
                    opened_on: tx.now.date(),
                    owner: None,
                    resolution: None,
                };
                self.open_exception(tx, format!("core-{reference}-{entry}"), exception);
            }
        }
        tx.emit(Event::InstructionCompleted { key: done.key });
        Ok(())
    }
}

/// The most items one report to the financial close carries. The count and
/// the total always cover everything.
const MAX_REPORTED: usize = 200;

impl PaymentsState {
    /// The month-end question from the financial close: has every bank
    /// account's cash been accounted for as at this day? Green when no
    /// reconciliation break opened on or before it is still open and no
    /// submission for a run valued on or before it is still without the
    /// bank's settlement. Each item says what is outstanding and where.
    pub fn fund_cash_settlement(&self, as_at: NaiveDate) -> serde_json::Value {
        let mut items = Vec::new();
        for (id, e) in self.exceptions.iter().filter(|(_, e)| e.resolution.is_none() && e.opened_on <= as_at) {
            items.push(serde_json::json!({
                "exception": id,
                "kind": e.kind,
                "account": e.account,
                "fund": self.config.as_ref().and_then(|c| c.accounts.get(&e.account)).map(|a| a.fund.clone()),
                "amount_minor": e.amount_minor,
                "since": e.opened_on,
                "owner": e.owner,
            }));
        }
        for (id, s) in &self.submissions {
            let outstanding = s.settled_on.is_none() && !matches!(s.status, SubmissionStatus::Failed(_));
            let Some(run) = self.runs.get(&s.run).filter(|r| outstanding && r.value_date <= as_at) else {
                continue;
            };
            items.push(serde_json::json!({
                "submission": id,
                "status": s.status,
                "account": s.account,
                "fund": run.fund,
                "value_date": run.value_date,
            }));
        }
        items.extend(self.unswept(as_at));
        let count = items.len();
        items.truncate(MAX_REPORTED);
        serde_json::json!({ "green": count == 0, "as_at": as_at, "items": items, "count": count })
    }

    /// Money received that nobody has identified, oldest first, for the
    /// financial close's aged-items check. `action` is the owner's name once
    /// someone has taken the item, so an item nobody owns shows as such.
    pub fn aged_unidentified_receipts(&self, as_at: NaiveDate) -> serde_json::Value {
        let mut open: Vec<(&Id, &Exception)> = self
            .exceptions
            .iter()
            .filter(|(_, e)| e.kind == ExceptionKind::UnidentifiedReceipt && e.resolution.is_none() && e.opened_on <= as_at)
            .collect();
        open.sort_by_key(|(id, e)| (e.opened_on, (*id).clone()));
        let total: i64 = open.iter().map(|(_, e)| e.amount_minor).sum();
        let count = open.len();
        let items: Vec<serde_json::Value> = open
            .into_iter()
            .take(MAX_REPORTED)
            .map(|(id, e)| {
                serde_json::json!({
                    "party": id,
                    "reason": format!("unidentified receipt on {}: {}", e.account, e.reference),
                    "amount_minor": e.amount_minor,
                    "since": e.opened_on,
                    "owner": e.owner,
                    "action": e.owner.as_ref().map(|owner| format!("assigned to {owner}")),
                })
            })
            .collect();
        serde_json::json!({ "kind": "unidentified_receipts", "as_at": as_at, "items": items, "total_minor": total, "count": count })
    }
}
