//! The underwriting module for the underwriting console's tests: its own
//! state machine, with the shipped configuration and its underwriters
//! (`amy` and `abe` underwriters, `sam`, `sue` and `cat` seniors), behind
//! the same JSON a node serves.
//!
//! Seeded with: `P1` issued on terms `amy` decided, and a claim's
//! non-disclosure review of it waiting for a senior (`ndr/CLM-1/L1`);
//! `P2` disclosed with a hazardous occupation, its case waiting in a
//! queue; `P3`, an adviser's application with nothing decided; and one
//! effect parked as undeliverable.

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use ic_channels::forward::{Source, REFUSED};
use ic_core::PolicyId;
use ic_log::StateMachine;
use ic_underwriting::command::{Command, Envelope};
use serde_json::Value;

#[path = "../../../underwriting/tests/common/mod.rs"]
pub mod world;

pub use world::World;

type Asked<'a> = Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'a>>;

/// The underwriting module's own code, asked and told as a node would be.
pub struct UnderwritingModule(pub Mutex<World>);

impl UnderwritingModule {
    /// The case open on a proposal's life, if any.
    pub fn case(&self, proposal: &str, life: &str) -> Option<String> {
        let w = self.0.lock().unwrap();
        w.s.proposal(&proposal.into())?.lives.get(&life.into())?.open_case().map(|c| c.id.clone())
    }

    /// How many cases an underwriter holds.
    pub fn held_by(&self, underwriter: &str) -> usize {
        self.0.lock().unwrap().s.workload(underwriter)
    }

    /// Every command the module has applied, as it received them.
    pub fn log(&self) -> Vec<Envelope> {
        self.0.lock().unwrap().log.clone()
    }
}

pub fn seeded() -> UnderwritingModule {
    let mut w = World::new();
    w.open("P1", 500_000_00);
    w.disclose("P1", "L1", &world::yes("hazardous_occupation"));
    let case = w.take("P1", "L1", "amy");
    // Terms with an exclusion, so the life is made an offer to accept.
    let excluded = ic_underwriting::model::CoverTerms {
        cover: "life".into(),
        outcome: ic_underwriting::model::CoverOutcome::Accept {
            loading_percent: 0,
            exclusions: vec![ic_underwriting::codes::AppliedExclusion {
                code: "EX-5001".into(),
                from: world::d(2026, 11, 11),
                until: None,
            }],
            revised_sum_insured_minor: None,
        },
    };
    let decided = w.propose(&case, "amy", vec![excluded]).unwrap();
    w.automatic(&decided);
    w.respond("P1", "L1", ic_underwriting::command::OfferResponse::Accept);
    w.ok(
        "issuer",
        Command::PolicyIssued {
            proposal: "P1".into(),
            layers: vec![],
        },
    );
    w.ok(
        "claims",
        Command::OpenNonDisclosureReview {
            claim_ref: "CLM-1".into(),
            policy: PolicyId::from("POL-P1"),
            life: "L1".into(),
            opened_by: "cl-1".into(),
        },
    );
    w.open("P2", 500_000_00);
    w.disclose("P2", "L1", &world::yes("hazardous_occupation"));
    // P3: an adviser's application on which the life gives their own consents.
    let with_adviser = w.open_command("P3", &["L1"], &[("L1", "life", "life", 500_000_00)], true);
    w.ok("new-business", Command::OpenProposal(Box::new(with_adviser)));
    let first = w.s.outbox(0, 1)[0].0;
    w.ok(
        "effects-driver",
        Command::ParkEffect {
            effect: first,
            reason: "nothing takes it yet".into(),
        },
    );
    UnderwritingModule(Mutex::new(w))
}

impl Source for UnderwritingModule {
    fn ask(&self, question: Value) -> Asked<'_> {
        let answer = ic_underwriting::answer(&self.0.lock().unwrap().s, question).map_err(|e| format!("{REFUSED}{e}"));
        Box::pin(async move { answer })
    }

    fn tell(&self, envelope: Value) -> Asked<'_> {
        let applied = (|| {
            let envelope: Envelope = serde_json::from_value(envelope).map_err(|e| format!("{REFUSED}not a command of this module: {e}"))?;
            let mut w = self.0.lock().unwrap();
            w.index += 1;
            let index = w.index;
            w.log.push(envelope.clone());
            serde_json::to_value(w.s.apply(index, envelope)).map_err(|e| e.to_string())
        })();
        Box::pin(async move { applied })
    }
}
