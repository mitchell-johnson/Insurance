//! The ledger client with a node that does not answer: the way a lost
//! region's voters look from the region that is left.

use std::time::{Duration, Instant};

use axum::routing::put;
use axum::{Json, Router};
use ic_raft::ClusterClient;

/// A node that answers: it acknowledges any consumer's events.
async fn answering() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let app = Router::new().route("/v1/partitions/{partition}/consumers/{consumer}", put(|| async { Json(()) }));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

/// A node that takes the connection and never answers, as a voter in a
/// region cut off by the network does to a caller still holding a route.
async fn silent() -> (tokio::net::TcpListener, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    (listener, addr)
}

/// Purpose: a node that did not answer is tried after the others for a
/// while, so a request for a group the client knows no leader of goes to a
/// node that answers first.
/// Why it matters: with a region lost, each of its voters costs a request
/// the whole attempt timeout (five seconds) before the client moves on. The
/// partitions' leaders all move at once, so without this every partition's
/// first request after the loss would wait out both lost voters in turn.
#[tokio::test]
async fn a_node_that_did_not_answer_is_tried_last() {
    let (_held, lost) = silent().await;
    let alive = answering().await;
    let client = ClusterClient::new(vec![lost.clone(), alive.clone()]).with_attempt_timeout(Duration::from_millis(400));

    // The first request finds out: the lost node first, as given, then the other.
    let started = Instant::now();
    client.acknowledge_events(0, "drill", 1).await.unwrap();
    assert!(started.elapsed() >= Duration::from_millis(400), "the lost node was tried first and waited out");

    // Another partition, with no leader known: the node that answers first.
    let started = Instant::now();
    client.acknowledge_events(1, "drill", 1).await.unwrap();
    assert!(
        started.elapsed() < Duration::from_millis(300),
        "the lost node was tried first again: {:?}",
        started.elapsed()
    );
}

/// Purpose: a node that is not answering is still tried, after the others,
/// so the client is never left with nowhere to send.
/// Why it matters: if the only nodes left were ones that once failed to
/// answer, skipping them would turn a slow ledger into an unavailable one.
#[tokio::test]
async fn a_node_tried_last_is_still_tried() {
    let (_held, lost) = silent().await;
    let alive = answering().await;
    let client = ClusterClient::new(vec![alive.clone(), lost.clone()]).with_attempt_timeout(Duration::from_millis(200));

    // Only the lost node is known to a second client sharing nothing: it
    // fails, and the error names it.
    let alone = ClusterClient::new(vec![lost.clone()]).with_attempt_timeout(Duration::from_millis(200));
    let first = alone.acknowledge_events(0, "drill", 1).await.unwrap_err().to_string();
    assert!(first.contains(&lost), "{first}");
    let again = alone.acknowledge_events(0, "drill", 1).await.unwrap_err().to_string();
    assert!(again.contains(&lost), "the lost node was not tried again: {again}");

    // With an answering node first, the lost one is never needed.
    client.acknowledge_events(0, "drill", 1).await.unwrap();
}

/// Purpose: a node that answers success with a body the client cannot read
/// is a failure that names the node, not an answer.
/// Why it matters: a proxy or a mismatched version answering 200 with
/// something else must not be taken as the ledger having confirmed a call.
#[tokio::test]
async fn a_success_the_client_cannot_read_is_a_failure() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let app = Router::new().route("/v1/partitions/{partition}/consumers/{consumer}", put(|| async { "not json" }));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ClusterClient::new(vec![addr.clone()]);
    let error = client.acknowledge_events(0, "drill", 1).await.unwrap_err().to_string();
    assert!(error.contains(&addr), "{error}");
}
