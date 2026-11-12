//! The web pages, compiled into the service, and what a browser needs to
//! run them: where to sign in, and a content security policy that allows
//! exactly the scripts shipped here.
//!
//! The pages load nothing from elsewhere. Each is one HTML file with one
//! inline script, plus the shared `/ui/app.js` and `/ui/app.css`.

use base64::Engine;
use serde::Serialize;
use sha2::{Digest, Sha256};

pub const INDEX: &str = include_str!("../ui/index.html");
pub const ADVISER: &str = include_str!("../ui/adviser.html");
pub const EMPLOYER: &str = include_str!("../ui/employer.html");
pub const STAFF: &str = include_str!("../ui/staff.html");
pub const CLAIMS: &str = include_str!("../ui/claims.html");
pub const COMPLAINTS: &str = include_str!("../ui/complaints.html");
pub const FINANCE: &str = include_str!("../ui/finance.html");
pub const OPS: &str = include_str!("../ui/ops.html");
pub const PRICING: &str = include_str!("../ui/pricing.html");
pub const STATUS: &str = include_str!("../ui/status.html");
pub const SCRIPT: &str = include_str!("../ui/app.js");
pub const STYLE: &str = include_str!("../ui/app.css");

/// Where one population signs in from a browser: the single-page
/// application registered in its tenant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SignIn {
    pub authority: String,
    pub tenant: String,
    pub client_id: String,
    /// The scope the page asks for, which makes the token's audience this service.
    pub scope: String,
}

/// What the pages are told at `/ui/config.json`. Nothing in it is secret:
/// a public client has no secret, and sign-in uses PKCE.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum UiConfig {
    /// The person types who they are. For a laptop only.
    Dev,
    Entra {
        customers: SignIn,
        workforce: SignIn,
    },
}

/// The scheme and host of an authority, if it is a plain web origin.
/// Anything else is refused, because the origin is written into a header.
pub fn origin(authority: &str) -> Option<String> {
    let (scheme, rest) = authority.split_once("://")?;
    let host = rest.split('/').next()?;
    let plain = !host.is_empty() && host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'));
    (matches!(scheme, "https" | "http") && plain).then(|| format!("{scheme}://{host}"))
}

/// The hashes of a page's inline scripts, as a policy lists them.
fn script_hashes(page: &str) -> Vec<String> {
    let mut hashes = Vec::new();
    let mut rest = page;
    while let Some(open) = rest.find("<script>") {
        let body = &rest[open + "<script>".len()..];
        let Some(close) = body.find("</script>") else { break };
        let digest = Sha256::digest(&body.as_bytes()[..close]);
        hashes.push(format!("'sha256-{}'", base64::engine::general_purpose::STANDARD.encode(digest)));
        rest = &body[close..];
    }
    hashes
}

impl UiConfig {
    /// The content security policy for one page: this service's own files,
    /// the page's own inline scripts by hash, calls to this service and to
    /// the sign-in authorities, and no framing.
    pub fn policy(&self, page: &str) -> String {
        let mut scripts = vec!["'self'".to_string()];
        scripts.extend(script_hashes(page));
        let mut connect = vec!["'self'".to_string()];
        if let UiConfig::Entra { customers, workforce } = self {
            for authority in [&customers.authority, &workforce.authority] {
                if let Some(origin) = origin(authority).filter(|o| !connect.contains(o)) {
                    connect.push(origin);
                }
            }
        }
        format!(
            "default-src 'none'; script-src {}; style-src 'self'; connect-src {}; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
            scripts.join(" "),
            connect.join(" ")
        )
    }
}
