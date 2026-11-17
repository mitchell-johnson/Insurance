//! What the second scenario run found in underwriting (SCN2-02, SCN2-03):
//! a conflicted underwriter reading a proposal, refusals that did not say
//! why, a tele-interview booked in the past, "look again" without end, a
//! peer review that drew nothing, and dates from the wall clock.

mod common;

use std::collections::BTreeSet;

use common::*;
use ic_core::{LifeId, PartyId};
use ic_underwriting::authority::{refusal, AuthorityProfile, CaseAction, Role, Standing, Subject};
use ic_underwriting::case::{CaseType, Queue};
use ic_underwriting::command::{Command, Event, OfferResponse, View};
use ic_underwriting::config::{SampleRates, Settings};
use ic_underwriting::evidence::EvidenceKind;
use ic_underwriting::model::ConsentKind;
use ic_underwriting::questions::Answer;
use ic_underwriting::special::{answer, Query};
use serde_json::json;

fn console(w: &mut World, c: Command) -> Result<ic_underwriting::command::Applied, ic_underwriting::command::Reject> {
    w.send("underwriting-console", c)
}

fn ask(w: &World, q: Query) -> serde_json::Value {
    answer(&w.s, serde_json::to_value(q).unwrap()).unwrap()
}

/// Purpose: once an underwriter declares a conflict with someone on an application, the
/// console's proposal question answers only that it is closed to them, and their queue leaves
/// the case out; anyone else reads both as before, and the declaration is recorded with its reason.
/// Why it matters: a conflicted underwriter could read a cousin's whole application through
/// the proposal question, found by the scenario run, F2-03-1; that is a privacy breach.
#[test]
fn a_conflicted_underwriter_reads_nothing_of_the_proposal() {
    let mut w = World::new();
    w.open("P1", 300_000_00);
    w.disclose("P1", "L1", &yes("heart_condition"));
    let declared = console(
        &mut w,
        Command::DeclareConflict {
            underwriter: "amy".into(),
            party: PartyId::from("PTY-L1"),
            reason: Some("my cousin".into()),
        },
    )
    .unwrap();
    assert!(matches!(&declared.events[0], Event::ConflictDeclared { reason: Some(r), .. } if r == "my cousin"));
    let read = |w: &World, who: &str| {
        ask(
            w,
            Query::ProposalFor {
                proposal: "P1".into(),
                underwriter: who.into(),
            },
        )
    };
    let closed = read(&w, "amy");
    assert_eq!(closed.as_object().map(|o| o.len()), Some(1), "nothing of it: {closed}");
    assert!(closed["closed_to_you"].as_str().unwrap().contains("closed to you"));
    assert_eq!(read(&w, "abe")["id"], "P1");
    assert!(ask(
        &w,
        Query::ProposalFor {
            proposal: "P9".into(),
            underwriter: "abe".into()
        }
    )
    .is_null());
    let queue = |w: &World, who: Option<&str>| {
        ask(
            w,
            Query::Queue {
                queue: Queue::Medical,
                now: w.now,
                underwriter: who.map(str::to_string),
            },
        )
        .as_array()
        .unwrap()
        .len()
    };
    assert_eq!((queue(&w, Some("amy")), queue(&w, Some("abe")), queue(&w, None)), (0, 1, 1));
}

fn profile(senior: bool, role: Role) -> AuthorityProfile {
    let s = ic_underwriting::shipped::load().unwrap();
    AuthorityProfile {
        underwriter: "st1".into(),
        senior,
        role,
        ..s.grades[0].clone()
    }
}

fn subject(sum: i64, loading: u32, exclusions: bool, outcome: &'static str) -> Subject<'static> {
    Subject {
        product: "aia-living",
        cover_type: "life",
        sum_insured_minor: sum,
        loading_percent: loading,
        has_exclusions: exclusions,
        outcome,
    }
}

/// Purpose: a refusal by the authority policies says why in words and who can act instead:
/// a conflict, a product not accredited, a cover type the profile does not decide, an amount
/// above the limit with both amounts in dollars, a loading, an exclusion, a decline or a deferral
/// beyond the profile, the second signature or a review by someone who may not give it, and a
/// move that is a queue lead's.
/// Why it matters: every refusal read "st1 is not authorised for this", so a junior could not
/// tell their limit or whom to hand the case to, found by the scenario run, F2-02-10, F2-03-3
/// and F2-03-6.
#[test]
fn an_authority_refusal_says_why_and_who_can_act() {
    let junior = profile(false, Role::Underwriter);
    let say = |p: &AuthorityProfile, action, s: Subject<'_>, standing| refusal(p, action, &s, standing);
    let none = Standing::default();
    assert!(say(&junior, CaseAction::Work, subject(1, 0, false, "none"), Standing { conflict: true, ..none }).contains("declared a conflict"));
    let mut other = junior.clone();
    other.products = BTreeSet::new();
    assert!(say(&other, CaseAction::Work, subject(1, 0, false, "none"), none).contains("not accredited for aia-living"));
    let mut no_life = junior.clone();
    no_life.limits.remove("life");
    assert!(say(&no_life, CaseAction::Decide, subject(1, 0, false, "standard"), none).contains("may not decide life cover"));
    assert_eq!(
        say(&junior, CaseAction::Decide, subject(800_000_00, 0, false, "standard"), none),
        "life cover of $800,000.00 is above st1's limit of $500,000.00: hand the case to a senior underwriter whose limit covers it"
    );
    assert!(say(&junior, CaseAction::Decide, subject(1, 50, false, "terms"), none).contains("may not apply a loading (50% was asked for)"));
    let mut some = junior.clone();
    some.max_loading_percent = 25;
    assert!(say(&some, CaseAction::Decide, subject(1, 50, false, "terms"), none).contains("above the most st1 may apply, 25%"));
    assert!(say(&junior, CaseAction::Decide, subject(1, 0, true, "terms"), none).contains("may not apply an exclusion"));
    assert!(say(&junior, CaseAction::Decide, subject(1, 0, false, "decline"), none).contains("may not decline cover"));
    assert!(say(&junior, CaseAction::Decide, subject(1, 0, false, "defer"), none).contains("may not defer cover"));
    assert!(say(&junior, CaseAction::Decide, subject(1, 0, false, "standard"), none).contains("not authorised"));
    assert!(say(&junior, CaseAction::Countersign, subject(1, 0, false, "standard"), none).contains("second signature is for a senior"));
    let senior = profile(true, Role::QueueLead);
    assert!(say(
        &senior,
        CaseAction::Countersign,
        subject(1, 0, false, "standard"),
        Standing { already_signed: true, ..none }
    )
    .contains("made the first decision"));
    assert!(say(&junior, CaseAction::PeerReview, subject(1, 0, false, "none"), none).contains("a review is for a senior"));
    assert_eq!(
        say(
            &senior,
            CaseAction::PeerReview,
            subject(1, 0, false, "none"),
            Standing { touched: true, ..none }
        ),
        "st1 worked on or decided this case, so another senior underwriter reviews it"
    );
    assert!(say(&junior, CaseAction::Reassign, subject(1, 0, false, "none"), none).contains("queue lead or a manager"));
    assert!(say(&senior, CaseAction::ReviewNonDisclosure, subject(1, 0, false, "none"), none).contains("not authorised"));

    // Through the module: an assistant's loading is refused naming the limit.
    let mut w = World::new();
    w.open("P1", 300_000_00);
    w.disclose("P1", "L1", &yes("heart_condition"));
    let case = w.take("P1", "L1", "ash");
    let refused = w.propose(&case, "ash", vec![accept("life", 50, None)]).unwrap_err();
    assert_eq!(refused.code, "not_authorised");
    assert!(refused.message.contains("ash may not apply a loading"), "{}", refused.message);
}

/// Purpose: a tele-interview is booked only for a time still to come, on the books' calendar
/// and the clock; a queue lead moves a case another underwriter holds, and anyone else is told
/// it is a queue lead's to move.
/// Why it matters: an interview was booked for March 2025 and accepted (F2-03-7), and a queue
/// lead could not move a colleague's case to the right queue (F2-03-8), found by the scenario run.
#[test]
fn an_interview_is_booked_ahead_and_a_queue_lead_moves_a_case() {
    let mut w = World::new();
    w.open("P1", 300_000_00);
    let book = |when| Command::ScheduleInterview {
        proposal: "P1".into(),
        life: "L1".into(),
        scheduled_for: when,
        booked_by: "sam".into(),
    };
    let past = w.now - chrono::Duration::days(400);
    let refused = console(&mut w, book(past)).unwrap_err();
    assert_eq!(refused.code, "scheduled_for");
    assert_eq!(
        {
            let now = w.now;
            console(&mut w, book(now))
        }
        .unwrap_err()
        .code,
        "scheduled_for",
        "not now either"
    );
    let later = w.now + chrono::Duration::days(2);
    console(&mut w, book(later)).unwrap();

    let case = w.take("P1", "L1", "ash");
    let mover = |who: &str| Command::MoveCase {
        case: case.clone(),
        underwriter: who.into(),
        case_type: CaseType::Referral,
        queue: Queue::Medical,
        reasons: set(&["RC-0201"]),
    };
    let refused = console(&mut w, mover("amy")).unwrap_err();
    assert!(refused.message.contains("queue lead or a manager"), "{}", refused.message);
    console(&mut w, mover("sam")).unwrap();
    assert_eq!(w.p("P1").lives[&LifeId::from("L1")].open_case().unwrap().queue, Queue::Medical);
}

/// Purpose: the life asks us to look again at terms once: a second "look again" is refused
/// with what they can do instead. The life's progress row carries each open offer's terms,
/// whether they may still ask us to look again, and the policy it would issue as on those terms.
/// Why it matters: a customer opened a third case by asking again and again (F2-02-9), and
/// accepted terms she was never shown (F2-02-7), found by the scenario run.
#[test]
fn terms_are_looked_at_again_once_and_shown_with_the_offer() {
    let mut w = World::new();
    w.open("P1", 300_000_00);
    w.disclose("P1", "L1", &yes("heart_condition"));
    let offer_on = |w: &mut World| {
        let case = w.case_of("P1", "L1");
        if w.p("P1").lives[&LifeId::from("L1")].open_case().unwrap().owner.is_none() {
            w.take("P1", "L1", "amy");
        }
        let decided = w.propose(&case, "amy", vec![accept("life", 50, None)]).unwrap();
        w.automatic(&decided);
    };
    offer_on(&mut w);
    let rows = ic_underwriting::progress::rows(w.p("P1"));
    let life = rows.iter().find(|r| r.kind == "proposal_life").unwrap();
    let offer = &life.body["offer"];
    assert_eq!(offer["terms"][0]["outcome"]["loading_percent"], 50, "{offer}");
    assert_eq!(offer["look_again"], true);
    assert_eq!(offer["application"]["covers"][0]["loadings"], json!(["0.50"]), "{offer}");
    w.respond("P1", "L1", OfferResponse::Counter);
    offer_on(&mut w);
    let rows = ic_underwriting::progress::rows(w.p("P1"));
    assert_eq!(rows.iter().find(|r| r.kind == "proposal_life").unwrap().body["offer"]["look_again"], false);
    let open = w.p("P1").lives[&LifeId::from("L1")].open_offer().unwrap().id.clone();
    let again = Command::RespondToOffer {
        proposal: "P1".into(),
        life: "L1".into(),
        offer: open,
        response: OfferResponse::Counter,
        signed: signed(
            "offer",
            &["P1", "L1", &w.p("P1").lives[&LifeId::from("L1")].open_offer().unwrap().id, "counter"],
        ),
    };
    let refused = w.send("new-business", again).unwrap_err();
    assert_eq!(refused.code, "looked_again");
    assert!(refused.message.contains("accept or decline"));
    w.respond("P1", "L1", OfferResponse::Accept);
}

/// Purpose: a peer review period is drawn once, under any name, and the draw says how many
/// decisions it was drawn from; a period with decisions to review draws at least one, the one
/// nearest its rate; and the report counts covers apart from decisions.
/// Why it matters: every draw answered nothing and could be redrawn with a new seed until the
/// sample suited the reviewer, and the report said "1 decision" beside three outcomes, found by
/// the scenario run, F2-03-5.
#[test]
fn a_peer_review_period_is_drawn_once_and_never_empty() {
    let mut w = World::new();
    let low = Settings {
        version: 2,
        sample: SampleRates {
            standard_per_mille: 1,
            non_standard_per_mille: 1,
            decline_per_mille: 1,
        },
        ..Settings::default()
    };
    w.ok(
        "config",
        Command::ActivateSettings {
            settings: Box::new(low),
            approval: approval(),
        },
    );
    for id in ["P1", "P2", "P3"] {
        w.open(id, 100_000_00);
        let d = w.disclose(id, "L1", &clean());
        w.automatic(&d);
    }
    let (from, to) = (w.now - chrono::Duration::days(1), w.now + chrono::Duration::days(1));
    let draw = |period: &str| Command::DrawPeerReviewSample {
        period: period.into(),
        from,
        to,
        seed: 3,
    };
    let drawn = console(&mut w, draw("2026-11")).unwrap();
    let Some(View::Sample { decisions, considered }) = drawn.view else { panic!() };
    assert_eq!((decisions.len(), considered), (1, 3), "one of three, though the rate would draw none");
    assert_eq!(console(&mut w, draw("2026-11")).unwrap_err().code, "already_drawn");
    let report = w.s.report(from, to);
    assert_eq!((report.decisions, report.covers), (3, 3));
    // A period with nothing in it draws nothing, and says so.
    let later = w.now + chrono::Duration::days(40);
    let empty = console(
        &mut w,
        Command::DrawPeerReviewSample {
            period: "2026-12".into(),
            from: later,
            to: later + chrono::Duration::days(1),
            seed: 3,
        },
    )
    .unwrap();
    assert_eq!(
        empty.view,
        Some(View::Sample {
            decisions: vec![],
            considered: 0
        })
    );
}

/// Purpose: an underwriter asks for evidence by a day they choose, after today; without one
/// the deadline runs the settings' days from the books' date, not the clock's.
/// Why it matters: the evidence form had no due date and the deadline came from the wall
/// clock, a year before the books' date, found by the scenario run, F2-02-6 and F2-02-8.
#[test]
fn evidence_is_due_on_the_day_chosen_on_the_books_calendar() {
    let mut w = World::new();
    w.open("P1", 300_000_00);
    w.consent("P1", "L1", ConsentKind::ObtainRecords);
    w.disclose("P1", "L1", &yes("hazardous_occupation"));
    let case = w.take("P1", "L1", "amy");
    let ask_for = |kind, due_on| Command::RequestEvidence {
        case: case.clone(),
        underwriter: "amy".into(),
        kinds: BTreeSet::from([kind]),
        due_on,
    };
    let today = w.today();
    assert_eq!(console(&mut w, ask_for(EvidenceKind::GpReport, Some(today))).unwrap_err().code, "due_on");
    console(&mut w, ask_for(EvidenceKind::GpReport, Some(d(2026, 12, 1)))).unwrap();
    // The books are a year ahead of the clock: the deadline follows the books.
    w.index += 1;
    let envelope = ic_underwriting::command::Envelope {
        key: "books".into(),
        caller: "underwriting-console".into(),
        at: w.now,
        today: d(2027, 11, 16),
        command: ask_for(EvidenceKind::MedicalExam, None),
    };
    use ic_log::StateMachine;
    w.s.apply(w.index, envelope).unwrap();
    let evidence = &w.p("P1").lives[&LifeId::from("L1")].evidence;
    assert_eq!(evidence[0].deadline.date_naive(), d(2026, 12, 1));
    assert!(evidence[1].deadline.date_naive() > d(2027, 11, 16), "{}", evidence[1].deadline);
}

/// Purpose: underwriting answers whether a product takes an application before anyone prices
/// it: AIA Essentials takes at most $500,000 on a life aged 61 to 70, said in dollars.
/// Why it matters: the quote priced $600,000 of cover for a 62-year-old that the product
/// forbids, found by the scenario run, F2-01-1.
#[test]
fn the_product_conditions_are_asked_before_a_quote() {
    let w = World::new();
    let question = |sum: i64| {
        json!({ "query": "eligibility", "product_code": "aia-essentials-life", "on": "2027-11-15",
            "lives": [{ "life": "life-1", "party": "quote", "factors": { "date_of_birth": "1965-06-01" } }],
            "covers": [{ "code": "life", "cover_type": "life", "life": "life-1", "sum_insured_minor": sum }] })
    };
    let over = answer(&w.s, question(600_000_00)).unwrap();
    assert_eq!(over["eligible"], false);
    assert_eq!(
        over["why"],
        "life on life-1: $600,000.00 is more than this cover takes at ages 61 to 70, at most $500,000.00"
    );
    assert_eq!(answer(&w.s, question(400_000_00)).unwrap(), json!({ "eligible": true }));
}

/// Purpose: from 15 November 2027 the questions ask about blood pressure or cholesterol,
/// asthma or a lung condition, and current medication, with a free-text question last; a yes to
/// any of them, or anything written, sends the life to a person in the medical queue.
/// Why it matters: the reasonable care set had no question for a blood pressure tablet and no
/// free text, so it could only be squeezed into "heart condition", found by the scenario run,
/// F2-02-4.
#[test]
fn the_reasonable_care_questions_take_medication_and_free_text() {
    let mut w = World::new();
    w.now = chrono::DateTime::parse_from_rfc3339("2027-11-16T09:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let set = w.s.config.question_set_on("personal-statement", w.today()).unwrap().clone();
    for q in [
        "high_blood_pressure_or_cholesterol",
        "asthma_or_lung_condition",
        "current_medication",
        "anything_else",
    ] {
        assert!(set.question(q).is_some(), "{q}");
    }
    let no = Answer::YesNo(false);
    let base = with(&[
        ("high_blood_pressure_or_cholesterol", no.clone()),
        ("asthma_or_lung_condition", no.clone()),
        ("current_medication", no),
    ]);
    for (i, (question, said)) in [
        ("current_medication", Answer::YesNo(true)),
        ("high_blood_pressure_or_cholesterol", Answer::YesNo(true)),
        ("asthma_or_lung_condition", Answer::YesNo(true)),
        ("anything_else", Answer::Text("I take a blood pressure tablet".into())),
    ]
    .into_iter()
    .enumerate()
    {
        let id = format!("P{i}");
        w.open(&id, 300_000_00);
        let mut answers = base.clone();
        let version = set.question(question).unwrap().version;
        answers.insert(question.to_string(), (version, said));
        let applied = w.disclose(&id, "L1", &answers);
        assert!(
            applied.events.iter().any(|e| matches!(e, Event::Referred { queue: Queue::Medical, .. })),
            "{question}: {:?}",
            applied.events
        );
    }
    w.open("PC", 300_000_00);
    let clean = w.disclose("PC", "L1", &base);
    assert!(!clean.events.iter().any(|e| matches!(e, Event::Referred { .. })));
}

/// The module with only a business date to give, as the books would.
struct Dated(Option<chrono::NaiveDate>, bool);

type Unavailable = ic_underwriting::driver::DriverError;

impl ic_underwriting::driver::Underwriting for Dated {
    async fn propose(
        &self,
        _envelope: ic_underwriting::command::Envelope,
    ) -> Result<Result<ic_underwriting::command::Applied, ic_underwriting::command::Reject>, Unavailable> {
        Err(Unavailable::Unavailable("not here".into()))
    }
    async fn ask(&self, _query: Query) -> Result<serde_json::Value, Unavailable> {
        Err(Unavailable::Unavailable("not here".into()))
    }
    async fn business_date(&self) -> Result<Option<chrono::NaiveDate>, Unavailable> {
        if self.1 {
            return Err(Unavailable::Unavailable("the books are not answering".into()));
        }
        Ok(self.0)
    }
}

/// Purpose: the day a command is made on is the books' business date where the books have one,
/// the clock's only while they have none, and nothing is dated when the books cannot be asked.
/// Why it matters: a policy was issued a year before the books' date and billed three periods at
/// once, because underwriting dated its work by the wall clock, found by the scenario run, F2-02-8.
#[tokio::test]
async fn work_is_dated_on_the_books_business_date() {
    use ic_underwriting::driver::{books_today, Underwriting};
    let at = chrono::DateTime::parse_from_rfc3339("2026-11-17T09:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let books = chrono::NaiveDate::from_ymd_opt(2027, 11, 15).unwrap();
    assert_eq!(books_today(&Dated(Some(books), false), at).await.unwrap(), books);
    assert_eq!(books_today(&Dated(None, false), at).await.unwrap(), at.date_naive());
    assert!(books_today(&Dated(None, true), at).await.is_err());
    let nothing = Dated(None, false);
    let sweep = ic_underwriting::command::Envelope {
        key: "k".into(),
        caller: "timer".into(),
        at,
        today: books,
        command: Command::Sweep { after: None, max: 1 },
    };
    assert!(nothing.propose(sweep).await.is_err());
    assert!(nothing.ask(Query::Outbox { after: 0, max: 1 }).await.is_err());
}
