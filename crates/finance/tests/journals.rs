//! Manual journals: a maker, a checker, and the rules a journal must meet.

mod common;

use common::*;
use ic_finance::books::{FinEvent, JournalDraft, JournalKind, JournalStatus, MAX_JOURNAL_LINES};
use ic_finance::close::CheckStatus;
use ic_finance::FinCommand;
use ic_money::FINE_PER_MINOR;

/// A core that has run from 20 November into December, so November is closing.
fn world() -> World {
    let mut w = World::new(1, d(2027, 11, 20));
    let (p, id) = w.issue(1, d(2027, 11, 20));
    w.pay(p, &id, "r1", MONTHLY);
    w.until(d(2027, 12, 3));
    w
}

fn nov() -> ic_finance::PeriodId {
    period(2027, 11)
}

fn propose(journal: JournalDraft) -> FinCommand {
    FinCommand::ProposeJournal { journal }
}

fn approve(id: &str) -> FinCommand {
    FinCommand::ApproveJournal { id: id.to_string() }
}

fn accrual(id: &str) -> JournalDraft {
    journal(id, JournalKind::Accrual, nov(), "6500", "2500", 250_00)
}

fn balance(w: &World, account: &str) -> i128 {
    w.books
        .state
        .trial_balance(nov(), None)
        .iter()
        .filter(|r| r.account == account)
        .map(|r| r.balance)
        .sum()
}

/// Purpose: a journal is entered by one person and posts only when a second person with the
/// approver's authority approves it. The maker cannot approve their own journal even when
/// they hold the authority, a decided journal cannot be decided again, and a rejection needs
/// a reason and posts nothing.
/// Why it matters: manual journals are the one way a person changes the books directly.
/// Separation of maker and checker, enforced in the ledger rather than on a screen, is the
/// control auditors test first.
#[test]
fn a_journal_posts_only_when_a_second_person_approves_it() {
    let mut w = world();
    let b = &mut w.books;
    // Anyone without the operator's role cannot enter one, and a command with no person is refused.
    assert_eq!(b.console_code("ana", propose(accrual("j1"))), "authority");
    assert_eq!(b.code("finance-console", None, propose(accrual("j1"))), "actor");
    assert_eq!(b.code("commission", Some("olive"), propose(accrual("j1"))), "caller");

    b.console("lena", propose(accrual("j1"))).unwrap();
    assert_eq!(balance(&w, "6500"), 0, "a proposed journal has not posted");
    let b = &mut w.books;
    let checks = b.state.checks(nov());
    let CheckStatus::Red { items } = &checks.iter().find(|c| c.id == "unapproved_journals").unwrap().status else {
        panic!("a proposed journal does not hold up the close")
    };
    assert_eq!(items, &vec!["journal j1 proposed by lena is not approved".to_string()]);

    assert_eq!(b.state.dashboard(nov(), d(2027, 12, 3)).journals.proposed, 1);
    assert_eq!(b.console_code("olive", approve("j1")), "authority");
    assert_eq!(b.console_code("lena", approve("j1")), "separation");
    assert_eq!(b.console_code("ana", approve("missing")), "journal");
    b.console("ana", approve("j1")).unwrap();
    let JournalStatus::Approved { checker, entry } = &b.state.journals["j1"].status else {
        panic!("not approved")
    };
    assert_eq!(checker, "ana");
    let posted = b.applied.last().unwrap();
    assert_eq!(posted.entries[0].id, *entry);
    assert_eq!(posted.entries[0].body.kind, "journal_accrual");
    assert_eq!(posted.entries[0].body.detail["maker"], "lena");
    assert_eq!(posted.entries[0].body.detail["evidence"], "vault://evidence/1");
    assert_eq!(b.console_code("ana", approve("j1")), "journal");
    assert_eq!(balance(&w, "6500"), 250_00 * FINE_PER_MINOR);
    assert_eq!(balance(&w, "2500"), -250_00 * FINE_PER_MINOR);

    let b = &mut w.books;
    b.console("olive", propose(accrual("j2"))).unwrap();
    let reject = |reason: &str| FinCommand::RejectJournal {
        id: "j2".to_string(),
        reason: reason.to_string(),
    };
    assert_eq!(b.console_code("ana", reject(" ")), "journal");
    assert_eq!(b.console_code("olive", reject("wrong account")), "authority");
    b.console("ana", reject("wrong account")).unwrap();
    assert!(matches!(b.applied.last().unwrap().events[0].event, FinEvent::JournalRejected { .. }));
    assert_eq!(b.console_code("ana", approve("j2")), "journal");
    assert_eq!(balance(&w, "6500"), 250_00 * FINE_PER_MINOR, "a rejected journal posts nothing");
    let counts = w.books.state.dashboard(nov(), d(2027, 12, 3)).journals;
    assert_eq!((counts.proposed, counts.approved, counts.rejected), (0, 1, 1));
}

/// Purpose: a journal is refused when it has no ID or reuses one, lacks a reason or
/// evidence, has fewer than two lines or too many, takes effect after its period, names an
/// account the chart does not have, posts to a control account, has a zero line, does not
/// balance, or is entered as the system-built year-end kind.
/// Why it matters: each rule closes a way to put an unexplained or unbalanced number into
/// the books. The control account lock in particular keeps the accounts fed from the core
/// equal to the core, which the daily control check depends on.
#[test]
fn a_journal_that_breaks_a_rule_is_refused_before_it_is_recorded() {
    let mut w = world();
    let b = &mut w.books;
    let code = |b: &mut Books, change: &dyn Fn(&mut JournalDraft)| {
        let mut j = accrual("j");
        change(&mut j);
        b.console_code("olive", propose(j))
    };
    assert_eq!(code(b, &|j| j.id.clear()), "journal");
    assert_eq!(code(b, &|j| j.reason = "  ".to_string()), "journal");
    assert_eq!(code(b, &|j| j.evidence.clear()), "journal");
    assert_eq!(code(b, &|j| j.evidence = vec![String::new()]), "journal");
    assert_eq!(code(b, &|j| j.lines.truncate(1)), "journal");
    assert_eq!(code(b, &|j| j.lines = vec![j.lines[0].clone(); MAX_JOURNAL_LINES + 1]), "journal");
    assert_eq!(code(b, &|j| j.value_date = d(2027, 12, 1)), "journal");
    assert_eq!(code(b, &|j| j.lines[0].account = "9999".to_string()), "account");
    assert_eq!(code(b, &|j| j.lines[0].account = "1100".to_string()), "locked");
    assert_eq!(code(b, &|j| j.lines[0].signed_fine = 0), "journal");
    assert_eq!(code(b, &|j| j.lines[0].signed_fine += 1), "unbalanced");
    assert_eq!(code(b, &|j| j.kind = JournalKind::YearEndClose), "journal");
    assert_eq!(code(b, &|j| j.period = period(2028, 6)), "period");
    assert_eq!(code(b, &|j| j.period = period(2028, 3).adjustment()), "period");
    assert!(b.state.journals.is_empty());

    b.console("olive", propose(accrual("j"))).unwrap();
    assert_eq!(b.console_code("olive", propose(accrual("j"))), "journal");

    // Two lines that cancel on one account post nothing, so there is nothing to approve.
    let mut cancels = accrual("nil");
    cancels.lines[1].account = "6500".to_string();
    b.console("olive", propose(cancels)).unwrap();
    assert_eq!(b.console_code("ana", approve("nil")), "journal");
}

/// Purpose: every journal balances within each statutory fund. Only a fund transfer journal
/// may name more than one fund, it must name a fund on every line, and only the finance lead
/// approves it.
/// Why it matters: policyholder money in a statutory fund is ring-fenced by law. A journal
/// that moved value between funds as a side effect of an accrual would breach that without
/// anyone having decided to transfer anything.
#[test]
fn money_moves_between_statutory_funds_only_by_an_approved_transfer() {
    let mut w = world();
    let b = &mut w.books;
    let mut across = accrual("x");
    across.lines[1].fund = Some("SF2-health".to_string());
    assert_eq!(b.console_code("olive", propose(across)), "fund");

    // Balanced in each fund, but spanning two: still only a transfer may.
    let mut transfer = journal("t1", JournalKind::Accrual, nov(), "1600", "2500", 900_00);
    let mut other = journal("t1", JournalKind::Accrual, nov(), "6500", "2600", 900_00);
    for line in &mut other.lines {
        line.fund = Some("SF2-health".to_string());
    }
    transfer.lines.extend(other.lines);
    assert_eq!(b.console_code("olive", propose(transfer.clone())), "fund");

    transfer.kind = JournalKind::FundTransfer;
    let mut unnamed = transfer.clone();
    unnamed.lines[2].fund = None;
    unnamed.lines[3].fund = None;
    assert_eq!(b.console_code("olive", propose(unnamed)), "fund_transfer");
    let one_fund = journal("t0", JournalKind::FundTransfer, nov(), "1600", "2600", 900_00);
    assert_eq!(b.console_code("olive", propose(one_fund)), "fund_transfer");

    b.console("olive", propose(transfer)).unwrap();
    assert_eq!(b.console_code("ana", approve("t1")), "authority");
    b.console("lena", approve("t1")).unwrap();
    let health = b.state.trial_balance(nov(), Some("SF2-health"));
    assert_eq!(health.len(), 2);
    assert_eq!(health.iter().map(|r| r.balance).sum::<i128>(), 0);
}

/// Purpose: a journal dated into an earlier period posts in the period it is entered in and
/// is reported there as a prior-period item under the period its value date falls in, apart
/// from the period's own activity.
/// Why it matters: a correction to an earlier month must not look like this month's trading.
/// Finance explains the two differently, and the regulator's returns restate the earlier
/// period from exactly these items.
#[test]
fn a_journal_dated_into_an_earlier_period_is_a_prior_period_item() {
    let mut w = world();
    w.until(d(2027, 12, 20));
    let dec = period(2027, 12);
    let mut correction = journal("c1", JournalKind::Correction, dec, "6500", "2500", 75_00);
    correction.value_date = d(2027, 11, 25);
    w.books.console("olive", propose(correction)).unwrap();
    w.books.console("ana", approve("c1")).unwrap();
    assert_eq!(w.books.applied.last().unwrap().entries[0].body.detail["origin_period"], "2027-11");

    let rows = w.books.state.movements(dec);
    let row = rows.iter().find(|r| r.account == "6500").unwrap();
    assert_eq!((row.opening, row.activity, row.closing), (0, 0, 75_00 * FINE_PER_MINOR));
    assert_eq!(row.prior_period[&nov()], 75_00 * FINE_PER_MINOR);
    assert_eq!(row.name, "Operating expenses");
    let summary = w.books.state.dashboard(dec, d(2027, 12, 20)).prior_period;
    assert_eq!(summary.len(), 1);
    assert_eq!((summary[0].origin, summary[0].entries), (nov(), 0));
    assert_eq!(summary[0].result_effect["NZD"], -75_00 * FINE_PER_MINOR, "an expense lowers the result");
    // November itself is untouched.
    assert_eq!(w.books.state.trial_balance(nov(), None).iter().filter(|r| r.account == "6500").count(), 0);
}

/// Purpose: an actuarial journal must name a registered extract, and the appointed actuary's
/// sign-off is given by an actuary who did not enter it, once, on a journal that is actuarial
/// and not rejected.
/// Why it matters: reserves are the largest judgement in an insurer's accounts. The journal
/// that books them has to be traceable to the frozen data it was calculated from and carry
/// the sign-off of the person the law holds responsible.
#[test]
fn an_actuarial_journal_names_its_extract_and_is_signed_by_the_actuary() {
    let mut w = world();
    let sign = |id: &str| FinCommand::SignActuarialJournal { id: id.to_string() };
    let mut reserves = journal("a1", JournalKind::Actuarial, nov(), "6000", "2400", 1_000_00);
    assert_eq!(w.books.console_code("kiri", propose(reserves.clone())), "extract");
    reserves.extract_id = Some("in_force-2027-11".to_string());
    assert_eq!(w.books.console_code("kiri", propose(reserves.clone())), "extract");

    let extract = w.extract("in_force", nov());
    let b = &mut w.books;
    b.send("reporting", None, FinCommand::RegisterExtract { extract }).unwrap();
    b.console("kiri", propose(reserves)).unwrap();
    assert_eq!(b.console_code("ana", sign("a1")), "authority");
    assert_eq!(b.console_code("kiri", sign("a1")), "separation");
    assert_eq!(b.console_code("aroha", sign("missing")), "journal");
    b.console("aroha", sign("a1")).unwrap();
    let events = b.send_keyed("again", "finance-console", Some("aroha"), sign("a1"));
    assert_eq!((events.result, events.events.len()), (Ok(()), 0), "a second signature changes nothing");
    assert_eq!(b.state.journals["a1"].actuary.as_deref(), Some("aroha"));
    b.console("ana", approve("a1")).unwrap();
    assert_eq!(b.applied.last().unwrap().entries[0].body.detail["extract"], "in_force-2027-11");

    b.console("olive", propose(accrual("j1"))).unwrap();
    assert_eq!(b.console_code("aroha", sign("j1")), "journal");
}

/// Purpose: a journal that does not balance is refused with the difference in dollars and
/// cents, saying which side is heavier, and a part of a cent is shown rather than hidden.
/// Why it matters: found by the scenario run, SCN-17 (F-SCN17-2). The refusal gave the
/// difference in millionths of a cent, so a journal one cent out read "off by 1000000",
/// which a finance operator cannot act on.
#[test]
fn an_unbalanced_journal_is_refused_in_dollars() {
    let mut w = world();
    let b = &mut w.books;
    let message = |b: &mut Books, change: &dyn Fn(&mut JournalDraft)| {
        let mut j = accrual("j");
        change(&mut j);
        b.console("olive", propose(j)).unwrap_err().message
    };
    assert_eq!(
        message(b, &|j| j.lines[0].signed_fine += FINE_PER_MINOR),
        "the journal does not balance: debits are more than credits by $0.01 NZD"
    );
    assert_eq!(
        message(b, &|j| j.lines[1].signed_fine -= 123_456_78 * FINE_PER_MINOR + 500_000),
        "the journal does not balance: credits are more than debits by $123,456.785 NZD"
    );
}
