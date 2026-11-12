//! The driver's production connections: Azure Blob Storage and Event Hubs
//! (against the stand-in that speaks their protocols), and the module and
//! the core on a real node.

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use common::*;
use ic_azure::blob::BlobContainer;
use ic_azure::credential::AzureCredential;
use ic_azure::eventhubs::EventHubsSink;
use ic_azure::testing::{FakeAzure, TOKEN};
use ic_core::command::LifeSpec;
use ic_core::{Command, LifeKind};
use ic_group::adapters::{event_message, reinsurance_envelope, ClusterCore, ClusterModule, ClusterPeers, QuarantineBlobs};
use ic_group::command::{caller, Envelope, GroupCommand, Resolution};
use ic_group::driver::{CustomerMaster, Driver, FileStore, Module, Scan};
use ic_group::event::{GroupEvent, ReinsuranceNotice};
use ic_group::mapping::{Columns, FileKind, FileMapping, SalaryPeriod, SalaryUnit};
use ic_group::model::Member;
use ic_group::record::MemberIdentity;
use ic_group::Question;
use ic_raft::server::{self, ColdTarget, EventTarget, Settings};
use ic_raft::{ClusterAuth, ClusterClient};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{group_scheme, issue_command};

/// Purpose: quarantine storage on Azure Blob Storage stores a file once, reports the
/// Defender for Storage scan from the blob's index tag (no tag yet, clean, anything else),
/// reads the file back, and will not build a blob path from a name that could leave the
/// scheme's folder.
/// Why it matters: the scan tag is the only thing between an outside party's upload and
/// the parser. Treating "no tag yet" or an unfamiliar verdict as clean would read an
/// unscanned or infected file; and scheme and file names come from outside too.
#[tokio::test]
async fn quarantine_storage_reads_the_scan_verdict_from_the_blob() {
    let fake = FakeAzure::start().await;
    let store = QuarantineBlobs::new(BlobContainer::new(&fake.base, "group-quarantine", AzureCredential::static_token(TOKEN)));
    assert_eq!(store.read("harbour", "nov.csv").await.unwrap(), None);
    assert_eq!(store.scan("harbour", "nov.csv").await.unwrap(), Scan::Pending, "not there yet");
    store.put("harbour", "nov.csv", b"first".to_vec()).await.unwrap();
    store.put("harbour", "nov.csv", b"second".to_vec()).await.unwrap();
    assert_eq!(store.read("harbour", "nov.csv").await.unwrap(), Some(b"first".to_vec()), "stored once");
    assert_eq!(store.scan("harbour", "nov.csv").await.unwrap(), Scan::Pending, "stored, not yet scanned");
    let key = "group-quarantine/harbour/nov.csv";
    fake.set_blob_tags(key, &[("Malware Scanning scan result", "No threats found"), ("owner", "a&b")]);
    assert_eq!(store.scan("harbour", "nov.csv").await.unwrap(), Scan::Clean);
    fake.set_blob_tags(key, &[("Malware Scanning scan result", "Malicious")]);
    assert_eq!(store.scan("harbour", "nov.csv").await.unwrap(), Scan::Malicious);
    fake.set_blob_tags(key, &[("Malware Scanning scan result", "Scan timed out")]);
    assert_eq!(
        store.scan("harbour", "nov.csv").await.unwrap(),
        Scan::Malicious,
        "an unfamiliar verdict is not clean"
    );

    for (scheme, file) in [
        ("..", "a.csv"),
        ("harbour", "../other/a.csv"),
        ("harbour", ""),
        ("har bour", "a.csv"),
        ("harbour", ".hidden"),
    ] {
        assert!(store.put(scheme, file, vec![1]).await.is_err(), "{scheme}/{file}");
        assert!(store.scan(scheme, file).await.is_err());
        assert!(store.read(scheme, file).await.is_err());
    }
    fake.set_down(true);
    assert!(store.put("harbour", "dec.csv", vec![1]).await.is_err());
    assert!(store.scan("harbour", "nov.csv").await.is_err());
    assert!(store.read("harbour", "nov.csv").await.is_err());
}

/// Purpose: what the driver sends to the reinsurance module is a command that module
/// actually accepts from this caller, for every kind of notice, and an event on the
/// stream carries the properties a consumer orders and de-duplicates by.
/// Why it matters: the two modules are built apart and meet only in this JSON. A field
/// renamed on either side would otherwise first show as possible matches silently not
/// reaching accumulation.
#[test]
fn notices_are_commands_the_reinsurance_module_accepts() {
    let policy = ic_core::PolicyId("pol-1".into());
    let notices = vec![
        ReinsuranceNotice::SetSchemeTerms {
            policy: policy.clone(),
            excess_path: ic_group::terms::ExcessPath::UnderwrittenExcess,
            sites: vec![("akl-hq".into(), 120)],
        },
        ReinsuranceNotice::NotePossibleMatch {
            policy: policy.clone(),
            life: life("E002"),
            party: party("party-a"),
        },
        ReinsuranceNotice::ClearPossibleMatch {
            policy: policy.clone(),
            life: life("E002"),
            party: party("party-a"),
        },
        ReinsuranceNotice::RecordRiskFactors {
            policy,
            life: life("E002"),
            date_of_birth: Some(d(1980, 5, 1)),
            sex: Some(ic_product::Sex::Female),
            smoker: Some(false),
        },
    ];
    for notice in &notices {
        let value = reinsurance_envelope("grp-ri:7", d(2027, 11, 1), notice);
        let envelope: ic_reinsurance::machine::Envelope = serde_json::from_value(value.clone()).unwrap_or_else(|e| panic!("{value}: {e}"));
        assert!(
            envelope.command.allowed_callers().contains(&envelope.caller.as_str()),
            "{}",
            envelope.command.name()
        );
        assert_eq!((envelope.key.as_str(), envelope.on), ("grp-ri:7", d(2027, 11, 1)));
    }
    let last = reinsurance_envelope("k", d(2027, 11, 1), &notices[3]);
    assert_eq!(last["command"]["record_risk_factors"]["factors"]["date_of_birth"], "1980-05-01");

    let event = GroupEvent::SchemeUnfrozen { scheme: SCHEME.into() };
    let (body, properties) = event_message(42, d(2027, 11, 2), &event);
    assert_eq!(body["event"]["type"], "scheme_unfrozen");
    assert_eq!(
        (properties["event-id"].as_str(), properties["seq"].as_u64(), properties["type"].as_str()),
        (Some("group:42"), Some(42), Some("scheme_unfrozen"))
    );
}

struct Everyone;

impl CustomerMaster for Everyone {
    async fn resolve(&self, _scheme: &str, identity: &MemberIdentity) -> Result<Resolution, String> {
        Ok(Resolution::Created {
            party: party(&format!("party-{}", identity.employee_id)),
        })
    }
}

fn named_mapping() -> FileMapping {
    FileMapping {
        id: "harbour-freight".to_string(),
        version: 1,
        kind: FileKind::Full,
        delimiter: ',',
        columns: Columns {
            employee_id: "Emp No".to_string(),
            given_name: Some("First".to_string()),
            family_name: Some("Last".to_string()),
            date_of_birth: Some("DOB".to_string()),
            category: Some("Category".to_string()),
            salary: Some("Salary".to_string()),
            billing_group: Some("Site".to_string()),
            ..Columns::default()
        },
        date_format: "%d/%m/%Y".to_string(),
        salary_unit: SalaryUnit::Major,
        salary_period: SalaryPeriod::Annual,
        categories: BTreeMap::new(),
        change_values: BTreeMap::new(),
        content_hash: String::new(),
    }
    .seal()
    .unwrap()
}

/// Purpose: on a real node, with the module in its own replicated group fed by the core's
/// event stream, an uploaded file goes from Blob Storage through the scan, approval and
/// matching to members in the core; the core's answers and events come back through the
/// cluster; events reach Event Hubs; and what has no receiver yet stays in the outbox.
/// Why it matters: every other test here runs the state machine in the test's own
/// process. This one is the deployed arrangement: commands as JSON through Raft, the
/// host's event feed, and the cluster client's routing. It is the test that fails if the
/// module cannot actually run where it is hosted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_file_reaches_the_core_on_a_real_node() {
    let dir = std::env::temp_dir().join(format!("ic-group-cluster-{}", std::process::id()));
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
    let node = server::start_with(settings, vec![ic_group::module_spec().unwrap()]).await.unwrap();
    let client = ClusterClient::new(vec![addr.clone()]).with_auth(auth);
    let patience = Duration::from_secs(20);

    // The core: a business date, the shipped products, and the scheme policy.
    let core = |n: u32, who: &str, command: Command| ic_core::Envelope {
        key: format!("setup-{n}"),
        caller: who.to_string(),
        salt: ic_raft::client::fresh_salt(),
        traceparent: None,
        command,
    };
    let today = d(2027, 11, 1);
    let mut setup = vec![("leader", Command::OpenBusinessDate { date: today })];
    setup.extend(shipped_activations().unwrap().into_iter().map(|c| ("config", c)));
    let founder = attrs("staff", 60_000_00, "akl");
    let issue = group_scheme(
        30,
        today,
        vec![LifeSpec {
            id: life("E001"),
            kind: LifeKind::Named { party: party("party-1") },
            factors: founder.factors(),
        }],
    );
    let (policy, group) = (issue.policy.clone(), issue.group.clone());
    setup.push(("new-business", issue_command(issue)));
    for (n, (who, command)) in setup.into_iter().enumerate() {
        let reply = client.propose_until(0, &core(n as u32, who, command), patience).await.unwrap();
        assert_eq!(reply.rejected, None);
    }

    let fake = FakeAzure::start().await;
    let credential = AzureCredential::static_token(TOKEN);
    let module = ClusterModule::new(client.clone(), patience);
    let driver = Driver::new(
        module.clone(),
        ClusterCore::new(client.clone(), 1, patience),
        ClusterPeers::new(
            client.clone(),
            Duration::from_millis(200),
            "reinsurance",
            EventHubsSink::new(&fake.base, "group-events", credential.clone()),
        ),
        Everyone,
        QuarantineBlobs::new(BlobContainer::new(&fake.base, "group-quarantine", credential)),
        "test-salt",
    );
    let staff = |n: u32, command: GroupCommand| Envelope {
        key: format!("staff-{n}"),
        caller: caller::STAFF.into(),
        actor: "staff-ana".into(),
        today,
        command,
    };
    let register = GroupCommand::RegisterScheme {
        scheme: SCHEME.into(),
        policy,
        group,
        employer: party("employer-1"),
        inception: today,
        product: Box::new(product()),
        terms: Box::new(terms()),
        founding: vec![Member {
            employee_id: "E001".into(),
            life: life("E001"),
            attrs: founder,
            joined: today,
            left: None,
            absence_since: None,
            party: Some(party("party-1")),
            fingerprint: None,
            auto_cap_minor: None,
        }],
        pools: vec![],
    };
    module.propose(staff(1, register)).await.unwrap().result.unwrap();
    let set = GroupCommand::SetMapping {
        scheme: SCHEME.into(),
        mapping: Box::new(named_mapping()),
    };
    module.propose(staff(2, set)).await.unwrap().result.unwrap();

    let file = "Emp No,First,Last,DOB,Category,Salary,Site\n\
E001,Ana,Founder,01/05/1980,staff,60000,akl\n\
E002,Bo,Ngata,02/06/1985,staff,60000,wlg\n\
E003,Cy,Patel,03/07/1975,executive,240000,wlg\n";
    driver
        .upload(caller::EMPLOYER, "admin-bo", SCHEME, "nov.csv", file.as_bytes().to_vec())
        .await
        .unwrap();
    assert_eq!(driver.read_files().await.unwrap().done, 0, "not scanned yet");
    fake.set_blob_tags("group-quarantine/harbour/nov.csv", &[("Malware Scanning scan result", "No threats found")]);

    let status = |module: ClusterModule| async move {
        let q = Question::File {
            scheme: SCHEME.into(),
            file: "nov.csv".into(),
        };
        module.ask(q).await.unwrap()["status"].as_str().unwrap_or_default().to_string()
    };
    let mut approved = false;
    for _ in 0..200 {
        driver.run_once().await.unwrap();
        let now = status(module.clone()).await;
        if now == "awaiting_approval" && !approved {
            approved = true;
            let approve = Envelope {
                key: "approve-1".into(),
                caller: caller::EMPLOYER.into(),
                actor: "admin-cy".into(),
                today,
                command: GroupCommand::ApproveFile {
                    scheme: SCHEME.into(),
                    file: "nov.csv".into(),
                },
            };
            module.propose(approve).await.unwrap().result.unwrap();
        }
        if now == "applied" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(status(module.clone()).await, "applied");
    let scheme = module.ask(Question::Scheme { scheme: SCHEME.into() }).await.unwrap();
    assert_eq!(scheme["members"].as_u64(), Some(3));
    // The module's group is fed the core's events by the host: the balance it shows is the core's.
    let mut outstanding = 0;
    for _ in 0..100 {
        outstanding = module.ask(Question::Scheme { scheme: SCHEME.into() }).await.unwrap()["outstanding_minor"]
            .as_i64()
            .unwrap();
        if outstanding == 112_62 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(outstanding, 112_62, "11.00 for the founder and 101.62 for the two joiners");
    assert!(fake.events_sent("group-events", 0) > 5);
    // Reinsurance is not hosted on this node and underwriting has no intake: their
    // entries wait, and only theirs.
    let pass = driver.deliver().await.unwrap();
    let waiting = module.ask(Question::Outbox { limit: 100, after: None }).await.unwrap();
    let kinds: Vec<&str> = waiting["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i[1].as_object().unwrap().keys().next().unwrap().as_str())
        .collect();
    assert!(
        !kinds.is_empty() && kinds.iter().all(|k| *k == "reinsurance" || *k == "underwriting"),
        "{kinds:?}"
    );
    assert_eq!(pass.failed as usize, kinds.len());

    node.stop().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
