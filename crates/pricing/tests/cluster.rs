//! The deployed arrangement on a real node: the module in its own
//! replicated group, its outbox carrying a price to the core, the core's
//! events coming back, and the driver working against the cluster and Blob
//! Storage (the stand-in that speaks its protocol).

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{NaiveDate, TimeZone, Utc};
use ic_azure::blob::{BlobContainer, WormStore};
use ic_azure::credential::AzureCredential;
use ic_azure::testing::{FakeAzure, TOKEN};
use ic_core::invest::{Direction, FundDefinition, InvestCommand, NewOrder, OrderAmount, OrderKind, Question, Reply};
use ic_core::Command as CoreCommand;
use ic_money::{Currency, FundId, Money, RoundingMode};
use ic_pricing::adapters::{BlobInbox, BlobOutbound, ClusterCore, ClusterModule};
use ic_pricing::command::*;
use ic_pricing::driver::{Driver, Inbox, Module, Scan};
use ic_pricing::service::{serve, Config};
use ic_product::Sex;
use ic_raft::server::{self, ColdTarget, EventTarget, Settings};
use ic_raft::{ClusterAuth, ClusterClient};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{invest_plan, issue_command};
use serde_json::json;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn fund() -> FundDefinition {
    let pricing = [
        ("previous_move", "0.05"),
        ("stale_asset_days", "3"),
        ("unexplained_move", "0.005"),
        ("error_threshold", "0.003"),
        ("launch_price", "1.0000"),
        ("pricing_lag_days", "1"),
    ];
    FundDefinition {
        id: FundId("growth".into()),
        version: 1,
        content_hash: "growth-v1".into(),
        name: "Growth".into(),
        currency: Currency::NZD,
        cut_off: "15:00".into(),
        buy_spread_bps: 25,
        sell_spread_bps: 25,
        price_decimals: 4,
        unit_rounding: RoundingMode::Down,
        non_pricing_days: Default::default(),
        open_to_new_money: true,
        pricing: pricing.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
    }
}

/// The inbound container, listed from a fixed set because the storage
/// stand-in does not list; scans and reads go to Blob Storage.
struct Listed(BlobInbox, Vec<String>);

impl Inbox for Listed {
    async fn list(&self, prefix: &str) -> Result<Vec<String>, String> {
        Ok(self.1.iter().filter(|n| n.starts_with(prefix)).cloned().collect())
    }
    async fn scan(&self, name: &str) -> Result<Scan, String> {
        self.0.scan(name).await
    }
    async fn read(&self, name: &str) -> Result<Option<Vec<u8>>, String> {
        self.0.read(name).await
    }
}

async fn eventually<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..200 {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}

/// Purpose: on a real node, a valuation file in Blob Storage becomes a
/// prepared price, a second person approves it, the host's outbox activates
/// it on the core, an order waiting for it executes, the executed order
/// comes back to the module as the policy's holding, the core's units on
/// issue mark the date complete, and the driver writes the administrator's
/// extract with that holding to the outbound container.
/// Why it matters: every other test runs the state machine in the test's
/// own process. This is the deployed path end to end: commands as JSON
/// through Raft, the core's own price command and refusal rules, the event
/// feed and the storage protocol. It is the test that fails if a price
/// struck here cannot actually reach a policy. The process's own loop is
/// run at the end, because it is what the container starts.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_valuation_becomes_a_price_an_order_and_an_extract() {
    let dir = std::env::temp_dir().join(format!("ic-pricing-cluster-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let addr = format!("127.0.0.1:{port}");
    let auth = ClusterAuth::tokens(vec!["test-cluster-token-0123456789abcdef".to_string()]).unwrap();
    let settings = Settings {
        id: 1,
        listen: addr.parse().unwrap(),
        data_dir: dir.join("node-1"),
        partitions: 1,
        peers: BTreeMap::from([(1, addr.clone())]),
        bootstrap: true,
        heartbeat_ms: 50,
        auth: auth.clone(),
        cold: ColdTarget::Dir(dir.join("cold")),
        events: EventTarget::Discard,
        segment_entries: 1000,
        snapshot_every: 1000,
    };
    let node = server::start_with(settings, vec![ic_pricing::module_spec().unwrap()]).await.unwrap();
    let client = ClusterClient::new(vec![addr.clone()]).with_auth(auth.clone());
    let patience = Duration::from_secs(20);

    // The core: a business date, the products, the fund, a policy with cash
    // waiting, and a buy order for today.
    let today = d(2027, 11, 15);
    let issue = invest_plan(1, today, d(1990, 3, 10), Sex::Female);
    let policy = issue.policy.clone();
    let invest = |c: InvestCommand| CoreCommand::Investment(Box::new(c));
    let mut setup = vec![("leader", CoreCommand::OpenBusinessDate { date: today })];
    setup.extend(shipped_activations().unwrap().into_iter().map(|c| ("config", c)));
    setup.push(("config", invest(InvestCommand::ActivateFund(Box::new(fund())))));
    setup.push(("new-business", issue_command(issue)));
    setup.push((
        "cash-matching",
        CoreCommand::ApplyReceipt {
            policy: policy.clone(),
            receipt: "r1".into(),
            amount: Money::new(61_84 + 5_000_00, Currency::NZD),
        },
    ));
    setup.push((
        "policy-servicing",
        invest(InvestCommand::AllocateCash {
            policy: policy.clone(),
            reference: "alloc-1".into(),
            amount_minor: 5_000_00,
        }),
    ));
    setup.push((
        "policy-servicing",
        invest(InvestCommand::PlaceOrder(Box::new(NewOrder {
            order_id: "buy-1".into(),
            policy: policy.clone(),
            fund: FundId("growth".into()),
            direction: Direction::Buy,
            amount: OrderAmount::Cash { minor: 1_002_50 },
            kind: OrderKind::PremiumAllocation,
            pricing_date: today,
            switch_to: None,
        }))),
    ));
    for (n, (who, command)) in setup.into_iter().enumerate() {
        let envelope = ic_core::Envelope {
            key: format!("setup-{n}"),
            caller: who.to_string(),
            salt: ic_raft::client::fresh_salt(),
            traceparent: None,
            command,
        };
        let reply = client.propose_until(0, &envelope, patience).await.unwrap();
        assert_eq!(reply.rejected, None, "setup {n}");
    }

    // The module and its driver.
    let module = ClusterModule::new(client.clone(), patience);
    let at = Utc.with_ymd_and_hms(2027, 11, 15, 6, 0, 0).unwrap();
    let send = |key: &str, caller: &str, actor: &str, roles: &[&str], command: Command| Envelope {
        key: key.into(),
        caller: caller.into(),
        actor: actor.into(),
        roles: roles.iter().map(|r| r.to_string()).collect(),
        at,
        command,
    };
    let out = module
        .propose(&send("fund-1", CALLER_CONFIG, "config", &[], Command::ActivateFund(Box::new(fund()))))
        .await
        .unwrap();
    assert_eq!(out, Output::Done);

    let fake = FakeAzure::start().await;
    let credential = AzureCredential::static_token(TOKEN);
    let inbound = BlobContainer::new(&fake.base, "unit-pricing-inbound", credential.clone());
    let file = "valuations/2027-11-15/acme-20271115.csv";
    let body = "H,acme,VAL-20271115\nV,growth,2027-11-15,0,0,0,0,,N\nT,1,0\n";
    inbound.put_new(file, "text/csv", body.as_bytes().to_vec()).await.unwrap();
    let driver = Driver::new(
        module.clone(),
        ClusterCore::new(client.clone(), 1),
        Listed(BlobInbox::new(&fake.base, "unit-pricing-inbound", credential.clone()), vec![file.to_string()]),
        BlobOutbound(BlobContainer::new(&fake.base, "unit-pricing-outbound", credential)),
    );
    driver.start(at).await.unwrap();

    // Not yet scanned: the file waits.
    let report = driver.pass(at).await;
    assert_eq!((report.files_waiting, report.files_taken, report.units_reported), (1, 0, 1), "{report:?}");
    fake.set_blob_tags(&format!("unit-pricing-inbound/{file}"), &[("Malware Scanning scan result", "No threats found")]);
    let report = driver.pass(at).await;
    assert_eq!((report.files_taken, report.units_reported), (1, 0), "{report:?}");
    assert!(report.errors.is_empty(), "{report:?}");

    // Maker and checker.
    let prepared = module
        .propose(&send(
            "prep-1",
            CALLER_CONSOLE,
            "maker",
            &[ROLE_PREPARER],
            Command::PreparePrice {
                fund: FundId("growth".into()),
                pricing_date: today,
            },
        ))
        .await
        .unwrap();
    let Output::Prepared { price, .. } = prepared else { panic!("{prepared:?}") };
    let approved = module
        .propose(&send(
            "appr-1",
            CALLER_CONSOLE,
            "checker",
            &[ROLE_APPROVER],
            Command::ApprovePrice {
                price: price.clone(),
                accepted: BTreeMap::new(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(approved, Output::Done);

    // The host's outbox carries the price to the core.
    eventually("the price on the core", || async {
        matches!(client.investment(0, &Question::Executable).await, Ok(Reply::Executable(v)) if v.iter().any(|e| e.price_id == price))
    })
    .await;
    eventually("the price active in the module", || async {
        module
            .ask(&json!({"q": "price", "id": price}))
            .await
            .map(|p| p["status"] == "active")
            .unwrap_or(false)
    })
    .await;

    // The order executes; its event reaches the module as a holding.
    let execute = ic_core::Envelope {
        key: "execute-1".into(),
        caller: "unit-orders".into(),
        salt: ic_raft::client::fresh_salt(),
        traceparent: None,
        command: invest(InvestCommand::ExecuteOrders {
            fund: FundId("growth".into()),
            pricing_date: today,
            limit: 100,
        }),
    };
    assert_eq!(client.propose_until(0, &execute, patience).await.unwrap().rejected, None);
    let policy_id = policy.0.clone();
    eventually("the holding in the module", || async {
        module
            .ask(&json!({"q": "holding", "fund": "growth", "policy": policy_id}))
            .await
            .map(|h| h["units_micro"] == 1_000_000_000)
            .unwrap_or(false)
    })
    .await;

    // The driver sees the date fully executed and writes the extract.
    let report = driver.pass(at).await;
    assert_eq!(report.extracts_written, 1, "{report:?}");
    let written = fake.blobs();
    let extract = String::from_utf8(written["unit-pricing-outbound/extracts/growth/2027-11-15.csv"].clone()).unwrap();
    assert!(extract.contains(&format!("D,{},1000.000000", policy.0)), "{extract}");
    assert!(extract.contains("F,NZD,1002.50,0.00,1002.50,1"), "{extract}");
    // Nothing more to do on the next pass.
    let report = driver.pass(at).await;
    assert_eq!((report.files_taken, report.units_reported, report.extracts_written), (0, 0, 0), "{report:?}");

    // The process as deployed runs its passes until told to stop.
    let config = Config {
        cluster: addr.clone(),
        inbox_endpoint: fake.base.clone(),
        inbox_container: "unit-pricing-inbound".into(),
        outbound_endpoint: fake.base.clone(),
        outbound_container: "unit-pricing-outbound".into(),
        cycle_every: Duration::from_millis(50),
        patience,
    };
    let stop = tokio::time::sleep(Duration::from_millis(300));
    serve(config, auth, AzureCredential::static_token(TOKEN), stop).await.unwrap();

    // The shipped binary refuses to start without its settings, runs with
    // them, and stops cleanly on SIGTERM, as Kubernetes ends a pod.
    let binary = || {
        // The test's own environment stays, so a coverage run still
        // records the binary; none of the driver's settings are in it.
        let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_ic-unit-pricing"));
        c.env_remove("IC_NODES");
        c
    };
    let refused = binary().output().unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("IC_NODES"));
    let token_file = dir.join("federated-token");
    std::fs::write(&token_file, "a-projected-service-account-token").unwrap();
    let mut child = binary()
        .env("IC_NODES", &addr)
        .env("IC_CLUSTER_TOKEN", "test-cluster-token-0123456789abcdef")
        .env("AZURE_FEDERATED_TOKEN_FILE", &token_file)
        .env("AZURE_TENANT_ID", "tenant-1")
        .env("AZURE_CLIENT_ID", "client-1")
        .env("AZURE_AUTHORITY_HOST", format!("{}/", fake.base))
        .env("IC_UNIT_PRICING_INBOX_ENDPOINT", &fake.base)
        .env("IC_UNIT_PRICING_OUTBOUND_ENDPOINT", &fake.base)
        .env("IC_UNIT_PRICING_CYCLE_MS", "100")
        .spawn()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(child.try_wait().unwrap().is_none(), "ic-unit-pricing exited at start");
    assert!(std::process::Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap()
        .success());
    let mut tries = 0;
    let exit = loop {
        if let Some(exit) = child.try_wait().unwrap() {
            break exit;
        }
        tries += 1;
        assert!(tries < 400, "ic-unit-pricing ignored SIGTERM");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(exit.success(), "ic-unit-pricing exited with {exit}");

    node.stop().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
