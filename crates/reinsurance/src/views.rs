//! Read-only views over the module's state: the exposure to each reinsurer,
//! concentration by scheme, the checks reconciliation runs, and the
//! comparisons used when a treaty changes or a book is migrated.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_core::{PartyId, PolicyId};
use serde::{Deserialize, Serialize};

use crate::account::LineKind;
use crate::cession::{cession_on, timeline, Cession, Source};
use crate::machine::Reinsurance;
use crate::premium::ChargeKind;
use crate::treaty::Treaty;
use crate::{LifeKey, ReinsurerId, RiskKey, TreatyId};

/// What the insurer has at stake with one reinsurer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exposure {
    /// Net balance not yet settled. Positive is owed to the reinsurer.
    pub balance_minor: i64,
    /// Recoveries expected on open claims and not yet billed.
    pub open_recoverables_minor: i64,
    /// Recoveries in dispute and not yet settled.
    pub disputed_minor: i64,
    /// Sum at risk ceded to the reinsurer on the date.
    pub ceded_sum_at_risk_minor: i64,
    /// The treaties with the reinsurer on a panel, by which its balance is asked (F2-18-6).
    #[serde(default)]
    pub treaties: std::collections::BTreeSet<TreatyId>,
}

/// One scheme's sums at risk, and how they fall across its sites.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemeConcentration {
    pub policy: PolicyId,
    pub gross_minor: i64,
    pub retained_minor: i64,
    pub ceded_minor: i64,
    pub unplaced_minor: i64,
    /// Site, headcount, and the retained sum at risk attributed to the site
    /// by headcount.
    pub sites: Vec<(String, u32, i64)>,
}

impl Reinsurance {
    /// The exposure to every reinsurer on a date.
    pub fn exposure(&self, on: NaiveDate) -> BTreeMap<ReinsurerId, Exposure> {
        let mut out: BTreeMap<ReinsurerId, Exposure> = BTreeMap::new();
        for (bucket, amount) in &self.pending {
            out.entry(bucket.reinsurer.clone()).or_default().balance_minor += bucket.kind.sign() * amount;
        }
        for s in self
            .statements
            .iter()
            .filter(|s| !matches!(s.status, crate::account::StatementStatus::Settled { .. }))
        {
            out.entry(s.reinsurer.clone()).or_default().balance_minor += s.net_minor;
        }
        for (reinsurer, open) in self.open_recoverables() {
            out.entry(reinsurer).or_default().open_recoverables_minor += open;
        }
        for item in self.disputes.values().filter(|d| d.resolution.is_none()) {
            out.entry(item.reinsurer.clone()).or_default().disputed_minor += item.amount_minor;
        }
        for book in self.lives.values() {
            for c in book.recorded.values().filter_map(|segments| cession_on(segments, on)) {
                for (reinsurer, amount) in &c.ceded {
                    out.entry(reinsurer.clone()).or_default().ceded_sum_at_risk_minor += amount;
                }
                for f in &c.facultative {
                    out.entry(f.reinsurer.clone()).or_default().ceded_sum_at_risk_minor += f.amount_minor;
                }
            }
        }
        for (reinsurer, exposure) in &mut out {
            exposure.treaties = self.treaties_of(reinsurer);
        }
        out
    }

    /// The treaties with this reinsurer on any version's panel.
    pub fn treaties_of(&self, reinsurer: &ReinsurerId) -> std::collections::BTreeSet<TreatyId> {
        self.treaties
            .iter()
            .filter(|(_, versions)| versions.values().any(|t| t.panel.iter().any(|p| &p.reinsurer == reinsurer)))
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Whether this reinsurer is on any version's panel of this treaty.
    pub fn on_panel(&self, treaty: &TreatyId, reinsurer: &ReinsurerId) -> bool {
        self.treaties
            .get(treaty)
            .is_some_and(|versions| versions.values().any(|t| t.panel.iter().any(|p| &p.reinsurer == reinsurer)))
    }

    /// Sums at risk on each scheme with terms on record, on a date.
    pub fn scheme_concentration(&self, on: NaiveDate) -> Vec<SchemeConcentration> {
        self.schemes
            .iter()
            .map(|(policy, terms)| {
                let mut c = SchemeConcentration {
                    policy: policy.clone(),
                    gross_minor: 0,
                    retained_minor: 0,
                    ceded_minor: 0,
                    unplaced_minor: 0,
                    sites: vec![],
                };
                for cession in self.policy_risks(policy).iter().filter_map(|k| self.cession(k, on)) {
                    c.gross_minor += cession.sum_at_risk_minor;
                    c.retained_minor += cession.retained_minor;
                    c.ceded_minor += cession.treaty_ceded() + cession.facultative_ceded();
                    c.unplaced_minor += cession.unplaced_minor;
                }
                let weights: Vec<i64> = terms.sites.iter().map(|(_, n)| *n as i64).collect();
                let kept = crate::math::split_exact(c.retained_minor + c.unplaced_minor, &weights);
                c.sites = terms.sites.iter().zip(kept).map(|((site, n), v)| (site.clone(), *n, v)).collect();
                c
            })
            .collect()
    }

    /// What the insurer keeps on a life on a date, across every policy.
    pub fn retained_on(&self, life: &LifeKey, on: NaiveDate) -> i64 {
        self.lives
            .get(life)
            .into_iter()
            .flat_map(|b| b.recorded.values())
            .filter_map(|segments| cession_on(segments, on))
            .map(|c| c.retained_minor + c.unplaced_minor)
            .sum()
    }

    /// The close check finance asks for: what stands between this module
    /// and a closed period ending on `period_end`. Nothing means green.
    ///
    /// A treaty owes a statement to `period_end` when its statement period
    /// ends then, by the calendar: monthly at every month end, quarterly at
    /// the end of March, June, September and December, yearly at the end of
    /// December. Whether the reinsurer has agreed or been paid is not
    /// checked: that waits on them, and the period does not.
    pub fn sweep_check(&self, period_end: NaiveDate) -> Vec<serde_json::Value> {
        use chrono::Datelike;
        let mut items = Vec::new();
        let due = |treaty: &TreatyId| {
            self.latest_treaty(treaty).is_some_and(|t| match t.statement.period {
                crate::treaty::StatementPeriod::Monthly => true,
                crate::treaty::StatementPeriod::Quarterly => period_end.month().is_multiple_of(3),
                crate::treaty::StatementPeriod::Yearly => period_end.month() == 12,
            })
        };
        let mut uncut: BTreeMap<(&TreatyId, &ReinsurerId), i64> = BTreeMap::new();
        for (bucket, amount) in self.pending.iter().filter(|(b, _)| b.value_date <= period_end) {
            *uncut.entry((&bucket.treaty, &bucket.reinsurer)).or_default() += bucket.kind.sign() * amount;
        }
        for ((treaty, reinsurer), net) in uncut {
            let cut = self
                .statements
                .iter()
                .any(|s| &s.treaty == treaty && &s.reinsurer == reinsurer && s.period_end >= period_end);
            if due(treaty) && !cut {
                items.push(serde_json::json!({"kind": "statement_not_cut", "treaty": treaty, "reinsurer": reinsurer, "net_minor": net}));
            }
        }
        for s in self.statements.iter().filter(|s| s.period_end <= period_end && s.supplementary_due) {
            items.push(serde_json::json!({"kind": "supplementary_listing_due", "statement": s.number, "treaty": s.treaty, "reinsurer": s.reinsurer}));
        }
        // Until the core has every entry, its clearing does not net to the statements.
        for (key, item) in self.exceptions.iter().filter(|(_, e)| e.code == "instruction_refused") {
            items.push(serde_json::json!({"kind": "instruction_refused", "exception": key, "detail": item.detail}));
        }
        if !self.outbox.is_empty() {
            items.push(serde_json::json!({"kind": "instructions_not_delivered", "count": self.outbox.len()}));
        }
        for detail in self.reconcile() {
            items.push(serde_json::json!({"kind": "does_not_reconcile", "detail": detail}));
        }
        items
    }

    /// The module's own invariants. An empty answer means it reconciles.
    pub fn reconcile(&self) -> Vec<String> {
        let mut breaks = Vec::new();
        // The cession record is exactly what the calculation gives today.
        for (life, book) in &self.lives {
            let real = book.risks.values().filter(|r| r.source != Source::Reservation);
            let rebuilt = timeline(real, |t| self.treaty(t), matches!(life, LifeKey::Party(_)));
            if rebuilt != book.recorded {
                breaks.push(format!("{life}: the cession record differs from a rebuild"));
            }
            for (key, segments) in &book.recorded {
                for (from, c) in segments.iter().filter_map(|(d, c)| c.as_ref().map(|c| (d, c))) {
                    if !c.balanced() {
                        breaks.push(format!("{key} from {from}: the parts of the cession do not add up"));
                    }
                }
                let unplaced = segments.last().and_then(|(_, c)| c.as_ref()).is_some_and(|c| c.unplaced_minor > 0);
                if unplaced != self.exceptions.contains_key(&format!("over_retention:{key}")) {
                    breaks.push(format!("{key}: cover over the retention and its exception item disagree"));
                }
            }
        }
        // Every premium on a reinsurer's account is a charge on a period.
        let mut charged: BTreeMap<(TreatyId, ReinsurerId, LineKind), i64> = BTreeMap::new();
        for period in self.periods.values() {
            for (k, amount) in &period.charged {
                let kind = match k.kind {
                    ChargeKind::Premium => LineKind::CededPremium,
                    ChargeKind::Commission => LineKind::Commission,
                    ChargeKind::Facultative => LineKind::FacultativePremium,
                };
                *charged.entry((k.treaty.clone(), k.reinsurer.clone(), kind)).or_default() += amount;
            }
        }
        for kind in [LineKind::CededPremium, LineKind::Commission, LineKind::FacultativePremium] {
            let booked: BTreeMap<_, _> = self.booked.iter().filter(|((_, _, k), v)| *k == kind && **v != 0).collect();
            let expected: BTreeMap<_, _> = charged.iter().filter(|((_, _, k), v)| *k == kind && **v != 0).collect();
            if booked != expected {
                breaks.push(format!("{kind:?}: what is booked to reinsurers differs from what is charged on periods"));
            }
        }
        // Recoveries billed are the sum over claims.
        let mut due: BTreeMap<(TreatyId, ReinsurerId), i64> = BTreeMap::new();
        for claim in self.claims.values() {
            for (k, v) in &claim.due {
                *due.entry(k.clone()).or_default() += v;
            }
        }
        due.retain(|_, v| *v != 0);
        let booked: BTreeMap<(TreatyId, ReinsurerId), i64> = self
            .booked
            .iter()
            .filter(|((_, _, k), v)| *k == LineKind::RecoveryDue && **v != 0)
            .map(|((t, r, _), v)| ((t.clone(), r.clone()), *v))
            .collect();
        if due != booked {
            breaks.push("recoveries billed to reinsurers differ from the sum over claims".to_string());
        }
        breaks
    }

    /// Migration: compare the retention the legacy system held per life with
    /// the rebuilt accumulation. Returns the lives that differ, with the
    /// legacy figure and ours.
    pub fn compare_retention(&self, legacy: &[(PartyId, i64)], on: NaiveDate) -> Vec<(PartyId, i64, i64)> {
        legacy
            .iter()
            .map(|(party, theirs)| (party.clone(), *theirs, self.retained_on(&LifeKey::Party(party.clone()), on)))
            .filter(|(_, theirs, ours)| theirs != ours)
            .collect()
    }

    /// What a proposed version of a treaty would change if the cover now on
    /// the treaty were moved to it: every cession that would differ on the
    /// date, as it is and as it would be.
    pub fn compare_treaty(&self, candidate: &Treaty, on: NaiveDate) -> Vec<(RiskKey, Option<Cession>, Option<Cession>)> {
        let mut out = Vec::new();
        for (life, book) in &self.lives {
            if !book.risks.values().any(|r| r.treaty_on(on).is_some_and(|t| t.id == candidate.id)) {
                continue;
            }
            let real = book.risks.values().filter(|r| r.source != Source::Reservation);
            let lookup = |t: &crate::TreatyRef| if t.id == candidate.id { Some(candidate) } else { self.treaty(t) };
            let proposed = timeline(real, lookup, matches!(life, LifeKey::Party(_)));
            for (key, segments) in proposed {
                let now = book.recorded.get(&key).and_then(|s| cession_on(s, on)).cloned();
                let then = cession_on(&segments, on).cloned();
                if now != then {
                    out.push((key, now, then));
                }
            }
        }
        out
    }
}
