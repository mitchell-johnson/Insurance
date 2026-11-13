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
use crate::model::{Field, PartyId, PartyKind, Relationship, Target};
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

/// Stored data that did not read back as written.
fn integrity(e: impl std::fmt::Display) -> StoreError {
    StoreError::Integrity(e.to_string())
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

/// A change to a person's own hold, waiting to be told to the core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerHoldNotice {
    pub row: i64,
    pub party: PartyId,
    pub policy: String,
    pub held: bool,
    pub reference: String,
    pub recorded_at: DateTime<Utc>,
    pub attempts: i32,
}

/// A death, or its retraction, waiting to be told to claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeathNotice {
    pub party: PartyId,
    pub date_of_death: NaiveDate,
    pub retract: bool,
    pub attempts: i32,
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
    /// `duplicate` (candidates to merge or dismiss) or `consider_marker`
    /// (a trained person decides whether the party needs a marker).
    pub kind: String,
    pub party: PartyId,
    pub candidates: Vec<PartyId>,
    /// For a prompt: what the concern was about.
    pub reason: Option<String>,
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
    /// The key being changed to, while a change is under way.
    next: Option<IndexKey>,
    /// How long a person's own spending hold stands after they ask for it
    /// to end.
    cooling_off: chrono::Duration,
}

/// The default cooling-off period before a spending hold ends.
pub const HOLD_COOLING_OFF_DAYS: i64 = 7;

/// How far a change of index key has got.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IndexStatus {
    /// The key lookups use, by its public id.
    pub current: String,
    /// The key being built, by its public id.
    pub next: Option<String>,
    /// Parties not yet indexed under the current key.
    pub waiting_current: i64,
    /// Parties not yet indexed under the next key.
    pub waiting_next: Option<i64>,
    /// Entries under keys no longer in use, still to be removed.
    pub retired_entries: i64,
    /// True once the next key covers everyone and can be promoted.
    pub ready_to_promote: bool,
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
        Self {
            pool,
            keys,
            index,
            next: None,
            cooling_off: chrono::Duration::days(HOLD_COOLING_OFF_DAYS),
        }
    }

    /// Set the cooling-off period before a spending hold ends.
    pub fn with_hold_cooling_off(mut self, period: chrono::Duration) -> Self {
        self.cooling_off = period;
        self
    }

    /// How long a spending hold stands after its removal is asked for.
    pub fn hold_cooling_off(&self) -> chrono::Duration {
        self.cooling_off
    }

    /// End every spending hold whose cooling-off period has passed.
    pub async fn end_spending_holds(&self, now: DateTime<Utc>) -> Result<usize> {
        let due: Vec<String> = self
            .client()
            .await?
            .query(
                "select party_id from spending_hold_removals where requested_at <= $1",
                &[&(now - self.cooling_off)],
            )
            .await
            .map_err(db)?
            .iter()
            .map(|r| r.get(0))
            .collect();
        let who = Who {
            caller: "customer-master".into(),
            actor: "cooling-off".into(),
        };
        let mut ended = 0;
        for id in due {
            let out = self.change(&PartyId(id), &Change::EndSpendingHold, None, &who, None, now).await?;
            ended += usize::from(!out.events.is_empty());
        }
        Ok(ended)
    }

    /// Build blind indexes under `next` as well, ahead of making it the
    /// current key. Refused if it is the current key.
    pub fn with_next_index(mut self, next: Option<IndexKey>) -> std::result::Result<Self, String> {
        if next.as_ref().is_some_and(|n| n.id() == self.index.id()) {
            return Err("the next index key is the current one".into());
        }
        self.next = next;
        Ok(self)
    }

    /// The keys every write indexes under: the current one, then the next.
    fn index_keys(&self) -> impl Iterator<Item = &IndexKey> {
        std::iter::once(&self.index).chain(self.next.as_ref())
    }

    /// How many parties are not yet indexed under a key.
    async fn waiting(&self, c: &impl GenericClient, key: &IndexKey) -> Result<i64> {
        let row = c
            .query_one(
                "select count(*) from parties p where p.shredded_at is null \
                 and not exists (select 1 from index_state s where s.party_id = p.id and s.key_id = $1)",
                &[&key.id()],
            )
            .await
            .map_err(db)?;
        Ok(row.get(0))
    }

    /// Where a change of index key stands.
    pub async fn index_status(&self) -> Result<IndexStatus> {
        let client = self.client().await?;
        let waiting_current = self.waiting(&*client, &self.index).await?;
        let waiting_next = match &self.next {
            Some(k) => Some(self.waiting(&*client, k).await?),
            None => None,
        };
        let active: Vec<String> = self.index_keys().map(IndexKey::id).collect();
        let retired: i64 = client
            .query_one("select count(*) from blind_index where key_id <> all($1)", &[&active])
            .await
            .map_err(db)?
            .get(0);
        Ok(IndexStatus {
            current: self.index.id(),
            next: self.next.as_ref().map(IndexKey::id),
            waiting_current,
            waiting_next,
            retired_entries: retired,
            ready_to_promote: waiting_next == Some(0),
        })
    }

    /// Entries made before keys were named are under the current key, if
    /// it has not changed since. Check one party's against the current key;
    /// if they agree, name them and count every party as indexed. If not,
    /// leave them, and the rebuild indexes everyone afresh.
    pub async fn adopt_index(&self) -> Result<bool> {
        let client = self.client().await?;
        let Some(row) = client
            .query_opt("select party_id from blind_index where key_id = '' limit 1", &[])
            .await
            .map_err(db)?
        else {
            return Ok(false);
        };
        let id = PartyId(row.get(0));
        let stored: BTreeSet<(String, Vec<u8>)> = client
            .query("select kind, digest from blind_index where party_id = $1 and key_id = ''", &[&id.0])
            .await
            .map_err(db)?
            .iter()
            .map(|r| (r.get(0), r.get(1)))
            .collect();
        let party = self.load_with(&*client, &id, false).await?.map(|l| l.party);
        let expected: BTreeSet<(String, Vec<u8>)> = party
            .map(|p| matching::party_keys(&p, &self.index).into_iter().map(|(k, d)| (k.to_string(), d)).collect())
            .unwrap_or_default();
        if expected != stored {
            tracing::warn!(party = %id, "older blind indexes are not under the current key; rebuilding them all");
            return Ok(false);
        }
        drop(client);
        let mut client = self.client().await?;
        let tx = client.transaction().await.map_err(db)?;
        let current = self.index.id();
        tx.execute("update blind_index set key_id = $1 where key_id = ''", &[&current])
            .await
            .map_err(db)?;
        tx.execute(
            "insert into index_state (party_id, key_id) select id, $1 from parties where shredded_at is null on conflict do nothing",
            &[&current],
        )
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(true)
    }

    /// Index up to `limit` parties that some key in use has not covered,
    /// under every key in use. Each party's facts are opened to do it.
    pub async fn rebuild_index(&self, limit: i64) -> Result<usize> {
        let mut done = 0;
        let keys: Vec<String> = self.index_keys().map(IndexKey::id).collect();
        let ids: Vec<String> = self
            .client()
            .await?
            .query(
                "select p.id from parties p where p.shredded_at is null and exists \
                 (select 1 from unnest($1::text[]) k(id) where not exists \
                  (select 1 from index_state s where s.party_id = p.id and s.key_id = k.id)) \
                 order by p.id limit $2",
                &[&keys, &limit],
            )
            .await
            .map_err(db)?
            .iter()
            .map(|r| r.get(0))
            .collect();
        for id in ids {
            let mut client = self.client().await?;
            let tx = client.transaction().await.map_err(db)?;
            if let Some(l) = self.load_with(&tx, &PartyId(id), true).await? {
                self.reindex(&tx, &l.party).await?;
                done += 1;
            }
            tx.commit().await.map_err(db)?;
        }
        Ok(done)
    }

    /// Remove up to `limit` entries made under keys no longer in use, once
    /// the current key covers everyone (until then they may still be the
    /// only way to find someone).
    pub async fn retire_index(&self, limit: i64) -> Result<u64> {
        let client = self.client().await?;
        if self.waiting(&*client, &self.index).await? > 0 {
            return Ok(0);
        }
        let active: Vec<String> = self.index_keys().map(IndexKey::id).collect();
        let mut n = 0;
        for table in ["blind_index", "index_state"] {
            n += client
                .execute(
                    &format!("delete from {table} where ctid = any(array(select ctid from {table} where key_id <> all($1) limit $2))"),
                    &[&active, &limit],
                )
                .await
                .map_err(db)?;
        }
        Ok(n)
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
            let seq = u64::try_from(seq).map_err(integrity)?;
            match (&key, ciphertext) {
                (Some(key), Some(ct)) => {
                    let rec = crypto::open(key, &id.0, seq, &ct, &commitment)
                        .map_err(crate::text)
                        .and_then(|payload| serde_json::from_value::<Recorded>(payload).map_err(crate::text))
                        .map_err(|e| integrity(format!("{id} event {seq}: {e}")))?;
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
                    let payload = serde_json::to_value(&rec).map_err(integrity)?;
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
            let body = serde_json::to_value(&public).map_err(integrity)?;
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
        for key in self.index_keys() {
            let key_id = key.id();
            for (kind, digest) in matching::party_keys(party, key) {
                tx.execute(
                    "insert into blind_index (party_id, kind, digest, key_id) values ($1, $2, $3, $4)",
                    &[&party.id.0, &kind, &digest, &key_id],
                )
                .await
                .map_err(db)?;
            }
            tx.execute(
                "insert into index_state (party_id, key_id) values ($1, $2) on conflict do nothing",
                &[&party.id.0, &key_id],
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
                return serde_json::from_value(answer).map_err(integrity);
            }
        }
        let (key, wrapped) = self.keys.issue(&id).await.map_err(StoreError::Unavailable)?;
        // party::create always starts with Created.
        let group = bodies.iter().find_map(|b| match b {
            EventBody::Created { group, .. } => Some(group.clone()),
            _ => None,
        });
        tx.execute(
            "insert into parties (id, kind, lifecycle, group_id, version, head_hash, created_at, updated_at) values ($1, $2, 'prospect', $3, 0, $4, $5, $5)",
            &[&id.0, &kind_text(kind), &group, &GENESIS.to_vec(), &now],
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
                return serde_json::from_value(answer).map_err(integrity);
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
        // The date a retraction withdraws is the one recorded before it.
        let died_on = loaded.party.current(Field::DateOfDeath, now);
        let held_before = loaded.party.spending_hold.is_some();
        let written = self.append(&tx, &mut loaded, bodies, who, now).await?;
        // The core hears of the person's own hold, policy by policy.
        for h in crate::relay::owner_hold_changes(&loaded.party, &written, held_before, now.date_naive()) {
            tx.execute(
                "insert into owner_hold_relay (party_id, policy, held, reference, due_on, recorded_at) values ($1, $2, $3, $4, $5, $6)",
                &[&id.0, &h.policy, &h.held, &h.reference, &h.due_on, &now],
            )
            .await
            .map_err(db)?;
        }
        // Claims is told of every death and every retraction, whoever
        // recorded it, in the order they happened.
        let died = written.iter().any(|r| matches!(r.body, EventBody::Deceased { .. }));
        let retracted = written.iter().any(|r| matches!(r.body, EventBody::DeathRetracted { .. }));
        for r in &written {
            match &r.body {
                EventBody::SpendingHoldRemovalRequested { .. } => {
                    tx.execute(
                        "insert into spending_hold_removals (party_id, requested_at) values ($1, $2) \
                         on conflict (party_id) do update set requested_at = excluded.requested_at",
                        &[&id.0, &now],
                    )
                    .await
                    .map_err(db)?;
                }
                EventBody::SpendingHoldRemovalCancelled | EventBody::SpendingHoldEnded { .. } => {
                    tx.execute("delete from spending_hold_removals where party_id = $1", &[&id.0])
                        .await
                        .map_err(db)?;
                }
                _ => {}
            }
        }
        if let (true, Change::MarkDeceased { date_of_death }) = (died, change) {
            self.queue_death(&tx, id, *date_of_death, false, now).await?;
            self.prompt_bereaved(&tx, id, now).await?;
        }
        if let (true, Some(date)) = (retracted, died_on.as_ref().and_then(Value::as_str).and_then(|d| d.parse::<NaiveDate>().ok())) {
            self.queue_death(&tx, id, date, true, now).await?;
        }
        let marked = written.iter().any(|r| match &r.body {
            EventBody::Asserted { assertions } => assertions.iter().any(|a| a.field == Field::Vulnerability),
            _ => false,
        });
        if marked {
            let review_on = loaded
                .party
                .current(Field::Vulnerability, now)
                .and_then(|m| m.get("review_on").and_then(Value::as_str).and_then(|d| d.parse::<NaiveDate>().ok()));
            match review_on {
                Some(on) => tx
                    .execute(
                        "insert into marker_reviews (party_id, review_on) values ($1, $2) \
                         on conflict (party_id) do update set review_on = excluded.review_on, prompted_at = null",
                        &[&id.0, &on],
                    )
                    .await
                    .map_err(db)?,
                None => tx.execute("delete from marker_reviews where party_id = $1", &[&id.0]).await.map_err(db)?,
            };
        }
        // The channels directory hears of ownership, assignees and children.
        for r in &written {
            let command = match &r.body {
                EventBody::RelationshipAdded(rel) => crate::relay::directory_command(id, rel, false),
                EventBody::RelationshipEnded { id: rid, .. } => loaded
                    .party
                    .relationships
                    .get(rid)
                    .and_then(|rel| crate::relay::directory_command(id, rel, true)),
                _ => None,
            };
            if let Some(command) = command {
                let command = serde_json::to_value(&command).map_err(integrity)?;
                tx.execute(
                    "insert into directory_relay (party_id, command, recorded_at) values ($1, $2, $3)",
                    &[&id.0, &command, &now],
                )
                .await
                .map_err(db)?;
            }
        }
        for r in &written {
            if let EventBody::ConcernRaised { about, .. } = &r.body {
                tx.execute(
                    "insert into reviews (id, party_id, candidates, status, created_at, kind, reason) values ($1, $2, '{}', 'open', $3, 'consider_marker', $4)",
                    &[&format!("rev_{}", uuid::Uuid::new_v4().simple()), &id.0, &now, about],
                )
                .await
                .map_err(db)?;
            }
        }
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

    /// After a death, a prompt for a person to consider a bereavement
    /// marker for whoever is left: the other owners of a policy the person
    /// owned, and a partner recorded either way. Never a marker itself, and
    /// never a second open prompt for the same reason.
    async fn prompt_bereaved(&self, tx: &Transaction<'_>, deceased: &PartyId, now: DateTime<Utc>) -> Result<()> {
        let today = now.date_naive();
        let rows = tx
            .query(
                "select distinct other from ( \
                   select o.party_id as other from relationships d join relationships o \
                     on o.target_kind = 'policy' and o.target_id = d.target_id and o.kind = 'owner' \
                   where d.party_id = $1 and d.kind = 'owner' and d.target_kind = 'policy' \
                     and (o.valid_to is null or o.valid_to > $2) \
                   union select target_id from relationships where party_id = $1 and kind = 'partner' and target_kind = 'party' \
                     and (valid_to is null or valid_to > $2) \
                   union select party_id from relationships where target_id = $1 and kind = 'partner' and target_kind = 'party' \
                     and (valid_to is null or valid_to > $2) \
                 ) left_behind join parties p on p.id = left_behind.other \
                 where other <> $1 and p.shredded_at is null and p.merged_into is null \
                   and not exists (select 1 from reviews r where r.party_id = other and r.kind = 'consider_marker' \
                                   and r.reason = 'bereavement' and r.status = 'open')",
                &[&deceased.0, &today],
            )
            .await
            .map_err(db)?;
        for r in rows {
            let other: String = r.get(0);
            tx.execute(
                "insert into reviews (id, party_id, candidates, status, created_at, kind, reason) values ($1, $2, '{}', 'open', $3, 'consider_marker', 'bereavement')",
                &[&format!("rev_{}", uuid::Uuid::new_v4().simple()), &other, &now],
            )
            .await
            .map_err(db)?;
        }
        Ok(())
    }

    /// Lock two parties in a fixed order, so two merges of the same pair
    /// cannot deadlock.
    async fn load_pair(&self, tx: &Transaction<'_>, a: &PartyId, b: &PartyId) -> Result<(Loaded, Loaded)> {
        let missing = |id: &PartyId| StoreError::Refused(reject("not_found", format!("no party {id}")));
        let (first, second) = if a < b { (a, b) } else { (b, a) };
        let l1 = self.load_with(tx, first, true).await?.ok_or(missing(first))?;
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
                "update reviews set status = $2, resolved_at = $3, resolved_by = $4 where id = $1 and status = 'open' and (kind = 'duplicate' or $2 = 'dismissed')",
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
                "select id, party_id, candidates, status, created_at, kind, reason from reviews where status = 'open' order by created_at limit $1",
                &[&limit],
            )
            .await
            .map_err(db)?;
        Ok(rows
            .iter()
            .map(|r| Review {
                id: r.get(0),
                kind: r.get(5),
                party: PartyId(r.get(1)),
                candidates: r.get::<_, Vec<String>>(2).into_iter().map(PartyId).collect(),
                reason: r.get(6),
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
                 where p.shredded_at is null and b.key_id in ($3, '') limit 50",
                &[&kinds, &digests, &self.index.id()],
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
        self.audit_where("party_id = $1", &party.0, limit).await
    }

    /// Everything one person did or was checked for, across every party:
    /// what they sent as the actor, and every authority question asked
    /// about them. Newest first.
    pub async fn audit_by(&self, actor: &PartyId, limit: i64) -> Result<Vec<(DateTime<Utc>, AuditLine)>> {
        self.audit_where("(actor = $1 or action like 'authority:' || $1 || ':%')", &actor.0, limit)
            .await
    }

    async fn audit_where(&self, filter: &str, value: &str, limit: i64) -> Result<Vec<(DateTime<Utc>, AuditLine)>> {
        let client = self.client().await?;
        let rows = client
            .query(
                &format!(
                    "select at, caller, actor, purpose, party_id, action, fields, decision, policy_version, break_glass from audit \
                     where {filter} order by id desc limit $2"
                ),
                &[&value, &limit],
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
                serde_json::from_value(body).map(|e| (r.get(0), e)).map_err(integrity)
            })
            .collect()
    }

    /// Directory commands waiting, in order: (row, party, command, attempts).
    pub async fn directory_to_relay(&self, limit: i64) -> Result<Vec<(i64, PartyId, Value, i32)>> {
        let client = self.client().await?;
        let rows = client
            .query("select id, party_id, command, attempts from directory_relay order by id limit $1", &[&limit])
            .await
            .map_err(db)?;
        Ok(rows.iter().map(|r| (r.get(0), PartyId(r.get(1)), r.get(2), r.get(3))).collect())
    }

    /// Hold changes due by `today`, in order.
    pub async fn owner_holds_to_relay(&self, today: NaiveDate, limit: i64) -> Result<Vec<OwnerHoldNotice>> {
        let client = self.client().await?;
        let rows = client
            .query(
                "select id, party_id, policy, held, reference, recorded_at, attempts from owner_hold_relay \
                 where due_on <= $1 order by id limit $2",
                &[&today, &limit],
            )
            .await
            .map_err(db)?;
        Ok(rows
            .iter()
            .map(|r| OwnerHoldNotice {
                row: r.get(0),
                party: PartyId(r.get(1)),
                policy: r.get(2),
                held: r.get(3),
                reference: r.get(4),
                recorded_at: r.get(5),
                attempts: r.get(6),
            })
            .collect())
    }

    /// A hold change told, given up, or failed with the reason.
    pub async fn owner_hold_done(&self, row: i64, failed: Option<&str>) -> Result<()> {
        let client = self.client().await?;
        match failed {
            None => client.execute("delete from owner_hold_relay where id = $1", &[&row]).await,
            Some(e) => {
                client
                    .execute(
                        "update owner_hold_relay set attempts = attempts + 1, last_error = $2 where id = $1",
                        &[&row, &e],
                    )
                    .await
            }
        }
        .map_err(db)?;
        Ok(())
    }

    /// A directory command accepted, or failed with the reason.
    pub async fn directory_done(&self, row: i64, failed: Option<&str>) -> Result<()> {
        let client = self.client().await?;
        match failed {
            None => client.execute("delete from directory_relay where id = $1", &[&row]).await,
            Some(e) => {
                client
                    .execute("update directory_relay set attempts = attempts + 1, last_error = $2 where id = $1", &[&row, &e])
                    .await
            }
        }
        .map_err(db)?;
        Ok(())
    }

    /// Open a prompt for every marker whose review date has come, once.
    /// Returns the number opened.
    pub async fn prompt_marker_reviews(&self, today: NaiveDate, now: DateTime<Utc>) -> Result<usize> {
        let mut client = self.client().await?;
        let tx = client.transaction().await.map_err(db)?;
        let due = tx
            .query(
                "update marker_reviews set prompted_at = $2 where review_on <= $1 and prompted_at is null returning party_id",
                &[&today, &now],
            )
            .await
            .map_err(db)?;
        for row in &due {
            let party: String = row.get(0);
            tx.execute(
                "insert into reviews (id, party_id, candidates, status, created_at, kind, reason) values ($1, $2, '{}', 'open', $3, 'consider_marker', 'review_date')",
                &[&format!("rev_{}", uuid::Uuid::new_v4().simple()), &party, &now],
            )
            .await
            .map_err(db)?;
        }
        tx.commit().await.map_err(db)?;
        Ok(due.len())
    }

    /// Queue a death, or its retraction, for claims. A newer notice about
    /// the same death replaces one not yet sent.
    async fn queue_death(&self, tx: &Transaction<'_>, id: &PartyId, date: NaiveDate, retract: bool, now: DateTime<Utc>) -> Result<()> {
        tx.execute("delete from death_relay where party_id = $1 and date_of_death = $2", &[&id.0, &date])
            .await
            .map_err(db)?;
        tx.execute(
            "insert into death_relay (party_id, date_of_death, retract, recorded_at) values ($1, $2, $3, $4)",
            &[&id.0, &date, &retract, &now],
        )
        .await
        .map_err(db)?;
        Ok(())
    }

    /// Deaths and retractions not yet told to claims on every partition,
    /// in the order they happened, with the attempts made so far.
    pub async fn deaths_to_relay(&self, limit: i64) -> Result<Vec<DeathNotice>> {
        let client = self.client().await?;
        let rows = client
            .query(
                "select party_id, date_of_death, retract, attempts from death_relay order by id limit $1",
                &[&limit],
            )
            .await
            .map_err(db)?;
        Ok(rows
            .iter()
            .map(|r| DeathNotice {
                party: PartyId(r.get(0)),
                date_of_death: r.get(1),
                retract: r.get(2),
                attempts: r.get(3),
            })
            .collect())
    }

    /// A notice every partition has accepted: nothing more to send.
    pub async fn death_relayed(&self, n: &DeathNotice) -> Result<()> {
        let client = self.client().await?;
        client
            .execute(
                "delete from death_relay where party_id = $1 and date_of_death = $2 and retract = $3",
                &[&n.party.0, &n.date_of_death, &n.retract],
            )
            .await
            .map_err(db)?;
        Ok(())
    }

    /// A notice not yet accepted everywhere: kept, with the reason, to be
    /// tried again.
    pub async fn death_failed(&self, n: &DeathNotice, error: &str) -> Result<()> {
        let client = self.client().await?;
        client
            .execute(
                "update death_relay set attempts = attempts + 1, last_error = $4 where party_id = $1 and date_of_death = $2 and retract = $3",
                &[&n.party.0, &n.date_of_death, &n.retract, &error],
            )
            .await
            .map_err(db)?;
        Ok(())
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
            "delete from death_relay where party_id = $1",
            "delete from marker_reviews where party_id = $1",
            "delete from directory_relay where party_id = $1",
            "delete from blind_index where party_id = $1",
            "delete from index_state where party_id = $1",
            "delete from spending_hold_removals where party_id = $1",
            "delete from owner_hold_relay where party_id = $1",
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
            kind: crate::model::RelationshipKind::parse(&kind).ok_or(bad("kind"))?,
            target: Target::from_parts(&target_kind, &target_id).ok_or(bad("target"))?,
            valid_from: r.get(5),
            valid_to: r.get(6),
            recorded_at: r.get(7),
            attributes: attributes.as_object().cloned().unwrap_or_default(),
        },
    })
}
