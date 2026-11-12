//! A bench for driving the reinsurance module in tests: the shipped
//! treaties activated, and helpers that build the core's events.
#![allow(dead_code)]

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_core::event::{Event, EventRecord, LayerInfo, LifeInfo, MemberCover};
use ic_core::{GroupId, LayerOrigin, LifeId, PartyId, PolicyId};
use ic_ledger::PartitionId;
use ic_product::Sex;
use ic_reinsurance::loader::{load_aggregate_treaty, load_rate_table, load_treaty};
use ic_reinsurance::treaty::RiskFactors;
use ic_reinsurance::{Applied, Command, Envelope, Instruction, InstructionBody, PostingKind, Reinsurance, ReinsurerId, RiskKey, TreatyId};

pub const RETAIL: &str = include_str!("../../../../treaties/retail_life_surplus.star");
pub const GROUP: &str = include_str!("../../../../treaties/group_life_surplus.star");
pub const RATES: &str = include_str!("../../../../treaties/rates/ri_life_base.star");
pub const CATASTROPHE: &str = include_str!("../../../../treaties/catastrophe_2027.star");
pub const STOP_LOSS: &str = include_str!("../../../../treaties/stop_loss_2027.star");

pub const K: i64 = 1_000_00;

pub fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

pub fn north() -> ReinsurerId {
    ReinsurerId::from("reinsurer-north")
}

pub fn south() -> ReinsurerId {
    ReinsurerId::from("reinsurer-south")
}

pub fn retail() -> TreatyId {
    TreatyId::from("retail-life-surplus")
}

pub fn group() -> TreatyId {
    TreatyId::from("group-life-surplus")
}

pub fn key(policy: &str, cover: &str, layer: u32, life: &str) -> RiskKey {
    RiskKey {
        policy: PolicyId::from(policy),
        cover: cover.to_string(),
        layer,
        life: LifeId::from(life),
    }
}

pub fn layer(cover: &str, cover_type: &str, n: u32, life: &str, sum: i64, start: NaiveDate) -> LayerInfo {
    LayerInfo {
        cover: cover.to_string(),
        cover_type: cover_type.to_string(),
        layer: n,
        life: LifeId::from(life),
        sum_insured_minor: sum,
        start,
    }
}

pub fn member_cover(life: &str, sum: i64) -> MemberCover {
    MemberCover {
        cover: "group-life".to_string(),
        cover_type: "group_life".to_string(),
        life: LifeId::from(life),
        sum_insured_minor: sum,
    }
}

pub struct Bench {
    pub ri: Reinsurance,
    pub index: u64,
    pub seq: BTreeMap<PartitionId, u64>,
    pub today: NaiveDate,
    /// Everything applied, for replay.
    pub log: Vec<Envelope>,
    /// Every instruction the module has issued.
    pub instructions: Vec<Instruction>,
}

impl Bench {
    /// An empty module on 1 February 2027.
    pub fn empty() -> Self {
        Bench {
            ri: Reinsurance::new(),
            index: 0,
            seq: BTreeMap::new(),
            today: d(2027, 2, 1),
            log: vec![],
            instructions: vec![],
        }
    }

    /// The shipped treaties and rates, activated.
    pub fn new() -> Self {
        let mut b = Self::empty();
        for (file, source) in [("retail_life_surplus.star", RETAIL), ("group_life_surplus.star", GROUP)] {
            let treaty = Box::new(load_treaty(file, source).unwrap());
            b.ok("config", Command::ActivateTreaty { treaty });
        }
        let table = Box::new(load_rate_table("ri_life_base.star", RATES).unwrap());
        b.ok("config", Command::ActivateRateTable { table });
        b
    }

    pub fn with_aggregates() -> Self {
        let mut b = Self::new();
        for (file, source) in [("catastrophe_2027.star", CATASTROPHE), ("stop_loss_2027.star", STOP_LOSS)] {
            let treaty = Box::new(load_aggregate_treaty(file, source).unwrap());
            b.ok("config", Command::ActivateAggregateTreaty { treaty });
        }
        b
    }

    pub fn send_keyed(&mut self, key: &str, caller: &str, command: Command) -> Applied {
        self.index += 1;
        let env = Envelope {
            key: key.to_string(),
            caller: caller.to_string(),
            on: self.today,
            command,
        };
        self.log.push(env.clone());
        let applied = self.ri.apply_envelope(self.index, env);
        self.instructions.extend(applied.instructions.iter().cloned());
        applied
    }

    pub fn send(&mut self, caller: &str, command: Command) -> Applied {
        let key = format!("k{}", self.index + 1);
        self.send_keyed(&key, caller, command)
    }

    /// Send a command that must be accepted.
    pub fn ok(&mut self, caller: &str, command: Command) -> Applied {
        let applied = self.send(caller, command);
        assert_eq!(applied.result, Ok(()), "command refused");
        applied
    }

    /// Send a command that must be refused, and return the code.
    pub fn refused(&mut self, caller: &str, command: Command) -> String {
        let before = self.ri.clone();
        let applied = self.send(caller, command);
        let code = applied.result.expect_err("command accepted").code;
        assert!(
            applied.instructions.is_empty() && applied.notices.is_empty(),
            "a refused command produced output"
        );
        assert_eq!(without_keys(&before), without_keys(&self.ri), "a refused command changed state");
        code
    }

    pub fn records(&mut self, partition: PartitionId, events: Vec<Event>) -> Vec<EventRecord> {
        events
            .into_iter()
            .map(|event| {
                let seq = self.seq.entry(partition).or_default();
                *seq += 1;
                EventRecord {
                    id: format!("{partition}-{seq}"),
                    partition,
                    seq: *seq,
                    business_date: self.today,
                    log_index: *seq,
                    traceparent: None,
                    event,
                }
            })
            .collect()
    }

    /// Feed events from a partition, dated today.
    pub fn feed(&mut self, partition: PartitionId, events: Vec<Event>) -> Applied {
        let events = self.records(partition, events);
        self.ok("event-feed", Command::CoreEvents { events })
    }

    /// Issue a retail policy on one named life.
    pub fn issue(&mut self, partition: PartitionId, policy: &str, party: &str, layers: Vec<LayerInfo>) -> Applied {
        let inception = layers.iter().map(|l| l.start).min().unwrap_or(self.today);
        let life = layers.first().map_or(LifeId::from("L1"), |l| l.life.clone());
        self.feed(
            partition,
            vec![Event::PolicyIssued {
                policy: PolicyId::from(policy),
                number: format!("N-{policy}"),
                group: GroupId::from(policy),
                policyholder: PartyId::from(party),
                product_code: "term-life".to_string(),
                product_hash: "hash".to_string(),
                inception,
                lives: vec![LifeInfo::new(life, Some(PartyId::from(party)))],
                layers,
                members: vec![],
            }],
        )
    }

    /// Issue a group scheme with unnamed members.
    pub fn issue_scheme(&mut self, partition: PartitionId, policy: &str, inception: NaiveDate, members: Vec<MemberCover>) -> Applied {
        self.feed(
            partition,
            vec![Event::PolicyIssued {
                policy: PolicyId::from(policy),
                number: format!("N-{policy}"),
                group: GroupId::from(policy),
                policyholder: PartyId::from("employer"),
                product_code: "group-life".to_string(),
                product_hash: "hash".to_string(),
                inception,
                lives: members.iter().map(|m| LifeInfo::new(m.life.clone(), None)).collect(),
                layers: vec![],
                members,
            }],
        )
    }

    pub fn add_layer(&mut self, partition: PartitionId, policy: &str, l: LayerInfo) -> Applied {
        self.feed(
            partition,
            vec![Event::LayerAdded {
                policy: PolicyId::from(policy),
                layer: l,
                origin: LayerOrigin::Increase,
                continues: None,
            }],
        )
    }

    pub fn factors(&mut self, policy: &str, life: &str, dob: NaiveDate, smoker: bool) {
        self.ok(
            "new-business",
            Command::RecordRiskFactors {
                policy: PolicyId::from(policy),
                life: LifeId::from(life),
                factors: RiskFactors {
                    date_of_birth: Some(dob),
                    sex: Some(Sex::Female),
                    smoker: Some(smoker),
                },
            },
        );
    }

    pub fn premium_due(
        &mut self,
        partition: PartitionId,
        policy: &str,
        receivable: u32,
        start: NaiveDate,
        end: NaiveDate,
        by_cover_type: &[(&str, i64)],
    ) -> Applied {
        self.feed(
            partition,
            vec![Event::PremiumDue {
                policy: PolicyId::from(policy),
                receivable,
                period_start: start,
                period_end: end,
                total_minor: by_cover_type.iter().map(|(_, a)| a).sum(),
                members: None,
                by_cover_type: by_cover_type.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            }],
        )
    }

    /// The total the core has been told to post of one kind for a reinsurer.
    pub fn posted(&self, kind: PostingKind, reinsurer: &ReinsurerId) -> i64 {
        self.instructions
            .iter()
            .filter_map(|i| match &i.body {
                InstructionBody::Post {
                    kind: k,
                    reinsurer: r,
                    amount_minor,
                    ..
                } if *k == kind && r == reinsurer => Some(*amount_minor),
                _ => None,
            })
            .sum()
    }

    /// Replay the whole log into a fresh module.
    pub fn replayed(&self) -> Reinsurance {
        let mut ri = Reinsurance::new();
        for (i, env) in self.log.iter().enumerate() {
            ri.apply_envelope(i as u64 + 1, env.clone());
        }
        ri
    }

    /// Assert the module's own invariants hold.
    pub fn assert_reconciles(&self) {
        assert_eq!(self.ri.reconcile(), Vec::<String>::new());
    }
}

/// The state as JSON without the idempotency record, which every command
/// adds to whether or not it is accepted.
pub fn without_keys(ri: &Reinsurance) -> serde_json::Value {
    let mut v = serde_json::to_value(ri).unwrap();
    v.as_object_mut().unwrap().remove("idempotency");
    v
}
