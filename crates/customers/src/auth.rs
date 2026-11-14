//! Who is calling: Microsoft Entra ID bearer tokens.
//!
//! Every caller is a service with a workload identity. Its token's app
//! roles name what it may act as (`group-admin`, `documents`, `claims`,
//! `portal`, `staff`, ...). A token with several roles names the one it
//! acts as in `x-ic-caller`. There is no development mode: tests sign their
//! own tokens and serve their own signing keys.

use axum::http::HeaderMap;

pub use ic_entra::{AuthError, ENTRA_AUTHORITY};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    /// The calling service's role, which the access policy decides on.
    pub role: String,
    /// The Entra object ID of the calling workload.
    pub subject: String,
}

/// Validates tokens issued by one tenant for this service, with the shared
/// validator (`crates/entra`).
#[derive(Clone, Debug)]
pub struct Entra(ic_entra::Validator);

impl Entra {
    /// Validate tokens for `audience` issued by `tenant` at `authority`.
    pub fn new(authority: &str, tenant: &str, audience: &str) -> Self {
        Self(ic_entra::Validator::new(authority, tenant, audience))
    }

    /// The calling service and the one role it acts as (`x-ic-caller`
    /// where its token carries several).
    pub async fn authenticate(&self, headers: &HeaderMap) -> Result<Caller, AuthError> {
        let claims = self.0.validate(headers).await?;
        Ok(Caller {
            role: claims.acting_as(headers)?,
            subject: claims.subject,
        })
    }
}
