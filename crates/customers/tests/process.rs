//! The process entry point, configured from the environment. Alone in its
//! file because it sets environment variables and signals itself.

mod common;

use std::time::Duration;

/// Purpose: the process reads its settings from the environment, refuses to start without
/// them, starts when they are complete, and stops cleanly on SIGTERM.
/// Why it matters: Kubernetes stops pods with SIGTERM on every deploy; a process that
/// ignored it would be killed mid-request, and one that started without its settings would
/// fail at the first request instead.
#[tokio::test]
async fn the_process_runs_from_the_environment_and_stops_on_sigterm() {
    let Some(server) = common::server() else { return };
    // Nothing set: refused with the setting's name.
    let e = ic_customers::work::main().await.unwrap_err();
    assert!(e.contains("IC_CUSTOMERS_INDEX_KEY"), "{e}");
    let db = common::Database::new(server).await;
    let dbname: String = db
        .connector
        .connect()
        .await
        .unwrap()
        .query_one("select current_database()", &[])
        .await
        .unwrap()
        .get(0);
    let azure = ic_azure::testing::FakeAzure::start().await;
    let authority = common::tenant().await;
    let url = std::env::var("IC_TEST_POSTGRES_URL").unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    for (k, v) in [
        ("IC_CUSTOMERS_DATABASE_URL", format!("{url} dbname={dbname}")),
        ("IC_CUSTOMERS_KEY_VAULT", azure.base.clone()),
        ("IC_CUSTOMERS_INDEX_KEY", common::INDEX_KEY.to_string()),
        ("IC_CUSTOMERS_TENANT", common::TENANT.to_string()),
        ("IC_CUSTOMERS_AUDIENCE", common::AUDIENCE.to_string()),
        ("IC_CUSTOMERS_AUTHORITY", authority),
        ("IC_EVENT_HUBS_NAMESPACE", azure.base.clone()),
        ("IC_CUSTOMERS_REGISTER_ENDPOINT", azure.base.clone()),
        ("IC_CUSTOMERS_LISTEN", format!("127.0.0.1:{port}")),
    ] {
        std::env::set_var(k, v);
    }
    let running = tokio::spawn(ic_customers::work::main());
    let mut up = false;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if reqwest::get(format!("http://127.0.0.1:{port}/healthz")).await.is_ok() {
            up = true;
            break;
        }
    }
    assert!(up, "the service did not start listening");
    let status = std::process::Command::new("kill")
        .args(["-TERM", &std::process::id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    tokio::time::timeout(Duration::from_secs(10), running).await.unwrap().unwrap().unwrap();
}
