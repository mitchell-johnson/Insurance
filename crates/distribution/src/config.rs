//! Configuration and how it changes: schedules, fragments, books and rules,
//! the structure, and the bulk changes an adviser move is made of.
//!
//! Nothing here takes effect when it is proposed. A change set is checked in
//! full against a copy of the configuration, then waits for a commission
//! approver and a compliance approver, neither of whom prepared it. Only
//! then is it activated, as one step.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use ic_core::counterparty::{CounterpartyAccount, CounterpartyLine, CounterpartyPosting, InternalLedger, PartyLedger};
use ic_core::{PartyId, PolicyId, Reject};
use ic_ledger::PartitionId;
use ic_money::Currency;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::authority::{Ask, DecisionRecord, LimitKind};
use crate::model::*;
use crate::rules::{CommissionType, Component, Composed, Fragment, Line, Rule, Share, WHOLE};
use crate::state::{reject, structure, Actor, Ctx, DistributionState, Event, WriteLayer};
use crate::structure::{Engagement, Relation, RelationKind, Structure};
use crate::{BookId, EngagementId, FragmentId};

/// What happens to renewal commission when a customer appoints a new adviser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdviserChangeMode {
    MovesAtOnce,
    /// Moves after this many months: a future-dated assignment.
    StandDown {
        months: u32,
    },
    StaysWithPrevious,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdviserChangeSetting {
    pub version: u32,
    pub mode: AdviserChangeMode,
}

/// What happens to a release of held money that would leave less held
/// than another module is actively collecting from the party.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwedElsewhere {
    /// It may go ahead with legal's signature as well as the approvers'.
    #[default]
    NeedsLegal,
    /// It is refused until the other module's recovery is settled.
    Refused,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub structure: Structure,
    pub schedules: BTreeMap<String, Vec<Schedule>>,
    pub fragments: BTreeMap<FragmentId, Vec<Fragment>>,
    pub books: BTreeMap<BookId, Book>,
    pub adviser_change: AdviserChangeSetting,
    /// The records each staff member is assigned: what a colleague covering
    /// for them inherits.
    pub staff_paths: BTreeMap<PartyId, BTreeSet<String>>,
    /// Changed through a change set, like every other business setting.
    #[serde(default)]
    pub release_owed_elsewhere: OwedElsewhere,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            structure: Structure::default(),
            schedules: BTreeMap::new(),
            fragments: BTreeMap::new(),
            books: BTreeMap::new(),
            adviser_change: AdviserChangeSetting {
                version: 1,
                mode: AdviserChangeMode::StaysWithPrevious,
            },
            staff_paths: BTreeMap::new(),
            release_owed_elsewhere: OwedElsewhere::default(),
        }
    }
}

/// A rule may carry this many lines with its fragments, so that one
/// allocation always fits in one core posting.
pub const MAX_COMPOSED_LINES: usize = 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdjustmentKind {
    /// A book-level item: a persistency adjustment, a quality measure, a
    /// negotiated one-off. Positive pays the payee; negative takes back.
    BookLevel {
        product: String,
        cover_type: String,
    },
    /// Money advanced, recovered from later statements at the agreed share.
    Advance,
    /// Keep back part of what the payee is owed as security.
    RetentionHold,
    RetentionRelease,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    ActivateSchedule(Schedule),
    ActivateFragment(Fragment),
    CreateBook {
        id: BookId,
        name: String,
        engagement: Option<EngagementId>,
        owner: Option<PartyId>,
        house: bool,
    },
    ActivateRule(Rule),
    /// New business is refused on the book from a date.
    CloseBook {
        book: BookId,
        from: NaiveDate,
    },
    WatchBook {
        book: BookId,
        until: NaiveDate,
    },
    StartEngagement {
        id: EngagementId,
        engagement: Engagement,
    },
    EndEngagement {
        id: EngagementId,
        on: NaiveDate,
    },
    Relate(Relation),
    EndRelation {
        child: PartyId,
        parent: PartyId,
        kind: RelationKind,
        on: NaiveDate,
    },
    AdviserChangeSetting(AdviserChangeSetting),
    /// Whether held money may be released, with legal's signature, to a
    /// party another module is collecting from, or is refused.
    ReleaseOwedElsewhere(OwedElsewhere),
    SetStaffPaths {
        staff: PartyId,
        paths: BTreeSet<String>,
    },
    /// Move layers to other books from a date: part of a book moving with
    /// an adviser, a retiring adviser's book split among successors.
    MoveLayers {
        layers: Vec<String>,
        to: Vec<(BookId, u32)>,
        from: NaiveDate,
    },
    SetServicing {
        policy: PolicyId,
        engagement: Option<EngagementId>,
    },
    Adjustment {
        reference: String,
        payee: PartyId,
        partition: PartitionId,
        currency: Currency,
        amount_minor: i64,
        kind: AdjustmentKind,
    },
    /// The commission option was keyed wrongly when the layer was written.
    /// The layer's terms are corrected from its start: commission already
    /// earned on it is worked out again under the right option and the
    /// differences post today. Commission expense changes.
    CorrectTerms {
        layer: String,
        option: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeState {
    Proposed,
    Activated,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeSet {
    pub id: String,
    pub changes: Vec<Change>,
    /// The signed instructions or the change record that support it.
    pub instructions_ref: String,
    pub hash: String,
    pub maker: DecisionRecord,
    pub proposed_on: NaiveDate,
    pub approval: Option<DecisionRecord>,
    pub compliance: Option<DecisionRecord>,
    pub state: ChangeState,
    pub rejected_because: Option<String>,
}

impl ChangeSet {
    fn amount(&self) -> i64 {
        self.changes
            .iter()
            .map(|c| match c {
                Change::Adjustment { amount_minor, .. } => amount_minor.abs(),
                _ => 0,
            })
            .sum()
    }
}

fn shares_whole(shares: &[(BookId, u32)]) -> bool {
    !shares.is_empty() && shares.iter().map(|(_, s)| *s as u64).sum::<u64>() == WHOLE as u64 && shares.iter().all(|(_, s)| *s > 0)
}

impl Config {
    pub fn schedule(&self, id: &str, day: NaiveDate) -> Option<&Schedule> {
        self.schedules.get(id)?.iter().rev().find(|s| s.effective_from <= day)
    }

    /// One version of a schedule: the one a layer was written under.
    pub fn schedule_version(&self, id: &str, version: u32) -> Option<&Schedule> {
        self.schedules.get(id)?.iter().find(|s| s.version == version)
    }

    pub fn fragment_on(&self, id: &FragmentId, day: NaiveDate) -> Option<&Fragment> {
        self.fragments.get(id)?.iter().rev().find(|f| f.effective_from <= day)
    }

    /// The rule in force on a book on a day, with the fragments it follows.
    pub fn composed(&self, book: &BookId, day: NaiveDate) -> Option<(&Rule, Composed<'_>)> {
        let rule = self.books.get(book)?.rule_on(day)?;
        let fragments: Vec<&Fragment> = rule.fragments.iter().filter_map(|f| self.fragment_on(f, day)).collect();
        Some((rule, Composed::of(rule, &fragments)))
    }

    fn check_rule(&self, rule: &Rule, from: NaiveDate) -> Result<(), Reject> {
        // Every version of every fragment that could apply from the date:
        // the rule must hold under each of them.
        let mut days = BTreeSet::from([from]);
        for id in &rule.fragments {
            let versions = self
                .fragments
                .get(id)
                .ok_or_else(|| Reject::new("unknown_fragment", format!("fragment {id} is not active")))?;
            if !versions.iter().any(|f| f.effective_from <= from) {
                return reject("unknown_fragment", format!("fragment {id} is not in effect on {from}"));
            }
            days.extend(versions.iter().map(|f| f.effective_from).filter(|d| *d > from));
        }
        for day in days {
            let fragments: Vec<&Fragment> = rule.fragments.iter().filter_map(|f| self.fragment_on(f, day)).collect();
            let composed = Composed::of(rule, &fragments);
            if composed.lines.len() > MAX_COMPOSED_LINES {
                return reject("invalid_rule", format!("a rule and its fragments carry at most {MAX_COMPOSED_LINES} lines"));
            }
            composed.validate(day).map_err(|e| Reject::new("invalid_rule", e.to_string()))?;
        }
        Ok(())
    }

    /// Apply one change to the configuration. Layer and money changes are
    /// not configuration and are checked and applied by the state machine.
    fn apply(&mut self, change: &Change, today: NaiveDate, max_backdate_days: u32) -> Result<(), Reject> {
        match change {
            Change::ActivateSchedule(s) => {
                s.validate().map_err(|e| Reject::new("invalid_schedule", e))?;
                let versions = self.schedules.entry(s.id.clone()).or_default();
                if let Some(last) = versions.last() {
                    if s.version != last.version + 1 || s.effective_from < last.effective_from {
                        return reject("config_conflict", format!("schedule {} continues from version {}", s.id, last.version));
                    }
                } else if s.version != 1 {
                    return reject("config_conflict", "a schedule starts at version 1");
                }
                versions.push(s.clone());
                Ok(())
            }
            Change::ActivateFragment(f) => {
                if f.effective_from < today {
                    return reject("backdated_fragment", "a shared fragment changes from today or later, never in the past");
                }
                let versions = self.fragments.entry(f.id.clone()).or_default();
                let expected = versions.last().map_or(1, |l| l.version + 1);
                if f.version != expected || versions.last().is_some_and(|l| f.effective_from < l.effective_from) {
                    return reject("config_conflict", format!("fragment {} continues at version {expected}", f.id));
                }
                versions.push(f.clone());
                // One change, one activation: every book that follows the
                // fragment must still be whole under the new version.
                let followers: Vec<Rule> = self
                    .books
                    .values()
                    .filter_map(|b| b.rules.last())
                    .filter(|r| r.fragments.contains(&f.id))
                    .cloned()
                    .collect();
                for rule in followers {
                    self.check_rule(&rule, f.effective_from.max(rule.effective_from))
                        .map_err(|e| Reject::new(&e.code, format!("book {}: {}", rule.book, e.message)))?;
                }
                Ok(())
            }
            Change::CreateBook {
                id,
                name,
                engagement,
                owner,
                house,
            } => {
                if self.books.contains_key(id) {
                    return reject("config_conflict", format!("book {id} already exists"));
                }
                if let Some(e) = engagement {
                    if !self.structure.engagements.contains_key(e) {
                        return reject("unknown_engagement", format!("engagement {e} is not known"));
                    }
                }
                if let Some(o) = owner {
                    if !self.structure.parties.contains_key(o) {
                        return reject("unknown_party", format!("{o} is not in the structure"));
                    }
                }
                self.books.insert(
                    id.clone(),
                    Book {
                        id: id.clone(),
                        name: name.clone(),
                        engagement: engagement.clone(),
                        owner: owner.clone(),
                        house: *house,
                        closed_to_new_from: None,
                        rules: vec![],
                        watch_until: None,
                    },
                );
                Ok(())
            }
            Change::ActivateRule(rule) => {
                let book = self
                    .books
                    .get(&rule.book)
                    .ok_or_else(|| Reject::new("unknown_book", format!("book {} does not exist", rule.book)))?;
                if book.house {
                    return reject("house_book", "the house book earns no commission and has no rule");
                }
                let expected = book.rules.last().map_or(1, |r| r.version + 1);
                if rule.version != expected {
                    return reject("config_conflict", format!("the next rule version on book {} is {expected}", rule.book));
                }
                if book.rules.last().is_some_and(|r| rule.effective_from < r.effective_from) {
                    return reject("config_conflict", "a rule cannot take effect before the version it replaces");
                }
                if rule.instructions_ref.trim().is_empty() {
                    return reject("invalid_rule", "a rule refers to the signed instructions that support it");
                }
                let limit = today.checked_sub_days(chrono::Days::new(max_backdate_days as u64)).unwrap_or(today);
                if rule.effective_from < limit {
                    return reject("backdating_limit", format!("a rule can be backdated to {limit} at the earliest"));
                }
                self.check_rule(rule, rule.effective_from)?;
                if rule.effective_from < today {
                    // Only who is paid base commission can change in the
                    // past. Overrides and fees change from today.
                    let extras = |r: &Rule| -> Vec<Line> { r.lines.iter().filter(|l| l.component != Component::Base).cloned().collect() };
                    let before = book.rule_on(rule.effective_from);
                    if before.is_some_and(|b| extras(b) != extras(rule) || b.fragments != rule.fragments) {
                        return reject(
                            "backdated_override",
                            "a backdated rule may change base commission lines only; overrides, fees and fragments change from today",
                        );
                    }
                }
                if let Some(b) = self.books.get_mut(&rule.book) {
                    b.rules.push(rule.clone());
                }
                Ok(())
            }
            Change::CloseBook { book, from } => {
                let b = self
                    .books
                    .get_mut(book)
                    .ok_or_else(|| Reject::new("unknown_book", format!("book {book} does not exist")))?;
                b.closed_to_new_from = Some(*from);
                Ok(())
            }
            Change::WatchBook { book, until } => {
                let b = self
                    .books
                    .get_mut(book)
                    .ok_or_else(|| Reject::new("unknown_book", format!("book {book} does not exist")))?;
                b.watch_until = Some(*until);
                Ok(())
            }
            Change::StartEngagement { id, engagement } => structure(self.structure.start_engagement(id.clone(), engagement.clone())),
            Change::EndEngagement { id, on } => structure(self.structure.end_engagement(id, *on)),
            Change::Relate(r) => structure(self.structure.relate(r.clone())),
            Change::EndRelation { child, parent, kind, on } => structure(self.structure.end_relation(child, parent, *kind, *on)),
            Change::AdviserChangeSetting(s) => {
                if s.version != self.adviser_change.version + 1 {
                    return reject("config_conflict", format!("the next setting version is {}", self.adviser_change.version + 1));
                }
                self.adviser_change = *s;
                Ok(())
            }
            Change::ReleaseOwedElsewhere(o) => {
                self.release_owed_elsewhere = *o;
                Ok(())
            }
            Change::SetStaffPaths { staff, paths } => {
                self.staff_paths.insert(staff.clone(), paths.clone());
                Ok(())
            }
            Change::MoveLayers { .. } | Change::SetServicing { .. } | Change::Adjustment { .. } | Change::CorrectTerms { .. } => Ok(()),
        }
    }
}

/// What a book is to do when its adviser moves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
// A request type built once per move: boxing the larger variant buys nothing.
#[allow(clippy::large_enum_variant)]
pub enum BookOutcome {
    /// The book stays with the previous provider; someone there services it.
    Stays { book: BookId, new_servicing: EngagementId },
    /// The book, or the named policies in it, move with the adviser into a
    /// new book of their own under the new arrangement.
    Moves {
        book: BookId,
        /// None moves every policy in the book.
        policies: Option<BTreeSet<PolicyId>>,
        new_book: BookId,
        new_book_name: String,
        /// The previous provider's signed release, or the agreement clause
        /// that says the adviser owns the book.
        release_ref: String,
        /// The ongoing parties' lines, as shares of the remainder.
        lines: Vec<Line>,
        liability: Vec<crate::rules::LiabilityLine>,
        fragments: Vec<FragmentId>,
        /// What the previous provider keeps: a share of the whole renewal
        /// commission until a date, or for good.
        tail: Option<(PartyId, u32, Option<NaiveDate>)>,
        /// Worked examples of what the new book's rule pays, tail
        /// included. They are checked like any rule's before the move
        /// activates.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        examples: Vec<crate::rules::RuleExample>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveRequest {
    pub from_engagement: EngagementId,
    pub new_engagement: EngagementId,
    /// The provider or authorised body the adviser is joining.
    pub to: PartyId,
    pub on: NaiveDate,
    pub accreditations: BTreeSet<String>,
    pub books: Vec<BookOutcome>,
}

impl DistributionState {
    /// Turn an adviser's move into the changes that carry it out: one
    /// engagement ends, another starts, and each book is settled as the
    /// parties' agreements and signed instructions say. The result is
    /// proposed like any other change set, so it has the same dry run and
    /// the same approvals. Nothing changes here.
    pub fn plan_move(&self, request: &MoveRequest) -> Result<Vec<Change>, Reject> {
        let old = self
            .config
            .structure
            .engagements
            .get(&request.from_engagement)
            .ok_or_else(|| Reject::new("unknown_engagement", format!("engagement {} is not known", request.from_engagement)))?;
        let mut changes = vec![
            Change::EndEngagement {
                id: request.from_engagement.clone(),
                on: request.on,
            },
            Change::StartEngagement {
                id: request.new_engagement.clone(),
                engagement: Engagement {
                    adviser: old.adviser.clone(),
                    with: request.to.clone(),
                    from: request.on,
                    to: None,
                    accreditations: request.accreditations.clone(),
                },
            },
        ];
        let watch_until = request
            .on
            .checked_add_months(chrono::Months::new(self.settings.watch_months_after_move))
            .unwrap_or(request.on);
        for outcome in &request.books {
            match outcome {
                BookOutcome::Stays { book, new_servicing } => {
                    if !self.config.books.contains_key(book) {
                        return reject("unknown_book", format!("book {book} does not exist"));
                    }
                    for policy in self.policies_in(book, None, request.on) {
                        if self
                            .policies
                            .get(&policy)
                            .is_some_and(|p| p.servicing.as_ref() == Some(&request.from_engagement))
                        {
                            changes.push(Change::SetServicing {
                                policy,
                                engagement: Some(new_servicing.clone()),
                            });
                        }
                    }
                    changes.push(Change::WatchBook {
                        book: book.clone(),
                        until: watch_until,
                    });
                }
                BookOutcome::Moves {
                    book,
                    policies,
                    new_book,
                    new_book_name,
                    release_ref,
                    lines,
                    liability,
                    fragments,
                    tail,
                    examples,
                } => {
                    if !self.config.books.contains_key(book) {
                        return reject("unknown_book", format!("book {book} does not exist"));
                    }
                    if release_ref.trim().is_empty() {
                        return reject(
                            "release_required",
                            "a book moves with the previous provider's signed release, or an agreement that says the adviser owns it",
                        );
                    }
                    let moving = self.policies_in(book, policies.as_ref(), request.on);
                    let layers: Vec<String> = self
                        .layers
                        .values()
                        .filter(|l| moving.contains(&l.policy) && l.books_on(request.on).iter().any(|(b, _)| b == book))
                        .map(|l| l.key())
                        .collect();
                    let mut rule_lines = lines.clone();
                    if let Some((payee, ppm, until)) = tail {
                        rule_lines.push(Line {
                            payee: payee.clone(),
                            earner: None,
                            component: Component::Base,
                            commission_type: Some(CommissionType::Renewal),
                            share: Share::OfComponent(*ppm),
                            from: request.on,
                            until: *until,
                            disputed: false,
                            absorbs_rounding: false,
                        });
                    }
                    changes.push(Change::CreateBook {
                        id: new_book.clone(),
                        name: new_book_name.clone(),
                        engagement: Some(request.new_engagement.clone()),
                        owner: Some(request.to.clone()),
                        house: false,
                    });
                    changes.push(Change::ActivateRule(Rule {
                        book: new_book.clone(),
                        version: 1,
                        effective_from: request.on,
                        lines: rule_lines,
                        liability: liability.clone(),
                        fragments: fragments.clone(),
                        clawback_by_cause: false,
                        examples: examples.clone(),
                        instructions_ref: release_ref.clone(),
                    }));
                    // A layer split between books keeps its other books.
                    let mut by_target: BTreeMap<Vec<(BookId, u32)>, Vec<String>> = BTreeMap::new();
                    for key in layers {
                        if let Some(l) = self.layers.get(&key) {
                            let to = l
                                .books_on(request.on)
                                .iter()
                                .map(|(b, s)| (if b == book { new_book.clone() } else { b.clone() }, *s))
                                .collect();
                            by_target.entry(to).or_default().push(key);
                        }
                    }
                    for (to, layers) in by_target {
                        changes.push(Change::MoveLayers { layers, to, from: request.on });
                    }
                    for policy in moving {
                        changes.push(Change::SetServicing {
                            policy,
                            engagement: Some(request.new_engagement.clone()),
                        });
                    }
                    for b in [book, new_book] {
                        changes.push(Change::WatchBook {
                            book: b.clone(),
                            until: watch_until,
                        });
                    }
                }
            }
        }
        Ok(changes)
    }

    /// The policies with a layer in a book on a day, narrowed to a named set.
    fn policies_in(&self, book: &BookId, only: Option<&BTreeSet<PolicyId>>, day: NaiveDate) -> BTreeSet<PolicyId> {
        self.layers
            .values()
            .filter(|l| l.books_on(day).iter().any(|(b, _)| b == book))
            .map(|l| l.policy.clone())
            .filter(|p| only.is_none_or(|o| o.contains(p)))
            .collect()
    }

    /// Check a whole change set: configuration against a copy, the rest
    /// against that copy and the current books.
    fn check_changes(&self, changes: &[Change]) -> Result<Config, Reject> {
        let today = self.date()?;
        if changes.is_empty() {
            return reject("empty_change", "a change set changes something");
        }
        let mut trial = self.config.clone();
        let mut references = BTreeSet::new();
        for change in changes {
            trial.apply(change, today, self.settings.max_backdate_days)?;
            match change {
                Change::ActivateRule(rule) => {
                    let fragments: Vec<&Fragment> = rule.fragments.iter().filter_map(|f| trial.fragment_on(f, rule.effective_from)).collect();
                    if rule.examples.is_empty() && self.settings.rule_examples_required {
                        return reject("rule_examples", "a rule is activated with worked examples of what it pays");
                    }
                    for example in &rule.examples {
                        if example.on < rule.effective_from {
                            return reject("rule_examples", format!("example {} is before the rule takes effect", example.name));
                        }
                        let fragments: Vec<&Fragment> = rule.fragments.iter().filter_map(|f| trial.fragment_on(f, example.on)).collect();
                        Composed::of(rule, &fragments)
                            .check_example(example)
                            .map_err(|e| Reject::new("rule_examples", e.to_string()))?;
                    }
                    for payee in Composed::of(rule, &fragments).payees() {
                        if self.payees.get(&payee).is_none_or(|p| p.agreement_ref.is_none()) {
                            return reject("payee_without_agreement", format!("{payee} is not a payee with a current agreement"));
                        }
                    }
                }
                Change::CorrectTerms { layer, option } => {
                    self.check_correction(layer, option)?;
                }
                Change::MoveLayers { layers, to, from } => {
                    if !shares_whole(to) {
                        return reject("invalid_shares", "a layer's books take shares that sum to the whole");
                    }
                    for (b, _) in to {
                        if !trial.books.contains_key(b) {
                            return reject("unknown_book", format!("book {b} does not exist"));
                        }
                    }
                    for key in layers {
                        let l = self
                            .layers
                            .get(key)
                            .ok_or_else(|| Reject::new("unknown_layer", format!("layer {key} is not known")))?;
                        if l.assignments.last().is_some_and(|a| a.from > *from) {
                            return reject("assignment_order", format!("layer {key} already has a later assignment"));
                        }
                    }
                }
                Change::SetServicing { policy, engagement } => {
                    if !self.policies.contains_key(policy) {
                        return reject("unknown_policy", format!("policy {policy} is not known to distribution"));
                    }
                    if let Some(e) = engagement {
                        if !trial.structure.engagements.contains_key(e) {
                            return reject("unknown_engagement", format!("engagement {e} is not known"));
                        }
                    }
                }
                Change::Adjustment {
                    reference,
                    payee,
                    amount_minor,
                    kind,
                    ..
                } => {
                    if !self.payees.contains_key(payee) {
                        return reject("unknown_payee", format!("{payee} is not a registered payee"));
                    }
                    let signed_ok = matches!(kind, AdjustmentKind::BookLevel { .. }) || *amount_minor > 0;
                    if *amount_minor == 0 || !signed_ok {
                        return reject(
                            "invalid_amount",
                            "an advance or a retention is a positive amount; a book-level item is not zero",
                        );
                    }
                    if self.requested.contains_key(&adjustment_reference(reference)) || !references.insert(reference.clone()) {
                        return reject("duplicate_reference", format!("adjustment {reference} has already been used"));
                    }
                }
                _ => {}
            }
        }
        Ok(trial)
    }

    pub(crate) fn propose_change(&mut self, ctx: &mut Ctx, actor: &Actor, id: String, changes: Vec<Change>, instructions_ref: String) -> Result<(), Reject> {
        actor.person()?;
        let maker = ctx
            .decision
            .clone()
            .ok_or_else(|| Reject::new("caller_not_allowed", "a change is proposed by a person"))?;
        if self.change_sets.contains_key(&id) {
            return reject("duplicate_change", format!("change {id} already exists"));
        }
        if instructions_ref.trim().is_empty() {
            return reject("instructions_required", "a change refers to the signed instructions or change record behind it");
        }
        self.check_changes(&changes)?;
        let mut h = Sha256::new();
        h.update(b"ic.distribution.change.v1");
        h.update(serde_json::to_vec(&changes).unwrap_or_default());
        let hash = hex::encode(h.finalize());
        ctx.emit(Event::ChangeProposed {
            id: id.clone(),
            hash: hash.clone(),
        });
        self.change_sets.insert(
            id.clone(),
            ChangeSet {
                id,
                changes,
                instructions_ref,
                hash,
                maker,
                proposed_on: self.date()?,
                approval: None,
                compliance: None,
                state: ChangeState::Proposed,
                rejected_because: None,
            },
        );
        Ok(())
    }

    pub(crate) fn reject_change(&mut self, ctx: &mut Ctx, id: &str, reason: String) -> Result<(), Reject> {
        let set = self
            .change_sets
            .get_mut(id)
            .ok_or_else(|| Reject::new("unknown_change", format!("change {id} is not known")))?;
        if set.state != ChangeState::Proposed {
            return reject("change_settled", "the change has already been activated or rejected");
        }
        set.state = ChangeState::Rejected;
        set.rejected_because = Some(reason);
        ctx.emit(Event::ChangeRejected { id: id.to_string() });
        Ok(())
    }

    pub(crate) fn approve_change(&mut self, ctx: &mut Ctx, actor: &Actor, id: &str, compliance: bool) -> Result<(), Reject> {
        let staff = actor.person()?;
        let set = self
            .change_sets
            .get(id)
            .ok_or_else(|| Reject::new("unknown_change", format!("change {id} is not known")))?;
        if set.state != ChangeState::Proposed {
            return reject("change_settled", "the change has already been activated or rejected");
        }
        let action = if compliance { "config.approve_compliance" } else { "config.approve" };
        let ask = Ask::new(action, "*")
            .amount(LimitKind::Adjustment, set.amount())
            .maker(set.maker.staff == staff.id);
        let decision = self.authorise(staff, &ask)?;
        let other = if compliance { &set.approval } else { &set.compliance };
        if other.as_ref().is_some_and(|d| d.staff == staff.id) {
            return reject("not_authorised", "the two approvals come from two people");
        }
        if (if compliance { &set.compliance } else { &set.approval }).is_some() {
            return reject("already_approved", "that approval has already been given");
        }
        let set = self
            .change_sets
            .get_mut(id)
            .ok_or_else(|| Reject::new("unknown_change", "the change is gone"))?;
        if compliance {
            set.compliance = Some(decision);
        } else {
            set.approval = Some(decision);
        }
        ctx.emit(Event::ChangeApproved {
            id: id.to_string(),
            by: staff.id.clone(),
            compliance,
        });
        if set.approval.is_some() && set.compliance.is_some() {
            let set = set.clone();
            self.activate(ctx, &set)?;
        }
        Ok(())
    }

    /// Activate an approved change set as one step. It is checked again
    /// first: the world may have moved since it was proposed.
    fn activate(&mut self, ctx: &mut Ctx, set: &ChangeSet) -> Result<(), Reject> {
        let today = self.date()?;
        let config = self.check_changes(&set.changes)?;
        self.config = config;
        for change in &set.changes {
            match change {
                Change::CreateBook { id, .. } => ctx.emit(Event::BookCreated { book: id.clone() }),
                Change::ActivateRule(rule) => {
                    ctx.emit(Event::RuleActivated {
                        book: rule.book.clone(),
                        version: rule.version,
                        hash: rule.content_hash(),
                    });
                    if rule.effective_from < today {
                        self.replay_book(ctx, &rule.book, rule.effective_from);
                    }
                }
                Change::StartEngagement { id, engagement } => ctx.emit(Event::EngagementStarted {
                    id: id.clone(),
                    adviser: engagement.adviser.clone(),
                    with: engagement.with.clone(),
                    from: engagement.from,
                }),
                Change::EndEngagement { id, on } => ctx.emit(Event::EngagementEnded { id: id.clone(), on: *on }),
                Change::MoveLayers { layers, to, from } => {
                    for key in layers {
                        if let Some(l) = self.layers.get_mut(key) {
                            l.assignments.retain(|a| a.from != *from);
                            l.assignments.push(BookAssignment {
                                from: *from,
                                shares: to.clone(),
                            });
                            ctx.emit(Event::LayerAssigned {
                                policy: l.policy.clone(),
                                layer: key.clone(),
                                books: to.clone(),
                                from: *from,
                            });
                        }
                    }
                }
                Change::SetServicing { policy, engagement } => self.set_servicing(ctx, policy, engagement.clone(), today),
                Change::CorrectTerms { layer, option } => self.correct_terms(ctx, layer, option),
                Change::Adjustment {
                    reference,
                    payee,
                    partition,
                    currency,
                    amount_minor,
                    kind,
                } => {
                    let party = |ledger| CounterpartyAccount::Party { party: payee.clone(), ledger };
                    // Debit first, credit second, for a positive amount.
                    let (kind_name, debit, credit) = match kind {
                        AdjustmentKind::BookLevel { product, cover_type } => (
                            "commission_adjustment",
                            CounterpartyAccount::Internal(InternalLedger::CommissionExpense {
                                product: product.clone(),
                                cover_type: cover_type.clone(),
                            }),
                            party(PartyLedger::CommissionPayable),
                        ),
                        AdjustmentKind::Advance => ("commission_advance", party(PartyLedger::Advance), party(PartyLedger::CommissionPayable)),
                        AdjustmentKind::RetentionHold => ("retention_held", party(PartyLedger::CommissionPayable), party(PartyLedger::Retention)),
                        AdjustmentKind::RetentionRelease => ("retention_released", party(PartyLedger::Retention), party(PartyLedger::CommissionPayable)),
                    };
                    let mut detail = BTreeMap::new();
                    detail.insert("change".to_string(), set.id.clone());
                    let posting = CounterpartyPosting {
                        reference: adjustment_reference(reference),
                        kind: kind_name.to_string(),
                        currency: *currency,
                        value_date: None,
                        policy: None,
                        reverses: None,
                        lines: vec![
                            CounterpartyLine {
                                account: debit,
                                amount_minor: *amount_minor,
                            },
                            CounterpartyLine {
                                account: credit,
                                amount_minor: -*amount_minor,
                            },
                        ],
                        detail,
                    };
                    self.request(ctx, *partition, posting, Some(format!("change:{}", set.id)));
                }
                _ => {}
            }
        }
        if let Some(s) = self.change_sets.get_mut(&set.id) {
            s.state = ChangeState::Activated;
        }
        ctx.emit(Event::ChangeActivated {
            id: set.id.clone(),
            hash: set.hash.clone(),
        });
        Ok(())
    }

    pub(crate) fn set_servicing(&mut self, ctx: &mut Ctx, policy: &PolicyId, engagement: Option<EngagementId>, on: NaiveDate) {
        if let Some(p) = self.policies.get_mut(policy) {
            if p.servicing != engagement {
                ctx.emit(Event::ServicingChanged {
                    policy: policy.clone(),
                    from: p.servicing.clone(),
                    to: engagement.clone(),
                    on,
                });
                p.servicing = engagement.clone();
            }
        }
        // A new servicing adviser settles the task a licence event raised.
        if let Some(e) = engagement {
            for t in self
                .tasks
                .values_mut()
                .filter(|t| t.kind == "needs_servicing_adviser" && t.subject == policy.0 && t.done.is_none())
            {
                t.done = Some(format!("servicing appointed: {e}"));
            }
        }
    }

    /// Record how a layer was written. The engagement must be able to advise
    /// on the product that day, and the books must be open to new business.
    pub(crate) fn write_layer(&mut self, ctx: &mut Ctx, w: WriteLayer) -> Result<(), Reject> {
        let key = layer_key(&w.policy, &w.cover, w.layer);
        let layer = self
            .layers
            .get(&key)
            .ok_or_else(|| Reject::new("unknown_layer", format!("layer {key} is not known")))?;
        let product = self.policies.get(&w.policy).map(|p| p.product_code.clone()).unwrap_or_default();
        if layer.terms.is_some() || !layer.writing.is_empty() {
            return reject("layer_written", "who wrote a layer, and on what terms, is a historical fact that never changes");
        }
        if w.writing.is_empty() || w.writing.iter().map(|(_, s)| *s as u64).sum::<u64>() != WHOLE as u64 || !shares_whole(&w.books) {
            return reject("invalid_shares", "the writing engagements and the books each take shares that sum to the whole");
        }
        for (e, _) in &w.writing {
            self.config
                .structure
                .may_advise(e, &product, layer.start)
                .map_err(|m| Reject::new("not_authorised_to_advise", m))?;
        }
        for (b, _) in &w.books {
            let book = self
                .config
                .books
                .get(b)
                .ok_or_else(|| Reject::new("unknown_book", format!("book {b} does not exist")))?;
            if book.closed_to_new_from.is_some_and(|c| layer.start >= c) {
                return reject("book_closed", format!("book {b} is closed to new business"));
            }
            // An adviser's book takes only what that adviser wrote; a book
            // with no engagement (a team's, or the house) takes anyone's.
            if let Some(owner) = book.engagement.as_ref().filter(|e| !w.writing.iter().any(|(x, _)| x == *e)) {
                return reject(
                    "book_not_writers",
                    format!("book {b} belongs to engagement {owner}, which did not write this layer"),
                );
            }
            if !book.house && book.rule_on(layer.start).is_none() {
                return reject("no_rule", format!("book {b} has no distribution rule in force on {}", layer.start));
            }
        }
        let schedule = self
            .config
            .schedule(&w.schedule, layer.start)
            .ok_or_else(|| Reject::new("unknown_schedule", format!("schedule {} is not in effect on {}", w.schedule, layer.start)))?;
        if w.annual_premium_minor <= 0 {
            return reject("invalid_amount", "a layer's annual premium is positive");
        }
        let mut terms = LayerTerms::from_schedule(schedule, &w.option, w.annual_premium_minor)
            .ok_or_else(|| Reject::new("unknown_option", format!("schedule {} has no option {}", w.schedule, w.option)))?;
        if w.replacement || (layer.indexation && !schedule.initial_on_indexation) {
            // Replacement cover and indexation steps earn trail only.
            terms.rates.initial_ppm = 0;
        }
        let from = layer.start;
        if let Some(l) = self.layers.get_mut(&key) {
            l.writing = w.writing.clone();
            l.terms = Some(terms);
            l.assignments = vec![BookAssignment { from, shares: w.books.clone() }];
        }
        if let Some(p) = self.policies.get_mut(&w.policy) {
            if p.default.is_none() {
                p.default = Some(DefaultAssignment {
                    writing: w.writing[0].0.clone(),
                    book: w.books[0].0.clone(),
                });
            }
            if p.servicing.is_none() {
                let e = w.writing[0].0.clone();
                self.set_servicing(ctx, &w.policy, Some(e), from);
            }
        }
        if w.replacement {
            let date = self.today.unwrap_or(from);
            // A layer is written once, so this is its only declaration.
            if let Some(l) = self.layers.get_mut(&key) {
                l.conduct.push(ConductRecord {
                    measure: ConductMeasure::ReplacementWritten,
                    date,
                    relates_to: None,
                });
            }
        }
        if !w.replacement && !self.layers.get(&key).is_some_and(|l| l.indexation) {
            let today = self.date()?;
            self.match_replacement(ctx, &key, today);
        }
        ctx.emit(Event::LayerAssigned {
            policy: w.policy,
            layer: key,
            books: w.books,
            from,
        });
        Ok(())
    }

    /// The customer appoints a new servicing adviser, or none. Servicing and
    /// access change at once, because they follow the customer. What happens
    /// to renewal commission is the insurer's setting: at once, after a
    /// stand-down (a future-dated assignment, so no timer has to fire), or
    /// not at all. The previous adviser's open exposure items stay theirs.
    pub(crate) fn appoint_adviser(&mut self, ctx: &mut Ctx, policy: PolicyId, engagement: Option<EngagementId>, book: Option<BookId>) -> Result<(), Reject> {
        let today = self.date()?;
        let record = self
            .policies
            .get(&policy)
            .ok_or_else(|| Reject::new("unknown_policy", format!("policy {policy} is not known to distribution")))?;
        if let Some(e) = &engagement {
            let eng = self
                .config
                .structure
                .engagements
                .get(e)
                .ok_or_else(|| Reject::new("unknown_engagement", format!("engagement {e} is not known")))?;
            if !eng.on(today) || self.config.structure.engagement_impairment(e, today).is_some() {
                return reject("adviser_unavailable", "that adviser cannot act for customers today");
            }
        }
        let setting = self.config.adviser_change;
        let moves_from = match setting.mode {
            AdviserChangeMode::MovesAtOnce => Some(today),
            AdviserChangeMode::StandDown { months } => today.checked_add_months(chrono::Months::new(months)),
            AdviserChangeMode::StaysWithPrevious => None,
        };
        let keys = record.layers.clone();
        if let (Some(from), Some(book), Some(_)) = (moves_from, &book, &engagement) {
            let target = self
                .config
                .books
                .get(book)
                .ok_or_else(|| Reject::new("unknown_book", format!("book {book} does not exist")))?;
            if target.engagement != engagement {
                return reject("wrong_book", "commission moves to a book of the adviser the customer appointed");
            }
            for key in &keys {
                if let Some(l) = self.layers.get_mut(key).filter(|l| !l.assignments.is_empty()) {
                    l.assignments.retain(|a| a.from < from);
                    l.assignments.push(BookAssignment {
                        from,
                        shares: vec![(book.clone(), WHOLE)],
                    });
                    ctx.emit(Event::LayerAssigned {
                        policy: policy.clone(),
                        layer: key.clone(),
                        books: vec![(book.clone(), WHOLE)],
                        from,
                    });
                }
            }
        }
        if let Some(p) = self.policies.get_mut(&policy) {
            p.adviser_change_setting = Some(setting.version);
            if let (Some(e), Some(b)) = (&engagement, &book) {
                p.default = Some(DefaultAssignment {
                    writing: e.clone(),
                    book: b.clone(),
                });
            }
        }
        self.set_servicing(ctx, &policy, engagement, today);
        Ok(())
    }
}

pub(crate) fn adjustment_reference(reference: &str) -> String {
    format!("adj:{reference}")
}
