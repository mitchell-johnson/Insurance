//! The service's settings, from the environment. Secrets come from files
//! the Key Vault CSI driver mounts. See `docs/customers.md`.

use std::time::Duration;

use crate::auth::ENTRA_AUTHORITY;

#[derive(Clone, PartialEq, Eq)]
pub enum Database {
    /// Azure Database for PostgreSQL, signed in with the workload identity.
    Azure { host: String, database: String, user: String },
    /// A server on this machine, for tests.
    Local(String),
}

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Database::Azure { host, database, user } => write!(f, "Azure({host}/{database} as {user})"),
            // A local URL may carry a password.
            Database::Local(_) => f.write_str("Local(..)"),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Config {
    pub database: Database,
    pub pool_size: usize,
    /// `https://<vault>.vault.azure.net` or a Managed HSM.
    pub key_vault: String,
    /// The key-encryption key's name.
    pub kek: String,
    /// The blind index key. Never changes.
    pub index_key: String,
    pub tenant: String,
    pub audience: String,
    pub authority: String,
    pub event_hubs_namespace: String,
    pub event_hub: String,
    pub priority_hub: String,
    pub hub_partitions: u16,
    pub register_endpoint: String,
    pub register_container: String,
    pub listen: String,
    pub cycle_every: Duration,
    pub sweep_every: Duration,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let db = match &self.database {
            Database::Azure { host, database, .. } => format!("{host}/{database}"),
            Database::Local(_) => "local".into(),
        };
        // Never the index key.
        write!(f, "Config(database {db}, vault {}, listen {})", self.key_vault, self.listen)
    }
}

impl Config {
    /// Read through `get` (the environment in production) and `read` (the
    /// file system, for mounted secrets). Every setting without a default
    /// is required.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>, read: impl Fn(&str) -> std::io::Result<String>) -> Result<Self, String> {
        let get = |key: &str| get(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let need = |key: &str| get(key).ok_or_else(|| format!("{key} is not set"));
        let number = |key: &str, default: u64| -> Result<u64, String> {
            match get(key) {
                Some(text) => text
                    .parse::<u64>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| format!("{key} must be a whole number above zero")),
                None => Ok(default),
            }
        };
        let index_key = match (get("IC_CUSTOMERS_INDEX_KEY"), get("IC_CUSTOMERS_INDEX_KEY_FILE")) {
            (Some(v), _) => v,
            (None, Some(path)) => read(&path)
                .map_err(|e| format!("IC_CUSTOMERS_INDEX_KEY_FILE cannot be read: {e}"))?
                .trim()
                .to_string(),
            (None, None) => return Err("IC_CUSTOMERS_INDEX_KEY is not set".into()),
        };
        if index_key.len() < 32 {
            return Err("IC_CUSTOMERS_INDEX_KEY must be at least 32 characters".into());
        }
        let database = match get("IC_CUSTOMERS_DATABASE_URL") {
            Some(url) => Database::Local(url),
            None => Database::Azure {
                host: need("IC_CUSTOMERS_PG_HOST")?,
                database: get("IC_CUSTOMERS_PG_DATABASE").unwrap_or_else(|| "customers".into()),
                user: need("IC_CUSTOMERS_PG_USER")?,
            },
        };
        let partitions = number("IC_CUSTOMERS_HUB_PARTITIONS", 8)?;
        Ok(Self {
            database,
            pool_size: usize::try_from(number("IC_CUSTOMERS_POOL_SIZE", 16)?).unwrap_or(16),
            key_vault: need("IC_CUSTOMERS_KEY_VAULT")?,
            kek: get("IC_CUSTOMERS_KEK").unwrap_or_else(|| "customers-kek".into()),
            index_key,
            tenant: need("IC_CUSTOMERS_TENANT")?,
            audience: need("IC_CUSTOMERS_AUDIENCE")?,
            authority: get("IC_CUSTOMERS_AUTHORITY").unwrap_or_else(|| ENTRA_AUTHORITY.into()),
            event_hubs_namespace: need("IC_EVENT_HUBS_NAMESPACE")?,
            event_hub: get("IC_CUSTOMERS_EVENT_HUB").unwrap_or_else(|| "customer-events".into()),
            priority_hub: get("IC_CUSTOMERS_PRIORITY_HUB").unwrap_or_else(|| "customer-priority".into()),
            hub_partitions: u16::try_from(partitions).map_err(|_| "IC_CUSTOMERS_HUB_PARTITIONS is too large".to_string())?,
            register_endpoint: need("IC_CUSTOMERS_REGISTER_ENDPOINT")?,
            register_container: get("IC_CUSTOMERS_REGISTER_CONTAINER").unwrap_or_else(|| "customers-erasures".into()),
            listen: get("IC_CUSTOMERS_LISTEN").unwrap_or_else(|| "0.0.0.0:8080".into()),
            cycle_every: Duration::from_secs(number("IC_CUSTOMERS_CYCLE_SECONDS", 2)?),
            sweep_every: Duration::from_secs(60 * number("IC_CUSTOMERS_SWEEP_MINUTES", 60)?),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn base() -> BTreeMap<&'static str, String> {
        [
            ("IC_CUSTOMERS_PG_HOST", "pg.postgres.database.azure.com"),
            ("IC_CUSTOMERS_PG_USER", "id-customers"),
            ("IC_CUSTOMERS_KEY_VAULT", "https://kv.vault.azure.net"),
            ("IC_CUSTOMERS_INDEX_KEY_FILE", "/mnt/secrets/customers-index-key"),
            ("IC_CUSTOMERS_TENANT", "t"),
            ("IC_CUSTOMERS_AUDIENCE", "api://customers"),
            ("IC_EVENT_HUBS_NAMESPACE", "https://ns.servicebus.windows.net"),
            ("IC_CUSTOMERS_REGISTER_ENDPOINT", "https://st.blob.core.windows.net"),
        ]
        .into_iter()
        .map(|(k, v)| (k, v.to_string()))
        .collect()
    }

    fn load(env: &BTreeMap<&'static str, String>) -> Result<Config, String> {
        Config::from_lookup(
            |k| env.get(k).cloned(),
            |p| {
                if p == "/mnt/secrets/customers-index-key" {
                    Ok("k".repeat(40) + "\n")
                } else {
                    Err(std::io::Error::other("no such file"))
                }
            },
        )
    }

    /// Purpose: a complete Azure configuration loads with its defaults, reads the index
    /// key from the mounted file, and never prints the key.
    /// Why it matters: a service that starts with a wrong or missing setting fails at the
    /// first request instead of at start, and a logged index key would undo blind indexing.
    #[test]
    fn the_azure_configuration_loads() {
        let c = load(&base()).unwrap();
        assert_eq!(
            c.database,
            Database::Azure {
                host: "pg.postgres.database.azure.com".into(),
                database: "customers".into(),
                user: "id-customers".into()
            }
        );
        assert_eq!(c.index_key.len(), 40);
        assert_eq!(c.kek, "customers-kek");
        assert_eq!(c.hub_partitions, 8);
        assert_eq!(c.sweep_every, Duration::from_secs(3600));
        let shown = format!("{c:?}");
        assert!(!shown.contains("kkkk") && shown.contains("pg.postgres.database.azure.com/customers"));
        let mut env = base();
        env.insert("IC_CUSTOMERS_DATABASE_URL", "host=localhost".into());
        let c = load(&env).unwrap();
        assert_eq!(c.database, Database::Local("host=localhost".into()));
        assert!(format!("{c:?}").contains("local"));
        assert_eq!(format!("{:?}", c.database), "Local(..)");
        let azure = load(&base()).unwrap().database;
        assert_eq!(format!("{azure:?}"), "Azure(pg.postgres.database.azure.com/customers as id-customers)");
    }

    /// Purpose: missing settings, a short or unreadable index key and bad numbers stop the
    /// service at start with the setting's name.
    /// Why it matters: an operator must be told exactly what is wrong before any request
    /// is taken.
    #[test]
    fn bad_settings_are_named() {
        for key in [
            "IC_CUSTOMERS_PG_HOST",
            "IC_CUSTOMERS_KEY_VAULT",
            "IC_CUSTOMERS_TENANT",
            "IC_EVENT_HUBS_NAMESPACE",
        ] {
            let mut env = base();
            env.remove(key);
            assert!(load(&env).unwrap_err().contains(key));
        }
        let mut env = base();
        env.remove("IC_CUSTOMERS_INDEX_KEY_FILE");
        assert!(load(&env).unwrap_err().contains("IC_CUSTOMERS_INDEX_KEY"));
        env.insert("IC_CUSTOMERS_INDEX_KEY", "short".into());
        assert!(load(&env).unwrap_err().contains("at least 32"));
        env.insert("IC_CUSTOMERS_INDEX_KEY_FILE", "/elsewhere".into());
        env.remove("IC_CUSTOMERS_INDEX_KEY");
        assert!(load(&env).unwrap_err().contains("cannot be read"));
        let mut env = base();
        env.insert("IC_CUSTOMERS_CYCLE_SECONDS", "0".into());
        assert!(load(&env).unwrap_err().contains("IC_CUSTOMERS_CYCLE_SECONDS"));
        env.insert("IC_CUSTOMERS_CYCLE_SECONDS", "3".into());
        env.insert("IC_CUSTOMERS_HUB_PARTITIONS", "70000".into());
        assert!(load(&env).unwrap_err().contains("too large"));
    }
}
