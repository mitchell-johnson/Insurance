//! `ic-payments-pump`: the payments module's driver.
//!
//! - `ic-payments-pump` works the module's queue until it is told to stop.
//!   One runs in every region, so whichever region holds the leaders can
//!   submit to the banks.
//! - `ic-payments-pump once` makes one pass and fails if anything could not
//!   be done.
//! - `ic-payments-pump drill <id>` runs the evacuation drill from this
//!   region and fails if any bank channel could not be reached.
//!
//! See [`ic_payments_service::settings::PumpSettings`] for the environment.

use std::sync::{Arc, Mutex};

use ic_azure::blob::BlobContainer;
use ic_azure::credential::AzureCredential;
use ic_cashmatch::Matcher;
use ic_payments_service::bank::HttpBank;
use ic_payments_service::ports::{ClusterCore, LogNotifier, MountedSecrets, VaultKeys};
use ic_payments_service::pump::{nz_now, Pump};
use ic_payments_service::settings::PumpSettings;
use ic_raft::ClusterClient;

#[tokio::main]
async fn main() -> Result<(), String> {
    tracing_subscriber::fmt()
        .json()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let settings = PumpSettings::from_lookup(|key| std::env::var(key).ok())?;
    let credential = AzureCredential::from_env().map_err(|e| e.to_string())?;
    let client = ClusterClient::new(settings.nodes.clone()).with_auth(settings.auth.clone());
    let pump = Pump {
        module: client.clone(),
        core: ClusterCore {
            client,
            matcher: Matcher::default(),
        },
        bank: HttpBank::default(),
        vault: BlobContainer::new(&settings.blob_endpoint, &settings.container, credential.clone()),
        keys: VaultKeys {
            vault: settings.key_vault.clone(),
            credential,
        },
        secrets: MountedSecrets(settings.secrets_dir.clone()),
        notifier: LogNotifier,
        region: settings.region.clone(),
        instance: settings.instance.clone(),
        clock: Arc::new(nz_now),
        grace: settings.grace,
        cursors: Mutex::default(),
    };
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        [] => loop {
            let pass = pump.pass().await;
            tracing::info!(
                sent = pass.sent,
                failed = pass.failed,
                messages = pass.messages,
                instructions = pass.instructions,
                "pass"
            );
            for problem in &pass.problems {
                tracing::warn!(target: "payments.pump", problem, "not done this pass");
            }
            tokio::select! {
                () = tokio::time::sleep(settings.interval) => {}
                _ = tokio::signal::ctrl_c() => return Ok(()),
            }
        },
        ["once"] => {
            let pass = pump.pass().await;
            println!("{pass:?}");
            if pass.problems.is_empty() {
                Ok(())
            } else {
                Err(pass.problems.join("; "))
            }
        }
        ["drill", id] => match pump.drill(id).await? {
            true => Ok(()),
            false => Err(format!("drill {id} from {}: a bank channel could not be reached", settings.region)),
        },
        _ => Err("usage: ic-payments-pump [once | drill <id>]".to_string()),
    }
}
