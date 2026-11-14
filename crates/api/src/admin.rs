//! What an operator does to a cluster of ledger nodes from a terminal or a
//! job: open the book, activate the shipped configuration, look at the
//! nodes, and change a group's members. `ic-admin` is the command.
//!
//! `apply` takes the insurer's own set-up file: the configuration each
//! module is given before the first day (finance's chart, the claims rules
//! and authority book, the banks and routes of payments, the complaints
//! timers and so on), as an ordered list of steps. Each step is a command
//! for the core's configuration caller or an envelope for a hosted module,
//! sent exactly as written. The file is the insurer's decision and its
//! record; nothing in it is invented here.
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
    /// Send the steps of a set-up file, in order.
    Apply { file: std::path::PathBuf },
    /// The configuration register: every product and rate table version on
    /// each partition, and every difference between partitions.
    Register,
    /// Load, look at, sign off or abandon a migration cohort.
    Migrate(crate::migrate::MigrateCommand),
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
       ic-admin apply <setup.json>
       ic-admin members <partition|all> <id=host:port,id=host:port,...>
       ic-admin register
       ic-admin migrate load <cohort.json>
       ic-admin migrate status <cohort>
       ic-admin migrate sign-off <cohort> <finance> <business-owner> <evidence> [actuary]
       ic-admin migrate abandon <cohort>
       ic-admin migrate commission <cohort.json>";

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
            ["apply", file] => Ok(Self::Apply { file: file.into() }),
            ["register"] => Ok(Self::Register),
            ["migrate", rest @ ..] => crate::migrate::MigrateCommand::parse(rest).map(Self::Migrate).ok_or_else(|| USAGE.to_string()),
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

/// One step of a set-up file.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum SetupStep {
    /// A configuration command for the core, sent as the caller `config`
    /// to every partition under `key`.
    Core { key: String, command: Box<Command> },
    /// An envelope for a hosted module, sent as written. The module's own
    /// key inside it makes a repeat safe.
    Module {
        module: String,
        envelope: Value,
        /// Refusals that mean the step is already as wanted, by a word
        /// each contains (`already_staff`, say).
        #[serde(default)]
        accept: Vec<String>,
    },
}

/// A set-up file: the steps, in the order they are sent.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupFile {
    pub steps: Vec<SetupStep>,
}

impl SetupFile {
    /// Read a file's text. Every step is checked before any is sent: a
    /// core step must be one the configuration caller may send and carry
    /// a key, and a module step names its module and carries an envelope.
    pub fn parse(text: &str) -> Result<Self, String> {
        let file: SetupFile = serde_json::from_str(text).map_err(|e| format!("the set-up file: {e}"))?;
        for (n, step) in file.steps.iter().enumerate() {
            let bad = |why: &str| Err(format!("step {}: {why}", n + 1));
            match step {
                SetupStep::Core { key, command } => {
                    if key.trim().is_empty() || key.len() > 96 {
                        return bad("a core step has a key of 1 to 96 characters");
                    }
                    if !command.allowed_callers().contains(&"config") {
                        return bad(&format!("{} is not a configuration command", command.name()));
                    }
                }
                SetupStep::Module { module, envelope, .. } => {
                    let named = !module.is_empty() && module.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
                    if !named || !envelope.is_object() {
                        return bad("a module step names its module and carries the envelope as an object");
                    }
                }
            }
        }
        Ok(file)
    }
}

/// Why a module did not take an envelope, if it did not: the host's own
/// refusal, or the module's answer saying so.
pub(crate) fn module_refusal(result: &Result<Value, String>) -> Option<String> {
    match result {
        Err(e) => Some(e.clone()),
        Ok(answer) => [&answer["refused"], &answer["result"]["Err"], &answer["Err"]]
            .into_iter()
            .find(|v| !v.is_null())
            .map(Value::to_string),
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
    pub(crate) client: ClusterClient,
    pub(crate) patience: Duration,
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

    pub(crate) async fn propose(&self, partition: PartitionId, key: String, caller: &str, command: Command) -> Result<ic_raft::Reply, String> {
        let envelope = Envelope {
            key,
            caller: caller.to_string(),
            salt: fresh_salt(),
            traceparent: None,
            command,
        };
        self.client.propose_until(partition, &envelope, self.patience).await.map_err(|e| e.to_string())
    }

    /// A partition's record of a migration cohort, if it has one.
    pub(crate) async fn cohort_on(&self, partition: PartitionId, cohort: &str) -> Result<Option<ic_core::migration::Cohort>, String> {
        let deadline = tokio::time::Instant::now() + self.patience;
        loop {
            match self.client.cohort(partition, cohort).await {
                Ok(found) => return Ok(found),
                Err(e) if tokio::time::Instant::now() >= deadline => return Err(e.to_string()),
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
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
                    reply
                        .rejected
                        .map_or(Ok(()), |refusal| Err(format!("partition {partition} refused to open: {refusal}")))?;
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
                reply
                    .rejected
                    .map_or(Ok(()), |refusal| Err(format!("partition {partition} refused {}: {refusal}", command.name())))?;
                if reply.duplicate {
                    unchanged += 1;
                } else {
                    activated += 1;
                }
            }
        }
        Ok(json!({ "items": activations.len(), "partitions": partitions, "activated": activated, "unchanged": unchanged }))
    }

    /// Send a set-up file's steps in order, stopping at the first one
    /// refused. A core step already applied and a module refusal the step
    /// accepts count as unchanged, so the file can be applied again after a
    /// failure or with steps added.
    pub async fn apply(&self, file: &SetupFile) -> Result<Value, String> {
        let partitions = self.partitions().await?;
        let (mut applied, mut unchanged) = (0u64, 0u64);
        for (n, step) in file.steps.iter().enumerate() {
            let n = n + 1;
            match step {
                SetupStep::Core { key, command } => {
                    let mut fresh = false;
                    for partition in 0..partitions {
                        let reply = self
                            .propose(partition, format!("setup:{key}:p{partition}"), "config", (**command).clone())
                            .await?;
                        reply.rejected.map_or(Ok(()), |refusal| {
                            Err(format!("step {n}: partition {partition} refused {}: {refusal}", command.name()))
                        })?;
                        fresh |= !reply.duplicate;
                    }
                    if fresh {
                        applied += 1;
                    } else {
                        unchanged += 1;
                    }
                }
                SetupStep::Module { module, envelope, accept } => {
                    let reply = self
                        .client
                        .propose_module_until(module, envelope, self.patience)
                        .await
                        .map_err(|e| format!("step {n}: {module}: {e}"))?;
                    match module_refusal(&reply.result) {
                        None => applied += 1,
                        Some(refusal) if accept.iter().any(|word| !word.is_empty() && refusal.contains(word.as_str())) => unchanged += 1,
                        Some(refusal) => return Err(format!("step {n}: {module} refused: {refusal}")),
                    }
                }
            }
        }
        Ok(json!({ "steps": file.steps.len(), "applied": applied, "unchanged": unchanged }))
    }

    /// The register of every partition, and the drift between them: each
    /// global version that is not the same on every partition. A table
    /// scoped to schemes lives only where those schemes do, so it is listed
    /// and not counted as drift.
    pub async fn register(&self) -> Result<Value, String> {
        use ic_core::catalog::RegisterRow;
        let partitions = self.partitions().await?;
        let mut by_partition: BTreeMap<PartitionId, Vec<RegisterRow>> = BTreeMap::new();
        for partition in 0..partitions {
            let deadline = tokio::time::Instant::now() + self.patience;
            let rows = loop {
                match self.client.register(partition).await {
                    Ok(rows) => break rows,
                    Err(e) if tokio::time::Instant::now() >= deadline => return Err(format!("partition {partition}: {e}")),
                    Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
                }
            };
            by_partition.insert(partition, rows);
        }
        // Each global version, as each partition has it.
        let key = |r: &RegisterRow| {
            format!(
                "{}/{}/{}",
                serde_json::to_value(r.kind).unwrap_or_default().as_str().unwrap_or(""),
                r.id,
                r.version
            )
        };
        let mut seen: BTreeMap<String, BTreeMap<PartitionId, &RegisterRow>> = BTreeMap::new();
        for (partition, rows) in &by_partition {
            for row in rows.iter().filter(|r| r.scope.is_none()) {
                seen.entry(key(row)).or_default().insert(*partition, row);
            }
        }
        let mut drift = Vec::new();
        for (item, held) in &seen {
            let missing: Vec<PartitionId> = (0..partitions).filter(|p| !held.contains_key(p)).collect();
            // Whether a version is in use follows from the rest, so it is not compared.
            let same =
                |a: &RegisterRow, b: &RegisterRow| (&a.content_hash, a.effective_from, &a.withdrawn) == (&b.content_hash, b.effective_from, &b.withdrawn);
            let first = held.values().next();
            let differs = held.values().any(|r| first.is_some_and(|f| !same(r, f)));
            if !missing.is_empty() || differs {
                drift.push(json!({ "item": item, "missing_on": missing, "differs": differs, "as_held": held }));
            }
        }
        let first = by_partition.get(&0).cloned().unwrap_or_default();
        let scoped: Vec<_> = by_partition
            .iter()
            .flat_map(|(p, rows)| rows.iter().filter(|r| r.scope.is_some()).map(move |r| json!({ "partition": p, "row": r })))
            .collect();
        Ok(
            json!({ "partitions": partitions, "register": first.into_iter().filter(|r| r.scope.is_none()).collect::<Vec<_>>(), "scoped": scoped, "drift": drift }),
        )
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
            AdminCommand::Apply { file } => {
                let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
                self.apply(&SetupFile::parse(&text)?).await
            }
            AdminCommand::Migrate(command) => self.migrate(command).await,
            AdminCommand::Register => self.register().await,
            AdminCommand::Members { partition, members } => self.set_members(partition, &members).await,
        }
    }
}
