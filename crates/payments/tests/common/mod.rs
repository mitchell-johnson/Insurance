//! A payments desk for tests: the state machine, a clock, a standard
//! configuration with two banks and three regions, and the people who work
//! there.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use ic_core::PolicyId;
use ic_log::StateMachine;
use ic_money::Currency;
use ic_payments::config::{BankAccount, Channel, ChannelKind, Endpoint, Route, Rules};
use ic_payments::model::{AccountDetails, Counterparty, NameCheck, NewItem, PayeeKind, Purpose, RunType, StatementLine};
use ic_payments::{Actor, Command, Config, Envelope, Event, Output, PaymentsState, Role};

pub const NZD: Currency = Currency::NZD;
pub const REGIONS: [&str; 3] = ["akl", "syd", "mel"];
pub const RUN_TYPES: [RunType; 12] = [
    RunType::Collection,
    RunType::LumpSumClaims,
    RunType::BenefitPayments,
    RunType::ProviderPayments,
    RunType::PremiumRefunds,
    RunType::Commission,
    RunType::Reinsurer,
    RunType::FundSettlement,
    RunType::TaxAndLevies,
    RunType::UnclaimedMoney,
    RunType::FundTransfer,
    RunType::ExGratia,
];

pub fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

pub fn t(y: i32, m: u32, day: u32, h: u32, min: u32) -> NaiveDateTime {
    d(y, m, day).and_hms_opt(h, min, 0).unwrap()
}

/// Monday 15 November 2027, nine in the morning.
pub fn monday() -> NaiveDateTime {
    t(2027, 11, 15, 9, 0)
}

pub fn person(id: &str, roles: &[Role]) -> Actor {
    Actor {
        id: id.to_string(),
        roles: roles.iter().copied().collect(),
    }
}

pub fn mia() -> Actor {
    person("mia", &[Role::Maker])
}
pub fn max() -> Actor {
    person("max", &[Role::Maker])
}
pub fn ari() -> Actor {
    person("ari", &[Role::Approver])
}
pub fn ben() -> Actor {
    person("ben", &[Role::Approver])
}
pub fn pam() -> Actor {
    person("pam", &[Role::PayeeMaintainer])
}
pub fn pat() -> Actor {
    person("pat", &[Role::PayeeMaintainer])
}
pub fn cal() -> Actor {
    person("cal", &[Role::ConnectivityAdmin])
}
pub fn lee() -> Actor {
    person("lee", &[Role::Legal])
}
pub fn rae() -> Actor {
    person("rae", &[Role::Reconciler])
}
pub fn col() -> Actor {
    person("col", &[Role::Collections])
}
pub fn svc() -> Actor {
    person("svc", &[Role::Service])
}

pub fn policy(id: &str) -> PolicyId {
    serde_json::from_value(serde_json::json!(id)).unwrap()
}

pub fn account(name: &str, number: &str) -> AccountDetails {
    AccountDetails {
        name: name.to_string(),
        number: number.to_string(),
    }
}

fn endpoint(channel: &str, region: &str) -> Endpoint {
    Endpoint {
        url: format!("https://{channel}.bank.test/{region}"),
        credential: format!("{channel}-{region}-credential"),
        signing_key: format!("{channel}-{region}-signing"),
        certificate_expires: d(2028, 6, 30),
        egress_address: format!("203.0.113.{}", region.len()),
        whitelisted: true,
        client_certificate: None,
        bank_authority: None,
    }
}

fn channel(id: &str, bank: &str, kind: ChannelKind, cutoff_hour: u32) -> (String, Channel) {
    let endpoints = REGIONS.iter().map(|r| (r.to_string(), endpoint(id, r))).collect();
    let channel = Channel {
        bank: bank.to_string(),
        kind,
        cutoff: NaiveTime::from_hms_opt(cutoff_hour, 0, 0).unwrap(),
        format: Default::default(),
        endpoints,
    };
    (id.to_string(), channel)
}

pub fn purpose_code(purpose: Purpose) -> &'static str {
    match purpose {
        Purpose::PremiumCollection => "collection",
        Purpose::ClaimsDisbursement => "claims",
        Purpose::ThirdPartyDisbursement => "third",
        Purpose::Refunds => "refunds",
        Purpose::Operating => "operating",
    }
}

/// Two banks, three regions, one statutory fund, every run type routed.
pub fn config() -> Config {
    let purposes = [
        Purpose::PremiumCollection,
        Purpose::ClaimsDisbursement,
        Purpose::ThirdPartyDisbursement,
        Purpose::Refunds,
        Purpose::Operating,
    ];
    let mut accounts = BTreeMap::new();
    for bank in ["anz", "bnz"] {
        for purpose in purposes {
            accounts.insert(
                format!("{bank}-sf1-{}", purpose_code(purpose)),
                BankAccount {
                    bank: bank.to_string(),
                    fund: "sf1".to_string(),
                    purpose,
                    currency: NZD,
                    at_bank: None,
                },
            );
        }
    }
    let route = |approvals| Route {
        primary: "anz-file".to_string(),
        secondary: Some("bnz-file".to_string()),
        urgent: Some("anz-api".to_string()),
        approvals,
        item_limit_minor: 100_000_00,
        run_limit_minor: 500_000_00,
        day_limit_minor: 800_000_00,
    };
    Config {
        banks: BTreeMap::from([("anz".to_string(), "ANZ".to_string()), ("bnz".to_string(), "BNZ".to_string())]),
        channels: BTreeMap::from([
            channel("anz-file", "anz", ChannelKind::File, 16),
            channel("anz-api", "anz", ChannelKind::Api, 22),
            channel("bnz-file", "bnz", ChannelKind::File, 15),
        ]),
        accounts,
        routes: RUN_TYPES.iter().map(|t| (*t, route(if *t == RunType::ExGratia { 2 } else { 1 }))).collect(),
        funds: BTreeSet::from(["sf1".to_string()]),
        rules: Rules {
            cooling_off_hours: 24,
            duplicate_window_days: 3,
            dishonours_before_suspension: 2,
            pre_advice_threshold_minor: 250_000_00,
            on_call: BTreeSet::from(["ari".to_string(), "ben".to_string()]),
            drill_max_age_days: 90,
            drill_account: account("Insurer Operating", "0100000001"),
            drill_amount_minor: 1_00,
            voter_regions: REGIONS.iter().map(|r| r.to_string()).collect(),
            holidays: BTreeSet::from([d(2027, 11, 17)]),
        },
    }
}

pub struct Desk {
    pub state: PaymentsState,
    pub now: NaiveDateTime,
    sent: u64,
}

impl Desk {
    /// A desk with nothing configured.
    pub fn bare() -> Self {
        Self {
            state: PaymentsState::default(),
            now: monday(),
            sent: 0,
        }
    }

    /// A configured desk whose outbound accounts each hold ten million.
    pub fn new() -> Self {
        let mut desk = Self::bare();
        desk.ok(
            &cal(),
            Command::ActivateConfig {
                version: 1,
                config: Box::new(config()),
            },
        );
        for code in config().accounts.keys() {
            desk.statement(code, d(2027, 11, 12), 10_000_000_00, &[]);
        }
        desk
    }

    pub fn envelope(&mut self, who: &Actor, command: Command) -> Envelope {
        self.sent += 1;
        Envelope {
            key: format!("k{}", self.sent),
            actor: who.clone(),
            at: self.now,
            command,
        }
    }

    pub fn send(&mut self, who: &Actor, command: Command) -> Output {
        let envelope = self.envelope(who, command);
        self.state.apply(self.sent, envelope)
    }

    pub fn ok(&mut self, who: &Actor, command: Command) -> Vec<Event> {
        match self.send(who, command) {
            Ok(events) => events,
            Err(reject) => panic!("refused: {} ({})", reject.code, reject.message),
        }
    }

    /// The code the command is refused with. The state must not change.
    pub fn code(&mut self, who: &Actor, command: Command) -> String {
        let mut before = self.state.clone();
        let reject = self.send(who, command).expect_err("the command should have been refused");
        // A refusal is remembered and moves the clock, and nothing else.
        before.answered = self.state.answered.clone();
        before.clock = self.state.clock;
        assert_eq!(before, self.state, "a refused command changed state");
        reject.code
    }

    pub fn at(&mut self, now: NaiveDateTime) {
        self.now = now;
    }

    /// Read a statement that opens at the last closing balance.
    pub fn statement(&mut self, account: &str, date: NaiveDate, opening: i64, lines: &[(&str, i64, &str)]) -> Vec<Event> {
        let lines: Vec<StatementLine> = lines
            .iter()
            .map(|(id, amount, reference)| StatementLine {
                id: id.to_string(),
                amount_minor: *amount,
                reference: reference.to_string(),
                payer_account: None,
            })
            .collect();
        let closing_minor = opening + lines.iter().map(|l| l.amount_minor).sum::<i64>();
        self.ok(
            &svc(),
            Command::RecordStatement {
                account: account.to_string(),
                date,
                opening_minor: opening,
                closing_minor,
                lines,
            },
        )
    }

    /// A payee that has passed every control and can be paid now.
    pub fn payee(&mut self, id: &str, kind: PayeeKind, number: &str) {
        self.ok(
            &pam(),
            Command::RegisterPayee {
                payee: id.to_string(),
                party: format!("party-{id}"),
                kind,
                account: account(id, number),
                name_check: NameCheck::Match,
            },
        );
        self.ok(&pat(), Command::ConfirmPayee { payee: id.to_string() });
        self.ok(
            &svc(),
            Command::ScreenPayee {
                payee: id.to_string(),
                clear: true,
            },
        );
        if kind.needs_legal() {
            self.ok(&lee(), Command::ApprovePayee { payee: id.to_string() });
            self.now += chrono::Duration::hours(24);
        }
    }

    pub fn authority(&mut self, id: &str, pol: &str, number: &str, sole_name: bool) {
        self.ok(
            &pam(),
            Command::CreateAuthority {
                authority: id.to_string(),
                policy: policy(pol),
                party: format!("party-{id}"),
                life: None,
                account: account(id, number),
                sole_name,
            },
        );
    }

    pub fn prepare(&mut self, run: &str, run_type: RunType, value_date: NaiveDate) {
        self.ok(
            &mia(),
            Command::PrepareRun {
                run: run.to_string(),
                run_type,
                fund: "sf1".to_string(),
                currency: NZD,
                value_date,
                urgent: false,
            },
        );
    }

    pub fn add(&mut self, run: &str, items: Vec<NewItem>) {
        self.ok(&mia(), Command::AddRunItems { run: run.to_string(), items });
    }

    /// Check and approve a prepared run, and return its first submission.
    pub fn approve(&mut self, run: &str) -> String {
        self.ok(&mia(), Command::CheckRun { run: run.to_string() });
        self.ok(&ari(), Command::ApproveRun { run: run.to_string() });
        self.state.runs[run].submissions.last().unwrap().clone()
    }

    /// Take a queued submission through the channel to the bank.
    pub fn deliver(&mut self, submission: &str) {
        self.ok(
            &svc(),
            Command::BeginSubmission {
                submission: submission.to_string(),
                region: "akl".to_string(),
            },
        );
        self.ok(
            &svc(),
            Command::RecordSubmitted {
                submission: submission.to_string(),
                content_hash: "c0ffee".to_string(),
                bank_reference: Some("bank-1".to_string()),
            },
        );
    }

    /// Deliver, have every item accepted, and settle on `on`.
    pub fn settle(&mut self, submission: &str, on: NaiveDate) -> Vec<Event> {
        self.deliver(submission);
        self.ok(
            &svc(),
            Command::RecordAcknowledgement {
                submission: submission.to_string(),
                refused: None,
                rejected: BTreeMap::new(),
            },
        );
        self.ok(
            &svc(),
            Command::RecordSettlement {
                submission: submission.to_string(),
                settled_on: on,
            },
        )
    }
}

/// A payment to a payee that is not policy money.
pub fn pay(item: &str, payee: &str, amount: i64) -> NewItem {
    NewItem {
        item: item.to_string(),
        source: format!("source-{item}"),
        counterparty: Counterparty::Payee(payee.to_string()),
        amount_minor: amount,
        policy: None,
        payable: None,
        refund: None,
        represents: None,
    }
}

/// A collection on an authority for its policy.
pub fn collect(item: &str, authority: &str, pol: &str, amount: i64) -> NewItem {
    NewItem {
        item: item.to_string(),
        source: format!("invoice-{item}"),
        counterparty: Counterparty::Authority(authority.to_string()),
        amount_minor: amount,
        policy: Some(policy(pol)),
        payable: None,
        refund: None,
        represents: None,
    }
}

/// The test configuration with the first bank, ANZ, spoken to in ISO 20022
/// and each of its accounts named as the bank knows it.
pub fn iso_config() -> Config {
    let mut c = config();
    for channel in c.channels.values_mut().filter(|ch| ch.bank == "anz") {
        channel.format = ic_payments::config::WireFormat::Iso20022;
        for (region, endpoint) in &mut channel.endpoints {
            endpoint.client_certificate = Some(format!("cert-{region}"));
            endpoint.bank_authority = Some("bank-ca".to_string());
        }
    }
    for (code, account) in c.accounts.iter_mut().filter(|(_, a)| a.bank == "anz") {
        account.at_bank = Some(ic_payments::iso20022::OrderingAccount {
            name: "Example Life Ltd".to_string(),
            number: format!("01-0100-{:07}-000", code.len()),
            bic: Some("ANZBNZ22".to_string()),
        });
    }
    c
}
