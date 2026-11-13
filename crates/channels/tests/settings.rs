//! The portal service's configuration.

use std::collections::BTreeMap;
use std::time::Duration;

use ic_channels::settings::{Identity, PortalSettings};
use ic_channels::ui::UiConfig;

fn load(vars: &[(&str, &str)]) -> Result<PortalSettings, String> {
    let map: BTreeMap<String, String> = vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    PortalSettings::from_lookup(
        |key| map.get(key).cloned(),
        |path| {
            if path == "/token" {
                Ok("file-token-0123456789abcdef0123456789\n".into())
            } else {
                Err(std::io::ErrorKind::NotFound.into())
            }
        },
    )
}

const NODES: (&str, &str) = ("IC_NODES", "n1:7000,n2:7000,n3:7000");
const OPEN: (&str, &str) = ("IC_CLUSTER_AUTH", "open");
const DEV: (&str, &str) = ("IC_AUTH", "dev");

/// Purpose: a laptop configuration loads with sensible defaults, and development identity is refused in Kubernetes.
/// Why it matters: development identity trusts caller headers; a deployment that started with it would let anyone be any customer.
#[test]
fn a_laptop_configuration_and_its_guard() {
    let s = load(&[NODES, OPEN, DEV]).unwrap();
    assert_eq!(s.listen.to_string(), "0.0.0.0:8090");
    assert_eq!(
        (s.customers.clone(), s.workforce.clone(), s.ui.clone()),
        (Identity::Dev, Identity::Dev, UiConfig::Dev)
    );
    assert_eq!((s.patience, s.heartbeat), (Duration::from_secs(10), Duration::from_secs(15)));
    assert_eq!((s.forward, s.currency.as_str()), (Duration::from_secs(15), "NZD"));
    assert_eq!((s.regions.home.as_str(), s.regions.nodes["local"].len(), s.nodes.len()), ("local", 3, 3));
    assert_eq!(s.client().nodes().len(), 3);
    assert!(matches!(s.customers.auth(), ic_api::auth::Auth::Dev));

    let refused = load(&[NODES, OPEN, DEV, ("KUBERNETES_SERVICE_HOST", "10.0.0.1")]).unwrap_err();
    assert!(refused.contains("refused in Kubernetes"));
    assert!(load(&[NODES, OPEN, ("IC_AUTH", "magic")]).unwrap_err().contains("entra or dev"));
}

/// Purpose: production needs both tenants: the workforce's Entra ID and the customers' External ID with its own authority.
/// Why it matters: validating customer tokens against the workforce tenant, or the other way round, would either lock everyone out or accept the wrong population as staff.
#[test]
fn production_needs_both_tenants() {
    let workforce = [
        ("IC_ENTRA_TENANT_ID", "staff-tenant"),
        ("IC_ENTRA_AUDIENCE", "api://portal"),
        ("IC_ENTRA_CLIENT_ID", "spa-staff"),
    ];
    let customers = [
        ("IC_CUSTOMER_TENANT_ID", "cust-tenant"),
        ("IC_CUSTOMER_AUDIENCE", "api://app"),
        ("IC_CUSTOMER_AUTHORITY", "https://contoso.ciamlogin.com/"),
        ("IC_CUSTOMER_CLIENT_ID", "spa-app"),
        ("IC_CUSTOMER_SCOPE", "api://app/portal"),
    ];
    let base = [NODES, ("IC_CLUSTER_TOKEN_FILE", "/token")];
    let all: Vec<(&str, &str)> = base.iter().chain(&workforce).chain(&customers).copied().collect();
    let s = load(&all).unwrap();
    assert_eq!(
        s.customers,
        Identity::Entra {
            authority: "https://contoso.ciamlogin.com/".into(),
            tenant: "cust-tenant".into(),
            audience: "api://app".into()
        }
    );
    assert!(matches!(&s.workforce, Identity::Entra { authority, .. } if authority == "https://login.microsoftonline.com"));
    assert!(matches!(s.workforce.auth(), ic_api::auth::Auth::Entra(_)));
    assert!(!s.cluster_auth.is_open());

    // The pages sign in through each tenant's single-page application.
    let UiConfig::Entra { customers, workforce: staff } = &s.ui else {
        panic!("entra sign-in expected")
    };
    assert_eq!(
        (customers.authority.as_str(), customers.client_id.as_str(), customers.scope.as_str()),
        ("https://contoso.ciamlogin.com", "spa-app", "api://app/portal")
    );
    assert_eq!(
        (staff.client_id.as_str(), staff.scope.as_str(), staff.tenant.as_str()),
        ("spa-staff", "api://portal/.default", "staff-tenant")
    );

    let without = |drop: &str| all.iter().filter(|(k, _)| *k != drop).copied().collect::<Vec<_>>();
    assert!(load(&without("IC_CUSTOMER_AUTHORITY")).unwrap_err().contains("IC_CUSTOMER_AUTHORITY"));
    assert!(load(&without("IC_CUSTOMER_TENANT_ID")).unwrap_err().contains("IC_CUSTOMER_TENANT_ID"));
    assert!(load(&without("IC_ENTRA_AUDIENCE")).unwrap_err().contains("IC_ENTRA_AUDIENCE"));
    assert!(load(&without("IC_ENTRA_CLIENT_ID")).unwrap_err().contains("IC_ENTRA_CLIENT_ID"));
    let mut odd = without("IC_CUSTOMER_AUTHORITY");
    odd.push(("IC_CUSTOMER_AUTHORITY", "contoso; script-src *"));
    assert!(load(&odd).unwrap_err().contains("web address"));
    assert!(load(&without("IC_CLUSTER_TOKEN_FILE")).unwrap_err().contains("IC_CLUSTER_TOKEN"));
    let mut explicit = all.clone();
    explicit.push(("IC_AUTH", "entra"));
    explicit.push(("IC_ENTRA_AUTHORITY", "https://login.microsoftonline.us"));
    assert!(matches!(load(&explicit).unwrap().workforce, Identity::Entra { authority, .. } if authority.ends_with(".us")));
}

/// Purpose: listen address, nodes, timings and the region map are each checked at start.
/// Why it matters: a service that starts on a bad configuration fails later and less clearly, usually during an incident.
#[test]
fn bad_values_are_refused_at_start() {
    assert!(load(&[OPEN, DEV]).unwrap_err().contains("IC_NODES"));
    assert!(load(&[("IC_NODES", " , "), OPEN, DEV]).unwrap_err().contains("IC_NODES"));
    assert!(load(&[NODES, OPEN, DEV, ("IC_PORTAL_LISTEN", "nowhere")])
        .unwrap_err()
        .contains("IC_PORTAL_LISTEN"));
    assert!(load(&[NODES, OPEN, DEV, ("IC_PORTAL_CURRENCY", "dollars")])
        .unwrap_err()
        .contains("IC_PORTAL_CURRENCY"));
    assert_eq!(
        load(&[NODES, OPEN, DEV, ("IC_PORTAL_CURRENCY", "AUD"), ("IC_PORTAL_FORWARD_SECS", "3")])
            .unwrap()
            .forward,
        Duration::from_secs(3)
    );
    assert_eq!(load(&[NODES, OPEN, DEV]).unwrap().partitions, 64);
    assert_eq!(load(&[NODES, OPEN, DEV, ("IC_PARTITIONS", "8")]).unwrap().partitions, 8);
    for bad in ["0", "65", "many"] {
        assert!(
            load(&[NODES, OPEN, DEV, ("IC_PARTITIONS", bad)]).unwrap_err().contains("IC_PARTITIONS"),
            "{bad}"
        );
    }
    // The customer master: off unless both are set, and never over plain HTTP to another host.
    assert_eq!(load(&[NODES, OPEN, DEV]).unwrap().master, None);
    let scope = ("IC_CUSTOMERS_SCOPE", "api://ic-customers/.default");
    for url in [
        "https://customers.internal",
        "http://localhost:8090",
        "http://ic-customers",
        "http://ic-customers.insurance.svc/",
        "http://ic-customers.ns.svc.cluster.local:80",
    ] {
        assert_eq!(
            load(&[NODES, OPEN, DEV, ("IC_CUSTOMERS_URL", url), scope]).unwrap().master,
            Some((url.to_string(), scope.1.to_string()))
        );
    }
    for url in [
        "http://customers.internal",
        "http://localhost.example.com",
        "http://svc.example.com",
        "http://",
        "ftp://ic-customers",
    ] {
        assert!(
            load(&[NODES, OPEN, DEV, ("IC_CUSTOMERS_URL", url), scope]).unwrap_err().contains("https"),
            "{url}"
        );
    }
    assert!(load(&[NODES, OPEN, DEV, scope]).unwrap_err().contains("together"));
    // So is the documents service.
    assert_eq!(
        load(&[
            NODES,
            OPEN,
            DEV,
            ("IC_DOCUMENTS_URL", "http://ic-documents"),
            ("IC_DOCUMENTS_SCOPE", "api://d/.default")
        ])
        .unwrap()
        .documents,
        Some(("http://ic-documents".to_string(), "api://d/.default".to_string()))
    );
    // The underwriting intake is configured the same way.
    let intake = ("IC_UNDERWRITING_INTAKE_SCOPE", "api://ic-underwriting-intake/.default");
    assert_eq!(load(&[NODES, OPEN, DEV]).unwrap().intake, None);
    assert_eq!(
        load(&[NODES, OPEN, DEV, ("IC_UNDERWRITING_INTAKE_URL", "https://intake.internal"), intake])
            .unwrap()
            .intake,
        Some(("https://intake.internal".to_string(), intake.1.to_string()))
    );
    assert!(load(&[NODES, OPEN, DEV, intake])
        .unwrap_err()
        .contains("IC_UNDERWRITING_INTAKE_URL and IC_UNDERWRITING_INTAKE_SCOPE together"));
    assert!(load(&[NODES, OPEN, DEV, ("IC_CUSTOMERS_URL", "https://customers.internal")])
        .unwrap_err()
        .contains("together"));
    assert!(load(&[NODES, OPEN, DEV, ("IC_LEDGER_PATIENCE_SECS", "soon")])
        .unwrap_err()
        .contains("IC_LEDGER_PATIENCE_SECS"));
    assert!(load(&[NODES, OPEN, DEV, ("IC_PORTAL_HEARTBEAT_SECS", "0")]).unwrap_err().contains("at least 1"));
    let s = load(&[
        NODES,
        OPEN,
        DEV,
        ("IC_PORTAL_LISTEN", "127.0.0.1:9000"),
        ("IC_LEDGER_PATIENCE_SECS", "3"),
        ("IC_PORTAL_HEARTBEAT_SECS", "5"),
    ])
    .unwrap();
    assert_eq!((s.listen.port(), s.patience.as_secs(), s.heartbeat.as_secs()), (9000, 3, 5));

    let regions = ("IC_REGIONS", "auckland=n1:7000;sydney=n2:7000,n3:7000");
    let s = load(&[NODES, OPEN, DEV, regions, ("IC_HOME_REGION", "auckland")]).unwrap();
    assert_eq!((s.regions.home.as_str(), s.regions.nodes.len()), ("auckland", 2));
    assert!(load(&[NODES, OPEN, DEV, regions]).unwrap_err().contains("together"));
    assert!(load(&[NODES, OPEN, DEV, ("IC_HOME_REGION", "auckland")]).unwrap_err().contains("together"));
    assert!(load(&[NODES, OPEN, DEV, regions, ("IC_HOME_REGION", "perth")])
        .unwrap_err()
        .contains("IC_REGIONS"));
    assert!(load(&[NODES, OPEN, DEV, ("IC_REGIONS", "auckland=other:1"), ("IC_HOME_REGION", "auckland")])
        .unwrap_err()
        .contains("not one of IC_NODES"));
}
