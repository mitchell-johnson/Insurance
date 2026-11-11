//! `ic-core-node`: a ledger node that hosts the partitions and no modules.
//! The node that is deployed is `ic-node` (crates/node), which hosts every
//! module as well. See [`ic_raft::server::Settings`] for the environment.

#[tokio::main]
async fn main() -> Result<(), String> {
    ic_raft::server::run(Vec::new()).await
}
