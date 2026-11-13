//! Splitting a scheme: part of an employer is sold or demerged, and the
//! members who go with it move to a new scheme from the first day of a
//! billing period. They keep their life records, the date they joined and
//! their underwritten cover; they do not leave, so nobody is offered a
//! continuation and nothing is underwritten again.
//!
//! The core moves the members in parts of at most one membership chunk,
//! each the same split (same new policy and date) until every part is in.
//! The new scheme is opened here when the first part is accepted, with its
//! own terms and employer and the old scheme's file layouts. No files are
//! taken on the old scheme until the last part is in.

use std::collections::BTreeSet;

use chrono::NaiveDate;
use ic_core::machine::MAX_MEMBERSHIP_CHUNK;
use ic_core::{Command, PartyId, PolicyId, Reject};

use crate::event::GroupEvent;
use crate::state::{broker_layers, CaseStatus, GroupState, Scheme, SchemeStatus, Splitting};
use crate::terms::{MemberBasis, SchemeTerms};
use crate::work::{After, Work};

/// What staff asked for, as the command carried it.
pub(crate) struct SplitAsk {
    pub into: String,
    pub new_policy: PolicyId,
    pub new_number: String,
    pub employer: PartyId,
    pub effective: NaiveDate,
    pub employee_ids: Vec<String>,
    pub terms: SchemeTerms,
}

impl GroupState {
    /// Check a split against the scheme as it stands and send its parts.
    pub(crate) fn start_split(&mut self, s: &mut Scheme, ask: SplitAsk) -> Result<(), Reject> {
        if s.status != SchemeStatus::Active || s.blocked.is_some() || s.frozen.is_some() {
            return Err(Reject::new(
                "scheme_not_ready",
                "a scheme is split only while it is active, not blocked and not frozen",
            ));
        }
        if s.splitting.is_some() || s.open_file.is_some() || s.in_flight.is_some() || !s.queue.is_empty() {
            return Err(Reject::new(
                "scheme_busy",
                "a scheme is split when no file, change or other split is being worked through",
            ));
        }
        if s.terms.basis == MemberBasis::Pool || ask.terms.basis == MemberBasis::Pool {
            return Err(Reject::new("pool_scheme", "a pool scheme declares aggregates and has no members to move"));
        }
        if ask.into.is_empty() || ask.into.contains(':') {
            return Err(Reject::new("invalid_scheme", "a scheme ID is not empty and has no colon"));
        }
        if self.schemes.contains_key(&ask.into) || ask.into == s.id || self.by_policy.contains_key(&ask.new_policy) || ask.new_policy == s.policy {
            return Err(Reject::new("scheme_exists", "the new scheme or its policy is already registered"));
        }
        ask.terms.validate().map_err(|e| Reject::new("invalid_terms", e))?;
        if let Some(menu) = &ask.terms.health_menu {
            menu.check_product(&s.product).map_err(|e| Reject::new("invalid_terms", e))?;
        }
        if let Some(broker) = &ask.terms.broker {
            broker_layers(&ask.into, &ask.new_policy, &s.product, broker)?;
        }
        let moving: BTreeSet<&str> = ask.employee_ids.iter().map(String::as_str).collect();
        if moving.is_empty() || moving.len() != ask.employee_ids.len() {
            return Err(Reject::new("invalid_split", "name each member who moves once"));
        }
        for id in &moving {
            if s.membership.active(id).is_none() {
                return Err(Reject::new("unknown_member", format!("{id} is not a member")));
            }
        }
        if s.membership.active_count() == moving.len() {
            return Err(Reject::new("not_a_split", "a split leaves members behind; to move a whole scheme, transfer it"));
        }
        // Underwriting deciding on cover a member is moving with would answer
        // to the wrong scheme.
        if let Some(c) = s
            .cases
            .values()
            .find(|c| moving.contains(c.employee_id.as_str()) && matches!(c.status, CaseStatus::Requested | CaseStatus::Writing))
        {
            return Err(Reject::new(
                "excess_open",
                format!("underwriting is deciding cover for {}; split when it has decided", c.employee_id),
            ));
        }

        let members = ask.employee_ids.len() as u32;
        self.emit(GroupEvent::SplitStarted {
            scheme: s.id.clone(),
            into: ask.into.clone(),
            new_policy: ask.new_policy.clone(),
            effective: ask.effective,
            members,
        });
        let parts: Vec<Vec<String>> = ask.employee_ids.chunks(MAX_MEMBERSHIP_CHUNK).map(<[String]>::to_vec).collect();
        let count = parts.len();
        for (n, employee_ids) in parts.into_iter().enumerate() {
            let lives = employee_ids.iter().filter_map(|id| s.membership.active(id)).map(|m| m.life.clone()).collect();
            let command = Command::SplitScheme(Box::new(ic_core::SplitScheme {
                policy: s.policy.clone(),
                new_policy: ask.new_policy.clone(),
                new_number: ask.new_number.clone(),
                lives,
                effective: ask.effective,
            }));
            let then = After::SplitPart {
                into: ask.into.clone(),
                employee_ids,
                last: n + 1 == count,
            };
            self.enqueue(
                s,
                None,
                Work::Core {
                    group: s.group.clone(),
                    command: Box::new(command),
                    then,
                },
            );
        }
        s.splitting = Some(Splitting {
            into: ask.into,
            new_policy: ask.new_policy,
            employer: ask.employer,
            effective: ask.effective,
            terms: ask.terms,
            members,
            moved: 0,
        });
        self.pump(s);
        Ok(())
    }

    /// The core moved a part of the members: open the new scheme on the
    /// first part, and move the members and their underwritten cover to it.
    pub(crate) fn split_part_moved(&mut self, s: &mut Scheme, into: &str, employee_ids: &[String], last: bool) {
        let Some(split) = s.splitting.as_mut().filter(|p| p.into == into) else {
            return;
        };
        let (new_policy, effective) = (split.new_policy.clone(), split.effective);
        if !self.schemes.contains_key(into) {
            let mut new = Scheme::new(
                into.to_string(),
                new_policy.clone(),
                s.group.clone(),
                split.employer.clone(),
                s.inception,
                s.product.clone(),
                split.terms.clone(),
            );
            new.mapping = s.mapping.clone();
            new.second_mapping = s.second_mapping.clone();
            let brokered = match &new.terms.broker {
                Some(broker) => broker_layers(into, &new_policy, &new.product, broker).unwrap_or_default(),
                None => Vec::new(),
            };
            self.open_scheme(new, brokered);
        }
        split.moved += employee_ids.len() as u32;
        let (moved, members) = (split.moved, split.members);
        let Some(mut new) = self.schemes.remove(into) else { return };
        for id in employee_ids {
            let Some(member) = s.membership.members.get_mut(id) else { continue };
            let mut moving = member.clone();
            member.left = Some(effective);
            moving.left = None;
            moving.absence_since = member.absence_since.take();
            if s.absence_leaves.remove(id) {
                new.absence_leaves.insert(id.clone());
            }
            new.membership.insert(moving);
            // Cover in place moves with the member, under the same layer.
            let cases: Vec<u64> = s
                .cases
                .values()
                .filter(|c| &c.employee_id == id && c.status == CaseStatus::Accepted)
                .map(|c| c.id)
                .collect();
            for case in cases {
                if let Some(c) = s.cases.remove(&case) {
                    new.cases.insert(case, c);
                }
            }
        }
        self.emit(GroupEvent::MembersMoved {
            scheme: s.id.clone(),
            into: into.to_string(),
            moved,
        });
        if last {
            s.splitting = None;
            self.emit(GroupEvent::SplitCompleted {
                scheme: s.id.clone(),
                into: into.to_string(),
                effective,
                members,
            });
        }
        self.schemes.insert(into.to_string(), new);
    }

    /// The core refused a part. What moved stays moved; the parts not yet
    /// sent are dropped, and staff decide what to do with the rest.
    pub(crate) fn split_refused(&mut self, s: &mut Scheme, into: &str, code: &str) {
        let Some(split) = s.splitting.take() else { return };
        s.queue.retain(|i| {
            !matches!(
                &i.work,
                Work::Core {
                    then: After::SplitPart { .. },
                    ..
                }
            )
        });
        self.emit(GroupEvent::SplitRefused {
            scheme: s.id.clone(),
            into: into.to_string(),
            moved: split.moved,
            code: code.to_string(),
        });
    }
}
