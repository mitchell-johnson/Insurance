//! Where the day's signed Merkle root is written, and who signs it.
//!
//! On Azure the root goes to an immutable Blob Storage container and is
//! signed by a Key Vault HSM key. Off Azure it goes to a directory with the
//! same write-once rule, so a local cluster behaves the same way.

use ic_azure::blob::{BlobContainer, DirWormStore, MemoryWormStore, WormStore};
use ic_azure::keyvault::{DevSigner, KeyVaultSigner, RootSigner, Signature};
use ic_azure::AzureError;

pub enum Store {
    Azure(BlobContainer),
    Dir(DirWormStore),
    Memory(MemoryWormStore),
}

impl WormStore for Store {
    async fn put_new(&self, name: &str, content_type: &str, body: Vec<u8>) -> Result<(), AzureError> {
        match self {
            Store::Azure(s) => s.put_new(name, content_type, body).await,
            Store::Dir(s) => s.put_new(name, content_type, body).await,
            Store::Memory(s) => s.put_new(name, content_type, body).await,
        }
    }

    async fn get(&self, name: &str) -> Result<Option<Vec<u8>>, AzureError> {
        match self {
            Store::Azure(s) => s.get(name).await,
            Store::Dir(s) => s.get(name).await,
            Store::Memory(s) => s.get(name).await,
        }
    }
}

pub enum Signer {
    Azure(KeyVaultSigner),
    Dev(DevSigner),
}

impl RootSigner for Signer {
    async fn sign(&self, message: &[u8]) -> Result<Signature, AzureError> {
        match self {
            Signer::Azure(s) => s.sign(message).await,
            Signer::Dev(s) => s.sign(message).await,
        }
    }
}
