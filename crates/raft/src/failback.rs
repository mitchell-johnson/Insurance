//! Returning leadership to the preferred region without an operator.
//!
//! Normally the voters in one region lead every group, because that is
//! where the callers are. When that region is lost the other voters take
//! over within seconds. When it comes back, its voters rejoin as followers
//! and catch up; this controller then hands leadership back to them, group
//! by group.
//!
//! Each node runs it for the groups it leads itself. A node outside the
//! preferred set that leads a group hands it to a preferred voter once that
//! voter has been level with the log for a settling period and reports the
//! same state hash. It does so only in a quiet window: not inside a frozen
//! period of the freeze calendar, not while an end-of-day run or a policy
//! transfer is in progress, and not while a module says it is busy. It moves
//! a set share of the groups at a time, then confirms that each moved group
//! still answers within its latency budget before moving more. A group
//! that does not stops the fail-back on this node and is logged as an error,
//! for an operator to look at. An operator can pause and resume it, and can
//! move any group's leader by hand, but is not needed for it to happen.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use chrono::{DateTime, NaiveTime, Utc};
use ic_ledger::PartitionId;
use openraft::async_runtime::WatchReceiver;
use openraft::storage::RaftStateMachine;
use serde::{Deserialize, Serialize};

use crate::client::ClusterClient;
use crate::network::GroupOf;
use crate::server::Host;
use crate::types::{NodeId, TypeConfig};

/// A preferred voter may be this many entries behind the leader and still
/// count as level: under load the newest entries are always in flight.
const LEVEL_WITHIN: u64 = 16;

/// How long a group may take to have a leader again after it is handed
/// over. An election that the handover started is still an election.
const HANDOVER_GRACE: Duration = Duration::from_secs(5);

/// How fail-back is configured. Read from the environment by the binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailbackSettings {
    /// The voters that should lead: those in the preferred region.
    pub preferred: BTreeSet<NodeId>,
    /// The share of this node's groups moved in one round, in percent. At
    /// least one group moves per round.
    pub share_percent: u32,
    /// How long a moved group may take to answer a read that needs a majority.
    pub latency_budget: Duration,
    /// How long a preferred voter must have been level before it is handed a group.
    pub settle: Duration,
    /// Periods of the day, in `timezone`, in which nothing is moved. A
    /// period that ends before it starts runs over midnight.
    pub frozen: Vec<(NaiveTime, NaiveTime)>,
    pub timezone: chrono_tz::Tz,
}

impl FailbackSettings {
    /// Read settings through `get`. `None` when no preferred nodes are set:
    /// leadership then stays wherever elections put it.
    ///
    /// - `IC_PREFERRED_NODES`: `1,2` — the node numbers in the preferred region.
    /// - `IC_FAILBACK_SHARE_PERCENT`: default 20.
    /// - `IC_FAILBACK_LATENCY_MS`: default 2000.
    /// - `IC_FAILBACK_SETTLE_SECONDS`: default 120.
    /// - `IC_FAILBACK_FROZEN`: `23:30-02:30,06:00-08:00`, default none.
    /// - `IC_FAILBACK_TIMEZONE`: default `Pacific/Auckland`.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Option<Self>, String> {
        let get = |key: &str| get(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let Some(list) = get("IC_PREFERRED_NODES") else {
            return Ok(None);
        };
        let mut preferred = BTreeSet::new();
        for id in list.split(',') {
            preferred.insert(id.trim().parse::<NodeId>().map_err(|e| format!("IC_PREFERRED_NODES: {e}"))?);
        }
        let number = |key: &str, default: u64| -> Result<u64, String> { get(key).map_or(Ok(default), |v| v.parse().map_err(|e| format!("{key}: {e}"))) };
        let share_percent = number("IC_FAILBACK_SHARE_PERCENT", 20)?;
        if share_percent == 0 || share_percent > 100 {
            return Err("IC_FAILBACK_SHARE_PERCENT must be between 1 and 100".to_string());
        }
        let mut frozen = Vec::new();
        for period in get("IC_FAILBACK_FROZEN").unwrap_or_default().split(',').filter(|p| !p.trim().is_empty()) {
            let time = |t: &str| NaiveTime::parse_from_str(t.trim(), "%H:%M").map_err(|e| format!("IC_FAILBACK_FROZEN: {t:?}: {e}"));
            let (from, to) = period
                .split_once('-')
                .ok_or_else(|| format!("IC_FAILBACK_FROZEN: expected HH:MM-HH:MM, got {period:?}"))?;
            frozen.push((time(from)?, time(to)?));
        }
        let timezone = get("IC_FAILBACK_TIMEZONE")
            .unwrap_or_else(|| "Pacific/Auckland".to_string())
            .parse()
            .map_err(|e| format!("IC_FAILBACK_TIMEZONE: {e}"))?;
        Ok(Some(Self {
            preferred,
            share_percent: share_percent as u32,
            latency_budget: Duration::from_millis(number("IC_FAILBACK_LATENCY_MS", 2000)?),
            settle: Duration::from_secs(number("IC_FAILBACK_SETTLE_SECONDS", 120)?),
            frozen,
            timezone,
        }))
    }

    /// Whether `now` falls in a frozen period of the calendar.
    pub fn is_frozen(&self, now: DateTime<Utc>) -> bool {
        let local = now.with_timezone(&self.timezone).time();
        self.frozen.iter().any(|(from, to)| {
            if from <= to {
                local >= *from && local < *to
            } else {
                local >= *from || local < *to
            }
        })
    }
}

/// A group this node hosts: a partition or a module.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupKey {
    Partition(PartitionId),
    Module(String),
}

impl std::fmt::Display for GroupKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GroupKey::Partition(p) => write!(f, "partition {p}"),
            GroupKey::Module(m) => write!(f, "module {m}"),
        }
    }
}

/// What fail-back is doing on one node.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FailbackStatus {
    /// Preferred nodes are configured.
    pub enabled: bool,
    pub paused: bool,
    /// Why fail-back stopped itself. Cleared by an operator's resume.
    pub halted: Option<String>,
    /// Why nothing was moved on the last pass, when there was something to move.
    pub waiting: Option<String>,
    /// Groups this node has handed back since it started, each confirmed to
    /// answer under its new leader.
    pub moved: u64,
}

#[derive(Default)]
struct Inner {
    status: FailbackStatus,
    /// Groups handed over in the last round and when, not yet confirmed.
    unconfirmed: Vec<(GroupKey, Instant)>,
    /// When each preferred voter was first seen level with each group.
    level_since: BTreeMap<(GroupKey, NodeId), Instant>,
}

/// One node's fail-back controller.
pub struct Failback {
    settings: Option<FailbackSettings>,
    inner: Mutex<Inner>,
}

/// What the leader of a group knows about its followers.
struct Lead {
    last_log_index: u64,
    matched: BTreeMap<NodeId, u64>,
    addrs: BTreeMap<NodeId, String>,
}

fn lead_of<SM: RaftStateMachine<TypeConfig>>(raft: &GroupOf<SM>) -> Option<Lead> {
    let metrics = raft.metrics().borrow_watched().clone();
    let replication = metrics.replication.filter(|_| metrics.state.is_leader())?;
    Some(Lead {
        last_log_index: metrics.last_log_index.unwrap_or(0),
        matched: replication.into_iter().map(|(id, log)| (id, log.map_or(0, |l| l.index()))).collect(),
        addrs: metrics.membership_config.nodes().map(|(id, node)| (*id, node.addr.clone())).collect(),
    })
}

impl Failback {
    pub fn new(settings: Option<FailbackSettings>) -> Self {
        let status = FailbackStatus {
            enabled: settings.is_some(),
            ..FailbackStatus::default()
        };
        Self {
            settings,
            inner: Mutex::new(Inner { status, ..Inner::default() }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn status(&self) -> FailbackStatus {
        self.lock().status.clone()
    }

    /// Stop handing groups back until resumed. Groups already moved stay moved.
    pub fn pause(&self) {
        self.lock().status.paused = true;
    }

    /// Carry on, after a pause or after fail-back stopped itself.
    pub fn resume(&self) {
        let mut inner = self.lock();
        inner.status.paused = false;
        inner.status.halted = None;
    }

    fn halt(&self, reason: String) {
        tracing::error!(%reason, "fail-back stopped on this node; an operator must look and resume it");
        let mut inner = self.lock();
        inner.status.halted = Some(reason);
        inner.unconfirmed.clear();
    }

    fn wait(&self, reason: Option<String>) -> usize {
        self.lock().status.waiting = reason;
        0
    }

    /// Check that a group handed over in the last round answers a read that
    /// needs a majority, inside the latency budget.
    /// `Ok(Err(..))` when it has no leader yet, which the handover is
    /// allowed for a short while; `Err` when it answered too slowly.
    async fn confirm(&self, key: &GroupKey, client: &ClusterClient, budget: Duration) -> Result<Result<(), String>, String> {
        let started = Instant::now();
        let answered = match key {
            GroupKey::Partition(p) => client.business_date(*p).await.map(|_| ()),
            GroupKey::Module(m) => client.query_module(m, &serde_json::Value::Null).await.map(|_| ()),
        };
        judge(key, answered.map_err(|e| e.to_string()), started.elapsed(), budget)
    }

    /// Why this node should not move anything just now, if there is a reason.
    fn busy(&self, settings: &FailbackSettings, host: &Host, now: DateTime<Utc>) -> Option<String> {
        if settings.is_frozen(now) {
            return Some("the freeze calendar forbids moves at this time of day".to_string());
        }
        for (partition, member) in &host.groups {
            let view = host.view(member);
            if !view.unfinished.is_empty() {
                return Some(format!("end-of-day is running on partition {partition}"));
            }
            if view.transfer_in_flight {
                return Some(format!("a policy transfer is in flight on partition {partition}"));
            }
        }
        // A module that is in the middle of something it must not be
        // disturbed in (a payment run being submitted) answers `false`.
        // A module that does not know the question is not asked twice.
        for (name, module) in &host.modules {
            if module.machine.query(serde_json::json!({ "failback_quiet": true })) == Ok(serde_json::Value::Bool(false)) {
                return Some(format!("module {name} is busy"));
            }
        }
        None
    }

    /// The preferred voter to hand a group to, if one has been level for
    /// long enough. Level means the leader has seen it acknowledge the log
    /// to within a few entries of the end.
    fn target(&self, settings: &FailbackSettings, key: &GroupKey, lead: &Lead) -> Option<NodeId> {
        let mut inner = self.lock();
        let mut chosen = None;
        for id in &settings.preferred {
            let level = lead.matched.get(id).is_some_and(|m| m + LEVEL_WITHIN >= lead.last_log_index && *m > 0);
            let slot = (key.clone(), *id);
            if !level {
                inner.level_since.remove(&slot);
                continue;
            }
            let since = *inner.level_since.entry(slot).or_insert_with(Instant::now);
            // Level for long enough, and at this moment holding every entry:
            // a handover is only taken up by a voter with the whole log.
            let whole = lead.matched.get(id) == Some(&lead.last_log_index);
            if chosen.is_none() && whole && since.elapsed() >= settings.settle {
                chosen = Some(*id);
            }
        }
        chosen
    }

    /// One pass. Returns how many groups were handed over.
    pub async fn pass(&self, host: &Host, client: &ClusterClient, now: DateTime<Utc>) -> usize {
        let Some(settings) = &self.settings else {
            return 0;
        };
        {
            let inner = self.lock();
            if inner.status.paused || inner.status.halted.is_some() {
                return 0;
            }
        }
        // First make sure the last round did no harm.
        let unconfirmed = std::mem::take(&mut self.lock().unconfirmed);
        let mut still = Vec::new();
        for (key, moved_at) in unconfirmed {
            let still_here = match &key {
                GroupKey::Partition(p) => host.groups[p].raft.is_leader(),
                GroupKey::Module(m) => host.modules[m].raft.is_leader(),
            };
            if still_here {
                // The handover has not happened yet, or did not take: it is
                // offered again on a later pass.
                if moved_at.elapsed() < HANDOVER_GRACE {
                    still.push((key, moved_at));
                }
                continue;
            }
            match self.confirm(&key, client, settings.latency_budget).await {
                Ok(Ok(())) => self.lock().status.moved += 1,
                Ok(Err(_)) if moved_at.elapsed() < HANDOVER_GRACE => still.push((key, moved_at)),
                Ok(Err(reason)) | Err(reason) => {
                    self.halt(reason);
                    return 0;
                }
            }
        }
        if !still.is_empty() {
            // Nothing more moves until the last round is known to be sound.
            self.lock().unconfirmed = still;
            return 0;
        }
        if settings.preferred.contains(&host.id) {
            return self.wait(None);
        }
        let mut led: Vec<(GroupKey, Lead)> = Vec::new();
        for (partition, member) in &host.groups {
            led.extend(lead_of(&member.raft).map(|lead| (GroupKey::Partition(*partition), lead)));
        }
        for (name, module) in &host.modules {
            led.extend(lead_of(&module.raft).map(|lead| (GroupKey::Module(name.clone()), lead)));
        }
        if led.is_empty() {
            return self.wait(None);
        }
        if let Some(reason) = self.busy(settings, host, now) {
            return self.wait(Some(reason));
        }
        let total = host.groups.len() + host.modules.len();
        let round = (total * settings.share_percent as usize / 100).max(1);
        let mut moved = Vec::new();
        for (key, lead) in led {
            if moved.len() >= round {
                break;
            }
            let Some(to) = self.target(settings, &key, &lead) else {
                continue;
            };
            // The voter must hold the same state where it has applied the
            // same entries: a replica that differs is never given the lead.
            match self.same_state(host, client, &key, to, &lead).await {
                Ok(true) => {}
                Ok(false) => continue,
                Err(reason) => {
                    self.halt(reason);
                    return moved.len();
                }
            }
            let handed = match &key {
                GroupKey::Partition(p) => host.groups[p].raft.trigger().transfer_leader(to).await.map_err(|e| e.to_string()),
                GroupKey::Module(m) => host.modules[m].raft.trigger().transfer_leader(to).await.map_err(|e| e.to_string()),
            };
            match handed {
                Ok(()) => {
                    tracing::info!(group = %key, to, "leadership handed back to the preferred region");
                    moved.push(key);
                }
                Err(error) => tracing::warn!(group = %key, to, %error, "leadership could not be handed back; will retry"),
            }
        }
        let mut inner = self.lock();
        inner.status.waiting = moved.is_empty().then(|| "no preferred voter has been level for long enough".to_string());
        let count = moved.len();
        inner.unconfirmed = moved.into_iter().map(|key| (key, Instant::now())).collect();
        count
    }

    /// Whether the voter reports the state this node holds. `Ok(false)`
    /// when it cannot be compared yet; an error when it differs.
    async fn same_state(&self, host: &Host, client: &ClusterClient, key: &GroupKey, to: NodeId, lead: &Lead) -> Result<bool, String> {
        let Some(addr) = lead.addrs.get(&to) else {
            return Ok(false);
        };
        let (mine, theirs) = match key {
            GroupKey::Partition(p) => {
                let mine = host.groups[p].status();
                let Ok(statuses) = client.status(addr).await else {
                    return Ok(false);
                };
                let Some(theirs) = statuses.into_iter().find(|s| s.partition == *p) else {
                    return Ok(false);
                };
                ((mine.last_applied, mine.state_hash), (theirs.last_applied, theirs.state_hash))
            }
            GroupKey::Module(m) => {
                let mine = host.modules[m].status();
                let Ok(statuses) = client.module_status(addr).await else {
                    return Ok(false);
                };
                let Some(theirs) = statuses.into_iter().find(|s| s.module == *m) else {
                    return Ok(false);
                };
                ((mine.last_applied, mine.state_hash), (theirs.last_applied, theirs.state_hash))
            }
        };
        agree(key, to, &mine, &theirs).map(|()| true)
    }
}

/// What a moved group's first answer says about the move. `Ok(Err(..))`
/// when it has not answered, which a handover is allowed for a short while;
/// `Err` when it answered too slowly.
fn judge(key: &GroupKey, answered: Result<(), String>, took: Duration, budget: Duration) -> Result<Result<(), String>, String> {
    match answered {
        Err(e) => Ok(Err(format!("{key} did not answer after its leader was moved: {e}"))),
        Ok(()) if took > budget => Err(format!(
            "{key} took {} ms to answer after its leader was moved; the budget is {} ms",
            took.as_millis(),
            budget.as_millis()
        )),
        Ok(()) => Ok(Ok(())),
    }
}

/// Two members' (last applied entry, state hash). The same entry with
/// another hash is a replica that has diverged: it is never given the lead.
fn agree<I: PartialEq>(key: &GroupKey, to: NodeId, mine: &(I, String), theirs: &(I, String)) -> Result<(), String> {
    if mine.0 == theirs.0 && mine.1 != theirs.1 {
        return Err(format!(
            "node {to} holds a different state of {key} at the same log entry: it must not lead, and the difference must be found"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Purpose: a voter that reports another state hash at the same log entry is
    /// refused the lead, for a partition and for a module, and the refusal names the
    /// node and the group; the same hash, or a hash at another entry, is no objection.
    /// Why it matters: a replica that has diverged and is handed the lead would serve
    /// and replicate wrong balances. This comparison is the last check before a
    /// handover, and a real divergence cannot be staged in a cluster test.
    #[test]
    fn a_voter_with_a_different_state_is_never_given_the_lead() {
        let hash = |h: &str| h.to_string();
        let partition = GroupKey::Partition(3);
        let refused = agree(&partition, 2, &(7u64, hash("a")), &(7, hash("b"))).unwrap_err();
        assert!(refused.contains("node 2") && refused.contains("partition 3"), "{refused}");
        let refused = agree(&GroupKey::Module("finance".to_string()), 2, &(7u64, hash("a")), &(7, hash("b"))).unwrap_err();
        assert!(refused.contains("module finance"), "{refused}");
        assert_eq!(agree(&partition, 2, &(7u64, hash("a")), &(7, hash("a"))), Ok(()));
        assert_eq!(agree(&partition, 2, &(7u64, hash("a")), &(6, hash("b"))), Ok(()));
    }

    /// Purpose: after a handover, an answer inside the budget confirms the move, no
    /// answer is reported as not yet confirmed, and an answer slower than the budget
    /// is an error that says how long it took and what the budget is.
    /// Why it matters: fail-back halts itself when a moved group is slow, so that a
    /// preferred region that cannot carry the load does not take every group. A wrong
    /// reading either halts a healthy fail-back or lets a slow one carry on.
    #[test]
    fn a_moved_group_is_judged_by_how_soon_it_answers() {
        let key = GroupKey::Partition(0);
        let (fast, slow, budget) = (Duration::from_millis(10), Duration::from_millis(900), Duration::from_millis(500));
        assert_eq!(judge(&key, Ok(()), fast, budget), Ok(Ok(())));
        let waiting = judge(&key, Err("no leader".to_string()), slow, budget).unwrap().unwrap_err();
        assert!(waiting.contains("partition 0 did not answer") && waiting.contains("no leader"), "{waiting}");
        let late = judge(&key, Ok(()), slow, budget).unwrap_err();
        assert!(late.contains("900 ms") && late.contains("500 ms"), "{late}");
    }
}
