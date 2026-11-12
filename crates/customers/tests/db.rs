//! Connecting to PostgreSQL: the Azure connector's token and TLS, the
//! local connector's limits, the pool and the migrations.

mod common;

use deadpool::managed::Manager as _;
use ic_azure::credential::AzureCredential;
use ic_customers::db::{self, Connector, Manager};

/// Purpose: a connection string without TLS is accepted only for this machine, and a
/// malformed one is refused with a reason.
/// Why it matters: a production database reached in the clear would expose every
/// encrypted payload's metadata and the service's sign-in on the network.
#[test]
fn plain_connections_are_only_local() {
    assert!(Connector::local("host=localhost user=x").is_ok());
    assert!(Connector::local("host=/var/run/postgresql user=x").is_ok());
    assert!(Connector::local("host=::1 user=x").is_ok());
    let e = Connector::local("host=pg.postgres.database.azure.com user=x").unwrap_err();
    assert!(e.contains("only allowed to this machine"));
    assert!(Connector::local("host=127.0.0.1 port=notaport")
        .unwrap_err()
        .contains("not a PostgreSQL connection string"));
    let shown = format!("{:?}", Connector::local("host=localhost dbname=customers password=secret").unwrap());
    assert!(shown.contains("customers") && !shown.contains("secret"));
}

/// Purpose: the Azure connector fetches an Entra ID token before connecting and insists on
/// TLS, so a server that does not offer TLS is refused, and a failed token fetch is
/// reported as such.
/// Why it matters: in Azure there is no database password; the token is the only
/// credential, and it must never travel over an unencrypted connection.
#[tokio::test]
async fn the_azure_connector_needs_a_token_and_tls() {
    // A local server without TLS: the token is fetched, then TLS is refused.
    let c = Connector::azure("127.0.0.1", "postgres", "id-customers", AzureCredential::static_token("t"));
    let e = c.connect().await.unwrap_err();
    assert!(e.starts_with("cannot connect to the database"), "{e}");
    // No identity endpoint: no token, no connection attempt.
    let c = Connector::azure(
        "127.0.0.1",
        "postgres",
        "id-customers",
        AzureCredential::managed_identity("http://127.0.0.1:9/token", None),
    );
    assert!(c.connect().await.unwrap_err().starts_with("no database token"));
    assert!(format!("{c:?}").contains("postgres"));
}

/// Purpose: the pool opens, applies the migrations once even when two replicas start
/// together, and a closed connection is not handed out again; a pool with no server fails
/// to open.
/// Why it matters: replicas start at the same time on every deploy, and a migration run
/// twice would fail the second start.
#[tokio::test]
async fn the_pool_migrates_once_and_recycles() {
    let Some(server) = common::server() else { return };
    let database = common::Database::new(server).await;
    let (a, b) = tokio::join!(db::open(database.connector.clone(), 2), db::open(database.connector.clone(), 2));
    let pool = a.unwrap();
    b.unwrap();
    let client = pool.get().await.unwrap();
    let n: i64 = client.query_one("select count(*) from cm_migrations", &[]).await.unwrap().get(0);
    assert_eq!(n, i64::try_from(db::MIGRATIONS.len()).unwrap());
    drop(client);
    db::open(database.connector.clone(), 2).await.unwrap();
    let manager = Manager(database.connector.clone());
    let mut fresh = manager.create().await.unwrap();
    assert!(manager.recycle(&mut fresh, &deadpool::managed::Metrics::default()).await.is_ok());
    let name: String = fresh.query_one("select current_database()", &[]).await.unwrap().get(0);
    let admin = common::server().unwrap().connect().await.unwrap();
    admin
        .execute(
            "select pg_terminate_backend(pid) from pg_stat_activity where datname = $1 and pid <> pg_backend_pid()",
            &[&name],
        )
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(manager.recycle(&mut fresh, &deadpool::managed::Metrics::default()).await.is_err());
    let nowhere = Connector::local("host=127.0.0.1 port=9 user=x connect_timeout=2").unwrap();
    assert!(db::open(nowhere, 1).await.is_err());
}
