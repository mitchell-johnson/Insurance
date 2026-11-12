//! Per-party data keys, wrapped in Azure Key Vault.
//!
//! A party's data key is made when the party is created, wrapped by the
//! key-encryption key in Key Vault (or Managed HSM), and only the wrapped
//! form is stored. Unwrapped keys are held in memory for a few minutes so a
//! busy party does not cost a Key Vault call per read, and are wiped when
//! they leave the cache.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ic_azure::keywrap::{KeyWrapper, WrappedKey};

use crate::crypto::{self, DataKey};
use crate::model::PartyId;

/// How long an unwrapped key is kept.
pub const CACHE_FOR: Duration = Duration::from_secs(300);
/// The most keys held at once. Past it the cache is emptied.
pub const CACHE_SIZE: usize = 50_000;

pub struct KeyRing<W> {
    wrapper: W,
    cache: Mutex<HashMap<PartyId, (DataKey, Instant)>>,
    ttl: Duration,
}

impl<W: KeyWrapper> KeyRing<W> {
    pub fn new(wrapper: W) -> Self {
        Self::with_ttl(wrapper, CACHE_FOR)
    }

    pub fn with_ttl(wrapper: W, ttl: Duration) -> Self {
        Self {
            wrapper,
            cache: Mutex::new(HashMap::new()),
            ttl,
        }
    }

    fn cache(&self) -> std::sync::MutexGuard<'_, HashMap<PartyId, (DataKey, Instant)>> {
        self.cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A new key for a party, and its wrapped form to store.
    pub async fn issue(&self, party: &PartyId) -> Result<(DataKey, WrappedKey), String> {
        let key = crypto::new_key();
        let wrapped = self
            .wrapper
            .wrap(key.as_ref())
            .await
            .map_err(|e| format!("Key Vault could not wrap a key: {e}"))?;
        self.remember(party, &key);
        Ok((key, wrapped))
    }

    /// The party's key from its wrapped form.
    pub async fn open(&self, party: &PartyId, wrapped: &WrappedKey) -> Result<DataKey, String> {
        if let Some((key, at)) = self.cache().get(party) {
            if at.elapsed() < self.ttl {
                return Ok(key.clone());
            }
        }
        let bytes = zeroize::Zeroizing::new(
            self.wrapper
                .unwrap(wrapped)
                .await
                .map_err(|e| format!("Key Vault could not unwrap a key: {e}"))?,
        );
        let key = crypto::key_from(&bytes).map_err(|e| e.to_string())?;
        self.remember(party, &key);
        Ok(key)
    }

    fn remember(&self, party: &PartyId, key: &DataKey) {
        let mut cache = self.cache();
        if cache.len() >= CACHE_SIZE {
            cache.clear();
        }
        cache.insert(party.clone(), (key.clone(), Instant::now()));
    }

    /// Drop a party's key from memory, when it is erased.
    pub fn forget(&self, party: &PartyId) {
        self.cache().remove(party);
    }

    /// Wrap a key again under the current version of the key-encryption
    /// key, after the vault key is rotated.
    pub async fn rewrap(&self, party: &PartyId, wrapped: &WrappedKey) -> Result<WrappedKey, String> {
        let key = self.open(party, wrapped).await?;
        self.wrapper
            .wrap(key.as_ref())
            .await
            .map_err(|e| format!("Key Vault could not wrap a key: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ic_azure::keywrap::DevWrapper;

    /// Purpose: an issued key opens again from its wrapped form, from the cache and after
    /// the cache has expired, and a forgotten key is no longer served from memory.
    /// Why it matters: erasure relies on the wrapped key being the only way back to the
    /// data key; the cache must not keep serving a key after the party is erased here.
    #[tokio::test]
    async fn keys_round_trip_through_the_wrapper_and_cache() {
        let ring = KeyRing::with_ttl(DevWrapper, Duration::from_millis(0));
        let p = PartyId("p".into());
        let (key, wrapped) = ring.issue(&p).await.unwrap();
        assert_eq!(ring.open(&p, &wrapped).await.unwrap(), key);
        let ring = KeyRing::new(DevWrapper);
        let (key, wrapped) = ring.issue(&p).await.unwrap();
        assert_eq!(ring.open(&p, &wrapped).await.unwrap(), key);
        ring.forget(&p);
        assert!(ring.cache().is_empty());
        assert_eq!(ring.rewrap(&p, &wrapped).await.unwrap(), wrapped);
        let bad = WrappedKey {
            key_id: "dev".into(),
            value: "AAAA".into(),
        };
        assert!(ring.open(&PartyId("q".into()), &bad).await.is_err());
        let worse = WrappedKey {
            key_id: "dev".into(),
            value: "!".into(),
        };
        assert!(ring.open(&PartyId("q".into()), &worse).await.is_err());
    }

    /// Purpose: the cache empties itself when full instead of growing.
    /// Why it matters: a long-running service reading millions of parties must not hold
    /// millions of unwrapped keys in memory.
    #[tokio::test]
    async fn the_cache_is_bounded() {
        let ring = KeyRing::new(DevWrapper);
        let key = crypto::new_key();
        for i in 0..=CACHE_SIZE {
            ring.remember(&PartyId(i.to_string()), &key);
        }
        assert_eq!(ring.cache().len(), 1);
    }
}
