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
use crate::BookId;

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
                });
                for info in layers {
                    self.add_layer(ctx, &policy, info, LayerOrigin::Original, date);
                }
            }
            CoreEvent::LayerAdded { policy, layer, origin } => self.add_layer(ctx, &policy, layer, origin, date),
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
                let reference = self.earn(ctx, partition, &policy, &cause, date, &net_by_cover_type);
                if let Some(reference) = &reference {
                    self.receipts.insert(format!("{policy}/{receipt}"), reference.clone());
                }
                self.premium_applied(&policy, reference, net_by_cover_type);
            }
            CoreEvent::SuspenseApplied { policy, net_by_cover_type, .. } => {
                let reference = self.earn(ctx, partition, &policy, &cause, date, &net_by_cover_type);
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
                PolicyStatus::Expired | PolicyStatus::Matured | PolicyStatus::Surrendered | PolicyStatus::PaidUp => {
                    self.policy_ended(ctx, &policy, &cause, date, EndKind::Other)
                }
                _ => {}
            },
            CoreEvent::PolicyReinstated { policy } => self.reinstated(ctx, &policy, &cause, date),
            CoreEvent::TransferLanded { policy, .. } => {
                // Layers, books and exposure items travel with the policy.
                // Payable accounts exist per payee on every partition, so
                // nothing else changes.
                if let Some(p) = self.policies.get_mut(&policy) {
                    p.partition = partition;
                }
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
    fn earn(&mut self, ctx: &mut Ctx, partition: PartitionId, policy: &PolicyId, cause: &str, date: NaiveDate, net: &BTreeMap<String, i64>) -> Option<String> {
        let record = self.policies.get(policy)?;
        let product = record.product_code.clone();
        let reference = format!("ce:{cause}");
        let mut lines: Vec<AllocLine> = vec![];
        let mut exposures: Vec<Exposure> = vec![];
        let mut initial_layers: Vec<String> = vec![];
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
                let books = layer.books_on(date);
                let mut parts: Vec<(CommissionType, i64, i64)> = vec![];
                if terms.rates.initial_ppm > 0 && layer.initial_paid.is_none() && premium > 0 {
                    parts.push((
                        CommissionType::Initial,
                        portion(terms.annual_premium_minor, terms.rates.initial_ppm),
                        terms.annual_premium_minor,
                    ));
                }
                if months_between(layer.start, date) >= terms.rates.renewal_from_month {
                    parts.push((CommissionType::Renewal, portion(premium, terms.rates.renewal_ppm), premium));
                }
                for (ctype, gross, basis) in parts {
                    let shares: Vec<i64> = books.iter().map(|(_, s)| *s as i64).collect();
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
                            let dest = self.destination(&a, date);
                            if ctype == CommissionType::Initial {
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
                    if ctype == CommissionType::Initial {
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

    fn record_allocation(&mut self, ctx: &mut Ctx, allocation: Allocation) {
        if let Some(posting) = posting_of(&allocation, self.currency_of(&allocation.policy)) {
            self.request(ctx, allocation.partition, posting, None);
        }
        self.allocations_by_date.entry(allocation.date).or_default().push(allocation.reference.clone());
        self.allocations.insert(allocation.reference.clone(), allocation);
    }

    /// Reverse an allocation with a new one: the same payees, the same
    /// accounts, the opposite sign. With `only_layer`, just that layer's lines.
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

    fn reverse_allocation(&mut self, ctx: &mut Ctx, reference: &str, cause: &str, date: NaiveDate, only_layer: Option<&str>) {
        let Some(original) = self.allocations.get(reference).cloned() else {
            return;
        };
        if original.reversed_by.is_some() {
            return;
        }
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
        let mut lines = vec![];
        let mut touched = vec![];
        for e in &open {
            let amount = e.at_risk(date) - e.clawed_minor;
            if amount <= 0 {
                continue;
            }
            let weights: Vec<i64> = e.liable.iter().map(|(_, s, _)| *s as i64).collect();
            for ((liable, _, _), share) in e.liable.iter().zip(split(amount, &weights)) {
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
            return;
        }
        for (id, amount) in &touched {
            if let Some(x) = self.exposures.get_mut(id) {
                x.clawed_minor += amount;
            }
        }
        let reference = format!("cb:{cause}");
        ctx.emit(Event::ClawbackRaised {
            reference: reference.clone(),
            policy: policy.clone(),
            total_minor: -lines.iter().map(|l| l.amount_minor).sum::<i64>(),
        });
        if let Some(p) = self.policies.get_mut(policy) {
            p.last_clawback = Some(reference.clone());
        }
        self.record_allocation(
            ctx,
            Allocation {
                reference,
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
    }

    /// Reinstatement reverses the clawback entry, so the same parties are
    /// restored and the exposure items resume.
    fn reinstated(&mut self, ctx: &mut Ctx, policy: &PolicyId, cause: &str, date: NaiveDate) {
        let Some(record) = self.policies.get_mut(policy) else {
            return;
        };
        record.ended = None;
        let Some(reference) = record.last_clawback.take() else {
            return;
        };
        let Some(clawback) = self.allocations.get(&reference).cloned() else {
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
            let Some(original) = self.allocations.get(&reference).cloned() else {
                continue;
            };
            if original.kind != "commission_earned" || original.reversed_by.is_some() {
                continue;
            }
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
            let Some(original) = self.allocations.get(&reference).cloned() else {
                continue;
            };
            if original.kind != "commission_earned" || original.reversed_by.is_some() {
                continue;
            }
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
        if let Some(reference) = copy.earn(&mut ctx, partition, policy, "dry-run", date, net) {
            for l in copy.allocations.get(&reference).map(|a| a.lines.as_slice()).unwrap_or_default() {
                *out.entry(l.payee.clone()).or_insert(0) += l.amount_minor;
            }
        }
        out
    }
}
