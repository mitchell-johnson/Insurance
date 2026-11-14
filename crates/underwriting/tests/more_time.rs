//! A life marked as needing more time.

mod common;

use common::*;
use ic_core::LifeId;
use ic_underwriting::command::{Command, Event};
use ic_underwriting::model::{OfferStatus, Stage};

fn mark(w: &mut World, id: &str, more_time: bool) -> ic_underwriting::command::Applied {
    w.ok(
        "new-business",
        Command::SetMoreTime {
            proposal: id.into(),
            life: "L1".into(),
            more_time,
        },
    )
}

/// Purpose: a life marked as needing more time (a vulnerability marker with the `more_time`
/// adjustment) has its offer run the further days the settings give, and the proposal with it;
/// an offer made to them after the mark runs longer too; and once the usual offer life has
/// passed, the marked customer's offer is still open while an unmarked customer's has lapsed.
/// Marking twice gives the days once, and clearing the mark shortens nothing given. Only the
/// customer's deadlines move: the case's own timers are not touched.
/// Why it matters: a customer who needs more time to read and decide an offer, and is not
/// given it, loses cover they would have accepted; but the insurer's timers on its own work
/// must not slow down because a customer is vulnerable (VUL-016).
#[test]
fn a_customer_who_needs_more_time_gets_longer_to_answer_an_offer() {
    let mut w = World::new();
    let more = w.s.config.settings.more_time_days;
    let offer_days = w.s.config.settings.offer_days;
    for id in ["P1", "P2", "P3"] {
        w.open(id, 300_000_00);
        let disclosed = w.disclose(id, "L1", &yes("smoked_last_12_months"));
        w.automatic(&disclosed);
        assert_eq!(w.stage(id, "L1"), Stage::OfferOpen, "{id}");
    }
    let offer = |w: &World, id: &str| w.p(id).lives[&LifeId::from("L1")].open_offer().map(|o| (o.expires_on, o.status));
    let (usual, _) = offer(&w, "P1").unwrap();
    let proposal_expiry = w.p("P1").expires_on;

    let marked = mark(&mut w, "P1", true);
    assert!(marked.events.iter().any(|e| matches!(e, Event::MoreTimeSet { more_time: true, .. })));
    assert_eq!(offer(&w, "P1").unwrap().0, usual + chrono::Duration::days(more));
    assert_eq!(w.p("P1").expires_on, proposal_expiry + chrono::Duration::days(more));
    let again = mark(&mut w, "P1", true);
    assert!(again.events.is_empty(), "marked once");
    assert_eq!(offer(&w, "P1").unwrap().0, usual + chrono::Duration::days(more));

    // Marked before the offer is made: it is made with the longer life.
    w.open("P4", 300_000_00);
    mark(&mut w, "P4", true);
    let disclosed = w.disclose("P4", "L1", &yes("smoked_last_12_months"));
    w.automatic(&disclosed);
    assert_eq!(offer(&w, "P4").unwrap().0, usual + chrono::Duration::days(more));

    // Cleared, nothing already given is taken back.
    mark(&mut w, "P2", true);
    mark(&mut w, "P2", false);
    assert_eq!(offer(&w, "P2").unwrap().0, usual + chrono::Duration::days(more));

    // After the usual offer life, the marked customer can still answer; the other cannot.
    w.days(offer_days + 1);
    w.ok("timer", Command::Sweep { after: None, max: 100 });
    assert_eq!(offer(&w, "P1").map(|(_, s)| s), Some(OfferStatus::Open));
    assert_eq!(w.stage("P1", "L1"), Stage::OfferOpen);
    assert!(offer(&w, "P3").is_none(), "the unmarked offer lapsed");
    assert_ne!(w.stage("P3", "L1"), Stage::OfferOpen);
    w.respond("P1", "L1", ic_underwriting::command::OfferResponse::Accept);
    assert_eq!(w.stage("P1", "L1"), Stage::Accepted);
}

/// Purpose: a life marked as needing more time while a medical report is still awaited has the
/// report's deadline moved by the further days, once; a report already settled is not touched.
/// Why it matters: the deadline is when the evidence is given up as unobtainable and the case
/// goes to a person. A customer who needs longer to arrange a GP visit or a blood test, and is
/// not given it, has their application judged without the evidence they were getting (VUL-016).
#[test]
fn a_customer_who_needs_more_time_gets_longer_to_supply_evidence() {
    let mut w = World::new();
    let more = w.s.config.settings.more_time_days;
    w.open("P1", 500_000_00);
    w.consent("P1", "L1", ic_underwriting::model::ConsentKind::ObtainRecords);
    w.disclose("P1", "L1", &yes("diabetes"));
    let deadlines = |w: &World| {
        w.p("P1").lives[&LifeId::from("L1")]
            .evidence
            .iter()
            .filter(|e| e.outstanding())
            .map(|e| e.deadline)
            .collect::<Vec<_>>()
    };
    let before = deadlines(&w);
    assert!(!before.is_empty(), "a report is awaited");
    mark(&mut w, "P1", true);
    let moved: Vec<_> = before.iter().map(|d| *d + chrono::Duration::days(more)).collect();
    assert_eq!(deadlines(&w), moved);
    mark(&mut w, "P1", true);
    assert_eq!(deadlines(&w), moved, "given once");
}
