//! Device-bound identity: enrolment, proof of possession on each request,
//! and transaction signing against a single-use challenge.

mod common;

use common::{code, p, Key};
use ic_channels::identity::*;
use ic_channels::staff::At;

const T0: At = 1_800_000_000;

fn enrol(device: &str, party: &str, key: &Key, enrolment: Enrolment) -> IdentityCommand {
    IdentityCommand::EnrolDevice {
        device: device.to_string(),
        party: p(party),
        public_key: key.public.clone(),
        hardware_backed: true,
        enrolment,
    }
}

fn verified() -> Enrolment {
    Enrolment::Verification {
        reference: "idv-123".to_string(),
    }
}

/// One party with one enrolled phone.
fn enrolled() -> (IdentityState, Key) {
    let mut s = IdentityState::default();
    let key = Key::new();
    s.apply(T0, enrol("phone", "kiri", &key, verified())).unwrap();
    (s, key)
}

fn issue(id: &str, device: &str, hash: &str, ttl: i64) -> IdentityCommand {
    IdentityCommand::IssueChallenge {
        id: id.to_string(),
        device: device.to_string(),
        action_hash: hash.to_string(),
        nonce: "0123456789abcdef".to_string(),
        ttl_secs: ttl,
    }
}

/// Purpose: a first device is enrolled only after identity verification and only with a real P-256 public key.
/// Why it matters: enrolment is the root of device trust; a device enrolled without verification or with a malformed key would let anyone bind a key to a customer.
#[test]
fn first_device_needs_verification_and_a_real_key() {
    let mut s = IdentityState::default();
    let key = Key::new();
    let blank = Enrolment::Verification { reference: "  ".to_string() };
    assert_eq!(code(s.apply(T0, enrol("phone", "kiri", &key, blank))), "unverified");
    for bad in ["not base64!", "AAAA", &key.public.replacen('B', "A", 1)[..40]] {
        let cmd = IdentityCommand::EnrolDevice {
            device: "phone".into(),
            party: p("kiri"),
            public_key: bad.to_string(),
            hardware_backed: true,
            enrolment: verified(),
        };
        assert_eq!(code(s.apply(T0, cmd)), "bad_key");
    }
    assert!(s.devices.is_empty(), "a rejected enrolment leaves nothing behind");

    let events = s.apply(T0, enrol("phone", "kiri", &key, verified())).unwrap();
    assert_eq!(
        events,
        vec![IdentityEvent::DeviceEnrolled {
            device: "phone".into(),
            party: p("kiri")
        }]
    );
    assert_eq!(s.devices["phone"].enrolled, T0);
    assert!(s.by_party[&p("kiri")].contains("phone"));
    assert_eq!(code(s.apply(T0, enrol("phone", "kiri", &key, verified()))), "duplicate");
    assert!(s.active_device("phone", &p("kiri")).is_some());
    assert!(s.active_device("phone", &p("mallory")).is_none(), "a device belongs to one party");
}

/// Purpose: a second device is enrolled by a signature from a device the same party already holds, over exactly the new device and key.
/// Why it matters: this is how a customer moves to a new phone without re-verifying; if the signature were not bound to the new key, or another party's device could approve, an attacker could add their own device.
#[test]
fn a_second_device_is_approved_by_an_existing_one() {
    let (mut s, phone) = enrolled();
    let laptop = Key::new();
    let message = format!("enrol\nlaptop\n{}", laptop.public);
    let by = |device: &str, signature: String| Enrolment::ExistingDevice {
        device: device.to_string(),
        signature,
    };

    // Signed over a different key: refused.
    let other = Key::new();
    let wrong = phone.sign(format!("enrol\nlaptop\n{}", other.public).as_bytes());
    assert_eq!(code(s.apply(T0, enrol("laptop", "kiri", &laptop, by("phone", wrong)))), "bad_signature");
    // Signed by a key that is not the approving device's.
    assert_eq!(
        code(s.apply(T0, enrol("laptop", "kiri", &laptop, by("phone", other.sign(message.as_bytes()))))),
        "bad_signature"
    );
    // The approving device must be the same party's.
    assert_eq!(
        code(s.apply(T0, enrol("laptop", "mallory", &laptop, by("phone", phone.sign(message.as_bytes()))))),
        "unknown_device"
    );
    assert_eq!(code(s.apply(T0, enrol("laptop", "kiri", &laptop, by("tablet", "x".into())))), "unknown_device");

    s.apply(T0 + 5, enrol("laptop", "kiri", &laptop, by("phone", phone.sign_asn1(message.as_bytes()))))
        .unwrap();
    assert_eq!(s.by_party[&p("kiri")].len(), 2);

    // A revoked device approves nothing.
    s.apply(T0 + 6, IdentityCommand::RevokeDevice { device: "phone".into() }).unwrap();
    let tablet = Key::new();
    let m = format!("enrol\ntablet\n{}", tablet.public);
    assert_eq!(
        code(s.apply(T0 + 7, enrol("tablet", "kiri", &tablet, by("phone", phone.sign(m.as_bytes()))))),
        "unknown_device"
    );
}

/// Purpose: both signature encodings verify, and anything malformed or signed over other bytes does not.
/// Why it matters: browsers and phone keystores encode ECDSA signatures differently; rejecting one locks out a whole channel, and accepting garbage defeats device binding.
#[test]
fn signatures_verify_in_both_encodings_only() {
    let key = Key::new();
    assert!(verify(&key.public, b"hello", &key.sign(b"hello")));
    assert!(verify(&key.public, b"hello", &key.sign_asn1(b"hello")));
    assert!(!verify(&key.public, b"hullo", &key.sign(b"hello")));
    assert!(!verify(&key.public, b"hello", "not base64!"));
    assert!(!verify("not base64!", b"hello", &key.sign(b"hello")));
    assert!(!verify(&Key::new().public, b"hello", &key.sign(b"hello")));
    // URL-safe base64 without padding, as WebAuthn clients send, is accepted.
    let url = key.sign(b"hello").replace('+', "-").replace('/', "_").trim_end_matches('=').to_string();
    assert!(verify(&key.public, b"hello", &url));
}

/// Purpose: a request proof is good only for the enrolled device's key, the exact request, and a short window either side of the server's clock.
/// Why it matters: proof of possession is what stops a stolen token being replayed from another machine or against another endpoint.
#[test]
fn request_proof_binds_the_device_the_request_and_the_time() {
    let (mut s, key) = enrolled();
    let kiri = p("kiri");
    let body = br#"{"x":1}"#;
    let proof = |ts: At, method: &str, path: &str, body: &[u8]| format!("{ts}.{}", key.sign(&proof_message(ts, method, path, body)));

    let good = proof(T0, "POST", "/v1/requests", body);
    assert!(s.check_proof(&kiri, "phone", &good, T0, "POST", "/v1/requests", body));
    assert!(s.check_proof(&kiri, "phone", &good, T0 + PROOF_WINDOW_SECS, "POST", "/v1/requests", body));
    assert!(s.check_proof(&kiri, "phone", &good, T0 - PROOF_WINDOW_SECS, "POST", "/v1/requests", body));
    assert!(
        !s.check_proof(&kiri, "phone", &good, T0 + PROOF_WINDOW_SECS + 1, "POST", "/v1/requests", body),
        "too old"
    );
    assert!(
        !s.check_proof(&kiri, "phone", &good, T0 - PROOF_WINDOW_SECS - 1, "POST", "/v1/requests", body),
        "from the future"
    );
    assert!(!s.check_proof(&kiri, "phone", &good, T0, "GET", "/v1/requests", body), "another method");
    assert!(!s.check_proof(&kiri, "phone", &good, T0, "POST", "/v1/other", body), "another path");
    assert!(!s.check_proof(&kiri, "phone", &good, T0, "POST", "/v1/requests", b"{}"), "another body");
    assert!(!s.check_proof(&p("mallory"), "phone", &good, T0, "POST", "/v1/requests", body), "another party");
    assert!(!s.check_proof(&kiri, "tablet", &good, T0, "POST", "/v1/requests", body), "unknown device");
    assert!(!s.check_proof(&kiri, "phone", "no-dot", T0, "POST", "/v1/requests", body));
    assert!(!s.check_proof(&kiri, "phone", "soon.sig", T0, "POST", "/v1/requests", body));

    s.apply(T0 + 1, IdentityCommand::RevokeDevice { device: "phone".into() }).unwrap();
    assert!(
        !s.check_proof(&kiri, "phone", &good, T0, "POST", "/v1/requests", body),
        "a revoked device proves nothing"
    );
}

/// Purpose: a challenge names one action, lives a bounded time, and is consumed by its first valid redemption.
/// Why it matters: transaction signing is the control on risky actions such as changing a bank account; a challenge that could be reused, redeemed for another action or kept alive would let one approval authorise many.
#[test]
fn a_challenge_signs_one_action_once() {
    let (mut s, key) = enrolled();
    let hash = action_hash(&serde_json::json!({ "kind": "change_bank_account", "policy": "pol-1" }));
    assert_eq!(hash.len(), 64);
    assert_ne!(hash, action_hash(&serde_json::json!({ "kind": "change_bank_account", "policy": "pol-2" })));

    assert_eq!(code(s.apply(T0, issue("c1", "tablet", &hash, 60))), "unknown_device");
    assert_eq!(code(s.apply(T0, issue("c1", "phone", "short", 60))), "bad_challenge");
    let short_nonce = IdentityCommand::IssueChallenge {
        id: "c1".into(),
        device: "phone".into(),
        action_hash: hash.clone(),
        nonce: "short".into(),
        ttl_secs: 60,
    };
    assert_eq!(code(s.apply(T0, short_nonce)), "bad_challenge");

    let events = s.apply(T0, issue("c1", "phone", &hash, 60)).unwrap();
    let IdentityEvent::ChallengeIssued { challenge } = &events[0] else {
        panic!("expected a challenge")
    };
    assert_eq!(challenge.expires, T0 + 60);
    assert_eq!(code(s.apply(T0, issue("c1", "phone", &hash, 60))), "duplicate");
    let signature = key.sign(&challenge_message(challenge));

    let redeem = |id: &str, hash: &str, signature: &str| IdentityCommand::RedeemChallenge {
        id: id.to_string(),
        action_hash: hash.to_string(),
        signature: signature.to_string(),
    };
    let other = action_hash(&serde_json::json!({ "kind": "cancel_policy" }));
    assert_eq!(code(s.apply(T0 + 1, redeem("c1", &other, &signature))), "wrong_action");
    assert_eq!(
        code(s.apply(T0 + 1, redeem("c1", &hash, &Key::new().sign(&challenge_message(challenge))))),
        "bad_signature"
    );
    assert_eq!(code(s.apply(T0 + 1, redeem("nope", &hash, &signature))), "no_challenge");

    let events = s.apply(T0 + 2, redeem("c1", &hash, &signature)).unwrap();
    assert_eq!(
        events,
        vec![IdentityEvent::ActionSigned {
            device: "phone".into(),
            party: p("kiri"),
            action_hash: hash.clone()
        }]
    );
    assert_eq!(code(s.apply(T0 + 3, redeem("c1", &hash, &signature))), "no_challenge", "used once");

    // The lifetime is capped, and an expired challenge is gone.
    let events = s.apply(T0 + 10, issue("c2", "phone", &hash, 86_400)).unwrap();
    let IdentityEvent::ChallengeIssued { challenge } = &events[0] else {
        panic!("expected a challenge")
    };
    assert_eq!(challenge.expires, T0 + 10 + MAX_CHALLENGE_SECS);
    let signature = key.sign(&challenge_message(challenge));
    assert_eq!(code(s.apply(challenge.expires, redeem("c2", &hash, &signature))), "no_challenge");
    let events = s.apply(T0 + 400, issue("c3", "phone", &hash, -5)).unwrap();
    let IdentityEvent::ChallengeIssued { challenge } = &events[0] else {
        panic!("expected a challenge")
    };
    assert_eq!(challenge.expires, T0 + 401, "a challenge lives at least a second");
}

/// Purpose: revoking a device or all of a party's sessions takes effect at once and cancels challenges in flight.
/// Why it matters: a lost phone or a compromised account must be cut off immediately, including an approval already on its screen.
#[test]
fn revocation_is_immediate_and_cancels_challenges() {
    let (mut s, key) = enrolled();
    let hash = action_hash(&serde_json::json!({ "kind": "x" }));
    let events = s.apply(T0, issue("c1", "phone", &hash, 60)).unwrap();
    let IdentityEvent::ChallengeIssued { challenge } = &events[0] else {
        panic!("expected a challenge")
    };
    let signature = key.sign(&challenge_message(challenge));

    // Sessions: tokens issued up to the revocation are refused, later ones accepted.
    assert!(s.session_valid(&p("kiri"), T0));
    let events = s.apply(T0 + 5, IdentityCommand::RevokeSessions { party: p("kiri") }).unwrap();
    assert_eq!(events, vec![IdentityEvent::SessionsRevoked { party: p("kiri") }]);
    assert!(!s.session_valid(&p("kiri"), T0 + 5));
    assert!(s.session_valid(&p("kiri"), T0 + 6));
    assert!(s.session_valid(&p("other"), T0));
    let redeem = IdentityCommand::RedeemChallenge {
        id: "c1".into(),
        action_hash: hash.clone(),
        signature: signature.clone(),
    };
    assert_eq!(code(s.apply(T0 + 6, redeem)), "no_challenge");

    // Device: the device is told to wipe, and cannot be challenged or revoked again.
    s.apply(T0 + 7, issue("c2", "phone", &hash, 60)).unwrap();
    let events = s.apply(T0 + 8, IdentityCommand::RevokeDevice { device: "phone".into() }).unwrap();
    assert_eq!(
        events,
        vec![IdentityEvent::DeviceRevoked {
            device: "phone".into(),
            party: p("kiri")
        }]
    );
    assert!(s.challenges.is_empty());
    assert_eq!(s.devices["phone"].revoked, Some(T0 + 8));
    assert_eq!(code(s.apply(T0 + 9, issue("c3", "phone", &hash, 60))), "unknown_device");
    assert_eq!(
        code(s.apply(T0 + 9, IdentityCommand::RevokeDevice { device: "phone".into() })),
        "unknown_device"
    );
    // Revoking the sessions of a party with no devices is still recorded.
    s.apply(T0 + 10, IdentityCommand::RevokeSessions { party: p("nobody") }).unwrap();
    assert!(!s.session_valid(&p("nobody"), T0 + 10));
}

/// Purpose: a device revoked between a challenge being issued and redeemed cannot redeem it, even if the challenge survived.
/// Why it matters: defence in depth on the signing path; the redemption re-checks the device rather than trusting that revocation cleaned up.
#[test]
fn a_challenge_for_a_device_since_revoked_is_refused() {
    let (mut s, key) = enrolled();
    let hash = action_hash(&serde_json::json!({ "kind": "x" }));
    let events = s.apply(T0, issue("c1", "phone", &hash, 60)).unwrap();
    let IdentityEvent::ChallengeIssued { challenge } = &events[0] else {
        panic!("expected a challenge")
    };
    let signature = key.sign(&challenge_message(challenge));
    // Simulate a state restored with the challenge present and the device revoked.
    s.devices.get_mut("phone").unwrap().revoked = Some(T0 + 1);
    let redeem = IdentityCommand::RedeemChallenge {
        id: "c1".into(),
        action_hash: hash,
        signature,
    };
    assert_eq!(code(s.apply(T0 + 2, redeem)), "unknown_device");
}
