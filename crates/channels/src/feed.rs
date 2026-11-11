//! Audience-scoped rows and the per-party change feed that devices sync from.
//!
//! Every row records the policy it concerns, the person it is about and
//! whether it implies something about health. Cedar policy maps a viewer's
//! relationships to the rows they may see. A party's feed is the ordered
//! list of changes to the rows visible to that party, across every policy
//! they have a relationship with: a scheme member's phone holds their own
//! cover and nothing else from the employer's scheme.
//!
//! A device takes a snapshot, then follows the changes after the snapshot's
//! version. Applying a change twice is harmless, because each carries the
//! version it produces. When a relationship ends the feed sends a purge.

use std::collections::{BTreeMap, VecDeque};

use ic_core::{PartyId, PolicyId};
use ic_readmodel::scope::{AudienceScope, Relationship, RowScope, ScopeError};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How many changes a feed keeps. A device further behind takes a new snapshot.
pub const CHANGE_LOG_LEN: usize = 512;

/// One row of a read model, with what audience scoping needs to know.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    /// Unique within the platform. Begins with the policy ID and a slash.
    pub key: String,
    pub policy: PolicyId,
    /// `policy`, `cover`, `billing`, `scheme`, `member`, `underwriting_terms`,
    /// `claim`, `adviser`, `request`, `notice`, `held_balance`.
    pub kind: String,
    /// The party the row is about, where it is about one person.
    pub subject: Option<PartyId>,
    /// Carries an insurance fact that implies health.
    pub sensitive: bool,
    pub body: Value,
}

impl Row {
    pub fn scope(&self) -> RowScope<'_> {
        RowScope {
            kind: &self.kind,
            subject: self.subject.as_ref().map(|p| p.0.as_str()),
            sensitive: self.sensitive,
        }
    }
}

/// Whether a viewer may see a row. `consented` holds the adults on the
/// policy who let the policyholder see their own sensitive rows.
pub fn visible(
    scope: &AudienceScope,
    viewer: &PartyId,
    relationships: &[Relationship],
    row: &Row,
    consented: &std::collections::BTreeSet<PartyId>,
    fresh: bool,
) -> Result<bool, ScopeError> {
    if relationships.is_empty() {
        return Ok(false);
    }
    if scope.may_view(&viewer.0, relationships, &row.scope(), fresh)? {
        return Ok(true);
    }
    // Family privacy: an adult's health-implying rows are theirs. The
    // policyholder sees them only while that person consents, and never on
    // stale relationship data.
    Ok(fresh && row.sensitive && relationships.contains(&Relationship::Policyholder) && row.subject.as_ref().is_some_and(|s| consented.contains(s)))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Upsert {
        body: Value,
    },
    /// The row no longer exists.
    Delete,
    /// The viewer lost access. The device removes the row and anything cached from it.
    Purge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    /// The feed version this change produces.
    pub version: u64,
    pub key: String,
    #[serde(flatten)]
    pub op: Op,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartyFeed {
    pub version: u64,
    /// key -> (version last changed at, body).
    pub rows: BTreeMap<String, (u64, Value)>,
    pub changes: VecDeque<Change>,
}

/// What a device receives when it asks for what changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum Sync {
    /// Replace everything held with these rows.
    Snapshot { version: u64, rows: Vec<SnapshotRow> },
    /// Apply these in order.
    Changes { version: u64, changes: Vec<Change> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SnapshotRow {
    pub key: String,
    pub version: u64,
    pub body: Value,
}

impl PartyFeed {
    fn push(&mut self, key: String, op: Op) {
        self.version += 1;
        match &op {
            Op::Upsert { body } => {
                self.rows.insert(key.clone(), (self.version, body.clone()));
            }
            Op::Delete | Op::Purge => {
                self.rows.remove(&key);
            }
        }
        self.changes.push_back(Change {
            version: self.version,
            key,
            op,
        });
        while self.changes.len() > CHANGE_LOG_LEN {
            self.changes.pop_front();
        }
    }

    /// Bring the rows under one policy to `visible`. Rows the viewer no
    /// longer sees are purged if access was lost and deleted if the row is
    /// gone. Returns how many changes were made.
    pub fn reconcile(&mut self, policy: &PolicyId, visible: &BTreeMap<String, Value>, exists: impl Fn(&str) -> bool) -> usize {
        let prefix = format!("{policy}/");
        let before = self.version;
        let held: Vec<String> = self
            .rows
            .range(prefix.clone()..)
            .take_while(|(k, _)| k.starts_with(&prefix))
            .map(|(k, _)| k.clone())
            .collect();
        for key in held {
            if !visible.contains_key(&key) {
                let op = if exists(&key) { Op::Purge } else { Op::Delete };
                self.push(key, op);
            }
        }
        for (key, body) in visible {
            if self.rows.get(key).is_none_or(|(_, held)| held != body) {
                self.push(key.clone(), Op::Upsert { body: body.clone() });
            }
        }
        (self.version - before) as usize
    }

    /// What a device at `have` needs. `None` asks for a snapshot.
    pub fn sync(&self, have: Option<u64>) -> Sync {
        let floor = self.changes.front().map_or(self.version, |c| c.version - 1);
        match have {
            Some(v) if v <= self.version && v >= floor => Sync::Changes {
                version: self.version,
                changes: self.changes.iter().filter(|c| c.version > v).cloned().collect(),
            },
            // A device that is ahead of the feed, or too far behind it,
            // starts again from a snapshot.
            _ => Sync::Snapshot {
                version: self.version,
                rows: self
                    .rows
                    .iter()
                    .map(|(key, (version, body))| SnapshotRow {
                        key: key.clone(),
                        version: *version,
                        body: body.clone(),
                    })
                    .collect(),
            },
        }
    }
}
