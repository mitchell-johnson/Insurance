//! Background work: publishing the outbox and the retention sweep.
//!
//! Both are safe to run on every replica at once. Publishing is at least
//! once (consumers de-duplicate on the event ID), and erasure takes the
//! party's row lock and checks again before it destroys anything.

use std::future::Future;
use std::time::Duration;

use chrono::{NaiveDate, Utc};
use ic_azure::blob::WormStore;
use ic_azure::eventhubs::EventHubsSink;
use ic_azure::keywrap::KeyWrapper;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::event::PublicEvent;
use crate::model::PartyId;
use crate::retention::erasable_from;
use crate::service::App;

/// Where public events go: one hub for everything, one for the changes
/// that take access away, which gateways read first.
pub struct Hubs {
    pub events: EventHubsSink,
    pub priority: EventHubsSink,
    /// Partitions of each hub. A party's events always go to the same one.
    pub partitions: u16,
}

/// The hub partition a party's events go to.
pub fn partition_of(party: &PartyId, partitions: u16) -> u16 {
    let d = Sha256::digest(party.0.as_bytes());
    u16::from_be_bytes([d[0], d[1]]) % partitions.max(1)
}

/// Publish what is waiting, a batch at a time, until nothing is. Returns
/// the number published.
pub async fn publish<W: KeyWrapper, S: WormStore>(app: &App<W, S>, hubs: &Hubs) -> Result<usize, String> {
    let mut total = 0;
    loop {
        let batch = app.store.outbox(500).await.map_err(|e| e.to_string())?;
        if batch.is_empty() {
            return Ok(total);
        }
        let mut groups: std::collections::BTreeMap<(bool, u16), Vec<(i64, PublicEvent)>> = Default::default();
        for (id, e) in batch {
            groups.entry((e.priority, partition_of(&e.party, hubs.partitions))).or_default().push((id, e));
        }
        for ((priority, partition), events) in groups {
            let messages: Vec<_> = events
                .iter()
                .map(|(_, e)| {
                    let body = serde_json::to_value(e).unwrap_or_default();
                    (body, json!({ "event-id": e.id, "party": e.party, "seq": e.seq, "type": e.kind }))
                })
                .collect();
            let hub = if priority { &hubs.priority } else { &hubs.events };
            hub.send_json(partition, &messages).await.map_err(|e| format!("Event Hubs: {e}"))?;
            let ids: Vec<i64> = events.iter().map(|(id, _)| *id).collect();
            app.store.published(&ids, Utc::now()).await.map_err(|e| e.to_string())?;
            total += ids.len();
        }
    }
}

/// How long a party must have been unchanged before the sweep looks at it.
pub const QUIET_DAYS: i64 = 30;

/// Erase every party whose retention has run out. Each batch is written to
/// the register before anything is destroyed. Returns the number erased.
pub async fn sweep<W: KeyWrapper, S: WormStore>(app: &App<W, S>, batch: i64) -> Result<usize, String> {
    let now = Utc::now();
    let today = now.date_naive();
    let candidates = app
        .store
        .sweep_candidates(today, now - chrono::Duration::days(QUIET_DAYS), batch)
        .await
        .map_err(|e| e.to_string())?;
    let due_on = |p: &crate::party::Party| p.last_recorded.and_then(|at| erasable_from(p, at, today)).is_some_and(|d| d <= today);
    let mut due = Vec::new();
    // The register lists everyone erased, those merged in included.
    let mut listed = Vec::new();
    for id in candidates {
        if let Some(p) = app.store.load(&id).await.map_err(|e| e.to_string())? {
            if due_on(&p) {
                listed.push(id.clone());
                listed.extend(p.absorbed.iter().cloned());
                due.push(id);
            }
        }
    }
    if due.is_empty() {
        return Ok(0);
    }
    app.register.record(&listed, now).await?;
    let mut erased = 0;
    for id in &due {
        if app.store.shred(id, due_on, Utc::now()).await.map_err(|e| e.to_string())? {
            tracing::info!(party = %id, "party erased under the retention schedule");
            erased += 1;
        }
    }
    Ok(erased)
}

/// After a database restore: erase again everything the register lists
/// since `since`. Returns the number listed and the number erased again.
pub async fn reshred<W: KeyWrapper, S: WormStore>(app: &App<W, S>, since: NaiveDate) -> Result<(usize, usize), String> {
    let listed = app.register.since(since, Utc::now().date_naive()).await?;
    let erased = app.store.reshred(&listed, Utc::now()).await.map_err(|e| e.to_string())?;
    Ok((listed.len(), erased))
}

/// Tell claims of every death waiting, a batch at a time. A death not
/// accepted on every partition stays, with the reason, for the next pass.
/// Returns the number told.
pub async fn relay_deaths<W: KeyWrapper, S: WormStore, C: crate::relay::Core>(app: &App<W, S>, core: &C) -> Result<usize, String> {
    use crate::relay::Failure;
    let waiting = app.store.deaths_to_relay(100).await.map_err(|e| e.to_string())?;
    if waiting.is_empty() {
        return Ok(0);
    }
    let partitions = core.partitions().await.map_err(|e| format!("the core's partitions: {e}"))?;
    let mut told = 0;
    for notice in waiting {
        let party = &notice.party;
        let failure = match crate::relay::send(core, partitions, &notice).await {
            Ok(()) => {
                app.store.death_relayed(&notice).await.map_err(|e| e.to_string())?;
                tracing::info!(party = %party, retract = notice.retract, "death told to claims");
                told += 1;
                continue;
            }
            Err(f) => f,
        };
        let (Failure::Refused(e) | Failure::Unreached(e)) = &failure;
        tracing::warn!(party = %party, error = %e, "claims not yet told of a death; will retry");
        app.store.death_failed(&notice, e).await.map_err(|e| e.to_string())?;
        if matches!(failure, Failure::Unreached(_)) {
            break;
        }
    }
    Ok(told)
}

/// Run the outbox every `every`, and the sweep every `sweep_every`, until
/// `stop` completes. A failed pass is logged and tried again next time.
pub async fn run<W: KeyWrapper, S: WormStore, C: crate::relay::Core>(
    app: &App<W, S>,
    hubs: &Hubs,
    core: Option<&C>,
    every: Duration,
    sweep_every: Duration,
    stop: impl Future<Output = ()>,
) {
    tokio::pin!(stop);
    let mut last_sweep: Option<std::time::Instant> = None;
    loop {
        if let Err(e) = publish(app, hubs).await {
            tracing::warn!(error = %e, "publishing the outbox failed; will retry");
        }
        if let Some(core) = core {
            if let Err(e) = relay_deaths(app, core).await {
                tracing::warn!(error = %e, "relaying deaths to claims failed; will retry");
            }
        }
        if last_sweep.is_none_or(|t| t.elapsed() >= sweep_every) {
            last_sweep = Some(std::time::Instant::now());
            match app.store.prompt_marker_reviews(Utc::now().date_naive(), Utc::now()).await {
                Ok(n) if n > 0 => tracing::info!(prompts = n, "vulnerability markers due for review"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "opening marker reviews failed; will retry"),
            }
            match sweep(app, 500).await {
                Ok(n) if n > 0 => tracing::info!(erased = n, "retention sweep"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "the retention sweep failed; will retry"),
            }
        }
        tokio::select! {
            _ = &mut stop => return,
            _ = tokio::time::sleep(every) => {}
        }
    }
}

/// Start the service from its settings and run it until `stop` turns true:
/// the HTTP API and the background work, side by side.
pub async fn start(
    config: crate::config::Config,
    credential: ic_azure::credential::AzureCredential,
    stop: tokio::sync::watch::Receiver<bool>,
) -> Result<(), String> {
    use crate::config::Database;
    use crate::db::Connector;
    let connector = match &config.database {
        Database::Azure { host, database, user } => Connector::azure(host, database, user, credential.clone()),
        Database::Local(url) => Connector::local(url)?,
    };
    let pool = crate::db::open(connector, config.pool_size).await?;
    let keys = crate::keys::KeyRing::new(ic_azure::keywrap::KeyVaultWrapper::new(&config.key_vault, &config.kek, credential.clone()));
    let index = crate::crypto::IndexKey::new(config.index_key.as_bytes())?;
    let app = std::sync::Arc::new(App {
        store: crate::store::Store::new(pool, keys, index),
        access: crate::access::Access::shipped(),
        auth: crate::auth::Entra::new(&config.authority, &config.tenant, &config.audience),
        register: crate::retention::Register::new(ic_azure::blob::BlobContainer::new(
            &config.register_endpoint,
            &config.register_container,
            credential.clone(),
        )),
    });
    let hubs = Hubs {
        events: EventHubsSink::new(&config.event_hubs_namespace, &config.event_hub, credential.clone()),
        priority: EventHubsSink::new(&config.event_hubs_namespace, &config.priority_hub, credential),
        partitions: config.hub_partitions,
    };
    let listener = tokio::net::TcpListener::bind(&config.listen)
        .await
        .map_err(|e| format!("cannot listen on {}: {e}", config.listen))?;
    let core = match &config.cluster {
        Some(c) => Some(crate::relay::ClusterCore::new(&c.nodes, c.auth.clone(), Duration::from_secs(10))?),
        None => {
            tracing::warn!("IC_CLUSTER_NODES is not set: deaths are kept but not told to claims");
            None
        }
    };
    tracing::info!(?config, policy = app.access.version(), "customer master started");
    let stopped = |mut s: tokio::sync::watch::Receiver<bool>| async move {
        let _ = s.wait_for(|v| *v).await;
    };
    let web = tokio::spawn(crate::service::serve(listener, crate::service::router(app.clone()), stopped(stop.clone())));
    run(&app, &hubs, core.as_ref(), config.cycle_every, config.sweep_every, stopped(stop)).await;
    web.await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())
}

/// The process: logging, signals, settings from the environment, then
/// [`start`]. The binary is only this call.
pub async fn main() -> Result<(), String> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .try_init()
        .ok();
    // The handlers are installed before anything else, so a signal during
    // start-up is kept.
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).map_err(|e| e.to_string())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).map_err(|e| e.to_string())?;
    let (stopping, stop) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        tokio::select! {
            _ = terminate.recv() => {}
            _ = interrupt.recv() => {}
        }
        let _ = stopping.send(true);
    });
    let config = crate::config::Config::from_lookup(|k| std::env::var(k).ok(), |p| std::fs::read_to_string(p))?;
    let credential = ic_azure::credential::AzureCredential::from_env().map_err(|e| e.to_string())?;
    start(config, credential, stop).await
}
