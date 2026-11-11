//! Where the day's signed Merkle root is written, and who signs it.
//!
//! On Azure the root goes to an immutable Blob Storage container and is
//! signed by a Key Vault HSM key. Off Azure it goes to a directory with the
//! same write-once rule, so a local cluster behaves the same way.

use std::io::Write;
use std::path::{Path, PathBuf};

use ic_azure::blob::{BlobContainer, MemoryWormStore, WormStore};
use ic_azure::keyvault::{DevSigner, KeyVaultSigner, RootSigner, Signature};
use ic_azure::AzureError;

/// A write-once store in a directory. An object, once written, is never
/// replaced: the second writer of a name is refused.
#[derive(Debug, Clone)]
pub struct DirWormStore {
    root: PathBuf,
}

impl DirWormStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn path(&self, name: &str) -> Result<PathBuf, AzureError> {
        let relative = Path::new(name);
        // Names come from the code, not from callers; refuse anything that
        // could climb out of the directory all the same.
        if relative.is_absolute() || relative.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
            return Err(AzureError::Response(format!("not a store name: {name}")));
        }
        Ok(self.root.join(relative))
    }
}

impl WormStore for DirWormStore {
    async fn put_new(&self, name: &str, _content_type: &str, body: Vec<u8>) -> Result<(), AzureError> {
        let path = self.path(name)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Write the whole object beside its name, then link it into place.
        // The link fails if the name exists, so a reader never sees half an
        // object and a second writer never replaces the first.
        let staged = path.with_extension(format!("{}.tmp", std::process::id()));
        let mut file = std::fs::File::create(&staged)?;
        file.write_all(&body)?;
        file.sync_all()?;
        let linked = std::fs::hard_link(&staged, &path);
        let _ = std::fs::remove_file(&staged);
        match linked {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(AzureError::AlreadyExists(name.to_string())),
            Err(e) => Err(e.into()),
        }
    }

    async fn get(&self, name: &str) -> Result<Option<Vec<u8>>, AzureError> {
        match std::fs::read(self.path(name)?) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

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
