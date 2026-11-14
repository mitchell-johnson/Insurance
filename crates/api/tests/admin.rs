//! `ic-admin` against a cluster of ledger nodes in this process.

mod common;

use std::collections::BTreeMap;
use std::process::Command as Process;
use std::time::Duration;

use common::{d, Ledger, TOKEN};
use ic_api::admin::{Admin, AdminCommand, AdminSettings, USAGE};
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
    // Forming groups that already exist changes nothing.
    let formed = admin.run(AdminCommand::Init).await.unwrap();
    assert!(formed["formed_by"].is_string(), "{formed}");

    let date = d(2027, 11, 15);
    let opened = admin.run(AdminCommand::Open { date }).await.unwrap();
    assert_eq!(opened, json!({ "business_date": "2027-11-15", "opened": [0, 1], "already_open": [] }));
    let again = admin.run(AdminCommand::Open { date }).await.unwrap();
    assert_eq!(again, json!({ "business_date": "2027-11-15", "opened": [], "already_open": [0, 1] }));
    let error = admin.run(AdminCommand::Open { date: d(2027, 11, 16) }).await.unwrap_err();
    assert!(error.contains("is on 2027-11-15, not 2027-11-16"), "{error}");
    for partition in 0..2 {
        assert_eq!(ledger.client.business_date(partition).await.unwrap(), Some(date));
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
        let quote = ledger.client.quote(partition, application).await.unwrap();
        assert!(
            matches!(quote, Some(Ok(_))),
            "partition {partition} could not price with the shipped configuration: {quote:?}"
        );
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
    for command in [
        AdminCommand::Init,
        AdminCommand::Status,
        AdminCommand::Open { date },
        AdminCommand::ActivateShipped,
    ] {
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
    assert_eq!(AdminCommand::parse(&args("init")), Ok(AdminCommand::Init));
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

/// Purpose: a set-up file's steps are sent in order to the core's configuration caller on
/// every partition and to a hosted module; a step the module refuses stops the run and is
/// named, unless the step says that refusal means it is already as wanted; the file applied
/// again changes nothing; and a file with a step that is not configuration, has no key or
/// names no module is refused whole before anything is sent.
/// Why it matters: this is how an insurer's own configuration (finance's chart, claims
/// rules, banks, timers) reaches a new deployment, and the job that does it can die half
/// way. A step silently skipped would leave a module unconfigured on day one; a file that
/// could carry any command would make the set-up job a way round every caller's role.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_set_up_file_is_applied_in_order_and_can_be_applied_again() {
    use ic_api::admin::SetupFile;
    let ledger = Ledger::start_hosting("setup", 1, 2, || vec![ic_finance::host::module_spec().unwrap()]).await;
    let admin = admin(&ledger, Duration::from_secs(20));
    admin.run(AdminCommand::Open { date: d(2027, 11, 15) }).await.unwrap();
    let chart = ic_finance::Chart::load("chart.star", include_str!("../../../config/finance/chart.star")).unwrap();
    let chart_id = format!("chart:{}:{}", chart.version, chart.content_hash);
    let finance = |key: &str, caller: &str, actor: Option<(&str, &str)>, command: serde_json::Value| {
        let mut envelope = json!({ "key": key, "caller": caller, "command": command });
        if let Some((actor, role)) = actor {
            envelope["actor"] = json!(actor);
            envelope["roles"] = json!([role]);
        }
        json!({ "module": { "module": "finance", "envelope": envelope } })
    };
    let rate =
        json!({ "core": { "key": "wage-index-1", "command": { "activate_index_rate": { "code": "wage", "effective_from": "2027-01-01", "rate": "0.03" } } } });
    let configure = finance(
        "setup-configure",
        "operator",
        None,
        json!({ "configure": { "partitions": 2, "year_end_month": 3 } }),
    );
    let propose = finance(
        "setup-chart",
        "finance-console",
        Some(("author", "finance_operator")),
        json!({ "propose_chart": { "chart": chart } }),
    );
    // The person who proposed a chart cannot approve it.
    let own = json!({ "approve_config": { "id": chart_id } });
    let self_approved = finance("setup-chart-self", "finance-console", Some(("author", "finance_lead")), own);
    let approve = finance(
        "setup-chart-ok",
        "finance-console",
        Some(("approver", "finance_lead")),
        json!({ "approve_config": { "id": chart_id } }),
    );

    let file = |steps: Vec<&serde_json::Value>| SetupFile::parse(&json!({ "steps": steps }).to_string()).unwrap();
    let error = admin
        .apply(&file(vec![&rate, &configure, &propose, &self_approved, &approve]))
        .await
        .unwrap_err();
    assert!(error.starts_with("step 4: finance refused"), "{error}");
    // The steps before it stand; the whole file, with the refusal accepted as known, goes through.
    let mut known = self_approved.clone();
    known["module"]["accept"] = json!(["separation"]);
    let report = admin.apply(&file(vec![&rate, &configure, &propose, &known, &approve])).await.unwrap();
    assert_eq!(
        report,
        json!({ "steps": 5, "applied": 3, "unchanged": 2 }),
        "the rate was already active; the refusal was accepted"
    );
    for partition in 0..2 {
        let wage = json!({ "activate_index_rate": { "code": "wage", "effective_from": "2027-01-01", "rate": "0.04" } });
        let again = SetupFile::parse(&json!({ "steps": [{ "core": { "key": "wage-index-2", "command": wage } }] }).to_string()).unwrap();
        let error = admin.apply(&again).await.unwrap_err();
        assert!(error.contains("refused activate_index_rate"), "partition {partition}: {error}");
    }

    // Through the command line, from a file on disk.
    let path = ledger.dir.join("setup.json");
    std::fs::write(&path, json!({ "steps": [rate, configure] }).to_string()).unwrap();
    let report = admin
        .run(AdminCommand::parse(&args(&format!("apply {}", path.display()))).unwrap())
        .await
        .unwrap();
    assert_eq!(report["steps"], 2);
    let missing = admin
        .run(AdminCommand::Apply {
            file: ledger.dir.join("none.json"),
        })
        .await
        .unwrap_err();
    assert!(missing.contains("none.json"), "{missing}");
    let unknown = json!({ "steps": [{ "module": { "module": "no-such-module", "envelope": {} } }] });
    let impatient = self::admin(&ledger, Duration::from_secs(1));
    let error = impatient.apply(&SetupFile::parse(&unknown.to_string()).unwrap()).await.unwrap_err();
    assert!(error.starts_with("step 1: no-such-module"), "{error}");

    let refused = |steps: serde_json::Value, why: &str| {
        let error = SetupFile::parse(&json!({ "steps": steps }).to_string()).unwrap_err();
        assert!(error.contains(why), "{error}");
    };
    refused(
        json!([{ "core": { "key": "k", "command": { "cancel_policy": { "policy": "pol-1" } } } }]),
        "step 1: cancel_policy is not a configuration command",
    );
    refused(
        json!([rate, { "core": { "key": " ", "command": rate["core"]["command"] } }]),
        "step 2: a core step has a key",
    );
    refused(json!([{ "module": { "module": "Finance", "envelope": {} } }]), "names its module");
    refused(json!([{ "module": { "module": "finance", "envelope": "text" } }]), "names its module");
    refused(json!([{ "shell": "rm" }]), "the set-up file");
    assert!(SetupFile::parse("{").unwrap_err().contains("the set-up file"));
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
            .envs(std::env::vars().filter(|(k, _)| k == "LLVM_PROFILE_FILE"))
            .envs(env.iter().copied())
            .args(args)
            .output()
            .unwrap();
        (output.status.success(), String::from_utf8_lossy(&output.stderr).into_owned())
    };
    let (ok, stderr) = run(&["reopen"], &[]);
    assert!(!ok && stderr.contains("usage: ic-admin init"), "{stderr}");
    let (ok, stderr) = run(&["status"], &[("IC_CLUSTER_AUTH", "open")]);
    assert!(!ok && stderr.contains("IC_NODES is required"), "{stderr}");
    let unreachable = [("IC_NODES", "127.0.0.1:1"), ("IC_CLUSTER_AUTH", "open"), ("IC_LEDGER_PATIENCE_SECS", "0")];
    let (ok, stderr) = run(&["status"], &unreachable);
    assert!(!ok && stderr.contains("no ledger node answered"), "{stderr}");
    let (ok, stderr) = run(&["open", "2027-11-15"], &unreachable);
    assert!(!ok && stderr.contains("no ledger node answered"), "{stderr}");
}

/// Purpose: the shipped configuration that `ic-admin activate-shipped` proposes records each
/// rate table's validation, made by finance's own suite against `config/finance/tables.star`,
/// immediately before the table is activated, for exactly the content activated, and every
/// shipped table passes.
/// Why it matters: finance's close blocks on an active table with no validation record. A
/// deployment that activated the shipped tables without one would fail its first close, and
/// a table that no longer passes its schema must stop the deployment, not go live.
#[test]
fn every_shipped_rate_table_is_validated_before_it_is_activated() {
    use ic_core::Command;
    let commands = ic_api::admin::shipped_with_validations().unwrap();
    let mut tables = 0;
    for (at, command) in commands.iter().enumerate() {
        if let Command::ActivateRateTable { table } = command {
            tables += 1;
            let Some(Command::RecordRateTableValidation(record)) = at.checked_sub(1).map(|i| &commands[i]) else {
                panic!("{} is not preceded by its validation", table.id);
            };
            assert_eq!(
                (&record.id, record.version, &record.content_hash),
                (&table.id, table.version, &table.content_hash)
            );
            assert_eq!(record.by, ic_api::admin::SHIPPED_VALIDATOR);
        }
    }
    assert!(tables >= 8, "{tables}");
    let validations = commands.iter().filter(|c| matches!(c, Command::RecordRateTableValidation(_))).count();
    assert_eq!(validations, tables);
}
