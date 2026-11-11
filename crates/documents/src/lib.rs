//! Policy documents and the correspondence lifecycle.
//!
//! Templates are configuration, versioned by content hash, approved by
//! legal and product, and tested by rendering before activation. Every
//! issued document is stored as rendered, with the data and versions it
//! came from. A correction is a reissue, never an edit. Mandatory notices
//! carry proof of delivery, and what depends on a notice waits for it.
//!
//! [`DocumentsState`] is the deterministic state machine. The drivers
//! around it ([`vault`], [`delivery`], [`service`]) render, encrypt, store
//! and send, and can be retried at any point.

pub mod channels;
pub mod command;
pub mod crypto;
pub mod driver;
pub mod event;
pub mod kinds;
pub mod model;
pub mod module;
pub mod render;
pub mod report;
pub mod service;
pub mod state;
pub mod template;
pub mod triggers;
pub mod vault;

pub use command::{Command, Envelope, Settings};
pub use event::{Event, EventRecord};
pub use kinds::{DocumentClass, DocumentKind};
pub use state::{Applied, DocumentsState, Gate, Waiting};
