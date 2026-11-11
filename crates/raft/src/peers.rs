//! Telling a member from an impostor with its number.
//!
//! Raft is only safe if a member never forgets what it has acknowledged. A
//! node that comes back with an empty disk under its old number has done
//! exactly that: it may vote twice in a term, or let a leader believe an
//! entry is on a majority when it is not. The honest fix is to bring the
//! machine back as a new member with a new number.
//!
//! To make that the only thing that can happen, every node gives itself a
//! random incarnation the first time it starts on a disk, and every node
//! remembers, durably, the incarnation it first saw for each peer. A peer
//! that turns up with a different one is refused until an operator replaces
//! it. The same file catches a disk mounted on the wrong node.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::store::write_atomic;
use crate::types::NodeId;

/// The HTTP headers a node names itself with on Raft traffic.
pub const NODE_HEADER: &str = "x-ic-node";
pub const INCARNATION_HEADER: &str = "x-ic-incarnation";
/// The incarnation the caller remembers for the node it is calling, if it has
/// met it. A node that is not that incarnation must not act on the message.
pub const EXPECTS_HEADER: &str = "x-ic-expects";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct IdentityFile {
    id: NodeId,
    incarnation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("node {peer} is not the node {peer} this member knows: it has lost its disk or is another machine. Replace it as a new member with a new number.")]
pub struct Impostor {
    pub peer: NodeId,
}

/// Who this node is, and who it has met.
#[derive(Clone)]
pub struct Identity {
    pub id: NodeId,
    pub incarnation: String,
    dir: Option<PathBuf>,
    met: Arc<Mutex<BTreeMap<NodeId, String>>>,
}

fn invalid(e: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

fn fresh_incarnation() -> String {
    let bytes: [u8; 16] = rand::random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl Identity {
    /// An identity that lasts as long as the process. For members without a disk.
    pub fn ephemeral(id: NodeId) -> Self {
        Self {
            id,
            incarnation: fresh_incarnation(),
            dir: None,
            met: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// Load this node's identity from `dir`, or create it on a new disk.
    /// A disk that belongs to another node is refused.
    pub fn open(id: NodeId, dir: impl AsRef<Path>) -> io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("identity.json");
        let incarnation = match std::fs::read(&path) {
            Ok(bytes) => {
                let file: IdentityFile = serde_json::from_slice(&bytes).map_err(|e| invalid(format!("{}: {e}", path.display())))?;
                if file.id != id {
                    return Err(invalid(format!(
                        "{} belongs to node {}, and this is node {id}: the wrong disk is attached",
                        dir.display(),
                        file.id
                    )));
                }
                file.incarnation
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let incarnation = fresh_incarnation();
                let file = IdentityFile {
                    id,
                    incarnation: incarnation.clone(),
                };
                write_atomic(&path, &serde_json::to_vec(&file).map_err(invalid)?)?;
                incarnation
            }
            Err(e) => return Err(e),
        };
        let met = match std::fs::read(dir.join("peers.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| invalid(format!("{}/peers.json: {e}", dir.display())))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e),
        };
        Ok(Self {
            id,
            incarnation,
            dir: Some(dir),
            met: Arc::new(Mutex::new(met)),
        })
    }

    /// The incarnation this node remembers for a peer, if they have met.
    pub fn known(&self, peer: NodeId) -> Option<String> {
        self.met.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&peer).cloned()
    }

    /// Check a peer's incarnation against the one first seen for its number,
    /// remembering it if this is the first meeting.
    pub fn admit(&self, peer: NodeId, incarnation: &str) -> Result<(), Impostor> {
        let mut met = self.met.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match met.get(&peer) {
            Some(known) if known == incarnation => Ok(()),
            Some(_) => Err(Impostor { peer }),
            None => {
                met.insert(peer, incarnation.to_string());
                if let Some(dir) = &self.dir {
                    // Remembered on disk before the peer is trusted. If the
                    // write fails the peer is still admitted this once and
                    // is checked again from memory until the next restart.
                    let saved = serde_json::to_vec(&*met)
                        .map_err(invalid)
                        .and_then(|bytes| write_atomic(&dir.join("peers.json"), &bytes));
                    if let Err(error) = saved {
                        tracing::error!(peer, %error, "could not record a peer's incarnation");
                    }
                }
                Ok(())
            }
        }
    }
}
