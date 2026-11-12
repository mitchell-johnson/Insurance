//! A lender's or assignee's interest in a policy.
//!
//! When a policy is assigned as security, the lender's interest is recorded
//! on the policy: who, when notice reached the insurer, how much, and from
//! what date. While an interest stands the owner cannot take value out of
//! the policy or give the cover up without the interested party's recorded
//! consent, and a claim pays interested parties first, in the order their
//! notices arrived.
//!
//! An interest is dated on both timelines: the date it takes effect and the
//! business date it was recorded, and the same for its release. Nothing is
//! deleted, so the question "who had an interest on the day of the event,
//! as we know it now" always has an answer.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::event::Event;
use crate::machine::{Ctx, Reject};
use crate::policy::Policy;
use crate::{PartyId, PolicyId};

/// What the owner may not do while an interest stands, without consent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestrictedAction {
    Surrender,
    Withdrawal,
    /// Giving up or reducing cover at the owner's request, cancellation
    /// included.
    ReduceCover,
    Loan,
    ChangeOwner,
}

impl RestrictedAction {
    pub fn name(self) -> &'static str {
        match self {
            RestrictedAction::Surrender => "surrender",
            RestrictedAction::Withdrawal => "withdrawal",
            RestrictedAction::ReduceCover => "reduce_cover",
            RestrictedAction::Loan => "loan",
            RestrictedAction::ChangeOwner => "change_owner",
        }
    }
}

/// The interested party's discharge of its interest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    /// The date from which the interest no longer stands.
    pub effective: NaiveDate,
    pub recorded_on: NaiveDate,
    /// Where the signed discharge is kept in the document vault.
    pub discharge: String,
}

/// One interest in a policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Interest {
    pub party: PartyId,
    /// The date notice of the assignment reached the insurer. Interests
    /// rank by it.
    pub notice_received: NaiveDate,
    /// The amount secured, in the policy's currency. None is the whole of
    /// the proceeds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extent_minor: Option<i64>,
    pub effective_from: NaiveDate,
    pub recorded_on: NaiveDate,
    /// Where the notice of assignment is kept in the document vault.
    pub notice: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released: Option<Release>,
    /// Consents the party has given and the owner has not yet used, by
    /// action, each with the vault reference of the signed consent.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub consents: BTreeMap<RestrictedAction, String>,
}

impl Interest {
    /// Whether the interest stands on a date, as known now.
    pub fn stands_on(&self, on: NaiveDate) -> bool {
        self.effective_from <= on && self.released.as_ref().is_none_or(|r| r.effective > on)
    }
}

/// An interest as a claim must honour it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ranked {
    pub interest: String,
    pub party: PartyId,
    pub extent_minor: Option<i64>,
    /// 1 is paid first.
    pub priority: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterestCommand {
    /// Record an interest from a notice of assignment.
    Record {
        policy: PolicyId,
        interest: String,
        party: PartyId,
        notice_received: NaiveDate,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        extent_minor: Option<i64>,
        effective_from: NaiveDate,
        notice: String,
    },
    /// Record the interested party's consent to one restricted action. It is
    /// used up by the next action of that kind.
    Consent {
        policy: PolicyId,
        interest: String,
        action: RestrictedAction,
        reference: String,
    },
    /// Release an interest on the party's signed discharge.
    Release {
        policy: PolicyId,
        interest: String,
        effective: NaiveDate,
        discharge: String,
    },
}

impl InterestCommand {
    pub fn allowed_callers(&self) -> &'static [&'static str] {
        &["policy-servicing"]
    }

    pub fn policy(&self) -> &PolicyId {
        match self {
            InterestCommand::Record { policy, .. } | InterestCommand::Consent { policy, .. } | InterestCommand::Release { policy, .. } => policy,
        }
    }
}

/// What happened to an interest, for notices, claims and the read models.
/// The party is an opaque ID; no personal data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterestEvent {
    Recorded {
        policy: PolicyId,
        interest: String,
        party: PartyId,
        notice_received: NaiveDate,
        extent_minor: Option<i64>,
        effective_from: NaiveDate,
    },
    ConsentRecorded {
        policy: PolicyId,
        interest: String,
        party: PartyId,
        action: RestrictedAction,
    },
    /// A recorded consent was used by the action it allowed.
    ConsentUsed {
        policy: PolicyId,
        interest: String,
        party: PartyId,
        action: RestrictedAction,
    },
    Released {
        policy: PolicyId,
        interest: String,
        party: PartyId,
        effective: NaiveDate,
    },
}

fn bad(code: &str, message: impl Into<String>) -> Reject {
    Reject::new(code, message)
}

/// A reference into the vault or an identifier: short, plain, never free text.
fn plain(s: &str) -> bool {
    !s.is_empty() && s.len() <= 120 && s.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':' | '/'))
}

pub(crate) fn apply(policy: &mut Policy, ctx: &mut Ctx<'_>, command: InterestCommand) -> Result<(), Reject> {
    match command {
        InterestCommand::Record {
            interest,
            party,
            notice_received,
            extent_minor,
            effective_from,
            notice,
            ..
        } => {
            if !plain(&interest) || !plain(&notice) || party.0.is_empty() {
                return Err(bad(
                    "invalid_interest",
                    "an interest has a plain identifier, a party and the vault reference of its notice",
                ));
            }
            if extent_minor.is_some_and(|m| m <= 0) || notice_received > ctx.date {
                return Err(bad(
                    "invalid_interest",
                    "the extent is above zero or left out, and notice cannot arrive after today",
                ));
            }
            let record = Interest {
                party,
                notice_received,
                extent_minor,
                effective_from,
                recorded_on: ctx.date,
                notice,
                released: None,
                consents: BTreeMap::new(),
            };
            if let Some(existing) = policy.interests.get(&interest) {
                let same = Interest {
                    recorded_on: existing.recorded_on,
                    released: existing.released.clone(),
                    consents: existing.consents.clone(),
                    ..record
                };
                return if *existing == same {
                    Ok(())
                } else {
                    Err(bad("duplicate_interest", format!("{interest} is recorded with other content")))
                };
            }
            ctx.emit(Event::Interest(InterestEvent::Recorded {
                policy: policy.id.clone(),
                interest: interest.clone(),
                party: record.party.clone(),
                notice_received,
                extent_minor,
                effective_from,
            }));
            policy.interests.insert(interest, record);
            Ok(())
        }
        InterestCommand::Consent {
            interest, action, reference, ..
        } => {
            let id = policy.id.clone();
            let record = standing(policy, &interest)?;
            if !plain(&reference) {
                return Err(bad("invalid_interest", "a consent carries the vault reference of the signed consent"));
            }
            if record.consents.get(&action) == Some(&reference) {
                return Ok(());
            }
            record.consents.insert(action, reference);
            ctx.emit(Event::Interest(InterestEvent::ConsentRecorded {
                policy: id,
                interest,
                party: record.party.clone(),
                action,
            }));
            Ok(())
        }
        InterestCommand::Release {
            interest,
            effective,
            discharge,
            ..
        } => {
            let id = policy.id.clone();
            let today = ctx.date;
            let Some(record) = policy.interests.get_mut(&interest) else {
                return Err(bad("unknown_interest", format!("{interest} is not recorded on this policy")));
            };
            if !plain(&discharge) || effective < record.effective_from {
                return Err(bad(
                    "invalid_interest",
                    "a release carries the vault reference of the discharge and is not before the interest began",
                ));
            }
            let release = Release {
                effective,
                recorded_on: today,
                discharge,
            };
            if let Some(existing) = &record.released {
                return if existing.effective == release.effective && existing.discharge == release.discharge {
                    Ok(())
                } else {
                    Err(bad("interest_released", format!("{interest} is already released")))
                };
            }
            record.released = Some(release);
            record.consents.clear();
            ctx.emit(Event::Interest(InterestEvent::Released {
                policy: id,
                interest,
                party: record.party.clone(),
                effective,
            }));
            Ok(())
        }
    }
}

fn standing<'a>(policy: &'a mut Policy, interest: &str) -> Result<&'a mut Interest, Reject> {
    match policy.interests.get_mut(interest) {
        None => Err(bad("unknown_interest", format!("{interest} is not recorded on this policy"))),
        Some(record) if record.released.is_some() => Err(bad("interest_released", format!("{interest} is already released"))),
        Some(record) => Ok(record),
    }
}

/// The interests that stand on a date, first notice first.
pub fn ranked(policy: &Policy, on: NaiveDate) -> Vec<Ranked> {
    let mut standing: Vec<(&String, &Interest)> = policy.interests.iter().filter(|(_, i)| i.stands_on(on)).collect();
    standing.sort_by_key(|(id, i)| (i.notice_received, i.recorded_on, (*id).clone()));
    standing
        .into_iter()
        .zip(1u32..)
        .map(|((id, i), priority)| Ranked {
            interest: id.clone(),
            party: i.party.clone(),
            extent_minor: i.extent_minor,
            priority,
        })
        .collect()
}

/// Refuse a restricted action unless every interest standing today has
/// consented to it. Checked before the action; nothing is changed.
pub(crate) fn permit(policy: &Policy, today: NaiveDate, action: RestrictedAction) -> Result<(), Reject> {
    match policy.interests.iter().find(|(_, i)| i.stands_on(today) && !i.consents.contains_key(&action)) {
        None => Ok(()),
        Some((id, _)) => Err(bad(
            "interest_consent_required",
            format!("interest {id} stands on this policy and its holder has not consented to this {}", action.name()),
        )),
    }
}

/// The action went through: the consents that allowed it are used up.
pub(crate) fn consume(policy: &mut Policy, ctx: &mut Ctx<'_>, action: RestrictedAction) {
    let id = policy.id.clone();
    let today = ctx.date;
    for (interest, record) in policy.interests.iter_mut().filter(|(_, i)| i.stands_on(today)) {
        if record.consents.remove(&action).is_some() {
            ctx.emit(Event::Interest(InterestEvent::ConsentUsed {
                policy: id.clone(),
                interest: interest.clone(),
                party: record.party.clone(),
                action,
            }));
        }
    }
}
