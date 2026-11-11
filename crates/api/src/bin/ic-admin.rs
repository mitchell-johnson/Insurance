//! `ic-admin`: operate a cluster of ledger nodes. See [`ic_api::admin`].

use ic_api::admin::{Admin, AdminCommand, AdminSettings};

#[tokio::main]
async fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = AdminCommand::parse(&args)?;
    let settings = AdminSettings::from_lookup(|key| std::env::var(key).ok())?;
    let report = Admin::new(&settings).run(command).await?;
    println!("{}", serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?);
    Ok(())
}
