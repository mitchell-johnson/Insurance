//! Reinsurer accounts: what is owed each way under each treaty, the
//! statement and listing for a period, and one net settlement.
//!
//! Every amount booked here is also an instruction to the core to post it,
//! so the reinsurer's account in the ledger and the account here are the
//! same numbers by construction, and reconciliation proves it. The module
//! keeps the account by treaty, reinsurer, partition, kind and value date;
//! the line-by-line detail is the core's journal.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_core::PolicyId;
use ic_ledger::PartitionId;
use ic_money::Currency;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::cession::{cession_on, Cession, Segment};
use crate::ids::pairs;
use crate::machine::{Ctx, InstructionBody, Notice, PostingKind, Reinsurance, Reject};
use crate::math::share_of;
use crate::treaty::ListingField;
use crate::{ReinsurerId, RiskKey, TreatyId};

/// What a movement on a reinsurer's account is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineKind {
    CededPremium,
    FacultativePremium,
    Commission,
    RecoveryDue,
    AggregateRecovery,
    ProfitCommission,
    DisputeSettlement,
    OpeningBalance,
}

impl LineKind {
    /// +1 where the amount is owed to the reinsurer, -1 where the reinsurer
    /// owes it.
    pub fn sign(self) -> i64 {
        match self {
            LineKind::CededPremium | LineKind::FacultativePremium | LineKind::DisputeSettlement | LineKind::OpeningBalance => 1,
            LineKind::Commission | LineKind::RecoveryDue | LineKind::AggregateRecovery | LineKind::ProfitCommission => -1,
        }
    }

    fn posting(self) -> PostingKind {
        match self {
            LineKind::CededPremium | LineKind::FacultativePremium => PostingKind::CededPremium,
            LineKind::Commission => PostingKind::Commission,
            LineKind::RecoveryDue => PostingKind::RecoveryDue,
            LineKind::AggregateRecovery => PostingKind::AggregateRecovery,
            LineKind::ProfitCommission => PostingKind::ProfitCommission,
            LineKind::DisputeSettlement => PostingKind::DisputeSettlement,
            LineKind::OpeningBalance => PostingKind::OpeningBalance,
        }
    }
}

/// One cell of a reinsurer's account that no statement has taken yet.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Bucket {
    pub treaty: TreatyId,
    pub reinsurer: ReinsurerId,
    pub value_date: NaiveDate,
    pub partition: PartitionId,
    pub kind: LineKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatementStatus {
    Issued,
    Agreed { reference: String },
    Instructed,
    Settled { payment_ref: String },
}

/// A reinsurer's statement for one treaty period.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Statement {
    pub number: u32,
    pub treaty: TreatyId,
    pub reinsurer: ReinsurerId,
    pub currency: Currency,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub cut_on: NaiveDate,
    /// The event sequence of each partition the statement reflects.
    pub cut: BTreeMap<PartitionId, u64>,
    #[serde(with = "pairs")]
    pub totals: BTreeMap<LineKind, i64>,
    /// Net owed to the reinsurer by partition. This is what each partition
    /// sweeps to settlement clearing.
    pub by_partition: BTreeMap<PartitionId, i64>,
    /// The part of the net that corrects earlier periods.
    pub prior_period_minor: i64,
    /// Positive is owed to the reinsurer.
    pub net_minor: i64,
    pub listing_rows: u32,
    pub document: String,
    pub sha256: String,
    pub status: StatementStatus,
    /// Why settlement is on hold, if it is.
    #[serde(default)]
    pub held: Option<String>,
    /// A cession in this period changed after the listing was sent.
    #[serde(default)]
    pub supplementary_due: bool,
    #[serde(default)]
    pub supplementary_issued: u32,
}

/// The last profit commission review of a treaty with a reinsurer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PcState {
    pub reviewed_on: Option<NaiveDate>,
    pub profit_minor: i64,
    /// Profit commission due to date, all of it booked.
    pub due_minor: i64,
}

/// What a listing shows of one cession: the sum at risk and the reinsurer's
/// share of it.
type Listed = Option<(i64, i64)>;

fn listed(c: Option<&Cession>, treaty: &TreatyId, reinsurer: &ReinsurerId) -> Listed {
    let c = c.filter(|c| c.treaty.as_ref().is_some_and(|t| &t.id == treaty))?;
    let ceded = c.ceded_to(reinsurer);
    (ceded != 0).then_some((c.sum_at_risk_minor, ceded))
}

/// A stable reference for a listing row that says nothing about the life.
fn row_ref(key: &RiskKey) -> String {
    let mut h = Sha256::new();
    h.update(b"ic.reinsurance.listing.v1\0");
    h.update(key.to_string().as_bytes());
    hex::encode(&h.finalize()[..8])
}

pub(crate) fn sha256_hex(body: &str) -> String {
    hex::encode(Sha256::digest(body.as_bytes()))
}

impl Reinsurance {
    /// Put an amount on a reinsurer's account and tell the core to post it.
    /// The amount is of the thing itself: a premium, a commission, a
    /// recovery. A negative amount takes it back.
    pub(crate) fn book(
        &mut self,
        ctx: &mut Ctx,
        kind: LineKind,
        treaty: &TreatyId,
        reinsurer: &ReinsurerId,
        partition: PartitionId,
        policy: Option<PolicyId>,
        value_date: NaiveDate,
        amount_minor: i64,
        reference: String,
    ) {
        if amount_minor == 0 {
            return;
        }
        let bucket = Bucket {
            treaty: treaty.clone(),
            reinsurer: reinsurer.clone(),
            value_date,
            partition,
            kind,
        };
        let cell = self.pending.entry(bucket.clone()).or_default();
        *cell += amount_minor;
        if *cell == 0 {
            self.pending.remove(&bucket);
        }
        *self.booked.entry((treaty.clone(), reinsurer.clone(), kind)).or_default() += amount_minor;
        ctx.instruct(
            partition,
            InstructionBody::Post {
                kind: kind.posting(),
                treaty: treaty.clone(),
                reinsurer: reinsurer.clone(),
                policy,
                amount_minor,
                value_date,
                reference,
            },
        );
    }

    /// Everything booked for a treaty and reinsurer since the start, by kind.
    pub fn booked(&self, treaty: &TreatyId, reinsurer: &ReinsurerId, kind: LineKind) -> i64 {
        self.booked.get(&(treaty.clone(), reinsurer.clone(), kind)).copied().unwrap_or(0)
    }

    fn pending_for(&self, treaty: &TreatyId, reinsurer: &ReinsurerId) -> impl Iterator<Item = (&Bucket, &i64)> {
        let from = Bucket {
            treaty: treaty.clone(),
            reinsurer: reinsurer.clone(),
            value_date: NaiveDate::MIN,
            partition: 0,
            kind: LineKind::CededPremium,
        };
        let (treaty, reinsurer) = (treaty.clone(), reinsurer.clone());
        self.pending
            .range(from..)
            .take_while(move |(b, _)| b.treaty == treaty && b.reinsurer == reinsurer)
    }

    /// The balance with a reinsurer under a treaty that has not been
    /// settled: what no statement has taken yet, plus statements not yet
    /// paid. Positive is owed to the reinsurer.
    pub fn balance(&self, treaty: &TreatyId, reinsurer: &ReinsurerId) -> i64 {
        let open: i64 = self.pending_for(treaty, reinsurer).map(|(b, a)| b.kind.sign() * a).sum();
        let unsettled: i64 = self
            .statements
            .iter()
            .filter(|s| &s.treaty == treaty && &s.reinsurer == reinsurer && !matches!(s.status, StatementStatus::Settled { .. }))
            .map(|s| s.net_minor)
            .sum();
        open + unsettled
    }

    pub fn statement(&self, number: u32) -> Option<&Statement> {
        self.statements.get(number.checked_sub(1)? as usize)
    }

    fn statement_mut(&mut self, number: u32) -> Result<&mut Statement, Reject> {
        number
            .checked_sub(1)
            .and_then(|i| self.statements.get_mut(i as usize))
            .ok_or_else(|| Reject::new("unknown_statement", format!("no statement {number}")))
    }

    /// The listing for a treaty and reinsurer as at a date: one row per
    /// cession, carrying only what the treaty lets a listing say.
    pub fn listing(&self, treaty: &TreatyId, reinsurer: &ReinsurerId, period_start: NaiveDate, period_end: NaiveDate) -> Vec<serde_json::Value> {
        let fields = self.latest_treaty(treaty).map(|t| t.listing_fields.clone()).unwrap_or_default();
        let mut rows = Vec::new();
        for book in self.lives.values() {
            for (key, segments) in &book.recorded {
                let Some((sum_at_risk, ceded)) = listed(cession_on(segments, period_end), treaty, reinsurer) else {
                    continue;
                };
                let Some(risk) = book.risks.get(key) else { continue };
                let premium: i64 = self
                    .periods
                    .range((key.policy.clone(), period_start)..=(key.policy.clone(), period_end))
                    .flat_map(|(_, p)| p.charged.iter())
                    .filter(|(k, _)| &k.risk == key && &k.reinsurer == reinsurer && &k.treaty == treaty && k.kind != crate::premium::ChargeKind::Commission)
                    .map(|(_, a)| *a)
                    .sum();
                let mut row = serde_json::json!({
                    "ref": row_ref(key),
                    "policy": self.policies.get(&key.policy).map_or("", |p| p.number.as_str()),
                    "cover": key.cover,
                    "layer": key.layer,
                    "from": risk.start,
                    "sum_at_risk_minor": sum_at_risk,
                    "ceded_minor": ceded,
                    "premium_minor": premium,
                });
                let factors = self.factors.get(&(key.policy.clone(), key.life.clone()));
                for field in &fields {
                    let (name, value) = match field {
                        ListingField::Age => ("age", serde_json::json!(factors.and_then(|f| f.age_on(period_end)))),
                        ListingField::Sex => ("sex", serde_json::json!(factors.and_then(|f| f.sex))),
                        ListingField::Smoker => ("smoker", serde_json::json!(factors.and_then(|f| f.smoker))),
                    };
                    row[name] = value;
                }
                rows.push(row);
            }
        }
        rows.sort_by(|a, b| a["ref"].as_str().cmp(&b["ref"].as_str()));
        rows
    }

    /// Cut a reinsurer's statement for the treaty period that ends on
    /// `period_end`: everything on the account valued up to that day.
    pub(crate) fn cut_statement(&mut self, ctx: &mut Ctx, treaty: TreatyId, reinsurer: ReinsurerId, period_end: NaiveDate) -> Result<(), Reject> {
        let terms = self
            .latest_treaty(&treaty)
            .ok_or_else(|| Reject::new("unknown_treaty", format!("treaty {treaty} is not active")))?;
        let (currency, period_start) = (terms.currency, terms.statement.period.start_for(period_end));
        if period_end >= ctx.on {
            return Err(Reject::new("period_open", format!("the period to {period_end} has not ended")));
        }
        if self
            .statements
            .iter()
            .any(|s| s.treaty == treaty && s.reinsurer == reinsurer && s.period_end >= period_end)
        {
            return Err(Reject::new("statement_exists", format!("a statement to {period_end} or later has been cut")));
        }
        let taken: Vec<(Bucket, i64)> = self
            .pending_for(&treaty, &reinsurer)
            .filter(|(b, _)| b.value_date <= period_end)
            .map(|(b, a)| (b.clone(), *a))
            .collect();
        let mut totals: BTreeMap<LineKind, i64> = BTreeMap::new();
        let mut by_partition: BTreeMap<PartitionId, i64> = BTreeMap::new();
        let (mut net, mut prior) = (0, 0);
        for (b, amount) in &taken {
            let owed = b.kind.sign() * amount;
            *totals.entry(b.kind).or_default() += amount;
            *by_partition.entry(b.partition).or_default() += owed;
            net += owed;
            if b.value_date < period_start {
                prior += owed;
            }
            self.pending.remove(b);
        }
        by_partition.retain(|_, v| *v != 0);
        let number = self.statements.len() as u32 + 1;
        let rows = self.listing(&treaty, &reinsurer, period_start, period_end);
        let document = format!("reinsurance/{treaty}/{reinsurer}/statement-{number}.json");
        let body = serde_json::json!({
            "statement": number,
            "treaty": treaty,
            "reinsurer": reinsurer,
            "currency": currency,
            "period_start": period_start,
            "period_end": period_end,
            "cut_on": ctx.on,
            "totals": totals.iter().map(|(k, v)| (serde_json::json!(k), *v)).collect::<Vec<_>>(),
            "prior_period_minor": prior,
            "net_owed_to_reinsurer_minor": net,
            "listing": rows,
        })
        .to_string();
        let sha256 = sha256_hex(&body);
        self.log_release(
            ctx,
            format!("statement {number}"),
            treaty.clone(),
            reinsurer.clone(),
            "treaty statement and listing".into(),
            vec![document.clone()],
        );
        ctx.notices.push(Notice::Document {
            name: document.clone(),
            sha256: sha256.clone(),
            body,
        });
        self.statements.push(Statement {
            number,
            treaty,
            reinsurer,
            currency,
            period_start,
            period_end,
            cut_on: ctx.on,
            cut: self.checkpoints.clone(),
            totals,
            by_partition,
            prior_period_minor: prior,
            net_minor: net,
            listing_rows: rows.len() as u32,
            document,
            sha256,
            status: StatementStatus::Issued,
            held: None,
            supplementary_due: false,
            supplementary_issued: 0,
        });
        Ok(())
    }

    pub(crate) fn agree_statement(&mut self, number: u32, reference: String) -> Result<(), Reject> {
        let s = self.statement_mut(number)?;
        match &s.status {
            StatementStatus::Issued if !reference.is_empty() => {
                s.status = StatementStatus::Agreed { reference };
                Ok(())
            }
            StatementStatus::Issued => Err(Reject::new("reference_required", "an agreement names the reinsurer's confirmation")),
            StatementStatus::Agreed { reference: have } if *have == reference => Ok(()),
            _ => Err(Reject::new("statement_not_open", format!("statement {number} is already agreed or paid"))),
        }
    }

    /// Put settlement of a statement on hold, or release the hold.
    pub(crate) fn hold_settlement(&mut self, number: u32, reason: Option<String>) -> Result<(), Reject> {
        let s = self.statement_mut(number)?;
        if matches!(s.status, StatementStatus::Instructed | StatementStatus::Settled { .. }) {
            return Err(Reject::new("statement_not_open", format!("statement {number} has already gone for payment")));
        }
        if reason.as_ref().is_some_and(String::is_empty) {
            return Err(Reject::new("reason_required", "a hold says why"));
        }
        s.held = reason;
        Ok(())
    }

    pub(crate) fn instruct_settlement(&mut self, ctx: &mut Ctx, number: u32, prepared_by: &str, approved_by: &str) -> Result<(), Reject> {
        if prepared_by.is_empty() || approved_by.is_empty() || prepared_by == approved_by {
            return Err(Reject::new("maker_checker", "a settlement is prepared by one person and approved by another"));
        }
        let s = self
            .statement(number)
            .ok_or_else(|| Reject::new("unknown_statement", format!("no statement {number}")))?;
        if let Some(reason) = &s.held {
            return Err(Reject::new("settlement_held", format!("settlement of statement {number} is on hold: {reason}")));
        }
        if s.supplementary_due {
            return Err(Reject::new(
                "supplementary_due",
                "the listing has changed since it was sent: issue the supplementary listing first",
            ));
        }
        let needs_agreement = self.latest_treaty(&s.treaty).is_some_and(|t| t.statement.agree_before_payment);
        match &s.status {
            StatementStatus::Agreed { .. } => {}
            StatementStatus::Issued if !needs_agreement => {}
            StatementStatus::Issued => {
                return Err(Reject::new(
                    "not_agreed",
                    "the treaty requires the reinsurer to agree the statement before payment",
                ))
            }
            _ => return Err(Reject::new("statement_not_open", format!("statement {number} has already gone for payment"))),
        }
        let s = s.clone();
        for (partition, amount) in &s.by_partition {
            ctx.instruct(
                *partition,
                InstructionBody::Sweep {
                    treaty: s.treaty.clone(),
                    reinsurer: s.reinsurer.clone(),
                    statement: number,
                    cut_seq: s.cut.get(partition).copied().unwrap_or(0),
                    amount_minor: *amount,
                },
            );
        }
        ctx.notices.push(Notice::SettlementDue {
            statement: number,
            treaty: s.treaty,
            reinsurer: s.reinsurer,
            currency: s.currency,
            net_minor: s.net_minor,
        });
        self.statement_mut(number)?.status = StatementStatus::Instructed;
        Ok(())
    }

    /// The bank's confirmation of a settlement. A payment for another amount
    /// is recorded as an open item, never forced to fit.
    pub(crate) fn record_settlement(&mut self, ctx: &mut Ctx, number: u32, payment_ref: String, amount_minor: i64) -> Result<(), Reject> {
        let s = self.statement_mut(number)?;
        if s.status != StatementStatus::Instructed || payment_ref.is_empty() {
            return Err(Reject::new("not_instructed", format!("statement {number} is not waiting for a payment")));
        }
        let net = s.net_minor;
        let detail = (amount_minor != net).then(|| format!("statement {number} is for {net} but payment {payment_ref} was for {amount_minor}"));
        let settled = detail.is_none();
        if settled {
            s.status = StatementStatus::Settled { payment_ref };
        }
        self.set_exception(ctx, format!("settlement_mismatch:{number}"), "settlement_mismatch", detail);
        if settled {
            self.set_exception(ctx, format!("settlement_returned:{number}"), "settlement_returned", None);
        }
        Ok(())
    }

    /// The bank sent a settlement back. The statement is owed again and a
    /// person is told: nothing is paid a second time without one. A return
    /// of some other payment than the one recorded changes nothing.
    pub(crate) fn settlement_returned(&mut self, ctx: &mut Ctx, number: u32, payment_ref: &str, reason: &str) -> Result<(), Reject> {
        let s = self.statement_mut(number)?;
        match &s.status {
            StatementStatus::Settled { payment_ref: paid } if paid == payment_ref => s.status = StatementStatus::Instructed,
            StatementStatus::Instructed if !payment_ref.is_empty() => {}
            _ => {
                return Err(Reject::new(
                    "not_settled",
                    format!("statement {number} was not settled by payment {payment_ref}"),
                ))
            }
        }
        let detail = format!("payment {payment_ref} for statement {number} came back from the bank: {reason}");
        self.set_exception(ctx, format!("settlement_returned:{number}"), "settlement_returned", Some(detail));
        Ok(())
    }

    /// A cession changed from a date. Any statement already cut for a period
    /// the change reaches is owed a supplementary listing.
    pub(crate) fn flag_supplementary(&mut self, ctx: &mut Ctx, key: &RiskKey, from: NaiveDate, was: &[Segment]) {
        for i in 0..self.statements.len() {
            let s = &self.statements[i];
            if from > s.period_end {
                continue;
            }
            let before = listed(cession_on(was, s.period_end), &s.treaty, &s.reinsurer);
            let after = listed(self.cession(key, s.period_end), &s.treaty, &s.reinsurer);
            if before == after {
                continue;
            }
            let number = s.number;
            self.supplements.entry((number, key.clone())).or_insert(before);
            if !self.statements[i].supplementary_due {
                self.statements[i].supplementary_due = true;
                ctx.notices.push(Notice::SupplementaryListingDue { statement: number });
            }
        }
    }

    /// Issue the supplementary listing for a statement: each row that
    /// changed, as it was sent and as it is now.
    pub(crate) fn issue_supplementary(&mut self, ctx: &mut Ctx, number: u32, reason: String) -> Result<(), Reject> {
        let s = self
            .statement(number)
            .ok_or_else(|| Reject::new("unknown_statement", format!("no statement {number}")))?
            .clone();
        if !s.supplementary_due || reason.is_empty() {
            return Err(Reject::new("nothing_to_issue", "a supplementary listing needs a changed cession and a reason"));
        }
        let keys: Vec<(RiskKey, Listed)> = self
            .supplements
            .iter()
            .filter(|((n, _), _)| *n == number)
            .map(|((_, k), was)| (k.clone(), *was))
            .collect();
        let mut rows = Vec::new();
        for (key, was) in keys {
            let now = listed(self.cession(&key, s.period_end), &s.treaty, &s.reinsurer);
            self.supplements.remove(&(number, key.clone()));
            if was != now {
                rows.push(serde_json::json!({
                    "ref": row_ref(&key),
                    "policy": self.policies.get(&key.policy).map_or("", |p| p.number.as_str()),
                    "cover": key.cover,
                    "layer": key.layer,
                    "was": was.map(|(sar, ceded)| serde_json::json!({"sum_at_risk_minor": sar, "ceded_minor": ceded})),
                    "now": now.map(|(sar, ceded)| serde_json::json!({"sum_at_risk_minor": sar, "ceded_minor": ceded})),
                }));
            }
        }
        let issue = s.supplementary_issued + 1;
        let document = format!("reinsurance/{}/{}/statement-{number}-supplementary-{issue}.json", s.treaty, s.reinsurer);
        let body = serde_json::json!({
            "statement": number,
            "supplementary": issue,
            "treaty": s.treaty,
            "reinsurer": s.reinsurer,
            "period_end": s.period_end,
            "issued_on": ctx.on,
            "reason": reason,
            "rows": rows,
        })
        .to_string();
        self.log_release(
            ctx,
            format!("statement {number} supplementary {issue}"),
            s.treaty,
            s.reinsurer,
            "supplementary listing".into(),
            vec![document.clone()],
        );
        ctx.notices.push(Notice::Document {
            name: document,
            sha256: sha256_hex(&body),
            body,
        });
        let s = self.statement_mut(number)?;
        s.supplementary_due = false;
        s.supplementary_issued = issue;
        Ok(())
    }

    /// Work out profit commission to date and book the change since the
    /// last review. A loss carries forward because the sum runs from the
    /// start of the treaty.
    pub(crate) fn review_profit_commission(&mut self, ctx: &mut Ctx, treaty: TreatyId, reinsurer: ReinsurerId) -> Result<(), Reject> {
        let terms = self
            .latest_treaty(&treaty)
            .and_then(|t| t.profit_commission.clone())
            .ok_or_else(|| Reject::new("no_profit_commission", format!("treaty {treaty} has no profit commission terms")))?;
        let premium = self.booked(&treaty, &reinsurer, LineKind::CededPremium);
        let profit = premium
            - self.booked(&treaty, &reinsurer, LineKind::Commission)
            - self.booked(&treaty, &reinsurer, LineKind::RecoveryDue)
            - share_of(premium, terms.expense_allowance);
        let due = share_of(profit.max(0), terms.share);
        let state = self.profit_commission.entry((treaty.clone(), reinsurer.clone())).or_default();
        let delta = due - state.due_minor;
        *state = PcState {
            reviewed_on: Some(ctx.on),
            profit_minor: profit,
            due_minor: due,
        };
        let on = ctx.on;
        let partition = self.checkpoints.keys().next().copied().unwrap_or(0);
        self.book(
            ctx,
            LineKind::ProfitCommission,
            &treaty,
            &reinsurer,
            partition,
            None,
            on,
            delta,
            format!("profit commission to {on}"),
        );
        Ok(())
    }
}
