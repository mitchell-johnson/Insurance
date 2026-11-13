//! The close as a Temporal workflow.
//!
//! One run does what one pass of the [driver](crate::driver) does, with each
//! step as an activity: record the Merkle roots, then take each closing
//! period in turn. Temporal records each finished step, shows which one a
//! run is on, retries a step that could not reach the ledger or the storage
//! on its own, and hands a step to another worker when one dies. The
//! workflow holds no logic and no state: every step asks the ledger what is
//! outstanding and every command is applied once by its key, so a step run
//! twice does nothing the second time.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use ic_azure::blob::WormStore;
use serde::{Deserialize, Serialize};
use temporalio_client::{Client, WorkflowGetResultOptions, WorkflowIdConflictPolicy, WorkflowStartOptions};
use temporalio_macros::{activities, workflow, workflow_methods};
use temporalio_sdk::activities::{ActivityContext, ActivityError};
use temporalio_sdk::{ActivityOptions, ApplicationFailure, Runtime, Worker, WorkerOptions, WorkflowContext, WorkflowResult};

use crate::driver::{Books, Driver, DriverError, Modules, Pass};
use crate::period::PeriodId;

/// The task queue close workers poll.
pub const TASK_QUEUE: &str = "ic-finance-close";

type Step<'a, T> = Pin<Box<dyn Future<Output = Result<T, DriverError>> + Send + 'a>>;

/// The steps of a run, whatever the driver behind them talks to.
pub trait CloseSteps: Send + Sync + 'static {
    fn record_roots(&self) -> Step<'_, Pass>;
    fn closing(&self) -> Step<'_, Vec<PeriodId>>;
    fn work_period(&self, period: PeriodId, earlier_open: bool) -> Step<'_, (bool, Pass)>;
}

impl<B, M, R, X> CloseSteps for Driver<B, M, R, X>
where
    B: Books + 'static,
    M: Modules + 'static,
    R: WormStore + 'static,
    X: WormStore + 'static,
{
    fn record_roots(&self) -> Step<'_, Pass> {
        Box::pin(Driver::record_roots(self))
    }

    fn closing(&self) -> Step<'_, Vec<PeriodId>> {
        Box::pin(Driver::closing(self))
    }

    fn work_period(&self, period: PeriodId, earlier_open: bool) -> Step<'_, (bool, Pass)> {
        Box::pin(Driver::work_period(self, period, earlier_open))
    }
}

/// What a run is started with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloseRunInput {
    /// How long Temporal lets one attempt of a step run before it gives the
    /// step to another worker.
    pub step_timeout_secs: u64,
}

/// One period's turn in a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeriodStep {
    pub period: PeriodId,
    /// Whether a period before this one is still open.
    pub earlier_open: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeriodOutcome {
    pub closed: bool,
    pub pass: Pass,
}

/// The steps as Temporal activities.
pub struct CloseActivities {
    pub steps: Arc<dyn CloseSteps>,
}

/// Something out of reach is tried again; an answer that cannot be read
/// will not read better the second time.
fn failure(e: DriverError) -> ActivityError {
    ActivityError::application(match e {
        DriverError::Unavailable(_) => ApplicationFailure::new(e),
        DriverError::Answer(_) => ApplicationFailure::non_retryable(e),
    })
}

#[activities]
impl CloseActivities {
    #[activity]
    pub async fn record_roots(self: Arc<Self>, _ctx: ActivityContext, _input: CloseRunInput) -> Result<Pass, ActivityError> {
        self.steps.record_roots().await.map_err(failure)
    }

    #[activity]
    pub async fn closing(self: Arc<Self>, _ctx: ActivityContext, _input: CloseRunInput) -> Result<Vec<PeriodId>, ActivityError> {
        self.steps.closing().await.map_err(failure)
    }

    #[activity]
    pub async fn work_period(self: Arc<Self>, _ctx: ActivityContext, step: PeriodStep) -> Result<PeriodOutcome, ActivityError> {
        let (closed, pass) = self.steps.work_period(step.period, step.earlier_open).await.map_err(failure)?;
        Ok(PeriodOutcome { closed, pass })
    }
}

/// Add what one step did to what the run has done.
pub fn merge(into: &mut Pass, step: Pass) {
    into.roots_recorded.extend(step.roots_recorded);
    into.extracts_registered.extend(step.extracts_registered);
    into.reports_sent.extend(step.reports_sent);
    into.closed.extend(step.closed);
    into.waiting.extend(step.waiting);
}

#[workflow]
#[derive(Default)]
pub struct CloseRunWorkflow;

#[workflow_methods]
impl CloseRunWorkflow {
    #[run]
    pub async fn run(ctx: &mut WorkflowContext<Self>, input: CloseRunInput) -> WorkflowResult<Pass> {
        let options = || ActivityOptions::start_to_close_timeout(Duration::from_secs(input.step_timeout_secs));
        let mut pass = ctx.execute_activity(CloseActivities::record_roots, input.clone(), options()).await?;
        let periods = ctx.execute_activity(CloseActivities::closing, input.clone(), options()).await?;
        let mut earlier_open = false;
        for period in periods {
            let step = PeriodStep { period, earlier_open };
            let outcome = ctx.execute_activity(CloseActivities::work_period, step, options()).await?;
            earlier_open |= !outcome.closed;
            merge(&mut pass, outcome.pass);
        }
        Ok(pass)
    }
}

/// Run a worker until `stop` resolves. In-flight steps finish first.
pub async fn run_worker(client: Client, task_queue: &str, steps: Arc<dyn CloseSteps>, stop: impl Future) -> Result<(), String> {
    let runtime = Runtime::from_current_tokio(Default::default()).map_err(|e| e.to_string())?;
    let options = WorkerOptions::new(task_queue)
        .register_workflow::<CloseRunWorkflow>()
        .map_err(|e| e.to_string())?
        .register_activities(CloseActivities { steps })
        .build();
    let worker = Worker::new(&runtime, client, options).map_err(|e| e.to_string())?;
    worker.run_until(stop).await.map_err(|e| e.to_string())
}

/// Start a run, or attach to the one already running, and wait for what it
/// did. There is one run at a time for a task queue: the workflow is named
/// after the queue, so every starter names the same one.
pub async fn close_run(client: &Client, task_queue: &str, input: CloseRunInput) -> Result<Pass, String> {
    let options = WorkflowStartOptions::new(task_queue, task_queue)
        .id_conflict_policy(WorkflowIdConflictPolicy::UseExisting)
        .build();
    let handle = client.start_workflow(CloseRunWorkflow::run, input, options).await.map_err(|e| e.to_string())?;
    // The debug form: it carries the failed step's own message.
    handle.get_result(WorkflowGetResultOptions::default()).await.map_err(|e| format!("{e}: {e:?}"))
}
