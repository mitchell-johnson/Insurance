//! Shared helpers for the unit pricing tests: a fund, valuations, and a
//! harness that sends commands as the right caller with fresh keys.
#![allow(dead_code)]

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use ic_money::{Currency, DecStr, FundId, RoundingMode};
use ic_pricing::command::*;
use ic_pricing::fund::FundDefinition;
use ic_pricing::state::PricingState;
use ic_pricing::valuation::Valuation;

pub fn d(s: &str) -> DecStr {
    DecStr::parse(s).unwrap_or_else(|e| panic!("{e}"))
}

pub fn day(n: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, n).unwrap_or_default()
}

pub fn fund_id(name: &str) -> FundId {
    FundId(name.to_string())
}

pub fn fund(name: &str) -> FundDefinition {
    let pricing = [
        ("previous_move", "0.05"),
        ("benchmark_gap", "0.01"),
        ("stale_asset_days", "3"),
        ("unexplained_move", "0.005"),
        ("error_threshold", "0.003"),
        ("launch_price", "1.0000"),
        ("pricing_lag_days", "1"),
    ];
    FundDefinition {
        id: fund_id(name),
        version: 1,
        content_hash: format!("{name}-v1"),
        name: format!("{name} fund"),
        currency: Currency::NZD,
        cut_off: "15:00".to_string(),
        buy_spread_bps: 25,
        sell_spread_bps: 25,
        price_decimals: 4,
        unit_rounding: RoundingMode::Down,
        non_pricing_days: Default::default(),
        open_to_new_money: true,
        pricing: pricing.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
    }
}

/// A valuation of `growth` with no stale assets and the benchmark matching
/// `benchmark`.
pub fn valuation(date: NaiveDate, nav: &str, units_micro: i64, benchmark: &str) -> Valuation {
    Valuation {
        fund: fund_id("growth"),
        pricing_date: date,
        nav: d(nav),
        units_on_issue_micro: units_micro,
        applications: d("0"),
        redemptions: d("0"),
        benchmark_return: Some(d(benchmark)),
        restated: false,
        stale_assets: vec![],
    }
}

pub const UNITS: i64 = 1_000_000_000_000;

pub struct Harness {
    pub state: PricingState,
    pub n: u64,
    pub at: DateTime<Utc>,
}

impl Harness {
    /// A state with `growth` active on two partitions.
    pub fn new() -> Self {
        let mut h = Harness {
            state: PricingState::new(),
            n: 0,
            at: Utc.with_ymd_and_hms(2026, 11, 2, 4, 0, 0).unwrap(),
        };
        h.ok(CALLER_CONFIG, "config", &[], Command::ActivateFund(Box::new(fund("growth"))));
        h.ok(CALLER_INTAKE, "driver", &[], Command::RecordPartitions { count: 2 });
        h
    }

    pub fn send(&mut self, caller: &str, actor: &str, roles: &[&str], command: Command) -> Output {
        self.n += 1;
        let e = Envelope {
            key: format!("k{}", self.n),
            caller: caller.to_string(),
            actor: actor.to_string(),
            roles: roles.iter().map(|r| r.to_string()).collect(),
            at: self.at,
            command,
        };
        self.state.apply_envelope(e)
    }

    pub fn ok(&mut self, caller: &str, actor: &str, roles: &[&str], command: Command) -> Output {
        let out = self.send(caller, actor, roles, command);
        assert!(!matches!(out, Output::Refused { .. }), "{out:?}");
        out
    }

    pub fn refused(&mut self, caller: &str, actor: &str, roles: &[&str], command: Command) -> String {
        let before = self.state.clone();
        match self.send(caller, actor, roles, command) {
            Output::Refused { code, .. } => {
                // A refusal leaves no trace beyond its remembered key.
                let mut after = self.state.clone();
                after.applied = before.applied.clone();
                assert_eq!(after, before, "a refusal changed the state");
                code
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    pub fn receive(&mut self, file: &str, v: Valuation) -> Output {
        self.ok(
            CALLER_INTAKE,
            "driver",
            &[],
            Command::ReceiveValuations {
                administrator: "acme".into(),
                file_id: file.into(),
                valuations: vec![v],
            },
        )
    }

    /// Both partitions report units on issue.
    pub fn core_units(&mut self, through: Option<NaiveDate>, each: i64) {
        for partition in 0..2 {
            self.ok(
                CALLER_INTAKE,
                "driver",
                &[],
                Command::RecordCoreUnits {
                    fund: fund_id("growth"),
                    partition,
                    executed_through: through,
                    units_micro: each,
                },
            );
        }
    }

    pub fn prepare(&mut self, date: NaiveDate) -> Output {
        self.send(
            CALLER_CONSOLE,
            "maker",
            &[ROLE_PREPARER],
            Command::PreparePrice {
                fund: fund_id("growth"),
                pricing_date: date,
            },
        )
    }

    pub fn prepared_id(&mut self, date: NaiveDate) -> String {
        match self.prepare(date) {
            Output::Prepared { price, .. } => price,
            other => panic!("{other:?}"),
        }
    }

    pub fn approve(&mut self, price: &str, accepted: BTreeMap<ic_pricing::strike::CheckKind, String>) -> Output {
        self.send(
            CALLER_CONSOLE,
            "checker",
            &[ROLE_APPROVER],
            Command::ApprovePrice { price: price.into(), accepted },
        )
    }

    /// Report every queued publication applied.
    pub fn deliver_all(&mut self) {
        let ids: Vec<String> = self.state.outbox(100).iter().map(|p| p.id.clone()).collect();
        for id in ids {
            self.ok(
                CALLER_OUTBOX,
                "host",
                &[],
                Command::RecordDelivery {
                    publication: id,
                    delivery: Delivery::Applied,
                },
            );
        }
    }

    /// Price `date` end to end from a valuation, returning the price ID.
    pub fn price_day(&mut self, date: NaiveDate, nav: &str, benchmark: &str, through: Option<NaiveDate>) -> String {
        self.receive(&format!("F-{date}"), valuation(date, nav, UNITS, benchmark));
        self.core_units(through, UNITS / 2);
        let id = self.prepared_id(date);
        assert_eq!(self.approve(&id, BTreeMap::new()), Output::Done);
        self.deliver_all();
        id
    }
}
