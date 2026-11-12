//! Recoveries: the reinsurers' share of each claim, from the cession record
//! as at the insured event.
//!
//! The claims module sends facts about a claim as they happen. The expected
//! recovery follows the claim's reserve and is posted as reinsurance
//! recoverable; it becomes due from the reinsurer as the claim is paid. Both
//! are worked out again whenever the claim or the cession behind it changes,
//! and only the difference is posted.
//!
//! When an admission is reversed, recoveries already taken are not reversed.
//! They are raised as disputed items on the reinsurer's account for people
//! to settle under the treaty.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{Days, NaiveDate};
use ic_core::{LifeId, PolicyId};
use ic_ledger::PartitionId;
use serde::{Deserialize, Serialize};

use crate::account::LineKind;
use crate::ids::pairs;
use crate::machine::{Ctx, InstructionBody, Notice, PostingKind, Reinsurance, Reject};
use crate::math::{pro_rata, split_exact};
use crate::treaty::AggregateKind;
use crate::{ClaimId, ReinsurerId, RiskKey, TreatyId};

/// A fact about a claim, from the claims module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimFact {
    Reserved {
        claim: ClaimId,
        policy: PolicyId,
        cover: String,
        life: LifeId,
        event_date: NaiveDate,
        gross_reserve_minor: i64,
        /// Paid by period, like income protection, not as one sum.
        #[serde(default)]
        periodic: bool,
        /// The event many claims belong to, for catastrophe cover.
        #[serde(default)]
        catastrophe_event: Option<String>,
    },
    ReserveChanged {
        claim: ClaimId,
        gross_reserve_minor: i64,
    },
    Admitted {
        claim: ClaimId,
        admitted_minor: i64,
    },
    Paid {
        claim: ClaimId,
        payment_ref: String,
        amount_minor: i64,
        #[serde(default)]
        period_end: Option<NaiveDate>,
        #[serde(default)]
        ex_gratia: bool,
    },
    Closed {
        claim: ClaimId,
        #[serde(default)]
        outcome: Option<String>,
    },
    AdmissionReversed {
        claim: ClaimId,
        decided_on: NaiveDate,
        reason_code: String,
        #[serde(default)]
        cause: Option<String>,
    },
    OverpaymentWrittenOff {
        claim: ClaimId,
        amount_minor: i64,
        #[serde(default)]
        insurer_error: bool,
    },
}

impl ClaimFact {
    pub fn claim(&self) -> &ClaimId {
        match self {
            ClaimFact::Reserved { claim, .. }
            | ClaimFact::ReserveChanged { claim, .. }
            | ClaimFact::Admitted { claim, .. }
            | ClaimFact::Paid { claim, .. }
            | ClaimFact::Closed { claim, .. }
            | ClaimFact::AdmissionReversed { claim, .. }
            | ClaimFact::OverpaymentWrittenOff { claim, .. } => claim,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStatus {
    #[default]
    Open,
    Closed,
    /// The admission was reversed. Nothing more is recovered.
    Reversed,
}

/// What the module holds about one claim: amounts and dates, nothing about
/// the person or the condition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimBook {
    pub policy: PolicyId,
    pub cover: String,
    pub life: LifeId,
    pub event_date: NaiveDate,
    pub periodic: bool,
    pub catastrophe_event: Option<String>,
    pub reserve_minor: i64,
    pub admitted_minor: Option<i64>,
    pub paid_minor: i64,
    pub ex_gratia_minor: i64,
    pub payments: BTreeSet<String>,
    pub status: ClaimStatus,
    /// Expected recoveries posted as recoverable, by treaty and reinsurer.
    #[serde(with = "pairs")]
    pub recoverable: BTreeMap<(TreatyId, ReinsurerId), i64>,
    /// Recoveries billed to the reinsurer.
    #[serde(with = "pairs")]
    pub due: BTreeMap<(TreatyId, ReinsurerId), i64>,
    pub notified: BTreeSet<TreatyId>,
    /// Treaties under which the reinsurers have a say, and by when.
    pub consent_due: BTreeMap<TreatyId, NaiveDate>,
    pub consent: BTreeMap<ReinsurerId, (bool, String)>,
    /// What this claim adds to each aggregate treaty's retained loss.
    pub aggregate: BTreeMap<TreatyId, i64>,
}

impl ClaimBook {
    fn gross(&self) -> i64 {
        self.reserve_minor
            .max(self.admitted_minor.unwrap_or(0))
            .max(self.paid_minor + self.ex_gratia_minor)
    }
}

/// Where a claim's reinsurers stand on consent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsentStatus {
    NotRequired,
    Waiting { due_by: NaiveDate },
    Agreed,
    Refused,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    pub repay_minor: i64,
    pub agreement_ref: String,
    pub prepared_by: String,
    pub approved_by: String,
    pub on: NaiveDate,
}

/// A recovery already taken on a claim whose admission was reversed. It
/// stays on the reinsurer's account until people settle it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisputedItem {
    pub claim: ClaimId,
    pub treaty: TreatyId,
    pub reinsurer: ReinsurerId,
    pub policy: PolicyId,
    pub partition: PartitionId,
    pub amount_minor: i64,
    pub raised_on: NaiveDate,
    pub decided_on: NaiveDate,
    pub reason_code: String,
    pub cause: Option<String>,
    /// The treaty clauses the item is raised under.
    pub clauses: Vec<String>,
    /// What the insurer has written off against the claimant, for the
    /// conversation about whether the reinsurer shares the cost.
    #[serde(default)]
    pub written_off_minor: i64,
    #[serde(default)]
    pub insurer_error: bool,
    #[serde(default)]
    pub resolution: Option<Resolution>,
}

/// The reference of a disputed item.
pub fn dispute_reference(claim: &ClaimId, treaty: &TreatyId, reinsurer: &ReinsurerId) -> String {
    format!("DSP/{claim}/{treaty}/{reinsurer}")
}

type ByReinsurer = BTreeMap<(TreatyId, ReinsurerId), i64>;

impl Reinsurance {
    pub fn claim(&self, id: &ClaimId) -> Option<&ClaimBook> {
        self.claims.get(id)
    }

    pub fn dispute(&self, reference: &str) -> Option<&DisputedItem> {
        self.disputes.get(reference)
    }

    fn claim_keys(&self, book: &ClaimBook) -> Vec<RiskKey> {
        self.policy_risks(&book.policy)
            .into_iter()
            .filter(|k| k.cover == book.cover && k.life == book.life)
            .collect()
    }

    /// The reinsurers' shares of an amount on a claim. `normal` is paid or
    /// reserved under the policy's terms; `ex_gratia` counts only where the
    /// treaty says so.
    fn recoveries(&self, book: &ClaimBook, normal: i64, ex_gratia: i64) -> ByReinsurer {
        let mut out = ByReinsurer::new();
        let day_before = book.event_date.checked_sub_days(Days::new(1)).unwrap_or(book.event_date);
        let cessions: Vec<_> = self
            .claim_keys(book)
            .iter()
            .filter_map(|k| self.cession(k, book.event_date).or_else(|| self.cession(k, day_before)))
            .collect();
        let total: i64 = cessions.iter().map(|c| c.sum_at_risk_minor).sum();
        if total <= 0 {
            return out;
        }
        // Per treaty: what is retained and ceded across the layers claimed on.
        let mut by_treaty: BTreeMap<&crate::TreatyRef, (i64, i64, BTreeMap<&ReinsurerId, i64>, BTreeMap<&ReinsurerId, i64>)> = BTreeMap::new();
        for c in &cessions {
            let Some(tref) = &c.treaty else { continue };
            let entry = by_treaty.entry(tref).or_default();
            entry.0 += c.sum_at_risk_minor;
            entry.1 += c.retained_minor;
            for (r, a) in &c.ceded {
                *entry.2.entry(r).or_default() += a;
            }
            for f in &c.facultative {
                *entry.3.entry(&f.reinsurer).or_default() += f.amount_minor;
            }
        }
        for (tref, (sum_at_risk, retained, ceded, facultative)) in by_treaty {
            let Some(treaty) = self.treaty(tref) else { continue };
            let amount = normal + if treaty.claims.ex_gratia_recoverable { ex_gratia } else { 0 };
            let mut add = |r: &ReinsurerId, v: i64| {
                if v != 0 {
                    *out.entry((tref.id.clone(), r.clone())).or_default() += v;
                }
            };
            if treaty.basis.proportional() || book.periodic {
                for (r, a) in &ceded {
                    add(r, pro_rata(amount, *a, total));
                }
            } else {
                // Excess of loss: the cost above the retention, up to what
                // was ceded, shared as the cession was.
                let on_treaty = pro_rata(amount, sum_at_risk, total);
                let ceded_total: i64 = ceded.values().sum();
                let recover = (on_treaty - retained).clamp(0, ceded_total.max(0));
                let weights: Vec<i64> = ceded.values().copied().collect();
                for (r, v) in ceded.keys().zip(split_exact(recover, &weights)) {
                    add(r, v);
                }
            }
            for (r, a) in &facultative {
                add(r, pro_rata(amount, *a, total));
            }
        }
        out
    }

    pub(crate) fn claim_fact(&mut self, ctx: &mut Ctx, fact: ClaimFact) -> Result<(), Reject> {
        let id = fact.claim().clone();
        if let ClaimFact::Reserved {
            policy,
            cover,
            life,
            event_date,
            gross_reserve_minor,
            periodic,
            catastrophe_event,
            ..
        } = fact
        {
            if gross_reserve_minor < 0 {
                return Err(Reject::new("invalid_amount", "a reserve cannot be negative"));
            }
            match self.claims.get_mut(&id) {
                Some(have) if have.policy == policy && have.cover == cover && have.life == life && have.event_date == event_date => {
                    have.reserve_minor = gross_reserve_minor
                }
                Some(_) => {
                    return Err(Reject::new(
                        "claim_exists",
                        format!("claim {id} is already recorded against another cover or date"),
                    ))
                }
                None => {
                    self.policy_claims.entry(policy.clone()).or_default().insert(id.clone());
                    self.claims.insert(
                        id.clone(),
                        ClaimBook {
                            policy,
                            cover,
                            life,
                            event_date,
                            periodic,
                            catastrophe_event,
                            reserve_minor: gross_reserve_minor,
                            admitted_minor: None,
                            paid_minor: 0,
                            ex_gratia_minor: 0,
                            payments: BTreeSet::new(),
                            status: ClaimStatus::Open,
                            recoverable: BTreeMap::new(),
                            due: BTreeMap::new(),
                            notified: BTreeSet::new(),
                            consent_due: BTreeMap::new(),
                            consent: BTreeMap::new(),
                            aggregate: BTreeMap::new(),
                        },
                    );
                }
            }
            self.sync_claim(ctx, &id);
            return Ok(());
        }
        let unknown = || Reject::new("unknown_claim", format!("no claim {id}"));
        let book = self.claims.get_mut(&id).ok_or_else(unknown)?;
        match fact {
            // A claim reserved for the first time was taken in above.
            ClaimFact::Reserved { gross_reserve_minor, .. } | ClaimFact::ReserveChanged { gross_reserve_minor, .. } => {
                if gross_reserve_minor < 0 {
                    return Err(Reject::new("invalid_amount", "a reserve cannot be negative"));
                }
                book.reserve_minor = gross_reserve_minor;
            }
            ClaimFact::Admitted { admitted_minor, .. } => {
                if admitted_minor < 0 {
                    return Err(Reject::new("invalid_amount", "an admitted amount cannot be negative"));
                }
                book.admitted_minor = Some(admitted_minor);
            }
            ClaimFact::Paid {
                payment_ref,
                amount_minor,
                ex_gratia,
                ..
            } => {
                if book.payments.contains(&payment_ref) {
                    return Ok(());
                }
                let paid = if ex_gratia { &mut book.ex_gratia_minor } else { &mut book.paid_minor };
                if payment_ref.is_empty() || *paid + amount_minor < 0 {
                    return Err(Reject::new(
                        "invalid_amount",
                        "a payment needs a reference, and payments cannot total less than nothing",
                    ));
                }
                *paid += amount_minor;
                book.payments.insert(payment_ref);
            }
            ClaimFact::Closed { .. } => {
                if book.status == ClaimStatus::Open {
                    book.status = ClaimStatus::Closed;
                }
            }
            ClaimFact::AdmissionReversed {
                decided_on,
                reason_code,
                cause,
                ..
            } => {
                if book.status != ClaimStatus::Reversed {
                    book.status = ClaimStatus::Reversed;
                    self.raise_disputes(ctx, &id, decided_on, &reason_code, cause);
                }
            }
            ClaimFact::OverpaymentWrittenOff {
                amount_minor, insurer_error, ..
            } => {
                for item in self.disputes.values_mut().filter(|d| d.claim == id) {
                    item.written_off_minor += amount_minor;
                    item.insurer_error |= insurer_error;
                }
                return Ok(());
            }
        }
        self.sync_claim(ctx, &id);
        Ok(())
    }

    /// Recoveries already billed on a claim become disputed items. They are
    /// never reversed automatically.
    fn raise_disputes(&mut self, ctx: &mut Ctx, id: &ClaimId, decided_on: NaiveDate, reason_code: &str, cause: Option<String>) {
        let Some(book) = self.claims.get(id) else { return };
        let partition = self.policies.get(&book.policy).map_or(0, |p| p.partition);
        let items: Vec<(String, DisputedItem)> = book
            .due
            .iter()
            .filter(|(_, amount)| **amount > 0)
            .map(|((treaty, reinsurer), amount)| {
                let clauses = self
                    .latest_treaty(treaty)
                    .map(|t| {
                        [
                            ("errors and omissions", &t.clauses.errors_and_omissions),
                            ("claims cooperation", &t.clauses.claims_cooperation),
                        ]
                    })
                    .into_iter()
                    .flatten()
                    .filter(|(_, c)| !c.is_empty())
                    .map(|(name, c)| format!("{name}: {c}"))
                    .collect();
                let item = DisputedItem {
                    claim: id.clone(),
                    treaty: treaty.clone(),
                    reinsurer: reinsurer.clone(),
                    policy: book.policy.clone(),
                    partition,
                    amount_minor: *amount,
                    raised_on: ctx.on,
                    decided_on,
                    reason_code: reason_code.to_string(),
                    cause: cause.clone(),
                    clauses,
                    written_off_minor: 0,
                    insurer_error: false,
                    resolution: None,
                };
                (dispute_reference(id, treaty, reinsurer), item)
            })
            .collect();
        for (reference, item) in items {
            ctx.notices.push(Notice::DisputedItemRaised {
                claim: id.clone(),
                reinsurer: item.reinsurer.clone(),
                reference: reference.clone(),
            });
            let detail = format!(
                "{} recovered from {} under {} on claim {id}, whose admission was reversed on {decided_on}; to be settled with the reinsurer",
                item.amount_minor, item.reinsurer, item.treaty
            );
            self.set_exception(ctx, format!("disputed_item:{reference}"), "disputed_item", Some(detail));
            self.disputes.insert(reference, item);
        }
    }

    pub(crate) fn resolve_dispute(
        &mut self,
        ctx: &mut Ctx,
        reference: &str,
        repay_minor: i64,
        agreement_ref: String,
        prepared_by: &str,
        approved_by: &str,
    ) -> Result<(), Reject> {
        let item = self
            .disputes
            .get(reference)
            .ok_or_else(|| Reject::new("unknown_dispute", format!("no disputed item {reference}")))?;
        let resolution = Resolution {
            repay_minor,
            agreement_ref,
            prepared_by: prepared_by.to_string(),
            approved_by: approved_by.to_string(),
            on: ctx.on,
        };
        match &item.resolution {
            Some(have) if have.repay_minor == repay_minor && have.agreement_ref == resolution.agreement_ref => return Ok(()),
            Some(_) => return Err(Reject::new("already_resolved", format!("{reference} has been settled"))),
            None => {}
        }
        if prepared_by.is_empty() || approved_by.is_empty() || prepared_by == approved_by {
            return Err(Reject::new("maker_checker", "a disputed item is settled by one person and approved by another"));
        }
        if resolution.agreement_ref.is_empty() || repay_minor < 0 || repay_minor > item.amount_minor {
            return Err(Reject::new(
                "invalid_resolution",
                "a settlement names the agreement and repays between nothing and the amount in dispute",
            ));
        }
        let (treaty, reinsurer, partition, policy) = (item.treaty.clone(), item.reinsurer.clone(), item.partition, item.policy.clone());
        let on = ctx.on;
        self.book(
            ctx,
            LineKind::DisputeSettlement,
            &treaty,
            &reinsurer,
            partition,
            Some(policy),
            on,
            repay_minor,
            reference.to_string(),
        );
        if let Some(item) = self.disputes.get_mut(reference) {
            item.resolution = Some(resolution);
        }
        self.set_exception(ctx, format!("disputed_item:{reference}"), "disputed_item", None);
        ctx.notices.push(Notice::DisputeResolved {
            reference: reference.to_string(),
            repay_minor,
        });
        Ok(())
    }

    pub(crate) fn reinsurer_consent(&mut self, ctx: &mut Ctx, claim: ClaimId, reinsurer: ReinsurerId, agreed: bool, reference: String) -> Result<(), Reject> {
        let book = self
            .claims
            .get_mut(&claim)
            .ok_or_else(|| Reject::new("unknown_claim", format!("no claim {claim}")))?;
        if reference.is_empty() {
            return Err(Reject::new("reference_required", "a consent names the reinsurer's answer"));
        }
        book.consent.insert(reinsurer.clone(), (agreed, reference.clone()));
        ctx.notices.push(Notice::ReinsurerConsent {
            claim: claim.clone(),
            reinsurer,
            agreed,
            reference,
        });
        self.sync_claim(ctx, &claim);
        Ok(())
    }

    /// Whether the reinsurers with a say on a claim have agreed.
    pub fn consent_status(&self, claim: &ClaimId) -> Option<ConsentStatus> {
        let book = self.claims.get(claim)?;
        let Some(due_by) = book.consent_due.values().min().copied() else {
            return Some(ConsentStatus::NotRequired);
        };
        if book.consent.values().any(|(agreed, _)| !agreed) {
            return Some(ConsentStatus::Refused);
        }
        let needed = book
            .consent_due
            .keys()
            .filter_map(|t| self.latest_treaty(t))
            .flat_map(|t| t.panel.iter().map(|p| &p.reinsurer));
        Some(if needed.into_iter().all(|r| book.consent.contains_key(r)) {
            ConsentStatus::Agreed
        } else {
            ConsentStatus::Waiting { due_by }
        })
    }

    /// Every claim on a cover whose event is on or after `from` is worked
    /// out again from the cession record.
    pub(crate) fn resync_claims(&mut self, ctx: &mut Ctx, key: &RiskKey, from: NaiveDate) {
        let ids: Vec<ClaimId> = self
            .policy_claims
            .get(&key.policy)
            .into_iter()
            .flatten()
            .filter(|id| {
                self.claims
                    .get(*id)
                    .is_some_and(|c| c.cover == key.cover && c.life == key.life && c.event_date >= from)
            })
            .cloned()
            .collect();
        for id in ids {
            self.sync_claim(ctx, &id);
        }
    }

    /// Bring what is posted for a claim into line with the claim and the
    /// cession record.
    fn sync_claim(&mut self, ctx: &mut Ctx, id: &ClaimId) {
        let Some(mut book) = self.claims.get(id).cloned() else { return };
        let partition = self.policies.get(&book.policy).map_or(0, |p| p.partition);
        let on = ctx.on;
        let (expected, due) = match book.status {
            // Frozen: what was taken stands, and nothing more is expected.
            ClaimStatus::Reversed => (book.due.clone(), book.due.clone()),
            ClaimStatus::Closed => {
                let due = self.recoveries(&book, book.paid_minor, book.ex_gratia_minor);
                (due.clone(), due)
            }
            ClaimStatus::Open => {
                let incurred = book.admitted_minor.unwrap_or(book.reserve_minor).max(book.paid_minor);
                (
                    self.recoveries(&book, incurred, book.ex_gratia_minor),
                    self.recoveries(&book, book.paid_minor, book.ex_gratia_minor),
                )
            }
        };
        for (key, delta) in deltas(&book.recoverable, &expected) {
            ctx.instruct(
                partition,
                InstructionBody::Post {
                    kind: PostingKind::Recoverable,
                    treaty: key.0,
                    reinsurer: key.1,
                    policy: Some(book.policy.clone()),
                    amount_minor: delta,
                    value_date: on,
                    reference: id.to_string(),
                },
            );
        }
        for (key, delta) in deltas(&book.due, &due) {
            self.book(
                ctx,
                LineKind::RecoveryDue,
                &key.0,
                &key.1,
                partition,
                Some(book.policy.clone()),
                on,
                delta,
                id.to_string(),
            );
        }
        book.recoverable = expected;
        book.due = due;

        // Large claims: tell the reinsurers, and ask them where they have a say.
        let treaties: BTreeSet<TreatyId> = book.recoverable.keys().map(|(t, _)| t.clone()).collect();
        let gross = book.gross();
        let mut unanswered = Vec::new();
        for tid in &treaties {
            let Some(treaty) = self.latest_treaty(tid) else { continue };
            if treaty.claims.notification_threshold_minor.is_some_and(|t| gross >= t) && book.notified.insert(tid.clone()) {
                ctx.notices.push(Notice::LargeClaimNotification {
                    claim: id.clone(),
                    treaty: tid.clone(),
                    reinsurers: treaty.panel.iter().map(|p| p.reinsurer.clone()).collect(),
                });
            }
            let needs_consent = treaty.claims.consent_threshold_minor.is_some_and(|t| gross >= t);
            if needs_consent && book.admitted_minor.is_none() && book.status == ClaimStatus::Open && !book.consent_due.contains_key(tid) {
                let due_by = on.checked_add_days(Days::new(treaty.claims.consent_days as u64)).unwrap_or(on);
                book.consent_due.insert(tid.clone(), due_by);
                ctx.notices.push(Notice::ReinsurerConsentRequired {
                    claim: id.clone(),
                    treaty: tid.clone(),
                    due_by,
                });
            }
            if book.consent_due.contains_key(tid) {
                unanswered.extend(
                    treaty
                        .panel
                        .iter()
                        .filter(|p| book.consent.get(&p.reinsurer).is_none_or(|(agreed, _)| !agreed))
                        .map(|p| p.reinsurer.to_string()),
                );
            }
        }
        let detail = (book.admitted_minor.is_some() && !unanswered.is_empty()).then(|| {
            format!(
                "claim {id} was admitted without the agreement of {}; the recovery may be disputed",
                unanswered.join(", ")
            )
        });
        self.set_exception(ctx, format!("admitted_without_consent:{id}"), "admitted_without_consent", detail);

        self.sync_aggregates(ctx, id, &mut book, partition);
        self.claims.insert(id.clone(), book);
    }

    /// Catastrophe and stop loss covers respond to what the insurer has paid
    /// and kept across many claims.
    fn sync_aggregates(&mut self, ctx: &mut Ctx, id: &ClaimId, book: &mut ClaimBook, partition: PartitionId) {
        let Some((product, cover_type)) = self
            .claim_keys(book)
            .first()
            .and_then(|k| self.risk(k))
            .map(|r| (r.product.clone(), r.cover_type.clone()))
        else {
            return;
        };
        let kept = book.paid_minor + book.ex_gratia_minor - book.due.values().sum::<i64>();
        let mut touched: Vec<(TreatyId, String)> = Vec::new();
        // Catastrophe covers first: a stop loss counts what they leave.
        let mut covering: Vec<_> = self.aggregates.values().filter(|a| a.covers(&product, &cover_type, book.event_date)).collect();
        covering.sort_by_key(|a| (matches!(a.kind, AggregateKind::StopLoss { .. }), a.id.clone()));
        for a in covering {
            let group = match (&a.kind, &book.catastrophe_event) {
                (AggregateKind::Catastrophe { .. }, Some(event)) => event.clone(),
                (AggregateKind::Catastrophe { .. }, None) => continue,
                (AggregateKind::StopLoss { .. }, _) => String::new(),
            };
            let cell = self.aggregate_net.entry((a.id.clone(), group.clone())).or_default();
            match book.aggregate.insert(a.id.clone(), kept) {
                Some(before) => cell.1 += kept - before,
                None => {
                    cell.0 += 1;
                    cell.1 += kept;
                }
            }
            touched.push((a.id.clone(), group));
        }
        for (tid, group) in touched {
            let Some(a) = self.aggregates.get(&tid) else { continue };
            let (lives, kept) = self.aggregate_net.get(&(tid.clone(), group.clone())).copied().unwrap_or_default();
            let recover = match &a.kind {
                AggregateKind::Catastrophe {
                    min_lives,
                    retention_minor,
                    limit_minor,
                } => {
                    if lives >= *min_lives {
                        (kept - retention_minor).clamp(0, *limit_minor)
                    } else {
                        0
                    }
                }
                AggregateKind::StopLoss { attachment_minor, limit_minor } => {
                    let from_catastrophe: i64 = self
                        .aggregate_posted
                        .iter()
                        .filter(|((t, _, _), _)| {
                            self.aggregates.get(t).is_some_and(|c| {
                                matches!(c.kind, AggregateKind::Catastrophe { .. }) && a.period_start <= c.period_start && c.period_start <= a.period_end
                            })
                        })
                        .map(|(_, v)| *v)
                        .sum();
                    (kept - from_catastrophe - attachment_minor).clamp(0, *limit_minor)
                }
            };
            let weights: Vec<i64> = a.panel.iter().map(|p| p.share as i64).collect();
            let shares: Vec<(ReinsurerId, i64)> = a.panel.iter().map(|p| p.reinsurer.clone()).zip(split_exact(recover, &weights)).collect();
            let on = ctx.on;
            for (reinsurer, share) in shares {
                let posted = self.aggregate_posted.entry((tid.clone(), group.clone(), reinsurer.clone())).or_default();
                let delta = share - *posted;
                *posted = share;
                let reference = if group.is_empty() {
                    format!("{tid}/{id}")
                } else {
                    format!("{tid}/{group}/{id}")
                };
                self.book(ctx, LineKind::AggregateRecovery, &tid, &reinsurer, partition, None, on, delta, reference);
            }
        }
    }

    /// Recoveries expected but not yet due, by reinsurer.
    pub fn open_recoverables(&self) -> BTreeMap<ReinsurerId, i64> {
        let mut out = BTreeMap::new();
        for book in self.claims.values() {
            for ((treaty, reinsurer), expected) in &book.recoverable {
                let open = expected - book.due.get(&(treaty.clone(), reinsurer.clone())).copied().unwrap_or(0);
                if open != 0 {
                    *out.entry(reinsurer.clone()).or_default() += open;
                }
            }
        }
        out
    }
}

/// The change from `was` to `now`, by key.
fn deltas(was: &ByReinsurer, now: &ByReinsurer) -> Vec<((TreatyId, ReinsurerId), i64)> {
    let keys: BTreeSet<_> = was.keys().chain(now.keys()).collect();
    keys.into_iter()
        .map(|k| (k.clone(), now.get(k).copied().unwrap_or(0) - was.get(k).copied().unwrap_or(0)))
        .filter(|(_, d)| *d != 0)
        .collect()
}
