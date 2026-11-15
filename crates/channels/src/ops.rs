//! Region health for the operations console and the public status page.
//!
//! The ledger's voters sit in more than one region. The home regions
//! normally hold every leader between them: one region, or two that share
//! the partitions (crates/raft/src/failback.rs). When a home region is lost
//! the other regions elect leaders within seconds and nothing committed is
//! lost; reads and writes cross to the surviving regions and are slower for
//! the duration. The operations console shows that; the status page says it in
//! plain words.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ic_raft::{ClusterClient, NodeStatus};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

/// Which nodes are in which region, and which regions are home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Regions {
    /// The first home region.
    pub home: String,
    /// Every region that normally leads: `home` alone, or the regions that
    /// share the leadership. A witness's region is never one.
    pub homes: BTreeSet<String>,
    /// region -> node addresses.
    pub nodes: BTreeMap<String, Vec<String>>,
}

impl Regions {
    /// Parse `auckland=node-0:7000,node-1:7000;sydney=node-2:7000`, with the
    /// home region, or the home regions separated by commas.
    pub fn parse(spec: &str, home: &str) -> Result<Self, String> {
        let mut nodes = BTreeMap::new();
        for part in spec.split(';').map(str::trim).filter(|p| !p.is_empty()) {
            let (region, list) = part.split_once('=').ok_or_else(|| format!("{part:?} is not region=host:port,..."))?;
            let addrs: Vec<String> = list.split(',').map(str::trim).filter(|a| !a.is_empty()).map(str::to_string).collect();
            if region.trim().is_empty() || addrs.is_empty() {
                return Err(format!("{part:?} names no region or no nodes"));
            }
            nodes.insert(region.trim().to_string(), addrs);
        }
        let homes: Vec<String> = home.split(',').map(str::trim).filter(|h| !h.is_empty()).map(str::to_string).collect();
        if homes.is_empty() {
            return Err(format!("the home region {home:?} is not one of the regions"));
        }
        if let Some(missing) = homes.iter().find(|h| !nodes.contains_key(*h)) {
            return Err(format!("the home region {missing:?} is not one of the regions"));
        }
        Ok(Self {
            home: homes[0].clone(),
            homes: homes.into_iter().collect(),
            nodes,
        })
    }

    /// One region holding every node: a cluster that is not spread out.
    pub fn single(nodes: Vec<String>) -> Self {
        Self {
            home: "local".to_string(),
            homes: ["local".to_string()].into(),
            nodes: [("local".to_string(), nodes)].into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegionHealth {
    pub name: String,
    pub home: bool,
    pub nodes: usize,
    pub reachable: usize,
    /// Partitions led from this region.
    pub leaders: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegionReport {
    pub regions: Vec<RegionHealth>,
    pub partitions: usize,
    pub partitions_with_leader: usize,
    /// A majority of the voters answered.
    pub quorum: bool,
    /// Every home region answers, and between them they lead every partition.
    pub at_home: bool,
}

/// What each node said, or `None` for a node that did not answer.
pub fn assess(regions: &Regions, observed: &BTreeMap<String, Option<Vec<NodeStatus>>>) -> RegionReport {
    let mut partitions = BTreeSet::new();
    let mut led = BTreeSet::new();
    let (mut total, mut reachable_total) = (0, 0);
    let mut out = Vec::new();
    for (name, addrs) in &regions.nodes {
        let mut health = RegionHealth {
            name: name.clone(),
            home: regions.homes.contains(name),
            nodes: addrs.len(),
            reachable: 0,
            leaders: 0,
        };
        for addr in addrs {
            total += 1;
            let Some(Some(statuses)) = observed.get(addr) else { continue };
            health.reachable += 1;
            reachable_total += 1;
            for status in statuses {
                partitions.insert(status.partition);
                if status.role == "leader" && led.insert(status.partition) {
                    health.leaders += 1;
                }
            }
        }
        out.push(health);
    }
    let homes: Vec<&RegionHealth> = out.iter().filter(|r| r.home).collect();
    let led_at_home: usize = homes.iter().map(|h| h.leaders).sum();
    RegionReport {
        partitions: partitions.len(),
        partitions_with_leader: led.len(),
        quorum: reachable_total * 2 > total,
        at_home: homes.iter().all(|h| h.reachable == h.nodes) && led_at_home == partitions.len() && !partitions.is_empty(),
        regions: out,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Operational,
    Degraded,
    Outage,
}

/// What the public is told.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusPage {
    pub level: Level,
    pub headline: String,
    pub detail: Vec<String>,
}

impl From<&RegionReport> for StatusPage {
    fn from(report: &RegionReport) -> Self {
        let serving = report.quorum && report.partitions > 0 && report.partitions_with_leader == report.partitions;
        let home_down = report.regions.iter().any(|r| r.home && r.reachable == 0);
        if !serving {
            return Self {
                level: Level::Outage,
                headline: "Some of our services are unavailable".to_string(),
                detail: vec![
                    "You may not be able to sign in or see your policies right now.".to_string(),
                    "Your cover is not affected, and nothing you have already sent us has been lost.".to_string(),
                    "If you need to make a claim or an urgent change, call us.".to_string(),
                ],
            };
        }
        if home_down || !report.at_home {
            let mut detail = vec![
                "Everything is working, but more slowly than usual.".to_string(),
                "One of our data centres is unavailable and we are running from the others.".to_string(),
                "Nothing has been lost. The app may take longer to load and to update, and will catch up by itself.".to_string(),
            ];
            if !home_down {
                detail[1] = "We are running from our backup data centres and will move back by ourselves when it is safe to.".to_string();
            }
            return Self {
                level: Level::Degraded,
                headline: "Slower than usual".to_string(),
                detail,
            };
        }
        Self {
            level: Level::Operational,
            headline: "All services are working normally".to_string(),
            detail: vec![],
        }
    }
}

type Report<'a> = Pin<Box<dyn Future<Output = RegionReport> + Send + 'a>>;

/// Where a report comes from.
pub trait RegionSource: Send + Sync + 'static {
    fn report(&self) -> Report<'_>;
}

/// A fixed report: for tests and for a service with no cluster.
pub struct Fixed(pub std::sync::Mutex<RegionReport>);

impl RegionSource for Fixed {
    fn report(&self) -> Report<'_> {
        let report = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        Box::pin(async move { report })
    }
}

/// Ask every node for its status. The answer is kept for a few seconds, so
/// the public status page cannot be used to load the ledger nodes.
pub struct Live {
    pub client: ClusterClient,
    pub regions: Regions,
    pub keep: Duration,
    cache: Mutex<Option<(Instant, RegionReport)>>,
}

impl Live {
    pub fn new(client: ClusterClient, regions: Regions) -> Arc<Self> {
        Arc::new(Self {
            client,
            regions,
            keep: Duration::from_secs(5),
            cache: Mutex::new(None),
        })
    }
}

impl RegionSource for Live {
    fn report(&self) -> Report<'_> {
        Box::pin(async move {
            let mut cache = self.cache.lock().await;
            if let Some((_, report)) = cache.as_ref().filter(|(at, _)| at.elapsed() < self.keep) {
                return report.clone();
            }
            let addrs: Vec<&String> = self.regions.nodes.values().flatten().collect();
            let answers = futures::future::join_all(addrs.iter().map(|addr| self.client.status(addr))).await;
            let observed = addrs.into_iter().cloned().zip(answers.into_iter().map(Result::ok)).collect();
            let report = assess(&self.regions, &observed);
            *cache = Some((Instant::now(), report.clone()));
            report
        })
    }
}
