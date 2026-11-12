//! Payment and collection runs: prepare, check, approve, submit,
//! acknowledge, settle, return, close.
//!
//! A run is the unit people approve and a submission is the unit the bank
//! acknowledges. The person who prepares a run never approves it. A
//! submission the bank may already hold is never sent again under the same
//! ID: it is settled, or failed, and what is still unpaid goes in a new one.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use ic_core::PayableKind;
use ic_money::{Currency, Money};

use crate::machine::{Event, PaymentsState, Tx};
use crate::model::{
    About, AccountDetails, AuthorityStatus, Counterparty, Expected, Id, Instruction, Item, ItemStatus, NewItem, OutboxEntry, RefundDestination, RefundStatus,
    Reject, ReturnReason, Role, Run, RunStatus, RunType, Submission, SubmissionStatus,
};

fn no_run(id: &str) -> Reject {
    Reject::new("unknown_run", format!("there is no run {id}"))
}

fn wrong_state(what: &str) -> Reject {
    Reject::new("wrong_state", what)
}

impl PaymentsState {
    fn run(&self, id: &str) -> Result<&Run, Reject> {
        self.runs.get(id).ok_or_else(|| no_run(id))
    }

    /// The channel a run goes through and the bank account behind it.
    fn route_of(&self, run_type: RunType, urgent: bool, fund: &str, currency: Currency) -> Result<(Id, Id), Reject> {
        let cfg = self.cfg()?;
        let route = cfg
            .routes
            .get(&run_type)
            .ok_or_else(|| Reject::new("no_route", format!("{run_type:?} runs have no route to a bank")))?;
        let channel = if urgent {
            route
                .urgent
                .clone()
                .ok_or_else(|| Reject::new("no_urgent_channel", format!("{run_type:?} runs have no urgent channel")))?
        } else {
            match &route.secondary {
                Some(second) if self.redirected.contains(&run_type) => second.clone(),
                _ => route.primary.clone(),
            }
        };
        let account = cfg
            .account_for(&channel, run_type, fund, currency)
            .ok_or_else(|| Reject::new("no_account", format!("there is no {currency:?} account for fund {fund} on channel {channel}")))?
            .clone();
        Ok((channel, account))
    }

    pub(crate) fn prepare_run(
        &mut self,
        tx: &mut Tx<'_>,
        id: Id,
        run_type: RunType,
        fund: String,
        currency: Currency,
        value_date: NaiveDate,
        urgent: bool,
    ) -> Result<(), Reject> {
        tx.need(Role::Maker)?;
        if self.runs.contains_key(&id) {
            return Err(Reject::new("duplicate_run", format!("run {id} already exists")));
        }
        if urgent && run_type.inbound() {
            return Err(Reject::new("no_urgent_channel", "a collection is never urgent"));
        }
        if value_date < tx.now.date() {
            return Err(Reject::new("value_date_passed", "a run cannot be dated in the past"));
        }
        self.route_of(run_type, urgent, &fund, currency)?;
        self.runs.insert(
            id.clone(),
            Run {
                run_type,
                fund,
                currency,
                value_date,
                urgent,
                status: RunStatus::Prepared,
                makers: BTreeSet::from([tx.actor.id.clone()]),
                approvers: BTreeSet::new(),
                items: BTreeMap::new(),
                submissions: Vec::new(),
            },
        );
        tx.emit(Event::RunPrepared { run: id, run_type });
        Ok(())
    }

    /// Everything that must be true of one new item, given the run it joins.
    fn vet_item(&self, run: &Run, n: &NewItem) -> Result<(), Reject> {
        let bad = |message: &str| Err(Reject::new("invalid_item", format!("item {}: {message}", n.item)));
        if n.item.is_empty() || n.source.is_empty() || n.amount_minor <= 0 {
            return bad("an item has an ID, a source and a positive amount");
        }
        if self.items.contains_key(&n.item) {
            return Err(Reject::new("duplicate_item", format!("item {} has been used before", n.item)));
        }
        if let Some(other) = self.sources.get(&n.source) {
            return Err(Reject::new("duplicate_source", format!("{} is already being paid by item {other}", n.source)));
        }
        if run.run_type.inbound() {
            let Counterparty::Authority(a) = &n.counterparty else {
                return bad("a collection draws on an authority");
            };
            let authority = self
                .authorities
                .get(a)
                .ok_or_else(|| Reject::new("unknown_authority", format!("there is no authority {a}")))?;
            if n.policy.as_ref() != Some(&authority.policy) || n.payable.is_some() || n.refund.is_some() {
                return bad("a collection names the authority's policy and nothing else");
            }
            if let Some(first) = &n.represents {
                let same = self
                    .item(first)
                    .is_some_and(|i| i.new.counterparty == n.counterparty && i.new.amount_minor == n.amount_minor);
                if !self.represent.contains(first) || !same {
                    return Err(Reject::new(
                        "cannot_represent",
                        format!("{first} is not a dishonoured collection waiting to be presented again"),
                    ));
                }
            }
            return Ok(());
        }
        if n.represents.is_some() {
            return bad("only a collection is presented again");
        }
        if n.policy.is_some() != n.payable.is_some() {
            return bad("policy money names both the policy and the payable it discharges");
        }
        if (n.payable == Some(PayableKind::Refund)) != n.refund.is_some() {
            return bad("a payment from refund payable names its refund, and only such a payment does");
        }
        if let Counterparty::Payee(p) = &n.counterparty {
            if !self.payees.contains_key(p) {
                return Err(Reject::new("unknown_payee", format!("there is no payee {p}")));
            }
        }
        match &n.refund {
            None if matches!(n.counterparty, Counterparty::Authority(_)) => bad("only a refund is paid to the account of an authority"),
            None => Ok(()),
            Some(id) => {
                let refund = self
                    .refunds
                    .get(id)
                    .ok_or_else(|| Reject::new("unknown_refund", format!("there is no refund {id}")))?;
                let (to, with) = match &refund.destination {
                    RefundDestination::Source { authority } => (Counterparty::Authority(authority.clone()), RunType::PremiumRefunds),
                    RefundDestination::Payee { payee } => (Counterparty::Payee(payee.clone()), RunType::PremiumRefunds),
                    RefundDestination::WithClaim { payee, .. } => (Counterparty::Payee(payee.clone()), RunType::LumpSumClaims),
                };
                if refund.status != RefundStatus::Raised {
                    return Err(Reject::new("refund_not_owed", format!("refund {id} is not waiting to be paid")));
                }
                if Some(&refund.policy) != n.policy.as_ref() || refund.amount_minor != n.amount_minor || refund.currency != run.currency {
                    return bad("a refund is paid whole, on its own policy, in its own currency");
                }
                if to != n.counterparty || with != run.run_type {
                    return Err(Reject::new("wrong_destination", format!("refund {id} goes to {to:?} in a {with:?} run")));
                }
                Ok(())
            }
        }
    }

    pub(crate) fn item(&self, id: &str) -> Option<&Item> {
        self.runs.get(self.items.get(id)?)?.items.get(id)
    }

    pub(crate) fn add_run_items(&mut self, tx: &mut Tx<'_>, id: &str, items: Vec<NewItem>) -> Result<(), Reject> {
        tx.need(Role::Maker)?;
        let run = self.run(id)?;
        if !matches!(run.status, RunStatus::Prepared | RunStatus::Checked) {
            return Err(wrong_state("items join a run before it is approved"));
        }
        let mut seen = BTreeSet::new();
        for n in &items {
            self.vet_item(run, n)?;
            let fresh = seen.insert(("item", &n.item)) && seen.insert(("source", &n.source));
            let fresh = fresh && n.refund.as_ref().is_none_or(|r| seen.insert(("refund", r)));
            if !(fresh && n.represents.as_ref().is_none_or(|r| seen.insert(("represents", r)))) {
                return Err(Reject::new("duplicate_item", format!("item {} repeats something else in this request", n.item)));
            }
        }
        for n in &items {
            self.items.insert(n.item.clone(), id.to_string());
            self.sources.insert(n.source.clone(), n.item.clone());
            if let Some(refund) = n.refund.as_ref().and_then(|r| self.refunds.get_mut(r)) {
                refund.status = RefundStatus::InRun(n.item.clone());
            }
            if let Some(first) = &n.represents {
                self.represent.remove(first);
            }
        }
        let count = items.len();
        if let Some(run) = self.runs.get_mut(id) {
            run.status = RunStatus::Prepared;
            run.makers.insert(tx.actor.id.clone());
            for new in items {
                let item = Item {
                    new,
                    status: ItemStatus::Pending,
                    account: None,
                    submission: None,
                    was_settled: false,
                    released: false,
                };
                run.items.insert(item.new.item.clone(), item);
            }
        }
        tx.emit(Event::RunItemsAdded { run: id.to_string(), count });
        Ok(())
    }

    /// An item that will not be paid by this run gives back what it held:
    /// its source can be paid again, its refund is owed again, a collection
    /// it was presenting again can be presented by another.
    pub(crate) fn free_item(&mut self, n: &NewItem) {
        self.sources.remove(&n.source);
        if let Some(refund) = n.refund.as_ref().and_then(|r| self.refunds.get_mut(r)) {
            refund.status = RefundStatus::Raised;
        }
        if let Some(first) = &n.represents {
            self.represent.insert(first.clone());
        }
    }

    /// Take an item out of a run that has not been approved.
    pub(crate) fn drop_item(&mut self, run_id: &str, item_id: &str) -> Option<NewItem> {
        let run = self.runs.get_mut(run_id)?;
        let item = run.items.remove(item_id)?;
        // The bank never saw it, so its ID is free to be used again.
        self.items.remove(item_id);
        if run.status == RunStatus::Checked {
            run.status = RunStatus::Prepared;
        }
        self.free_item(&item.new);
        Some(item.new)
    }

    pub(crate) fn remove_run_item(&mut self, tx: &mut Tx<'_>, id: &str, item: &str) -> Result<(), Reject> {
        tx.need(Role::Maker)?;
        let run = self.run(id)?;
        let found = run
            .items
            .get(item)
            .ok_or_else(|| Reject::new("unknown_item", format!("run {id} has no item {item}")))?;
        // After approval the only thing that can leave is an item the bank
        // does not hold: taking it out pays less, never more.
        let open = matches!(run.status, RunStatus::Prepared | RunStatus::Checked) || (run.status == RunStatus::Approved && found.status == ItemStatus::Pending);
        if !open {
            return Err(wrong_state("an item leaves a run before approval, or when the bank does not hold it"));
        }
        self.drop_item(id, item);
        if let Some(run) = self.runs.get_mut(id) {
            run.makers.insert(tx.actor.id.clone());
        }
        tx.emit(Event::RunItemRemoved {
            run: id.to_string(),
            item: item.to_string(),
        });
        Ok(())
    }

    pub(crate) fn cancel_run(&mut self, tx: &mut Tx<'_>, id: &str) -> Result<(), Reject> {
        tx.need(Role::Maker)?;
        let run = self.run(id)?;
        if !matches!(run.status, RunStatus::Prepared | RunStatus::Checked) {
            return Err(wrong_state("a run can be cancelled until it is approved"));
        }
        let items: Vec<Id> = run.items.keys().cloned().collect();
        for item in items {
            self.drop_item(id, &item);
        }
        if let Some(run) = self.runs.get_mut(id) {
            run.status = RunStatus::Cancelled;
        }
        tx.emit(Event::RunCancelled { run: id.to_string() });
        Ok(())
    }

    /// The account an item pays or draws on, and why it cannot go, if it cannot.
    fn examine(&self, tx: &Tx<'_>, run: &Run, item: &Item, item_limit: i64, window: i64) -> (Option<AccountDetails>, Option<String>) {
        let (account, mut hold) = match &item.new.counterparty {
            Counterparty::Payee(p) => match self.payees.get(p) {
                Some(payee) => (Some(payee.account.clone()), payee.blocked(tx.now).map(str::to_string)),
                None => (None, Some("unknown_payee".to_string())),
            },
            Counterparty::Authority(a) => match self.authorities.get(a) {
                Some(authority) => {
                    let hold = if self.blocked_accounts.contains(&authority.account.number) {
                        Some("account_blocked")
                    } else if run.run_type.inbound() && authority.status != AuthorityStatus::Active {
                        Some("authority_not_active")
                    } else if authority.status == AuthorityStatus::Cancelled {
                        Some("source_unavailable")
                    } else {
                        None
                    };
                    (Some(authority.account.clone()), hold.map(str::to_string))
                }
                None => (None, Some("unknown_authority".to_string())),
            },
        };
        if hold.is_none() && item.new.amount_minor > item_limit {
            hold = Some("over_item_limit".to_string());
        }
        // The same account and amount close together in time is how a
        // payment made twice looks. A person decides.
        if hold.is_none() && !item.released {
            let twin = self
                .runs
                .values()
                .filter(|r| r.status != RunStatus::Cancelled && (r.value_date - run.value_date).num_days().abs() <= window)
                .any(|r| {
                    r.items.values().any(|other| {
                        other.new.item != item.new.item
                            && other.new.amount_minor == item.new.amount_minor
                            && !matches!(other.status, ItemStatus::Rejected(_) | ItemStatus::Returned(_))
                            && other.account.as_ref().or(self.account_of(&other.new.counterparty)).map(|a| &a.number) == account.as_ref().map(|a| &a.number)
                    })
                });
            if twin {
                hold = Some("possible_duplicate".to_string());
            }
        }
        (account, hold)
    }

    fn account_of(&self, counterparty: &Counterparty) -> Option<&AccountDetails> {
        match counterparty {
            Counterparty::Payee(p) => self.payees.get(p).map(|p| &p.account),
            Counterparty::Authority(a) => self.authorities.get(a).map(|a| &a.account),
        }
    }

    pub(crate) fn check_run(&mut self, tx: &mut Tx<'_>, id: &str) -> Result<(), Reject> {
        tx.need(Role::Maker)?;
        let run = self.run(id)?;
        if !matches!(run.status, RunStatus::Prepared | RunStatus::Checked) {
            return Err(wrong_state("a run is checked before it is approved"));
        }
        if run.items.is_empty() {
            return Err(Reject::new("empty_run", "there is nothing in the run"));
        }
        let cfg = self.cfg()?;
        let route = cfg
            .routes
            .get(&run.run_type)
            .ok_or_else(|| Reject::new("no_route", "the run's type has no route"))?;
        let window = i64::from(cfg.rules.duplicate_window_days);
        let findings: Vec<_> = run
            .items
            .iter()
            .map(|(item_id, item)| (item_id.clone(), self.examine(tx, run, item, route.item_limit_minor, window)))
            .collect();
        let going: i64 = findings
            .iter()
            .filter(|(_, (_, hold))| hold.is_none())
            .map(|(item_id, _)| run.items[item_id].new.amount_minor)
            .sum();
        if going > route.run_limit_minor {
            return Err(Reject::new(
                "over_run_limit",
                format!("the run totals {going}, over its limit of {}", route.run_limit_minor),
            ));
        }
        let held = findings.iter().filter(|(_, (_, hold))| hold.is_some()).count();
        if let Some(run) = self.runs.get_mut(id) {
            for (item_id, (account, hold)) in findings {
                if let Some(item) = run.items.get_mut(&item_id) {
                    item.account = account;
                    item.status = hold.map_or(ItemStatus::Pending, ItemStatus::Held);
                }
            }
            run.status = RunStatus::Checked;
            run.makers.insert(tx.actor.id.clone());
        }
        tx.emit(Event::RunChecked { run: id.to_string(), held });
        Ok(())
    }

    pub(crate) fn release_item(&mut self, tx: &mut Tx<'_>, id: &str, item_id: &str) -> Result<(), Reject> {
        tx.need(Role::Approver)?;
        let run = self.run(id)?;
        if run.makers.contains(&tx.actor.id) {
            return Err(Reject::new("same_person", "someone who prepared the run cannot release its holds"));
        }
        let item = run
            .items
            .get(item_id)
            .ok_or_else(|| Reject::new("unknown_item", format!("run {id} has no item {item_id}")))?;
        if run.status != RunStatus::Checked || item.status != ItemStatus::Held("possible_duplicate".to_string()) {
            return Err(Reject::new("not_releasable", "only a possible duplicate on a checked run can be released"));
        }
        if let Some(item) = self.runs.get_mut(id).and_then(|r| r.items.get_mut(item_id)) {
            item.released = true;
            item.status = ItemStatus::Pending;
        }
        tx.emit(Event::ItemReleased {
            run: id.to_string(),
            item: item_id.to_string(),
            by: tx.actor.id.clone(),
        });
        Ok(())
    }

    /// What the account will hold once everything already on its way has
    /// passed through: the last statement, what has settled since, and what
    /// is with the bank.
    pub fn available(&self, account: &str) -> Option<i64> {
        let book = self.books.get(account)?;
        book.statement_date?;
        let settled: i64 = book.expected.values().map(|e| e.amount_minor).sum();
        let with_bank: i64 = self
            .submissions
            .values()
            .filter(|s| s.account == account && s.settled_on.is_none() && !matches!(s.status, SubmissionStatus::Failed(_)))
            .filter_map(|s| self.runs.get(&s.run))
            .filter(|run| !run.run_type.inbound())
            .flat_map(|run| run.items.values())
            .filter(|i| matches!(i.status, ItemStatus::Submitted | ItemStatus::Acknowledged))
            .map(|i| i.new.amount_minor)
            .sum();
        Some(book.closing_minor + settled - with_bank)
    }

    /// The bank still takes work for `value_date` at this moment.
    fn in_time(&self, tx: &Tx<'_>, channel: &str, value_date: NaiveDate) -> Result<(), Reject> {
        let cfg = self.cfg()?;
        let cutoff = cfg.channels.get(channel).map(|c| c.cutoff);
        let today = tx.now.date();
        let open = cfg.is_business_day(value_date) && (today < value_date || (today == value_date && cutoff.is_some_and(|c| tx.now.time() <= c)));
        if open {
            Ok(())
        } else {
            Err(Reject::new("after_cutoff", format!("the bank no longer takes work for {value_date}")))
        }
    }

    /// Put every pending item of an approved run in a new submission.
    fn queue_submission(&mut self, tx: &mut Tx<'_>, id: &str, channel: Id, account: Id) {
        let Some(run) = self.runs.get_mut(id) else { return };
        let submission = format!("{id}-{}", run.submissions.len() + 1);
        let mut items = Vec::new();
        for (item_id, item) in &mut run.items {
            if item.status == ItemStatus::Pending {
                item.status = ItemStatus::Submitted;
                item.submission = Some(submission.clone());
                items.push(item_id.clone());
            }
        }
        run.submissions.push(submission.clone());
        self.submissions.insert(
            submission.clone(),
            Submission {
                run: id.to_string(),
                channel: channel.clone(),
                account,
                items,
                status: SubmissionStatus::Queued,
                region: None,
                begun_at: None,
                content_hash: None,
                bank_reference: None,
                settled_on: None,
            },
        );
        tx.emit(Event::SubmissionQueued {
            run: id.to_string(),
            submission,
            channel,
        });
    }

    pub(crate) fn approve_run(&mut self, tx: &mut Tx<'_>, id: &str) -> Result<(), Reject> {
        tx.need(Role::Approver)?;
        let run = self.run(id)?;
        let me = &tx.actor.id;
        if run.status != RunStatus::Checked {
            return Err(wrong_state("a run is approved after it has been checked"));
        }
        if run.makers.contains(me) {
            return Err(Reject::new("same_person", "someone who prepared the run cannot approve it"));
        }
        if run.approvers.contains(me) {
            return Err(Reject::new("already_approved", "each approval comes from a different person"));
        }
        if run.items.values().any(|i| matches!(i.status, ItemStatus::Held(_))) {
            return Err(Reject::new("items_held", "held items are released or removed before approval"));
        }
        let cfg = self.cfg()?;
        let route = cfg
            .routes
            .get(&run.run_type)
            .ok_or_else(|| Reject::new("no_route", "the run's type has no route"))?;
        let (channel, account) = self.route_of(run.run_type, run.urgent, &run.fund, run.currency)?;
        let needed = if run.urgent {
            // Out of hours there is no cutoff to meet, and two people who
            // are on call must both agree.
            if !cfg.rules.on_call.contains(me) {
                return Err(Reject::new("not_on_call", "an urgent run is approved by the people on call"));
            }
            usize::from(route.approvals).max(2)
        } else {
            self.in_time(tx, &channel, run.value_date)?;
            usize::from(route.approvals)
        };
        let total = run.live_total();
        let same_day: i64 = self
            .runs
            .values()
            .filter(|r| r.run_type == run.run_type && r.value_date == run.value_date && matches!(r.status, RunStatus::Approved | RunStatus::Closed))
            .map(Run::live_total)
            .sum();
        if same_day + total > route.day_limit_minor {
            return Err(Reject::new(
                "over_day_limit",
                format!("{:?} runs for {} would total {}", run.run_type, run.value_date, same_day + total),
            ));
        }
        if !run.run_type.inbound() {
            // Money is not released against a balance nobody has seen.
            let available = self
                .available(&account)
                .ok_or_else(|| Reject::new("balance_unknown", format!("no statement has been read for account {account}")))?;
            if available < total {
                return Err(Reject::new(
                    "insufficient_funds",
                    format!("account {account} will hold {available}, the run needs {total}"),
                ));
            }
        }
        let large = total >= cfg.rules.pre_advice_threshold_minor && !run.run_type.inbound();
        let value_date = run.value_date;
        let approved = run.approvers.len() + 1 >= needed;
        let by = me.clone();
        if let Some(run) = self.runs.get_mut(id) {
            run.approvers.insert(by.clone());
            if approved {
                run.status = RunStatus::Approved;
            }
        }
        tx.emit(Event::RunApproval {
            run: id.to_string(),
            by,
            approved,
        });
        if approved {
            if large {
                self.push_outbox(OutboxEntry {
                    key: format!("pre-advice-{id}"),
                    instruction: Instruction::Notify {
                        to: Role::Treasury,
                        subject: "large_payment_pre_advice".to_string(),
                        detail: format!("run {id} pays {total} from account {account} on {value_date}"),
                    },
                    about: None,
                });
            }
            self.queue_submission(tx, id, channel, account);
        }
        Ok(())
    }

    /// Send what is still unpaid in an approved run again, as a new
    /// submission with a new ID. The approval stands: nothing was added.
    pub(crate) fn resubmit(&mut self, tx: &mut Tx<'_>, id: &str, value_date: NaiveDate) -> Result<(), Reject> {
        tx.need(Role::Maker)?;
        let run = self.run(id)?;
        if run.status != RunStatus::Approved || !run.items.values().any(|i| i.status == ItemStatus::Pending) {
            return Err(wrong_state("only an approved run with unpaid items is sent again"));
        }
        if value_date < run.value_date {
            return Err(Reject::new("value_date_passed", "a run is never moved to an earlier date"));
        }
        let (channel, account) = self.route_of(run.run_type, run.urgent, &run.fund, run.currency)?;
        if !run.urgent {
            self.in_time(tx, &channel, value_date)?;
        }
        if let Some(run) = self.runs.get_mut(id) {
            run.value_date = value_date;
        }
        self.queue_submission(tx, id, channel, account);
        Ok(())
    }

    /// Send a type of run through the second bank, or back through the first.
    pub(crate) fn redirect_route(&mut self, tx: &mut Tx<'_>, run_type: RunType, secondary: bool) -> Result<(), Reject> {
        tx.need(Role::ConnectivityAdmin)?;
        let has_second = self.cfg()?.routes.get(&run_type).is_some_and(|r| r.secondary.is_some());
        if !has_second {
            return Err(Reject::new("no_secondary", format!("{run_type:?} runs have no second bank")));
        }
        if secondary {
            self.redirected.insert(run_type);
        } else {
            self.redirected.remove(&run_type);
        }
        // What no driver has taken yet follows the route. Anything a driver
        // has taken may be at the first bank already and stays there.
        let waiting: Vec<Id> = self
            .submissions
            .iter()
            .filter(|(_, s)| s.status == SubmissionStatus::Queued)
            .filter(|(_, s)| self.runs.get(&s.run).is_some_and(|r| r.run_type == run_type && !r.urgent))
            .map(|(id, _)| id.clone())
            .collect();
        for id in waiting {
            let run = &self.runs[&self.submissions[&id].run];
            let (channel, account) = self.route_of(run_type, false, &run.fund, run.currency)?;
            if let Some(s) = self.submissions.get_mut(&id) {
                s.channel = channel;
                s.account = account;
            }
        }
        tx.emit(Event::RouteRedirected { run_type, secondary });
        Ok(())
    }

    fn submission_mut(&mut self, id: &str) -> Result<&mut Submission, Reject> {
        self.submissions
            .get_mut(id)
            .ok_or_else(|| Reject::new("unknown_submission", format!("there is no submission {id}")))
    }

    /// A driver takes a submission to send from one region.
    pub(crate) fn begin_submission(&mut self, tx: &mut Tx<'_>, id: &str, region: String) -> Result<(), Reject> {
        tx.need(Role::Service)?;
        let today = tx.now.date();
        let s = self
            .submissions
            .get(id)
            .ok_or_else(|| Reject::new("unknown_submission", format!("there is no submission {id}")))?;
        if s.status != SubmissionStatus::Queued {
            return Err(wrong_state("a submission is taken once"));
        }
        let cfg = self.cfg()?;
        let endpoint = cfg.channels.get(&s.channel).and_then(|c| c.endpoints.get(&region));
        if !endpoint.is_some_and(|e| e.whitelisted && e.certificate_expires > today) {
            return Err(Reject::new(
                "region_not_provisioned",
                format!("{region} cannot reach channel {} today", s.channel),
            ));
        }
        let s = self.submission_mut(id)?;
        s.status = SubmissionStatus::Sending;
        s.region = Some(region.clone());
        s.begun_at = Some(tx.now);
        tx.emit(Event::SubmissionBegun {
            submission: id.to_string(),
            region,
        });
        Ok(())
    }

    pub(crate) fn record_submitted(&mut self, tx: &mut Tx<'_>, id: &str, content_hash: String, bank_reference: Option<String>) -> Result<(), Reject> {
        tx.need(Role::Service)?;
        let s = self.submission_mut(id)?;
        if s.status != SubmissionStatus::Sending {
            return Err(wrong_state("only a submission a driver has taken can be recorded as sent"));
        }
        s.status = SubmissionStatus::Sent;
        s.content_hash = Some(content_hash.clone());
        s.bank_reference = bank_reference;
        tx.emit(Event::SubmissionSent {
            submission: id.to_string(),
            content_hash,
        });
        Ok(())
    }

    /// The bank does not hold the submission (it refused the whole file, or
    /// confirmed that nothing arrived). Its items are unpaid again.
    fn fail_submission(&mut self, tx: &mut Tx<'_>, id: &str, reason: String) {
        let Some(s) = self.submissions.get_mut(id) else { return };
        s.status = SubmissionStatus::Failed(reason.clone());
        let (run, items) = (s.run.clone(), s.items.clone());
        if let Some(run) = self.runs.get_mut(&run) {
            for item in items {
                if let Some(item) = run.items.get_mut(&item) {
                    item.status = ItemStatus::Pending;
                    item.submission = None;
                }
            }
        }
        tx.emit(Event::SubmissionFailed {
            submission: id.to_string(),
            reason,
        });
    }

    pub(crate) fn record_not_received(&mut self, tx: &mut Tx<'_>, id: &str, reason: String) -> Result<(), Reject> {
        tx.need(Role::Service)?;
        if !matches!(self.submission_mut(id)?.status, SubmissionStatus::Queued | SubmissionStatus::Sending) {
            return Err(wrong_state("the bank has this submission"));
        }
        self.fail_submission(tx, id, reason);
        Ok(())
    }

    pub(crate) fn record_acknowledgement(&mut self, tx: &mut Tx<'_>, id: &str, refused: Option<String>, rejected: BTreeMap<Id, String>) -> Result<(), Reject> {
        tx.need(Role::Service)?;
        let s = self.submission_mut(id)?;
        // An acknowledgement proves the bank holds the file, even if the
        // driver that sent it never managed to say so.
        if !matches!(s.status, SubmissionStatus::Sending | SubmissionStatus::Sent) {
            return Err(wrong_state("only a submission at the bank is acknowledged"));
        }
        if let Some(reason) = refused {
            self.fail_submission(tx, id, reason);
            return Ok(());
        }
        if let Some(stranger) = rejected.keys().find(|item| !s.items.contains(item)) {
            return Err(Reject::new("unknown_item", format!("item {stranger} is not in submission {id}")));
        }
        s.status = SubmissionStatus::Acknowledged;
        let (run_id, items) = (s.run.clone(), s.items.clone());
        let mut freed = Vec::new();
        if let Some(run) = self.runs.get_mut(&run_id) {
            for item_id in &items {
                let Some(item) = run.items.get_mut(item_id) else { continue };
                match rejected.get(item_id) {
                    Some(reason) => {
                        item.status = ItemStatus::Rejected(reason.clone());
                        freed.push(item.new.clone());
                    }
                    None => item.status = ItemStatus::Acknowledged,
                }
            }
        }
        for n in &freed {
            self.free_item(n);
            self.failed_before_settlement(n);
            let reason = rejected.get(&n.item).cloned().unwrap_or_default();
            self.origin_returned(n, tx.now.date(), &reason);
        }
        tx.emit(Event::SubmissionAcknowledged {
            submission: id.to_string(),
            accepted: items.len() - rejected.len(),
            rejected: rejected.into_keys().collect(),
        });
        Ok(())
    }

    pub(crate) fn record_settlement(&mut self, tx: &mut Tx<'_>, id: &str, settled_on: NaiveDate) -> Result<(), Reject> {
        tx.need(Role::Service)?;
        let s = self.submission_mut(id)?;
        if s.status != SubmissionStatus::Acknowledged || s.settled_on.is_some() {
            return Err(wrong_state("a submission settles once, after it is acknowledged"));
        }
        s.settled_on = Some(settled_on);
        let (run_id, account, item_ids) = (s.run.clone(), s.account.clone(), s.items.clone());
        let Some(run) = self.runs.get_mut(&run_id) else { return Ok(()) };
        let (inbound, currency) = (run.run_type.inbound(), run.currency);
        let mut settled = Vec::new();
        for item_id in &item_ids {
            if let Some(item) = run.items.get_mut(item_id).filter(|i| i.status == ItemStatus::Acknowledged) {
                item.status = ItemStatus::Settled;
                item.was_settled = true;
                settled.push(item.new.clone());
            }
        }
        let mut total = 0;
        for n in settled {
            total += n.amount_minor;
            self.waiting.remove(&n.item);
            self.origin_settled(&n, settled_on);
            if let Some(refund) = n.refund.as_ref().and_then(|r| self.refunds.get_mut(r)) {
                refund.status = RefundStatus::Paid;
            }
            if let Some(policy) = n.policy.clone() {
                let amount = Money::new(n.amount_minor, currency);
                let (caller, command) = match n.payable {
                    Some(payable) => (
                        "payments",
                        ic_core::Command::SettlePayable {
                            policy,
                            payment: n.item.clone(),
                            payable,
                            refund: n.refund.clone(),
                            amount,
                            bank_account: account.clone(),
                        },
                    ),
                    // A settled collection is a receipt, applied the way
                    // every receipt is.
                    None => (
                        "cash-matching",
                        ic_core::Command::ApplyReceipt {
                            policy,
                            receipt: n.item.clone(),
                            amount,
                        },
                    ),
                };
                self.push_outbox(OutboxEntry {
                    key: format!("settle-{}", n.item),
                    instruction: Instruction::Core {
                        caller: caller.to_string(),
                        command: Box::new(command),
                    },
                    about: Some(About::Item(n.item.clone())),
                });
            }
            tx.emit(Event::ItemSettled {
                item: n.item,
                source: n.source,
                amount_minor: n.amount_minor,
            });
        }
        // The bank shows a settled file as one entry quoting the file's ID.
        if total != 0 {
            let amount_minor = if inbound { total } else { -total };
            self.books.entry(account).or_default().expected.insert(
                id.to_string(),
                Expected {
                    amount_minor,
                    since: settled_on,
                },
            );
        }
        tx.emit(Event::SubmissionSettled {
            submission: id.to_string(),
            total_minor: total,
        });
        Ok(())
    }

    /// The bank sent an item back: a payment that could not be delivered,
    /// or a collection that was dishonoured.
    pub(crate) fn record_return(&mut self, tx: &mut Tx<'_>, item_id: &str, reason: ReturnReason, returned_on: NaiveDate) -> Result<(), Reject> {
        tx.need(Role::Service)?;
        let run_id = self
            .items
            .get(item_id)
            .cloned()
            .ok_or_else(|| Reject::new("unknown_item", format!("there is no item {item_id}")))?;
        let item = self
            .item(item_id)
            .ok_or_else(|| Reject::new("unknown_item", format!("there is no item {item_id}")))?;
        if !matches!(item.status, ItemStatus::Acknowledged | ItemStatus::Settled) {
            return Err(wrong_state("only an item the bank accepted can come back"));
        }
        let (n, was_settled) = (item.new.clone(), item.was_settled);
        let run = &self.runs[&run_id];
        let (inbound, run_type) = (run.run_type.inbound(), run.run_type);
        let account = item.submission.as_ref().and_then(|s| self.submissions.get(s)).map(|s| s.account.clone());
        let threshold = self.cfg()?.rules.dishonours_before_suspension;
        if let Some(item) = self.runs.get_mut(&run_id).and_then(|r| r.items.get_mut(item_id)) {
            item.status = ItemStatus::Returned(reason.clone());
        }
        self.free_item(&n);
        // A second presentment that came back is the end of it.
        if let Some(first) = &n.represents {
            self.represent.remove(first);
        }
        if !was_settled {
            self.failed_before_settlement(&n);
        }
        self.origin_returned(&n, returned_on, &format!("{reason:?}"));
        if was_settled {
            // The core heard it settle, so the core hears it come back, and
            // the statement will show the money moving the other way.
            if let Some(policy) = n.policy.clone() {
                let (caller, command) = if inbound {
                    (
                        "cash-matching",
                        ic_core::Command::ReverseReceipt {
                            policy,
                            receipt: n.item.clone(),
                        },
                    )
                } else {
                    (
                        "payments",
                        ic_core::Command::ReturnPayment {
                            policy,
                            payment: n.item.clone(),
                        },
                    )
                };
                self.push_outbox(OutboxEntry {
                    key: format!("return-{}", n.item),
                    instruction: Instruction::Core {
                        caller: caller.to_string(),
                        command: Box::new(command),
                    },
                    about: Some(About::Item(n.item.clone())),
                });
            }
            if let Some(account) = account {
                let amount_minor = if inbound { -n.amount_minor } else { n.amount_minor };
                self.books.entry(account).or_default().expected.insert(
                    n.item.clone(),
                    Expected {
                        amount_minor,
                        since: returned_on,
                    },
                );
            }
        }
        match &n.counterparty {
            Counterparty::Authority(a) if inbound => {
                let dishonours = self.authorities.get_mut(a).map_or(0, |authority| {
                    authority.dishonours += 1;
                    authority.dishonours
                });
                let active = self.authorities.get(a).is_some_and(|x| x.status == AuthorityStatus::Active);
                if reason.account_unusable() {
                    if self.authorities.get(a).is_some_and(|x| x.status != AuthorityStatus::Cancelled) {
                        self.end_authority(tx, a, "account_unusable");
                    }
                } else if dishonours >= threshold && active {
                    if let Some(authority) = self.authorities.get_mut(a) {
                        authority.status = AuthorityStatus::Suspended;
                        authority.reason = Some("repeated_dishonour".to_string());
                    }
                    tx.emit(Event::AuthoritySuspended {
                        authority: a.clone(),
                        reason: "repeated_dishonour".to_string(),
                    });
                } else if reason == ReturnReason::InsufficientFunds && n.represents.is_none() && active {
                    // Presented once more, and only once.
                    self.represent.insert(n.item.clone());
                }
            }
            Counterparty::Payee(p) if reason.account_unusable() => {
                if let Some(payee) = self.payees.get_mut(p) {
                    payee.suspect = true;
                }
            }
            _ => {}
        }
        if !inbound {
            self.push_outbox(OutboxEntry {
                key: format!("returned-{}", n.item),
                instruction: Instruction::Notify {
                    to: Role::Maker,
                    subject: "payment_returned".to_string(),
                    detail: format!("{run_type:?} item {} for {} came back: {reason:?}", n.item, n.source),
                },
                about: None,
            });
        }
        tx.emit(Event::ItemReturned {
            item: n.item,
            source: n.source,
            amount_minor: n.amount_minor,
            reason,
        });
        Ok(())
    }

    pub(crate) fn close_run(&mut self, tx: &mut Tx<'_>, id: &str) -> Result<(), Reject> {
        tx.need(Role::Maker)?;
        let run = self.run(id)?;
        if run.status != RunStatus::Approved {
            return Err(wrong_state("a run closes after it has been approved and paid"));
        }
        // Submitted equals acknowledged plus rejected, and acknowledged
        // equals settled plus returned: nothing is left in between.
        if let Some((item, _)) = run.items.iter().find(|(_, i)| !i.status.terminal()) {
            return Err(Reject::new(
                "run_not_finished",
                format!("item {item} is neither settled, returned nor rejected"),
            ));
        }
        // A collection that is still settled when its run closes was
        // honoured: the authority's run of dishonours is over.
        let honoured: Vec<Id> = run
            .items
            .values()
            .filter(|i| run.run_type.inbound() && i.status == ItemStatus::Settled)
            .filter_map(|i| match &i.new.counterparty {
                Counterparty::Authority(a) => Some(a.clone()),
                Counterparty::Payee(_) => None,
            })
            .collect();
        for a in honoured {
            if let Some(authority) = self.authorities.get_mut(&a) {
                authority.dishonours = 0;
            }
        }
        if let Some(run) = self.runs.get_mut(id) {
            run.status = RunStatus::Closed;
        }
        tx.emit(Event::RunClosed { run: id.to_string() });
        Ok(())
    }
}
