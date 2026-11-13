//! Commission from the core's events: what each premium earns and for whom,
//! clawback when a policy lapses or is cancelled, and reversals that follow
//! the lines of the entry they reverse, whatever the rule says today.
//!
//! Ingesting an event never fails for a business reason. The checkpoint
//! always advances; what cannot be worked out becomes an exception item.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_core::counterparty::{CounterpartyAccount, CounterpartyLine, CounterpartyPosting, InternalLedger, PartyLedger};
use ic_core::event::{Event as CoreEvent, LayerInfo};
use ic_core::{EventRecord, LayerOrigin, PartyId, PolicyId, PolicyStatus, Reject};
use ic_ledger::PartitionId;
use ic_money::Currency;

use crate::model::*;
use crate::rules::{portion, Allocated, CommissionType, Component, HeldReason, WHOLE};
use crate::state::{reject, Ctx, DistributionState, Event};
use crate::{BookId, EngagementId};

/// Split `total` by weights. The pieces sum to `total`; the last takes the odd cent.
fn split(total: i64, weights: &[i64]) -> Vec<i64> {
    let sum: i128 = weights.iter().map(|w| *w as i128).sum();
    if sum <= 0 {
        return vec![0; weights.len()];
    }
    let mut out: Vec<i64> = weights.iter().map(|w| (total as i128 * *w as i128 / sum) as i64).collect();
    let odd = total - out.iter().sum::<i64>();
    if let Some(last) = out.iter_mut().rev().find(|_| true) {
        *last += odd;
    }
    out
}

/// Which share of a matched layer a piece of commission is for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Part {
    /// The layer's own business, on its own books.
    Own,
    /// Trail on the matched amount, on the books of the cover replaced.
    Replaced,
    /// Initial commission on the matched amount; true while it is disputed.
    Matched(bool),
}

/// The claims events that carry an overpayment's whole position.
const CLAIMS_OVERPAYMENT_KINDS: [&str; 4] = [
    "overpayment_raised",
    "recovery_status_changed",
    "overpayment_recovered",
    "overpayment_written_off",
];

fn ledger_of(dest: Dest) -> PartyLedger {
    match dest {
        Dest::Payable => PartyLedger::CommissionPayable,
        Dest::Held(_) => PartyLedger::Held,
    }
}

/// Turn commission detail into the one balanced posting the core takes: one
/// line per payee account, one per expense account.
fn posting_of(a: &Allocation, currency: Currency) -> Option<CounterpartyPosting> {
    let mut parties: BTreeMap<(PartyId, PartyLedger), i64> = BTreeMap::new();
    let mut expense: BTreeMap<(String, String), i64> = BTreeMap::new();
    for l in &a.lines {
        *parties.entry((l.payee.clone(), ledger_of(l.dest))).or_insert(0) -= l.amount_minor;
        *expense.entry((l.product.clone(), l.cover_type.clone())).or_insert(0) += l.amount_minor;
    }
    let mut lines: Vec<CounterpartyLine> = expense
        .into_iter()
        .filter(|(_, v)| *v != 0)
        .map(|((product, cover_type), amount_minor)| CounterpartyLine {
            account: CounterpartyAccount::Internal(InternalLedger::CommissionExpense { product, cover_type }),
            amount_minor,
        })
        .collect();
    lines.extend(
        parties
            .into_iter()
            .filter(|(_, v)| *v != 0)
            .map(|((party, ledger), amount_minor)| CounterpartyLine {
                account: CounterpartyAccount::Party { party, ledger },
                amount_minor,
            }),
    );
    if lines.is_empty() {
        return None;
    }
    let mut detail = BTreeMap::new();
    detail.insert("cause".to_string(), a.cause.clone());
    // The breakdown a statement needs: layer, book, component, earner and
    // rule version for each amount. Opaque IDs only.
    detail.insert(
        "lines".to_string(),
        a.lines
            .iter()
            .map(|l| {
                format!(
                    "{}|{}|{}|{}|{}|{}|{}",
                    l.payee,
                    l.amount_minor,
                    l.layer.as_deref().unwrap_or(""),
                    l.book.as_ref().map_or("", |b| b.0.as_str()),
                    l.component.as_ref().map(Component::label).unwrap_or_default(),
                    l.earner.as_ref().map_or("", |e| e.0.as_str()),
                    l.rule_version.map(|v| v.to_string()).unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>()
            .join(";"),
    );
    Some(CounterpartyPosting {
        reference: a.reference.clone(),
        kind: a.kind.clone(),
        currency,
        value_date: Some(a.date),
        policy: Some(a.policy.clone()),
        reverses: a.reverses.clone(),
        lines,
        detail,
    })
}

impl DistributionState {
    /// The currency of a policy's partition postings. The core keeps one
    /// currency per policy; this module is told it on every confirmation and
    /// otherwise works in New Zealand dollars.
    fn currency_of(&self, _policy: &PolicyId) -> Currency {
        Currency::NZD
    }

    pub(crate) fn ingest(&mut self, ctx: &mut Ctx, record: EventRecord) -> Result<(), Reject> {
        let last = self.checkpoints.get(&record.partition).copied().unwrap_or(0);
        if record.seq <= last {
            // Delivered before. At-least-once delivery makes this normal.
            return Ok(());
        }
        if record.seq != last + 1 {
            return reject(
                "event_gap",
                format!(
                    "partition {} is at event {last}; {} arrived. Ask for the missing events.",
                    record.partition, record.seq
                ),
            );
        }
        self.checkpoints.insert(record.partition, record.seq);
        if self.today.is_none_or(|t| record.business_date > t) {
            self.today = Some(record.business_date);
        }
        let date = record.business_date;
        let partition = record.partition;
        let cause = record.id.clone();
        match record.event {
            CoreEvent::BusinessDateAdvanced { to, .. } => {
                // The event is recorded on the day that closed; the new day starts here.
                if self.today.is_none_or(|t| to > t) {
                    self.today = Some(to);
                }
            }
            CoreEvent::PolicyIssued {
                policy,
                product_code,
                inception,
                lives,
                layers,
                ..
            } => {
                self.policies.entry(policy.clone()).or_insert_with(|| PolicyRecord {
                    partition,
                    product_code,
                    inception,
                    ended: None,
                    servicing: None,
                    default: None,
                    layers: vec![],
                    replaces: None,
                    last_clawback: None,
                    adviser_change_setting: None,
                    altered_by: None,
                    lives: lives.into_iter().filter_map(|l| l.party.map(|p| (l.life.0, p))).collect(),
                });
                for info in layers {
                    self.add_layer(ctx, &policy, info, LayerOrigin::Original, date);
                }
            }
            CoreEvent::LayerAdded {
                policy,
                layer,
                origin,
                continues,
            } => match continues.map(|old| layer_key(&policy, &layer.cover, old)) {
                Some(old) if self.layers.contains_key(&old) => self.layer_carried_on(ctx, &policy, &old, layer, date),
                _ => self.add_layer(ctx, &policy, layer, origin, date),
            },
            CoreEvent::LayersMerged {
                policy,
                cover,
                removed,
                into,
                sum_insured_minor,
            } => {
                for n in removed {
                    if let Some(l) = self.layers.get_mut(&layer_key(&policy, &cover, n)) {
                        l.ended = Some(l.ended.unwrap_or(date));
                    }
                }
                if let Some(l) = self.layers.get_mut(&layer_key(&policy, &cover, into)) {
                    l.sum_insured_minor = sum_insured_minor;
                }
            }
            CoreEvent::LayerReduced {
                policy,
                cover,
                layer,
                continued_as,
                effective,
                sum_insured_minor,
                kept_minor,
            } => {
                let (from, to) = (layer_key(&policy, &cover, layer), layer_key(&policy, &cover, continued_as));
                self.layer_reduced(
                    ctx,
                    &policy,
                    &from,
                    (&to, continued_as),
                    (kept_minor, sum_insured_minor),
                    &cause,
                    effective,
                    date,
                )
            }
            CoreEvent::LayerEnded {
                policy,
                cover,
                layer,
                effective,
                ..
            } => self.layer_ended(ctx, &policy, &layer_key(&policy, &cover, layer), &cause, effective, date),
            CoreEvent::LayerUnwound {
                policy, cover, layer, start, ..
            } => {
                let key = layer_key(&policy, &cover, layer);
                let paid = self.layers.get_mut(&key).and_then(|l| {
                    l.ended = Some(start);
                    l.initial_paid.clone()
                });
                // A layer that existed for no days earned nothing.
                if let Some(reference) = paid {
                    self.reverse_allocation(ctx, &reference, &cause, date, Some(&key));
                }
            }
            CoreEvent::ReceiptApplied {
                policy,
                receipt,
                net_by_cover_type,
                ..
            } => {
                let reference = self.earn(ctx, partition, &policy, &cause, date, &net_by_cover_type, false);
                if let Some(reference) = &reference {
                    self.receipts.insert(format!("{policy}/{receipt}"), reference.clone());
                }
                self.premium_applied(&policy, reference, net_by_cover_type);
            }
            CoreEvent::SuspenseApplied { policy, net_by_cover_type, .. } => {
                let reference = self.earn(ctx, partition, &policy, &cause, date, &net_by_cover_type, false);
                self.premium_applied(&policy, reference, net_by_cover_type);
            }
            CoreEvent::ReceiptReversed { policy, receipt, .. } => {
                // A dishonoured premium reverses the lines of the entry that
                // recorded it, to the same payees, whatever the rule says today.
                if let Some(reference) = self.receipts.get(&format!("{policy}/{receipt}")).cloned() {
                    self.reverse_allocation(ctx, &reference, &cause, date, None);
                    // Nothing of it is left to hand back.
                    for record in self
                        .premiums
                        .get_mut(&policy)
                        .into_iter()
                        .flatten()
                        .filter(|r| r.reference.as_ref() == Some(&reference))
                    {
                        record.back = record.net.clone();
                    }
                }
            }
            CoreEvent::PremiumUnapplied { policy, net_by_cover_type, .. } => self.premium_unapplied(ctx, &policy, &cause, date, &net_by_cover_type),
            CoreEvent::PolicyStatusChanged { policy, to, .. } => match to {
                PolicyStatus::Lapsed => self.policy_ended(ctx, &policy, &cause, date, EndKind::Lapsed),
                PolicyStatus::Cancelled => self.policy_ended(ctx, &policy, &cause, date, EndKind::Cancelled),
                PolicyStatus::Terminated => self.policy_ended(ctx, &policy, &cause, date, EndKind::Claim),
                PolicyStatus::Avoided => self.policy_avoided(ctx, &policy, &cause, date),
                PolicyStatus::Expired | PolicyStatus::Matured | PolicyStatus::Surrendered | PolicyStatus::PaidUp => {
                    self.policy_ended(ctx, &policy, &cause, date, EndKind::Other)
                }
                _ => {}
            },
            CoreEvent::PolicyReinstated { policy } => self.reinstated(ctx, &policy, &cause, date),
            // The person behind a scheme member's life, as soon as it is
            // known: what the replacement rule matches lives by.
            CoreEvent::MemberJoined {
                policy,
                life,
                party: Some(party),
                ..
            }
            | CoreEvent::MemberIdentified { policy, life, party } => {
                if let Some(p) = self.policies.get_mut(&policy) {
                    p.lives.insert(life.0, party);
                }
            }
            CoreEvent::TransferLanded { policy, .. } => {
                // Layers, books and exposure items travel with the policy.
                // Payable accounts exist per payee on every partition, so
                // nothing else changes.
                if let Some(p) = self.policies.get_mut(&policy) {
                    p.partition = partition;
                }
            }
            CoreEvent::Module { module, kind, detail, .. } if module == "claims" && kind == "premium_waived" => {
                // Premium the insurer paid for the customer during a claim.
                // It earns as premium received does, where the schedule
                // the layer was written under says so.
                let net: BTreeMap<String, i64> = detail
                    .iter()
                    .filter_map(|(k, v)| Some((k.strip_prefix("net:")?.to_string(), v.parse().ok()?)))
                    .collect();
                if let Some(policy) = detail.get("policy").map(|p| PolicyId(p.clone())) {
                    let reference = self.earn(ctx, partition, &policy, &cause, date, &net, true);
                    self.premium_applied(&policy, reference, net);
                }
            }
            CoreEvent::Module { module, kind, subject, detail } if module == "claims" && CLAIMS_OVERPAYMENT_KINDS.contains(&kind.as_str()) => {
                self.claims_overpayment(&subject, &detail, date)
            }
            CoreEvent::CounterpartyPosted {
                entry,
                reference,
                kind,
                value_date,
                lines,
                ..
            } => self.confirmed(ctx, partition, entry.to_string(), reference, kind, value_date, lines),
            _ => {}
        }
        Ok(())
    }

    fn add_layer(&mut self, ctx: &mut Ctx, policy: &PolicyId, info: LayerInfo, origin: LayerOrigin, date: NaiveDate) {
        let key = layer_key(policy, &info.cover, info.layer);
        if self.layers.contains_key(&key) {
            return;
        }
        let mut record = LayerRecord {
            policy: policy.clone(),
            cover: info.cover.clone(),
            layer: info.layer,
            cover_type: info.cover_type,
            start: info.start,
            sum_insured_minor: info.sum_insured_minor,
            indexation: origin == LayerOrigin::Indexation,
            writing: vec![],
            assignments: vec![],
            terms: None,
            initial_paid: None,
            ended: None,
            life: Some(info.life.0.clone()),
            given_up: None,
            matched_away_minor: 0,
            replacement: None,
        };
        if origin == LayerOrigin::Indexation {
            // An indexation step inherits the cover's terms and book. It
            // earns initial commission only where the schedule pays it.
            let sibling = self
                .layers
                .values()
                .filter(|l| &l.policy == policy && l.cover == info.cover && l.terms.is_some() && l.sum_insured_minor > 0)
                .max_by_key(|l| l.layer);
            if let Some(s) = sibling {
                let mut terms = s.terms.clone();
                if let Some(t) = terms.as_mut() {
                    t.annual_premium_minor = (t.annual_premium_minor as i128 * info.sum_insured_minor as i128 / s.sum_insured_minor as i128) as i64;
                    let pays = self.config.schedule(&t.schedule, info.start).is_some_and(|sch| sch.initial_on_indexation);
                    if !pays {
                        t.rates.initial_ppm = 0;
                    }
                }
                record.terms = terms;
                record.writing = s.writing.clone();
                let shares = s.books_on(info.start).to_vec();
                if !shares.is_empty() {
                    record.assignments = vec![BookAssignment { from: info.start, shares }];
                }
            }
        } else if origin == LayerOrigin::Increase {
            // An increase belongs to the policy's current arrangement. Its
            // terms come from whoever submits it; until then it earns nothing.
            self.task(
                ctx,
                "layer_needs_terms",
                &key,
                date,
                "an increase was added and has not been written to a book".into(),
            );
        }
        if let Some(p) = self.policies.get_mut(policy) {
            p.layers.push(key.clone());
        }
        self.layers.insert(key, record);
    }

    /// Underwriting changed a layer's terms and nothing else: the core ends
    /// the layer and starts another in its place. It is the same cover, so
    /// the new layer takes the old one's writing, books, terms, clock and
    /// any replacement match; the clawback the ending raised is reversed and
    /// its exposure items carry on; and the old layer was not given up.
    /// Inside cooling-off the ending reversed the initial commission, and the
    /// new layer earns it again on its next premium.
    fn layer_carried_on(&mut self, ctx: &mut Ctx, policy: &PolicyId, old: &str, info: LayerInfo, date: NaiveDate) {
        let key = layer_key(policy, &info.cover, info.layer);
        let Some(before) = self.layers.get_mut(old).filter(|_| old != key) else {
            return;
        };
        before.given_up = None;
        before.ended.get_or_insert(info.start);
        let mut new = before.clone();
        new.layer = info.layer;
        new.sum_insured_minor = info.sum_insured_minor;
        new.ended = None;
        new.matched_away_minor = 0;
        if self.layers.contains_key(&key) {
            return;
        }
        self.layers.insert(key.clone(), new.clone());
        if let Some(p) = self.policies.get_mut(policy) {
            p.layers.push(key.clone());
        }
        self.continue_exposure(ctx, old, &key, &new, date);
    }

    /// Where an amount for a payee goes on a day: paid, or held and why.
    fn destination(&self, a: &Allocated, day: NaiveDate) -> Dest {
        if a.disputed {
            return Dest::Held(HeldReason::Dispute);
        }
        if let Some((from, reason, _)) = self.payees.get(&a.payee).and_then(|p| p.accrual_hold.as_ref()) {
            if *from <= day {
                return Dest::Held(*reason);
            }
        }
        // The standing rule: a suspended payee accrues to held. The rule is
        // not edited; the licence record alone redirects the line.
        let structure = &self.config.structure;
        let earner_impaired = a.earner.as_ref().is_some_and(|e| structure.engagement_impairment(e, day).is_some());
        if earner_impaired || structure.payee_impairment(&a.payee, day).is_some() {
            return Dest::Held(HeldReason::SuspendedLicence);
        }
        Dest::Payable
    }

    /// Work out what a premium earns. Split it across the layers of each
    /// cover type in proportion to each layer's premium, build the components
    /// for each layer, and allocate each by its book's rule in force that day.
    fn earn(
        &mut self,
        ctx: &mut Ctx,
        partition: PartitionId,
        policy: &PolicyId,
        cause: &str,
        date: NaiveDate,
        net: &BTreeMap<String, i64>,
        waived: bool,
    ) -> Option<String> {
        let record = self.policies.get(policy)?;
        let product = record.product_code.clone();
        let reference = format!("ce:{cause}");
        let mut lines: Vec<AllocLine> = vec![];
        let mut exposures: Vec<Exposure> = vec![];
        let mut initial_layers: Vec<String> = vec![];
        let mut matched_exposures: Vec<(String, String)> = vec![];
        let mut problems: Vec<String> = vec![];
        for (cover_type, amount) in net {
            let layers: Vec<&LayerRecord> = record
                .layers
                .iter()
                .filter_map(|k| self.layers.get(k))
                .filter(|l| &l.cover_type == cover_type && l.in_force(date))
                .collect();
            // By premium where every layer has one on record, otherwise by sum insured.
            let by_premium = layers.iter().all(|l| l.terms.as_ref().is_some_and(|t| t.annual_premium_minor > 0));
            let weights: Vec<i64> = layers
                .iter()
                .map(|l| {
                    if by_premium {
                        l.terms.as_ref().map_or(0, |t| t.annual_premium_minor)
                    } else {
                        l.sum_insured_minor
                    }
                })
                .collect();
            for (layer, premium) in layers.iter().zip(split(*amount, &weights)) {
                let Some(terms) = &layer.terms else { continue };
                if waived
                    && !self
                        .config
                        .schedule_version(&terms.schedule, terms.schedule_version)
                        .is_none_or(|s| s.earns_on_waived_premium)
                {
                    continue;
                }
                let own: Vec<(BookId, i64)> = layer.books_on(date).iter().map(|(b, s)| (b.clone(), *s as i64)).collect();
                // A layer the replacement rule matched: the matched amount
                // earns no initial commission and its trail follows the
                // books of the cover it replaced. Above it is new business.
                let matched = layer.replacement.as_ref();
                let live = matched.filter(|m| !m.overturned);
                let new_share = |amount: i64| match live {
                    Some(m) => amount - (amount as i128 * m.matched_minor as i128 / layer.sum_insured_minor.max(1) as i128) as i64,
                    None => amount,
                };
                let mut parts: Vec<(CommissionType, i64, Part)> = vec![];
                if terms.rates.initial_ppm > 0 && premium > 0 && !waived {
                    // Initial commission on the matched amount is its own
                    // payment, if it is paid at all: into held while the
                    // match is disputed, in the ordinary way once a dispute
                    // is decided for the adviser, and never twice.
                    let apart = matched.filter(|m| !m.initial_on_whole);
                    let on_matched = apart.map_or(0, |m| {
                        (terms.annual_premium_minor as i128 * m.matched_minor as i128 / layer.sum_insured_minor.max(1) as i128) as i64
                    });
                    let new_business = terms.annual_premium_minor - on_matched;
                    if layer.initial_paid.is_none() && new_business > 0 {
                        parts.push((CommissionType::Initial, new_business, Part::Own));
                    }
                    if let Some(m) = apart.filter(|m| (m.disputed.is_some() || m.overturned) && m.matched_initial.is_none()) {
                        parts.push((CommissionType::Initial, on_matched, Part::Matched(m.disputed.is_some())));
                    }
                }
                if months_between(layer.start, date) >= terms.rates.renewal_from_month {
                    let new_business = new_share(premium);
                    parts.push((CommissionType::Renewal, new_business, Part::Own));
                    if premium != new_business {
                        parts.push((CommissionType::Renewal, premium - new_business, Part::Replaced));
                    }
                }
                for (ctype, basis, part) in parts {
                    let rate = if ctype == CommissionType::Initial {
                        terms.rates.initial_ppm
                    } else {
                        terms.rates.renewal_ppm
                    };
                    let gross = portion(basis, rate);
                    let books = match (part, live) {
                        (Part::Replaced, Some(m)) => &m.books,
                        _ => &own,
                    };
                    let shares: Vec<i64> = books.iter().map(|(_, s)| *s).collect();
                    let grosses = split(gross, &shares);
                    let bases = split(basis, &shares);
                    for (((book, _), gross), basis) in books.iter().zip(grosses).zip(bases) {
                        if self.config.books.get(book).is_none_or(|b| b.house) {
                            continue;
                        }
                        let Some((rule, composed)) = self.config.composed(book, date) else {
                            problems.push(format!("book {book} has no rule in force on {date}"));
                            continue;
                        };
                        let mut allocated: Vec<Allocated> = vec![];
                        if gross != 0 {
                            match composed.allocate_base(gross, ctype, date) {
                                Ok(a) => allocated.extend(a),
                                Err(e) => {
                                    problems.push(format!("book {book}: {e}"));
                                    continue;
                                }
                            }
                        }
                        for component in composed.extra_components(ctype, date) {
                            allocated.extend(composed.allocate_extra(&component, basis, ctype == CommissionType::Initial, ctype, date));
                        }
                        for a in allocated.into_iter().filter(|a| a.amount_minor != 0) {
                            let dest = if part == Part::Matched(true) {
                                Dest::Held(HeldReason::Dispute)
                            } else {
                                self.destination(&a, date)
                            };
                            if ctype == CommissionType::Initial {
                                if matches!(part, Part::Matched(_)) {
                                    matched_exposures.push((layer.key(), format!("{reference}#{}", exposures.len() + 1)));
                                }
                                let mut liable = composed.liable(&a.component, date);
                                if liable.is_empty() {
                                    liable = vec![(a.payee.clone(), WHOLE, None)];
                                }
                                exposures.push(Exposure {
                                    id: format!("{reference}#{}", exposures.len() + 1),
                                    policy: policy.clone(),
                                    layer: layer.key(),
                                    book: book.clone(),
                                    component: a.component.clone(),
                                    paid_to: a.payee.clone(),
                                    liable,
                                    amount_minor: a.amount_minor,
                                    paid_on: date,
                                    layer_start: layer.start,
                                    scale: terms.clawback.clone(),
                                    cooling_off_days: terms.cooling_off_days,
                                    product: product.clone(),
                                    cover_type: cover_type.clone(),
                                    open: true,
                                    clawed_minor: 0,
                                    last_clawback: None,
                                });
                            }
                            lines.push(AllocLine {
                                payee: a.payee,
                                dest,
                                amount_minor: a.amount_minor,
                                book: Some(book.clone()),
                                layer: Some(layer.key()),
                                component: Some(a.component),
                                commission_type: Some(ctype),
                                earner: a.earner,
                                rule_version: Some(rule.version),
                                product: product.clone(),
                                cover_type: cover_type.clone(),
                            });
                        }
                    }
                    if ctype == CommissionType::Initial && part == Part::Own {
                        initial_layers.push(layer.key());
                    }
                }
            }
        }
        for message in problems {
            ctx.emit(Event::CommissionException {
                policy: policy.clone(),
                cause: cause.to_string(),
                message: message.clone(),
            });
            self.task(ctx, "commission_exception", &policy.0, date, message);
        }
        if lines.is_empty() {
            return None;
        }
        for key in initial_layers {
            if let Some(l) = self.layers.get_mut(&key) {
                l.initial_paid = Some(reference.clone());
            }
        }
        for (key, exposure) in matched_exposures {
            if let Some(m) = self.layers.get_mut(&key).and_then(|l| l.replacement.as_mut()) {
                m.matched_initial = Some(reference.clone());
                m.matched_exposures.push(exposure);
            }
        }
        let allocation = Allocation {
            reference: reference.clone(),
            partition,
            policy: policy.clone(),
            kind: "commission_earned".into(),
            cause: cause.to_string(),
            date,
            lines,
            reverses: None,
            exposures: exposures.iter().map(|e| e.id.clone()).collect(),
            reversed_by: None,
        };
        for e in exposures {
            self.exposures.insert(e.id.clone(), e);
        }
        ctx.emit(Event::CommissionEarned {
            reference: reference.clone(),
            policy: policy.clone(),
            total_minor: allocation.lines.iter().map(|l| l.amount_minor).sum(),
        });
        self.record_allocation(ctx, allocation);
        Some(reference)
    }

    pub(crate) fn record_allocation(&mut self, ctx: &mut Ctx, allocation: Allocation) {
        if let Some(posting) = posting_of(&allocation, self.currency_of(&allocation.policy)) {
            self.request(ctx, allocation.partition, posting, None);
        }
        self.allocations_by_date.entry(allocation.date).or_default().push(allocation.reference.clone());
        self.allocations.insert(allocation.reference.clone(), allocation);
    }

    /// Reverse an allocation with a new one: the same payees, the same
    /// accounts, the opposite sign. With `only_layer`, just that layer's lines.
    /// Claims said where one of its overpayments stands. Every one of its
    /// events carries the whole position, so the latest replaces what was
    /// held. One that names no party or no reference cannot be shown and is
    /// left out; claims' own records are unaffected.
    fn claims_overpayment(&mut self, claim: &str, detail: &BTreeMap<String, String>, date: NaiveDate) {
        let (Some(reference), Some(party)) = (detail.get("overpayment"), detail.get("party").filter(|p| !p.is_empty())) else {
            return;
        };
        let flag = |name: &str| detail.get(name).is_some_and(|v| v == "true");
        let text = |name: &str| detail.get(name).cloned().unwrap_or_default();
        let line = crate::workbench::MemoLine {
            owner: "claims".into(),
            reference: reference.clone(),
            party: PartyId(party.clone()),
            case_ref: claim.to_string(),
            policy: detail.get("policy").map(|p| PolicyId(p.clone())),
            outstanding_minor: detail.get("outstanding_minor").and_then(|v| v.parse().ok()).unwrap_or(0),
            status: text("status"),
            cause: text("cause"),
            insurer_error: flag("insurer_error"),
            good_faith: flag("good_faith"),
            as_at: date,
        };
        self.changed.insert(line.party.clone(), self.seq);
        self.memo.insert(format!("claims:{reference}"), line);
    }

    fn premium_applied(&mut self, policy: &PolicyId, reference: Option<String>, net: BTreeMap<String, i64>) {
        if net.is_empty() || !self.policies.contains_key(policy) {
            return;
        }
        self.premiums.entry(policy.clone()).or_default().push(PremiumRecord {
            reference,
            net,
            back: BTreeMap::new(),
        });
    }

    /// Premium already paid went back to the policyholder because the
    /// billing it paid was reversed. The renewal commission it earned goes
    /// back with it, from the payees who received it and in the proportion
    /// handed back. The most recent premium is taken first: billing is
    /// reversed from the latest period backwards, and premium is applied to
    /// the oldest period first. Initial commission is not touched here; it
    /// follows the layer and the clawback schedule.
    fn premium_unapplied(&mut self, ctx: &mut Ctx, policy: &PolicyId, cause: &str, date: NaiveDate, net: &BTreeMap<String, i64>) {
        let Some(partition) = self.policies.get(policy).map(|p| p.partition) else {
            return;
        };
        let mut records = self.premiums.remove(policy).unwrap_or_default();
        let mut lines: Vec<AllocLine> = vec![];
        for (cover_type, amount) in net {
            let mut remaining = *amount;
            for record in records.iter_mut().rev() {
                if remaining <= 0 {
                    break;
                }
                let paid = record.net.get(cover_type).copied().unwrap_or(0);
                let before = record.back.get(cover_type).copied().unwrap_or(0);
                let take = remaining.min(paid - before);
                if take <= 0 {
                    continue;
                }
                remaining -= take;
                let after = before + take;
                record.back.insert(cover_type.clone(), after);
                let Some(original) = record
                    .reference
                    .as_ref()
                    .and_then(|r| self.allocations.get(r))
                    .filter(|a| a.reversed_by.is_none())
                else {
                    continue;
                };
                // The difference of two running shares, so that premium
                // handed back in parts takes back exactly the whole line.
                let share = |line: i64, of: i64| (line as i128 * of as i128 / paid as i128) as i64;
                lines.extend(
                    original
                        .lines
                        .iter()
                        .filter(|l| &l.cover_type == cover_type && l.commission_type == Some(CommissionType::Renewal))
                        .map(|l| AllocLine {
                            amount_minor: -(share(l.amount_minor, after) - share(l.amount_minor, before)),
                            ..l.clone()
                        })
                        .filter(|l| l.amount_minor != 0),
                );
            }
        }
        self.premiums.insert(policy.clone(), records);
        if lines.is_empty() {
            return;
        }
        let reference = format!("cu:{cause}");
        ctx.emit(Event::CommissionReversed {
            reference: reference.clone(),
            reverses: cause.to_string(),
        });
        self.record_allocation(
            ctx,
            Allocation {
                reference,
                partition,
                policy: policy.clone(),
                kind: "commission_reversed".into(),
                cause: cause.to_string(),
                date,
                lines,
                reverses: None,
                exposures: vec![],
                reversed_by: None,
            },
        );
    }

    pub(crate) fn reverse_allocation(&mut self, ctx: &mut Ctx, reference: &str, cause: &str, date: NaiveDate, only_layer: Option<&str>) {
        // Unknown, or already reversed: there is nothing to take back.
        let Some(original) = self.allocations.get(reference).filter(|a| a.reversed_by.is_none()).cloned() else {
            return;
        };
        let lines: Vec<AllocLine> = original
            .lines
            .iter()
            // Reversing one layer takes back what was paid for writing it.
            // Renewal on its premium goes back when the premium does.
            .filter(|l| only_layer.is_none_or(|k| l.layer.as_deref() == Some(k) && l.commission_type != Some(CommissionType::Renewal)))
            .map(|l| AllocLine {
                amount_minor: -l.amount_minor,
                ..l.clone()
            })
            .collect();
        if lines.is_empty() {
            return;
        }
        let new_reference = format!("cr:{cause}");
        // The exposure items the original created no longer stand, and the
        // layers it paid initial commission on have not been paid it.
        for id in &original.exposures {
            if let Some(e) = self.exposures.get_mut(id).filter(|e| only_layer.is_none_or(|k| e.layer == k)) {
                e.open = false;
            }
        }
        for l in self.layers.values_mut().filter(|l| l.initial_paid.as_deref() == Some(reference)) {
            if only_layer.is_none_or(|k| l.key() == k) {
                l.initial_paid = None;
            }
        }
        if only_layer.is_none() {
            if let Some(o) = self.allocations.get_mut(reference) {
                o.reversed_by = Some(new_reference.clone());
            }
        }
        ctx.emit(Event::CommissionReversed {
            reference: new_reference.clone(),
            reverses: reference.to_string(),
        });
        self.record_allocation(
            ctx,
            Allocation {
                reference: new_reference,
                partition: self.policies.get(&original.policy).map_or(original.partition, |p| p.partition),
                policy: original.policy.clone(),
                kind: "commission_reversed".into(),
                cause: cause.to_string(),
                date,
                lines,
                // The core checks a reversal against its own partition's
                // references; a policy that has moved posts without one.
                reverses: (self.policies.get(&original.policy).map(|p| p.partition) == Some(original.partition)).then(|| reference.to_string()),
                exposures: vec![],
                reversed_by: None,
            },
        );
    }

    /// A policy ended. A lapse or cancellation inside the clawback period
    /// works through the open exposure items and debits whoever each one
    /// names as liable. A cancellation in cooling-off reverses all commission
    /// paid, from whoever received it. A claim never triggers clawback.
    fn policy_ended(&mut self, ctx: &mut Ctx, policy: &PolicyId, cause: &str, date: NaiveDate, kind: EndKind) {
        let Some(record) = self.policies.get_mut(policy) else {
            return;
        };
        record.ended = Some((date, kind));
        let partition = record.partition;
        let open: Vec<Exposure> = self.exposures.values().filter(|e| &e.policy == policy && e.open).cloned().collect();
        if !matches!(kind, EndKind::Lapsed | EndKind::Cancelled) {
            for e in open {
                if let Some(x) = self.exposures.get_mut(&e.id) {
                    x.open = false;
                }
            }
            return;
        }
        let cooling_off = kind == EndKind::Cancelled && open.iter().any(|e| (date - e.layer_start).num_days() <= e.cooling_off_days as i64);
        if cooling_off {
            let references: Vec<String> = self
                .allocations
                .values()
                .filter(|a| &a.policy == policy && a.kind == "commission_earned" && a.reversed_by.is_none())
                .map(|a| a.reference.clone())
                .collect();
            for (n, reference) in references.iter().enumerate() {
                self.reverse_allocation(ctx, reference, &format!("{cause}.{n}"), date, None);
            }
            return;
        }
        // Where the replaced policy's unexpired clawback carries over to its
        // replacement, nothing is clawed back now.
        let replaced_by = self
            .policies
            .iter()
            .find(|(_, p)| p.replaces.as_ref() == Some(policy))
            .map(|(id, _)| id.clone());
        let carries = replaced_by.is_some()
            && open
                .iter()
                .filter_map(|e| self.layers.get(&e.layer))
                .filter_map(|l| l.terms.as_ref())
                .filter_map(|t| self.config.schedule(&t.schedule, date))
                .any(|s| s.replacement_carries_clawback);
        if carries {
            if let Some(new_policy) = replaced_by {
                for e in open {
                    if let Some(x) = self.exposures.get_mut(&e.id) {
                        x.policy = new_policy.clone();
                    }
                }
            }
            return;
        }
        // Cover given up: what the replacement rule looks back at.
        if replaced_by.is_none() {
            for l in self
                .layers
                .values_mut()
                .filter(|l| &l.policy == policy && l.ended.is_none() && l.terms.is_some())
            {
                l.given_up = Some((date, l.sum_insured_minor));
            }
        }
        if let Some(reference) = self.claw_back(ctx, policy, partition, cause, date, date, &open, None) {
            if let Some(p) = self.policies.get_mut(policy) {
                p.last_clawback = Some(reference);
            }
        }
        let given_up: Vec<String> = self
            .layers
            .iter()
            .filter(|(_, l)| &l.policy == policy && l.given_up.is_some())
            .map(|(k, _)| k.clone())
            .collect();
        for key in given_up {
            self.match_given_up(ctx, &key, date);
        }
    }

    /// The policy was avoided for non-disclosure and is treated as never
    /// having been in force, so it earned nobody anything. Every line of
    /// commission still standing on it is reversed, initial and renewal,
    /// from whoever received it, and any clawback is given back to whoever
    /// bore it: after this every party nets to nothing on the policy. This
    /// is so whether the insurer refunds the premium or keeps it. Its cover
    /// was never given up, so the replacement rule does not look at it.
    fn policy_avoided(&mut self, ctx: &mut Ctx, policy: &PolicyId, cause: &str, date: NaiveDate) {
        let Some(record) = self.policies.get_mut(policy).filter(|p| p.ended.is_none()) else {
            return;
        };
        record.ended = Some((date, EndKind::Avoided));
        let partition = record.partition;
        // What stands on the policy is the sum of every allocation except
        // the ones that restate another in place (a move to held, a
        // reallocation under a backdated rule), whose originals were
        // rewritten to match.
        let mut net: BTreeMap<String, (AllocLine, i64)> = BTreeMap::new();
        for a in self
            .allocations
            .values()
            .filter(|a| &a.policy == policy && !matches!(a.kind.as_str(), "commission_held" | "commission_reallocated"))
        {
            for l in &a.lines {
                let key = serde_json::to_string(&AllocLine { amount_minor: 0, ..l.clone() }).unwrap_or_default();
                net.entry(key).or_insert_with(|| (l.clone(), 0)).1 += l.amount_minor;
            }
        }
        let lines: Vec<AllocLine> = net
            .into_values()
            .filter(|(_, amount)| *amount != 0)
            .map(|(l, amount)| AllocLine { amount_minor: -amount, ..l })
            .collect();
        for e in self.exposures.values_mut().filter(|e| &e.policy == policy) {
            e.open = false;
        }
        for l in self.layers.values_mut().filter(|l| &l.policy == policy) {
            l.ended.get_or_insert(l.start);
            l.given_up = None;
        }
        self.premiums.remove(policy);
        if lines.is_empty() {
            return;
        }
        let reference = format!("cr:{cause}");
        let references: Vec<String> = self
            .allocations
            .values_mut()
            .filter(|a| &a.policy == policy && a.kind == "commission_earned" && a.reversed_by.is_none())
            .map(|a| {
                a.reversed_by = Some(reference.clone());
                a.reference.clone()
            })
            .collect();
        ctx.emit(Event::CommissionReversed {
            reference: reference.clone(),
            reverses: references.first().cloned().unwrap_or_default(),
        });
        self.record_allocation(
            ctx,
            Allocation {
                reference,
                partition,
                policy: policy.clone(),
                kind: "commission_reversed".into(),
                cause: cause.to_string(),
                date,
                lines,
                reverses: None,
                exposures: vec![],
                reversed_by: None,
            },
        );
    }

    /// One layer of a cover ended while the policy carries on: the cover was
    /// reduced or removed. Commission is clawed back on that layer alone, as
    /// at the day the reduction took effect, from whoever its exposure items
    /// name as liable. Inside the cooling-off period the layer's initial
    /// commission is reversed in full, from whoever received it. The layer
    /// earns nothing from that day. A layer that ends with the whole policy
    /// is handled by the policy's own ending, and a claim never claws back.
    fn layer_ended(&mut self, ctx: &mut Ctx, policy: &PolicyId, key: &str, cause: &str, effective: NaiveDate, date: NaiveDate) {
        let Some(partition) = self.policies.get(policy).filter(|p| p.ended.is_none()).map(|p| p.partition) else {
            return;
        };
        let Some(layer) = self.layers.get_mut(key).filter(|l| l.ended.is_none()) else {
            return;
        };
        layer.ended = Some(effective);
        let (start, paid) = (layer.start, layer.initial_paid.clone());
        let open: Vec<Exposure> = self.exposures.values().filter(|e| e.layer == key && e.open).cloned().collect();
        let cooling_off = open.iter().any(|e| (effective - start).num_days() <= e.cooling_off_days as i64);
        match paid {
            Some(reference) if cooling_off => self.reverse_allocation(ctx, &reference, cause, date, Some(key)),
            _ => {
                let altered_by = self.altered_by(policy, date);
                self.claw_back(ctx, policy, partition, cause, effective, date, &open, altered_by.as_ref());
                if let Some(l) = self.layers.get_mut(key).filter(|l| l.terms.is_some()) {
                    l.given_up = Some((effective, l.sum_insured_minor));
                    self.match_given_up(ctx, key, date);
                }
            }
        }
        // The layer is gone: nothing more can be clawed back on it.
        for e in open {
            if let Some(x) = self.exposures.get_mut(&e.id) {
                x.open = false;
            }
        }
    }

    /// A layer was cut in part. The part given up is treated as a layer that
    /// ended: its share of the commission still at risk is clawed back, or,
    /// inside cooling-off, its share of the initial commission is reversed
    /// from whoever received it. The part kept carries on as the new layer
    /// with the same terms, books and start, its premium and its exposure in
    /// proportion, and it is not new business: it earns no initial commission.
    fn layer_reduced(
        &mut self,
        ctx: &mut Ctx,
        policy: &PolicyId,
        from: &str,
        to: (&str, u32),
        kept_of: (i64, i64),
        cause: &str,
        effective: NaiveDate,
        date: NaiveDate,
    ) {
        let (kept, whole) = kept_of;
        let (to, to_layer) = to;
        let Some(partition) = self.policies.get(policy).filter(|p| p.ended.is_none()).map(|p| p.partition) else {
            return;
        };
        let Some(old) = self.layers.get_mut(from).filter(|l| l.ended.is_none() && kept > 0 && kept < whole) else {
            return;
        };
        old.ended = Some(effective);
        let share = |amount: i64| (amount as i128 * kept as i128 / whole as i128) as i64;
        let mut new = old.clone();
        new.layer = to_layer;
        new.sum_insured_minor = kept;
        new.ended = None;
        if let Some(t) = new.terms.as_mut() {
            t.annual_premium_minor = share(t.annual_premium_minor);
        }
        let (start, paid) = (new.start, new.initial_paid.clone());
        self.layers.insert(to.to_string(), new);
        if let Some(p) = self.policies.get_mut(policy) {
            p.layers.push(to.to_string());
        }
        // Each exposure item splits: the part kept follows the new layer,
        // and the part given up is clawed back or reversed now.
        let open: Vec<Exposure> = self.exposures.values().filter(|e| e.layer == from && e.open).cloned().collect();
        let mut given_up = vec![];
        let mut kept_items = vec![];
        for e in &open {
            let (amount, clawed) = (share(e.amount_minor), share(e.clawed_minor));
            given_up.push(Exposure {
                amount_minor: e.amount_minor - amount,
                clawed_minor: e.clawed_minor - clawed,
                ..e.clone()
            });
            kept_items.push((e.id.clone(), amount, clawed));
        }
        let cooling_off = open.iter().any(|e| (effective - start).num_days() <= e.cooling_off_days as i64);
        let original = paid
            .and_then(|r| self.allocations.get(&r))
            .filter(|a| cooling_off && a.reversed_by.is_none())
            .cloned();
        match original {
            None => {
                let altered_by = self.altered_by(policy, date);
                self.claw_back(ctx, policy, partition, cause, effective, date, &given_up, altered_by.as_ref());
                if let Some(l) = self.layers.get_mut(from).filter(|l| l.terms.is_some()) {
                    l.given_up = Some((effective, whole - kept));
                    self.match_given_up(ctx, from, date);
                }
            }
            Some(original) => {
                let lines: Vec<AllocLine> = original
                    .lines
                    .iter()
                    .filter(|l| l.layer.as_deref() == Some(from) && l.commission_type == Some(CommissionType::Initial))
                    .map(|l| AllocLine {
                        amount_minor: -(l.amount_minor - share(l.amount_minor)),
                        ..l.clone()
                    })
                    .filter(|l| l.amount_minor != 0)
                    .collect();
                let reference = format!("cr:{cause}");
                ctx.emit(Event::CommissionReversed {
                    reference: reference.clone(),
                    reverses: original.reference.clone(),
                });
                self.record_allocation(
                    ctx,
                    Allocation {
                        reference,
                        partition,
                        policy: policy.clone(),
                        kind: "commission_reversed".into(),
                        cause: cause.to_string(),
                        date,
                        lines,
                        reverses: None,
                        exposures: vec![],
                        reversed_by: None,
                    },
                );
            }
        }
        // The items that remain are the kept share, on the new layer.
        for (id, amount, clawed) in kept_items {
            if let Some(x) = self.exposures.get_mut(&id) {
                x.amount_minor = amount;
                x.clawed_minor = clawed;
                x.layer = to.to_string();
                // What was clawed back was on the share given up.
                x.last_clawback = None;
            }
        }
    }

    /// The engagement that submitted an alteration to the policy on this
    /// business date, if servicing said so before the core carried it out.
    fn altered_by(&self, policy: &PolicyId, date: NaiveDate) -> Option<EngagementId> {
        let (engagement, on) = self.policies.get(policy)?.altered_by.as_ref()?;
        (*on == date).then(|| engagement.clone())
    }

    /// Clawback by cause. Where the rule the commission was paid under has
    /// the switch on, a clawback set off by an alteration is borne by the
    /// payees of the engagement that submitted the alteration, not by the
    /// parties liable when the commission was paid: the parties the altering
    /// engagement's own book names as liable for the component or, where it
    /// names nobody, the payees of its base initial commission in their
    /// shares. An alteration by the engagement that wrote the layer, or by
    /// one with no book and rule of its own, changes nothing.
    fn borne_by_cause(&self, e: &Exposure, engagement: &EngagementId, day: NaiveDate) -> Option<Vec<(PartyId, u32, Option<PartyId>)>> {
        let (rule, _) = self.config.composed(&e.book, day)?;
        if !rule.clawback_by_cause || self.layers.get(&e.layer)?.writing.iter().any(|(w, _)| w == engagement) {
            return None;
        }
        let book = self.config.books.values().find(|b| b.engagement.as_ref() == Some(engagement) && !b.house)?;
        let (_, composed) = self.config.composed(&book.id, day)?;
        let liable = composed.liable(&e.component, day);
        if !liable.is_empty() {
            return Some(liable);
        }
        let paid = composed.allocate_base(WHOLE as i64, CommissionType::Initial, day).ok()?;
        Some(
            paid.into_iter()
                .filter(|a| a.amount_minor > 0)
                .map(|a| (a.payee, a.amount_minor as u32, None))
                .collect(),
        )
    }

    /// Claw back what is still at risk on these exposure items as at a day,
    /// from whoever each names as liable, or from the altering engagement's
    /// payees where clawback by cause applies. Returns the reference of the
    /// clawback, if anything was at risk.
    pub(crate) fn claw_back(
        &mut self,
        ctx: &mut Ctx,
        policy: &PolicyId,
        partition: PartitionId,
        cause: &str,
        as_at: NaiveDate,
        date: NaiveDate,
        open: &[Exposure],
        altered_by: Option<&EngagementId>,
    ) -> Option<String> {
        let mut lines = vec![];
        let mut touched = vec![];
        for e in open {
            let amount = e.at_risk(as_at) - e.clawed_minor;
            if amount <= 0 {
                continue;
            }
            let by_cause = altered_by.and_then(|engagement| self.borne_by_cause(e, engagement, as_at));
            let bearers = by_cause.as_ref().unwrap_or(&e.liable);
            let weights: Vec<i64> = bearers.iter().map(|(_, s, _)| *s as i64).collect();
            for ((liable, _, _), share) in bearers.iter().zip(split(amount, &weights)) {
                if share != 0 {
                    lines.push(AllocLine {
                        payee: liable.clone(),
                        dest: Dest::Payable,
                        amount_minor: -share,
                        book: Some(e.book.clone()),
                        layer: Some(e.layer.clone()),
                        component: Some(e.component.clone()),
                        commission_type: Some(CommissionType::Initial),
                        earner: None,
                        rule_version: None,
                        product: e.product.clone(),
                        cover_type: e.cover_type.clone(),
                    });
                }
            }
            touched.push((e.id.clone(), amount));
        }
        if lines.is_empty() {
            return None;
        }
        let reference = format!("cb:{cause}");
        for (id, amount) in &touched {
            if let Some(x) = self.exposures.get_mut(id) {
                x.clawed_minor += amount;
                x.last_clawback = Some((reference.clone(), *amount));
            }
        }
        ctx.emit(Event::ClawbackRaised {
            reference: reference.clone(),
            policy: policy.clone(),
            total_minor: -lines.iter().map(|l| l.amount_minor).sum::<i64>(),
        });
        self.record_allocation(
            ctx,
            Allocation {
                reference: reference.clone(),
                partition,
                policy: policy.clone(),
                kind: "commission_clawback".into(),
                cause: cause.to_string(),
                date,
                lines,
                reverses: None,
                exposures: touched.into_iter().map(|(id, _)| id).collect(),
                reversed_by: None,
            },
        );
        Some(reference)
    }

    /// Reinstatement reverses the clawback entry, so the same parties are
    /// restored and the exposure items resume.
    fn reinstated(&mut self, ctx: &mut Ctx, policy: &PolicyId, cause: &str, date: NaiveDate) {
        let Some(record) = self.policies.get_mut(policy) else {
            return;
        };
        record.ended = None;
        let keys = record.layers.clone();
        if keys.iter().filter_map(|k| self.layers.get(k)).any(|l| l.matched_away_minor > 0) {
            // Its cover was matched to a replacement and the clawback has
            // already been dealt with layer by layer: a person decides.
            self.task(
                ctx,
                "reinstated_after_replacement",
                &policy.0,
                date,
                "a policy whose cover was matched to a replacement was reinstated; review its clawback".into(),
            );
            return;
        }
        for l in self.layers.values_mut().filter(|l| &l.policy == policy && l.ended.is_none()) {
            l.given_up = None;
        }
        let Some(record) = self.policies.get_mut(policy) else {
            return;
        };
        let Some((reference, clawback)) = record.last_clawback.take().and_then(|r| self.allocations.get(&r).cloned().map(|a| (r, a))) else {
            return;
        };
        self.reverse_allocation(ctx, &reference, cause, date, None);
        // What each exposure item gave up comes back, and it runs again.
        for id in &clawback.exposures {
            if let Some(e) = self.exposures.get_mut(id) {
                e.open = true;
                e.clawed_minor = 0;
            }
        }
    }

    /// A rule was activated with a past effective date. Base commission
    /// earned on the book since that date is allocated again under the
    /// corrected rule, and the differences post today as transfers between
    /// payee accounts. Commission expense does not change.
    pub(crate) fn replay_book(&mut self, ctx: &mut Ctx, book: &BookId, from: NaiveDate) {
        let Ok(today) = self.date() else { return };
        let references: Vec<String> = self.allocations_by_date.range(from..).flat_map(|(_, v)| v.iter().cloned()).collect();
        for reference in references {
            let Some(original) = self
                .allocations
                .get(&reference)
                .filter(|a| a.kind == "commission_earned" && a.reversed_by.is_none())
                .cloned()
            else {
                continue;
            };
            let mut lines = vec![];
            let mut groups: BTreeMap<(String, CommissionType), Vec<&AllocLine>> = BTreeMap::new();
            for l in original
                .lines
                .iter()
                .filter(|l| l.book.as_ref() == Some(book) && l.component == Some(Component::Base))
            {
                if let (Some(layer), Some(ctype)) = (&l.layer, l.commission_type) {
                    groups.entry((layer.clone(), ctype)).or_default().push(l);
                }
            }
            for ((layer, ctype), old) in groups {
                let gross: i64 = old.iter().map(|l| l.amount_minor).sum();
                let Some((rule, composed)) = self.config.composed(book, original.date) else {
                    continue;
                };
                let Ok(new) = composed.allocate_base(gross, ctype, original.date) else {
                    continue;
                };
                let template = old[0];
                for l in &old {
                    lines.push(AllocLine {
                        amount_minor: -l.amount_minor,
                        ..(*l).clone()
                    });
                }
                for a in new.into_iter().filter(|a| a.amount_minor != 0) {
                    lines.push(AllocLine {
                        dest: self.destination(&a, original.date),
                        payee: a.payee,
                        amount_minor: a.amount_minor,
                        book: Some(book.clone()),
                        layer: Some(layer.clone()),
                        component: Some(Component::Base),
                        commission_type: Some(ctype),
                        earner: a.earner,
                        rule_version: Some(rule.version),
                        product: template.product.clone(),
                        cover_type: template.cover_type.clone(),
                    });
                }
            }
            // Nothing to post where the corrected rule pays the same people the same amounts.
            let mut net: BTreeMap<(PartyId, Dest), i64> = BTreeMap::new();
            for l in &lines {
                *net.entry((l.payee.clone(), l.dest)).or_insert(0) += l.amount_minor;
            }
            if net.values().all(|v| *v == 0) {
                continue;
            }
            // The original now stands as corrected: a later reversal or a
            // further replay starts from the corrected lines.
            let corrected: Vec<AllocLine> = {
                let mut kept: Vec<AllocLine> = original
                    .lines
                    .iter()
                    .filter(|l| !(l.book.as_ref() == Some(book) && l.component == Some(Component::Base)))
                    .cloned()
                    .collect();
                kept.extend(lines.iter().filter(|l| l.amount_minor > 0).cloned());
                kept
            };
            let n = self.allocations.len();
            if let Some(o) = self.allocations.get_mut(&reference) {
                o.lines = corrected;
            }
            self.record_allocation(
                ctx,
                Allocation {
                    reference: format!("cx:{}:{n}", self.seq),
                    partition: self.policies.get(&original.policy).map_or(original.partition, |p| p.partition),
                    policy: original.policy.clone(),
                    kind: "commission_reallocated".into(),
                    cause: reference.clone(),
                    date: today,
                    lines,
                    reverses: None,
                    exposures: vec![],
                    reversed_by: None,
                },
            );
        }
    }

    /// A licence event took effect in the past. What its payees were paid
    /// since the effective date moves from payable to held.
    pub(crate) fn move_paid_to_held(&mut self, ctx: &mut Ctx, from: NaiveDate) {
        let Ok(today) = self.date() else { return };
        let references: Vec<String> = self.allocations_by_date.range(from..).flat_map(|(_, v)| v.iter().cloned()).collect();
        for reference in references {
            let Some(original) = self
                .allocations
                .get(&reference)
                .filter(|a| a.kind == "commission_earned" && a.reversed_by.is_none())
                .cloned()
            else {
                continue;
            };
            let mut lines = vec![];
            let mut corrected = original.lines.clone();
            for l in corrected.iter_mut().filter(|l| l.dest == Dest::Payable && l.amount_minor > 0) {
                let probe = Allocated {
                    payee: l.payee.clone(),
                    earner: l.earner.clone(),
                    component: l.component.clone().unwrap_or(Component::Base),
                    disputed: false,
                    amount_minor: l.amount_minor,
                };
                if let Dest::Held(reason) = self.destination(&probe, original.date) {
                    lines.push(AllocLine {
                        amount_minor: -l.amount_minor,
                        ..l.clone()
                    });
                    l.dest = Dest::Held(reason);
                    lines.push(l.clone());
                }
            }
            if lines.is_empty() {
                continue;
            }
            let n = self.allocations.len();
            if let Some(o) = self.allocations.get_mut(&reference) {
                o.lines = corrected;
            }
            self.record_allocation(
                ctx,
                Allocation {
                    reference: format!("ch:{}:{n}", self.seq),
                    partition: self.policies.get(&original.policy).map_or(original.partition, |p| p.partition),
                    policy: original.policy.clone(),
                    kind: "commission_held".into(),
                    cause: reference.clone(),
                    date: today,
                    lines,
                    reverses: None,
                    exposures: vec![],
                    reversed_by: None,
                },
            );
        }
    }

    /// The core posted something to counterparty accounts. Positions are
    /// built from this, never from what the module asked for.
    fn confirmed(
        &mut self,
        ctx: &mut Ctx,
        partition: PartitionId,
        entry: String,
        reference: String,
        kind: String,
        date: NaiveDate,
        lines: Vec<CounterpartyLine>,
    ) {
        let seq = self.seq;
        self.carried(&reference);
        let owner = self.requested.get_mut(&reference).and_then(|r| {
            r.state = PostingState::Posted { entry, seq };
            r.owner.clone()
        });
        // Where held money came from: the allocation's own lines say.
        let lineage = self.allocations.get(&reference).map(|a| {
            a.lines
                .iter()
                .filter_map(|l| match l.dest {
                    Dest::Held(reason) if l.amount_minor > 0 => Some((l.payee.clone(), l.book.clone(), reason, l.amount_minor)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        });
        for line in lines {
            let CounterpartyAccount::Party { party, ledger } = line.account else {
                continue;
            };
            let amount = -line.amount_minor;
            self.changed.insert(party.clone(), seq);
            *self
                .positions
                .entry(party.clone())
                .or_default()
                .entry(partition)
                .or_default()
                .entry(ledger)
                .or_insert(0) += amount;
            self.movements.push(Movement {
                seq,
                party: party.clone(),
                partition,
                ledger,
                amount_minor: amount,
                kind: kind.clone(),
                reference: reference.clone(),
                date,
            });
            if ledger != PartyLedger::Held {
                continue;
            }
            if amount > 0 {
                let from_allocation: Vec<_> = lineage.iter().flatten().filter(|(p, ..)| *p == party).cloned().collect();
                let detailed: i64 = from_allocation.iter().map(|(.., a)| *a).sum();
                if !from_allocation.is_empty() && detailed == amount {
                    for (_, book, reason, a) in from_allocation {
                        self.held.push(HeldItem {
                            party: party.clone(),
                            partition,
                            book,
                            reason: Some(reason),
                            date,
                            reference: reference.clone(),
                            amount_minor: a,
                            remaining_minor: a,
                        });
                    }
                } else {
                    self.held.push(HeldItem {
                        party: party.clone(),
                        partition,
                        book: None,
                        reason: None,
                        date,
                        reference: reference.clone(),
                        amount_minor: amount,
                        remaining_minor: amount,
                    });
                }
                ctx.emit(Event::AmountHeld {
                    party,
                    amount_minor: amount,
                    reason: lineage.as_ref().and_then(|l| l.first()).map(|(_, _, r, _)| *r),
                    reference: reference.clone(),
                });
            } else {
                // Oldest first.
                let mut left = -amount;
                for item in self
                    .held
                    .iter_mut()
                    .filter(|i| i.party == party && i.partition == partition && i.remaining_minor > 0)
                {
                    let take = left.min(item.remaining_minor);
                    item.remaining_minor -= take;
                    left -= take;
                    if left == 0 {
                        break;
                    }
                }
                ctx.emit(Event::AmountReleased {
                    party,
                    amount_minor: -amount,
                    reference: reference.clone(),
                });
            }
        }
        if let Some(owner) = owner {
            self.posting_confirmed_for(ctx, &owner);
        }
    }

    /// Initial commission still at risk of clawback on a day, by the party
    /// that would bear it.
    pub fn clawback_exposure(&self, day: NaiveDate) -> BTreeMap<PartyId, i64> {
        let mut out = BTreeMap::new();
        for e in self.exposures.values().filter(|e| e.open) {
            let amount = (e.at_risk(day) - e.clawed_minor).max(0);
            let weights: Vec<i64> = e.liable.iter().map(|(_, s, _)| *s as i64).collect();
            for ((liable, _, _), share) in e.liable.iter().zip(split(amount, &weights)) {
                *out.entry(liable.clone()).or_insert(0) += share;
            }
        }
        out.retain(|_, v| *v != 0);
        out
    }

    /// What a premium would earn and for whom, without changing anything:
    /// commission disclosure at the point of sale, and the dry run that shows
    /// who gains and loses under a proposed change.
    pub fn dry_run(&self, policy: &PolicyId, date: NaiveDate, net: &BTreeMap<String, i64>) -> BTreeMap<PartyId, i64> {
        let mut copy = self.clone();
        let mut ctx = Ctx {
            events: vec![],
            postings: vec![],
            decision: None,
        };
        let mut out = BTreeMap::new();
        let partition = copy.policies.get(policy).map_or(0, |p| p.partition);
        if let Some(reference) = copy.earn(&mut ctx, partition, policy, "dry-run", date, net, false) {
            for l in copy.allocations.get(&reference).map(|a| a.lines.as_slice()).unwrap_or_default() {
                *out.entry(l.payee.clone()).or_insert(0) += l.amount_minor;
            }
        }
        out
    }
}
