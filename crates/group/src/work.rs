//! The work queue of a scheme: membership changes on their way to the core
//! in chunks, what happens when the core answers, and everything that
//! follows from a change once it is applied: who the member is, cover above
//! the automatic amount, a leaver's continuation option, renewal and ending.

use std::collections::BTreeSet;

use chrono::{Days, Months, NaiveDate};
use ic_core::command::{CoverSpec, ExcessLayer, IssuePolicy, LifeSpec, MemberChange};
use ic_core::event::Event as CoreEvent;
use ic_core::machine::MAX_MEMBERSHIP_CHUNK;
use ic_core::policy::MemberPays;
use ic_core::{Command, GroupId, LayerOrigin, LifeFactors, LifeId, LifeKind, PartyId, PolicyId, PoolAggregate, Reject};
use ic_product::schedule::age_on;
use serde::{Deserialize, Serialize};

use crate::benefit::{known_category, member_cover};
use crate::command::{ExcessDecision, GroupCommand, MatchDecision, Resolution};
use crate::event::{ContinuationExcess, ExceptionKind, GroupEvent, IssueOutcome, MemberLetter, Outbound, ReinsuranceNotice, UnderwritingRequest};
use crate::mapping::RowReject;
use crate::model::{ChangeKind, JoinAs, Proposed};
use crate::record::MemberAttrs;
use crate::state::{
    CaseStatus, ClassLine, ContinuedExcess, ContinuedStatus, ExcessCase, FileStatus, GroupLine, GroupState, InFlight, Invoice, Offer, OfferStatus, Renewal,
    Scheme, SchemeStatus,
};
use crate::terms::{ExcessLoading, MemberBasis, NoAgreement, SchemeTerms};

/// The core asks for a retry: nothing is wrong with what was sent.
const TRANSIENT: [&str; 2] = ["invoice_in_progress", "policy_transferring"];
/// The core refuses anything for this policy: no row is at fault.
const POLICY_LEVEL: [&str; 7] = [
    "not_in_force",
    "unknown_policy",
    "not_a_scheme",
    "unknown_product",
    "policy_transferred",
    "caller_not_allowed",
    "business_date",
];
/// Invoices kept per scheme for the employer's view.
const INVOICES_KEPT: usize = 36;

/// What happens in the module once the core has accepted a command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "then", rename_all = "snake_case")]
pub enum After {
    Linked { employee_id: String, party: PartyId, how: String },
    LayerWritten { case: u64 },
    LayerEnded { case: u64 },
    ContinuationIssued { offer: u64 },
    SchemeCancelled { effective: NaiveDate },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "work", rename_all = "snake_case")]
pub enum Work {
    /// A change from a file, or one the module raised itself.
    Member {
        change: Proposed,
        /// The customer master's answer, for a joiner on a named scheme.
        resolution: Option<Resolution>,
        /// The life a joiner is given, fixed when the change is sent.
        life: Option<LifeId>,
    },
    Reinstate {
        employee_id: String,
    },
    /// Set or lift the ceiling on a member's automatic cover.
    Cap {
        employee_id: String,
        cap_minor: Option<i64>,
        effective: NaiveDate,
        reference: Option<String>,
    },
    Pool {
        pool: String,
        category: String,
        effective: NaiveDate,
        headcount: u32,
        total_salary_minor: i64,
    },
    /// A single command for the core that is not a membership change.
    Core {
        group: GroupId,
        command: Box<Command>,
        then: After,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub id: u64,
    /// The file the work came from, if any.
    pub file: Option<String>,
    pub work: Work,
}

impl Item {
    /// Who or what the work is about, for messages and for keeping two
    /// changes to one member out of the same chunk.
    fn subject(&self) -> String {
        match &self.work {
            Work::Member { change, .. } => change.employee_id.clone(),
            Work::Reinstate { employee_id } | Work::Cap { employee_id, .. } => employee_id.clone(),
            Work::Pool { pool, .. } => format!("pool:{pool}"),
            Work::Core { then, .. } => match then {
                After::Linked { employee_id, .. } => employee_id.clone(),
                After::LayerWritten { case } | After::LayerEnded { case } => format!("case:{case}"),
                After::ContinuationIssued { offer } => format!("offer:{offer}"),
                After::SchemeCancelled { .. } => "scheme".to_string(),
            },
        }
    }

    fn kind(&self) -> &'static str {
        match &self.work {
            Work::Member { change, .. } => change.kind.code(),
            Work::Reinstate { .. } => "reinstate",
            Work::Cap { .. } => "cover_ceiling",
            Work::Pool { .. } => "pool_declaration",
            Work::Core { command, .. } => command.name(),
        }
    }
}

fn factors(attrs: &MemberAttrs, cap_minor: Option<i64>) -> LifeFactors {
    LifeFactors {
        auto_cover_cap_minor: cap_minor,
        ..attrs.factors()
    }
}

fn leave(employee_id: &str, attrs: &MemberAttrs, effective: NaiveDate) -> Proposed {
    Proposed {
        row: 0,
        employee_id: employee_id.to_string(),
        kind: ChangeKind::Leave,
        effective,
        attrs: attrs.clone(),
        fields: vec![],
        fingerprint: None,
        needs_backdating_approval: false,
    }
}

/// The terms for the next scheme year where nothing was renegotiated.
fn rolled(terms: &SchemeTerms) -> SchemeTerms {
    SchemeTerms {
        scheme_year_start: terms.renewal_date,
        renewal_date: terms.renewal_date.checked_add_months(Months::new(12)).unwrap_or(terms.renewal_date),
        ..terms.clone()
    }
}

impl GroupState {
    pub(crate) fn enqueue(&mut self, s: &mut Scheme, file: Option<String>, work: Work) {
        s.next_item += 1;
        s.queue.push_back(Item { id: s.next_item, file, work });
    }

    fn enqueue_core(&mut self, s: &mut Scheme, command: Command, then: After) {
        let group = s.group.clone();
        self.enqueue(
            s,
            None,
            Work::Core {
                group,
                command: Box::new(command),
                then,
            },
        );
    }

    /// The membership change the core is asked to make for a piece of work.
    /// `Ok(None)` is work that touches only this module. `Err` is work that
    /// no longer fits the membership.
    fn build(&self, s: &Scheme, item: &mut Item) -> Result<Option<MemberChange>, &'static str> {
        match &mut item.work {
            Work::Member { change, resolution, life } => match &change.kind {
                ChangeKind::Join => {
                    if s.membership.active(&change.employee_id).is_some() {
                        return Err("already_member");
                    }
                    let id = s.membership.next_life(&change.employee_id);
                    *life = Some(id.clone());
                    let kind = match resolution {
                        Some(Resolution::Linked { party }) | Some(Resolution::Created { party }) => LifeKind::Named { party: party.clone() },
                        _ => LifeKind::UnnamedMember {
                            member_ref: change.employee_id.clone(),
                            linked_party: None,
                        },
                    };
                    Ok(Some(MemberChange::Joined {
                        life: LifeSpec {
                            id,
                            kind,
                            factors: factors(&change.attrs, None),
                        },
                        effective: change.effective,
                    }))
                }
                ChangeKind::Leave => {
                    let m = s.membership.active(&change.employee_id).ok_or("unknown_member")?;
                    Ok(Some(MemberChange::Left {
                        life: m.life.clone(),
                        effective: change.effective.max(m.joined),
                    }))
                }
                ChangeKind::Change => {
                    let m = s.membership.active(&change.employee_id).ok_or("unknown_member")?;
                    Ok(Some(MemberChange::Changed {
                        life: m.life.clone(),
                        effective: change.effective,
                        factors: factors(&change.attrs, m.auto_cap_minor),
                    }))
                }
                _ => Ok(None),
            },
            Work::Reinstate { employee_id } => {
                let m = s.membership.members.get(employee_id.as_str()).filter(|m| m.left.is_some()).ok_or("not_left")?;
                Ok(Some(MemberChange::Reinstated { life: m.life.clone() }))
            }
            Work::Cap {
                employee_id,
                cap_minor,
                effective,
                ..
            } => {
                let m = s.membership.active(employee_id).ok_or("unknown_member")?;
                Ok(Some(MemberChange::Changed {
                    life: m.life.clone(),
                    effective: (*effective).max(m.joined),
                    factors: factors(&m.attrs, *cap_minor),
                }))
            }
            Work::Pool {
                pool,
                category,
                effective,
                headcount,
                total_salary_minor,
            } => {
                let life = LifeId(format!("pool:{pool}"));
                let factors = LifeFactors {
                    category: Some(category.clone()),
                    pool: Some(PoolAggregate {
                        headcount: *headcount,
                        total_salary_minor: *total_salary_minor,
                    }),
                    ..LifeFactors::default()
                };
                Ok(Some(if s.pools.contains_key(pool.as_str()) {
                    MemberChange::Changed {
                        life,
                        effective: *effective,
                        factors,
                    }
                } else {
                    MemberChange::Joined {
                        life: LifeSpec {
                            id: life,
                            kind: LifeKind::Pool {
                                headcount: *headcount,
                                total_salary_minor: *total_salary_minor,
                            },
                            factors,
                        },
                        effective: *effective,
                    }
                }))
            }
            Work::Core { .. } => Ok(None),
        }
    }

    fn send_core(&mut self, s: &mut Scheme, group: GroupId, command: Command, items: Vec<Item>) {
        s.core_seq += 1;
        let key = format!("grp:{}:{}", s.id, s.core_seq);
        let outbox = self.send(Outbound::Core {
            key: key.clone(),
            group,
            command: Box::new(command),
        });
        self.core_keys.insert(key.clone(), s.id.clone());
        s.in_flight = Some(InFlight { key, outbox, items });
    }

    /// Send the next piece of work to the core, if nothing is in flight.
    /// One command at a time per scheme, so the core applies a scheme's
    /// changes in the order the files gave them.
    pub(crate) fn pump(&mut self, s: &mut Scheme) {
        loop {
            let stopped = s.blocked.is_some() || matches!(s.status, SchemeStatus::Terminated { .. });
            if s.in_flight.is_some() || s.waiting || stopped {
                return;
            }
            let Some(front) = s.queue.pop_front() else { return };
            if let Work::Core { group, command, .. } = &front.work {
                let (group, command) = (group.clone(), (**command).clone());
                self.send_core(s, group, command, vec![front]);
                return;
            }
            s.queue.push_front(front);
            let mut batch: Vec<Item> = Vec::new();
            let mut changes = Vec::new();
            let mut seen = BTreeSet::new();
            while batch.len() < s.chunk_limit {
                let Some(mut item) = s.queue.pop_front() else { break };
                if matches!(item.work, Work::Core { .. }) || seen.contains(&item.subject()) {
                    s.queue.push_front(item);
                    break;
                }
                match self.build(s, &mut item) {
                    Err(code) => self.refuse(s, item, code),
                    Ok(None) if batch.is_empty() => self.complete(s, item),
                    Ok(None) => {
                        s.queue.push_front(item);
                        break;
                    }
                    Ok(Some(change)) => {
                        seen.insert(item.subject());
                        changes.push(change);
                        batch.push(item);
                    }
                }
            }
            if !batch.is_empty() {
                let command = Command::ApplyMembership {
                    policy: s.policy.clone(),
                    changes,
                };
                let group = s.group.clone();
                self.send_core(s, group, command, batch);
                return;
            }
        }
    }

    /// The core's answer to the command in flight.
    fn core_result(&mut self, key: &str, error: Option<String>) -> Result<(), Reject> {
        // An answer delivered twice, or for something already dealt with,
        // changes nothing.
        let Some(scheme) = self.core_keys.remove(key) else { return Ok(()) };
        self.with_scheme(&scheme, |st, s| {
            let Some(flight) = s.in_flight.take() else { return Ok(()) };
            st.outbox.remove(&flight.outbox);
            let items = flight.items;
            match error {
                None => {
                    s.chunk_limit = (s.chunk_limit * 2).min(MAX_MEMBERSHIP_CHUNK);
                    for item in items {
                        st.complete(s, item);
                    }
                }
                Some(code) if TRANSIENT.contains(&code.as_str()) => {
                    items.into_iter().rev().for_each(|i| s.queue.push_front(i));
                    s.waiting = true;
                }
                Some(code) => {
                    let continuation = items.iter().any(|i| {
                        matches!(
                            &i.work,
                            Work::Core {
                                then: After::ContinuationIssued { .. },
                                ..
                            }
                        )
                    });
                    if POLICY_LEVEL.contains(&code.as_str()) && !continuation {
                        // No row is at fault, so there is nothing to search for.
                        items.into_iter().rev().for_each(|i| s.queue.push_front(i));
                        s.blocked = Some(code.clone());
                        st.raise(&s.id, ExceptionKind::SchemeBlocked, &code);
                        st.emit(GroupEvent::SchemeBlocked { scheme: s.id.clone(), code });
                    } else if items.len() > 1 {
                        // One of these rows is refused: halve the chunk until it
                        // is alone, so the rest of the file still applies.
                        s.chunk_limit = (items.len() / 2).max(1);
                        items.into_iter().rev().for_each(|i| s.queue.push_front(i));
                    } else {
                        for item in items {
                            st.refuse(s, item, &code);
                        }
                    }
                }
            }
            st.pump(s);
            Ok(())
        })
    }

    fn account(&mut self, s: &mut Scheme, item: &Item, refused: Option<&str>) {
        let Some(file) = &item.file else { return };
        if let Some(f) = s.files.get_mut(file) {
            f.remaining = f.remaining.saturating_sub(1);
            match (refused, &item.work) {
                (Some(code), Work::Member { change, .. }) => {
                    f.refused += 1;
                    f.rejected.push(RowReject::new(change.row, Some(&change.employee_id), code, None));
                }
                (Some(_), _) => f.refused += 1,
                (None, _) => f.applied += 1,
            }
        }
        self.finish_file_if_done(s, file);
    }

    /// The work could not be done. Nothing is silently dropped: the refusal
    /// is published and raised for a person to look at.
    fn refuse(&mut self, s: &mut Scheme, item: Item, code: &str) {
        let subject = item.subject();
        self.emit(GroupEvent::RowRefused {
            scheme: s.id.clone(),
            file: item.file.clone(),
            employee_id: subject.clone(),
            change: item.kind().to_string(),
            code: code.to_string(),
        });
        match &item.work {
            Work::Core {
                then: After::ContinuationIssued { offer },
                ..
            } => {
                if let Some(o) = s.offers.get_mut(offer) {
                    o.status = OfferStatus::Open;
                }
                self.emit(GroupEvent::ContinuationRefused {
                    scheme: s.id.clone(),
                    offer: *offer,
                    code: code.to_string(),
                });
            }
            Work::Core {
                then: After::LayerWritten { case },
                ..
            } => {
                if let Some(c) = s.cases.get_mut(case) {
                    c.status = CaseStatus::NotProceeded;
                    let life = c.life.clone();
                    let request_key = format!("grp-uw:{}:{}", s.id, c.request_case());
                    // A refused replacement leaves the cover it was to replace as it was.
                    let suffix = if c.replaces.is_some() { format!("/{case}") } else { String::new() };
                    self.send(Outbound::UnderwritingOutcome {
                        key: format!("{request_key}/excess-rejected{suffix}"),
                        request_key,
                        outcome: IssueOutcome::Rejected { code: code.to_string() },
                    });
                    self.emit(GroupEvent::ExcessCoverDecided {
                        scheme: s.id.clone(),
                        case: *case,
                        life,
                        in_place: false,
                    });
                }
                self.raise(&s.id, ExceptionKind::RowRefused, &subject);
            }
            Work::Core {
                then: After::LayerEnded { case },
                ..
            } => {
                // The layer is not there to end: the case is over either way.
                if let Some(c) = s.cases.get_mut(case) {
                    c.status = CaseStatus::Ended;
                }
            }
            _ => {
                self.raise(&s.id, ExceptionKind::RowRefused, &subject);
            }
        }
        self.account(s, &item, Some(code));
    }

    /// The core accepted the work (or it needed nothing from the core):
    /// bring the module's own view up to date and do what follows.
    fn complete(&mut self, s: &mut Scheme, item: Item) {
        self.account(s, &item, None);
        match item.work {
            Work::Member { change, resolution, life } => self.after_member(s, change, resolution, life),
            Work::Reinstate { employee_id } => {
                s.membership.reinstate(&employee_id);
                for c in s.cases.values_mut().filter(|c| c.employee_id == employee_id && c.ended_by_leave) {
                    c.status = CaseStatus::Accepted;
                    c.ended_by_leave = false;
                }
                // The member has not left after all: there is nothing to continue.
                let withdrawn: Vec<u64> = s
                    .offers
                    .values_mut()
                    .filter(|o| o.employee_id == employee_id && o.status == OfferStatus::Open)
                    .map(|o| {
                        o.status = OfferStatus::Expired;
                        o.id
                    })
                    .collect();
                for offer in withdrawn {
                    self.emit(GroupEvent::ContinuationExpired { scheme: s.id.clone(), offer });
                }
            }
            Work::Cap {
                employee_id,
                cap_minor,
                effective,
                reference,
            } => {
                if let Some(m) = s.membership.members.get_mut(&employee_id) {
                    m.auto_cap_minor = cap_minor;
                }
                self.excess_check(s, &employee_id, effective, reference);
            }
            Work::Pool {
                pool, effective, headcount, ..
            } => {
                s.pools.insert(pool.clone(), effective);
                s.declaration_due = None;
                self.clear(&s.id, ExceptionKind::DeclarationOverdue, None);
                self.emit(GroupEvent::PoolDeclared {
                    scheme: s.id.clone(),
                    pool,
                    effective,
                    headcount,
                });
            }
            Work::Core { then, .. } => self.after_core(s, then),
        }
    }

    fn after_core(&mut self, s: &mut Scheme, then: After) {
        match then {
            After::Linked { employee_id, party, how } => {
                let Some(m) = s.membership.members.get_mut(&employee_id) else { return };
                m.party = Some(party.clone());
                let life = m.life.clone();
                // A request for this member's excess that has not left yet
                // was waiting to know who to ask.
                for out in self.outbox.values_mut() {
                    if let Outbound::Underwriting { request, .. } = out {
                        if request.scheme == s.id && request.life == life {
                            request.party = Some(party.clone());
                        }
                    }
                }
                self.emit(GroupEvent::MemberLinked {
                    scheme: s.id.clone(),
                    life: life.clone(),
                    party,
                    how,
                });
                self.clear_possible(s, &employee_id, &life);
            }
            After::LayerWritten { case } => {
                let Some(c) = s.cases.get_mut(&case) else { return };
                c.status = CaseStatus::Accepted;
                let (life, end_from, layer) = (c.life.clone(), c.end_from, c.layer);
                let replaced = c.replaces.map(|old| (old, c.effective));
                self.emit(GroupEvent::ExcessCoverDecided {
                    scheme: s.id.clone(),
                    case,
                    life,
                    in_place: true,
                });
                // The layer this one takes the place of ends the day this one starts.
                if let Some((old, from)) = replaced {
                    self.end_layer(s, old, from);
                }
                if let (Some(from), Some(_)) = (end_from, layer) {
                    self.end_layer(s, case, from);
                }
            }
            After::LayerEnded { case } => {
                let Some(c) = s.cases.get_mut(&case) else { return };
                c.status = CaseStatus::Ended;
                let life = c.life.clone();
                self.emit(GroupEvent::ExcessCoverEnded {
                    scheme: s.id.clone(),
                    case,
                    life,
                });
            }
            After::ContinuationIssued { offer } => {
                let Some(o) = s.offers.get_mut(&offer) else { return };
                if let OfferStatus::Issuing { policy, group } = o.status.clone() {
                    o.status = OfferStatus::TakenUp { policy: policy.clone() };
                    let life = o.life.clone();
                    let ask = o.continued_excess.as_mut().filter(|c| c.status == ContinuedStatus::Waiting).map(|c| {
                        c.status = ContinuedStatus::Asked;
                        (c.clone(), o.left, o.earlier.clone())
                    });
                    if let Some((c, inception, earlier)) = ask {
                        let member = s.membership.by_life(&life);
                        if let Some(party) = member.and_then(|m| m.party.clone()) {
                            let attrs = member.map(|m| m.attrs.clone()).unwrap_or_default();
                            self.send(Outbound::ContinueExcess(ContinuationExcess {
                                key: format!("grp-cont:{}:{offer}", s.id),
                                policy: policy.clone(),
                                group: group.clone(),
                                party,
                                product_code: c.product_code,
                                cover: c.cover,
                                date_of_birth: attrs.date_of_birth,
                                sex: attrs.sex,
                                smoker: attrs.smoker,
                                amount_minor: c.amount_minor,
                                inception,
                                scheme_policy: s.policy.clone(),
                                scheme_life: life.clone(),
                                earlier,
                            }));
                        }
                    }
                    self.emit(GroupEvent::ContinuationTakenUp {
                        offer,
                        scheme: s.id.clone(),
                        life,
                        new_policy: policy,
                        new_group: group,
                    });
                }
            }
            After::SchemeCancelled { effective } => {
                s.status = SchemeStatus::Terminated { effective };
                self.emit(GroupEvent::SchemeTerminated {
                    scheme: s.id.clone(),
                    effective,
                });
            }
        }
    }

    /// Underwriting need not answer a request nobody wants any more. One
    /// that has not left yet is dropped; one that has is withdrawn there.
    fn withdraw_request(&mut self, scheme: &str, case: u64) {
        let request_key = format!("grp-uw:{scheme}:{case}");
        let waiting = self.outbox.iter().find_map(|(n, out)| match out {
            Outbound::Underwriting { key, .. } if *key == request_key => Some(*n),
            _ => None,
        });
        match waiting {
            Some(n) => {
                self.outbox.remove(&n);
            }
            None => {
                self.send(Outbound::WithdrawUnderwriting {
                    key: format!("{request_key}/withdraw"),
                    request_key,
                });
            }
        }
    }

    fn clear_possible(&mut self, s: &mut Scheme, employee_id: &str, life: &LifeId) {
        let Some(candidates) = s.possible.remove(employee_id) else { return };
        for party in candidates {
            self.notify_reinsurance(ReinsuranceNotice::ClearPossibleMatch {
                policy: s.policy.clone(),
                life: life.clone(),
                party,
            });
        }
        self.clear(&s.id, ExceptionKind::PossibleMatch, Some(employee_id));
        self.emit(GroupEvent::PossibleMatchResolved {
            scheme: s.id.clone(),
            life: life.clone(),
        });
    }

    /// Ask the documents module for a letter to a member about their leave
    /// of absence. A member not yet linked to a person cannot be written to
    /// by the module, so staff are told instead and send it themselves.
    fn member_letter(&mut self, s: &Scheme, kind: &str, life: LifeId, party: Option<PartyId>, since: NaiveDate, cover_ends: NaiveDate) {
        let request = format!("grp:{kind}:{}:{}:{since}", s.id, life.0);
        let Some(party) = party else {
            self.raise(&s.id, ExceptionKind::LetterNotRequested, &request);
            self.emit(GroupEvent::LetterNotRequested {
                scheme: s.id.clone(),
                request,
                code: "no_party".to_string(),
            });
            return;
        };
        let business_date = self.today();
        self.send(Outbound::Letter(MemberLetter {
            request,
            kind: kind.to_string(),
            party,
            policy: s.policy.clone(),
            scheme: s.id.clone(),
            life,
            since,
            cover_ends,
            business_date,
        }));
    }

    fn risk_notice(&mut self, s: &Scheme, life: &LifeId, attrs: &MemberAttrs) {
        self.notify_reinsurance(ReinsuranceNotice::RecordRiskFactors {
            policy: s.policy.clone(),
            life: life.clone(),
            date_of_birth: attrs.date_of_birth,
            sex: attrs.sex,
            smoker: attrs.smoker,
        });
    }

    fn after_member(&mut self, s: &mut Scheme, change: Proposed, resolution: Option<Resolution>, life: Option<LifeId>) {
        let today = self.today();
        match &change.kind {
            ChangeKind::Join => {
                let Some(life) = life else { return };
                let (party, how, candidates) = match resolution {
                    Some(Resolution::Linked { party }) => (Some(party), "matched", vec![]),
                    Some(Resolution::Created { party }) => (Some(party), "created", vec![]),
                    Some(Resolution::Possible { candidates }) => (None, "", candidates),
                    None => (None, "", vec![]),
                };
                s.membership.apply(
                    &change,
                    Some(&JoinAs {
                        life: life.clone(),
                        party: party.clone(),
                    }),
                );
                if let Some(party) = party {
                    self.emit(GroupEvent::MemberLinked {
                        scheme: s.id.clone(),
                        life: life.clone(),
                        party,
                        how: how.to_string(),
                    });
                }
                if !candidates.is_empty() {
                    // Never linked on a guess: the member joins unlinked and
                    // a person decides. Reinsurance counts the possibility.
                    for party in &candidates {
                        self.notify_reinsurance(ReinsuranceNotice::NotePossibleMatch {
                            policy: s.policy.clone(),
                            life: life.clone(),
                            party: party.clone(),
                        });
                    }
                    self.raise(&s.id, ExceptionKind::PossibleMatch, &change.employee_id);
                    self.emit(GroupEvent::PossibleMatchRaised {
                        scheme: s.id.clone(),
                        life: life.clone(),
                        candidates: candidates.len() as u32,
                    });
                    s.possible.insert(change.employee_id.clone(), candidates);
                }
                self.risk_notice(s, &life, &change.attrs);
                self.excess_check(s, &change.employee_id, change.effective, None);
            }
            ChangeKind::Change => {
                s.membership.apply(&change, None);
                let Some(life) = s.membership.active(&change.employee_id).map(|m| m.life.clone()) else {
                    return;
                };
                if change.fields.iter().any(|f| ["date_of_birth", "sex", "smoker"].contains(&f.as_str())) {
                    self.risk_notice(s, &life, &change.attrs);
                }
                self.excess_check(s, &change.employee_id, change.effective, None);
            }
            ChangeKind::Leave => {
                let Some(member) = s.membership.active(&change.employee_id).cloned() else {
                    return;
                };
                s.membership.apply(&change, None);
                s.absence_leaves.remove(&change.employee_id);
                let mut ended = Vec::new();
                let mut unwanted = Vec::new();
                // Underwritten cover in place on leaving, for a scheme that
                // lets it be continued.
                let mut carried = (0_i64, Vec::new());
                for c in s.cases.values_mut().filter(|c| c.employee_id == change.employee_id) {
                    if s.terms.continuation.underwritten_excess && matches!(c.status, CaseStatus::Accepted | CaseStatus::Writing) && c.counts_minor() > 0 {
                        carried.0 += c.counts_minor();
                        carried.1.push(format!("grp-uw:{}:{}", s.id, c.request_case()));
                    }
                    match c.status {
                        CaseStatus::Requested => {
                            c.status = CaseStatus::NotProceeded;
                            unwanted.push(c.id);
                        }
                        CaseStatus::Accepted | CaseStatus::Writing => {
                            // The core ended the layer with the member.
                            c.status = CaseStatus::Ended;
                            c.ended_by_leave = true;
                            ended.push(c.id);
                        }
                        _ => {}
                    }
                }
                for case in unwanted {
                    self.withdraw_request(&s.id, case);
                }
                for case in ended {
                    self.emit(GroupEvent::ExcessCoverEnded {
                        scheme: s.id.clone(),
                        case,
                        life: member.life.clone(),
                    });
                }
                self.clear_possible(s, &change.employee_id, &member.life);
                // A continuation option, for a leaver young enough to hold one.
                let terms = &s.terms.continuation;
                let too_old = terms
                    .max_age
                    .zip(member.attrs.date_of_birth)
                    .is_some_and(|(max, dob)| age_on(dob, change.effective) >= max);
                let automatic_minor: i64 = member_cover(&s.product, &member.attrs, member.auto_cap_minor)
                    .iter()
                    .map(|c| c.automatic_minor)
                    .sum();
                let (underwritten_minor, earlier) = carried;
                let max_sum_insured_minor = automatic_minor + underwritten_minor;
                if terms.products.is_empty() || too_old || max_sum_insured_minor <= 0 {
                    return;
                }
                let id = self.fresh_id();
                let expires = today
                    .max(change.effective)
                    .checked_add_days(Days::new(u64::from(terms.take_up_days)))
                    .unwrap_or(today);
                s.offers.insert(
                    id,
                    Offer {
                        id,
                        employee_id: change.employee_id.clone(),
                        life: member.life.clone(),
                        left: change.effective,
                        expires,
                        max_sum_insured_minor,
                        status: OfferStatus::Open,
                        underwritten_minor,
                        earlier,
                        continued_excess: None,
                    },
                );
                self.emit(GroupEvent::ContinuationOffered {
                    scheme: s.id.clone(),
                    offer: id,
                    life: member.life,
                    expires,
                });
            }
            ChangeKind::AbsenceStart => {
                s.membership.apply(&change, None);
                let limit = Days::new(u64::from(s.terms.leave_of_absence_days));
                let Some(m) = s.membership.active(&change.employee_id) else { return };
                if let (Some(since), Some(ends)) = (m.absence_since, change.effective.checked_add_days(limit)) {
                    let (life, party, cover_ends) = (m.life.clone(), m.party.clone(), s.terms.cover_end_for(ends));
                    self.emit(GroupEvent::AbsenceStarted {
                        scheme: s.id.clone(),
                        life: life.clone(),
                        since,
                        cover_ends,
                    });
                    self.member_letter(s, "absence_started", life, party, since, cover_ends);
                }
            }
            _ => {
                s.membership.apply(&change, None);
            }
        }
    }

    pub(crate) fn today(&self) -> NaiveDate {
        self.business_date().unwrap_or(NaiveDate::MIN)
    }

    /// Bring a member's cover above the automatic amount into line with
    /// what the formula now gives: ask for underwriting of any new excess,
    /// and end cover the formula no longer supports.
    fn excess_check(&mut self, s: &mut Scheme, employee_id: &str, effective: NaiveDate, reference: Option<String>) {
        let Some(member) = s.membership.active(employee_id).cloned() else { return };
        for amounts in member_cover(&s.product, &member.attrs, member.auto_cap_minor) {
            let wanted = amounts.excess_minor();
            let mine = |c: &&ExcessCase| c.employee_id == employee_id && c.cover == amounts.cover && c.life == member.life;
            let mut held: i64 = s.cases.values().filter(mine).map(ExcessCase::counts_minor).sum();
            // Too much: newest first, withdraw requests and end layers until
            // what is held fits under the formula.
            let newest_first: Vec<u64> = s.cases.values().filter(mine).filter(|c| c.counts_minor() > 0).map(|c| c.id).rev().collect();
            for id in newest_first {
                if held <= wanted {
                    break;
                }
                let Some(c) = s.cases.get_mut(&id) else { continue };
                held -= c.counts_minor();
                if c.status == CaseStatus::Requested {
                    c.status = CaseStatus::NotProceeded;
                    self.withdraw_request(&s.id, id);
                    self.emit(GroupEvent::ExcessCoverDecided {
                        scheme: s.id.clone(),
                        case: id,
                        life: member.life.clone(),
                        in_place: false,
                    });
                } else {
                    self.end_layer(s, id, effective);
                }
            }
            if held >= wanted {
                continue;
            }
            let id = self.fresh_id();
            let excess_minor = wanted - held;
            s.cases.insert(
                id,
                ExcessCase {
                    id,
                    employee_id: employee_id.to_string(),
                    life: member.life.clone(),
                    cover: amounts.cover.clone(),
                    excess_minor,
                    accepted_minor: 0,
                    status: CaseStatus::Requested,
                    effective: effective.max(member.joined),
                    layer: None,
                    end_from: None,
                    ended_by_leave: false,
                    reference: reference.clone(),
                    in_employer_total: false,
                    replaces: None,
                },
            );
            let request = UnderwritingRequest {
                scheme: s.id.clone(),
                case: id,
                policy: s.policy.clone(),
                life: member.life.clone(),
                party: member.party.clone(),
                group: s.group.clone(),
                policyholder: s.employer.clone(),
                product_code: s.product.code.clone(),
                product_hash: s.product.content_hash.clone(),
                cover: amounts.cover.clone(),
                cover_type: amounts.cover_type.clone(),
                date_of_birth: member.attrs.date_of_birth,
                sex: member.attrs.sex,
                smoker: member.attrs.smoker,
                excess_minor,
                effective: effective.max(member.joined),
                path: s.terms.excess_path,
                honour_without_evidence_minor: s.terms.takeover.as_ref().and_then(|t| t.honour_excess_up_to_minor),
                reference: reference.clone(),
            };
            self.send(Outbound::Underwriting {
                key: format!("grp-uw:{}:{id}", s.id),
                request,
            });
            self.emit(GroupEvent::ExcessUnderwritingRequested {
                scheme: s.id.clone(),
                case: id,
                life: member.life.clone(),
                cover: amounts.cover,
                excess_minor,
                path: s.terms.excess_path,
                reference: reference.clone(),
            });
        }
    }

    /// End a case's layer from a date: now if the core's layer number is
    /// known, otherwise as soon as it is.
    fn end_layer(&mut self, s: &mut Scheme, case: u64, from: NaiveDate) {
        let Some(c) = s.cases.get_mut(&case) else { return };
        c.end_from = Some(from.max(c.effective));
        let (Some(layer), CaseStatus::Accepted) = (c.layer, c.status) else { return };
        let command = Command::EndExcessLayer {
            policy: s.policy.clone(),
            cover: c.cover.clone(),
            layer,
            effective: from.max(c.effective),
        };
        self.enqueue_core(s, command, After::LayerEnded { case });
    }

    /// Underwriting reissued the terms of cover that is in place, with the
    /// member now paying the loading (staff asked for it and the member
    /// agreed). The core changes who pays only by a new layer, so a layer
    /// with the same terms, the member paying, is written from today, and
    /// the old one ends from the same day once the new one is there. If
    /// the core refuses the new layer the old one stays as it was.
    fn replace_with_member_paying(
        &mut self,
        s: &mut Scheme,
        old: ExcessCase,
        sum_insured_minor: i64,
        loadings: Vec<ic_money::DecStr>,
        exclusions: Vec<String>,
    ) -> Result<(), Reject> {
        let case = old.id;
        if sum_insured_minor <= 0 || sum_insured_minor > old.accepted_minor {
            return Err(Reject::new(
                "invalid_amount",
                "reissued terms are for a positive amount, no more than the cover in place",
            ));
        }
        if loadings.is_empty() {
            return Err(Reject::new("no_loading", "there is no loading for the member to pay"));
        }
        if s.cases.values().any(|c| c.replaces == Some(case) && c.status == CaseStatus::Writing) {
            return Err(Reject::new("case_closed", format!("case {case} is already being replaced")));
        }
        let start = self.today().max(old.effective);
        let id = self.fresh_id();
        let layer = ExcessLayer {
            // The same cover carried on: reinsurance keeps its cession.
            continues: old.layer,
            policy: s.policy.clone(),
            cover: old.cover.clone(),
            life: old.life.clone(),
            start,
            sum_insured_minor,
            loadings,
            exclusions,
            loading_itemised: false,
            member_pays: MemberPays::Loading,
            reference: old.reference.clone().unwrap_or_else(|| format!("grp-case:{case}")),
        };
        s.cases.insert(
            id,
            ExcessCase {
                id,
                accepted_minor: sum_insured_minor,
                status: CaseStatus::Writing,
                effective: start,
                layer: None,
                end_from: None,
                ended_by_leave: false,
                in_employer_total: false,
                replaces: Some(case),
                ..old
            },
        );
        self.enqueue_core(s, Command::AddExcessLayer(Box::new(layer)), After::LayerWritten { case: id });
        self.pump(s);
        Ok(())
    }

    fn record_excess_decision(&mut self, s: &mut Scheme, case: u64, decision: ExcessDecision) -> Result<(), Reject> {
        let itemised_terms = s.terms.excess_loading == ExcessLoading::EmployerItemised;
        let policy = s.policy.clone();
        // Members who would be in the employer's total with this one.
        let mut holders: BTreeSet<&str> = s
            .cases
            .values()
            .filter(|c| matches!(c.status, CaseStatus::Writing | CaseStatus::Accepted))
            .map(|c| c.employee_id.as_str())
            .collect();
        holders.extend(s.cases.get(&case).map(|c| c.employee_id.as_str()));
        let too_few_for_a_total = (holders.len() as u32) < s.terms.excess_loading_min_count;
        let member_pays_loading = match s.terms.excess_loading {
            ExcessLoading::MemberPays => true,
            ExcessLoading::EmployerTotal => too_few_for_a_total,
            ExcessLoading::EmployerItemised => false,
        };
        // Underwriting's second word on cover already in place.
        let in_place = s
            .cases
            .get(&case)
            .filter(|c| c.status == CaseStatus::Accepted && c.layer.is_some() && c.end_from.is_none())
            .cloned();
        let decision = match (in_place, decision) {
            (Some(_), ExcessDecision::ReofferDeclined) => {
                self.emit(GroupEvent::LoadingMoveDeclined { scheme: s.id.clone(), case });
                return Ok(());
            }
            (
                Some(old),
                ExcessDecision::Accepted {
                    sum_insured_minor,
                    loadings,
                    exclusions,
                    member_pays_loading: true,
                    ..
                },
            ) => return self.replace_with_member_paying(s, old, sum_insured_minor, loadings, exclusions),
            (_, decision) => decision,
        };
        let c = s
            .cases
            .get_mut(&case)
            .filter(|c| c.status == CaseStatus::Requested)
            .ok_or_else(|| Reject::new("case_closed", format!("case {case} is not waiting for a decision")))?;
        let life = c.life.clone();
        match decision {
            ExcessDecision::ReofferDeclined => return Err(Reject::new("case_closed", format!("case {case} has no cover in place to reoffer"))),
            ExcessDecision::Accepted {
                sum_insured_minor,
                loadings,
                exclusions,
                member_consents_to_itemising,
                member_pays_loading: member_agreed_to_pay,
            } => {
                let member_pays_loading = member_pays_loading || member_agreed_to_pay;
                if sum_insured_minor <= 0 || sum_insured_minor > c.excess_minor {
                    return Err(Reject::new(
                        "invalid_amount",
                        "the accepted amount must be positive and no more than was asked for",
                    ));
                }
                // The employer may see a loaded premium on a member's line
                // only with that member's recorded consent.
                if itemised_terms && !loadings.is_empty() && !member_consents_to_itemising {
                    return Err(Reject::new(
                        "consent_required",
                        "this scheme itemises loadings, which needs the member's consent",
                    ));
                }
                c.status = CaseStatus::Writing;
                c.accepted_minor = sum_insured_minor;
                let has_loading = !loadings.is_empty();
                c.in_employer_total = has_loading && !member_pays_loading && !itemised_terms;
                let layer = ExcessLayer {
                    continues: None,
                    policy,
                    cover: c.cover.clone(),
                    life: c.life.clone(),
                    start: c.effective,
                    sum_insured_minor,
                    loadings,
                    exclusions,
                    loading_itemised: itemised_terms && member_consents_to_itemising,
                    // Who pays is settled here, when the terms are accepted,
                    // because it is what the member was told in the offer.
                    // The standard rate stays on the scheme invoice.
                    member_pays: if member_pays_loading && has_loading {
                        MemberPays::Loading
                    } else {
                        MemberPays::Nothing
                    },
                    reference: c.reference.clone().unwrap_or_else(|| format!("grp-case:{case}")),
                };
                self.enqueue_core(s, Command::AddExcessLayer(Box::new(layer)), After::LayerWritten { case });
                self.pump(s);
            }
            ExcessDecision::Declined | ExcessDecision::NotProceeded => {
                c.status = if decision == ExcessDecision::Declined {
                    CaseStatus::Declined
                } else {
                    CaseStatus::NotProceeded
                };
                self.emit(GroupEvent::ExcessCoverDecided {
                    scheme: s.id.clone(),
                    case,
                    life,
                    in_place: false,
                });
            }
        }
        Ok(())
    }

    fn start_termination(&mut self, s: &mut Scheme, today: NaiveDate, effective: NaiveDate, reason: &str) {
        s.status = SchemeStatus::Terminating { effective };
        self.emit(GroupEvent::TerminationStarted {
            scheme: s.id.clone(),
            effective,
            reason: reason.to_string(),
        });
        // Every member leaves on the day, so that each has a leaving event
        // and a continuation option; then the policy itself ends.
        let mut work: Vec<Work> = s
            .membership
            .active_members()
            .map(|m| Work::Member {
                change: leave(&m.employee_id, &m.attrs, effective),
                resolution: None,
                life: None,
            })
            .collect();
        work.push(Work::Core {
            group: s.group.clone(),
            command: Box::new(Command::CancelPolicy { policy: s.policy.clone() }),
            then: After::SchemeCancelled { effective },
        });
        for w in work {
            if effective > today {
                s.next_item += 1;
                s.scheduled.insert(
                    (effective, s.next_item),
                    Item {
                        id: s.next_item,
                        file: None,
                        work: w,
                    },
                );
            } else {
                self.enqueue(s, None, w);
            }
        }
        self.pump(s);
    }

    fn tick(&mut self, today: NaiveDate) {
        self.prune(today);
        let ids: Vec<String> = self.schemes.keys().cloned().collect();
        for id in ids {
            // The scheme is there: the ID came from the map a moment ago.
            let _ = self.with_scheme(&id, |st, s| {
                st.tick_scheme(s, today);
                Ok(())
            });
        }
    }

    fn tick_scheme(&mut self, s: &mut Scheme, today: NaiveDate) {
        if matches!(s.status, SchemeStatus::Terminated { .. }) {
            return;
        }
        // Changes dated ahead are released on their day, in order.
        while let Some(entry) = s.scheduled.first_entry() {
            if entry.key().0 > today {
                break;
            }
            let item = entry.remove();
            if let Some(f) = item.file.as_ref().and_then(|f| s.files.get_mut(f)) {
                // Counted as applied or refused when it is, like any other.
                f.status = FileStatus::Applying;
                f.remaining += 1;
                f.scheduled = f.scheduled.saturating_sub(1);
            }
            s.queue.push_back(item);
        }
        s.waiting = false;

        // A leave of absence that has run past what the terms allow.
        let limit = Days::new(u64::from(s.terms.leave_of_absence_days));
        type Overdue = (String, MemberAttrs, LifeId, Option<PartyId>, NaiveDate, NaiveDate);
        let overdue: Vec<Overdue> = s
            .membership
            .active_members()
            .filter(|m| !s.absence_leaves.contains(&m.employee_id))
            .filter_map(|m| {
                let since = m.absence_since?;
                let ends = since.checked_add_days(limit)?;
                (ends <= today).then(|| {
                    (
                        m.employee_id.clone(),
                        m.attrs.clone(),
                        m.life.clone(),
                        m.party.clone(),
                        since,
                        s.terms.cover_end_for(ends),
                    )
                })
            })
            .collect();
        for (employee_id, attrs, life, party, since, cover_ends) in overdue {
            s.absence_leaves.insert(employee_id.clone());
            self.emit(GroupEvent::AbsenceLimitReached {
                scheme: s.id.clone(),
                life: life.clone(),
                cover_ends,
            });
            self.member_letter(s, "absence_limit_reached", life, party, since, cover_ends);
            let work = Work::Member {
                change: leave(&employee_id, &attrs, cover_ends),
                resolution: None,
                life: None,
            };
            if cover_ends > today {
                s.next_item += 1;
                s.scheduled.insert(
                    (cover_ends, s.next_item),
                    Item {
                        id: s.next_item,
                        file: None,
                        work,
                    },
                );
            } else {
                self.enqueue(s, None, work);
            }
        }

        let expired: Vec<u64> = s
            .offers
            .values_mut()
            .filter(|o| o.status == OfferStatus::Open && o.expires < today)
            .map(|o| {
                o.status = OfferStatus::Expired;
                o.id
            })
            .collect();
        for offer in expired {
            self.emit(GroupEvent::ContinuationExpired { scheme: s.id.clone(), offer });
        }

        // A file nobody has finished with for too long is a break.
        let max_age = Days::new(u64::from(s.terms.file_max_age_days));
        let stale: Vec<String> = s
            .files
            .values_mut()
            .filter(|f| !f.status.is_closed() && !f.stale_flagged)
            .filter(|f| f.received.checked_add_days(max_age).is_some_and(|limit| limit < today))
            .map(|f| {
                f.stale_flagged = true;
                f.id.clone()
            })
            .collect();
        for file in stale {
            self.raise(&s.id, ExceptionKind::FileStale, &file);
        }

        if s.declaration_due.is_some_and(|due| due < today) {
            self.raise(&s.id, ExceptionKind::DeclarationOverdue, "pool");
        }

        if s.status == SchemeStatus::Active {
            self.tick_renewal(s, today);
        }
        self.pump(s);
    }

    fn tick_renewal(&mut self, s: &mut Scheme, today: NaiveDate) {
        let renewal_date = s.terms.renewal_date;
        let opens = renewal_date
            .checked_sub_days(Days::new(u64::from(s.terms.renewal_lead_days)))
            .unwrap_or(renewal_date);
        if s.renewal.is_none() && today >= opens {
            s.renewal = Some(Renewal {
                opened: today,
                agreed: None,
                running_on_old_rates: false,
            });
            self.emit(GroupEvent::RenewalOpened {
                scheme: s.id.clone(),
                renewal_date,
                members: s.membership.active_count() as u32,
            });
        }
        if today < renewal_date {
            return;
        }
        let agreed = s.renewal.as_mut().and_then(|r| r.agreed.take());
        let guaranteed = s.terms.rate_guarantee_until.is_some_and(|g| g > renewal_date);
        if agreed.is_some() || guaranteed {
            // Agreed terms, or rates still under guarantee: the year rolls.
            s.terms = agreed.map_or_else(|| rolled(&s.terms), |(terms, _)| terms);
            s.terms_version += 1;
            s.renewal = None;
            if s.terms.basis == MemberBasis::Pool {
                s.declaration_due = s.terms.scheme_year_start.checked_add_days(Days::new(u64::from(s.terms.declaration_due_days)));
            }
            self.clear(&s.id, ExceptionKind::RenewalNotAgreed, None);
            self.emit(GroupEvent::SchemeRenewed {
                scheme: s.id.clone(),
                scheme_year_start: s.terms.scheme_year_start,
                terms_version: s.terms_version,
            });
            self.notify_reinsurance(ReinsuranceNotice::SetSchemeTerms {
                policy: s.policy.clone(),
                excess_path: s.terms.excess_path,
                sites: s.terms.sites.clone(),
            });
            return;
        }
        match s.terms.no_agreement {
            NoAgreement::End => self.start_termination(s, today, today, "renewal_not_agreed"),
            NoAgreement::ContinueOnOldRates { days } => {
                let until = renewal_date.checked_add_days(Days::new(u64::from(days))).unwrap_or(renewal_date);
                if today >= until {
                    self.start_termination(s, today, today, "renewal_not_agreed");
                    return;
                }
                let first = s.renewal.as_mut().is_some_and(|r| !std::mem::replace(&mut r.running_on_old_rates, true));
                if first {
                    self.raise(&s.id, ExceptionKind::RenewalNotAgreed, "renewal");
                    self.emit(GroupEvent::RenewalRunningOnOldRates { scheme: s.id.clone(), until });
                }
            }
        }
    }

    /// What the core's stream says that the module keeps a view of: the
    /// invoice as it is built, adjustments, the balance, and the number the
    /// core gave an underwritten layer.
    fn observe(&mut self, event: CoreEvent) -> Result<(), Reject> {
        let policy = match &event {
            CoreEvent::SchemeInvoiceChunk { policy, .. }
            | CoreEvent::PremiumAdjusted { policy, .. }
            | CoreEvent::LoadingAdjusted { policy, .. }
            | CoreEvent::PolicyBalance { policy, .. }
            | CoreEvent::LayerAdded { policy, .. }
            | CoreEvent::MemberCoverLapsed { policy, .. } => policy,
            _ => return Ok(()),
        };
        let Some(scheme) = self.by_policy.get(policy).cloned() else { return Ok(()) };
        self.with_scheme(&scheme, |st, s| {
            let group_of = |s: &Scheme, life: &LifeId| {
                s.membership
                    .by_life(life)
                    .and_then(|m| m.attrs.billing_group.clone())
                    .filter(|g| s.terms.has_billing_group(g))
                    .unwrap_or_else(|| s.terms.default_billing_group().to_string())
            };
            match event {
                CoreEvent::SchemeInvoiceChunk {
                    period_index,
                    lines,
                    complete,
                    loading_minor,
                    ..
                } => {
                    let mut invoice = s.building.take().filter(|i| i.period_index == period_index).unwrap_or_else(|| Invoice {
                        period_index,
                        terms_version: s.terms_version,
                        // The balance as the core last gave it, before any of
                        // this invoice. Adjustments already in that balance
                        // are taken out again below, where they are itemised.
                        brought_forward_minor: s.outstanding_minor,
                        ..Invoice::default()
                    });
                    for (life, total_minor) in lines {
                        let line: &mut GroupLine = invoice.by_group.entry(group_of(s, &life)).or_default();
                        line.members += 1;
                        line.total_minor += total_minor;
                        let attrs = s.membership.by_life(&life).map(|m| &m.attrs);
                        let category = attrs.and_then(|a| a.category.clone()).unwrap_or_default();
                        let occupation_class = attrs.and_then(|a| a.occupation_class.clone());
                        let at = invoice
                            .by_class
                            .binary_search_by(|l| (&l.category, &l.occupation_class).cmp(&(&category, &occupation_class)));
                        let i = at.unwrap_or_else(|i| {
                            invoice.by_class.insert(
                                i,
                                ClassLine {
                                    category,
                                    occupation_class,
                                    ..ClassLine::default()
                                },
                            );
                            i
                        });
                        invoice.by_class[i].members += 1;
                        invoice.by_class[i].total_minor += total_minor;
                    }
                    invoice.loading_minor += loading_minor;
                    if !complete {
                        s.building = Some(invoice);
                        return Ok(());
                    }
                    invoice.adjustments_by_group = std::mem::take(&mut s.pending_adjustments);
                    invoice.loading_adjustments_minor = std::mem::take(&mut s.pending_loading_adjustments_minor);
                    invoice.brought_forward_minor -= invoice.adjustments_by_group.values().sum::<i64>() + invoice.loading_adjustments_minor;
                    st.emit(GroupEvent::InvoiceIssued {
                        scheme: s.id.clone(),
                        period_index,
                        total_minor: invoice.total_minor(),
                        members: invoice.members(),
                    });
                    s.invoices.push_back(invoice);
                    if s.invoices.len() > INVOICES_KEPT {
                        s.invoices.pop_front();
                    }
                }
                CoreEvent::PremiumAdjusted { life, total_minor, .. } => {
                    *s.pending_adjustments.entry(group_of(s, &life)).or_default() += total_minor;
                }
                CoreEvent::LoadingAdjusted { total_minor, .. } => s.pending_loading_adjustments_minor += total_minor,
                CoreEvent::PolicyBalance {
                    outstanding_minor,
                    oldest_unpaid_due,
                    ..
                } => {
                    s.outstanding_minor = outstanding_minor;
                    s.oldest_unpaid_due = oldest_unpaid_due;
                    if outstanding_minor <= 0 {
                        s.short_payments = 0;
                        if std::mem::take(&mut s.joiners_suspended) {
                            st.clear(&s.id, ExceptionKind::PersistentShortPayment, None);
                            st.emit(GroupEvent::JoinersResumed { scheme: s.id.clone() });
                        }
                    }
                }
                // The member stopped paying for cover that was theirs to pay
                // for, and the core ended it. The case is over; the employer
                // learns only that the cover is no longer in place.
                CoreEvent::MemberCoverLapsed { life, layers, .. } => {
                    let ended: Vec<u64> = s
                        .cases
                        .values_mut()
                        .filter(|c| c.life == life && c.status == CaseStatus::Accepted)
                        .filter(|c| c.layer.is_some_and(|n| layers.iter().any(|(cover, layer)| *cover == c.cover && *layer == n)))
                        .map(|c| {
                            c.status = CaseStatus::Ended;
                            c.id
                        })
                        .collect();
                    for case in ended {
                        st.emit(GroupEvent::ExcessCoverEnded {
                            scheme: s.id.clone(),
                            case,
                            life: life.clone(),
                        });
                    }
                }
                CoreEvent::LayerAdded {
                    layer,
                    origin: LayerOrigin::UnderwrittenExcess,
                    ..
                } => {
                    let waiting = s
                        .cases
                        .values_mut()
                        .filter(|c| matches!(c.status, CaseStatus::Writing | CaseStatus::Accepted))
                        .find(|c| c.life == layer.life && c.cover == layer.cover && c.layer.is_none());
                    if let Some(c) = waiting {
                        c.layer = Some(layer.layer);
                        let request_key = format!("grp-uw:{}:{}", s.id, c.request_case());
                        let suffix = if c.replaces.is_some() { format!("/{}", c.id) } else { String::new() };
                        st.send(Outbound::UnderwritingOutcome {
                            key: format!("{request_key}/excess-issued{suffix}"),
                            request_key,
                            outcome: IssueOutcome::Issued {
                                life: layer.life.clone(),
                                cover: layer.cover.clone(),
                                layer: layer.layer,
                            },
                        });
                        if let (Some(from), CaseStatus::Accepted, id) = (c.end_from, c.status, c.id) {
                            st.end_layer(s, id, from);
                            st.pump(s);
                        }
                    }
                }
                _ => {}
            }
            Ok(())
        })
    }

    pub(crate) fn dispatch_work(&mut self, _actor: &str, today: NaiveDate, command: GroupCommand) -> Result<(), Reject> {
        use GroupCommand::*;
        match command {
            Reinstate { scheme, employee_ids, reason } => self.with_scheme(&scheme, |st, s| {
                if employee_ids.is_empty() {
                    return Err(Reject::new("nothing_to_do", "no members were named"));
                }
                for id in &employee_ids {
                    if !s.membership.members.get(id.as_str()).is_some_and(|m| m.left.is_some()) {
                        return Err(Reject::new("not_left", format!("member {id} has not left the scheme")));
                    }
                }
                st.emit(GroupEvent::MembersReinstated {
                    scheme: s.id.clone(),
                    count: employee_ids.len() as u32,
                    reason,
                });
                for employee_id in employee_ids {
                    st.enqueue(s, None, Work::Reinstate { employee_id });
                }
                st.pump(s);
                Ok(())
            }),
            RequestExcessAgain { scheme, employee_id, reason } => self.with_scheme(&scheme, |st, s| {
                if reason.trim().is_empty() {
                    return Err(Reject::new("missing_value", "say why underwriting is asked again"));
                }
                let member = s
                    .membership
                    .active(&employee_id)
                    .cloned()
                    .ok_or_else(|| Reject::new("unknown_member", format!("{employee_id} is not a member")))?;
                let open = |c: &ExcessCase| c.employee_id == employee_id && matches!(c.status, CaseStatus::Requested | CaseStatus::Writing);
                if s.cases.values().any(open) {
                    return Err(Reject::new("case_open", format!("underwriting is already deciding for {employee_id}")));
                }
                let before = s.cases.len();
                // Cover asked again starts from today, not from when the member joined.
                st.excess_check(s, &employee_id, today.max(member.joined), None);
                if s.cases.len() == before {
                    return Err(Reject::new(
                        "nothing_to_ask",
                        format!("{employee_id} holds all the cover the formula gives, or none above the automatic amount"),
                    ));
                }
                Ok(())
            }),
            ResolveMatch { scheme, employee_id, decision } => self.with_scheme(&scheme, |st, s| {
                let candidates = s
                    .possible
                    .get(&employee_id)
                    .ok_or_else(|| Reject::new("no_possible_match", format!("member {employee_id} has no possible match to decide")))?;
                let (party, how) = match decision {
                    MatchDecision::SamePerson { party } if candidates.contains(&party) => (party, "resolved_same_person"),
                    MatchDecision::DifferentPerson { party } if !candidates.contains(&party) => (party, "resolved_different_person"),
                    _ => return Err(Reject::new("not_a_candidate", "the decision does not fit the candidates")),
                };
                st.link(s, &employee_id, party, how)
            }),
            SelfRegister { scheme, employee_id, party } => self.with_scheme(&scheme, |st, s| st.link(s, &employee_id, party, "self_registered")),
            RecordExcessDecision { scheme, case, decision } => self.with_scheme(&scheme, |st, s| st.record_excess_decision(s, case, decision)),
            RequireUnderwrittenExcess {
                policy,
                life,
                cover,
                excess_minor,
                reference,
            } => {
                let scheme = self
                    .by_policy
                    .get(&policy)
                    .cloned()
                    .ok_or_else(|| Reject::new("unknown_policy", format!("policy {policy} is not a registered scheme")))?;
                self.with_scheme(&scheme, |st, s| {
                    let m = s
                        .membership
                        .by_life(&life)
                        .filter(|m| m.left.is_none())
                        .ok_or_else(|| Reject::new("unknown_member", format!("member {life} is not an active member")))?;
                    let automatic = member_cover(&s.product, &m.attrs, m.auto_cap_minor)
                        .into_iter()
                        .find(|c| c.cover == cover)
                        .map(|c| c.automatic_minor)
                        .ok_or_else(|| Reject::new("unknown_cover", format!("member {life} has no cover {cover}")))?;
                    if excess_minor <= 0 || excess_minor > automatic {
                        return Err(Reject::new(
                            "invalid_amount",
                            "the excess must be positive and within the member's automatic cover",
                        ));
                    }
                    let work = Work::Cap {
                        employee_id: m.employee_id.clone(),
                        cap_minor: Some(automatic - excess_minor),
                        effective: today,
                        reference: Some(reference),
                    };
                    st.enqueue(s, None, work);
                    st.pump(s);
                    Ok(())
                })
            }
            DeclarePool {
                scheme,
                pool,
                category,
                effective,
                headcount,
                total_salary_minor,
            } => self.with_scheme(&scheme, |st, s| {
                if s.terms.basis != MemberBasis::Pool {
                    return Err(Reject::new("not_a_pool_scheme", "only a pool scheme declares aggregates"));
                }
                if s.status != SchemeStatus::Active {
                    return Err(Reject::new("scheme_ending", "the scheme is ending"));
                }
                if pool.is_empty() || !known_category(&s.product, &category) {
                    return Err(Reject::new("unknown_value", "the pool needs a name and a category the scheme has"));
                }
                if headcount == 0 || total_salary_minor <= 0 {
                    return Err(Reject::new("invalid_amount", "a declaration needs a headcount and a total salary"));
                }
                let earliest = today.checked_sub_days(Days::new(u64::from(s.terms.backdating_limit_days))).unwrap_or(today);
                if effective > today || effective < s.inception.max(earliest) {
                    return Err(Reject::new("backdating_limit", "the declaration's date is outside what the scheme allows"));
                }
                let work = Work::Pool {
                    pool,
                    category,
                    effective,
                    headcount,
                    total_salary_minor,
                };
                st.enqueue(s, None, work);
                st.pump(s);
                Ok(())
            }),
            TakeUpContinuation {
                scheme,
                offer,
                policy,
                number,
                group,
                product_code,
                cover,
                frequency,
                sum_insured_minor,
            } => self.with_scheme(&scheme, |st, s| {
                let o = s
                    .offers
                    .get(&offer)
                    .filter(|o| o.status == OfferStatus::Open)
                    .ok_or_else(|| Reject::new("offer_closed", format!("offer {offer} is not open")))?;
                if today > o.expires {
                    return Err(Reject::new("offer_expired", format!("offer {offer} expired on {}", o.expires)));
                }
                if !s.terms.continuation.products.contains(&product_code) {
                    return Err(Reject::new(
                        "product_not_offered",
                        format!("{product_code} is not a continuation product of this scheme"),
                    ));
                }
                if sum_insured_minor <= 0 || sum_insured_minor > o.max_sum_insured_minor {
                    return Err(Reject::new(
                        "invalid_amount",
                        "the amount must be positive and no more than the cover held on leaving",
                    ));
                }
                // The policy is issued for the automatic part; anything above
                // it is underwriting's to add on the terms it accepted.
                let automatic_minor = o.automatic_minor();
                if automatic_minor <= 0 {
                    return Err(Reject::new("no_automatic_cover", "the leaver had no automatic cover to continue"));
                }
                let excess_minor = sum_insured_minor - sum_insured_minor.min(automatic_minor);
                let continued = (excess_minor > 0).then(|| ContinuedExcess {
                    amount_minor: excess_minor,
                    cover: cover.clone(),
                    product_code: product_code.clone(),
                    status: ContinuedStatus::Waiting,
                });
                let sum_insured_minor = sum_insured_minor.min(automatic_minor);
                let member = s.membership.by_life(&o.life);
                let (party, attrs) = member
                    .and_then(|m| Some((m.party.clone()?, m.attrs.clone())))
                    .ok_or_else(|| Reject::new("not_identified", "the member must be identified before a policy can be issued to them"))?;
                // No evidence of health and no gap: the policy starts the day
                // scheme cover ended, on the product's standard terms.
                let life = LifeId("L1".to_string());
                let issue = IssuePolicy {
                    // Claims reads the scheme's history through this link.
                    continuation_of: Some(ic_core::command::ContinuationOf {
                        policy: s.policy.clone(),
                        life: o.life.clone(),
                    }),
                    policy: policy.clone(),
                    number,
                    group: group.clone(),
                    policyholder: party.clone(),
                    product_code,
                    inception: o.left,
                    frequency,
                    lives: vec![LifeSpec {
                        id: life.clone(),
                        kind: LifeKind::Named { party },
                        factors: LifeFactors {
                            date_of_birth: attrs.date_of_birth,
                            sex: attrs.sex,
                            smoker: attrs.smoker,
                            ..LifeFactors::default()
                        },
                    }],
                    covers: vec![CoverSpec {
                        excess_option: None,
                        code: cover,
                        life: Some(life),
                        sum_insured_minor,
                        loadings: vec![],
                        exclusions: vec![],
                    }],
                    migrated: false,
                };
                if let Some(o) = s.offers.get_mut(&offer) {
                    o.status = OfferStatus::Issuing { policy, group: group.clone() };
                    o.continued_excess = continued;
                }
                let work = Work::Core {
                    group,
                    command: Box::new(Command::IssuePolicy(Box::new(issue))),
                    then: After::ContinuationIssued { offer },
                };
                st.enqueue(s, None, work);
                st.pump(s);
                Ok(())
            }),
            AgreeRenewal { scheme, terms, rate_table } => self.with_scheme(&scheme, |st, s| {
                let renewal_date = s.terms.renewal_date;
                if s.terms.rate_guarantee_until.is_some_and(|g| g > renewal_date) {
                    return Err(Reject::new("rate_guarantee", "rates are guaranteed past this renewal"));
                }
                terms.validate().map_err(|e| Reject::new("invalid_terms", e))?;
                if terms.scheme_year_start != renewal_date {
                    return Err(Reject::new("renewal_dates", "the new scheme year must start on the renewal date"));
                }
                let renewal = s
                    .renewal
                    .as_mut()
                    .filter(|_| s.status == SchemeStatus::Active)
                    .ok_or_else(|| Reject::new("renewal_not_open", "the scheme has no renewal open"))?;
                // A pool is priced on what it declares: its renewal waits for
                // an overdue declaration.
                if st.exceptions.values().any(|e| e.scheme == s.id && e.kind == ExceptionKind::DeclarationOverdue) {
                    return Err(Reject::new("declaration_overdue", "the scheme's declaration is overdue; renewal waits for it"));
                }
                renewal.agreed = Some((*terms, rate_table.clone()));
                st.clear(&s.id, ExceptionKind::RenewalNotAgreed, None);
                st.emit(GroupEvent::RenewalAgreed {
                    scheme: s.id.clone(),
                    rate_table,
                    terms_version: s.terms_version + 1,
                });
                Ok(())
            }),
            Terminate { scheme, effective, reason } => self.with_scheme(&scheme, |st, s| {
                if s.status != SchemeStatus::Active {
                    return Err(Reject::new("scheme_ending", "the scheme is already ending"));
                }
                if effective < today {
                    return Err(Reject::new("past_date", "a scheme is ended from today or a later date"));
                }
                st.start_termination(s, today, effective, &reason);
                Ok(())
            }),
            RecordShortPayment { scheme, shortfall_minor } => self.with_scheme(&scheme, |st, s| {
                if shortfall_minor <= 0 {
                    return Err(Reject::new("invalid_amount", "a shortfall is a positive amount"));
                }
                s.short_payments += 1;
                st.emit(GroupEvent::ShortPaymentRecorded {
                    scheme: s.id.clone(),
                    in_a_row: s.short_payments,
                    shortfall_minor,
                });
                let limit = s.terms.suspend_joiners_after_short_payments;
                if !s.joiners_suspended && limit.is_some_and(|n| s.short_payments >= n) {
                    s.joiners_suspended = true;
                    st.raise(&s.id, ExceptionKind::PersistentShortPayment, "short_payment");
                    st.emit(GroupEvent::JoinersSuspended { scheme: s.id.clone() });
                }
                Ok(())
            }),
            CoreResult { key, error } => self.core_result(&key, error),
            CommissionRefused { key, code } => {
                let found = self
                    .outbox
                    .iter()
                    .find_map(|(id, o)| matches!(o, Outbound::Commission { key: k, .. } if *k == key).then_some(*id));
                // Already settled: nothing more to do.
                let Some(id) = found else { return Ok(()) };
                let Some(Outbound::Commission { layer, .. }) = self.outbox.remove(&id) else {
                    return Ok(());
                };
                if let Some(scheme) = self.by_policy.get(&layer.policy).cloned() {
                    self.raise(&scheme, ExceptionKind::BrokerNotRecorded, &layer.cover);
                    self.emit(GroupEvent::BrokerNotRecorded {
                        scheme,
                        cover: layer.cover,
                        code,
                    });
                }
                Ok(())
            }
            RecordContinuedExcess { scheme, offer, outcome } => self.with_scheme(&scheme, |st, s| {
                let c = s
                    .offers
                    .get_mut(&offer)
                    .and_then(|o| o.continued_excess.as_mut())
                    .ok_or_else(|| Reject::new("unknown_offer", format!("offer {offer} continued no underwritten cover")))?;
                if c.status != ContinuedStatus::Asked {
                    // Recorded already: the same answer read again.
                    return Ok(());
                }
                c.status = if outcome == "issued" {
                    ContinuedStatus::InPlace
                } else {
                    ContinuedStatus::NotContinued { outcome: outcome.clone() }
                };
                st.emit(GroupEvent::ContinuedExcessDecided {
                    scheme: s.id.clone(),
                    offer,
                    outcome,
                });
                Ok(())
            }),
            LetterRefused { request, code } => {
                let found = self
                    .outbox
                    .iter()
                    .find_map(|(id, o)| matches!(o, Outbound::Letter(l) if l.request == request).then_some(*id));
                // Already settled: nothing more to do.
                let Some(id) = found else { return Ok(()) };
                let Some(Outbound::Letter(letter)) = self.outbox.remove(&id) else {
                    return Ok(());
                };
                self.raise(&letter.scheme, ExceptionKind::LetterNotRequested, &request);
                self.emit(GroupEvent::LetterNotRequested {
                    scheme: letter.scheme,
                    request,
                    code,
                });
                Ok(())
            }
            ObserveCore { event } => self.observe(*event),
            Ack { ids } => {
                for id in ids {
                    // A command for the core leaves the outbox only when the
                    // core's answer has been recorded.
                    if !matches!(self.outbox.get(&id), Some(Outbound::Core { .. })) {
                        self.outbox.remove(&id);
                    }
                }
                Ok(())
            }
            Tick => {
                self.tick(today);
                Ok(())
            }
            // Everything else is handled before this is reached.
            other => Err(Reject::new("unhandled", format!("{} was not handled", other.name()))),
        }
    }

    /// Link a member to a person, through the core. Only ever on a decision
    /// by a person or the member's own verified registration.
    fn link(&mut self, s: &mut Scheme, employee_id: &str, party: PartyId, how: &str) -> Result<(), Reject> {
        let m = s
            .membership
            .active(employee_id)
            .ok_or_else(|| Reject::new("unknown_member", format!("member {employee_id} is not an active member")))?;
        match &m.party {
            Some(p) if *p == party => return Ok(()),
            Some(_) => return Err(Reject::new("already_identified", "the member is already linked to a different person")),
            None => {}
        }
        let command = Command::IdentifyMember {
            policy: s.policy.clone(),
            life: m.life.clone(),
            party: party.clone(),
        };
        let then = After::Linked {
            employee_id: employee_id.to_string(),
            party,
            how: how.to_string(),
        };
        self.enqueue_core(s, command, then);
        self.pump(s);
        Ok(())
    }
}

/// A scheme's policy in the core, for a continuation issued elsewhere.
pub fn scheme_of<'a>(state: &'a GroupState, policy: &PolicyId) -> Option<&'a Scheme> {
    state.by_policy.get(policy).and_then(|id| state.schemes.get(id))
}
