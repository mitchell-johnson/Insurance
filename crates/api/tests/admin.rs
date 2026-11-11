//! `ic-admin` against a cluster of ledger nodes in this process.

mod common;

use std::collections::BTreeMap;
use std::process::Command as Process;
use std::time::Duration;

use common::{d, Ledger, TOKEN};
use ic_api::admin::{Admin, AdminCommand, AdminSettings, USAGE};
use ic_raft::{Answer, Query};
use serde_json::json;

fn admin(ledger: &Ledger, patience: Duration) -> Admin {
    Admin::new(&AdminSettings {
        nodes: ledger.nodes(),
        auth: ledger.auth.clone(),
        patience,
    })
}

fn args(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_string).collect()
}

/// Purpose: bring an empty three-node ledger into service the way the deployment job does:
/// open the business date, activate the shipped configuration, and run both again; then
/// read the nodes' status and re-state the members of the groups.
/// Why it matters: these are the first commands run against a production ledger and the
/// ones a runbook repeats under pressure. Each must be safe to run twice, because the job
/// that runs them can die half way; and opening a ledger that is already on another date
/// must be refused, because that would move the business date outside the close.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_empty_ledger_is_opened_and_configured_and_the_job_can_run_twice() {
    let ledger = Ledger::start("admin", 3, 2).await;
    let admin = admin(&ledger, Duration::from_secs(20));
    assert_eq!(admin.partitions().await, Ok(2));

    let date = d(2027, 11, 15);
    let opened = admin.run(AdminCommand::Open { date }).await.unwrap();
    assert_eq!(opened, json!({ "business_date": "2027-11-15", "opened": [0, 1], "already_open": [] }));
    let again = admin.run(AdminCommand::Open { date }).await.unwrap();
    assert_eq!(again, json!({ "business_date": "2027-11-15", "opened": [], "already_open": [0, 1] }));
    let error = admin.run(AdminCommand::Open { date: d(2027, 11, 16) }).await.unwrap_err();
    assert!(error.contains("is on 2027-11-15, not 2027-11-16"), "{error}");
    for partition in 0..2 {
        assert_eq!(
            ledger.client.query(partition, &Query::BusinessDate).await.unwrap(),
            Answer::BusinessDate(Some(date))
        );
    }

    let first = admin.run(AdminCommand::ActivateShipped).await.unwrap();
    let items = first["items"].as_u64().unwrap();
    assert!(items >= 8, "{first}");
    assert_eq!(first, json!({ "items": items, "partitions": 2, "activated": items * 2, "unchanged": 0 }));
    let second = admin.run(AdminCommand::ActivateShipped).await.unwrap();
    assert_eq!(second, json!({ "items": items, "partitions": 2, "activated": 0, "unchanged": items * 2 }));

    // The configuration is live: a quote prices on both partitions.
    for n in 1..=4u64 {
        let application = ic_sim::persona::term_life(
            n,
            date,
            d(1990, 3, 10),
            ic_product::Sex::Female,
            false,
            500_000_00,
            None,
            ic_product::Frequency::Monthly,
            None,
        );
        let partition = ic_core::home_partition(&application.group) % 2;
        let query = Query::Quote {
            application: Box::new(application),
        };
        let Answer::Quote(Some(Ok(_))) = ledger.client.query(partition, &query).await.unwrap() else {
            panic!("partition {partition} could not price with the shipped configuration");
        };
    }

    let status = admin.run(AdminCommand::Status).await.unwrap();
    let nodes = status.as_object().unwrap();
    assert_eq!(nodes.len(), 3);
    for (addr, partitions) in nodes {
        assert_eq!(partitions.as_array().map(Vec::len), Some(2), "{addr}: {partitions}");
        assert_eq!(partitions[0]["voters"], json!([1, 2, 3]), "{addr}");
    }

    // Re-stating the members a group already has changes nothing and succeeds.
    let members: BTreeMap<u64, String> = ledger.settings.iter().map(|(id, s)| (*id, s.listen.to_string())).collect();
    let every = admin
        .run(AdminCommand::Members {
            partition: None,
            members: members.clone(),
        })
        .await
        .unwrap();
    assert_eq!(every["partitions"], json!([0, 1]));
    let one = admin
        .run(AdminCommand::Members {
            partition: Some(1),
            members: members.clone(),
        })
        .await
        .unwrap();
    assert_eq!(one["partitions"], json!([1]));
    let hasty = self::admin(&ledger, Duration::from_millis(300));
    let error = hasty.run(AdminCommand::Members { partition: Some(7), members }).await.unwrap_err();
    assert!(error.starts_with("partition 7:"), "{error}");

    // With every node gone, each action says so instead of hanging.
    let nodes = ledger.nodes();
    let auth = ledger.auth.clone();
    ledger.stop().await;
    let gone = Admin::new(&AdminSettings {
        nodes,
        auth,
        patience: Duration::from_millis(300),
    });
    for command in [AdminCommand::Status, AdminCommand::Open { date }, AdminCommand::ActivateShipped] {
        let error = gone.run(command.clone()).await.unwrap_err();
        assert!(error.contains("no ledger node answered"), "{command:?}: {error}");
    }
}

/// Purpose: the command line and the environment of `ic-admin` are read correctly, and each
/// mistake is refused with the usage or with the setting named.
/// Why it matters: `ic-admin members` rewrites who votes in a Raft group. A mistyped member
/// list that was half understood, such as a node given twice or an address dropped, could
/// remove a healthy node from a group. Anything not exactly understood must be refused
/// before a single request is sent.
#[test]
fn the_command_line_is_read_strictly() {
    assert_eq!(AdminCommand::parse(&args("status")), Ok(AdminCommand::Status));
    assert_eq!(AdminCommand::parse(&args("open 2027-11-15")), Ok(AdminCommand::Open { date: d(2027, 11, 15) }));
    assert_eq!(AdminCommand::parse(&args("activate-shipped")), Ok(AdminCommand::ActivateShipped));
    let members: BTreeMap<u64, String> = [(1, "a:7000".to_string()), (4, "d:7000".to_string())].into();
    assert_eq!(
        AdminCommand::parse(&args("members all 1=a:7000,4=d:7000")),
        Ok(AdminCommand::Members {
            partition: None,
            members: members.clone()
        })
    );
    assert_eq!(
        AdminCommand::parse(&args("members 3 4=d:7000,1=a:7000,")),
        Ok(AdminCommand::Members { partition: Some(3), members })
    );

    let refused = |line: &str, names: &str| {
        let error = AdminCommand::parse(&args(line)).unwrap_err();
        assert!(error.contains(names), "{line:?}: {error:?} does not mention {names:?}");
    };
    for line in ["", "reopen", "open", "status now", "members all", "open 2027-11-15 2027-11-16"] {
        refused(line, USAGE);
    }
    refused("open tomorrow", "date:");
    refused("members some 1=a:7000", "partition:");
    refused("members all 1", "not id=host:port");
    refused("members all one=a:7000", "node id");
    refused("members all 1=a:7000,1=b:7000", "given twice");
    refused("members all 1=", "no address");
    refused("members all ,", "at least one member");

    let read = |vars: &[(&str, &str)]| AdminSettings::from_lookup(|key| vars.iter().find(|(k, _)| *k == key).map(|(_, v)| (*v).to_string()));
    let settings = read(&[("IC_NODES", "a:7000,b:7000"), ("IC_CLUSTER_TOKEN", TOKEN)]).unwrap();
    assert_eq!(settings.nodes, vec!["a:7000", "b:7000"]);
    assert_eq!(settings.patience, Duration::from_secs(60));
    assert_eq!(settings.auth.outgoing(), Some(TOKEN));
    let settings = read(&[("IC_NODES", "a:7000"), ("IC_CLUSTER_AUTH", "open"), ("IC_LEDGER_PATIENCE_SECS", "5")]).unwrap();
    assert_eq!(settings.patience, Duration::from_secs(5));
    assert!(read(&[("IC_NODES", "a:7000")]).unwrap_err().contains("IC_CLUSTER_TOKEN_FILE"));
    assert!(read(&[("IC_CLUSTER_AUTH", "open")]).unwrap_err().contains("IC_NODES is required"));
    assert!(read(&[("IC_NODES", ","), ("IC_CLUSTER_AUTH", "open")]).unwrap_err().contains("IC_NODES:"));
    assert!(read(&[("IC_NODES", "a:1"), ("IC_CLUSTER_AUTH", "open"), ("IC_LEDGER_PATIENCE_SECS", "x")])
        .unwrap_err()
        .contains("IC_LEDGER_PATIENCE_SECS"));
}

/// Purpose: the `ic-admin` binary itself fails with the usage on a bad command line, names
/// a missing setting, reports an unreachable ledger, and exits non-zero each time.
/// Why it matters: it runs inside deployment jobs, where the exit code is the only thing
/// the pipeline looks at. A failed open that exited zero would let a deployment proceed
/// against a ledger that was never opened.
#[test]
fn the_binary_fails_loudly() {
    let run = |args: &[&str], env: &[(&str, &str)]| {
        let output = Process::new(env!("CARGO_BIN_EXE_ic-admin"))
            .env_clear()
            .envs(env.iter().copied())
            .args(args)
            .output()
            .unwrap();
        (output.status.success(), String::from_utf8_lossy(&output.stderr).into_owned())
    };
    let (ok, stderr) = run(&["reopen"], &[]);
    assert!(!ok && stderr.contains("usage: ic-admin status"), "{stderr}");
    let (ok, stderr) = run(&["status"], &[("IC_CLUSTER_AUTH", "open")]);
    assert!(!ok && stderr.contains("IC_NODES is required"), "{stderr}");
    let unreachable = [("IC_NODES", "127.0.0.1:1"), ("IC_CLUSTER_AUTH", "open"), ("IC_LEDGER_PATIENCE_SECS", "0")];
    let (ok, stderr) = run(&["status"], &unreachable);
    assert!(!ok && stderr.contains("no ledger node answered"), "{stderr}");
    let (ok, stderr) = run(&["open", "2027-11-15"], &unreachable);
    assert!(!ok && stderr.contains("no ledger node answered"), "{stderr}");
}
