//! An assessed claim run through the real claims module hosted in the real core, with
//! the finance ledger fed the core's own events: the facts the mart reads arrive by the
//! keys the claims module really publishes.

mod common;

use chrono::NaiveDate;
use common::*;
use ic_core::claims_module as claims;
use ic_core::{Command, PayableKind, PolicyId};
use ic_money::{Currency::NZD, Money};
use ic_product::{Frequency, Sex};
use ic_sim::persona::{issue_command, term_life};

use claims::command::{Notification, Periodic};
use claims::config::{Profile, Staff};
use claims::{Actor, AuthorityBook, Channel, ClaimType, DecisionKind, Flags, Line, Notifier, PayeeKind, Queue, Request, Role, Rules, RunKind};

fn actor(who: &str) -> Actor {
    Actor {
        staff: who.to_string(),
        purpose: "working the claim".to_string(),
    }
}

fn rules() -> Rules {
    let kinds = [
        DecisionKind::Reserve,
        DecisionKind::Admit,
        DecisionKind::Decline,
        DecisionKind::ReverseAdmission,
        DecisionKind::EndBenefit,
    ];
    Rules {
        version: 1,
        escalation_days: [1, 3, 7],
        large_claim_minor: i64::MAX,
        two_signature_admission_minor: i64::MAX,
        two_signature_ex_gratia_minor: i64::MAX,
        two_signature_write_off_minor: i64::MAX,
        sharp_reserve_move_pct: 1_000,
        reason_codes: [("R-1".to_string(), kinds.into_iter().collect())].into(),
        cause_codes: [("CC-0001".to_string(), vec!["cancer.skin".to_string()])].into(),
        medical_review_months: 6,
        income_declaration_months: 3,
        review_grace_days: 14,
        max_offset_pct: 25,
        late_death_days: 30,
        estate_simplified_max_minor: i64::MAX,
        pre_approval_days: 30,
        recurrence_days: 180,
        prepared_by: "config-ann".into(),
        approved_by: vec!["config-bob".into(), "config-cat".into()],
        ..Rules::default()
    }
}

fn authority() -> AuthorityBook {
    let types = [ClaimType::Trauma, ClaimType::IncomeProtection];
    let senior = Profile {
        lines: [Line::LumpSum, Line::IncomeProtection].into(),
        max_admission_minor: types.into_iter().map(|t| (t, i64::MAX)).collect(),
        max_payment_minor: i64::MAX,
        max_reserve_minor: i64::MAX,
        may_decline: true,
        max_ex_gratia_minor: 0,
        max_write_off_minor: 0,
        may_override_exclusion: false,
        may_reverse_admission: true,
        may_change_recovery_status: false,
        senior: true,
    };
    let person = |roles: &[Role]| Staff {
        profile: "senior".to_string(),
        roles: roles.iter().copied().collect(),
        queues: [Queue::LumpSum, Queue::IncomeProtectionNew, Queue::IncomeProtectionOngoing, Queue::ComplexLarge].into(),
        wip_limit: 0,
        vulnerable_trained: true,
        related_parties: Default::default(),
    };
    AuthorityBook {
        version: 1,
        profiles: [("senior".to_string(), senior)].into(),
        staff: [
            ("sam".to_string(), person(&[Role::Assessor, Role::QueueLead])),
            ("sue".to_string(), person(&[Role::Assessor, Role::ClaimsManager])),
        ]
        .into(),
        prepared_by: "config-ann".into(),
        approved_by: vec!["config-bob".into(), "config-cat".into()],
    }
}

fn wrap(command: claims::Command) -> Command {
    Command::Claims(Box::new(command))
}

fn ok(w: &mut World, p: u16, command: claims::Command) {
    let name = command.name();
    if let Err(e) = w.core.submit(p, "claims", wrap(command)) {
        panic!("{name} was refused: {e}");
    }
}

/// Notify, acknowledge, give to sam, record the cause and check the cover.
fn open_claim(w: &mut World, p: u16, claim: &str, policy: &PolicyId, claim_type: ClaimType, event_date: NaiveDate) {
    let notification = Notification {
        claim: claim.into(),
        policy: policy.0.clone(),
        cover: "trauma".into(),
        life: Some("life-1".into()),
        claim_type,
        event_date,
        channel: Channel::App,
        notifier: Notifier::Policyholder,
        claimant: Some("party-claimant".into()),
        claimed_minor: None,
        flags: Flags::default(),
        health: None,
        underwriters: vec![],
        catastrophe_event: None,
        cause_code: None,
    };
    ok(w, p, claims::Command::Notify(Box::new(notification)));
    ok(
        w,
        p,
        claims::Command::Acknowledge {
            claim: claim.into(),
            delivery_ref: "delivered".into(),
        },
    );
    ok(
        w,
        p,
        claims::Command::Assign {
            claim: claim.into(),
            to: "sam".into(),
            actor: actor("sam"),
        },
    );
    ok(
        w,
        p,
        claims::Command::RecordCause {
            claim: claim.into(),
            cause_code: "CC-0001".into(),
            actor: actor("sam"),
        },
    );
    ok(
        w,
        p,
        claims::Command::CoverCheck {
            claim: claim.into(),
            relied_on: vec!["in_force".into()],
            actor: actor("sam"),
        },
    );
}

/// Open and complete a task on a claim; a new one of `kind`, or one the claim already has.
fn task(w: &mut World, claim: &str, task: &str, kind: Option<claims::TaskKind>) {
    if let Some(kind) = kind {
        ok(
            w,
            0,
            claims::Command::OpenTask {
                claim: claim.into(),
                task: task.into(),
                kind,
                due: None,
                actor: actor("sam"),
            },
        );
    }
    ok(
        w,
        0,
        claims::Command::CompleteTask {
            claim: claim.into(),
            task: task.into(),
            outcome: "held".into(),
            attendees: vec!["sam".into(), "sue".into()],
            vault_ref: None,
            spend_minor: 0,
            actor: actor("sam"),
        },
    );
}

fn decide(claim: &str, request: Request) -> claims::Command {
    claims::Command::Decide {
        claim: claim.into(),
        request,
        reasons: vec!["R-1".into()],
        actor: actor("sam"),
    }
}

/// Purpose: on a term life policy with trauma cover, an income claim is notified on 22
/// November 2027, assessed (cause, cover check) and admitted the same day with a benefit
/// of $3,000 a month, the first month paid and settled; on 6 December the admission is
/// reversed by two seniors after a case conference and a talk with the claimant. A trauma
/// claim on a second policy is declined by two people on 24 November, after a talk with the claimant. The finance ledger is fed every
/// event and Merkle root the core published. November's experience extract counts one
/// admitted claim with $3,000 a month in the term life trauma cell; December's takes the
/// claim and the monthly benefit back. November's conduct report shows one acceptance
/// and one decline, each with its turnaround from registration; December's shows one
/// admission reversed, against November. The claims by cause extract holds both claims
/// under `cancer`, withheld because two claims are fewer than five. The books never stall.
/// Why it matters: the other tests write the claims module's facts by hand. This is the
/// one that fails if an assessed decision, a benefit in payment or a reversal stops
/// reaching the actuary's experience study and the conduct return in the shape the
/// finance ledger reads, and it runs the claims postings through the books at the same
/// time.
#[test]
fn an_assessed_claim_admitted_paid_and_reversed_reaches_the_mart_and_the_books() {
    let start = d(2027, 11, 15);
    let mut w = World::new(1, start);
    w.core
        .activate_everywhere(wrap(claims::Command::ActivateRules { rules: Box::new(rules()) }))
        .unwrap();
    w.core
        .activate_everywhere(wrap(claims::Command::ActivateAuthority { book: Box::new(authority()) }))
        .unwrap();
    let mut policies = vec![];
    for n in 1..=2u64 {
        let issue = term_life(
            n,
            start,
            d(1990, 3, 10),
            Sex::Female,
            false,
            500_000_00,
            Some(100_000_00),
            Frequency::Monthly,
            None,
        );
        let id = issue.policy.clone();
        w.core.submit(0, "new-business", issue_command(issue)).unwrap();
        w.pay(0, &id, &format!("r-{n}"), 2_000_00);
        policies.push(id);
    }
    w.until(d(2027, 11, 22));
    let first = d(2027, 11, 22);
    open_claim(&mut w, 0, "clm-ip", &policies[0], ClaimType::IncomeProtection, first);
    let periodic = Periodic {
        monthly_minor: 3_000_00,
        first_period: first,
        end: d(2029, 11, 22),
        expected_end: d(2028, 11, 22),
    };
    let admit = Request::Admit {
        amount_minor: None,
        periodic: Some(periodic),
        exclusion_override: None,
        exclusion_reviewed: false,
        payer_deceased: false,
    };
    ok(&mut w, 0, decide("clm-ip", admit));
    ok(
        &mut w,
        0,
        claims::Command::VerifyPayee {
            claim: "clm-ip".into(),
            party: "party-claimant".into(),
            kind: PayeeKind::Policyholder,
            account_ref: "acct-1".into(),
            name_check_ref: "name-1".into(),
            screening_ref: "screen-1".into(),
            authority_doc: None,
            actor: actor("sam"),
        },
    );
    ok(
        &mut w,
        0,
        claims::Command::PrepareRun {
            run: "run-1".into(),
            kind: RunKind::Benefit,
            benefit_date: first,
            actor: actor("sue"),
        },
    );
    ok(
        &mut w,
        0,
        claims::Command::ApproveRun {
            run: "run-1".into(),
            actor: actor("sam"),
        },
    );
    let settle = Command::SettlePayable {
        policy: policies[0].clone(),
        payment: format!("run-1/clm-ip/{first}"),
        payable: PayableKind::Benefit,
        refund: None,
        amount: Money::new(3_000_00, NZD),
        bank_account: "nz-main-claims".into(),
    };
    w.core.submit(0, "payments", settle).unwrap();

    w.until(d(2027, 11, 24));
    open_claim(&mut w, 0, "clm-t", &policies[1], ClaimType::Trauma, d(2027, 11, 20));
    task(&mut w, "clm-t", "talk-t", Some(claims::TaskKind::ClaimantConversation));
    ok(&mut w, 0, decide("clm-t", Request::Decline));
    ok(
        &mut w,
        0,
        claims::Command::Approve {
            claim: "clm-t".into(),
            actor: actor("sue"),
        },
    );

    w.until(d(2027, 12, 6));
    ok(
        &mut w,
        0,
        claims::Command::Escalate {
            claim: "clm-ip".into(),
            actor: actor("sam"),
        },
    );
    let conference = w.core.state(0).claims.claims["clm-ip"]
        .task(claims::TaskKind::CaseConference)
        .next()
        .unwrap()
        .id
        .clone();
    task(&mut w, "clm-ip", &conference, None);
    task(&mut w, "clm-ip", "talk-1", Some(claims::TaskKind::ClaimantConversation));
    ok(&mut w, 0, decide("clm-ip", Request::ReverseAdmission { false_declaration: false }));
    ok(
        &mut w,
        0,
        claims::Command::Approve {
            claim: "clm-ip".into(),
            actor: actor("sue"),
        },
    );
    w.until(d(2028, 1, 3));
    assert!(w.books.state.stalled.is_empty(), "{:?}", w.books.state.stalled);

    let (nov, dec) = (period(2027, 11), period(2027, 12));
    let mart = &w.books.state.mart;
    let cells = |p: ic_finance::PeriodId| {
        let e = mart.experience(p.start(), p.end()).unwrap();
        assert!(e.reconciled);
        let header: Vec<String> = e.lines[0].split(',').map(str::to_string).collect();
        e.lines[1..]
            .iter()
            .map(|l| {
                header
                    .iter()
                    .cloned()
                    .zip(l.split(',').map(str::to_string))
                    .collect::<std::collections::BTreeMap<_, _>>()
            })
            .filter(|r| r["admitted_claims"] != "0" || r["admitted_monthly_minor"] != "0")
            .map(|r| {
                (
                    r["product"].clone(),
                    r["cover_type"].clone(),
                    r["admitted_claims"].clone(),
                    r["admitted_monthly_minor"].clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    let row = |claims: &str, monthly: &str| ("term-life".to_string(), "trauma".to_string(), claims.to_string(), monthly.to_string());
    assert_eq!(cells(nov), [row("1", "300000")]);
    assert_eq!(cells(dec), [row("-1", "-300000")]);

    let november = w.books.state.conduct_report(nov, None);
    assert_eq!((november.accepted, november.declined), (1, 1));
    assert_eq!((november.accepted_turnaround.decisions, november.accepted_turnaround.max_days), (1, 0));
    assert_eq!((november.declined_turnaround.decisions, november.declined_turnaround.max_days), (1, 0));
    let december = w.books.state.conduct_report(dec, None);
    assert_eq!(december.admissions_reversed.into_iter().collect::<Vec<_>>(), [(nov, 1)]);

    let by_cause = mart.claims_by_cause(nov.start(), nov.end()).unwrap();
    assert_eq!(by_cause.lines.len(), 1);
    assert_eq!(by_cause.control["withheld_claims"], 2);
}
