//! Policy transfer between groups.
//!
//! The partition key is the policyholder's group, so a policy whose owner
//! changes moves between groups and often between partitions:
//!
//! 1. **Freeze**: the source marks the policy as transferring. New commands
//!    are refused until it lands or the transfer is aborted.
//! 2. **Record**: the source writes a transfer record holding the policy's
//!    full state and its balance in every account, with a hash.
//! 3. **Move**: the balances leave through transfer in-transit accounts, one
//!    per unit type, and the record goes through the outbox.
//! 4. **Land**: the target creates the policy from the record, posts the
//!    opening balances from its own in-transit accounts and acknowledges.
//! 5. **Close**: the source marks the policy transferred.
//!
//! If the target rejects the record, the source reverses its in-transit entry
//! and unfreezes: nothing has moved. Every step is idempotent, so a message
//! delivered twice or late changes nothing.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_ledger::{AccountUnit, Dimensions, Ledger, PartitionId};
use ic_product::RateTable;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::accounts;
use crate::catalog::Catalog;
use crate::event::{Event, MessageBody, OutboundMessage};
use crate::lifecycle::{layer_infos, life_infos};
use crate::machine::{CoreState, Ctx, Reject};
use crate::policy::{Policy, PolicyStatus, TransferOut};
use crate::postings::PostingSet;
use crate::GroupId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferBalance {
    pub role: String,
    pub unit: AccountUnit,
    /// Signed balance, debits positive, in the account's unit.
    pub balance: i128,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferRecord {
    pub transfer_id: String,
    pub source_partition: PartitionId,
    pub source_business_date: NaiveDate,
    pub target_group: GroupId,
    pub target_partition: PartitionId,
    /// The policy's full state: layers, terms, receivables, standing.
    pub policy: Policy,
    pub balances: Vec<TransferBalance>,
    /// Scoped configuration travels with the policy.
    pub scoped_tables: Vec<RateTable>,
    pub hash: String,
}

impl TransferRecord {
    fn compute_hash(&self) -> String {
        let mut h = Sha256::new();
        h.update(b"ic.transfer.v1");
        let body = (
            &self.transfer_id,
            self.source_partition,
            self.source_business_date,
            &self.target_group,
            self.target_partition,
            &self.policy,
            &self.balances,
            &self.scoped_tables,
        );
        match serde_json::to_vec(&body) {
            Ok(b) => h.update(b),
            Err(_) => h.update(b"unencodable"),
        }
        hex::encode(h.finalize())
    }
}

pub(crate) fn begin(
    ledger: &mut Ledger,
    catalog: &Catalog,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    transfer_id: String,
    target_group: GroupId,
    target_partition: PartitionId,
) -> Result<(), Reject> {
    if !matches!(policy.status, PolicyStatus::InForce | PolicyStatus::Lapsed) {
        return Err(Reject::new("not_transferable", "only a policy in force or lapsed can be transferred"));
    }
    if policy.pending_invoice.is_some() {
        return Err(Reject::new("invoice_in_progress", "a scheme invoice is being built; retry shortly"));
    }
    if target_group == policy.group {
        return Err(Reject::new("same_group", "the policy is already in that group"));
    }
    let mut set = PostingSet::new();
    let mut balances = Vec::new();
    for account in accounts::policy_accounts(&policy.id, policy.currency) {
        let balance = ledger.balance(&account.id);
        if ledger.available(&account.id) != ledger.natural_balance(&account.id) {
            return Err(Reject::new("holds_outstanding", "the policy has holds; release them before transferring"));
        }
        if balance != 0 {
            set.credit(account.clone(), balance, Dimensions::default());
            set.debit(accounts::transfer_in_transit(&account.unit), balance, Dimensions::default());
            balances.push(TransferBalance {
                role: account.role.clone(),
                unit: account.unit.clone(),
                balance,
            });
        }
    }
    let postings = set.into_postings(ledger)?;
    let mut detail = BTreeMap::new();
    detail.insert("transfer".to_string(), transfer_id.clone());
    if !postings.is_empty() {
        ctx.post(ledger, "transfer_out", Some(policy), ctx.date, postings.clone(), detail)?;
    }
    let mut record = TransferRecord {
        transfer_id: transfer_id.clone(),
        source_partition: ledger.partition(),
        source_business_date: ctx.date,
        target_group: target_group.clone(),
        target_partition,
        policy: policy.clone(),
        balances,
        scoped_tables: catalog.scoped_tables_for(&policy.group),
        hash: String::new(),
    };
    record.hash = record.compute_hash();
    policy.transfer_out = Some(TransferOut {
        transfer_id: transfer_id.clone(),
        target_group: target_group.clone(),
        target_partition,
        postings,
    });
    ctx.messages.push(OutboundMessage {
        id: format!("{transfer_id}:record"),
        to_partition: target_partition,
        body: MessageBody::TransferRecord(Box::new(record)),
    });
    ctx.emit(Event::TransferStarted {
        policy: policy.id.clone(),
        transfer_id,
        target_group,
        target_partition,
    });
    Ok(())
}

pub(crate) fn land(state: &mut CoreState, ctx: &mut Ctx<'_>, record: TransferRecord) -> Result<(), Reject> {
    if record.target_partition != state.partition {
        return Err(Reject::new("wrong_partition", "this transfer is addressed to another partition"));
    }
    if record.compute_hash() != record.hash {
        return Err(Reject::new("bad_record", "the transfer record does not match its hash"));
    }
    if state.transfers_seen.contains(&record.transfer_id) {
        // Delivered before. The first delivery's acknowledgement is already
        // in the outbox, so there is nothing more to do.
        return Ok(());
    }
    let id = record.policy.id.clone();
    let refuse = if state.policies.get(&id).is_some_and(|p| p.status != PolicyStatus::Transferred) {
        Some("a policy with this ID is already here")
    } else if state.catalog.product(&record.policy.product_hash).is_none() {
        Some("the policy's product version is not active on the target partition")
    } else {
        None
    };
    state.transfers_seen.insert(record.transfer_id.clone());
    if let Some(reason) = refuse {
        ctx.messages.push(OutboundMessage {
            id: format!("{}:rejected", record.transfer_id),
            to_partition: record.source_partition,
            body: MessageBody::TransferRejected {
                policy: id.clone(),
                transfer_id: record.transfer_id.clone(),
                reason: reason.to_string(),
            },
        });
        ctx.emit(Event::TransferRejected {
            policy: id,
            transfer_id: record.transfer_id,
            reason: reason.to_string(),
        });
        return Ok(());
    }
    let mut policy = record.policy;
    let old_group = std::mem::replace(&mut policy.group, record.target_group.clone());
    policy.history.push((record.source_partition, old_group));
    policy.transfer_out = None;
    for t in record.scoped_tables {
        // Re-scope the scheme's own tables to the group they now belong to.
        let mut t = t;
        if let Some(scope) = &mut t.scope {
            if !scope.contains(&record.target_group.0) {
                scope.push(record.target_group.0.clone());
            }
        }
        state.catalog.activate_rate_table(t);
    }
    let mut set = PostingSet::new();
    for account in accounts::policy_accounts(&policy.id, policy.currency) {
        state.ledger.open_account(account.clone())?;
        if let Some(b) = record.balances.iter().find(|b| b.role == account.role && b.unit == account.unit) {
            set.debit(account.clone(), b.balance, Dimensions::default());
            set.credit(accounts::transfer_in_transit(&account.unit), b.balance, Dimensions::default());
        }
    }
    let postings = set.into_postings(&mut state.ledger)?;
    let mut detail = BTreeMap::new();
    detail.insert("transfer".to_string(), record.transfer_id.clone());
    if !postings.is_empty() {
        ctx.post(&mut state.ledger, "transfer_in", Some(&policy), ctx.date, postings, detail)?;
    }
    ctx.emit(Event::TransferLanded {
        policy: id.clone(),
        transfer_id: record.transfer_id.clone(),
        group: record.target_group,
        from_partition: record.source_partition,
        lives: life_infos(&policy),
        layers: layer_infos(&policy),
        number: policy.number.clone(),
    });
    ctx.messages.push(OutboundMessage {
        id: format!("{}:landed", record.transfer_id),
        to_partition: record.source_partition,
        body: MessageBody::TransferLanded {
            policy: id.clone(),
            transfer_id: record.transfer_id,
        },
    });
    state.policies.insert(id, policy);
    Ok(())
}

pub(crate) fn complete(policy: &mut Policy, ctx: &mut Ctx<'_>, transfer_id: &str) -> Result<(), Reject> {
    match &policy.transfer_out {
        Some(t) if t.transfer_id == transfer_id => {}
        // Already closed, or an acknowledgement for a transfer that was aborted.
        _ => return Ok(()),
    }
    policy.transfer_out = None;
    // The policy now lives on the target. What stays here is a stub that
    // links to it; the history stays on this partition's chain.
    policy.receivables.clear();
    policy.receipts.clear();
    policy.lapse = None;
    policy.standing = None;
    ctx.emit(Event::PolicyStatusChanged {
        policy: policy.id.clone(),
        from: policy.status,
        to: PolicyStatus::Transferred,
    });
    policy.status = PolicyStatus::Transferred;
    ctx.emit(Event::TransferCompleted {
        policy: policy.id.clone(),
        transfer_id: transfer_id.to_string(),
    });
    Ok(())
}

pub(crate) fn abort(ledger: &mut Ledger, policy: &mut Policy, ctx: &mut Ctx<'_>, transfer_id: &str) -> Result<(), Reject> {
    let Some(t) = policy.transfer_out.clone().filter(|t| t.transfer_id == transfer_id) else {
        return Ok(());
    };
    if !t.postings.is_empty() {
        let reversal = t.postings.iter().map(ic_ledger::Posting::reversed).collect();
        let mut detail = BTreeMap::new();
        detail.insert("transfer".to_string(), transfer_id.to_string());
        ctx.post(ledger, "transfer_out_reversed", Some(policy), ctx.date, reversal, detail)?;
    }
    policy.transfer_out = None;
    ctx.emit(Event::TransferAborted {
        policy: policy.id.clone(),
        transfer_id: transfer_id.to_string(),
    });
    Ok(())
}
