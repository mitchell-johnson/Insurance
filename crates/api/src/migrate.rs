//! Loading a cohort of policies from another system: `ic-admin migrate`.
//!
//! The mapping code that reads the old system writes one cohort file: the
//! cohort's name, currency and cutover day, the old system's control
//! totals, and for every policy the ordinary issue command, its opening
//! position, and the old system's own figures for it. This tool does the
//! rest through the core's migration commands and nothing else:
//!
//! - `migrate load <cohort.json>` checks the file adds up to its own
//!   control totals, opens the cohort on each partition that has policies
//!   in it (with that partition's share of the totals), loads every policy,
//!   and reports what was refused by reason and what does not reconcile.
//!   A refused policy is an exception for a person; the rest still load, so
//!   a trial load in a throwaway environment yields the whole count.
//! - `migrate status <cohort>` shows each partition's loaded figures beside
//!   its control totals.
//! - `migrate sign-off <cohort> <finance> <business-owner> <evidence>
//!   [actuary]` signs every partition off, only if every one reconciles,
//!   and then releases the policies.
//! - `migrate abandon <cohort>` takes the load out again, before sign-off.
//! - `migrate commission <cohort.json>`, once the cohort is released, sends
//!   each policy's commission book (the policy's `commission` in the file:
//!   agent code, option, annual premiums by cover and what the old system
//!   last paid each payee) to the commission module as
//!   `migrate_commission`, and reports what it refused.
//!
//! Every step is safe to repeat: commands carry keys made from the cohort
//! and the policy.

use sha2::Digest;
use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_core::command::IssuePolicy;
use ic_core::migration::{Cohort, CohortStatus, ControlTotals, Load, MigrationCommand, Opening, SignOff, MAX_COHORT_STEP};
use ic_core::{home_partition, Command};
use ic_ledger::PartitionId;
use ic_money::Currency;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::admin::Admin;

/// The old system's own figures for one policy on the cutover day.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyFigures {
    pub sum_insured_minor: i64,
    pub period_premium_minor: i64,
    #[serde(default)]
    pub receivable_minor: i64,
    #[serde(default)]
    pub unearned_minor: i64,
    #[serde(default)]
    pub suspense_minor: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CohortPolicy {
    pub issue: IssuePolicy,
    pub opening: Opening,
    pub legacy: LegacyFigures,
    /// The commission module's `migrate_commission` for the policy, where
    /// an adviser was paid on it: `agent_code`, `option`, `premiums` and
    /// `last_paid`. The policy is filled in from `issue`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commission: Option<Value>,
}

impl CohortPolicy {
    fn figures(&self) -> ControlTotals {
        ControlTotals {
            policies: 1,
            lives: self.issue.lives.len() as u32,
            layers: self.issue.covers.iter().filter(|c| c.life.is_some()).count() as u32,
            sum_insured_minor: self.legacy.sum_insured_minor,
            period_premium_minor: self.legacy.period_premium_minor,
            receivable_minor: self.legacy.receivable_minor,
            unearned_minor: self.legacy.unearned_minor,
            suspense_minor: self.legacy.suspense_minor,
        }
    }
}

fn sum<'a>(policies: impl Iterator<Item = &'a CohortPolicy>) -> ControlTotals {
    let mut total = ControlTotals::default();
    for p in policies {
        let f = p.figures();
        total.policies += f.policies;
        total.lives += f.lives;
        total.layers += f.layers;
        total.sum_insured_minor += f.sum_insured_minor;
        total.period_premium_minor += f.period_premium_minor;
        total.receivable_minor += f.receivable_minor;
        total.unearned_minor += f.unearned_minor;
        total.suspense_minor += f.suspense_minor;
    }
    total
}

fn listed(differences: &[(&'static str, i64, i64)], left: &str, right: &str) -> Vec<Value> {
    differences.iter().map(|(what, a, b)| json!({ "figure": what, left: a, right: b })).collect()
}

/// One cohort, as the mapping code wrote it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CohortFile {
    pub cohort: String,
    pub currency: Currency,
    pub cutover: NaiveDate,
    /// The old system's totals for the cohort, taken independently of the
    /// policy rows.
    pub control: ControlTotals,
    pub policies: Vec<CohortPolicy>,
}

impl CohortFile {
    /// Read a cohort file. It is refused unless every policy is marked as
    /// migrated, none is listed twice, and the policy rows add up to the
    /// control totals: a file that disagrees with itself is not loaded.
    pub fn parse(text: &str) -> Result<Self, String> {
        let file: CohortFile = serde_json::from_str(text).map_err(|e| format!("cohort file: {e}"))?;
        let mut seen = std::collections::BTreeSet::new();
        for p in &file.policies {
            if !p.issue.migrated {
                return Err(format!("cohort file: policy {} is not marked as migrated", p.issue.policy));
            }
            if !seen.insert(&p.issue.policy) {
                return Err(format!("cohort file: policy {} is listed twice", p.issue.policy));
            }
        }
        let differences = sum(file.policies.iter()).differences(&file.control);
        if !differences.is_empty() {
            return Err(format!(
                "cohort file: the policy rows do not add up to the control totals: {}",
                Value::from(listed(&differences, "rows", "control"))
            ));
        }
        Ok(file)
    }

    /// The file's policies by the partition each one's group lives on.
    fn split(&self, partitions: PartitionId) -> BTreeMap<PartitionId, Vec<&CohortPolicy>> {
        let mut out: BTreeMap<PartitionId, Vec<&CohortPolicy>> = BTreeMap::new();
        for p in &self.policies {
            out.entry(home_partition(&p.issue.group) % partitions).or_default().push(p);
        }
        out
    }
}

/// What `ic-admin migrate` was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrateCommand {
    Load { file: std::path::PathBuf },
    Status { cohort: String },
    SignOff { cohort: String, sign_off: SignOff },
    Abandon { cohort: String },
    Commission { file: std::path::PathBuf },
}

impl MigrateCommand {
    /// Read the words after `migrate`.
    pub fn parse(args: &[&str]) -> Option<Self> {
        match args {
            ["load", file] => Some(Self::Load { file: file.into() }),
            ["status", cohort] => Some(Self::Status { cohort: cohort.to_string() }),
            ["abandon", cohort] => Some(Self::Abandon { cohort: cohort.to_string() }),
            ["commission", file] => Some(Self::Commission { file: file.into() }),
            ["sign-off", cohort, finance, owner, evidence, rest @ ..] if rest.len() <= 1 => Some(Self::SignOff {
                cohort: cohort.to_string(),
                sign_off: SignOff {
                    finance: finance.to_string(),
                    business_owner: owner.to_string(),
                    actuary: rest.first().map(|a| a.to_string()),
                    evidence: evidence.to_string(),
                },
            }),
            _ => None,
        }
    }
}

impl Admin {
    async fn migration(&self, partition: PartitionId, key: String, step: MigrationCommand) -> Result<ic_raft::Reply, String> {
        self.propose(partition, key, "migration", Command::Migration(Box::new(step))).await
    }

    /// Each partition's record of a cohort, where it has one.
    async fn cohorts(&self, cohort: &str) -> Result<BTreeMap<PartitionId, Cohort>, String> {
        let mut out = BTreeMap::new();
        for partition in 0..self.partitions().await? {
            if let Some(found) = self.cohort_on(partition, cohort).await? {
                out.insert(partition, found);
            }
        }
        Ok(out)
    }

    /// Load a cohort file: open, load every policy, and report.
    pub async fn migrate_load(&self, file: &CohortFile) -> Result<Value, String> {
        let partitions = self.partitions().await?;
        let name = &file.cohort;
        let mut refused: BTreeMap<String, u64> = BTreeMap::new();
        let mut exceptions = Vec::new();
        let mut loaded = 0u64;
        for (partition, policies) in file.split(partitions) {
            let open = MigrationCommand::OpenCohort {
                cohort: name.clone(),
                currency: file.currency,
                cutover: file.cutover,
                control: sum(policies.iter().copied()),
            };
            let reply = self.migration(partition, format!("migration:{name}:open:p{partition}"), open).await?;
            reply
                .rejected
                .map_or(Ok(()), |refusal| Err(format!("partition {partition} did not open cohort {name}: {refusal}")))?;
            for p in policies {
                let load = MigrationCommand::Load(Box::new(Load {
                    cohort: name.clone(),
                    issue: p.issue.clone(),
                    opening: p.opening.clone(),
                }));
                let reply = self.migration(partition, format!("migration:{name}:load:{}", p.issue.policy), load).await?;
                match reply.rejected {
                    None => loaded += 1,
                    Some(refusal) => {
                        *refused.entry(refusal.code.clone()).or_insert(0) += 1;
                        // Enough to start work on; the counts are complete.
                        if exceptions.len() < 200 {
                            exceptions.push(json!({ "policy": p.issue.policy, "code": refusal.code, "message": refusal.message }));
                        }
                    }
                }
            }
        }
        let status = self.migrate_status(name).await?;
        Ok(json!({
            "cohort": name,
            "policies": file.policies.len(),
            "loaded": loaded,
            "refused": refused,
            "exceptions": exceptions,
            "reconciled": status["reconciled"],
            "partitions": status["partitions"],
        }))
    }

    /// Each partition's loaded figures beside its control totals.
    pub async fn migrate_status(&self, cohort: &str) -> Result<Value, String> {
        let found = self.cohorts(cohort).await?;
        if found.is_empty() {
            return Err(format!("no partition has a cohort called {cohort}"));
        }
        let mut reconciled = true;
        let mut rows = Vec::new();
        for (partition, c) in &found {
            let differences = c.loaded.differences(&c.control);
            let taken_out = matches!(c.status, CohortStatus::Abandoning | CohortStatus::Abandoned);
            reconciled &= differences.is_empty() && !taken_out;
            rows.push(json!({
                "partition": partition,
                "status": c.status,
                "cutover": c.cutover,
                "loaded": c.loaded,
                "control": c.control,
                "waiting": c.waiting.len(),
                "differences": listed(&differences, "loaded", "control"),
            }));
        }
        Ok(json!({ "cohort": cohort, "reconciled": reconciled, "partitions": rows }))
    }

    /// Sign the cohort off on every partition and release its policies.
    /// Nothing is signed unless every partition reconciles.
    pub async fn migrate_sign_off(&self, cohort: &str, sign_off: &SignOff) -> Result<Value, String> {
        let status = self.migrate_status(cohort).await?;
        if status["reconciled"] != json!(true) {
            return Err(format!(
                "cohort {cohort} does not reconcile on every partition; nothing was signed: {}",
                status["partitions"]
            ));
        }
        let partitions: Vec<PartitionId> = self.cohorts(cohort).await?.into_keys().collect();
        // The key names who signed, so a sign-off refused for its signers can be made
        // again correctly, and the same sign-off sent twice is still one.
        let signers = serde_json::to_vec(sign_off).map_err(|e| e.to_string())?;
        let signed: String = sha2::Sha256::digest(&signers).iter().take(8).map(|b| format!("{b:02x}")).collect();
        for partition in &partitions {
            let step = MigrationCommand::SignOff {
                cohort: cohort.to_string(),
                sign_off: sign_off.clone(),
            };
            let reply = self
                .migration(*partition, format!("migration:{cohort}:sign-off:{signed}:p{partition}"), step)
                .await?;
            reply.rejected.map_or(Ok(()), |refusal| {
                Err(format!("partition {partition} refused the sign-off of cohort {cohort}: {refusal}"))
            })?;
        }
        let mut released = 0u64;
        for partition in &partitions {
            let mut round = 0u64;
            loop {
                let Some(c) = self.cohort_on(*partition, cohort).await? else { break };
                if c.status != CohortStatus::SignedOff {
                    break;
                }
                let waiting = c.waiting.len() as u64;
                let step = MigrationCommand::Release {
                    cohort: cohort.to_string(),
                    limit: MAX_COHORT_STEP,
                };
                // The key names what is left, so a repeated run sends the same
                // step again and a later step is a new one.
                let reply = self
                    .migration(*partition, format!("migration:{cohort}:release:p{partition}:{waiting}:{round}"), step)
                    .await?;
                reply.rejected.map_or(Ok(()), |refusal| {
                    Err(format!("partition {partition} refused to release cohort {cohort}: {refusal}"))
                })?;
                released += waiting.min(MAX_COHORT_STEP as u64);
                round += 1;
            }
        }
        Ok(json!({ "cohort": cohort, "signed_off": partitions, "released": released }))
    }

    /// Send each released policy's commission book to the commission
    /// module. Nothing is sent until the cohort is released everywhere, so
    /// the module already knows each policy as migrated. The module's key
    /// makes a repeat safe, and it skips layers it has already written.
    pub async fn migrate_commission(&self, file: &CohortFile) -> Result<Value, String> {
        let name = &file.cohort;
        let found = self.cohorts(name).await?;
        if found.is_empty() || found.values().any(|c| c.status != CohortStatus::Released) {
            return Err(format!("cohort {name} is not released on every partition; no commission was sent"));
        }
        let (mut sent, mut refused) = (0u64, Vec::new());
        for p in file.policies.iter() {
            let Some(body) = &p.commission else { continue };
            let mut body = body.clone();
            let Some(fields) = body.as_object_mut() else {
                return Err(format!("policy {}: its commission is not an object", p.issue.policy));
            };
            fields.insert("policy".to_string(), json!(p.issue.policy));
            let envelope = json!({
                "key": format!("migration:{name}:commission:{}", p.issue.policy),
                "caller": "migration",
                "command": { "migrate_commission": body },
            });
            let reply = self
                .client
                .propose_module_until("distribution", &envelope, self.patience)
                .await
                .map_err(|e| format!("policy {}: distribution: {e}", p.issue.policy))?;
            match crate::admin::module_refusal(&reply.result) {
                None => sent += 1,
                Some(why) => refused.push(json!({ "policy": p.issue.policy, "refusal": why })),
            }
        }
        Ok(json!({ "cohort": name, "sent": sent, "refused": refused }))
    }

    /// Take a cohort's load out again on every partition.
    pub async fn migrate_abandon(&self, cohort: &str) -> Result<Value, String> {
        let found = self.cohorts(cohort).await?;
        if found.is_empty() {
            return Err(format!("no partition has a cohort called {cohort}"));
        }
        for partition in found.keys() {
            let mut round = 0u64;
            loop {
                let Some(c) = self.cohort_on(*partition, cohort).await? else { break };
                if c.status == CohortStatus::Abandoned {
                    break;
                }
                let step = MigrationCommand::Abandon {
                    cohort: cohort.to_string(),
                    limit: MAX_COHORT_STEP,
                };
                let key = format!("migration:{cohort}:abandon:p{partition}:{}:{round}", c.waiting.len());
                let reply = self.migration(*partition, key, step).await?;
                reply.rejected.map_or(Ok(()), |refusal| {
                    Err(format!("partition {partition} refused to abandon cohort {cohort}: {refusal}"))
                })?;
                round += 1;
            }
        }
        Ok(json!({ "cohort": cohort, "abandoned": found.keys().collect::<Vec<_>>() }))
    }

    pub async fn migrate(&self, command: MigrateCommand) -> Result<Value, String> {
        match command {
            MigrateCommand::Load { file } => {
                let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
                self.migrate_load(&CohortFile::parse(&text)?).await
            }
            MigrateCommand::Status { cohort } => self.migrate_status(&cohort).await,
            MigrateCommand::SignOff { cohort, sign_off } => self.migrate_sign_off(&cohort, &sign_off).await,
            MigrateCommand::Abandon { cohort } => self.migrate_abandon(&cohort).await,
            MigrateCommand::Commission { file } => {
                let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
                self.migrate_commission(&CohortFile::parse(&text)?).await
            }
        }
    }
}
