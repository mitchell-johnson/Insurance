//! Who is calling.
//!
//! Production: OAuth 2.0 bearer tokens issued by Microsoft Entra ID, verified
//! against the tenant's published signing keys. A caller's app roles name the
//! modules it may act as (`new-business`, `cash-matching`, and so on).
//! Customers and partners sign in through Microsoft Entra External ID and
//! carry no module roles; what they may see is decided by audience scoping.
//!
//! Development: with no tenant configured, identity comes from headers. That
//! mode refuses to start outside the insurance in a box.

use axum::http::HeaderMap;
use ic_entra::Validator;
use ic_readmodel::scope::Relationship;

pub use ic_entra::{AuthError, ENTRA_AUTHORITY};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    /// Opaque subject: the Entra object ID, which the customer master maps to a party.
    pub subject: String,
    pub roles: Vec<String>,
    /// Relationships asserted in development mode only. In production this
    /// API serves staff and services, and people reach their policies through
    /// the portal, which holds the customer master's relationships.
    pub dev_relationships: Vec<Relationship>,
    /// When the verified token was issued (`iat`), in seconds since the
    /// epoch. `None` in development mode and for a token that does not say.
    pub issued: Option<i64>,
}

impl Caller {
    pub fn has_role(&self, role: &str) -> bool {
        self.roles.iter().any(|r| r == role)
    }
}

#[derive(Clone, Debug)]
pub enum Auth {
    /// Identity from `x-dev-subject`, `x-dev-roles` and `x-dev-relationships`.
    Dev,
    /// Tokens checked by the shared validator (`crates/entra`).
    Entra(Validator),
}

impl Auth {
    /// Validate tokens issued by `tenant` at the global Entra ID authority.
    pub fn entra(tenant: &str, audience: &str) -> Self {
        Self::entra_at(ENTRA_AUTHORITY, tenant, audience)
    }

    /// Validate tokens issued by `tenant` at another authority: a sovereign
    /// cloud's, such as `https://login.microsoftonline.us`.
    pub fn entra_at(authority: &str, tenant: &str, audience: &str) -> Self {
        Auth::Entra(Validator::new(authority, tenant, audience))
    }

    pub async fn authenticate(&self, headers: &HeaderMap) -> Result<Caller, AuthError> {
        let header = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).map(str::to_string);
        match self {
            Auth::Dev => {
                let subject = header("x-dev-subject").ok_or(AuthError::Missing)?;
                let list = |k: &str| {
                    header(k)
                        .map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>())
                        .unwrap_or_default()
                };
                let dev_relationships = list("x-dev-relationships")
                    .iter()
                    .filter_map(|r| serde_json::from_value(serde_json::Value::String(r.clone())).ok())
                    .collect();
                Ok(Caller {
                    subject,
                    roles: list("x-dev-roles"),
                    dev_relationships,
                    issued: None,
                })
            }
            Auth::Entra(validator) => {
                let claims = validator.validate(headers).await?;
                Ok(Caller {
                    subject: claims.subject,
                    roles: claims.roles,
                    dev_relationships: vec![],
                    issued: claims.issued,
                })
            }
        }
    }
}
