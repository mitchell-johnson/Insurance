//! The core's events, retention, legal holds, shredding, and the rules every command obeys.

mod support;

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_core::{Event as CoreEvent, EventRecord, PolicyStatus, Standing};
use ic_documents::command::{Command, HoldScope};
use ic_documents::model::{Channel, RetentionBasis, RetentionRule};
use ic_documents::{DocumentKind, DocumentsState, Event};
use ic_log::{MemoryLog, Replica};
use support::*;

fn core(seq: u64, on: NaiveDate, event: CoreEvent) -> EventRecord {
    EventRecord {
        id: format!("7-{seq}"),
        partition: 7,
        seq,
        business_date: on,
        log_index: seq,
        traceparent: None,
        event,
    }
}

fn status(seq: u64, on: NaiveDate, to: PolicyStatus) -> EventRecord {
    core(
        seq,
        on,
        CoreEvent::PolicyStatusChanged {
            policy: policy("pol-1"),
            from: PolicyStatus::InForce,
            to,
        },
    )
}

fn standing(seq: u64, on: NaiveDate, to: Option<Standing>) -> EventRecord {
    core(
        seq,
        on,
        CoreEvent::StandingChanged {
            policy: policy("pol-1"),
            from: Some(Standing::Paid),
            to,
            oldest_unpaid_due: None,
        },
    )
}

fn observe(records: Vec<EventRecord>) -> Command {
    Command::ObserveCoreEvents { partition: 7, records }
}

fn short_retention(kit: &mut Kit) {
    let mut rules: BTreeMap<_, _> = DocumentKind::ALL
        .iter()
        .map(|k| {
            (
                *k,
                RetentionRule {
                    basis: RetentionBasis::Issue,
                    days: 30,
                },
            )
        })
        .collect();
    rules.insert(
        DocumentKind::PolicySchedule,
        RetentionRule {
            basis: RetentionBasis::PolicyEnd,
            days: 60,
        },
    );
    kit.ok("config", "ops", Command::ActivateRetention { version: 1, rules });
}

fn schedule(kit: &Kit, id: &str) -> ic_documents::command::Issue {
    let mut spec = kit.issue_spec(id, DocumentKind::PolicySchedule, "schedule");
    spec.wordings = vec![tref("wording", 1)];
    spec
}

/// Purpose: core events become required documents: a standing change to grace asks for a lapse
/// notice with its purpose, a receipt for a receipt, an issue for a schedule. The checkpoint
/// advances, a redelivered batch is skipped, a gap or another partition's event is refused,
/// and issuing the document for the event clears the requirement.
/// Why it matters: notices are events, not steps. Delivery of the stream is at least once, so
/// a replayed batch must not ask for a second notice, and a missed event must stop the feed.
#[test]
fn core_events_require_documents_exactly_once() {
    let mut kit = Kit::ready();
    let today = kit.today();
    let pol = policy("pol-1");
    let batch = vec![
        standing(1, today, Some(Standing::InGrace)),
        core(
            2,
            today,
            CoreEvent::ReceiptApplied {
                policy: pol.clone(),
                receipt: "rc".into(),
                allocated_minor: 100,
                to_suspense_minor: 0,
                net_by_cover_type: Default::default(),
            },
        ),
        core(3, today, CoreEvent::PolicyReinstated { policy: pol.clone() }),
        standing(4, today, Some(Standing::Paid)),
        core(
            5,
            today,
            CoreEvent::IndexationChoiceRecorded {
                policy: pol.clone(),
                declined: true,
            },
        ),
    ];
    let events = kit.ok("relay", "svc", observe(batch.clone()));
    assert_eq!(
        events,
        vec![
            Event::DocumentRequired {
                trigger: "7-1:LapseNotice".into(),
                policy: pol.clone(),
                kind: DocumentKind::LapseNotice
            },
            Event::DocumentRequired {
                trigger: "7-2:PremiumReceipt".into(),
                policy: pol.clone(),
                kind: DocumentKind::PremiumReceipt
            },
            Event::DocumentRequired {
                trigger: "7-3:PolicySchedule".into(),
                policy: pol.clone(),
                kind: DocumentKind::PolicySchedule
            },
        ]
    );
    assert_eq!(kit.state.triggers["7-1:LapseNotice"].purpose.as_deref(), Some("lapse"));
    assert_eq!(kit.state.checkpoints[&7], 5);
    assert_eq!(kit.state.report(today, today).documents_awaiting_trigger, 3);

    // Redelivery, in whole or overlapping, changes nothing.
    assert!(kit.ok("relay", "svc", observe(batch)).is_empty());
    let overlap = vec![standing(5, today, None), standing(6, today, Some(Standing::Suspended))];
    let events = kit.ok("relay", "svc", observe(overlap));
    assert_eq!(
        events,
        vec![Event::DocumentRequired {
            trigger: "7-6:SuspensionNotice".into(),
            policy: pol.clone(),
            kind: DocumentKind::SuspensionNotice
        }]
    );
    assert_eq!(kit.err("relay", "svc", observe(vec![standing(9, today, None)])), "gap");
    let mut other = standing(7, today, None);
    other.partition = 8;
    assert_eq!(kit.err("relay", "svc", observe(vec![other])), "wrong_partition");
    assert_eq!(kit.state.checkpoints[&7], 6);

    let mut notice = kit.issue_spec("l1", DocumentKind::LapseNotice, "lapse");
    notice.trigger_event = Some("7-1".into());
    notice.notice = Some(ic_documents::command::NoticeSpec {
        purpose: "lapse".into(),
        corrects: None,
    });
    kit.issue(notice);
    assert!(!kit.state.triggers.contains_key("7-1:LapseNotice"));

    let dismiss = |trigger: &str, reason: &str| Command::DismissTrigger {
        trigger: trigger.into(),
        reason: reason.into(),
    };
    assert_eq!(kit.err("documents", "svc", dismiss("7-2:PremiumReceipt", "")), "no_reason");
    assert_eq!(kit.err("documents", "svc", dismiss("nope", "x")), "unknown_trigger");
    assert_eq!(
        kit.ok("documents", "svc", dismiss("7-2:PremiumReceipt", "customer opted out of receipts")),
        vec![Event::TriggerDismissed {
            trigger: "7-2:PremiumReceipt".into()
        }]
    );
    assert_eq!(kit.state.triggers.len(), 2);
}

/// Purpose: events older than the live cutoff advance the checkpoint and update retention
/// facts but ask for no documents; status changes that end a policy and events that need no
/// document are handled without a trigger for the wrong thing.
/// Why it matters: "the service ignores events older than a cutoff, so rebuilding a read
/// model or replaying a stream cannot trigger a flood of old messages" to real customers.
#[test]
fn replayed_history_sends_nothing() {
    let mut kit = Kit::ready();
    let old = kit.today() - chrono::Duration::days(8);
    let pol = policy("pol-1");
    let layer = ic_core::event::LayerInfo {
        cover: "life".into(),
        cover_type: "life".into(),
        layer: 1,
        life: ic_core::LifeId::from("l1"),
        sum_insured_minor: 1,
        start: old,
    };
    let events = kit.ok(
        "relay",
        "svc",
        observe(vec![
            standing(1, old, Some(Standing::InGrace)),
            core(
                2,
                old,
                CoreEvent::AnniversaryReached {
                    policy: pol.clone(),
                    number: 1,
                    on: old,
                },
            ),
            core(
                3,
                old,
                CoreEvent::LayerAdded {
                    policy: pol.clone(),
                    layer,
                    origin: ic_core::LayerOrigin::Indexation,
                },
            ),
        ]),
    );
    assert!(events.is_empty());
    assert_eq!(kit.state.checkpoints[&7], 3);
    assert!(kit.state.triggers.is_empty());

    let today = kit.today();
    let events = kit.ok(
        "relay",
        "svc",
        observe(vec![status(4, today, PolicyStatus::Lapsed), status(5, today, PolicyStatus::InForce)]),
    );
    assert_eq!(
        events,
        vec![Event::DocumentRequired {
            trigger: "7-4:GeneralCorrespondence".into(),
            policy: pol,
            kind: DocumentKind::GeneralCorrespondence
        }]
    );
    let too_many: Vec<_> = (6..=206).map(|seq| standing(seq, today, None)).collect();
    assert_eq!(kit.err("relay", "svc", observe(too_many)), "invalid");
}

/// Purpose: a document is destroyed the day after its retention ends: its key is dropped, its
/// vault object queued for deletion and acknowledged, and it leaves the inbox. A contractual
/// document's clock starts only when the core says the policy ended, and a reinstatement
/// stops it again.
/// Why it matters: retention is per document kind as code, and contractual documents are
/// held for the life of the policy plus the period. Destroying a schedule of a policy still
/// in force would destroy the contract's evidence.
#[test]
fn retention_runs_from_issue_or_from_the_end_of_the_policy() {
    let mut kit = Kit::ready();
    short_retention(&mut kit);
    kit.issue(schedule(&kit, "s1"));
    kit.issue(kit.issue_spec("r1", DocumentKind::PremiumReceipt, "receipt"));
    assert_eq!(kit.state.documents["r1"].destroy_after, Some(date("2027-12-15")));
    assert_eq!(kit.state.documents["s1"].destroy_after, None);

    kit.advance_days(30);
    assert!(kit.tick().is_empty(), "the last day of retention is still retained");
    kit.advance_days(1);
    assert_eq!(kit.tick(), vec![Event::DocumentDestroyed { document: "r1".into() }]);
    let doc = &kit.state.documents["r1"];
    assert!(doc.wrapped_key.is_none() && doc.destroyed_on == Some(date("2027-12-16")));
    assert_eq!(kit.state.pending_deletes["r1"], "docs/r1");
    assert_eq!(kit.state.inbox(&party("aroha")).len(), 1);
    assert!(
        kit.state.pending_sends(10).iter().all(|s| s.document != "r1"),
        "a destroyed document is not sent"
    );
    assert_eq!(
        kit.err(
            "servicing",
            "csr",
            Command::RetryDelivery {
                document: "r1".into(),
                party: party("aroha"),
                channels: vec![Channel::Post]
            }
        ),
        "destroyed"
    );
    kit.ok("retention", "svc", Command::AckDeleted { documents: vec!["r1".into()] });
    assert!(kit.state.pending_deletes.is_empty());

    // The policy lapses, is reinstated, then is cancelled: only the last starts the clock.
    let today = kit.today();
    kit.ok("relay", "svc", observe(vec![status(1, today, PolicyStatus::Lapsed)]));
    assert_eq!(kit.state.documents["s1"].destroy_after, Some(today + chrono::Duration::days(60)));
    kit.ok(
        "relay",
        "svc",
        observe(vec![core(2, today, CoreEvent::PolicyReinstated { policy: policy("pol-1") })]),
    );
    assert_eq!(kit.state.documents["s1"].destroy_after, None);
    kit.advance_days(100);
    assert!(kit.tick().is_empty());
    let ended = kit.today();
    kit.ok("relay", "svc", observe(vec![status(3, ended, PolicyStatus::Cancelled)]));
    // A schedule issued after the policy ended takes the same clock.
    kit.issue(schedule(&kit, "s2"));
    assert_eq!(kit.state.documents["s2"].destroy_after, Some(ended + chrono::Duration::days(60)));
    kit.advance_days(61);
    let events = kit.tick();
    assert_eq!(events.len(), 2);
    assert!(kit.state.schedule_history(&policy("pol-1")).iter().all(|d| d.destroyed_on.is_some()));
}

/// Purpose: a legal hold on a document, a party or a policy stops destruction at the end of
/// retention; releasing it lets the next tick destroy the document. Holds need a case, and
/// releasing a hold that is not there is refused.
/// Why it matters: "a legal hold blocks it". Destroying a document under a hold is
/// destruction of evidence, and cannot be undone.
#[test]
fn a_legal_hold_blocks_destruction_until_released() {
    let mut kit = Kit::ready();
    short_retention(&mut kit);
    for id in ["r1", "r2", "r3"] {
        kit.issue(kit.issue_spec(id, DocumentKind::PremiumReceipt, "receipt"));
    }
    let hold = |scope| Command::PlaceLegalHold { scope, case: "LIT-9".into() };
    assert_eq!(
        kit.err(
            "legal",
            "counsel",
            Command::PlaceLegalHold {
                scope: HoldScope::Document("r1".into()),
                case: String::new()
            }
        ),
        "no_case"
    );
    assert_eq!(
        kit.ok("legal", "counsel", hold(HoldScope::Document("r1".into()))),
        vec![Event::LegalHoldPlaced { case: "LIT-9".into() }]
    );
    assert!(kit.state.is_held("r1") && !kit.state.is_held("r2") && !kit.state.is_held("ghost"));
    kit.advance_days(31);
    let events = kit.tick();
    assert_eq!(events.len(), 2);
    assert!(kit.state.documents["r1"].destroyed_on.is_none());

    assert_eq!(
        kit.err(
            "legal",
            "counsel",
            Command::ReleaseLegalHold {
                scope: HoldScope::Policy(policy("pol-1"))
            }
        ),
        "no_hold"
    );
    assert_eq!(
        kit.ok(
            "legal",
            "counsel",
            Command::ReleaseLegalHold {
                scope: HoldScope::Document("r1".into())
            }
        ),
        vec![Event::LegalHoldReleased]
    );
    assert_eq!(kit.tick(), vec![Event::DocumentDestroyed { document: "r1".into() }]);

    // Holds by policy and by party cover every document under them.
    kit.issue(kit.issue_spec("r4", DocumentKind::PremiumReceipt, "receipt"));
    kit.ok("legal", "counsel", hold(HoldScope::Policy(policy("pol-1"))));
    assert!(kit.state.is_held("r4"));
    kit.ok(
        "legal",
        "counsel",
        Command::ReleaseLegalHold {
            scope: HoldScope::Policy(policy("pol-1")),
        },
    );
    kit.ok("legal", "counsel", hold(HoldScope::Party(party("aroha"))));
    assert!(kit.state.is_held("r4"));
    assert_eq!(kit.err("privacy", "officer", Command::ShredParty { party: party("aroha") }), "legal_hold");
}

/// Purpose: shredding a party destroys their informational documents at once, keeps their
/// contractual documents and proven notices until each one's retention ends, and destroys
/// the party's key only when the last document is gone. A shredded party takes no new key.
/// Why it matters: "documents belong to parties, encrypted under their keys, so shredding
/// covers them, with contractual documents held until the policy's own retention clock ends."
/// Getting the order wrong either breaks the privacy promise or destroys a live contract.
#[test]
fn shredding_a_party_keeps_what_must_be_kept_until_its_time() {
    let mut kit = Kit::ready();
    short_retention(&mut kit);
    kit.issue(kit.issue_spec("r1", DocumentKind::PremiumReceipt, "receipt"));
    kit.issue(kit.renewal_spec("n1"));
    kit.issue(schedule(&kit, "s1"));
    // A document addressed to Aroha but owned by Ben is Ben's, and stays.
    let mut bens = kit.issue_spec("b1", DocumentKind::PremiumReceipt, "receipt");
    bens.owner = party("ben");
    kit.issue(bens);

    assert_eq!(kit.err("privacy", "officer", Command::ShredParty { party: party("nobody") }), "unknown_party");
    let events = kit.ok("privacy", "officer", Command::ShredParty { party: party("aroha") });
    assert_eq!(
        events,
        vec![
            Event::PartyShredRequested { party: party("aroha") },
            Event::DocumentDestroyed { document: "r1".into() }
        ]
    );
    assert!(kit.state.party_keys.contains_key(&party("aroha")), "the key stays while documents remain");
    assert!(kit.state.documents["b1"].destroyed_on.is_none());
    assert_eq!(
        kit.err(
            "documents",
            "svc",
            Command::RegisterPartyKey {
                party: party("aroha"),
                wrapped_key: "new".into()
            }
        ),
        "shredded"
    );
    // Asking again is harmless.
    assert!(kit.ok("privacy", "officer", Command::ShredParty { party: party("aroha") }).is_empty());

    kit.advance_days(31);
    assert_eq!(
        kit.tick(),
        vec![
            Event::DocumentDestroyed { document: "b1".into() },
            Event::DocumentDestroyed { document: "n1".into() }
        ]
    );
    let today = kit.today();
    kit.ok("relay", "svc", observe(vec![status(1, today, PolicyStatus::Surrendered)]));
    kit.advance_days(61);
    let events = kit.tick();
    assert_eq!(
        events,
        vec![
            Event::DocumentDestroyed { document: "s1".into() },
            Event::PartyKeyDestroyed { party: party("aroha") }
        ]
    );
    assert!(!kit.state.party_keys.contains_key(&party("aroha")));
    assert!(kit.state.party_keys.contains_key(&party("ben")));
    assert!(kit.ok("privacy", "officer", Command::ShredParty { party: party("aroha") }).is_empty());
}

/// Purpose: a party's key is registered once: the same key again is accepted, another is
/// refused so a second writer uses the first, and an empty one is refused.
/// Why it matters: two drivers racing to create a party's key must end with one key. If the
/// second replaced the first, every document encrypted under the first would be unreadable.
#[test]
fn the_first_party_key_stands() {
    let mut kit = Kit::new();
    let register = |key: &str| Command::RegisterPartyKey {
        party: party("aroha"),
        wrapped_key: key.into(),
    };
    assert_eq!(kit.err("documents", "svc", register("")), "invalid");
    kit.ok("documents", "svc", register("k1"));
    kit.ok("documents", "svc", register("k1"));
    assert_eq!(kit.err("documents", "svc", register("k2")), "key_exists");
    assert_eq!(kit.state.party_keys[&party("aroha")], "k1");
}

/// Purpose: every command obeys the same rules: a repeated key returns the original result
/// (accepted or refused) and changes nothing, a caller not allowed to send a command is
/// refused, a command names an actor, time never runs backwards, and published events are
/// released in order.
/// Why it matters: delivery to the module is at least once and from several services. These
/// rules are what make a retry safe and the audit trail attributable.
#[test]
fn every_command_is_idempotent_attributed_and_from_an_allowed_caller() {
    let mut kit = Kit::ready();
    let issue = Command::IssueDocument(Box::new(kit.renewal_spec("n1")));
    let first = kit.send_keyed("issue-n1", "documents", "svc", issue.clone());
    assert!(first.result.is_ok() && !first.duplicate && first.events.len() == 2);
    let again = kit.send_keyed("issue-n1", "documents", "svc", issue.clone());
    assert!(again.result.is_ok() && again.duplicate && again.events.is_empty() && again.index == first.index);
    let refused = kit.send_keyed("issue-again", "documents", "svc", issue.clone());
    assert_eq!(refused.result.as_ref().unwrap_err().code, "document_exists");
    let repeat = kit.send_keyed("issue-again", "documents", "svc", issue.clone());
    assert!(repeat.duplicate && repeat.result == refused.result);
    assert_eq!(kit.state.documents.len(), 1);

    assert_eq!(kit.err("delivery", "svc", issue.clone()), "not_allowed");
    assert_eq!(kit.err("documents", "", issue), "no_actor");
    assert_eq!(kit.err("documents", "svc", Command::Tick), "not_allowed");

    // A proposer whose clock is behind does not move time back.
    let before = kit.state.now.unwrap();
    kit.now -= chrono::Duration::days(3);
    let events = kit.deliver("n1", "aroha", Channel::Email);
    assert!(matches!(&events[0], Event::DeliveryConfirmed { on, .. } if *on == date("2027-11-15")));
    assert_eq!(kit.state.now, Some(before));

    let pending = kit.state.pending_events(1000);
    assert!(pending.windows(2).all(|w| w[1].seq == w[0].seq + 1) && pending[0].seq == 1 && pending[0].id == "doc-1");
    let last = pending.last().unwrap().seq;
    kit.ok("relay", "svc", Command::MarkPublished { through_seq: last - 1 });
    assert_eq!(kit.state.pending_events(1000).len(), 1);
    assert_eq!(kit.state.pending_events(1000)[0].seq, last);
}

/// Purpose: replaying the same log through a fresh state machine reaches the same state hash,
/// and the state survives being saved and loaded as JSON.
/// Why it matters: the module runs as a replicated group. Replicas that applied the same
/// entries must hold the same state, and a snapshot must restore exactly what was saved.
#[test]
fn replaying_the_log_reaches_the_same_state() {
    let mut kit = Kit::ready();
    kit.issue(kit.renewal_spec("n1"));
    kit.deliver("n1", "aroha", Channel::Email);
    kit.advance_days(40);
    kit.tick();
    let mut replica = Replica::recover(MemoryLog::new(), DocumentsState::new()).unwrap();
    for envelope in &kit.log {
        replica.propose(envelope.clone()).unwrap();
    }
    assert_eq!(replica.state.state_hash(), kit.state.state_hash());
    let again = Replica::recover(replica.log.clone(), DocumentsState::new()).unwrap();
    assert_eq!(again.state.state_hash(), kit.state.state_hash());
    let restored: DocumentsState = serde_json::from_slice(&serde_json::to_vec(&kit.state).unwrap()).unwrap();
    assert_eq!(restored, kit.state);
}

/// Purpose: idempotency keys older than the keeping period are forgotten
/// as ticks pass, a bounded number per tick, while recent keys still
/// return the original result, and a saved state without the key log loads.
/// Why it matters: every issued document leaves a key, so without pruning
/// the replicated state and every snapshot grow for the life of the
/// insurer; forgetting a key too soon would let a retry repeat a command.
#[test]
fn old_keys_are_forgotten_and_recent_ones_kept() {
    use ic_documents::state::{KEEP_KEYS_DAYS, MAX_CHUNK};
    let mut kit = Kit::new();
    let refused = |kit: &mut Kit, key: &str| {
        kit.send_keyed(
            key,
            "nobody",
            "svc",
            Command::DismissTrigger {
                trigger: "t".into(),
                reason: "r".into(),
            },
        )
    };
    for i in 0..MAX_CHUNK + 5 {
        assert_eq!(refused(&mut kit, &format!("old-{i}")).result.unwrap_err().code, "not_allowed");
    }
    kit.advance_days(KEEP_KEYS_DAYS);
    kit.tick();
    assert_eq!(kit.state.remembered_keys(), MAX_CHUNK + 5, "a key is kept for the whole period");
    assert!(refused(&mut kit, "old-0").duplicate);

    kit.advance_days(1);
    assert!(refused(&mut kit, "recent").result.is_err());
    kit.tick();
    // One tick forgets a bounded number, oldest first; the next finishes.
    assert_eq!(kit.state.remembered_keys(), 5 + 1);
    kit.tick();
    assert_eq!(kit.state.remembered_keys(), 1);
    assert!(refused(&mut kit, "recent").duplicate);
    // A forgotten key is judged again on its merits.
    assert!(!refused(&mut kit, "old-0").duplicate);

    let mut saved = serde_json::to_value(&kit.state).unwrap();
    saved.as_object_mut().unwrap().remove("key_log").expect("the state saves its key log");
    let older: DocumentsState = serde_json::from_value(saved).unwrap();
    assert_eq!(older.remembered_keys(), 2);
}
