//! The financial year end and its adjustment period.

mod common;

use common::*;
use ic_finance::books::{JournalKind, JournalStatus};
use ic_finance::close::{Capacity, CheckStatus, PeriodStatus};
use ic_finance::FinCommand;

/// Purpose: once the last month of the financial year has closed, the ledger builds the
/// journal that closes every income and expense balance to retained earnings, fund by fund,
/// in the year's adjustment period. It needs a maker and a checker like any journal. The
/// closed month keeps its own trial balance, the new year opens with no income or expense
/// brought forward, and the adjustment period is signed off and closed on its own.
/// Why it matters: year-end entries arrive weeks after March has been reported. They need
/// somewhere to go that is neither the frozen month nor the new year, and the closing
/// journal has to be exact in every fund or the new year's result starts wrong.
#[test]
fn a_financial_year_closes_income_and_expense_to_retained_earnings() {
    let mut w = World::new(1, d(2028, 2, 20));
    let (p, id) = w.issue(1, d(2028, 2, 20));
    w.pay(p, &id, "r1", 3 * MONTHLY);
    w.until(d(2028, 4, 4));
    let (feb, mar, apr) = (period(2028, 2), period(2028, 3), period(2028, 4));
    let adj = mar.adjustment();
    let year_end = |id: &str, year_end| FinCommand::ProposeYearEndClose {
        id: id.to_string(),
        year_end,
        evidence: vec!["vault://year-end/2028".to_string()],
    };
    assert_eq!(w.books.console_code("olive", year_end("ye", feb)), "period");
    assert_eq!(w.books.console_code("olive", year_end("ye", adj)), "period");
    assert_eq!(w.books.console_code("olive", year_end("ye", mar)), "period", "March has not closed yet");
    assert_eq!(w.books.console_code("ana", year_end("ye", mar)), "authority");

    w.close(feb);
    w.close(mar);
    let frozen = w.books.state.periods[&mar].closed.clone().unwrap();
    let result: i128 = frozen
        .trial_balance
        .iter()
        .filter(|r| ["4000", "6000"].contains(&r.account.as_str()))
        .map(|r| r.balance)
        .sum();
    assert_ne!(result, 0);

    let b = &mut w.books;
    b.console("olive", year_end("ye", mar)).unwrap();
    let built = &b.state.journals["ye"];
    assert_eq!(
        (built.draft.kind, built.draft.period, built.draft.value_date),
        (JournalKind::YearEndClose, adj, mar.end())
    );
    assert_eq!(built.status, JournalStatus::Proposed);
    assert_eq!(b.console_code("olive", year_end("ye", mar)), "journal", "the ID is taken");
    assert_eq!(b.console_code("olive", FinCommand::ApproveJournal { id: "ye".to_string() }), "authority");
    b.console("ana", FinCommand::ApproveJournal { id: "ye".to_string() }).unwrap();
    assert_eq!(b.applied.last().unwrap().entries[0].body.kind, "journal_year_end_close");

    let state = &b.state;
    let after = state.trial_balance(adj, None);
    assert!(
        after.iter().all(|r| !["4000", "6000"].contains(&r.account.as_str())),
        "income and expense are closed out"
    );
    assert_eq!(after.iter().filter(|r| r.account == "3900").map(|r| r.balance).sum::<i128>(), result);
    assert_eq!(after.iter().map(|r| r.balance).sum::<i128>(), 0);
    assert_eq!(state.trial_balance(mar, None), frozen.trial_balance, "the closed month is as it was reported");
    let year = state.movements(adj);
    assert!(year.iter().filter(|r| r.account == "4000").all(|r| r.year_to_date == 0 && r.closing == 0));
    let opening = state.movements(apr);
    assert!(opening
        .iter()
        .filter(|r| r.account == "4000")
        .all(|r| r.opening == 0 && r.year_to_date == r.activity));

    // The adjustment period has no sweeps, extracts or control days of its own.
    let checks = state.checks(adj);
    assert!(
        checks.iter().all(|c| matches!(c.status, CheckStatus::Green | CheckStatus::NotDue)),
        "{checks:?}"
    );
    assert_eq!(checks.iter().find(|c| c.id == "commission_sweep").unwrap().status, CheckStatus::NotDue);
    assert_eq!(checks.iter().find(|c| c.id == "aged:held_balance").unwrap().status, CheckStatus::NotDue);
    let board = state.dashboard(adj, d(2028, 4, 20));
    assert_eq!(
        (board.status, board.control.expected, board.extracts.len()),
        (Some(PeriodStatus::Closing), 0, 0)
    );
    assert_eq!(board.sign_offs_needed, vec![Capacity::FinanceLead]);

    b.console(
        "lena",
        FinCommand::SignOff {
            period: adj,
            capacity: Capacity::FinanceLead,
        },
    )
    .unwrap();
    b.send("close-workflow", None, FinCommand::ClosePeriod { period: adj }).unwrap();
    assert_eq!(b.state.periods[&adj].status, PeriodStatus::Closed);
    assert_eq!(b.console_code("olive", year_end("ye2", mar)), "period_closed");
}
