//! A small insurer for the tests: the core in a box, the distribution module
//! beside it, and the driver that carries events one way and postings the other.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use ic_core::counterparty::{CounterpartyAccount, CounterpartyLine, CounterpartyPosting, InternalLedger, PartyLedger};
use ic_core::{Command as CoreCommand, PartyId, PolicyId, Reject};
use ic_distribution::authority::{shipped_templates, Role, Staff};
use ic_distribution::config::Change;
use ic_distribution::model::{ClawbackStep, InitialEarned, Schedule, ScheduleOption};
use ic_distribution::rules::{CommissionType, Component, LiabilityLine, Line, Rule, Share, WHOLE};
use ic_distribution::state::{Settings, WriteLayer};
use ic_distribution::structure::{Engagement, LicenceState, PartyKind};
use ic_distribution::workbench::Action;
use ic_distribution::{Actor, BookId, Command, DistributionState, EngagementId, Envelope, Event};
use ic_money::{Currency::NZD, Money};
use ic_product::{Frequency, Sex};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, term_life};
use ic_sim::Cluster;

pub fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

pub fn party(s: &str) -> PartyId {
    PartyId::from(s)
}

pub fn pct(p: f64) -> u32 {
    (p * 10_000.0).round() as u32
}

/// A person holding the shipped templates for these roles, assigned to the whole queue.
pub fn person(id: &str, roles: &[Role]) -> Staff {
    let templates = shipped_templates().into_iter().filter(|t| t.roles.iter().any(|r| roles.contains(r))).collect();
    Staff {
        id: party(id),
        templates,
        grants: vec![],
        paths: BTreeSet::from(["*".to_string()]),
        purpose: "case-1".into(),
    }
}

pub fn staff(id: &str, roles: &[Role]) -> Actor {
    Actor::Staff(Box::new(person(id, roles)))
}

pub fn admin() -> Actor {
    staff("staff-admin", &[Role::DistributionAdmin])
}
pub fn ops() -> Actor {
    staff("staff-ops", &[Role::CommissionOps])
}
pub fn approver() -> Actor {
    staff("staff-approver", &[Role::CommissionApprover])
}
pub fn compliance() -> Actor {
    staff("staff-compliance", &[Role::Compliance])
}
pub fn finance() -> Actor {
    staff("staff-finance", &[Role::CounterpartyFinance])
}
pub fn finance_approver() -> Actor {
    staff("staff-fin-approver", &[Role::CounterpartyFinanceApprover])
}
pub fn finance_approver_2() -> Actor {
    staff("staff-fin-approver-2", &[Role::CounterpartyFinanceApprover])
}
pub fn legal() -> Actor {
    staff("staff-legal", &[Role::Legal])
}
pub fn conduct() -> Actor {
    staff("staff-conduct", &[Role::ConductOfficer])
}
pub fn module(name: &str) -> Actor {
    Actor::Module(name.to_string())
}

pub struct World {
    pub cluster: Cluster,
    pub dist: DistributionState,
    index: u64,
    key: u64,
    /// Every event the module has emitted, in order.
    pub events: Vec<Event>,
    /// Postings the core refused, by reference and code.
    pub refused: Vec<(String, String)>,
    /// When false, postings stay in the outbox until `deliver` is called.
    pub auto_deliver: bool,
    pub outbox: Vec<(u16, ic_core::counterparty::CounterpartyPosting)>,
    /// Every envelope applied, in order: what a replica would replay.
    pub log: Vec<(u64, Envelope)>,
}

pub const START: (i32, u32, u32) = (2027, 3, 1);

impl World {
    pub fn new() -> Self {
        Self::with(1, Settings::default())
    }

    pub fn with(partitions: u16, settings: Settings) -> Self {
        let mut cluster = Cluster::in_memory(partitions, 7);
        cluster.open(d(START.0, START.1, START.2)).unwrap();
        for cmd in shipped_activations().unwrap() {
            cluster.activate_everywhere(cmd).unwrap();
        }
        let mut w = Self {
            cluster,
            dist: DistributionState::new(settings),
            index: 0,
            key: 0,
            events: vec![],
            refused: vec![],
            auto_deliver: true,
            outbox: vec![],
            log: vec![],
        };
        w.sync();
        w
    }

    pub fn today(&self) -> NaiveDate {
        self.cluster.business_date().unwrap()
    }

    /// Send one command to the module with a fresh key.
    pub fn send(&mut self, actor: Actor, command: Command) -> Result<Vec<Event>, Reject> {
        self.key += 1;
        let key = format!("k{}", self.key);
        self.send_keyed(&key, actor, command)
    }

    pub fn send_keyed(&mut self, key: &str, actor: Actor, command: Command) -> Result<Vec<Event>, Reject> {
        self.index += 1;
        let envelope = Envelope {
            key: key.to_string(),
            actor,
            command,
        };
        self.log.push((self.index, envelope.clone()));
        let out = self.dist.apply_envelope(self.index, envelope);
        self.events.extend(out.events.iter().cloned());
        self.outbox.extend(out.postings);
        out.result.map(|()| out.events)
    }

    /// Send what the module asked for to the core, with its reference as the key.
    pub fn deliver(&mut self) {
        for (partition, posting) in std::mem::take(&mut self.outbox) {
            let reference = posting.reference.clone();
            let applied = self
                .cluster
                .submit_keyed(partition, &reference, "distribution", CoreCommand::PostCounterparty(Box::new(posting)));
            if let Err(e) = applied.result {
                self.refused.push((reference.clone(), e.code.clone()));
                self.send(
                    module("event-relay"),
                    Command::PostingRefused {
                        reference,
                        code: e.code,
                        message: e.message,
                    },
                )
                .unwrap();
            }
        }
    }

    /// Carry events to the module and postings to the core until both are quiet.
    pub fn sync(&mut self) {
        loop {
            let events = self.cluster.take_events();
            if events.is_empty() && (self.outbox.is_empty() || !self.auto_deliver) {
                break;
            }
            for e in events {
                self.send(module("event-relay"), Command::Ingest(Box::new(e))).unwrap();
            }
            if self.auto_deliver {
                self.deliver();
            }
        }
    }

    pub fn days(&mut self, n: u32) {
        for _ in 0..n {
            let close = self.cluster.end_of_day().unwrap();
            assert!(close.breaks.is_empty(), "{:?}", close.breaks);
            self.sync();
        }
    }

    pub fn run_until(&mut self, date: NaiveDate) {
        while self.today() < date {
            self.days(1);
        }
    }

    /// Issue a monthly term life policy and return its ID and partition.
    pub fn issue(&mut self, n: u64, life_cover_minor: i64, trauma: Option<i64>) -> (PolicyId, u16) {
        let today = self.today();
        let issue = term_life(n, today, d(1990, 3, 10), Sex::Female, false, life_cover_minor, trauma, Frequency::Monthly, None);
        let id = issue.policy.clone();
        let partition = self.cluster.partition_of(&issue.group);
        self.cluster.submit(partition, "new-business", issue_command(issue)).unwrap();
        self.sync();
        (id, partition)
    }

    /// A term life policy on a named person, so that two policies can be on
    /// the same life.
    pub fn issue_for(&mut self, n: u64, person: &str, life_cover_minor: i64) -> PolicyId {
        let today = self.today();
        let dob = d(1990, 3, 10);
        let mut issue = term_life(n, today, dob, Sex::Female, false, life_cover_minor, None, Frequency::Monthly, None);
        issue.lives = vec![ic_sim::persona::named_life("life-1", person, dob, Sex::Female, false)];
        let id = issue.policy.clone();
        let partition = self.cluster.partition_of(&issue.group);
        self.cluster.submit(partition, "new-business", issue_command(issue)).unwrap();
        self.sync();
        id
    }

    pub fn owing(&self, policy: &PolicyId) -> i64 {
        let p = self.cluster.locate(policy).unwrap();
        self.cluster.state(p).policies[policy].outstanding_minor()
    }

    /// Pay exactly what the policy owes.
    pub fn pay(&mut self, policy: &PolicyId, receipt: &str) -> i64 {
        let amount = self.owing(policy);
        self.pay_amount(policy, receipt, amount);
        amount
    }

    pub fn pay_amount(&mut self, policy: &PolicyId, receipt: &str, amount: i64) {
        let p = self.cluster.locate(policy).unwrap();
        self.cluster
            .submit(
                p,
                "cash-matching",
                CoreCommand::ApplyReceipt {
                    policy: policy.clone(),
                    receipt: receipt.into(),
                    amount: Money::new(amount, NZD),
                },
            )
            .unwrap();
        self.sync();
    }

    pub fn core(&mut self, policy: &PolicyId, caller: &str, command: CoreCommand) -> Result<(), Reject> {
        let p = self.cluster.locate(policy).unwrap();
        let r = self.cluster.submit(p, caller, command);
        self.sync();
        r
    }

    /// The party's balance on an account as the core's ledgers hold it,
    /// summed across partitions. Positive is in the party's favour.
    pub fn ledger(&self, who: &str, ledger: PartyLedger) -> i64 {
        (0..self.cluster.partitions())
            .map(|p| {
                let account = ic_core::counterparty::party_account(&party(who), ledger, NZD);
                -self.cluster.state(p).ledger.balance(&account.id) as i64
            })
            .sum()
    }

    pub fn payable(&self, who: &str) -> i64 {
        self.ledger(who, PartyLedger::CommissionPayable)
    }
    pub fn held(&self, who: &str) -> i64 {
        self.ledger(who, PartyLedger::Held)
    }

    /// The reconciliations the design lists, checked against the core's ledgers.
    pub fn assert_reconciled(&self) {
        assert!(self.cluster.reconcile().is_empty(), "{:?}", self.cluster.reconcile());
        for who in self.dist.positions.keys() {
            for ledger in [
                PartyLedger::CommissionPayable,
                PartyLedger::Held,
                PartyLedger::Receivable,
                PartyLedger::Retention,
                PartyLedger::Advance,
                PartyLedger::Clearing,
            ] {
                assert_eq!(self.dist.balance(who, ledger), self.ledger(&who.0, ledger), "{who} {ledger:?}");
            }
        }
        assert!(self.dist.unconfirmed().is_empty(), "postings still unconfirmed");
        assert!(self.dist.held_control(self.today()).agrees_with_ledger);
    }

    pub fn propose(&mut self, id: &str, changes: Vec<Change>) -> Result<Vec<Event>, Reject> {
        self.send(
            ops(),
            Command::ProposeChange {
                id: id.into(),
                changes,
                instructions_ref: format!("doc-{id}"),
            },
        )
    }

    /// Propose a change and take it through both approvals.
    pub fn activate(&mut self, id: &str, changes: Vec<Change>) -> Result<(), Reject> {
        self.propose(id, changes)?;
        self.send(
            approver(),
            Command::ApproveChange {
                id: id.into(),
                compliance: false,
            },
        )?;
        self.send(
            compliance(),
            Command::ApproveChange {
                id: id.into(),
                compliance: true,
            },
        )?;
        self.sync();
        Ok(())
    }

    pub fn add_party(&mut self, id: &str, kind: PartyKind) {
        self.send(admin(), Command::AddParty { party: party(id), kind }).unwrap();
    }

    pub fn payee(&mut self, id: &str) {
        self.send(
            admin(),
            Command::RegisterPayee {
                party: party(id),
                agreement_ref: format!("agreement-{id}"),
                advance_recovery_ppm: pct(20.0),
            },
        )
        .unwrap();
    }

    pub fn account(&mut self, id: &str, account_ref: &str) {
        self.send(
            admin(),
            Command::AddPayeeAccount {
                party: party(id),
                account_ref: account_ref.into(),
                name_check_ref: "name-ok".into(),
                step_up_ref: "step-up".into(),
                appointment_ref: None,
            },
        )
        .unwrap();
    }

    pub fn engage(&mut self, id: &str, adviser: &str, with: &str, from: NaiveDate) {
        self.send(
            admin(),
            Command::StartEngagement {
                id: EngagementId::from(id),
                engagement: Engagement {
                    adviser: party(adviser),
                    with: party(with),
                    from,
                    to: None,
                    accreditations: BTreeSet::from(["term-life".to_string()]),
                },
            },
        )
        .unwrap();
    }

    pub fn write(&mut self, policy: &PolicyId, cover: &str, layer: u32, engagement: &str, book: &str, annual_premium_minor: i64) -> Result<Vec<Event>, Reject> {
        self.send(
            module("new-business"),
            Command::WriteLayer(Box::new(WriteLayer {
                policy: policy.clone(),
                cover: cover.into(),
                layer,
                writing: vec![(EngagementId::from(engagement), WHOLE)],
                books: vec![(BookId::from(book), WHOLE)],
                schedule: "standard".into(),
                option: "upfront".into(),
                annual_premium_minor,
                replacement: false,
            })),
        )
    }
}

impl World {
    pub fn status(&self, policy: &PolicyId) -> ic_core::PolicyStatus {
        let p = self.cluster.locate(policy).unwrap();
        self.cluster.state(p).policies[policy].status
    }

    /// Run days until the policy has the status, failing after a year.
    pub fn until_status(&mut self, policy: &PolicyId, status: ic_core::PolicyStatus) {
        for _ in 0..366 {
            if self.status(policy) == status {
                return;
            }
            self.days(1);
        }
        panic!("policy never became {status:?}");
    }

    pub fn licence(&mut self, who: &str, state: LicenceState, effective: NaiveDate) -> Result<Vec<Event>, Reject> {
        let r = self.send(
            compliance(),
            Command::RecordLicence {
                party: party(who),
                state,
                effective,
                source_ref: format!("notice-{who}-{effective}"),
            },
        );
        self.sync();
        r
    }

    /// Prepare a workbench action as counterparty finance.
    pub fn act(&mut self, id: &str, who: &str, action: Action) -> Result<Vec<Event>, Reject> {
        self.send(
            finance(),
            Command::ProposeAction {
                id: id.into(),
                party: party(who),
                action,
                reason_code: "reason".into(),
                document_ref: format!("doc-{id}"),
            },
        )
    }

    pub fn sign(&mut self, id: &str, actor: Actor, as_role: Role) -> Result<Vec<Event>, Reject> {
        let r = self.send(actor, Command::ApproveAction { id: id.into(), as_role });
        self.sync();
        r
    }

    pub fn cut(&mut self, who: &str) -> Result<Vec<Event>, Reject> {
        let r = self.send(ops(), Command::CutStatement { payee: party(who) });
        self.sync();
        r
    }

    pub fn settle(&mut self, reference: &str) {
        self.send(module("payments"), Command::PaymentSettled { reference: reference.into() }).unwrap();
        self.sync();
    }

    pub fn statement(&self, who: &str, number: u32) -> &ic_distribution::Statement {
        &self.dist.statements[&format!("{who}.{number}")]
    }

    /// Post straight to the core as another module would: a claims
    /// overpayment, a reinsurance recovery, an employer's arrears.
    pub fn post_as_finance(&mut self, partition: u16, reference: &str, lines: Vec<(CounterpartyAccount, i64)>) -> Result<(), Reject> {
        let posting = CounterpartyPosting {
            reference: reference.into(),
            kind: "external".into(),
            currency: NZD,
            value_date: None,
            policy: None,
            reverses: None,
            lines: lines
                .into_iter()
                .map(|(account, amount_minor)| CounterpartyLine { account, amount_minor })
                .collect(),
            detail: BTreeMap::new(),
        };
        let r = self
            .cluster
            .submit(partition, "counterparty-finance", CoreCommand::PostCounterparty(Box::new(posting)));
        self.sync();
        r
    }

    /// The net of an internal account across partitions; debit positive.
    pub fn internal(&self, ledger: InternalLedger) -> i64 {
        (0..self.cluster.partitions())
            .map(|p| {
                let account = ic_core::counterparty::internal_account(&ledger, NZD);
                self.cluster.state(p).ledger.balance(&account.id) as i64
            })
            .sum()
    }
}

/// An advised policy in the worked example with its first premium paid.
/// Returns the world, the policy and the renewal commission the premium earned.
pub fn advised_in(mut w: World) -> (World, PolicyId, i64) {
    let (policy, _) = w.issue(1, 500_000_00, None);
    w.write(&policy, "life", 1, "eng-a-one", "a-at-fap-one", 1_200_00).unwrap();
    w.pay(&policy, "r1");
    let renewal = w.payable("a-co") + w.payable("fap-one") + w.held("fap-one") - 1_200_00;
    (w, policy, renewal)
}

#[track_caller]
pub fn code<T: std::fmt::Debug>(r: Result<T, Reject>) -> String {
    r.unwrap_err().code
}

pub fn acct(who: &str, ledger: PartyLedger) -> CounterpartyAccount {
    CounterpartyAccount::Party { party: party(who), ledger }
}

pub fn schedule() -> Schedule {
    Schedule {
        id: "standard".into(),
        version: 1,
        effective_from: d(2020, 1, 1),
        options: BTreeMap::from([
            (
                "upfront".to_string(),
                ScheduleOption {
                    initial_ppm: pct(100.0),
                    renewal_ppm: pct(10.0),
                    renewal_from_month: 0,
                },
            ),
            (
                "level".to_string(),
                ScheduleOption {
                    initial_ppm: 0,
                    renewal_ppm: pct(30.0),
                    renewal_from_month: 0,
                },
            ),
        ]),
        clawback: vec![
            ClawbackStep { months_under: 12, ppm: WHOLE },
            ClawbackStep {
                months_under: 24,
                ppm: pct(50.0),
            },
        ],
        cooling_off_days: 14,
        initial_earned: InitialEarned::FirstPremium,
        initial_on_indexation: false,
        earns_on_waived_premium: true,
        replacement_carries_clawback: false,
    }
}

pub fn base(payee: &str, share: Share, absorbs: bool) -> Line {
    Line {
        payee: party(payee),
        earner: None,
        component: Component::Base,
        commission_type: None,
        share,
        from: d(2020, 1, 1),
        until: None,
        disputed: false,
        absorbs_rounding: absorbs,
    }
}

pub fn liable(who: &str) -> LiabilityLine {
    LiabilityLine {
        component: Component::Base,
        liable: party(who),
        share: WHOLE,
        guarantor: None,
        from: d(2020, 1, 1),
        until: None,
    }
}

pub fn rule(book: &str, version: u32, from: NaiveDate, lines: Vec<Line>, liability: Vec<LiabilityLine>) -> Rule {
    Rule {
        book: BookId::from(book),
        version,
        effective_from: from,
        lines,
        liability,
        fragments: vec![],
        clawback_by_cause: false,
        instructions_ref: "signed-instructions".into(),
    }
}

pub fn renewal_only(mut line: Line) -> Line {
    line.commission_type = Some(CommissionType::Renewal);
    line
}

/// The worked example's starting point: adviser A engaged by FAP One, whose
/// book pays 75% to A's company and 25% to FAP One, with FAP One bearing
/// clawback. FAP Two and its engagement exist for the move.
pub fn worked_example() -> World {
    worked_example_in(World::new())
}

pub fn worked_example_in(mut w: World) -> World {
    for (id, kind) in [
        ("adviser-a", PartyKind::Adviser),
        ("a-co", PartyKind::Company),
        ("fap-one", PartyKind::Fap),
        ("fap-two", PartyKind::Fap),
        ("group-old", PartyKind::Group),
    ] {
        w.add_party(id, kind);
    }
    for p in ["a-co", "fap-one", "fap-two", "group-old"] {
        w.payee(p);
        w.account(p, &format!("acct-{p}"));
    }
    w.engage("eng-a-one", "adviser-a", "fap-one", d(2020, 1, 1));
    w.activate(
        "setup",
        vec![
            Change::ActivateSchedule(schedule()),
            Change::CreateBook {
                id: BookId::from("a-at-fap-one"),
                name: "A at FAP One".into(),
                engagement: Some(EngagementId::from("eng-a-one")),
                owner: Some(party("fap-one")),
                house: false,
            },
            Change::ActivateRule(rule(
                "a-at-fap-one",
                1,
                d(START.0, START.1, START.2),
                vec![
                    base("a-co", Share::OfRemainder(pct(75.0)), true),
                    base("fap-one", Share::OfRemainder(pct(25.0)), false),
                ],
                vec![liable("fap-one")],
            )),
        ],
    )
    .unwrap();
    w
}
