//! The clinical store: where answers, evidence and underwriters' notes are
//! kept. Nothing clinical is in the module's state or its log; they hold a
//! reference into this store and a hash.
//!
//! Every record is sealed with AES-256-GCM under a key that belongs to one
//! person. That key is itself wrapped by a key-encryption key that never
//! leaves Azure Key Vault (`ic_azure::keywrap`), and the wrapped key and the
//! sealed records are kept in an erasable Blob container
//! (`ic_azure::records`). Destroying a person's wrapped key makes every
//! record about them unreadable wherever copies of the ciphertext survive,
//! which is how an erasure reaches backups.
//!
//! A read names who is reading and why, and returns the access record for
//! the caller to log.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use ic_azure::keywrap::{KeyWrapper, WrappedKey};
use ic_azure::records::RecordStore;
use ic_azure::AzureError;
use ic_core::PartyId;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum ClinicalError {
    #[error("the vault refused: {0}")]
    Vault(String),
    #[error("storage failed: {0}")]
    Storage(String),
    #[error("{0} is already stored; records are written once")]
    Exists(String),
    #[error("{0} is not in the store")]
    NotFound(String),
    #[error("the person's key has been destroyed: their records can no longer be read")]
    Shredded,
    #[error("the record does not open: it has been altered or belongs elsewhere")]
    Tampered,
    #[error("a read names who is reading and for what purpose")]
    NoPurpose,
    #[error("{0} is not a clinical store reference")]
    BadReference(String),
}

/// Key material and a nonce for one write, from the caller's secure random
/// source. Never reused.
#[derive(Clone)]
pub struct Fresh {
    pub key: [u8; 32],
    pub nonce: [u8; 12],
}

impl Fresh {
    pub fn random() -> Self {
        use rand::RngCore;
        let mut fresh = Fresh { key: [0; 32], nonce: [0; 12] };
        let mut rng = rand::thread_rng();
        rng.fill_bytes(&mut fresh.key);
        rng.fill_bytes(&mut fresh.nonce);
        fresh
    }
}

/// What the state machine is told about a stored record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    pub vault_ref: String,
    /// SHA-256 of the record as it was before sealing.
    pub hash: String,
}

/// Who read what, and why. The caller writes it to the access log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessRecord {
    pub who: String,
    pub purpose: String,
    pub vault_ref: String,
}

/// The clinical store over a key wrapper and an erasable record store:
/// `KeyVaultWrapper` and `BlobRecords` in production, `DevWrapper` and
/// `MemoryRecords` on a laptop.
pub struct ClinicalStore<W, R> {
    pub wrapper: W,
    pub records: R,
}

fn key_name(party: &PartyId) -> String {
    format!("keys/{party}")
}

fn erased_name(party: &PartyId) -> String {
    format!("erased/{party}")
}

fn parse(vault_ref: &str) -> Result<(PartyId, &str), ClinicalError> {
    let bad = || ClinicalError::BadReference(vault_ref.to_string());
    let rest = vault_ref.strip_prefix("clinical://").ok_or_else(bad)?;
    let (party, record) = rest.split_once('/').filter(|(p, r)| !p.is_empty() && !r.is_empty()).ok_or_else(bad)?;
    Ok((PartyId::from(party), record))
}

fn storage(e: AzureError) -> ClinicalError {
    ClinicalError::Storage(e.to_string())
}

fn vault(e: AzureError) -> ClinicalError {
    ClinicalError::Vault(e.to_string())
}

impl<W: KeyWrapper, R: RecordStore> ClinicalStore<W, R> {
    async fn unwrap(&self, stored: &[u8]) -> Result<[u8; 32], ClinicalError> {
        let wrapped: WrappedKey = serde_json::from_slice(stored).map_err(|e| ClinicalError::Vault(format!("the stored key is not a wrapped key: {e}")))?;
        let key = self.wrapper.unwrap(&wrapped).await.map_err(vault)?;
        key.try_into()
            .map_err(|_| ClinicalError::Vault("the unwrapped key is not 256 bits".to_string()))
    }

    /// The person's key, made from `fresh` if they have none yet.
    async fn key_for(&self, party: &PartyId, fresh: &Fresh) -> Result<[u8; 32], ClinicalError> {
        let name = key_name(party);
        if let Some(stored) = self.records.get(&name).await.map_err(storage)? {
            return self.unwrap(&stored).await;
        }
        if self.records.get(&erased_name(party)).await.map_err(storage)?.is_some() {
            return Err(ClinicalError::Shredded);
        }
        let wrapped = self.wrapper.wrap(&fresh.key).await.map_err(vault)?;
        let stored = serde_json::to_vec(&wrapped).map_err(|e| ClinicalError::Vault(e.to_string()))?;
        match self.records.put_new(&name, stored).await {
            Ok(()) => Ok(fresh.key),
            // Another writer made the person's key first. Theirs is the key.
            Err(AzureError::AlreadyExists(_)) => {
                let theirs = self.records.get(&name).await.map_err(storage)?.ok_or(ClinicalError::Shredded)?;
                self.unwrap(&theirs).await
            }
            Err(e) => Err(storage(e)),
        }
    }

    /// Seal a record about a person. `kind` and `id` name it, for example
    /// `disclosure` and the proposal and life. A record is written once.
    pub async fn put(&self, party: &PartyId, kind: &str, id: &str, record: &[u8], fresh: Fresh) -> Result<Stored, ClinicalError> {
        let vault_ref = format!("clinical://{party}/{kind}/{id}");
        let name = format!("records/{party}/{kind}/{id}");
        let key = self.key_for(party, &fresh).await?;
        let cipher = Aes256Gcm::new(&key.into());
        // The reference is bound into the seal: a record moved under another name does not open.
        let payload = Payload {
            msg: record,
            aad: vault_ref.as_bytes(),
        };
        let mut sealed = fresh.nonce.to_vec();
        sealed.extend(cipher.encrypt(Nonce::from_slice(&fresh.nonce), payload).map_err(|_| ClinicalError::Tampered)?);
        match self.records.put_new(&name, sealed).await {
            Ok(()) => Ok(Stored {
                vault_ref,
                hash: hex::encode(Sha256::digest(record)),
            }),
            Err(AzureError::AlreadyExists(_)) => Err(ClinicalError::Exists(vault_ref)),
            Err(e) => Err(storage(e)),
        }
    }

    /// Open a record for a named person and purpose.
    pub async fn get(&self, vault_ref: &str, who: &str, purpose: &str) -> Result<(Vec<u8>, AccessRecord), ClinicalError> {
        if who.is_empty() || purpose.is_empty() {
            return Err(ClinicalError::NoPurpose);
        }
        let (party, record) = parse(vault_ref)?;
        let sealed = self
            .records
            .get(&format!("records/{party}/{record}"))
            .await
            .map_err(storage)?
            .ok_or_else(|| ClinicalError::NotFound(vault_ref.to_string()))?;
        let stored = self.records.get(&key_name(&party)).await.map_err(storage)?.ok_or(ClinicalError::Shredded)?;
        let key = self.unwrap(&stored).await?;
        if sealed.len() < 12 {
            return Err(ClinicalError::Tampered);
        }
        let (nonce, ciphertext) = sealed.split_at(12);
        let payload = Payload {
            msg: ciphertext,
            aad: vault_ref.as_bytes(),
        };
        let plain = Aes256Gcm::new(&key.into())
            .decrypt(Nonce::from_slice(nonce), payload)
            .map_err(|_| ClinicalError::Tampered)?;
        let access = AccessRecord {
            who: who.to_string(),
            purpose: purpose.to_string(),
            vault_ref: vault_ref.to_string(),
        };
        Ok((plain, access))
    }

    /// Destroy the person's key. Every record about them, in this store and
    /// in any copy of it, is unreadable from now on, and nothing more can be
    /// stored about them. Safe to repeat.
    pub async fn shred(&self, party: &PartyId) -> Result<(), ClinicalError> {
        // The marker goes down first, so that a write racing the erasure
        // cannot give the person a new key after the old one has gone.
        match self.records.put_new(&erased_name(party), Vec::new()).await {
            Ok(()) | Err(AzureError::AlreadyExists(_)) => {}
            Err(e) => return Err(storage(e)),
        }
        self.records.delete(&key_name(party)).await.map_err(storage)
    }
}

#[cfg(test)]
mod tests {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use ic_azure::records::MemoryRecords;

    use super::*;

    /// A key-encryption key held in memory, standing in for the vault: a key
    /// wrapped under one does not open under another.
    #[derive(Clone)]
    struct LocalVault {
        kek: [u8; 32],
        /// Wraps answered wrongly, to play a vault that returns a short key.
        short: bool,
    }

    impl KeyWrapper for LocalVault {
        async fn wrap(&self, key: &[u8]) -> Result<WrappedKey, AzureError> {
            let digest = Sha256::new().chain_update(self.kek).chain_update(key).finalize();
            let mut out = digest[..12].to_vec();
            let sealed = Aes256Gcm::new(&self.kek.into()).encrypt(Nonce::from_slice(&digest[..12]), key);
            out.extend(sealed.map_err(|_| AzureError::Response("wrap failed".into()))?);
            Ok(WrappedKey {
                key_id: "local/v1".into(),
                value: URL_SAFE_NO_PAD.encode(out),
            })
        }
        async fn unwrap(&self, wrapped: &WrappedKey) -> Result<Vec<u8>, AzureError> {
            let refused = || AzureError::Response("the wrapped key does not open under this key-encryption key".into());
            let bytes = URL_SAFE_NO_PAD.decode(&wrapped.value).map_err(|_| refused())?;
            if bytes.len() < 12 {
                return Err(refused());
            }
            let (nonce, sealed) = bytes.split_at(12);
            let key = Aes256Gcm::new(&self.kek.into())
                .decrypt(Nonce::from_slice(nonce), sealed)
                .map_err(|_| refused())?;
            Ok(if self.short { key[..16].to_vec() } else { key })
        }
    }

    fn store() -> ClinicalStore<LocalVault, MemoryRecords> {
        ClinicalStore {
            wrapper: LocalVault { kek: [9; 32], short: false },
            records: MemoryRecords::default(),
        }
    }

    fn fresh(n: u8) -> Fresh {
        Fresh { key: [n; 32], nonce: [n; 12] }
    }

    async fn stored(s: &ClinicalStore<LocalVault, MemoryRecords>, name: &str) -> Vec<u8> {
        s.records.get(name).await.unwrap().unwrap()
    }

    /// Purpose: a record is sealed and opens again for a named reader and purpose, with the
    /// hash the state machine holds; it is written once; what is stored is not the plaintext;
    /// and one person's records share that person's key while another's do not.
    /// Why it matters: this is the only place health answers live. The hash ties the sealed
    /// record to the signed disclosure, and a read with no purpose is a read nobody can audit.
    #[tokio::test]
    async fn a_record_is_sealed_under_the_persons_key_and_read_with_a_purpose() {
        let s = store();
        let (ann, bob) = (PartyId::from("ann"), PartyId::from("bob"));
        let first = s.put(&ann, "disclosure", "P1/L1", b"heart condition: yes", fresh(1)).await.unwrap();
        assert_eq!(first.vault_ref, "clinical://ann/disclosure/P1/L1");
        assert_eq!(first.hash, hex::encode(Sha256::digest(b"heart condition: yes")));
        assert_eq!(
            s.put(&ann, "disclosure", "P1/L1", b"other", fresh(2)).await,
            Err(ClinicalError::Exists(first.vault_ref.clone()))
        );
        for name in s.records.names() {
            assert!(!stored(&s, &name).await.windows(5).any(|w| w == b"heart"), "{name} is stored in the clear");
        }

        let (plain, access) = s.get(&first.vault_ref, "amy", "underwriting case P1/L1/case/1").await.unwrap();
        assert_eq!(plain, b"heart condition: yes");
        assert_eq!((access.who.as_str(), access.vault_ref.as_str()), ("amy", "clinical://ann/disclosure/P1/L1"));
        assert_eq!(s.get(&first.vault_ref, "amy", "").await, Err(ClinicalError::NoPurpose));
        assert_eq!(s.get(&first.vault_ref, "", "x").await, Err(ClinicalError::NoPurpose));
        assert_eq!(
            s.get("clinical://ann/disclosure/nope", "amy", "x").await,
            Err(ClinicalError::NotFound("clinical://ann/disclosure/nope".into()))
        );
        for bad in ["vault://x", "clinical://ann", "clinical:///x", "clinical://ann/"] {
            assert_eq!(s.get(bad, "amy", "x").await, Err(ClinicalError::BadReference(bad.into())));
        }

        // A second record for the same person uses the same key, not the fresh one offered.
        let wrapped = stored(&s, "keys/ann").await;
        let notes = s.put(&ann, "notes", "P1/L1/1", b"notes", fresh(3)).await.unwrap();
        assert_eq!(stored(&s, "keys/ann").await, wrapped);
        assert_eq!(s.get(&notes.vault_ref, "amy", "case").await.unwrap().0, b"notes");
        s.put(&bob, "disclosure", "P1/L2", b"bob's", fresh(4)).await.unwrap();
        assert_ne!(stored(&s, "keys/bob").await, wrapped);
        let random = Fresh::random();
        assert_ne!((random.key, random.nonce), (Fresh::random().key, [0; 12]));
    }

    /// Purpose: destroying a person's key makes every record about them unreadable while
    /// another person's stay readable, can be repeated, and stops anything more being stored
    /// about them; a sealed record that is altered, cut short or moved under another name does
    /// not open; a key that another vault wrapped, a stored key that is not a wrapped key and a
    /// vault that answers with the wrong size of key are all refused; and a storage outage is
    /// reported, not hidden.
    /// Why it matters: erasure on request has to reach backups, which only destroying the key
    /// can do, and a write that slipped in behind an erasure would quietly undo it. A record
    /// that could be swapped or edited in storage would be worthless as evidence of what a
    /// person disclosed.
    #[tokio::test]
    async fn shredding_a_key_ends_access_and_tampering_is_detected() {
        let s = store();
        let (ann, bob) = (PartyId::from("ann"), PartyId::from("bob"));
        let a = s.put(&ann, "disclosure", "1", b"ann's answers", fresh(1)).await.unwrap();
        let b = s.put(&bob, "disclosure", "1", b"bob's answers", fresh(2)).await.unwrap();

        // Moved under another name, altered, or cut short.
        let sealed = stored(&s, "records/ann/disclosure/1").await;
        s.records.overwrite("records/ann/disclosure/2", sealed.clone());
        assert_eq!(s.get("clinical://ann/disclosure/2", "amy", "x").await, Err(ClinicalError::Tampered));
        let mut altered = sealed.clone();
        *altered.last_mut().unwrap() ^= 1;
        s.records.overwrite("records/ann/disclosure/1", altered);
        assert_eq!(s.get(&a.vault_ref, "amy", "x").await, Err(ClinicalError::Tampered));
        s.records.overwrite("records/ann/disclosure/1", sealed[..5].to_vec());
        assert_eq!(s.get(&a.vault_ref, "amy", "x").await, Err(ClinicalError::Tampered));
        s.records.overwrite("records/ann/disclosure/1", sealed);
        assert!(s.get(&a.vault_ref, "amy", "x").await.is_ok());

        // Another vault's key-encryption key cannot unwrap, and nor can a damaged wrapped key.
        let other = ClinicalStore {
            wrapper: LocalVault { kek: [8; 32], short: false },
            records: s.records.clone(),
        };
        assert!(matches!(other.get(&a.vault_ref, "amy", "x").await, Err(ClinicalError::Vault(_))));
        assert!(matches!(other.put(&ann, "notes", "9", b"n", fresh(5)).await, Err(ClinicalError::Vault(_))));
        let wrong_size = ClinicalStore {
            wrapper: LocalVault { kek: [9; 32], short: true },
            records: s.records.clone(),
        };
        assert!(matches!(wrong_size.get(&a.vault_ref, "amy", "x").await, Err(ClinicalError::Vault(_))));
        let good_key = stored(&s, "keys/bob").await;
        for damaged in [
            b"not json".to_vec(),
            br#"{"key_id":"local/v1","value":"AAAA"}"#.to_vec(),
            br#"{"key_id":"local/v1","value":"!"}"#.to_vec(),
        ] {
            s.records.overwrite("keys/bob", damaged);
            assert!(matches!(s.get(&b.vault_ref, "amy", "x").await, Err(ClinicalError::Vault(_))));
        }
        s.records.overwrite("keys/bob", good_key);

        s.shred(&ann).await.unwrap();
        assert_eq!(s.get(&a.vault_ref, "amy", "x").await, Err(ClinicalError::Shredded));
        assert_eq!(s.get(&b.vault_ref, "amy", "x").await.unwrap().0, b"bob's answers");
        // Repeated, as after a crash part way through.
        s.shred(&ann).await.unwrap();
        // Nothing more is stored about an erased person, so no new key appears for them.
        assert_eq!(s.put(&ann, "notes", "2", b"late note", fresh(6)).await, Err(ClinicalError::Shredded));
        assert!(!s.records.names().contains(&"keys/ann".to_string()));

        s.records.set_down(true);
        let outage = |r: Result<(), ClinicalError>| assert!(matches!(r, Err(ClinicalError::Storage(_))), "{r:?}");
        outage(s.get(&b.vault_ref, "amy", "x").await.map(|_| ()));
        outage(s.put(&bob, "notes", "1", b"n", fresh(3)).await.map(|_| ()));
        outage(s.shred(&bob).await);
        assert_eq!(
            ClinicalError::Shredded.to_string(),
            "the person's key has been destroyed: their records can no longer be read"
        );
    }

    /// Stores whose failures are placed exactly: the key appears between the
    /// look and the write, as when two writers race, or one call fails.
    #[derive(Clone, Default)]
    struct Racing {
        inner: MemoryRecords,
        /// What another writer stores under the key name just before ours lands.
        rival: Option<Vec<u8>>,
        /// Fail the write of a sealed record.
        fail_record_write: bool,
        /// The rival's key is erased again before it can be read.
        vanish: bool,
    }

    impl RecordStore for Racing {
        async fn put_new(&self, name: &str, body: Vec<u8>) -> Result<(), AzureError> {
            if name.starts_with("keys/") {
                if let Some(rival) = &self.rival {
                    if !self.vanish {
                        self.inner.overwrite(name, rival.clone());
                    }
                    return Err(AzureError::AlreadyExists(name.to_string()));
                }
            }
            if name.starts_with("records/") && self.fail_record_write {
                return Err(AzureError::Response("write failed".into()));
            }
            self.inner.put_new(name, body).await
        }
        async fn get(&self, name: &str) -> Result<Option<Vec<u8>>, AzureError> {
            self.inner.get(name).await
        }
        async fn delete(&self, name: &str) -> Result<(), AzureError> {
            self.inner.delete(name).await
        }
    }

    /// Purpose: when two writers make a person's first record at once, both seal under the key
    /// that landed first; if that key is erased before the loser reads it the write is refused
    /// as erased; and a failed write of the sealed record is an error with nothing recorded.
    /// Why it matters: two keys for one person would mean one of them is never destroyed by an
    /// erasure, and a record sealed under a key nobody stored could never be read.
    #[tokio::test]
    async fn two_first_writers_share_the_key_that_landed_first() {
        let vault = LocalVault { kek: [9; 32], short: false };
        let ann = PartyId::from("ann");
        let rival_key = serde_json::to_vec(&vault.wrap(&[7; 32]).await.unwrap()).unwrap();

        let racing = ClinicalStore {
            wrapper: vault.clone(),
            records: Racing {
                rival: Some(rival_key.clone()),
                ..Racing::default()
            },
        };
        let ours = racing.put(&ann, "disclosure", "1", b"answers", fresh(1)).await.unwrap();
        assert_eq!(racing.records.inner.get("keys/ann").await.unwrap(), Some(rival_key.clone()));
        // Readable with the rival's key, which is the only one stored.
        let plain = ClinicalStore {
            wrapper: vault.clone(),
            records: racing.records.inner.clone(),
        };
        assert_eq!(plain.get(&ours.vault_ref, "amy", "x").await.unwrap().0, b"answers");

        let vanished = ClinicalStore {
            wrapper: vault.clone(),
            records: Racing {
                rival: Some(rival_key),
                vanish: true,
                ..Racing::default()
            },
        };
        assert_eq!(vanished.put(&ann, "disclosure", "1", b"answers", fresh(1)).await, Err(ClinicalError::Shredded));

        let failing = ClinicalStore {
            wrapper: vault,
            records: Racing {
                fail_record_write: true,
                ..Racing::default()
            },
        };
        assert!(matches!(
            failing.put(&ann, "disclosure", "1", b"answers", fresh(1)).await,
            Err(ClinicalError::Storage(_))
        ));
        assert_eq!(failing.records.inner.names(), vec!["keys/ann".to_string()]);
    }
}
