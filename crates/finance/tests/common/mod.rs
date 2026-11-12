//! Shared harness: a finance ledger fed from a simulated core.
#![allow(dead_code)]

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_core::event::EventRecord;
use ic_core::{Command, PolicyId};
use ic_finance::books::ExtractRecord;
use ic_finance::books::{FinApplied, JournalDraft, JournalKind, JournalLine, ROLE_ACTUARY, ROLE_APPROVER, ROLE_LEAD, ROLE_OPERATOR};
use ic_finance::close::{Capacity, CheckSource, CheckStatus, Outcome};
use ic_finance::{Chart, CloseConfig, FinCommand, FinEnvelope, FinanceState, PeriodId, Reject};
use ic_ledger::{Hash32, PartitionId};
use ic_money::{Currency::NZD, Money};
use ic_product::{Frequency, Sex};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, term_life};
use ic_sim::Cluster;

pub const CHART: &str = include_str!("../../../../config/finance/chart.star");
pub const CLOSE: &str = include_str!("../../../../config/finance/close.star");
pub const MONTHLY: i64 = 61_84;

pub fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

pub fn period(y: i32, m: u32) -> PeriodId {
    PeriodId::month(y, m).unwrap()
}

pub fn chart() -> Chart {
    Chart::load("chart.star", CHART).unwrap()
}

pub fn close_config() -> CloseConfig {
    CloseConfig::load("close.star", CLOSE).unwrap()
}

/// The people of the finance team, each with the roles the gateway would verify.
pub fn roles(actor: &str) -> Vec<String> {
    let held: &[&str] = match actor {
        "olive" | "oscar" => &[ROLE_OPERATOR],
        "ana" => &[ROLE_APPROVER],
        "lena" | "liam" => &[ROLE_LEAD],
        "aroha" => &[ROLE_ACTUARY],
        // Holds two roles, to show that separation is by person, not by role.
        "kiri" => &[ROLE_OPERATOR, ROLE_ACTUARY],
        _ => &[],
    };
    held.iter().map(|r| r.to_string()).collect()
}

/// A finance ledger and the log index it has reached.
pub struct Books {
    pub state: FinanceState,
    pub index: u64,
    pub applied: Vec<FinApplied>,
    /// Every envelope applied, in order: the log.
    pub log: Vec<FinEnvelope>,
}

impl Books {
    pub fn empty() -> Self {
        Self {
            state: FinanceState::new(),
            index: 0,
            applied: vec![],
            log: vec![],
        }
    }

    /// Configured for `partitions` with the shipped chart and close configuration active.
    pub fn ready(partitions: PartitionId) -> Self {
        let mut b = Self::empty();
        b.send("operator", None, FinCommand::Configure { partitions, year_end_month: 3 }).unwrap();
        b.activate_chart(chart());
        b.activate_close(close_config());
        b
    }

    pub fn activate_chart(&mut self, chart: Chart) {
        let id = format!("chart:{}:{}", chart.version, chart.content_hash);
        self.console("olive", FinCommand::ProposeChart { chart }).unwrap();
        self.console("lena", FinCommand::ApproveConfig { id }).unwrap();
    }

    pub fn activate_close(&mut self, config: CloseConfig) {
        let id = format!("close:{}:{}", config.version, config.content_hash);
        self.console("olive", FinCommand::ProposeCloseConfig { config }).unwrap();
        self.console("lena", FinCommand::ApproveConfig { id }).unwrap();
    }

    pub fn apply(&mut self, env: FinEnvelope) -> FinApplied {
        self.index += 1;
        self.log.push(env.clone());
        let applied = self.state.apply_envelope(self.index, env);
        self.applied.push(applied.clone());
        applied
    }

    pub fn send_keyed(&mut self, key: &str, caller: &str, actor: Option<&str>, command: FinCommand) -> FinApplied {
        self.apply(FinEnvelope {
            key: key.to_string(),
            caller: caller.to_string(),
            actor: actor.map(str::to_string),
            roles: actor.map(roles).unwrap_or_default(),
            command,
        })
    }

    pub fn send(&mut self, caller: &str, actor: Option<&str>, command: FinCommand) -> Result<(), Reject> {
        let key = format!("k{}", self.index + 1);
        self.send_keyed(&key, caller, actor, command).result
    }

    /// A command a person sends from the finance console.
    pub fn console(&mut self, actor: &str, command: FinCommand) -> Result<(), Reject> {
        self.send("finance-console", Some(actor), command)
    }

    /// Feed core events, each partition's in order.
    pub fn feed(&mut self, events: &[EventRecord]) -> Result<(), Reject> {
        let mut by_partition: BTreeMap<PartitionId, Vec<EventRecord>> = BTreeMap::new();
        for e in events {
            by_partition.entry(e.partition).or_default().push(e.clone());
        }
        for (partition, events) in by_partition {
            for chunk in events.chunks(1_000) {
                self.send(
                    "feed",
                    None,
                    FinCommand::Ingest {
                        partition,
                        events: chunk.to_vec(),
                    },
                )?;
            }
        }
        Ok(())
    }

    pub fn code(&mut self, caller: &str, actor: Option<&str>, command: FinCommand) -> String {
        self.send(caller, actor, command).unwrap_err().code
    }

    pub fn console_code(&mut self, actor: &str, command: FinCommand) -> String {
        self.console(actor, command).unwrap_err().code
    }
}

/// A simulated core and the finance ledger it feeds.
pub struct World {
    pub core: Cluster,
    pub books: Books,
    /// The Merkle root the core published for each closed date.
    pub roots: BTreeMap<NaiveDate, Hash32>,
    /// Every event the core has published, in the order it was taken.
    pub events: Vec<EventRecord>,
}

impl World {
    /// A core of `partitions` opened on `start` with the shipped products.
    pub fn new(partitions: PartitionId, start: NaiveDate) -> Self {
        let mut core = Cluster::in_memory(partitions, 11);
        core.open(start).unwrap();
        for cmd in shipped_activations().unwrap() {
            core.activate_everywhere(cmd).unwrap();
        }
        Self {
            core,
            books: Books::ready(partitions),
            roots: BTreeMap::new(),
            events: Vec::new(),
        }
    }

    /// Issue the `n`th term life policy on `inception` and return where it lives.
    pub fn issue(&mut self, n: u64, inception: NaiveDate) -> (PartitionId, PolicyId) {
        let issue = term_life(n, inception, d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
        let (p, id) = (self.core.partition_of(&issue.group), issue.policy.clone());
        self.core.submit(p, "new-business", issue_command(issue)).unwrap();
        (p, id)
    }

    pub fn pay(&mut self, p: PartitionId, policy: &PolicyId, receipt: &str, minor: i64) {
        let cmd = Command::ApplyReceipt {
            policy: policy.clone(),
            receipt: receipt.to_string(),
            amount: Money::new(minor, NZD),
        };
        self.core.submit(p, "cash-matching", cmd).unwrap();
    }

    /// Close `n` business days, feeding every day's events and Merkle root to the books.
    pub fn days(&mut self, n: u32) {
        for _ in 0..n {
            let close = self.core.end_of_day().unwrap();
            assert!(close.breaks.is_empty(), "core breaks on {}: {:?}", close.closed, close.breaks);
            self.roots.insert(close.closed, close.merkle_root);
            let events = self.core.take_events();
            self.books.feed(&events).unwrap();
            self.events.extend(events);
            self.books
                .send(
                    "feed",
                    None,
                    FinCommand::RecordMerkleRoot {
                        date: close.closed,
                        root: close.merkle_root,
                        reference: format!("worm://roots/{}", close.closed),
                    },
                )
                .unwrap();
        }
    }

    pub fn extract(&self, kind: &str, p: PeriodId) -> ExtractRecord {
        ExtractRecord {
            id: format!("{kind}-{p}"),
            kind: kind.to_string(),
            as_at: p.end(),
            content_hash: Hash32([7; 32]),
            rows: 1,
            merkle_root: self.roots[&p.end()],
            definition_version: 1,
            location: format!("worm://extracts/{kind}-{p}"),
            control: BTreeMap::new(),
            reconciled: true,
        }
    }

    /// Everything other modules and people owe a period's close, all clean:
    /// the reported checks, the aged item reports and the two extracts.
    pub fn make_green(&mut self, p: PeriodId) {
        let config = self.books.state.close_config.clone().unwrap();
        for check in &config.checks {
            if let CheckSource::Reported { by } = &check.source {
                let actor = (by == "finance-console").then_some("olive");
                let report = FinCommand::ReportCheck {
                    period: p,
                    check: check.id.clone(),
                    outcome: Outcome::Green,
                    evidence: Some(format!("vault://close/{p}/{}", check.id)),
                };
                self.books.send(by, actor, report).unwrap();
            }
        }
        for aged in &config.aged {
            // A later report already covers this period.
            if self.books.state.aged.get(&aged.kind).is_some_and(|r| r.as_at > p.end()) {
                continue;
            }
            let report = FinCommand::ReportAgedItems {
                kind: aged.kind.clone(),
                as_at: p.end(),
                items: vec![],
                total_minor: 0,
                count: 0,
            };
            self.books.send(&aged.by, None, report).unwrap();
        }
        for kind in ["in_force", "movement"] {
            let extract = self.extract(kind, p);
            self.books.send("reporting", None, FinCommand::RegisterExtract { extract }).unwrap();
        }
    }

    /// Take a period that has ended through its whole close.
    pub fn close(&mut self, p: PeriodId) {
        self.make_green(p);
        if p.is_quarter_end(3) {
            let mut actuarial = journal(&format!("act-{p}"), JournalKind::Actuarial, p, "6000", "2400", 100_00);
            actuarial.extract_id = Some(format!("in_force-{p}"));
            self.books.console("olive", FinCommand::ProposeJournal { journal: actuarial }).unwrap();
            self.books.console("ana", FinCommand::ApproveJournal { id: format!("act-{p}") }).unwrap();
            self.books
                .console("aroha", FinCommand::SignActuarialJournal { id: format!("act-{p}") })
                .unwrap();
        }
        // Until the core tags cash postings with their statutory fund, the
        // funds do not balance on their own and the finance lead waives it.
        self.waive_if_red(p, "fund_balance");
        let blockers = self.books.state.blockers(p);
        assert!(blockers.is_empty(), "{p} is blocked: {blockers:?}");
        let sign = |capacity| FinCommand::SignOff { period: p, capacity };
        self.books.console("lena", sign(Capacity::FinanceLead)).unwrap();
        if p.is_quarter_end(3) {
            self.books.console("aroha", sign(Capacity::AppointedActuary)).unwrap();
        }
        self.books.send("close-workflow", None, FinCommand::ClosePeriod { period: p }).unwrap();
    }

    pub fn waive_if_red(&mut self, p: PeriodId, check: &str) {
        let red = self
            .books
            .state
            .checks(p)
            .iter()
            .any(|c| c.id == check && matches!(c.status, CheckStatus::Red { .. }));
        if red {
            let waive = FinCommand::WaiveCheck {
                period: p,
                check: check.to_string(),
                reason: "known gap in the core's fund tagging".to_string(),
                remediation_owner: "oscar".to_string(),
                due: p.end() + chrono::Days::new(30),
            };
            self.books.console("lena", waive).unwrap();
        }
    }

    /// Close days until the core's business date is `date`.
    pub fn until(&mut self, date: NaiveDate) {
        while self.core.business_date().unwrap() < date {
            self.days(1);
        }
    }
}

/// A balanced two-line journal in NZD within one fund.
pub fn journal(id: &str, kind: JournalKind, period: PeriodId, debit: &str, credit: &str, minor: i64) -> JournalDraft {
    let line = |account: &str, signed: i128| JournalLine {
        account: account.to_string(),
        currency: NZD,
        signed_fine: signed * ic_money::FINE_PER_MINOR,
        fund: Some("SF1-life".to_string()),
        product: None,
        cover_type: None,
        posting_type: None,
    };
    JournalDraft {
        id: id.to_string(),
        kind,
        period,
        value_date: period.end(),
        lines: vec![line(debit, i128::from(minor)), line(credit, -i128::from(minor))],
        reason: "month-end accrual".to_string(),
        evidence: vec!["vault://evidence/1".to_string()],
        extract_id: None,
    }
}
