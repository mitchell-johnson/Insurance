//! The reinstatement proposal for a lapsed policy, built from the policy as
//! the core holds it. Collections calls this when the core refuses
//! `Reinstate` with `underwriting_required`: it reads the policy from the
//! core and sends what this returns as `open_proposal`, caller `collections`.

use chrono::NaiveDate;
use ic_core::policy::{LifeKind, Policy, PolicyStatus};

use crate::command::OpenProposal;
use crate::model::{Entry, ProposalId, ProposedCover, ProposedLife, QuoteBinding};

/// The proposal's ID: one per lapse, so a retry opens nothing twice and a
/// later lapse of the same policy gets a proposal of its own.
pub fn proposal_id(policy: &Policy, lapsed_on: NaiveDate) -> ProposalId {
    ProposalId::from(format!("reinstate:{}:{lapsed_on}", policy.id).as_str())
}

/// The proposal to underwrite the cover the policy held the day before it
/// lapsed, for the lives it was on. `None` when the policy is not lapsed,
/// held no cover, or covers someone who is not a named person (a scheme
/// member comes back through the membership file instead).
pub fn reinstatement_proposal(policy: &Policy, today: NaiveDate) -> Option<OpenProposal> {
    if policy.status != PolicyStatus::Lapsed {
        return None;
    }
    let lapsed_on = policy.lapse.as_ref()?.lapsed_on;
    let held_on = lapsed_on.pred_opt()?;
    let mut covers = Vec::new();
    for cover in policy.covers.values().filter(|c| !c.formula_driven) {
        let mut by_life = std::collections::BTreeMap::new();
        for layer in cover.layers.iter().filter(|l| l.in_force_on(held_on)) {
            *by_life.entry(layer.life.clone()).or_insert(0i64) += layer.sum_insured_minor;
        }
        for (life, sum_insured_minor) in by_life {
            covers.push(ProposedCover {
                code: cover.code.clone(),
                cover_type: cover.cover_type.clone(),
                life,
                sum_insured_minor,
                option: None,
            });
        }
    }
    if covers.is_empty() {
        return None;
    }
    let mut lives = Vec::new();
    for life in covers.iter().map(|c| &c.life).collect::<std::collections::BTreeSet<_>>() {
        let record = policy.lives.get(life)?;
        let LifeKind::Named { party } = &record.kind else { return None };
        lives.push(ProposedLife {
            life: life.clone(),
            party: party.clone(),
            factors: record.factors.as_of(held_on)?.clone(),
        });
    }
    Some(OpenProposal {
        proposal: proposal_id(policy, lapsed_on),
        entry: Entry::Reinstatement { policy: policy.id.clone() },
        policy: policy.id.clone(),
        policy_number: policy.number.clone(),
        group: policy.group.clone(),
        policyholder: policy.policyholder.clone(),
        product_code: policy.product_code.clone(),
        frequency: policy.frequency,
        channel: "collections".to_string(),
        adviser: None,
        // The premium is the policy's own: reinstatement does not re-price.
        quote: QuoteBinding {
            product_hash: policy.product_hash.clone(),
            rate_tables: vec![],
            premium_minor: 0,
            quoted_on: today,
        },
        lives,
        covers,
        legacy_received_on: None,
        replacement: false,
    })
}
