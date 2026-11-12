//! The driver's Azure ports against stand-ins that speak Blob Storage's
//! protocol, and the process settings.

use std::collections::HashMap;

use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::routing::get;
use axum::Router;
use ic_azure::blob::{BlobContainer, WormStore};
use ic_azure::credential::AzureCredential;
use ic_azure::testing::{FakeAzure, TOKEN};
use ic_pricing::adapters::{parse_list, BlobInbox, BlobOutbound};
use ic_pricing::driver::{Inbox, Outbound, Scan};
use ic_pricing::service::{run, Config};

/// Purpose: a List Blobs page gives its names, unescaped, and the marker of
/// the next page, with an empty marker meaning the last page.
/// Why it matters: a name read wrongly is a file never taken, and a marker
/// missed is every file after the first page never seen.
#[test]
fn list_pages_are_read() {
    let xml = "<EnumerationResults><Blobs><Blob><Name>valuations/2026-11-03/a&amp;b.csv</Name></Blob>\
               <Blob><Name>valuations/2026-11-03/c.csv</Name></Blob></Blobs><NextMarker>m2</NextMarker></EnumerationResults>";
    let (names, marker) = parse_list(xml);
    assert_eq!(names, vec!["valuations/2026-11-03/a&b.csv", "valuations/2026-11-03/c.csv"]);
    assert_eq!(marker.as_deref(), Some("m2"));
    let (names, marker) = parse_list("<EnumerationResults><Blobs /><NextMarker /></EnumerationResults>");
    assert!(names.is_empty() && marker.is_none());
    assert_eq!(parse_list("<Name>cut").0, Vec::<String>::new());
}

async fn list_blobs(Path(container): Path<String>, Query(q): Query<HashMap<String, String>>) -> (StatusCode, String) {
    if container != "inbound" || q.get("comp").map(String::as_str) != Some("list") || q.get("restype").map(String::as_str) != Some("container") {
        return (StatusCode::BAD_REQUEST, "bad request".into());
    }
    let prefix = q.get("prefix").cloned().unwrap_or_default();
    if prefix.starts_with("fail/") {
        return (StatusCode::FORBIDDEN, "AuthorizationPermissionMismatch".into());
    }
    match q.get("marker") {
        None => (
            StatusCode::OK,
            format!("<EnumerationResults><Blobs><Blob><Name>{prefix}a.csv</Name></Blob></Blobs><NextMarker>p2</NextMarker></EnumerationResults>"),
        ),
        Some(_) => (
            StatusCode::OK,
            format!("<EnumerationResults><Blobs><Blob><Name>{prefix}b.csv</Name></Blob></Blobs><NextMarker /></EnumerationResults>"),
        ),
    }
}

/// Purpose: the inbox lists a prefix across every page, and a storage
/// refusal is an error naming the prefix.
/// Why it matters: listing is how the driver finds the administrator's
/// files; an error swallowed here would look like a day with no valuation.
#[tokio::test]
async fn inbox_lists_every_page() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, Router::new().route("/{container}", get(list_blobs))).await });
    let inbox = BlobInbox::new(&base, "inbound", AzureCredential::static_token(TOKEN));
    assert_eq!(
        inbox.list("valuations/2026-11-03/").await.unwrap(),
        vec!["valuations/2026-11-03/a.csv", "valuations/2026-11-03/b.csv"]
    );
    assert!(inbox.list("fail/").await.unwrap_err().contains("fail/"));
}

/// Purpose: the inbox reads the Defender scan verdict from the blob's tag
/// (none yet, clean, anything else) and reads the file; the outbound
/// container writes once and treats a second write of the same name as
/// done; storage being down is an error everywhere.
/// Why it matters: only a clean verdict may let a file be parsed, and an
/// extract written twice after a crash must not fail the pass forever.
#[tokio::test]
async fn scan_read_and_write_once() {
    let fake = FakeAzure::start().await;
    let credential = AzureCredential::static_token(TOKEN);
    let raw = BlobContainer::new(&fake.base, "inbound", credential.clone());
    raw.put_new("valuations/2026-11-03/a.csv", "text/csv", b"H".to_vec()).await.unwrap();
    let inbox = BlobInbox::new(&fake.base, "inbound", credential.clone());
    let name = "valuations/2026-11-03/a.csv";
    assert_eq!(inbox.scan(name).await.unwrap(), Scan::Pending);
    fake.set_blob_tags(&format!("inbound/{name}"), &[("Malware Scanning scan result", "No threats found")]);
    assert_eq!(inbox.scan(name).await.unwrap(), Scan::Clean);
    fake.set_blob_tags(&format!("inbound/{name}"), &[("Malware Scanning scan result", "Malicious")]);
    assert_eq!(inbox.scan(name).await.unwrap(), Scan::Malicious);
    assert_eq!(inbox.read(name).await.unwrap(), Some(b"H".to_vec()));
    assert_eq!(inbox.read("valuations/none.csv").await.unwrap(), None);

    let out = BlobOutbound(BlobContainer::new(&fake.base, "outbound", credential));
    out.put_new("extracts/growth/2026-11-03.csv", b"first".to_vec()).await.unwrap();
    out.put_new("extracts/growth/2026-11-03.csv", b"first".to_vec()).await.unwrap();
    assert_eq!(fake.blobs()["outbound/extracts/growth/2026-11-03.csv"], b"first".to_vec());

    fake.set_down(true);
    assert!(inbox.scan(name).await.is_err());
    assert!(inbox.read(name).await.is_err());
    assert!(out.put_new("extracts/growth/2026-11-04.csv", vec![]).await.is_err());
}

/// Purpose: the settings need the cluster and both storage endpoints, take
/// defaults for the rest, and refuse a cycle that is not a positive whole
/// number; the process stops at once when a setting is missing.
/// Why it matters: a driver started against the wrong container, or with a
/// zero cycle that spins, fails at start where the rollout sees it.
#[tokio::test]
async fn settings_are_read_from_the_environment() {
    let env = |pairs: &'static [(&'static str, &'static str)]| move |k: &str| pairs.iter().find(|(key, _)| *key == k).map(|(_, v)| v.to_string());
    let base: &'static [(&str, &str)] = &[
        ("IC_NODES", "n1:7000,n2:7000"),
        ("IC_UNIT_PRICING_INBOX_ENDPOINT", "https://stin.blob.core.windows.net"),
        ("IC_UNIT_PRICING_OUTBOUND_ENDPOINT", "https://stout.blob.core.windows.net"),
    ];
    let c = Config::from_lookup(env(base)).unwrap();
    assert_eq!(
        (c.inbox_container.as_str(), c.outbound_container.as_str()),
        ("unit-pricing-inbound", "unit-pricing-outbound")
    );
    assert_eq!(c.cycle_every.as_millis(), 30_000);
    let zero: &'static [(&str, &str)] = &[
        ("IC_NODES", "n1:7000"),
        ("IC_UNIT_PRICING_INBOX_ENDPOINT", "https://a"),
        ("IC_UNIT_PRICING_OUTBOUND_ENDPOINT", "https://b"),
        ("IC_UNIT_PRICING_CYCLE_MS", "0"),
    ];
    assert!(Config::from_lookup(env(zero)).unwrap_err().contains("IC_UNIT_PRICING_CYCLE_MS"));
    assert!(Config::from_lookup(env(&[])).unwrap_err().contains("IC_NODES"));
    let err = run(env(&[]), |_| Err(std::io::Error::other("no file"))).await.unwrap_err();
    assert!(err.contains("IC_NODES"));
}
