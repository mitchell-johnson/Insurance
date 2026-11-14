//! The reinsurance module for the exceptions console's tests: its own state
//! machine behind the same JSON a node serves, with the shipped retail
//! treaty and rates in force and one policy ceded, whose cession the core
//! refused, so one exception is open.

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use chrono::NaiveDate;
use ic_channels::forward::{Source, REFUSED};
use ic_core::event::{Event, EventRecord, LayerInfo, LifeInfo};
use ic_core::{GroupId, LifeId, PartyId, PolicyId};
use ic_reinsurance::loader::{load_rate_table, load_treaty};
use ic_reinsurance::{Command, Envelope, Reinsurance};
use serde_json::Value;

type Asked<'a> = Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'a>>;

const RETAIL: &str = include_str!("../../../../treaties/retail_life_surplus.star");
const RATES: &str = include_str!("../../../../treaties/rates/ri_life_base.star");

pub fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

/// The reinsurance module's own code, asked and told as a node would be.
pub struct ReinsuranceModule(pub Mutex<(Reinsurance, u64)>);

impl ReinsuranceModule {
    fn send(&self, caller: &str, command: Command) {
        let mut held = self.0.lock().unwrap();
        held.1 += 1;
        let index = held.1;
        let envelope = Envelope {
            key: format!("setup-{index}"),
            caller: caller.to_string(),
            on: d(2027, 2, 1),
            command,
        };
        let applied = held.0.apply_envelope(index, envelope);
        assert_eq!(applied.result, Ok(()), "setup command {index} was refused");
    }

    /// Whether an exception is open.
    pub fn open(&self, id: &str) -> bool {
        self.0.lock().unwrap().0.exceptions().contains_key(id)
    }
}

/// The retail treaty and rates, a policy with 800,000 of life cover on one
/// life ceded, and the core's refusal of that cession: exception `delivery:1`.
pub fn seeded() -> ReinsuranceModule {
    let module = ReinsuranceModule(Mutex::new((Reinsurance::new(), 0)));
    module.send(
        "config",
        Command::ActivateTreaty {
            treaty: Box::new(load_treaty("retail_life_surplus.star", RETAIL).unwrap()),
        },
    );
    module.send(
        "config",
        Command::ActivateRateTable {
            table: Box::new(load_rate_table("ri_life_base.star", RATES).unwrap()),
        },
    );
    let issued = Event::PolicyIssued {
        policy: PolicyId::from("P1"),
        number: "N-P1".to_string(),
        group: GroupId::from("P1"),
        policyholder: PartyId::from("anna"),
        product_code: "term-life".to_string(),
        product_hash: "hash".to_string(),
        inception: d(2027, 2, 1),
        lives: vec![LifeInfo::new(LifeId::from("L1"), Some(PartyId::from("anna")))],
        layers: vec![LayerInfo {
            cover: "life".to_string(),
            cover_type: "life".to_string(),
            layer: 1,
            life: LifeId::from("L1"),
            sum_insured_minor: 800_000_00,
            start: d(2027, 2, 1),
            option: None,
        }],
        members: vec![],
    };
    let record = EventRecord {
        id: "3-1".to_string(),
        partition: 3,
        seq: 1,
        business_date: d(2027, 2, 1),
        log_index: 1,
        traceparent: None,
        event: issued,
    };
    module.send("event-feed", Command::CoreEvents { events: vec![record] });
    module.send(
        "driver",
        Command::InstructionRefused {
            seq: 1,
            code: "unknown_policy".to_string(),
            message: "the core does not know P1".to_string(),
        },
    );
    assert!(module.open("delivery:1"), "the refused cession is an exception");
    module
}

impl Source for ReinsuranceModule {
    fn ask(&self, question: Value) -> Asked<'_> {
        let answer = ic_reinsurance::host::answer(&self.0.lock().unwrap().0, question).map_err(|e| format!("{REFUSED}{e}"));
        Box::pin(async move { answer })
    }

    fn tell(&self, envelope: Value) -> Asked<'_> {
        let applied = (|| {
            let envelope: Envelope = serde_json::from_value(envelope).map_err(|e| format!("{REFUSED}not a command of this module: {e}"))?;
            let mut held = self.0.lock().unwrap();
            held.1 += 1;
            let index = held.1;
            serde_json::to_value(held.0.apply_envelope(index, envelope)).map_err(|e| e.to_string())
        })();
        Box::pin(async move { applied })
    }
}
