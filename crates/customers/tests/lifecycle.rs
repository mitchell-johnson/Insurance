//! Tamper evidence, erasure, the retention sweep, publishing and the
//! service's own start, over a real PostgreSQL database.

mod common;

use std::time::Duration;

use axum::http::StatusCode;
use chrono::{TimeZone, Utc};
use common::world;
use ic_customers::model::{PartyId, PartyKind};
use ic_customers::party::{Change, FactInput};
use ic_customers::store::{StoreError, Who};
use ic_customers::work::{self, Hubs};
use serde_json::json;

macro_rules! world_or_skip {
    () => {
        match world().await {
            Some(w) => w,
            None => return,
        }
    };
}

fn staff() -> Who {
    Who {
        caller: "staff".into(),
        actor: "u1".into(),
    }
}

/// Purpose: every party's hash chain verifies, the stored payloads are ciphertext, and the
/// database refuses to change or delete an event or an audit line, even for someone with
/// direct access.
/// Why it matters: the record must be tamper evident; an edit to history would otherwise
/// be invisible, and the audit trail is evidence.
#[tokio::test]
async fn history_is_tamper_evident() {
    let w = world_or_skip!();
    let p = w.person("Nikau", "Pene", "1988-08-08", &[("email", json!("n@x.nz"))]).await;
    let (status, out) = w.post(&format!("/v1/admin/verify/{p}"), "operations", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["links"], 2);
    let (status, _) = w.post(&format!("/v1/admin/verify/{p}"), "staff", json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = w.post("/v1/admin/verify/pty_none", "operations", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // No personal value is stored in the clear.
    let c = w.db.connector.connect().await.unwrap();
    let rows = c.query("select ciphertext from party_events where party_id = $1", &[&p]).await.unwrap();
    for r in rows {
        let ct: Vec<u8> = r.get(0);
        assert!(!String::from_utf8_lossy(&ct).contains("Pene"));
    }
    for statement in [
        format!("update party_events set kind = 'x' where party_id = '{p}'"),
        format!("update party_events set ciphertext = '\\x00' where party_id = '{p}'"),
        format!("delete from party_events where party_id = '{p}'"),
        "update audit set decision = 'deny'".to_string(),
        "delete from audit".to_string(),
    ] {
        assert!(w.sql(&statement).await.is_err(), "{statement}");
    }
    // A chain whose head was altered no longer verifies.
    w.sql(&format!("update parties set head_hash = '\\x00' where id = '{p}'")).await.unwrap();
    let (status, _) = w.post(&format!("/v1/admin/verify/{p}"), "operations", json!({})).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    // A link altered underneath the trigger, as by a restore of a bad
    // backup, is found too.
    w.sql("alter table party_events disable trigger party_events_guard").await.unwrap();
    w.sql(&format!("update party_events set kind = 'tampered' where party_id = '{p}' and seq = 1"))
        .await
        .unwrap();
    assert!(matches!(w.app.store.verify_chain(&PartyId(p.clone())).await, Err(StoreError::Integrity(_))));
    // A payload changed in storage fails to open, and the read says so.
    w.sql(&format!("update party_events set ciphertext = '\\x0102' where party_id = '{p}' and seq = 2"))
        .await
        .unwrap();
    let (status, _) = w.get(&format!("/v1/parties/{p}"), "staff", "servicing").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    // A payload missing while the key is still there is an integrity fault.
    w.sql(&format!("update party_events set ciphertext = null where party_id = '{p}' and seq = 2"))
        .await
        .unwrap();
    assert!(matches!(w.app.store.load(&PartyId(p.clone())).await, Err(StoreError::Integrity(_))));
    // A row whose version disagrees with its stream is refused.
    let q = w.person("Other", "Person", "1970-01-01", &[]).await;
    w.sql(&format!("update parties set version = 9 where id = '{q}'")).await.unwrap();
    assert!(matches!(w.app.store.load(&PartyId(q.clone())).await, Err(StoreError::Integrity(_))));
    // A relationship row the service cannot read is refused, not guessed at.
    let r = w.person("Rel", "Row", "1970-01-01", &[]).await;
    w.command(
        &r,
        "staff",
        json!({ "command": "add_relationship", "id": "o", "kind": "owner", "target": { "kind": "policy", "id": "PX" }, "valid_from": "2026-01-01" }),
    )
    .await;
    w.sql(&format!("update relationships set kind = 'alien', attributes = '[]' where party_id = '{r}'"))
        .await
        .unwrap();
    assert!(w.app.store.relationships_of(&PartyId(r)).await.is_err());
    // A merge loop is reported rather than followed for ever.
    let (a, b) = (w.person("A", "Loop", "1970-01-01", &[]).await, w.person("B", "Loop", "1970-01-01", &[]).await);
    w.sql(&format!("update parties set merged_into = '{b}' where id = '{a}'")).await.unwrap();
    w.sql(&format!("update parties set merged_into = '{a}' where id = '{b}'")).await.unwrap();
    assert!(matches!(w.app.store.resolve(&PartyId(a)).await, Err(StoreError::Integrity(_))));
}

/// Purpose: erasing a party destroys its key, purges its indexes and relationships, blanks
/// its payloads and leaves a tombstone whose chain still verifies; it is then gone from
/// reads, matching and commands, and anyone merged into it goes with it.
/// Why it matters: this is the right to be forgotten; what is left must be unreadable by
/// anyone while the record of what happened stays tamper evident.
#[tokio::test]
async fn erasure_destroys_the_key_and_keeps_the_chain() {
    let w = world_or_skip!();
    let store = &w.app.store;
    let p = w.person("Erin", "Gone", "1940-04-04", &[("ird_number", json!("49091850"))]).await;
    let dup = w.person("Erin", "Gone", "1940-04-04", &[]).await;
    let (status, _) = w.post("/v1/merges", "staff", json!({ "survivor": p, "absorbed": dup })).await;
    assert_eq!(status, StatusCode::OK);
    let id = PartyId(p.clone());
    // A hold stops erasure; so does a check that no longer holds.
    store
        .change(&id, &Change::PlaceHold { reference: "LH-1".into() }, None, &staff(), None, Utc::now())
        .await
        .unwrap();
    assert!(!store.shred(&id, |_| true, Utc::now()).await.unwrap());
    store
        .change(&id, &Change::ReleaseHold { reference: "LH-1".into() }, None, &staff(), None, Utc::now())
        .await
        .unwrap();
    assert!(!store.shred(&id, |_| false, Utc::now()).await.unwrap());
    assert!(!store.shred(&PartyId("pty_none".into()), |_| true, Utc::now()).await.unwrap());
    assert!(!store.shred(&PartyId(dup.clone()), |_| true, Utc::now()).await.unwrap());
    assert!(store.shred(&id, |_| true, Utc::now()).await.unwrap());
    assert!(!store.shred(&id, |_| true, Utc::now()).await.unwrap());
    let c = w.db.connector.connect().await.unwrap();
    for (table, column) in [("party_keys", "party_id"), ("blind_index", "party_id"), ("relationships", "party_id")] {
        let n: i64 = c
            .query_one(
                &format!("select count(*) from {table} where {column} = any($1)"),
                &[&vec![p.clone(), dup.clone()]],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(n, 0, "{table}");
    }
    let left: i64 = c
        .query_one("select count(*) from party_events where party_id = $1 and ciphertext is not null", &[&p])
        .await
        .unwrap()
        .get(0);
    assert_eq!(left, 0);
    assert_eq!(store.verify_chain(&id).await.unwrap(), 6);
    let erased = store.load(&id).await.unwrap().unwrap();
    assert!(erased.shredded && erased.kind == Some(PartyKind::Person));
    let (status, _) = w.get(&format!("/v1/parties/{p}"), "staff", "servicing").await;
    assert_eq!(status, StatusCode::GONE);
    let (status, _) = w.get(&format!("/v1/parties/{dup}"), "staff", "servicing").await;
    assert_eq!(status, StatusCode::GONE);
    let (status, _) = w.command(&p, "staff", json!({ "command": "activate" })).await;
    assert_eq!(status, StatusCode::GONE);
    let (_, m) = w
        .post(
            "/v1/parties/match",
            "claims",
            json!({ "source": "claims", "family_name": "Gone", "national_id": "49091850", "date_of_birth": "1940-04-04" }),
        )
        .await;
    assert_eq!(m["outcome"], "none");
    // The tombstone went out on the priority channel.
    let outbox = store.outbox(100).await.unwrap();
    assert!(outbox.iter().any(|(_, e)| e.kind == "shredded" && e.priority && e.party == id));
    // An erased party's export says only that it was erased.
    let records = store.records(&id).await.unwrap().unwrap();
    assert_eq!(records.len(), 1);
    // Merging into or out of an erased party is refused.
    let x = w.person("X", "Alive", "1970-01-01", &[]).await;
    let (status, _) = w.post("/v1/merges", "staff", json!({ "survivor": x, "absorbed": p })).await;
    assert_eq!(status, StatusCode::GONE);
}

/// Purpose: the sweep erases only parties whose retention has run out, writes them to the
/// register first, and after a restore brings an erased key back, erasing again from the
/// register removes it.
/// Why it matters: retention is enforced by code, not by someone remembering; and a
/// database restore must never undo an erasure.
#[tokio::test]
async fn the_sweep_erases_what_has_expired_and_a_restore_cannot_undo_it() {
    let w = world_or_skip!();
    let store = &w.app.store;
    let who = staff();
    let long_ago = Utc.with_ymd_and_hms(2015, 1, 1, 0, 0, 0).unwrap();
    let facts = [FactInput {
        field: ic_customers::model::Field::FamilyName,
        value: json!("Old"),
        valid_from: None,
        valid_to: None,
    }];
    // A prospect from 2015: due. A customer whose policy ended in 2024: not yet.
    let old = store.create(PartyKind::Person, &facts, None, &who, None, &[], long_ago).await.unwrap().party;
    let kept = store.create(PartyKind::Person, &facts, None, &who, None, &[], long_ago).await.unwrap().party;
    store
        .change(
            &kept,
            &Change::AddRelationship {
                id: "o1".into(),
                kind: ic_customers::model::RelationshipKind::Owner,
                target: ic_customers::model::Target::Policy("P9".into()),
                valid_from: "2015-01-01".parse().unwrap(),
                valid_to: Some("2024-01-01".parse().unwrap()),
                attributes: Default::default(),
            },
            None,
            &who,
            None,
            long_ago,
        )
        .await
        .unwrap();
    let recent = w.person("New", "Person", "1990-01-01", &[]).await;
    assert_eq!(work::sweep(&w.app, 100).await.unwrap(), 1);
    assert!(store.load(&old).await.unwrap().unwrap().shredded);
    assert!(!store.load(&kept).await.unwrap().unwrap().shredded);
    assert!(!store.load(&PartyId(recent)).await.unwrap().unwrap().shredded);
    assert_eq!(work::sweep(&w.app, 100).await.unwrap(), 0);
    // A restore brings back the key row of the erased party.
    let c = w.db.connector.connect().await.unwrap();
    c.batch_execute(&format!(
        "update parties set shredded_at = null where id = '{old}'; insert into party_keys values ('{old}', 'k', 'w', now())"
    ))
    .await
    .unwrap();
    let (status, out) = w.post("/v1/admin/reshred", "operations", json!({ "since": "2020-01-01" })).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["listed"], 1);
    let n: i64 = c
        .query_one("select count(*) from party_keys where party_id = $1", &[&old.0])
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 0);
    let (status, _) = w.post("/v1/admin/reshred", "staff", json!({ "since": "2020-01-01" })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// Purpose: the outbox is published to the event hub and the priority hub, each party's
/// events to one partition, and nothing is published twice; while Event Hubs is down
/// nothing is marked published.
/// Why it matters: gateways and modules learn of changes only from these events; a lost or
/// out-of-order revocation would leave access in place.
#[tokio::test]
async fn the_outbox_is_published_once() {
    let w = world_or_skip!();
    let credential = ic_azure::credential::AzureCredential::static_token(ic_azure::testing::TOKEN);
    let hubs = Hubs {
        events: ic_azure::eventhubs::EventHubsSink::new(&w.azure.base, "customer-events", credential.clone()),
        priority: ic_azure::eventhubs::EventHubsSink::new(&w.azure.base, "customer-priority", credential),
        partitions: 4,
    };
    let p = w.person("Pub", "Lish", "1970-01-01", &[]).await;
    w.command(&p, "staff", json!({ "command": "set_consent", "purpose": "marketing", "granted": false }))
        .await;
    w.azure.set_down(true);
    assert!(work::publish(&w.app, &hubs).await.is_err());
    assert_eq!(w.app.store.outbox(10).await.unwrap().len(), 3);
    w.azure.set_down(false);
    assert_eq!(work::publish(&w.app, &hubs).await.unwrap(), 3);
    assert_eq!(work::publish(&w.app, &hubs).await.unwrap(), 0);
    let part = work::partition_of(&PartyId(p), 4);
    assert_eq!(w.azure.events_sent("customer-events", part), 2);
    assert_eq!(w.azure.events_sent("customer-priority", part), 1);
    assert_eq!(work::partition_of(&PartyId("x".into()), 0), 0);
}

/// Purpose: when Key Vault is unavailable, creating and reading parties answer 503 and
/// write nothing; when it returns, the same requests succeed.
/// Why it matters: no personal data may ever be written unencrypted or under a key that
/// was not wrapped, and callers must be told to retry rather than be given an error that
/// looks final.
#[tokio::test]
async fn key_vault_outages_are_retryable() {
    let w = world_or_skip!();
    let p = w.person("Kei", "Vault", "1970-01-01", &[]).await;
    w.azure.set_down(true);
    let body = json!({ "kind": "person", "facts": [{ "field": "family_name", "value": "Later" }] });
    let (status, _) = w.post("/v1/parties", "staff", body.clone()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    // Within the cache window a read still works; past it, it would not.
    let (status, _) = w.get(&format!("/v1/parties/{p}"), "staff", "servicing").await;
    assert_eq!(status, StatusCode::OK);
    w.azure.set_down(false);
    let (status, _) = w.post("/v1/parties", "staff", body).await;
    assert_eq!(status, StatusCode::CREATED);
    // Rewrapping every key under the current key version.
    let (status, out) = w.post("/v1/admin/rewrap", "operations", json!({ "current_key_id": "none-yet" })).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["rewrapped"], 2);
    w.azure.set_down(true);
    let (status, _) = w.post("/v1/admin/rewrap", "operations", json!({ "current_key_id": "none-yet" })).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let (status, _) = w.post("/v1/admin/rewrap", "staff", json!({ "current_key_id": "x" })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// Purpose: when the database is unreachable the service answers 503, and readiness fails.
/// Why it matters: an orchestrator must stop routing to a replica that cannot reach its
/// database, and callers must retry rather than treat the party as missing.
#[tokio::test]
async fn database_outages_are_retryable() {
    let w = world_or_skip!();
    let p = w.person("Data", "Base", "1970-01-01", &[]).await;
    let c = w.db.connector.connect().await.unwrap();
    // End every connection to the test database and refuse new ones.
    let name: String = c.query_one("select current_database()", &[]).await.unwrap().get(0);
    let admin = common::server().unwrap().connect().await.unwrap();
    admin
        .batch_execute(&format!(
            "alter database {name} allow_connections false; select pg_terminate_backend(pid) from pg_stat_activity where datname = '{name}' and pid <> pg_backend_pid();"
        ))
        .await
        .unwrap();
    let (status, _) = w.get(&format!("/v1/parties/{p}"), "staff", "servicing").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let (status, _) = w.get("/readyz", "staff", "x").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    admin.batch_execute(&format!("alter database {name} allow_connections true")).await.unwrap();
    let (status, _) = w.get(&format!("/v1/parties/{p}"), "staff", "servicing").await;
    assert_eq!(status, StatusCode::OK);
}

/// Purpose: the service starts from its settings against a local database and the Azure
/// stand-in, serves requests on its port, runs its background work, and stops cleanly when
/// told to.
/// Why it matters: this is the code path the deployed process runs; a mistake in wiring
/// would otherwise only show in Azure.
#[tokio::test]
async fn the_service_starts_and_stops() {
    let Some(server) = common::server() else { return };
    let db = common::Database::new(server).await;
    let azure = ic_azure::testing::FakeAzure::start().await;
    let authority = common::tenant().await;
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let url = std::env::var("IC_TEST_POSTGRES_URL").unwrap();
    let dbname: String = db
        .connector
        .connect()
        .await
        .unwrap()
        .query_one("select current_database()", &[])
        .await
        .unwrap()
        .get(0);
    let env = std::collections::BTreeMap::from([
        ("IC_CUSTOMERS_DATABASE_URL", format!("{url} dbname={dbname}")),
        ("IC_CUSTOMERS_KEY_VAULT", azure.base.clone()),
        ("IC_CUSTOMERS_INDEX_KEY", common::INDEX_KEY.to_string()),
        ("IC_CUSTOMERS_TENANT", common::TENANT.to_string()),
        ("IC_CUSTOMERS_AUDIENCE", common::AUDIENCE.to_string()),
        ("IC_CUSTOMERS_AUTHORITY", authority.clone()),
        ("IC_EVENT_HUBS_NAMESPACE", azure.base.clone()),
        ("IC_CUSTOMERS_REGISTER_ENDPOINT", azure.base.clone()),
        ("IC_CUSTOMERS_LISTEN", format!("127.0.0.1:{port}")),
        ("IC_CUSTOMERS_CYCLE_SECONDS", "1".to_string()),
    ]);
    let config = ic_customers::config::Config::from_lookup(|k| env.get(k).cloned(), |_| Err(std::io::Error::other("none"))).unwrap();
    let credential = ic_azure::credential::AzureCredential::static_token(ic_azure::testing::TOKEN);
    let (stopping, stop) = tokio::sync::watch::channel(false);
    let running = tokio::spawn(work::start(config, credential, stop));
    let http = reqwest::Client::new();
    let token = common::token_with(&authority, &["staff"], common::AUDIENCE);
    let mut created = None;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let resp = http
            .post(format!("http://127.0.0.1:{port}/v1/parties"))
            .bearer_auth(&token)
            .json(&json!({ "kind": "person", "facts": [{ "field": "family_name", "value": "Started" }] }))
            .send()
            .await;
        if let Ok(r) = resp {
            created = Some(r.status());
            break;
        }
    }
    assert_eq!(created, Some(reqwest::StatusCode::CREATED));
    // The background work publishes the new party's events.
    let mut published = false;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if azure.seen().iter().any(|l| l.starts_with("send customer-events/")) {
            published = true;
            break;
        }
    }
    assert!(published, "{:?}", azure.seen());
    stopping.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(10), running).await.unwrap().unwrap().unwrap();
    // A port in use stops the start with a reason.
    let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut env = env;
    env.insert("IC_CUSTOMERS_LISTEN", busy.local_addr().unwrap().to_string());
    let config = ic_customers::config::Config::from_lookup(|k| env.get(k).cloned(), |_| Err(std::io::Error::other("none"))).unwrap();
    let (_s, stop) = tokio::sync::watch::channel(false);
    let e = work::start(config, ic_azure::credential::AzureCredential::static_token("t"), stop)
        .await
        .unwrap_err();
    assert!(e.contains("cannot listen"));
}

/// Purpose: the background loop keeps going when publishing or the sweep fails, and stops
/// when told to; a service configured for an Azure database that does not offer TLS
/// refuses to start.
/// Why it matters: an Event Hubs or database blip must not end the loop that relays every
/// change to the gateways, and a deployment must never fall back to an unencrypted
/// database connection.
#[tokio::test]
async fn background_work_survives_faults() {
    let w = world_or_skip!();
    let credential = ic_azure::credential::AzureCredential::static_token(ic_azure::testing::TOKEN);
    let hubs = Hubs {
        events: ic_azure::eventhubs::EventHubsSink::new(&w.azure.base, "customer-events", credential.clone()),
        priority: ic_azure::eventhubs::EventHubsSink::new(&w.azure.base, "customer-priority", credential.clone()),
        partitions: 2,
    };
    w.person("Lo", "Op", "1970-01-01", &[]).await;
    let pass = |every: u64| {
        work::run(
            &w.app,
            &hubs,
            Duration::from_secs(every),
            Duration::ZERO,
            tokio::time::sleep(Duration::from_millis(300)),
        )
    };
    w.azure.set_down(true);
    pass(3600).await;
    assert!(!w.app.store.outbox(10).await.unwrap().is_empty(), "nothing was published");
    w.sql("alter table parties rename to parties_gone").await.unwrap();
    pass(3600).await;
    w.sql("alter table parties_gone rename to parties").await.unwrap();
    w.azure.set_down(false);
    pass(3600).await;
    assert!(w.app.store.outbox(10).await.unwrap().is_empty(), "published once Event Hubs returned");
    // An Azure database is reached only over TLS.
    let env = std::collections::BTreeMap::from([
        ("IC_CUSTOMERS_PG_HOST", "127.0.0.1".to_string()),
        ("IC_CUSTOMERS_PG_USER", "postgres".to_string()),
        ("IC_CUSTOMERS_KEY_VAULT", w.azure.base.clone()),
        ("IC_CUSTOMERS_INDEX_KEY", common::INDEX_KEY.to_string()),
        ("IC_CUSTOMERS_TENANT", common::TENANT.to_string()),
        ("IC_CUSTOMERS_AUDIENCE", common::AUDIENCE.to_string()),
        ("IC_EVENT_HUBS_NAMESPACE", w.azure.base.clone()),
        ("IC_CUSTOMERS_REGISTER_ENDPOINT", w.azure.base.clone()),
    ]);
    let config = ic_customers::config::Config::from_lookup(|k| env.get(k).cloned(), |_| Err(std::io::Error::other("none"))).unwrap();
    let (_s, stop) = tokio::sync::watch::channel(false);
    let e = work::start(config, credential, stop).await.unwrap_err();
    assert!(e.contains("cannot connect to the database"), "{e}");
}

/// Purpose: two creations with the same idempotency key, reaching the store together, make
/// one party; the second gets the first's answer.
/// Why it matters: the HTTP layer checks for a remembered answer first, but two replicas can
/// pass that check at once; the store must still not create the person twice.
#[tokio::test]
async fn a_repeated_creation_makes_one_party() {
    let w = world_or_skip!();
    let idem = ic_customers::store::Idempotency {
        caller: "staff".into(),
        key: "k-1".into(),
        request: b"same".to_vec(),
    };
    let facts = [FactInput {
        field: ic_customers::model::Field::FamilyName,
        value: json!("Twice"),
        valid_from: None,
        valid_to: None,
    }];
    let now = Utc::now();
    let first = w
        .app
        .store
        .create(PartyKind::Person, &facts, None, &staff(), Some(&idem), &[], now)
        .await
        .unwrap();
    let second = w
        .app
        .store
        .create(PartyKind::Person, &facts, None, &staff(), Some(&idem), &[], now)
        .await
        .unwrap();
    assert_eq!(first.party, second.party);
}
