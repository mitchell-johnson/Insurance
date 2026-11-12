//! The end-of-day run against real ledger nodes, with workers and nodes
//! dying underneath it.
//!
//! The nodes are real `ic-raft` hosts over HTTP on localhost with durable
//! logs. The run is driven directly, without a Temporal server, and killed
//! in ways that are harsher than anything Temporal does: a replacement
//! worker here starts the day from the first step with no memory of what the
//! dead one finished. If that is safe, resuming from recorded history is.
//!
//! The yardstick throughout is the single-process simulator: the same
//! business through the same number of days must leave the same book.

mod common;

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use common::{day, opening, simulated, Ledger, PATIENCE};
use ic_azure::blob::{MemoryWormStore, WormStore};
use ic_azure::keyvault::DevSigner;
use ic_azure::merkle::{root_name, verify_published, PublishedRoot};
use ic_core::machine::ClosedDay;
use ic_raft::NodeId;
use ic_workflows::eod::{PartitionCheck, PartitionStep, PublishStep, PublishedSummary, RoundStep, UnitPass, UnitStep};
use ic_workflows::{run_end_of_day, run_unit_orders, EndOfDayError, EndOfDayInput, EndOfDayReport, LedgerSteps, StepError, Steps};

/// Run a day cleanly and insist it reconciles.
async fn close<S: Steps<Error = StepError>>(steps: &S, input: &EndOfDayInput) -> EndOfDayReport {
    let report = run_end_of_day(steps, input).await.unwrap();
    assert_eq!(report.breaks, vec![], "reconciliation breaks on {}", input.closing);
    report
}

/// Purpose: a clean run over a replicated, two-partition ledger leaves exactly the book
/// the simulator's own end-of-day leaves, across the first monthly billing date, and
/// publishes a root for each day that an independent verifier accepts.
/// Why it matters: the orchestration was moved out of the simulator into separate steps
/// over the network. This is the proof that the move changed nothing about the money, and
/// that the tamper-evidence trail is still produced for every closed day.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_clean_run_matches_the_simulator_and_publishes_each_root() {
    let ledger = Ledger::start("clean", 3, 2).await;
    let steps = ledger.steps();
    assert_eq!(ledger.books().await, simulated(2, 0));

    let days = 32;
    for n in 0..days {
        let report = close(&steps, &ledger.input(day(n))).await;
        assert_eq!(report.closed, day(n));
        assert_eq!(report.heads.len(), 2);

        let stored = steps.store().get(&root_name(day(n))).await.unwrap().expect("no root was published");
        let published: PublishedRoot = serde_json::from_slice(&stored).unwrap();
        assert!(verify_published(&published));
        assert_eq!(published.root, report.published.root);
        assert_eq!(published.heads, report.heads);
    }
    assert_eq!(ledger.books().await, simulated(2, days));
    assert!(
        ledger.books().await[&0].2.iter().all(|(_, _, _, periods)| *periods == 2),
        "the monthly bill did not run"
    );
    assert_eq!(steps.store().names().len(), days as usize);
    ledger.stop().await;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Death {
    /// The worker dies before it starts the step.
    BeforeTheStep,
    /// The worker does the step and dies before it can report the result.
    AfterTheStep,
}

/// A worker that dies at its `at`-th step.
struct DyingWorker<'a, S> {
    steps: &'a S,
    at: u32,
    how: Death,
    taken: AtomicU32,
}

impl<S: Steps<Error = StepError>> DyingWorker<'_, S> {
    fn dead() -> StepError {
        StepError::Unavailable("the worker died".to_string())
    }

    async fn step<T>(&self, work: impl std::future::Future<Output = Result<T, StepError>>) -> Result<T, StepError> {
        let n = self.taken.fetch_add(1, Ordering::SeqCst);
        if n > self.at || (n == self.at && self.how == Death::BeforeTheStep) {
            return Err(Self::dead());
        }
        let result = work.await;
        if n == self.at {
            return Err(Self::dead());
        }
        result
    }
}

impl<S: Steps<Error = StepError>> Steps for DyingWorker<'_, S> {
    type Error = StepError;

    async fn cutoff(&self, step: PartitionStep) -> Result<ClosedDay, StepError> {
        self.step(self.steps.cutoff(step)).await
    }
    async fn run_round(&self, step: RoundStep) -> Result<u32, StepError> {
        self.step(self.steps.run_round(step)).await
    }
    async fn unit_orders(&self, step: UnitStep) -> Result<UnitPass, StepError> {
        self.step(self.steps.unit_orders(step)).await
    }
    async fn reconcile(&self, step: PartitionStep) -> Result<PartitionCheck, StepError> {
        self.step(self.steps.reconcile(step)).await
    }
    async fn publish(&self, step: PublishStep) -> Result<PublishedSummary, StepError> {
        self.step(self.steps.publish(step)).await
    }
}

/// Purpose: kill the worker at every step of the run in turn, both before the step and
/// after the step has taken effect but before its result is recorded, then let a fresh
/// worker run the whole day again from the top. The book must end identical to the
/// simulator's, with no break on any day.
/// Why it matters: Temporal workers are ordinary pods and die mid-run on every rollout and
/// node drain. The "after" case is the dangerous one: the ledger has moved but nobody
/// knows. A cutoff applied twice would skip a business day; a billing chunk applied twice
/// would charge customers twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_worker_dying_at_any_step_changes_nothing() {
    let ledger = Ledger::start("dying-worker", 1, 2).await;
    let steps = ledger.steps();
    // Clean days up to the eve of the first monthly bill, so the crashes
    // below land on days with real billing work as well as quiet ones.
    let mut days = 0;
    while days < 29 {
        close(&steps, &ledger.input(day(days))).await;
        days += 1;
    }

    let mut deaths = 0;
    for how in [Death::BeforeTheStep, Death::AfterTheStep] {
        for at in 0.. {
            let input = ledger.input(day(days));
            let doomed = DyingWorker {
                steps: &steps,
                at,
                how,
                taken: AtomicU32::new(0),
            };
            let first = run_end_of_day(&doomed, &input).await;
            // A replacement worker with no memory of the first one's progress.
            let report = close(&steps, &input).await;
            days += 1;
            match first {
                Err(EndOfDayError::Step(e)) => {
                    assert!(e.is_retryable());
                    deaths += 1;
                }
                // The run has fewer than `at` steps: every step has been tried.
                Ok(clean) => {
                    assert_eq!(clean, report, "running a finished day again gave a different report");
                    break;
                }
                Err(other) => panic!("unexpected failure: {other}"),
            }
        }
    }
    // Two partitions: two cutoffs, at least two rounds, two reconciliations, one publish.
    assert!(deaths >= 14, "only {deaths} crash points were exercised");
    assert_eq!(ledger.books().await, simulated(2, days));
    assert!(
        ledger.books().await[&1].2.iter().all(|(_, _, _, periods)| *periods == 2),
        "no billing day was crossed"
    );
    assert_eq!(steps.store().names().len() as u64, days, "a day's root was published more or less than once");
    ledger.stop().await;
}

/// Purpose: two workers run the same day at the same moment. Both finish, both report the
/// same heads and the same root, and the book is what one run would have left.
/// Why it matters: Temporal gives a step to a second worker when the first is slow to
/// answer, so two live workers doing the same step at once is normal, not a fault. The
/// ledger's idempotency keys, not the orchestrator, are what stop the double work.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_workers_on_the_same_day_equal_one() {
    let ledger = Ledger::start("two-workers", 3, 2).await;
    let steps = ledger.steps();
    for n in 0..3 {
        let input = ledger.input(day(n));
        let (a, b) = tokio::join!(run_end_of_day(&steps, &input), run_end_of_day(&steps, &input));
        let (a, b) = (a.unwrap(), b.unwrap());
        assert_eq!(a.heads, b.heads);
        assert_eq!(a.published, b.published);
        assert_eq!(a.breaks, vec![]);
        assert_eq!(b.breaks, vec![]);
    }
    assert_eq!(ledger.books().await, simulated(2, 3));
    assert_eq!(steps.store().names().len(), 3);
    ledger.stop().await;
}

/// The real steps, with a ledger node killed after the first cutoff.
struct KillsALeader<'a> {
    steps: &'a LedgerSteps<DevSigner, MemoryWormStore>,
    ledger: &'a tokio::sync::Mutex<Ledger>,
    killed: Mutex<Option<NodeId>>,
}

impl Steps for KillsALeader<'_> {
    type Error = StepError;

    async fn cutoff(&self, step: PartitionStep) -> Result<ClosedDay, StepError> {
        let closed = self.steps.cutoff(step.clone()).await?;
        if step.partition == 0 {
            let mut ledger = self.ledger.lock().await;
            let leader = ledger.leader_of(1).expect("no leader");
            ledger.kill(leader).await;
            *self.killed.lock().unwrap() = Some(leader);
        }
        Ok(closed)
    }
    async fn run_round(&self, step: RoundStep) -> Result<u32, StepError> {
        self.steps.run_round(step).await
    }
    async fn unit_orders(&self, step: UnitStep) -> Result<UnitPass, StepError> {
        self.steps.unit_orders(step).await
    }
    async fn reconcile(&self, step: PartitionStep) -> Result<PartitionCheck, StepError> {
        self.steps.reconcile(step).await
    }
    async fn publish(&self, step: PublishStep) -> Result<PublishedSummary, StepError> {
        self.steps.publish(step).await
    }
}

/// Purpose: a ledger node that leads a partition is killed in the middle of the run. The
/// run finishes without anyone intervening, the node comes back and catches up, and the
/// book matches the simulator.
/// Why it matters: end-of-day is the longest stretch of sustained writes in the day, so it
/// is the most likely moment for a node failure to land. The run must absorb a leader
/// election inside a step rather than fail the day.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_run_rides_out_a_ledger_leader_crash() {
    let ledger = Ledger::start("leader-crash", 3, 2).await;
    let steps = ledger.steps();
    let input = ledger.input(day(0));
    let shared = tokio::sync::Mutex::new(ledger);
    let chaos = KillsALeader {
        steps: &steps,
        ledger: &shared,
        killed: Mutex::new(None),
    };
    let report = close(&chaos, &input).await;
    assert_eq!(report.closed, day(0));
    let killed = chaos.killed.lock().unwrap().expect("no node was killed");

    let mut ledger = shared.into_inner();
    assert_eq!(ledger.running.len(), 2);
    assert_eq!(ledger.books().await, simulated(2, 1));
    ledger.boot(killed).await;
    close(&steps, &ledger.input(day(1))).await;
    assert_eq!(ledger.books().await, simulated(2, 2));
    // The node that was killed holds the same state as the others again.
    let hashes: Vec<String> = {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        loop {
            let hashes: Vec<String> = ledger.running.values().map(|r| r.host.statuses()[0].state_hash.clone()).collect();
            if hashes.iter().all(|h| *h == hashes[0]) || tokio::time::Instant::now() >= deadline {
                break hashes;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    assert!(hashes.iter().all(|h| *h == hashes[0]), "the restarted node did not catch up: {hashes:?}");
    ledger.stop().await;
}

/// Purpose: a run asked to close a day the ledger is not on is refused outright and
/// leaves the ledger untouched, while asking again for the day just closed returns the
/// same report as the first time.
/// Why it matters: a misfired schedule or an operator's typo must not advance the business
/// date, and "did last night's run finish?" must be answerable by simply running it again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_wrong_day_is_refused_and_the_right_day_repeats() {
    let ledger = Ledger::start("wrong-day", 1, 1).await;
    let steps = ledger.steps();
    let before = ledger.books().await;

    for wrong in [day(1), day(40), opening().pred_opt().unwrap()] {
        match run_end_of_day(&steps, &ledger.input(wrong)).await {
            Err(EndOfDayError::Step(e @ StepError::Refused(_))) => {
                assert!(!e.is_retryable());
                assert!(e.to_string().contains(&wrong.to_string()), "{e} does not name the day");
            }
            other => panic!("closing {wrong} should have been refused, got {other:?}"),
        }
    }
    assert_eq!(ledger.books().await, before);
    assert!(steps.store().names().is_empty());

    let first = close(&steps, &ledger.input(day(0))).await;
    let after = ledger.books().await;
    let again = close(&steps, &ledger.input(day(0))).await;
    assert_eq!(again, first);
    assert_eq!(ledger.books().await, after);

    // Two days on, the first day is history and cannot be run again.
    close(&steps, &ledger.input(day(1))).await;
    assert!(matches!(
        run_end_of_day(&steps, &ledger.input(day(0))).await,
        Err(EndOfDayError::Step(StepError::Refused(_)))
    ));
    ledger.stop().await;
}

/// Purpose: when no ledger node answers, a step gives up after its patience with an error
/// marked as worth retrying; and when the store already holds a different root for the
/// day, publishing fails with an error marked as not worth retrying.
/// Why it matters: this marking is what Temporal acts on. An outage must be retried until
/// it clears, without waking anyone. A conflicting root means a closed day has changed,
/// which no retry can fix and a person must see.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn outages_are_retryable_and_a_conflicting_root_is_not() {
    let mut ledger = Ledger::start("outage", 1, 1).await;
    let steps = ledger.steps();
    let report = close(&steps, &ledger.input(day(0))).await;

    // A second store that already holds something else for the next day.
    let tampered = MemoryWormStore::default();
    let mut other: PublishedRoot = serde_json::from_slice(&steps.store().get(&root_name(day(0))).await.unwrap().unwrap()).unwrap();
    other.business_date = day(1);
    tampered
        .put_new(&root_name(day(1)), "application/json", serde_json::to_vec(&other).unwrap())
        .await
        .unwrap();
    let conflicted = LedgerSteps::new(ledger.client.clone(), DevSigner, tampered).with_patience(PATIENCE);
    match run_end_of_day(&conflicted, &ledger.input(day(1))).await {
        Err(EndOfDayError::Step(e @ StepError::Refused(_))) => assert!(!e.is_retryable()),
        other => panic!("a conflicting root should be refused, got {other:?}"),
    }
    // The day itself closed; only the publication is outstanding, and the
    // honest store takes it.
    let republished = close(&steps, &ledger.input(day(1))).await;
    assert_ne!(republished.published.root, report.published.root);

    ledger.kill(1).await;
    let impatient = LedgerSteps::new(ledger.client.clone(), DevSigner, MemoryWormStore::default()).with_patience(Duration::from_millis(300));
    match run_end_of_day(&impatient, &ledger.input(day(2))).await {
        Err(EndOfDayError::Step(e @ StepError::Unavailable(_))) => assert!(e.is_retryable()),
        other => panic!("an outage should be reported as unavailable, got {other:?}"),
    }
    ledger.boot(1).await;
    close(&steps, &ledger.input(day(2))).await;
    assert_eq!(ledger.books().await, simulated(1, 3));
    ledger.stop().await;
}

/// Purpose: a business date is due to close from the midnight that ends it in New Zealand
/// and not a second before, through both daylight saving changes; and the scheduled job's
/// reading of the ledger gives the partition count and the earliest date any partition is
/// on, or says the ledger is not open.
/// Why it matters: this decides when the insurer's day ends. The job that runs it may fire
/// early, late or twice. It must never close a day still in progress, which would put the
/// evening's payments on the wrong day, and after an outage it must close the oldest open
/// day first, which is the day a half-finished close left behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_day_is_due_only_after_its_midnight_in_new_zealand() {
    use chrono::{TimeZone, Utc};
    use ic_workflows::due::{cutoff_utc, is_due, ledger_day, LedgerDay};

    // Summer: New Zealand is thirteen hours ahead of UTC.
    let summer = common::d(2027, 12, 31);
    assert_eq!(cutoff_utc(summer), Some(Utc.with_ymd_and_hms(2027, 12, 31, 11, 0, 0).unwrap()));
    assert!(!is_due(summer, Utc.with_ymd_and_hms(2027, 12, 31, 10, 59, 59).unwrap()));
    assert!(is_due(summer, Utc.with_ymd_and_hms(2027, 12, 31, 11, 0, 0).unwrap()));
    // Winter: twelve.
    let winter = common::d(2028, 6, 30);
    assert!(!is_due(winter, Utc.with_ymd_and_hms(2028, 6, 30, 11, 59, 59).unwrap()));
    assert!(is_due(winter, Utc.with_ymd_and_hms(2028, 6, 30, 12, 0, 0).unwrap()));
    // The 25-hour day when daylight saving ends, and the 23-hour day when it starts.
    let long = cutoff_utc(common::d(2028, 4, 2)).unwrap() - cutoff_utc(common::d(2028, 4, 1)).unwrap();
    let short = cutoff_utc(common::d(2028, 9, 24)).unwrap() - cutoff_utc(common::d(2028, 9, 23)).unwrap();
    assert_eq!((long.num_hours(), short.num_hours()), (25, 23));
    // A date with no day after it is never due, rather than a panic.
    assert_eq!(cutoff_utc(chrono::NaiveDate::MAX), None);
    assert!(!is_due(chrono::NaiveDate::MAX, Utc::now()));
    // A day that ended long ago is due; the days of this test's ledger are years away.
    assert!(is_due(common::d(2020, 1, 1), Utc::now()));

    let ledger = Ledger::start("due", 1, 2).await;
    let patience = Duration::from_secs(20);
    let day = ledger_day(&ledger.client, patience).await.unwrap();
    assert_eq!(
        day,
        LedgerDay {
            business_date: common::opening(),
            partitions: 2
        }
    );
    // Part way through a close, one partition is a day ahead: the day still
    // to close is the earlier one.
    ledger.advance(1, common::day(1)).await;
    assert_eq!(ledger_day(&ledger.client, patience).await.unwrap().business_date, common::opening());
    ledger.stop().await;

    // A ledger nobody has opened, and one nobody can reach.
    let unopened = Ledger::start_empty("due-unopened", 1, 1).await;
    let error = ledger_day(&unopened.client, patience).await.unwrap_err();
    assert!(error.contains("ic-admin open"), "{error}");
    let client = unopened.client.clone();
    unopened.stop().await;
    let error = ledger_day(&client, Duration::from_millis(300)).await.unwrap_err();
    assert!(error.contains("no ledger node answered"), "{error}");
}

/// Steps that answer from a script, to put the run in situations a healthy
/// ledger never produces.
struct Scripted {
    /// Policies still unfinished after every round.
    unfinished: u32,
    /// What each partition reports as its in-transit net, and whether a
    /// transfer is in flight.
    in_transit: Vec<(&'static str, bool)>,
    /// What every pass over the unit orders reports.
    units: UnitPass,
    unit_passes: AtomicU32,
    published: AtomicU32,
}

impl Steps for Scripted {
    type Error = String;

    async fn cutoff(&self, step: PartitionStep) -> Result<ClosedDay, String> {
        Ok(ClosedDay {
            date: step.closing,
            closing_head: ic_ledger::Hash32::ZERO,
        })
    }

    async fn run_round(&self, _step: RoundStep) -> Result<u32, String> {
        Ok(self.unfinished)
    }

    async fn unit_orders(&self, _step: UnitStep) -> Result<UnitPass, String> {
        self.unit_passes.fetch_add(1, Ordering::SeqCst);
        Ok(self.units.clone())
    }

    async fn reconcile(&self, step: PartitionStep) -> Result<PartitionCheck, String> {
        let (net, in_flight) = self.in_transit[usize::from(step.partition)];
        Ok(PartitionCheck {
            breaks: vec![],
            transfer_in_transit: net.to_string(),
            transfer_in_flight: in_flight,
        })
    }

    async fn publish(&self, step: PublishStep) -> Result<PublishedSummary, String> {
        self.published.fetch_add(1, Ordering::SeqCst);
        Ok(PublishedSummary {
            name: root_name(step.closing),
            root: ic_ledger::Hash32::ZERO,
        })
    }
}

/// Purpose: the run's own judgements, on situations forced by scripted steps: money left
/// in transit between partitions with no transfer in flight is a break, the same money
/// with a transfer in flight is not, a partition whose in-transit total cannot be read is
/// a break, and policy work that never finishes stops the run as stuck before anything is
/// published. Unit orders that a pass cannot move are reported and do not stop the close,
/// and unit order work that never runs out is cut off after a fixed number of passes.
/// Each day has one workflow ID.
/// Why it matters: money in transit with nothing in flight means a transfer debited one
/// partition and never credited the other: the one imbalance no single partition can see.
/// This is the only check that would catch it, and a healthy ledger never trips it, so
/// without a forced case the check itself would never have been seen to work.
#[tokio::test]
async fn money_stranded_between_partitions_is_a_break_and_endless_work_is_stuck() {
    let input = EndOfDayInput::new(day(0), 2);
    assert_eq!(input.workflow_id(), format!("end-of-day-{}", day(0)));
    assert_eq!(EndOfDayInput::new(day(0), 64).workflow_id(), input.workflow_id(), "one day, one run");
    let scripted = |unfinished, in_transit| Scripted {
        unfinished,
        in_transit,
        units: UnitPass { progressed: false, left: 0 },
        unit_passes: AtomicU32::new(0),
        published: AtomicU32::new(0),
    };

    // Balanced: what left one partition arrived on the other.
    let balanced = run_end_of_day(&scripted(0, vec![("-5000", false), ("5000", false)]), &input).await.unwrap();
    assert_eq!(balanced.breaks, vec![]);
    assert!(balanced.unit_orders_left.is_empty());

    // Unit orders nothing can move (their policy is in flight, say) are reported, one pass
    // each partition, and the day still closes.
    let mut held = scripted(0, vec![("0", false), ("0", false)]);
    held.units = UnitPass { progressed: false, left: 4 };
    let report = run_end_of_day(&held, &input).await.unwrap();
    assert_eq!(report.unit_orders_left, [(0, 4), (1, 4)].into());
    assert_eq!((held.unit_passes.load(Ordering::SeqCst), held.published.load(Ordering::SeqCst)), (2, 1));
    // Work that keeps changing and never runs out is cut off, reported, and the day closes.
    let mut churning = scripted(0, vec![("0", false), ("0", false)]);
    churning.units = UnitPass { progressed: true, left: 7 };
    let report = run_end_of_day(&churning, &input).await.unwrap();
    assert_eq!(report.unit_orders_left, [(0, 7), (1, 7)].into());
    assert_eq!(churning.unit_passes.load(Ordering::SeqCst), 2 * ic_workflows::eod::MAX_UNIT_PASSES);

    // Stranded: 50.00 left partition 0 and is nowhere.
    let stranded = run_end_of_day(&scripted(0, vec![("-5000", false), ("0", false)]), &input).await.unwrap();
    assert_eq!(stranded.breaks.len(), 1);
    assert_eq!(stranded.breaks[0].check, "transfer_in_transit");
    assert!(stranded.breaks[0].detail.contains("-5000"), "{}", stranded.breaks[0].detail);

    // The same position mid-transfer is expected.
    let mid_transfer = run_end_of_day(&scripted(0, vec![("-5000", true), ("0", false)]), &input).await.unwrap();
    assert_eq!(mid_transfer.breaks, vec![]);

    // A total that is not a number is never read as zero.
    let unreadable = run_end_of_day(&scripted(0, vec![("0", false), ("fifty", false)]), &input).await.unwrap();
    assert_eq!(unreadable.breaks.len(), 1);
    assert!(unreadable.breaks[0].detail.contains("partition 1"));

    // Work that never finishes: stopped, and nothing published for the day.
    let endless = scripted(3, vec![("0", false), ("0", false)]);
    let stuck = run_end_of_day(&endless, &input).await.unwrap_err();
    assert_eq!(
        stuck,
        EndOfDayError::Stuck {
            partition: 0,
            rounds: ic_workflows::eod::MAX_ROUNDS,
            unfinished: 3
        }
    );
    assert!(stuck.to_string().contains("still has 3 unfinished policies"));
    assert_eq!(endless.published.load(Ordering::SeqCst), 0);
}

/// Purpose: on real ledger nodes, unit orders wait while their date has no price, the
/// order run executes them once a price is active, and the close replays them when the
/// price is corrected, using the fund's approved minimum to decide which differences are
/// paid. Running either again changes nothing.
/// Why it matters: the core only executes and replays when it is told to. This run is the
/// thing that tells it, so if it missed a fund, sent a wrong threshold or doubled a
/// command on a retry, customers would hold the wrong number of units with every
/// reconciliation in the core still clean.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_run_executes_unit_orders_and_replays_a_corrected_price() {
    use ic_core::invest::{Direction, FundDefinition, InvestCommand, NewOrder, OrderAmount, OrderKind, Question, Reply, UnitPrice};
    use ic_core::Command;
    use ic_money::{Currency::NZD, DecStr, FundId, Money, RoundingMode};

    let ledger = Ledger::start("units", 1, 1).await;
    let steps = ledger.steps();
    let policy = common::first_policy();
    let growth = FundId("growth".to_string());
    let invest = |command: InvestCommand| Command::Investment(Box::new(command));
    let fund = FundDefinition {
        id: growth.clone(),
        version: 1,
        content_hash: "hash-growth-1".to_string(),
        name: "Growth".to_string(),
        currency: NZD,
        cut_off: "14:00".to_string(),
        buy_spread_bps: 0,
        sell_spread_bps: 0,
        price_decimals: 4,
        unit_rounding: RoundingMode::HalfEven,
        non_pricing_days: Default::default(),
        open_to_new_money: true,
        // Differences worth less than NZD 50 on an order are recorded and not paid.
        pricing: [("compensation_minimum_minor".to_string(), "5000".to_string())].into(),
    };
    ledger.run(0, "fund", "config", invest(InvestCommand::ActivateFund(Box::new(fund)))).await;
    let receipt = Command::ApplyReceipt {
        policy: policy.clone(),
        receipt: "r-units".to_string(),
        amount: Money::new(5_000_00, NZD),
    };
    ledger.run(0, "receipt", "cash-matching", receipt).await;
    let allocate = InvestCommand::AllocateCash {
        policy: policy.clone(),
        reference: "alloc-1".to_string(),
        amount_minor: 1_000_00,
    };
    ledger.run(0, "allocate", "policy-servicing", invest(allocate)).await;
    for (id, minor) in [("buy-1", 900_00), ("buy-2", 100_00)] {
        let order = NewOrder {
            order_id: id.to_string(),
            policy: policy.clone(),
            fund: growth.clone(),
            direction: Direction::Buy,
            amount: OrderAmount::Cash { minor },
            kind: OrderKind::PremiumAllocation,
            pricing_date: day(0),
            switch_to: None,
        };
        ledger.run(0, id, "policy-servicing", invest(InvestCommand::PlaceOrder(Box::new(order)))).await;
    }
    let units = || async {
        match ledger.client.investment(0, &Question::Holdings { policy: policy.clone() }).await.unwrap() {
            Reply::Holdings(Some(h)) => (h.units_micro.get("growth").copied().unwrap_or(0), h.pending_cash_minor),
            other => panic!("{other:?}"),
        }
    };
    let price = |id: &str, at: &str, version: u32, supersedes: Option<&str>| {
        invest(InvestCommand::ActivateUnitPrice(Box::new(UnitPrice {
            price_id: id.to_string(),
            fund: growth.clone(),
            pricing_date: day(0),
            version,
            buy: DecStr::parse(at).unwrap(),
            sell: DecStr::parse(at).unwrap(),
            mid: DecStr::parse(at).unwrap(),
            inputs_hash: format!("inputs-{id}"),
            supersedes: supersedes.map(str::to_string),
        })))
    };

    // No price yet: the orders wait, through the run and through the close.
    assert_eq!(run_unit_orders(&steps, 0, day(0)).await.unwrap(), 0);
    let report = close(&steps, &ledger.input(day(0))).await;
    assert!(report.unit_orders_left.is_empty());
    assert_eq!(units().await, (0, 1_000_00));

    // The price for the day is struck the next morning and the run executes both orders.
    ledger.run(0, "p-1", "unit-pricing", price("p-1", "2.0000", 1, None)).await;
    assert_eq!(run_unit_orders(&steps, 0, day(1)).await.unwrap(), 0);
    assert_eq!(units().await, (500_000_000, 0));
    assert_eq!(run_unit_orders(&steps, 0, day(1)).await.unwrap(), 0);
    assert_eq!(units().await, (500_000_000, 0));

    // The price should have been 2.5000. The close replays: 900.00 bought 360 units, not
    // 450, and 90 come back; 100.00 bought 40, not 50, a difference of NZD 25, under the
    // fund's minimum, so that order is left as it is.
    ledger.run(0, "p-2", "unit-pricing", price("p-2", "2.5000", 2, Some("p-1"))).await;
    let first = close(&steps, &ledger.input(day(1))).await;
    assert!(first.unit_orders_left.is_empty());
    assert_eq!(units().await, (410_000_000, 0));
    assert_eq!(ledger.client.investment(0, &Question::ToReplay).await.unwrap(), Reply::ToReplay(vec![]));
    let again = close(&steps, &ledger.input(day(1))).await;
    assert_eq!(again, first);
    assert_eq!(units().await, (410_000_000, 0));
    ledger.stop().await;
}
