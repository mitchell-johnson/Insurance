//! The driver's production connections: Azure Blob Storage and Event Hubs
//! (against the stand-in that speaks their protocols), and the module and
//! the core on a real node.

mod common;
/// The customer master's test harness, for its stand-in Entra ID tenant and signed tokens.
// It reads the clock to date its tokens, which this crate forbids only for the state machine.
#[allow(clippy::disallowed_methods)]
#[path = "../../customers/tests/common/mod.rs"]
mod customer_master;

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
    event_message, excess_answer, follow_transfer, reinsurance_envelope, underwriting_envelope, underwriting_outcome, underwriting_withdrawal, ClusterCore,
    ClusterModule, ClusterPeers, QuarantineBlobs,
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
        // Bo may be somebody already known; everybody else is new.
        if identity.employee_id == "E002" {
            return Ok(Resolution::Possible {
                candidates: vec![party("party-known")],
            });
        }
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
        delegated_authority: None,
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

    // Takeover terms and reinsurance's reference go on the entry only where there are any,
    // and underwriting still reads the proposal with them there.
    let mut taken_over = excess_request(Some("party-9"));
    let plain = underwriting_envelope(key, at, today, &taken_over).unwrap();
    taken_over.honour_without_evidence_minor = Some(150_000_00);
    taken_over.reference = Some("ri-cap-77".into());
    let with_both = underwriting_envelope(key, at, today, &taken_over).unwrap();
    let entry = &with_both["command"]["open_proposal"]["entry"];
    assert_eq!(
        (entry["takeover_accepted_minor"].as_i64(), entry["reinsurance_reference"].as_str()),
        (Some(150_000_00), Some("ri-cap-77"))
    );
    serde_json::from_value::<ic_underwriting::command::Envelope>(with_both).unwrap();
    assert_eq!(plain["command"]["open_proposal"]["entry"].as_object().unwrap().len(), 2);
    // An amount of nothing or a blank reference is none at all: underwriting would refuse it.
    taken_over.honour_without_evidence_minor = Some(0);
    taken_over.reference = Some(" ".into());
    assert_eq!(underwriting_envelope(key, at, today, &taken_over).unwrap()["command"], plain["command"]);

    // A scheme whose broker underwrites names the authority on the entry.
    let mut delegated = excess_request(Some("party-9"));
    delegated.delegated_authority = Some("auth-harbour-broker".into());
    let entry = &underwriting_envelope(key, at, today, &delegated).unwrap()["command"]["open_proposal"]["entry"];
    assert_eq!(entry["delegated_authority"], "auth-harbour-broker");

    let value = underwriting_envelope(key, at, today, &excess_request(Some("party-9"))).unwrap();
    let text = value.to_string();
    assert!(!text.contains("salary") && !text.contains("category"), "{text}");
    let envelope: ic_underwriting::command::Envelope = serde_json::from_value(value).unwrap();
    assert!(envelope.command.allowed_callers().contains(&envelope.caller.as_str()));
    assert_eq!((envelope.key.as_str(), envelope.at, envelope.today), (key, at, today));
    let Uw::OpenProposal(open) = envelope.command else { panic!("not a proposal") };
    assert_eq!(open.proposal.0, key);
    assert_eq!(
        open.entry,
        Entry::GroupExcess {
            policy: open.policy.clone(),
            takeover_accepted_minor: None,
            reinsurance_reference: None,
        }
    );
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

    // What the core made of accepted terms goes back as the issuer's report.
    let issued = ic_group::event::IssueOutcome::Issued {
        life: life("E003"),
        cover: "group-life".into(),
        layer: 4,
    };
    let envelope: ic_underwriting::command::Envelope =
        serde_json::from_value(underwriting_outcome("grp-uw:harbour:north:7/excess-issued", at, today, key, &issued)).unwrap();
    assert!(envelope.command.allowed_callers().contains(&envelope.caller.as_str()));
    let Uw::PolicyIssued { proposal, layers } = envelope.command else {
        panic!("not an issue")
    };
    assert_eq!(proposal.0, key);
    assert_eq!(
        layers,
        vec![ic_underwriting::command::IssuedLayer {
            life: life("E003"),
            cover: "group-life".into(),
            layer: 4
        }]
    );
    let rejected = ic_group::event::IssueOutcome::Rejected {
        code: "exceeds_maximum".into(),
    };
    let envelope: ic_underwriting::command::Envelope =
        serde_json::from_value(underwriting_outcome("grp-uw:harbour:north:7/excess-rejected", at, today, key, &rejected)).unwrap();
    assert!(envelope.command.allowed_callers().contains(&envelope.caller.as_str()));
    assert!(matches!(envelope.command, Uw::IssueRejected { proposal, code } if proposal.0 == key && code == "exceeds_maximum"));
}

/// Purpose: of everything in underwriting's outbox, only terms and endings for a scheme
/// member's excess are read as answers; accepted terms carry the amount, loadings and
/// exclusions; a decline is a decline and a lapse or withdrawal is not proceeded with;
/// and the member's consent to the employer seeing a loading is taken only from
/// underwriting's own record of it, never assumed.
/// Why it matters: the outbox holds every other module's work as well. Reading an
/// individual policy's terms as a scheme decision would write cover nobody asked for,
/// and inventing consent would show a member's health loading on an employer's invoice.
#[test]
fn only_answers_to_scheme_excess_requests_are_read_from_underwriting() {
    use ic_underwriting::command::Effect;
    use ic_underwriting::model::{Entry, ProposalId};

    let policy = ic_core::PolicyId("pol-1".into());
    let excess = Entry::GroupExcess {
        policy: policy.clone(),
        takeover_accepted_minor: None,
        reinsurance_reference: None,
    };
    let terms = |entry: Entry, proposal: &str| Effect::ApplyTerms {
        key: format!("{proposal}/terms"),
        proposal: ProposalId(proposal.into()),
        entry,
        policy: policy.clone(),
        effective: d(2027, 11, 1),
        employer_may_see_loading: false,
        member_pays_loading: false,
        covers: vec![ic_core::command::CoverSpec {
            excess_option: None,
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
            member_pays_loading: false,
        }
    );
    // Consent is what underwriting recorded from the member's signed step, and absent means no.
    let mut consented = serde_json::to_value(terms(excess.clone(), "grp-uw:harbour:7")).unwrap();
    consented["apply_terms"]["employer_may_see_loading"] = serde_json::json!(true);
    assert!(matches!(
        excess_answer(&consented).unwrap().decision,
        ExcessDecision::Accepted {
            member_consents_to_itemising: true,
            ..
        }
    ));
    // A reissue with the member paying says so on the effect; absent means no.
    consented["apply_terms"]["member_pays_loading"] = serde_json::json!(true);
    assert!(matches!(
        excess_answer(&consented).unwrap().decision,
        ExcessDecision::Accepted { member_pays_loading: true, .. }
    ));
    consented["apply_terms"].as_object_mut().unwrap().remove("member_pays_loading");
    consented["apply_terms"].as_object_mut().unwrap().remove("employer_may_see_loading");
    assert!(matches!(
        excess_answer(&consented).unwrap().decision,
        ExcessDecision::Accepted {
            member_consents_to_itemising: false,
            ..
        }
    ));
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
            "distribution",
            "documents",
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
            health: None,
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
    // And the notices did what they are for. The possible match is on reinsurance's own
    // list of exceptions, so the member's cover is known not to be counted against the
    // other person's retention; and its books still agree with themselves.
    let reinsurance = |question: serde_json::Value| {
        let client = client.clone();
        async move { client.query_module("reinsurance", &question).await.unwrap().unwrap() }
    };
    let exceptions = reinsurance(serde_json::json!("exceptions")).await.to_string();
    assert!(exceptions.contains("possible_match:") && exceptions.contains("party-known"), "{exceptions}");
    assert_eq!(reinsurance(serde_json::json!("reconcile")).await, serde_json::json!([]));
    // Its outbox holds nothing for us, and withdrawing what it never opened is settled.
    assert_eq!(driver.collect_answers().await.unwrap(), Default::default());
    let answers = driver.peers.underwriting_answers(0, 10).await.unwrap();
    assert_eq!((answers.through, answers.more, answers.answers.len()), (0, false, 0));
    driver.peers.withdraw_underwriting("w-1", today, "grp-uw:harbour:999").await.unwrap();
    let nothing = ic_group::event::IssueOutcome::Rejected {
        code: "exceeds_maximum".into(),
    };
    driver.peers.underwriting_outcome("o-1", today, "grp-uw:harbour:999", &nothing).await.unwrap();

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
    let authority = customer_master::tenant().await;
    let token = customer_master::token_with(&authority, &["group-admin"], "api://group-schemes");
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
        .env("IC_GROUP_AUDIENCE", "api://group-schemes")
        .env("IC_GROUP_ENTRA_AUTHORITY", &authority)
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
        .bearer_auth(&token)
        .body(r#"{"ask":"scheme","scheme":"harbour"}"#)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(answer["members"].as_u64(), Some(3), "{answer}");
    // The probes the deployment uses, and the counters its passes feed.
    assert_eq!(http.get(format!("http://{listen}/readyz")).send().await.unwrap().status().as_u16(), 200);
    let mut counted = String::new();
    for _ in 0..200 {
        counted = http.get(format!("http://{listen}/metrics")).send().await.unwrap().text().await.unwrap();
        if counted.contains("ic_group_passes_total ") && !counted.contains("ic_group_passes_total 0\n") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!counted.contains("ic_group_passes_total 0\n"), "{counted}");
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
        assert!(tries < 2400, "ic-group ignored SIGTERM");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(exit.success(), "ic-group exited with {exit}");

    node.stop().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Purpose: a scheme cover's layer reaches the commission module as a `write_layer` it reads
/// and accepts from `group-admin`: layer 0, the broker's engagement and book taking the
/// whole, the agreed schedule, option and expected premium, never a replacement. Its key
/// carries the hour of the attempt; the module's answers mean written (also when an
/// earlier attempt wrote it), try again (the module has not yet seen the scheme in the
/// core, which a real commission module with no feed answers), or a refusal for a person.
/// Why it matters: the two modules are built apart and meet only in this JSON, and the
/// commission module keeps a refusal under its key for good. Without the hour, a layer sent
/// a moment before the commission module saw the scheme would never be written, and the
/// broker would never be paid.
#[test]
fn a_scheme_layer_is_a_command_the_commission_module_accepts() {
    use ic_distribution::{Actor, Command as Dist, DistributionState, Envelope as DistEnvelope};
    use ic_group::adapters::{commission_answer, commission_envelope};

    let layer = ic_group::event::CommissionLayer {
        policy: ic_core::PolicyId("pol-1".into()),
        cover: "group-life".into(),
        engagement: "eng-broker-x".into(),
        book: "x-schemes".into(),
        schedule: "standard".into(),
        option: "level".into(),
        annual_premium_minor: 24_000_00,
    };
    let at = chrono::DateTime::parse_from_rfc3339("2027-11-03T09:41:00Z").unwrap().to_utc();
    let value = commission_envelope("grp-dist:harbour:group-life", at, &layer);
    let envelope: DistEnvelope = serde_json::from_value(value.clone()).unwrap_or_else(|e| panic!("{value}: {e}"));
    assert_eq!(envelope.key, "grp-dist:harbour:group-life@20271103T09");
    assert_eq!(envelope.actor, Actor::Module("group-admin".into()));
    let Dist::WriteLayer(w) = &envelope.command else {
        panic!("{:?}", envelope.command)
    };
    assert_eq!(
        (w.layer, w.writing.len(), w.writing[0].1, w.books[0].1, w.replacement, w.annual_premium_minor),
        (0, 1, 1_000_000, 1_000_000, false, 24_000_00)
    );
    assert_eq!(
        (w.writing[0].0.to_string(), w.books[0].0.to_string()),
        ("eng-broker-x".into(), "x-schemes".into())
    );
    let later = at + chrono::Duration::hours(1);
    assert_ne!(commission_envelope("grp-dist:harbour:group-life", later, &layer)["key"], value["key"]);

    // A commission module that has not seen the scheme: try again, not a refusal for a person.
    let mut fresh = DistributionState::default();
    let output = fresh.apply_envelope(1, envelope);
    let answer = serde_json::to_value(&output).unwrap();
    assert!(output.result.is_err(), "{answer}");
    assert!(commission_answer(&answer).is_err(), "{answer}");
    // The other answers.
    assert_eq!(commission_answer(&serde_json::json!({"result": {"Ok": null}})), Ok(None));
    assert_eq!(commission_answer(&serde_json::json!({"result": {"Err": {"code": "layer_written"}}})), Ok(None));
    assert!(commission_answer(&serde_json::json!({"result": {"Err": {"code": "unknown_layer"}}})).is_err());
    assert!(
        commission_answer(&serde_json::json!({"Err": {"code": "unknown_book"}})).is_err(),
        "not the module's output"
    );
    assert_eq!(
        commission_answer(&serde_json::json!({"result": {"Err": {"code": "unknown_book"}}})),
        Ok(Some("unknown_book".to_string()))
    );
}

/// Purpose: a member's leave of absence letter, as the driver sends it, is a command the
/// documents module's own types read: caller `group`, the documents module's kind, the member
/// as the party addressed, the scheme policy and life, and the dates as the letter's detail;
/// and the documents module's real state takes it, with the answer read as taken.
/// Why it matters: GRP-038. A shape the documents module could not read would be refused
/// every pass, and the member abroad would never learn the day their cover ends.
#[test]
fn a_members_letter_is_a_command_the_documents_module_accepts() {
    use ic_documents::command::{Command as Docs, Envelope as DocsEnvelope};
    use ic_documents::state::DocumentsState;
    use ic_group::adapters::{letter_answer, letter_envelope};
    use ic_log::StateMachine;

    let since = chrono::NaiveDate::from_ymd_opt(2027, 11, 1).unwrap();
    let letter = ic_group::event::MemberLetter {
        request: format!("grp:absence_started:harbour:E002:{since}"),
        kind: "absence_started".into(),
        party: ic_core::PartyId("party-E002".into()),
        policy: ic_core::PolicyId("pol-1".into()),
        scheme: "harbour".into(),
        life: ic_core::LifeId("E002".into()),
        since,
        cover_ends: chrono::NaiveDate::from_ymd_opt(2028, 1, 1).unwrap(),
        business_date: since,
    };
    let at = chrono::DateTime::parse_from_rfc3339("2027-11-01T09:00:00Z").unwrap().to_utc();
    let value = letter_envelope(at, &letter);
    let envelope: DocsEnvelope = serde_json::from_value(value.clone()).unwrap_or_else(|e| panic!("{value}: {e}"));
    assert_eq!(
        (envelope.caller.as_str(), envelope.key.as_str()),
        ("group", "grp-letter:grp:absence_started:harbour:E002:2027-11-01")
    );
    let Docs::RequestLetter(request) = &envelope.command else {
        panic!("{:?}", envelope.command)
    };
    assert_eq!(request.kind, "absence_started");
    assert_eq!(request.detail["cover_ends"], "2028-01-01");
    assert_eq!(request.life.as_deref(), Some("E002"));

    let mut docs = DocumentsState::new();
    let output = docs.apply(1, envelope);
    let answer = serde_json::to_value(&output).unwrap();
    assert_eq!(letter_answer(&answer), Ok(None), "{answer}");
    // The same letter again is one letter.
    let again: DocsEnvelope = serde_json::from_value(letter_envelope(at + chrono::Duration::hours(1), &letter)).unwrap();
    assert_eq!(letter_answer(&serde_json::to_value(docs.apply(2, again)).unwrap()), Ok(None));
    // A refusal is for a person; anything that is not the module's answer is tried again.
    assert_eq!(
        letter_answer(&serde_json::json!({"result": {"Err": {"code": "unknown_letter_kind"}}})),
        Ok(Some("unknown_letter_kind".to_string()))
    );
    assert!(letter_answer(&serde_json::json!({"Ok": null})).is_err());
}

/// Purpose: a leaver's underwritten cover goes to underwriting in the shape agreed with that
/// thread on 13 Nov 2026: an `open_proposal` from `group-admin` named `grp-cont:{scheme}:{offer}`,
/// entry `group_continuation` with the continuation policy, the scheme policy and life, the
/// policy's inception and the earlier proposals, and one cover of the amount above the
/// automatic part; and underwriting's `proposal_ended` for it is read back as the outcome.
/// Why it matters: underwriting holds the terms, so this message is the only way the cover
/// carries on. A shape it could not read would leave every such leaver on the automatic
/// amount without anyone knowing.
#[test]
fn a_continued_cover_is_the_proposal_underwriting_agreed() {
    use ic_group::adapters::{continuation_answer, continuation_envelope};
    let day = chrono::NaiveDate::from_ymd_opt(2027, 12, 1).unwrap();
    let request = ic_group::event::ContinuationExcess {
        key: "grp-cont:harbour:7".into(),
        policy: ic_core::PolicyId("pol-cont-7".into()),
        group: ic_core::GroupId("grp-cont-7".into()),
        party: ic_core::PartyId("party-E004".into()),
        product_code: "term-life".into(),
        cover: "life".into(),
        cover_type: "life".into(),
        date_of_birth: Some(chrono::NaiveDate::from_ymd_opt(1980, 5, 1).unwrap()),
        sex: Some(ic_product::Sex::Female),
        smoker: Some(false),
        amount_minor: 150_000_00,
        inception: day,
        scheme_policy: ic_core::PolicyId("pol-1".into()),
        scheme_life: ic_core::LifeId("E004".into()),
        earlier: vec!["grp-uw:harbour:1".into()],
    };
    let at = chrono::DateTime::parse_from_rfc3339("2027-12-10T09:00:00Z").unwrap().to_utc();
    let v = continuation_envelope(at, day, &request);
    assert_eq!((v["key"].as_str(), v["caller"].as_str()), (Some("grp-cont:harbour:7"), Some("group-admin")));
    let p = &v["command"]["open_proposal"];
    assert_eq!(p["proposal"], "grp-cont:harbour:7");
    assert_eq!(p["entry"]["entry"], "group_continuation");
    assert_eq!(p["entry"]["policy"], "pol-cont-7");
    assert_eq!(p["entry"]["scheme_policy"], "pol-1");
    assert_eq!(p["entry"]["scheme_life"], "E004");
    assert_eq!(p["entry"]["inception"], "2027-12-01");
    assert_eq!(p["entry"]["earlier"][0], "grp-uw:harbour:1");
    assert_eq!(
        (p["policy"].as_str(), p["policyholder"].as_str(), p["channel"].as_str()),
        (Some("pol-cont-7"), Some("party-E004"), Some("group-continuation"))
    );
    assert_eq!(p["lives"][0]["life"], "L1");
    // Underwriting's own types read it as the continuation it expects.
    let open: ic_underwriting::command::OpenProposal = serde_json::from_value(p.clone()).unwrap_or_else(|e| panic!("{p}: {e}"));
    let ic_underwriting::model::Entry::GroupContinuation { inception, earlier, .. } = &open.entry else {
        panic!("{:?}", open.entry)
    };
    assert_eq!((*inception, earlier.len()), (day, 1));
    assert_eq!(
        (p["covers"][0]["code"].as_str(), p["covers"][0]["sum_insured_minor"].as_i64()),
        (Some("life"), Some(150_000_00))
    );

    let ended = |entry: &str, proposal: &str| serde_json::json!({"proposal_ended": {"key": "eff-9", "proposal": proposal, "entry": {"entry": entry}, "outcome": "issued"}});
    assert_eq!(
        continuation_answer(&ended("group_continuation", "grp-cont:harbour:7")),
        Some(("eff-9".into(), "harbour".into(), 7, "issued".into()))
    );
    assert_eq!(continuation_answer(&ended("group_excess", "grp-cont:harbour:7")), None);
    assert_eq!(continuation_answer(&ended("group_continuation", "grp-uw:harbour:7")), None);
}
