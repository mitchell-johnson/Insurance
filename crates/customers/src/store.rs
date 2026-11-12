//! The customer master on PostgreSQL.
//!
//! One transaction per command: lock the party's row, load and open its
//! stream, decide, append the events with their hash links, update the
//! lookup tables (relationships, blind indexes) and write the public events
//! to the outbox. Either all of it commits or none of it. The row lock and
//! the stream version together stop two conflicting edits.

use std::collections::BTreeSet;

use chrono::{DateTime, NaiveDate, Utc};
use ic_azure::keywrap::{KeyWrapper, WrappedKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio_postgres::types::ToSql;
use tokio_postgres::{GenericClient, Transaction};

use crate::crypto::{self, DataKey, IndexKey};
use crate::db::Pool;
use crate::event::{chain_hash, EventBody, PublicEvent, Recorded, GENESIS};
use crate::keys::KeyRing;
use crate::matching;
use crate::model::{PartyId, PartyKind, Relationship, Target};
use crate::party::{self, reject, Change, FactInput, Party, Reject};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    /// The command was refused and nothing was written.
    #[error("{0}")]
    Refused(Reject),
    /// The database or Key Vault could not be reached. Nothing is known to
    /// have been written; the caller may send the same again.
    #[error("unavailable: {0}")]
    Unavailable(String),
    /// Stored data failed a check: a payload did not open or a hash link
    /// did not hold.
    #[error("integrity: {0}")]
    Integrity(String),
}

impl From<Reject> for StoreError {
    fn from(r: Reject) -> Self {
        StoreError::Refused(r)
    }
}

fn db(e: tokio_postgres::Error) -> StoreError {
    StoreError::Unavailable(format!("database: {e}"))
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Who sent a command, for the record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Who {
    /// The calling service's role, such as `group-admin`.
    pub caller: String,
    /// The person or process it acted for.
    pub actor: String,
}

/// What a command did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    pub party: PartyId,
    pub version: u64,
    /// The kinds of the events written; empty when the command repeated
    /// something already recorded.
    pub events: Vec<String>,
}

/// A repeated request is answered from here.
#[derive(Debug, Clone)]
pub struct Idempotency {
    pub caller: String,
    pub key: String,
    pub request: Vec<u8>,
}

impl Idempotency {
    fn hash(&self) -> Vec<u8> {
        Sha256::digest(&self.request).to_vec()
    }
}

/// A relationship row, with the party it belongs to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelationshipRow {
    pub party: PartyId,
    #[serde(flatten)]
    pub relationship: Relationship,
}

/// An open possible-duplicate review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Review {
    pub id: String,
    pub party: PartyId,
    pub candidates: Vec<PartyId>,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

/// One audit line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditLine {
    pub caller: String,
    pub actor: String,
    pub purpose: String,
    pub party: Option<PartyId>,
    pub action: String,
    pub fields: Vec<String>,
    pub decision: String,
    pub policy_version: String,
    pub break_glass: bool,
}

pub struct Store<W> {
    pool: Pool,
    keys: KeyRing<W>,
    index: IndexKey,
}

/// The public facts of a stream, and its key.
struct Stream {
    version: u64,
    head: Vec<u8>,
    shredded: bool,
    kind: PartyKind,
    key: Option<DataKey>,
}

/// What locking and loading a party in a transaction found.
struct Loaded {
    party: Party,
    head: Vec<u8>,
    key: Option<DataKey>,
}

fn kind_text(kind: PartyKind) -> &'static str {
    match kind {
        PartyKind::Person => "person",
        PartyKind::Organisation => "organisation",
    }
}

fn version_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

impl<W: KeyWrapper> Store<W> {
    pub fn new(pool: Pool, keys: KeyRing<W>, index: IndexKey) -> Self {
        Self { pool, keys, index }
    }

    pub fn index_key(&self) -> &IndexKey {
        &self.index
    }

    async fn client(&self) -> Result<deadpool::managed::Object<crate::db::Manager>> {
        self.pool.get().await.map_err(|e| StoreError::Unavailable(format!("database: {e}")))
    }

    /// Whether the database answers.
    pub async fn ready(&self) -> Result<()> {
        let client = self.client().await?;
        client.simple_query("select 1").await.map_err(db)?;
        Ok(())
    }

    async fn wrapped_key(&self, c: &impl GenericClient, id: &PartyId) -> Result<Option<WrappedKey>> {
        let row = c
            .query_opt("select key_id, wrapped from party_keys where party_id = $1", &[&id.0])
            .await
            .map_err(db)?;
        Ok(row.map(|r| WrappedKey {
            key_id: r.get(0),
            value: r.get(1),
        }))
    }

    /// Read and open a party's stream. With `lock`, inside a transaction,
    /// the party's row is locked first.
    async fn load_with(&self, c: &impl GenericClient, id: &PartyId, lock: bool) -> Result<Option<Loaded>> {
        let Some((records, mut stream)) = self.read_stream(c, id, lock).await? else {
            return Ok(None);
        };
        let mut party = Party::new(id.clone());
        for rec in &records {
            party.apply(rec);
        }
        if stream.shredded {
            party.shredded = true;
            party.kind = Some(stream.kind);
        } else if party.version != stream.version {
            return Err(StoreError::Integrity(format!(
                "{id} is at version {} but its stream ends at {}",
                stream.version, party.version
            )));
        }
        party.version = stream.version;
        Ok(Some(Loaded {
            party,
            head: std::mem::take(&mut stream.head),
            key: stream.key.take(),
        }))
    }

    async fn read_stream(&self, c: &impl GenericClient, id: &PartyId, lock: bool) -> Result<Option<(Vec<Recorded>, Stream)>> {
        let sql = if lock {
            "select version, head_hash, shredded_at is not null, kind from parties where id = $1 for update"
        } else {
            "select version, head_hash, shredded_at is not null, kind from parties where id = $1"
        };
        let Some(row) = c.query_opt(sql, &[&id.0]).await.map_err(db)? else {
            return Ok(None);
        };
        let version: i64 = row.get(0);
        let head: Vec<u8> = row.get(1);
        let shredded: bool = row.get(2);
        let kind = if row.get::<_, String>(3) == "organisation" {
            PartyKind::Organisation
        } else {
            PartyKind::Person
        };
        let key = match self.wrapped_key(c, id).await? {
            Some(w) => Some(self.keys.open(id, &w).await.map_err(StoreError::Unavailable)?),
            None => None,
        };
        let rows = c
            .query(
                "select seq, kind, recorded_at, caller, commitment, ciphertext from party_events where party_id = $1 order by seq",
                &[&id.0],
            )
            .await
            .map_err(db)?;
        let mut records = Vec::new();
        for r in rows {
            let seq: i64 = r.get(0);
            let kind: String = r.get(1);
            let recorded_at: DateTime<Utc> = r.get(2);
            let caller: String = r.get(3);
            let commitment: Vec<u8> = r.get(4);
            let ciphertext: Option<Vec<u8>> = r.get(5);
            let seq = u64::try_from(seq).map_err(|_| StoreError::Integrity("negative sequence".into()))?;
            match (&key, ciphertext) {
                (Some(key), Some(ct)) => {
                    let payload = crypto::open(key, &id.0, seq, &ct, &commitment).map_err(|e| StoreError::Integrity(format!("{id} event {seq}: {e}")))?;
                    let rec = serde_json::from_value::<Recorded>(payload).map_err(|e| StoreError::Integrity(format!("{id} event {seq}: {e}")))?;
                    records.push(rec);
                }
                // Erased: only the tombstone means anything.
                _ if shredded => {
                    if kind == "shredded" {
                        records.push(Recorded {
                            seq,
                            recorded_at,
                            caller,
                            actor: String::new(),
                            body: EventBody::Shredded,
                        });
                    }
                }
                _ => return Err(StoreError::Integrity(format!("{id} event {seq} has no payload or key"))),
            }
        }
        let stream = Stream {
            version: u64::try_from(version).unwrap_or(0),
            head,
            shredded,
            kind,
            key,
        };
        Ok(Some((records, stream)))
    }

    /// Every event of a party, opened: what an access request returns.
    pub async fn records(&self, id: &PartyId) -> Result<Option<Vec<Recorded>>> {
        let client = self.client().await?;
        Ok(self.read_stream(&*client, id, false).await?.map(|(r, _)| r))
    }

    /// An earlier answer to the same request, if there is one.
    pub async fn recall(&self, idem: &Idempotency) -> Result<Option<Value>> {
        let mut client = self.client().await?;
        let tx = client.transaction().await.map_err(db)?;
        self.remembered(&tx, idem).await
    }

    /// A party as it stands, or `None` if there is no such party.
    pub async fn load(&self, id: &PartyId) -> Result<Option<Party>> {
        let client = self.client().await?;
        Ok(self.load_with(&*client, id, false).await?.map(|l| l.party))
    }

    /// Follow merges to the party that survives.
    pub async fn resolve(&self, id: &PartyId) -> Result<Option<PartyId>> {
        let client = self.client().await?;
        let mut current = id.clone();
        for _ in 0..32 {
            let Some(row) = client
                .query_opt("select merged_into from parties where id = $1", &[&current.0])
                .await
                .map_err(db)?
            else {
                return Ok(None);
            };
            match row.get::<_, Option<String>>(0) {
                Some(next) => current = PartyId(next),
                None => return Ok(Some(current)),
            }
        }
        Err(StoreError::Integrity(format!("{id} merges in a loop")))
    }

    /// Append events to a locked party and bring the lookup tables with
    /// them. Returns the records written.
    async fn append(&self, tx: &Transaction<'_>, loaded: &mut Loaded, bodies: Vec<EventBody>, who: &Who, now: DateTime<Utc>) -> Result<Vec<Recorded>> {
        let id = loaded.party.id.clone();
        let mut written = Vec::new();
        for body in bodies {
            let seq = loaded.party.version + 1;
            let rec = Recorded {
                seq,
                recorded_at: now,
                caller: who.caller.clone(),
                actor: who.actor.clone(),
                body,
            };
            let (ciphertext, commitment) = match (&rec.body, &loaded.key) {
                (EventBody::Shredded, _) => (None, vec![0u8; 32]),
                (_, Some(key)) => {
                    let payload = serde_json::to_value(&rec).map_err(|e| StoreError::Integrity(e.to_string()))?;
                    let env = crypto::seal(key, &id.0, seq, &payload);
                    (Some(env.ciphertext), env.commitment.to_vec())
                }
                (_, None) => return Err(StoreError::Refused(reject("shredded", "the party's record has been erased"))),
            };
            let kind = rec.body.kind();
            let hash = chain_hash(&loaded.head, &id, seq, kind, now, &commitment);
            let fields: Vec<String> = rec.body.fields().iter().map(|f| f.as_str().to_string()).collect();
            tx.execute(
                "insert into party_events (party_id, seq, kind, recorded_at, caller, fields, commitment, prev_hash, hash, ciphertext) \
                 values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
                &[
                    &id.0,
                    &version_i64(seq),
                    &kind,
                    &now,
                    &who.caller,
                    &fields,
                    &commitment,
                    &loaded.head,
                    &hash,
                    &ciphertext,
                ],
            )
            .await
            .map_err(db)?;
            let ended = match &rec.body {
                EventBody::RelationshipEnded { id: rid, .. } => loaded.party.relationships.get(rid).cloned(),
                _ => None,
            };
            loaded.party.apply(&rec);
            self.project(tx, &id, &rec).await?;
            let public = PublicEvent::of(&id, &rec, ended.as_ref());
            let body = serde_json::to_value(&public).map_err(|e| StoreError::Integrity(e.to_string()))?;
            tx.execute(
                "insert into outbox (party_id, seq, priority, body, created_at) values ($1, $2, $3, $4, $5)",
                &[&id.0, &version_i64(seq), &public.priority, &body, &now],
            )
            .await
            .map_err(db)?;
            loaded.head = hash;
            written.push(rec);
        }
        let p = &loaded.party;
        tx.execute(
            "update parties set lifecycle = $2, group_id = $3, version = $4, head_hash = $5, merged_into = $6, holds = $7, updated_at = $8 where id = $1",
            &[
                &id.0,
                &p.lifecycle.map_or("prospect", |l| l.as_str()),
                &p.group,
                &version_i64(p.version),
                &loaded.head,
                &p.merged_into.as_ref().map(|m| m.0.clone()),
                &i32::try_from(p.holds.len()).unwrap_or(i32::MAX),
                &now,
            ],
        )
        .await
        .map_err(db)?;
        if !written.is_empty() && !p.shredded {
            self.reindex(tx, p).await?;
        }
        Ok(written)
    }

    /// Bring the relationship table up to date with one event.
    async fn project(&self, tx: &Transaction<'_>, id: &PartyId, rec: &Recorded) -> Result<()> {
        match &rec.body {
            EventBody::RelationshipAdded(r) => {
                tx.execute(
                    "insert into relationships (party_id, id, kind, target_kind, target_id, valid_from, valid_to, recorded_at, attributes) \
                     values ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
                    &[
                        &id.0,
                        &r.id,
                        &r.kind.as_str(),
                        &r.target.kind(),
                        &r.target.id(),
                        &r.valid_from,
                        &r.valid_to,
                        &r.recorded_at,
                        &Value::Object(r.attributes.clone()),
                    ],
                )
                .await
                .map_err(db)?;
            }
            EventBody::RelationshipEnded { id: rid, valid_to } => {
                tx.execute(
                    "update relationships set valid_to = $3 where party_id = $1 and id = $2",
                    &[&id.0, rid, valid_to],
                )
                .await
                .map_err(db)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Replace a party's blind indexes with those its record calls for.
    async fn reindex(&self, tx: &Transaction<'_>, party: &Party) -> Result<()> {
        tx.execute("delete from blind_index where party_id = $1", &[&party.id.0]).await.map_err(db)?;
        for (kind, digest) in matching::party_keys(party, &self.index) {
            tx.execute(
                "insert into blind_index (party_id, kind, digest) values ($1, $2, $3)",
                &[&party.id.0, &kind, &digest],
            )
            .await
            .map_err(db)?;
        }
        Ok(())
    }

    /// Look up an earlier answer to the same request. A different request
    /// under the same key is refused.
    async fn remembered(&self, tx: &Transaction<'_>, idem: &Idempotency) -> Result<Option<Value>> {
        let row = tx
            .query_opt(
                "select request_hash, response from idempotency where caller = $1 and key = $2",
                &[&idem.caller, &idem.key],
            )
            .await
            .map_err(db)?;
        match row {
            Some(r) if r.get::<_, Vec<u8>>(0) == idem.hash() => Ok(Some(r.get(1))),
            Some(_) => Err(StoreError::Refused(reject(
                "idempotency_conflict",
                "this idempotency key was used for a different request",
            ))),
            None => Ok(None),
        }
    }

    async fn remember(&self, tx: &Transaction<'_>, idem: &Idempotency, response: &Value, now: DateTime<Utc>) -> Result<()> {
        tx.execute(
            "insert into idempotency (caller, key, request_hash, response, created_at) values ($1, $2, $3, $4, $5)",
            &[&idem.caller, &idem.key, &idem.hash(), response, &now],
        )
        .await
        .map_err(db)?;
        Ok(())
    }

    /// Create a party. With `review`, also record the possible duplicates
    /// found, for a person to resolve.
    pub async fn create(
        &self,
        kind: PartyKind,
        facts: &[FactInput],
        group: Option<String>,
        who: &Who,
        idem: Option<&Idempotency>,
        review: &[PartyId],
        now: DateTime<Utc>,
    ) -> Result<Outcome> {
        let id = PartyId::generate();
        let bodies = party::create(&id, kind, facts, group, &who.caller, now)?;
        let mut client = self.client().await?;
        let tx = client.transaction().await.map_err(db)?;
        if let Some(idem) = idem {
            if let Some(answer) = self.remembered(&tx, idem).await? {
                return serde_json::from_value(answer).map_err(|e| StoreError::Integrity(e.to_string()));
            }
        }
        let (key, wrapped) = self.keys.issue(&id).await.map_err(StoreError::Unavailable)?;
        let Some(EventBody::Created { group, .. }) = bodies.first() else {
            unreachable!("party::create starts with Created")
        };
        tx.execute(
            "insert into parties (id, kind, lifecycle, group_id, version, head_hash, created_at, updated_at) values ($1, $2, 'prospect', $3, 0, $4, $5, $5)",
            &[&id.0, &kind_text(kind), group, &GENESIS.to_vec(), &now],
        )
        .await
        .map_err(db)?;
        tx.execute(
            "insert into party_keys (party_id, key_id, wrapped, created_at) values ($1, $2, $3, $4)",
            &[&id.0, &wrapped.key_id, &wrapped.value, &now],
        )
        .await
        .map_err(db)?;
        let mut loaded = Loaded {
            party: Party::new(id.clone()),
            head: GENESIS.to_vec(),
            key: Some(key),
        };
        let written = self.append(&tx, &mut loaded, bodies, who, now).await?;
        if !review.is_empty() {
            let candidates: Vec<String> = review.iter().map(|p| p.0.clone()).collect();
            tx.execute(
                "insert into reviews (id, party_id, candidates, status, created_at) values ($1, $2, $3, 'open', $4)",
                &[&format!("rev_{}", uuid::Uuid::new_v4().simple()), &id.0, &candidates, &now],
            )
            .await
            .map_err(db)?;
        }
        let outcome = Outcome {
            party: id,
            version: loaded.party.version,
            events: written.iter().map(|r| r.body.kind().to_string()).collect(),
        };
        if let Some(idem) = idem {
            self.remember(&tx, idem, &serde_json::to_value(&outcome).unwrap_or_default(), now).await?;
        }
        tx.commit().await.map_err(db)?;
        Ok(outcome)
    }

    /// Apply a command to a party. `expected` is the version the caller
    /// last saw, if it wants the change refused when someone else has
    /// changed the party since.
    pub async fn change(
        &self,
        id: &PartyId,
        change: &Change,
        expected: Option<u64>,
        who: &Who,
        idem: Option<&Idempotency>,
        now: DateTime<Utc>,
    ) -> Result<Outcome> {
        let mut client = self.client().await?;
        let tx = client.transaction().await.map_err(db)?;
        if let Some(idem) = idem {
            if let Some(answer) = self.remembered(&tx, idem).await? {
                return serde_json::from_value(answer).map_err(|e| StoreError::Integrity(e.to_string()));
            }
        }
        let mut loaded = self.load_with(&tx, id, true).await?.ok_or_else(|| reject("not_found", "no such party"))?;
        if let Some(v) = expected {
            if v != loaded.party.version {
                return Err(StoreError::Refused(reject(
                    "version_conflict",
                    format!("the party is at version {}, not {v}", loaded.party.version),
                )));
            }
        }
        let bodies = loaded.party.decide(change, &who.caller, now)?;
        if let Change::AddRelationship {
            target: Target::Party(other), ..
        } = change
        {
            let exists = tx
                .query_opt("select 1 from parties where id = $1 and shredded_at is null", &[&other.0])
                .await
                .map_err(db)?
                .is_some();
            if !exists {
                return Err(StoreError::Refused(reject("not_found", format!("no party {other}"))));
            }
        }
        let written = self.append(&tx, &mut loaded, bodies, who, now).await?;
        let outcome = Outcome {
            party: id.clone(),
            version: loaded.party.version,
            events: written.iter().map(|r| r.body.kind().to_string()).collect(),
        };
        if let Some(idem) = idem {
            self.remember(&tx, idem, &serde_json::to_value(&outcome).unwrap_or_default(), now).await?;
        }
        tx.commit().await.map_err(db)?;
        Ok(outcome)
    }

    /// Lock two parties in a fixed order, so two merges of the same pair
    /// cannot deadlock.
    async fn load_pair(&self, tx: &Transaction<'_>, a: &PartyId, b: &PartyId) -> Result<(Loaded, Loaded)> {
        let missing = |id: &PartyId| StoreError::Refused(reject("not_found", format!("no party {id}")));
        let (first, second) = if a < b { (a, b) } else { (b, a) };
        let l1 = self.load_with(tx, first, true).await?.ok_or_else(|| missing(first))?;
        let l2 = self.load_with(tx, second, true).await?.ok_or_else(|| missing(second))?;
        Ok(if a < b { (l1, l2) } else { (l2, l1) })
    }

    /// Merge `absorbed` into `survivor`. The absorbed ID resolves to the
    /// survivor from then on, so nothing in the core has to change.
    pub async fn merge(&self, survivor: &PartyId, absorbed: &PartyId, review: Option<&str>, who: &Who, now: DateTime<Utc>) -> Result<Outcome> {
        if survivor == absorbed {
            return Err(StoreError::Refused(reject("invalid", "a party cannot be merged into itself")));
        }
        let mut client = self.client().await?;
        let tx = client.transaction().await.map_err(db)?;
        let (mut s, mut a) = self.load_pair(&tx, survivor, absorbed).await?;
        let (on_survivor, on_absorbed) = party::merge(&s.party, &a.party)?;
        let written = self.append(&tx, &mut s, vec![on_survivor], who, now).await?;
        self.append(&tx, &mut a, vec![on_absorbed], who, now).await?;
        if let Some(review) = review {
            self.close_review(&tx, review, "merged", who, now).await?;
        }
        tx.commit().await.map_err(db)?;
        Ok(Outcome {
            party: survivor.clone(),
            version: s.party.version,
            events: written.iter().map(|r| r.body.kind().to_string()).collect(),
        })
    }

    /// Undo a merge.
    pub async fn unmerge(&self, survivor: &PartyId, absorbed: &PartyId, who: &Who, now: DateTime<Utc>) -> Result<Outcome> {
        let mut client = self.client().await?;
        let tx = client.transaction().await.map_err(db)?;
        let (mut s, mut a) = self.load_pair(&tx, survivor, absorbed).await?;
        let (on_survivor, on_absorbed) = party::unmerge(&s.party, &a.party)?;
        self.append(&tx, &mut s, vec![on_survivor], who, now).await?;
        let written = self.append(&tx, &mut a, vec![on_absorbed], who, now).await?;
        tx.commit().await.map_err(db)?;
        Ok(Outcome {
            party: absorbed.clone(),
            version: a.party.version,
            events: written.iter().map(|r| r.body.kind().to_string()).collect(),
        })
    }

    async fn close_review(&self, tx: &Transaction<'_>, review: &str, status: &str, who: &Who, now: DateTime<Utc>) -> Result<()> {
        let n = tx
            .execute(
                "update reviews set status = $2, resolved_at = $3, resolved_by = $4 where id = $1 and status = 'open'",
                &[&review, &status, &now, &who.actor],
            )
            .await
            .map_err(db)?;
        if n == 0 {
            return Err(StoreError::Refused(reject("not_found", format!("no open review {review}"))));
        }
        Ok(())
    }

    /// Close a review: the parties are different people.
    pub async fn dismiss_review(&self, review: &str, who: &Who, now: DateTime<Utc>) -> Result<()> {
        let mut client = self.client().await?;
        let tx = client.transaction().await.map_err(db)?;
        self.close_review(&tx, review, "dismissed", who, now).await?;
        tx.commit().await.map_err(db)
    }

    pub async fn open_reviews(&self, limit: i64) -> Result<Vec<Review>> {
        let client = self.client().await?;
        let rows = client
            .query(
                "select id, party_id, candidates, status, created_at from reviews where status = 'open' order by created_at limit $1",
                &[&limit],
            )
            .await
            .map_err(db)?;
        Ok(rows
            .iter()
            .map(|r| Review {
                id: r.get(0),
                party: PartyId(r.get(1)),
                candidates: r.get::<_, Vec<String>>(2).into_iter().map(PartyId).collect(),
                status: r.get(3),
                created_at: r.get(4),
            })
            .collect())
    }

    /// The parties any of these index entries point at, survivors only.
    pub async fn candidates(&self, keys: &[(&'static str, Vec<u8>)]) -> Result<Vec<PartyId>> {
        if keys.is_empty() {
            return Ok(vec![]);
        }
        let client = self.client().await?;
        let kinds: Vec<&str> = keys.iter().map(|k| k.0).collect();
        let digests: Vec<&[u8]> = keys.iter().map(|k| k.1.as_slice()).collect();
        let rows = client
            .query(
                "select distinct coalesce(p.merged_into, p.id) from blind_index b join parties p on p.id = b.party_id \
                 join unnest($1::text[], $2::bytea[]) as k(kind, digest) on b.kind = k.kind and b.digest = k.digest \
                 where p.shredded_at is null limit 50",
                &[&kinds, &digests],
            )
            .await
            .map_err(db)?;
        let mut out: BTreeSet<PartyId> = BTreeSet::new();
        for r in rows {
            out.insert(PartyId(r.get(0)));
        }
        Ok(out.into_iter().collect())
    }

    /// Relationships pointing at a target (a policy, a scheme or a party).
    pub async fn relationships_to(&self, target: &Target) -> Result<Vec<RelationshipRow>> {
        let client = self.client().await?;
        let rows = client
            .query(
                "select party_id, id, kind, target_kind, target_id, valid_from, valid_to, recorded_at, attributes \
                 from relationships where target_kind = $1 and target_id = $2 order by party_id, id",
                &[&target.kind(), &target.id()],
            )
            .await
            .map_err(db)?;
        rows.iter().map(row_to_relationship).collect()
    }

    /// Relationships the party, or anyone merged into it, holds.
    pub async fn relationships_of(&self, party: &PartyId) -> Result<Vec<RelationshipRow>> {
        let client = self.client().await?;
        let rows = client
            .query(
                "select r.party_id, r.id, r.kind, r.target_kind, r.target_id, r.valid_from, r.valid_to, r.recorded_at, r.attributes \
                 from relationships r join parties p on p.id = r.party_id where p.id = $1 or p.merged_into = $1 order by r.party_id, r.id",
                &[&party.0],
            )
            .await
            .map_err(db)?;
        rows.iter().map(row_to_relationship).collect()
    }

    /// Write one audit line.
    pub async fn audit(&self, line: &AuditLine, now: DateTime<Utc>) -> Result<()> {
        let client = self.client().await?;
        client
            .execute(
                "insert into audit (at, caller, actor, purpose, party_id, action, fields, decision, policy_version, break_glass) \
                 values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
                &[
                    &now,
                    &line.caller,
                    &line.actor,
                    &line.purpose,
                    &line.party.as_ref().map(|p| p.0.clone()),
                    &line.action,
                    &line.fields,
                    &line.decision,
                    &line.policy_version,
                    &line.break_glass,
                ],
            )
            .await
            .map_err(db)?;
        Ok(())
    }

    /// The audit trail for a party, newest first.
    pub async fn audit_of(&self, party: &PartyId, limit: i64) -> Result<Vec<(DateTime<Utc>, AuditLine)>> {
        let client = self.client().await?;
        let rows = client
            .query(
                "select at, caller, actor, purpose, party_id, action, fields, decision, policy_version, break_glass from audit \
                 where party_id = $1 order by id desc limit $2",
                &[&party.0, &limit],
            )
            .await
            .map_err(db)?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get(0),
                    AuditLine {
                        caller: r.get(1),
                        actor: r.get(2),
                        purpose: r.get(3),
                        party: r.get::<_, Option<String>>(4).map(PartyId),
                        action: r.get(5),
                        fields: r.get(6),
                        decision: r.get(7),
                        policy_version: r.get(8),
                        break_glass: r.get(9),
                    },
                )
            })
            .collect())
    }

    /// Unpublished public events, priority first, then oldest first.
    pub async fn outbox(&self, limit: i64) -> Result<Vec<(i64, PublicEvent)>> {
        let client = self.client().await?;
        let rows = client
            .query(
                "select id, body from outbox where published_at is null order by priority desc, id limit $1",
                &[&limit],
            )
            .await
            .map_err(db)?;
        rows.iter()
            .map(|r| {
                let body: Value = r.get(1);
                serde_json::from_value(body)
                    .map(|e| (r.get(0), e))
                    .map_err(|e| StoreError::Integrity(e.to_string()))
            })
            .collect()
    }

    pub async fn published(&self, ids: &[i64], now: DateTime<Utc>) -> Result<()> {
        let client = self.client().await?;
        client
            .execute("update outbox set published_at = $2 where id = any($1)", &[&ids, &now])
            .await
            .map_err(db)?;
        Ok(())
    }

    /// Walk a party's hash chain from the stored public columns. Returns
    /// the number of links checked.
    pub async fn verify_chain(&self, id: &PartyId) -> Result<u64> {
        let client = self.client().await?;
        let rows = client
            .query(
                "select seq, kind, recorded_at, commitment, prev_hash, hash from party_events where party_id = $1 order by seq",
                &[&id.0],
            )
            .await
            .map_err(db)?;
        let head: Option<Vec<u8>> = client
            .query_opt("select head_hash from parties where id = $1", &[&id.0])
            .await
            .map_err(db)?
            .map(|r| r.get(0));
        let head = head.ok_or_else(|| StoreError::Refused(reject("not_found", "no such party")))?;
        let mut prev = GENESIS.to_vec();
        let mut n = 0u64;
        for r in &rows {
            let seq: i64 = r.get(0);
            let kind: String = r.get(1);
            let recorded_at: DateTime<Utc> = r.get(2);
            let commitment: Vec<u8> = r.get(3);
            let stored_prev: Vec<u8> = r.get(4);
            let hash: Vec<u8> = r.get(5);
            let expected = chain_hash(&prev, id, u64::try_from(seq).unwrap_or(0), &kind, recorded_at, &commitment);
            if stored_prev != prev || hash != expected || seq != version_i64(n + 1) {
                return Err(StoreError::Integrity(format!("{id}: the hash chain breaks at event {seq}")));
            }
            prev = hash;
            n += 1;
        }
        if prev != head {
            return Err(StoreError::Integrity(format!("{id}: the head does not match the last event")));
        }
        Ok(n)
    }

    /// Parties that may be due for erasure: none of their own or incoming
    /// relationships still open or in force, no hold, not merged, and
    /// nothing changed since `quiet_since`.
    pub async fn sweep_candidates(&self, today: NaiveDate, quiet_since: DateTime<Utc>, limit: i64) -> Result<Vec<PartyId>> {
        let client = self.client().await?;
        let rows = client
            .query(
                "select p.id from parties p where p.shredded_at is null and p.merged_into is null and p.holds = 0 and p.updated_at < $2 \
                 and not exists (select 1 from relationships r where r.party_id = p.id and (r.valid_to is null or r.valid_to > $1)) \
                 and not exists (select 1 from relationships r where r.target_kind = 'party' and r.target_id = p.id and (r.valid_to is null or r.valid_to > $1)) \
                 order by p.updated_at limit $3",
                &[&today, &quiet_since, &limit],
            )
            .await
            .map_err(db)?;
        Ok(rows.iter().map(|r| PartyId(r.get(0))).collect())
    }

    /// Erase a party, and anyone merged into it: destroy the key, purge
    /// the lookup tables, null the payloads and write the tombstone.
    /// `still_due` is checked again under the lock, so a hold or a new
    /// relationship that arrived since the sweep looked stops it.
    pub async fn shred(&self, id: &PartyId, still_due: impl Fn(&Party) -> bool, now: DateTime<Utc>) -> Result<bool> {
        let mut client = self.client().await?;
        let tx = client.transaction().await.map_err(db)?;
        let Some(loaded) = self.load_with(&tx, id, true).await? else {
            return Ok(false);
        };
        if loaded.party.shredded || !loaded.party.holds.is_empty() || loaded.party.merged_into.is_some() || !still_due(&loaded.party) {
            return Ok(false);
        }
        let absorbed: Vec<PartyId> = loaded.party.absorbed.iter().cloned().collect();
        self.erase(&tx, loaded, now).await?;
        for other in absorbed {
            if let Some(l) = self.bare(&tx, &other).await? {
                self.erase(&tx, l, now).await?;
            }
        }
        tx.commit().await.map_err(db)?;
        Ok(true)
    }

    /// Lock a party's row without opening its key: enough to append a
    /// tombstone.
    async fn bare(&self, tx: &Transaction<'_>, id: &PartyId) -> Result<Option<Loaded>> {
        let row = tx
            .query_opt(
                "select version, head_hash, shredded_at is not null from parties where id = $1 for update",
                &[&id.0],
            )
            .await
            .map_err(db)?;
        Ok(row.map(|r| {
            let mut party = Party::new(id.clone());
            party.version = u64::try_from(r.get::<_, i64>(0)).unwrap_or(0);
            party.shredded = r.get(2);
            Loaded {
                party,
                head: r.get(1),
                key: None,
            }
        }))
    }

    /// Destroy a locked party's key, purge its lookup rows, blank its
    /// payloads and write the tombstone.
    async fn erase(&self, tx: &Transaction<'_>, mut l: Loaded, now: DateTime<Utc>) -> Result<()> {
        if l.party.shredded {
            return Ok(());
        }
        let id = l.party.id.clone();
        let who = Who {
            caller: "customer-master".into(),
            actor: "retention".into(),
        };
        l.key = None;
        self.append(tx, &mut l, vec![EventBody::Shredded], &who, now).await?;
        for sql in [
            "delete from party_keys where party_id = $1",
            "delete from blind_index where party_id = $1",
            "delete from relationships where party_id = $1",
            "update party_events set ciphertext = null where party_id = $1 and ciphertext is not null",
            "update parties set shredded_at = $2 where id = $1",
        ] {
            let params: Vec<&(dyn ToSql + Sync)> = if sql.contains("$2") { vec![&id.0, &now] } else { vec![&id.0] };
            tx.execute(sql, &params).await.map_err(db)?;
        }
        self.keys.forget(&id);
        Ok(())
    }

    /// After a database restore: erase again every party the register says
    /// was erased but whose key has come back. The key is not opened.
    pub async fn reshred(&self, parties: &[PartyId], now: DateTime<Utc>) -> Result<usize> {
        let mut n = 0;
        for p in parties {
            let mut client = self.client().await?;
            let tx = client.transaction().await.map_err(db)?;
            let has_key = tx
                .query_opt("select 1 from party_keys where party_id = $1", &[&p.0])
                .await
                .map_err(db)?
                .is_some();
            if let (true, Some(mut l)) = (has_key, self.bare(&tx, p).await?) {
                // The row says erased only if the restore kept that; either
                // way the key must go.
                l.party.shredded = false;
                self.erase(&tx, l, now).await?;
                n += 1;
            }
            tx.commit().await.map_err(db)?;
        }
        Ok(n)
    }

    /// Wrap every party's key again under the current version of the
    /// key-encryption key. Returns the number rewrapped.
    pub async fn rewrap_all(&self, older_than: &str, limit: i64) -> Result<usize> {
        let client = self.client().await?;
        let rows = client
            .query(
                "select party_id, key_id, wrapped from party_keys where key_id <> $1 limit $2",
                &[&older_than, &limit],
            )
            .await
            .map_err(db)?;
        let mut n = 0;
        for r in rows {
            let party = PartyId(r.get(0));
            let wrapped = WrappedKey {
                key_id: r.get(1),
                value: r.get(2),
            };
            let new = self.keys.rewrap(&party, &wrapped).await.map_err(StoreError::Unavailable)?;
            client
                .execute(
                    "update party_keys set key_id = $2, wrapped = $3 where party_id = $1 and key_id = $4",
                    &[&party.0, &new.key_id, &new.value, &wrapped.key_id],
                )
                .await
                .map_err(db)?;
            n += 1;
        }
        Ok(n)
    }
}

fn row_to_relationship(r: &tokio_postgres::Row) -> Result<RelationshipRow> {
    let kind: String = r.get(2);
    let target_kind: String = r.get(3);
    let target_id: String = r.get(4);
    let attributes: Value = r.get(8);
    let bad = |what: &str| StoreError::Integrity(format!("relationship row has an unknown {what}"));
    Ok(RelationshipRow {
        party: PartyId(r.get(0)),
        relationship: Relationship {
            id: r.get(1),
            kind: crate::model::RelationshipKind::parse(&kind).ok_or_else(|| bad("kind"))?,
            target: Target::from_parts(&target_kind, &target_id).ok_or_else(|| bad("target"))?,
            valid_from: r.get(5),
            valid_to: r.get(6),
            recorded_at: r.get(7),
            attributes: match attributes {
                Value::Object(m) => m,
                _ => serde_json::Map::new(),
            },
        },
    })
}
