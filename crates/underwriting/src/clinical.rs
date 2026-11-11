//! The clinical store: where answers, evidence and underwriters' notes are
//! kept. Nothing clinical is in the module's state or its log; they hold a
//! reference into this store and a hash.
//!
//! Every record is sealed with AES-256-GCM under a key that belongs to one
//! person. That key is itself wrapped by a key-encryption key held in a
//! vault (Azure Key Vault in production). Destroying a person's wrapped key
//! makes every record about them unreadable wherever copies of the
//! ciphertext survive, which is how an erasure reaches backups.
//!
//! A read names who is reading and why, and returns the access record for
//! the caller to log.

use std::collections::BTreeMap;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
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

/// Wraps and unwraps a person's key under the key-encryption key.
pub trait KeyVault {
    fn wrap(&self, key: &[u8; 32]) -> Result<Vec<u8>, ClinicalError>;
    fn unwrap(&self, wrapped: &[u8]) -> Result<[u8; 32], ClinicalError>;
}

/// Where sealed records and wrapped keys are kept.
pub trait Blobs {
    fn put(&mut self, name: &str, bytes: Vec<u8>) -> Result<(), ClinicalError>;
    fn get(&self, name: &str) -> Result<Option<Vec<u8>>, ClinicalError>;
    fn delete(&mut self, name: &str) -> Result<(), ClinicalError>;
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

pub struct ClinicalStore<V, B> {
    pub vault: V,
    pub blobs: B,
}

fn key_name(party: &PartyId) -> String {
    format!("keys/{party}")
}

fn parse(vault_ref: &str) -> Result<(PartyId, &str), ClinicalError> {
    let bad = || ClinicalError::BadReference(vault_ref.to_string());
    let rest = vault_ref.strip_prefix("clinical://").ok_or_else(bad)?;
    let (party, record) = rest.split_once('/').filter(|(p, r)| !p.is_empty() && !r.is_empty()).ok_or_else(bad)?;
    Ok((PartyId::from(party), record))
}

impl<V: KeyVault, B: Blobs> ClinicalStore<V, B> {
    /// Seal a record about a person. `kind` and `id` name it, for example
    /// `disclosure` and the proposal and life. A record is written once.
    pub fn put(&mut self, party: &PartyId, kind: &str, id: &str, record: &[u8], fresh: Fresh) -> Result<Stored, ClinicalError> {
        let vault_ref = format!("clinical://{party}/{kind}/{id}");
        let name = format!("records/{party}/{kind}/{id}");
        if self.blobs.get(&name)?.is_some() {
            return Err(ClinicalError::Exists(vault_ref));
        }
        let key = match self.blobs.get(&key_name(party))? {
            Some(wrapped) => self.vault.unwrap(&wrapped)?,
            None => {
                self.blobs.put(&key_name(party), self.vault.wrap(&fresh.key)?)?;
                fresh.key
            }
        };
        let cipher = Aes256Gcm::new(&key.into());
        // The reference is bound into the seal: a record moved under another name does not open.
        let payload = Payload { msg: record, aad: vault_ref.as_bytes() };
        let mut sealed = fresh.nonce.to_vec();
        sealed.extend(cipher.encrypt(Nonce::from_slice(&fresh.nonce), payload).map_err(|_| ClinicalError::Tampered)?);
        self.blobs.put(&name, sealed)?;
        Ok(Stored {
            vault_ref,
            hash: hex::encode(Sha256::digest(record)),
        })
    }

    /// Open a record for a named person and purpose.
    pub fn get(&self, vault_ref: &str, who: &str, purpose: &str) -> Result<(Vec<u8>, AccessRecord), ClinicalError> {
        if who.is_empty() || purpose.is_empty() {
            return Err(ClinicalError::NoPurpose);
        }
        let (party, record) = parse(vault_ref)?;
        let sealed = self.blobs.get(&format!("records/{party}/{record}"))?.ok_or_else(|| ClinicalError::NotFound(vault_ref.to_string()))?;
        let wrapped = self.blobs.get(&key_name(&party))?.ok_or(ClinicalError::Shredded)?;
        let key = self.vault.unwrap(&wrapped)?;
        if sealed.len() < 12 {
            return Err(ClinicalError::Tampered);
        }
        let (nonce, ciphertext) = sealed.split_at(12);
        let payload = Payload { msg: ciphertext, aad: vault_ref.as_bytes() };
        let plain = Aes256Gcm::new(&key.into()).decrypt(Nonce::from_slice(nonce), payload).map_err(|_| ClinicalError::Tampered)?;
        let access = AccessRecord {
            who: who.to_string(),
            purpose: purpose.to_string(),
            vault_ref: vault_ref.to_string(),
        };
        Ok((plain, access))
    }

    /// Destroy the person's key. Every record about them, in this store and
    /// in any copy of it, is unreadable from now on.
    pub fn shred(&mut self, party: &PartyId) -> Result<(), ClinicalError> {
        self.blobs.delete(&key_name(party))
    }
}

/// Blobs in memory, for tests and for running locally.
#[derive(Debug, Default, Clone)]
pub struct MemoryBlobs {
    pub blobs: BTreeMap<String, Vec<u8>>,
    /// Set to make every call fail, to exercise an outage.
    pub down: bool,
}

impl MemoryBlobs {
    fn up(&self) -> Result<(), ClinicalError> {
        if self.down {
            Err(ClinicalError::Storage("unavailable".to_string()))
        } else {
            Ok(())
        }
    }
}

impl Blobs for MemoryBlobs {
    fn put(&mut self, name: &str, bytes: Vec<u8>) -> Result<(), ClinicalError> {
        self.up()?;
        self.blobs.insert(name.to_string(), bytes);
        Ok(())
    }
    fn get(&self, name: &str) -> Result<Option<Vec<u8>>, ClinicalError> {
        self.up()?;
        Ok(self.blobs.get(name).cloned())
    }
    fn delete(&mut self, name: &str) -> Result<(), ClinicalError> {
        self.up()?;
        self.blobs.remove(name);
        Ok(())
    }
}

/// A key-encryption key held in memory, for tests and for running locally.
/// Production wraps keys in Azure Key Vault, where the key never leaves the
/// vault.
pub struct LocalVault {
    kek: [u8; 32],
}

impl LocalVault {
    pub fn new(kek: [u8; 32]) -> Self {
        Self { kek }
    }
}

impl KeyVault for LocalVault {
    fn wrap(&self, key: &[u8; 32]) -> Result<Vec<u8>, ClinicalError> {
        // A wrapped key is only ever wrapped once, so a nonce derived from it is never reused.
        let digest = Sha256::new().chain_update(self.kek).chain_update(key).finalize();
        let mut out = digest[..12].to_vec();
        let sealed = Aes256Gcm::new(&self.kek.into()).encrypt(Nonce::from_slice(&digest[..12]), key.as_slice());
        out.extend(sealed.map_err(|_| ClinicalError::Vault("wrap failed".to_string()))?);
        Ok(out)
    }
    fn unwrap(&self, wrapped: &[u8]) -> Result<[u8; 32], ClinicalError> {
        let refused = || ClinicalError::Vault("the wrapped key does not open under this key-encryption key".to_string());
        if wrapped.len() < 12 {
            return Err(refused());
        }
        let (nonce, sealed) = wrapped.split_at(12);
        let key = Aes256Gcm::new(&self.kek.into()).decrypt(Nonce::from_slice(nonce), sealed).map_err(|_| refused())?;
        key.try_into().map_err(|_| refused())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> ClinicalStore<LocalVault, MemoryBlobs> {
        ClinicalStore {
            vault: LocalVault::new([9; 32]),
            blobs: MemoryBlobs::default(),
        }
    }

    fn fresh(n: u8) -> Fresh {
        Fresh { key: [n; 32], nonce: [n; 12] }
    }

    /// Purpose: a record is sealed and opens again for a named reader and purpose, with the
    /// hash the state machine holds; it is written once; what is stored is not the plaintext;
    /// and one person's records share that person's key while another's do not.
    /// Why it matters: this is the only place health answers live. The hash ties the sealed
    /// record to the signed disclosure, and a read with no purpose is a read nobody can audit.
    #[test]
    fn a_record_is_sealed_under_the_persons_key_and_read_with_a_purpose() {
        let mut s = store();
        let (ann, bob) = (PartyId::from("ann"), PartyId::from("bob"));
        let stored = s.put(&ann, "disclosure", "P1/L1", b"heart condition: yes", fresh(1)).unwrap();
        assert_eq!(stored.vault_ref, "clinical://ann/disclosure/P1/L1");
        assert_eq!(stored.hash, hex::encode(Sha256::digest(b"heart condition: yes")));
        assert_eq!(s.put(&ann, "disclosure", "P1/L1", b"other", fresh(2)), Err(ClinicalError::Exists(stored.vault_ref.clone())));
        assert!(!s.blobs.blobs.values().any(|b| b.windows(5).any(|w| w == b"heart")), "nothing is stored in the clear");

        let (plain, access) = s.get(&stored.vault_ref, "amy", "underwriting case P1/L1/case/1").unwrap();
        assert_eq!(plain, b"heart condition: yes");
        assert_eq!((access.who.as_str(), access.vault_ref.as_str()), ("amy", "clinical://ann/disclosure/P1/L1"));
        assert_eq!(s.get(&stored.vault_ref, "amy", ""), Err(ClinicalError::NoPurpose));
        assert_eq!(s.get(&stored.vault_ref, "", "x"), Err(ClinicalError::NoPurpose));
        assert_eq!(s.get("clinical://ann/disclosure/nope", "amy", "x"), Err(ClinicalError::NotFound("clinical://ann/disclosure/nope".into())));
        for bad in ["vault://x", "clinical://ann", "clinical:///x", "clinical://ann/"] {
            assert_eq!(s.get(bad, "amy", "x"), Err(ClinicalError::BadReference(bad.into())));
        }

        // A second record for the same person uses the same key, not the fresh one offered.
        let wrapped = s.blobs.blobs["keys/ann"].clone();
        let notes = s.put(&ann, "notes", "P1/L1/1", b"notes", fresh(3)).unwrap();
        assert_eq!(s.blobs.blobs["keys/ann"], wrapped);
        assert_eq!(s.get(&notes.vault_ref, "amy", "case").unwrap().0, b"notes");
        s.put(&bob, "disclosure", "P1/L2", b"bob's", fresh(4)).unwrap();
        assert_ne!(s.blobs.blobs["keys/bob"], wrapped);
        let random = Fresh::random();
        assert_ne!((random.key, random.nonce), (Fresh::random().key, [0; 12]));
    }

    /// Purpose: destroying a person's key makes every record about them unreadable while
    /// another person's stay readable; a sealed record that is altered, cut short or moved
    /// under another name does not open; a key wrapped under a different key-encryption key is
    /// refused; and a storage outage is reported, not hidden.
    /// Why it matters: erasure on request has to reach backups, which only destroying the key
    /// can do. And a record that could be swapped or edited in storage would be worthless as
    /// evidence of what a person disclosed.
    #[test]
    fn shredding_a_key_ends_access_and_tampering_is_detected() {
        let mut s = store();
        let (ann, bob) = (PartyId::from("ann"), PartyId::from("bob"));
        let a = s.put(&ann, "disclosure", "1", b"ann's answers", fresh(1)).unwrap();
        let b = s.put(&bob, "disclosure", "1", b"bob's answers", fresh(2)).unwrap();

        // Moved under another name, altered, or cut short.
        let sealed = s.blobs.blobs["records/ann/disclosure/1"].clone();
        s.blobs.blobs.insert("records/ann/disclosure/2".into(), sealed.clone());
        assert_eq!(s.get("clinical://ann/disclosure/2", "amy", "x"), Err(ClinicalError::Tampered));
        let mut altered = sealed.clone();
        *altered.last_mut().unwrap() ^= 1;
        s.blobs.blobs.insert("records/ann/disclosure/1".into(), altered);
        assert_eq!(s.get(&a.vault_ref, "amy", "x"), Err(ClinicalError::Tampered));
        s.blobs.blobs.insert("records/ann/disclosure/1".into(), sealed[..5].to_vec());
        assert_eq!(s.get(&a.vault_ref, "amy", "x"), Err(ClinicalError::Tampered));
        s.blobs.blobs.insert("records/ann/disclosure/1".into(), sealed);
        assert!(s.get(&a.vault_ref, "amy", "x").is_ok());

        // Another vault's key-encryption key cannot unwrap, and nor can a damaged wrapped key.
        let other = ClinicalStore { vault: LocalVault::new([8; 32]), blobs: s.blobs.clone() };
        assert!(matches!(other.get(&a.vault_ref, "amy", "x"), Err(ClinicalError::Vault(_))));
        assert!(matches!(s.vault.unwrap(&[1, 2, 3]), Err(ClinicalError::Vault(_))));
        let short = Aes256Gcm::new(&[9u8; 32].into()).encrypt(Nonce::from_slice(&[0; 12]), b"short".as_slice()).unwrap();
        assert!(matches!(s.vault.unwrap(&[vec![0; 12], short].concat()), Err(ClinicalError::Vault(_))));

        s.shred(&ann).unwrap();
        assert_eq!(s.get(&a.vault_ref, "amy", "x"), Err(ClinicalError::Shredded));
        assert_eq!(s.get(&b.vault_ref, "amy", "x").unwrap().0, b"bob's answers");

        s.blobs.down = true;
        let outage = ClinicalError::Storage("unavailable".into());
        assert_eq!(s.get(&b.vault_ref, "amy", "x"), Err(outage.clone()));
        assert_eq!(s.put(&bob, "notes", "1", b"n", fresh(3)), Err(outage.clone()));
        assert_eq!(s.shred(&bob), Err(outage.clone()));
        assert_eq!(s.blobs.put("x", vec![]), Err(outage));
        assert_eq!(ClinicalError::Shredded.to_string(), "the person's key has been destroyed: their records can no longer be read");
    }
}
