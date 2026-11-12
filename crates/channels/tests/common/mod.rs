//! Shared test harness: a simulated core cluster feeding a channel state,
//! and device keys that sign as a phone or a browser does.
#![allow(dead_code)]

pub mod claims;
pub mod commission;
pub mod complaints;
pub mod customers;
pub mod finance;
pub mod pricing;
pub mod reinsurance;

use base64::Engine;
use chrono::NaiveDate;
use ic_channels::directory::{Actor, DirectoryCommand, Engagement};
use ic_channels::feed::{Op, PartyFeed};
use ic_channels::staff::At;
use ic_channels::state::{Output, EVENT_RELAY};
use ic_channels::{ChannelState, Command, Effect, Envelope};
use ic_core::command::IssuePolicy;
use ic_core::{PartyId, PolicyId, Reject};
use ic_log::StateMachine;
use ic_sim::config::shipped_activations;
use ic_sim::persona::issue_command;
use ic_sim::Cluster;
use ring::rand::SystemRandom;
use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING, ECDSA_P256_SHA256_FIXED_SIGNING};

pub fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

/// The business date every test opens on.
pub fn open() -> NaiveDate {
    d(2027, 11, 15)
}

/// Nine in the morning on the opening date, in seconds since the epoch.
pub fn t0() -> At {
    open().and_hms_opt(9, 0, 0).unwrap().and_utc().timestamp()
}

pub fn p(id: &str) -> PartyId {
    PartyId::from(id)
}

pub fn party(id: &str) -> Actor {
    Actor::Party(p(id))
}

pub fn staff(id: &str) -> Actor {
    Actor::Staff(p(id))
}

pub fn module(name: &str) -> Actor {
    Actor::Module(name.to_string())
}

pub fn code<T: std::fmt::Debug>(result: Result<T, Reject>) -> String {
    result.unwrap_err().code
}

/// A device's key pair, as its hardware keystore holds it.
pub struct Key {
    fixed: EcdsaKeyPair,
    asn1: EcdsaKeyPair,
    rng: SystemRandom,
    /// Base64 of the uncompressed public point.
    pub public: String,
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

impl Key {
    pub fn new() -> Self {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let fixed = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng).unwrap();
        let asn1 = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng).unwrap();
        let public = b64(fixed.public_key().as_ref());
        Self { fixed, asn1, rng, public }
    }

    /// Sign as a browser does: the fixed-length form.
    pub fn sign(&self, message: &[u8]) -> String {
        b64(self.fixed.sign(&self.rng, message).unwrap().as_ref())
    }

    /// Sign as a phone keystore does: the ASN.1 form.
    pub fn sign_asn1(&self, message: &[u8]) -> String {
        b64(self.asn1.sign(&self.rng, message).unwrap().as_ref())
    }
}

/// A core cluster and the channel state that follows it.
pub struct World {
    pub cluster: Cluster,
    pub ch: ChannelState,
    seq: u64,
    pub at: At,
}

impl World {
    pub fn new() -> Self {
        let mut cluster = Cluster::in_memory(2, 7);
        cluster.open(open()).unwrap();
        for activation in shipped_activations().unwrap() {
            cluster.activate_everywhere(activation).unwrap();
        }
        let mut world = Self {
            cluster,
            ch: ChannelState::shipped().unwrap(),
            seq: 0,
            at: t0(),
        };
        world.relay();
        world
    }

    /// Apply one command under a fresh key, one second after the last.
    pub fn send(&mut self, actor: Actor, command: Command) -> Result<Vec<Effect>, Reject> {
        self.seq += 1;
        let key = format!("k-{}", self.seq);
        self.send_keyed(&key, actor, command).result
    }

    pub fn send_keyed(&mut self, key: &str, actor: Actor, command: Command) -> Output {
        self.seq += 1;
        self.at += 1;
        self.ch.apply(
            self.seq,
            Envelope {
                key: key.to_string(),
                at: self.at,
                actor,
                command,
            },
        )
    }

    pub fn dir(&mut self, actor: Actor, command: DirectoryCommand) -> Result<Vec<Effect>, Reject> {
        self.send(actor, Command::Directory(command))
    }

    /// Deliver every event the core has produced since the last relay.
    pub fn relay(&mut self) -> Vec<Effect> {
        let records = self.cluster.take_events();
        self.send(module(EVENT_RELAY), Command::CoreEvents { records }).unwrap()
    }

    /// Issue a policy in the core and relay its events.
    pub fn issue(&mut self, issue: IssuePolicy) -> PolicyId {
        let (partition, policy) = (self.cluster.partition_of(&issue.group), issue.policy.clone());
        self.cluster.submit(partition, "new-business", issue_command(issue)).unwrap();
        self.relay();
        policy
    }

    pub fn core(&mut self, policy: &PolicyId, caller: &str, command: ic_core::Command) {
        let partition = self.cluster.locate(policy).unwrap();
        self.cluster.submit(partition, caller, command).unwrap();
    }

    pub fn feed(&self, party: &str) -> Option<&PartyFeed> {
        self.ch.data.feeds.get(&p(party))
    }

    /// The keys a party's device holds.
    pub fn held(&self, party: &str) -> Vec<String> {
        self.feed(party).map(|f| f.rows.keys().cloned().collect()).unwrap_or_default()
    }

    /// The body of one row on a party's device.
    pub fn row(&self, party: &str, key: &str) -> Option<serde_json::Value> {
        self.feed(party).and_then(|f| f.rows.get(key)).map(|(_, body)| body.clone())
    }

    /// The last change made to a key in a party's feed.
    pub fn last_op(&self, party: &str, key: &str) -> Option<Op> {
        self.feed(party)
            .and_then(|f| f.changes.iter().rev().find(|c| c.key == key))
            .map(|c| c.op.clone())
    }

    /// What a viewer may see of a policy through a portal, as row keys.
    pub fn sees(&self, viewer: &str, policy: &PolicyId) -> Vec<String> {
        let on = self.ch.today(self.at);
        self.ch.visible_rows(&p(viewer), policy, on, true).1.into_iter().map(|r| r.key).collect()
    }

    /// Record an adviser's engagement with a financial advice provider.
    pub fn engage(&mut self, id: &str, adviser: &str, fap: &str, to: Option<NaiveDate>) -> Result<Vec<Effect>, Reject> {
        self.dir(
            module("distribution"),
            DirectoryCommand::RecordEngagement {
                engagement: Engagement {
                    id: id.to_string(),
                    adviser: p(adviser),
                    fap: p(fap),
                    from: d(2025, 1, 1),
                    to,
                },
            },
        )
    }
}
