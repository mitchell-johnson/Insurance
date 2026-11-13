//! Signed steps. A disclosure, an interview confirmation and a response to
//! an offer each count only when signed by the person's device key. The
//! identity platform holds the keys; the caller supplies the public key it
//! resolved for the person, and the signature is checked here, inside the
//! state machine, so an unsigned step can never be recorded.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

/// The kind of device key.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Alg {
    #[default]
    Ed25519,
    /// ECDSA P-256 with SHA-256: the keys the channel identity module
    /// enrols for the portal and the app.
    EcdsaP256,
}

impl Alg {
    fn is_default(&self) -> bool {
        *self == Alg::Ed25519
    }
}

/// A signature by a device key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedStep {
    pub key_id: String,
    /// The public key the identity platform holds for the person, base64.
    /// Ed25519: the 32-byte key. P-256: the uncompressed point, or the
    /// SubjectPublicKeyInfo that holds it.
    pub public_key: String,
    /// The signature over the step's message, base64. P-256: ASN.1 DER or
    /// the fixed 64-byte form.
    pub signature: String,
    /// Absent on every step recorded before P-256 keys were taken.
    #[serde(default, skip_serializing_if = "Alg::is_default")]
    pub alg: Alg,
}

/// The SubjectPublicKeyInfo header of an uncompressed P-256 public key.
const P256_SPKI_HEADER: [u8; 26] = [
    0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
];

fn verifies_p256(key: &[u8], message: &[u8], sig: &[u8]) -> bool {
    use ring::signature::{UnparsedPublicKey, ECDSA_P256_SHA256_ASN1, ECDSA_P256_SHA256_FIXED};
    let point = key.strip_prefix(&P256_SPKI_HEADER[..]).unwrap_or(key);
    UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, point).verify(message, sig).is_ok()
        || UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, point).verify(message, sig).is_ok()
}

/// The bytes a person signs for a step. Domain separated, so a signature on
/// one kind of step can never be replayed as another.
pub fn message(domain: &str, parts: &[&str]) -> Vec<u8> {
    let mut m = format!("ic.underwriting.{domain}.v1").into_bytes();
    for p in parts {
        m.push(0);
        m.extend_from_slice(p.as_bytes());
    }
    m
}

impl SignedStep {
    /// Whether this is a valid signature over `message`.
    pub fn verifies(&self, message: &[u8]) -> bool {
        let Ok(key) = STANDARD.decode(&self.public_key) else {
            return false;
        };
        let Ok(sig) = STANDARD.decode(&self.signature) else {
            return false;
        };
        if self.alg == Alg::EcdsaP256 {
            return verifies_p256(&key, message, &sig);
        }
        let (Ok(key), Ok(sig)) = (<[u8; 32]>::try_from(key.as_slice()), <[u8; 64]>::try_from(sig.as_slice())) else {
            return false;
        };
        let Ok(key) = VerifyingKey::from_bytes(&key) else {
            return false;
        };
        key.verify(message, &Signature::from_bytes(&sig)).is_ok()
    }
}

/// Sign a step with a device key. Used by tests, by the simulator and by
/// tools that stand in for a device; a real device signs in its own secure
/// hardware.
pub fn sign(key_id: &str, secret: &[u8; 32], message: &[u8]) -> SignedStep {
    use ed25519_dalek::{Signer, SigningKey};
    let key = SigningKey::from_bytes(secret);
    SignedStep {
        key_id: key_id.to_string(),
        public_key: STANDARD.encode(key.verifying_key().as_bytes()),
        signature: STANDARD.encode(key.sign(message).to_bytes()),
        alg: Alg::Ed25519,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Purpose: a signature verifies over its own message and fails over any other message, under
    /// another key, or when the key or signature is not valid base64 of the right length.
    /// Why it matters: the disclosure record is evidence at claim time. If a tampered or unsigned
    /// record could pass, the insurer could not rely on it and the customer could be held to
    /// answers they never gave.
    #[test]
    fn only_a_true_signature_over_the_same_message_verifies() {
        let msg = message("disclosure", &["p1", "l1", "abc"]);
        let step = sign("device-1", &[7; 32], &msg);
        assert!(step.verifies(&msg));
        assert!(!step.verifies(&message("disclosure", &["p1", "l1", "abd"])));
        assert!(!step.verifies(&message("offer", &["p1", "l1", "abc"])));
        let other = sign("device-2", &[8; 32], &msg);
        assert!(!SignedStep {
            public_key: other.public_key.clone(),
            ..step.clone()
        }
        .verifies(&msg));
        for (key, sig) in [
            ("not base64!", step.signature.as_str()),
            (step.public_key.as_str(), "not base64!"),
            ("AAAA", step.signature.as_str()),
            (step.public_key.as_str(), "AAAA"),
        ] {
            assert!(!SignedStep {
                key_id: "k".into(),
                public_key: key.into(),
                signature: sig.into(),
                alg: Alg::Ed25519,
            }
            .verifies(&msg));
        }
        // Thirty-two bytes that are not a point on the curve are not a key.
        for y in 2u8..20 {
            assert!(!SignedStep {
                public_key: STANDARD.encode([y; 32]),
                ..step.clone()
            }
            .verifies(&msg));
        }
    }

    /// Purpose: a P-256 device key, given as its uncompressed point or as SubjectPublicKeyInfo,
    /// verifies its signature in ASN.1 or fixed form over the same message and no other; the same
    /// key and signature marked Ed25519 do not verify; and a step without `alg` reads as Ed25519
    /// and is written without it.
    /// Why it matters: the portal and the app enrol P-256 keys, so without this no customer could
    /// sign a disclosure. Steps already in the log carry no `alg`, and their records must read and
    /// hash as they did.
    #[test]
    fn a_p256_device_key_signs_in_either_encoding() {
        use ring::rand::SystemRandom;
        use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING, ECDSA_P256_SHA256_FIXED_SIGNING};
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
        let msg = message("disclosure", &["p1", "l1", "abc"]);
        for alg in [&ECDSA_P256_SHA256_ASN1_SIGNING, &ECDSA_P256_SHA256_FIXED_SIGNING] {
            let pair = EcdsaKeyPair::from_pkcs8(alg, pkcs8.as_ref(), &rng).unwrap();
            let point = pair.public_key().as_ref().to_vec();
            let mut spki = P256_SPKI_HEADER.to_vec();
            spki.extend_from_slice(&point);
            let signature = STANDARD.encode(pair.sign(&rng, &msg).unwrap().as_ref());
            for key in [&point, &spki] {
                let step = SignedStep {
                    key_id: "device-1".into(),
                    public_key: STANDARD.encode(key),
                    signature: signature.clone(),
                    alg: Alg::EcdsaP256,
                };
                assert!(step.verifies(&msg));
                assert!(!step.verifies(&message("disclosure", &["p1", "l1", "abd"])));
                assert!(!SignedStep {
                    alg: Alg::Ed25519,
                    ..step.clone()
                }
                .verifies(&msg));
                assert_eq!(serde_json::to_value(&step).unwrap()["alg"], "ecdsa-p256");
            }
        }
        let old: SignedStep = serde_json::from_value(serde_json::json!({"key_id": "k", "public_key": "", "signature": ""})).unwrap();
        assert_eq!(old.alg, Alg::Ed25519);
        assert!(serde_json::to_value(&old).unwrap().get("alg").is_none());
    }

    /// Purpose: message parts are separated, so ("ab", "c") and ("a", "bc") sign differently.
    /// Why it matters: without a separator a signature for one proposal and life could be
    /// presented for a different pair whose IDs concatenate to the same text.
    #[test]
    fn message_parts_cannot_run_together() {
        assert_ne!(message("d", &["ab", "c"]), message("d", &["a", "bc"]));
    }
}
