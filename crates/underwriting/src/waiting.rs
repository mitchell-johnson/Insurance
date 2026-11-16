//! An income cover's waiting period shortened when business cover ends.
//!
//! AIA Living income cover (1108 AL-IP s22, the Waiting Period Reduction
//! Benefit): when the cover has a 26, 52 or 104 week waiting period and the
//! life's Business Continuity or Rural Continuity cover is cancelled because
//! the life left the business, the business was sold or it stopped trading,
//! the waiting period drops to that of the cancelled cover, with no health
//! questions. It applies to the lesser of the two monthly benefits. (The
//! wording also names Business Overheads and Locum Cover; no product here
//! sells them.)
//!
//! The request opens as a proposal, entry `waiting_reduction`, on the
//! policy holding the income cover: one life, that cover, and the policy,
//! life and cover that was cancelled, with why and where the proof is kept.
//! A person on the financial queue checks why it was cancelled
//! (`check_option_evidence`; reason RC-0982). Accepted, the decision has no
//! underwriting (source `facility`) and reserves no reinsurance capacity,
//! since no amount is added. The driver then reads both policies in the
//! core and sends the core `ConvertCover` from underwriting: the income
//! cover, with its waiting choice set to the cancelled cover's, for the
//! lesser of the two benefits, naming the proof. The core allows it only
//! from 26, 52 or 104 weeks, only to a shorter wait, and before age 65.

use ic_core::conversion::ConvertCover;
use ic_core::policy::{CoverStatus, Layer, LifeKind, Policy, PolicyStatus};
use ic_core::LifeId;

use crate::case::{CaseType, Queue};
use crate::codes::reason;
use crate::command::{OpenProposal, Reject};
use crate::flow;
use crate::machine::{reject, Cx};
use crate::model::{Entry, Proposal};

/// The AIA Living income covers that carry the benefit.
pub const INCOME_COVERS: [&str; 4] = ["income-agreed-value", "income-indemnity", "income-loss-of-earnings", "mortgage-income-rent"];
pub const INCOME_PRODUCT: &str = "aia-living";
/// The business covers whose cancellation shortens the wait.
pub const BUSINESS_COVERS: [&str; 3] = ["business-continuity-agreed-value", "business-continuity-indemnity", "rural-continuity"];
/// The axis the waiting period is chosen on.
pub const AXIS: &str = "waiting";

/// What a reduction request must be before it opens.
pub(crate) fn door(open: &OpenProposal) -> Result<(), Reject> {
    let Entry::WaitingReduction {
        policy,
        cancelled_policy,
        cancelled_cover,
        evidence,
        ..
    } = &open.entry
    else {
        return Ok(());
    };
    let refuse = |why: &str| reject("waiting_reduction", why);
    if open.lives.len() != 1 || open.covers.len() != 1 || policy != &open.policy || cancelled_policy == policy || open.replacement {
        return refuse("a waiting period is shortened on one income cover of one life, after cover on another policy ended");
    }
    if open.product_code != INCOME_PRODUCT || !INCOME_COVERS.contains(&open.covers[0].code.as_str()) {
        return refuse("the Waiting Period Reduction Benefit is on AIA Living income cover");
    }
    if !BUSINESS_COVERS.contains(&cancelled_cover.as_str()) {
        return refuse("the cover cancelled is Business Continuity or Rural Continuity");
    }
    if evidence.trim().is_empty() {
        return refuse("the request names where the proof of why the cover was cancelled is kept");
    }
    Ok(())
}

/// A new request goes to a person to check why the business cover ended.
pub(crate) fn start(p: &mut Proposal, cx: &mut Cx) -> Result<(), Reject> {
    if !matches!(p.entry, Entry::WaitingReduction { .. }) {
        return Ok(());
    }
    let life = p.lives.keys().next().cloned().expect("one life, checked at the door");
    flow::open_case(
        p,
        &life,
        CaseType::Referral,
        Queue::Financial,
        [reason::WAITING_REDUCTION.to_string()].into(),
        cx,
    )
    .map(|_| ())
}

/// The choice an option code makes on an axis.
fn choice_on<'a>(option: &'a str, axis: &str) -> Option<&'a str> {
    option.split(',').filter_map(|p| p.split_once('=')).find(|(a, _)| *a == axis).map(|(_, c)| c)
}

fn party(policy: &Policy, life: &LifeId) -> Option<String> {
    match &policy.lives.get(life)?.kind {
        LifeKind::Named { party } => Some(party.to_string()),
        _ => None,
    }
}

/// As [`conversion`], for policies the core may not hold and the terms as
/// the module sent them: one cover of one life.
pub fn conversion_for(
    entry: &Entry,
    held: Option<&Policy>,
    cancelled: Option<&Policy>,
    covers: &[ic_core::command::CoverSpec],
    reference: &str,
    today: chrono::NaiveDate,
) -> Result<ConvertCover, String> {
    let (Some(held), Some(cancelled)) = (held, cancelled) else {
        return Err("waiting_policy_not_found".into());
    };
    let [ic_core::command::CoverSpec { code, life: Some(life), .. }] = covers else {
        return Err("waiting_not_one_cover".into());
    };
    conversion(entry, held, cancelled, code, life, reference, today)
}

/// The core's `ConvertCover` for an accepted reduction, from both policies
/// as the core holds them today. The reason is the code the request is
/// refused with, for a person to look at.
pub fn conversion(
    entry: &Entry,
    held: &Policy,
    cancelled: &Policy,
    cover: &str,
    life: &LifeId,
    reference: &str,
    today: chrono::NaiveDate,
) -> Result<ConvertCover, String> {
    let Entry::WaitingReduction {
        cancelled_life,
        cancelled_cover,
        evidence,
        ..
    } = entry
    else {
        return Err("not_a_waiting_reduction".into());
    };
    if party(held, life).is_none() || party(held, life) != party(cancelled, cancelled_life) {
        return Err("waiting_not_the_same_life".into());
    }
    // The business cover has ended: its benefit and wait are those of the
    // day before it ended.
    let Some(ended) = cancelled.covers.get(cancelled_cover) else {
        return Err("waiting_cover_not_held".into());
    };
    // Cancelled is what the core has recorded, whatever day it took effect.
    if ended.status == CoverStatus::InForce && cancelled.status == PolicyStatus::InForce {
        return Err("waiting_cover_not_cancelled".into());
    }
    let last_day = |l: &Layer| l.end.map(|e| e - chrono::Duration::days(1));
    let on = ended
        .ended_on
        .map(|e| e - chrono::Duration::days(1))
        .or_else(|| ended.layers.iter().filter(|l| &l.life == cancelled_life).filter_map(last_day).max());
    let Some(on) = on else {
        return Err("waiting_cover_not_held".into());
    };
    let before: Vec<&Layer> = ended.layers.iter().filter(|l| &l.life == cancelled_life && l.in_force_on(on)).collect();
    let benefit: i64 = before.iter().map(|l| l.sum_insured_minor).sum();
    let wait = before
        .iter()
        .max_by_key(|l| (l.start, l.id))
        .and_then(|l| l.excess_option.as_deref())
        .and_then(|o| choice_on(o, AXIS))
        .map(str::to_string);
    let (Some(wait), true) = (wait, benefit > 0) else {
        return Err("waiting_cover_not_held".into());
    };
    // The income cover as held now: its newest choices, with the wait changed.
    let layers: Vec<&Layer> = held
        .covers
        .get(cover)
        .filter(|c| c.running_on(today))
        .map(|c| c.layers.iter().filter(|l| &l.life == life && l.in_force_on(today)).collect())
        .unwrap_or_default();
    let income: i64 = layers.iter().map(|l| l.sum_insured_minor).sum();
    let Some(option) = layers.iter().max_by_key(|l| (l.start, l.id)).and_then(|l| l.excess_option.clone()) else {
        return Err("waiting_income_not_held".into());
    };
    let changed: Vec<String> = option
        .split(',')
        .map(|part| match part.split_once('=') {
            Some((AXIS, _)) => format!("{AXIS}={wait}"),
            _ => part.to_string(),
        })
        .collect();
    Ok(ConvertCover {
        policy: held.id.clone(),
        cover: cover.to_string(),
        life: life.clone(),
        sum_insured_minor: income.min(benefit),
        to_cover: cover.to_string(),
        excess_option: changed.join(","),
        reference: reference.to_string(),
        evidence: Some(evidence.clone()),
    })
}
