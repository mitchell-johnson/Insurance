//! The outbox, the questions the hosted module answers, and the driver that
//! carries instructions to the core. The core is the simulator's cluster; the
//! module runs in this process as its replicated group would run it.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::NaiveDate;
use common::*;
use ic_core::reinsurance::{cession_on, reinsurer_balance};
use ic_core::{GroupId, PartyId, PolicyId, Reject};
use ic_ledger::PartitionId;
use ic_money::Currency::NZD;
use ic_product::{Frequency, Sex};
use ic_reinsurance::delivery::CoreDelivery;
use ic_reinsurance::driver::{Core, Driver, DriverError, LocalModule, Module, Pass, DRIVER};
use ic_reinsurance::host::{answer, module_spec, react, Query, MAX_ANSWER, MODULE};
use ic_reinsurance::loader::{load_rate_table, load_treaty};
use ic_reinsurance::service::Config;
use ic_reinsurance::treaty::RiskFactors;
use ic_reinsurance::{Command, Envelope, Instruction, InstructionBody, Notice};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, named_life, term_life};
use ic_sim::{Cluster, Delivery};
use serde_json::{json, Value};

/// The simulator's partitions as the driver sees the core, with switches to
/// play a refusal, a partition that has not opened and a lost connection.
#[derive(Clone)]
struct SimCore {
    cluster: Arc<Mutex<Cluster>>,
    refuse: Arc<Mutex<Option<Reject>>>,
    unopened: Arc<Mutex<Option<PartitionId>>>,
    down: Arc<Mutex<bool>>,
}

impl Core for SimCore {
    async fn business_date(&self, partition: PartitionId) -> Result<Option<NaiveDate>, DriverError> {
        if *self.unopened.lock().unwrap() == Some(partition) {
            return Ok(None);
        }
        Ok(self.cluster.lock().unwrap().business_date())
    }

    async fn send(&self, delivery: &CoreDelivery) -> Result<Result<(), Reject>, DriverError> {
        if *self.down.lock().unwrap() {
            return Err(DriverError::Unavailable("no node answered".into()));
        }
        if let Some(refusal) = self.refuse.lock().unwrap().clone() {
            return Ok(Err(refusal));
        }
        let applied = self
            .cluster
            .lock()
            .unwrap()
            .submit_keyed(delivery.partition, &delivery.key, delivery.caller, delivery.command.clone());
        Ok(applied.result)
    }
}

struct World {
    core: SimCore,
    module: LocalModule,
    driver: Driver<LocalModule, SimCore>,
    n: u64,
}

impl World {
    fn new() -> Self {
        let mut cluster = Cluster::in_memory(4, 11);
        cluster.open(d(2027, 3, 1)).unwrap();
        for cmd in shipped_activations().unwrap() {
            cluster.activate_everywhere(cmd).unwrap();
        }
        let core = SimCore {
            cluster: Arc::new(Mutex::new(cluster)),
            refuse: Arc::default(),
            unopened: Arc::default(),
            down: Arc::default(),
        };
        let module = LocalModule::default();
        let driver = Driver {
            module: module.clone(),
            core: core.clone(),
            batch: 2,
        };
        let mut w = World { core, module, driver, n: 0 };
        for (file, source) in [("retail_life_surplus.star", RETAIL), ("group_life_surplus.star", GROUP)] {
            let treaty = Box::new(load_treaty(file, source).unwrap());
            w.tell("config", Command::ActivateTreaty { treaty }).unwrap();
        }
        let table = Box::new(load_rate_table("ri_life_base.star", RATES).unwrap());
        w.tell("config", Command::ActivateRateTable { table }).unwrap();
        w.feed();
        w
    }

    fn tell(&mut self, caller: &str, command: Command) -> Result<(), String> {
        self.n += 1;
        let applied = self.module.apply(Envelope {
            key: format!("test-{}", self.n),
            caller: caller.to_string(),
            on: d(2027, 3, 1),
            command,
        });
        applied.result.map_err(|e| e.code)
    }

    /// Show the module every event the partitions have produced, as the host does.
    fn feed(&mut self) {
        let events = self.core.cluster.lock().unwrap().take_events();
        for record in events {
            for envelope in react(&record) {
                assert_eq!(self.module.apply(envelope).result, Ok(()));
            }
        }
    }

    fn issue(&mut self, n: u64, sum: i64) -> (PartitionId, PolicyId) {
        let dob = d(1987, 6, 15);
        let mut issue = term_life(n, d(2027, 3, 1), dob, Sex::Female, false, sum, None, Frequency::Monthly, None);
        issue.policyholder = PartyId("party-kiri".into());
        issue.lives = vec![named_life("life-1", "party-kiri", dob, Sex::Female, false)];
        let id = issue.policy.clone();
        self.tell(
            "new-business",
            Command::RecordRiskFactors {
                policy: id.clone(),
                life: "life-1".into(),
                factors: RiskFactors {
                    date_of_birth: Some(dob),
                    sex: Some(Sex::Female),
                    smoker: Some(false),
                },
            },
        )
        .unwrap();
        let p = {
            let mut c = self.core.cluster.lock().unwrap();
            let p = c.partition_of(&issue.group);
            c.submit(p, "new-business", issue_command(issue)).unwrap();
            p
        };
        self.feed();
        (p, id)
    }

    fn ask(&self, query: Query) -> Value {
        answer(&self.module.state(), serde_json::to_value(query).unwrap()).unwrap()
    }

    fn retained(&self, p: PartitionId, id: &PolicyId) -> Option<i64> {
        let c = self.core.cluster.lock().unwrap();
        cession_on(&c.state(p).policies[id], "life", 1, &"life-1".into(), d(2027, 3, 1)).map(|c| c.retained_minor)
    }
}

/// Purpose: a pass of the driver carries every instruction in the outbox to the core in
/// order, the outbox is empty afterwards, the policy record and the ledger say what the
/// module worked out, and a second pass has nothing to do.
/// Why it matters: this is the only path by which anything reinsurance works out reaches
/// the policy and the ledger. An instruction left in the outbox, or sent out of order, is a
/// cession or a premium the books never see.
#[tokio::test]
async fn a_pass_carries_the_outbox_to_the_core_in_order() {
    let mut w = World::new();
    let (p, id) = w.issue(1, 800_000_00);
    let waiting = w.module.state().outbox(usize::MAX).len();
    assert!(waiting >= 3, "a cession, premium and commission for each reinsurer");
    assert_eq!(w.retained(p, &id), None, "nothing has reached the core yet");

    let pass = w.driver.deliver().await.unwrap();
    assert_eq!(
        pass,
        Pass {
            delivered: waiting as u32,
            ..Pass::default()
        }
    );
    assert!(pass.clear());
    assert_eq!(w.retained(p, &id), Some(500_000_00));
    let state = w.module.state();
    assert!(state.outbox(usize::MAX).is_empty());
    let owed = state.balance(&retail(), &north());
    assert!(owed > 0);
    assert_eq!(
        reinsurer_balance(&w.core.cluster.lock().unwrap().state(p).ledger, "retail-life-surplus", "reinsurer-north", NZD),
        owed as i128
    );

    // The core's own events about what it was sent come back to the module and change nothing.
    w.feed();
    assert_eq!(w.driver.deliver().await.unwrap(), Pass::default());
    assert_eq!(w.ask(Query::Reconcile), json!([]));
}

/// Purpose: when the module or the core cannot be reached mid-pass the driver reports it,
/// acknowledges what the core had already taken, and the next pass finishes the rest
/// without anything being applied twice.
/// Why it matters: the driver runs for years and nodes fail under it. A pass that lost its
/// place would resend from the start, and one that acknowledged too much would skip
/// instructions the core never saw.
#[tokio::test]
async fn a_pass_cut_short_is_finished_by_the_next() {
    let mut w = World::new();
    let (p, id) = w.issue(1, 800_000_00);
    let waiting = w.module.state().outbox(usize::MAX).len();

    w.module.set_down(true);
    assert!(matches!(w.driver.deliver().await, Err(DriverError::Unavailable(_))));
    assert!(matches!(
        w.module.propose(react(&common_record())[0].clone()).await,
        Err(DriverError::Unavailable(_))
    ));
    w.module.set_down(false);

    *w.core.down.lock().unwrap() = true;
    let err = w.driver.deliver().await.unwrap_err();
    assert_eq!(err.to_string(), "unavailable: no node answered");
    assert_eq!(w.module.state().outbox(usize::MAX).len(), waiting);
    *w.core.down.lock().unwrap() = false;

    // The core takes the first instruction and then refuses to answer.
    let first = w.module.state().outbox(1)[0].1.clone();
    let delivery = w.module.state().to_core(&first, d(2027, 3, 1)).unwrap();
    assert_eq!(w.core.send(&delivery).await, Ok(Ok(())));
    let pass = w.driver.deliver().await.unwrap();
    assert_eq!(pass.delivered, waiting as u32, "the instruction already applied counts as delivered, once");
    assert_eq!(w.retained(p, &id), Some(500_000_00));
    let c = w.core.cluster.lock().unwrap();
    assert_eq!(
        reinsurer_balance(&c.state(p).ledger, "retail-life-surplus", "reinsurer-north", NZD),
        w.module.state().balance(&retail(), &north()) as i128
    );
}

fn common_record() -> ic_core::EventRecord {
    ic_core::EventRecord {
        id: "9-1".into(),
        partition: 9,
        seq: 1,
        business_date: d(2027, 3, 1),
        log_index: 1,
        traceparent: None,
        event: ic_core::event::Event::PolicyStatusChanged {
            policy: PolicyId::from("nobody"),
            from: ic_core::PolicyStatus::InForce,
            to: ic_core::PolicyStatus::Lapsed,
        },
    }
}

/// Purpose: an instruction the core refuses stops delivery at that instruction, opens an
/// exception that names it, and stays stopped on every pass until a person abandons it
/// with a reason; delivery then carries on, and the abandoned instruction is kept.
/// Why it matters: a refusal means the module and the core disagree about something. Passing
/// over it would leave the ledger short without anyone knowing; retrying forever in silence
/// would stop all reinsurance postings with nothing on anyone's worklist.
#[tokio::test]
async fn a_refused_instruction_stops_delivery_until_a_person_decides() {
    let mut w = World::new();
    w.issue(1, 800_000_00);
    let waiting = w.module.state().outbox(usize::MAX).len() as u32;
    let (first, _) = w.module.state().outbox(1).into_iter().map(|(s, i)| (s, i.clone())).next().unwrap();
    *w.core.refuse.lock().unwrap() = Some(Reject::new("period_closed", "the ledger period is closed"));

    for _ in 0..2 {
        let pass = w.driver.deliver().await.unwrap();
        assert_eq!((pass.delivered, pass.dropped), (0, 0));
        assert_eq!(pass.held.as_ref().map(|(seq, r)| (*seq, r.code.as_str())), Some((first, "period_closed")));
        assert!(!pass.clear());
    }
    let state = w.module.state();
    let exception = &state.exceptions()[&format!("delivery:{first}")];
    assert_eq!(exception.code, "instruction_refused");
    assert!(exception.detail.contains("period_closed: the ledger period is closed"), "{}", exception.detail);
    assert_eq!(state.outbox(usize::MAX).len() as u32, waiting);
    assert_eq!(
        state
            .notices(usize::MAX)
            .iter()
            .filter(|(_, n)| matches!(n, Notice::ExceptionOpened { code, .. } if code == "instruction_refused"))
            .count(),
        1,
        "the refusal is raised once however often it is met"
    );

    // Only a refused instruction can be abandoned, by operations, with a reason and an approver.
    let abandon = |seq, reason: &str, by: &str| Command::AbandonInstruction {
        seq,
        reason: reason.into(),
        approved_by: by.into(),
    };
    assert_eq!(w.tell("reinsurance-ops", abandon(first + 1, "r", "lee")), Err("not_refused".into()));
    assert_eq!(w.tell("reinsurance-ops", abandon(first, " ", "lee")), Err("reason_required".into()));
    assert_eq!(w.tell("reinsurance-ops", abandon(first, "r", "")), Err("reason_required".into()));
    assert_eq!(w.tell(DRIVER, abandon(first, "r", "lee")), Err("caller_not_allowed".into()));
    w.tell("reinsurance-ops", abandon(first, "posted by journal JNL-88 instead", "lee")).unwrap();
    let state = w.module.state();
    assert!(!state.exceptions().contains_key(&format!("delivery:{first}")));
    assert_eq!(w.ask(Query::Abandoned)[0]["reason"], "posted by journal JNL-88 instead");
    assert_eq!(state.abandoned()[0].seq, first);
    assert_eq!(state.abandoned()[0].approved_by, "lee");
    assert_eq!(w.tell("reinsurance-ops", abandon(first, "again", "lee")), Err("not_refused".into()));

    *w.core.refuse.lock().unwrap() = None;
    let pass = w.driver.deliver().await.unwrap();
    assert_eq!(pass.delivered, waiting - 1);
    assert!(w.module.state().outbox(usize::MAX).is_empty());
}

/// Purpose: a refusal that is acknowledged past, because the core later took the
/// instruction, closes its exception; and instructions for a partition that has no
/// business date yet wait without being reported as refused.
/// Why it matters: an exception that outlives its cause trains people to ignore the
/// worklist. A partition still being brought up is not a defect and must not page anyone.
#[tokio::test]
async fn a_refusal_that_clears_closes_its_exception_and_an_unopened_partition_waits() {
    let mut w = World::new();
    let (p, _) = w.issue(1, 800_000_00);
    *w.core.unopened.lock().unwrap() = Some(p);
    let pass = w.driver.deliver().await.unwrap();
    assert_eq!((pass.delivered, pass.waiting_on, pass.held.clone()), (0, Some(p), None));
    assert!(w.module.state().exceptions().is_empty());
    *w.core.unopened.lock().unwrap() = None;

    *w.core.refuse.lock().unwrap() = Some(Reject::new("busy", "try later"));
    assert!(w.driver.deliver().await.unwrap().held.is_some());
    assert_eq!(w.module.state().exceptions().len(), 1);
    *w.core.refuse.lock().unwrap() = None;
    assert!(w.driver.deliver().await.unwrap().clear());
    assert!(w.module.state().exceptions().is_empty());
}

/// Purpose: a cession the old partition refuses because its policy is moving is passed
/// over by the driver, not held, and the record sent on landing puts it on the policy
/// where it now lives.
/// Why it matters: a transfer is routine. If the driver treated this refusal like any
/// other, every transfer that met a cession change would stop reinsurance delivery for the
/// whole insurer until someone abandoned an instruction that was never needed.
#[tokio::test]
async fn a_cession_refused_for_a_transfer_is_passed_over_not_held() {
    let mut w = World::new();
    let (src, b) = w.issue(7, 300_000_00);
    assert!(w.driver.deliver().await.unwrap().clear());
    let dst = {
        let mut c = w.core.cluster.lock().unwrap();
        let target_group = GroupId(
            (0..)
                .map(|i| format!("grp-new-{i}"))
                .find(|g| c.partition_of(&GroupId(g.clone())) != src)
                .unwrap(),
        );
        let dst = c.partition_of(&target_group);
        c.submit(
            src,
            "transfer",
            ic_core::Command::BeginTransfer {
                policy: b.clone(),
                transfer_id: "t-1".into(),
                target_group,
                target_partition: dst,
            },
        )
        .unwrap();
        dst
    };
    w.feed();
    w.issue(1, 400_000_00);
    let pass = w.driver.deliver().await.unwrap();
    assert_eq!((pass.dropped, pass.held.clone()), (1, None));
    assert_eq!(w.retained(src, &b), Some(300_000_00));
    {
        let mut c = w.core.cluster.lock().unwrap();
        while c.messages_in_flight() > 0 {
            c.relay_messages(|_| Delivery::Deliver);
        }
    }
    w.feed();
    let pass = w.driver.deliver().await.unwrap();
    assert!(pass.clear() && pass.delivered >= 1);
    assert_eq!(w.retained(dst, &b), Some(100_000_00));
    assert!(w.module.state().exceptions().is_empty());
}

/// Purpose: the outbox numbers what the module issues, gives it up only when acknowledged,
/// and refuses an acknowledgement for something not yet issued or from anyone but the
/// driver; notices are kept and acknowledged the same way.
/// Why it matters: the outbox is the module's promise that nothing it decided is lost
/// between it and the core. An acknowledgement that could run ahead of what was issued
/// would let a faulty driver discard instructions that do not exist yet.
#[test]
fn the_outbox_gives_up_only_what_is_acknowledged() {
    let mut b = Bench::new();
    assert!(b.ri.outbox(10).is_empty());
    let applied = b.issue(3, "P1", "anna", vec![layer("life", "life", 1, "L1", 800 * K, d(2027, 2, 1))]);
    // Billed before the life's rating factors are known: unpriced, then priced.
    b.premium_due(3, "P1", 1, d(2027, 2, 1), d(2027, 3, 1), &[("life", 100_00)]);
    b.factors("P1", "L1", d(1987, 6, 15), false);
    b.premium_due(3, "P1", 2, d(2027, 3, 1), d(2027, 4, 1), &[("life", 100_00)]);
    let all: Vec<(u64, Instruction)> = b.ri.outbox(usize::MAX).into_iter().map(|(s, i)| (s, i.clone())).collect();
    assert_eq!(all.iter().map(|(s, _)| *s).collect::<Vec<_>>(), (1..=all.len() as u64).collect::<Vec<_>>());
    assert_eq!(all.iter().map(|(_, i)| i.clone()).collect::<Vec<_>>(), b.instructions);
    assert_eq!(all[0].1, applied.instructions[0]);
    assert!(matches!(all[0].1.body, InstructionBody::RecordCession { .. }));
    assert_eq!(b.ri.outbox(2).len(), 2);
    let last = all.len() as u64;

    let ack = |through| Command::AcknowledgeInstructions { through };
    assert_eq!(b.refused(DRIVER, ack(last + 1)), "unknown_instruction");
    assert_eq!(b.refused("reinsurance-ops", ack(1)), "caller_not_allowed");
    b.ok(DRIVER, ack(2));
    assert_eq!(b.ri.outbox(usize::MAX).first().map(|(s, _)| *s), Some(3));
    b.ok(DRIVER, ack(1)); // an old acknowledgement arriving late changes nothing
    assert_eq!(b.ri.outbox(usize::MAX).len() as u64, last - 2);
    let refused = |seq, code: &str| Command::InstructionRefused {
        seq,
        code: code.into(),
        message: "m".into(),
    };
    assert_eq!(b.refused(DRIVER, refused(1, "x")), "unknown_instruction");
    assert_eq!(b.refused(DRIVER, refused(3, " ")), "reason_required");
    b.ok(DRIVER, ack(last));
    assert!(b.ri.outbox(usize::MAX).is_empty());
    // Numbers are never reused.
    b.add_layer(3, "P1", layer("life", "life", 2, "L1", 100 * K, d(2027, 2, 1)));
    assert!(b.ri.outbox(1)[0].0 > last);

    let notices = b.ri.notices(usize::MAX).len() as u64;
    assert!(notices >= 1, "the unpriced first period raised an exception");
    assert_eq!(b.ri.notices(1).len(), 1);
    assert_eq!(b.refused(DRIVER, Command::AcknowledgeNotices { through: notices + 1 }), "unknown_notice");
    b.ok(DRIVER, Command::AcknowledgeNotices { through: notices });
    assert!(b.ri.notices(usize::MAX).is_empty());
    assert_eq!(b.replayed(), b.ri);
}

/// Purpose: every question the hosted module answers returns what the state holds, an
/// answer is capped, and a question it does not know is refused.
/// Why it matters: these answers are what the driver, the workbench and finance read. They
/// come over the wire as JSON, so a renamed field or a question that silently answered
/// null would only be found by whoever relied on it.
#[test]
fn the_hosted_module_answers_its_questions() {
    let mut b = Bench::new();
    b.factors("P1", "L1", d(1987, 6, 15), false);
    b.issue(3, "P1", "anna", vec![layer("life", "life", 1, "L1", 800 * K, d(2027, 2, 1))]);
    b.premium_due(3, "P1", 1, d(2027, 2, 1), d(2027, 3, 1), &[("life", 100_00)]);
    b.today = d(2027, 4, 2);
    b.ok(
        "month-end",
        Command::CutStatement {
            treaty: retail(),
            reinsurer: north(),
            period_end: d(2027, 3, 31),
        },
    );
    let ask = |b: &Bench, q: Query| answer(&b.ri, serde_json::to_value(q).unwrap()).unwrap();
    let k = key("P1", "life", 1, "L1");

    let outbox = ask(&b, Query::Outbox { limit: 1_000_000 });
    assert_eq!(outbox[0][0], 1);
    assert_eq!(outbox[0][1]["id"], b.instructions[0].id.as_str());
    assert!(outbox.as_array().unwrap().len() <= MAX_ANSWER);
    assert_eq!(ask(&b, Query::Outbox { limit: 1 }).as_array().unwrap().len(), 1);
    assert_eq!(ask(&b, Query::Notices { limit: 1 }).as_array().unwrap().len(), 1);

    let delivery = ask(&b, Query::Delivery { seq: 1, on: d(2027, 4, 2) });
    assert_eq!((&delivery[0], &delivery[2]), (&json!(3), &json!("reinsurance")));
    assert_eq!(delivery[1], b.instructions[0].id.as_str());
    assert_eq!(delivery[3]["record_cession"]["policy"], "P1");
    let no_such = answer(&b.ri, json!({"delivery": {"seq": 999, "on": "2027-04-02"}})).unwrap_err();
    assert!(no_such.contains("not waiting"), "{no_such}");

    assert_eq!(ask(&b, Query::Cession { key: k })[0][1]["retained_minor"], 500 * K);
    let life = ic_reinsurance::LifeKey::Party(PartyId::from("anna"));
    assert_eq!(ask(&b, Query::Retained { life, on: d(2027, 2, 1) }), json!(500 * K));
    let balance = ask(
        &b,
        Query::Balance {
            treaty: retail(),
            reinsurer: north(),
        },
    );
    assert_eq!(balance, json!(b.ri.balance(&retail(), &north())));
    assert!(balance.as_i64().unwrap() > 0);
    assert_eq!(ask(&b, Query::Statement { number: 1 })["net_minor"], balance);
    assert_eq!(ask(&b, Query::Statement { number: 2 }), Value::Null);
    let listing = ask(
        &b,
        Query::Listing {
            treaty: retail(),
            reinsurer: north(),
            period_start: d(2027, 1, 1),
            period_end: d(2027, 3, 31),
        },
    );
    assert_eq!(listing.as_array().unwrap().len(), 1);
    assert_eq!(
        ask(&b, Query::Exposure { on: d(2027, 3, 1) })["reinsurer-north"]["ceded_sum_at_risk_minor"],
        180 * K
    );
    assert_eq!(ask(&b, Query::SchemeConcentration { on: d(2027, 3, 1) }), json!([]));
    assert_eq!(ask(&b, Query::ConsentStatus { claim: "C1".into() }), Value::Null);
    assert_eq!(ask(&b, Query::Exceptions), json!({}));
    assert_eq!(ask(&b, Query::Abandoned), json!([]));
    assert_eq!(ask(&b, Query::Reconcile), json!([]));
    assert_eq!(ask(&b, Query::StateHash), json!(hex::encode(b.ri.state_hash().0)));

    let err = answer(&b.ri, json!({"everything": {}})).unwrap_err();
    assert!(err.starts_with("not a question of this module"), "{err}");
}

/// Purpose: the module declares itself to the node under its permanent name as a reader of
/// events, shows itself each event as a feed batch keyed by the event's own ID, and takes
/// an event shown twice once.
/// Why it matters: the name is the module's directory on every node's disk and can never
/// change. The key is what makes at-least-once delivery from the host safe: without it a
/// re-shown issue event would count a cover twice against the retention.
#[test]
fn the_module_is_hosted_as_a_reader_of_events() {
    let spec = module_spec().unwrap();
    assert_eq!((spec.name.as_str(), spec.events, MODULE), ("reinsurance", true, "reinsurance"));
    let record = common_record();
    let envelopes = react(&record);
    assert_eq!(envelopes.len(), 1);
    assert_eq!(
        (envelopes[0].key.as_str(), envelopes[0].caller.as_str(), envelopes[0].on),
        ("core:9-1", "event-feed", d(2027, 3, 1))
    );
    let module = LocalModule::default();
    assert!(!module.apply(envelopes[0].clone()).duplicate);
    let again = module.apply(envelopes[0].clone());
    assert!(again.duplicate && again.result.is_ok());
}

/// Purpose: a module that first sees a partition part-way through its events takes them
/// from there, and raises an exception saying how many it never saw; a gap after that is
/// still refused.
/// Why it matters: a module added to a running cluster starts at the oldest event the
/// partition still holds. Cover written before that is missing from every retention
/// calculation, and the insurer could over-retain without knowing unless it is said.
#[test]
fn starting_part_way_through_a_partitions_events_is_said_out_loud() {
    let mut b = Bench::new();
    let mut records = b.records(5, (0..42).map(|_| status("P0", ic_core::PolicyStatus::InForce)).collect());
    let late = records.split_off(40);
    let applied = b.ok("event-feed", Command::CoreEvents { events: vec![late[0].clone()] });
    assert!(matches!(&applied.notices[..], [Notice::ExceptionOpened { code, .. }] if code == "events_not_seen"));
    let exception = &b.ri.exceptions()["feed_start:5"];
    assert!(
        exception.detail.contains("first read at event 41") && exception.detail.contains("the 40 before it"),
        "{}",
        exception.detail
    );
    b.ok("event-feed", Command::CoreEvents { events: vec![late[1].clone()] });
    let mut more = b.records(
        5,
        vec![status("P0", ic_core::PolicyStatus::InForce), status("P0", ic_core::PolicyStatus::InForce)],
    );
    assert_eq!(b.refused("event-feed", Command::CoreEvents { events: vec![more.remove(1)] }), "gap");
    // A partition read from its first event raises nothing.
    b.issue(6, "P1", "anna", vec![layer("life", "life", 1, "L1", 100 * K, d(2027, 2, 1))]);
    assert_eq!(b.ri.exceptions().len(), 1);
}

fn status(policy: &str, to: ic_core::PolicyStatus) -> ic_core::event::Event {
    ic_core::event::Event::PolicyStatusChanged {
        policy: PolicyId::from(policy),
        from: ic_core::PolicyStatus::InForce,
        to,
    }
}

/// Purpose: the driver process reads its settings from the environment, falls back to its
/// defaults, and refuses to start without the cluster's addresses or with a setting that is
/// not a number above zero.
/// Why it matters: a driver that started with a zero pause would spin a core; one that
/// started with no cluster would look healthy and carry nothing.
#[test]
fn the_driver_process_checks_its_settings() {
    let lookup = |pairs: &'static [(&'static str, &'static str)]| move |key: &str| pairs.iter().find(|(k, _)| *k == key).map(|(_, v)| (*v).to_string());
    let config = Config::from_lookup(lookup(&[("IC_NODES", " node-1:7000,node-2:7000 ")])).unwrap();
    assert_eq!(
        config,
        Config {
            cluster: "node-1:7000,node-2:7000".into(),
            cycle_every: Duration::from_secs(2),
            patience: Duration::from_secs(10),
            batch: 100,
        }
    );
    let tuned = Config::from_lookup(lookup(&[
        ("IC_NODES", "n:1"),
        ("IC_REINSURANCE_CYCLE_MS", "250"),
        ("IC_REINSURANCE_PATIENCE_MS", "3000"),
        ("IC_REINSURANCE_BATCH", "7"),
    ]))
    .unwrap();
    assert_eq!(
        (tuned.cycle_every, tuned.patience, tuned.batch),
        (Duration::from_millis(250), Duration::from_secs(3), 7)
    );
    assert!(Config::from_lookup(lookup(&[])).unwrap_err().contains("IC_NODES"));
    assert!(Config::from_lookup(lookup(&[("IC_NODES", "  ")])).unwrap_err().contains("IC_NODES"));
    for bad in ["0", "-5", "soon"] {
        let pairs: &'static [(&str, &str)] = Box::leak(Box::new([("IC_NODES", "n:1"), ("IC_REINSURANCE_CYCLE_MS", bad)]));
        assert!(Config::from_lookup(lookup(pairs)).unwrap_err().contains("IC_REINSURANCE_CYCLE_MS"), "{bad}");
    }
}

/// Purpose: the driver's loop keeps making passes at its interval, shows each result to its
/// observer whether the pass worked or not, and returns when told to stop.
/// Why it matters: the loop is what runs in production. One that ended on the first failed
/// pass would leave reinsurance undelivered after any brief loss of the cluster.
#[tokio::test]
async fn the_loop_keeps_going_through_failures_and_stops_when_told() {
    let mut w = World::new();
    w.issue(1, 800_000_00);
    *w.core.down.lock().unwrap() = true;
    let core = w.core.clone();
    let mut seen = Vec::new();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let mut stop = Some(stop);
    w.driver
        .run(
            Duration::from_millis(1),
            async {
                let _ = stopped.await;
            },
            |pass| {
                seen.push(pass.clone());
                match seen.len() {
                    1 => *core.down.lock().unwrap() = false,
                    2 => *core.refuse.lock().unwrap() = Some(Reject::new("busy", "later")),
                    4 => {
                        if let Some(stop) = stop.take() {
                            let _ = stop.send(());
                        }
                    }
                    _ => {}
                }
            },
        )
        .await;
    assert_eq!(seen.len(), 4);
    assert!(matches!(seen[0], Err(DriverError::Unavailable(_))));
    assert!(matches!(&seen[1], Ok(pass) if pass.clear() && pass.delivered > 0));
    assert_eq!(seen[2], Ok(Pass::default()), "nothing new to carry");
    assert!(w.module.state().outbox(usize::MAX).is_empty());
}
