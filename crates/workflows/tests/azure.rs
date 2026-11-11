//! The worker's settings turned into real steps: the day's root signed by
//! Key Vault and written to Blob Storage, against a local stand-in, and the
//! directory and in-memory arrangements used off Azure.
//!
//! This file holds one test because it sets the process environment the
//! workload identity webhook would set.

mod common;

use std::collections::BTreeMap;

use common::{day, scratch, Ledger};
use ic_azure::merkle::{root_name, verify_published, PublishedRoot};
use ic_azure::testing::FakeAzure;
use ic_workflows::settings::WorkerSettings;
use ic_workflows::{run_end_of_day, EndOfDayError};

/// Purpose: a worker configured from its environment for Azure closes a day and leaves the
/// signed root in the immutable container under the worker's own identity; an Azure
/// outage fails the publishing step in a way the run can retry, with the ledger's day
/// already safely closed, and the retry publishes the same root; and the same settings
/// pointed at a directory, or at nothing, close the following days the same way.
/// Why it matters: the publish step is the last step of the close and the only one that
/// depends on Azure. If an outage there were treated as fatal, a storage blip at midnight
/// would need a person; if it lost the root, the day would have no tamper evidence. The
/// other workflow tests use an in-memory store and a development signer, so without this
/// the worker's production wiring would be untested.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_worker_publishes_the_signed_root_through_an_azure_outage() {
    let azure = FakeAzure::start().await;
    let dir = scratch("worker-azure");
    let token_file = dir.join("federated-token");
    std::fs::write(&token_file, "projected-token").unwrap();
    std::env::set_var("AZURE_FEDERATED_TOKEN_FILE", &token_file);
    std::env::set_var("AZURE_TENANT_ID", "tenant-1");
    std::env::set_var("AZURE_CLIENT_ID", "client-worker");
    std::env::set_var("AZURE_AUTHORITY_HOST", &azure.base);

    let ledger = Ledger::start("worker-azure-ledger", 3, 2).await;
    let nodes = ledger.client.nodes().join(",");
    let settings = |extra: &[(&str, &str)]| {
        let mut vars: BTreeMap<String, String> = BTreeMap::from([
            ("IC_NODES".to_string(), nodes.clone()),
            ("IC_CLUSTER_TOKEN".to_string(), common::TOKEN.to_string()),
            ("IC_STEP_PATIENCE_SECS".to_string(), "2".to_string()),
            ("IC_STEP_TIMEOUT_SECS".to_string(), "5".to_string()),
        ]);
        vars.extend(extra.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())));
        WorkerSettings::from_lookup(|key| vars.get(key).cloned())
    };

    // On Azure. The outage begins before the close.
    let on_azure = settings(&[("IC_BLOB_ENDPOINT", &azure.base), ("IC_KEY_VAULT_URL", &azure.base)]).unwrap();
    let steps = on_azure.steps().unwrap();
    azure.set_down(true);
    let failed = run_end_of_day(&steps, &ledger.input(day(0))).await;
    match failed {
        Err(EndOfDayError::Step(e)) => assert!(e.is_retryable(), "an Azure outage was treated as final: {e}"),
        other => panic!("expected the publishing step to fail, got {other:?}"),
    }
    assert!(azure.blobs().is_empty());

    // Azure returns. The same run, repeated, finishes: the ledger's part is
    // not done twice and the root is published.
    azure.set_down(false);
    let report = run_end_of_day(&steps, &ledger.input(day(0))).await.unwrap();
    assert_eq!(report.closed, day(0));
    assert_eq!(report.breaks, vec![]);
    let blobs = azure.blobs();
    let stored = blobs
        .get(&format!("merkle-roots/{}", root_name(day(0))))
        .expect("the root is not in the container");
    let published: PublishedRoot = serde_json::from_slice(stored).unwrap();
    assert!(verify_published(&published));
    assert_eq!(published.root, report.published.root);
    assert_eq!(published.signature.key_id, "https://fake.vault/keys/merkle-root-signing/v7");
    assert!(azure.seen().iter().any(|s| s.contains("client_id=client-worker")));
    // And once more: nothing changes.
    assert_eq!(run_end_of_day(&steps, &ledger.input(day(0))).await.unwrap(), report);
    assert_eq!(azure.blobs().len(), 1);

    // Off Azure, to a directory.
    let roots = dir.join("roots");
    let to_dir = settings(&[("IC_ROOT_DIR", roots.to_str().unwrap())]).unwrap();
    let report = run_end_of_day(&to_dir.steps().unwrap(), &ledger.input(day(1))).await.unwrap();
    let on_disk: PublishedRoot = serde_json::from_slice(&std::fs::read(roots.join(root_name(day(1)))).unwrap()).unwrap();
    assert_eq!(on_disk.root, report.published.root);
    assert_eq!(azure.blobs().len(), 1, "a worker configured for a directory wrote to Azure");

    // With nowhere configured the root is held in memory: a run still completes.
    let in_memory = settings(&[]).unwrap();
    let report = run_end_of_day(&in_memory.steps().unwrap(), &ledger.input(day(2))).await.unwrap();
    assert_eq!(report.closed, day(2));

    // What the settings refuse.
    for (extra, says) in [
        (vec![("IC_BLOB_ENDPOINT", "https://x.blob.core.windows.net")], "IC_KEY_VAULT_URL"),
        (vec![("IC_BLOB_ENDPOINT", "https://x"), ("IC_ROOT_DIR", "/tmp/roots")], "not both"),
        (vec![("IC_STEP_TIMEOUT_SECS", "2")], "must exceed"),
        (vec![("IC_STEP_PATIENCE_SECS", "soon")], "IC_STEP_PATIENCE_SECS"),
        (vec![("IC_STEP_TIMEOUT_SECS", "later")], "IC_STEP_TIMEOUT_SECS"),
        (vec![("IC_NODES", " ")], "IC_NODES is required"),
    ] {
        let refused = settings(&extra).err().unwrap_or_else(|| panic!("{extra:?} was accepted"));
        assert!(refused.contains(says), "{extra:?}: {refused}");
    }
    ledger.stop().await;
    let _ = std::fs::remove_dir_all(&dir);
}
