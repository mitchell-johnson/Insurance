//! Rules that hold for every command, kind and event, and the module
//! reached over the cluster's routes.

mod support;

use std::collections::BTreeMap;
use std::time::Duration;

use axum::routing::post;
use axum::{Json, Router};
use ic_core::EventRecord;
use ic_documents::command::{ApprovalRole, Command, HoldScope, LegacyImport, Settings};
use ic_documents::driver::{ClusterCore, ClusterModule, CoreLink, DriverError, Module};
use ic_documents::model::CoreInstruction;
use ic_documents::model::{AttemptOutcome, Channel, DefectCause};
use ic_documents::module::Query;
use ic_documents::template::Part;
use ic_documents::triggers::triggers;
use ic_documents::{DocumentClass, DocumentKind};
use ic_raft::ClusterClient;
use serde_json::{json, Value};
use support::*;

fn every_command(kit: &Kit) -> Vec<(Command, &'static str)> {
    let today = kit.today();
    let t = tref("renewal", 1);
    let legacy = LegacyImport {
        document: "old".into(),
        kind: DocumentKind::PolicySchedule,
        policy: None,
        data_class: Default::default(),
        owner: party("aroha"),
        issued_on: today,
        language: "en".into(),
        content_sha256: "0".repeat(64),
        blob: "b".into(),
        wrapped_key: "k".into(),
        bytes: 1,
    };
    vec![
        (Command::ActivateSettings(Settings::default()), "activate_settings"),
        (
            Command::ActivateRetention {
                version: 1,
                rules: BTreeMap::new(),
            },
            "activate_retention",
        ),
        (
            Command::RegisterPart(Part {
                id: "p".into(),
                version: 1,
                bodies: BTreeMap::new(),
            }),
            "register_part",
        ),
        (
            Command::SubmitTemplate(Box::new(template("x", DocumentKind::Reminder, "Hello."))),
            "submit_template",
        ),
        (
            Command::ApproveTemplate {
                template: t.clone(),
                role: ApprovalRole::Legal,
            },
            "approve_template",
        ),
        (
            Command::RunRenderingTests {
                template: t.clone(),
                cases: vec![],
            },
            "run_rendering_tests",
        ),
        (
            Command::ActivateTemplate {
                template: t.clone(),
                effective_from: today,
            },
            "activate_template",
        ),
        (
            Command::DisableTemplate {
                template: t.clone(),
                reason: "r".into(),
                case: "c".into(),
            },
            "disable_template",
        ),
        (
            Command::RegisterPartyKey {
                party: party("zed"),
                wrapped_key: "w".into(),
            },
            "register_party_key",
        ),
        (Command::IssueDocument(Box::new(kit.renewal_spec("n9"))), "issue_document"),
        (Command::ImportLegacy(Box::new(legacy)), "import_legacy"),
        (
            Command::RecordSend {
                document: "n1".into(),
                party: party("aroha"),
                channel: Channel::Email,
                masked_destination: "m".into(),
                provider_reference: "r".into(),
            },
            "record_send",
        ),
        (
            Command::RecordOutcome {
                document: "n1".into(),
                party: party("aroha"),
                channel: Channel::Email,
                outcome: AttemptOutcome::Delivered,
            },
            "record_outcome",
        ),
        (
            Command::RetryDelivery {
                document: "n1".into(),
                party: party("aroha"),
                channels: vec![Channel::Email],
            },
            "retry_delivery",
        ),
        (Command::Tick, "tick"),
        (
            Command::RaiseNoticeDefect {
                defect: "d".into(),
                case: "c".into(),
                description: "x".into(),
                cause: DefectCause::Other,
                from: today,
                to: today,
                grace_days: None,
            },
            "raise_notice_defect",
        ),
        (
            Command::AttachDefectNotices {
                defect: "d".into(),
                notices: vec![],
            },
            "attach_defect_notices",
        ),
        (
            Command::ExcludeFromDefect {
                defect: "d".into(),
                notice: "n".into(),
                reason: "r".into(),
            },
            "exclude_from_defect",
        ),
        (
            Command::CloseNoticeDefect {
                defect: "d".into(),
                control: "c".into(),
            },
            "close_notice_defect",
        ),
        (Command::ObserveCoreEvents { partition: 1, records: vec![] }, "observe_core_events"),
        (
            Command::DismissTrigger {
                trigger: "t".into(),
                reason: "r".into(),
            },
            "dismiss_trigger",
        ),
        (Command::AckCoreInstructions { ids: vec![] }, "ack_core_instructions"),
        (Command::MarkPublished { through_seq: 0 }, "mark_published"),
        (
            Command::PlaceLegalHold {
                scope: HoldScope::Party(party("aroha")),
                case: "c".into(),
            },
            "place_legal_hold",
        ),
        (
            Command::ReleaseLegalHold {
                scope: HoldScope::Policy(policy("pol-1")),
            },
            "release_legal_hold",
        ),
        (Command::ShredParty { party: party("aroha") }, "shred_party"),
        (Command::AckDeleted { documents: vec![] }, "ack_deleted"),
    ]
}

/// Purpose: every command has a name, at least one service allowed to send
/// it, is refused by name from any other service, and survives the trip
/// through JSON that the log makes.
/// Why it matters: the caller check is the separation of duties between
/// services: only legal places a hold, only privacy erases a person. A
/// command with no check, or one that changed shape in the log, would be a
/// hole in that separation or a replica that cannot replay.
#[test]
fn every_command_is_named_guarded_and_survives_the_log() {
    let mut kit = Kit::ready();
    let commands = every_command(&kit);
    assert_eq!(commands.len(), 27);
    let names: std::collections::BTreeSet<&str> = commands.iter().map(|(_, n)| *n).collect();
    assert_eq!(names.len(), commands.len(), "no two commands share a name");
    for (command, name) in commands {
        assert_eq!(command.name(), name);
        assert!(!command.allowed_callers().is_empty());
        assert!(!command.allowed_callers().contains(&"nobody"));
        let through_log: Command = serde_json::from_value(serde_json::to_value(&command).unwrap()).unwrap();
        assert_eq!(through_log, command);
        let applied = kit.send("nobody", "someone", command);
        let reject = applied.result.unwrap_err();
        assert_eq!(reject.code, "not_allowed");
        assert!(reject.message.contains(name), "{}", reject.message);
        assert!(applied.events.is_empty());
    }
}

/// Purpose: every kind of document has a class, and proof of delivery and
/// compliance approval follow from it as the design says.
/// Why it matters: the class decides retention, the proof rule decides
/// whether post is forced as the last resort, and the approval rule decides
/// who must sign a template. A kind that fell into the wrong class would
/// get all three wrong without any error.
#[test]
fn every_kind_has_its_class_and_rules() {
    let mut by_class: BTreeMap<String, u32> = BTreeMap::new();
    for kind in DocumentKind::ALL {
        *by_class.entry(format!("{:?}", kind.class())).or_default() += 1;
        if kind.class() == DocumentClass::MandatoryNotice {
            assert!(kind.requires_proof() && kind.needs_compliance_approval());
        }
        if kind.requires_proof() {
            assert!(kind.needs_compliance_approval());
        }
    }
    assert_eq!(by_class.len(), 4);
    assert_eq!(by_class.values().sum::<u32>() as usize, DocumentKind::ALL.len());
    assert_eq!(DocumentKind::MigrationNotice.class(), DocumentClass::GeneratedEvidence);
    assert!(!DocumentKind::MigrationNotice.requires_proof());
    assert!(DocumentKind::MigrationNotice.needs_compliance_approval());
    assert!(DocumentKind::ComplaintOutcome.requires_proof() && DocumentKind::RemediationLetter.requires_proof());
    assert!(!DocumentKind::PolicySchedule.requires_proof() && !DocumentKind::PolicySchedule.needs_compliance_approval());
    assert_eq!(DocumentKind::Statement.class(), DocumentClass::Informational);
}

/// Purpose: each change to a policy's cover requires a new schedule, and an
/// event that changes nothing a customer holds requires no document.
/// Why it matters: the sequence of schedules is the policy's documentary
/// history. A change with no schedule leaves the customer holding a
/// document that no longer describes their cover.
#[test]
fn changes_to_cover_require_a_new_schedule() {
    let record = |seq: u64, event: Value| -> EventRecord {
        serde_json::from_value(json!({ "id": format!("3-{seq}"), "partition": 3, "seq": seq, "business_date": "2027-11-15", "log_index": seq, "event": event }))
            .unwrap()
    };
    let changes = [
        json!({ "anniversary_reached": { "policy": "pol-1", "number": 2, "on": "2027-11-15" } }),
        json!({ "layers_merged": { "policy": "pol-1", "cover": "life", "removed": [2], "into": 1, "sum_insured_minor": 50000000 } }),
        json!({ "cover_expired": { "policy": "pol-1", "cover": "tpd", "on": "2027-11-15" } }),
        json!({ "policy_reinstated": { "policy": "pol-1" } }),
        json!({ "layer_added": { "policy": "pol-1", "layer": { "cover": "life", "cover_type": "life", "layer": 2, "life": "life-1", "sum_insured_minor": 10000000, "start": "2027-11-15" }, "origin": "increase" } }),
    ];
    for (i, event) in changes.into_iter().enumerate() {
        let found = triggers(&record(i as u64 + 1, event));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, DocumentKind::PolicySchedule);
        assert_eq!(found[0].policy, policy("pol-1"));
        assert_eq!(found[0].id, format!("3-{}:PolicySchedule", i + 1));
    }
    let nothing = record(9, json!({ "indexation_choice_recorded": { "policy": "pol-1", "declined": true } }));
    assert!(triggers(&nothing).is_empty());
}

/// Purpose: the driver reaches the module through the cluster's routes: a
/// command comes back as what the module made of it, a question as its
/// answer, a module refusal and a cluster with no leader as errors that
/// say which.
/// Why it matters: in production every driver step goes over these two
/// routes. An unreachable cluster must read as "try again", never as a
/// refusal, or the driver would give up on a notice it should retry.
#[tokio::test]
async fn the_driver_reaches_the_module_over_the_cluster() {
    async fn commands(Json(command): Json<Value>) -> Json<Value> {
        if command["key"] == "undecodable" {
            return Json(json!({ "log_index": 4, "result": { "Err": "not a command of this module" } }));
        }
        if command["key"] == "odd" {
            return Json(json!({ "log_index": 4, "result": { "Ok": "not an output" } }));
        }
        Json(json!({ "log_index": 7, "result": { "Ok": { "index": 7, "result": { "Ok": null }, "events": [], "duplicate": false } } }))
    }
    async fn query(Json(question): Json<Value>) -> Json<Value> {
        if question == json!("settings") {
            Json(json!({ "Ok": { "version": 3 } }))
        } else {
            Json(json!({ "Err": "no such template" }))
        }
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let app = Router::new()
        .route("/v1/modules/documents/commands", post(commands))
        .route("/v1/modules/documents/query", post(query));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let kit = Kit::new();
    let envelope = |key: &str| ic_documents::Envelope {
        key: key.into(),
        caller: "delivery".into(),
        actor: "svc".into(),
        at: kit.now,
        command: Command::Tick,
    };
    let module = ClusterModule::new(ClusterClient::new(vec![addr]), Duration::from_millis(200));
    let applied = module.propose(envelope("tick:1")).await.unwrap();
    assert_eq!((applied.index, applied.duplicate), (7, false));
    assert_eq!(
        module.propose(envelope("undecodable")).await,
        Err(DriverError::Answer("not a command of this module".into()))
    );
    assert!(matches!(module.propose(envelope("odd")).await, Err(DriverError::Answer(_))));
    assert_eq!(module.ask(Query::Settings).await.unwrap(), json!({ "version": 3 }));
    assert_eq!(module.ask(Query::StateHash).await, Err(DriverError::Answer("no such template".into())));

    let nowhere = ClusterModule::new(ClusterClient::new(vec!["127.0.0.1:1".into()]), Duration::from_millis(50));
    assert!(matches!(nowhere.propose(envelope("tick:2")).await, Err(DriverError::Unavailable(m)) if m.contains("module documents")));
    assert!(matches!(nowhere.ask(Query::Settings).await, Err(DriverError::Unavailable(_))));
}

/// Purpose: an instruction for the core is sent to the partition that
/// holds the policy, as caller `documents` under a key made from the
/// instruction's own ID; a refusal by the core and a policy no partition
/// holds are errors that leave the instruction to be tried again.
/// Why it matters: this is how the core learns a lapse notice was
/// delivered and when a reopened choice may be made. Sent to the wrong
/// partition or under a changing key it would be lost or applied twice;
/// a refusal swallowed as success would be a proof of delivery the core
/// never has.
#[tokio::test]
async fn instructions_reach_the_partition_that_holds_the_policy() {
    use axum::extract::{Path, State};
    use std::sync::{Arc, Mutex};
    type Seen = Arc<Mutex<Vec<(u16, Value)>>>;

    async fn query(Path(partition): Path<u16>, Json(question): Json<Value>) -> Json<Value> {
        let policy = question["locate"]["policy"].as_str().unwrap_or_default().to_string();
        let held = (partition == 2 && policy == "pol-1") || (partition == 1 && policy == "pol-refused");
        if !held {
            return Json(Value::Null);
        }
        Json(
            json!({ "policy": policy, "number": "P-1", "group": "g-1", "partition": partition, "status": "in_force", "standing": null, "product_code": "term-life" }),
        )
    }
    async fn commands(State(seen): State<Seen>, Path(partition): Path<u16>, Json(envelope): Json<Value>) -> Json<Value> {
        seen.lock().unwrap().push((partition, envelope.clone()));
        let mut reply = ic_raft::Reply {
            log_index: 9,
            ..Default::default()
        };
        if partition == 1 {
            reply.rejected = Some(ic_core::Reject::new("reopen_conflict", "the reference was used with other terms"));
        }
        Json(serde_json::to_value(reply).unwrap())
    }
    let seen: Seen = Arc::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let app = Router::new()
        .route("/v1/partitions/{partition}/query", post(query))
        .route("/v1/partitions/{partition}/commands", post(commands))
        .with_state(seen.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let core = ClusterCore::new(ClusterClient::new(vec![addr]), 4, Duration::from_millis(200));
    let today = Kit::new().today();

    let delivery = CoreInstruction::RecordNoticeDelivery {
        policy: policy("pol-1"),
        purpose: "lapse".into(),
        notice: "n1".into(),
        delivered_on: today,
        earliest_effect: today + chrono::Duration::days(14),
    };
    core.instruct("notice-delivery:n1:pol-1", &delivery).await.unwrap();
    core.instruct("notice-delivery:n1:pol-1", &delivery).await.unwrap();
    let window = |p: &str| CoreInstruction::OpenReopenWindow {
        policy: policy(p),
        reopen_reference: "d1:n1".into(),
        purpose: "renewal".into(),
        from: today,
        until: today + chrono::Duration::days(30),
    };
    core.instruct("reopen:d1:n1:pol-1", &window("pol-1")).await.unwrap();
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 3);
        assert!(seen.iter().all(|(partition, _)| *partition == 2));
        assert_eq!(seen[0].1["key"], json!("documents:notice-delivery:n1:pol-1"));
        assert_eq!(seen[0].1["key"], seen[1].1["key"], "a repeat carries the same key");
        assert_eq!(seen[0].1["caller"], json!("documents"));
        assert_eq!(seen[0].1["command"]["record_notice_delivery"]["purpose"], json!("lapse"));
        assert_eq!(seen[2].1["command"]["open_reopen_window"]["reopen_reference"], json!("d1:n1"));
        assert!(!seen[0].1["salt"].as_str().unwrap().is_empty());
    }

    let refused = core.instruct("reopen:d1:n1:pol-refused", &window("pol-refused")).await.unwrap_err();
    assert!(refused.contains("reopen_conflict"), "{refused}");
    assert_eq!(
        core.instruct("reopen:d1:n1:pol-9", &window("pol-9")).await.unwrap_err(),
        "no partition holds policy pol-9"
    );

    let nowhere = ClusterCore::new(ClusterClient::new(vec!["127.0.0.1:1".into()]), 1, Duration::from_millis(50));
    assert!(nowhere.instruct("x", &delivery).await.is_err());
}
