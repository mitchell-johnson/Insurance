//! Encryption and the vault, against in-memory storage and against a local
//! stand-in for Azure Blob Storage and Key Vault.

#[path = "support/azure.rs"]
mod azure;
mod support;

use azure::Fake;
use chrono::NaiveDate;
use ic_documents::crypto::{self, CryptoError, KeyVaultWrap, KeyWrap, LocalWrap};
use ic_documents::model::Document;
use ic_documents::vault::{self, BlobVault, Contents, MemoryStore, ObjectStore, VaultError};
use serde_json::json;

fn contents() -> Contents {
    Contents {
        text: "Dear Aroha. Your premium will be $123.45.".into(),
        data: json!({"name": "Aroha", "premium": 12345}),
        pdf: None,
    }
}

/// The module's record of a stored document, as the state would hold it.
fn record(id: &str, stored: &vault::Stored) -> Document {
    let mut kit = support::Kit::ready();
    let mut spec = kit.issue_spec(id, ic_documents::DocumentKind::PremiumReceipt, "receipt");
    spec.blob = stored.blob.clone();
    spec.wrapped_key = stored.wrapped_key.clone();
    spec.content_sha256 = stored.content_sha256.clone();
    spec.data_sha256 = stored.data_sha256.clone();
    kit.issue(spec);
    kit.state.documents[id].clone()
}

/// Purpose: sealed bytes open only with the same key and the same context,
/// and any change to them is detected.
/// Why it matters: this is the whole protection of a customer's documents
/// at rest. Bytes that opened under another document's context could be
/// swapped between customers; bytes that opened after being changed would
/// let a stored notice be altered unnoticed.
#[test]
fn sealed_bytes_open_only_with_their_key_and_context() {
    let key = crypto::new_key();
    let sealed = crypto::seal(&key, b"doc-1", b"secret").unwrap();
    assert_eq!(crypto::open(&key, b"doc-1", &sealed).unwrap(), b"secret");
    assert_ne!(crypto::seal(&key, b"doc-1", b"secret").unwrap(), sealed, "every sealing uses a fresh nonce");
    assert_eq!(crypto::open(&key, b"doc-2", &sealed), Err(CryptoError::Unreadable));
    assert_eq!(crypto::open(&crypto::new_key(), b"doc-1", &sealed), Err(CryptoError::Unreadable));
    let mut changed = sealed.clone();
    *changed.last_mut().unwrap() ^= 1;
    assert_eq!(crypto::open(&key, b"doc-1", &changed), Err(CryptoError::Unreadable));
    assert_eq!(crypto::open(&key, b"doc-1", &sealed[..5]), Err(CryptoError::Unreadable));
    assert_eq!(crypto::seal(&key[..10], b"x", b"y"), Err(CryptoError::BadKey));
    assert_eq!(crypto::open(&key[..10], b"x", &sealed), Err(CryptoError::BadKey));
}

/// Purpose: a document's key wrapped under its owner's key comes back only
/// under that owner and that document, and malformed wrappings are refused.
/// Why it matters: crypto-shredding rests on this chain. If a document key
/// could be unwrapped without the owner's key, dropping the owner's key
/// would erase nothing.
#[tokio::test]
async fn a_document_key_is_bound_to_its_owner_and_document() {
    let owner = crypto::new_key();
    let key = crypto::new_key();
    let wrapped = crypto::wrap_under(&owner, "doc-1", &key).unwrap();
    assert_eq!(crypto::unwrap_under(&owner, "doc-1", &wrapped).unwrap(), key);
    assert_eq!(crypto::unwrap_under(&owner, "doc-2", &wrapped), Err(CryptoError::Unreadable));
    assert_eq!(crypto::unwrap_under(&crypto::new_key(), "doc-1", &wrapped), Err(CryptoError::Unreadable));
    assert_eq!(crypto::unwrap_under(&owner, "doc-1", "not base64!"), Err(CryptoError::Unreadable));
    // Something sealed under the owner that is not a key.
    let short = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, crypto::seal(&owner, b"doc-1", b"short").unwrap());
    assert_eq!(crypto::unwrap_under(&owner, "doc-1", &short), Err(CryptoError::BadKey));

    let local = LocalWrap::new(crypto::new_key());
    let wrapped = local.wrap(&owner).await.unwrap();
    assert_eq!(local.unwrap(&wrapped).await.unwrap(), owner);
    assert!(LocalWrap::new(crypto::new_key()).unwrap(&wrapped).await.is_err());
}

/// Purpose: an owner's key is wrapped and unwrapped by Key Vault, the
/// wrapping names the key version that made it, and the service is never
/// asked to call an address outside its own vault.
/// Why it matters: the key that protects every customer's key must never
/// leave Key Vault, old wrappings must survive a key rotation, and a
/// wrapped value tampered to point at another host must not make this
/// service send it a token.
#[tokio::test]
async fn owner_keys_are_wrapped_by_key_vault() {
    let fake = Fake::start().await;
    let wrap = KeyVaultWrap::new(&format!("{}/", fake.base), "documents-kek", Fake::credential());
    let key = crypto::new_key();
    let wrapped = wrap.wrap(&key).await.unwrap();
    assert!(wrapped.starts_with(&format!("{}/keys/documents-kek/v7 ", fake.base)));
    assert_eq!(wrap.unwrap(&wrapped).await.unwrap(), key);

    assert_eq!(wrap.unwrap("no-space").await, Err(CryptoError::Unreadable));
    let elsewhere = wrapped.replace(&fake.base, "http://127.0.0.1:1");
    assert_eq!(wrap.unwrap(&elsewhere).await, Err(CryptoError::Unreadable));
    assert_eq!(fake.lock().calls.len(), 2, "the tampered value caused no call");

    // An outage is reported as the key service failing, to be retried.
    fake.force("keyvault", 503);
    assert!(matches!(wrap.wrap(&key).await, Err(CryptoError::KeyService(m)) if m.contains("503")));
    assert!(matches!(wrap.unwrap(&wrapped).await, Err(CryptoError::KeyService(_))));
    fake.force("keyvault", 0);

    // Answers that cannot be right are refused, not used.
    fake.body("wrap", json!({ "value": "abc" }));
    assert!(matches!(wrap.wrap(&key).await, Err(CryptoError::KeyService(_))));
    fake.body("wrap", json!("not an object"));
    assert!(matches!(wrap.wrap(&key).await, Err(CryptoError::KeyService(_))));
    fake.body("unwrap", json!({ "value": "!!!" }));
    assert_eq!(wrap.unwrap(&wrapped).await, Err(CryptoError::Unreadable));
    fake.body("unwrap", json!({ "value": "YWJj" }));
    assert_eq!(wrap.unwrap(&wrapped).await, Err(CryptoError::BadKey));

    // A Managed HSM is asked for a token for the HSM, and an unreachable
    // one is an error, not a hang.
    let hsm = KeyVaultWrap::new("http://127.0.0.1:1/x.managedhsm.", "kek", Fake::credential());
    assert!(matches!(hsm.wrap(&key).await, Err(CryptoError::KeyService(_))));
}

/// Purpose: a stored document comes back exactly as issued with its data,
/// under a name no retry reuses, and nothing readable is in storage.
/// Why it matters: the vault is the insurer's evidence of what a customer
/// was told. It must return the same bytes for decades, and the stored
/// object must give nothing away to someone who reads the container.
#[tokio::test]
async fn a_document_is_stored_encrypted_and_read_back_as_issued() {
    let objects = MemoryStore::default();
    let owner = crypto::new_key();
    let until = NaiveDate::from_ymd_opt(2034, 11, 15);
    let stored = vault::store(&objects, &owner, "d1", &contents(), until).await.unwrap();
    let again = vault::store(&objects, &owner, "d1", &contents(), None).await.unwrap();
    assert_ne!(stored.blob, again.blob, "a retry writes a new object");
    assert_eq!(stored.content_sha256, again.content_sha256);
    assert_eq!(objects.names().len(), 2);
    assert_eq!(objects.kept_until(&stored.blob), until);
    assert_eq!(objects.kept_until(&again.blob), None);
    assert_eq!(stored.bytes, contents().text.len() as u64);

    let raw = objects.get(&stored.blob).await.unwrap().unwrap();
    assert!(!String::from_utf8_lossy(&raw).contains("Aroha"));

    let doc = record("d1", &stored);
    assert_eq!(vault::fetch(&objects, &owner, &doc).await.unwrap(), contents());
    // The orphan from the retry cannot be opened with the recorded key.
    let mut orphan = doc.clone();
    orphan.blob = again.blob.clone();
    assert_eq!(vault::fetch(&objects, &owner, &orphan).await, Err(VaultError::Crypto(CryptoError::Unreadable)));
    assert_eq!(
        objects.put_new(&stored.blob, vec![1], None).await,
        Err(VaultError::AlreadyExists(stored.blob.clone()))
    );
}

/// Purpose: every way a stored document can be wrong or gone is reported
/// as what it is: destroyed, missing, changed, not matching its hash, or
/// storage being down.
/// Why it matters: a document that silently read back wrong would be
/// served to a customer or a court as what was sent. Each failure needs a
/// different response from the operator, so each must be told apart.
#[tokio::test]
async fn a_damaged_or_destroyed_document_is_reported_not_served() {
    let objects = MemoryStore::default();
    let owner = crypto::new_key();
    let stored = vault::store(&objects, &owner, "d1", &contents(), None).await.unwrap();
    let doc = record("d1", &stored);

    let mut destroyed = doc.clone();
    destroyed.wrapped_key = None;
    assert_eq!(vault::fetch(&objects, &owner, &destroyed).await, Err(VaultError::Destroyed));

    let mut wrong_hash = doc.clone();
    wrong_hash.content_sha256 = "0".repeat(64);
    assert_eq!(vault::fetch(&objects, &owner, &wrong_hash).await, Err(VaultError::Tampered));
    let mut wrong_data = doc.clone();
    wrong_data.data_sha256 = Some("0".repeat(64));
    assert_eq!(vault::fetch(&objects, &owner, &wrong_data).await, Err(VaultError::Tampered));

    // Sealed correctly, but not a document.
    let key = crypto::unwrap_under(&owner, "d1", &stored.wrapped_key).unwrap();
    objects.put_new("odd", crypto::seal(&key, b"d1", b"not json").unwrap(), None).await.unwrap();
    let mut odd = doc.clone();
    odd.blob = "odd".into();
    assert_eq!(vault::fetch(&objects, &owner, &odd).await, Err(VaultError::Tampered));

    objects.corrupt(&stored.blob);
    assert_eq!(vault::fetch(&objects, &owner, &doc).await, Err(VaultError::Crypto(CryptoError::Unreadable)));

    objects.set_down(true);
    assert!(matches!(vault::fetch(&objects, &owner, &doc).await, Err(VaultError::Storage(_))));
    assert!(matches!(
        vault::store(&objects, &owner, "d2", &contents(), None).await,
        Err(VaultError::Storage(_))
    ));
    assert!(matches!(objects.delete(&stored.blob).await, Err(VaultError::Storage(_))));
    objects.set_down(false);

    objects.delete(&stored.blob).await.unwrap();
    objects.delete(&stored.blob).await.unwrap();
    assert_eq!(vault::fetch(&objects, &owner, &doc).await, Err(VaultError::Missing("d1".into())));
}

/// Purpose: the Blob Storage vault writes once, asks storage to lock a
/// document for its retention period, reads back, deletes, and reports a
/// locked object and an outage as what they are.
/// Why it matters: this is the adapter production runs on. A write that
/// could overwrite, a lock that was not asked for, or a locked delete
/// mistaken for success would each quietly break the evidence the vault
/// exists to keep.
#[tokio::test]
async fn the_blob_vault_writes_once_locks_and_deletes() {
    let fake = Fake::start().await;
    let blobs = BlobVault::new(&format!("{}/blob/", fake.base), "vault", Fake::credential());
    let until = NaiveDate::from_ymd_opt(2034, 11, 15);

    blobs.put_new("documents/d1/a", b"one".to_vec(), until).await.unwrap();
    blobs.put_new("documents/d1/b", b"two".to_vec(), None).await.unwrap();
    assert_eq!(fake.lock().locks["vault/documents/d1/a"], "Wed, 15 Nov 2034 00:00:00 GMT");
    assert_eq!(
        blobs.put_new("documents/d1/a", b"x".to_vec(), None).await,
        Err(VaultError::AlreadyExists("documents/d1/a".into()))
    );
    assert_eq!(blobs.get("documents/d1/a").await.unwrap().unwrap(), b"one");
    assert_eq!(blobs.get("documents/none").await.unwrap(), None);

    assert_eq!(blobs.delete("documents/d1/a").await, Err(VaultError::Locked("documents/d1/a".into())));
    blobs.delete("documents/d1/b").await.unwrap();
    blobs.delete("documents/d1/b").await.unwrap();
    assert_eq!(blobs.get("documents/d1/b").await.unwrap(), None);

    fake.force("blob", 503);
    for result in [
        blobs.put_new("n", vec![], None).await,
        blobs.get("n").await.map(|_| ()),
        blobs.delete("n").await,
    ] {
        assert_eq!(result, Err(VaultError::Storage("Blob Storage returned 503".into())));
    }
    let nowhere = BlobVault::new("http://127.0.0.1:1", "vault", Fake::credential());
    assert!(matches!(nowhere.get("n").await, Err(VaultError::Storage(_))));
    assert!(matches!(nowhere.put_new("n", vec![], None).await, Err(VaultError::Storage(_))));
    assert!(matches!(nowhere.delete("n").await, Err(VaultError::Storage(_))));
}

/// Purpose: a PDF an external composer set is read back only when the
/// record says that composer set it and its hash matches; a PDF where the
/// record has none, a different PDF, a missing one, or one that is not
/// base64 is reported as tampered.
/// Why it matters: the composed PDF is the document as issued, so it must
/// be held to the same proof as the text. A swapped attachment that read
/// back without complaint would be served to a customer or a court as
/// what was sent.
#[tokio::test]
async fn a_composed_pdf_reads_back_only_as_recorded() {
    let pdf = b"%PDF-1.7 composed".to_vec();
    let objects = MemoryStore::default();
    let owner = crypto::new_key();
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &pdf);
    let composed = Contents {
        pdf: Some(encoded),
        ..contents()
    };
    let stored = vault::store(&objects, &owner, "d1", &composed, None).await.unwrap();
    let mut doc = record("d1", &stored);
    assert_eq!(
        vault::fetch(&objects, &owner, &doc).await,
        Err(VaultError::Tampered),
        "the record names no composer"
    );

    let rendition = ic_documents::compose::Rendition {
        composer: "Compose Platform".into(),
        version: "24.3".into(),
        pdf_sha256: Some(ic_documents::template::sha256_hex(&pdf)),
    };
    doc.rendition = Some(rendition.clone());
    let read = vault::fetch(&objects, &owner, &doc).await.unwrap();
    assert_eq!(read.issued_pdf().unwrap(), Some(pdf));

    doc.rendition = Some(ic_documents::compose::Rendition {
        pdf_sha256: Some("0".repeat(64)),
        ..rendition.clone()
    });
    assert_eq!(vault::fetch(&objects, &owner, &doc).await, Err(VaultError::Tampered), "another PDF");
    doc.rendition = Some(ic_documents::compose::Rendition {
        pdf_sha256: None,
        ..rendition.clone()
    });
    assert_eq!(
        vault::fetch(&objects, &owner, &doc).await,
        Err(VaultError::Tampered),
        "a PDF where none was set"
    );

    let garbled = Contents {
        pdf: Some("%%% not base64".into()),
        ..contents()
    };
    let stored = vault::store(&objects, &owner, "d2", &garbled, None).await.unwrap();
    let mut doc = record("d2", &stored);
    doc.rendition = Some(rendition);
    assert_eq!(vault::fetch(&objects, &owner, &doc).await, Err(VaultError::Tampered));
}
