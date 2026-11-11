//! Identifiers used by the reinsurance module. Like the core's, they are
//! opaque: a reinsurer or a life is known here only by its ID.

use std::fmt;

use ic_core::{LifeId, PartyId, PolicyId};
use serde::{Deserialize, Serialize};

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

id!(ReinsurerId, "A reinsurer: a party in the customer master.");
id!(TreatyId, "A treaty, across all its versions.");
id!(ClaimId, "A claim in the claims module.");

/// One version of a treaty. In-force cover stays on the version it was
/// written under.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TreatyRef {
    pub id: TreatyId,
    pub version: u32,
}

impl fmt::Display for TreatyRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.id, self.version)
    }
}

/// What a cession attaches to: one layer of a cover, or a scheme member's
/// cover (layer 0 with the member's life).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RiskKey {
    pub policy: PolicyId,
    pub cover: String,
    pub layer: u32,
    pub life: LifeId,
}

impl fmt::Display for RiskKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}/{}/{}", self.policy, self.cover, self.layer, self.life)
    }
}

/// Whose retention a risk counts against. A life linked to a party
/// accumulates across every policy and partition. A scheme member who has not
/// been linked to a party accumulates only against their own member record.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifeKey {
    Party(PartyId),
    Unmatched { policy: PolicyId, life: LifeId },
}

impl fmt::Display for LifeKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LifeKey::Party(p) => write!(f, "party:{p}"),
            LifeKey::Unmatched { policy, life } => write!(f, "member:{policy}/{life}"),
        }
    }
}

/// JSON object keys are strings, so a map keyed by a structure is written as
/// a list of entries. The order is the map's, so the encoding is canonical.
pub(crate) mod pairs {
    use std::collections::BTreeMap;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<K: Serialize, V: Serialize, S: Serializer>(map: &BTreeMap<K, V>, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(map.iter())
    }

    pub fn deserialize<'de, K: Deserialize<'de> + Ord, V: Deserialize<'de>, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<K, V>, D::Error> {
        Ok(Vec::<(K, V)>::deserialize(d)?.into_iter().collect())
    }
}
