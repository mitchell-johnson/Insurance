//! Notices for staff, published to the module's event hub.

use std::sync::{Arc, Mutex};

use ic_azure::AzureError;
use ic_underwriting::case::Queue;
use ic_underwriting::command::{Effect, NoticeKind, Recipient};
use ic_underwriting::driver::{DriverError, Forward};
use ic_underwriting::notices::{notice_id, StaffFeed, StaffNotices};
use serde_json::{json, Value};

/// Remembers what was published, or fails every time with `down`'s status.
#[derive(Clone, Default)]
struct Feed {
    sent: Arc<Mutex<Vec<(String, String, Value)>>>,
    down: Option<u16>,
}

impl StaffFeed for Feed {
    async fn publish(&self, id: &str, kind: &str, body: Value) -> Result<(), AzureError> {
        if let Some(status) = self.down {
            return Err(AzureError::Status {
                service: "Event Hubs",
                status,
                body: "no".into(),
            });
        }
        self.sent.lock().unwrap().push((id.into(), kind.into(), body));
        Ok(())
    }
}

fn notice(to: Recipient, kind: NoticeKind) -> Effect {
    Effect::Notify {
        to,
        kind,
        proposal: "P1".into(),
        life: Some("L1".into()),
        reasons: vec!["a reason that may be clinical".into()],
    }
}

/// Purpose: a notice for an underwriter, a queue lead or the underwriting manager is published
/// under an ID taken from its number in the outbox, the same on every try, and carries what it
/// is, who it is for, the proposal and the life, and not the reasons.
/// Why it matters: these notices are how an escalated case or a failed issue reaches a person;
/// parked, as they were before, nobody heard of them until they looked. The hub is read by more
/// people than may see a case's reasons, which can be clinical, and two drivers run at once, so
/// a consumer must be able to drop the second copy by its ID.
#[tokio::test]
async fn a_staff_notice_is_published_once_by_id_and_without_its_reasons() {
    let feed = Feed::default();
    let staff = StaffNotices { staff: feed.clone() };
    let cases = [
        (
            Recipient::Underwriter { underwriter: "u1".into() },
            NoticeKind::EvidenceArrived,
            json!({"underwriter": "u1"}),
        ),
        (
            Recipient::QueueLead { queue: Queue::Medical },
            NoticeKind::CaseEscalated,
            json!({"queue_lead": "medical"}),
        ),
        (Recipient::UnderwritingManager, NoticeKind::IssueFailed, json!("underwriting_manager")),
    ];
    for (seq, (to, kind, expected_to)) in (40..).zip(cases) {
        let effect = notice(to, kind);
        assert_eq!(staff.forward(seq, &effect).await, Ok(None));
        assert_eq!(staff.forward(seq, &effect).await, Ok(None), "a second try is taken too");
        let sent = feed.sent.lock().unwrap().clone();
        let name = json!(kind).as_str().unwrap().to_string();
        let expected = (
            notice_id(seq),
            name.clone(),
            json!({"notice": name, "to": expected_to, "proposal": "P1", "life": "L1"}),
        );
        assert_eq!(sent[sent.len() - 2..], [expected.clone(), expected]);
    }
    assert_eq!(notice_id(40), "underwriting-notice:40");
    assert!(feed.sent.lock().unwrap().iter().all(|(_, _, body)| !body.to_string().contains("reason")));
}

/// Purpose: a letter to a customer or an adviser, and an evidence request, are not published to
/// the staff hub: they are parked with the reason that their receiver is missing. A hub that is
/// busy or cannot be reached stops the pass as unavailable; a hub that is missing or refuses the
/// driver's identity parks the notice with the status.
/// Why it matters: a decline letter on the staff hub would show a customer's outcome to staff
/// who have no part in the case, and would not reach the customer either. Parking a notice
/// because the hub was briefly down would need a person to put it back when the next pass would
/// have delivered it; but stopping on a hub that was never created would hold every policy
/// waiting behind the notice until someone noticed.
#[tokio::test]
async fn letters_are_parked_and_a_hub_that_is_down_is_tried_again() {
    let feed = Feed::default();
    let staff = StaffNotices { staff: feed.clone() };
    for to in [Recipient::Party { party: "party-1".into() }, Recipient::Adviser { adviser: "a1".into() }] {
        assert_eq!(
            staff.forward(1, &notice(to, NoticeKind::Declined)).await,
            Ok(Some("correspondence is not connected to underwriting".into()))
        );
    }
    assert_eq!(
        staff.forward(2, &Effect::ChaseEvidence { request: "E1".into() }).await,
        Ok(Some("no evidence provider is connected".into()))
    );
    assert!(feed.sent.lock().unwrap().is_empty());

    let escalated = notice(Recipient::UnderwritingManager, NoticeKind::CaseEscalated);
    for status in [503, 429, 408] {
        let down = StaffNotices {
            staff: Feed {
                down: Some(status),
                ..Feed::default()
            },
        };
        let answer = down.forward(3, &escalated).await;
        assert!(
            matches!(answer, Err(DriverError::Unavailable(ref why)) if why.contains(&status.to_string())),
            "{answer:?}"
        );
    }
    for status in [404, 403] {
        let refused = StaffNotices {
            staff: Feed {
                down: Some(status),
                ..Feed::default()
            },
        };
        assert_eq!(
            refused.forward(3, &escalated).await,
            Ok(Some(format!("the staff notices hub refused the notice: {status}: no")))
        );
    }
}
