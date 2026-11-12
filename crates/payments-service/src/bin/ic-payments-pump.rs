//! `ic-payments-pump`: the payments module's driver.
//!
//! - `ic-payments-pump` works the module's queue until it is told to stop.
//!   One runs in every region, so whichever region holds the leaders can
//!   submit to the banks.
//! - `ic-payments-pump once` makes one pass and fails if anything could not
//!   be done.
//! - `ic-payments-pump drill [<id>]` runs the evacuation drill from this
//!   region and fails if any bank channel could not be reached. Without an
//!   ID the drill is named for the day and the region.
//!
//! See [`ic_payments_service::settings::PumpSettings`] for the environment.

#[tokio::main]
async fn main() -> Result<(), String> {
    ic_payments_service::pump::log_to_stderr();
    let args: Vec<String> = std::env::args().skip(1).collect();
    ic_payments_service::pump::run_from(&args, |key| std::env::var(key).ok(), ic_payments_service::pump::interrupted()).await
}
