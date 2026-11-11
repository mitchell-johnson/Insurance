//! Claims on the real partition: every claims decision posts through the
//! core's ledger, and the claim records, the core's claim ledger and the
//! journal are reconciled after every step.

use chrono::NaiveDate;
use ic_core::accounts;
use ic_core::claims_module as claims;
use ic_core::event::Event;
use ic_core::{Command, GroupId, PayableKind, PolicyId, PolicyStatus, Reject};
use ic_ledger::PartitionId;
use ic_money::{Currency::NZD, Money};
use ic_product::{Frequency, Sex};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, term_life};
use ic_sim::Cluster;

use claims::command::{Notification, Periodic};
use claims::config::{Exclusion, Profile, Staff};
use claims::model::{EndReason, RecoveryStatus};
use claims::{Actor, AuthorityBook, Channel, ClaimType, DecisionKind, Flags, Line, Notifier, PayeeKind, Queue, Request, Role, Rules, RunKind, Status};

const CLAIMS_BANK: &str = "nz-main-claims";

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn start() -> NaiveDate {
    d(2027, 11, 15)
}

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
        DecisionKind::Withdraw,
        DecisionKind::ReverseAdmission,
        DecisionKind::ExGratia,
        DecisionKind::EndBenefit,
        DecisionKind::RecoveryStatus,
        DecisionKind::WriteOff,
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
        exclusions: [("EX-0001".to_string(), Exclusion::default())].into(),
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
    let lines: std::collections::BTreeSet<Line> = [Line::Health, Line::LumpSum, Line::IncomeProtection].into();
    let types = [ClaimType::Death, ClaimType::TerminalIllness, ClaimType::Trauma, ClaimType::IncomeProtection];
    let senior = Profile {
        lines,
        max_admission_minor: types.into_iter().map(|t| (t, i64::MAX)).collect(),
        max_payment_minor: i64::MAX,
        max_reserve_minor: i64::MAX,
        may_decline: true,
        max_ex_gratia_minor: i64::MAX,
        max_write_off_minor: i64::MAX,
        may_override_exclusion: true,
        may_reverse_admission: true,
        may_change_recovery_status: true,
        senior: true,
    };
    let person = |roles: &[Role]| Staff {
        profile: "senior".to_string(),
        roles: roles.iter().copied().collect(),
        queues: [Queue::LumpSum, Queue::IncomeProtectionNew, Queue::IncomeProtectionOngoing, Queue::ComplexLarge, Queue::Recoveries].into(),
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

fn cluster() -> Cluster {
    let mut c = Cluster::in_memory(2, 42);
    c.keep_journal = true;
    c.open(start()).unwrap();
    for cmd in shipped_activations().unwrap() {
        c.activate_everywhere(cmd).unwrap();
    }
    c.activate_everywhere(wrap(claims::Command::ActivateRules { rules: Box::new(rules()) })).unwrap();
    c.activate_everywhere(wrap(claims::Command::ActivateAuthority { book: Box::new(authority()) })).unwrap();
    c
}

fn code(r: Result<(), Reject>) -> String {
    r.expect_err("the command should have been refused").code
}

fn clean(c: &Cluster) {
    let breaks = c.reconcile();
    assert!(breaks.is_empty(), "{breaks:?}");
}

fn days(c: &mut Cluster, n: u32) {
    for _ in 0..n {
        let close = c.end_of_day().unwrap();
        assert!(close.breaks.is_empty(), "reconciliation breaks on {}: {:?}", close.closed, close.breaks);
    }
}

/// The `nth` policy number whose group lives on partition 0.
fn on_zero(c: &Cluster, nth: usize) -> u64 {
    (1..).filter(|n| c.partition_of(&GroupId(format!("grp-{n:08}"))) == 0).nth(nth).unwrap()
}

/// A term life policy on partition 0 with 500,000 of life cover and 100,000 of trauma cover,
/// with enough paid to stay in good standing for a year.
fn policy(c: &mut Cluster, nth: usize, paid: bool) -> PolicyId {
    let n = on_zero(c, nth);
    let issue = term_life(n, start(), d(1990, 3, 10), Sex::Female, false, 500_000_00, Some(100_000_00), Frequency::Monthly, None);
    let id = issue.policy.clone();
    c.submit(0, "new-business", issue_command(issue)).unwrap();
    if paid {
        let receipt = Command::ApplyReceipt {
            policy: id.clone(),
            receipt: format!("r-{nth}"),
            amount: Money::new(2_000_00, NZD),
        };
        c.submit(0, "cash-matching", receipt).unwrap();
    }
    id
}

fn run(c: &mut Cluster, command: claims::Command) -> Result<(), Reject> {
    c.submit(0, "claims", wrap(command))
}

fn ok(c: &mut Cluster, command: claims::Command) {
    let name = command.name();
    if let Err(e) = run(c, command) {
        panic!("{name} was refused: {e}");
    }
    clean(c);
}

fn notification(claim: &str, policy: &PolicyId, cover: &str, claim_type: ClaimType, event_date: NaiveDate) -> claims::Command {
    claims::Command::Notify(Box::new(Notification {
        claim: claim.into(),
        policy: policy.0.clone(),
        cover: cover.into(),
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
    }))
}

/// Notify, acknowledge, give to sam and assess.
fn open_claim(c: &mut Cluster, claim: &str, policy: &PolicyId, cover: &str, claim_type: ClaimType, event_date: NaiveDate) {
    ok(c, notification(claim, policy, cover, claim_type, event_date));
    ok(
        c,
        claims::Command::Acknowledge {
            claim: claim.into(),
            delivery_ref: "delivered".into(),
        },
    );
    ok(
        c,
        claims::Command::Assign {
            claim: claim.into(),
            to: "sam".into(),
            actor: actor("sam"),
        },
    );
    ok(
        c,
        claims::Command::RecordCause {
            claim: claim.into(),
            cause_code: "CC-0001".into(),
            actor: actor("sam"),
        },
    );
    ok(
        c,
        claims::Command::CoverCheck {
            claim: claim.into(),
            relied_on: vec!["in_force".into()],
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

fn admit(amount: Option<i64>, periodic: Option<Periodic>, payer_deceased: bool) -> Request {
    Request::Admit {
        amount_minor: amount,
        periodic,
        exclusion_override: None,
        exclusion_reviewed: false,
        payer_deceased,
    }
}

fn verify_payee(c: &mut Cluster, claim: &str) {
    ok(
        c,
        claims::Command::VerifyPayee {
            claim: claim.into(),
            party: "party-claimant".into(),
            kind: PayeeKind::Policyholder,
            account_ref: "acct-1".into(),
            name_check_ref: "name-1".into(),
            screening_ref: "screen-1".into(),
            authority_doc: None,
            actor: actor("sam"),
        },
    );
}

fn pay(claim: &str, payment: &str, amount: i64, reissue_of: Option<&str>) -> claims::Command {
    claims::Command::Pay {
        claim: claim.into(),
        payment_ref: payment.into(),
        amount_minor: amount,
        reissue_of: reissue_of.map(str::to_string),
        actor: actor("sam"),
    }
}

fn settle(policy: &PolicyId, payment: &str, minor: i64) -> Command {
    Command::SettlePayable {
        policy: policy.clone(),
        payment: payment.into(),
        payable: PayableKind::Benefit,
        refund: None,
        amount: Money::new(minor, NZD),
        bank_account: CLAIMS_BANK.into(),
    }
}

/// (claim reserve, benefit payable, recovery receivable) in the ledger for a policy.
fn money(c: &Cluster, id: &PolicyId) -> (i128, i128, i128) {
    let l = &c.state(0).ledger;
    (
        l.natural_balance(&accounts::claim_reserve(id, NZD).id),
        l.natural_balance(&accounts::benefit_payable(id, NZD).id),
        l.natural_balance(&accounts::claim_recovery_receivable(id, NZD).id),
    )
}

fn claim<'a>(c: &'a Cluster, id: &str) -> &'a claims::Claim {
    &c.state(0).claims.claims[id]
}

fn module_events(c: &Cluster, kind: &str) -> Vec<std::collections::BTreeMap<String, String>> {
    let events = c.nodes[&0].outbox.iter().filter_map(|e| match &e.event {
        Event::Module { module, kind: k, detail, .. } if module == "claims" && k == kind => Some(detail.clone()),
        _ => None,
    });
    events.collect()
}

/// Purpose: a lump sum claim from notification to settlement on the real ledger: the reserve at
/// notification, the admission moving it to benefit payable, the payment released by claims and
/// settled by the payments module, a bank return and a reissue, with the claim records, the
/// core's claim ledger and the journal agreeing after every command.
/// Why it matters: this is the money path of every claim. The claims module never posts, so the
/// proof that a decision and its postings commit together, and that the bank cannot be paid
/// something claims did not release, has to be made against the real state machine.
#[test]
fn a_lump_sum_claim_runs_from_notification_to_the_bank() {
    let mut c = cluster();
    let id = policy(&mut c, 0, true);
    days(&mut c, 20);
    let today = c.business_date().unwrap();

    // Only the claims module may send claims work.
    let n = notification("clm-1", &id, "trauma", ClaimType::Trauma, today);
    assert_eq!(code(c.submit(0, "payments", wrap(n))), "caller_not_allowed");
    let hash = c.state(0).state_hash();
    open_claim(&mut c, "clm-1", &id, "trauma", ClaimType::Trauma, today);
    assert_ne!(c.state(0).state_hash(), hash, "claims state is part of the partition's hash");
    assert_eq!(money(&c, &id), (100_000_00, 0, 0));
    let facts = claim(&c, "clm-1").cover_check.clone().unwrap().facts;
    assert_eq!((facts.in_force, facts.sum_insured_minor, facts.standing.as_deref()), (true, 100_000_00, Some("paid")));
    assert_eq!((facts.cover_type.as_str(), facts.life_on_policy, facts.ends_on_death), ("trauma", true, true));
    assert_eq!(facts.policy_status, "in_force");
    assert_eq!(facts.life_party.as_deref(), Some(facts.policyholder.as_str()));

    // A policy with an open claim stays on its partition.
    let transfer = Command::BeginTransfer {
        policy: id.clone(),
        transfer_id: "t-1".into(),
        target_group: GroupId("grp-elsewhere".into()),
        target_partition: 1,
    };
    assert_eq!(code(c.submit(0, "transfer", transfer)), "open_claim");

    ok(&mut c, decide("clm-1", admit(Some(80_000_00), None, false)));
    assert_eq!(money(&c, &id), (0, 80_000_00, 0));
    // The estimate was 100,000 and the admission 80,000: claims expense shows 80,000.
    let expense = accounts::claims_incurred("term-life", "trauma", NZD);
    assert_eq!(c.state(0).ledger.natural_balance(&expense.id), 80_000_00);

    // The bank cannot be paid what claims has not released.
    assert_eq!(code(c.submit(0, "payments", settle(&id, "pay-1", 80_000_00))), "payment_not_instructed");
    verify_payee(&mut c, "clm-1");
    ok(&mut c, pay("clm-1", "pay-1", 80_000_00, None));
    assert_eq!(claim(&c, "clm-1").status, Status::Paid);
    assert_eq!(money(&c, &id), (0, 80_000_00, 0), "released, and owed until the bank settles");
    assert_eq!(code(c.submit(0, "payments", settle(&id, "pay-1", 79_999_00))), "payment_not_instructed");
    c.submit(0, "payments", settle(&id, "pay-1", 80_000_00)).unwrap();
    clean(&c);
    assert_eq!(money(&c, &id), (0, 0, 0));
    assert_eq!(c.state(0).ledger.balance(&accounts::bank(CLAIMS_BANK, NZD).id), -80_000_00);
    // Settled: claims cannot call it back by itself.
    let returned = claims::Command::PaymentReturned {
        claim: "clm-1".into(),
        payment_ref: "pay-1".into(),
    };
    assert_eq!(code(run(&mut c, returned)), "payment_settled");

    // The bank sends it back: owed again, the claim is open again, and a task says so.
    let back = Command::ReturnPayment {
        policy: id.clone(),
        payment: "pay-1".into(),
    };
    c.submit(0, "payments", back).unwrap();
    clean(&c);
    assert_eq!(money(&c, &id), (0, 80_000_00, 0));
    let record = claim(&c, "clm-1");
    assert_eq!((record.status, record.payable_minor), (Status::Admitted, 80_000_00));
    assert!(record.tasks.iter().any(|t| t.id == "payment-failure/pay-1"));
    assert_eq!(module_events(&c, "payment_returned").len(), 1);

    ok(&mut c, pay("clm-1", "pay-2", 80_000_00, Some("pay-1")));
    c.submit(0, "payments", settle(&id, "pay-2", 80_000_00)).unwrap();
    clean(&c);
    assert_eq!(money(&c, &id), (0, 0, 0));
    assert_eq!(claim(&c, "clm-1").status, Status::Paid);

    // Every entry names its claim and none carries the cause.
    let entries: Vec<_> = c.nodes[&0].journal.iter().filter(|e| e.body.kind.starts_with("claim_")).collect();
    assert_eq!(entries.len(), 2, "the reserve and the admission; a release with nothing left posts nothing");
    assert!(entries.iter().all(|e| e.body.detail["claim"] == "clm-1" && e.body.detail.values().all(|v| v != "CC-0001")));
    assert_eq!(module_events(&c, "claim_reserved")[0]["gross_reserve_minor"], "10000000");
    assert_eq!(module_events(&c, "claim_paid").len(), 2);
    days(&mut c, 2);
}

/// Purpose: admitting a death claim on a single-life policy ends the policy as at the date of
/// death in the same command as the admission, and a claim on a policy in grace is paid less the
/// premium owing, which settles the arrears.
/// Why it matters: these are the two places an admission changes the policy itself. If the
/// admission committed and the termination or the premium deduction did not, the insurer would
/// keep billing a dead customer or pay a claim and still chase the premium.
#[test]
fn an_admission_can_end_the_policy_or_take_the_premium_owing() {
    let mut c = cluster();
    let dead = policy(&mut c, 0, true);
    let unpaid = policy(&mut c, 1, false);
    days(&mut c, 20);
    let today = c.business_date().unwrap();
    let died = today.pred_opt().unwrap();

    open_claim(&mut c, "clm-death", &dead, "life", ClaimType::Death, died);
    assert_eq!(module_events(&c, "death_notified")[0]["date_of_death"], died.to_string());
    ok(&mut c, decide("clm-death", admit(None, None, true)));
    let p = &c.state(0).policies[&dead];
    assert_eq!(p.status, PolicyStatus::Terminated);
    assert_eq!(money(&c, &dead), (0, 500_000_00, 0));
    assert!(c.nodes[&0].journal.iter().any(|e| e.body.kind == "terminated_on_claim"));
    // A second death claim cannot end a policy that has already ended.
    ok(&mut c, notification("clm-death-2", &dead, "life", ClaimType::Death, died));
    assert!(!claim(&c, "clm-death-2").cover_check.as_ref().is_some_and(|k| k.facts.policy_status == "in_force"));

    // In grace: 61 days of premium unpaid by now would be past grace, 20 is inside it.
    open_claim(&mut c, "clm-grace", &unpaid, "trauma", ClaimType::Trauma, today);
    let facts = claim(&c, "clm-grace").cover_check.clone().unwrap().facts;
    assert_eq!(facts.standing.as_deref(), Some("in_grace"));
    let owing = facts.outstanding_premium_minor;
    assert!(owing > 0 && facts.in_force);
    ok(&mut c, decide("clm-grace", admit(Some(50_000_00), None, false)));
    assert_eq!(money(&c, &unpaid), (0, i128::from(50_000_00 - owing), 0));
    assert_eq!(claim(&c, "clm-grace").payable_minor, 50_000_00 - owing);
    assert_eq!(c.state(0).policies[&unpaid].outstanding_minor(), 0, "the deducted premium paid the arrears");
    days(&mut c, 3);
}

fn benefit(monthly: i64, first: NaiveDate) -> Periodic {
    Periodic {
        monthly_minor: monthly,
        first_period: first,
        end: first.checked_add_months(chrono::Months::new(24)).unwrap(),
        expected_end: first.checked_add_months(chrono::Months::new(12)).unwrap(),
    }
}

fn pay_run(c: &mut Cluster, run_id: &str, benefit_date: NaiveDate) {
    ok(
        c,
        claims::Command::PrepareRun {
            run: run_id.into(),
            kind: RunKind::Benefit,
            benefit_date,
            actor: actor("sue"),
        },
    );
    ok(
        c,
        claims::Command::ApproveRun {
            run: run_id.into(),
            actor: actor("sam"),
        },
    );
}

/// Purpose: a benefit in payment on the real ledger, then the design addendum's reversal of
/// admission: two periods paid and settled, the admission reversed, the receivable raised for
/// exactly what was paid, part repaid, the rest written off, with zero reconciliation breaks
/// and day ends in between.
/// Why it matters: the addendum's entry 5 moves money in four different ways (benefit, close
/// with overpayment, recovery, write-off). Each must leave the ledger, the core's claim record
/// and the claims module agreeing, or the daily control would cry wolf on every reversal.
#[test]
fn a_benefit_is_paid_reversed_and_the_overpayment_worked_out() {
    let mut c = cluster();
    let id = policy(&mut c, 0, true);
    days(&mut c, 5);
    let first = c.business_date().unwrap();
    open_claim(&mut c, "clm-ip", &id, "trauma", ClaimType::IncomeProtection, first);
    ok(&mut c, decide("clm-ip", admit(None, Some(benefit(3_000_00, first)), false)));
    verify_payee(&mut c, "clm-ip");
    let reserve = claim(&c, "clm-ip").reserve_minor;
    assert_eq!(money(&c, &id), (i128::from(reserve), 0, 0));

    pay_run(&mut c, "run-1", first);
    let p1 = format!("run-1/clm-ip/{first}");
    assert_eq!(money(&c, &id), (i128::from(reserve - 3_000_00), 3_000_00, 0));
    c.submit(0, "payments", settle(&id, &p1, 3_000_00)).unwrap();
    clean(&c);
    days(&mut c, 31);
    let second = first.checked_add_months(chrono::Months::new(1)).unwrap();
    pay_run(&mut c, "run-2", second);
    c.submit(0, "payments", settle(&id, &format!("run-2/clm-ip/{second}"), 3_000_00)).unwrap();
    clean(&c);
    ok(
        &mut c,
        claims::Command::ConfirmRun {
            run: "run-2".into(),
            settled_minor: 3_000_00,
        },
    );

    // The reversal: senior queue, conference, conversation, two seniors.
    ok(
        &mut c,
        claims::Command::Escalate {
            claim: "clm-ip".into(),
            actor: actor("sam"),
        },
    );
    let conference = claim(&c, "clm-ip").task(claims::TaskKind::CaseConference).next().unwrap().id.clone();
    for (task, kind) in [(conference, None), ("talk-1".to_string(), Some(claims::TaskKind::ClaimantConversation))] {
        if let Some(kind) = kind {
            ok(
                &mut c,
                claims::Command::OpenTask {
                    claim: "clm-ip".into(),
                    task: task.clone(),
                    kind,
                    due: None,
                    actor: actor("sam"),
                },
            );
        }
        ok(
            &mut c,
            claims::Command::CompleteTask {
                claim: "clm-ip".into(),
                task,
                outcome: "held".into(),
                attendees: vec!["sam".into(), "sue".into()],
                vault_ref: None,
                spend_minor: 0,
                actor: actor("sam"),
            },
        );
    }
    ok(&mut c, decide("clm-ip", Request::ReverseAdmission { false_declaration: false }));
    ok(
        &mut c,
        claims::Command::Approve {
            claim: "clm-ip".into(),
            actor: actor("sue"),
        },
    );
    assert_eq!(claim(&c, "clm-ip").status, Status::Reversed);
    assert_eq!(money(&c, &id), (0, 0, 6_000_00));
    let reversed = module_events(&c, "admission_reversed");
    assert_eq!(reversed[0]["cause"], "rules_unmapped");
    let closing = c.nodes[&0].journal.iter().rev().find(|e| e.body.kind == "claim_closed").unwrap();
    assert_eq!(closing.body.detail["outcome"], "admission_reversed");
    days(&mut c, 2);

    let op = claim(&c, "clm-ip").overpayments[0].id.clone();
    let repaid = claims::Command::RecordRepayment {
        claim: "clm-ip".into(),
        overpayment: op.clone(),
        amount_minor: 1_000_00,
    };
    c.submit(0, "payments", wrap(repaid)).unwrap();
    clean(&c);
    assert_eq!(money(&c, &id), (0, 0, 5_000_00));
    ok(&mut c, decide("clm-ip", Request::WriteOff { overpayment: op }));
    assert_eq!(money(&c, &id), (0, 0, 0));
    let written_off = accounts::claim_recovery_write_off("term-life", NZD);
    assert_eq!(c.state(0).ledger.natural_balance(&written_off.id), 5_000_00);
    assert_eq!(module_events(&c, "overpayment_written_off")[0]["insurer_error"], "true");
    // With nothing open or owed, the policy may move again.
    assert!(c.state(0).policies[&id].claims.values().all(|l| !l.open && l.payable_minor == 0 && l.receivable_minor == 0));
    days(&mut c, 2);
}

/// Purpose: declared income raises an overpayment through the ledger and an offset recovers it
/// from the next benefit, on the same policy and, for the same person, from a benefit on
/// another policy on the partition.
/// Why it matters: an offset credits the receivable on the policy that holds the overpayment
/// while paying the claimant from another. If the core credited the wrong policy the control
/// accounts of both would be out by the offset for ever.
#[test]
fn an_offset_recovers_an_overpayment_across_policies() {
    let mut c = cluster();
    let n = on_zero(&c, 0);
    let a = policy(&mut c, 0, true);
    // A second policy of the same person.
    let mut issue = term_life(on_zero(&c, 1), start(), d(1990, 3, 10), Sex::Female, false, 500_000_00, Some(100_000_00), Frequency::Monthly, None);
    issue.lives[0].kind = ic_core::LifeKind::Named {
        party: ic_core::PartyId(format!("party-{n:08}")),
    };
    let b = issue.policy.clone();
    c.submit(0, "new-business", issue_command(issue)).unwrap();
    let receipt = Command::ApplyReceipt {
        policy: b.clone(),
        receipt: "r-b".into(),
        amount: Money::new(2_000_00, NZD),
    };
    c.submit(0, "cash-matching", receipt).unwrap();
    days(&mut c, 5);
    let first = c.business_date().unwrap();

    open_claim(&mut c, "clm-a", &a, "trauma", ClaimType::IncomeProtection, first);
    ok(&mut c, decide("clm-a", admit(None, Some(benefit(2_000_00, first)), false)));
    verify_payee(&mut c, "clm-a");
    pay_run(&mut c, "run-1", first);
    ok(
        &mut c,
        claims::Command::DeclareIncome {
            claim: "clm-a".into(),
            from: first,
            other_income_minor: 500_00,
            actor: actor("sam"),
        },
    );
    assert_eq!(money(&c, &a), (i128::from(claim(&c, "clm-a").reserve_minor), 2_000_00, 500_00));
    let op = claim(&c, "clm-a").overpayments[0].id.clone();
    let offset = Request::RecoveryStatus {
        overpayment: op.clone(),
        status: RecoveryStatus::Offset,
        offset_pct: 10,
        instalment_minor: 0,
    };
    ok(&mut c, decide("clm-a", offset));

    // The same person's second claim, on the other policy, starts paying.
    open_claim(&mut c, "clm-b", &b, "trauma", ClaimType::IncomeProtection, first);
    ok(&mut c, decide("clm-b", admit(None, Some(benefit(1_000_00, first)), false)));
    verify_payee(&mut c, "clm-b");
    // End the first benefit so only the second is paid by the next run.
    let end = Request::EndBenefit {
        end: first.checked_add_months(chrono::Months::new(1)).unwrap(),
        reason: EndReason::Recovered,
    };
    ok(&mut c, decide("clm-a", end));
    assert_eq!(claim(&c, "clm-a").status, Status::Ended);
    pay_run(&mut c, "run-2", first);
    // 10% of 1,000 came off the benefit on policy b and off the receivable on policy a.
    assert_eq!(money(&c, &b).1, 900_00);
    assert_eq!(money(&c, &a).2, 400_00);
    assert_eq!(claim(&c, "clm-a").overpayments[0].recovered_minor, 100_00);
    let paid = c.nodes[&0].journal.iter().rev().find(|e| e.body.kind == "benefit_paid").unwrap();
    assert_eq!(paid.body.detail["claim"], "clm-b");
    days(&mut c, 2);
}

/// Purpose: the day end lets claims work through what has fallen due (a missed service timer
/// escalates with nobody sending a command), and a claim is closed, reopened, paid ex gratia
/// and has an overpayment raised by hand, all through the real ledger with no breaks.
/// Why it matters: service timers that only move when someone looks are not a control; and the
/// less common postings (reopen, ex gratia, a manual overpayment) are where an unbalanced or
/// misdirected entry would hide longest.
#[test]
fn the_day_end_drives_timers_and_the_rarer_postings_balance() {
    let mut c = cluster();
    let id = policy(&mut c, 0, true);
    days(&mut c, 3);
    let today = c.business_date().unwrap();
    ok(&mut c, notification("clm-quiet", &id, "trauma", ClaimType::Trauma, today));
    days(&mut c, 30);
    let escalated = module_events(&c, "timer_escalated");
    assert!(escalated.iter().any(|e| e["timer"] == "Acknowledgement" && e["level"] == "3"), "{escalated:?}");
    assert_eq!(c.state(0).claims.due_backlog(c.business_date().unwrap()), 0);

    open_claim(&mut c, "clm-1", &id, "life", ClaimType::TerminalIllness, today);
    ok(&mut c, decide("clm-1", Request::Withdraw));
    assert_eq!(money(&c, &id), (100_000_00, 0, 0), "only the quiet claim's reserve is left");
    ok(
        &mut c,
        claims::Command::Reopen {
            claim: "clm-1".into(),
            basis: claims::command::ReopenBasis::NewEvidence,
            reserve_minor: 200_000_00,
            actor: actor("sue"),
        },
    );
    assert_eq!(money(&c, &id), (300_000_00, 0, 0));
    ok(
        &mut c,
        claims::Command::CoverCheck {
            claim: "clm-1".into(),
            relied_on: vec![],
            actor: actor("sue"),
        },
    );
    ok(
        &mut c,
        claims::Command::VerifyPayee {
            claim: "clm-1".into(),
            party: "party-claimant".into(),
            kind: PayeeKind::Policyholder,
            account_ref: "acct-1".into(),
            name_check_ref: "name-1".into(),
            screening_ref: "screen-1".into(),
            authority_doc: None,
            actor: actor("sue"),
        },
    );
    let gift = claims::Command::Decide {
        claim: "clm-1".into(),
        request: Request::ExGratia {
            amount_minor: 500_00,
            payment_ref: "xg-1".into(),
        },
        reasons: vec!["R-1".into()],
        actor: actor("sue"),
    };
    ok(&mut c, gift);
    assert_eq!(money(&c, &id), (300_000_00, 500_00, 0));
    assert_eq!(c.state(0).ledger.natural_balance(&accounts::ex_gratia("term-life", NZD).id), 500_00);
    c.submit(0, "payments", settle(&id, "xg-1", 500_00)).unwrap();
    clean(&c);
    ok(
        &mut c,
        claims::Command::RaiseOverpayment {
            claim: "clm-1".into(),
            overpayment: "op-1".into(),
            amount_minor: 200_00,
            cause: claims::OverpaymentCause::InsurerError,
            good_faith: false,
            actor: actor("sue"),
        },
    );
    assert_eq!(money(&c, &id), (300_000_00, 0, 200_00));
    // A payment released and then failed before the bank settled it is simply owed again.
    let partial = claims::Command::Decide {
        claim: "clm-1".into(),
        request: admit(Some(1_000_00), None, false),
        reasons: vec!["R-1".into()],
        actor: actor("sue"),
    };
    ok(&mut c, partial);
    let release = claims::Command::Pay {
        claim: "clm-1".into(),
        payment_ref: "pay-1".into(),
        amount_minor: 1_000_00,
        reissue_of: None,
        actor: actor("sue"),
    };
    ok(&mut c, release);
    let failed = claims::Command::PaymentReturned {
        claim: "clm-1".into(),
        payment_ref: "pay-1".into(),
    };
    ok(&mut c, failed.clone());
    assert_eq!(code(run(&mut c, failed)), "unknown_payment");
    assert_eq!(code(c.submit(0, "payments", settle(&id, "pay-1", 1_000_00))), "payment_not_instructed");
    assert_eq!(money(&c, &id), (100_000_00, 1_000_00, 200_00));
    days(&mut c, 2);
}
