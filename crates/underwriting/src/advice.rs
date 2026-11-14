//! New business advised to the commission module.
//!
//! When an adviser's new policy is issued, the commission module is told
//! who wrote it, under which provider, on which commission option, whether
//! it replaces cover held elsewhere, and each accepted cover's annual
//! premium. It writes the adviser's layers itself; this module knows
//! nothing of engagements, books or schedules. One command per proposal,
//! under the key `uw-nb:{proposal}`, so a resend is one attribution.
//!
//! The annual premium of a cover is the quote's yearly figure for it at
//! standard rates where the quote breaks the premium down by cover. Where
//! it does not, the first period's premium is made yearly and shared
//! across the proposed covers in proportion to their amounts, the last
//! cover taking what the whole-unit shares leave, so the shares always add
//! up to the yearly premium. A cover accepted with a loading carries the
//! loading on its premium, rounded half up by the money crate. A cover
//! declined, deferred or postponed earns nothing and is left out.

use std::collections::BTreeMap;

use ic_money::{dec_i128, round_integer, RoundingMode};

use crate::command::Effect;
use crate::machine::Cx;
use crate::model::{CoverOutcome, Entry, Proposal, Stage};

/// The idempotency key the advice goes under at the commission module.
pub fn key(proposal: &str) -> String {
    format!("uw-nb:{proposal}")
}

/// Each proposed cover's yearly premium at standard rates, by cover code.
pub fn standard_premiums(p: &Proposal) -> BTreeMap<String, i64> {
    let quoted = &p.quote.cover_premiums_minor;
    if !quoted.is_empty() {
        return quoted.clone();
    }
    let yearly = i128::from(p.quote.premium_minor) * i128::from(p.frequency.periods_per_year());
    let total: i128 = p.covers.iter().map(|c| i128::from(c.sum_insured_minor)).sum();
    let mut shares = BTreeMap::new();
    if yearly <= 0 || total <= 0 {
        return shares;
    }
    let mut given: i128 = 0;
    for (n, c) in p.covers.iter().enumerate() {
        // Multiply first, divide last; the last cover takes the remainder.
        let share = if n + 1 == p.covers.len() {
            yearly - given
        } else {
            yearly * i128::from(c.sum_insured_minor) / total
        };
        given += share;
        *shares.entry(c.code.clone()).or_insert(0) += i64::try_from(share).unwrap_or(i64::MAX);
    }
    shares
}

/// A premium with a percentage loading added, rounded half up.
pub fn loaded(premium_minor: i64, loading_percent: u32) -> i64 {
    if loading_percent == 0 {
        return premium_minor;
    }
    let loaded = dec_i128(i128::from(premium_minor) * (100 + i128::from(loading_percent))) / dec_i128(100);
    round_integer(loaded, RoundingMode::HalfUp)
        .ok()
        .and_then(|v| i64::try_from(v).ok())
        .unwrap_or(i64::MAX)
}

/// Each accepted cover's annual premium as charged, by cover code.
pub fn accepted_premiums(p: &Proposal) -> BTreeMap<String, i64> {
    let standard = standard_premiums(p);
    let mut premiums = BTreeMap::new();
    for f in p.lives.values().filter(|f| f.stage == Stage::Accepted) {
        let decision = f.effective_decision();
        for c in p.covers.iter().filter(|c| c.life == f.life) {
            if let Some(CoverOutcome::Accept { loading_percent, .. }) = decision.and_then(|d| d.terms_for(&c.code)) {
                if let Some(premium) = standard.get(&c.code).filter(|v| **v > 0) {
                    premiums.insert(c.code.clone(), loaded(*premium, *loading_percent));
                }
            }
        }
    }
    premiums
}

/// The policy is issued: an adviser's new business is advised to the
/// commission module, once. A policy with no premium to pay commission on
/// is not advised; commission operations see it has no layers.
pub(crate) fn advise(p: &Proposal, cx: &mut Cx) {
    let (Entry::NewBusiness, Some(adviser)) = (&p.entry, &p.adviser) else {
        return;
    };
    let premiums = accepted_premiums(p);
    if premiums.is_empty() {
        return;
    }
    cx.effect(Effect::AdviseNewBusiness {
        key: key(&p.id.0),
        proposal: p.id.clone(),
        policy: p.policy.clone(),
        adviser: adviser.adviser.clone(),
        provider: adviser.fap.clone(),
        option: p.commission_option.clone().unwrap_or_else(|| "upfront".into()),
        replacement: p.replacement,
        premiums,
    });
}
