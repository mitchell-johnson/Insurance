//! The reinsurance driver process: carries the module's instructions to the
//! core and its notices to the modules they are for. Settings are in the
//! environment; see `ic_reinsurance::service` and `docs/reinsurance.md`. Any
//! number may run: the core applies each instruction once.

#[tokio::main]
async fn main() -> Result<(), String> {
    ic_reinsurance::service::run(|key| std::env::var(key).ok(), |path| std::fs::read_to_string(path)).await
}
