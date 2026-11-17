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
use ic_reinsurance::driver::{group_command, handed, payments_command, Audiences, Core, Driver, DriverError, Handed, LocalModule, Module, Pass, Round, DRIVER};
use ic_reinsurance::host::{answer, module_spec, react, Query, MAX_ANSWER, MODULE};
use ic_reinsurance::loader::{load_rate_table, load_treaty};
use ic_reinsurance::machine::Audience;
use ic_reinsurance::service::Config;
use ic_reinsurance::treaty::ExcessPath;
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
    /// When set, `refuse` applies only to the delivery with this key.
    refuse_only: Arc<Mutex<Option<String>>>,
    unopened: Arc<Mutex<Option<PartitionId>>>,
    down: Arc<Mutex<bool>>,
}

impl Core for SimCore {
    async fn business_date(&self, partition: PartitionId) -> Result<Option<NaiveDate>, DriverError> {
        if *self.down.lock().unwrap() {
            return Err(DriverError::Unavailable("no node answered".into()));
        }
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
            if self.refuse_only.lock().unwrap().as_ref().is_none_or(|only| *only == delivery.key) {
                return Ok(Err(refusal));
            }
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
            refuse_only: Arc::default(),
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

/// Purpose: an instruction the core refuses holds its own lane (the cover's cession record,
/// or money on its partition) at that instruction, opens an exception that names it, and
/// stays held on every pass until a person abandons it with a reason; every other lane is
/// delivered meanwhile, past it in the outbox, and the held lane carries on once it is
/// abandoned, with the abandoned instruction kept.
/// Why it matters: a refusal means the module and the core disagree about something. Passing
/// over it would leave the ledger short without anyone knowing; retrying forever in silence
/// would stop all reinsurance postings with nothing on anyone's worklist; and holding every
/// cover behind one bad entry would let one defect stop reinsurance for the whole insurer.
#[tokio::test]
async fn a_refused_instruction_holds_its_lane_until_a_person_decides() {
    let mut w = World::new();
    w.issue(1, 800_000_00);
    w.issue(2, 900_000_00);
    let outbox: Vec<(u64, ic_reinsurance::Instruction)> = w.module.state().outbox(usize::MAX).into_iter().map(|(s, i)| (s, i.clone())).collect();
    let waiting = outbox.len() as u32;
    let (first, refused) = outbox[0].clone();
    let behind = outbox.iter().skip(1).filter(|(_, i)| i.lane() == refused.lane()).count() as u32;
    assert!(outbox.iter().any(|(_, i)| i.lane() != refused.lane()), "the test needs a second lane");
    *w.core.refuse_only.lock().unwrap() = Some(w.module.state().to_core(&refused, d(2027, 3, 1)).unwrap().key);
    *w.core.refuse.lock().unwrap() = Some(Reject::new("period_closed", "the ledger period is closed"));

    let pass = w.driver.deliver().await.unwrap();
    assert_eq!((pass.delivered, pass.dropped), (waiting - 1 - behind, 0));
    assert_eq!(
        pass.held.iter().map(|(seq, r)| (*seq, r.code.as_str())).collect::<Vec<_>>(),
        vec![(first, "period_closed")]
    );
    assert!(!pass.clear());
    let pass = w.driver.deliver().await.unwrap();
    assert_eq!(
        (pass.delivered, pass.held.len()),
        (0, 1),
        "the refused instruction is offered again, and nothing behind it"
    );
    let state = w.module.state();
    let exception = &state.exceptions()[&format!("delivery:{first}")];
    assert_eq!(exception.code, "instruction_refused");
    assert!(exception.detail.contains("period_closed: the ledger period is closed"), "{}", exception.detail);
    assert!(exception.detail.contains(&refused.lane()), "{}", exception.detail);
    assert_eq!(state.outbox(usize::MAX).len() as u32, 1 + behind);
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
    assert_eq!(pass.delivered, behind);
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
    assert_eq!((pass.delivered, pass.waiting_on, pass.held.clone()), (0, Some(p), vec![]));
    assert!(w.module.state().exceptions().is_empty());
    *w.core.unopened.lock().unwrap() = None;

    *w.core.refuse.lock().unwrap() = Some(Reject::new("busy", "try later"));
    let held = w.driver.deliver().await.unwrap().held;
    assert!(!held.is_empty());
    assert_eq!(w.module.state().exceptions().len(), held.len(), "one exception for the head of each lane");
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
    assert_eq!((pass.dropped, pass.held.clone()), (1, vec![]));
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
/// driver, by number as well as through a number; notices are kept and acknowledged the same way.
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
    // Past a held instruction, delivered ones are acknowledged by number; the rest stay.
    let delivered = |seqs: Vec<u64>| Command::AcknowledgeDelivered { seqs };
    assert_eq!(b.refused(DRIVER, delivered(vec![4, last + 1])), "unknown_instruction");
    assert_eq!(b.refused("reinsurance-ops", delivered(vec![4])), "caller_not_allowed");
    assert_eq!(b.ri.outbox(usize::MAX).len() as u64, last - 2, "a refused acknowledgement removes nothing");
    b.ok(DRIVER, delivered(vec![4, 1]));
    assert_eq!(b.ri.outbox(usize::MAX).iter().map(|(s, _)| *s).take(2).collect::<Vec<_>>(), vec![3, 5]);
    assert_eq!(b.ri.outbox_after(Some(3), 1).first().map(|(s, _)| *s), Some(5));
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
    assert_eq!(b.refused(DRIVER, Command::AcknowledgeNotices { seqs: vec![notices + 1] }), "unknown_notice");
    b.ok(DRIVER, Command::AcknowledgeNotices { seqs: (1..=notices).collect() });
    assert!(b.ri.notices(usize::MAX).is_empty());
    assert_eq!(b.replayed(), b.ri);
}

/// Purpose: every question the hosted module answers returns what the state holds, an
/// answer is capped, and a question it does not know is refused; a balance asked of a
/// treaty that does not exist, or of a reinsurer not on its panel, is no answer rather
/// than 0; and the exposure names each reinsurer's treaties.
/// Why it matters: these answers are what the driver, the workbench and finance read. They
/// come over the wire as JSON, so a renamed field or a question that silently answered
/// null would only be found by whoever relied on it. An auditor asking a misspelt treaty's
/// balance was told 0, and could not find a treaty's ID anywhere (found by the scenario
/// run, F2-18-6).
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

    let outbox = ask(&b, Query::Outbox { limit: 1_000_000, after: None });
    assert_eq!(outbox[0][0], 1);
    assert_eq!(outbox[0][1]["id"], b.instructions[0].id.as_str());
    assert!(outbox.as_array().unwrap().len() <= MAX_ANSWER);
    assert_eq!(ask(&b, Query::Outbox { limit: 1, after: None }).as_array().unwrap().len(), 1);
    assert_eq!(ask(&b, Query::Notices { audience: None, limit: 1 }).as_array().unwrap().len(), 1);

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
    // No such treaty, or a reinsurer not on its panel: no answer rather than a balance of 0 (F2-18-6).
    for (treaty, reinsurer) in [("no-such-treaty", "reinsurer-north"), ("retail-life-surplus", "no-such")] {
        let unknown = Query::Balance {
            treaty: ic_reinsurance::TreatyId::from(treaty),
            reinsurer: ic_reinsurance::ReinsurerId::from(reinsurer),
        };
        assert_eq!(ask(&b, unknown), Value::Null, "{treaty} {reinsurer}");
    }
    // The exposure names each reinsurer's treaties, so the treaty a balance needs is found there.
    assert!(ask(&b, Query::Exposure { on: d(2027, 3, 1) })["reinsurer-north"]["treaties"]
        .as_array()
        .unwrap()
        .contains(&json!(retail())));
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
/// defaults, and refuses to start without the cluster's addresses, without the storage
/// account and Event Hubs namespace, or with a setting that is not a number above zero.
/// Why it matters: a driver that started with a zero pause would spin a core; one that
/// started with no cluster would look healthy and carry nothing; and one that started with
/// nowhere to file statements would leave them in the outbox with nobody the wiser.
#[test]
fn the_driver_process_checks_its_settings() {
    let lookup = |pairs: &'static [(&'static str, &'static str)]| move |key: &str| pairs.iter().find(|(k, _)| *k == key).map(|(_, v)| (*v).to_string());
    let config = Config::from_lookup(lookup(&[
        ("IC_NODES", " node-1:7000,node-2:7000 "),
        ("IC_BLOB_ENDPOINT", "https://acct.blob.core.windows.net"),
        ("IC_EVENT_HUBS_ENDPOINT", "https://ns.servicebus.windows.net"),
    ]))
    .unwrap();
    assert_eq!(
        config,
        Config {
            cluster: "node-1:7000,node-2:7000".into(),
            cycle_every: Duration::from_secs(2),
            patience: Duration::from_secs(10),
            batch: 100,
            records_endpoint: "https://acct.blob.core.windows.net".into(),
            records_container: "reinsurance-records".into(),
            event_hubs: "https://ns.servicebus.windows.net".into(),
            event_hub: "reinsurance-events".into(),
            drops_endpoint: None,
            api: None,
        }
    );
    let tuned = Config::from_lookup(lookup(&[
        ("IC_NODES", "n:1"),
        ("IC_REINSURANCE_CYCLE_MS", "250"),
        ("IC_REINSURANCE_PATIENCE_MS", "3000"),
        ("IC_REINSURANCE_BATCH", "7"),
        ("IC_BLOB_ENDPOINT", "b"),
        ("IC_EVENT_HUBS_ENDPOINT", "e"),
        ("IC_REINSURANCE_RECORDS_CONTAINER", "records"),
        ("IC_REINSURANCE_EVENT_HUB", "hub"),
    ]))
    .unwrap();
    assert_eq!(
        (tuned.cycle_every, tuned.patience, tuned.batch),
        (Duration::from_millis(250), Duration::from_secs(3), 7)
    );
    assert_eq!((tuned.records_container.as_str(), tuned.event_hub.as_str()), ("records", "hub"));
    assert!(Config::from_lookup(lookup(&[])).unwrap_err().contains("IC_NODES"));
    assert!(Config::from_lookup(lookup(&[("IC_NODES", "  ")])).unwrap_err().contains("IC_NODES"));
    // Nowhere to file statements, or nobody to tell of exceptions, is not a way to run.
    assert!(Config::from_lookup(lookup(&[("IC_NODES", "n:1"), ("IC_EVENT_HUBS_ENDPOINT", "e")]))
        .unwrap_err()
        .contains("IC_BLOB_ENDPOINT"));
    assert!(Config::from_lookup(lookup(&[("IC_NODES", "n:1"), ("IC_BLOB_ENDPOINT", "b")]))
        .unwrap_err()
        .contains("IC_EVENT_HUBS_ENDPOINT"));
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
    with_group_notices(&mut w);
    *w.core.down.lock().unwrap() = true;
    let core = w.core.clone();
    let mut seen = Vec::new();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let mut stop = Some(stop);
    let audiences = TestAudiences::default();
    *audiences.refuse.lock().unwrap() = Some(Reject::new("invalid_amount", "too much"));
    w.driver
        .run(
            &audiences,
            Duration::from_millis(1),
            async {
                let _ = stopped.await;
            },
            |pass, round| {
                // Notices cannot be dated while the core is away, and are once it is back.
                assert_eq!(round.is_ok(), pass.is_ok());
                seen.push((pass.clone(), round.clone()));
                match seen.len() {
                    1 => *core.down.lock().unwrap() = false,
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
    assert!(matches!(seen[0], (Err(DriverError::Unavailable(_)), Err(DriverError::Unavailable(_)))));
    // The policy's instructions are carried. The scheme's are for a partition the core
    // does not have, so delivery is held there, and its notices are refused. Both are
    // results like any other: the loop goes on.
    let held = |pass: &Result<Pass, DriverError>| matches!(pass, Ok(Pass { held, .. }) if !held.is_empty() && held.iter().all(|(_, refusal)| refusal.code == "unknown_partition"));
    assert!(matches!(&seen[1].0, Ok(pass) if pass.delivered > 0) && held(&seen[1].0), "{:?}", seen[1]);
    assert_eq!(seen[1].1, Ok(Round { taken: 0, refused: 2 }));
    for later in &seen[2..] {
        assert!(matches!(&later.0, Ok(pass) if pass.delivered == 0) && held(&later.0), "{later:?}");
        assert_eq!(later.1, Ok(Round::default()), "refused notices are not offered again");
    }
    assert!(!w.module.state().outbox(usize::MAX).is_empty(), "what is held stays in the outbox");
}

/// A module that answers questions but cannot be told anything, as when the
/// group's leader is lost between a read and the write that follows it.
struct Deaf(LocalModule);

impl Module for Deaf {
    async fn propose(&self, _: Envelope) -> Result<ic_reinsurance::Applied, DriverError> {
        Err(DriverError::Unavailable("the module lost its leader".into()))
    }

    async fn ask(&self, query: Query) -> Result<Value, DriverError> {
        self.0.ask(query).await
    }
}

/// Purpose: when an audience refuses a notice and the module then cannot be told so, the
/// round stops with the module's error, and the notice is still waiting, not set aside and
/// not lost.
/// Why it matters: the refusal is only real once it is in the module's replicated state.
/// A round that carried on, or counted the notice as dealt with, would drop it silently; the
/// next round must offer it again.
#[tokio::test]
async fn a_refusal_the_module_cannot_be_told_of_stops_the_round() {
    let mut w = World::new();
    with_group_notices(&mut w);
    let audiences = TestAudiences::default();
    *audiences.refuse.lock().unwrap() = Some(Reject::new("invalid_amount", "too much"));
    let deaf = Driver {
        module: Deaf(w.module.clone()),
        core: w.core.clone(),
        batch: 2,
    };
    assert_eq!(
        deaf.hand_on(&audiences, d(2027, 3, 2)).await,
        Err(DriverError::Unavailable("the module lost its leader".into()))
    );
    let state = w.module.state();
    assert_eq!(state.notices_for(Audience::Group, usize::MAX).len(), 2);
    assert!(state.exceptions().keys().all(|k| !k.starts_with("notice:")));
    // Once the module can be told, the same refusals are recorded.
    assert_eq!(w.driver.hand_on(&audiences, d(2027, 3, 2)).await, Ok(Round { taken: 0, refused: 2 }));
}

/// A module that says it has recorded a refusal and has not, as a stale
/// follower or a broken build might.
struct Forgetful(LocalModule);

impl Module for Forgetful {
    async fn propose(&self, envelope: Envelope) -> Result<ic_reinsurance::Applied, DriverError> {
        if !matches!(envelope.command, Command::NoticeRefused { .. }) {
            return self.0.propose(envelope).await;
        }
        Ok(ic_reinsurance::Applied {
            index: 0,
            result: Ok(()),
            instructions: vec![],
            notices: vec![],
            duplicate: true,
        })
    }

    async fn ask(&self, query: Query) -> Result<Value, DriverError> {
        self.0.ask(query).await
    }
}

/// Purpose: a notice that is refused, reported to the module, and then offered by the
/// module again in the same round stops the round with an error naming the notice.
/// Why it matters: the round reads the waiting notices until none are left. If a refusal
/// did not take the notice out of that list, as happened when a retried notice was refused
/// a second time under the key of the first refusal, the driver would offer it for ever
/// and do nothing else.
#[tokio::test]
async fn a_refused_notice_still_on_offer_stops_the_round() {
    let mut w = World::new();
    let waiting = with_group_notices(&mut w);
    let audiences = TestAudiences::default();
    *audiences.refuse.lock().unwrap() = Some(Reject::new("invalid_amount", "too much"));
    let driver = Driver {
        module: Forgetful(w.module.clone()),
        core: w.core.clone(),
        batch: 1,
    };
    let stopped = driver.hand_on(&audiences, d(2027, 3, 2)).await.unwrap_err();
    assert!(
        matches!(&stopped, DriverError::Answer(why) if why.contains(&format!("notice {} was refused", waiting[0]))),
        "{stopped}"
    );
    // With a module that records it, a second refusal after a retry is recorded too.
    assert_eq!(w.driver.hand_on(&audiences, d(2027, 3, 2)).await, Ok(Round { taken: 0, refused: 2 }));
    w.tell("reinsurance-ops", Command::RetryNotice { seq: waiting[0] }).unwrap();
    assert_eq!(w.ask(Query::NoticeRetries { seq: waiting[0] }), json!(1));
    assert_eq!(w.driver.hand_on(&audiences, d(2027, 3, 2)).await, Ok(Round { taken: 0, refused: 1 }));
    assert!(w.module.state().exceptions().contains_key(&format!("notice:{}", waiting[0])));
}

/// Purpose: a module's reply to a notice is read as taken only when it says so: both
/// modules' answer shapes are understood, a refusal comes back as the audience's own code
/// and message, a command the host could not read is a refusal, and a reply that is neither
/// an acceptance nor a readable refusal is an error.
/// Why it matters: a notice counted as taken is removed for good. Reading an unexpected
/// reply as acceptance would lose an underwriting referral or a facultative answer with
/// nothing to show it had not arrived.
#[test]
fn a_reply_to_a_notice_is_taken_only_when_it_says_so() {
    assert_eq!(handed(Ok(json!({"result": {"Ok": null}, "duplicate": false}))), Ok(Handed::Taken));
    assert_eq!(handed(Ok(json!({"Ok": {"events": []}}))), Ok(Handed::Taken));
    let reject = json!({"code": "unknown_policy", "message": "no scheme S9"});
    let refused = Ok(Handed::Refused(Reject::new("unknown_policy", "no scheme S9")));
    assert_eq!(handed(Ok(json!({"result": {"Err": reject}}))), refused);
    assert_eq!(handed(Ok(json!({"Err": reject}))), refused);
    assert_eq!(
        handed(Err("unknown variant `x`".into())),
        Ok(Handed::Refused(Reject::new("unreadable", "unknown variant `x`")))
    );
    assert!(matches!(handed(Ok(json!({"Err": "no"}))), Err(DriverError::Answer(why)) if why.contains("cannot be read")));
    for odd in [json!({"result": {}}), json!({"accepted": true}), json!(null), json!("ok")] {
        assert!(matches!(handed(Ok(odd)), Err(DriverError::Answer(why)) if why.contains("neither accepted nor refused")));
    }
}

/// Purpose: a settlement due to a reinsurer becomes the payments module's "offer payment"
/// command, keyed by the statement so it is one payment however often it is offered; money
/// due from a reinsurer, a nil settlement and any other notice produce no command.
/// Why it matters: the payment reference goes to the bank as the end-to-end reference and
/// comes back on the settlement. If it changed between tries a reinsurer could be paid twice
/// for one statement; if a negative net were offered, payments would refuse it and an
/// exception would open every month for a statement that is in order.
#[test]
fn a_settlement_due_is_offered_to_payments_once_per_statement() {
    let due = |net_minor: i64| Notice::SettlementDue {
        statement: 7,
        treaty: "retail".into(),
        reinsurer: "north-re".into(),
        currency: ic_money::Currency::NZD,
        net_minor,
        attempt: 0,
    };
    assert_eq!(
        payments_command(&due(90_000_00)).unwrap(),
        json!({
            "key": "reinsurance:settlement:7",
            "actor": {"id": "reinsurance", "roles": ["service"]},
            "at": "1970-01-01T00:00:00",
            "command": {"type": "offer_payment", "waiting": {
                "payment": "ri-stmt-7", "source": "reinsurance:statement:7", "payee": "north-re", "amount_minor": 90_000_00}},
        })
    );
    assert_eq!(payments_command(&due(0)), None);
    assert_eq!(payments_command(&due(-1)), None);
    assert_eq!(payments_command(&Notice::ExceptionClosed { id: "x".into() }), None);
}

/// An audience that records what it is handed, and can refuse or be away.
#[derive(Clone, Default)]
struct TestAudiences {
    taken: Arc<Mutex<Vec<(u64, Notice, NaiveDate)>>>,
    refuse: Arc<Mutex<Option<Reject>>>,
    away: Arc<Mutex<bool>>,
}

impl Audiences for TestAudiences {
    fn reached(&self) -> Vec<Audience> {
        vec![Audience::Group]
    }

    async fn hand_on(&self, seq: u64, notice: &Notice, on: NaiveDate) -> Result<Handed, DriverError> {
        if *self.away.lock().unwrap() {
            return Err(DriverError::Unavailable("the group module did not answer".into()));
        }
        if let Some(refusal) = self.refuse.lock().unwrap().clone() {
            return Ok(Handed::Refused(refusal));
        }
        self.taken.lock().unwrap().push((seq, notice.clone(), on));
        Ok(Handed::Taken)
    }
}

/// A scheme whose two members are each found to be a person already insured, so
/// each raises an underwritten-excess notice for group administration.
fn with_group_notices(w: &mut World) -> Vec<u64> {
    let feed = |w: &mut World, seq: u64, event: ic_core::event::Event| {
        let record = ic_core::EventRecord {
            id: format!("77-{seq}"),
            partition: 77,
            seq,
            business_date: d(2027, 3, 1),
            log_index: seq,
            traceparent: None,
            event,
        };
        assert_eq!(w.module.apply(react(&record)[0].clone()).result, Ok(()));
    };
    feed(
        w,
        1,
        ic_core::event::Event::PolicyIssued {
            policy: PolicyId::from("S1"),
            number: "N-S1".into(),
            group: GroupId::from("S1"),
            policyholder: PartyId::from("employer"),
            product_code: "group-life".into(),
            product_hash: "hash".into(),
            inception: d(2027, 2, 1),
            lives: ["M1", "M2"].iter().map(|m| ic_core::event::LifeInfo::new((*m).into(), None)).collect(),
            layers: vec![],
            members: vec![member_cover("M1", 700 * K), member_cover("M2", 900 * K)],
        },
    );
    w.tell(
        "group-admin",
        Command::SetSchemeTerms {
            policy: PolicyId::from("S1"),
            excess_path: ExcessPath::UnderwrittenExcess,
            sites: vec![],
        },
    )
    .unwrap();
    for (seq, (member, party)) in [("M1", "anna"), ("M2", "ben")].into_iter().enumerate() {
        feed(
            w,
            seq as u64 + 2,
            ic_core::event::Event::MemberIdentified {
                policy: PolicyId::from("S1"),
                life: member.into(),
                party: PartyId::from(party),
            },
        );
    }
    let state = w.module.state();
    let waiting: Vec<u64> = state.notices_for(Audience::Group, usize::MAX).iter().map(|(s, _)| *s).collect();
    assert_eq!(waiting.len(), 2);
    waiting
}

/// Purpose: notices for an audience the carrier reaches are handed on once each, in order,
/// with their outbox number and the business date, and leave the outbox; notices for
/// audiences it does not reach stay where they are.
/// Why it matters: the underwritten-excess notice is the only thing that starts
/// underwriting for cover above the retention. Lost, the excess sits unplaced; sent twice
/// without a stable number, group administration would open two cases.
#[tokio::test]
async fn notices_are_handed_to_the_audience_they_are_for() {
    let mut w = World::new();
    let waiting = with_group_notices(&mut w);
    let others = w.module.state().notices(usize::MAX).len() - 2;
    assert!(others >= 2, "the over-retention exceptions are for staff");
    let audiences = TestAudiences::default();

    let round = w.driver.hand_on(&audiences, d(2027, 3, 2)).await.unwrap();
    assert_eq!(round, Round { taken: 2, refused: 0 });
    let taken = audiences.taken.lock().unwrap().clone();
    assert_eq!(
        taken.iter().map(|(s, _, on)| (*s, *on)).collect::<Vec<_>>(),
        vec![(waiting[0], d(2027, 3, 2)), (waiting[1], d(2027, 3, 2))]
    );
    assert!(matches!(&taken[0].1, Notice::RequireUnderwrittenExcess { life, excess_minor, .. } if life.0 == "M1" && *excess_minor == 200 * K));
    let state = w.module.state();
    assert!(state.notices_for(Audience::Group, usize::MAX).is_empty());
    assert_eq!(state.notices(usize::MAX).len(), others, "nobody reaches staff yet, so their notices wait");
    assert_eq!(w.driver.hand_on(&audiences, d(2027, 3, 2)).await.unwrap(), Round::default());

    // The command group administration is sent, in its own wire format.
    let command = group_command(waiting[0], &taken[0].1, d(2027, 3, 2)).unwrap();
    assert_eq!(
        command,
        json!({
            "key": format!("ri-notice:{}", waiting[0]),
            "caller": "reinsurance",
            "actor": "reinsurance-driver",
            "today": "2027-03-02",
            "command": {"require_underwritten_excess": {
                "policy": "S1", "life": "M1", "cover": "group-life", "excess_minor": 200 * K,
                "reference": format!("UWX/S1/group-life/0/M1/{}", 200 * K),
            }},
        })
    );
    assert_eq!(group_command(1, &Notice::ExceptionClosed { id: "x".into() }, d(2027, 3, 2)), None);
}

/// A feed for staff that keeps the IDs it was sent.
#[derive(Clone, Default)]
struct StaffSeen(Arc<Mutex<Vec<String>>>);

impl ic_reinsurance::carriers::StaffFeed for StaffSeen {
    async fn publish(&self, id: &str, _kind: &str, _body: serde_json::Value) -> Result<(), ic_azure::AzureError> {
        self.0.lock().unwrap().push(id.to_string());
        Ok(())
    }
}

/// Purpose: with every carrier in place a round empties the outbox: notices for staff go to
/// their feed and are acknowledged like any other. And while one audience is away (here
/// group administration) the round still tells staff, reports the failure, and hands the
/// waiting notices to that audience on the next round, each once.
/// Why it matters: the exceptions for staff are how anyone learns that something else is
/// stuck. If an hour's outage of one module, or of Blob Storage, held every notice behind
/// it, staff would hear nothing in exactly the hour they most need to.
#[tokio::test]
async fn one_audience_being_away_does_not_keep_staff_waiting() {
    let mut w = World::new();
    let waiting = with_group_notices(&mut w);
    let for_staff = w.module.state().notices_for(Audience::Staff, usize::MAX).len();
    assert!(for_staff >= 2);
    let carriers = ic_reinsurance::carriers::Carriers {
        modules: TestAudiences::default(),
        records: ic_azure::blob::MemoryWormStore::default(),
        staff: StaffSeen::default(),
        outbound: ic_reinsurance::carriers::MemoryDrops::default(),
    };
    *carriers.modules.away.lock().unwrap() = true;
    assert!(matches!(w.driver.hand_on(&carriers, d(2027, 3, 2)).await, Err(DriverError::Unavailable(_))));
    assert_eq!(carriers.staff.0.lock().unwrap().len(), for_staff);
    let state = w.module.state();
    assert!(state.notices_for(Audience::Staff, usize::MAX).is_empty());
    assert_eq!(state.notices_for(Audience::Group, usize::MAX).len(), 2);

    *carriers.modules.away.lock().unwrap() = false;
    assert_eq!(w.driver.hand_on(&carriers, d(2027, 3, 2)).await.unwrap(), Round { taken: 2, refused: 0 });
    assert_eq!(carriers.modules.taken.lock().unwrap().iter().map(|(s, _, _)| *s).collect::<Vec<_>>(), waiting);
    assert_eq!(carriers.staff.0.lock().unwrap().len(), for_staff, "nothing is told twice");
    assert!(w.module.state().notices(usize::MAX).is_empty());
}

/// Purpose: a notice its audience refuses is set aside with an exception and does not hold
/// up the notice behind it; operations can send it again or abandon it with a reason, and
/// an audience that is away loses nothing.
/// Why it matters: group administration refuses an excess larger than the member's cover,
/// which happens when the membership changed in between. One stale notice must not stop
/// every later member's underwriting, and it must not vanish without a person deciding.
#[tokio::test]
async fn a_refused_notice_is_set_aside_for_a_person_and_the_rest_carry_on() {
    let mut w = World::new();
    let waiting = with_group_notices(&mut w);
    let audiences = TestAudiences::default();

    *audiences.away.lock().unwrap() = true;
    assert!(matches!(w.driver.hand_on(&audiences, d(2027, 3, 2)).await, Err(DriverError::Unavailable(_))));
    assert_eq!(w.module.state().notices_for(Audience::Group, usize::MAX).len(), 2);
    *audiences.away.lock().unwrap() = false;

    *audiences.refuse.lock().unwrap() = Some(Reject::new("invalid_amount", "more than the member's automatic cover"));
    let round = w.driver.hand_on(&audiences, d(2027, 3, 2)).await.unwrap();
    assert_eq!(round, Round { taken: 0, refused: 2 });
    let state = w.module.state();
    assert!(state.notices_for(Audience::Group, usize::MAX).is_empty(), "set aside, not offered again");
    let exception = &state.exceptions()[&format!("notice:{}", waiting[0])];
    assert_eq!(exception.code, "notice_refused");
    assert!(
        exception.detail.contains("Group") && exception.detail.contains("invalid_amount: more than"),
        "{}",
        exception.detail
    );
    *audiences.refuse.lock().unwrap() = None;
    assert_eq!(w.driver.hand_on(&audiences, d(2027, 3, 2)).await.unwrap(), Round::default());

    // An acknowledgement does not remove a notice that was set aside.
    w.tell(DRIVER, Command::AcknowledgeNotices { seqs: vec![waiting[0]] }).unwrap();
    assert!(w.module.state().notices(usize::MAX).iter().any(|(s, _)| *s == waiting[0]));

    let (first, second) = (waiting[0], waiting[1]);
    assert_eq!(w.tell(DRIVER, Command::RetryNotice { seq: first }), Err("caller_not_allowed".into()));
    assert_eq!(w.tell("reinsurance-ops", Command::RetryNotice { seq: 9_999 }), Err("not_refused".into()));
    w.tell("reinsurance-ops", Command::RetryNotice { seq: first }).unwrap();
    assert_eq!(w.driver.hand_on(&audiences, d(2027, 3, 3)).await.unwrap(), Round { taken: 1, refused: 0 });
    assert!(!w.module.state().exceptions().contains_key(&format!("notice:{first}")));

    let abandon = |seq, reason: &str, by: &str| Command::AbandonNotice {
        seq,
        reason: reason.into(),
        approved_by: by.into(),
    };
    assert_eq!(w.tell("reinsurance-ops", abandon(first, "r", "lee")), Err("not_refused".into()));
    assert_eq!(w.tell("reinsurance-ops", abandon(second, "", "lee")), Err("reason_required".into()));
    assert_eq!(w.tell("reinsurance-ops", abandon(second, "r", " ")), Err("reason_required".into()));
    w.tell("reinsurance-ops", abandon(second, "member left the scheme on 1 March", "lee")).unwrap();
    let state = w.module.state();
    assert!(!state.exceptions().contains_key(&format!("notice:{second}")));
    assert!(!state.notices(usize::MAX).iter().any(|(s, _)| *s == second));
    assert_eq!(state.notices_abandoned().len(), 1);
    assert_eq!(w.ask(Query::AbandonedNotices)[0]["reason"], "member left the scheme on 1 March");
    assert_eq!(
        w.ask(Query::Notices {
            audience: Some(Audience::Group),
            limit: 5
        }),
        json!([])
    );

    // Refusals the module itself turns away.
    let refused = |seq, code: &str| Command::NoticeRefused {
        seq,
        code: code.into(),
        message: "m".into(),
    };
    assert_eq!(w.tell(DRIVER, refused(second, "x")), Err("unknown_notice".into()));
    let staff = w.module.state().notices(1)[0].0;
    assert_eq!(w.tell(DRIVER, refused(staff, " ")), Err("reason_required".into()));
}

/// Purpose: a cycle carries instructions and then hands notices on, and holds the notices
/// back while the first partition has no business date or cannot be asked for one.
/// Why it matters: a notice is dated with the business date when it is handed on. Sending
/// it before the insurer's first day is open would date it by a clock the system does not
/// otherwise trust.
#[tokio::test]
async fn a_cycle_delivers_then_hands_on_and_waits_for_a_business_date() {
    let mut w = World::new();
    with_group_notices(&mut w);
    let audiences = TestAudiences::default();
    *w.core.unopened.lock().unwrap() = Some(0);
    let (pass, round) = w.driver.cycle(&audiences).await;
    assert!(pass.is_ok());
    assert_eq!(round, Ok(Round::default()));
    assert!(audiences.taken.lock().unwrap().is_empty());
    *w.core.unopened.lock().unwrap() = None;
    *w.core.down.lock().unwrap() = true;
    let (_, round) = w.driver.cycle(&audiences).await;
    assert!(matches!(round, Err(DriverError::Unavailable(_))));
    *w.core.down.lock().unwrap() = false;
    let (_, round) = w.driver.cycle(&audiences).await;
    assert_eq!(round, Ok(Round { taken: 2, refused: 0 }));
    assert_eq!(audiences.taken.lock().unwrap()[0].2, d(2027, 3, 1));
}

/// Claims rules and authority for the partitions: enough for a claim to be
/// notified and worked by one senior assessor.
mod claims_setup {
    use ic_core::claims_module as claims;

    use claims::config::{Exclusion, HealthBenefit, Profile, Staff};
    use claims::{AuthorityBook, ClaimType, DecisionKind, Line, Queue, Role, Rules};

    pub fn rules() -> Rules {
        let kinds = [
            DecisionKind::Reserve,
            DecisionKind::Admit,
            DecisionKind::Decline,
            DecisionKind::Withdraw,
            DecisionKind::ReverseAdmission,
            DecisionKind::ExGratia,
            DecisionKind::EndBenefit,
            DecisionKind::RecoveryStatus,
            DecisionKind::WriteOff,
        ];
        Rules {
            version: 1,
            escalation_days: [1, 3, 7],
            large_claim_minor: i64::MAX,
            two_signature_admission_minor: i64::MAX,
            two_signature_ex_gratia_minor: i64::MAX,
            two_signature_write_off_minor: i64::MAX,
            sharp_reserve_move_pct: 1_000,
            reason_codes: [("R-1".to_string(), kinds.into_iter().collect())].into(),
            straight_through_max_minor: [(ClaimType::Health, 2_000_00)].into(),
            health_benefits: [(
                "gp".to_string(),
                HealthBenefit {
                    per_event_minor: 60_00,
                    per_year_minor: 600_00,
                    lifetime_minor: i64::MAX / 4,
                    copay_pct: 0,
                    excess_minor: 0,
                },
            )]
            .into(),
            cause_codes: [("CC-0001".to_string(), vec!["cancer.skin".to_string()])].into(),
            exclusions: [("EX-0001".to_string(), Exclusion::default())].into(),
            medical_review_months: 6,
            income_declaration_months: 3,
            review_grace_days: 14,
            max_offset_pct: 25,
            late_death_days: 30,
            estate_simplified_max_minor: i64::MAX,
            pre_approval_days: 30,
            recurrence_days: 180,
            prepared_by: "config-ann".into(),
            approved_by: vec!["config-bob".into(), "config-cat".into()],
            ..Rules::default()
        }
    }

    pub fn authority() -> AuthorityBook {
        let lines: std::collections::BTreeSet<Line> = [Line::Health, Line::LumpSum, Line::IncomeProtection].into();
        let types = [
            ClaimType::Death,
            ClaimType::TerminalIllness,
            ClaimType::Trauma,
            ClaimType::IncomeProtection,
            ClaimType::Health,
        ];
        let senior = Profile {
            lines,
            max_admission_minor: types.into_iter().map(|t| (t, i64::MAX)).collect(),
            max_payment_minor: i64::MAX,
            max_reserve_minor: i64::MAX,
            may_decline: true,
            max_ex_gratia_minor: i64::MAX,
            max_write_off_minor: i64::MAX,
            may_override_exclusion: true,
            may_reverse_admission: true,
            may_change_recovery_status: true,
            senior: true,
        };
        let person = |roles: &[Role]| Staff {
            profile: "senior".to_string(),
            roles: roles.iter().copied().collect(),
            queues: [
                Queue::Health,
                Queue::Group,
                Queue::LumpSum,
                Queue::IncomeProtectionNew,
                Queue::IncomeProtectionOngoing,
                Queue::ComplexLarge,
                Queue::Recoveries,
            ]
            .into(),
            wip_limit: 0,
            vulnerable_trained: true,
            related_parties: Default::default(),
        };
        AuthorityBook {
            version: 1,
            profiles: [("senior".to_string(), senior)].into(),
            staff: [
                ("sam".to_string(), person(&[Role::Assessor, Role::QueueLead])),
                ("sue".to_string(), person(&[Role::Assessor, Role::ClaimsManager])),
            ]
            .into(),
            prepared_by: "config-ann".into(),
            approved_by: vec!["config-bob".into(), "config-cat".into()],
        }
    }
}

fn claims_command(command: ic_core::claims_module::Command) -> ic_core::Command {
    ic_core::Command::Claims(Box::new(command))
}

/// Purpose: a large claim notified on a partition reaches the module as the claims
/// module's published fact, and the module's answers reach the claim through the driver:
/// the large-claim notification and the consent requirement are recorded on the claim, a
/// reinsurer's consent follows them, and a disputed item settled later is told to claims
/// with the claim it belongs to. A notice the core refuses is set aside for a person, and
/// sending it again uses a new key so the core looks at it afresh.
/// Why it matters: claims holds admission of a large claim until the reinsurers have been
/// told and have consented. If these notices stopped in the module's outbox, as they did
/// before this, an assessor could admit a three million dollar claim with nothing on the
/// claim to say consent was needed.
#[tokio::test]
async fn claims_hears_what_reinsurance_decides_about_its_claims() {
    use ic_core::claims_module as claims;
    let mut w = World::new();
    {
        let mut c = w.core.cluster.lock().unwrap();
        let rules = Box::new(claims_setup::rules());
        c.activate_everywhere(claims_command(claims::Command::ActivateRules { rules })).unwrap();
        let book = Box::new(claims_setup::authority());
        c.activate_everywhere(claims_command(claims::Command::ActivateAuthority { book })).unwrap();
    }
    let (p, policy) = w.issue(1, 3_000_000_00);
    w.driver.deliver().await.unwrap();
    let notify = claims::Command::Notify(Box::new(claims::command::Notification {
        claim: "clm-1".into(),
        policy: policy.0.clone(),
        cover: "life".into(),
        life: Some("life-1".into()),
        claim_type: claims::ClaimType::Death,
        event_date: d(2027, 3, 1),
        channel: claims::Channel::App,
        notifier: claims::Notifier::Policyholder,
        claimant: Some("party-claimant".into()),
        claimed_minor: None,
        flags: claims::Flags::default(),
        health: None,
        underwriters: vec![],
        catastrophe_event: None,
        cause_code: None,
    }));
    w.core.cluster.lock().unwrap().submit(p, "claims", claims_command(notify)).unwrap();
    w.feed();
    let claim_id = ic_reinsurance::ClaimId::from("clm-1");
    let book = w.module.state().claim(&claim_id).cloned().expect("the reserve reached the module as an event");
    assert_eq!((book.policy.clone(), book.reserve_minor), (policy.clone(), 3_000_000_00));
    let for_claims = |w: &World| w.module.state().notices_for(Audience::Claims, usize::MAX).len();
    assert_eq!(for_claims(&w), 2, "a large claim notification and a consent requirement");

    // No audience outside the core is needed for these.
    let nobody = TestAudiences::default();
    let on = d(2027, 3, 1);
    *w.core.down.lock().unwrap() = true;
    assert!(matches!(w.driver.hand_on(&nobody, on).await, Err(DriverError::Unavailable(_))));
    assert_eq!(for_claims(&w), 2);
    *w.core.down.lock().unwrap() = false;
    assert_eq!(w.driver.hand_on(&nobody, on).await, Ok(Round { taken: 2, refused: 0 }));
    assert_eq!(for_claims(&w), 0);
    let on_claim = |w: &World| w.core.cluster.lock().unwrap().state(p).claims.claims["clm-1"].reinsurance.clone();
    let seen = on_claim(&w);
    assert!(seen.large_claim_notified);
    assert!(seen.consent_required.contains("retail-life-surplus"), "{seen:?}");

    // Each reinsurer consents; claims records each and the requirement is met.
    for reinsurer in [north(), south()] {
        let consent = Command::ReinsurerConsent {
            claim: claim_id.clone(),
            reinsurer,
            agreed: true,
            reference: "email-77".into(),
        };
        w.tell("reinsurance-ops", consent).unwrap();
    }
    assert_eq!(w.driver.hand_on(&nobody, on).await, Ok(Round { taken: 2, refused: 0 }));
    let seen = on_claim(&w);
    assert!(seen.consent_required.is_empty() && seen.consents.len() == 2, "{seen:?}");
    // Handing the same notices on again, as after a lost acknowledgement, changes nothing.
    assert_eq!(w.driver.hand_on(&nobody, on).await, Ok(Round::default()));

    // A fact about a claim the partition does not hold: the core refuses the notice.
    let ghost = ic_core::EventRecord {
        id: format!("{p}-ghost"),
        partition: p,
        seq: w.module.state().checkpoint(p) + 1,
        business_date: on,
        log_index: 0,
        traceparent: None,
        event: ic_core::event::Event::Module {
            module: "claims".into(),
            kind: "claim_reserved".into(),
            subject: "clm-ghost".into(),
            detail: [
                ("policy", policy.0.as_str()),
                ("cover", "life"),
                ("life", "life-1"),
                ("event_date", "2027-03-01"),
                ("gross_reserve_minor", "150000000"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        },
    };
    assert_eq!(w.module.apply(react(&ghost)[0].clone()).result, Ok(()));
    let seq = w.module.state().notices_for(Audience::Claims, 1)[0].0;
    let key = |w: &World| w.ask(Query::NoticeDelivery { seq })[1].as_str().unwrap().to_string();
    let first_key = key(&w);
    assert_eq!(w.ask(Query::NoticeDelivery { seq })[0], json!(p));
    assert_eq!(w.ask(Query::NoticeDelivery { seq })[2], json!("reinsurance"));
    assert_eq!(w.driver.hand_on(&nobody, on).await, Ok(Round { taken: 0, refused: 1 }));
    let exception = w.module.state().exceptions()[&format!("notice:{seq}")].clone();
    assert!(exception.detail.contains("Claims"), "{}", exception.detail);
    assert_eq!(w.driver.hand_on(&nobody, on).await, Ok(Round::default()), "set aside, not offered again");
    // Sent again by a person, it goes under a new key: the core would answer the old one
    // with the refusal it remembers.
    w.tell("reinsurance-ops", Command::RetryNotice { seq }).unwrap();
    assert_ne!(key(&w), first_key);
    assert_eq!(w.driver.hand_on(&nobody, on).await, Ok(Round { taken: 0, refused: 1 }));
    let abandon = Command::AbandonNotice {
        seq,
        reason: "the claim was entered in error and never existed".into(),
        approved_by: "lee".into(),
    };
    w.tell("reinsurance-ops", abandon).unwrap();
    assert_eq!(for_claims(&w), 0);
    // Only notices for claims that are still waiting have a command for the core.
    let gone = answer(&w.module.state(), serde_json::to_value(Query::NoticeDelivery { seq }).unwrap());
    assert!(gone.unwrap_err().contains("not waiting to be handed to claims"));
    let staff = w.module.state().notices(1)[0].0;
    assert!(answer(&w.module.state(), serde_json::to_value(Query::NoticeDelivery { seq: staff }).unwrap()).is_err());
}
