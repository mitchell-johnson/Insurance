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
use ic_underwriting::model::{ProposalStatus, Stage};
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
