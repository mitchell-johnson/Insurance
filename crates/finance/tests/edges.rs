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
        ("feed", FinCommand::ClassifyPolicies),
        (
            "feed",
            FinCommand::RecordCutoverTotal {
                account: "x".to_string(),
                unit: BalanceFamily::Currency(NZD),
                total: 0,
                evidence: "x".to_string(),
            },
        ),
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

/// Purpose: a migration loads the old system's outstanding claims in March, with the
/// other side in the claims migration control account, and loads a little more two days
/// later in the same month. March's check is green: that is the cutover. In April a
/// further load of 5.00 and its reversal reach the account; the balance is back where it
/// was, and April's check is red all the same, naming the account, the month it was
/// loaded in and the 10.00 that passed through. March itself is red until the finance
/// lead records the old system's total and it agrees with the 1,000.00 held: an operator
/// may not record it, nor may anyone without saying where it comes from or for an
/// account that is not a migration control account, and a figure that differs is named
/// beside the balance. Once March is closed the figure can no longer be replaced. May, with nothing, is green. An account
/// the configuration names that the chart does not have is reported, not ignored.
/// Why it matters: the control account's balance is agreed with the old system once, at
/// cutover. Anything through it afterwards is a record loaded or backed out behind that
/// agreement, and a net of nil is exactly what such a correction looks like.
#[test]
fn a_migration_control_account_must_stay_still_after_cutover() {
    let load = |s: &mut Stream, minor: i128| {
        s.entry(vec![
            line(
                "claims_migration_control",
                AccountClass::Asset,
                AccountUnit::Minor(NZD),
                minor,
                Some("SF1-life"),
            ),
            line("claim_reserve", AccountClass::Liability, AccountUnit::Minor(NZD), -minor, Some("SF1-life")),
        ]);
    };
    let mut s = Stream::open(d(2028, 3, 28));
    load(&mut s, 900_00);
    s.close();
    s.close();
    load(&mut s, 100_00);
    while s.date < d(2028, 4, 10) {
        s.close();
    }
    load(&mut s, 5_00);
    load(&mut s, -5_00);
    while s.date < d(2028, 6, 2) {
        s.close();
    }
    let mut b = Books::ready(1);
    b.feed(&s.events).unwrap();

    let check = "migration_control";
    let march = period(2028, 3);
    let held = 1_000_00 * FINE_PER_MINOR;
    let red = |b: &Books, p| match status(b, p, check) {
        CheckStatus::Red { items } => items,
        other => panic!("{other:?}"),
    };
    // Nothing to agree the load with yet.
    let items = red(&b, march);
    assert_eq!(items.len(), 1, "{items:?}");
    assert!(
        items[0].contains(&format!("holds {held} ")) && items[0].contains("no total from the old system"),
        "{items:?}"
    );
    let record = |total: i128, evidence: &str| FinCommand::RecordCutoverTotal {
        account: "1430".to_string(),
        unit: BalanceFamily::Currency(NZD),
        total,
        evidence: evidence.to_string(),
    };
    assert_eq!(b.console_code("olive", record(held, "old claims system, run 42")), "authority");
    assert_eq!(b.console_code("lena", record(held, " ")), "cutover_total");
    let mut other = record(held, "x");
    if let FinCommand::RecordCutoverTotal { account, .. } = &mut other {
        *account = "2200".to_string();
    }
    assert_eq!(b.console_code("lena", other), "cutover_total");
    b.console("lena", record(held - 1, "old claims system, run 41")).unwrap();
    let items = red(&b, march);
    assert!(
        items[0].contains(&format!("the old system's total is {} (old claims system, run 41)", held - 1)),
        "{items:?}"
    );
    b.console("lena", record(held, "old claims system, run 42")).unwrap();
    assert_eq!(status(&b, march, check), CheckStatus::Green);
    assert!(format!("{:?}", b.applied.last().unwrap().events).contains("CutoverTotalRecorded"));
    let CheckStatus::Red { items } = status(&b, period(2028, 4), check) else {
        panic!("a load after cutover was not noticed")
    };
    assert_eq!(items.len(), 1, "{items:?}");
    assert!(
        items[0].starts_with(&format!("1430 Claims migration control had {} posted through it ", 10_00 * FINE_PER_MINOR))
            && items[0].contains("loaded in 2028-03"),
        "{}",
        items[0]
    );
    assert_eq!(status(&b, period(2028, 5), check), CheckStatus::Green);
    // The reinsurance account has never moved: nothing to say about it.
    assert!(b.state.blockers(period(2028, 5)).iter().all(|c| c.id != check));

    let mut config = close_config();
    config.version += 1;
    config.migration_control.push("1499".to_string());
    b.activate_close(config);
    let CheckStatus::Red { items } = status(&b, period(2028, 5), check) else {
        panic!("an account that is not in the chart was ignored")
    };
    assert_eq!(items, ["migration control account 1499 is not in the chart"]);
    // Once the month it was loaded in is closed, the agreement stands.
    b.state.periods.get_mut(&march).unwrap().closed = Some(ic_finance::close::ClosedPeriod {
        final_seq: 0,
        final_head: Hash32::ZERO,
        trial_balance: vec![],
        trial_balance_hash: Hash32::ZERO,
        by: None,
        index: 0,
    });
    let refused = b.console("lena", record(held + 1, "late")).unwrap_err();
    assert!(refused.message.contains("loaded in 2028-03, which is closed"), "{refused:?}");
}

/// Purpose: the core activates version 3 of a rate table on 28 March and version 4 on 10
/// April, and nobody records a validation. The March close shows version 3 as activated
/// unchecked, April shows both, and a validation recorded for other content of version 3
/// clears nothing. When the validation of version 3's own content arrives in May, version
/// 3 drops out of every period and version 4 stays. A validation recorded before its
/// table is activated counts once the table goes live. The shipped close lists the check
/// as blocking.
/// Why it matters: the core records that a table was validated and does not insist on it,
/// so the close is the only place a table that went live unchecked is seen. It has to
/// match on the content hash, or a report run on one draft would vouch for another.
#[test]
fn a_rate_table_activated_without_a_validation_shows_at_the_close() {
    let activated = |version: u32, hash: &str| Event::RateTableActivated {
        id: "life_base".to_string(),
        version,
        content_hash: hash.to_string(),
        scoped: false,
    };
    let validated = |version: u32, hash: &str| Event::RateTableValidated {
        id: "life_base".to_string(),
        version,
        content_hash: hash.to_string(),
        report_hash: "report".to_string(),
        schema_version: 1,
        by: "pipeline".to_string(),
    };
    let mut s = Stream::open(d(2028, 3, 28));
    s.push(activated(3, "aaa"));
    while s.date < d(2028, 4, 10) {
        s.close();
    }
    s.push(activated(4, "bbb"));
    s.push(validated(3, "other"));
    s.push(validated(5, "ccc"));
    let mut b = Books::ready(1);
    b.feed(&s.events).unwrap();

    let check = "rate_table_validation";
    let red = |b: &Books, p| match status(b, p, check) {
        CheckStatus::Red { items } => items,
        other => panic!("{check} is {other:?} for {p}"),
    };
    let march = red(&b, period(2028, 3));
    assert_eq!(
        march,
        ["rate table life_base version 3 (aaa) was activated on 2028-03-28 with no validation recorded for that content"]
    );
    assert_eq!(red(&b, period(2028, 4)).len(), 2);
    let declared = close_config().checks.into_iter().find(|c| c.id == check).unwrap();
    assert!(declared.blocking);
    assert!(b.state.blockers(period(2028, 4)).iter().any(|c| c.id == check));

    let from = s.events.len();
    while s.date < d(2028, 5, 3) {
        s.close();
    }
    s.push(validated(3, "aaa"));
    s.push(activated(5, "ccc"));
    b.feed(&s.events[from..]).unwrap();
    assert_eq!(status(&b, period(2028, 3), check), CheckStatus::Green);
    let may = red(&b, period(2028, 5));
    assert_eq!(may.len(), 1, "{may:?}");
    assert!(may[0].starts_with("rate table life_base version 4 (bbb)"), "{}", may[0]);
}

/// Purpose: in March the core bills health premiums with $15.00 of GST and a $2.00 levy,
/// each under its own posting type, and withholds $30.00 of tax from an income benefit
/// under `withholding_benefit`, which reconciles to withholding tax payable. Once March is
/// closed, $30.00 of benefit withholding is due to Inland Revenue on 20 April; December's
/// falls due on the working day after 15 January; an open month or an unknown kind is
/// refused. In April something credits GST payable under the
/// posting type `premium`, and the levy account moves with no posting type at all.
/// March's reconciliation shows each account's movement by posting type and product,
/// nothing unexplained and the check green; April's names both stray postings and the
/// check is red and blocks. A tax account missing from
/// the chart is reported, a declaration with no posting types is refused, and the
/// adjustment period has nothing to reconcile.
/// Why it matters: tax accounts are paid over to someone else on a return. A posting that
/// reaches one by a route nobody expected is either tax not collected from the customer
/// or money owed that no return will include, and it is found now or by the authority.
#[test]
fn tax_accounts_reconcile_to_the_posting_types_that_may_move_them() {
    let typed = |role: &str, class: AccountClass, minor: i128, posting_type: &str| {
        let mut l = line(role, class, AccountUnit::Minor(NZD), minor, Some("SF2-health"));
        l.dims.posting_type = Some(posting_type.to_string());
        l.dims.product = Some("health".to_string());
        l
    };
    let mut s = Stream::open(d(2028, 3, 28));
    s.entry(vec![
        typed("premium_receivable", AccountClass::Asset, 117_00, "premium"),
        typed("unearned_premium", AccountClass::Liability, -100_00, "premium"),
        typed("gst_payable", AccountClass::Liability, -15_00, "gst"),
        typed("levy_payable", AccountClass::Liability, -2_00, "levy"),
    ]);
    // An income benefit paid with tax withheld from it.
    s.entry(vec![
        typed("benefit_payable", AccountClass::Liability, 30_00, "withholding_benefit"),
        typed("withholding_tax_payable", AccountClass::Liability, -30_00, "withholding_benefit"),
    ]);
    while s.date < d(2028, 4, 10) {
        s.close();
    }
    s.entry(vec![
        typed("premium_receivable", AccountClass::Asset, 3_00, "premium"),
        typed("gst_payable", AccountClass::Liability, -3_00, "premium"),
    ]);
    s.entry(vec![
        line("premium_receivable", AccountClass::Asset, AccountUnit::Minor(NZD), 1_00, Some("SF2-health")),
        line("levy_payable", AccountClass::Liability, AccountUnit::Minor(NZD), -1_00, Some("SF2-health")),
    ]);
    while s.date < d(2028, 5, 2) {
        s.close();
    }
    let mut b = Books::ready(1);
    b.feed(&s.events).unwrap();
    let (mar, apr) = (period(2028, 3), period(2028, 4));

    let check = "tax_posting_types";
    assert_eq!(status(&b, mar, check), CheckStatus::Green);
    let march = ic_finance::host::answer(&b.state, serde_json::json!({"q": "tax_reconciliation", "period": "2028-03"})).unwrap();
    let gst = &march["accounts"][0];
    assert_eq!((gst["account"].as_str(), gst["name"].as_str()), (Some("2200"), Some("GST payable")));
    assert_eq!(gst["movement"], serde_json::json!(-15_00 * FINE_PER_MINOR));
    let lines: Vec<(Option<&str>, Option<&str>, i128)> = gst["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            (
                l["posting_type"].as_str(),
                l["product"].as_str(),
                serde_json::from_value(l["movement"].clone()).unwrap(),
            )
        })
        .collect();
    assert_eq!(lines, [(Some("gst"), Some("health"), -15_00 * FINE_PER_MINOR)]);
    assert_eq!(march["accounts"][1]["movement"], serde_json::json!(-2_00 * FINE_PER_MINOR));
    let withholding = &march["accounts"][2];
    assert_eq!(withholding["account"].as_str(), Some("2220"));
    assert_eq!(withholding["movement"], serde_json::json!(-30_00 * FINE_PER_MINOR));
    assert_eq!(withholding["lines"][0]["posting_type"].as_str(), Some("withholding_benefit"));
    assert!(march["unexplained"].as_array().unwrap().is_empty());

    let CheckStatus::Red { items } = status(&b, apr, check) else {
        panic!("a stray posting to GST payable was not noticed")
    };
    assert_eq!(items.len(), 2, "{items:?}");
    assert!(
        items[0].starts_with(&format!("2200 GST payable moved by {} ", -3_00 * FINE_PER_MINOR)) && items[0].contains("under posting type premium"),
        "{}",
        items[0]
    );
    assert!(
        items[1].starts_with("2210 Levies payable") && items[1].contains("under no posting type"),
        "{}",
        items[1]
    );
    assert!(b.state.blockers(apr).iter().any(|c| c.id == check));
    assert!(b.state.tax_reconciliation(apr.adjustment()).accounts.is_empty());

    // What is owed to Inland Revenue is asked of a closed month only.
    let due = |b: &Books, kind: &str, p: &str| ic_finance::host::answer(&b.state, serde_json::json!({"q": "withholding_due", "kind": kind, "period": p}));
    assert!(due(&b, "benefit", "2028-03").unwrap_err().contains("not closed"));
    let closed = || ic_finance::close::ClosedPeriod {
        final_seq: 0,
        final_head: Hash32::ZERO,
        trial_balance: vec![],
        trial_balance_hash: Hash32::ZERO,
        by: None,
        index: 0,
    };
    b.state.periods.get_mut(&mar).unwrap().closed = Some(closed());
    let owed = due(&b, "benefit", "2028-03").unwrap();
    assert_eq!(
        (owed["account"].as_str(), owed["amount_minor"].as_i64(), owed["due"].as_str()),
        (Some("2220"), Some(30_00), Some("2028-04-20"))
    );
    assert_eq!(due(&b, "rwt", "2028-03").unwrap()["amount_minor"], 0);
    assert!(due(&b, "lottery", "2028-03").unwrap_err().contains("no tax account"));
    // December's falls on 15 January, a Saturday in 2028, so the Monday after.
    let dec = period(2027, 12);
    let march_state = b.state.periods[&mar].clone();
    b.state.periods.entry(dec).or_insert(march_state).closed = Some(closed());
    assert_eq!(due(&b, "benefit", "2027-12").unwrap()["due"], "2028-01-17");

    let mut config = close_config();
    config.version += 1;
    config.tax[1].account = "2299".to_string();
    b.activate_close(config.clone());
    assert_eq!(b.state.tax_reconciliation(mar).unexplained, ["tax account 2299 is not in the chart"]);
    config.tax[0].posting_types.clear();
    assert!(config.validate().is_err());
}

/// Purpose: with the financial year ending in March, a term life policy incepting on 28
/// March 2028 is placed in `individual-life/FY2028/other` on the day it is issued, a
/// health policy incepting on 5 April in `health/FY2029/other`, and a term life policy
/// issued in April but backdated to 1 March in FY2028, because the cohort follows
/// inception. A pet policy no rule names waits, and the close check lists it and is red.
/// A new close configuration that names pet policies and also moves term life to
/// another portfolio is approved; classifying then places the pet policy under the new
/// rule and leaves every policy already placed where it was. Only an operator or the
/// lead may classify, there is nothing to classify by before a close configuration is
/// in force, and a rule naming a product twice or an unknown profitability band is
/// refused.
/// Why it matters: an accounting group is fixed for the life of the contracts in it.
/// If a later rule change moved policies already placed, every group's measurement since
/// inception would silently change; if a policy could stay ungrouped, it would be left
/// out of every group's figures.
#[test]
fn a_policy_is_placed_in_its_accounting_group_at_issue_and_never_moved() {
    let issued = |policy: &str, product: &str, inception: NaiveDate| Event::PolicyIssued {
        policy: policy.into(),
        number: policy.to_string(),
        group: "grp".into(),
        policyholder: "party-x".into(),
        product_code: product.to_string(),
        product_hash: "h".to_string(),
        inception,
        lives: vec![],
        layers: vec![],
        members: vec![],
    };
    let mut s = Stream::open(d(2028, 3, 28));
    s.push(issued("pol-1", "term-life", d(2028, 3, 28)));
    s.push(issued("pol-2", "pet-plan", d(2028, 3, 28)));
    while s.date < d(2028, 4, 5) {
        s.close();
    }
    s.push(issued("pol-3", "health-plan", d(2028, 4, 5)));
    s.push(issued("pol-4", "term-life", d(2028, 3, 1)));
    while s.date < d(2028, 5, 2) {
        s.close();
    }
    let mut b = Books::ready(1);
    b.feed(&s.events).unwrap();
    let group = |b: &Books, policy: &str| {
        let g = ic_finance::host::answer(&b.state, serde_json::json!({"q": "accounting_group", "partition": 0, "policy": policy})).unwrap();
        (
            g["portfolio"].as_str().map(str::to_string),
            g["cohort"].as_str().map(str::to_string),
            g["rule_version"].as_u64(),
            g["assigned_on"].as_str().map(str::to_string),
        )
    };
    let placed =
        |portfolio: &str, cohort: &str, version: u64, on: &str| (Some(portfolio.to_string()), Some(cohort.to_string()), Some(version), Some(on.to_string()));
    assert_eq!(group(&b, "pol-1"), placed("individual-life", "FY2028", 1, "2028-03-28"));
    assert_eq!(group(&b, "pol-3"), placed("health", "FY2029", 1, "2028-04-05"));
    assert_eq!(group(&b, "pol-4"), placed("individual-life", "FY2028", 1, "2028-04-05"));
    assert_eq!(group(&b, "pol-2"), (None, None, None, None));
    let check = "accounting_groups";
    for p in [period(2028, 3), period(2028, 4)] {
        match status(&b, p, check) {
            CheckStatus::Red { items } => {
                assert_eq!(items.len(), 1);
                assert!(
                    items[0].contains("pol-2") && items[0].contains("pet-plan") && items[0].contains("2028-03-28"),
                    "{items:?}"
                );
            }
            other => panic!("{other:?}"),
        }
    }
    let counts = ic_finance::host::answer(&b.state, serde_json::json!({"q": "accounting_groups"})).unwrap();
    assert_eq!(
        counts,
        serde_json::json!({"groups": [{"group": "health/FY2029/other", "policies": 1}, {"group": "individual-life/FY2028/other", "policies": 2}], "ungrouped": 1})
    );

    let mut next = close_config();
    next.version = 2;
    next.content_hash = "v2".to_string();
    next.grouping.retain(|g| g.product != "term-life");
    for (product, portfolio) in [("pet-plan", "pet"), ("term-life", "moved")] {
        next.grouping.push(ic_finance::close::GroupRule {
            product: product.to_string(),
            portfolio: portfolio.to_string(),
            profitability: "onerous".to_string(),
        });
    }
    b.activate_close(next.clone());
    assert_eq!(b.console_code("ana", FinCommand::ClassifyPolicies), "authority");
    b.console("olive", FinCommand::ClassifyPolicies).unwrap();
    assert_eq!(group(&b, "pol-2"), placed("pet", "FY2028", 2, "2028-05-01"));
    assert_eq!(group(&b, "pol-1"), placed("individual-life", "FY2028", 1, "2028-03-28"));
    assert_eq!(status(&b, period(2028, 3), check), CheckStatus::Green);
    let last = b.applied.last().unwrap();
    assert!(
        format!("{:?}", last.events).contains("PoliciesClassified { placed: 1, still_ungrouped: 0, rule_version: 2 }"),
        "{:?}",
        last.events
    );

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
    assert_eq!(bare.console_code("olive", FinCommand::ClassifyPolicies), "close_config");
    for bad in ["sometimes", ""] {
        let mut wrong = next.clone();
        wrong.grouping[0].profitability = bad.to_string();
        assert!(wrong.validate().is_err());
    }
    let mut twice = next.clone();
    twice.grouping.push(next.grouping[0].clone());
    assert!(twice.validate().unwrap_err().to_string().contains("declared twice"));
}
