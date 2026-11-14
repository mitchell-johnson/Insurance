//! The driver against the real reinsurance module and the real core, each
//! in this process: a proposal goes from disclosure to a policy in the
//! ledger with nobody carrying anything by hand.

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, NaiveDate, Utc};
use common::*;
use ic_core::{PolicyId, PolicyStatus};
use ic_ledger::PartitionId;
use ic_log::StateMachine;
use ic_reinsurance::capacity::{Outcome, ReservationStatus};
use ic_reinsurance::driver::LocalModule;
use ic_reinsurance::loader::{load_rate_table, load_treaty};
use ic_sim::config::shipped_activations;
use ic_sim::Cluster;
use ic_underwriting::command::{Applied, Command, Effect, Envelope, Reject};
use ic_underwriting::config::{InterimSettings, Settings};
use ic_underwriting::driver::{Channels, Core, CoreAnswer, Driver, DriverError, Forward, NothingConnected, Pass, Refusal, Reinsurer, Schemes, Underwriting};
use ic_underwriting::model::{ConsentKind, Entry, ProposalStatus, Stage};
use ic_underwriting::progress::ProgressRow;
use ic_underwriting::reinstatement::reinstatement_proposal;
use ic_underwriting::service::channels_envelope;
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
    /// What collections lists as waiting for underwriting.
    lapsed: Arc<Mutex<Vec<PolicyId>>>,
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

    async fn lapsed_for_underwriting(&self) -> Result<Vec<ic_core::policy::Policy>, DriverError> {
        if *self.down.lock().unwrap() {
            return Err(DriverError::Unavailable("collections did not answer".into()));
        }
        let cluster = self.cluster.lock().unwrap();
        Ok(self
            .lapsed
            .lock()
            .unwrap()
            .iter()
            .filter_map(|p| cluster.state(0).policies.get(p).cloned())
            .collect())
    }
}

/// Group administration as a recorder: what it was sent, and whether it refuses.
/// The real channel module, taking what the driver sends as JSON.
#[derive(Clone)]
struct Portal {
    state: Arc<Mutex<ic_channels::state::ChannelState>>,
}

impl Default for Portal {
    fn default() -> Self {
        Portal {
            state: Arc::new(Mutex::new(ic_channels::state::ChannelState::shipped().unwrap())),
        }
    }
}

impl Portal {
    /// The rows published for a policy, by key.
    fn rows(&self, policy: &str) -> BTreeMap<String, ic_channels::feed::Row> {
        let state = self.state.lock().unwrap();
        state.data.published.get(&PolicyId::from(policy)).cloned().unwrap_or_default()
    }
}

impl Channels for Portal {
    async fn publish(&self, key: &str, at: DateTime<Utc>, rows: &[ProgressRow]) -> Result<Result<(), Refusal>, DriverError> {
        let envelope: ic_channels::state::Envelope = serde_json::from_value(channels_envelope(key, at, rows)).unwrap();
        let output = self.state.lock().unwrap().apply(0, envelope);
        Ok(output.result.map(|_| ()).map_err(|r| Refusal::new(&r.code, r.message)))
    }
}

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
    async fn forward(&self, _seq: u64, effect: &Effect) -> Result<Option<String>, DriverError> {
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
    portal: Portal,
    driver: Driver<Module, Reinsurance, Ledger, Group, F, Portal>,
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
        lapsed: Arc::default(),
    };
    let group = Group::default();
    let portal = Portal::default();
    let driver = Driver {
        underwriting: module.clone(),
        reinsurer: reinsurance.clone(),
        core: ledger.clone(),
        schemes: group.clone(),
        channels: portal.clone(),
        forward,
        batch: 3,
        clock,
    };
    Bench {
        module,
        reinsurance,
        ledger,
        group,
        portal,
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
    let layer = {
        let cluster = b.ledger.cluster.lock().unwrap();
        let issued = &cluster.state(0).policies[&policy];
        assert_eq!((issued.status, issued.product_hash.as_str()), (PolicyStatus::InForce, b.product_hash.as_str()));
        issued.covers["life"].layers[0].id
    };

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

    // The portals show the proposal issued: to everyone on the policy the proposal, and to the
    // life alone where their own application stands.
    let rows = b.portal.rows(&policy.to_string());
    let shown = &rows[&format!("{policy}/underwriting/P1")];
    assert_eq!((shown.kind.as_str(), shown.sensitive, shown.subject.clone()), ("proposal", false, None));
    assert_eq!((shown.body["status"].as_str(), shown.body["waiting_on"].clone()), (Some("issued"), Value::Null));
    let life = &rows[&format!("{policy}/underwriting/P1/L1")];
    assert_eq!(
        (life.kind.as_str(), life.sensitive, life.body["stage"].as_str()),
        ("proposal_life", true, Some("accepted"))
    );
    assert_eq!(life.subject, Some(ic_core::PartyId::from("PTY-L1")));
    assert!(b.module.with(|w| w.s.progress_pending(10).is_empty()));

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

/// Purpose: where interim accidental death cover is configured, with the shipped
/// `interim-accident` product, the driver issues it in the core when the proposal opens and tells the module it is in force; when the proposal becomes
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
                product_code: "interim-accident".into(),
                cover_code: "accidental-death".into(),
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
    {
        let cluster = b.ledger.cluster.lock().unwrap();
        let written = &cluster.state(0).policies[&interim.policy];
        assert_eq!(written.status, PolicyStatus::InForce);
        let covers: Vec<i64> = written.covers.values().map(|c| c.sum_insured_on(c.start)).collect();
        assert_eq!(covers, vec![250_000_00], "the most interim cover on a life, not the amount applied for");
    }

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
            open.entry = Entry::GroupExcess {
                policy: open.policy.clone(),
                takeover_accepted_minor: None,
                reinsurance_reference: None,
                delegated_authority: None,
            };
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
        assert!(
            key.starts_with("uw:grp-uw:acme:") && *caller == "underwriting" && *today == start(),
            "{decisions:?}"
        );
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
    assert!(
        pass.parked
            .iter()
            .all(|(_, reason)| reason == "correspondence is not connected to underwriting"),
        "{pass:?}"
    );
    assert_eq!(b.module.with(|w| (w.s.outbox(0, 10).len(), w.s.parked().len())), (0, pass.parked.len()));
}

/// Purpose: the day's sweep goes through every open proposal a page at a time and lapses the
/// ones past their expiry, telling how many pages and lapses there were. Run again on the same
/// day it reports the same and changes nothing, and the lapse notices it produced are carried
/// on the next pass like any other effect.
/// Why it matters: nothing else moves time forward for a proposal. Without the sweep an
/// abandoned application holds reinsurance capacity and interim cover for ever; and because
/// two drivers run, a sweep that did its work twice would chase a doctor twice for one report.
#[tokio::test]
async fn the_daily_sweep_pages_through_open_proposals_once_a_day() {
    let b = bench(TakesAll::default());
    b.module.with(|w| {
        w.now = DateTime::parse_from_rfc3339("2026-10-01T09:00:00Z").unwrap().with_timezone(&Utc);
    });
    for id in ["P1", "P2", "P3"] {
        b.open(id, 500_000_00);
    }
    b.pass().await;

    let swept = b.driver.sweep(2).await.unwrap();
    assert_eq!((swept.pages, swept.lapsed), (2, 3), "{swept:?}");
    for id in ["P1", "P2", "P3"] {
        assert!(matches!(b.status(id), ProposalStatus::Lapsed { .. }), "{:?}", b.status(id));
    }

    let before = b.module.with(|w| w.s.clone());
    assert_eq!(b.driver.sweep(2).await.unwrap(), swept);
    assert!(b.module.with(|w| w.s == before), "the second sweep of the day changed nothing");

    let mut day = None;
    let (pass, sweep) = b.driver.cycle(&mut day, 2).await;
    assert_eq!(sweep, Some(Ok(swept)));
    assert_eq!(day, Some(start()));
    assert!(pass.unwrap().delivered >= 3, "the lapse notices were carried");
    let (_, sweep) = b.driver.cycle(&mut day, 2).await;
    assert_eq!(sweep, None, "one sweep a day from this driver");
}

/// Purpose: more cover on a policy in force goes through underwriting like new business and
/// reaches the core as a new layer on the same policy, on the decided terms; the reservation is
/// confirmed against that layer; the proposal ends issued; and the ledger reconciles.
/// Why it matters: an increase the customer accepted and the core never wrote is cover they
/// believe they have and do not. The layer number is what ties the reinsurance placed for the
/// increase to the cover actually on risk.
#[tokio::test]
async fn an_accepted_increase_becomes_a_layer_on_the_policy_in_force() {
    let b = bench(TakesAll::default());
    b.open("P1", 500_000_00);
    b.module.with(|w| w.disclose("P1", "L1", &clean()));
    b.pass().await;
    let (policy, number, group) = b
        .module
        .with(|w| (w.p("P1").policy.clone(), w.p("P1").policy_number.clone(), w.p("P1").group.clone()));

    let more = |id: &str, sum: i64| {
        let hash = b.product_hash.clone();
        b.module.with(|w| {
            let mut open = w.open_command(id, &["L1"], &[("L1", "life", "life", sum)], false);
            open.quote.product_hash = hash;
            open.entry = Entry::Increase { policy: policy.clone() };
            open.policy = policy.clone();
            open.policy_number = number.clone();
            open.group = group.clone();
            w.ok("new-business", Command::OpenProposal(Box::new(open)));
            w.disclose(id, "L1", &clean());
        })
    };

    more("P2", 200_000_00);
    let pass = b.pass().await;
    assert!(pass.parked.is_empty(), "{pass:?}");
    assert!(matches!(b.status("P2"), ProposalStatus::Issued { .. }), "{:?}", b.status("P2"));
    {
        let cluster = b.ledger.cluster.lock().unwrap();
        let layers = &cluster.state(0).policies[&policy].covers["life"].layers;
        assert_eq!(layers.len(), 2);
        assert_eq!((layers[1].sum_insured_minor, layers[1].origin), (200_000_00, ic_core::LayerOrigin::Increase));
        assert!(cluster.reconcile().is_empty());
    }
    let ri = b.reinsurance.module.state();
    assert_eq!(
        ri.reservation("P2/L1/life/1").unwrap().status,
        ReservationStatus::Confirmed {
            policy: policy.clone(),
            cover: "life".into(),
            layers: vec![2],
        }
    );
}

/// Purpose: a future insurability option, once a person accepts the proof of the life event,
/// reaches the real core as an option exercise on the policy in force: the core writes a layer
/// on the original cover's terms, counts the use, and the proposal ends issued. A second use
/// on the same event, which the product does not allow, comes back as the core's refusal and
/// the proposal waits for a person as issue failed.
/// Why it matters: the core holds the option's limits and its uses, so the module must carry
/// the request to it rather than write an increase that skips them, and a refused use must
/// not be lost or written anyway (UW-030).
#[tokio::test]
async fn an_accepted_option_is_exercised_on_the_policy_in_force() {
    let b = bench(TakesAll::default());
    b.open("P1", 500_000_00);
    b.module.with(|w| w.disclose("P1", "L1", &clean()));
    b.pass().await;
    let (policy, number, group) = b
        .module
        .with(|w| (w.p("P1").policy.clone(), w.p("P1").policy_number.clone(), w.p("P1").group.clone()));
    let exercise = |id: &str| {
        let hash = b.product_hash.clone();
        b.module.with(|w| {
            let mut open = w.open_command(id, &["L1"], &[("L1", "life", "life", 100_000_00)], false);
            open.quote.product_hash = hash;
            open.entry = Entry::InsurabilityOption {
                policy: policy.clone(),
                event: "marriage".into(),
                event_date: w.today(),
                evidence: "vault://PTY-L1/certificate/1".into(),
            };
            open.policy = policy.clone();
            open.policy_number = number.clone();
            open.group = group.clone();
            w.ok("new-business", Command::OpenProposal(Box::new(open)));
            let case = w.take(id, "L1", "amy");
            w.ok(
                "underwriting-console",
                Command::CheckOptionEvidence {
                    case,
                    underwriter: "amy".into(),
                    accepted: true,
                    note: String::new(),
                },
            );
        })
    };

    exercise("O1");
    let pass = b.pass().await;
    assert!(pass.parked.is_empty(), "{pass:?}");
    assert!(matches!(b.status("O1"), ProposalStatus::Issued { .. }), "{:?}", b.status("O1"));
    {
        let cluster = b.ledger.cluster.lock().unwrap();
        let held = &cluster.state(0).policies[&policy];
        let layers = &held.covers["life"].layers;
        assert_eq!((layers.len(), layers[1].sum_insured_minor), (2, 100_000_00));
        assert!(cluster.reconcile().is_empty());
    }

    exercise("O2");
    b.pass().await;
    assert!(
        matches!(b.status("O2"), ProposalStatus::IssueFailed { code } if code == "option_used"),
        "{:?}",
        b.status("O2")
    );
    let cluster = b.ledger.cluster.lock().unwrap();
    assert_eq!(cluster.state(0).policies[&policy].covers["life"].layers.len(), 2, "one use for the event");
}

/// Purpose: a customer's new adviser replaces their cover and tops it up. The application is
/// the increase alone, advised as a replacement: once the customer confirms the replacement
/// disclosure, only the increase is underwritten and the core writes it as a second layer on the
/// same cover. The cover in force is untouched: same layer, same start, so no gap and no new
/// waiting period, and the customer holds one cover with one sum insured.
/// Why it matters: a replacement written as a cancellation and a new policy would leave the
/// customer uncovered between the two, restart their waiting periods and underwrite what was
/// already accepted (ADV-027).
#[tokio::test]
async fn a_replacement_with_a_top_up_underwrites_only_the_increase_on_the_same_cover() {
    let b = bench(TakesAll::default());
    b.open("P1", 500_000_00);
    b.module.with(|w| w.disclose("P1", "L1", &clean()));
    b.pass().await;
    let (policy, number, group) = b
        .module
        .with(|w| (w.p("P1").policy.clone(), w.p("P1").policy_number.clone(), w.p("P1").group.clone()));
    let first = {
        let cluster = b.ledger.cluster.lock().unwrap();
        cluster.state(0).policies[&policy].covers["life"].layers[0].clone()
    };

    let hash = b.product_hash.clone();
    b.module.with(|w| {
        let mut open = w.open_command("P2", &["L1"], &[("L1", "life", "life", 200_000_00)], true);
        open.quote.product_hash = hash;
        open.entry = Entry::Increase { policy: policy.clone() };
        open.policy = policy.clone();
        open.policy_number = number.clone();
        open.group = group.clone();
        open.replacement = true;
        w.ok("new-business", Command::OpenProposal(Box::new(open)));
        let confirm = w.ok(
            "new-business",
            Command::RecordConsent {
                proposal: "P2".into(),
                life: "L1".into(),
                kind: ConsentKind::ReplacementDisclosure,
                granted: true,
                signed: Some(signed("consent", &["P2", "L1", "replacement_disclosure"])),
            },
        );
        assert!(!confirm.events.is_empty());
        let disclosed = w.disclose("P2", "L1", &clean());
        let sums: Vec<i64> = disclosed
            .effects
            .iter()
            .filter_map(|e| match e {
                Effect::ReserveCapacity { sum_insured_minor, .. } => Some(*sum_insured_minor),
                _ => None,
            })
            .collect();
        assert_eq!(sums, vec![200_000_00], "only the increase is underwritten and placed");
    });
    let pass = b.pass().await;
    assert!(pass.parked.is_empty(), "{pass:?}");
    assert!(matches!(b.status("P2"), ProposalStatus::Issued { .. }), "{:?}", b.status("P2"));
    let cluster = b.ledger.cluster.lock().unwrap();
    let held = &cluster.state(0).policies[&policy];
    assert_eq!(held.covers.keys().collect::<Vec<_>>(), vec!["life"], "one cover");
    let cover = &held.covers["life"];
    assert_eq!(cover.layers[0], first, "the cover in force is untouched: no gap, no new waiting period");
    assert_eq!(cover.layers[1].origin, ic_core::LayerOrigin::Increase);
    assert_eq!(cover.sum_insured_on(cover.layers[1].start), 700_000_00, "one sum insured");
    assert!(cluster.reconcile().is_empty());
}

/// Purpose: a change of terms on cover already in force goes through underwriting and reaches
/// the core as the same cover rewritten: the old layer ends, a new one carries the decided
/// loading with the same sum insured and origin, and the proposal ends issued. A proposal that
/// comes out on the terms the cover already has writes nothing and still ends issued. Neither
/// reserves reinsurance capacity.
/// Why it matters: an alteration the customer accepted and the core never wrote leaves them
/// paying, and claiming, on terms underwriting has replaced; one that parked for a person every
/// time the answer was "no change" would fill the queue with work that is already done; and
/// reserving capacity again for cover already ceded would count the life twice against the
/// retention and send ordinary alterations to the reinsurer.
#[tokio::test]
async fn an_accepted_alteration_rewrites_the_cover_on_the_policy_in_force() {
    let b = bench(TakesAll::default());
    b.open("P1", 500_000_00);
    b.module.with(|w| w.disclose("P1", "L1", &clean()));
    b.pass().await;
    let (policy, number, group) = b
        .module
        .with(|w| (w.p("P1").policy.clone(), w.p("P1").policy_number.clone(), w.p("P1").group.clone()));
    let alter = |id: &str, smoked: bool| {
        let hash = b.product_hash.clone();
        b.module.with(|w| {
            let mut open = w.open_command(id, &["L1"], &[("L1", "life", "life", 500_000_00)], false);
            open.quote.product_hash = hash;
            open.entry = Entry::Alteration { policy: policy.clone() };
            open.policy = policy.clone();
            open.policy_number = number.clone();
            open.group = group.clone();
            w.ok("new-business", Command::OpenProposal(Box::new(open)));
            w.disclose(id, "L1", &if smoked { yes("smoked_last_12_months") } else { clean() });
        })
    };

    // Nothing new disclosed: standard terms, which the cover already has.
    alter("P2", false);
    let pass = b.pass().await;
    assert!(pass.parked.is_empty(), "{pass:?}");
    assert!(matches!(b.status("P2"), ProposalStatus::Issued { .. }), "{:?}", b.status("P2"));
    assert_eq!(b.ledger.cluster.lock().unwrap().state(0).policies[&policy].covers["life"].layers.len(), 1);

    alter("P3", true);
    b.pass().await;
    b.module.with(|w| w.respond("P3", "L1", ic_underwriting::command::OfferResponse::Accept));
    let pass = b.pass().await;
    assert!(pass.parked.is_empty(), "{pass:?}");
    assert!(matches!(b.status("P3"), ProposalStatus::Issued { .. }), "{:?}", b.status("P3"));
    let cluster = b.ledger.cluster.lock().unwrap();
    let layers = &cluster.state(0).policies[&policy].covers["life"].layers;
    assert_eq!(layers.len(), 2);
    assert_eq!((layers[0].end, layers[1].start, layers[1].end), (Some(start()), start(), None));
    assert_eq!(
        (layers[1].sum_insured_minor, layers[1].origin, layers[1].loadings[0].as_str()),
        (500_000_00, ic_core::LayerOrigin::Original, "0.50")
    );
    assert!(cluster.reconcile().is_empty());

    // The cover is already ceded and its sum does not change: nothing more is reserved.
    let ri = b.reinsurance.module.state();
    assert!(ri.reservation("P1/L1/life/1").is_some());
    assert!(ri.reservation("P2/L1/life/1").is_none() && ri.reservation("P3/L1/life/1").is_none());
}

/// Purpose: a change of occupation after issue goes through underwriting as an alteration. Once
/// the new terms are accepted, the core is sent the life's new occupation class with them: the
/// old layer ends and a new one starts on the decided terms, and the life is rated on the new
/// class from that day while the old class stands for the days before (UW-020).
/// Why it matters: a riskier occupation priced on the old class is cover sold below its cost,
/// and a class changed back to the start of the policy would rewrite premiums already paid.
#[tokio::test]
async fn a_change_of_occupation_reaches_the_core_with_the_new_class() {
    let b = bench(TakesAll::default());
    b.open("P1", 500_000_00);
    b.module.with(|w| w.disclose("P1", "L1", &clean()));
    b.pass().await;
    let (policy, number, group) = b
        .module
        .with(|w| (w.p("P1").policy.clone(), w.p("P1").policy_number.clone(), w.p("P1").group.clone()));
    let hash = b.product_hash.clone();
    b.module.with(|w| {
        let mut open = w.open_command("P2", &["L1"], &[("L1", "life", "life", 500_000_00)], false);
        open.quote.product_hash = hash;
        open.entry = Entry::Alteration { policy: policy.clone() };
        open.policy = policy.clone();
        open.policy_number = number.clone();
        open.group = group.clone();
        open.lives[0].factors.occupation_class = Some("heavy".into());
        w.ok("new-business", Command::OpenProposal(Box::new(open)));
        w.disclose("P2", "L1", &yes("smoked_last_12_months"));
    });
    b.pass().await;
    b.module.with(|w| w.respond("P2", "L1", ic_underwriting::command::OfferResponse::Accept));
    let pass = b.pass().await;
    assert!(pass.parked.is_empty(), "{pass:?}");
    assert!(matches!(b.status("P2"), ProposalStatus::Issued { .. }), "{:?}", b.status("P2"));
    let cluster = b.ledger.cluster.lock().unwrap();
    let held = &cluster.state(0).policies[&policy];
    let factors = &held.lives[&ic_core::LifeId::from("L1")].factors;
    assert_eq!(factors.as_of(start()).unwrap().occupation_class.as_deref(), Some("heavy"));
    let layers = &held.covers["life"].layers;
    assert_eq!((layers.len(), layers[1].loadings[0].as_str()), (2, "0.50"));
    assert!(cluster.reconcile().is_empty());
}

/// Purpose: a leaver's continued cover above the automatic limit reaches the core as a layer
/// on the continuation policy group administration issued, on the terms the member's scheme
/// excess was accepted on, and the end of the proposal is left for group administration to read.
/// One taken up after its inception is backdated to it: the core starts the layer on the inception.
/// Why it matters: the leaver pays for this cover from the day scheme cover ended; a layer
/// that started on the day of take-up would leave them without the underwritten amount in
/// between while they believed they had it.
#[tokio::test]
async fn a_continued_excess_becomes_a_layer_on_the_continuation_policy() {
    let b = bench(TakesAll::default());
    b.open("P1", 500_000_00);
    b.module.with(|w| w.disclose("P1", "L1", &clean()));
    b.pass().await;
    let (policy, number, group) = b
        .module
        .with(|w| (w.p("P1").policy.clone(), w.p("P1").policy_number.clone(), w.p("P1").group.clone()));
    let hash = b.product_hash.clone();
    b.module.with(|w| {
        let mut open = w.open_command("grp-uw:S1:1", &["L1"], &[("L1", "life", "group_life", 300_000_00)], false);
        open.product_code = "group-life".into();
        open.entry = Entry::GroupExcess {
            policy: PolicyId::from("SCHEME-1"),
            takeover_accepted_minor: None,
            reinsurance_reference: None,
            delegated_authority: None,
        };
        open.policy = PolicyId::from("SCHEME-1");
        w.ok("group-admin", Command::OpenProposal(Box::new(open)));
        w.disclose("grp-uw:S1:1", "L1", &yes("smoked_last_12_months"));
    });
    b.pass().await;
    b.module
        .with(|w| w.respond("grp-uw:S1:1", "L1", ic_underwriting::command::OfferResponse::Accept));
    b.pass().await;
    // Group administration wrote the excess on the scheme policy.
    b.module.with(|w| {
        w.ok(
            "issuer",
            Command::PolicyIssued {
                proposal: "grp-uw:S1:1".into(),
                layers: vec![ic_underwriting::command::IssuedLayer {
                    life: "L1".into(),
                    cover: "life".into(),
                    layer: 2,
                }],
            },
        )
    });
    b.pass().await;
    let continuation = |id: &str, inception: NaiveDate| {
        let hash = hash.clone();
        let (policy, number, group) = (policy.clone(), number.clone(), group.clone());
        b.module.with(move |w| {
            let mut open = w.open_command(id, &["L1"], &[("L1", "life", "life", 200_000_00)], false);
            open.quote.product_hash = hash;
            open.channel = "group-continuation".into();
            open.entry = Entry::GroupContinuation {
                policy: policy.clone(),
                scheme_policy: PolicyId::from("SCHEME-1"),
                scheme_life: "M7".into(),
                inception,
                earlier: vec!["grp-uw:S1:1".into()],
            };
            open.policy = policy;
            open.policy_number = number;
            open.group = group;
            w.ok("group-admin", Command::OpenProposal(Box::new(open)));
        })
    };
    continuation("grp-cont:S1:1", start());
    let pass = b.pass().await;
    assert!(pass.parked.is_empty(), "{pass:?}");
    assert!(
        matches!(b.status("grp-cont:S1:1"), ProposalStatus::Issued { .. }),
        "{:?}",
        b.status("grp-cont:S1:1")
    );
    {
        let cluster = b.ledger.cluster.lock().unwrap();
        let layers = &cluster.state(0).policies[&policy].covers["life"].layers;
        assert_eq!(layers.len(), 2);
        assert_eq!(
            (layers[1].sum_insured_minor, layers[1].loadings[0].as_str(), layers[1].start),
            (200_000_00, "0.50", start())
        );
        assert!(cluster.reconcile().is_empty());
    }

    // Taken up ten days after its inception: the layer is backdated to the inception.
    {
        let mut cluster = b.ledger.cluster.lock().unwrap();
        let mut date = start();
        while date < start() + chrono::Duration::days(10) {
            date = date.succ_opt().unwrap();
            cluster.submit(0, "leader", ic_core::Command::AdvanceBusinessDate { to: date }).unwrap();
        }
    }
    continuation("grp-cont:S1:2", start());
    let pass = b.pass().await;
    assert!(pass.parked.is_empty(), "{pass:?}");
    let cluster = b.ledger.cluster.lock().unwrap();
    let layers = &cluster.state(0).policies[&policy].covers["life"].layers;
    assert_eq!((layers.len(), layers[2].start), (3, start()), "starts on the inception, not the take-up");
    assert!(cluster.reconcile().is_empty());
}

/// Purpose: a lapsed policy goes through underwriting for reinstatement, opened by collections
/// with the proposal built from the policy as the core holds it (its lives, the cover held the
/// day before the lapse, its product version; nothing for a policy that is not lapsed), and the decision
/// reaches the core as an approval recorded against the lapse, carrying the decided loading
/// and good for as long as the proposal is; no reinsurance capacity is reserved for cover
/// already ceded; and the proposal ends issued with nothing left in the outbox.
/// Why it matters: collections can only reinstate a long-lapsed policy once the core holds
/// underwriting's decision. A decision that stayed in underwriting's outbox would leave a
/// customer who has paid their arrears and passed underwriting unable to get their cover back.
#[tokio::test]
async fn a_reinstatement_decision_is_recorded_against_the_lapse() {
    let b = bench(TakesAll::default());
    b.open("P1", 500_000_00);
    b.module.with(|w| w.disclose("P1", "L1", &clean()));
    b.pass().await;
    let (policy, number, group) = b
        .module
        .with(|w| (w.p("P1").policy.clone(), w.p("P1").policy_number.clone(), w.p("P1").group.clone()));
    {
        let mut cluster = b.ledger.cluster.lock().unwrap();
        for _ in 0..95 {
            cluster.end_of_day().unwrap();
        }
        assert_eq!(cluster.state(0).policies[&policy].status, PolicyStatus::Lapsed);
    }
    // What collections sends: the proposal built from the lapsed policy as the core holds it.
    let mut open = {
        let cluster = b.ledger.cluster.lock().unwrap();
        let lapsed = &cluster.state(0).policies[&policy];
        let mut in_force = lapsed.clone();
        in_force.status = PolicyStatus::InForce;
        assert_eq!(reinstatement_proposal(&in_force, d(2027, 6, 1)), None);
        reinstatement_proposal(lapsed, d(2027, 6, 1)).expect("a lapsed policy with named lives")
    };
    assert!(open.proposal.0.starts_with(&format!("reinstate:{policy}:")));
    assert_eq!(
        (open.policy_number.as_str(), &open.group, open.channel.as_str()),
        (number.as_str(), &group, "collections")
    );
    assert_eq!(open.quote.product_hash, b.product_hash);
    assert_eq!(
        open.covers
            .iter()
            .map(|c| (c.code.as_str(), c.life.0.as_str(), c.sum_insured_minor))
            .collect::<Vec<_>>(),
        vec![("life", "L1", 500_000_00)]
    );
    assert_eq!(open.lives.len(), 1);
    // Kept as P2 for the steps below.
    open.proposal = "P2".into();
    let expires = b.module.with(|w| {
        w.days(95);
        w.ok("collections", Command::OpenProposal(Box::new(open)));
        w.disclose("P2", "L1", &yes("smoked_last_12_months"));
        w.p("P2").expires_on
    });
    b.pass().await;
    b.module.with(|w| w.respond("P2", "L1", ic_underwriting::command::OfferResponse::Accept));
    let pass = b.pass().await;
    assert!(pass.parked.is_empty(), "{pass:?}");
    assert!(matches!(b.status("P2"), ProposalStatus::Issued { .. }), "{:?}", b.status("P2"));

    let cluster = b.ledger.cluster.lock().unwrap();
    let held = &cluster.state(0).policies[&policy];
    assert_eq!(held.status, PolicyStatus::Lapsed, "the decision alone reinstates nothing");
    let approval = held.lapse.as_ref().unwrap().approval.clone().expect("the decision is with the lapse");
    assert_eq!((approval.reference.as_str(), approval.valid_until), ("P2/issue/1", expires));
    assert_eq!(
        (approval.terms.len(), approval.terms[0].cover.as_str(), approval.terms[0].loadings[0].as_str()),
        (1, "life", "0.50")
    );
    assert!(b.reinsurance.module.state().reservation("P2/L1/life/1").is_none());
}

/// Purpose: collections lists the lapsed policies whose reinstatement the core refused for want
/// of underwriting. The day's sweep opens a reinstatement proposal for each, as collections,
/// from the policy as the core holds it; a policy already opened is not opened again, one that
/// is no longer lapsed is reported and skipped, and when collections cannot be asked the sweep
/// still stands and says why.
/// Why it matters: a customer who has paid their arrears is waiting on a decision nobody has
/// started. Without this step the core's refusal sits on a collections task until a person
/// re-keys the application.
#[tokio::test]
async fn the_daily_sweep_opens_a_reinstatement_for_each_lapse_collections_lists() {
    let b = bench(TakesAll::default());
    b.open("P1", 500_000_00);
    b.module.with(|w| w.disclose("P1", "L1", &clean()));
    b.pass().await;
    let lapsed = b.module.with(|w| w.p("P1").policy.clone());
    *b.ledger.lapsed.lock().unwrap() = vec![lapsed.clone()];
    // Listed while still in force: nothing to reinstate.
    let early = b.driver.sweep(10).await.unwrap();
    assert_eq!((early.reinstatements, early.not_reinstated.len()), (0, 1));
    assert_eq!(early.not_reinstated[0].0, lapsed);
    {
        let mut cluster = b.ledger.cluster.lock().unwrap();
        for _ in 0..95 {
            cluster.end_of_day().unwrap();
        }
        assert_eq!(cluster.state(0).policies[&lapsed].status, PolicyStatus::Lapsed);
    }

    let swept = b.driver.sweep(10).await.unwrap();
    assert_eq!(
        (swept.reinstatements, swept.not_reinstated.len(), swept.reinstatements_unavailable.clone()),
        (1, 0, None)
    );
    let id = {
        let cluster = b.ledger.cluster.lock().unwrap();
        reinstatement_proposal(&cluster.state(0).policies[&lapsed], start()).unwrap().proposal
    };
    let opened = b.module.with(|w| {
        let p = w.p(&id.0);
        (p.channel.clone(), p.status.clone(), p.entry.clone())
    });
    assert_eq!(
        opened,
        ("collections".to_string(), ProposalStatus::Open, Entry::Reinstatement { policy: lapsed.clone() })
    );

    // Asked again, nothing new is opened.
    let again = b.driver.sweep(10).await.unwrap();
    assert_eq!(again.reinstatements, 0);
    // Collections out of reach: the sweep stands and says so.
    *b.ledger.down.lock().unwrap() = true;
    let unreachable = b.driver.sweep(10).await.unwrap();
    assert!(unreachable.reinstatements_unavailable.unwrap().contains("collections did not answer"));
}

/// Purpose: a non-disclosure review that ends in avoidance reaches the core: claims opens the
/// review on a policy in force, two seniors who never touched the life decide to avoid it and
/// keep the premiums, and the driver alone carries the remedy so that the core holds the policy
/// avoided with nothing parked and the books reconciling.
/// Why it matters: avoiding a policy is decided in underwriting and done in the core. A remedy
/// that stayed in the outbox would leave a policy the insurer has avoided still in force,
/// billing the customer and open to claims.
#[tokio::test]
async fn an_avoidance_decided_in_review_reaches_the_core() {
    let b = bench(TakesAll::default());
    b.open("P1", 500_000_00);
    b.module.with(|w| w.disclose("P1", "L1", &clean()));
    b.pass().await;
    let policy = b.module.with(|w| w.p("P1").policy.clone());
    assert_eq!(b.ledger.cluster.lock().unwrap().state(0).policies[&policy].status, PolicyStatus::InForce);

    b.module.with(|w| {
        w.ok(
            "claims",
            Command::OpenNonDisclosureReview {
                claim_ref: "CLM-1".into(),
                policy: policy.clone(),
                life: "L1".into(),
                opened_by: "cl-1".into(),
            },
        );
        let review = "ndr/CLM-1/L1".to_string();
        w.ok(
            "underwriting-console",
            Command::TakeCase {
                case: review.clone(),
                underwriter: "sam".into(),
            },
        );
        w.ok(
            "underwriting-console",
            Command::RecordNonDisclosureOutcome {
                review,
                outcome: ic_underwriting::command::NonDisclosureOutcome::Avoid,
                decided_by: "sam".into(),
                countersigned_by: Some("sue".into()),
                reasons: set(&["RC-0980"]),
                varied_terms: vec![],
                premium: Some(ic_underwriting::command::PremiumOnAvoidance::Retain),
            },
        );
    });
    let pass = b.pass().await;
    assert!(pass.parked.is_empty(), "{pass:?}");
    {
        let cluster = b.ledger.cluster.lock().unwrap();
        assert_eq!(cluster.state(0).policies[&policy].status, PolicyStatus::Avoided);
        assert!(cluster.reconcile().is_empty());
    }
    assert_eq!(b.pass().await, Pass::default(), "nothing is left to carry");
}

/// Refuses everything offered, so it is parked with the reason.
#[derive(Clone, Default)]
struct RefusesAll;

impl Forward for RefusesAll {
    async fn forward(&self, _seq: u64, _effect: &Effect) -> Result<Option<String>, DriverError> {
        Ok(Some("nobody takes this here".into()))
    }
}

/// Purpose: the driver's run loop keeps cycling until told to stop, showing each cycle's
/// results: a pass that parks effects is reported with what it parked, and a cycle whose
/// delivery or daily sweep cannot reach the module is reported as failed, and the loop carries
/// on to the next cycle rather than end.
/// Why it matters: the service runs this loop for as long as it is up. A loop that stopped on
/// the first unreachable node would leave every later decision undelivered until someone
/// restarted it, and parked effects nobody hears of are effects nobody puts back.
#[tokio::test]
async fn the_run_loop_reports_parked_and_failed_cycles_and_carries_on() {
    let b = bench(RefusesAll);
    b.open("P1", 500_000_00);
    b.module.with(|w| w.disclose("P1", "L1", &clean()));
    let mut parked = 0;
    b.driver
        .run(
            std::time::Duration::from_millis(5),
            10,
            tokio::time::sleep(std::time::Duration::from_millis(30)),
            |pass, _| parked += pass.as_ref().map_or(0, |p| p.parked.len()),
        )
        .await;
    assert!(parked > 0, "the staff notices had nowhere to go");

    *b.module.down.lock().unwrap() = true;
    let (mut cycles, mut failed_passes, mut failed_sweeps) = (0, 0, 0);
    b.driver
        .run(
            std::time::Duration::from_millis(5),
            10,
            tokio::time::sleep(std::time::Duration::from_millis(30)),
            |pass, sweep| {
                cycles += 1;
                failed_passes += usize::from(pass.is_err());
                failed_sweeps += usize::from(matches!(sweep, Some(Err(_))));
            },
        )
        .await;
    assert!(cycles > 1, "carried on after a failed cycle");
    assert!(failed_passes > 0 && failed_sweeps > 0, "a cycle that cannot reach the module is reported");
}
