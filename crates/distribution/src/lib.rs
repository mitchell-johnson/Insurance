//! Distribution: advisers and the businesses above them, commission, and the
//! position of every counterparty the insurer owes or is owed by.
//!
//! The module is a deterministic state machine ([`DistributionState`]). It
//! follows every partition's event stream, works out what each event earns
//! and for whom, and asks the core to post it with a reference derived from
//! the event, so a retry posts once. It builds each party's position from
//! the core's own record of what was posted, never from what it asked for.
//!
//! - [`structure`]: the dated hierarchy, engagements and licence status.
//! - [`rules`]: distribution rules, shared fragments and allocation.
//! - [`model`]: schedules, books, layers, exposure items, positions.
//! - [`state`]: the state machine, its commands and events.
//! - [`workbench`]: the held balance workbench and its controls.
//! - [`authority`]: staff roles, authority profiles and the Cedar policy.

pub mod authority;
pub mod model;
pub mod rules;
pub mod state;
pub mod structure;
pub mod workbench;

mod commission;
pub mod config;
mod statement;

use std::fmt;

use serde::{Deserialize, Serialize};

pub use state::{Actor, Command, DistributionState, Envelope, Event, Output};
pub use statement::{Statement, StatementLine};

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

id!(EngagementId, "An engagement between an adviser and a provider.");
id!(BookId, "A commission book: business that shares one payment arrangement.");
id!(FragmentId, "A shared fragment of distribution rule lines.");
