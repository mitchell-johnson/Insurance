//! The end-of-day run as a Temporal workflow.
//!
//! The workflow holds no logic of its own: it runs [`run_end_of_day`] with
//! each step as an activity. Temporal records each finished step, so when a
//! worker dies another one continues from the step after the last recorded
//! one, and a step that was in flight is simply run again.

use std::sync::Arc;
use std::time::Duration;

use ic_core::machine::ClosedDay;
use temporalio_client::{Client, WorkflowGetResultOptions, WorkflowIdConflictPolicy, WorkflowStartOptions};
use temporalio_macros::{activities, workflow, workflow_methods};
use temporalio_sdk::activities::{ActivityContext, ActivityError};
use temporalio_sdk::{ActivityExecutionError, ActivityOptions, ApplicationFailure, Runtime, Worker, WorkerOptions, WorkflowContext, WorkflowResult};

use crate::eod::{
    run_end_of_day, EndOfDayError, EndOfDayInput, EndOfDayReport, PartitionCheck, PartitionStep, PublishStep, PublishedSummary, RoundStep, Steps,
};
use crate::steps::{LedgerSteps, StepError};
use crate::store::{Signer, Store};

/// The task queue end-of-day workers poll.
pub const TASK_QUEUE: &str = "ic-end-of-day";

/// The steps as Temporal activities.
pub struct EndOfDayActivities {
    pub steps: LedgerSteps<Signer, Store>,
}

/// Tell Temporal whether to try the step again.
fn failure(e: StepError) -> ActivityError {
    let retry = e.is_retryable();
    ActivityError::application(if retry {
        ApplicationFailure::new(e)
    } else {
        ApplicationFailure::non_retryable(e)
    })
}

#[activities]
impl EndOfDayActivities {
    #[activity]
    pub async fn cutoff(self: Arc<Self>, _ctx: ActivityContext, step: PartitionStep) -> Result<ClosedDay, ActivityError> {
        self.steps.cutoff(step).await.map_err(failure)
    }

    #[activity]
    pub async fn run_round(self: Arc<Self>, _ctx: ActivityContext, step: RoundStep) -> Result<u32, ActivityError> {
        self.steps.run_round(step).await.map_err(failure)
    }

    #[activity]
    pub async fn reconcile(self: Arc<Self>, _ctx: ActivityContext, step: PartitionStep) -> Result<PartitionCheck, ActivityError> {
        self.steps.reconcile(step).await.map_err(failure)
    }

    #[activity]
    pub async fn publish(self: Arc<Self>, _ctx: ActivityContext, step: PublishStep) -> Result<PublishedSummary, ActivityError> {
        self.steps.publish(step).await.map_err(failure)
    }
}

/// The steps as seen from inside the workflow: each one an activity.
struct ViaActivities {
    ctx: WorkflowContext<EndOfDayWorkflow>,
    step_timeout: Duration,
}

impl ViaActivities {
    fn options(&self) -> ActivityOptions {
        ActivityOptions::start_to_close_timeout(self.step_timeout)
    }
}

impl Steps for ViaActivities {
    type Error = ActivityExecutionError;

    async fn cutoff(&self, step: PartitionStep) -> Result<ClosedDay, ActivityExecutionError> {
        self.ctx.execute_activity(EndOfDayActivities::cutoff, step, self.options()).await
    }

    async fn run_round(&self, step: RoundStep) -> Result<u32, ActivityExecutionError> {
        self.ctx.execute_activity(EndOfDayActivities::run_round, step, self.options()).await
    }

    async fn reconcile(&self, step: PartitionStep) -> Result<PartitionCheck, ActivityExecutionError> {
        self.ctx.execute_activity(EndOfDayActivities::reconcile, step, self.options()).await
    }

    async fn publish(&self, step: PublishStep) -> Result<PublishedSummary, ActivityExecutionError> {
        self.ctx.execute_activity(EndOfDayActivities::publish, step, self.options()).await
    }
}

#[workflow]
#[derive(Default)]
pub struct EndOfDayWorkflow;

#[workflow_methods]
impl EndOfDayWorkflow {
    #[run]
    pub async fn run(ctx: &mut WorkflowContext<Self>, input: EndOfDayInput) -> WorkflowResult<EndOfDayReport> {
        let steps = ViaActivities {
            ctx: ctx.clone(),
            step_timeout: Duration::from_secs(input.step_timeout_secs),
        };
        match run_end_of_day(&steps, &input).await {
            Ok(report) => Ok(report),
            Err(EndOfDayError::Step(e)) => Err(e.into()),
            Err(stuck) => Err(ApplicationFailure::non_retryable(stuck.to_string()).into()),
        }
    }
}

/// Run a worker until `stop` resolves. In-flight steps finish first.
pub async fn run_worker(client: Client, task_queue: &str, steps: LedgerSteps<Signer, Store>, stop: impl std::future::Future) -> Result<(), String> {
    let runtime = Runtime::from_current_tokio(Default::default()).map_err(|e| e.to_string())?;
    let options = WorkerOptions::new(task_queue)
        .register_workflow::<EndOfDayWorkflow>()
        .map_err(|e| e.to_string())?
        .register_activities(EndOfDayActivities { steps })
        .build();
    let worker = Worker::new(&runtime, client, options).map_err(|e| e.to_string())?;
    worker.run_until(stop).await.map_err(|e| e.to_string())
}

/// Start the day's run, or attach to it if it is already running, and wait
/// for its report.
pub async fn close_day(client: &Client, task_queue: &str, input: EndOfDayInput) -> Result<EndOfDayReport, String> {
    let options = WorkflowStartOptions::new(task_queue, input.workflow_id())
        .id_conflict_policy(WorkflowIdConflictPolicy::UseExisting)
        .build();
    let handle = client.start_workflow(EndOfDayWorkflow::run, input, options).await.map_err(|e| e.to_string())?;
    // The debug form: it carries the failed step's own message, which the
    // display form leaves out.
    handle.get_result(WorkflowGetResultOptions::default()).await.map_err(|e| format!("{e}: {e:?}"))
}
