//! New policies written with no health questions under a conversion
//! facility of a policy already in force.
//!
//! Two AIA facilities do this:
//!
//! - **Children's Trauma Conversion** (1105 AL-CC s6). A child covered by
//!   the Built-in Children's Trauma Benefit of a parent's Critical Conditions
//!   cover takes a policy of their own: standalone Critical Conditions, or
//!   accelerated Critical Conditions with an equal amount of Life Cover. The
//!   amount is at most the lower of $50,000 and half the parent's Critical
//!   Conditions amount. The request is made within 60 days of the policy
//!   anniversary after the child turns 21. It is not available for a child
//!   who claimed, or could have claimed, the Children's Trauma or the
//!   Children's and Maternity Benefit. It is priced at the child's age.
//! - **Move to Business Continuity** (AIA Start-Up Income Protection). After
//!   three years of trading, Start-Up Income moves to AIA Living Business
//!   Business Continuity, for no more monthly benefit than it had, on the
//!   terms it was accepted on. The Start-Up policy ends once Business
//!   Continuity is in force.
//!
//! The request opens as a proposal, entry `facility`, with a quote for the
//! new policy at the life's own age. The door checks what it can from the
//! request: the covers, the amounts and the window. A person on the
//! financial queue then checks what only a person can (the claims search
//! for a child; the accounts for a business), with `check_option_evidence`.
//! Accepted, the covers are decided with no underwriting (source
//! `facility`, reason RC-0979). Before the driver issues, it reads the
//! policy the facility belongs to in the core: in force, the cover on the
//! life, the amount within the limit, and for a child the anniversary is
//! one of that policy's. Business Continuity takes the Start-Up cover's
//! loadings and exclusions. Not accepted, the request ends withdrawn and
//! the policyholder is told (RC-0981).

use chrono::{Datelike, Months, NaiveDate};
use ic_core::command::IssuePolicy;
use ic_core::policy::{Policy, PolicyStatus};

use crate::case::{CaseType, Queue};
use crate::codes::reason;
use crate::command::{OpenProposal, Reject};
use crate::flow;
use crate::machine::{reject, Cx};
use crate::model::{Entry, Facility, Proposal};

/// The product a child's own policy is written on, and its covers.
pub const CHILD_PRODUCT: &str = "aia-living";
pub const STANDALONE: &str = "critical-conditions";
pub const ACCELERATED: &str = "critical-conditions-accelerated";
pub const LIFE: &str = "life";
/// The parent's covers that carry the Built-in Children's Trauma Benefit's
/// conversion facility.
pub const CHILD_SOURCES: [&str; 2] = [STANDALONE, ACCELERATED];
/// At most the lower of this and [`CHILD_PERCENT`] of the parent's amount.
pub const CHILD_MAX_MINOR: i64 = 50_000_00;
pub const CHILD_PERCENT: i64 = 50;
/// The anniversary after the child turns this age opens the window.
pub const CHILD_AGE: u32 = 21;
pub const CHILD_WINDOW_DAYS: i64 = 60;

/// Start-Up Income and where it moves to.
pub const START_UP_PRODUCT: &str = "aia-start-up-income";
pub const START_UP_COVER: &str = "start-up-income";
pub const BUSINESS_PRODUCT: &str = "aia-living-business";
pub const BUSINESS_COVERS: [&str; 2] = ["business-continuity-agreed-value", "business-continuity-indemnity"];
pub const TRADING_YEARS: u32 = 3;

fn years_after(date: NaiveDate, years: u32) -> Option<NaiveDate> {
    date.checked_add_months(Months::new(years * 12))
}

/// What a facility request must be before it opens.
pub(crate) fn door(open: &OpenProposal, cx: &Cx) -> Result<(), Reject> {
    let Entry::Facility {
        facility,
        from_policy,
        from_cover,
        trigger_date,
        evidence,
        ..
    } = &open.entry
    else {
        return Ok(());
    };
    let refuse = |why: &str| reject("facility", why);
    if open.lives.len() != 1 || open.replacement || !open.replaces.is_empty() || from_policy == &open.policy {
        return refuse("a facility writes one life's own new policy, beside the policy it belongs to");
    }
    if evidence.trim().is_empty() {
        return refuse("a facility names where what is to be checked is kept");
    }
    let mut codes: Vec<(&str, i64)> = open.covers.iter().map(|c| (c.code.as_str(), c.sum_insured_minor)).collect();
    codes.sort();
    match facility {
        Facility::ChildrensTraumaConversion => {
            if open.product_code != CHILD_PRODUCT || !CHILD_SOURCES.contains(&from_cover.as_str()) {
                return refuse("a child's conversion is from a parent's Critical Conditions cover to AIA Living");
            }
            let amount = match codes.as_slice() {
                [(STANDALONE, a)] => *a,
                [(ACCELERATED, a), (LIFE, l)] if a == l => *a,
                _ => {
                    return refuse("a child takes standalone Critical Conditions, or accelerated Critical Conditions with equal Life Cover");
                }
            };
            if amount > CHILD_MAX_MINOR {
                return refuse("a child's conversion is at most $50,000");
            }
            let born = open.lives[0].factors.date_of_birth;
            let turned = born.and_then(|b| years_after(b, CHILD_AGE));
            let next = born.and_then(|b| years_after(b, CHILD_AGE + 1));
            let (Some(turned), Some(next)) = (turned, next) else {
                return refuse("a child's conversion needs the child's date of birth");
            };
            if *trigger_date < turned || *trigger_date >= next {
                return refuse("the window opens on the policy anniversary after the child turns 21");
            }
            if cx.today < *trigger_date || cx.today > *trigger_date + chrono::Duration::days(CHILD_WINDOW_DAYS) {
                return refuse("a child's conversion is asked for within 60 days of the anniversary after they turn 21");
            }
        }
        Facility::BusinessContinuity => {
            if open.product_code != BUSINESS_PRODUCT || from_cover != START_UP_COVER {
                return refuse("Start-Up Income moves to AIA Living Business Business Continuity");
            }
            if !matches!(codes.as_slice(), [(code, _)] if BUSINESS_COVERS.contains(code)) {
                return refuse("Start-Up Income moves to one Business Continuity cover");
            }
            if years_after(*trigger_date, TRADING_YEARS).is_none_or(|due| cx.today < due) {
                return refuse("Start-Up Income moves after three years of trading");
            }
        }
    }
    Ok(())
}

/// A new facility request goes to a person to check what it asks.
pub(crate) fn start(p: &mut Proposal, cx: &mut Cx) -> Result<(), Reject> {
    if !matches!(p.entry, Entry::Facility { .. }) {
        return Ok(());
    }
    let life = p.lives.keys().next().cloned().expect("one life, checked at the door");
    flow::open_case(p, &life, CaseType::Referral, Queue::Financial, [reason::FACILITY.to_string()].into(), cx).map(|_| ())
}

/// The anniversaries of a policy's inception fall on its day and month; one
/// that started on 29 February has its anniversary on 28 February in other
/// years.
pub fn is_anniversary(inception: NaiveDate, day: NaiveDate) -> bool {
    if day <= inception {
        return false;
    }
    let years = (day.year() - inception.year()) as u32;
    years_after(inception, years) == Some(day)
}

/// The policy the facility belongs to, read from the core, allows the new
/// policy: in force, holding the cover on the life, within the amount, and
/// for a child the window opened on one of its anniversaries. Business
/// Continuity takes the Start-Up cover's terms. The reason is the code the
/// issue is refused with, for a person to look at.
pub fn verify(entry: &Entry, source: &Policy, issue: &mut IssuePolicy, today: NaiveDate) -> Result<(), String> {
    let Entry::Facility {
        facility,
        from_life,
        from_cover,
        trigger_date,
        ..
    } = entry
    else {
        return Ok(());
    };
    if source.status != PolicyStatus::InForce {
        return Err("facility_policy_not_in_force".into());
    }
    let Some(cover) = source.covers.get(from_cover).filter(|c| c.running_on(today)) else {
        return Err("facility_cover_not_held".into());
    };
    let layers: Vec<_> = cover.layers.iter().filter(|l| &l.life == from_life && l.in_force_on(today)).collect();
    let held: i64 = layers.iter().map(|l| l.sum_insured_minor).sum();
    if held <= 0 {
        return Err("facility_cover_not_held".into());
    }
    match facility {
        Facility::ChildrensTraumaConversion => {
            if !is_anniversary(source.inception, *trigger_date) {
                return Err("facility_not_an_anniversary".into());
            }
            // Multiply first: the child's amount is within half the parent's.
            if issue.covers.iter().any(|c| c.sum_insured_minor * 100 > held * CHILD_PERCENT) {
                return Err("facility_amount".into());
            }
        }
        Facility::BusinessContinuity => {
            if issue.covers.iter().any(|c| c.sum_insured_minor > held) {
                return Err("facility_amount".into());
            }
            // The cover moves on the terms it was accepted on.
            for c in &mut issue.covers {
                for l in &layers {
                    for loading in &l.loadings {
                        if !c.loadings.contains(loading) {
                            c.loadings.push(loading.clone());
                        }
                    }
                    for exclusion in &l.exclusions {
                        if !c.exclusions.contains(exclusion) {
                            c.exclusions.push(exclusion.clone());
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
