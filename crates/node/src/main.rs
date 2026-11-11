//! `ic-node`: one ledger node, hosting the partitions and every module.
//! See [`ic_raft::server::Settings`] for its environment variables.

#[tokio::main]
async fn main() -> Result<(), String> {
    ic_raft::server::run(ic_node::modules()?).await
}
