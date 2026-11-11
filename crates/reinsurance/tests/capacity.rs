//! The capacity step: what is already held on a life, and whether the treaty
//! takes a new cover automatically, asked before any terms are offered.

mod common;

use common::*;
use ic_core::{LifeId, PartyId, PolicyId};
use ic_reinsurance::capacity::{CapacityRequest, FacultativeStatus, Outcome, ReservationStatus};
use ic_reinsurance::machine::{FacTarget, FacultativeTerms};
use ic_reinsurance::{Command, Notice};

fn request(id: &str, party: &str, sum: i64, commencement: chrono::NaiveDate) -> CapacityRequest {
    CapacityRequest {
        reservation_key: id.to_string(),
        party: PartyId::from(party),
        product_code: "term-life".into(),
        cover_code: "life".into(),
        cover_type: "life".into(),
        sum_insured_minor: sum,
        commencement,
        proposal_expires: d(2027, 3, 31),
        risk_codes: vec![],
    }
}

fn reserve(b: &mut Bench, id: &str, party: &str, sum: i64, commencement: chrono::NaiveDate) {
    b.ok("underwriting", Command::ReserveCapacity(Box::new(request(id, party, sum, commencement))));
}

fn terms(target: FacTarget, reference: &str, amount: i64) -> Command {
    Command::RecordFacultativeTerms(Box::new(FacultativeTerms {
        target,
        reference: reference.to_string(),
        reinsurer: south(),
        amount_minor: amount,
        rate: 2_000_000,
        terms_ref: format!("vault/{reference}"),
        loading: Some("+50".into()),
        exclusions: vec![],
    }))
}

/// Purpose: a proposal on a life the insurer already covers is told what is held, how the new
/// cover would split, and the event sequences the answer reflects, without changing any
/// recorded cession.
/// Why it matters: the answer is given before terms are offered. An underwriter who is not
/// shown existing cover on another partition would offer terms the treaty cannot support.
#[test]
fn a_reservation_sees_cover_already_held_on_the_life() {
    let mut b = Bench::new();
    b.issue(3, "P1", "anna", vec![layer("life", "life", 1, "L1", 300 * K, d(2027, 2, 1))]);
    let before = b.ri.cession_record(&key("P1", "life", 1, "L1")).to_vec();
    let applied = b.send(
        "underwriting",
        Command::ReserveCapacity(Box::new(request("R1", "anna", 400 * K, d(2027, 3, 1)))),
    );
    assert_eq!(applied.result, Ok(()));
    assert!(applied.instructions.is_empty(), "a reservation is not a cession");
    let answer = b.ri.capacity("R1").unwrap();
    assert_eq!(
        answer.outcome,
        Outcome::Automatic {
            retained_minor: 200 * K,
            ceded_minor: 200 * K
        }
    );
    assert_eq!(answer.held.retained_minor, 300 * K);
    assert_eq!(answer.treaty.unwrap().to_string(), "retail-life-surplus@1");
    assert_eq!(answer.life_index_sequence.get(&3), Some(&1));
    assert_eq!(b.ri.cession_record(&key("P1", "life", 1, "L1")), &before[..]);
    assert_eq!(b.ri.reservation("R1").unwrap().status, ReservationStatus::Held);
    assert_eq!(b.ri.facultative_terms("R1"), Some(&FacultativeStatus::Pending));
    assert!(b.ri.capacity("nope").is_none() && b.ri.facultative_terms("nope").is_none());
    b.assert_reconciles();
}

/// Purpose: two open proposals on the same life see each other, and the one that would take
/// the life over the automatic limit is referred for facultative terms with the exact excess.
/// Why it matters: without reservations two proposals in flight would each be told the treaty
/// has room, and both would be issued on capacity that exists once.
#[test]
fn two_proposals_on_one_life_see_each_other() {
    let mut b = Bench::new();
    reserve(&mut b, "R1", "anna", 400 * K, d(2027, 3, 1));
    reserve(&mut b, "R2", "anna", 5000 * K, d(2027, 3, 2));
    let answer = b.ri.capacity("R2").unwrap();
    assert_eq!(
        answer.outcome,
        Outcome::FacultativeRequired {
            reason_code: "above_automatic_limit".into(),
            retained_minor: 100 * K,
            excess_minor: 900 * K
        }
    );
    assert_eq!(answer.held.reserved_minor, 400 * K);
    // The earlier commencement keeps its place whichever proposal asked first.
    assert_eq!(
        b.ri.capacity("R1").unwrap().outcome,
        Outcome::Automatic {
            retained_minor: 400 * K,
            ceded_minor: 0
        }
    );
}

/// Purpose: the whole path of a referred proposal: terms recorded against the reservation, the
/// underwriter notified, the cover issued and confirmed, and the reinsurer's terms carried to
/// the issued layer whichever of the confirmation and the core's event arrives first.
/// Why it matters: this is the hand-off between underwriting, the core and reinsurance. If the
/// terms were lost between reservation and layer, cover would be issued over the limit with
/// nothing behind it.
#[test]
fn facultative_terms_follow_the_proposal_to_the_issued_layer() {
    for confirm_first in [true, false] {
        let mut b = Bench::new();
        reserve(&mut b, "R2", "anna", 5000 * K, d(2027, 3, 2));
        let applied = b.ok("reinsurance-ops", terms(FacTarget::Reservation("R2".into()), "FAC-1", 500 * K));
        assert_eq!(applied.notices, vec![Notice::FacultativeTermsRecorded { reservation_id: "R2".into() }]);
        assert!(matches!(b.ri.facultative_terms("R2"), Some(FacultativeStatus::Offered { loading: Some(l), .. }) if l == "+50"));
        assert_eq!(
            b.ri.capacity("R2").unwrap().outcome,
            Outcome::Automatic {
                retained_minor: 500 * K,
                ceded_minor: 4500 * K
            }
        );
        let confirm = Command::ConfirmCapacity {
            reservation_id: "R2".into(),
            policy: PolicyId::from("P5"),
            cover: "life".into(),
            layers: vec![1],
        };
        b.today = d(2027, 3, 2);
        if confirm_first {
            b.ok("new-business", confirm.clone());
            // Until the layer arrives the reservation stands in for it.
            assert!(b.ri.capacity("R2").is_some());
            b.issue(4, "P5", "anna", vec![layer("life", "life", 1, "L1", 5000 * K, d(2027, 3, 2))]);
        } else {
            b.issue(4, "P5", "anna", vec![layer("life", "life", 1, "L1", 5000 * K, d(2027, 3, 2))]);
            assert!(b.ri.exceptions().contains_key("over_retention:P5/life/1/L1"));
            b.ok("new-business", confirm.clone());
        }
        let c = b.ri.cession(&key("P5", "life", 1, "L1"), d(2027, 3, 2)).unwrap();
        assert_eq!(
            (c.retained_minor, c.treaty_ceded(), c.facultative_ceded(), c.unplaced_minor),
            (500 * K, 4000 * K, 500 * K, 0),
            "confirm_first={confirm_first}"
        );
        assert_eq!(c.facultative[0].reference, "FAC-1");
        assert!(b.ri.exceptions().is_empty());
        assert!(b.ri.capacity("R2").is_none(), "the reservation has left the accumulation");
        // The offer's loading was needed only while the proposal was open.
        assert!(matches!(b.ri.facultative_terms("R2"), Some(FacultativeStatus::Offered { loading: None, .. })));
        // Confirming again is harmless; confirming against another cover is refused.
        b.ok("new-business", confirm);
        let other = Command::ConfirmCapacity {
            reservation_id: "R2".into(),
            policy: PolicyId::from("P6"),
            cover: "life".into(),
            layers: vec![1],
        };
        assert_eq!(b.refused("new-business", other), "reservation_confirmed");
        assert_eq!(
            b.refused(
                "underwriting",
                Command::ReleaseCapacity {
                    reservation_id: "R2".into(),
                    reason: "x".into()
                }
            ),
            "reservation_confirmed"
        );
        b.assert_reconciles();
    }
}

/// Purpose: a risk the treaty excludes is answered as excluded, needing facultative terms for
/// the whole cover, and stays excluded on the issued layer until terms are recorded.
/// Why it matters: an excluded risk ceded automatically is a recovery the reinsurer will
/// refuse at claim time, which is the worst moment to find out.
#[test]
fn an_excluded_risk_needs_terms_for_the_whole_cover() {
    let mut b = Bench::empty();
    let source = RETAIL.replace("recapture_after_years = 10,", "recapture_after_years = 10,\n    exclusions = [\"aviation\"],");
    b.ok(
        "config",
        Command::ActivateTreaty {
            treaty: Box::new(ic_reinsurance::loader::load_treaty("t.star", &source).unwrap()),
        },
    );
    let mut r = request("R1", "anna", 600 * K, d(2027, 3, 1));
    r.risk_codes = vec!["aviation".into()];
    b.ok("underwriting", Command::ReserveCapacity(Box::new(r)));
    assert_eq!(
        b.ri.capacity("R1").unwrap().outcome,
        Outcome::Excluded {
            reason_code: "excluded_risk".into()
        }
    );
    b.ok(
        "new-business",
        Command::ConfirmCapacity {
            reservation_id: "R1".into(),
            policy: PolicyId::from("P1"),
            cover: "life".into(),
            layers: vec![1, 2],
        },
    );
    b.today = d(2027, 3, 1);
    b.issue(1, "P1", "anna", vec![layer("life", "life", 1, "L1", 600 * K, d(2027, 3, 1))]);
    let k = key("P1", "life", 1, "L1");
    assert_eq!(b.ri.cession(&k, d(2027, 3, 1)).unwrap().unplaced_minor, 600 * K);
    // Layer 2 has not arrived, so the reservation still stands in for it.
    assert!(b.ri.capacity("R1").is_some());
    // Terms given later for the cover itself apply from the day they are recorded.
    b.today = d(2027, 3, 10);
    b.ok("reinsurance-ops", terms(FacTarget::Risk(k.clone()), "FAC-9", 600 * K));
    assert_eq!(b.ri.cession(&k, d(2027, 3, 9)).unwrap().unplaced_minor, 600 * K);
    let c = b.ri.cession(&k, d(2027, 3, 10)).unwrap();
    assert_eq!((c.facultative_ceded(), c.unplaced_minor), (600 * K, 0));
    assert!(b.ri.exceptions().is_empty());
    assert_eq!(b.refused("reinsurance-ops", terms(FacTarget::Risk(k), "FAC-9", 1)), "duplicate_reference");
    b.assert_reconciles();
}

/// Purpose: a reservation is released by command or when its proposal expires, and released
/// capacity is free for the next proposal.
/// Why it matters: a reservation that never let go would block capacity on a life for ever
/// after a proposal was abandoned.
#[test]
fn released_and_expired_reservations_free_the_capacity() {
    let mut b = Bench::new();
    reserve(&mut b, "R1", "anna", 4500 * K, d(2027, 3, 1));
    reserve(&mut b, "R2", "anna", 4500 * K, d(2027, 3, 2));
    assert!(matches!(b.ri.capacity("R2").unwrap().outcome, Outcome::FacultativeRequired { .. }));
    b.ok("reinsurance-ops", terms(FacTarget::Reservation("R1".into()), "FAC-1", K));
    let applied = b.ok(
        "underwriting",
        Command::ReleaseCapacity {
            reservation_id: "R1".into(),
            reason: "withdrawn".into(),
        },
    );
    assert_eq!(
        applied.notices,
        vec![Notice::CapacityReleased {
            reservation_id: "R1".into(),
            reason: "withdrawn".into()
        }]
    );
    assert!(matches!(b.ri.capacity("R2").unwrap().outcome, Outcome::Automatic { .. }));
    assert!(matches!(b.ri.facultative_terms("R1"), Some(FacultativeStatus::Offered { loading: None, .. })));
    // Releasing twice is harmless, and nothing else can be done with it.
    assert!(b
        .ok(
            "underwriting",
            Command::ReleaseCapacity {
                reservation_id: "R1".into(),
                reason: "again".into()
            }
        )
        .notices
        .is_empty());
    let confirm = Command::ConfirmCapacity {
        reservation_id: "R1".into(),
        policy: PolicyId::from("P1"),
        cover: "life".into(),
        layers: vec![1],
    };
    assert_eq!(b.refused("new-business", confirm), "reservation_released");
    assert_eq!(
        b.refused("reinsurance-ops", terms(FacTarget::Reservation("R1".into()), "FAC-2", K)),
        "reservation_not_held"
    );
    // R2 expires on 31 March. The first command on a later day releases it.
    b.today = d(2027, 3, 31);
    assert!(b
        .ok(
            "group-admin",
            Command::ClearPossibleMatch {
                policy: PolicyId::from("x"),
                life: LifeId::from("x"),
                party: PartyId::from("x")
            }
        )
        .notices
        .is_empty());
    b.today = d(2027, 4, 1);
    let applied = b.ok(
        "group-admin",
        Command::ClearPossibleMatch {
            policy: PolicyId::from("x"),
            life: LifeId::from("x"),
            party: PartyId::from("x"),
        },
    );
    assert_eq!(
        applied.notices,
        vec![Notice::CapacityReleased {
            reservation_id: "R2".into(),
            reason: "expired".into()
        }]
    );
    assert_eq!(b.ri.reservation("R2").unwrap().status, ReservationStatus::Released { reason: "expired".into() });
    b.assert_reconciles();
}

/// Purpose: every malformed or out-of-order capacity command is refused with its own code and
/// leaves the state untouched.
/// Why it matters: these commands come from other modules over an at-least-once channel. A
/// refusal that half-applied would leave a reservation the sender does not know it holds.
#[test]
fn malformed_capacity_commands_are_refused_whole() {
    let mut b = Bench::new();
    reserve(&mut b, "R1", "anna", 400 * K, d(2027, 3, 1));
    // The same request again is the same reservation.
    reserve(&mut b, "R1", "anna", 400 * K, d(2027, 3, 1));
    let again = Command::ReserveCapacity(Box::new(request("R1", "anna", 500 * K, d(2027, 3, 1))));
    assert_eq!(b.refused("underwriting", again), "reservation_exists");
    for bad in [request("", "anna", K, d(2027, 3, 1)), request("R3", "anna", 0, d(2027, 3, 1)), {
        let mut r = request("R4", "anna", K, d(2027, 3, 1));
        r.proposal_expires = d(2027, 1, 1);
        r
    }] {
        assert_eq!(b.refused("underwriting", Command::ReserveCapacity(Box::new(bad))), "invalid_request");
    }
    let unknown = "unknown_reservation";
    assert_eq!(
        b.refused(
            "underwriting",
            Command::ReleaseCapacity {
                reservation_id: "zz".into(),
                reason: "x".into()
            }
        ),
        unknown
    );
    let confirm = |id: &str, layers: Vec<u32>| Command::ConfirmCapacity {
        reservation_id: id.into(),
        policy: PolicyId::from("P1"),
        cover: "life".into(),
        layers,
    };
    assert_eq!(b.refused("new-business", confirm("zz", vec![1])), unknown);
    assert_eq!(b.refused("new-business", confirm("R1", vec![])), "invalid_request");
    assert_eq!(b.refused("reinsurance-ops", terms(FacTarget::Reservation("zz".into()), "F", K)), unknown);
    assert_eq!(
        b.refused("reinsurance-ops", terms(FacTarget::Risk(key("P9", "life", 1, "L1")), "F", K)),
        "unknown_risk"
    );
    assert_eq!(b.refused("reinsurance-ops", terms(FacTarget::Reservation("R1".into()), "", K)), "invalid_terms");
    assert_eq!(
        b.refused("reinsurance-ops", terms(FacTarget::Reservation("R1".into()), "F", 0)),
        "invalid_terms"
    );
    assert_eq!(
        b.refused(
            "reinsurance-ops",
            Command::DeclineFacultative {
                reservation_id: "zz".into(),
                reference: "D".into()
            }
        ),
        unknown
    );
    b.ok(
        "reinsurance-ops",
        Command::DeclineFacultative {
            reservation_id: "R1".into(),
            reference: "D-1".into(),
        },
    );
    assert_eq!(b.ri.facultative_terms("R1"), Some(&FacultativeStatus::Declined { reference: "D-1".into() }));
    assert_eq!(
        b.refused(
            "reinsurance-ops",
            Command::DeclineFacultative {
                reservation_id: "R1".into(),
                reference: "D-2".into()
            }
        ),
        "reservation_not_held"
    );
}

/// Purpose: scheme cover on a member who may be this person is shown to the underwriter as a
/// possible match and is not counted in the outcome.
/// Why it matters: addendum decision 2. The underwriter must be able to see cover that may be
/// on the life, while an unconfirmed match must not move anyone's retention.
#[test]
fn a_possible_match_is_shown_but_not_counted() {
    let mut b = Bench::new();
    b.issue_scheme(2, "S1", d(2027, 2, 1), vec![member_cover("M1", 700 * K)]);
    b.ok(
        "group-admin",
        Command::NotePossibleMatch {
            policy: PolicyId::from("S1"),
            life: LifeId::from("M1"),
            party: PartyId::from("anna"),
        },
    );
    reserve(&mut b, "R1", "anna", 400 * K, d(2027, 3, 1));
    let answer = b.ri.capacity("R1").unwrap();
    assert_eq!(answer.held.possible_match_minor, 700 * K);
    assert_eq!(answer.held.retained_minor, 0);
    assert_eq!(
        answer.outcome,
        Outcome::Automatic {
            retained_minor: 400 * K,
            ceded_minor: 0
        }
    );
    reserve(&mut b, "R2", "bella", 400 * K, d(2027, 3, 1));
    assert_eq!(b.ri.capacity("R2").unwrap().held.possible_match_minor, 0);
}

/// Purpose: a release of evidence to a reinsurer is logged with its purpose and the documents
/// named, and a release with no purpose, no documents or no treaty is refused.
/// Why it matters: health evidence leaves the insurer only under a treaty and for a stated
/// purpose. The log is how a privacy request or an audit finds every release.
#[test]
fn evidence_releases_are_logged_with_their_purpose() {
    let mut b = Bench::new();
    let release = |treaty: &str, purpose: &str, documents: Vec<String>| Command::LogEvidenceRelease {
        subject: "R1".into(),
        treaty: treaty.into(),
        reinsurer: south(),
        purpose: purpose.into(),
        documents,
    };
    let applied = b.ok("underwriting", release("retail-life-surplus", "facultative referral", vec!["doc-1".into()]));
    assert!(
        matches!(&applied.notices[..], [Notice::ReleaseLogged { purpose, documents, .. }] if purpose == "facultative referral" && documents == &["doc-1".to_string()])
    );
    assert_eq!(
        b.refused("underwriting", release("retail-life-surplus", "", vec!["doc-1".into()])),
        "invalid_release"
    );
    assert_eq!(b.refused("underwriting", release("retail-life-surplus", "p", vec![])), "invalid_release");
    assert_eq!(
        b.refused("underwriting", release("no-such-treaty", "p", vec!["doc-1".into()])),
        "invalid_release"
    );
}
