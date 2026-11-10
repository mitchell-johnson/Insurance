//! The ledger's cold tier.
//!
//! Journal entries are kept forever and policies last decades: about 15 TB
//! per replica over fifty years. Closed days older than a set age are sealed
//! into segments in write-once object storage, still covered by the hash
//! chain and the published Merkle roots, and fetched on demand for replay or
//! audit. On Azure the segments go to an immutable Blob Storage container
//! whose lifecycle policy moves them to the cool and then the cold access
//! tier; they stay online, so a segment is retrievable in milliseconds.
//!
//! A segment is a run of consecutive entries from one partition. Its
//! manifest records the chain hash before its first entry and after its
//! last, so segments link to each other exactly as entries do, and a segment
//! can be verified on its own.

use ic_ledger::{sha256, Hash32, JournalEntry, PartitionId};
use serde::{Deserialize, Serialize};

use crate::blob::WormStore;
use crate::AzureError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentManifest {
    pub partition: PartitionId,
    pub first_seq: u64,
    pub last_seq: u64,
    /// The chain head before the first entry of the segment.
    pub prev_hash: Hash32,
    /// The chain head after the last entry.
    pub last_hash: Hash32,
    pub first_business_date: chrono::NaiveDate,
    pub last_business_date: chrono::NaiveDate,
    /// SHA-256 of the segment body.
    pub content_hash: Hash32,
    pub entries: u64,
}

fn names(partition: PartitionId, first: u64, last: u64) -> (String, String) {
    let base = format!("ledger/p{partition:02}/{first:012}-{last:012}");
    (format!("{base}.jsonl"), format!("{base}.manifest.json"))
}

/// Seal a run of consecutive entries. The entries must be in order with no
/// gaps and their chain links intact.
pub async fn seal_segment(store: &impl WormStore, entries: &[JournalEntry]) -> Result<SegmentManifest, AzureError> {
    let (Some(first), Some(last)) = (entries.first(), entries.last()) else {
        return Err(AzureError::Response("an empty segment cannot be sealed".to_string()));
    };
    let mut body = Vec::new();
    let mut prev = first.prev_hash;
    for (i, e) in entries.iter().enumerate() {
        if e.id.partition != first.id.partition || e.id.seq != first.id.seq + i as u64 || e.prev_hash != prev {
            return Err(AzureError::Response(format!("entry {} does not follow its predecessor", e.id)));
        }
        prev = e.hash;
        serde_json::to_writer(&mut body, e).map_err(|e| AzureError::Response(e.to_string()))?;
        body.push(b'\n');
    }
    let manifest = SegmentManifest {
        partition: first.id.partition,
        first_seq: first.id.seq,
        last_seq: last.id.seq,
        prev_hash: first.prev_hash,
        last_hash: last.hash,
        first_business_date: first.business_date,
        last_business_date: last.business_date,
        content_hash: sha256(&[&body]),
        entries: entries.len() as u64,
    };
    let (segment, manifest_name) = names(manifest.partition, manifest.first_seq, manifest.last_seq);
    store.put_new(&segment, "application/x-ndjson", body).await?;
    store
        .put_new(
            &manifest_name,
            "application/json",
            serde_json::to_vec_pretty(&manifest).map_err(|e| AzureError::Response(e.to_string()))?,
        )
        .await?;
    Ok(manifest)
}

/// Fetch a sealed segment and check it against its manifest before use.
pub async fn fetch_segment(store: &impl WormStore, partition: PartitionId, first_seq: u64, last_seq: u64) -> Result<Vec<JournalEntry>, AzureError> {
    let (segment, manifest_name) = names(partition, first_seq, last_seq);
    let missing = || AzureError::Response(format!("segment {segment} is not in the cold tier"));
    let manifest: SegmentManifest =
        serde_json::from_slice(&store.get(&manifest_name).await?.ok_or_else(missing)?).map_err(|e| AzureError::Response(e.to_string()))?;
    let body = store.get(&segment).await?.ok_or_else(missing)?;
    if sha256(&[&body]) != manifest.content_hash {
        return Err(AzureError::Response(format!("segment {segment} does not match its manifest")));
    }
    let entries: Vec<JournalEntry> = body
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .map(serde_json::from_slice)
        .collect::<Result<_, _>>()
        .map_err(|e| AzureError::Response(e.to_string()))?;
    if entries.last().map(|e| e.hash) != Some(manifest.last_hash) || entries.first().map(|e| e.prev_hash) != Some(manifest.prev_hash) {
        return Err(AzureError::Response(format!("segment {segment} does not link to the chain")));
    }
    Ok(entries)
}
