//! The single-process core wired to Azure services instead of their
//! in-memory stand-ins, against a local stand-in for the Azure endpoints.
//!
//! This file holds one test because it sets the process environment the
//! workload identity webhook would set.

mod common;

use common::d;
use ic_api::engine::{AzureConfig, Engine, EngineConfig};
use ic_azure::merkle::verify_published;
use ic_azure::testing::FakeAzure;
use ic_product::{Frequency, Sex};
use ic_sim::persona::{issue_command, term_life};

/// Purpose: with Blob Storage, Key Vault and Event Hubs configured, the core publishes
/// every event to the hub partition of its ledger partition and the day's signed Merkle
/// root to the container; and during an Azure outage it keeps taking business, holds the
/// events it could not publish, sends them once when the service returns even if the process
/// was restarted in between, and reports a close whose root could not be published as
/// exactly that.
/// Why it matters: Azure being briefly unavailable must not stop the insurer issuing
/// policies or taking payments, and must not lose events that downstream systems (the
/// document service, the data platform) are waiting for. A close that claimed a published
/// root it did not have would leave a day without tamper evidence and nobody aware of it.
#[tokio::test]
async fn the_core_rides_out_an_azure_outage_without_losing_events() {
    let azure = FakeAzure::start().await;
    let token_file = std::env::temp_dir().join(format!("ic-api-federated-{}", std::process::id()));
    std::fs::write(&token_file, "projected-token").unwrap();
    std::env::set_var("AZURE_FEDERATED_TOKEN_FILE", &token_file);
    std::env::set_var("AZURE_TENANT_ID", "tenant-1");
    std::env::set_var("AZURE_CLIENT_ID", "client-api");
    std::env::set_var("AZURE_AUTHORITY_HOST", &azure.base);

    let data = common::scratch("azure-box");
    let config = EngineConfig {
        partitions: 2,
        data_dir: Some(data.clone()),
        opening_date: d(2027, 11, 15),
        load_shipped_config: true,
        azure: AzureConfig {
            blob_endpoint: Some(azure.base.clone()),
            merkle_container: "merkle-roots".to_string(),
            key_vault_url: Some(azure.base.clone()),
            merkle_key: "merkle-root-signing".to_string(),
            event_hubs_endpoint: Some(azure.base.clone()),
            event_hub: "policy-events".to_string(),
        },
    };
    let mut engine = Engine::start(&config).unwrap();
    let issue = |engine: &Engine, n: u64| {
        let application = term_life(n, d(2027, 11, 15), d(1990, 5, 5), Sex::Male, false, 250_000_00, None, Frequency::Monthly, None);
        let partition = engine.cluster.partition_of(&application.group);
        (partition, issue_command(application))
    };

    let (p1, first) = issue(&engine, 1);
    let applied = engine.submit(p1, "app-1", "new-business", first).await;
    assert!(applied.result.is_ok());
    let sent_before = azure.events_sent("policy-events", p1);
    assert!(sent_before >= applied.events.len() && sent_before > 0, "{:?}", azure.seen());

    // The outage: business carries on, the events wait.
    azure.set_down(true);
    let (p2, second) = issue(&engine, 2);
    let during = engine.submit(p2, "app-2", "new-business", second).await;
    assert!(during.result.is_ok(), "an Azure outage stopped a policy being issued");
    let held_back = azure.events_sent("policy-events", p2);
    let close = engine.end_of_day().await.unwrap();
    assert!(close.breaks.is_empty());
    assert_eq!(close.business_date, Some(d(2027, 11, 16)), "the day closed in spite of the outage");
    assert_eq!(close.published_root, None);
    assert!(close.publish_error.as_deref().is_some_and(|e| e.contains("503")), "{:?}", close.publish_error);
    assert!(azure.blobs().is_empty());

    // The process dies with those events unsent, and Azure comes back.
    // The restarted process sends what the stream is missing and no more.
    drop(engine);
    azure.set_down(false);
    let mut engine = Engine::start(&config).unwrap();
    engine.relay().await;
    assert!(
        azure.events_sent("policy-events", p2) > held_back,
        "the events held during the outage were never sent"
    );
    let published: usize = (0..2).map(|p| azure.events_sent("policy-events", p)).sum();
    let emitted: u64 = (0..2).map(|p| engine.view.checkpoint(p)).sum();
    assert_eq!(published as u64, emitted, "events were lost or sent twice across the outage and the restart");

    // A restart with nothing owing sends nothing.
    drop(engine);
    let mut engine = Engine::start(&config).unwrap();
    engine.relay().await;
    let again: usize = (0..2).map(|p| azure.events_sent("policy-events", p)).sum();
    assert_eq!(again, published, "a restart re-sent events the stream already had");

    // The next close publishes its root, signed by the vault key.
    let close = engine.end_of_day().await.unwrap();
    let published = close.published_root.expect("the root was not published once Azure was back");
    assert!(verify_published(&published));
    assert_eq!(published.signature.key_id, "https://fake.vault/keys/merkle-root-signing/v7");
    assert_eq!(azure.blobs().len(), 1);
    assert!(azure.blobs().keys().all(|k| k.starts_with("merkle-roots/") && k.contains("2027-11-16")));
    let _ = std::fs::remove_dir_all(&data);
    assert!(azure.seen().iter().any(|s| s.contains("client_id=client-api")));
    let _ = std::fs::remove_file(token_file);
}
