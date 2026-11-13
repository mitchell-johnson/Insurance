//! A Temporal server for a test: the one at `IC_TEST_TEMPORAL_ADDRESS` if
//! that is set (CI sets it), otherwise a dev server started from the
//! executable named by `IC_TEST_TEMPORAL_EXE`. `IC_SKIP_TEMPORAL_TESTS=1`
//! leaves the tests out on a machine with neither, and says so.

use std::time::Duration;

use temporalio_sdk::testing::{EphemeralExe, LocalServer, LocalWorkflowEnvironmentOptions, WorkflowEnvironment};

pub struct Temporal {
    pub address: String,
    /// The dev server, kept alive for as long as this is.
    _local: Option<WorkflowEnvironment<LocalServer>>,
}

pub fn skipped(what: &str) -> bool {
    let skip = std::env::var("IC_SKIP_TEMPORAL_TESTS").is_ok_and(|v| v == "1");
    if skip {
        eprintln!("IC_SKIP_TEMPORAL_TESTS=1: {what} was NOT run");
    }
    skip
}

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// A task queue of this test's own, so a shared server's other workers stay out.
pub fn task_queue(name: &str) -> String {
    format!("ic-finance-test-{name}-{}-{}", std::process::id(), free_port())
}

pub async fn start() -> Temporal {
    if let Ok(address) = std::env::var("IC_TEST_TEMPORAL_ADDRESS") {
        return Temporal { address, _local: None };
    }
    let exe = std::env::var("IC_TEST_TEMPORAL_EXE").expect(
        "no Temporal server: set IC_TEST_TEMPORAL_ADDRESS to a running server, IC_TEST_TEMPORAL_EXE to a dev server executable, or IC_SKIP_TEMPORAL_TESTS=1",
    );
    let port = free_port();
    let options = LocalWorkflowEnvironmentOptions::builder()
        .port(port)
        .startup_timeout(Duration::from_secs(60))
        .server_executable(EphemeralExe::ExistingPath(exe))
        .build();
    let local = WorkflowEnvironment::start_local(options).await.expect("could not start a Temporal dev server");
    Temporal {
        address: format!("127.0.0.1:{port}"),
        _local: Some(local),
    }
}
