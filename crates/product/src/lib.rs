//! Insurance products as code.
//!
//! A product is a Starlark module, versioned and effective-dated, validated
//! into a strongly typed [`ProductDefinition`] at load time. Starlark plays no
//! part at runtime. Each version is identified by the content hash of its
//! source, activated through the log, and the hash is stored on every posting
//! the version produces.
//!
//! The premium calculation ([`rating::cover_premium`]) is one pure function,
//! used unchanged at quote, issue, anniversary and retrospective adjustment,
//! so a quote and a policy can never disagree.

pub mod definition;
pub mod discount;
pub mod loader;
pub mod rating;
pub mod schedule;

pub use definition::*;
pub use loader::{load_definitions, load_fund_source, load_product, load_rate_table, LoadError};
