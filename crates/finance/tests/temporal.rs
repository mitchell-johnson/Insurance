//! The close as a Temporal workflow, on a real Temporal server.

#[path = "support/temporal_server.rs"]
mod temporal_server;

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::NaiveDate;
use ic_finance::driver::{DriverError, Pass};
use ic_finance::temporal::{close_run, run_worker, CloseRunInput, CloseSteps};
use ic_finance::PeriodId;
use temporalio_client::envconfig::LoadClientConfigProfileOptions;
use temporalio_client::{Client, ClientOptions, Connection};

type Step<'a, T> = Pin<Box<dyn Future<Output = Result<T, DriverError>> + Send + 'a>>;

/// Steps that record how they were called and fail the way they are told to.
#[derive(Default)]
struct Scripted {
    calls: Mutex<Vec<String>>,
    /// How many more times recording the roots finds the storage out of reach.
    roots_down: Mutex<u32>,
    /// Whether the ledger's answer about its periods cannot be read.
    unreadable: Mutex<bool>,
}

impl Scripted {
    fn called(&self, what: String) {
        self.calls.lock().unwrap().push(what);
    }
}

fn month(m: u32) -> PeriodId {
    PeriodId::month(2027, m).unwrap()
}

impl CloseSteps for Scripted {
    fn record_roots(&self) -> Step<'_, Pass> {
        Box::pin(async move {
            self.called("roots".to_string());
            let mut down = self.roots_down.lock().unwrap();
            if *down > 0 {
                *down -= 1;
                return Err(DriverError::Unavailable("blob storage did not answer".to_string()));
            }
            Ok(Pass {
                roots_recorded: vec![NaiveDate::from_ymd_opt(2027, 12, 1).unwrap()],
                ..Pass::default()
            })
        })
    }

    fn closing(&self) -> Step<'_, Vec<PeriodId>> {
        Box::pin(async move {
            self.called("closing".to_string());
            if *self.unreadable.lock().unwrap() {
                return Err(DriverError::Answer("work: missing field `closing`".to_string()));
            }
            Ok(vec![month(10), month(11), month(12)])
        })
    }

    fn work_period(&self, period: PeriodId, earlier_open: bool) -> Step<'_, (bool, Pass)> {
        Box::pin(async move {
            self.called(format!("{period} earlier_open={earlier_open}"));
            // October closes; November waits for a signature; December is behind it.
            let closed = period == month(10);
            let pass = Pass {
                reports_sent: vec![format!("check-{period}")],
                closed: if closed { vec![period] } else { vec![] },
                waiting: if closed { vec![] } else { vec![format!("{period} is not signed off")] },
                ..Pass::default()
            };
            Ok((closed, pass))
        })
    }
}

/// Purpose: on a real Temporal server, a run records the roots, asks which periods are
/// closing and takes each in turn, telling each whether one before it is still open, and
/// returns everything the steps did as one report. A step that could not reach storage is
/// tried again by Temporal with no help from the service, and the run carries on from
/// there without repeating a finished step. A step whose answer cannot be read fails the
/// run at once, with the step's own words, and is not retried. Two starters at the same
/// moment get the same run.
/// Why it matters: this is what the workflow adds to the timed pass. A month-end is hours
/// of steps across several systems; when storage blinks or a pod is replaced the close
/// has to pick up at the step it was on, and someone has to be able to see which step
/// that is. A fault that will not mend itself must surface instead of being retried all
/// night.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_close_runs_as_a_workflow_step_by_step() {
    if temporal_server::skipped("the finance close workflow test") {
        return;
    }
    let server = temporal_server::start().await;
    // The only test in this file, so nothing else reads the environment meanwhile.
    std::env::set_var("TEMPORAL_ADDRESS", &server.address);
    std::env::set_var("TEMPORAL_NAMESPACE", "default");
    let connect = || async {
        let (connection, options) = ClientOptions::load_from_config(LoadClientConfigProfileOptions::default()).unwrap();
        Client::new(Connection::connect(connection).await.unwrap(), options).unwrap()
    };
    let queue = temporal_server::task_queue("steps");
    let steps = Arc::new(Scripted::default());
    *steps.roots_down.lock().unwrap() = 2;
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    // The worker is not `Send`: it runs beside the test's body on this task.
    let worker = run_worker(connect().await, &queue, steps.clone(), stopped);
    let body = async {
        let client = connect().await;
        let input = CloseRunInput { step_timeout_secs: 10 };

        let (first, second) = tokio::join!(close_run(&client, &queue, input.clone()), close_run(&client, &queue, input.clone()));
        let pass = first.unwrap();
        assert_eq!(second.unwrap(), pass, "two starters did not get the same run");
        assert_eq!(pass.roots_recorded.len(), 1);
        assert_eq!(pass.reports_sent, ["check-2027-10", "check-2027-11", "check-2027-12"]);
        assert_eq!(pass.closed, [month(10)]);
        assert_eq!(pass.waiting, ["2027-11 is not signed off", "2027-12 is not signed off"]);
        assert_eq!(
            *steps.calls.lock().unwrap(),
            [
                "roots",
                "roots",
                "roots",
                "closing",
                "2027-10 earlier_open=false",
                "2027-11 earlier_open=false",
                "2027-12 earlier_open=true"
            ],
            "one run, the roots tried three times and nothing else repeated"
        );

        // An answer that cannot be read: the run fails with the step's words and is not retried.
        steps.calls.lock().unwrap().clear();
        *steps.unreadable.lock().unwrap() = true;
        let failed = close_run(&client, &queue, input.clone()).await.unwrap_err();
        assert!(failed.contains("missing field `closing`"), "{failed}");
        assert_eq!(*steps.calls.lock().unwrap(), ["roots", "closing"]);

        // And the next run is a new one.
        *steps.unreadable.lock().unwrap() = false;
        assert_eq!(close_run(&client, &queue, input).await.unwrap().closed, [month(10)]);

        stop.send(()).unwrap();
    };
    let (stopped_cleanly, ()) = tokio::time::timeout(Duration::from_secs(120), async { tokio::join!(worker, body) })
        .await
        .expect("the run did not finish");
    stopped_cleanly.unwrap();
}
