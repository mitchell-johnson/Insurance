//! Connections to Azure Database for PostgreSQL.
//!
//! In Azure the service signs in with its workload identity: the password
//! is a Microsoft Entra ID token, fetched for each new connection, and the
//! connection is TLS verified against the public roots. There is no
//! database password anywhere. Tests connect to a local server with a URL.

use std::sync::Arc;

use deadpool::managed::{self, Metrics, RecycleError, RecycleResult};
use ic_azure::credential::AzureCredential;
use tokio_postgres::config::SslMode;
use tokio_postgres::{Client, Config, NoTls};

/// The Entra ID scope for Azure Database for PostgreSQL.
pub const SCOPE_POSTGRES: &str = "https://ossrdbms-aad.database.windows.net/.default";

/// The migrations, in order. Each runs once, in one transaction.
pub const MIGRATIONS: &[(i32, &str)] = &[
    (1, include_str!("../migrations/001_init.sql")),
    (2, include_str!("../migrations/002_death_relay.sql")),
    (3, include_str!("../migrations/003_retractions.sql")),
    (4, include_str!("../migrations/004_prompts.sql")),
    (5, include_str!("../migrations/005_marker_reviews.sql")),
    (6, include_str!("../migrations/006_directory_relay.sql")),
    (7, include_str!("../migrations/007_index_keys.sql")),
    (8, include_str!("../migrations/008_spending_holds.sql")),
    (9, include_str!("../migrations/009_owner_hold_relay.sql")),
];

#[derive(Clone)]
enum Password {
    /// As given in the configuration, or none.
    Fixed,
    /// An Entra ID token for each new connection.
    Entra(AzureCredential),
}

/// How to open a connection.
#[derive(Clone)]
pub struct Connector {
    config: Config,
    password: Password,
    tls: Option<tokio_postgres_rustls::MakeRustlsConnect>,
}

impl std::fmt::Debug for Connector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Connector({:?}, db {:?})", self.config.get_hosts(), self.config.get_dbname())
    }
}

fn tls() -> tokio_postgres_rustls::MakeRustlsConnect {
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("the ring provider supports the default protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    tokio_postgres_rustls::MakeRustlsConnect::new(config)
}

impl Connector {
    /// A server in Azure: `host`, `database` and the Entra ID principal
    /// name the service signs in as. TLS is required and verified.
    pub fn azure(host: &str, database: &str, user: &str, credential: AzureCredential) -> Self {
        let mut config = Config::new();
        config.host(host).port(5432).dbname(database).user(user).ssl_mode(SslMode::Require);
        Self {
            config,
            password: Password::Entra(credential),
            tls: Some(tls()),
        }
    }

    /// A server named by a connection string, without TLS. Refused for any
    /// host but the loopback, so it cannot be used to reach a real server
    /// in the clear.
    pub fn local(url: &str) -> Result<Self, String> {
        let config: Config = url.parse().map_err(|e| format!("not a PostgreSQL connection string: {e}"))?;
        let local = config.get_hosts().iter().all(|h| match h {
            tokio_postgres::config::Host::Tcp(name) => name == "localhost" || name == "127.0.0.1" || name == "::1",
            tokio_postgres::config::Host::Unix(_) => true,
        });
        if !local {
            return Err("a connection without TLS is only allowed to this machine".into());
        }
        Ok(Self {
            config,
            password: Password::Fixed,
            tls: None,
        })
    }

    /// The same server, another database.
    pub fn with_database(&self, database: &str) -> Self {
        let mut c = self.clone();
        c.config.dbname(database);
        c
    }

    pub async fn connect(&self) -> Result<Client, String> {
        let mut config = self.config.clone();
        if let Password::Entra(credential) = &self.password {
            let token = credential.token(SCOPE_POSTGRES).await.map_err(|e| format!("no database token: {e}"))?;
            config.password(token);
        }
        let failed = |e: tokio_postgres::Error| format!("cannot connect to the database: {e}");
        let client = match &self.tls {
            Some(tls) => {
                let (client, connection) = config.connect(tls.clone()).await.map_err(failed)?;
                tokio::spawn(connection);
                client
            }
            None => {
                let (client, connection) = config.connect(NoTls).await.map_err(failed)?;
                tokio::spawn(connection);
                client
            }
        };
        Ok(client)
    }
}

/// The pool's manager: opens connections and checks them on reuse.
pub struct Manager(pub Connector);

impl managed::Manager for Manager {
    type Type = Client;
    type Error = String;

    async fn create(&self) -> Result<Client, String> {
        self.0.connect().await
    }

    async fn recycle(&self, client: &mut Client, _: &Metrics) -> RecycleResult<String> {
        if client.is_closed() {
            return Err(RecycleError::message("the connection is closed"));
        }
        client.simple_query("").await.map(|_| ()).map_err(|e| RecycleError::message(e.to_string()))
    }
}

pub type Pool = managed::Pool<Manager>;

/// A pool of up to `size` connections, with the schema brought up to date.
pub async fn open(connector: Connector, size: usize) -> Result<Pool, String> {
    let pool = Pool::builder(Manager(connector))
        .max_size(size)
        .wait_timeout(Some(std::time::Duration::from_secs(10)))
        .create_timeout(Some(std::time::Duration::from_secs(10)))
        .runtime(deadpool::Runtime::Tokio1)
        .build()
        .map_err(crate::text)?;
    migrate(&pool).await?;
    Ok(pool)
}

/// Apply the migrations not yet applied. Several replicas may start at
/// once; an advisory lock lets one do it while the others wait.
async fn migrate(pool: &Pool) -> Result<(), String> {
    let mut client = pool.get().await.map_err(|e| format!("cannot reach the database: {e}"))?;
    let tx = client.transaction().await.map_err(crate::text)?;
    tx.execute("select pg_advisory_xact_lock(7151001)", &[]).await.map_err(crate::text)?;
    tx.execute(
        "create table if not exists cm_migrations (version integer primary key, applied_at timestamptz not null default now())",
        &[],
    )
    .await
    .map_err(crate::text)?;
    for (version, sql) in MIGRATIONS {
        let done = tx
            .query_opt("select 1 from cm_migrations where version = $1", &[version])
            .await
            .map_err(crate::text)?
            .is_some();
        if !done {
            tx.batch_execute(sql).await.map_err(|e| format!("migration {version} failed: {e}"))?;
            tx.execute("insert into cm_migrations (version) values ($1)", &[version])
                .await
                .map_err(crate::text)?;
            tracing::info!(version, "database migration applied");
        }
    }
    tx.commit().await.map_err(crate::text)
}
