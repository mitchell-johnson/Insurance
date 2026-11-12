//! Replacements nobody declared.
//!
//! A replacement is meant to arrive as one: the layer is written with
//! `replacement` set, or the policies are linked. Where new cover is simply
//! written on a life soon after cover on that life was given up, the rule
//! here matches the two when the layer is written. The matched amount is
//! then a replacement in every way that matters to commission: it earns no
//! initial commission, its trail follows the books of the cover it replaced,
//! and where a whole layer was matched the clawback raised when it was given
//! up is reversed and its exposure items carry on, on their own clock.
//! Anything above the matched amount is new business.
//!
//! The writing adviser is told and may dispute the match. While a dispute is
//! open the initial commission on the matched amount is earned into held;
//! the decision either takes it back or leaves it to be released.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_core::{PartyId, Reject};

use crate::model::*;
use crate::rules::{portion, CommissionType, HeldReason, WHOLE};
use crate::state::{reject, Ctx, DistributionState, Event};
use crate::BookId;

impl DistributionState {
    /// The person a layer covers, where the core named one.
    fn person_of(&self, layer: &LayerRecord) -> Option<&PartyId> {
        self.policies.get(&layer.policy)?.lives.get(layer.life.as_ref()?)
    }

    /// Look for cover given up on the same life that this newly written
    /// layer replaces, and treat the matched amount as a replacement.
    pub(crate) fn match_replacement(&mut self, ctx: &mut Ctx, key: &str, today: NaiveDate) {
        let window = self.settings.replacement_window_months;
        let Some(new) = self.layers.get(key).cloned() else {
            return;
        };
        let Some(person) = self.person_of(&new).cloned() else {
            return;
        };
        let mut candidates: Vec<(NaiveDate, String, i64, MatchReason)> = vec![];
        for (k, old) in &self.layers {
            let Some((on, amount)) = old.given_up else { continue };
            let free = amount - old.matched_away_minor;
            if k == key
                || free <= 0
                || old.cover_type != new.cover_type
                || on > new.start
                || months_between(on, new.start) >= window
                || self.person_of(old) != Some(&person)
            {
                continue;
            }
            let Some(reason) = self.match_reason(&new, old, free, new.start) else {
                continue;
            };
            candidates.push((on, k.clone(), free, reason));
        }
        // Oldest cover given up first, so the answer never depends on order.
        candidates.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        let Some(reason) = candidates.first().map(|c| c.3) else {
            return;
        };
        let mut left = new.sum_insured_minor;
        let mut replaced: Vec<(String, i64)> = vec![];
        let mut books: BTreeMap<BookId, i64> = BTreeMap::new();
        let mut continued: Vec<String> = vec![];
        for (on, k, free, _) in candidates {
            let take = free.min(left);
            if take == 0 {
                break;
            }
            left -= take;
            let Some(old) = self.layers.get_mut(&k) else { continue };
            old.matched_away_minor += take;
            let whole = take == old.sum_insured_minor;
            for (book, share) in old.books_on(on) {
                *books.entry(book.clone()).or_insert(0) += (take as i128 * *share as i128 / WHOLE as i128) as i64;
            }
            replaced.push((k.clone(), take));
            if whole {
                continued.extend(self.continue_exposure(ctx, &k, key, &new, today));
            }
        }
        let matched_minor = new.sum_insured_minor - left;
        ctx.emit(Event::ReplacementMatched {
            policy: new.policy.clone(),
            layer: key.to_string(),
            matched_minor,
            reason,
            replaced: replaced.iter().map(|(k, _)| k.clone()).collect(),
        });
        if let Some(l) = self.layers.get_mut(key) {
            l.replacement = Some(ReplacementMatch {
                matched_minor,
                reason,
                on: today,
                replaced,
                books: books.into_iter().collect(),
                continued,
                disputed: None,
                matched_initial: None,
                matched_exposures: vec![],
                overturned: false,
                initial_on_whole: false,
            });
        }
    }

    /// Why new cover counts as replacing old cover, if it does: the same
    /// adviser, a related provider, or, for anyone, the amount given up.
    fn match_reason(&self, new: &LayerRecord, old: &LayerRecord, free: i64, day: NaiveDate) -> Option<MatchReason> {
        let structure = &self.config.structure;
        let engaged = |l: &LayerRecord| -> Vec<(PartyId, PartyId)> {
            l.writing
                .iter()
                .filter_map(|(e, _)| structure.engagements.get(e))
                .map(|e| (e.adviser.clone(), e.with.clone()))
                .collect()
        };
        let (mine, theirs) = (engaged(new), engaged(old));
        if mine.iter().any(|(a, _)| theirs.iter().any(|(b, _)| a == b)) {
            Some(MatchReason::SameAdviser)
        } else if mine.iter().any(|(_, a)| theirs.iter().any(|(_, b)| structure.related(a, b, day))) {
            Some(MatchReason::RelatedAdviser)
        } else if (new.sum_insured_minor - free).abs() <= portion(free, self.settings.replacement_amount_tolerance_ppm) {
            Some(MatchReason::AmountsMatch)
        } else {
            None
        }
    }

    /// Cover was given up after new cover was already written on the same
    /// life: an adviser moving a customer who is still in force writes the
    /// new policy first and the old one is cancelled afterwards. The rule
    /// looks back from the cover given up to layers written in the window
    /// before it. What the new layer has already been paid as initial
    /// commission on the matched amount is taken back from whoever received
    /// it, and from then on the matched amount is a replacement as if the
    /// match had been made when it was written.
    pub(crate) fn match_given_up(&mut self, ctx: &mut Ctx, old_key: &str, today: NaiveDate) {
        let window = self.settings.replacement_window_months;
        let Some(old) = self.layers.get(old_key).cloned() else {
            return;
        };
        let (Some((on, amount)), Some(person)) = (old.given_up, self.person_of(&old).cloned()) else {
            return;
        };
        let mut free = amount - old.matched_away_minor;
        let mut candidates: Vec<(NaiveDate, String, i64, MatchReason)> = vec![];
        for (k, new) in &self.layers {
            let taken = new.replacement.as_ref().filter(|m| !m.overturned).map_or(0, |m| m.matched_minor);
            let available = new.sum_insured_minor - taken;
            let linked = self.policies.get(&new.policy).and_then(|p| p.replaces.as_ref()) == Some(&old.policy);
            if new.policy == old.policy
                || linked
                || available <= 0
                || new.terms.is_none()
                || new.indexation
                || new.ended.is_some()
                || new.cover_type != old.cover_type
                || new.start > on
                || months_between(new.start, on) >= window
                || self.person_of(new) != Some(&person)
            {
                continue;
            }
            if let Some(reason) = self.match_reason(new, &old, free, on) {
                candidates.push((new.start, k.clone(), available, reason));
            }
        }
        candidates.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        for (_, new_key, available, reason) in candidates {
            let take = free.min(available);
            if take <= 0 {
                break;
            }
            free -= take;
            let Some(new) = self.layers.get(&new_key).cloned() else { continue };
            let whole = take == old.sum_insured_minor;
            if let Some(o) = self.layers.get_mut(old_key) {
                o.matched_away_minor += take;
            }
            let continued = if whole {
                self.continue_exposure(ctx, old_key, &new_key, &new, today)
            } else {
                vec![]
            };
            self.take_back_initial(ctx, &new, take, today);
            let mut books: BTreeMap<BookId, i64> = BTreeMap::new();
            for (book, share) in old.books_on(on) {
                *books.entry(book.clone()).or_insert(0) += (take as i128 * *share as i128 / WHOLE as i128) as i64;
            }
            let m = match self.layers.get_mut(&new_key).and_then(|l| l.replacement.take().filter(|m| !m.overturned)) {
                Some(mut m) => {
                    m.matched_minor += take;
                    m.replaced.push((old_key.to_string(), take));
                    m.continued.extend(continued);
                    for (book, amount) in books {
                        match m.books.iter_mut().find(|(b, _)| *b == book) {
                            Some((_, a)) => *a += amount,
                            None => m.books.push((book, amount)),
                        }
                    }
                    m
                }
                None => ReplacementMatch {
                    matched_minor: take,
                    reason,
                    on: today,
                    replaced: vec![(old_key.to_string(), take)],
                    books: books.into_iter().collect(),
                    continued,
                    disputed: None,
                    matched_initial: None,
                    matched_exposures: vec![],
                    overturned: false,
                    initial_on_whole: false,
                },
            };
            ctx.emit(Event::ReplacementMatched {
                policy: new.policy.clone(),
                layer: new_key.clone(),
                matched_minor: m.matched_minor,
                reason: m.reason,
                replaced: m.replaced.iter().map(|(k, _)| k.clone()).collect(),
            });
            if let Some(l) = self.layers.get_mut(&new_key) {
                l.replacement = Some(m);
            }
        }
    }

    /// Initial commission a layer was paid on an amount now matched as a
    /// replacement goes back, in proportion, from whoever received it, and
    /// its exposure items shrink to what is left.
    fn take_back_initial(&mut self, ctx: &mut Ctx, layer: &LayerRecord, matched: i64, today: NaiveDate) {
        let key = layer.key();
        let Some(original) = layer
            .initial_paid
            .as_ref()
            .and_then(|r| self.allocations.get(r))
            .filter(|a| a.reversed_by.is_none())
            .cloned()
        else {
            return;
        };
        let whole = layer.sum_insured_minor.max(1) as i128;
        let share = |amount: i64| (amount as i128 * matched as i128 / whole) as i64;
        let lines: Vec<AllocLine> = original
            .lines
            .iter()
            .filter(|l| l.layer.as_deref() == Some(key.as_str()) && l.commission_type == Some(CommissionType::Initial))
            .map(|l| AllocLine {
                amount_minor: -share(l.amount_minor),
                ..l.clone()
            })
            .filter(|l| l.amount_minor != 0)
            .collect();
        for id in &original.exposures {
            if let Some(e) = self.exposures.get_mut(id).filter(|e| e.layer == key) {
                let (amount, clawed) = (share(e.amount_minor), share(e.clawed_minor));
                e.amount_minor -= amount;
                e.clawed_minor -= clawed;
            }
        }
        if lines.is_empty() {
            return;
        }
        let reference = format!("cr:matched:{}:{}", key.replace('/', "."), self.seq);
        ctx.emit(Event::CommissionReversed {
            reference: reference.clone(),
            reverses: original.reference.clone(),
        });
        let partition = self.policies.get(&layer.policy).map_or(original.partition, |p| p.partition);
        self.record_allocation(
            ctx,
            Allocation {
                reference,
                partition,
                policy: layer.policy.clone(),
                kind: "commission_reversed".into(),
                cause: format!("matched:{}", key.replace('/', ".")),
                date: today,
                lines,
                reverses: None,
                exposures: vec![],
                reversed_by: None,
            },
        );
    }

    /// The whole of a layer given up was matched: the clawback raised when
    /// it went is reversed, to the parties who bore it, and its exposure
    /// items carry on under the new layer on their original clock.
    pub(crate) fn continue_exposure(&mut self, ctx: &mut Ctx, old: &str, new_key: &str, new: &LayerRecord, today: NaiveDate) -> Vec<String> {
        let items: Vec<(String, String, i64)> = self
            .exposures
            .values()
            .filter(|e| e.layer == old)
            .filter_map(|e| e.last_clawback.as_ref().map(|(r, amount)| (e.id.clone(), r.clone(), *amount)))
            .collect();
        let mut references: Vec<&String> = items.iter().map(|(_, r, _)| r).collect();
        references.dedup();
        for reference in references {
            self.reverse_allocation(ctx, reference, &format!("carried:{}", old.replace('/', ".")), today, Some(old));
        }
        for (id, _, amount) in &items {
            if let Some(e) = self.exposures.get_mut(id) {
                e.open = true;
                e.clawed_minor -= amount;
                e.last_clawback = None;
                e.policy = new.policy.clone();
                e.layer = new_key.to_string();
            }
        }
        items.into_iter().map(|(id, _, _)| id).collect()
    }

    fn matched_layer(&self, key: &str) -> Result<(&LayerRecord, &ReplacementMatch), Reject> {
        let layer = self
            .layers
            .get(key)
            .ok_or_else(|| Reject::new("unknown_layer", format!("layer {key} is not known")))?;
        match layer.replacement.as_ref().filter(|m| !m.overturned) {
            Some(m) => Ok((layer, m)),
            None => reject("not_matched", format!("layer {key} is not treated as a replacement by the rule")),
        }
    }

    pub(crate) fn dispute_replacement(&mut self, ctx: &mut Ctx, key: &str, reference: String) -> Result<(), Reject> {
        let today = self.date()?;
        let (_, m) = self.matched_layer(key)?;
        if reference.trim().is_empty() {
            return reject("invalid_dispute", "a dispute carries its reference");
        }
        if m.disputed.is_some() {
            return reject("already_disputed", "the match is already disputed");
        }
        if let Some(m) = self.layers.get_mut(key).and_then(|l| l.replacement.as_mut()) {
            m.disputed = Some(reference.clone());
        }
        self.task(
            ctx,
            "replacement_disputed",
            key,
            today,
            format!("the writing adviser disputes the replacement match on {key} ({reference})"),
        );
        ctx.emit(Event::ReplacementDisputed {
            layer: key.to_string(),
            reference,
        });
        Ok(())
    }

    /// Decide a disputed match. It stands: the initial commission held on
    /// the matched amount is taken back from held. It does not: the layer
    /// is new business, what is held is left to be released, the clawback
    /// on the cover given up is raised again, and its trail follows the
    /// layer's own books from now on.
    pub(crate) fn decide_replacement(&mut self, ctx: &mut Ctx, key: &str, replacement: bool) -> Result<(), Reject> {
        let today = self.date()?;
        let (layer, m) = self.matched_layer(key)?;
        if m.disputed.is_none() {
            return reject("not_disputed", "there is no dispute to decide on that match");
        }
        let (policy, m) = (layer.policy.clone(), m.clone());
        let partition = self.policies.get(&policy).map(|p| p.partition).unwrap_or_default();
        let held: Vec<Exposure> = m.matched_exposures.iter().filter_map(|id| self.exposures.get(id)).cloned().collect();
        if replacement {
            let lines: Vec<AllocLine> = held
                .iter()
                .map(|e| AllocLine {
                    payee: e.paid_to.clone(),
                    dest: Dest::Held(HeldReason::Dispute),
                    amount_minor: -e.amount_minor,
                    book: Some(e.book.clone()),
                    layer: Some(e.layer.clone()),
                    component: Some(e.component.clone()),
                    commission_type: Some(CommissionType::Initial),
                    earner: None,
                    rule_version: None,
                    product: e.product.clone(),
                    cover_type: e.cover_type.clone(),
                })
                .collect();
            for e in &held {
                if let Some(x) = self.exposures.get_mut(&e.id) {
                    x.open = false;
                }
            }
            if let Some(original) = m.matched_initial.clone().filter(|_| !lines.is_empty()) {
                let reference = format!("cr:decided:{}", key.replace('/', "."));
                ctx.emit(Event::CommissionReversed {
                    reference: reference.clone(),
                    reverses: original,
                });
                self.record_allocation(
                    ctx,
                    Allocation {
                        reference,
                        partition,
                        policy: policy.clone(),
                        kind: "commission_reversed".into(),
                        cause: format!("decided:{key}"),
                        date: today,
                        lines,
                        reverses: None,
                        exposures: vec![],
                        reversed_by: None,
                    },
                );
            }
        } else {
            // The cover given up really was given up.
            let given_up_on = m
                .replaced
                .iter()
                .filter_map(|(k, _)| self.layers.get(k).and_then(|l| l.given_up).map(|(on, _)| on))
                .min()
                .unwrap_or(today);
            let continued: Vec<Exposure> = m.continued.iter().filter_map(|id| self.exposures.get(id)).filter(|e| e.open).cloned().collect();
            self.claw_back(
                ctx,
                &policy,
                partition,
                &format!("overturned:{}", key.replace('/', ".")),
                given_up_on,
                today,
                &continued,
                None,
            );
            for e in &continued {
                if let Some(x) = self.exposures.get_mut(&e.id) {
                    x.open = false;
                }
            }
            for (k, amount) in &m.replaced {
                if let Some(l) = self.layers.get_mut(k) {
                    l.matched_away_minor -= amount;
                }
            }
        }
        // Decided for the adviser before the layer's own initial commission
        // was paid: that payment, still to come, is on the whole layer.
        let own_to_come = self.layers.get(key).is_some_and(|l| l.initial_paid.is_none());
        if let Some(x) = self.layers.get_mut(key).and_then(|l| l.replacement.as_mut()) {
            x.disputed = None;
            x.overturned = !replacement;
            x.initial_on_whole = !replacement && x.matched_initial.is_none() && own_to_come;
        }
        ctx.emit(Event::ReplacementDecided {
            layer: key.to_string(),
            replacement,
        });
        Ok(())
    }
}
