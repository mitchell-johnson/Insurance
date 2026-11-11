//! What the portal service runs beside its listener.

use std::sync::Arc;
use std::time::Duration;

use ic_raft::ClusterClient;

use crate::directory::Actor;
use crate::http::Channel;
use crate::query::MODULE;
use crate::staff::At;
use crate::state::{Command, Envelope, GATEWAY};

/// Seconds since the epoch, from the system clock.
pub fn wall_clock() -> At {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Whether the channel module's group has a leader that has applied
/// everything in its log. The leader is the member that feeds the module
/// the core's events, so a leader that is level is a module that is current.
/// A member cut off from the majority can go on calling itself leader for a
/// moment, so this alone proves nothing: the heartbeat that follows is a
/// command and is only committed by a majority.
pub async fn module_current(client: &ClusterClient) -> bool {
    for addr in client.nodes() {
        let Ok(statuses) = client.module_status(addr).await else { continue };
        if let Some(status) = statuses.iter().find(|s| s.module == MODULE && s.role == "leader") {
            return status.last_applied == status.last_log_index;
        }
    }
    false
}

/// One heartbeat: if the module is current, tell it so. The module uses
/// this to judge whether relationship data is fresh; when the gateway
/// cannot confirm it, sensitive rows stop being shown until it can.
pub async fn beat(channel: &dyn Channel, client: &ClusterClient, at: At) -> bool {
    if !module_current(client).await {
        return false;
    }
    let envelope = Envelope {
        key: format!("heartbeat:{at}"),
        at,
        actor: Actor::Module(GATEWAY.to_string()),
        command: Command::Heartbeat,
    };
    channel.propose(envelope).await.is_ok_and(|output| output.result.is_ok())
}

/// Send a heartbeat every `every`, for as long as the service runs.
pub async fn heartbeat(channel: Arc<dyn Channel>, client: ClusterClient, every: Duration, clock: Arc<dyn Fn() -> At + Send + Sync>) {
    let mut ticker = tokio::time::interval(every);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        if !beat(channel.as_ref(), &client, clock()).await {
            tracing::warn!("the channel module could not be confirmed as current: no heartbeat sent");
        }
    }
}
