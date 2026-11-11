//! The portal service's configuration, read from the environment.

use std::net::SocketAddr;
use std::time::Duration;

use ic_api::auth::{Auth, ENTRA_AUTHORITY};
use ic_raft::{ClusterAuth, ClusterClient};

use crate::ops::Regions;

/// How one population of callers is identified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identity {
    /// Trust the caller's own headers. For a laptop only.
    Dev,
    Entra {
        authority: String,
        tenant: String,
        audience: String,
    },
}

impl Identity {
    pub fn auth(&self) -> Auth {
        match self {
            Identity::Dev => Auth::Dev,
            Identity::Entra { authority, tenant, audience } => Auth::entra_at(authority, tenant, audience),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PortalSettings {
    pub listen: SocketAddr,
    /// Customers, advisers and employer administrators.
    pub customers: Identity,
    /// Staff and other modules.
    pub workforce: Identity,
    pub nodes: Vec<String>,
    pub cluster_auth: ClusterAuth,
    /// How long a request keeps trying the nodes before it answers 503.
    pub patience: Duration,
    pub regions: Regions,
    /// How often the gateway tells the module the event stream is current.
    pub heartbeat: Duration,
}

impl PortalSettings {
    /// Read settings through `get`, which is `std::env::var` in production,
    /// and `read`, which reads a file.
    ///
    /// - `IC_PORTAL_LISTEN`: default `0.0.0.0:8090`.
    /// - `IC_NODES`: `host:port,host:port,...` of the ledger nodes, which
    ///   host the channel module.
    /// - `IC_CLUSTER_TOKEN_FILE`, `IC_CLUSTER_TOKEN`, `IC_CLUSTER_AUTH`: the
    ///   token the nodes require; see [`ClusterAuth::from_lookup`].
    /// - `IC_LEDGER_PATIENCE_SECS`: default 10.
    /// - `IC_ENTRA_TENANT_ID`, `IC_ENTRA_AUDIENCE`, `IC_ENTRA_AUTHORITY`:
    ///   the workforce tenant (Microsoft Entra ID).
    /// - `IC_CUSTOMER_TENANT_ID`, `IC_CUSTOMER_AUDIENCE`,
    ///   `IC_CUSTOMER_AUTHORITY`: the customer tenant (Microsoft Entra
    ///   External ID). The authority is the tenant's own, such as
    ///   `https://contoso.ciamlogin.com`.
    /// - `IC_AUTH=dev`: trust caller headers instead. Refused in Kubernetes.
    /// - `IC_REGIONS`: `auckland=host:port,host:port;sydney=...`, and
    ///   `IC_HOME_REGION`: the region that normally holds the leaders. Without
    ///   them every node is taken to be in one region.
    /// - `IC_PORTAL_HEARTBEAT_SECS`: default 15.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>, read: impl Fn(&str) -> std::io::Result<String>) -> Result<Self, String> {
        let raw = &get;
        let get = |key: &str| raw(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let listen = get("IC_PORTAL_LISTEN")
            .unwrap_or_else(|| "0.0.0.0:8090".to_string())
            .parse()
            .map_err(|e| format!("IC_PORTAL_LISTEN: {e}"))?;
        let seconds = |key: &str, default: u64| match get(key) {
            Some(v) => v
                .parse::<u64>()
                .map_err(|e| format!("{key}: {e}"))
                .and_then(|s| if s == 0 { Err(format!("{key} must be at least 1")) } else { Ok(s) }),
            None => Ok(default),
        };
        let list = get("IC_NODES").ok_or("set IC_NODES to the ledger nodes that host the channel module")?;
        let nodes = ClusterClient::from_list(&list).map_err(|e| format!("IC_NODES: {e}"))?.nodes().to_vec();
        let (customers, workforce) = match get("IC_AUTH").as_deref() {
            Some("dev") if get("KUBERNETES_SERVICE_HOST").is_some() => {
                return Err("IC_AUTH=dev is refused in Kubernetes: callers must present Entra tokens".to_string());
            }
            Some("dev") => (Identity::Dev, Identity::Dev),
            None | Some("entra") => {
                let identity = |prefix: &str, default_authority: Option<&str>| -> Result<Identity, String> {
                    let need = |name: &str| get(&format!("{prefix}_{name}")).ok_or_else(|| format!("set {prefix}_{name}, or IC_AUTH=dev on a laptop"));
                    Ok(Identity::Entra {
                        authority: match (get(&format!("{prefix}_AUTHORITY")), default_authority) {
                            (Some(authority), _) => authority,
                            (None, Some(default)) => default.to_string(),
                            (None, None) => return Err(format!("set {prefix}_AUTHORITY to the External ID tenant's authority")),
                        },
                        tenant: need("TENANT_ID")?,
                        audience: need("AUDIENCE")?,
                    })
                };
                (identity("IC_CUSTOMER", None)?, identity("IC_ENTRA", Some(ENTRA_AUTHORITY))?)
            }
            Some(other) => return Err(format!("IC_AUTH must be entra or dev, not {other:?}")),
        };
        let regions = match (get("IC_REGIONS"), get("IC_HOME_REGION")) {
            (Some(spec), Some(home)) => {
                let regions = Regions::parse(&spec, &home).map_err(|e| format!("IC_REGIONS: {e}"))?;
                if let Some(stray) = regions.nodes.values().flatten().find(|addr| !nodes.contains(addr)) {
                    return Err(format!("IC_REGIONS names {stray}, which is not one of IC_NODES"));
                }
                regions
            }
            (None, None) => Regions::single(nodes.clone()),
            _ => return Err("set IC_REGIONS and IC_HOME_REGION together".to_string()),
        };
        Ok(Self {
            listen,
            customers,
            workforce,
            cluster_auth: ClusterAuth::from_lookup(raw, read)?,
            nodes,
            patience: Duration::from_secs(seconds("IC_LEDGER_PATIENCE_SECS", 10)?),
            regions,
            heartbeat: Duration::from_secs(seconds("IC_PORTAL_HEARTBEAT_SECS", 15)?),
        })
    }

    pub fn client(&self) -> ClusterClient {
        ClusterClient::new(self.nodes.clone()).with_auth(self.cluster_auth.clone())
    }
}
