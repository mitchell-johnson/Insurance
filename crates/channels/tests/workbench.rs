//! The held balance workbench: positions published by the modules that own
//! the money, and actions on held balances under maker-checker with legal
//! as a named role.

mod common;

use common::*;
use ic_channels::classes::DataClass;
use ic_channels::staff::{actions as staff_actions, Profile, StaffCommand, Template};
use ic_channels::workbench::*;
use ic_channels::{Command, Effect};
use ic_core::Reject;
use serde_json::json;

const FAP: &str = "fap-1";

fn template(id: &str, actions: &[&str], profile: &str, ceiling: i64) -> Template {
    Template {
        id: id.to_string(),
        actions: actions.iter().map(|a| a.to_string()).collect(),
        classes: [DataClass::Ordinary, DataClass::Financial].into(),
        default_profile: profile.to_string(),
        ceilings: [(LIMIT.to_string(), ceiling)].into(),
        break_glass: false,
    }
}

fn profile(id: &str, limit: i64) -> Profile {
    Profile {
        id: id.to_string(),
        version: 1,
        limits: [(LIMIT.to_string(), limit)].into(),
    }
}

/// A finance team of three, a lawyer, and two people who configure.
fn staffed() -> World {
    let mut w = World::new();
    w.send(
        module("entitlements"),
        Command::Staff(StaffCommand::Bootstrap {
            templates: vec![
                template("administrator", &[staff_actions::MANAGE_STAFF, actions::CONFIGURE], "none", 0),
                template(
                    "counterparty-finance",
                    &[actions::VIEW, actions::PREPARE, actions::APPROVE],
                    "finance-1",
                    1_000_000_00,
                ),
                template("legal", &[actions::VIEW, actions::APPROVE_LEGAL], "legal-1", 10_000_000_00),
            ],
            profiles: vec![profile("none", 0), profile("finance-1", 100_000_00), profile("legal-1", 10_000_000_00)],
            admin: p("alice"),
            admin_templates: vec!["administrator".into()],
        }),
    )
    .unwrap();
    for (who, role) in [
        ("bob", "administrator"),
        ("fin-1", "counterparty-finance"),
        ("fin-2", "counterparty-finance"),
        ("fin-3", "counterparty-finance"),
        ("lex", "legal"),
    ] {
        w.send(
            staff("alice"),
            Command::Staff(StaffCommand::Employ {
                party: p(who),
                team: "finance".into(),
                manager: None,
                templates: vec![role.into()],
            }),
        )
        .unwrap();
    }
    w
}

fn item(id: &str, kind: ItemKind, amount: i64, period: &str, reason: Option<HoldReason>) -> Item {
    Item {
        id: id.to_string(),
        kind,
        amount_minor: amount,
        currency: "NZD".into(),
        period: Some(period.to_string()),
        book: Some("book-7".into()),
        reason,
        since: d(2027, 10, 1),
    }
}

fn publish(items: Vec<Item>) -> Command {
    Command::Workbench(WorkbenchCommand::Publish {
        party: p(FAP),
        items,
        documents: vec![],
        cases: vec![],
    })
}

/// The FAP has 80,000.00 held over two periods and owes 30,000.00 of clawback.
fn held(w: &mut World) {
    let held = Some(HoldReason::SuspendedLicence);
    w.send(
        module("commission"),
        publish(vec![
            item("h-10", ItemKind::Held, 50_000_00, "2027-10", held),
            item("h-11", ItemKind::Held, 30_000_00, "2027-11", held),
            item("cb-1", ItemKind::ClawbackReceivable, -30_000_00, "2027-09", None),
        ]),
    )
    .unwrap();
}

fn prepare(id: &str, action: Action) -> Command {
    Command::Workbench(WorkbenchCommand::Prepare {
        id: id.to_string(),
        party: p(FAP),
        currency: "NZD".into(),
        action,
        reason_code: "licence_revoked".into(),
        document: "vault:fma-2027-118".into(),
    })
}

fn release(amount: i64) -> Action {
    Action::Release {
        amount_minor: amount,
        account: "acct-verified-1".into(),
        new_account: false,
    }
}

fn approve(id: &str, legal: bool) -> Command {
    Command::Workbench(WorkbenchCommand::Approve { id: id.to_string(), legal })
}

fn ready(effects: &Result<Vec<Effect>, Reject>) -> bool {
    effects
        .as_ref()
        .unwrap()
        .iter()
        .any(|e| matches!(e, Effect::Workbench(WorkbenchEvent::Ready { .. })))
}

/// Purpose: a party's position is assembled from what each owning module publishes, with status, documents and cases, and nets correctly.
/// Why it matters: approvers sign on the strength of this screen; a missing receivable or a module overwriting another's lines would have finance release money the party owes back.
#[test]
fn a_position_is_built_from_what_modules_publish() {
    let mut w = staffed();
    held(&mut w);
    w.send(
        module("finance"),
        Command::Workbench(WorkbenchCommand::Publish {
            party: p(FAP),
            items: vec![
                item("op-1", ItemKind::Overpayment, -5_000_00, "2027-08", None),
                item("pi-1", ItemKind::PaymentInstruction, 0, "2027-01", None),
            ],
            documents: vec![Document {
                reference: "vault:agreement-1".into(),
                kind: "agreement".into(),
                filed: d(2025, 1, 1),
            }],
            cases: vec!["conduct-77".into()],
        }),
    )
    .unwrap();
    let status = |source: &str| {
        Command::Workbench(WorkbenchCommand::RecordStatus {
            party: p(FAP),
            status: PartyStatus::LicenceRevoked,
            effective: open(),
            source: source.to_string(),
        })
    };
    assert_eq!(code(w.send(module("commission"), status("FMA"))), "forbidden");
    assert_eq!(code(w.send(module("compliance"), status(" "))), "no_source");
    w.send(module("compliance"), status("FMA notice 2027/118")).unwrap();

    let view = w.ch.data.workbench.position(&p(FAP)).unwrap();
    assert_eq!(view["held"], json!({ "NZD": 80_000_00 }));
    assert_eq!(view["net"], json!({ "NZD": 45_000_00 }));
    assert_eq!(view["status"], "licence_revoked");
    assert_eq!(view["items"].as_array().unwrap().len(), 5);
    assert_eq!(view["cases"], json!(["conduct-77"]));
    assert_eq!(view["documents"][0]["kind"], "agreement");
    assert!(w.ch.data.workbench.position(&p("nobody")).is_none());

    // The payee sees their own held money and why, and nothing else.
    let own = w.ch.data.workbench.own_held(&p(FAP));
    assert_eq!(own["held"], json!({ "NZD": 80_000_00 }));
    assert_eq!(own["entries"].as_array().unwrap().len(), 2);
    assert_eq!(own["entries"][0]["reason"], "suspended_licence");
    assert!(own["entries"][0].get("id").is_none());
    assert_eq!(w.ch.data.workbench.own_held(&p("nobody")), json!({ "held": {}, "entries": [] }));

    // A module replaces its own lines only; publishing none removes them.
    w.send(
        module("commission"),
        publish(vec![item("h-11", ItemKind::Held, 30_000_00, "2027-11", Some(HoldReason::Dispute))]),
    )
    .unwrap();
    assert_eq!(w.ch.data.workbench.position(&p(FAP)).unwrap()["net"], json!({ "NZD": 25_000_00 }));
    w.send(module("commission"), publish(vec![])).unwrap();
    assert_eq!(w.ch.data.workbench.position(&p(FAP)).unwrap()["held"], json!({}));

    // What is refused.
    assert_eq!(code(w.send(staff("fin-1"), publish(vec![]))), "forbidden");
    assert_eq!(code(w.send(module("identity"), publish(vec![]))), "forbidden");
    assert_eq!(
        code(w.send(module("commission"), publish(vec![item("h", ItemKind::Held, 1, "p", None)]))),
        "bad_item"
    );
    assert_eq!(
        code(w.send(
            module("commission"),
            publish(vec![item("h", ItemKind::Held, -1, "p", Some(HoldReason::Dispute))])
        )),
        "bad_item"
    );
    assert_eq!(
        code(w.send(module("commission"), publish(vec![item(" ", ItemKind::Advance, 1, "p", None)]))),
        "bad_item"
    );
    let mut odd = item("x", ItemKind::Advance, 1, "p", None);
    odd.currency = "dollars".into();
    assert_eq!(code(w.send(module("commission"), publish(vec![odd]))), "bad_item");
}

/// Purpose: a release is prepared by one person with its reason and document, approved by another, sent to the owning module, and closed with the core's reference.
/// Why it matters: this is money leaving a held account; preparer and approver must differ, the amount must exist and not be promised twice, and only the module that posts it may say it is done.
#[test]
fn a_release_is_prepared_approved_and_posted() {
    let mut w = staffed();
    held(&mut w);

    // Preparation is validated before anyone is asked to approve.
    assert_eq!(code(w.send(party("fin-1"), prepare("a1", release(1)))), "forbidden");
    assert_eq!(code(w.send(module("commission"), prepare("a1", release(1)))), "forbidden");
    assert_eq!(code(w.send(staff("lex"), prepare("a1", release(1)))), "not_authorised");
    assert_eq!(code(w.send(staff("nobody"), prepare("a1", release(1)))), "not_authorised");
    assert_eq!(code(w.send(staff("fin-1"), prepare("a1", release(0)))), "bad_amount");
    assert_eq!(code(w.send(staff("fin-1"), prepare("a1", release(80_000_01)))), "exceeds_held");
    let no_account = Action::Release {
        amount_minor: 1,
        account: " ".into(),
        new_account: false,
    };
    assert_eq!(code(w.send(staff("fin-1"), prepare("a1", no_account))), "no_account");
    let undocumented = |reason: &str, document: &str| {
        Command::Workbench(WorkbenchCommand::Prepare {
            id: "a1".into(),
            party: p(FAP),
            currency: "NZD".into(),
            action: release(1),
            reason_code: reason.into(),
            document: document.into(),
        })
    };
    assert_eq!(code(w.send(staff("fin-1"), undocumented("", "doc"))), "undocumented");
    assert_eq!(code(w.send(staff("fin-1"), undocumented("reason", " "))), "undocumented");
    let elsewhere = Command::Workbench(WorkbenchCommand::Prepare {
        id: "a1".into(),
        party: p("nobody"),
        currency: "NZD".into(),
        action: release(1),
        reason_code: "r".into(),
        document: "d".into(),
    });
    assert_eq!(code(w.send(staff("fin-1"), elsewhere)), "unknown_party");
    assert!(w.ch.data.workbench.proposals.is_empty());

    let effects = w.send(staff("fin-1"), prepare("a1", release(40_000_00))).unwrap();
    assert_eq!(effects, vec![Effect::Workbench(WorkbenchEvent::Prepared { id: "a1".into() })]);
    assert_eq!(code(w.send(staff("fin-1"), prepare("a1", release(1)))), "duplicate");
    let a1 = &w.ch.data.workbench.proposals["a1"];
    assert_eq!((a1.needs, a1.module.as_str()), (Needs { approvers: 1, legal: false }, "commission"));
    // What is already promised cannot be promised again.
    assert_eq!(code(w.send(staff("fin-2"), prepare("a2", release(40_000_01)))), "exceeds_held");

    // The preparer cannot approve; nor can someone without the role; legal is not asked.
    assert_eq!(code(w.send(staff("fin-1"), approve("a1", false))), "not_authorised");
    assert_eq!(code(w.send(staff("lex"), approve("a1", false))), "not_authorised");
    assert_eq!(code(w.send(staff("lex"), approve("a1", true))), "not_needed");
    assert_eq!(code(w.send(party("fin-2"), approve("a1", false))), "forbidden");
    assert_eq!(code(w.send(staff("fin-2"), approve("zz", false))), "not_found");
    let effects = w.send(staff("fin-2"), approve("a1", false));
    assert_eq!(
        effects.unwrap(),
        vec![Effect::Workbench(WorkbenchEvent::Ready {
            id: "a1".into(),
            module: "commission".into()
        })]
    );
    assert_eq!(code(w.send(staff("fin-3"), approve("a1", false))), "not_awaiting");

    // Only the posting module closes it, with the core's reference.
    let resolve = |reference: Option<&str>, failed: Option<(&str, &str)>| {
        Command::Workbench(WorkbenchCommand::Resolve {
            id: "a1".into(),
            reference: reference.map(String::from),
            failed: failed.map(|(c, m)| (c.to_string(), m.to_string())),
        })
    };
    assert_eq!(code(w.send(module("finance"), resolve(Some("x"), None))), "forbidden");
    assert_eq!(code(w.send(staff("fin-2"), resolve(Some("x"), None))), "forbidden");
    assert_eq!(code(w.send(module("commission"), resolve(None, None))), "no_reference");
    w.send(module("commission"), resolve(Some("core-9"), None)).unwrap();
    assert_eq!(w.ch.data.workbench.proposals["a1"].state, ProposalState::Applied { reference: "core-9".into() });
    assert_eq!(code(w.send(module("commission"), resolve(Some("core-9"), None))), "not_approved");
    let missing = Command::Workbench(WorkbenchCommand::Resolve {
        id: "zz".into(),
        reference: None,
        failed: None,
    });
    assert_eq!(code(w.send(module("commission"), missing)), "not_found");
    // Once posted it no longer counts against the held balance here: the module republishes.
    w.send(staff("fin-2"), prepare("a2", release(80_000_00))).unwrap();
}

/// Purpose: above the threshold an action needs two approvers plus legal, each a different person, and nobody can prepare or approve beyond their own authority.
/// Why it matters: the addendum's control on large releases; one person approving twice, or legal's seat being filled by finance, would defeat it.
#[test]
fn large_actions_need_two_approvers_and_legal() {
    let mut w = staffed();
    held(&mut w);
    w.send(staff("fin-1"), prepare("big", release(60_000_00))).unwrap();
    assert_eq!(w.ch.data.workbench.proposals["big"].needs, Needs { approvers: 2, legal: true });

    assert!(!ready(&w.send(staff("fin-2"), approve("big", false))));
    assert_eq!(code(w.send(staff("fin-2"), approve("big", false))), "already_approved");
    assert_eq!(code(w.send(staff("fin-2"), approve("big", true))), "already_approved");
    assert_eq!(
        code(w.send(staff("fin-3"), approve("big", true))),
        "not_authorised",
        "finance cannot sit in legal's seat"
    );
    assert!(!ready(&w.send(staff("lex"), approve("big", true))));
    assert!(ready(&w.send(staff("fin-3"), approve("big", false))));
    assert_eq!(w.ch.data.workbench.proposals["big"].approvals.len(), 3);

    // The other order: both approvers first, then only legal is outstanding.
    w.send(
        module("commission"),
        publish(vec![item("h-12", ItemKind::Held, 500_000_00, "2027-12", Some(HoldReason::LegalHold))]),
    )
    .unwrap();
    w.send(staff("fin-2"), prepare("big2", release(70_000_00))).unwrap();
    w.send(staff("fin-1"), approve("big2", false)).unwrap();
    w.send(staff("fin-3"), approve("big2", false)).unwrap();
    w.send(
        staff("alice"),
        Command::Staff(StaffCommand::Employ {
            party: p("fin-4"),
            team: "finance".into(),
            manager: None,
            templates: vec!["counterparty-finance".into()],
        }),
    )
    .unwrap();
    assert_eq!(code(w.send(staff("fin-4"), approve("big2", false))), "not_needed");
    assert!(ready(&w.send(staff("lex"), approve("big2", true))));

    // Beyond the preparer's authority profile.
    assert_eq!(code(w.send(staff("fin-1"), prepare("huge", release(100_000_01)))), "not_authorised");
}

/// Purpose: releasing to a new account for a party in liquidation or deceased always needs legal, whatever the amount.
/// Why it matters: paying a liquidator or an estate to an unproven account is the fraud and preference risk the addendum singles out.
#[test]
fn a_new_account_for_a_party_in_liquidation_needs_legal() {
    let mut w = staffed();
    held(&mut w);
    let to_new = || Action::Release {
        amount_minor: 100,
        account: "acct-liquidator".into(),
        new_account: true,
    };
    w.send(staff("fin-1"), prepare("n0", to_new())).unwrap();
    assert_eq!(
        w.ch.data.workbench.proposals["n0"].needs,
        Needs { approvers: 1, legal: false },
        "an ordinary party's new account follows payee verification alone"
    );

    for (id, status) in [("n1", PartyStatus::InLiquidation), ("n2", PartyStatus::Deceased)] {
        w.send(
            module("customer-master"),
            Command::Workbench(WorkbenchCommand::RecordStatus {
                party: p(FAP),
                status,
                effective: open(),
                source: "liquidator's appointment".into(),
            }),
        )
        .unwrap();
        w.send(staff("fin-1"), prepare(id, to_new())).unwrap();
        assert_eq!(w.ch.data.workbench.proposals[id].needs, Needs { approvers: 1, legal: true });
    }
    assert!(!ready(&w.send(staff("fin-2"), approve("n1", false))));
    assert!(ready(&w.send(staff("lex"), approve("n1", true))));
    // An existing account for the same party does not.
    w.send(staff("fin-1"), prepare("n3", release(100))).unwrap();
    assert!(!w.ch.data.workbench.proposals["n3"].needs.legal);
}

/// Purpose: offset, write-off, split, dividend and hold pending are each validated against the position, and a hold sets a review that the daily control reports when due.
/// Why it matters: each of these posts to the ledger; an offset larger than the receivable or a split that allocates more than is held would create money.
#[test]
fn the_other_actions_are_validated_against_the_position() {
    let mut w = staffed();
    held(&mut w);
    let bad = |w: &mut World, action| code(w.send(staff("fin-1"), prepare("x", action)));
    let offset = |amount, receivable: &str| Action::Offset {
        amount_minor: amount,
        receivable: receivable.into(),
    };
    assert_eq!(bad(&mut w, offset(1, "nope")), "unknown_receivable");
    assert_eq!(bad(&mut w, offset(1, "h-10")), "unknown_receivable", "a held line is not a receivable");
    assert_eq!(bad(&mut w, offset(30_000_01, "cb-1")), "exceeds_receivable");
    assert_eq!(bad(&mut w, offset(0, "cb-1")), "bad_amount");
    let share = |who: &str, amount, agreement: &str| Share {
        party: p(who),
        amount_minor: amount,
        agreement: agreement.into(),
    };
    assert_eq!(
        bad(
            &mut w,
            Action::Split {
                shares: vec![share("a", 10, "ag-1")]
            }
        ),
        "bad_split"
    );
    assert_eq!(
        bad(
            &mut w,
            Action::Split {
                shares: vec![share("a", 10, "ag-1"), share("b", 0, "ag-2")]
            }
        ),
        "bad_split"
    );
    assert_eq!(
        bad(
            &mut w,
            Action::Split {
                shares: vec![share("a", 10, "ag-1"), share("b", 10, " ")]
            }
        ),
        "bad_split"
    );
    assert_eq!(
        bad(
            &mut w,
            Action::Split {
                shares: vec![share("a", 50_000_00, "ag-1"), share("b", 30_000_01, "ag-2")]
            }
        ),
        "exceeds_held"
    );
    let dividend = |received| Action::RecordDividend {
        receivable: "cb-1".into(),
        received_minor: received,
    };
    assert_eq!(bad(&mut w, dividend(-1)), "bad_amount");
    assert_eq!(bad(&mut w, dividend(30_000_01)), "bad_amount");
    let hold = |reference: &str| Action::HoldPending {
        review_on: d(2027, 12, 1),
        legal_reference: reference.into(),
    };
    assert_eq!(bad(&mut w, hold(" ")), "no_legal_reference");
    let other_currency = Command::Workbench(WorkbenchCommand::Prepare {
        id: "x".into(),
        party: p(FAP),
        currency: "AUD".into(),
        action: Action::HoldPending {
            review_on: d(2027, 12, 1),
            legal_reference: "L-1".into(),
        },
        reason_code: "r".into(),
        document: "d".into(),
    });
    assert_eq!(code(w.send(staff("fin-1"), other_currency)), "nothing_held");

    // Good ones, and what each needs.
    for (id, action, needs) in [
        ("off", offset(30_000_00, "cb-1"), Needs { approvers: 1, legal: false }),
        ("wo-small", Action::WriteOff { amount_minor: 1_000_00 }, Needs { approvers: 1, legal: false }),
        ("wo", Action::WriteOff { amount_minor: 1_000_01 }, Needs { approvers: 2, legal: false }),
        (
            "split",
            Action::Split {
                shares: vec![share("a", 10_000_00, "ag-1"), share("b", 5_000_00, "ag-2")],
            },
            Needs { approvers: 1, legal: false },
        ),
        ("div", dividend(3_000_00), Needs { approvers: 1, legal: false }),
        ("hold", hold("L-2027-9"), Needs { approvers: 1, legal: false }),
    ] {
        w.send(staff("fin-1"), prepare(id, action)).unwrap();
        assert_eq!(w.ch.data.workbench.proposals[id].needs, needs, "{id}");
    }
    assert!(!ready(&w.send(staff("fin-2"), approve("wo", false))));
    assert!(ready(&w.send(staff("fin-3"), approve("wo", false))));

    // A hold pending sets the review, which the control reports once due.
    let effects = w.send(staff("fin-2"), approve("hold", false)).unwrap();
    assert!(effects.contains(&Effect::Workbench(WorkbenchEvent::ReviewSet {
        party: p(FAP),
        on: d(2027, 12, 1),
        legal_reference: "L-2027-9".into()
    })));
    assert_eq!(w.ch.data.workbench.control(d(2027, 11, 30))["reviews_due"], json!([]));
    assert_eq!(w.ch.data.workbench.control(d(2027, 12, 1))["reviews_due"][0]["legal_reference"], "L-2027-9");
    assert_eq!(w.ch.data.workbench.position(&p(FAP)).unwrap()["review"]["on"], "2027-12-01");

    // Declined, withdrawn and failed actions close without posting.
    let decline = |id: &str, reason: &str| {
        Command::Workbench(WorkbenchCommand::Decline {
            id: id.into(),
            reason: reason.into(),
        })
    };
    assert_eq!(code(w.send(staff("fin-2"), decline("off", " "))), "no_reason");
    assert_eq!(
        code(w.send(staff("fin-1"), decline("off", "wrong receivable"))),
        "not_authorised",
        "the preparer cannot decline their own"
    );
    assert_eq!(code(w.send(party("x"), decline("off", "no"))), "forbidden");
    assert_eq!(code(w.send(staff("fin-2"), decline("zz", "no"))), "not_found");
    w.send(staff("fin-2"), decline("off", "wrong receivable")).unwrap();
    assert_eq!(code(w.send(staff("fin-2"), decline("off", "again"))), "not_awaiting");
    let withdraw = |id: &str| Command::Workbench(WorkbenchCommand::Withdraw { id: id.into() });
    assert_eq!(code(w.send(staff("fin-2"), withdraw("split"))), "not_found", "only the preparer withdraws");
    assert_eq!(code(w.send(module("commission"), withdraw("split"))), "forbidden");
    w.send(staff("fin-1"), withdraw("split")).unwrap();
    assert_eq!(code(w.send(staff("fin-1"), withdraw("split"))), "not_awaiting");
    assert_eq!(w.ch.data.workbench.proposals["split"].state, ProposalState::Withdrawn);
    w.send(
        module("commission"),
        Command::Workbench(WorkbenchCommand::Resolve {
            id: "wo".into(),
            reference: None,
            failed: Some(("period_closed".into(), "the period is closed".into())),
        }),
    )
    .unwrap();
    assert!(matches!(&w.ch.data.workbench.proposals["wo"].state, ProposalState::Failed { code, .. } if code == "period_closed"));
}

/// Purpose: the daily control totals held money across all parties by age band, and the thresholds change only with two people who hold the configuring role.
/// Why it matters: the aged held total is a blocking month-end check; and a single person able to raise the two-approver threshold could then approve alone.
#[test]
fn the_daily_control_and_its_thresholds() {
    let mut w = staffed();
    held(&mut w);
    let mut old = item("h-old", ItemKind::Held, 7_00, "2026-01", Some(HoldReason::MissingPaymentDetails));
    old.since = d(2026, 1, 1);
    let mut aud = item("h-aud", ItemKind::Held, 9_00, "2027-11", Some(HoldReason::Dispute));
    aud.currency = "AUD".into();
    aud.since = open();
    w.send(
        module("reinsurance"),
        Command::Workbench(WorkbenchCommand::Publish {
            party: p("reinsurer-1"),
            items: vec![old, aud],
            documents: vec![],
            cases: vec![],
        }),
    )
    .unwrap();
    let control = w.ch.data.workbench.control(open());
    assert_eq!(control["parties"], 2);
    let nzd = control["held"].as_array().unwrap().iter().find(|c| c["currency"] == "NZD").unwrap();
    assert_eq!(nzd["total_minor"], 80_007_00);
    assert_eq!(
        nzd["aged"],
        json!([
            { "days": "0-30", "minor": 0 }, { "days": "31-90", "minor": 80_000_00 }, { "days": "91-180", "minor": 0 },
            { "days": "181-365", "minor": 0 }, { "days": "over 365", "minor": 7_00 },
        ])
    );
    assert_eq!(
        control["held"].as_array().unwrap().iter().find(|c| c["currency"] == "AUD").unwrap()["aged"][0]["minor"],
        9_00
    );

    let change = |settings: Settings, approved_by: &str| {
        Command::Workbench(WorkbenchCommand::ChangeSettings {
            settings,
            approved_by: p(approved_by),
        })
    };
    let lower = Settings {
        two_approver_above_minor: 10_000_00,
        ..Settings::default()
    };
    assert_eq!(code(w.send(staff("alice"), change(lower.clone(), "alice"))), "maker_checker");
    assert_eq!(code(w.send(staff("alice"), change(lower.clone(), "fin-1"))), "not_authorised");
    assert_eq!(code(w.send(staff("fin-1"), change(lower.clone(), "alice"))), "not_authorised");
    assert_eq!(code(w.send(module("finance"), change(lower.clone(), "alice"))), "forbidden");
    for bad in [
        Settings {
            two_approver_above_minor: -1,
            ..Settings::default()
        },
        Settings {
            write_off_second_above_minor: -1,
            ..Settings::default()
        },
        Settings {
            age_bands: vec![30, 30],
            ..Settings::default()
        },
    ] {
        assert_eq!(code(w.send(staff("alice"), change(bad, "bob"))), "bad_settings");
    }
    w.send(staff("alice"), change(lower, "bob")).unwrap();
    w.send(staff("fin-1"), prepare("a", release(10_000_01))).unwrap();
    assert_eq!(w.ch.data.workbench.proposals["a"].needs, Needs { approvers: 2, legal: true });
    // With no bands the control is one total.
    w.send(
        staff("bob"),
        change(
            Settings {
                age_bands: vec![],
                ..Settings::default()
            },
            "alice",
        ),
    )
    .unwrap();
    assert_eq!(
        w.ch.data.workbench.control(open())["held"][1]["aged"],
        json!([{ "days": "all", "minor": 80_007_00 }])
    );
}

/// Purpose: the proof of debt extract states what the party owes and is owed, by period, each line traced to the entry it came from.
/// Why it matters: this statement is lodged with a liquidator; it must reconcile to the position and be traceable line by line.
#[test]
fn the_proof_of_debt_extract_reconciles_and_traces() {
    let mut w = staffed();
    held(&mut w);
    w.send(
        module("finance"),
        Command::Workbench(WorkbenchCommand::Publish {
            party: p(FAP),
            items: vec![
                item("pi-1", ItemKind::PaymentInstruction, 0, "2027-01", None),
                item("op-1", ItemKind::Overpayment, -5_000_00, "2027-08", None),
            ],
            documents: vec![],
            cases: vec![],
        }),
    )
    .unwrap();
    let extract = w.ch.data.workbench.proof_of_debt(&p(FAP), open()).unwrap();
    assert_eq!(extract["owed_by_party"], json!({ "NZD": 35_000_00 }));
    assert_eq!(extract["owed_to_party"], json!({ "NZD": 80_000_00 }));
    assert_eq!(extract["net"], json!({ "NZD": 45_000_00 }));
    let lines = extract["lines"].as_array().unwrap();
    assert_eq!(
        lines.iter().map(|l| l["period"].as_str().unwrap()).collect::<Vec<_>>(),
        ["2027-08", "2027-09", "2027-10", "2027-11"]
    );
    assert_eq!(lines[0]["lineage"], "finance:op-1");
    assert_eq!(lines[1]["lineage"], "commission:cb-1");
    assert_eq!(extract["as_at"], "2027-11-15");
    assert!(w.ch.data.workbench.proof_of_debt(&p("nobody"), open()).is_none());
}
