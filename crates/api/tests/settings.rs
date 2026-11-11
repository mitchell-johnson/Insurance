//! The API's settings: which arrangement it runs in and whom it trusts.

use std::path::PathBuf;
use std::time::Duration;

use chrono::NaiveDate;
use ic_api::settings::{ApiSettings, Identity, Mode};

const TOKEN: &str = "test-cluster-token-0123456789abcdef";

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2027, 11, 15).unwrap()
}

fn read(vars: &[(&str, &str)]) -> Result<ApiSettings, String> {
    ApiSettings::from_lookup(|key| vars.iter().find(|(k, _)| *k == key).map(|(_, v)| (*v).to_string()), today())
}

fn refused(vars: &[(&str, &str)], names: &str) {
    let error = read(vars).err().unwrap_or_else(|| panic!("{vars:?} should have been refused"));
    assert!(error.contains(names), "{error:?} does not mention {names:?}");
}

/// Purpose: with no node addresses the API is the insurance in a box, with the documented
/// defaults, and each single-process setting is read.
/// Why it matters: this is the arrangement developers and demos run. The opening date in
/// particular is set once on an empty ledger and never again, so a default or a parse that
/// went wrong would put a ledger on the wrong day for good.
#[test]
fn without_nodes_the_api_is_the_insurance_in_a_box() {
    let settings = read(&[("IC_AUTH", "dev")]).unwrap();
    assert_eq!(settings.listen.to_string(), "0.0.0.0:8080");
    assert_eq!(settings.identity, Identity::Dev);
    let Mode::Single(engine) = settings.mode else {
        panic!("expected the single-process mode");
    };
    assert_eq!(engine.partitions, ic_ledger::PARTITION_COUNT);
    assert_eq!(engine.opening_date, today());
    assert_eq!(engine.data_dir, None);
    assert!(engine.load_shipped_config);
    assert_eq!(engine.azure.blob_endpoint, None);
    assert_eq!(engine.azure.merkle_container, "merkle-roots");
    assert_eq!(engine.azure.merkle_key, "merkle-root-signing");
    assert_eq!(engine.azure.event_hub, "policy-events");

    let settings = read(&[
        ("IC_AUTH", "dev"),
        ("IC_LISTEN", "127.0.0.1:9000"),
        ("IC_PARTITIONS", "4"),
        ("IC_DATA_DIR", "/var/lib/ic"),
        ("IC_OPENING_DATE", "2027-01-01"),
        ("IC_LOAD_SHIPPED_CONFIG", "false"),
    ])
    .unwrap();
    assert_eq!(settings.listen.to_string(), "127.0.0.1:9000");
    let Mode::Single(engine) = settings.mode else {
        panic!("expected the single-process mode");
    };
    assert_eq!(engine.partitions, 4);
    assert_eq!(engine.data_dir, Some(PathBuf::from("/var/lib/ic")));
    assert_eq!(engine.opening_date, NaiveDate::from_ymd_opt(2027, 1, 1).unwrap());
    assert!(!engine.load_shipped_config);

    // On Azure, callers are identified by Entra ID and the endpoints are passed on.
    let settings = read(&[
        ("IC_ENTRA_TENANT_ID", "tenant"),
        ("IC_ENTRA_AUDIENCE", "api://core"),
        ("IC_BLOB_ENDPOINT", "https://acct.blob.core.windows.net"),
        ("IC_KEY_VAULT_URL", "https://vault.vault.azure.net"),
        ("IC_EVENT_HUBS_ENDPOINT", "https://ns.servicebus.windows.net"),
        ("IC_MERKLE_CONTAINER", "roots"),
        ("IC_MERKLE_KEY", "signing"),
        ("IC_EVENT_HUB", "events"),
    ])
    .unwrap();
    assert_eq!(
        settings.identity,
        Identity::Entra {
            authority: "https://login.microsoftonline.com".to_string(),
            tenant: "tenant".to_string(),
            audience: "api://core".to_string()
        }
    );
    let sovereign = read(&[
        ("IC_ENTRA_TENANT_ID", "tenant"),
        ("IC_ENTRA_AUDIENCE", "api://core"),
        ("IC_ENTRA_AUTHORITY", "https://login.microsoftonline.us"),
    ])
    .unwrap();
    assert!(matches!(sovereign.identity, Identity::Entra { authority, .. } if authority == "https://login.microsoftonline.us"));
    let Mode::Single(engine) = settings.mode else {
        panic!("expected the single-process mode");
    };
    assert_eq!(engine.azure.blob_endpoint.as_deref(), Some("https://acct.blob.core.windows.net"));
    assert_eq!(engine.azure.key_vault_url.as_deref(), Some("https://vault.vault.azure.net"));
    assert_eq!(engine.azure.event_hubs_endpoint.as_deref(), Some("https://ns.servicebus.windows.net"));
    assert_eq!(
        (
            engine.azure.merkle_container.as_str(),
            engine.azure.merkle_key.as_str(),
            engine.azure.event_hub.as_str()
        ),
        ("roots", "signing", "events")
    );
}

/// Purpose: with node addresses the API is a gateway: it reads the nodes, the cluster token,
/// the partition count and its patience, and keeps no ledger of its own.
/// Why it matters: this is the production arrangement. A gateway that started without a
/// cluster token would be turned away by every node; one that also opened a local ledger
/// would take business that the real ledger never saw.
#[test]
fn with_nodes_the_api_is_a_gateway() {
    let base = [
        ("IC_ENTRA_TENANT_ID", "tenant"),
        ("IC_ENTRA_AUDIENCE", "api://core"),
        ("IC_NODES", "node-0:7000, node-1:7000 ,node-2:7000"),
        ("IC_CLUSTER_TOKEN", TOKEN),
    ];
    let settings = read(&base).unwrap();
    let Mode::Cluster(cluster) = settings.mode else {
        panic!("expected the gateway mode");
    };
    assert_eq!(cluster.nodes, vec!["node-0:7000", "node-1:7000", "node-2:7000"]);
    assert_eq!(cluster.partitions, ic_ledger::PARTITION_COUNT);
    assert_eq!(cluster.patience, Duration::from_secs(10));
    assert_eq!(cluster.auth.outgoing(), Some(TOKEN));

    let mut tuned = base.to_vec();
    tuned.extend([("IC_PARTITIONS", "8"), ("IC_LEDGER_PATIENCE_SECS", "3")]);
    let Mode::Cluster(cluster) = read(&tuned).unwrap().mode else {
        panic!("expected the gateway mode");
    };
    assert_eq!((cluster.partitions, cluster.patience), (8, Duration::from_secs(3)));

    let without = |key: &str| base.iter().copied().filter(|(k, _)| *k != key).collect::<Vec<_>>();
    refused(&without("IC_CLUSTER_TOKEN"), "IC_CLUSTER_TOKEN_FILE");
    let mut both = base.to_vec();
    both.push(("IC_DATA_DIR", "/var/lib/ic"));
    refused(&both, "keeps no ledger of its own");
    let mut empty = without("IC_NODES");
    empty.push(("IC_NODES", " , "));
    refused(&empty, "IC_NODES");
    let mut bad = base.to_vec();
    bad.push(("IC_LEDGER_PATIENCE_SECS", "soon"));
    refused(&bad, "IC_LEDGER_PATIENCE_SECS");
}

/// Purpose: malformed settings are refused with the setting named, and the development
/// identity, which trusts whatever headers a caller sends, is refused wherever the process
/// could be reachable by real callers: in Kubernetes, or with any Azure endpoint configured.
/// Why it matters: `IC_AUTH=dev` lets any caller claim any role. One stray variable in a
/// manifest must not be able to turn authentication off in production, and a typo in a
/// setting must stop the process at start-up rather than run it with a wrong default.
#[test]
fn bad_settings_and_unsafe_identities_are_refused() {
    refused(&[], "IC_ENTRA_TENANT_ID");
    refused(&[("IC_ENTRA_TENANT_ID", "tenant")], "IC_ENTRA_AUDIENCE");
    refused(&[("IC_AUTH", "none")], "IC_ENTRA_TENANT_ID");
    refused(&[("IC_AUTH", "dev"), ("IC_LISTEN", "everywhere")], "IC_LISTEN");
    refused(&[("IC_AUTH", "dev"), ("IC_PARTITIONS", "many")], "IC_PARTITIONS");
    refused(&[("IC_AUTH", "dev"), ("IC_PARTITIONS", "0")], "IC_PARTITIONS must be between");
    refused(&[("IC_AUTH", "dev"), ("IC_PARTITIONS", "65")], "IC_PARTITIONS must be between");
    refused(&[("IC_AUTH", "dev"), ("IC_OPENING_DATE", "tomorrow")], "IC_OPENING_DATE");

    for endpoint in ["IC_BLOB_ENDPOINT", "IC_KEY_VAULT_URL", "IC_EVENT_HUBS_ENDPOINT"] {
        refused(&[("IC_AUTH", "dev"), (endpoint, "https://example.invalid")], "refused when any Azure endpoint");
    }
    refused(&[("IC_AUTH", "dev"), ("KUBERNETES_SERVICE_HOST", "10.0.0.1")], "refused in Kubernetes");
    refused(
        &[
            ("IC_AUTH", "dev"),
            ("KUBERNETES_SERVICE_HOST", "10.0.0.1"),
            ("IC_NODES", "n:1"),
            ("IC_CLUSTER_TOKEN", TOKEN),
        ],
        "refused in Kubernetes",
    );
    // Entra ID is fine in Kubernetes, and blank values count as unset.
    let settings = read(&[
        ("IC_AUTH", "entra"),
        ("IC_ENTRA_TENANT_ID", "tenant"),
        ("IC_ENTRA_AUDIENCE", "api://core"),
        ("KUBERNETES_SERVICE_HOST", "10.0.0.1"),
        ("IC_DATA_DIR", "  "),
    ])
    .unwrap();
    assert!(matches!(settings.identity, Identity::Entra { .. }));
    assert!(matches!(settings.mode, Mode::Single(engine) if engine.data_dir.is_none()));
    // The identity turns into the authenticator the routes use.
    assert!(matches!(Identity::Dev.auth(), ic_api::auth::Auth::Dev));
    assert!(matches!(settings.identity.auth(), ic_api::auth::Auth::Entra(_)));
}
