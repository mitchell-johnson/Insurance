//! A lender or other party with an interest in a policy is copied on the
//! notices that warn it may end.

mod support;

use chrono::NaiveDate;
use ic_core::interest::{InterestEvent, RestrictedAction};
use ic_core::{Event as CoreEvent, EventRecord, PolicyStatus, Standing};
use ic_documents::command::{Command, NoticeSpec};
use ic_documents::model::{Channel, RecipientRole};
use ic_documents::module::{self, Query};
use ic_documents::{DocumentKind, DocumentsState};
use serde_json::json;
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

fn recorded(seq: u64, on: NaiveDate, interest: &str, who: &str, from: NaiveDate) -> EventRecord {
    core(
        seq,
        on,
        CoreEvent::Interest(InterestEvent::Recorded {
            kind: Default::default(),
            policy: policy("pol-1"),
            interest: interest.into(),
            party: party(who),
            notice_received: on,
            extent_minor: None,
            effective_from: from,
        }),
    )
}

fn released(seq: u64, on: NaiveDate, interest: &str, who: &str, effective: NaiveDate) -> EventRecord {
    core(
        seq,
        on,
        CoreEvent::Interest(InterestEvent::Released {
            policy: policy("pol-1"),
            interest: interest.into(),
            party: party(who),
            effective,
        }),
    )
}

fn standing(seq: u64, on: NaiveDate, to: Standing) -> EventRecord {
    core(
        seq,
        on,
        CoreEvent::StandingChanged {
            policy: policy("pol-1"),
            from: Some(Standing::Paid),
            to: Some(to),
            oldest_unpaid_due: None,
        },
    )
}

fn observe(kit: &mut Kit, records: Vec<EventRecord>) {
    kit.ok("relay", "svc", Command::ObserveCoreEvents { partition: 7, records });
}

fn lapse_notice(kit: &Kit, id: &str, event: &str) -> ic_documents::command::Issue {
    let mut notice = kit.issue_spec(id, DocumentKind::LapseNotice, "lapse");
    notice.trigger_event = Some(event.into());
    notice.notice = Some(NoticeSpec {
        purpose: "lapse".into(),
        corrects: None,
    });
    notice
}

fn days(n: i64) -> chrono::Duration {
    chrono::Duration::days(n)
}

/// Purpose: a lapse notice for a policy assigned to a lender is refused
/// until the lender is among its recipients, and the trigger names them.
/// Why it matters: a lender who is not told the policy securing a loan is
/// in arrears cannot pay the premium to protect its security, which is the
/// reason the interest was noted.
#[test]
fn a_lapse_notice_must_be_copied_to_the_lender() {
    let mut kit = Kit::ready();
    let today = kit.today();
    observe(
        &mut kit,
        vec![recorded(1, today, "int-1", "lender", today - days(30)), standing(2, today, Standing::InGrace)],
    );
    assert_eq!(kit.state.triggers["7-2:LapseNotice"].copy_to, vec![party("lender")]);

    let alone = lapse_notice(&kit, "l1", "7-2");
    assert_eq!(kit.issue_err(alone), "interested_party_missing");

    let mut copied = lapse_notice(&kit, "l1", "7-2");
    copied.recipients.push(recipient("lender", RecipientRole::InterestedParty, &[Channel::Email]));
    kit.issue(copied);
    assert_eq!(kit.state.documents["l1"].deliveries.len(), 2);
    assert!(!kit.state.triggers.contains_key("7-2:LapseNotice"));
}

/// Purpose: a suspension notice is held to the same rule, and two interests
/// held by one party ask for one copy.
/// Why it matters: suspension stops cover, which a lender needs to know as
/// much as arrears, and a party listed twice would be refused as a
/// duplicate recipient so the notice could never be issued.
#[test]
fn a_suspension_notice_is_copied_once_to_each_party() {
    let mut kit = Kit::ready();
    let data = sample_data();
    kit.activate(
        template("suspension", DocumentKind::SuspensionNotice, "Dear {{name}}. Your cover is suspended."),
        &data,
    );
    let today = kit.today();
    observe(
        &mut kit,
        vec![
            recorded(1, today, "int-1", "lender", today),
            recorded(2, today, "int-2", "lender", today),
            recorded(3, today, "int-3", "ben", today),
            standing(4, today, Standing::Suspended),
        ],
    );
    assert_eq!(kit.state.triggers["7-4:SuspensionNotice"].copy_to, vec![party("ben"), party("lender")]);

    let mut notice = kit.issue_spec("s1", DocumentKind::SuspensionNotice, "suspension");
    notice.trigger_event = Some("7-4".into());
    notice.recipients.push(recipient("lender", RecipientRole::InterestedParty, &[Channel::Email]));
    assert_eq!(kit.issue_err(notice.clone()), "interested_party_missing");
    notice.recipients.push(recipient("ben", RecipientRole::InterestedParty, &[Channel::Post]));
    kit.issue(notice);
}

/// Purpose: an interest stops asking for copies from the date its release
/// takes effect, not before, and one that has not yet begun asks for none.
/// Why it matters: a lender whose loan is repaid has no right to the
/// owner's correspondence, and sending it would disclose personal
/// information to a party with no interest.
#[test]
fn a_released_or_future_interest_is_owed_nothing() {
    let mut kit = Kit::ready();
    let today = kit.today();
    observe(
        &mut kit,
        vec![
            recorded(1, today, "int-1", "lender", today - days(10)),
            released(2, today, "int-1", "lender", today + days(2)),
            recorded(3, today, "int-2", "ben", today + days(5)),
            // A release of an interest never reported changes nothing.
            released(4, today, "int-9", "ben", today),
            standing(5, today, Standing::InGrace),
        ],
    );
    assert_eq!(kit.state.interested_parties(&policy("pol-1"), today), vec![party("lender")]);
    assert_eq!(kit.state.interested_parties(&policy("pol-1"), today + days(2)), vec![]);
    assert_eq!(kit.state.interested_parties(&policy("pol-1"), today + days(5)), vec![party("ben")]);
    assert_eq!(kit.state.interested_parties(&policy("pol-2"), today), vec![]);

    // Before the release takes effect the lender is still owed the copy.
    let early = lapse_notice(&kit, "l1", "7-5");
    assert_eq!(kit.issue_err(early), "interested_party_missing");
    // Once it has, the notice goes to the owner alone, although the
    // trigger was raised while the interest stood.
    kit.advance_days(2);
    let later = lapse_notice(&kit, "l1", "7-5");
    kit.issue(later);
}

/// Purpose: the letter saying a policy has lapsed is copied to the lender
/// when it answers the core's event, while ordinary correspondence and
/// other documents are not held to the rule.
/// Why it matters: the lender must learn the security has ended, but
/// requiring a copy of every letter and receipt would send the owner's
/// routine correspondence to a third party.
#[test]
fn the_letter_that_a_policy_ended_is_copied_and_routine_letters_are_not() {
    let mut kit = Kit::ready();
    let data = sample_data();
    kit.activate(template("letter", DocumentKind::GeneralCorrespondence, "Dear {{name}}. A letter."), &data);
    let today = kit.today();
    observe(
        &mut kit,
        vec![
            recorded(1, today, "int-1", "lender", today),
            core(
                2,
                today,
                CoreEvent::PolicyStatusChanged {
                    policy: policy("pol-1"),
                    from: PolicyStatus::InForce,
                    to: PolicyStatus::Lapsed,
                },
            ),
            core(3, today, CoreEvent::PolicyReinstated { policy: policy("pol-1") }),
        ],
    );
    assert_eq!(kit.state.triggers["7-2:GeneralCorrespondence"].copy_to, vec![party("lender")]);
    assert!(kit.state.triggers["7-3:PolicySchedule"].copy_to.is_empty());

    let routine = kit.issue_spec("g1", DocumentKind::GeneralCorrespondence, "letter");
    kit.issue(routine);
    let receipt = kit.issue_spec("r1", DocumentKind::PremiumReceipt, "receipt");
    kit.issue(receipt);

    let mut ended = kit.issue_spec("g2", DocumentKind::GeneralCorrespondence, "letter");
    ended.trigger_event = Some("7-2".into());
    assert_eq!(kit.issue_err(ended.clone()), "interested_party_missing");
    ended.recipients.push(recipient("lender", RecipientRole::InterestedParty, &[Channel::Email]));
    kit.issue(ended);
}

/// Purpose: an interest reported by an event too old to raise documents is
/// still recorded, and consent events change nothing.
/// Why it matters: a module added to a running cluster replays history; if
/// it skipped old interest events with the old notices, every lender
/// recorded before it started would silently receive no copies.
#[test]
fn an_interest_from_replayed_history_still_stands() {
    let mut kit = Kit::ready();
    let today = kit.today();
    let long_ago = today - days(400);
    let consent = |seq: u64, used: bool| {
        let (policy, interest, party, action) = (policy("pol-1"), "int-1".to_string(), party("lender"), RestrictedAction::Surrender);
        core(
            seq,
            long_ago,
            CoreEvent::Interest(if used {
                InterestEvent::ConsentUsed {
                    policy,
                    interest,
                    party,
                    action,
                }
            } else {
                InterestEvent::ConsentRecorded {
                    policy,
                    interest,
                    party,
                    action,
                }
            }),
        )
    };
    observe(
        &mut kit,
        vec![
            recorded(1, long_ago, "int-1", "lender", long_ago),
            consent(2, false),
            consent(3, true),
            standing(4, long_ago, Standing::InGrace),
        ],
    );
    assert!(kit.state.triggers.is_empty());
    assert_eq!(kit.state.interested_parties(&policy("pol-1"), today), vec![party("lender")]);

    // A corrected or late notice with no pending trigger is held to the rule too.
    let notice = lapse_notice(&kit, "l1", "7-4");
    assert_eq!(kit.issue_err(notice), "interested_party_missing");
}

/// Purpose: the module answers who is owed a copy on a date, the interests
/// survive a save and restore, and a state saved before interests were
/// tracked still loads.
/// Why it matters: the documents service asks this question to address the
/// copy, a restored replica must enforce the same rule as the others, and
/// a snapshot from the running version must not stop the upgraded module
/// from starting.
#[test]
fn the_question_and_the_saved_state() {
    let mut kit = Kit::ready();
    let today = kit.today();
    observe(&mut kit, vec![recorded(1, today, "int-1", "lender", today)]);
    let ask = |state: &DocumentsState| {
        module::answer(
            state,
            serde_json::to_value(Query::InterestedParties {
                policy: policy("pol-1"),
                on: today,
            })
            .unwrap(),
        )
        .unwrap()
    };
    assert_eq!(ask(&kit.state), json!(["lender"]));

    let mut saved = serde_json::to_value(&kit.state).unwrap();
    let restored: DocumentsState = serde_json::from_value(saved.clone()).unwrap();
    assert_eq!(restored, kit.state);
    assert_eq!(ask(&restored), json!(["lender"]));

    saved.as_object_mut().unwrap().remove("interests").expect("the state saves its interests");
    let older: DocumentsState = serde_json::from_value(saved).unwrap();
    assert_eq!(ask(&older), json!([]));
}

/// Purpose: avoiding a policy for non-disclosure requires one avoidance
/// letter, a mandatory notice copied to any lender, and not the general
/// "your policy has ended" letter as well; and the policy's life is
/// recorded as ended for the retention of its contractual documents.
/// Why it matters: avoidance takes away cover the policyholder thought
/// they had and may keep their premium, so they must be told, provably,
/// once; a lender loses its security on the same day; and a schedule kept
/// for ever because the module never saw the policy end breaks the
/// retention rules.
#[test]
fn an_avoided_policy_requires_the_avoidance_letter() {
    let mut kit = Kit::ready();
    let today = kit.today();
    observe(
        &mut kit,
        vec![
            recorded(1, today, "int-1", "lender", today),
            core(
                2,
                today,
                CoreEvent::PolicyStatusChanged {
                    policy: policy("pol-1"),
                    from: PolicyStatus::InForce,
                    to: PolicyStatus::Avoided,
                },
            ),
            core(
                3,
                today,
                CoreEvent::PolicyAvoided {
                    policy: policy("pol-1"),
                    case: "nd-1".into(),
                    premium: ic_core::PremiumOnAvoidance::Refund,
                    refunded_minor: 120_00,
                },
            ),
        ],
    );
    let kinds: Vec<(&str, DocumentKind)> = kit.state.triggers.values().map(|t| (t.id.as_str(), t.kind)).collect();
    assert_eq!(kinds, vec![("7-3:AvoidanceNotice", DocumentKind::AvoidanceNotice)]);
    assert_eq!(kit.state.triggers["7-3:AvoidanceNotice"].copy_to, vec![party("lender")]);
    assert_eq!(DocumentKind::AvoidanceNotice.class(), ic_documents::DocumentClass::MandatoryNotice);

    let data = sample_data();
    kit.activate(
        template("avoidance", DocumentKind::AvoidanceNotice, "Dear {{name}}. We have avoided your policy."),
        &data,
    );
    let mut letter = kit.issue_spec("a1", DocumentKind::AvoidanceNotice, "avoidance");
    letter.trigger_event = Some("7-3".into());
    assert_eq!(kit.issue_err(letter.clone()), "interested_party_missing");
    letter.recipients.push(recipient("lender", RecipientRole::InterestedParty, &[Channel::Post]));
    kit.issue(letter);
    assert!(kit.state.documents["a1"].requires_proof());
    assert!(kit.state.triggers.is_empty());

    // The policy's life ended on the day it was avoided: a schedule issued
    // under it is now kept for a period from that day, not for ever.
    let mut schedule = kit.issue_spec("s1", DocumentKind::PolicySchedule, "schedule");
    schedule.wordings = vec![tref("wording", 1)];
    kit.issue(schedule);
    let answer = ic_documents::module::answer(&kit.state, json!({ "policy_documents": { "policy": "pol-1" } })).unwrap();
    let s1 = answer.as_array().unwrap().iter().find(|d| d["id"] == "s1").unwrap().clone();
    assert_eq!(s1["destroy_after"], json!((today + days(2557)).to_string()), "seven years from the avoidance");
    // A policy still in force keeps its schedule with no date set.
    let mut other = kit.issue_spec("s2", DocumentKind::PolicySchedule, "schedule");
    other.policy = Some(policy("pol-2"));
    other.wordings = vec![tref("wording", 1)];
    kit.issue(other);
    assert_eq!(kit.state.documents["s2"].destroy_after, None);
}
