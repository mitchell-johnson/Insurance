//! A proposal's progress, as rows for the portals.
//!
//! The channel module keeps audience-scoped read models; a module publishes
//! rows of its own into them under keys `{policy}/underwriting/...`. A
//! proposal has one row anyone on the policy may see (where it stands and
//! whose move it is) and one row per life that only that person sees,
//! because a life's stage (waiting for evidence, declined) implies health.
//!
//! The module keeps, for each open proposal, the rows last published. A
//! command that changes a proposal so that its rows differ marks it pending
//! under a new version; the driver reads what is pending
//! ([`crate::special::Query::Progress`]), publishes it, and says so
//! (`Command::ProgressPublished`). A version that has moved on in between
//! is published again on the next pass.

use ic_core::{PartyId, PolicyId};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::model::{Proposal, ProposalId, ProposalStatus, Stage};

/// The name the rows are published under, which the channel module checks
/// against the key.
pub const MODULE: &str = "underwriting";

/// One read model row, in the form the channel module takes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressRow {
    pub key: String,
    pub policy: PolicyId,
    /// `proposal` or `proposal_life`.
    pub kind: String,
    pub subject: Option<PartyId>,
    pub sensitive: bool,
    pub body: Value,
}

/// A proposal waiting to be published.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub proposal: ProposalId,
    pub version: u64,
    pub rows: Vec<ProgressRow>,
}

/// Whose move it is at a stage: the applicant's to answer or to accept, or
/// the insurer's. A final stage is nobody's.
fn waiting_on(stage: Stage) -> Option<&'static str> {
    match stage {
        Stage::AwaitingDisclosure | Stage::OfferOpen => Some("applicant"),
        s if s.is_final() => None,
        _ => Some("insurer"),
    }
}

/// The proposal's status as the portals show it. A decline is "ended":
/// that a proposal was declined implies health, and the row is not scoped
/// to one person. Issuing and a failed issue are the insurer's business.
fn status(p: &Proposal) -> &'static str {
    match &p.status {
        ProposalStatus::Open | ProposalStatus::Issuing | ProposalStatus::IssueFailed { .. } => "open",
        ProposalStatus::Declined { .. } => "ended",
        other => other.key(),
    }
}

/// What the proposal is for, by the name its entry has on the wire.
fn entry(p: &Proposal) -> Value {
    serde_json::to_value(&p.entry).unwrap_or_default()["entry"].clone()
}

/// The rows for a proposal as it stands.
pub fn rows(p: &Proposal) -> Vec<ProgressRow> {
    let open = p.status.is_open();
    let proposal_waits = match &p.status {
        ProposalStatus::Open if p.lives.values().any(|f| waiting_on(f.stage) == Some("applicant")) => Some("applicant"),
        s if s.is_open() => Some("insurer"),
        _ => None,
    };
    let mut rows = vec![ProgressRow {
        key: format!("{}/{MODULE}/{}", p.policy, p.id),
        policy: p.policy.clone(),
        kind: "proposal".into(),
        subject: None,
        sensitive: false,
        body: json!({
            "proposal": p.id,
            "policy_number": p.policy_number,
            "entry": entry(p),
            "status": status(p),
            "waiting_on": proposal_waits,
            "received_on": p.received_on,
            "expires_on": p.expires_on,
        }),
    }];
    for (life, f) in &p.lives {
        let mut body = json!({
            "proposal": p.id,
            "life": life,
            "stage": f.stage.key(),
            "waiting_on": if open { waiting_on(f.stage) } else { None },
        });
        // Terms offered and waiting for the life's answer: the offer letter
        // holds the terms; this says which offer to answer and by when.
        // A group member's offer to take over their own loading is one too.
        if open {
            let waiting = f
                .open_offer()
                .map(|o| (o.id.clone(), o.expires_on))
                .or_else(|| p.open_reoffer().filter(|r| &r.life == life).map(|r| (r.id.clone(), r.expires_on)));
            if let Some((offer, expires_on)) = waiting {
                body["offer"] = json!({ "offer": offer, "expires_on": expires_on });
            }
        }
        // A replacement shows the life what it gains, for the disclosure
        // statement they sign: the added cover and the premium quoted.
        // A new policy names the policy it replaces; a top-up replaces on its own.
        if p.replacement {
            let named = p.replaces.iter().find(|r| &r.life == life);
            let cover = match named {
                Some(r) => p.cover(life, &r.cover),
                None => p.covers_on(life).next(),
            };
            if let Some(c) = cover {
                body["replacement"] = json!({
                    "cover": c.code,
                    "sum_insured_minor": c.sum_insured_minor,
                    "premium_minor": p.quote.premium_minor,
                    "replaced_policy": named.map_or(&p.policy, |r| &r.replaced_policy),
                });
            }
        }
        rows.push(ProgressRow {
            key: format!("{}/{MODULE}/{}/{life}", p.policy, p.id),
            policy: p.policy.clone(),
            kind: "proposal_life".into(),
            subject: Some(f.party.clone()),
            sensitive: true,
            body,
        });
    }
    rows
}
