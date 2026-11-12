//! Cases the simulated core does not produce on its own: investment fund units, a line in
//! the wrong statutory fund, a partition that falls behind, and commands sent before the
//! ledger is set up or by the wrong service.

mod common;

use std::collections::BTreeMap;

use chrono::NaiveDate;
use common::*;
use ic_core::event::{Event, EventRecord, PostedLine, RoleTotal};
use ic_finance::books::{AgedItem, ExtractRecord, JournalKind};
use ic_finance::close::{Capacity, CheckStatus, Outcome};
use ic_finance::FinCommand;
use ic_ledger::{AccountClass, AccountUnit, BalanceFamily, Dimensions, EntryId, Hash32, Ledger};
use ic_money::{Currency::NZD, FundId, FINE_PER_MINOR};

/// A hand-built stream of one partition's events, chained the way the core chains them.
struct Stream {
    events: Vec<EventRecord>,
    head: Hash32,
    date: NaiveDate,
    entries: u64,
    roles: BTreeMap<(String, AccountClass, AccountUnit), i128>,
}

impl Stream {
    fn open(date: NaiveDate) -> Self {
        let mut s = Self {
            events: vec![],
            head: Ledger::genesis(0),
            date,
            entries: 0,
            roles: BTreeMap::new(),
        };
        let opened = Event::BusinessDateAdvanced {
            from: None,
            to: date,
            closing_head: s.head,
        };
        s.push(opened);
        s
    }

    fn push(&mut self, event: Event) {
        let seq = self.events.len() as u64 + 1;
        self.events.push(EventRecord {
            id: format!("0-{seq}"),
            partition: 0,
            seq,
            business_date: self.date,
            log_index: seq,
            traceparent: None,
            event,
        });
    }

    fn entry(&mut self, lines: Vec<PostedLine>) {
        self.entries += 1;
        self.head = Hash32([self.entries as u8; 32]);
        for line in &lines {
            *self.roles.entry((line.role.clone(), line.class, line.unit.clone())).or_insert(0) += line.signed;
        }
        let posted = Event::EntryPosted {
            entry: EntryId {
                partition: 0,
                seq: self.entries,
            },
            kind: "synthetic".to_string(),
            policy: None,
            hash: self.head,
            value_date: Some(self.date),
            reverses: None,
            lines,
        };
        self.push(posted);
    }

    fn close(&mut self) {
        let from = self.date;
        self.date = from.succ_opt().unwrap();
        let advanced = Event::BusinessDateAdvanced {
            from: Some(from),
            to: self.date,
            closing_head: self.head,
        };
        self.push(advanced);
        let totals = self
            .roles
            .iter()
            .filter(|(_, balance)| **balance != 0)
            .map(|((role, class, unit), balance)| RoleTotal {
                role: role.clone(),
                class: *class,
                unit: unit.clone(),
                balance: *balance,
            })
            .collect();
        let control = Event::ControlTotals {
            date: from,
            closing_head: self.head,
            totals,
        };
        self.push(control);
    }
}

fn line(role: &str, class: AccountClass, unit: AccountUnit, signed: i128, fund: Option<&str>) -> PostedLine {
    PostedLine {
        role: role.to_string(),
        class,
        unit,
        signed,
        dims: Dimensions {
            statutory_fund: fund.map(str::to_string),
            ..Dimensions::default()
        },
    }
}

fn status(b: &Books, period: ic_finance::PeriodId, check: &str) -> CheckStatus {
    b.state.checks(period).into_iter().find(|c| c.id == check).unwrap().status
}

/// Purpose: units of an investment fund reach the books in their own unit family, separate
/// from money, on accounts of their own. They balance within the fund's units, the control
/// check compares them like any other role, and money-only reports and the year-end close
/// leave them alone.
/// Why it matters: a unit is not a dollar. Adding micro-units of a growth fund into a
/// currency column would make the trial balance meaningless, and investment-linked business
/// is a third of the product set the core was designed for.
#[test]
fn units_of_an_investment_fund_are_kept_apart_from_money() {
    let growth = FundId("growth".to_string());
    let units = AccountUnit::Units(growth.clone());
    let mut s = Stream::open(d(2028, 3, 30));
    s.entry(vec![
        line("fund_unit_position", AccountClass::Asset, units.clone(), 1_500_000, Some("SF3-investment")),
        line("unit_account", AccountClass::Liability, units.clone(), -1_500_000, Some("SF3-investment")),
    ]);
    s.entry(vec![
        line("settlement", AccountClass::Asset, AccountUnit::Minor(NZD), 100_00, Some("SF3-investment")),
        line(
            "earned_premium",
            AccountClass::Income,
            AccountUnit::Fine(NZD),
            -100_00 * FINE_PER_MINOR,
            Some("SF3-investment"),
        ),
    ]);
    s.close();
    s.close();
    let mut b = Books::ready(1);
    b.feed(&s.events).unwrap();

    let mar = period(2028, 3);
    assert!(b.state.day(d(2028, 3, 30), 0).unwrap().differences.is_empty());
    let rows = b.state.trial_balance(mar, None);
    let unit_rows: Vec<_> = rows.iter().filter(|r| r.unit == BalanceFamily::Fund(growth.clone())).collect();
    assert_eq!(
        unit_rows.iter().map(|r| (r.account.as_str(), r.balance)).collect::<Vec<_>>(),
        vec![("1300", 1_500_000), ("2300", -1_500_000)]
    );
    assert_eq!(b.state.ledger.balance(&ic_ledger::AccountId("1300/units/growth".to_string())), 1_500_000);
    assert_eq!(b.state.ledger.balance(&ic_ledger::AccountId("1000/NZD".to_string())), 100_00 * FINE_PER_MINOR);
    assert_eq!(status(&b, mar, "trial_balance"), CheckStatus::Green);
    assert_eq!(status(&b, mar, "fund_balance"), CheckStatus::Green);

    // The year-end journal closes income in money and does not touch the units.
    for kind in ["in_force", "movement"] {
        let extract = ExtractRecord {
            id: format!("{kind}-mar"),
            kind: kind.to_string(),
            as_at: mar.end(),
            content_hash: Hash32([7; 32]),
            rows: 1,
            merkle_root: Hash32([5; 32]),
            definition_version: 1,
            location: "worm://extracts/x".to_string(),
            control: BTreeMap::new(),
            reconciled: true,
        };
        let root = FinCommand::RecordMerkleRoot {
            date: mar.end(),
            root: Hash32([5; 32]),
            reference: "worm://roots/x".to_string(),
        };
        b.send("feed", None, root).unwrap();
        b.send("reporting", None, FinCommand::RegisterExtract { extract }).unwrap();
    }
    let year_end = FinCommand::ProposeYearEndClose {
        id: "ye".to_string(),
        year_end: mar,
        evidence: vec!["vault://year-end".to_string()],
    };
    assert_eq!(b.console_code("olive", year_end), "period", "March is still open");
    let summary = b.state.dashboard(mar, d(2028, 4, 1));
    assert!(summary.prior_period.is_empty());
    assert_eq!((summary.control.expected, summary.control.checked), (2, 2));
}

/// Purpose: when a line reaches the books in a different statutory fund from the rest of its
/// entry, the books still balance as a whole but the fund balance check turns red and names
/// each fund that is out, including postings with no fund at all.
/// Why it matters: statutory funds are ring-fenced. An entry that balances across two funds
/// has moved policyholder money between them without a transfer, and the whole-of-books
/// trial balance cannot see it.
#[test]
fn a_line_in_the_wrong_statutory_fund_turns_the_fund_check_red() {
    let mut s = Stream::open(d(2028, 3, 30));
    s.entry(vec![
        line("settlement", AccountClass::Asset, AccountUnit::Minor(NZD), 40_00, None),
        line("settlement", AccountClass::Asset, AccountUnit::Minor(NZD), 60_00, Some("SF2-health")),
        line("premium_receivable", AccountClass::Asset, AccountUnit::Minor(NZD), -100_00, Some("SF1-life")),
    ]);
    s.close();
    let mut b = Books::ready(1);
    b.feed(&s.events).unwrap();
    let mar = period(2028, 3);
    assert_eq!(status(&b, mar, "trial_balance"), CheckStatus::Green);
    let CheckStatus::Red { items } = status(&b, mar, "fund_balance") else {
        panic!("a fund out of balance was not reported")
    };
    assert_eq!(items.len(), 3);
    assert_eq!(
        items[0],
        format!(
            "postings with no statutory fund are out of balance in Currency(NZD) by {}",
            40_00 * FINE_PER_MINOR
        )
    );
    assert_eq!(
        items[1],
        format!("statutory fund SF1-life is out of balance in Currency(NZD) by {}", -100_00 * FINE_PER_MINOR)
    );
    assert!(items[2].starts_with("statutory fund SF2-health"));
    assert_eq!(b.state.trial_balance(mar, Some("SF2-health")).len(), 1);
}

/// Purpose: before any day arrives the control check says so. When one partition has
/// reported a date and another has not, the check names the date and how many partitions
/// are missing, the date is not treated as anchored, and the period does not start closing.
/// Why it matters: a partition whose feed has stalled looks, from the ledger's totals alone,
/// just like a partition with no business. The close must not go ahead on a ledger that is
/// missing a sixty-fourth of the insurer.
#[test]
fn a_partition_that_has_not_reported_keeps_the_control_check_red() {
    let mut w = World::new(2, d(2027, 11, 29));
    let nov = period(2027, 11);
    let CheckStatus::Red { items } = status(&w.books, nov, "control_check") else {
        panic!("an empty ledger passed the control check")
    };
    assert_eq!(items, vec!["no business day has been received from the core".to_string()]);
    let empty = w.books.state.dashboard(nov, d(2027, 11, 29));
    assert_eq!((empty.status, empty.control.expected), (None, 0));

    for _ in 0..3 {
        w.core.end_of_day().unwrap();
    }
    let events = w.core.take_events();
    let only_first: Vec<_> = events.iter().filter(|e| e.partition == 0).cloned().collect();
    w.books.feed(&only_first).unwrap();
    let CheckStatus::Red { items } = status(&w.books, nov, "control_check") else {
        panic!("a missing partition passed the control check")
    };
    assert_eq!(
        items,
        vec![
            "2027-11-29: 1 partitions not yet checked against the core".to_string(),
            "2027-11-30: 1 partitions not yet checked against the core".to_string()
        ]
    );
    let board = w.books.state.dashboard(nov, d(2027, 12, 2));
    assert_eq!((board.control.expected, board.control.checked), (4, 2));
    assert!(
        board.control.unanchored.is_empty(),
        "a date is judged against its root only once every partition is in"
    );
    assert_eq!(board.status, Some(ic_finance::close::PeriodStatus::Open), "November has not ended everywhere");

    let rest: Vec<_> = events.iter().filter(|e| e.partition == 1).cloned().collect();
    w.books.feed(&rest).unwrap();
    assert_eq!(w.books.state.periods[&nov].status, ic_finance::close::PeriodStatus::Closing);
}

/// Purpose: every command is accepted only from the service that owns it, and commands that
/// need a chart or a close configuration are refused until one is active. A long list of
/// red items is cut to the first fifty with a count of the rest.
/// Why it matters: the caller list is what stops, say, the commission service from closing
/// a period or the feed from approving a journal. A ledger that half-worked before it was
/// configured would post to accounts that a later chart might not have.
#[test]
fn commands_are_refused_from_the_wrong_service_or_before_setup() {
    let mut b = Books::ready(1);
    let p = period(2028, 3);
    let wrong: Vec<(&str, FinCommand)> = vec![
        ("feed", FinCommand::ProposeChart { chart: chart() }),
        ("feed", FinCommand::ProposeCloseConfig { config: close_config() }),
        ("feed", FinCommand::ApproveConfig { id: "x".to_string() }),
        (
            "reporting",
            FinCommand::RecordMerkleRoot {
                date: p.end(),
                root: Hash32([1; 32]),
                reference: "x".to_string(),
            },
        ),
        ("feed", FinCommand::ApproveJournal { id: "x".to_string() }),
        (
            "feed",
            FinCommand::RejectJournal {
                id: "x".to_string(),
                reason: "x".to_string(),
            },
        ),
        ("feed", FinCommand::SignActuarialJournal { id: "x".to_string() }),
        (
            "feed",
            FinCommand::ProposeYearEndClose {
                id: "x".to_string(),
                year_end: p,
                evidence: vec![],
            },
        ),
        (
            "feed",
            FinCommand::WaiveCheck {
                period: p,
                check: "x".to_string(),
                reason: "x".to_string(),
                remediation_owner: "x".to_string(),
                due: p.end(),
            },
        ),
        (
            "feed",
            FinCommand::SignOff {
                period: p,
                capacity: Capacity::FinanceLead,
            },
        ),
    ];
    for (caller, command) in wrong {
        let name = command.name();
        let refused = b.send(caller, Some("lena"), command).unwrap_err();
        assert_eq!((refused.code.as_str(), refused.message), ("caller", format!("{caller} may not send {name}")));
    }
    // The two reports are open to whichever caller the close configuration names.
    let report = FinCommand::ReportCheck {
        period: p,
        check: "commission_sweep".to_string(),
        outcome: Outcome::Green,
        evidence: None,
    };
    assert_eq!((report.name(), report.allowed_callers().len()), ("report_check", 0));
    let aged = |items: Vec<AgedItem>| FinCommand::ReportAgedItems {
        kind: "policy_suspense".to_string(),
        as_at: d(2028, 3, 31),
        total_minor: 0,
        count: items.len() as u32,
        items,
    };
    assert_eq!(aged(vec![]).name(), "report_aged_items");

    // Sixty old suspense items: the check lists fifty and counts the rest.
    let old = |n: u32| AgedItem {
        party: format!("pol-{n:03}"),
        reason: "unallocated".to_string(),
        amount_minor: 10_00,
        since: d(2027, 12, 1),
        owner: None,
        action: None,
    };
    b.send("close-workflow", None, aged((0..60).map(old).collect())).unwrap();
    let CheckStatus::Red { items } = status(&b, p, "aged:policy_suspense") else {
        panic!("old suspense items did not turn the check red")
    };
    assert_eq!((items.len(), items[50].as_str()), (51, "and 10 more"));

    // Before a chart or a close configuration is active.
    let mut bare = Books::empty();
    bare.send(
        "operator",
        None,
        FinCommand::Configure {
            partitions: 1,
            year_end_month: 3,
        },
    )
    .unwrap();
    let accrual = journal("j", JournalKind::Accrual, p, "6500", "2500", 1_00);
    assert_eq!(bare.console_code("olive", FinCommand::ProposeJournal { journal: accrual }), "chart");
    let year_end = FinCommand::ProposeYearEndClose {
        id: "ye".to_string(),
        year_end: p,
        evidence: vec!["x".to_string()],
    };
    assert_eq!(bare.console_code("olive", year_end), "chart");
    let sign = FinCommand::SignOff {
        period: p,
        capacity: Capacity::FinanceLead,
    };
    assert_eq!(bare.console_code("lena", sign), "close_config");
    assert_eq!(bare.code("commission", None, report), "check");
    assert_eq!(bare.code("collections", None, aged(vec![])), "aged");
}
