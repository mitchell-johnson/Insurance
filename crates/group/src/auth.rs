//! Who is calling the service: Microsoft Entra ID bearer tokens.
//!
//! Every caller is a service with a workload identity (the portal, the
//! staff console's backend, another module's driver). Its token is issued
//! for this service's application, and the token's app roles are the
//! caller names it may act as (`group-admin`, `employer-portal`,
//! `member-portal`, ...). There is no shared key and no development mode:
//! tests sign their own tokens and serve their own signing keys.

use axum::http::HeaderMap;

pub use ic_entra::{AuthError, ENTRA_AUTHORITY};

/// A caller whose token was accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    /// The caller names this workload may act as.
    pub roles: Vec<String>,
    /// The Entra object ID of the calling workload, for the log.
    pub subject: String,
}

impl Caller {
    pub fn may_act_as(&self, caller: &str) -> bool {
        self.roles.iter().any(|r| r == caller)
    }
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

    /// The caller behind a request's bearer token: signed by the tenant,
    /// issued for this service, not expired, and carrying at least one role.
    pub async fn authenticate(&self, headers: &HeaderMap) -> Result<Caller, AuthError> {
        let claims = self.0.validate(headers).await?;
        if claims.roles.is_empty() {
            return Err(AuthError::Invalid("the token carries no role".to_string()));
        }
        Ok(Caller {
            roles: claims.roles,
            subject: claims.subject,
        })
    }
}
