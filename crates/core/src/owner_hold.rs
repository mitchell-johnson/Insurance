//! A hold the owner sets on their own money going out.
//!
//! The customer master owns the hold: the person places it, asks for it to
//! end under fresh verification, waits out the cooling-off and may change
//! their mind there; staff cannot lift it (`docs/decisions.md`, decision 3).
//! The master tells the core, policy by policy, when the hold stands and
//! when it has ended, and the core keeps both dates and the master's
//! reference.
//!
//! While it stands the core refuses what would take value out of the policy
//! at the owner's request: a surrender, a withdrawal and a refund of a
//! credit balance. Premiums are still collected, cover carries on, claims
//! are paid, and money the insurer owes for its own error is still raised.
//! Payments holds the same kinds of money where it leaves, so the two
//! together stop a request made before the hold and one made after it.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::event::Event;
use crate::machine::{Ctx, Reject};
use crate::policy::Policy;
use crate::PolicyId;

/// The hold as the core keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerHold {
    /// The customer master's reference for the hold.
    pub reference: String,
    pub since: NaiveDate,
}

/// The customer master says the owner's hold stands, or has ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetOwnerHold {
    pub policy: PolicyId,
    pub held: bool,
    /// The master's reference: for the hold when placing it, and for how
    /// it ended (the cooling-off that ran out, or the legal direction).
    pub reference: String,
}

impl SetOwnerHold {
    pub fn allowed_callers(&self) -> &'static [&'static str] {
        &["customer-master"]
    }
}

pub(crate) fn apply(policy: &mut Policy, ctx: &mut Ctx<'_>, command: SetOwnerHold) -> Result<(), Reject> {
    if !crate::interest::plain(&command.reference) {
        return Err(Reject::new("invalid_hold", "a hold carries the customer master's plain reference"));
    }
    if command.held == policy.owner_hold.is_some() {
        return Ok(());
    }
    policy.owner_hold = command.held.then(|| OwnerHold {
        reference: command.reference.clone(),
        since: ctx.date,
    });
    ctx.emit(Event::OwnerHoldChanged {
        policy: policy.id.clone(),
        held: command.held,
        reference: command.reference,
    });
    Ok(())
}

/// Refuse an owner's request to take value out while their hold stands.
pub(crate) fn permit(policy: &Policy) -> Result<(), Reject> {
    match &policy.owner_hold {
        None => Ok(()),
        Some(hold) => Err(Reject::new(
            "customer_hold",
            format!(
                "the owner set a hold on money going out on {}; it ends only through the customer master",
                hold.since
            ),
        )),
    }
}
