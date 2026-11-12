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
use ic_group::adapters::{
    event_message, excess_answer, follow_transfer, reinsurance_envelope, underwriting_envelope, underwriting_withdrawal, ClusterCore, ClusterModule,
    ClusterPeers, QuarantineBlobs,
};
use ic_group::command::{caller, Envelope, ExcessDecision, GroupCommand, Resolution};
use ic_group::driver::{CustomerMaster, Driver, FileStore, Module, Peers, Scan};
use ic_group::event::{GroupEvent, ReinsuranceNotice, UnderwritingRequest};
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

/// Purpose: a command goes to the group's home partition; only when that partition says
/// the policies were moved is it offered to the others, in turn, until one neither
/// disowns the policy nor has moved it on; if none takes it the first answer stands; and
/// a partition that cannot be reached is an error, not an answer.
/// Why it matters: a region's partitions are rebalanced by moving policies. A member
/// change sent only to the old home would be refused for ever, and one treated as
/// accepted when a partition was merely unreachable would be a member with no cover.
#[tokio::test]
async fn a_command_follows_a_policy_that_was_moved() {
    let run = |home: u16, answers: [Result<Option<&'static str>, &'static str>; 3]| async move {
        let asked = std::sync::Mutex::new(Vec::new());
        let answer = follow_transfer(home, 3, |p| {
            asked.lock().unwrap().push(p);
            let a = answers[p as usize];
            async move { a.map(|code| code.map(str::to_string)).map_err(str::to_string) }
        })
        .await;
        (answer, asked.into_inner().unwrap())
    };
    let moved = Some("policy_transferred");
    assert_eq!(run(1, [Ok(None), Ok(None), Ok(None)]).await, (Ok(None), vec![1]));
    assert_eq!(
        run(1, [Ok(None), Ok(Some("not_in_force")), Ok(None)]).await,
        (Ok(Some("not_in_force".into())), vec![1])
    );
    assert_eq!(run(1, [Ok(Some("unknown_policy")), Ok(moved), Ok(None)]).await, (Ok(None), vec![1, 0, 2]));
    assert_eq!(run(0, [Ok(moved), Ok(Some("frozen")), Ok(None)]).await, (Ok(Some("frozen".into())), vec![0, 1]));
    assert_eq!(
        run(2, [Ok(Some("unknown_policy")), Ok(moved), Ok(moved)]).await,
        (Ok(Some("policy_transferred".into())), vec![2, 0, 1])
    );
    assert_eq!(run(1, [Ok(None), Err("no leader"), Ok(None)]).await, (Err("no leader".into()), vec![1]));
    assert_eq!(run(1, [Err("no leader"), Ok(moved), Ok(None)]).await, (Err("no leader".into()), vec![1, 0]));
}

fn excess_request(party_id: Option<&str>) -> UnderwritingRequest {
    UnderwritingRequest {
        scheme: "harbour:north".into(),
        case: 7,
        policy: ic_core::PolicyId("pol-1".into()),
        life: life("E003"),
        party: party_id.map(party),
        group: ic_core::GroupId("grp-1".into()),
        policyholder: party("employer-1"),
        product_code: "group-life".into(),
        product_hash: "abc123".into(),
        cover: "group-life".into(),
        cover_type: "group_life".into(),
        date_of_birth: Some(d(1975, 7, 3)),
        sex: Some(ic_product::Sex::Male),
        smoker: None,
        excess_minor: 210_000_00,
        effective: d(2027, 11, 1),
        path: ic_group::terms::ExcessPath::UnderwrittenExcess,
        honour_without_evidence_minor: None,
        reference: None,
    }
}

/// Purpose: a request for a member's excess is a proposal the underwriting module accepts
/// from this caller, for one life and one cover of exactly the excess, named so that the
/// answer finds its case; a withdrawal is one too; and a member who is not yet a known
/// person makes no proposal at all.
/// Why it matters: the two modules meet only in this JSON. A wrong field would be a
/// member left without cover above the automatic amount with nothing failing anywhere;
/// salary or category leaking into the proposal would tell underwriting more than the
/// amount it is asked to decide.
#[test]
fn requests_are_proposals_the_underwriting_module_accepts() {
    use ic_underwriting::command::Command as Uw;
    use ic_underwriting::model::Entry;

    let at = chrono::DateTime::parse_from_rfc3339("2027-11-01T02:03:04Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let today = d(2027, 11, 1);
    let key = "grp-uw:harbour:north:7";
    assert_eq!(underwriting_envelope(key, at, today, &excess_request(None)), None);

    let value = underwriting_envelope(key, at, today, &excess_request(Some("party-9"))).unwrap();
    let text = value.to_string();
    assert!(!text.contains("salary") && !text.contains("category"), "{text}");
    let envelope: ic_underwriting::command::Envelope = serde_json::from_value(value).unwrap();
    assert!(envelope.command.allowed_callers().contains(&envelope.caller.as_str()));
    assert_eq!((envelope.key.as_str(), envelope.at, envelope.today), (key, at, today));
    let Uw::OpenProposal(open) = envelope.command else { panic!("not a proposal") };
    assert_eq!(open.proposal.0, key);
    assert_eq!(open.entry, Entry::GroupExcess { policy: open.policy.clone() });
    assert_eq!(
        (open.policy.0.as_str(), open.policyholder, open.product_code.as_str()),
        ("pol-1", party("employer-1"), "group-life")
    );
    assert_eq!(
        (open.quote.product_hash.as_str(), open.quote.premium_minor, open.quote.quoted_on),
        ("abc123", 0, today)
    );
    assert_eq!(open.lives.len(), 1);
    assert_eq!((open.lives[0].life.clone(), open.lives[0].party.clone()), (life("E003"), party("party-9")));
    assert_eq!(
        (
            open.lives[0].factors.date_of_birth,
            open.lives[0].factors.salary_minor,
            open.lives[0].factors.category.clone()
        ),
        (Some(d(1975, 7, 3)), None, None)
    );
    assert_eq!(open.covers.len(), 1);
    assert_eq!(
        (
            open.covers[0].code.as_str(),
            open.covers[0].cover_type.as_str(),
            open.covers[0].sum_insured_minor
        ),
        ("group-life", "group_life", 210_000_00)
    );
    assert!(ic_underwriting::codes::COVER_TYPES.contains(&open.covers[0].cover_type.as_str()));

    let withdrawal = underwriting_withdrawal("grp-uw:harbour:north:7/withdraw", at, today, key);
    let envelope: ic_underwriting::command::Envelope = serde_json::from_value(withdrawal).unwrap();
    assert!(envelope.command.allowed_callers().contains(&envelope.caller.as_str()));
    assert!(matches!(envelope.command, Uw::Withdraw { proposal } if proposal.0 == key));
}

/// Purpose: of everything in underwriting's outbox, only terms and endings for a scheme
/// member's excess are read as answers; accepted terms carry the amount, loadings and
/// exclusions; a decline is a decline and a lapse or withdrawal is not proceeded with;
/// and an answer never claims the member's consent to the employer seeing a loading.
/// Why it matters: the outbox holds every other module's work as well. Reading an
/// individual policy's terms as a scheme decision would write cover nobody asked for,
/// and inventing consent would show a member's health loading on an employer's invoice.
#[test]
fn only_answers_to_scheme_excess_requests_are_read_from_underwriting() {
    use ic_underwriting::command::Effect;
    use ic_underwriting::model::{Entry, ProposalId};

    let policy = ic_core::PolicyId("pol-1".into());
    let excess = Entry::GroupExcess { policy: policy.clone() };
    let terms = |entry: Entry, proposal: &str| Effect::ApplyTerms {
        key: format!("{proposal}/terms"),
        proposal: ProposalId(proposal.into()),
        entry,
        policy: policy.clone(),
        effective: d(2027, 11, 1),
        covers: vec![ic_core::command::CoverSpec {
            code: "group-life".into(),
            life: Some(life("E003")),
            sum_insured_minor: 200_000_00,
            loadings: vec![ic_money::DecStr::parse("0.5").unwrap()],
            exclusions: vec!["EX-BACK".into()],
        }],
    };
    let ended = |entry: Entry, proposal: &str, outcome: &str| Effect::ProposalEnded {
        key: format!("{proposal}/ended"),
        proposal: ProposalId(proposal.into()),
        entry,
        policy: policy.clone(),
        outcome: outcome.into(),
    };
    let read = |effect: Effect| excess_answer(&serde_json::to_value(effect).unwrap());

    let accepted = read(terms(excess.clone(), "grp-uw:harbour:north:7")).unwrap();
    assert_eq!(
        (accepted.key.as_str(), accepted.scheme.as_str(), accepted.case),
        ("grp-uw:harbour:north:7/terms", "harbour:north", 7)
    );
    assert_eq!(
        accepted.decision,
        ExcessDecision::Accepted {
            sum_insured_minor: 200_000_00,
            loadings: vec![ic_money::DecStr::parse("0.5").unwrap()],
            exclusions: vec!["EX-BACK".into()],
            member_consents_to_itemising: false,
        }
    );
    for (outcome, decision) in [
        ("declined", ExcessDecision::Declined),
        ("lapsed", ExcessDecision::NotProceeded),
        ("withdrawn", ExcessDecision::NotProceeded),
    ] {
        let answer = read(ended(excess.clone(), "grp-uw:harbour:7", outcome)).unwrap();
        assert_eq!(
            (answer.key.as_str(), answer.case, answer.decision),
            ("grp-uw:harbour:7/ended", 7, decision),
            "{outcome}"
        );
    }

    // Not ours: another entry point, another kind of effect, or a name we did not give.
    let increase = Entry::Increase { policy: policy.clone() };
    assert_eq!(read(terms(increase.clone(), "grp-uw:harbour:7")), None);
    assert_eq!(read(ended(increase, "grp-uw:harbour:7", "declined")), None);
    assert_eq!(
        read(Effect::ReleaseCapacity {
            reservation: "r".into(),
            reason: "x".into()
        }),
        None
    );
    for name in ["P1", "grp-uw:harbour", "grp-uw:harbour:seven"] {
        assert_eq!(read(ended(excess.clone(), name, "declined")), None, "{name}");
    }
    let mut no_cover = serde_json::to_value(terms(excess, "grp-uw:harbour:7")).unwrap();
    no_cover["apply_terms"]["covers"] = serde_json::json!([]);
    assert_eq!(excess_answer(&no_cover), None);
}

/// Purpose: on a real node, with the module in its own replicated group fed by the core's
/// event stream, an uploaded file goes from Blob Storage through the scan, approval and
/// matching to members in the core; the core's answers and events come back through the
/// cluster; events reach Event Hubs; and what has no receiver yet stays in the outbox.
/// Why it matters: every other test here runs the state machine in the test's own
/// process. This one is the deployed arrangement: commands as JSON through Raft, the
/// host's event feed, and the cluster client's routing. It is the test that fails if the
/// module cannot actually run where it is hosted. The shipped binary is run at the end
/// because it is what the container starts: one that ignored SIGTERM would be killed
/// mid-file on every rollout.
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
    let modules = vec![
        ic_group::module_spec().unwrap(),
        ic_reinsurance::host::module_spec().unwrap(),
        ic_underwriting::module_spec().unwrap(),
    ];
    let node = server::start_with(settings, modules).await.unwrap();
    let client = ClusterClient::new(vec![addr.clone()]).with_auth(auth);
    let patience = Duration::from_secs(20);
    // Before the core has opened a business date there is none, which is not an error.
    assert_eq!(
        ic_group::driver::Core::business_date(&ClusterCore::new(client.clone(), 1, patience)).await,
        Ok(None)
    );

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
            "underwriting",
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
    // Reinsurance took its notices. Underwriting is hosted with no rules active for the
    // product, so it refuses the proposal: the request waits, and only the request.
    let pass = driver.deliver().await.unwrap();
    let waiting = module.ask(Question::Outbox { limit: 100, after: None }).await.unwrap();
    let kinds: Vec<&str> = waiting["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i[1].as_object().unwrap().keys().next().unwrap().as_str())
        .collect();
    assert_eq!(kinds, vec!["underwriting"]);
    assert_eq!(pass.failed, 1);
    // Its outbox holds nothing for us, and withdrawing what it never opened is settled.
    assert_eq!(driver.collect_answers().await.unwrap(), Default::default());
    let answers = driver.peers.underwriting_answers(0, 10).await.unwrap();
    assert_eq!((answers.through, answers.more, answers.answers.len()), (0, false, 0));
    driver.peers.withdraw_underwriting("w-1", today, "grp-uw:harbour:999").await.unwrap();

    // The shipped binary: from its environment alone it joins the same cluster with a
    // workload identity, answers a portal's question, and stops on SIGTERM.
    let binary = || {
        let mut p = std::process::Command::new(env!("CARGO_BIN_EXE_ic-group"));
        p.env_clear()
            .envs(std::env::vars().filter(|(k, _)| k == "LLVM_PROFILE_FILE"))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        p
    };
    let refused = binary().output().unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("IC_CLUSTER_NODES"));

    let token_file = dir.join("federated-token");
    std::fs::write(&token_file, "a-projected-service-account-token").unwrap();
    let api_key = "service-key-0123456789abcdef0123456789";
    let listen = format!("127.0.0.1:{}", std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port());
    let mut child = binary()
        .env("IC_NODES", &addr)
        .env("IC_CLUSTER_TOKEN", "test-cluster-token-0123456789abcdef")
        .env("AZURE_FEDERATED_TOKEN_FILE", &token_file)
        .env("AZURE_TENANT_ID", "tenant-1")
        .env("AZURE_CLIENT_ID", "client-1")
        .env("AZURE_AUTHORITY_HOST", format!("{}/", fake.base))
        .env("IC_GROUP_QUARANTINE_ENDPOINT", &fake.base)
        .env("IC_GROUP_EVENT_HUBS_NAMESPACE", &fake.base)
        .env("IC_GROUP_CUSTOMER_MASTER", &fake.base)
        .env("IC_GROUP_CUSTOMER_MASTER_SCOPE", "api://customers/.default")
        .env("IC_GROUP_FINGERPRINT_SALT", "test-salt-0123456789abcdef0123456789")
        .env("IC_GROUP_API_KEY", api_key)
        .env("IC_GROUP_LISTEN", &listen)
        .env("IC_GROUP_CYCLE_SECONDS", "1")
        .spawn()
        .unwrap();
    let http = reqwest::Client::new();
    let mut tries = 0;
    while http.get(format!("http://{listen}/healthz")).send().await.is_err() {
        assert!(child.try_wait().unwrap().is_none(), "ic-group exited at start");
        tries += 1;
        assert!(tries < 1200, "ic-group did not start listening");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let answer: serde_json::Value = http
        .post(format!("http://{listen}/v1/questions"))
        .bearer_auth(api_key)
        .body(r#"{"ask":"scheme","scheme":"harbour"}"#)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(answer["members"].as_u64(), Some(3), "{answer}");
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
        assert!(tries < 400, "ic-group ignored SIGTERM");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(exit.success(), "ic-group exited with {exit}");

    node.stop().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
