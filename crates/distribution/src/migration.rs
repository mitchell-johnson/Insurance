//! Policies loaded from another system.
//!
//! The core issues a migrated policy and says so at once (`PolicyMigrated`).
//! It is not a new sale: its initial commission was paid by the old system,
//! so its layers earn trail only and no clawback clock starts at the
//! cutover. The old system knows a policy by a single agent code, so the
//! migration load maps each code to an engagement and a book once
//! (`MapLegacyAgent`, a change set) and sends `MigrateCommission` per
//! policy: the code, the commission option, each cover's annual premium and
//! what each payee was last paid on the policy for a year. The module writes
//! the layers from the code (writing, servicing and the book's entitlement,
//! with that assumption recorded) and checks its own renewal commission on
//! the premium against what the old system last paid. A difference beyond
//! the tolerance raises a task; the record stays for the reconciliation.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_core::{PartyId, PolicyId, Reject};
use serde::{Deserialize, Serialize};

use crate::rules::WHOLE;
use crate::state::{reject, Ctx, DistributionState, WriteLayer};
use crate::{BookId, EngagementId};

/// Where an old system's agent code now points.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyAgent {
    pub engagement: EngagementId,
    pub book: BookId,
}

/// A migrated policy's commission, as the migration load sends it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrateCommission {
    pub policy: PolicyId,
    pub agent_code: String,
    pub option: String,
    /// Each cover's annual premium, by cover code.
    pub premiums: BTreeMap<String, i64>,
    /// What the old system last paid each payee on this policy, for a year.
    #[serde(default)]
    pub last_paid: BTreeMap<PartyId, i64>,
}

/// One payee's year of commission: the old system's and this module's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayeeCheck {
    pub payee: PartyId,
    pub legacy_minor: i64,
    pub computed_minor: i64,
}

/// What was done for a migrated policy, kept for the reconciliation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationRecord {
    pub cutover: NaiveDate,
    pub agent_code: String,
    pub engagement: EngagementId,
    pub book: BookId,
    pub assumptions: Vec<String>,
    pub checks: Vec<PayeeCheck>,
    /// Every payee within the tolerance.
    pub agrees: bool,
}

/// How far a payee's year may differ from what the old system paid: the
/// larger of one dollar and one per cent. Rounding in the old system and
/// rate changes inside a year make an exact match the exception.
fn tolerance(legacy: i64) -> i64 {
    (legacy.abs() / 100).max(100)
}

impl DistributionState {
    pub(crate) fn policy_migrated(&mut self, policy: &PolicyId, cutover: NaiveDate) {
        let Some(record) = self.policies.get_mut(policy) else { return };
        record.migrated = Some(cutover);
        // Layers written before the core said so (new business advice that
        // was waiting) earn trail only too.
        for key in record.layers.clone() {
            if let Some(t) = self.layers.get_mut(&key).filter(|l| l.initial_paid.is_none()).and_then(|l| l.terms.as_mut()) {
                t.rates.initial_ppm = 0;
            }
        }
    }

    pub(crate) fn migrate_commission(&mut self, ctx: &mut Ctx, m: MigrateCommission) -> Result<(), Reject> {
        let record = self
            .policies
            .get(&m.policy)
            .cloned()
            .ok_or_else(|| Reject::new("unknown_policy", format!("policy {} is not known to distribution", m.policy)))?;
        let Some(cutover) = record.migrated else {
            return reject("not_migrated", format!("policy {} was not loaded from another system", m.policy));
        };
        if m.option.trim().is_empty() || m.premiums.values().any(|p| *p <= 0) || m.last_paid.values().any(|p| *p < 0) {
            return reject(
                "invalid_migration",
                "a migrated policy names its option, positive premiums and what was last paid",
            );
        }
        let agent = self
            .config
            .legacy_agents
            .get(&m.agent_code)
            .cloned()
            .ok_or_else(|| Reject::new("unknown_agent_code", format!("agent code {} is not mapped to an engagement", m.agent_code)))?;
        let schedules = &self.config.product_schedules;
        let mut writes = vec![];
        let mut net: BTreeMap<String, i64> = BTreeMap::new();
        for key in &record.layers {
            let Some(layer) = self.layers.get(key) else { continue };
            if layer.ended.is_some() || layer.indexation {
                continue;
            }
            let premium = *m
                .premiums
                .get(&layer.cover)
                .ok_or_else(|| Reject::new("no_premium", format!("no premium was given for cover {}", layer.cover)))?;
            *net.entry(layer.cover_type.clone()).or_insert(0) += premium;
            if !layer.writing.is_empty() {
                continue;
            }
            let schedule = schedules
                .get(&crate::newbusiness::schedule_key(&record.product_code, Some(&layer.cover_type)))
                .or_else(|| schedules.get(&record.product_code))
                .cloned()
                .ok_or_else(|| Reject::new("no_schedule", format!("no commission schedule is named for {}", record.product_code)))?;
            writes.push(WriteLayer {
                policy: m.policy.clone(),
                cover: layer.cover.clone(),
                layer: layer.layer,
                writing: vec![(agent.engagement.clone(), WHOLE)],
                books: vec![(agent.book.clone(), WHOLE)],
                schedule,
                option: m.option.clone(),
                annual_premium_minor: premium,
                replacement: false,
            });
        }
        // Each write checks everything before it changes anything; one that
        // is refused leaves those before it written, and sending the command
        // again once the cause is fixed writes the rest.
        for w in writes {
            self.write_layer(ctx, w)?;
        }
        let today = self.today.unwrap_or(cutover);
        let computed = self.dry_run(&m.policy, today, &net);
        let payees: std::collections::BTreeSet<&PartyId> = computed.keys().chain(m.last_paid.keys()).collect();
        let checks: Vec<PayeeCheck> = payees
            .into_iter()
            .map(|p| PayeeCheck {
                payee: p.clone(),
                legacy_minor: m.last_paid.get(p).copied().unwrap_or(0),
                computed_minor: computed.get(p).copied().unwrap_or(0),
            })
            .collect();
        let agrees = checks.iter().all(|c| (c.computed_minor - c.legacy_minor).abs() <= tolerance(c.legacy_minor));
        if !agrees {
            let detail = checks
                .iter()
                .filter(|c| (c.computed_minor - c.legacy_minor).abs() > tolerance(c.legacy_minor))
                .map(|c| format!("{}: last paid {} a year, now {}", c.payee, c.legacy_minor, c.computed_minor))
                .collect::<Vec<_>>()
                .join("; ");
            self.task(ctx, "migration_commission_mismatch", &m.policy.0, today, detail);
        }
        let assumptions = vec![format!(
            "the old system's single agent code {} is taken as the writing and servicing adviser (engagement {}) and the book {} for every cover",
            m.agent_code, agent.engagement, agent.book
        )];
        self.migrations.insert(
            m.policy.clone(),
            MigrationRecord {
                cutover,
                agent_code: m.agent_code,
                engagement: agent.engagement,
                book: agent.book,
                assumptions,
                checks,
                agrees,
            },
        );
        Ok(())
    }
}
