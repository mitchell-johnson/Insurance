//! Signed steps. A disclosure, an interview confirmation and a response to
//! an offer each count only when signed by the person's device key. The
//! identity platform holds the keys; the caller supplies the public key it
//! resolved for the person, and the signature is checked here, inside the
//! state machine, so an unsigned step can never be recorded.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

/// An Ed25519 signature by a device key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedStep {
    pub key_id: String,
    /// The public key the identity platform holds for the person, base64.
    pub public_key: String,
    /// The signature over the step's message, base64.
    pub signature: String,
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
                signature: sig.into()
            }
            .verifies(&msg));
        }
    }

    /// Purpose: message parts are separated, so ("ab", "c") and ("a", "bc") sign differently.
    /// Why it matters: without a separator a signature for one proposal and life could be
    /// presented for a different pair whose IDs concatenate to the same text.
    #[test]
    fn message_parts_cannot_run_together() {
        assert_ne!(message("d", &["ab", "c"]), message("d", &["a", "bc"]));
    }
}
