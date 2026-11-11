//! The adapters against a local stand-in for the Azure REST endpoints, and
//! the Merkle root and cold tier paths against a real simulated ledger.

use chrono::NaiveDate;
use ic_azure::blob::{BlobContainer, MemoryWormStore, WormStore};
use ic_azure::cold_tier::{fetch_segment, seal_segment};
use ic_azure::credential::AzureCredential;
use ic_azure::eventhubs::{EventHubsSink, EventSink};
use ic_azure::keyvault::{DevSigner, KeyVaultSigner, RootSigner};
use ic_azure::merkle::{publish_root, root_name, verify_published, PublishedRoot};
use ic_azure::testing::FakeAzure;
use ic_azure::AzureError;
use ic_ledger::verify_chain;
use ic_product::{Frequency, Sex};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, term_life};
use ic_sim::Cluster;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn small_ledger() -> Cluster {
    let mut c = Cluster::in_memory(2, 1);
    c.open(d(2027, 11, 15)).unwrap();
    for cmd in shipped_activations().unwrap() {
        c.activate_everywhere(cmd).unwrap();
    }
    for n in 1..=10 {
        let issue = term_life(n, d(2027, 11, 15), d(1985, 1, 1), Sex::Male, false, 300_000_00, None, Frequency::Monthly, None);
        let p = c.partition_of(&issue.group);
        c.submit(p, "new-business", issue_command(issue)).unwrap();
    }
    c
}

/// Purpose: against a stand-in for Azure's HTTP endpoints, the workload identity credential
/// exchanges its federated token once per scope, Blob Storage writes once and refuses an overwrite,
/// Key Vault returns a signature naming the key version, and events for ledger partition n go to
/// hub partition n.
/// Why it matters: these adapters are the only code that talks to Azure, and no test here can reach
/// the real services. This pins the requests they make, so a change to a header or a URL is caught
/// before a deployment finds it.
#[tokio::test]
async fn workload_identity_blob_key_vault_and_event_hubs_speak_the_expected_protocols() {
    let fake = FakeAzure::start().await;
    let base = fake.base.clone();
    let token_file = std::env::temp_dir().join(format!("ic-azure-federated-{}", std::process::id()));
    std::fs::write(&token_file, "projected-service-account-token\n").unwrap();
    let cred = AzureCredential::workload_identity(&base, "tenant-1", "client-1", token_file.to_str().unwrap());

    // Blob Storage: write once, read back, refuse to overwrite.
    let store = BlobContainer::new(&base, "merkle-roots", cred.clone());
    store
        .put_new("merkle-roots/2027/2027-11-15.json", "application/json", b"{}".to_vec())
        .await
        .unwrap();
    assert_eq!(store.get("merkle-roots/2027/2027-11-15.json").await.unwrap(), Some(b"{}".to_vec()));
    assert!(matches!(
        store.put_new("merkle-roots/2027/2027-11-15.json", "application/json", b"x".to_vec()).await,
        Err(AzureError::AlreadyExists(_))
    ));
    assert_eq!(store.get("nothing-here").await.unwrap(), None);

    // Key Vault: the response names the key version that signed.
    let sig = KeyVaultSigner::new(&base, "merkle-root", cred.clone()).sign(b"root").await.unwrap();
    assert_eq!(sig.key_id, "https://fake.vault/keys/merkle-root/v7");
    assert!(sig.value.starts_with("sig-over-"));

    // Event Hubs: ledger partition n goes to hub partition n.
    let mut c = small_ledger();
    let events = c.take_events();
    let p1: Vec<_> = events.iter().filter(|e| e.partition == 1).cloned().collect();
    EventHubsSink::new(&base, "ledger-events", cred).publish(1, &p1).await.unwrap();

    let seen = fake.seen();
    // one token exchange per scope, carrying the federated assertion; later calls use the cache
    let tokens: Vec<_> = seen.iter().filter(|s| s.starts_with("token ")).collect();
    assert_eq!(tokens.len(), 3, "{seen:?}");
    assert!(tokens[0].contains("client_assertion=projected-service-account-token") && tokens[0].contains("client_id=client-1"));
    assert!(tokens[0].contains("scope=https%3A%2F%2Fstorage.azure.com%2F.default"));
    assert!(seen.iter().any(|s| s == "sign merkle-root"));
    assert!(seen.iter().any(|s| s.starts_with("send ledger-events/1 x")));
    let _ = std::fs::remove_file(token_file);
}

/// Purpose: the day's Merkle root is signed and stored, verifies, can be published again unchanged,
/// and a different root for the same day is refused.
/// Why it matters: the published root is the insurer's evidence that a closed day's books have not
/// changed since. It must be repeatable, because the close can be retried, and impossible to
/// replace.
#[tokio::test]
async fn the_days_merkle_root_is_signed_published_once_and_verifiable() {
    let mut c = small_ledger();
    let close = c.end_of_day().unwrap();
    let store = MemoryWormStore::default();
    let published = publish_root(&DevSigner, &store, close.closed, &close.heads).await.unwrap();
    assert_eq!(published.root, close.merkle_root);
    assert!(verify_published(&published));
    // publishing the same day again changes nothing
    assert_eq!(publish_root(&DevSigner, &store, close.closed, &close.heads).await.unwrap(), published);
    // a different root for a closed day is refused: closed days never change
    let mut tampered = close.heads.clone();
    tampered.insert(0, ic_ledger::Hash32::ZERO);
    assert!(publish_root(&DevSigner, &store, close.closed, &tampered).await.is_err());
    let stored: PublishedRoot = serde_json::from_slice(&store.get(&root_name(close.closed)).await.unwrap().unwrap()).unwrap();
    assert_eq!(stored, published);
}

/// Purpose: journal segments sealed in write-once storage link to each other as entries do, sealing
/// the same entries again changes nothing, altered content or a gap is refused, and the segments
/// read back verify as one chain to the ledger's head.
/// Why it matters: once sealed, entries are dropped from the nodes, so the cold tier is the only
/// copy of old history. It must be complete, in order, and safe for a new leader to repeat after
/// the old one died part way.
#[tokio::test]
async fn sealed_segments_link_to_the_chain_and_replay_from_the_cold_tier() {
    let mut c = small_ledger();
    for _ in 0..40 {
        c.end_of_day().unwrap();
    }
    let journal = c.nodes[&0].journal.clone();
    assert!(journal.len() > 100);
    let store = MemoryWormStore::default();
    let cut = journal.len() / 2;
    let a = seal_segment(&store, &journal[..cut]).await.unwrap();
    let b = seal_segment(&store, &journal[cut..]).await.unwrap();
    // segments link to each other exactly as entries do
    assert_eq!(a.last_hash, b.prev_hash);
    assert_eq!(b.last_hash, c.state(0).ledger.head());
    // sealing the same entries again changes nothing: a new leader repeats
    // what a failed one may already have done
    assert_eq!(seal_segment(&store, &journal[..cut]).await.unwrap(), a);
    // different content over a sealed range is refused by the write-once store
    let mut altered = journal[..cut].to_vec();
    altered[0].salt.push('x');
    assert!(matches!(seal_segment(&store, &altered).await, Err(AzureError::AlreadyExists(_))));
    // a gap is refused
    assert!(seal_segment(&store, &[journal[0].clone(), journal[2].clone()]).await.is_err());
    // fetched back, the two segments verify as one chain
    let mut all = fetch_segment(&store, 0, a.first_seq, a.last_seq).await.unwrap();
    all.extend(fetch_segment(&store, 0, b.first_seq, b.last_seq).await.unwrap());
    assert_eq!(verify_chain(0, &all).unwrap(), c.state(0).ledger.head());
}

/// Purpose: the directory store used off Azure is write-once like the real one: a name is
/// written once and read back, a second write of the name is refused and leaves the first
/// in place, a missing name reads as absent, and a name that would leave the directory is
/// refused.
/// Why it matters: the Compose cluster and single-machine installs keep sealed journal
/// segments and Merkle roots in this store. If it let a second writer replace an object,
/// the evidence that a closed day has not changed could itself be changed.
#[tokio::test]
async fn the_directory_store_writes_each_name_once() {
    use ic_azure::blob::DirWormStore;
    let dir = std::env::temp_dir().join(format!("ic-azure-dir-store-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = DirWormStore::new(&dir);
    assert_eq!(store.get("roots/2027/day.json").await.unwrap(), None);
    store.put_new("roots/2027/day.json", "application/json", b"first".to_vec()).await.unwrap();
    assert!(matches!(
        store.put_new("roots/2027/day.json", "application/json", b"second".to_vec()).await,
        Err(AzureError::AlreadyExists(name)) if name == "roots/2027/day.json"
    ));
    assert_eq!(store.get("roots/2027/day.json").await.unwrap(), Some(b"first".to_vec()));
    let left_behind: Vec<_> = std::fs::read_dir(dir.join("roots/2027")).unwrap().collect();
    assert_eq!(left_behind.len(), 1, "a staging file was left beside the object");

    for name in ["../outside", "/etc/passwd", "roots/../../outside"] {
        assert!(
            matches!(store.put_new(name, "text/plain", vec![]).await, Err(AzureError::Response(_))),
            "{name}"
        );
        assert!(matches!(store.get(name).await, Err(AzureError::Response(_))), "{name}");
    }
    // A directory where an object should be is an error, not an absent object.
    assert!(matches!(store.get("roots/2027").await, Err(AzureError::Io(_))));
    assert!(store.put_new("roots", "text/plain", vec![]).await.is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
