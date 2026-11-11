//! What the employer sees of the money: the invoice by billing group, with
//! adjustments and loadings, and what happens when a scheme keeps paying short.

mod common;

use common::*;
use ic_core::Reject;
use ic_group::command::{caller, ExcessDecision, GroupCommand};
use ic_group::event::{ExceptionKind, GroupEvent};
use ic_group::mapping::FileKind;
use ic_group::record::RowChange;
use ic_money::DecStr;

fn code(r: Result<(), Reject>) -> String {
    r.unwrap_err().code
}

fn members() -> Vec<ic_group::record::MemberRow> {
    vec![
        row(1, "E001", attrs("staff", 60_000_00, "akl")),
        row(2, "E002", attrs("staff", 60_000_00, "wlg")),
        row(3, "E004", attrs("executive", 240_000_00, "wlg")),
    ]
}

/// Purpose: the module's invoice view agrees with the core to the cent: members and
/// premium by billing group, retrospective adjustments against the group they belong to,
/// and underwriting loadings as one scheme total that names no member.
/// Why it matters: this is the document the employer pays from and splits across its own
/// cost centres. If it differs from what the core posted, the employer pays the wrong
/// amount; if a loading showed against a member, the employer learns about their health.
#[test]
fn the_invoice_view_agrees_with_the_core_by_billing_group() {
    let mut h = Harness::new(terms());
    h.apply("f1", FileKind::Full, members());
    // The founder was billed 11.00 at issue; the two joiners' first month came as adjustments.
    assert_eq!(h.owed(), 112_62);
    assert_eq!(h.scheme().outstanding_minor, 112_62, "the balance follows the core's own event");
    assert_eq!(h.scheme().pending_adjustments["wlg"], 101_62);

    // Underwriting accepts the executive's excess with a 50% loading.
    let case = h.requests[0].case;
    h.underwriting(
        case,
        ExcessDecision::Accepted {
            sum_insured_minor: 210_000_00,
            loadings: vec![DecStr::parse("0.5").unwrap()],
            exclusions: vec![],
            member_consents_to_itemising: false,
        },
    )
    .unwrap();
    let before_invoice = h.owed();
    h.days(31);
    let s = h.scheme();
    assert_eq!(s.invoices.len(), 1);
    let invoice = &s.invoices[0];
    assert_eq!(invoice.period_index, 1);
    assert_eq!((invoice.by_group["akl"].members, invoice.by_group["akl"].total_minor), (1, 11_00));
    assert_eq!(invoice.by_group["wlg"].members, 2);
    assert!(invoice.loading_minor > 0, "the loading is billed");
    assert!(invoice.adjustments_by_group["wlg"] >= 101_62);
    assert!(s.pending_adjustments.is_empty() && s.pending_loading_adjustments_minor == 0);
    // Everything owed is either on this invoice or was the founder's first month.
    assert_eq!(h.owed(), 11_00 + invoice.total_minor());
    assert_eq!(
        h.owed() - before_invoice,
        invoice.total_minor() - invoice.adjustments_by_group["wlg"] - invoice.loading_adjustments_minor
    );
    assert_eq!(invoice.members(), 3);
    assert!(h.saw(|e| matches!(
        e,
        GroupEvent::InvoiceIssued {
            period_index: 1,
            members: 3,
            ..
        }
    )));
    // The loading is nowhere in a member's group line: wlg is exactly the two unloaded premiums
    // plus the unloaded excess layer.
    let wlg_unloaded = invoice.by_group["wlg"].total_minor;
    assert_eq!(
        wlg_unloaded + invoice.by_group["akl"].total_minor + invoice.loading_minor,
        h.owed() - before_invoice
    );

    h.pay(h.owed());
    assert_eq!((h.owed(), h.scheme().outstanding_minor), (0, 0));

    // A joiner with no billing group is billed under the scheme's first; one with a group
    // the scheme does not have is refused, so nothing is billed where nobody will pay it.
    let today = h.today();
    let mut no_group = attrs("staff", 60_000_00, "akl");
    no_group.billing_group = None;
    let rows = vec![
        change(1, "E009", RowChange::Join, today, attrs("staff", 60_000_00, "chc")),
        change(2, "E010", RowChange::Join, today, no_group),
    ];
    h.apply("f2", FileKind::Changes, rows);
    assert!(h.scheme().files["f2"]
        .rejected
        .iter()
        .any(|r| r.row == 1 && r.field.as_deref() == Some("billing_group")));
    h.days(31);
    let invoice = h.scheme().invoices.back().unwrap();
    assert_eq!(invoice.by_group["akl"].members, 2);
    assert_eq!(invoice.by_group.len(), 2);
}

/// Purpose: a scheme that pays short as many times in a row as its terms allow is raised
/// to staff and takes no new joiners, its existing members keep their cover, and paying
/// in full lifts the suspension.
/// Why it matters: persistent short payment is how a scheme drifts into arrears while
/// still adding lives. Cutting existing members off for it would punish employees for
/// their employer's bookkeeping; adding more lives would grow the debt.
#[test]
fn persistent_short_payment_suspends_joiners_until_paid() {
    let mut h = Harness::new(terms());
    h.apply("f1", FileKind::Full, members());
    let short = |h: &mut Harness, minor| {
        let r = h.send_as(
            caller::CASH,
            "cash-matching",
            GroupCommand::RecordShortPayment {
                scheme: SCHEME.into(),
                shortfall_minor: minor,
            },
        );
        h.settle();
        r
    };
    assert_eq!(code(short(&mut h, 0)), "invalid_amount");
    short(&mut h, 10_00).unwrap();
    assert!(!h.scheme().joiners_suspended);
    short(&mut h, 12_00).unwrap();
    assert!(h.scheme().joiners_suspended);
    assert!(h.saw(|e| matches!(e, GroupEvent::ShortPaymentRecorded { in_a_row: 2, .. })));
    assert!(h.saw(|e| matches!(e, GroupEvent::JoinersSuspended { .. })));
    assert!(h.g.exceptions.values().any(|e| e.kind == ExceptionKind::PersistentShortPayment));
    short(&mut h, 5_00).unwrap();
    assert_eq!(h.published.iter().filter(|e| matches!(e, GroupEvent::JoinersSuspended { .. })).count(), 1);

    // A file with a joiner and a salary change: the change applies, the joiner does not.
    let today = h.today();
    let rows = vec![
        change(1, "E007", RowChange::Join, today, attrs("staff", 60_000_00, "akl")),
        change(2, "E002", RowChange::Change, today, attrs("staff", 65_000_00, "wlg")),
    ];
    h.apply("f2", FileKind::Changes, rows);
    let f = &h.scheme().files["f2"];
    assert_eq!((f.summary.joiners, f.summary.changes, f.applied), (0, 1, 1));
    assert!(f.rejected.iter().any(|r| r.code == "joiners_suspended" && r.row == 1));
    assert_eq!(h.core_lives(), vec!["E001", "E002", "E004"]);

    h.pay(h.owed());
    assert!(!h.scheme().joiners_suspended);
    assert_eq!(h.scheme().short_payments, 0);
    assert!(h.saw(|e| matches!(e, GroupEvent::JoinersResumed { .. })));
    assert!(!h.g.exceptions.values().any(|e| e.kind == ExceptionKind::PersistentShortPayment));
    h.apply(
        "f3",
        FileKind::Changes,
        vec![change(1, "E007", RowChange::Join, today, attrs("staff", 60_000_00, "akl"))],
    );
    assert!(h.core_lives().contains(&"E007".to_string()));
}
