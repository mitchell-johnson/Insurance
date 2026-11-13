//! Field-level encryption, payload commitments and blind indexes.
//!
//! Every party has its own data key. Everything personal about the party
//! (each event payload) is sealed under that key with AES-256-GCM, bound to
//! the party and the event's place in the stream so a sealed payload cannot
//! be moved to another party or another position. The data key is stored
//! only wrapped by a key-encryption key that never leaves Azure Key Vault
//! (see [`crate::keys`]). Destroying the wrapped key makes every payload
//! unreadable wherever a copy survives.
//!
//! The hash chain does not cover ciphertext, which changes when a party's
//! key is rotated and disappears when it is shredded. It covers a salted
//! commitment to the plaintext: SHA-256 over a random salt and the payload.
//! The salt is sealed with the payload, so after shredding nobody can test a
//! guessed value against the commitment, and the chain still verifies.
//!
//! Blind indexes are keyed hashes (HMAC-SHA-256) of normalised values such
//! as an IRD number or an email address. They allow exact lookups without
//! decrypting anything, and without the index key they reveal nothing.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

pub const KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 12;
const SALT_BYTES: usize = 16;

/// A party's data key. Wiped from memory when dropped.
pub type DataKey = Zeroizing<[u8; KEY_BYTES]>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CryptoError {
    /// The key is wrong, or the bytes were changed after they were sealed.
    #[error("the payload could not be opened with this key")]
    Unreadable,
    #[error("a key is {KEY_BYTES} bytes")]
    BadKey,
}

/// A new random data key.
pub fn new_key() -> DataKey {
    let mut key = Zeroizing::new([0u8; KEY_BYTES]);
    rand::rngs::OsRng.fill_bytes(key.as_mut());
    key
}

/// A data key from bytes the key service returned.
pub fn key_from(bytes: &[u8]) -> Result<DataKey, CryptoError> {
    let array: [u8; KEY_BYTES] = bytes.try_into().map_err(|_| CryptoError::BadKey)?;
    Ok(Zeroizing::new(array))
}

/// What is sealed: the payload and the salt of its commitment.
#[derive(Serialize, Deserialize)]
struct Sealed {
    salt: String,
    payload: serde_json::Value,
}

/// A sealed payload: the ciphertext (nonce first) and the commitment the
/// hash chain covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    pub ciphertext: Vec<u8>,
    pub commitment: [u8; 32],
}

/// The commitment to a payload under a salt.
fn commit(salt: &[u8], payload: &serde_json::Value) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"ic.customers.commitment.v1\0");
    h.update(salt);
    h.update(serde_json::to_vec(payload).unwrap_or_default());
    h.finalize().into()
}

/// The additional data a payload is bound to.
fn context(party: &str, seq: u64) -> Vec<u8> {
    format!("ic.customers.event.v1\0{party}\0{seq}").into_bytes()
}

/// Seal one event payload of `party` at `seq`.
pub fn seal(key: &DataKey, party: &str, seq: u64, payload: &serde_json::Value) -> Envelope {
    let mut salt = [0u8; SALT_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    let commitment = commit(&salt, payload);
    let plain = Zeroizing::new(
        serde_json::to_vec(&Sealed {
            salt: hex::encode(salt),
            payload: payload.clone(),
        })
        .unwrap_or_default(),
    );
    let mut nonce = [0u8; NONCE_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    // A 32-byte key always makes a cipher, and encryption cannot fail.
    let cipher = Aes256Gcm::new_from_slice(key.as_ref()).expect("a data key is 32 bytes");
    let body = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &plain,
                aad: &context(party, seq),
            },
        )
        .expect("AES-GCM encryption of an in-memory buffer cannot fail");
    Envelope {
        ciphertext: [nonce.as_slice(), &body].concat(),
        commitment,
    }
}

/// Open a payload sealed for `party` at `seq`, and check it against the
/// commitment the chain holds.
pub fn open(key: &DataKey, party: &str, seq: u64, ciphertext: &[u8], commitment: &[u8]) -> Result<serde_json::Value, CryptoError> {
    if ciphertext.len() < NONCE_BYTES {
        return Err(CryptoError::Unreadable);
    }
    let (nonce, body) = ciphertext.split_at(NONCE_BYTES);
    let cipher = Aes256Gcm::new_from_slice(key.as_ref()).map_err(|_| CryptoError::BadKey)?;
    let plain = Zeroizing::new(
        cipher
            .decrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: body,
                    aad: &context(party, seq),
                },
            )
            .map_err(|_| CryptoError::Unreadable)?,
    );
    let sealed: Sealed = serde_json::from_slice(&plain).map_err(|_| CryptoError::Unreadable)?;
    let salt = hex::decode(&sealed.salt).map_err(|_| CryptoError::Unreadable)?;
    if commit(&salt, &sealed.payload).as_slice() != commitment {
        return Err(CryptoError::Unreadable);
    }
    Ok(sealed.payload)
}

/// The key blind indexes are computed under. It is a secret from Key Vault
/// and must never change: a new key makes every index unfindable until the
/// indexes are rebuilt from the events.
#[derive(Clone)]
pub struct IndexKey(Zeroizing<Vec<u8>>);

impl IndexKey {
    pub const MIN_BYTES: usize = 32;

    pub fn new(secret: &[u8]) -> Result<Self, String> {
        if secret.len() < Self::MIN_BYTES {
            return Err(format!("the blind index key must be at least {} bytes", Self::MIN_BYTES));
        }
        Ok(Self(Zeroizing::new(secret.to_vec())))
    }

    /// The blind index of a normalised value of one kind. The kind is bound
    /// in, so an email and an IRD number never share an index.
    pub fn digest(&self, kind: &str, value: &str) -> Vec<u8> {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&self.0).expect("HMAC takes a key of any length");
        mac.update(b"ic.customers.index.v1\0");
        mac.update(kind.as_bytes());
        mac.update(&[0]);
        mac.update(value.as_bytes());
        mac.finalize().into_bytes().to_vec()
    }
}

impl IndexKey {
    /// A public name for the key: the first eight bytes of a MAC under it,
    /// in hex. It tells keys apart without saying anything about them.
    pub fn id(&self) -> String {
        hex::encode(&self.digest("key-id", "ic-customers blind index")[..8])
    }
}

impl std::fmt::Debug for IndexKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("IndexKey(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Purpose: a payload sealed for a party at a position opens again with the same key,
    /// and only there.
    /// Why it matters: sealed payloads are bound to their stream position so that one
    /// person's record can never be replayed as another's, and a wrong key or a moved
    /// payload must fail rather than return someone else's data.
    #[test]
    fn sealed_payloads_open_only_where_they_were_sealed() {
        let key = new_key();
        let payload = json!({"given_name": "Aroha"});
        let env = seal(&key, "pty_1", 3, &payload);
        assert_eq!(open(&key, "pty_1", 3, &env.ciphertext, &env.commitment).unwrap(), payload);
        assert_eq!(open(&key, "pty_2", 3, &env.ciphertext, &env.commitment), Err(CryptoError::Unreadable));
        assert_eq!(open(&key, "pty_1", 4, &env.ciphertext, &env.commitment), Err(CryptoError::Unreadable));
        assert_eq!(open(&new_key(), "pty_1", 3, &env.ciphertext, &env.commitment), Err(CryptoError::Unreadable));
        assert_eq!(open(&key, "pty_1", 3, &env.ciphertext[..5], &env.commitment), Err(CryptoError::Unreadable));
        assert_eq!(open(&key, "pty_1", 3, &env.ciphertext, &[0; 32]), Err(CryptoError::Unreadable));
    }

    /// Purpose: two seals of the same payload give different ciphertexts and commitments.
    /// Why it matters: a deterministic commitment to a date of birth could be confirmed by
    /// guessing; the random salt sealed with the payload prevents that, including after
    /// the key is shredded.
    #[test]
    fn commitments_are_salted() {
        let key = new_key();
        let payload = json!("1980-02-03");
        let a = seal(&key, "p", 1, &payload);
        let b = seal(&key, "p", 1, &payload);
        assert_ne!(a.ciphertext, b.ciphertext);
        assert_ne!(a.commitment, b.commitment);
    }

    /// Purpose: a sealed blob whose inner document is not what the sealer writes, or whose
    /// salt is not hex, is refused as unreadable.
    /// Why it matters: corrupted or forged stored data must never decode into a payload.
    #[test]
    fn malformed_inner_documents_are_unreadable() {
        let key = new_key();
        let cipher = Aes256Gcm::new_from_slice(key.as_ref()).unwrap();
        let nonce = [7u8; NONCE_BYTES];
        for inner in [b"not json".to_vec(), br#"{"salt":"zz","payload":1}"#.to_vec()] {
            let body = cipher
                .encrypt(
                    Nonce::from_slice(&nonce),
                    Payload {
                        msg: &inner,
                        aad: &context("p", 1),
                    },
                )
                .unwrap();
            let ct = [nonce.as_slice(), &body].concat();
            assert_eq!(open(&key, "p", 1, &ct, &[0; 32]), Err(CryptoError::Unreadable));
        }
    }

    /// Purpose: keys come only from 32 bytes, and the index key refuses short secrets and
    /// separates kinds.
    /// Why it matters: a truncated key from the key service must fail loudly, and an email
    /// and an IRD number with the same text must not collide in the index.
    #[test]
    fn keys_and_index_digests_are_checked() {
        assert_eq!(key_from(&[1; 31]), Err(CryptoError::BadKey));
        assert_eq!(key_from(&[1; 32]).unwrap().as_ref(), &[1u8; 32]);
        assert!(IndexKey::new(&[1; 31]).is_err());
        let k = IndexKey::new(&[9; 32]).unwrap();
        assert_eq!(k.digest("email", "a"), k.digest("email", "a"));
        assert_ne!(k.digest("email", "a"), k.digest("ird", "a"));
        assert_eq!(format!("{k:?}"), "IndexKey(..)");
    }
}
