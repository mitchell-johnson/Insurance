//! The unit pricing driver process: valuation files in, units on issue from
//! the core, extracts out. Settings are in the environment; see
//! `ic_pricing::service` and `docs/unit-pricing.md`.

#[tokio::main]
async fn main() -> Result<(), String> {
    ic_pricing::service::run(|key| std::env::var(key).ok(), |path| std::fs::read_to_string(path)).await
}
