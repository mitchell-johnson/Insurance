//! Notices for staff, published to the module's event hub.

use std::sync::{Arc, Mutex};

use ic_azure::AzureError;
use ic_underwriting::case::Queue;
use ic_underwriting::command::{Effect, NoticeKind, Recipient};
use ic_underwriting::driver::{DriverError, Forward};
use ic_underwriting::notices::{notice_id, NoLetters, Notices, StaffFeed};
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
    let staff = Notices {
        staff: feed.clone(),
        letters: NoLetters,
        evidence: Feed::default(),
        delegates: Feed::default(),
        clock: chrono::Utc::now,
    };
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

/// Purpose: a letter to a customer or an adviser is not published to the staff hub: with no
/// documents module it is parked with the reason that its receiver is missing. A hub that is
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
    let staff = Notices {
        staff: feed.clone(),
        letters: NoLetters,
        evidence: Feed::default(),
        delegates: Feed::default(),
        clock: chrono::Utc::now,
    };
    for to in [Recipient::Party { party: "party-1".into() }, Recipient::Adviser { adviser: "a1".into() }] {
        assert_eq!(
            staff.forward(1, &notice(to, NoticeKind::Declined)).await,
            Ok(Some("correspondence is not connected to underwriting".into()))
        );
    }
    assert!(feed.sent.lock().unwrap().is_empty());

    let escalated = notice(Recipient::UnderwritingManager, NoticeKind::CaseEscalated);
    for status in [503, 429, 408] {
        let down = Notices {
            staff: Feed {
                down: Some(status),
                ..Feed::default()
            },
            letters: NoLetters,
            evidence: Feed::default(),
            delegates: Feed::default(),
            clock: chrono::Utc::now,
        };
        let answer = down.forward(3, &escalated).await;
        assert!(
            matches!(answer, Err(DriverError::Unavailable(ref why)) if why.contains(&status.to_string())),
            "{answer:?}"
        );
    }
    for status in [404, 403] {
        let refused = Notices {
            staff: Feed {
                down: Some(status),
                ..Feed::default()
            },
            letters: NoLetters,
            evidence: Feed::default(),
            delegates: Feed::default(),
            clock: chrono::Utc::now,
        };
        assert_eq!(
            refused.forward(3, &escalated).await,
            Ok(Some(format!("the staff notices hub refused the notice: {status}: no")))
        );
    }
}

/// Records each letter request, refuses the ones scripted, or cannot be reached.
#[derive(Clone, Default)]
struct Documents {
    sent: Arc<Mutex<Vec<(String, Value)>>>,
    refuse: Option<(&'static str, &'static str)>,
    down: bool,
}

impl ic_underwriting::notices::Letters for Documents {
    async fn request(
        &self,
        key: &str,
        _at: chrono::DateTime<chrono::Utc>,
        command: Value,
    ) -> Result<Result<(), ic_underwriting::driver::Refusal>, DriverError> {
        if self.down {
            return Err(DriverError::Unavailable("documents".into()));
        }
        self.sent.lock().unwrap().push((key.into(), command));
        Ok(match self.refuse {
            Some((code, message)) => Err(ic_underwriting::driver::Refusal::new(code, message)),
            None => Ok(()),
        })
    }
}

fn at_noon() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2027-02-01T12:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc)
}

/// Purpose: a letter to a party or an adviser goes to the documents module as `request_letter`
/// under the request `uw:{outbox number}`, with the kind, the recipient in the module's own
/// shape, the proposal as subject, the life, the reason codes and the business date; a refusal
/// parks it with documents' code; a documents module that cannot be reached stops the pass; and
/// with no documents module the letter is parked as before.
/// Why it matters: these are the letters a person is owed: an offer, a decline with the reasons
/// they may be told, a lapsed proposal. The request must be the same on every try, or a retry
/// after a lost reply would send the person a second letter.
#[tokio::test]
async fn a_letter_is_requested_from_documents_under_a_stable_request() {
    let documents = Documents::default();
    let notices = Notices {
        staff: Feed::default(),
        letters: documents.clone(),
        evidence: Feed::default(),
        delegates: Feed::default(),
        clock: at_noon,
    };
    let declined = notice(Recipient::Party { party: "PTY-L1".into() }, NoticeKind::Declined);
    assert_eq!(notices.forward(12, &declined).await, Ok(None));
    assert_eq!(notices.forward(12, &declined).await, Ok(None));
    let sent = documents.sent.lock().unwrap().clone();
    let expected = json!({"request_letter": {
        "request": "uw:12", "kind": "declined", "recipient": {"party": {"party": "PTY-L1"}},
        "policy": null, "subject": "P1", "life": "L1", "reasons": ["a reason that may be clinical"],
        "detail": {}, "business_date": "2027-02-01",
    }});
    assert_eq!(sent, vec![("uw:12".to_string(), expected.clone()), ("uw:12".to_string(), expected)]);

    let refusing = Notices {
        staff: Feed::default(),
        evidence: Feed::default(),
        delegates: Feed::default(),
        letters: Documents {
            refuse: Some(("clinical_to_adviser", "not to an adviser")),
            ..Documents::default()
        },
        clock: at_noon,
    };
    let to_adviser = notice(Recipient::Adviser { adviser: "ADV-1".into() }, NoticeKind::OfferMade);
    assert_eq!(
        refusing.forward(13, &to_adviser).await,
        Ok(Some("documents refused the letter: clinical_to_adviser: not to an adviser".into()))
    );
    let away = Notices {
        staff: Feed::default(),
        evidence: Feed::default(),
        delegates: Feed::default(),
        letters: Documents {
            down: true,
            ..Documents::default()
        },
        clock: at_noon,
    };
    assert!(matches!(away.forward(14, &declined).await, Err(DriverError::Unavailable(_))));
    let none = Notices {
        staff: Feed::default(),
        letters: NoLetters,
        evidence: Feed::default(),
        delegates: Feed::default(),
        clock: at_noon,
    };
    assert_eq!(
        none.forward(15, &declined).await,
        Ok(Some("correspondence is not connected to underwriting".into()))
    );
}

/// Applies each letter request to a real documents state machine, as the
/// cluster would after the driver sent it.
#[derive(Clone)]
struct RealDocuments {
    state: Arc<Mutex<(ic_documents::DocumentsState, u64)>>,
}

impl ic_underwriting::notices::Letters for RealDocuments {
    async fn request(&self, key: &str, at: chrono::DateTime<chrono::Utc>, command: Value) -> Result<Result<(), ic_underwriting::driver::Refusal>, DriverError> {
        use ic_log::StateMachine;
        let envelope = ic_underwriting::service::documents_envelope(key, at, command);
        let envelope: ic_documents::command::Envelope = serde_json::from_value(envelope).map_err(|e| DriverError::Unavailable(e.to_string()))?;
        let mut guard = self.state.lock().unwrap();
        let (state, index) = &mut *guard;
        *index += 1;
        let applied = state.apply(*index, envelope);
        Ok(applied.result.map_err(|r| ic_underwriting::driver::Refusal::new(&r.code, r.message)))
    }
}

/// Purpose: every letter underwriting can ask for, to a party or an adviser as the module sends
/// it, is read and accepted by the real documents state machine under underwriting's own name;
/// sent again, it is taken as the same request; and a health letter to an adviser is parked.
/// Why it matters: the two modules agree on the letter only through JSON. A renamed field or a
/// kind that documents does not know would park every decline letter in production, and nobody
/// would hear of it until a customer complained that they were never told.
#[tokio::test]
async fn documents_accepts_every_letter_underwriting_sends() {
    let documents = RealDocuments {
        state: Arc::new(Mutex::new((ic_documents::DocumentsState::new(), 0))),
    };
    let notices = Notices {
        staff: Feed::default(),
        letters: documents.clone(),
        evidence: Feed::default(),
        delegates: Feed::default(),
        clock: at_noon,
    };
    let to_party = [
        NoticeKind::OfferMade,
        NoticeKind::Declined,
        NoticeKind::ProposalLapsed,
        NoticeKind::ProposalWithdrawn,
        NoticeKind::PolicyIssued,
        NoticeKind::AdviserUnavailable,
        NoticeKind::InterviewToConfirm,
    ];
    for (seq, kind) in (100..).zip(to_party) {
        let effect = notice(Recipient::Party { party: "PTY-L1".into() }, kind);
        assert_eq!(notices.forward(seq, &effect).await, Ok(None), "{kind:?} to the party");
        assert_eq!(notices.forward(seq, &effect).await, Ok(None), "{kind:?} sent again");
    }
    // An adviser may be told a proposal lapsed. A decline to an adviser is
    // health information, which documents refuses, so the letter is parked
    // for a person rather than sent.
    let lapsed = notice(Recipient::Adviser { adviser: "ADV-1".into() }, NoticeKind::ProposalLapsed);
    assert_eq!(notices.forward(200, &lapsed).await, Ok(None));
    let declined = notice(Recipient::Adviser { adviser: "ADV-1".into() }, NoticeKind::Declined);
    let parked = notices.forward(201, &declined).await.unwrap();
    assert!(
        parked.as_deref().is_some_and(|why| why.starts_with("documents refused the letter: ")),
        "{parked:?}"
    );
    assert_eq!(documents.state.lock().unwrap().1, 16);
}

/// Purpose: an evidence order (request, chase, cancel) is published to the evidence hub under an
/// ID from its outbox number, carrying the request and, for a new order, the kind and the
/// party, and nothing else; a request for a delegate's file goes to the delegates hub; neither
/// reaches the staff hub; a hub that refuses the driver parks the order with its status; and an
/// effect meant for none of them is parked.
/// Why it matters: the provider's adapter acts on these orders, so a lost one leaves a case
/// waiting for a report nobody asked for. The hubs are read by outside parties' adapters, so
/// an order must not carry a proposal's reasons or answers.
#[tokio::test]
async fn evidence_orders_and_delegate_requests_go_to_their_own_hubs() {
    use ic_underwriting::evidence::EvidenceKind;
    use ic_underwriting::notices::order_id;
    let (staff, evidence, delegates) = (Feed::default(), Feed::default(), Feed::default());
    let notices = Notices {
        staff: staff.clone(),
        evidence: evidence.clone(),
        delegates: delegates.clone(),
        letters: NoLetters,
        clock: chrono::Utc::now,
    };
    let orders = [
        Effect::RequestEvidence {
            request: "E1".into(),
            kind: EvidenceKind::GpReport,
            party: "PTY-1".into(),
        },
        Effect::ChaseEvidence { request: "E1".into() },
        Effect::CancelEvidence { request: "E1".into() },
    ];
    for (seq, order) in (60..).zip(&orders) {
        assert_eq!(notices.forward(seq, order).await, Ok(None));
    }
    assert_eq!(
        evidence.sent.lock().unwrap().clone(),
        vec![
            (
                order_id(60),
                "evidence_request".into(),
                json!({"order": "request", "request": "E1", "kind": "gp_report", "party": "PTY-1"})
            ),
            (order_id(61), "evidence_chase".into(), json!({"order": "chase", "request": "E1"})),
            (order_id(62), "evidence_cancel".into(), json!({"order": "cancel", "request": "E1"})),
        ]
    );
    let file = Effect::RequestDelegateFile {
        proposal: "P1".into(),
        life: "L1".into(),
        delegate_ref: "D-9".into(),
    };
    assert_eq!(notices.forward(63, &file).await, Ok(None));
    assert_eq!(
        delegates.sent.lock().unwrap().clone(),
        vec![(
            order_id(63),
            "delegate_file_request".into(),
            json!({"proposal": "P1", "life": "L1", "delegate_ref": "D-9"})
        )]
    );
    assert!(staff.sent.lock().unwrap().is_empty());

    let refusing = Notices {
        evidence: Feed {
            down: Some(403),
            ..Feed::default()
        },
        ..notices.clone()
    };
    assert_eq!(
        refusing.forward(64, &orders[1]).await,
        Ok(Some("the evidence hub refused the order: 403: no".into()))
    );
    // An effect that is no notice, order or letter has nowhere to go here and is parked.
    let other = Effect::ReleaseCapacity {
        reservation: "R1".into(),
        reason: "declined".into(),
    };
    assert!(notices.forward(65, &other).await.unwrap().is_some());
}
