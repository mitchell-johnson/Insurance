//! The portal service's configuration, read from the environment.

use std::net::SocketAddr;
use std::time::Duration;

use ic_api::auth::{Auth, ENTRA_AUTHORITY};
use ic_raft::{ClusterAuth, ClusterClient};

use crate::ops::Regions;
use crate::ui::{origin, SignIn, UiConfig};

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
    /// How the web pages sign people in.
    pub ui: UiConfig,
    /// How often the commission module's changes are carried to the channels.
    pub forward: Duration,
    /// The currency of the commission module's balances.
    pub currency: String,
    /// How many partitions the ledger has: the claims console asks each.
    pub partitions: ic_ledger::PartitionId,
    /// The customer master's base URL and token scope, for self-service.
    pub master: Option<(String, String)>,
    /// The documents module's base URL and token scope, for downloads.
    pub documents: Option<(String, String)>,
    /// The underwriting intake's base URL and token scope, for signing disclosures.
    pub intake: Option<(String, String)>,
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
    /// - `IC_ENTRA_CLIENT_ID`, `IC_CUSTOMER_CLIENT_ID`: the single-page
    ///   application registered in each tenant, which the web pages sign in
    ///   through. `IC_ENTRA_SCOPE`, `IC_CUSTOMER_SCOPE`: the scope they ask
    ///   for; default `<audience>/.default`.
    /// - `IC_AUTH=dev`: trust caller headers instead. Refused in Kubernetes.
    /// - `IC_REGIONS`: `auckland=host:port,host:port;sydney=...`, and
    ///   `IC_HOME_REGION`: the region that normally holds the leaders. Without
    ///   them every node is taken to be in one region.
    /// - `IC_PORTAL_HEARTBEAT_SECS`: default 15.
    /// - `IC_PORTAL_FORWARD_SECS`: how often the commission module's changes
    ///   (engagements, licences, held money) are carried to the channels;
    ///   default 15. `IC_PORTAL_CURRENCY`: the currency of those balances;
    ///   default `NZD`.
    /// - `IC_DOCUMENTS_URL` and `IC_DOCUMENTS_SCOPE`: the documents service
    ///   and the scope of its app registration, for PDF downloads.
    /// - `IC_UNDERWRITING_INTAKE_URL` and `IC_UNDERWRITING_INTAKE_SCOPE`: the
    ///   underwriting intake and the scope of its audience, for disclosures.
    /// - `IC_CUSTOMERS_URL` and `IC_CUSTOMERS_SCOPE`: the customer master and
    ///   the scope of its token (`api://<its application>/.default`), set
    ///   together; without them a person's own record answers 503.
    /// - `IC_PARTITIONS`: how many partitions the ledger has, as the core
    ///   gateway is told; default 64.
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
        let (customers, workforce, ui) = match get("IC_AUTH").as_deref() {
            Some("dev") if get("KUBERNETES_SERVICE_HOST").is_some() => {
                return Err("IC_AUTH=dev is refused in Kubernetes: callers must present Entra tokens".to_string());
            }
            Some("dev") => (Identity::Dev, Identity::Dev, UiConfig::Dev),
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
                let sign_in = |prefix: &str, identity: &Identity| -> Result<SignIn, String> {
                    let Identity::Entra { authority, tenant, audience } = identity else {
                        return Err(format!("{prefix}: not an Entra identity"));
                    };
                    if origin(authority).is_none() {
                        return Err(format!(
                            "{prefix}_AUTHORITY must be a web address such as https://login.microsoftonline.com, not {authority:?}"
                        ));
                    }
                    Ok(SignIn {
                        authority: authority.trim_end_matches('/').to_string(),
                        tenant: tenant.clone(),
                        client_id: get(&format!("{prefix}_CLIENT_ID"))
                            .ok_or_else(|| format!("set {prefix}_CLIENT_ID to the single-page application the web pages sign in through"))?,
                        scope: get(&format!("{prefix}_SCOPE")).unwrap_or_else(|| format!("{audience}/.default")),
                    })
                };
                let (customers, workforce) = (identity("IC_CUSTOMER", None)?, identity("IC_ENTRA", Some(ENTRA_AUTHORITY))?);
                let ui = UiConfig::Entra {
                    customers: sign_in("IC_CUSTOMER", &customers)?,
                    workforce: sign_in("IC_ENTRA", &workforce)?,
                };
                (customers, workforce, ui)
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
            ui,
            forward: Duration::from_secs(seconds("IC_PORTAL_FORWARD_SECS", 15)?),
            master: service(get("IC_CUSTOMERS_URL"), get("IC_CUSTOMERS_SCOPE"), "IC_CUSTOMERS")?,
            documents: service(get("IC_DOCUMENTS_URL"), get("IC_DOCUMENTS_SCOPE"), "IC_DOCUMENTS")?,
            intake: service(get("IC_UNDERWRITING_INTAKE_URL"), get("IC_UNDERWRITING_INTAKE_SCOPE"), "IC_UNDERWRITING_INTAKE")?,
            partitions: match get("IC_PARTITIONS") {
                None => ic_ledger::PARTITION_COUNT,
                Some(p) => match p.parse::<ic_ledger::PartitionId>() {
                    Ok(n) if (1..=ic_ledger::PARTITION_COUNT).contains(&n) => n,
                    _ => return Err(format!("IC_PARTITIONS must be between 1 and {}, not {p:?}", ic_ledger::PARTITION_COUNT)),
                },
            },
            currency: match get("IC_PORTAL_CURRENCY") {
                Some(c) if c.len() == 3 && c.chars().all(|ch| ch.is_ascii_uppercase()) => c,
                Some(c) => return Err(format!("IC_PORTAL_CURRENCY must be a three-letter code such as NZD, not {c:?}")),
                None => "NZD".to_string(),
            },
        })
    }

    pub fn client(&self) -> ClusterClient {
        ClusterClient::new(self.nodes.clone()).with_auth(self.cluster_auth.clone())
    }
}

/// Another service's base URL and token scope: both or neither, and https
/// unless it runs on this machine.
fn service(url: Option<String>, scope: Option<String>, name: &str) -> Result<Option<(String, String)>, String> {
    match (url, scope) {
        (Some(url), Some(scope)) if url.starts_with("https://") || local(&url) => Ok(Some((url, scope))),
        (Some(url), Some(_)) => Err(format!("{name}_URL must be https, not {url:?}")),
        (None, None) => Ok(None),
        _ => Err(format!("set {name}_URL and {name}_SCOPE together")),
    }
}

/// Plain HTTP only to this machine or to a service inside the cluster,
/// named by its single-label service name or its `.svc` name, where the
/// network policy admits only the portal and traffic does not leave the
/// cluster's network.
fn local(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else { return false };
    let host = rest.split(['/', ':']).next().unwrap_or_default();
    !host.is_empty() && (host == "localhost" || !host.contains('.') || host.ends_with(".svc") || host.ends_with(".svc.cluster.local"))
}
