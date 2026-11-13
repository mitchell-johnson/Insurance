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
        // No cluster answers here: deaths would wait, and nothing else is held up.
        ("IC_CLUSTER_NODES", "127.0.0.1:9".to_string()),
        ("IC_CLUSTER_AUTH", "open".to_string()),
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
    let p = w.person("Lo", "Op", "1970-01-01", &[]).await;
    w.command(&p, "claims", json!({ "command": "mark_deceased", "date_of_death": "2026-10-01" }))
        .await;
    let core = common::FakeCore {
        partitions: 2,
        ..Default::default()
    };
    let pass = |every: u64| {
        work::run(
            &w.app,
            &hubs,
            Some(&core),
            Duration::from_secs(every),
            Duration::ZERO,
            tokio::time::sleep(Duration::from_millis(300)),
        )
    };
    w.azure.set_down(true);
    pass(3600).await;
    assert!(!w.app.store.outbox(10).await.unwrap().is_empty(), "nothing was published");
    w.sql("alter table parties rename to parties_gone").await.unwrap();
    w.sql("alter table death_relay rename to death_relay_gone").await.unwrap();
    w.sql("alter table marker_reviews rename to marker_reviews_gone").await.unwrap();
    w.sql("alter table directory_relay rename to directory_relay_gone").await.unwrap();
    pass(3600).await;
    assert_eq!(core.proposed.lock().unwrap().len(), 2, "told on the first pass, once");
    w.sql("alter table parties_gone rename to parties").await.unwrap();
    w.sql("alter table death_relay_gone rename to death_relay").await.unwrap();
    w.sql("alter table marker_reviews_gone rename to marker_reviews").await.unwrap();
    w.sql("alter table directory_relay_gone rename to directory_relay").await.unwrap();
    w.azure.set_down(false);
    pass(3600).await;
    assert!(w.app.store.outbox(10).await.unwrap().is_empty(), "published once Event Hubs returned");
    assert_eq!(core.proposed.lock().unwrap().len(), 2, "the death went to both partitions, once");
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

/// Purpose: every death recorded here is told to claims on every core partition, as
/// `RecordDeath` with a key per party, date and partition; a refusal or an unreachable
/// cluster keeps it with the reason for the next pass; a repeat writes nothing new; a
/// retraction goes the same way as `RetractDeath` for the date that was recorded, and
/// replaces a death not yet told; erasing the party removes what is waiting.
/// Why it matters: claims stops benefits in payment from the day after a death on
/// whichever partition each claim sits. A death that never reached one partition would
/// keep paying a dead person; a retraction that never reached one would leave a living
/// person's income stopped.
#[tokio::test]
async fn deaths_are_told_to_claims_on_every_partition() {
    let w = world_or_skip!();
    let core = common::FakeCore {
        partitions: 3,
        ..Default::default()
    };
    let p = w.person("Mate", "Tonu", "1940-01-01", &[]).await;
    let mark = |d: &str| json!({ "command": "mark_deceased", "date_of_death": d });
    // Recorded by staff, not claims: still told.
    w.command(&p, "staff", mark("2026-10-01")).await;
    w.command(&p, "staff", mark("2026-10-01")).await;
    *core.refuse.lock().unwrap() = Some(1);
    assert_eq!(work::relay_deaths(&w.app, &core).await.unwrap(), 0);
    let row = w
        .sql("update death_relay set attempts = attempts where attempts = 1 and last_error like 'partition 1 refused%'")
        .await
        .unwrap();
    assert_eq!(row, 1, "kept with the reason");
    *core.refuse.lock().unwrap() = None;
    *core.unreachable.lock().unwrap() = true;
    assert!(work::relay_deaths(&w.app, &core).await.is_err(), "no partitions, nothing tried");
    *core.unreachable.lock().unwrap() = false;
    *core.lost.lock().unwrap() = true;
    assert_eq!(work::relay_deaths(&w.app, &core).await.unwrap(), 0);
    *core.lost.lock().unwrap() = false;
    core.proposed.lock().unwrap().clear();
    assert_eq!(work::relay_deaths(&w.app, &core).await.unwrap(), 1);
    assert_eq!(work::relay_deaths(&w.app, &core).await.unwrap(), 0, "told once");
    let sent = core.proposed.lock().unwrap().clone();
    assert_eq!(sent.len(), 3);
    for (partition, (n, key, command)) in sent.iter().enumerate() {
        assert_eq!(usize::from(*n), partition);
        assert_eq!(key, &format!("death:{p}:2026-10-01:{partition}:2"), "the third attempt has its own key");
        assert_eq!(command, &json!({ "claims": { "record_death": { "party": p, "date_of_death": "2026-10-01" } } }));
    }
    // Retracted after it was told: the retraction follows, for the recorded date.
    let retract = json!({ "command": "retract_death", "reason": "wrong person" });
    w.command(&p, "claims", retract.clone()).await;
    core.proposed.lock().unwrap().clear();
    assert_eq!(work::relay_deaths(&w.app, &core).await.unwrap(), 1);
    let sent = core.proposed.lock().unwrap().clone();
    assert_eq!(sent.len(), 3);
    assert_eq!(sent[0].1, format!("retraction:{p}:2026-10-01:0:0"));
    assert_eq!(
        sent[0].2,
        json!({ "claims": { "retract_death": { "party": p, "date_of_death": "2026-10-01" } } })
    );
    // Retracted before the death was told: only the retraction goes, which claims
    // accepts where it never heard of the death.
    let q = w.person("Ora", "Tonu", "1941-01-01", &[]).await;
    w.command(&q, "staff", mark("2026-10-02")).await;
    w.command(&q, "staff", retract).await;
    core.proposed.lock().unwrap().clear();
    assert_eq!(work::relay_deaths(&w.app, &core).await.unwrap(), 1);
    let sent = core.proposed.lock().unwrap().clone();
    assert!(sent.iter().all(|(_, k, _)| k.starts_with(&format!("retraction:{q}:2026-10-02:"))), "{sent:?}");
    // Erased while waiting: nothing left to send.
    w.command(&q, "staff", mark("2026-10-03")).await;
    w.sql(&format!("delete from death_relay where party_id <> '{q}'")).await.unwrap();
    assert_eq!(w.sql("select 1 from death_relay").await.unwrap(), 1);
    w.app.store.reshred(&[PartyId(q.clone())], Utc::now()).await.unwrap();
    assert_eq!(w.sql("select 1 from death_relay").await.unwrap(), 0);
}

/// Purpose: when a vulnerability marker's review date comes, one prompt opens for a trained
/// person, and only once; a new marker with a later date starts again; removing the marker
/// or erasing the party leaves nothing due.
/// Why it matters: a marker kept past its time labels someone who may no longer need it,
/// and one never reviewed may miss what has changed (matrix VUL-017).
#[tokio::test]
async fn markers_are_reviewed_when_due() {
    let w = world_or_skip!();
    let p = w.person("Rewi", "Kara", "1950-05-05", &[]).await;
    let today = Utc::now().date_naive();
    let marker = |on: chrono::NaiveDate, reason: &str| {
        json!({ "command": "record", "reason": reason, "facts": [{ "field": "vulnerability",
            "value": { "kind": "life_event", "review_on": on.to_string(), "adjustments": ["more_time"] } }] })
    };
    let (status, out) = w.command(&p, "staff", marker(today, "first")).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(
        w.app.store.prompt_marker_reviews(today - chrono::Duration::days(1), Utc::now()).await.unwrap(),
        0,
        "not yet"
    );
    assert_eq!(w.app.store.prompt_marker_reviews(today, Utc::now()).await.unwrap(), 1);
    assert_eq!(w.app.store.prompt_marker_reviews(today, Utc::now()).await.unwrap(), 0, "once");
    let reviews = w.app.store.open_reviews(10).await.unwrap();
    assert!(reviews
        .iter()
        .any(|r| r.kind == "consider_marker" && r.reason.as_deref() == Some("review_date") && r.party.0 == p));
    // Renewed with a later date: due again then.
    let later = today + chrono::Duration::days(180);
    w.command(&p, "staff", marker(later, "renewed at review")).await;
    assert_eq!(w.app.store.prompt_marker_reviews(today, Utc::now()).await.unwrap(), 0);
    assert_eq!(w.app.store.prompt_marker_reviews(later, Utc::now()).await.unwrap(), 1);
    // Removed: nothing due.
    w.command(&p, "staff", marker(later, "renewed again")).await;
    let remove = json!({ "command": "record", "reason": "removed at review", "facts": [{ "field": "vulnerability", "value": false }] });
    let (status, out) = w.command(&p, "staff", remove).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(w.sql("select 1 from marker_reviews").await.unwrap(), 0);
    // Erased: nothing due.
    w.command(&p, "staff", marker(later, "marked again")).await;
    assert_eq!(w.sql("select 1 from marker_reviews").await.unwrap(), 1);
    w.app.store.reshred(&[PartyId(p.clone())], Utc::now()).await.unwrap();
    assert_eq!(w.sql("select 1 from marker_reviews").await.unwrap(), 0);
}

/// Purpose: relationship changes the channels directory needs (a policy's owner arriving
/// or leaving, an assignee arriving or leaving, a child with the date they control their
/// own health information, and that child's record ending) are told to it in order as module `customer-master`; other relationships
/// are not; a refusal or no answer keeps the change and holds up only that party's later
/// changes; erasing the party removes what is waiting.
/// Why it matters: the directory decides who sees which policy in the app. An assignee
/// told of leaving before arriving would keep access, a new owner never told would leave
/// the old one seeing the policy, and an ended owner or dependant not told would keep
/// their view.
#[tokio::test]
async fn relationship_changes_reach_the_channels_directory() {
    let w = world_or_skip!();
    let core = common::FakeCore::default();
    let owner = w.person("Ana", "Owner", "1970-01-01", &[]).await;
    let child = w.person("Tia", "Owner", "2015-01-01", &[]).await;
    let bank = w.person("Bank", "Assignee", "1960-01-01", &[]).await;
    let add = |id: &str, kind: &str, attributes: serde_json::Value| {
        json!({ "command": "add_relationship", "id": id, "kind": kind, "target": { "kind": "policy", "id": "P1" },
                "valid_from": "2026-01-01", "attributes": attributes })
    };
    w.command(&owner, "staff", add("own", "owner", json!({}))).await;
    w.command(&owner, "staff", add("adv", "adviser", json!({}))).await;
    w.command(&child, "staff", add("dep", "dependant", json!({ "controls_from": "2031-01-01" })))
        .await;
    w.command(&bank, "staff", add("asg", "assignee", json!({}))).await;
    let (status, out) = w
        .command(&bank, "staff", json!({ "command": "end_relationship", "id": "asg", "valid_to": "2026-06-01" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    // The bank's changes wait; everyone else's go.
    *core.refuse_party.lock().unwrap() = Some(bank.clone());
    assert_eq!(work::relay_directory(&w.app, &core).await.unwrap(), 2);
    assert_eq!(
        w.sql("select 1 from directory_relay where attempts = 1 and last_error like 'refused%'")
            .await
            .unwrap(),
        1
    );
    *core.refuse_party.lock().unwrap() = None;
    *core.lost.lock().unwrap() = true;
    assert_eq!(work::relay_directory(&w.app, &core).await.unwrap(), 0);
    *core.lost.lock().unwrap() = false;
    assert_eq!(work::relay_directory(&w.app, &core).await.unwrap(), 2);
    assert_eq!(work::relay_directory(&w.app, &core).await.unwrap(), 0, "told once");
    let told: Vec<serde_json::Value> = core.directed.lock().unwrap().iter().map(|(_, c)| c.clone()).collect();
    assert_eq!(
        told,
        vec![
            json!({ "record_ownership": { "policy": "P1", "policyholder": owner } }),
            json!({ "record_dependant": { "policy": "P1", "party": child, "controls_from": "2031-01-01" } }),
            json!({ "record_assignment": { "policy": "P1", "party": bank, "active": true } }),
            json!({ "record_assignment": { "policy": "P1", "party": bank, "active": false } }),
        ]
    );
    assert!(core.directed.lock().unwrap()[2].0.ends_with(":2"), "a new key per attempt");
    // The same relationship again changes nothing, so tells nothing.
    w.command(&child, "staff", add("dep", "dependant", json!({ "controls_from": "2031-01-01" })))
        .await;
    assert_eq!(
        w.sql("select 1 from directory_relay").await.unwrap(),
        0,
        "the same relationship again changes nothing"
    );
    // An owner and a dependant ending are told too.
    for (party, id) in [(&owner, "own"), (&child, "dep")] {
        let (status, out) = w
            .command(party, "staff", json!({ "command": "end_relationship", "id": id, "valid_to": "2026-07-01" }))
            .await;
        assert_eq!(status, StatusCode::OK, "{out}");
    }
    core.directed.lock().unwrap().clear();
    assert_eq!(work::relay_directory(&w.app, &core).await.unwrap(), 2);
    let told: Vec<serde_json::Value> = core.directed.lock().unwrap().iter().map(|(_, c)| c.clone()).collect();
    assert_eq!(
        told,
        vec![
            json!({ "end_ownership": { "policy": "P1", "party": owner } }),
            json!({ "end_dependant": { "policy": "P1", "party": child } }),
        ]
    );
    // Erased while waiting: nothing left to send.
    w.command(
        &owner,
        "staff",
        json!({ "command": "add_relationship", "id": "own2", "kind": "owner",
        "target": { "kind": "policy", "id": "P2" }, "valid_from": "2026-01-01" }),
    )
    .await;
    assert_eq!(w.sql("select 1 from directory_relay").await.unwrap(), 1);
    w.app.store.reshred(&[PartyId(owner.clone())], Utc::now()).await.unwrap();
    assert_eq!(w.sql("select 1 from directory_relay").await.unwrap(), 0);
}

/// Purpose: the blind index key can be changed without anyone becoming unfindable.
/// Entries made before keys were named are adopted as the current key's after one
/// party's are checked against it; with a next key every party is indexed again under
/// both, the status says when the next key covers everyone, and once it is promoted
/// lookups use only its entries and the old key's are removed. Older entries that do not
/// match the current key are not adopted; everyone is indexed afresh instead. An
/// operator reads the status at `/v1/admin/index`; other callers cannot.
/// Why it matters: the index key can leak or need rotating like any secret. Every
/// duplicate check and match depends on it, so a change that dropped entries early would
/// let a known person be created again or a claimant go unmatched.
#[tokio::test]
async fn the_index_key_can_be_changed_without_losing_anyone() {
    let w = world_or_skip!();
    let ana = PartyId(w.person("Ana", "Moana", "1980-02-03", &[("email", json!("ana@example.nz"))]).await);
    let ben = PartyId(w.person("Ben", "Tane", "1975-06-07", &[]).await);
    let next = "a-next-index-key-of-at-least-32-bytes!!";
    // Entries from before keys were named: adopted once, after a check.
    w.sql("update blind_index set key_id = ''").await.unwrap();
    w.sql("delete from index_state").await.unwrap();
    assert!(findable(&w.app.store, common::INDEX_KEY, &ana).await, "older entries still answer");
    assert!(w.app.store.adopt_index().await.unwrap());
    assert!(!w.app.store.adopt_index().await.unwrap(), "only once");
    let status = w.app.store.index_status().await.unwrap();
    assert_eq!((status.waiting_current, status.next.clone(), status.ready_to_promote), (0, None, false));

    // Changing: everyone is indexed under both keys, a few at a time.
    let changing = w.store_with(common::INDEX_KEY, Some(next)).await;
    let status = changing.index_status().await.unwrap();
    assert_eq!((status.waiting_current, status.waiting_next, status.ready_to_promote), (0, Some(2), false));
    assert_eq!(changing.rebuild_index(1).await.unwrap(), 1);
    assert_eq!(changing.rebuild_index(10).await.unwrap(), 1);
    assert_eq!(changing.rebuild_index(10).await.unwrap(), 0, "nothing left");
    assert!(changing.index_status().await.unwrap().ready_to_promote);
    assert!(findable(&changing, common::INDEX_KEY, &ana).await, "lookups still use the current key");
    assert!(!findable(&changing, next, &ana).await, "and not the next one yet");
    assert_eq!(changing.retire_index(100).await.unwrap(), 0, "nothing is retired while both are in use");

    // Promoted: only the new key's entries answer, and the old ones go.
    let promoted = w.store_with(next, None).await;
    assert!(findable(&promoted, next, &ana).await);
    let status = promoted.index_status().await.unwrap();
    assert_eq!((status.waiting_current, status.retired_entries > 0), (0, true));
    assert!(!findable(&promoted, common::INDEX_KEY, &ana).await, "the old key finds nobody");
    assert!(promoted.retire_index(100).await.unwrap() > 0);
    assert_eq!(promoted.index_status().await.unwrap().retired_entries, 0);
    assert_eq!(
        w.sql(&format!(
            "select 1 from index_state where key_id <> '{}'",
            promoted.index_status().await.unwrap().current
        ))
        .await
        .unwrap(),
        0
    );

    // Older entries that are not the current key's are not adopted; everyone is
    // indexed afresh, and older entries are kept until then.
    w.sql("update blind_index set key_id = ''").await.unwrap();
    w.sql("delete from index_state").await.unwrap();
    assert!(!w.app.store.adopt_index().await.unwrap());
    assert_eq!(w.app.store.retire_index(100).await.unwrap(), 0, "kept while anyone is waiting");
    assert_eq!(w.app.store.index_status().await.unwrap().waiting_current, 2);
    assert_eq!(work::maintain_index(&w.app).await.unwrap(), (2, 0));
    assert!(findable(&w.app.store, common::INDEX_KEY, &ben).await);
    assert!(findable(&w.app.store, common::INDEX_KEY, &ana).await);
    assert_eq!(w.app.store.index_status().await.unwrap().waiting_current, 0);
    // Erasing a party removes what it was indexed under.
    w.app.store.reshred(std::slice::from_ref(&ana), Utc::now()).await.unwrap();
    assert_eq!(w.sql(&format!("select 1 from index_state where party_id = '{}'", ana.0)).await.unwrap(), 0);

    // Only an operator reads the status.
    let (status, out) = w.get("/v1/admin/index", "operations", "operations").await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["waiting_current"], 0);
    assert_eq!(out["next"], serde_json::Value::Null);
    let (status, _) = w.get("/v1/admin/index", "staff", "operations").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// Whether the store finds a party by its own index entries under `key`.
async fn findable(store: &ic_customers::store::Store<ic_azure::keywrap::KeyVaultWrapper>, key: &str, who: &PartyId) -> bool {
    let key = ic_customers::crypto::IndexKey::new(key.as_bytes()).unwrap();
    let party = store.load(who).await.unwrap().unwrap();
    let keys: Vec<(&'static str, Vec<u8>)> = ic_customers::matching::party_keys(&party, &key).into_iter().collect();
    assert!(!keys.is_empty());
    store.candidates(&keys).await.unwrap().contains(who)
}

/// Purpose: a person's own hold on their money going out takes effect at once when they
/// place it in the app (or ask staff to); ending it needs a passed identity check made
/// after the hold and within a day, then waits out the cooling-off period, during which
/// they can change their mind; the sweep ends it only once the period has passed; staff
/// cannot ask for it to end, only legal can lift it, with a reference, and the
/// service's own ending cannot be sent by anyone. Payments reads where it stands.
/// Decision 3 in docs/decisions.md, persona row SVC-022.
/// Why it matters: a customer who knows they are at risk (gambling, a scam in progress,
/// a bad night) asks for this to protect themselves; if a scammer or a persuasive call
/// could lift it on the spot it would protect no one.
#[tokio::test]
async fn a_customers_own_spending_hold_ends_only_after_cooling_off() {
    let w = world_or_skip!();
    let me = w.person("Lin", "Chen", "1979-09-09", &[]).await;
    let app = |method: &'static str, body: Option<serde_json::Value>| common::Call {
        method,
        path: if method == "GET" {
            format!("/v1/parties/{me}/spending-hold")
        } else {
            format!("/v1/parties/{me}/commands")
        },
        role: "portal",
        purpose: Some("self_service"),
        body,
        headers: vec![("x-ic-party", me.clone()), ("x-ic-actor", "app-session-1".into())],
    };
    let status_of = || async {
        let (status, out) = w.get(&format!("/v1/parties/{me}/spending-hold"), "payments", "payments").await;
        assert_eq!(status, StatusCode::OK, "{out}");
        out
    };
    assert_eq!(status_of().await["held"], false);
    let (status, out) = w.send(app("POST", Some(json!({ "command": "place_spending_hold" })))).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["events"], json!(["spending_hold_placed"]));
    let held = status_of().await;
    assert_eq!((held["held"].clone(), held["removal"].clone()), (json!(true), serde_json::Value::Null));
    let (status, mine) = w.send(app("GET", None)).await;
    assert_eq!((status, mine["held"].clone()), (StatusCode::OK, json!(true)), "the person sees their own");
    // Staff may place it for the person, never end it.
    let (status, out) = w.command(&me, "staff", json!({ "command": "place_spending_hold" })).await;
    assert_eq!((status, out["events"].clone()), (StatusCode::OK, json!([])));
    for (role, command) in [
        ("staff", json!({ "command": "request_hold_removal", "verification": "ev-1" })),
        ("staff", json!({ "command": "override_spending_hold", "reference": "CIV-2026-1" })),
        ("migration", json!({ "command": "end_spending_hold" })),
        ("legal", json!({ "command": "end_spending_hold" })),
    ] {
        let (status, _) = w.command(&me, role, command.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{role} {command}");
    }
    // Ending it needs a fresh identity check made after the hold.
    let remove = |evidence: &str| app("POST", Some(json!({ "command": "request_hold_removal", "verification": evidence })));
    let (status, out) = w.send(remove("ev-1")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{out}");
    let verify = |evidence: &str, at: chrono::DateTime<Utc>| {
        json!({ "command": "record_verification", "method": "document_and_selfie", "provider": "idv",
                "outcome": "passed", "evidence_ref": evidence, "verified_at": at })
    };
    let (status, out) = w.command(&me, "identity", verify("ev-old", Utc::now() - chrono::Duration::days(2))).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(
        w.send(remove("ev-old")).await.0,
        StatusCode::UNPROCESSABLE_ENTITY,
        "too old, and before the hold"
    );
    w.command(&me, "identity", verify("ev-1", Utc::now())).await;
    let (status, out) = w.send(remove("ev-1")).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let waiting = status_of().await;
    assert_eq!(waiting["held"], true, "still held while cooling off");
    let requested: chrono::DateTime<Utc> = serde_json::from_value(waiting["removal"]["requested_at"].clone()).unwrap();
    let ends: chrono::DateTime<Utc> = serde_json::from_value(waiting["removal"]["ends_at"].clone()).unwrap();
    assert_eq!(ends - requested, chrono::Duration::days(7));
    // The person changes their mind, then asks again.
    let (_, out) = w.send(app("POST", Some(json!({ "command": "cancel_hold_removal" })))).await;
    assert_eq!(out["events"], json!(["spending_hold_removal_cancelled"]));
    assert_eq!(status_of().await["removal"], serde_json::Value::Null);
    let (status, _) = w.send(app("POST", Some(json!({ "command": "cancel_hold_removal" })))).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "nothing waiting");
    w.send(remove("ev-1")).await;
    // Placing it again while a removal waits cancels the removal.
    let (_, out) = w.send(app("POST", Some(json!({ "command": "place_spending_hold" })))).await;
    assert_eq!(out["events"], json!(["spending_hold_removal_cancelled"]));
    w.send(remove("ev-1")).await;
    // The sweep ends it only once the period has passed.
    assert_eq!(w.app.store.end_spending_holds(Utc::now() + chrono::Duration::days(6)).await.unwrap(), 0);
    assert_eq!(status_of().await["held"], true);
    assert_eq!(w.app.store.end_spending_holds(Utc::now() + chrono::Duration::days(8)).await.unwrap(), 1);
    assert_eq!(status_of().await["held"], false);
    assert_eq!(w.sql("select 1 from spending_hold_removals").await.unwrap(), 0);
    let (status, _) = w.send(remove("ev-1")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "no hold to remove");
    // Legal lifts it at once, with a reference.
    w.send(app("POST", Some(json!({ "command": "place_spending_hold" })))).await;
    let (status, _) = w
        .command(&me, "legal", json!({ "command": "override_spending_hold", "reference": "bad ref!" }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, out) = w
        .command(&me, "legal", json!({ "command": "override_spending_hold", "reference": "CIV-2026-1" }))
        .await;
    assert_eq!((status, out["events"].clone()), (StatusCode::OK, json!(["spending_hold_ended"])), "{out}");
    assert_eq!(status_of().await["held"], false);
    let (status, _) = w
        .command(&me, "legal", json!({ "command": "override_spending_hold", "reference": "CIV-2026-1" }))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "nothing to lift");
    // Somebody else's app session cannot read or touch it; documents cannot read it.
    let (status, _) = w.get(&format!("/v1/parties/{me}/spending-hold"), "documents", "notices").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Erasing the person clears anything waiting. A check made before this hold does
    // not end it.
    w.send(app("POST", Some(json!({ "command": "place_spending_hold" })))).await;
    assert_eq!(w.send(remove("ev-1")).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    w.command(&me, "identity", verify("ev-2", Utc::now())).await;
    assert_eq!(w.send(remove("ev-2")).await.0, StatusCode::OK);
    assert_eq!(w.sql("select 1 from spending_hold_removals").await.unwrap(), 1);
    w.app.store.reshred(&[PartyId(me.clone())], Utc::now()).await.unwrap();
    assert_eq!(w.sql("select 1 from spending_hold_removals").await.unwrap(), 0);
}

/// The hold changes the fake core took, as (partition, policy, held, reference).
fn holds_taken(core: &common::FakeCore) -> Vec<(u16, String, bool, String)> {
    let on = *core.policies_on.lock().unwrap();
    core.proposed
        .lock()
        .unwrap()
        .iter()
        .filter(|(p, _, _)| on.is_none_or(|on| on == *p))
        .filter_map(|(p, _, c)| match serde_json::from_value::<ic_core::Command>(c.clone()) {
            Ok(ic_core::Command::SetOwnerHold(h)) => Some((*p, h.policy.0.clone(), h.held, h.reference.clone())),
            _ => None,
        })
        .collect()
}

/// Purpose: a person's own spending hold reaches the core on every policy they own,
/// follows ownership while it stands, ends when legal lifts it, waits out a core that
/// cannot be reached, and gives up on a policy no partition ever holds.
/// Why it matters: the core is where a withdrawal or surrender is refused, so a hold
/// the core never hears of protects nothing until money reaches the bank, and a hold
/// left on a policy after it changes hands would stop the next owner's money.
#[tokio::test]
async fn a_spending_hold_is_told_to_the_core_policy_by_policy() {
    let w = world_or_skip!();
    let me = w.person("Ana", "Ruiz", "1968-04-04", &[]).await;
    let today = Utc::now().date_naive();
    let add = |id: &str, kind: &str, policy: &str, to: Option<&str>| {
        json!({ "command": "add_relationship", "id": id, "kind": kind, "target": { "kind": "policy", "id": policy },
                "valid_from": "2026-01-01", "valid_to": to })
    };
    for c in [
        add("o1", "owner", "P1", None),
        add("o2", "owner", "P2", Some("2099-01-01")),
        add("old", "owner", "P0", Some("2026-02-01")),
        add("adv", "adviser", "P3", None),
    ] {
        let (status, out) = w.command(&me, "staff", c).await;
        assert_eq!(status, StatusCode::OK, "{out}");
    }
    let core = common::FakeCore {
        partitions: 2,
        ..Default::default()
    };
    *core.policies_on.lock().unwrap() = Some(1);
    assert_eq!(
        work::relay_owner_holds(&w.app, &core, Utc::now()).await.unwrap(),
        0,
        "nothing held, nothing told"
    );
    assert!(core.proposed.lock().unwrap().is_empty());

    let (status, out) = w.command(&me, "staff", json!({ "command": "place_spending_hold" })).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    // Out of reach: the changes wait, with the reason.
    *core.lost.lock().unwrap() = true;
    assert_eq!(work::relay_owner_holds(&w.app, &core, Utc::now()).await.unwrap(), 0);
    assert_eq!(
        w.sql("select 1 from owner_hold_relay where attempts = 1 and last_error like 'partition%'")
            .await
            .unwrap(),
        1,
        "the party's later changes wait behind the first"
    );
    *core.lost.lock().unwrap() = false;
    core.proposed.lock().unwrap().clear();
    assert_eq!(work::relay_owner_holds(&w.app, &core, Utc::now()).await.unwrap(), 2);
    let placed = holds_taken(&core);
    assert_eq!(
        placed.iter().map(|h| (h.0, h.1.as_str(), h.2)).collect::<Vec<_>>(),
        vec![(1, "P1", true), (1, "P2", true)],
        "owned policies only, not the one already passed on"
    );
    assert!(placed[0].3.starts_with(&format!("spending-hold:{me}:")), "{placed:?}");
    assert_eq!(core.proposed.lock().unwrap().len(), 4, "each change goes to every partition");
    assert_eq!(
        w.sql("select 1 from owner_hold_relay where policy = 'P2' and not held and due_on = '2099-01-01'")
            .await
            .unwrap(),
        1,
        "P2 is let go when its ownership ends"
    );

    // While it stands, a new policy takes it and a policy passed on is let go.
    w.command(&me, "staff", add("o4", "owner", "P4", None)).await;
    w.command(
        &me,
        "staff",
        json!({ "command": "end_relationship", "id": "o1", "valid_to": today.to_string() }),
    )
    .await;
    // A partition that refuses for another reason holds the party's changes.
    *core.refuse.lock().unwrap() = Some(0);
    assert_eq!(work::relay_owner_holds(&w.app, &core, Utc::now()).await.unwrap(), 0);
    *core.refuse.lock().unwrap() = None;
    core.proposed.lock().unwrap().clear();
    assert_eq!(work::relay_owner_holds(&w.app, &core, Utc::now()).await.unwrap(), 2);
    let moved = holds_taken(&core);
    assert_eq!(moved.iter().map(|h| (h.1.as_str(), h.2)).collect::<Vec<_>>(), vec![("P4", true), ("P1", false)]);
    assert!(moved[1].3.starts_with("ownership-ended:"), "{moved:?}");
    assert!(core.proposed.lock().unwrap().iter().all(|(_, key, _)| key.starts_with("owner-hold:")));

    // Legal lifts it: every policy still owned is released under legal's reference.
    let (status, out) = w
        .command(&me, "legal", json!({ "command": "override_spending_hold", "reference": "CIV-2026-7" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    core.proposed.lock().unwrap().clear();
    assert_eq!(work::relay_owner_holds(&w.app, &core, Utc::now()).await.unwrap(), 2);
    let lifted = holds_taken(&core);
    assert_eq!(
        lifted.iter().map(|h| (h.1.as_str(), h.2, h.3.as_str())).collect::<Vec<_>>(),
        vec![("P2", false, "CIV-2026-7"), ("P4", false, "CIV-2026-7")]
    );
    // Ownership changes after it ends tell the core nothing.
    w.command(&me, "staff", add("o5", "owner", "P5", None)).await;
    assert_eq!(w.sql("select 1 from owner_hold_relay where due_on <= now()::date").await.unwrap(), 0);

    // A policy no partition holds is tried again for a week, then given up.
    *core.policies_on.lock().unwrap() = Some(9);
    w.command(&me, "staff", json!({ "command": "place_spending_hold" })).await;
    assert_eq!(work::relay_owner_holds(&w.app, &core, Utc::now()).await.unwrap(), 0);
    assert_eq!(
        w.sql("select 1 from owner_hold_relay where last_error = 'no partition holds the policy yet'")
            .await
            .unwrap(),
        1
    );
    let later = Utc::now() + chrono::Duration::days(work::OWNER_HOLD_WAIT_DAYS + 1);
    assert_eq!(work::relay_owner_holds(&w.app, &core, later).await.unwrap(), 0, "given up, not told");
    assert_eq!(w.sql("select 1 from owner_hold_relay where due_on <= now()::date").await.unwrap(), 0);
    // The change due when P2's ownership ends is still waiting for its day.
    assert_eq!(w.sql("select 1 from owner_hold_relay where due_on = '2099-01-01'").await.unwrap(), 2);
}
