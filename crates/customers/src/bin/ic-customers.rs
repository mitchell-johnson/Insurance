//! The customer master process. Configuration is in the environment; see
//! `ic_customers::config::Config` and `docs/customers.md`.

#[tokio::main]
async fn main() -> Result<(), String> {
    ic_customers::work::main().await
}
