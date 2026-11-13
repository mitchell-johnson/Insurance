//! Who is to receive a death benefit: the owner's nomination.
//!
//! A nomination names people and their shares of what a death claim pays
//! after any lender or assignee has been paid. It is dated on both
//! timelines: the date it takes effect and the business date it was
//! recorded. A nomination is never edited or deleted. A change is a new
//! nomination that takes over from its own effective date, and taking a
//! nomination away is a new one that names nobody. So the question "who was
//! nominated on the day of the death, as we know it now" always has an
//! answer, and so does "as we knew it on a given day".
//!
//! The core holds the record and answers the question. Whether a change
//! must wait for a conversation with a trained person is decided by
//! servicing, which knows the customer; the core keeps the reference of the
//! conversation where one was held.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::event::Event;
use crate::machine::{Ctx, Reject};
use crate::policy::Policy;
use crate::{LifeId, PartyId, PolicyId};

/// The most people one nomination may name.
pub const MAX_NOMINEES: usize = 20;

/// One nominated person and their share.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Nominee {
    pub party: PartyId,
    /// Their share of what is left after interests, in whole percent.
    pub share_pct: u32,
}

/// One nomination as recorded. Kept for ever.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Nomination {
    pub id: String,
    /// The life whose death benefit this is about. None covers every life
    /// on the policy that has no nomination of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub life: Option<LifeId>,
    /// Shares add up to 100. Empty takes the nomination away: the benefit
    /// then goes where the policy says it goes without one.
    pub nominees: Vec<Nominee>,
    pub effective_from: NaiveDate,
    pub recorded_on: NaiveDate,
    /// Where the signed form is kept in the document vault.
    pub form: String,
    /// The reference of the conversation held before the change, where
    /// servicing paused it for one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
}

/// Record a nomination, a change to one, or its removal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Nominate {
    pub policy: PolicyId,
    pub nomination: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub life: Option<LifeId>,
    pub nominees: Vec<Nominee>,
    pub effective_from: NaiveDate,
    pub form: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
}

impl Nominate {
    pub fn allowed_callers(&self) -> &'static [&'static str] {
        &["policy-servicing"]
    }
}

fn bad(message: impl Into<String>) -> Reject {
    Reject::new("invalid_nomination", message)
}

/// A reference into the vault or an identifier: short, plain, never free text.
fn plain(s: &str) -> bool {
    !s.is_empty() && s.len() <= 120 && s.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':' | '/'))
}

pub(crate) fn apply(policy: &mut Policy, ctx: &mut Ctx<'_>, command: Nominate) -> Result<(), Reject> {
    let Nominate {
        nomination,
        life,
        nominees,
        effective_from,
        form,
        conversation,
        ..
    } = command;
    if !plain(&nomination) || !plain(&form) || conversation.as_deref().is_some_and(|c| !plain(c)) {
        return Err(bad("a nomination has a plain identifier and the vault reference of its signed form"));
    }
    if effective_from > ctx.date || effective_from < policy.inception {
        return Err(bad("a nomination takes effect on or after the policy's start and not after today"));
    }
    if life.as_ref().is_some_and(|l| !policy.lives.contains_key(l)) {
        return Err(bad("the life is not on this policy"));
    }
    let mut parties: Vec<&PartyId> = nominees.iter().map(|n| &n.party).collect();
    parties.sort();
    parties.dedup();
    let shares: u64 = nominees.iter().map(|n| u64::from(n.share_pct)).sum();
    let named_ok =
        nominees.len() <= MAX_NOMINEES && parties.len() == nominees.len() && nominees.iter().all(|n| !n.party.0.is_empty() && n.share_pct > 0) && shares == 100;
    if !nominees.is_empty() && !named_ok {
        return Err(bad(format!(
            "a nomination names up to {MAX_NOMINEES} different people with shares above zero that add up to 100"
        )));
    }
    let record = Nomination {
        id: nomination,
        life,
        nominees,
        effective_from,
        recorded_on: ctx.date,
        form,
        conversation,
    };
    if let Some(existing) = policy.nominations.iter().find(|n| n.id == record.id) {
        let same = Nomination {
            recorded_on: existing.recorded_on,
            ..record
        };
        return if *existing == same {
            Ok(())
        } else {
            Err(Reject::new("duplicate_nomination", format!("{} is recorded with other content", same.id)))
        };
    }
    if record.nominees.is_empty() && in_force(policy, record.life.as_ref(), ctx.date, None).is_none_or(|n| n.life != record.life) {
        return Err(bad("there is no nomination to take away"));
    }
    ctx.emit(Event::NominationRecorded {
        policy: policy.id.clone(),
        nomination: record.id.clone(),
        life: record.life.clone(),
        nominees: record.nominees.clone(),
        effective_from,
        conversation_held: record.conversation.is_some(),
    });
    policy.nominations.push(record);
    Ok(())
}

/// The nomination that governs a life's death benefit on a date. With
/// `known_on`, the answer is the one the record gave on that business date;
/// without it, the answer as the record stands now.
///
/// A nomination for the life itself wins over one for the whole policy.
/// Among those in effect by the date the latest effective date governs, and
/// of two with the same effective date the one recorded later. The result
/// may be a removal: one that names nobody.
pub fn in_force<'a>(policy: &'a Policy, life: Option<&LifeId>, on: NaiveDate, known_on: Option<NaiveDate>) -> Option<&'a Nomination> {
    let governing = |scope: Option<&LifeId>| {
        policy
            .nominations
            .iter()
            .enumerate()
            .filter(|(_, n)| n.life.as_ref() == scope && n.effective_from <= on && known_on.is_none_or(|k| n.recorded_on <= k))
            .max_by_key(|(at, n)| (n.effective_from, n.recorded_on, *at))
            .map(|(_, n)| n)
    };
    life.and_then(|l| governing(Some(l))).or_else(|| governing(None))
}

/// The people nominated for a life's death benefit on a date, as known now.
/// Empty where there is no nomination or it was taken away.
pub fn nominees<'a>(policy: &'a Policy, life: Option<&LifeId>, on: NaiveDate) -> &'a [Nominee] {
    in_force(policy, life, on, None).map_or(&[], |n| n.nominees.as_slice())
}
