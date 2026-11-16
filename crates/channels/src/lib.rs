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
//! - [`wordings`]: the text of each definition a cover is bound to, by version.
//! - [`feed`]: audience-scoped rows and the per-party device sync feed.

pub mod claims;
pub mod classes;
pub mod collections;
pub mod commission;
pub mod complaints;
pub mod correspondence;
pub mod countersign;
pub mod customers;
pub mod directory;
pub mod disclosures;
pub mod documents;
pub mod feed;
pub mod finance;
pub mod find;
pub mod forward;
pub mod group;
pub mod http;
pub mod identity;
pub mod mcp;
pub mod ops;
pub mod payments;
pub mod pricing;
pub mod query;
pub mod reinsurance;
pub mod replacements;
pub mod serve;
pub mod servicing;
pub mod settings;
pub mod staff;
pub mod state;
pub mod ui;
pub mod underwriting;
pub mod wordings;
pub mod workbench;

pub use directory::Actor;
pub use state::{ChannelState, Command, Effect, Envelope};
