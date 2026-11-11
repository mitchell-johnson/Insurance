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

use ic_azure::credential::AzureCredential;
use ic_payments_service::pump::DeployedPump;
use ic_payments_service::settings::PumpSettings;

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
    let stop = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    DeployedPump::deployed(&settings, credential).run(&args, settings.interval, stop).await
}
