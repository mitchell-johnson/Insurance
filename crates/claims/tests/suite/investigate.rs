//! Restricted investigations and recovery from a provider.

use ic_claims::investigate::Outcome;
use ic_claims::model::{HealthItem, OverpaymentCause};
use ic_claims::report::{Question, Reply};
use ic_claims::{ClaimType, Command, Instruction, RecoveryStatus, RunKind, Status};

use crate::world::*;

/// A dental invoice from provider-9 for one service on one date.
fn invoice(id: &str, code: &str, invoice_minor: i64) -> Command {
    let mut n = World::notification(id, "pol-3", "health", ClaimType::Health);
    n.health = Some(HealthItem {
        category: "dental".into(),
        invoice_minor,
        provider: Some("provider-9".into()),
        pre_approval_request: false,
        settles_pre_approval: None,
        service_code: Some(code.into()),
        service_date: Some(d(2027, 2, 20)),
    });
    Command::Notify(Box::new(n))
}

fn provider_run(w: &mut World, run: &str) {
    w.ok(Command::PrepareRun {
        run: run.into(),
        kind: RunKind::Provider,
        benefit_date: w.core.today,
        actor: actor("ana"),
    });
    w.ok(Command::ApproveRun {
        run: run.into(),
        actor: actor("sam"),
    });
}

fn close(case: &str, outcome: Outcome, who: &str) -> Command {
    Command::CloseInvestigation {
        case: case.into(),
        outcome,
        actor: actor(who),
    }
}

fn cases(w: &World, staff: &str) -> Vec<ic_claims::investigate::CaseView> {
    match w.state.answer(&Question::Cases { staff: staff.into() }, w.core.today) {
        Reply::Cases(found) => found,
        other => panic!("a cases question gets cases, not {other:?}"),
    }
}

/// Purpose: a provider billing the same service on the same date twice opens a restricted case:
/// the second invoice is not paid by the rules and looks to the assessor and the claimant like
/// any claim waiting for a person; only an investigator sees the case or can close it. When the
/// investigator confirms it, the duplicate is declined and what was paid on the first becomes the
/// provider's debt, taken back out of the provider's next run.
/// Why it matters: persona test CLM-015. An investigation that front line staff or the member
/// could see would tip off the provider; a duplicate paid by the rules before anyone looks is
/// money lost; and a recovery nobody takes is a debt written off by neglect.
#[test]
fn a_service_billed_twice_is_investigated_quietly_and_recovered_from_the_provider() {
    let mut w = World::new();
    w.ok(invoice("h-1", "D110", 150));
    assert_eq!(w.claim("h-1").status, Status::Admitted, "the first invoice goes straight through");
    provider_run(&mut w, "prov-1");
    assert_eq!(w.claim("h-1").status, Status::Paid);

    // The same service again: registered and reserved, not admitted, and in a case.
    w.ok(invoice("h-2", "D110", 150));
    let held = w.claim("h-2").clone();
    assert_eq!((held.status, held.reserve_minor), (Status::Open, 150));
    let case = held.investigation.clone().expect("a case is open");
    assert_eq!(w.claim("h-1").investigation.as_deref(), Some(case.as_str()));
    let opened = &w.core.noticed("investigation_opened")[0];
    assert_eq!((opened["rule"].as_str(), opened["claims"].as_str()), ("duplicate_billing", "h-1,h-2"));
    // Another service from the same provider is untouched.
    w.ok(invoice("h-3", "D220", 150));
    assert_eq!(w.claim("h-3").status, Status::Admitted);

    // Nothing shows outside the investigators' view.
    let work = serde_json::to_string(&w.state.work_view("h-2").expect("a view")).expect("serialises");
    let own = serde_json::to_string(&w.state.claimant_view("h-2").expect("a view")).expect("serialises");
    for shown in [work, own] {
        assert!(!shown.contains("investigat") && !shown.contains("duplicate"), "{shown}");
    }
    assert!(w.claim("h-2").tasks.is_empty(), "no task tells anyone");
    assert!(cases(&w, "ana").is_empty());
    let seen = cases(&w, "ivy");
    assert_eq!((seen.len(), seen[0].claims.len(), seen[0].rule.as_str()), (1, 2, "duplicate_billing"));
    assert_eq!(
        w.refused(close(&case, Outcome::Cleared, "ana")),
        "unknown_case",
        "the same answer as for no case"
    );
    assert_eq!(w.refused(close("investigation/none", Outcome::Cleared, "ivy")), "unknown_case");

    // Money stays where it is while the case is open.
    w.assign("h-2", "ana");
    assert_eq!(
        w.refused(Command::Pay {
            claim: "h-2".into(),
            payment_ref: "p-1".into(),
            amount_minor: 50,
            reissue_of: None,
            actor: actor("ana"),
        }),
        "payment_held"
    );

    // Confirmed: the duplicate is declined and the first payment is the provider's debt.
    let outside = Outcome::Confirmed {
        decline: vec!["h-3".into()],
        recover: vec![],
    };
    assert_eq!(w.refused(close(&case, outside, "ivy")), "not_in_case");
    let too_much = Outcome::Confirmed {
        decline: vec![],
        recover: vec![("h-1".into(), 1_000)],
    };
    assert_eq!(w.refused(close(&case, too_much, "ivy")), "invalid_recovery");
    let paid = w.claim("h-1").paid_minor(false);
    let confirmed = Outcome::Confirmed {
        decline: vec!["h-2".into()],
        recover: vec![("h-1".into(), paid)],
    };
    w.ok(close(&case, confirmed.clone(), "ivy"));
    assert_eq!(w.refused(close(&case, confirmed, "ivy")), "unknown_case", "a closed case is closed");
    assert_eq!(w.claim("h-2").status, Status::Declined);
    assert!(w.claim("h-1").investigation.is_none() && w.claim("h-2").investigation.is_none());
    let debt = w.claim("h-1").overpayments[0].clone();
    assert_eq!(
        (debt.cause, debt.status, debt.offset_pct, debt.amount_minor),
        (OverpaymentCause::ProviderBilling, RecoveryStatus::Offset, 100, paid)
    );
    assert_eq!(w.core.noticed("investigation_closed")[0]["outcome"], "confirmed");
    assert!(cases(&w, "ivy")[0].closed.is_some());

    // The provider's next invoice pays the debt down before anything is paid out.
    w.ok(invoice("h-4", "D330", 150));
    let owed = w.claim("h-4").payable_minor + w.claim("h-3").payable_minor;
    provider_run(&mut w, "prov-2");
    let kept: i64 = w
        .core
        .instructions
        .iter()
        .filter_map(|i| match i {
            Instruction::Pay { offset: Some(o), .. } if o.claim == "h-1" => Some(o.amount_minor),
            _ => None,
        })
        .sum();
    assert_eq!(kept, paid.min(owed));
    assert_eq!(w.claim("h-1").overpayments[0].recovered_minor, kept);
    assert_eq!(w.state.runs["prov-2"].control_total_minor, owed - kept);
}

/// Purpose: the compliance module refers a case over named claims; a repeat of the same referral
/// changes nothing, a different one under the same case or an unknown claim is refused, and a
/// cleared case lets the claims go on as they would have.
/// Why it matters: referrals arrive at least once over a relay. A repeat that opened a second
/// case, or a cleared case that left a claim held, would stop an honest member's claim for good.
#[test]
fn a_referred_case_is_opened_once_and_clearing_it_releases_the_claims() {
    let mut w = World::new();
    w.benefit_in_payment("ip-1");
    let refer = |claims: &[&str]| Command::ReferForInvestigation {
        case: "cmp-1".into(),
        rule: "soon_after_inception".into(),
        claims: claims.iter().map(|c| c.to_string()).collect(),
    };
    assert_eq!(w.refused(refer(&[])), "invalid_referral");
    assert_eq!(w.refused(refer(&["c-404"])), "unknown_claim");
    w.ok(refer(&["ip-1"]));
    w.ok(refer(&["ip-1"]));
    assert_eq!(w.core.noticed("investigation_opened").len(), 1);
    w.open_claim("c-1");
    assert_eq!(w.refused(refer(&["ip-1", "c-1"])), "case_exists");
    assert_eq!(cases(&w, "ivy")[0].referred_by, "compliance");

    // Held while the case is open: the run leaves the benefit out.
    let run = Command::PrepareRun {
        run: "run-0".into(),
        kind: RunKind::Benefit,
        benefit_date: d(2027, 3, 1),
        actor: actor("ana"),
    };
    assert_eq!(w.refused(run), "nothing_to_pay", "nothing was due to anyone else");
    w.ok(close("cmp-1", Outcome::Cleared, "ivy"));
    assert!(w.claim("ip-1").investigation.is_none());
    w.pay_run("run-1", d(2027, 3, 1));
    assert_eq!(w.state.runs["run-1"].count, 1, "the held month is paid once cleared");
}
