//! Publishing the day's Merkle root.
//!
//! At end-of-day every partition's head hash feeds a Merkle tree. The root
//! for the business date is signed and published to write-once storage
//! outside the core. Altering any past entry then breaks the chain and the
//! published root, even for someone with full access to the ledger's disks.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_ledger::{merkle_root, Hash32, PartitionId};
use serde::{Deserialize, Serialize};

use crate::blob::WormStore;
use crate::keyvault::{RootSigner, Signature};
use crate::AzureError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedRoot {
    pub business_date: NaiveDate,
    pub root: Hash32,
    /// Each partition's head at the cutoff: enough to recompute the root.
    pub heads: BTreeMap<PartitionId, Hash32>,
    pub signature: Signature,
}

pub fn root_name(date: NaiveDate) -> String {
    format!("merkle-roots/{}/{}.json", date.format("%Y"), date)
}

fn signed_message(date: NaiveDate, root: Hash32) -> Vec<u8> {
    format!("ic.merkle.published.v1|{date}|{root}").into_bytes()
}

/// Sign and publish. Publishing the same day's root again is a no-op if it
/// is the same root, and an error if it differs: closed days never change.
pub async fn publish_root(
    signer: &impl RootSigner,
    store: &impl WormStore,
    date: NaiveDate,
    heads: &BTreeMap<PartitionId, Hash32>,
) -> Result<PublishedRoot, AzureError> {
    let root = merkle_root(date, heads);
    let name = root_name(date);
    if let Some(existing) = store.get(&name).await? {
        let published: PublishedRoot = serde_json::from_slice(&existing).map_err(|e| AzureError::Response(e.to_string()))?;
        return if published.root == root {
            Ok(published)
        } else {
            Err(AzureError::AlreadyExists(format!("{name} is already published with a different root")))
        };
    }
    let signature = signer.sign(&signed_message(date, root)).await?;
    let published = PublishedRoot {
        business_date: date,
        root,
        heads: heads.clone(),
        signature,
    };
    let body = serde_json::to_vec_pretty(&published).map_err(|e| AzureError::Response(e.to_string()))?;
    store.put_new(&name, "application/json", body).await?;
    Ok(published)
}

/// The independent verifier's check: the published heads give the published
/// root, and a partition's chain, replayed to the cutoff, gives its head.
pub fn verify_published(published: &PublishedRoot) -> bool {
    merkle_root(published.business_date, &published.heads) == published.root
}
