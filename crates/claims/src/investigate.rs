//! Restricted investigations.
//!
//! A fraud rule, or the compliance module, opens an investigation over one
//! or more claims. It is a restricted case: only staff with the investigator
//! role see it or act on it, and nothing about it reaches a view that front
//! line staff or the claimant read. While it is open, the claims in it are
//! not admitted by the rules and no money leaves for them; to everyone else
//! they look like claims waiting for an assessor.
//!
//! Claims runs two rules itself, each when a claim is registered: duplicate
//! provider billing (the same provider billing the same service on the same
//! date twice, on one member's claims or across members) and an early claim
//! (an event within `Rules.early_claim_days` of the cover starting). What
//! needs a person or a pattern over many claims (altered documents, a
//! network of claimants) belongs to the compliance module, which refers it
//! here with `ReferForInvestigation`. A claim can be in more than one case;
//! it stays restricted until the last of them is closed.

use std::collections::BTreeSet;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::command::Actor;
use crate::config::Role;
use crate::model::{Claim, OverpaymentCause, RecoveryStatus, Status};
use crate::port::{Instruction, Reject};
use crate::state::{ClaimsState, Cx};
use crate::ClaimId;

/// The rule that raises a case when a provider bills one service twice.
pub const DUPLICATE_BILLING: &str = "duplicate_billing";

/// The rule that raises a case when the event falls soon after the cover started.
pub const EARLY_CLAIM: &str = "early_claim";

/// The key a provider's billed service is known by: the date of service,
/// the provider and the service code.
pub type Billed = (NaiveDate, String, String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Investigation {
    pub case: String,
    pub rule: String,
    pub opened_on: NaiveDate,
    /// Who referred it: `claims` for a rule run here, else the caller.
    pub referred_by: String,
    pub claims: BTreeSet<ClaimId>,
    pub closed: Option<Closed>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Closed {
    pub on: NaiveDate,
    pub by: String,
    pub outcome: Outcome,
}

/// How an investigation ends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Nothing wrong: the claims go on as they would have.
    Cleared,
    /// Confirmed. Each claim named in `decline` that is still open is
    /// declined. Each `(claim, amount)` in `recover` that was paid to a
    /// provider becomes an overpayment owed by that provider, taken back
    /// out of its next provider runs.
    Confirmed {
        #[serde(default)]
        decline: Vec<ClaimId>,
        #[serde(default)]
        recover: Vec<(ClaimId, i64)>,
    },
}

/// What an investigator sees of a case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseView {
    pub case: String,
    pub rule: String,
    pub opened_on: NaiveDate,
    pub referred_by: String,
    /// Each claim with its status, amount billed and provider.
    pub claims: Vec<(ClaimId, Status, i64, Option<String>)>,
    pub closed: Option<Closed>,
}

/// The billed service a health claim carries, where the invoice gave one.
pub(crate) fn billed(c: &Claim) -> Option<Billed> {
    let h = c.health.as_ref()?;
    Some((h.service_date?, h.provider.clone()?, h.service_code.clone()?))
}

impl ClaimsState {
    /// Run the rules on a claim being registered. Returns whether a case is
    /// open on it.
    pub(crate) fn screen(&mut self, cx: &mut Cx<'_>, claim: &mut Claim, cover_start: Option<NaiveDate>) -> bool {
        if self.screen_billing(cx, claim) {
            return true;
        }
        let days = self.rules.early_claim_days;
        let early = cover_start.is_some_and(|start| days > 0 && claim.event_date <= crate::state::plus_days(start, days));
        if early {
            let case = format!("investigation/{}", claim.id);
            self.open_case(cx, &case, EARLY_CLAIM, "claims", [claim.id.clone()].into(), Some(claim));
        }
        early
    }

    /// The duplicate billing rule: any other claim on this partition for the
    /// same billed service joins one case with this one.
    fn screen_billing(&mut self, cx: &mut Cx<'_>, claim: &mut Claim) -> bool {
        let Some(key) = billed(claim) else { return false };
        let (date, provider, code) = key;
        let from = (date, provider.clone(), code.clone(), String::new());
        let others: Vec<ClaimId> = self
            .billed
            .range(from..)
            .take_while(|(d, p, s, _)| *d == date && *p == provider && *s == code)
            .map(|(_, _, _, id)| id.clone())
            .filter(|id| *id != claim.id)
            .collect();
        if others.is_empty() {
            return false;
        }
        let case = format!("investigation/{}", claim.id);
        let mut members: BTreeSet<ClaimId> = others.iter().cloned().collect();
        members.insert(claim.id.clone());
        self.open_case(cx, &case, DUPLICATE_BILLING, "claims", members, Some(claim));
        true
    }

    /// A case referred from outside, such as the compliance module.
    pub(crate) fn refer(&mut self, cx: &mut Cx<'_>, case: String, rule: String, referred_by: &str, claims: Vec<ClaimId>) -> Result<(), Reject> {
        if case.is_empty() || rule.is_empty() || claims.is_empty() {
            return Err(Reject::new("invalid_referral", "a referral names its case, its rule and at least one claim"));
        }
        if let Some(existing) = self.investigations.get(&case) {
            // A repeat of the same referral is already done.
            return if existing.rule == rule && existing.claims == claims.iter().cloned().collect() {
                Ok(())
            } else {
                Err(Reject::new("case_exists", format!("case {case} is already open with other claims")))
            };
        }
        for id in &claims {
            self.claim(id)?;
        }
        self.open_case(cx, &case, &rule, referred_by, claims.into_iter().collect(), None);
        Ok(())
    }

    fn open_case(&mut self, cx: &mut Cx<'_>, case: &str, rule: &str, referred_by: &str, claims: BTreeSet<ClaimId>, new: Option<&mut Claim>) {
        let new_id = new.as_ref().map(|c| c.id.clone());
        for id in claims.iter().filter(|id| Some(*id) != new_id.as_ref()) {
            if let Ok(mut other) = self.claim(id) {
                other.investigation.get_or_insert_with(|| case.to_string());
                self.commit(other);
            }
        }
        if let Some(c) = new {
            c.investigation = Some(case.to_string());
        }
        self.investigations.insert(
            case.to_string(),
            Investigation {
                case: case.to_string(),
                rule: rule.to_string(),
                opened_on: cx.today,
                referred_by: referred_by.to_string(),
                claims: claims.clone(),
                closed: None,
            },
        );
        // For the compliance module and the investigators' queue. The case
        // is the subject; nothing clinical and nothing a letter could use.
        let list = claims.into_iter().collect::<Vec<_>>().join(",");
        cx.notify("investigation_opened", case, &[("rule", rule.to_string()), ("claims", list)]);
    }

    fn investigator(&self, actor: &Actor) -> Result<(), Reject> {
        self.staff(actor)?;
        if self.authority.has_role(&actor.staff, Role::Investigator) {
            Ok(())
        } else {
            // Said the same way as for a case that does not exist, so the
            // answer tells nobody a case is there.
            Err(Reject::new("unknown_case", "there is no such case"))
        }
    }

    /// An investigator closes a case.
    pub(crate) fn close_case(&mut self, cx: &mut Cx<'_>, case: &str, outcome: Outcome, actor: &Actor) -> Result<(), Reject> {
        self.investigator(actor)?;
        let Some(open) = self.investigations.get(case).filter(|i| i.closed.is_none()).cloned() else {
            return Err(Reject::new("unknown_case", "there is no such case"));
        };
        if let Outcome::Confirmed { decline, recover } = &outcome {
            let outside = decline.iter().chain(recover.iter().map(|(c, _)| c)).find(|c| !open.claims.contains(*c));
            if let Some(c) = outside {
                return Err(Reject::new("not_in_case", format!("claim {c} is not in this case")));
            }
            for (id, amount) in recover {
                let c = self.claim(id)?;
                let provider = c.health.as_ref().and_then(|h| h.provider.as_ref());
                let paid = c.paid_minor(false);
                let raised: i64 = c.overpayments.iter().map(|o| o.amount_minor).sum();
                if provider.is_none() || *amount <= 0 || raised + amount > paid {
                    return Err(Reject::new(
                        "invalid_recovery",
                        format!("claim {id} was not paid {amount} to a provider that is not already being recovered"),
                    ));
                }
            }
        }
        // The claims leave the case before anything is decided on them, so
        // a decline or a recovery is the ordinary kind from here on.
        for id in &open.claims {
            if let Ok(mut c) = self.claim(id) {
                if c.investigation.as_deref() == Some(case) {
                    // Still restricted while another case holds it.
                    c.investigation = self
                        .investigations
                        .values()
                        .find(|i| i.case != case && i.closed.is_none() && i.claims.contains(id))
                        .map(|i| i.case.clone());
                }
                self.commit(c);
            }
        }
        if let Outcome::Confirmed { decline, recover } = &outcome {
            for id in decline {
                let mut c = self.claim(id)?;
                if c.status.is_open() && c.status != Status::InPayment {
                    self.close(cx, &mut c, Status::Declined, None)?;
                    crate::state::note(&mut c, cx, Some(actor), "declined_after_review");
                    self.commit(c);
                }
            }
            for (id, amount) in recover {
                let mut c = self.claim(id)?;
                let Some(mut o) = Self::overpayment(&c, cx, *amount, OverpaymentCause::ProviderBilling, false) else {
                    continue;
                };
                cx.core.execute(Instruction::RaiseOverpayment {
                    claim: c.id.clone(),
                    policy: c.policy.clone(),
                    cover: c.cover.clone(),
                    overpayment: o.id.clone(),
                    amount_minor: *amount,
                })?;
                // A provider's billing is recovered in full from what it is
                // paid next: there is no hardship to weigh.
                o.status = RecoveryStatus::Offset;
                o.offset_pct = 100;
                o.history.push((cx.today, RecoveryStatus::Offset, 0));
                crate::overpay::publish(cx, "overpayment_raised", &c, &o, &[("amount_minor", amount.to_string())]);
                c.overpayments.push(o);
                self.commit(c);
            }
        }
        let code = match &outcome {
            Outcome::Cleared => "cleared",
            Outcome::Confirmed { .. } => "confirmed",
        };
        if let Some(i) = self.investigations.get_mut(case) {
            i.closed = Some(Closed {
                on: cx.today,
                by: actor.staff.clone(),
                outcome,
            });
        }
        cx.notify("investigation_closed", case, &[("outcome", code.to_string())]);
        Ok(())
    }

    /// The cases an investigator may see, open first. Anyone else sees none.
    pub fn cases(&self, staff: &str) -> Vec<CaseView> {
        if !self.authority.has_role(staff, Role::Investigator) {
            return vec![];
        }
        let mut out: Vec<CaseView> = self
            .investigations
            .values()
            .map(|i| CaseView {
                case: i.case.clone(),
                rule: i.rule.clone(),
                opened_on: i.opened_on,
                referred_by: i.referred_by.clone(),
                claims: i
                    .claims
                    .iter()
                    .filter_map(|id| self.claims.get(id))
                    .map(|c| (c.id.clone(), c.status, c.claimed_minor, c.health.as_ref().and_then(|h| h.provider.clone())))
                    .collect(),
                closed: i.closed.clone(),
            })
            .collect();
        out.sort_by_key(|c| (c.closed.is_some(), c.opened_on));
        out
    }
}
