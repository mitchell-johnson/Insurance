//! The customer master: who the insurer's parties are.
//!
//! The core knows people only by opaque ID. This service owns everything
//! else about them: identity, contact details, tax details, consents, due
//! diligence and relationships (owner, life insured, beneficiary, adviser,
//! employer administrator and the rest), on two timelines, with an
//! append-only, hash-chained event stream per party, field-level
//! encryption under a per-party key wrapped in Azure Key Vault,
//! crypto-shredding driven by a retention schedule, purpose-based access
//! with every read logged, and matching and merging as events.
//!
//! See `docs/customers.md`.

#![forbid(unsafe_code)]

pub mod access;
pub mod auth;
pub mod config;
pub mod crypto;
pub mod db;
pub mod event;
pub mod keys;
pub mod matching;
pub mod model;
pub mod party;
pub mod relay;
pub mod retention;
pub mod service;
pub mod store;
pub mod validate;
pub mod work;
