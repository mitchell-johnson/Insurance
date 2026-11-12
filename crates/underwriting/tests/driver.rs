//! The driver against the real reinsurance module and the real core, each
//! in this process: a proposal goes from disclosure to a policy in the
//! ledger with nobody carrying anything by hand.

mod common;

use std::sync::{Arc, Mutex};

use chrono::{DateTime, NaiveDate, Utc};
use common::*;
use ic_core::PolicyStatus;
use ic_ledger::PartitionId;
use ic_log::StateMachine;
use ic_reinsurance::capacity::{Outcome, ReservationStatus};
use ic_reinsurance::driver::LocalModule;
use ic_reinsurance::loader::{load_rate_table, load_treaty};
use ic_sim::config::shipped_activations;
use ic_sim::Cluster;
use ic_underwriting::command::{Applied, Command, Effect, Envelope, Reject};
use ic_underwriting::driver::{Core, CoreAnswer, Driver, DriverError, Forward, NothingConnected, Pass, Refusal, Reinsurer, Schemes, Underwriting};
use ic_underwriting::config::{InterimSettings, Settings};
use ic_underwriting::model::{Entry, ProposalStatus, Stage};
use ic_underwriting::special::{answer, Query};
use serde_json::Value;

const RETAIL: &str = include_str!("../../../treaties/retail_life_surplus.star");
const RATES: &str = include_str!("../../../treaties/rates/ri_life_base.star");

/// The module in this process. It can be made unreachable.
#[derive(Clone)]
struct Module {
    world: Arc<Mutex<World>>,
    down: Arc<Mutex<bool>>,
}

impl Module {
    fn with<T>(&self, f: impl FnOnce(&mut World) -> T) -> T {
        f(&mut self.world.lock().unwrap())
    }
    fn up(&self) -> Result<(), DriverError> {
        if *self.down.lock().unwrap() {
            return Err(DriverError::Unavailable("the underwriting module cannot be reached".into()));
        }
        Ok(())
    }
}

impl Underwriting for Module {
    async fn propose(&self, envelope: Envelope) -> Result<Result<Applied, Reject>, DriverError> {
        self.up()?;
        Ok(self.with(|w| {
            w.index += 1;
            w.log.push(envelope.clone());
            w.s.apply(w.index, envelope)
        }))
    }
    async fn ask(&self, query: Query) -> Result<Value, DriverError> {
        self.up()?;
        self.with(|w| answer(&w.s, serde_json::to_value(&query).unwrap())).map_err(DriverError::Answer)
    }
}

/// The real reinsurance module in this process.
#[derive(Clone, Default)]
struct Reinsurance {
    module: LocalModule,
    down: Arc<Mutex<bool>>,
    /// Every command sent, by key.
    sent: Arc<Mutex<Vec<String>>>,
}

impl Reinsurer for Reinsurance {
    async fn propose(&self, envelope: ic_reinsurance::Envelope) -> Result<Result<(), Refusal>, DriverError> {
        if *self.down.lock().unwrap() {
            return Err(DriverError::Unavailable("the reinsurance module cannot be reached".into()));
        }
        self.sent.lock().unwrap().push(envelope.key.clone());
        Ok(self.module.apply(envelope).result.map_err(|r| Refusal::new(&r.code, r.message)))
    }
    async fn capacity(&self, reservation: &str) -> Result<Option<Outcome>, DriverError> {
        Ok(self.module.state().capacity(reservation).map(|a| a.outcome))
    }
}

/// The real core, one partition, in this process.
#[derive(Clone)]
struct Ledger {
    cluster: Arc<Mutex<Cluster>>,
    down: Arc<Mutex<bool>>,
}

impl Core for Ledger {
    async fn send(&self, _partition: PartitionId, key: &str, caller: &str, command: ic_core::Command) -> Result<CoreAnswer, DriverError> {
        if *self.down.lock().unwrap() {
            return Err(DriverError::Unavailable("no ledger node answered".into()));
        }
        let applied = self.cluster.lock().unwrap().submit_keyed(0, key, caller, command);
        Ok(CoreAnswer {
            refused: applied.result.err().map(|r| Refusal::new(&r.code, r.message)),
            events: applied.events.into_iter().map(|e| e.event).collect(),
        })
    }
}

/// Group administration as a recorder: what it was sent, and whether it refuses.
#[derive(Clone, Default)]
struct Group {
    sent: Arc<Mutex<Vec<ic_group::command::Envelope>>>,
    refuse: Arc<Mutex<bool>>,
}

impl Schemes for Group {
    async fn propose(&self, envelope: ic_group::command::Envelope) -> Result<Result<(), Refusal>, DriverError> {
        if *self.refuse.lock().unwrap() {
            return Ok(Err(Refusal::new("unknown_case", "no such case")));
        }
        self.sent.lock().unwrap().push(envelope);
        Ok(Ok(()))
    }
}

/// Takes everything offered and remembers it.
#[derive(Clone, Default)]
struct TakesAll(Arc<Mutex<Vec<Effect>>>);

impl Forward for TakesAll {
    async fn forward(&self, effect: &Effect) -> Result<Option<String>, DriverError> {
        self.0.lock().unwrap().push(effect.clone());
        Ok(None)
    }
}

fn start() -> NaiveDate {
    d(2027, 2, 1)
}

fn clock() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2027-02-01T09:00:00Z").unwrap().with_timezone(&Utc)
}

struct Bench<F> {
    module: Module,
    reinsurance: Reinsurance,
    ledger: Ledger,
    group: Group,
    driver: Driver<Module, Reinsurance, Ledger, Group, F>,
    product_hash: String,
}

fn bench<F: Forward + Clone>(forward: F) -> Bench<F> {
    let mut world = World::new();
    world.now = clock();

    let reinsurance = Reinsurance::default();
    let config = |command| {
        let applied = reinsurance.module.apply(ic_reinsurance::Envelope {
            key: format!("config-{}", reinsurance.module.state().state_hash()),
            caller: "config".into(),
            on: start(),
            command,
        });
        assert_eq!(applied.result, Ok(()));
    };
    config(ic_reinsurance::Command::ActivateTreaty {
        treaty: Box::new(load_treaty("retail_life_surplus.star", RETAIL).unwrap()),
    });
    config(ic_reinsurance::Command::ActivateRateTable {
        table: Box::new(load_rate_table("ri_life_base.star", RATES).unwrap()),
    });

    let mut cluster = Cluster::in_memory(1, 7);
    cluster.open(start()).unwrap();
    for activation in shipped_activations().unwrap() {
        cluster.activate_everywhere(activation).unwrap();
    }
    let product_hash = cluster.state(0).catalog.product_on("term-life", start()).unwrap().content_hash.clone();

    let module = Module {
        world: Arc::new(Mutex::new(world)),
        down: Arc::default(),
    };
    let ledger = Ledger {
        cluster: Arc::new(Mutex::new(cluster)),
        down: Arc::default(),
    };
    let group = Group::default();
    let driver = Driver {
        underwriting: module.clone(),
        reinsurer: reinsurance.clone(),
        core: ledger.clone(),
        schemes: group.clone(),
        forward,
        batch: 3,
        clock,
    };
    Bench {
        module,
        reinsurance,
        ledger,
        group,
        driver,
        product_hash,
    }
}

impl<F: Forward> Bench<F> {
    /// Open a one-life proposal for term life bound to the product version the core holds.
    fn open(&self, id: &str, sum: i64) {
        let hash = self.product_hash.clone();
        self.module.with(|w| {
            let mut open = w.open_command(id, &["L1"], &[("L1", "life", "life", sum)], false);
            open.quote.product_hash = hash;
            open.group = ic_core::GroupId::from(format!("grp-{id}").as_str());
            w.ok("new-business", Command::OpenProposal(Box::new(open)));
        });
    }

    async fn pass(&self) -> Pass {
        self.driver.deliver().await.unwrap()
    }

    fn status(&self, id: &str) -> ProposalStatus {
        self.module.with(|w| w.p(id).status.clone())
    }
}

/// Purpose: a clean application reaches a policy in the ledger with the driver alone carrying
/// between the modules: capacity is reserved with reinsurance and its answer brought back, the
/// policy is issued in the core on the quoted product version, the layer the core wrote is
/// confirmed against the reservation, the rating factors are recorded, and the outbox ends
/// empty. Run again, the driver finds nothing to do.
/// Why it matters: this is the path every straight-through policy takes in production. Each
/// hand-off here is one that, if it were missing, would leave an accepted applicant with no
/// policy or the insurer with cover the treaty never heard of.
#[tokio::test]
async fn a_clean_application_becomes_a_policy_with_nobody_carrying() {
    let b = bench(TakesAll::default());
    b.open("P1", 500_000_00);
    b.module.with(|w| w.disclose("P1", "L1", &clean()));
    assert_eq!(b.module.with(|w| w.stage("P1", "L1")), Stage::AwaitingCapacity);

    let pass = b.pass().await;
    assert!(pass.parked.is_empty(), "{pass:?}");
    assert!(matches!(b.status("P1"), ProposalStatus::Issued { .. }), "{:?}", b.status("P1"));

    // In the ledger, in force, on the version that was quoted.
    let policy = b.module.with(|w| w.p("P1").policy.clone());
    let cluster = b.ledger.cluster.lock().unwrap();
    let issued = &cluster.state(0).policies[&policy];
    assert_eq!((issued.status, issued.product_hash.as_str()), (PolicyStatus::InForce, b.product_hash.as_str()));
    let layer = issued.covers["life"].layers[0].id;
    drop(cluster);

    // The reservation stands in for exactly the layer the core wrote.
    let ri = b.reinsurance.module.state();
    let reservation = ri.reservation("P1/L1/life/1").unwrap();
    assert_eq!(
        reservation.status,
        ReservationStatus::Confirmed {
            policy: policy.clone(),
            cover: "life".into(),
            layers: vec![layer],
        }
    );
    let keys = b.reinsurance.sent.lock().unwrap().clone();
    assert_eq!(keys[0], "uw:P1/L1/life/1:reserve");
    assert!(keys.contains(&"uw:P1/L1/life/1:confirm".to_string()) && keys.contains(&format!("uw:{policy}:L1:factors")));

    // Everything else went to whatever is connected, and nothing is left.
    assert!(b.driver.forward.0.lock().unwrap().iter().any(|e| matches!(e, Effect::Notify { .. })));
    assert!(b.module.with(|w| w.s.outbox(0, 10).is_empty() && w.s.parked().is_empty()));
    assert_eq!(b.pass().await, Pass::default());
}

/// Purpose: each receiver in turn cannot be reached while a proposal is being carried. The pass
/// stops with an error and says so; what was delivered before the stop stays acknowledged; and
/// once the receiver is back the next pass carries on from the same effect to a policy, with
/// one policy in the ledger and one reservation at reinsurance.
/// Why it matters: outages between modules are ordinary. A driver that lost its place would
/// either leave an accepted applicant without a policy or send the same instruction as if it
/// were new. The keys on each delivery are what make stopping anywhere safe.
#[tokio::test]
async fn an_outage_stops_the_pass_and_the_next_one_carries_on() {
    let b = bench(TakesAll::default());
    b.open("P1", 500_000_00);
    b.module.with(|w| w.disclose("P1", "L1", &clean()));
    let waiting = b.module.with(|w| w.s.outbox(0, 100).len());

    // The module itself is away: nothing can be read.
    *b.module.down.lock().unwrap() = true;
    assert!(matches!(b.driver.deliver().await, Err(DriverError::Unavailable(_))));
    *b.module.down.lock().unwrap() = false;

    // Reinsurance is away: the notices ahead of the reservation go, the reservation waits.
    *b.reinsurance.down.lock().unwrap() = true;
    assert!(matches!(b.driver.deliver().await, Err(DriverError::Unavailable(_))));
    assert_eq!(b.module.with(|w| w.stage("P1", "L1")), Stage::AwaitingCapacity);
    let left = b.module.with(|w| w.s.outbox(0, 100));
    assert!(left.len() <= waiting && matches!(left[0].1, Effect::ReserveCapacity { .. }), "{left:?}");
    *b.reinsurance.down.lock().unwrap() = false;

    // The ledger is away: capacity is answered and acknowledged, the issue waits at the front.
    *b.ledger.down.lock().unwrap() = true;
    assert!(matches!(b.driver.deliver().await, Err(DriverError::Unavailable(_))));
    assert!(matches!(b.status("P1"), ProposalStatus::Issuing), "{:?}", b.status("P1"));
    assert!(matches!(b.module.with(|w| w.s.outbox(0, 1))[0].1, Effect::IssuePolicy { .. }));
    *b.ledger.down.lock().unwrap() = false;

    let pass = b.pass().await;
    assert!(pass.parked.is_empty(), "{pass:?}");
    assert!(matches!(b.status("P1"), ProposalStatus::Issued { .. }));
    assert_eq!(b.ledger.cluster.lock().unwrap().state(0).policies.len(), 1);
    let reserves = b.reinsurance.sent.lock().unwrap().iter().filter(|k| k.ends_with(":reserve")).count();
    assert_eq!(reserves, 1, "the reservation was sent once: the stop came after it was acknowledged");
}

/// Purpose: where interim accidental death cover is configured, the driver issues it in the
/// core when the proposal opens and tells the module it is in force; when the proposal becomes
/// a policy the driver cancels the interim policy in the core. Both are real policies in the
/// ledger, and at the end only the applied-for policy is in force.
/// Why it matters: interim cover is a promise made the day the applicant signs. If it were
/// never written it could not be claimed on, and if it were never ended the insurer would be on
/// risk twice for the same life with a premium for one.
#[tokio::test]
async fn interim_cover_is_written_at_the_start_and_ended_when_the_policy_is_issued() {
    let b = bench(TakesAll::default());
    b.module.with(|w| {
        let settings = Settings {
            version: 2,
            interim: Some(InterimSettings {
                product_code: "term-life".into(),
                cover_code: "life".into(),
                max_sum_insured_minor: 250_000_00,
            }),
            ..Settings::default()
        };
        w.ok(
            "config",
            Command::ActivateSettings {
                settings: Box::new(settings),
                approval: approval(),
            },
        );
    });
    b.open("P1", 500_000_00);

    let pass = b.pass().await;
    assert!(pass.parked.is_empty(), "{pass:?}");
    let interim = b.module.with(|w| w.p("P1").interim.clone().unwrap());
    assert!(interim.issued && !interim.ended);
    assert_eq!(b.ledger.cluster.lock().unwrap().state(0).policies[&interim.policy].status, PolicyStatus::InForce);

    b.module.with(|w| w.disclose("P1", "L1", &clean()));
    let pass = b.pass().await;
    assert!(pass.parked.is_empty(), "{pass:?}");
    let policy = b.module.with(|w| w.p("P1").policy.clone());
    let cluster = b.ledger.cluster.lock().unwrap();
    assert_eq!(cluster.state(0).policies[&policy].status, PolicyStatus::InForce);
    assert_ne!(cluster.state(0).policies[&interim.policy].status, PolicyStatus::InForce);
}

/// Purpose: the applicant withdraws before the driver has carried anything. The reservation is
/// still made, because the release that follows it needs something to release; the capacity
/// answer the module no longer wants is counted as stale and not parked; and the release leaves
/// reinsurance holding nothing for the proposal.
/// Why it matters: effects are carried in order, some time after they were decided. An answer
/// arriving for a proposal that has ended is normal, and treating it as a fault would fill the
/// parked list with noise that hides real ones. Capacity left reserved for a withdrawn proposal
/// would count against the life's retention until it expired.
#[tokio::test]
async fn an_answer_the_module_no_longer_wants_is_stale_and_capacity_is_released() {
    let b = bench(TakesAll::default());
    b.open("P1", 500_000_00);
    b.module.with(|w| {
        w.disclose("P1", "L1", &clean());
        w.ok("new-business", Command::Withdraw { proposal: "P1".into() });
    });

    let pass = b.pass().await;
    assert!(pass.parked.is_empty(), "{pass:?}");
    assert_eq!(pass.stale, 1, "{pass:?}");
    let ri = b.reinsurance.module.state();
    assert!(matches!(ri.reservation("P1/L1/life/1").unwrap().status, ReservationStatus::Released { .. }));
    assert!(ri.capacity("P1/L1/life/1").is_none());
    assert!(b.ledger.cluster.lock().unwrap().state(0).policies.is_empty());
}

/// Purpose: the product was repriced between quote and acceptance, so the core holds a version
/// other than the one the quote was bound to. The core refuses the issue, the driver brings the
/// refusal back as the core's answer, nothing is parked, and the proposal shows the reason and
/// waits for a new quote. No policy is written.
/// Why it matters: issuing on terms the applicant never saw is a conduct breach. The refusal is
/// an ordinary outcome with its own path for the applicant, so the driver must carry it back
/// and not treat it as something broken.
#[tokio::test]
async fn a_changed_product_comes_back_as_the_cores_refusal() {
    let b = bench(TakesAll::default());
    b.module.with(|w| {
        let mut open = w.open_command("P1", &["L1"], &[("L1", "life", "life", 500_000_00)], false);
        open.quote.product_hash = "the-version-before".into();
        w.ok("new-business", Command::OpenProposal(Box::new(open)));
        w.disclose("P1", "L1", &clean());
    });

    let pass = b.pass().await;
    assert!(pass.parked.is_empty(), "{pass:?}");
    assert!(
        matches!(b.status("P1"), ProposalStatus::IssueFailed { ref code, .. } if code == "product_changed"),
        "{:?}",
        b.status("P1")
    );
    assert!(b.ledger.cluster.lock().unwrap().state(0).policies.is_empty());
    assert!(b.driver.forward.0.lock().unwrap().iter().any(|e| matches!(e, Effect::Notify { .. })));
}

/// Purpose: group administration asks for a member's excess cover to be underwritten. An
/// accepted answer reaches it as the sum and terms to add, and a withdrawn request as "not
/// proceeded with", each under the effect's own key with "uw:" in front, as the caller
/// "underwriting". When group administration refuses the answer, the effect is parked with its
/// reason and the rest of the outbox still moves.
/// Why it matters: the scheme honours cover up to its limit while the excess is decided, and
/// the member's record waits on this answer. Group administration's own driver delivers the
/// same answers under the same key, so the key is what stops a decision being recorded twice.
#[tokio::test]
async fn an_excess_decision_reaches_group_administration_under_the_agreed_key() {
    use ic_group::command::{ExcessDecision, GroupCommand};

    let b = bench(TakesAll::default());
    let excess = |id: &str| {
        b.module.with(|w| {
            let mut open = w.open_command(id, &["L1"], &[("L1", "life", "life", 300_000_00)], false);
            open.product_code = "group-life".into();
            open.entry = Entry::GroupExcess { policy: open.policy.clone() };
            w.ok("group-admin", Command::OpenProposal(Box::new(open)));
        })
    };

    excess("grp-uw:acme:1");
    b.module.with(|w| w.disclose("grp-uw:acme:1", "L1", &clean()));
    excess("grp-uw:acme:2");
    b.module.with(|w| {
        w.ok(
            "group-admin",
            Command::Withdraw {
                proposal: "grp-uw:acme:2".into(),
            },
        )
    });
    let pass = b.pass().await;
    assert!(pass.parked.is_empty(), "{pass:?}");

    let sent = b.group.sent.lock().unwrap().clone();
    let decisions: Vec<_> = sent.iter().map(|e| (e.key.as_str(), e.caller.as_str(), e.today, &e.command)).collect();
    assert_eq!(decisions.len(), 2, "{decisions:?}");
    for (key, caller, today, _) in &decisions {
        assert!(key.starts_with("uw:grp-uw:acme:") && *caller == "underwriting" && *today == start(), "{decisions:?}");
    }
    assert!(decisions.iter().any(|d| matches!(
        d.3,
        GroupCommand::RecordExcessDecision { scheme, case: 1, decision: ExcessDecision::Accepted { sum_insured_minor: 300_000_00, loadings, exclusions, .. } }
            if scheme == "acme" && loadings.is_empty() && exclusions.is_empty()
    )));
    assert!(decisions.iter().any(|d| matches!(
        d.3,
        GroupCommand::RecordExcessDecision { scheme, case: 2, decision: ExcessDecision::NotProceeded } if scheme == "acme"
    )));

    // Refused by group administration: parked where a person can see it.
    *b.group.refuse.lock().unwrap() = true;
    excess("grp-uw:acme:3");
    b.module.with(|w| {
        w.ok(
            "group-admin",
            Command::Withdraw {
                proposal: "grp-uw:acme:3".into(),
            },
        )
    });
    let pass = b.pass().await;
    assert_eq!(pass.parked.len(), 1, "{pass:?}");
    assert!(pass.parked[0].1.contains("unknown_case"), "{pass:?}");
    assert!(b.module.with(|w| w.s.outbox(0, 10).is_empty() && w.s.parked().len() == 1));
}

/// Purpose: with nothing connected for notices, evidence and delegate files, each such effect
/// is parked with a reason that names what is missing, while capacity and the policy are still
/// carried: the proposal is issued and the outbox is empty.
/// Why it matters: a notice with no receiver must not block a policy behind it, and must not
/// vanish either. Parked with a reason, it is in front of a person and can be put back once
/// the receiver exists.
#[tokio::test]
async fn with_nothing_connected_notices_are_parked_and_the_policy_still_issues() {
    let b = bench(NothingConnected);
    b.open("P1", 500_000_00);
    b.module.with(|w| w.disclose("P1", "L1", &clean()));

    let pass = b.pass().await;
    assert!(matches!(b.status("P1"), ProposalStatus::Issued { .. }), "{:?}", b.status("P1"));
    assert!(!pass.parked.is_empty());
    assert!(pass.parked.iter().all(|(_, reason)| reason == "correspondence is not connected to underwriting"), "{pass:?}");
    assert_eq!(b.module.with(|w| (w.s.outbox(0, 10).len(), w.s.parked().len())), (0, pass.parked.len()));
}
