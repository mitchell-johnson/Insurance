//! Key wrapping and erasable storage against the local stand-in for the
//! Azure REST endpoints.

use ic_azure::credential::AzureCredential;
use ic_azure::keywrap::{DevWrapper, KeyVaultWrapper, KeyWrapper, WrappedKey};
use ic_azure::records::{BlobRecords, MemoryRecords, RecordStore};
use ic_azure::testing::{FakeAzure, TOKEN};
use ic_azure::AzureError;

/// Purpose: a data key wrapped by Key Vault comes back naming the key version that wrapped it and
/// unwraps to the same bytes through that version; a stored key name that points at another
/// vault, another key or a path is refused before any request is made; a key wrapped under a
/// different key does not unwrap; and an outage is an error, not a wrong key.
/// Why it matters: every clinical record is readable only through this call. Losing the version
/// would orphan keys at the first rotation, and following a stored name blindly would send the
/// module's token to whatever URL someone managed to write into storage.
#[tokio::test]
async fn a_data_key_is_wrapped_and_unwrapped_by_the_version_that_wrapped_it() {
    let fake = FakeAzure::start().await;
    let cred = AzureCredential::static_token(TOKEN);
    let vault = KeyVaultWrapper::new(&format!("{}/", fake.base), "clinical-kek", cred.clone());
    let key = [42u8; 32];

    let wrapped = vault.wrap(&key).await.unwrap();
    assert_eq!(wrapped.key_id, format!("{}/keys/clinical-kek/v7", fake.base));
    assert_ne!(wrapped.value.as_bytes(), key);
    assert_eq!(vault.unwrap(&wrapped).await.unwrap(), key);
    assert_eq!(fake.seen(), vec!["wrap clinical-kek".to_string(), "unwrap clinical-kek/v7".to_string()]);

    // Names that are not a version of this key in this vault never leave the process.
    for key_id in [
        "https://elsewhere.example/keys/clinical-kek/v7".to_string(),
        format!("{}/keys/another-key/v7", fake.base),
        format!("{}/keys/clinical-kek/", fake.base),
        format!("{}/keys/clinical-kek/v7/../../other", fake.base),
        format!("{}/keys/clinical-kek/v7?x=1", fake.base),
    ] {
        let forged = WrappedKey {
            key_id,
            value: wrapped.value.clone(),
        };
        assert!(matches!(vault.unwrap(&forged).await, Err(AzureError::Response(_))));
    }
    assert_eq!(fake.seen().len(), 2, "a refused name made a request");

    // A version the vault no longer has, and a key wrapped under another key.
    let retired = WrappedKey {
        key_id: format!("{}/keys/clinical-kek/v6", fake.base),
        value: wrapped.value.clone(),
    };
    assert!(matches!(vault.unwrap(&retired).await, Err(AzureError::Status { status: 404, .. })));
    let other = KeyVaultWrapper::new(&fake.base, "another-key", cred.clone());
    let theirs = other.wrap(&key).await.unwrap();
    let crossed = WrappedKey {
        key_id: wrapped.key_id.clone(),
        value: theirs.value,
    };
    assert!(matches!(vault.unwrap(&crossed).await, Err(AzureError::Status { status: 400, .. })));

    // A caller without the vault's token is refused.
    let stranger = KeyVaultWrapper::new(&fake.base, "clinical-kek", AzureCredential::static_token("not-the-token"));
    assert!(matches!(stranger.wrap(&key).await, Err(AzureError::Status { status: 403, .. })));

    fake.set_down(true);
    assert!(matches!(vault.wrap(&key).await, Err(AzureError::Status { status: 503, .. })));
    assert!(matches!(vault.unwrap(&wrapped).await, Err(AzureError::Status { status: 503, .. })));
    fake.set_down(false);
    assert_eq!(vault.unwrap(&wrapped).await.unwrap(), key);

    // The stand-in for a laptop round-trips and refuses what is not a key.
    let dev = DevWrapper;
    let local = dev.wrap(&key).await.unwrap();
    assert_eq!(dev.unwrap(&local).await.unwrap(), key);
    let broken = WrappedKey {
        key_id: "dev".into(),
        value: "not base64url!".into(),
    };
    assert!(matches!(dev.unwrap(&broken).await, Err(AzureError::Response(_))));
}

/// Purpose: the erasable store writes a name once, reads it back, deletes it so that it is gone,
/// treats deleting a missing name as done, and reports an outage as an error; the Blob adapter
/// and the in-memory store behave alike.
/// Why it matters: erasure is a legal duty. A delete that fails when retried would leave an
/// erasure half done for ever, and an outage read as "not found" would look like a record that
/// had been erased when it had not.
#[tokio::test]
async fn erasable_records_are_written_once_and_deleted_for_good() {
    let fake = FakeAzure::start().await;
    let blob = BlobRecords::new(&format!("{}/", fake.base), "clinical", AzureCredential::static_token(TOKEN));
    let memory = MemoryRecords::default();

    async fn exercise(store: &impl RecordStore) {
        store.put_new("keys/party-1", b"wrapped".to_vec()).await.unwrap();
        assert!(matches!(
            store.put_new("keys/party-1", b"again".to_vec()).await,
            Err(AzureError::AlreadyExists(_))
        ));
        assert_eq!(store.get("keys/party-1").await.unwrap(), Some(b"wrapped".to_vec()));
        assert_eq!(store.get("keys/party-2").await.unwrap(), None);
        store.delete("keys/party-1").await.unwrap();
        assert_eq!(store.get("keys/party-1").await.unwrap(), None);
        // Retried, as after a crash between the delete and recording it.
        store.delete("keys/party-1").await.unwrap();
        // The name is free again only because the record is gone.
        store.put_new("keys/party-1", b"new".to_vec()).await.unwrap();
    }
    exercise(&blob).await;
    exercise(&memory).await;
    assert_eq!(fake.blobs().get("clinical/keys/party-1"), Some(&b"new".to_vec()));
    assert_eq!(fake.seen().iter().filter(|s| *s == "delete clinical/keys/party-1").count(), 2);
    assert_eq!(memory.names(), vec!["keys/party-1".to_string()]);
    memory.overwrite("keys/party-1", b"altered".to_vec());
    assert_eq!(memory.get("keys/party-1").await.unwrap(), Some(b"altered".to_vec()));

    fake.set_down(true);
    memory.set_down(true);
    for result in [blob.get("keys/party-1").await.map(|_| ()), memory.get("keys/party-1").await.map(|_| ())] {
        assert!(matches!(result, Err(AzureError::Status { status: 503, .. })));
    }
    for result in [blob.delete("keys/party-1").await, memory.delete("keys/party-1").await] {
        assert!(matches!(result, Err(AzureError::Status { status: 503, .. })));
    }
    for result in [blob.put_new("keys/party-3", vec![1]).await, memory.put_new("keys/party-3", vec![1]).await] {
        assert!(matches!(result, Err(AzureError::Status { status: 503, .. })));
    }

    let stranger = BlobRecords::new(&fake.base, "clinical", AzureCredential::static_token("not-the-token"));
    fake.set_down(false);
    assert!(matches!(stranger.delete("keys/party-1").await, Err(AzureError::Status { status: 403, .. })));
    assert!(fake.blobs().contains_key("clinical/keys/party-1"));
}
