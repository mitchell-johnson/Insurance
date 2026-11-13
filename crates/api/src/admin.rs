//! What an operator does to a cluster of ledger nodes from a terminal or a
//! job: open the book, activate the shipped configuration, look at the
//! nodes, and change a group's members. `ic-admin` is the command.
//!
//! Every action here is safe to repeat. The runbooks rely on that: a job
//! that died half way is simply run again.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::NaiveDate;
use ic_core::{Command, Envelope};
use ic_ledger::PartitionId;
use ic_raft::client::fresh_salt;
use ic_raft::{ClusterAuth, ClusterClient, NodeId, NodeStatus};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// The schemas the shipped rate tables are validated against.
const TABLE_SCHEMAS: &str = include_str!("../../../config/finance/tables.star");

/// Who the shipped tables' validations are recorded as.
pub const SHIPPED_VALIDATOR: &str = "ic-admin-shipped";

/// The shipped configuration in activation order, with each rate table's
/// validation record proposed just before the table itself, so finance's
/// close finds every active table validated. The shipped tables are the
/// synthetic basis described in `docs/products.md`; the manifest says so.
/// A shipped table that fails validation stops the whole activation.
pub fn shipped_with_validations() -> Result<Vec<Command>, String> {
    use ic_finance::tables::{load_schemas, validate, Manifest};
    let mut activations = ic_sim::config::shipped_activations().map_err(|e| e.to_string())?;
    activations.extend(ic_sim::config::shipped_fund_activations().map_err(|e| e.to_string())?);
    let schemas = load_schemas("config/finance/tables.star", TABLE_SCHEMAS).map_err(|e| e.to_string())?;
    let manifest = Manifest {
        model: "shipped-synthetic-basis".to_string(),
        model_version: "1".to_string(),
        basis_report: "docs/products.md".to_string(),
        author: "engineering".to_string(),
    };
    let mut out = Vec::with_capacity(activations.len() * 2);
    for command in activations {
        if let Command::ActivateRateTable { table } = &command {
            let schema = schemas
                .get(&table.id)
                .ok_or_else(|| format!("config/finance/tables.star declares no schema for shipped table {}", table.id))?;
            let record = validate(table, schema, &manifest, None).record(SHIPPED_VALIDATOR)?;
            out.push(Command::RecordRateTableValidation(Box::new(record)));
        }
        out.push(command);
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdminCommand {
    /// Form every partition's Raft group from the nodes' configured peers.
    Init,
    /// Every node's view of every partition.
    Status,
    /// Open the business date on every partition of an empty ledger.
    Open { date: NaiveDate },
    /// Activate the products, rate tables and definitions shipped in this
    /// build on every partition.
    ActivateShipped,
    /// Set the voters of one partition's group, or of every partition's.
    Members {
        partition: Option<PartitionId>,
        members: BTreeMap<NodeId, String>,
    },
}

pub const USAGE: &str = "usage: ic-admin init
       ic-admin status
       ic-admin open <YYYY-MM-DD>
       ic-admin activate-shipped
       ic-admin members <partition|all> <id=host:port,id=host:port,...>";

impl AdminCommand {
    /// Read a command line, without the program name.
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        match args.as_slice() {
            ["init"] => Ok(Self::Init),
            ["status"] => Ok(Self::Status),
            ["open", date] => Ok(Self::Open {
                date: NaiveDate::parse_from_str(date, "%Y-%m-%d").map_err(|e| format!("date: {e}"))?,
            }),
            ["activate-shipped"] => Ok(Self::ActivateShipped),
            ["members", partition, list] => {
                let partition = match *partition {
                    "all" => None,
                    p => Some(p.parse().map_err(|e| format!("partition: {e}"))?),
                };
                let mut members = BTreeMap::new();
                for item in list.split(',').map(str::trim).filter(|i| !i.is_empty()) {
                    let (id, addr) = item.split_once('=').ok_or_else(|| format!("members: {item:?} is not id=host:port"))?;
                    let id: NodeId = id.trim().parse().map_err(|e| format!("members: node id {id:?}: {e}"))?;
                    if addr.trim().is_empty() || members.insert(id, addr.trim().to_string()).is_some() {
                        return Err(format!("members: node {id} is given twice or has no address"));
                    }
                }
                if members.is_empty() {
                    return Err("members: a group needs at least one member".to_string());
                }
                Ok(Self::Members { partition, members })
            }
            _ => Err(USAGE.to_string()),
        }
    }
}

/// Where the nodes are and how to be let in.
#[derive(Debug, Clone)]
pub struct AdminSettings {
    pub nodes: Vec<String>,
    pub auth: ClusterAuth,
    /// How long an action keeps trying the cluster.
    pub patience: Duration,
}

impl AdminSettings {
    /// - `IC_NODES`: `host:port,host:port,...` of the ledger nodes. Required.
    /// - `IC_CLUSTER_TOKEN_FILE`, `IC_CLUSTER_TOKEN`, `IC_CLUSTER_AUTH`: the
    ///   token the nodes require; see [`ClusterAuth::from_lookup`]. Required.
    /// - `IC_LEDGER_PATIENCE_SECS`: default 60.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let auth = ClusterAuth::from_lookup(&get, |path| std::fs::read_to_string(path))?;
        let get = |key: &str| get(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let nodes = ClusterClient::from_list(&get("IC_NODES").ok_or("IC_NODES is required")?)
            .map_err(|e| format!("IC_NODES: {e}"))?
            .nodes()
            .to_vec();
        let patience = match get("IC_LEDGER_PATIENCE_SECS") {
            Some(v) => v.parse().map_err(|e| format!("IC_LEDGER_PATIENCE_SECS: {e}"))?,
            None => 60,
        };
        Ok(Self {
            nodes,
            auth,
            patience: Duration::from_secs(patience),
        })
    }
}

pub struct Admin {
    client: ClusterClient,
    patience: Duration,
}

impl Admin {
    pub fn new(settings: &AdminSettings) -> Self {
        Self {
            client: ClusterClient::new(settings.nodes.clone()).with_auth(settings.auth.clone()),
            patience: settings.patience,
        }
    }

    /// Every node's status, or why it could not be had.
    pub async fn status(&self) -> BTreeMap<String, Result<Vec<NodeStatus>, String>> {
        let mut out = BTreeMap::new();
        for addr in self.client.nodes() {
            out.insert(addr.clone(), self.client.status(addr).await);
        }
        out
    }

    /// How many partitions the ledger has, as the nodes report it.
    pub async fn partitions(&self) -> Result<PartitionId, String> {
        self.client.partitions(self.patience).await
    }

    /// Form every partition's group, by asking the first node that answers.
    /// Run once when a cluster is created; running it again changes nothing.
    pub async fn init(&self) -> Result<Value, String> {
        let deadline = tokio::time::Instant::now() + self.patience;
        let mut last = "no nodes configured".to_string();
        loop {
            for addr in self.client.nodes() {
                match self.client.init(addr).await {
                    Ok(()) => return Ok(json!({ "formed_by": addr })),
                    Err(e) => last = format!("{addr}: {e}"),
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(format!("no ledger node answered: {last}"));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    async fn propose(&self, partition: PartitionId, key: String, caller: &str, command: Command) -> Result<ic_raft::Reply, String> {
        let envelope = Envelope {
            key,
            caller: caller.to_string(),
            salt: fresh_salt(),
            traceparent: None,
            command,
        };
        self.client.propose_until(partition, &envelope, self.patience).await.map_err(|e| e.to_string())
    }

    async fn business_date(&self, partition: PartitionId) -> Result<Option<NaiveDate>, String> {
        let deadline = tokio::time::Instant::now() + self.patience;
        loop {
            match self.client.business_date(partition).await {
                Ok(date) => return Ok(date),
                Err(e) if tokio::time::Instant::now() >= deadline => return Err(e.to_string()),
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }

    /// Open the business date on every partition. A partition already on
    /// that date is left alone; one on another date is an error, because the
    /// date of a ledger in use moves only by the close.
    pub async fn open(&self, date: NaiveDate) -> Result<Value, String> {
        let mut opened = Vec::new();
        let mut already = Vec::new();
        for partition in 0..self.partitions().await? {
            match self.business_date(partition).await? {
                Some(on) if on == date => already.push(partition),
                Some(on) => {
                    return Err(format!(
                        "partition {partition} is on {on}, not {date}: a ledger in use is moved on by the close, not opened again"
                    ))
                }
                None => {
                    let reply = self
                        .propose(partition, format!("open:p{partition}"), "leader", Command::OpenBusinessDate { date })
                        .await?;
                    if let Some(refusal) = reply.rejected {
                        return Err(format!("partition {partition} refused to open: {refusal}"));
                    }
                    opened.push(partition);
                }
            }
        }
        Ok(json!({ "business_date": date, "opened": opened, "already_open": already }))
    }

    /// Activate everything shipped in this build on every partition. The
    /// key is the content, so running it again does nothing, and running a
    /// newer build activates only what changed.
    pub async fn activate_shipped(&self) -> Result<Value, String> {
        let activations = shipped_with_validations()?;
        let partitions = self.partitions().await?;
        let (mut activated, mut unchanged) = (0u64, 0u64);
        for command in &activations {
            let content = serde_json::to_vec(command).map_err(|e| e.to_string())?;
            let digest: String = Sha256::digest(&content).iter().take(16).map(|b| format!("{b:02x}")).collect();
            for partition in 0..partitions {
                let reply = self
                    .propose(partition, format!("shipped:{digest}:p{partition}"), "config", command.clone())
                    .await?;
                if let Some(refusal) = reply.rejected {
                    return Err(format!("partition {partition} refused {}: {refusal}", command.name()));
                }
                if reply.duplicate {
                    unchanged += 1;
                } else {
                    activated += 1;
                }
            }
        }
        Ok(json!({ "items": activations.len(), "partitions": partitions, "activated": activated, "unchanged": unchanged }))
    }

    /// Set the voters of a partition's group, or of every group.
    pub async fn set_members(&self, partition: Option<PartitionId>, members: &BTreeMap<NodeId, String>) -> Result<Value, String> {
        let partitions: Vec<PartitionId> = match partition {
            Some(p) => vec![p],
            None => (0..self.partitions().await?).collect(),
        };
        for p in &partitions {
            let deadline = tokio::time::Instant::now() + self.patience;
            loop {
                match self.client.set_members(*p, members).await {
                    Ok(()) => break,
                    Err(e) if tokio::time::Instant::now() >= deadline => return Err(format!("partition {p}: {e}")),
                    Err(_) => tokio::time::sleep(Duration::from_millis(250)).await,
                }
            }
        }
        Ok(json!({ "partitions": partitions, "members": members }))
    }

    /// Carry out a command and describe what happened.
    pub async fn run(&self, command: AdminCommand) -> Result<Value, String> {
        match command {
            AdminCommand::Init => self.init().await,
            AdminCommand::Status => {
                let status = self.status().await;
                if status.values().all(Result::is_err) {
                    return Err(format!("no ledger node answered: {status:?}"));
                }
                Ok(status
                    .into_iter()
                    .map(|(addr, s)| match s {
                        Ok(s) => (addr, json!(s)),
                        Err(e) => (addr, json!({ "unreachable": e })),
                    })
                    .collect::<serde_json::Map<_, _>>()
                    .into())
            }
            AdminCommand::Open { date } => self.open(date).await,
            AdminCommand::ActivateShipped => self.activate_shipped().await,
            AdminCommand::Members { partition, members } => self.set_members(partition, &members).await,
        }
    }
}
