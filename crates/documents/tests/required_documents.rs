//! The documents the core's events require, answered from the policy the
//! partition holds: a real core issues the policy and reports it, and the
//! service issues the schedule, notices and receipts the events ask for.

mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::NaiveDate;
use ic_core::interest::InterestEvent;
use ic_core::{Event as CoreEvent, EventRecord, PolicyId, Standing};
use ic_documents::channels::{Channels, Contact, MemoryDirectory, Outbound, SendError};
use ic_documents::command::Command;
use ic_documents::complaint_letters::LetterPass;
use ic_documents::crypto::{self, LocalWrap};
use ic_documents::driver::{Driver, LocalModule, Module};
use ic_documents::kinds::DataClass;
use ic_documents::model::{Channel, RecipientRole};
use ic_documents::required_documents::{document_id, policy_data, run_required_documents, ClusterPolicies, Policies, PolicyRecord};
use ic_documents::template::AlternateFormat;
use ic_documents::vault::MemoryStore;
use ic_documents::DocumentKind;
use ic_product::{Frequency, Sex};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, term_life};
use ic_sim::Cluster;
use serde_json::{json, Value};
use support::*;

#[derive(Clone, Default)]
struct Quiet;

impl Channels for Quiet {
    async fn send(&self, message: &Outbound) -> Result<String, SendError> {
        Ok(format!("ref-{}-{}", message.document, message.party))
    }
}

type TestDriver = Driver<LocalModule, MemoryStore, LocalWrap, Quiet, MemoryDirectory>;

/// The core's partitions, read as the cluster would answer.
#[derive(Clone)]
struct Core {
    cluster: Arc<Mutex<Cluster>>,
    down: Arc<Mutex<bool>>,
}

impl Policies for Core {
    async fn policy(&self, policy: &PolicyId) -> Result<Option<PolicyRecord>, String> {
        if *self.down.lock().unwrap() {
            return Err("no leader".into());
        }
        let cluster = self.cluster.lock().unwrap();
        let Some(partition) = cluster.locate(policy) else { return Ok(None) };
        Ok(cluster.state(partition).policies.get(policy).map(|p| PolicyRecord {
            policy: p.clone(),
            outstanding_minor: Some(0),
            oldest_unpaid_due: None,
        }))
    }
}

const SCHEDULE: &str = "Schedule {{policy}} for {{name}}, {{product}} from {{inception|date}}.{{#each covers}} {{cover}} {{sum_insured|money}}.{{/each}}";
const WORDING: &str = "This policy pays the sum insured.";
const LAPSE: &str = "Dear {{name}}. Policy {{policy}} is unpaid since {{oldest_unpaid_due|date}}.";
const RECEIPT: &str = "Dear {{name}}. We received {{allocated|money}} for policy {{policy}} (receipt {{receipt}}).";

fn sample() -> Value {
    json!({
        "name": "Aroha", "policy": "KTL1", "product": "term-life", "inception": "2027-11-15",
        "covers": [{ "cover": "life", "sum_insured": 50000000 }],
        "oldest_unpaid_due": "2027-11-15", "allocated": 12345, "receipt": "rc-1"
    })
}

fn open() -> NaiveDate {
    NaiveDate::from_ymd_opt(2027, 11, 15).unwrap()
}

struct World {
    driver: Arc<TestDriver>,
    core: Core,
    next_seq: u64,
}

impl World {
    /// A core with one term life policy issued, a documents module with
    /// the templates given active, and the policyholder in the directory.
    async fn new(templates: &[(&str, DocumentKind, &str)]) -> Self {
        let mut kit = Kit::new();
        let now = kit.now;
        for (id, kind, body) in templates {
            let mut t = template(id, *kind, body);
            if *kind == DocumentKind::LapseNotice {
                t.minimum_notice_days = Some(14);
            }
            kit.activate(t, &sample());
        }
        let module = LocalModule::default();
        for envelope in &kit.log {
            assert!(module.propose(envelope.clone()).await.unwrap().result.is_ok());
        }
        let directory = MemoryDirectory::default();
        directory.set("party-00000001", Channel::Email, "Aroha Ngata", "aroha@example.nz");
        directory.set("lender", Channel::Post, "Kiwi Bank", "1 Queen Street\nAuckland");
        let driver = Arc::new(Driver {
            module,
            objects: MemoryStore::default(),
            wrap: LocalWrap::new(crypto::new_key()),
            channels: Quiet,
            directory,
            actor: "documents-driver".into(),
            clock: Arc::new(move || now),
            batch: 100,
        });
        let mut cluster = Cluster::in_memory(1, 7);
        cluster.open(open()).unwrap();
        for activation in shipped_activations().unwrap() {
            cluster.activate_everywhere(activation).unwrap();
        }
        let issue = term_life(
            1,
            open(),
            NaiveDate::from_ymd_opt(1985, 3, 2).unwrap(),
            Sex::Female,
            false,
            50_000_000,
            Some(10_000_000),
            Frequency::Monthly,
            None,
        );
        cluster.submit(0, "new-business", issue_command(issue)).unwrap();
        let core = Core {
            cluster: Arc::new(Mutex::new(cluster)),
            down: Default::default(),
        };
        let mut world = Self { driver, core, next_seq: 1 };
        world.relay().await;
        world
    }

    /// Hand the documents module what the core has said since last time.
    async fn relay(&mut self) {
        let records = self.core.cluster.lock().unwrap().take_events();
        self.next_seq = records.last().map_or(self.next_seq, |r| r.seq + 1);
        self.observe(0, records).await;
    }

    async fn observe(&self, partition: u16, records: Vec<EventRecord>) {
        let applied = self
            .driver
            .module
            .propose(ic_documents::Envelope {
                key: format!("observe-{partition}-{}", records.first().map_or(0, |r| r.seq)),
                caller: "relay".into(),
                actor: "relay".into(),
                at: chrono::Utc::now(),
                command: Command::ObserveCoreEvents { partition, records },
            })
            .await
            .unwrap();
        assert_eq!(applied.result, Ok(()));
    }

    async fn pass(&self) -> LetterPass {
        self.driver.required_documents(&self.core).await
    }
}

/// An event on a partition the test makes up, in order from 1.
fn event(seq: u64, event: CoreEvent) -> EventRecord {
    EventRecord {
        id: format!("9-{seq}"),
        partition: 9,
        seq,
        business_date: open(),
        log_index: seq,
        traceparent: None,
        event,
    }
}

fn the_policy() -> PolicyId {
    PolicyId("pol-00000001".into())
}

/// Purpose: a policy the core issues gets its schedule without anyone
/// asking: the service reads the policy from the partition, picks the
/// product's active schedule template and wording, and issues it to the
/// policyholder with the policy's number, product, start and each cover's
/// sum insured; the trigger is then answered and the next pass does
/// nothing.
/// Why it matters: until this step existed every schedule, arrears notice
/// and receipt the core's events required waited for a service that was
/// never built. A customer whose cover changed after a claim would hold a
/// schedule that still showed the old sums.
#[tokio::test]
async fn a_new_policy_gets_its_schedule_from_the_policy() {
    let w = World::new(&[
        ("schedule", DocumentKind::PolicySchedule, SCHEDULE),
        ("wording", DocumentKind::PolicyWording, WORDING),
    ])
    .await;
    let triggers = w.driver.module.state().triggers;
    let schedule = triggers.values().find(|t| t.kind == DocumentKind::PolicySchedule).unwrap().clone();

    let pass = w.pass().await;
    assert_eq!((pass.issued, pass.errors.clone()), (1, vec![]), "{pass:?}");
    let document = w.driver.document(&document_id(&schedule)).await.unwrap();
    assert_eq!(document.kind, DocumentKind::PolicySchedule);
    assert_eq!(document.wordings, vec![tref("wording", 1)]);
    assert_eq!(document.deliveries[0].recipient.party, party("party-00000001"));
    assert_eq!(document.deliveries[0].recipient.relationship.as_deref(), Some("policyholder"));
    let text = w.driver.read(&document.id).await.unwrap().text;
    assert_eq!(
        text,
        "Schedule KTL00000001 for Aroha Ngata, term-life from 15 November 2027. life $500,000.00. trauma $100,000.00."
    );
    assert!(w.driver.module.state().triggers.values().all(|t| t.kind != DocumentKind::PolicySchedule));
    assert_eq!(w.pass().await, LetterPass::default());
}

/// Purpose: an arrears notice goes as a notice with its clock, with the
/// date the oldest bill fell due, to the policyholder and a copy to the
/// lender the policy is assigned to; a receipt quotes the amount applied;
/// both are financial documents.
/// Why it matters: the arrears notice starts the clock the core waits on
/// before a policy can lapse, and a lender is entitled to hear its
/// security is at risk. Neither ever went out before this step.
#[tokio::test]
async fn an_arrears_notice_and_a_receipt_are_issued_with_their_facts() {
    let w = World::new(&[("lapse", DocumentKind::LapseNotice, LAPSE), ("receipt", DocumentKind::PremiumReceipt, RECEIPT)]).await;
    let lender = CoreEvent::Interest(InterestEvent::Recorded {
        owner: None,
        kind: Default::default(),
        policy: the_policy(),
        interest: "i-1".into(),
        party: party("lender"),
        notice_received: open(),
        extent_minor: None,
        effective_from: open(),
    });
    let grace = CoreEvent::StandingChanged {
        policy: the_policy(),
        from: Some(Standing::Paid),
        to: Some(Standing::InGrace),
        oldest_unpaid_due: Some(open()),
    };
    let receipt = CoreEvent::ReceiptApplied {
        policy: the_policy(),
        receipt: "rc-9".into(),
        allocated_minor: 4_250,
        to_suspense_minor: 0,
        net_by_cover_type: Default::default(),
        fund: None,
    };
    w.observe(9, vec![event(1, lender), event(2, grace), event(3, receipt)]).await;

    let pass = w.pass().await;
    assert_eq!(pass.issued, 2, "{pass:?}");
    let notice = w.driver.document("required:9-2:LapseNotice").await.unwrap();
    assert_eq!(notice.notice.as_ref().unwrap().purpose, "lapse");
    assert_eq!(notice.data_class, DataClass::Financial);
    let to: Vec<(String, RecipientRole)> = notice.deliveries.iter().map(|d| (d.recipient.party.0.clone(), d.recipient.role)).collect();
    assert_eq!(
        to,
        vec![
            ("party-00000001".to_string(), RecipientRole::Addressee),
            ("lender".to_string(), RecipientRole::InterestedParty)
        ]
    );
    assert_eq!(
        w.driver.read(&notice.id).await.unwrap().text,
        "Dear Aroha Ngata. Policy KTL00000001 is unpaid since 15 November 2027."
    );
    assert_eq!(
        w.driver.read("required:9-3:PremiumReceipt").await.unwrap().text,
        "Dear Aroha Ngata. We received $42.50 for policy KTL00000001 (receipt rc-9)."
    );
}

/// Purpose: a document whose product has no active template of its kind
/// is left and said once a pass however many policies wait for it; a
/// schedule with no wording is left the same way; a policyholder with no
/// contact point, or who has died, is left for a person; a policy no
/// partition holds, the core being down, and the module's question failing
/// are reported; and a trigger left is answered once the gap is filled.
/// Why it matters: the service must never invent wording or write to
/// nobody, but one missing template must not stop the other documents,
/// and every gap has to be visible to whoever runs the service.
#[tokio::test]
async fn documents_wait_for_templates_and_people_and_survive_outages() {
    let mut w = World::new(&[("schedule", DocumentKind::PolicySchedule, SCHEDULE)]).await;
    let avoided = |seq: u64, policy: &str| {
        event(
            seq,
            CoreEvent::PolicyAvoided {
                policy: PolicyId(policy.into()),
                case: "nd-1".into(),
                premium: ic_core::policy::PremiumOnAvoidance::Retain,
                refunded_minor: 0,
            },
        )
    };
    w.observe(9, vec![avoided(1, "pol-00000001"), avoided(2, "pol-00000001")]).await;
    let pass = w.pass().await;
    assert_eq!(pass.issued, 0);
    assert_eq!(pass.left, 3);
    assert_eq!(
        pass.errors,
        vec![
            "product term-life has no active AvoidanceNotice template; its documents are waiting".to_string(),
            "product term-life has no active PolicyWording template; its documents are waiting".to_string(),
        ]
    );

    w.observe(9, vec![avoided(3, "pol-nowhere")]).await;
    let pass = w.pass().await;
    assert!(
        pass.errors.iter().any(|e| e == "9-3:AvoidanceNotice: policy pol-nowhere was not found"),
        "{:?}",
        pass.errors
    );

    *w.core.down.lock().unwrap() = true;
    let pass = w.pass().await;
    assert!(pass.errors.iter().all(|e| e.contains("no leader")), "{:?}", pass.errors);
    *w.core.down.lock().unwrap() = false;

    w.driver.module.set_down(true);
    assert!(w.pass().await.errors[0].starts_with("triggers: "));
    w.driver.module.set_down(false);

    // The wording is activated: the schedule waits now for a person.
    let mut kit = Kit::new();
    kit.activate(template("wording", DocumentKind::PolicyWording, WORDING), &sample());
    for envelope in kit.log.iter().filter(|e| {
        matches!(
            e.command,
            Command::SubmitTemplate(_) | Command::ApproveTemplate { .. } | Command::RunRenderingTests { .. } | Command::ActivateTemplate { .. }
        )
    }) {
        let mut envelope = envelope.clone();
        envelope.key = format!("wording-{}", envelope.key);
        assert_eq!(w.driver.module.propose(envelope).await.unwrap().result, Ok(()));
    }
    w.driver.directory.set_contact(
        "party-00000001",
        Channel::Email,
        Contact {
            name: "Aroha Ngata".into(),
            destination: "aroha@example.nz".into(),
            deceased: true,
            ..Contact::default()
        },
    );
    let pass = w.pass().await;
    assert!(
        pass.errors
            .iter()
            .any(|e| e.ends_with("party-00000001 has died; the document is left for a person")),
        "{:?}",
        pass.errors
    );
    w.driver.directory.remove("party-00000001", Channel::Email);
    let pass = w.pass().await;
    assert!(
        pass.errors
            .iter()
            .any(|e| e.ends_with("party-00000001 has no contact point; the document is left for a person")),
        "{:?}",
        pass.errors
    );

    w.driver.directory.set("party-00000001", Channel::Email, "Aroha Ngata", "aroha@example.nz");
    assert_eq!(w.pass().await.issued, 1, "the schedule goes once it can");
    w.relay().await;
}

/// Purpose: a policyholder who asked for braille or audio still gets
/// their schedule in the ordinary format when the template is not
/// offered in theirs; large print, which any template can be set in,
/// goes as asked.
/// Why it matters: a missing alternate format must not hold back a
/// document the policyholder is owed; they get the ordinary one rather
/// than nothing.
#[tokio::test]
async fn a_format_the_template_lacks_falls_back_to_the_ordinary_one() {
    for (asked, sent) in [("braille", None), ("audio", None), ("large_print", Some(AlternateFormat::LargePrint))] {
        let w = World::new(&[
            ("schedule", DocumentKind::PolicySchedule, SCHEDULE),
            ("wording", DocumentKind::PolicyWording, WORDING),
        ])
        .await;
        w.driver.directory.set_contact(
            "party-00000001",
            Channel::Email,
            Contact {
                name: "Aroha Ngata".into(),
                destination: "aroha@example.nz".into(),
                format: Some(asked.into()),
                ..Contact::default()
            },
        );
        let schedule = w
            .driver
            .module
            .state()
            .triggers
            .values()
            .find(|t| t.kind == DocumentKind::PolicySchedule)
            .unwrap()
            .clone();
        let pass = w.pass().await;
        assert_eq!((pass.issued, pass.errors.clone()), (1, vec![]), "{asked}: {pass:?}");
        let document = w.driver.document(&document_id(&schedule)).await.unwrap();
        assert_eq!(document.format, sent, "{asked}");
    }
}

/// Purpose: the data a template is given carries the policy's facts and
/// covers in force on the date, and nothing about health: no loading and
/// no exclusion, even on a policy written with a loading.
/// Why it matters: a schedule or notice can be sent to a lender or an
/// employer; a loading implies a health condition and must never reach a
/// document built from these facts.
#[tokio::test]
async fn the_data_carries_the_policy_and_nothing_about_health() {
    let mut cluster = Cluster::in_memory(1, 7);
    cluster.open(open()).unwrap();
    for activation in shipped_activations().unwrap() {
        cluster.activate_everywhere(activation).unwrap();
    }
    let issue = term_life(
        2,
        open(),
        NaiveDate::from_ymd_opt(1970, 1, 1).unwrap(),
        Sex::Male,
        true,
        20_000_000,
        None,
        Frequency::Yearly,
        Some("0.5"),
    );
    cluster.submit(0, "new-business", issue_command(issue)).unwrap();
    let policy = cluster.state(0).policies[&PolicyId("pol-00000002".into())].clone();
    let data = policy_data(
        &PolicyRecord {
            policy,
            outstanding_minor: Some(1_000),
            oldest_unpaid_due: Some(open()),
        },
        open(),
    );
    assert_eq!(data["policy"], "KTL00000002");
    assert_eq!(data["covers"][0]["sum_insured"], 20_000_000);
    assert_eq!(data["outstanding"], 1_000);
    assert_eq!(data["oldest_unpaid_due"], "2027-11-15");
    assert!(data["premium"].as_i64().is_some_and(|p| p > 0), "{data}");
    let text = data.to_string();
    assert!(!text.contains("loading") && !text.contains("0.5") && !text.contains("exclusion"), "{text}");
}

/// Purpose: in production the step finds a policy over the cluster by
/// asking each partition whether it holds it and reading it by number;
/// a policy no partition holds is none, and a cluster nobody answers is
/// an error.
/// Why it matters: every required document goes over these routes. An
/// unreachable cluster must read as "try again", never as a policy that
/// does not exist.
#[tokio::test]
async fn the_step_reads_policies_over_the_cluster() {
    use axum::extract::Path;
    use axum::routing::post;
    use axum::{Json, Router};
    use ic_raft::ClusterClient;

    let mut cluster = Cluster::in_memory(1, 7);
    cluster.open(open()).unwrap();
    for activation in shipped_activations().unwrap() {
        cluster.activate_everywhere(activation).unwrap();
    }
    let issue = term_life(
        1,
        open(),
        NaiveDate::from_ymd_opt(1985, 3, 2).unwrap(),
        Sex::Female,
        false,
        50_000_000,
        None,
        Frequency::Monthly,
        None,
    );
    cluster.submit(0, "new-business", issue_command(issue)).unwrap();
    let policy = serde_json::to_value(&cluster.state(0).policies[&the_policy()]).unwrap();
    let entry = json!({ "policy": "pol-00000001", "number": "KTL00000001", "group": "grp-00000001", "partition": 1, "status": "in_force", "standing": null, "product_code": "term-life" });
    let answers = Arc::new((
        entry.clone(),
        json!({ "entry": entry, "policy": policy, "billing": { "outstanding_minor": 700, "oldest_unpaid_due": "2027-11-15" }, "business_date": "2027-11-15" }),
    ));
    let app = Router::new().route(
        "/v1/partitions/{partition}/query",
        post(move |Path(partition): Path<u16>, Json(query): Json<Value>| {
            let answers = answers.clone();
            async move {
                Json(match (partition, query.get("locate").is_some()) {
                    (0, _) => Value::Null,
                    (_, true) => answers.0.clone(),
                    (_, false) => answers.1.clone(),
                })
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let policies = ClusterPolicies::new(ClusterClient::new(vec![addr.clone()]), 2);
    let found = policies.policy(&the_policy()).await.unwrap().unwrap();
    assert_eq!(found.policy.number, "KTL00000001");
    assert_eq!((found.outstanding_minor, found.oldest_unpaid_due), (Some(700), Some(open())));
    let only_first = ClusterPolicies::new(ClusterClient::new(vec![addr]), 1);
    assert_eq!(only_first.policy(&the_policy()).await, Ok(None));
    let nowhere = ClusterPolicies::new(ClusterClient::new(vec!["127.0.0.1:1".into()]), 1);
    assert!(nowhere.policy(&the_policy()).await.is_err());
}

/// Purpose: the step runs a pass straight away and on every interval
/// until it is stopped.
/// Why it matters: a step that quietly stopped would leave every
/// schedule and arrears notice unsent while the triggers pile up.
#[tokio::test]
async fn the_step_runs_until_stopped() {
    let w = World::new(&[
        ("schedule", DocumentKind::PolicySchedule, SCHEDULE),
        ("wording", DocumentKind::PolicyWording, WORDING),
    ])
    .await;
    let passes = Arc::new(Mutex::new(vec![]));
    let seen = passes.clone();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(run_required_documents(
        w.core.clone(),
        w.driver.clone(),
        Duration::from_millis(5),
        async move {
            let _ = rx.await;
        },
        move |pass: &LetterPass| seen.lock().unwrap().push(pass.clone()),
    ));
    tokio::time::sleep(Duration::from_millis(40)).await;
    tx.send(()).unwrap();
    task.await.unwrap();
    let passes = passes.lock().unwrap().clone();
    assert!(passes.len() >= 2);
    assert_eq!(passes.iter().map(|p| p.issued).sum::<u32>(), 1);
}
