//! The reinsurance driver process: carries the module's instructions to the
//! core. Settings are in the environment; see `ic_reinsurance::service::Config`
//! and `docs/reinsurance.md`. Any number may run: the core applies each
//! instruction once.

use ic_raft::{ClusterAuth, ClusterClient};
use ic_reinsurance::driver::{ClusterAudiences, ClusterCore, ClusterModule, Driver};
use ic_reinsurance::service::Config;

async fn stop_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}

#[tokio::main]
async fn main() -> Result<(), String> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let config = Config::from_lookup(|key| std::env::var(key).ok())?;
    let auth = ClusterAuth::from_lookup(|key| std::env::var(key).ok(), |path| std::fs::read_to_string(path))?;
    let client = ClusterClient::from_list(&config.cluster)?.with_auth(auth);
    let driver = Driver {
        module: ClusterModule::new(client.clone(), config.patience),
        core: ClusterCore::new(client.clone(), config.patience),
        batch: config.batch,
    };
    tracing::info!(?config, "reinsurance driver started");
    let audiences = ClusterAudiences::new(client, config.patience);
    driver.run(&audiences, config.cycle_every, stop_signal(), |_, _| {}).await;
    Ok(())
}
