//! Who may talk to a ledger node.
//!
//! Every route except the health probe requires the cluster token: a secret
//! shared by the nodes, the API and the workers, held in Key Vault and
//! mounted into each pod as a file. Transport encryption and peer identity
//! are the mesh's job (mutual TLS between pods); the token is the second
//! lock, so a pod that merely reaches a node's port still cannot write to
//! the ledger.
//!
//! More than one token may be accepted at a time so the secret can be
//! rotated without stopping the cluster: add the new one everywhere, switch
//! senders to it, then remove the old one.

use sha2::{Digest, Sha256};

/// Tokens shorter than this are refused: they are guessable.
pub const MIN_TOKEN_LEN: usize = 32;

#[derive(Clone, PartialEq, Eq)]
pub struct ClusterAuth {
    /// Tokens accepted from callers. Empty means callers are not checked.
    accept: Vec<String>,
    /// On a node: the access map that holds each workload to its grant
    /// (`crate::access`). `None`: every accepted token may do everything.
    access_file: Option<String>,
}

impl std::fmt::Debug for ClusterAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the tokens themselves.
        write!(f, "ClusterAuth({} tokens, access map {:?})", self.accept.len(), self.access_file)
    }
}

impl ClusterAuth {
    /// No checking at all. For tests and a cluster on one developer's machine.
    pub fn open() -> Self {
        Self {
            accept: vec![],
            access_file: None,
        }
    }

    /// Accept these tokens; send the first.
    pub fn tokens(tokens: Vec<String>) -> Result<Self, String> {
        if tokens.is_empty() {
            return Err("no cluster token given".to_string());
        }
        if let Some(short) = tokens.iter().find(|t| t.len() < MIN_TOKEN_LEN) {
            return Err(format!(
                "a cluster token of {} characters is too short; use at least {MIN_TOKEN_LEN}",
                short.len()
            ));
        }
        Ok(Self {
            accept: tokens,
            access_file: None,
        })
    }

    /// Read through `get`, which is `std::env::var` in production, and
    /// `read`, which reads a file.
    ///
    /// - `IC_CLUSTER_TOKEN_FILE`: a file of tokens, one per line. The first
    ///   is sent; all are accepted. This is the Key Vault mount.
    /// - `IC_CLUSTER_TOKEN`: the same, comma separated, for local use.
    /// - `IC_CLUSTER_AUTH=open`: no token. Refused if a token is also set.
    ///
    /// One of them is required: a node never runs unauthenticated by accident.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>, read: impl Fn(&str) -> std::io::Result<String>) -> Result<Self, String> {
        let get = |key: &str| get(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let split = |text: &str| {
            text.split([',', '\n'])
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        let tokens = match (get("IC_CLUSTER_TOKEN_FILE"), get("IC_CLUSTER_TOKEN")) {
            (Some(_), Some(_)) => return Err("set IC_CLUSTER_TOKEN_FILE or IC_CLUSTER_TOKEN, not both".to_string()),
            (Some(path), None) => Some(split(&read(&path).map_err(|e| format!("IC_CLUSTER_TOKEN_FILE {path}: {e}"))?)),
            (None, Some(inline)) => Some(split(&inline)),
            (None, None) => None,
        };
        match (tokens, get("IC_CLUSTER_AUTH").as_deref()) {
            (Some(_), Some("open")) => Err("IC_CLUSTER_AUTH=open contradicts a configured cluster token".to_string()),
            (Some(tokens), None | Some("token")) => Self::tokens(tokens),
            (None, Some("open")) => Ok(Self::open()),
            (None, None | Some("token")) => Err("set IC_CLUSTER_TOKEN_FILE (or IC_CLUSTER_TOKEN), or IC_CLUSTER_AUTH=open for a local cluster".to_string()),
            (_, Some(other)) => Err(format!("IC_CLUSTER_AUTH must be token or open, not {other:?}")),
        }
    }

    /// On a node: hold each workload to its grant in the access map at
    /// `path` (`IC_CLUSTER_ACCESS_FILE`; see `crate::access`).
    pub fn with_access_file(mut self, path: impl Into<String>) -> Self {
        self.access_file = Some(path.into());
        self
    }

    /// The access map's file, where one is set.
    pub fn access_file(&self) -> Option<&str> {
        self.access_file.as_deref()
    }

    pub fn is_open(&self) -> bool {
        self.accept.is_empty()
    }

    /// The token this process presents to others.
    pub fn outgoing(&self) -> Option<&str> {
        self.accept.first().map(String::as_str)
    }

    /// Add this process's token to an outgoing request.
    pub fn sign(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        // Every call to a node carries the caller's trace (crates/telemetry).
        let request = ic_telemetry::Traced::traced(request);
        match self.outgoing() {
            Some(token) => request.bearer_auth(token),
            None => request,
        }
    }

    /// Whether an `Authorization` header value carries an accepted token.
    pub fn admits(&self, authorization: Option<&str>) -> bool {
        if self.is_open() {
            return true;
        }
        let Some(presented) = authorization.and_then(|v| v.strip_prefix("Bearer ")) else {
            return false;
        };
        // Compare digests, and look at every accepted token every time, so
        // the time taken says nothing about how close a guess was.
        let presented = Sha256::digest(presented.as_bytes());
        let mut admitted = false;
        for token in &self.accept {
            let expected = Sha256::digest(token.as_bytes());
            let difference = presented.iter().zip(expected.iter()).fold(0u8, |acc, (a, b)| acc | (a ^ b));
            admitted |= difference == 0;
        }
        admitted
    }
}
