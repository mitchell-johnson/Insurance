//! Opaque identifiers. The core knows people only by ID; the customer master
//! owns everything else about them.

use std::fmt;

use ic_ledger::{PartitionId, PARTITION_COUNT};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

macro_rules! id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_string())
            }
        }
    };
}

id!(PolicyId, "Opaque policy ID.");
id!(GroupId, "The policyholder's group: the partition key.");
id!(PartyId, "A party in the customer master.");
id!(LifeId, "A life record under a policy.");

/// The partition a group lives on. Frozen forever: changing this function is
/// a new ledger and a migration. Groups that have been moved by application
/// level migration are looked up in the policy directory instead.
pub fn home_partition(group: &GroupId) -> PartitionId {
    let d = Sha256::digest(group.0.as_bytes());
    u16::from_be_bytes([d[0], d[1]]) % PARTITION_COUNT
}
