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
use ic_workflows::eod::{PartitionCheck, PartitionStep, PublishStep, PublishedSummary, RoundStep};
use ic_workflows::{run_end_of_day, EndOfDayError, EndOfDayInput, EndOfDayReport, LedgerSteps, StepError, Steps};

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
