//! The held balance workbench, used the way the addendum describes: a
//! provider loses its licence, money accrues to held, and every way out of
//! held is a typed, approved, posted action.

mod common;

use common::*;
use ic_core::counterparty::{CounterpartyAccount, InternalLedger, PartyLedger};
use ic_core::{PolicyId, PolicyStatus};
use ic_distribution::authority::{LimitKind, Role};
use ic_distribution::config::Change;
use ic_distribution::rules::{HeldReason, Share};
use ic_distribution::state::{CaseKind, Settings};
use ic_distribution::structure::{CeasedReason, LicenceState, PartyKind};
use ic_distribution::workbench::{Action, ActionState, Debt, PositionNote, RecoveryStatus, SplitShare};
use ic_distribution::{Actor, Command, Event};

fn start() -> chrono::NaiveDate {
    d(START.0, START.1, START.2)
}

/// FAP One's licence was suspended from day one, so everything it earned is
/// held. A week has passed, so its accounts are past their cooling-off.
/// Returns the world, the policy and the amount held.
fn suspended_with(settings: Settings) -> (World, PolicyId, i64) {
    let (mut w, policy, _) = advised_in(worked_example_in(World::with(1, settings)));
    w.days(7);
    w.licence("fap-one", LicenceState::Suspended, start()).unwrap();
    let held = w.held("fap-one");
    assert!(held > 300_00);
    (w, policy, held)
}

fn suspended() -> (World, PolicyId, i64) {
    suspended_with(Settings::default())
}

fn release(amount_minor: i64) -> Action {
    Action::Release {
        amount_minor,
        account_ref: "acct-fap-one".into(),
    }
}

/// Purpose: a licence suspension dated in the past moves what the provider was paid since
/// that date into held, stops new business under its engagements, opens a conduct case with the
/// affected engagements, payees and policies attached, and sends later earnings to held.
/// Why it matters: this is addendum entry 4 end to end. Each piece missing is money paid to an
/// unlicensed provider, advice given without a licence, or customers nobody is told about.
#[test]
fn a_backdated_suspension_moves_paid_commission_to_held_and_opens_a_conduct_case() {
    let (mut w, policy, renewal) = advised_in(worked_example());
    let paid = w.payable("fap-one");
    let a_co = w.payable("a-co");
    w.days(7);
    let events = w.licence("fap-one", LicenceState::Suspended, start()).unwrap();
    assert_eq!(w.payable("fap-one"), 0);
    assert_eq!(w.held("fap-one"), paid);
    assert_eq!(w.payable("a-co"), a_co, "the adviser's company has its own standing");
    let changed = events.iter().find_map(|e| match e {
        Event::AuthorisationChanged {
            engagements,
            suspended_payees,
            policies_to_service_directly,
            ..
        } => Some((engagements.clone(), suspended_payees.clone(), *policies_to_service_directly)),
        _ => None,
    });
    let (engagements, payees, direct) = changed.unwrap();
    assert_eq!(engagements.len(), 1);
    assert!(payees.contains(&party("fap-one")) && payees.contains(&party("adviser-a")));
    assert_eq!(direct, 1);
    let case = w.dist.cases.values().next().unwrap();
    assert_eq!((case.kind, case.owner), (CaseKind::Conduct, Role::ConductOfficer));
    assert!(case.policies.contains(&policy) && case.payees.contains(&party("fap-one")));
    assert_eq!(case.workstreams.len(), 3);

    // No new business under the engagement, and no customer can appoint the adviser.
    let (second, _) = w.issue(2, 500_000_00, None);
    assert_eq!(code(w.write(&second, "life", 1, "eng-a-one", "a-at-fap-one", 1_200_00)), "not_authorised_to_advise");
    let appoint = Command::AppointAdviser {
        policy: second,
        engagement: Some("eng-a-one".into()),
        book: None,
    };
    assert_eq!(code(w.send(module("customer-app"), appoint)), "adviser_unavailable");

    // Later earnings accrue to held; the statement says why nothing is paid.
    w.days(31);
    w.pay(&policy, "r2");
    assert_eq!(w.payable("fap-one"), 0);
    assert_eq!(w.held("fap-one"), paid + ic_distribution::rules::portion(renewal, pct(25.0)));
    assert!(w.dist.held.iter().all(|h| h.reason == Some(HeldReason::SuspendedLicence)));
    w.cut("fap-one").unwrap();
    let statement = w.statement("fap-one", 1);
    assert_eq!(statement.payment_held.as_deref(), Some("licence not active"));
    assert_eq!(statement.held_minor, w.held("fap-one"));
    assert_eq!(statement.held_reasons, vec!["SuspendedLicence".to_string()]);
    assert!(statement.payments.is_empty());
    w.assert_reconciled();
}

/// Purpose: licence records are added to, never rewritten: the same notice twice is a no-op, a
/// different status on the same date is refused, a future date is refused, and a return to
/// active sends new earnings to payable while what was held stays held.
/// Why it matters: the regulator's register is replayed and corrected. Idempotence stops a
/// second conduct case; keeping held money held after reactivation leaves its release a decision.
#[test]
fn licence_history_is_append_only_and_reactivation_does_not_release_held_money() {
    let (mut w, policy, held) = suspended();
    assert!(w.licence("fap-one", LicenceState::Suspended, start()).unwrap().is_empty());
    assert_eq!(w.dist.cases.len(), 1);
    assert_eq!(code(w.licence("fap-one", LicenceState::Revoked, start())), "structure");
    let tomorrow = w.today().succ_opt().unwrap();
    assert_eq!(code(w.licence("fap-one", LicenceState::Active, tomorrow)), "future_licence_event");
    assert_eq!(code(w.licence("ghost", LicenceState::Lapsed, start())), "structure");
    let blank = Command::RecordLicence {
        party: party("fap-two"),
        state: LicenceState::Lapsed,
        effective: start(),
        source_ref: " ".into(),
    };
    assert_eq!(code(w.send(compliance(), blank.clone())), "structure");
    assert_eq!(code(w.send(ops(), blank)), "not_authorised");

    w.days(1);
    let today = w.today();
    let events = w.licence("fap-one", LicenceState::Active, today).unwrap();
    assert!(!events.iter().any(|e| matches!(e, Event::CaseOpened { .. })));
    w.days(31);
    w.pay(&policy, "r2");
    assert!(w.payable("fap-one") > 0);
    assert_eq!(w.held("fap-one"), held);

    let case = w.dist.cases.keys().next().unwrap().clone();
    assert_eq!(code(w.send(ops(), Command::CloseCase { id: case.clone(), outcome: "done".into() })), "not_authorised");
    assert_eq!(code(w.send(conduct(), Command::CloseCase { id: case.clone(), outcome: " ".into() })), "invalid_case");
    w.send(conduct(), Command::CloseCase { id: case.clone(), outcome: "customers written to".into() }).unwrap();
    assert_eq!(code(w.send(conduct(), Command::CloseCase { id: case, outcome: "again".into() })), "invalid_case");
    assert_eq!(code(w.send(conduct(), Command::CloseCase { id: "ghost".into(), outcome: "x".into() })), "unknown_case");
    w.assert_reconciled();
}

/// Purpose: a release pays held money to a verified account through the approver, the ledger
/// and the payments module; the action completes only when the payment settles, and the held,
/// clearing and settlement accounts all agree.
/// Why it matters: this is the only door out of held. Every step being a posting through the
/// core is what makes the finance ledger and the audit trail follow with nothing done by hand.
#[test]
fn a_release_is_prepared_approved_posted_paid_and_only_then_complete() {
    let (mut w, _, held) = suspended();
    let cash = w.internal(InternalLedger::Settlement);
    let proposed = w.act("rel-1", "fap-one", release(held)).unwrap();
    assert!(matches!(&proposed[0], Event::ActionProposed { kind, amount_minor, .. } if kind == "release" && *amount_minor == held));
    assert_eq!(w.dist.actions["rel-1"].position_shown.held_minor, held, "the approver sees the position");
    assert_eq!(w.dist.position(&party("fap-one"), w.today()).actions_in_progress, vec!["rel-1".to_string()]);
    assert_eq!(w.held("fap-one"), held, "nothing moves on a proposal");

    let events = w.sign("rel-1", finance_approver(), Role::CounterpartyFinanceApprover).unwrap();
    assert!(events.iter().any(|e| matches!(e, Event::PaymentRequested { reference, account_ref, amount_minor, .. }
        if reference == "pay:rel-1" && account_ref == "acct-fap-one" && *amount_minor == held)));
    assert_eq!(w.held("fap-one"), 0);
    assert_eq!(w.ledger("fap-one", PartyLedger::Clearing), held);
    assert_eq!(w.dist.actions["rel-1"].state, ActionState::Posting);

    w.settle("pay:rel-1");
    assert_eq!(w.ledger("fap-one", PartyLedger::Clearing), 0);
    assert_eq!(w.internal(InternalLedger::Settlement), cash - held);
    assert_eq!(w.dist.actions["rel-1"].state, ActionState::Completed);
    assert!(w.events.iter().any(|e| matches!(e, Event::ActionCompleted { id } if id == "rel-1")));
    assert!(w.events.iter().any(|e| matches!(e, Event::AmountReleased { .. })));
    let posting = &w.dist.requested["wb:rel-1:1"].posting;
    assert_eq!(posting.detail["reason_code"], "reason");
    assert_eq!(posting.detail["document"], "doc-rel-1");
    w.assert_reconciled();
}

/// Purpose: a released payment that the bank returns goes back to the held account it came
/// from, with a task for someone to find out why.
/// Why it matters: a returned payment that fell into suspense or back to payable would either
/// vanish from the workbench or be paid out on the next statement to a suspended provider.
#[test]
fn a_returned_release_goes_back_to_held_with_a_task() {
    let (mut w, _, held) = suspended();
    let cash = w.internal(InternalLedger::Settlement);
    w.act("rel-1", "fap-one", release(held)).unwrap();
    w.sign("rel-1", finance_approver(), Role::CounterpartyFinanceApprover).unwrap();
    let returned = Command::PaymentReturned {
        reference: "pay:rel-1".into(),
        reason: "account closed".into(),
    };
    assert_eq!(code(w.send(finance(), returned.clone())), "caller_not_allowed");
    w.send(module("payments"), returned.clone()).unwrap();
    w.sync();
    assert_eq!(w.held("fap-one"), held);
    assert_eq!(w.ledger("fap-one", PartyLedger::Clearing), 0);
    assert_eq!(w.internal(InternalLedger::Settlement), cash);
    assert!(w.dist.tasks.values().any(|t| t.kind == "payment_returned" && t.detail == "account closed"));
    assert!(w.events.iter().any(|e| matches!(e, Event::PaymentFailed { .. })));
    assert_eq!(code(w.send(module("payments"), returned)), "unknown_payment");
    w.assert_reconciled();
}

/// Purpose: the workbench refuses a release that is not safe: more than is held, nothing, an
/// account that is unknown or inside its cooling-off, a party that is not a payee, and money
/// already committed to another action.
/// Why it matters: each refusal is a control from the addendum. Redirected held balances are
/// the fraud target; paying the same held money twice is the operational one.
#[test]
fn an_unsafe_release_is_refused_when_proposed_and_again_at_the_last_signature() {
    let (mut w, _, held) = suspended();
    assert_eq!(code(w.act("r", "fap-one", release(held + 1))), "insufficient_held");
    assert_eq!(code(w.act("r", "fap-one", release(0))), "invalid_amount");
    let elsewhere = Action::Release {
        amount_minor: held,
        account_ref: "acct-of-a-fraudster".into(),
    };
    assert_eq!(code(w.act("r", "fap-one", elsewhere)), "account_not_verified");
    w.account("fap-one", "acct-new");
    let fresh = Action::Release {
        amount_minor: held,
        account_ref: "acct-new".into(),
    };
    assert_eq!(code(w.act("r", "fap-one", fresh)), "account_not_verified", "still inside its cooling-off");

    // Held money for a party that is no payee: there is no account to release to.
    w.add_party("estate-1", PartyKind::Counterparty);
    w.post_as_finance(0, "ext-1", vec![(CounterpartyAccount::Internal(InternalLedger::Settlement), 50_00), (acct("estate-1", PartyLedger::Held), -50_00)])
        .unwrap();
    assert_eq!(w.held("estate-1"), 50_00);
    let to_estate = Action::Release {
        amount_minor: 50_00,
        account_ref: "x".into(),
    };
    assert_eq!(code(w.act("r", "estate-1", to_estate)), "unknown_payee");

    // Two releases of the same money: the second fails its final check.
    w.act("rel-a", "fap-one", release(held)).unwrap();
    w.act("rel-b", "fap-one", release(held)).unwrap();
    w.sign("rel-a", finance_approver(), Role::CounterpartyFinanceApprover).unwrap();
    assert_eq!(code(w.sign("rel-b", finance_approver(), Role::CounterpartyFinanceApprover)), "insufficient_held");
    assert_eq!(w.dist.actions["rel-b"].state, ActionState::Proposed);
    w.settle("pay:rel-a");
    w.assert_reconciled();
}

/// Purpose: the preparer can never approve, each signature comes from a different person, a
/// preparer of any action cannot sign as approver, a role that is not waited for cannot sign,
/// and amounts above a person's authority are refused with the reason.
/// Why it matters: maker-checker and authority limits are the controls a regulator and an
/// auditor test first. A denial that says why turns a gap into a request, not a workaround.
#[test]
fn maker_checker_and_authority_limits_hold_on_every_signature() {
    let (mut w, _, held) = suspended_with(Settings {
        two_approver_threshold_minor: 100_00,
        ..Settings::default()
    });
    // Above the threshold: two approvers and legal.
    let both = || staff("staff-both", &[Role::CounterpartyFinance, Role::Legal]);
    w.send(
        both(),
        Command::ProposeAction {
            id: "big".into(),
            party: party("fap-one"),
            action: release(held),
            reason_code: "liquidator_request".into(),
            document_ref: "doc".into(),
        },
    )
    .unwrap();
    assert_eq!(
        w.dist.actions["big"].needed,
        vec![Role::CounterpartyFinanceApprover, Role::CounterpartyFinanceApprover, Role::Legal]
    );
    let denied = w.sign("big", both(), Role::Legal).unwrap_err();
    assert_eq!(denied.code, "not_authorised");
    assert!(denied.message.contains("you prepared this"), "{}", denied.message);
    assert_eq!(code(w.sign("big", finance(), Role::CounterpartyFinanceApprover)), "not_authorised");
    assert_eq!(code(w.sign("big", legal(), Role::CounterpartyFinanceApprover)), "not_authorised");
    assert_eq!(code(w.sign("big", conduct(), Role::ConductOfficer)), "approval_not_needed");
    assert_eq!(code(w.sign("big", admin(), Role::DistributionAdmin)), "approval_not_needed");
    w.sign("big", finance_approver(), Role::CounterpartyFinanceApprover).unwrap();
    assert_eq!(code(w.sign("big", finance_approver(), Role::CounterpartyFinanceApprover)), "not_authorised");
    w.sign("big", legal(), Role::Legal).unwrap();
    assert_eq!(w.held("fap-one"), held, "one signature is still missing");
    w.sign("big", finance_approver_2(), Role::CounterpartyFinanceApprover).unwrap();
    assert_eq!(w.held("fap-one"), 0);
    assert_eq!(code(w.sign("big", legal(), Role::Legal)), "action_settled");
    assert_eq!(w.dist.actions["big"].approvals.len(), 3);

    // A person whose profile stops below the amount.
    let mut junior = person("staff-junior", &[Role::CounterpartyFinance]);
    junior.templates[0].default_profile.limits.insert(LimitKind::Release, 10_00);
    let propose = |actor: Actor| Command::ProposeAction {
        id: "second".into(),
        party: party("fap-one"),
        action: release(50_00),
        reason_code: "r".into(),
        document_ref: "d".into(),
    }
    .pipe(|c| (actor, c));
    let (actor, command) = propose(Actor::Staff(Box::new(junior.clone())));
    let denied = w.send(actor, command).unwrap_err();
    assert!(denied.message.contains("above your limit of 1000"), "{}", denied.message);
    // No path to the counterparty, and no declared purpose.
    let mut stranger = person("staff-stranger", &[Role::CounterpartyFinance]);
    stranger.paths = ["fap-two".to_string()].into();
    let (actor, command) = propose(Actor::Staff(Box::new(stranger.clone())));
    assert!(w.send(actor, command).unwrap_err().message.contains("no path to fap-one"));
    stranger.paths = ["fap-one".to_string()].into();
    stranger.purpose = "  ".into();
    let (actor, command) = propose(Actor::Staff(Box::new(stranger)));
    assert!(w.send(actor, command).unwrap_err().message.contains("purpose"));
    let (_, command) = propose(admin());
    assert!(w.send(admin(), command.clone()).unwrap_err().message.contains("none of your roles"));
    assert_eq!(code(w.send(module("claims"), command)), "caller_not_allowed");
}

trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}
impl<T> Pipe for T {}

/// Purpose: a party in liquidation is paid only through an account held under the
/// liquidator's appointment, after its own cooling-off, and legal signs whatever the amount.
/// Why it matters: after a licence loss the held balance belongs to creditors. The appointment
/// document, the cooling-off and the legal signature are what stand between it and a director
/// asking for it to be sent somewhere else.
#[test]
fn a_release_to_a_liquidator_needs_the_appointment_account_and_legal() {
    let (mut w, _, held) = suspended();
    let today = w.today();
    w.send(
        admin(),
        Command::MarkCeased {
            party: party("fap-one"),
            on: today,
            reason: CeasedReason::Liquidation,
        },
    )
    .unwrap();
    w.send(
        admin(),
        Command::AddPayeeAccount {
            party: party("fap-one"),
            account_ref: "acct-liquidator".into(),
            name_check_ref: "name-ok".into(),
            step_up_ref: "step-up".into(),
            appointment_ref: Some("appointment-doc".into()),
        },
    )
    .unwrap();
    w.send(
        finance(),
        Command::AttachDocument {
            party: party("fap-one"),
            kind: "liquidator_appointment".into(),
            document_ref: "appointment-doc".into(),
        },
    )
    .unwrap();
    let to_liquidator = Action::Release {
        amount_minor: held,
        account_ref: "acct-liquidator".into(),
    };
    assert_eq!(code(w.act("liq", "fap-one", to_liquidator.clone())), "account_not_verified", "a liquidator's account cools off too");
    w.days(5);
    w.act("liq", "fap-one", to_liquidator).unwrap();
    assert_eq!(w.dist.actions["liq"].needed, vec![Role::CounterpartyFinanceApprover, Role::Legal]);
    w.sign("liq", finance_approver(), Role::CounterpartyFinanceApprover).unwrap();
    assert_eq!(w.held("fap-one"), held);
    w.sign("liq", legal(), Role::Legal).unwrap();
    assert_eq!(w.held("fap-one"), 0);
    w.settle("pay:liq");
    // The statement run never pays to an appointment account on its own.
    assert!(w.dist.payees[&party("fap-one")].default_account(w.today()).is_some_and(|a| a.account_ref == "acct-fap-one"));
    let position = w.dist.position(&party("fap-one"), w.today());
    assert_eq!(position.ceased, Some((today, CeasedReason::Liquidation)));
    assert_eq!(position.documents.len(), 1);
    assert_eq!(position.licence_history.len(), 1);
    assert_eq!(position.open_cases.len(), 1);
    w.assert_reconciled();
}

/// Purpose: held commission is netted against the clawback debt the same provider owes, the
/// rest of the debt is settled by a liquidator's dividend with the remainder written off as
/// one command, and the proof of debt shows every period with its ledger references.
/// Why it matters: a wound-up provider's debt stays on the books for the proof of debt, and
/// offset, dividend and write-off are the only ways it leaves. Bad debt must hit its own
/// account, not disappear into commission expense.
#[test]
fn held_is_offset_against_clawback_debt_and_a_dividend_writes_off_the_rest() {
    let (mut w, policy, held) = suspended();
    w.until_status(&policy, PolicyStatus::Lapsed);
    let debt = 1_200_00;
    assert_eq!(w.payable("fap-one"), -debt, "clawback is a debt on the commission account");
    let proof = w.dist.proof_of_debt(&party("fap-one"), w.today());
    assert_eq!((proof.owed_by_party_minor, proof.owed_to_party_minor, proof.net_minor), (debt, held, held - debt));
    assert!(proof.lines.iter().all(|(_, _, _, refs)| !refs.is_empty()));
    assert_eq!(proof.hash, w.dist.proof_of_debt(&party("fap-one"), w.today()).hash);
    assert_eq!(w.dist.proof_of_debt(&party("fap-one"), start().pred_opt().unwrap()).lines.len(), 0);

    let offset = |amount_minor| Action::Offset {
        amount_minor,
        against: Debt::Commission,
    };
    assert_eq!(code(w.act("off", "fap-one", offset(held + 1))), "insufficient_held");
    assert_eq!(code(w.act("off", "a-co", offset(1))), "insufficient_held");
    let nothing_owed = Action::Offset {
        amount_minor: held,
        against: Debt::Receivable,
    };
    assert_eq!(code(w.act("off", "fap-one", nothing_owed)), "insufficient_debt");
    w.act("off", "fap-one", offset(held)).unwrap();
    w.sign("off", finance_approver(), Role::CounterpartyFinanceApprover).unwrap();
    assert_eq!(w.held("fap-one"), 0);
    assert_eq!(w.payable("fap-one"), held - debt);
    assert_eq!(w.dist.actions["off"].state, ActionState::Completed);

    let left = debt - held;
    let dividend = |received_minor, receipt_ref: &str| Action::RecordDividend {
        debt: Debt::Commission,
        received_minor,
        receipt_ref: receipt_ref.into(),
    };
    assert_eq!(code(w.act("div", "fap-one", dividend(100_00, " "))), "invalid_dividend");
    assert_eq!(code(w.act("div", "fap-one", dividend(left + 1, "rcpt"))), "insufficient_debt");
    assert_eq!(code(w.act("div", "a-co", dividend(1, "rcpt"))), "insufficient_debt");
    let cash = w.internal(InternalLedger::Settlement);
    w.act("div", "fap-one", dividend(100_00, "liquidator-receipt-1")).unwrap();
    w.sign("div", finance_approver(), Role::CounterpartyFinanceApprover).unwrap();
    assert_eq!(w.payable("fap-one"), 0);
    assert_eq!(w.internal(InternalLedger::Settlement), cash + 100_00);
    assert_eq!(w.internal(InternalLedger::CommissionBadDebt), left - 100_00);
    assert_eq!(w.dist.summary(&party("fap-one")).net_minor, 0);
    w.assert_reconciled();
}

/// Purpose: writing off a debt needs a second approver above the threshold, writing off a held
/// balance is allowed only below the floor or beyond the age, and each posts to its own
/// write-off account.
/// Why it matters: write-offs are where money leaves quietly. The floor, the age and the
/// second signature are the limits the addendum sets on that.
#[test]
fn write_offs_follow_the_floor_the_age_and_the_second_signature() {
    let (mut w, policy, held) = suspended_with(Settings {
        write_off_second_approver_minor: 500_00,
        ..Settings::default()
    });
    assert_eq!(code(w.act("woh", "fap-one", Action::WriteOffHeld { amount_minor: held })), "write_off_not_allowed");
    w.until_status(&policy, PolicyStatus::Lapsed);
    let write_off = |amount_minor| Action::WriteOffDebt {
        amount_minor,
        debt: Debt::Commission,
    };
    assert_eq!(code(w.act("wod", "fap-one", write_off(1_200_01))), "insufficient_debt");
    w.act("wod", "fap-one", write_off(1_200_00)).unwrap();
    assert_eq!(w.dist.actions["wod"].needed, vec![Role::CounterpartyFinanceApprover, Role::CounterpartyFinanceApprover]);
    w.sign("wod", finance_approver(), Role::CounterpartyFinanceApprover).unwrap();
    assert_eq!(w.payable("fap-one"), -1_200_00);
    w.sign("wod", finance_approver_2(), Role::CounterpartyFinanceApprover).unwrap();
    assert_eq!(w.payable("fap-one"), 0);
    assert_eq!(w.internal(InternalLedger::CommissionBadDebt), 1_200_00);

    // Two years on, the held balance is old enough to write off.
    w.dist.settings.write_off_age_days = 100;
    w.act("woh", "fap-one", Action::WriteOffHeld { amount_minor: held }).unwrap();
    w.sign("woh", finance_approver(), Role::CounterpartyFinanceApprover).unwrap();
    assert_eq!(w.held("fap-one"), 0);
    assert_eq!(w.internal(InternalLedger::WriteOff { class: "held".into() }), -held);
    w.assert_reconciled();
}

/// Purpose: a held balance kept pending a legal outcome has a review date and a legal
/// reference, needs legal's signature, raises a task when the review falls due, and stops
/// blocking month-end while the review date is ahead.
/// Why it matters: "leave it held" must be a decision someone made and will revisit, or held
/// balances age silently, which is the failure the daily control and month-end check exist for.
#[test]
fn holding_pending_a_review_is_a_decision_with_a_date_and_unblocks_month_end() {
    let (mut w, _, held) = suspended_with(Settings {
        month_end_held_threshold_minor: 100_00,
        month_end_held_age_days: 30,
        ..Settings::default()
    });
    w.days(40);
    assert_eq!(w.dist.month_end_blocks(w.today()), vec![(party("fap-one"), held)]);
    let today = w.today();
    let review = today.checked_add_days(chrono::Days::new(10)).unwrap();
    let hold = |review_on, legal_ref: &str| Action::HoldPending {
        review_on,
        legal_ref: legal_ref.into(),
    };
    assert_eq!(code(w.act("hold", "fap-one", hold(today, "legal-1"))), "invalid_hold");
    assert_eq!(code(w.act("hold", "fap-one", hold(review, ""))), "invalid_hold");
    assert_eq!(code(w.act("hold", "a-co", hold(review, "legal-1"))), "insufficient_held");
    w.act("hold", "fap-one", hold(review, "legal-1")).unwrap();
    w.sign("hold", finance_approver(), Role::CounterpartyFinanceApprover).unwrap();
    assert_eq!(w.dist.month_end_blocks(w.today()).len(), 1, "not decided until legal signs");
    let events = w.sign("hold", legal(), Role::Legal).unwrap();
    assert!(events.iter().any(|e| matches!(e, Event::ActionCompleted { .. })));
    assert!(w.dist.month_end_blocks(w.today()).is_empty());
    assert_eq!(w.held("fap-one"), held);

    // The daily control records the aged total; on the review date it raises the task.
    let control = |w: &mut World| w.send(module("end-of-day"), Command::RecordDailyControl).unwrap();
    control(&mut w);
    control(&mut w);
    assert_eq!(w.dist.controls.len(), 1, "one snapshot per day");
    assert_eq!(w.dist.controls[0].aged_minor, [0, held, 0, 0]);
    assert!(w.dist.controls[0].agrees_with_ledger && w.dist.controls[0].parties == 1);
    let tasks = w.dist.tasks.len();
    w.days(10);
    control(&mut w);
    assert_eq!(w.dist.tasks.len(), tasks + 1);
    let task = w.dist.tasks.values().filter(|t| t.kind == "held_review" && t.done.is_none()).next_back().unwrap().id.clone();
    assert_eq!(w.dist.month_end_blocks(w.today()).len(), 1, "the review date has arrived");
    w.send(finance(), Command::CompleteTask { id: task.clone(), note: "reviewed".into() }).unwrap();
    assert_eq!(code(w.send(finance(), Command::CompleteTask { id: task, note: "again".into() })), "task_done");
    assert_eq!(code(w.send(finance(), Command::CompleteTask { id: "ghost".into(), note: "x".into() })), "unknown_task");
    assert_eq!(code(w.send(finance(), Command::RecordDailyControl)), "caller_not_allowed");
    w.days(400);
    assert_eq!(w.dist.held_control(w.today()).aged_minor, [0, 0, 0, held]);
    w.days(1);
    assert_eq!(w.dist.held_control(start()).aged_minor[0], held);
}

/// Purpose: a disputed held amount is allocated between the parties by a split that carries
/// each party's agreement, and lands in their payable accounts.
/// Why it matters: disputes over who earned a share end in an agreed split. The agreement
/// references on the action are the evidence if either party later disagrees.
#[test]
fn a_disputed_amount_is_split_between_the_parties_who_agreed_to_it() {
    let mut w = worked_example();
    let today = w.today();
    let mut disputed = base("fap-one", Share::OfRemainder(pct(25.0)), false);
    disputed.disputed = true;
    let lines = vec![base("a-co", Share::OfRemainder(pct(75.0)), true), disputed];
    w.activate("dispute", vec![Change::ActivateRule(rule("a-at-fap-one", 2, today, lines, vec![liable("fap-one")]))])
        .unwrap();
    let (mut w, _, _) = advised_in(w);
    let held = w.held("fap-one");
    let a_co = w.payable("a-co");
    let share = |who: &str, amount_minor, agreement: &str| SplitShare {
        party: party(who),
        amount_minor,
        agreement_ref: agreement.into(),
    };
    let split = |shares| Action::Split { shares };
    assert_eq!(code(w.act("split", "fap-one", split(vec![]))), "invalid_split");
    assert_eq!(code(w.act("split", "fap-one", split(vec![share("a-co", 100, " ")]))), "invalid_split");
    assert_eq!(code(w.act("split", "fap-one", split(vec![share("adviser-a", 100, "agreed")]))), "unknown_payee");
    assert_eq!(code(w.act("split", "fap-one", split(vec![share("a-co", held + 1, "agreed")]))), "insufficient_held");
    w.act("split", "fap-one", split(vec![share("a-co", 100_00, "a-agrees"), share("fap-one", held - 100_00, "fap-agrees")]))
        .unwrap();
    w.sign("split", finance_approver(), Role::CounterpartyFinanceApprover).unwrap();
    assert_eq!(w.held("fap-one"), 0);
    assert_eq!(w.payable("a-co"), a_co + 100_00);
    assert_eq!(w.payable("fap-one"), held - 100_00);
    w.assert_reconciled();
}

/// Purpose: an aged clawback debt moves to the party a rule names as guarantor, and to nobody
/// else.
/// Why it matters: a guarantee is only worth something if the debt can actually be moved to
/// the guarantor, and it must be impossible to move a debt to a party that never agreed.
#[test]
fn an_aged_debt_moves_to_the_guarantor_the_rule_names() {
    let mut w = worked_example();
    let today = w.today();
    let mut liability = liable("fap-one");
    liability.guarantor = Some(party("group-old"));
    let lines = vec![base("a-co", Share::OfRemainder(pct(75.0)), true), base("fap-one", Share::OfRemainder(pct(25.0)), false)];
    w.activate("guaranteed", vec![Change::ActivateRule(rule("a-at-fap-one", 2, today, lines, vec![liability]))])
        .unwrap();
    let (mut w, policy, _) = advised_in(w);
    w.until_status(&policy, PolicyStatus::Lapsed);
    let debt = -w.payable("fap-one");
    assert!(debt > 0);
    let to = |who: &str, amount_minor| Action::MoveToGuarantor {
        guarantor: party(who),
        amount_minor,
    };
    assert_eq!(code(w.act("g", "fap-one", to("fap-two", debt))), "not_a_guarantor");
    assert_eq!(code(w.act("g", "fap-one", to("group-old", debt + 1))), "insufficient_debt");
    w.act("g", "fap-one", to("group-old", debt)).unwrap();
    w.sign("g", finance_approver(), Role::CounterpartyFinanceApprover).unwrap();
    assert_eq!(w.payable("fap-one"), 0);
    assert_eq!(w.payable("group-old"), -debt);
    w.assert_reconciled();
}

/// Purpose: a claims overpayment is raised as held, cannot be offset or described as pursued
/// until the conduct officer and legal sign the change of recovery status, and is then offset
/// against money held for the claimant or written off to its own class.
/// Why it matters: the addendum's reversal-of-admission entry says an overpayment made by the
/// insurer's error is not pursued by default. The workbench is the general counterparty
/// facility, so the same controls must hold for a claimant as for a provider.
#[test]
fn an_overpayment_is_held_until_conduct_and_legal_agree_to_pursue_it() {
    let mut w = worked_example();
    w.add_party("claimant-1", PartyKind::Counterparty);
    let settlement = || CounterpartyAccount::Internal(InternalLedger::Settlement);
    w.post_as_finance(0, "claim-9-overpaid", vec![(acct("claimant-1", PartyLedger::Receivable), 500_00), (settlement(), -500_00)])
        .unwrap();
    w.post_as_finance(0, "claim-10-benefit", vec![(settlement(), 200_00), (acct("claimant-1", PartyLedger::Held), -200_00)])
        .unwrap();
    let note = |recovery| PositionNote {
        class: "claim_overpayment".into(),
        recovery,
        insurer_error: true,
        case_ref: "claim-9".into(),
    };
    let describe = |recovery| Command::DescribePosition {
        party: party("claimant-1"),
        reference: "claim-9-overpaid".into(),
        note: note(recovery),
    };
    assert_eq!(code(w.send(module("claims"), describe(RecoveryStatus::Pursued))), "recovery_status");
    assert_eq!(code(w.send(module("new-business"), describe(RecoveryStatus::Held))), "caller_not_allowed");
    w.send(module("claims"), describe(RecoveryStatus::Held)).unwrap();
    w.send(module("claims"), describe(RecoveryStatus::Held)).unwrap();
    assert_eq!(code(w.send(module("claims"), describe(RecoveryStatus::Pursued))), "recovery_status");
    assert_eq!(w.dist.position(&party("claimant-1"), w.today()).notes.len(), 1);

    let offset = Action::Offset {
        amount_minor: 200_00,
        against: Debt::Receivable,
    };
    assert_eq!(code(w.act("net", "claimant-1", offset.clone())), "recovery_held");
    let set = |reference: &str, to| Action::SetRecovery {
        reference: reference.into(),
        to,
    };
    assert_eq!(code(w.act("rec", "claimant-1", set("ghost", RecoveryStatus::Pursued))), "unknown_reference");
    assert_eq!(code(w.act("rec", "claimant-1", set("claim-9-overpaid", RecoveryStatus::Held))), "invalid_recovery");
    assert_eq!(code(w.act("rec", "fap-one", set("claim-9-overpaid", RecoveryStatus::Pursued))), "invalid_recovery");
    w.act("rec", "claimant-1", set("claim-9-overpaid", RecoveryStatus::Pursued)).unwrap();
    assert_eq!(w.dist.actions["rec"].needed, vec![Role::CounterpartyFinanceApprover, Role::ConductOfficer, Role::Legal]);
    w.sign("rec", finance_approver(), Role::CounterpartyFinanceApprover).unwrap();
    w.sign("rec", conduct(), Role::ConductOfficer).unwrap();
    assert_eq!(code(w.act("net", "claimant-1", offset.clone())), "recovery_held");
    w.sign("rec", legal(), Role::Legal).unwrap();
    assert_eq!(w.dist.notes["claim-9-overpaid"].1.recovery, RecoveryStatus::Pursued);

    w.act("net", "claimant-1", offset).unwrap();
    w.sign("net", finance_approver(), Role::CounterpartyFinanceApprover).unwrap();
    assert_eq!(w.held("claimant-1"), 0);
    assert_eq!(w.ledger("claimant-1", PartyLedger::Receivable), -300_00);
    let write_off = Action::WriteOffDebt {
        amount_minor: 300_00,
        debt: Debt::Receivable,
    };
    w.act("wo", "claimant-1", write_off).unwrap();
    w.sign("wo", finance_approver(), Role::CounterpartyFinanceApprover).unwrap();
    assert_eq!(w.ledger("claimant-1", PartyLedger::Receivable), 0);
    assert_eq!(w.internal(InternalLedger::WriteOff { class: "claim_overpayment".into() }), 300_00);
    // Writing off a held recovery needs no conduct signature: it favours the customer.
    w.act("rec2", "claimant-1", set("claim-9-overpaid", RecoveryStatus::WrittenOff)).unwrap();
    assert_eq!(w.dist.actions["rec2"].needed, vec![Role::CounterpartyFinanceApprover]);
    w.assert_reconciled();
}

/// Purpose: an action needs a well-formed ID, a reason code and a document; it can be rejected
/// with a reason by someone on the workbench; and a settled action takes no more signatures.
/// Why it matters: the reason code and document reference are the audit trail. An action that
/// could be approved after being rejected would defeat the rejection.
#[test]
fn actions_carry_their_evidence_and_a_rejection_is_final() {
    let (mut w, _, held) = suspended();
    let propose = |id: &str, reason: &str, document: &str| Command::ProposeAction {
        id: id.into(),
        party: party("fap-one"),
        action: release(held),
        reason_code: reason.into(),
        document_ref: document.into(),
    };
    assert_eq!(code(w.send(finance(), propose("bad id", "r", "d"))), "invalid_action");
    assert_eq!(code(w.send(finance(), propose("", "r", "d"))), "invalid_action");
    assert_eq!(code(w.send(finance(), propose("a", " ", "d"))), "invalid_action");
    assert_eq!(code(w.send(finance(), propose("a", "r", ""))), "invalid_action");
    w.send(finance(), propose("a", "r", "d")).unwrap();
    assert_eq!(code(w.send(finance(), propose("a", "r", "d"))), "duplicate_action");
    let reject = |id: &str| Command::RejectAction {
        id: id.into(),
        reason: "wrong account".into(),
    };
    assert_eq!(code(w.send(admin(), reject("a"))), "not_authorised");
    assert_eq!(code(w.send(module("claims"), reject("a"))), "caller_not_allowed");
    assert_eq!(code(w.send(legal(), reject("ghost"))), "unknown_action");
    w.send(legal(), reject("a")).unwrap();
    assert_eq!(w.dist.actions["a"].state, ActionState::Rejected);
    assert_eq!(w.dist.actions["a"].rejected_because.as_deref(), Some("wrong account"));
    assert_eq!(code(w.send(legal(), reject("a"))), "action_settled");
    assert_eq!(code(w.sign("a", finance_approver(), Role::CounterpartyFinanceApprover)), "action_settled");
    assert_eq!(code(w.sign("ghost", finance_approver(), Role::CounterpartyFinanceApprover)), "unknown_action");
    let approve = Command::ApproveAction {
        id: "a".into(),
        as_role: Role::Legal,
    };
    assert_eq!(code(w.send(module("claims"), approve)), "caller_not_allowed");
    let attach = |kind: &str, document: &str| Command::AttachDocument {
        party: party("fap-one"),
        kind: kind.into(),
        document_ref: document.into(),
    };
    assert_eq!(code(w.send(finance(), attach("", "d"))), "invalid_document");
    assert_eq!(code(w.send(admin(), attach("demand", "d"))), "not_authorised");
    assert_eq!(w.held("fap-one"), held);
}

/// Purpose: when the core refuses a posting an approved action asked for, the action records
/// the failure, a task is raised and the refusal is an event; the refusal is recorded once.
/// Why it matters: an approved action whose posting silently failed would look done on the
/// workbench while the money never moved. The failure has to be loud and attached to the action.
#[test]
fn a_posting_the_core_refuses_marks_the_action_failed_and_raises_a_task() {
    let (mut w, _, held) = suspended();
    w.act("rel-1", "fap-one", release(held)).unwrap();
    w.auto_deliver = false;
    w.sign("rel-1", finance_approver(), Role::CounterpartyFinanceApprover).unwrap();
    assert_eq!(w.dist.unconfirmed().len(), 1);
    w.outbox.clear();
    let refused = Command::PostingRefused {
        reference: "wb:rel-1:1".into(),
        code: "ledger_closed".into(),
        message: "the period is closed".into(),
    };
    assert_eq!(code(w.send(finance(), refused.clone())), "caller_not_allowed");
    let events = w.send(module("event-relay"), refused.clone()).unwrap();
    assert!(events.iter().any(|e| matches!(e, Event::PostingRefused { code, .. } if code == "ledger_closed")));
    assert_eq!(w.dist.actions["rel-1"].failed.as_deref(), Some("ledger_closed: the period is closed"));
    assert!(w.dist.tasks.values().any(|t| t.kind == "posting_refused" && t.subject == "wb:rel-1:1"));
    assert_eq!(code(w.send(module("event-relay"), refused)), "posting_settled");
    let unknown = Command::PostingRefused {
        reference: "nothing".into(),
        code: "x".into(),
        message: "y".into(),
    };
    assert_eq!(code(w.send(module("event-relay"), unknown)), "unknown_reference");
    assert!(w.dist.unconfirmed().is_empty());
    assert_eq!(w.held("fap-one"), held, "the money never moved");
}

/// Purpose: held money on one partition is netted against a debt on another through the
/// party's clearing account, which returns to zero across the partitions.
/// Why it matters: a provider's policies sit on many partitions and no entry can span two.
/// If the cross-partition legs did not net to zero, the ledgers would hold a phantom balance.
#[test]
fn an_offset_across_partitions_nets_through_clearing_to_zero() {
    let mut w = worked_example_in(World::with(4, Settings::default()));
    let (first, p1) = w.issue(1, 500_000_00, None);
    let mut n = 2;
    let (second, p2) = loop {
        let (policy, p) = w.issue(n, 500_000_00, None);
        if p != p1 {
            break (policy, p);
        }
        n += 1;
    };
    assert_ne!(p1, p2);
    for policy in [&first, &second] {
        w.write(policy, "life", 1, "eng-a-one", "a-at-fap-one", 1_200_00).unwrap();
        w.pay(policy, "r1");
    }
    w.days(1);
    w.licence("fap-one", LicenceState::Suspended, start()).unwrap();
    // The first policy lapses; the second keeps paying.
    for _ in 0..200 {
        if w.status(&first) == PolicyStatus::Lapsed {
            break;
        }
        w.days(1);
        if w.owing(&second) > 0 {
            let receipt = format!("r-{}", w.today());
            w.pay(&second, &receipt);
        }
    }
    assert_eq!(w.status(&first), PolicyStatus::Lapsed);
    let held = w.held("fap-one");
    let by_partition = w.dist.by_partition(&party("fap-one"), PartyLedger::Held);
    assert_eq!(by_partition.len(), 2, "held on both partitions: {by_partition:?}");
    assert_eq!(w.payable("fap-one"), -1_200_00);
    let offset = Action::Offset {
        amount_minor: held,
        against: Debt::Commission,
    };
    w.act("net", "fap-one", offset).unwrap();
    w.sign("net", finance_approver(), Role::CounterpartyFinanceApprover).unwrap();
    assert_eq!(w.held("fap-one"), 0);
    assert_eq!(w.payable("fap-one"), held - 1_200_00);
    assert_eq!(w.ledger("fap-one", PartyLedger::Clearing), 0);
    assert_eq!(w.dist.requested.keys().filter(|k| k.starts_with("wb:net:")).count(), 2);
    assert_eq!(w.dist.actions["net"].state, ActionState::Completed);
    w.assert_reconciled();
}
