//! The channels: the customer app, the adviser portal, the employer portal,
//! the staff console and the operations console.
//!
//! No business rule lives here. A channel shows a person the slice of the
//! platform their relationships allow, and turns what they ask for into a
//! typed request that goes through the same authorisation as everything
//! else. Reads come from read models followed from the core's event stream;
//! writes are asynchronous by contract and shown as pending until an event
//! resolves them.
//!
//! - [`state`]: the module's replicated state machine.
//! - [`directory`]: who is who, and each party's relationship to each policy,
//!   including advisers' engagements and the licence cascade.
//! - [`identity`]: enrolled devices, proof of possession and transaction signing.
//! - [`staff`]: role templates, authority profiles, grants and the four
//!   questions asked of every staff action.
//! - [`classes`]: data classes and masking.
//! - [`feed`]: audience-scoped rows and the per-party device sync feed.

pub mod claims;
pub mod classes;
pub mod complaints;
pub mod countersign;
pub mod directory;
pub mod feed;
pub mod finance;
pub mod forward;
pub mod http;
pub mod identity;
pub mod ops;
pub mod pricing;
pub mod query;
pub mod reinsurance;
pub mod serve;
pub mod settings;
pub mod staff;
pub mod state;
pub mod ui;
pub mod workbench;

pub use directory::Actor;
pub use state::{ChannelState, Command, Effect, Envelope};
