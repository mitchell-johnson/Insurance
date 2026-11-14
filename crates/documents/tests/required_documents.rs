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
use ic_documents::command::{ApprovalRole, Command};
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
    /// Cover code to the sum its layers now hold, as a claim leaves them.
    cut: Arc<Mutex<std::collections::BTreeMap<String, i64>>>,
    /// Further changes to the policy as the core holds it.
    edit: Arc<Mutex<Option<Box<dyn Fn(&mut ic_core::policy::Policy) + Send>>>>,
}

impl Policies for Core {
    async fn policy(&self, policy: &PolicyId) -> Result<Option<PolicyRecord>, String> {
        if *self.down.lock().unwrap() {
            return Err("no leader".into());
        }
        let cluster = self.cluster.lock().unwrap();
        let Some(partition) = cluster.locate(policy) else { return Ok(None) };
        let cut = self.cut.lock().unwrap().clone();
        Ok(cluster.state(partition).policies.get(policy).map(|p| {
            let mut p = p.clone();
            for (cover, sum) in &cut {
                for layer in &mut p.covers.get_mut(cover).unwrap().layers {
                    layer.sum_insured_minor = *sum;
                }
            }
            if let Some(edit) = self.edit.lock().unwrap().as_ref() {
                edit(&mut p);
            }
            PolicyRecord {
                policy: p,
                outstanding_minor: Some(0),
                oldest_unpaid_due: None,
            }
        }))
    }
}

const SCHEDULE: &str = "Schedule {{policy}} for {{name}}, {{product}} from {{inception|date}}.{{#each covers}} {{cover}} {{sum_insured|money}}.{{/each}}";
const WORDING: &str = "This policy pays the sum insured.";
const LAPSE: &str = "Dear {{name}}. Policy {{policy}} is unpaid since {{oldest_unpaid_due|date}}.";
const PREMIUM: &str = "Premium for {{policy}}, {{period_start|date}} to {{period_end|date}}: {{base|money}}.{{#each discounts}} {{name}} -{{amount|money}}.{{/each}} Premium {{net_premium|money}}, charges {{charges|money}}, total {{total|money}}.";
const DISCOUNT: &str = "Dear {{name}}. Your {{tier}} discount on {{policy}} ends on {{from|date}}.";
const REINSTATED: &str = "Dear {{name}}. Your cover on {{policy}} is back from {{effective|date}}.{{#each covers}} {{cover}} {{sum_insured|money}}.{{/each}} Billed {{billed|money}}.";
const RECEIPT: &str = "Dear {{name}}. We received {{allocated|money}} for policy {{policy}} (receipt {{receipt}}).";

fn sample() -> Value {
    json!({
        "name": "Aroha", "policy": "KTL1", "product": "term-life", "inception": "2027-11-15",
        "covers": [{ "cover": "life", "sum_insured": 50000000 }],
        "oldest_unpaid_due": "2027-11-15", "allocated": 12345, "receipt": "rc-1",
        "period_start": "2027-11-15", "period_end": "2027-12-14", "base": 5000,
        "discounts": [{ "name": "Multi-policy", "amount": 500 }], "net_premium": 4500, "charges": 0, "total": 4500,
        "tier": "gold", "from": "2028-01-15", "effective": "2027-12-01", "billed": 1200
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
        let templates = templates
            .iter()
            .map(|(id, kind, body)| {
                let mut t = template(id, *kind, body);
                if *kind == DocumentKind::LapseNotice {
                    t.minimum_notice_days = Some(14);
                }
                t
            })
            .collect();
        Self::with(templates).await
    }

    /// The same, with the templates as written.
    async fn with(templates: Vec<ic_documents::template::Template>) -> Self {
        let mut kit = Kit::new();
        let now = kit.now;
        for t in templates {
            if t.bodies.len() == 1 {
                kit.activate(t, &sample());
                continue;
            }
            // A template with translations is tested in every language.
            let r = t.reference();
            kit.ok("config", "author", Command::SubmitTemplate(Box::new(t.clone())));
            kit.approve(&r, &[ApprovalRole::Legal, ApprovalRole::Product, ApprovalRole::Compliance]);
            let en = kit.case_for(&t, &sample());
            let cases = t
                .bodies
                .keys()
                .map(|language| {
                    let text = t.render(&kit.state.parts, language, &sample()).unwrap();
                    ic_documents::command::RenderCase {
                        language: language.clone(),
                        expected_sha256: ic_documents::template::sha256_hex(text.as_bytes()),
                        ..en.clone()
                    }
                })
                .collect();
            kit.ok("config", "author", Command::RunRenderingTests { template: r.clone(), cases });
            let today = kit.today();
            kit.ok(
                "config",
                "product",
                Command::ActivateTemplate {
                    template: r,
                    effective_from: today,
                },
            );
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
            cut: Default::default(),
            edit: Default::default(),
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

/// Purpose: a partial trauma payment that cuts the trauma cover and the
/// life cover linked to it (two `LayerReduced` the same day) is answered
/// with one new schedule, issued from the policy as the core then holds
/// it, showing both reduced sums, with the wording it is issued under.
/// Why it matters: after a claim the customer must be told what cover is
/// left (CLM-009); a schedule still showing the old sums would promise a
/// benefit already paid, and two schedules for one claim would confuse.
#[tokio::test]
async fn a_partial_trauma_payment_brings_one_new_schedule_with_the_cover_left() {
    let w = World::new(&[
        ("schedule", DocumentKind::PolicySchedule, SCHEDULE),
        ("wording", DocumentKind::PolicyWording, WORDING),
    ])
    .await;
    assert_eq!(w.pass().await.issued, 1, "the schedule at issue");
    // $25,000 paid on the $100,000 trauma cover; the life cover it
    // accelerates loses the same.
    w.core
        .cut
        .lock()
        .unwrap()
        .extend([("trauma".to_string(), 7_500_000), ("life".to_string(), 47_500_000)]);
    let paid_on = open() + chrono::Duration::days(40);
    let reduced = |seq: u64, cover: &str, before: i64, kept: i64| EventRecord {
        business_date: paid_on,
        ..event(
            seq,
            CoreEvent::LayerReduced {
                policy: the_policy(),
                cover: cover.into(),
                layer: 1,
                continued_as: 2,
                effective: paid_on,
                sum_insured_minor: before,
                kept_minor: kept,
            },
        )
    };
    w.observe(9, vec![reduced(1, "trauma", 10_000_000, 7_500_000), reduced(2, "life", 50_000_000, 47_500_000)])
        .await;
    let schedules: Vec<_> = w
        .driver
        .module
        .state()
        .triggers
        .values()
        .filter(|t| t.kind == DocumentKind::PolicySchedule)
        .cloned()
        .collect();
    assert_eq!(schedules.len(), 1, "one schedule for the day's changes");
    let pass = w.pass().await;
    assert_eq!((pass.issued, pass.errors.clone()), (1, vec![]), "{pass:?}");
    let document = w.driver.document(&document_id(&schedules[0])).await.unwrap();
    assert_eq!(document.wordings, vec![tref("wording", 1)]);
    assert_eq!(
        w.driver.read(&document.id).await.unwrap().text,
        "Schedule KTL00000001 for Aroha Ngata, term-life from 15 November 2027. life $475,000.00. trauma $75,000.00."
    );
}

/// Purpose: a scheme member whose own cover goes into grace is sent the
/// lapse notice themselves, as a notice whose purpose names their life
/// (`member_lapse:<life>`), with no copy to the scheme's lender; when the
/// cover is reinstated they are sent a confirmation showing their own
/// cover and nobody else's; a member linked to no person is left for one.
/// Why it matters: the core holds a member's cover until this notice is
/// proven delivered, so it must reach the member, not the employer; and
/// one member's letter must never show another member's cover.
#[tokio::test]
async fn a_scheme_member_is_warned_and_told_of_reinstatement_themselves() {
    let w = World::new(&[
        ("lapse", DocumentKind::LapseNotice, LAPSE),
        ("reinstated", DocumentKind::Endorsement, REINSTATED),
    ])
    .await;
    *w.core.edit.lock().unwrap() = Some(Box::new(|p: &mut ic_core::policy::Policy| {
        let mut record = p.lives.values().next().unwrap().clone();
        for (id, linked) in [("life-7", Some("member-7")), ("life-8", None)] {
            record.id = ic_core::LifeId(id.into());
            record.kind = ic_core::policy::LifeKind::UnnamedMember {
                member_ref: id.into(),
                linked_party: linked.map(|l| ic_core::PartyId(l.into())),
            };
            p.lives.insert(record.id.clone(), record.clone());
        }
        let life = p.covers.get_mut("life").unwrap();
        let mut layer = life.layers[0].clone();
        layer.id = 9;
        layer.life = ic_core::LifeId("life-7".into());
        layer.sum_insured_minor = 20_000_000;
        life.layers.push(layer);
    }));
    w.driver.directory.set("member-7", Channel::Email, "Mere Tipene", "mere@example.nz");
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
    let grace = |seq: u64, life: &str| {
        event(
            seq,
            CoreEvent::MemberStandingChanged {
                policy: the_policy(),
                life: ic_core::LifeId(life.into()),
                from: Some(Standing::Paid),
                to: Some(Standing::InGrace),
                oldest_unpaid_due: Some(open()),
            },
        )
    };
    let reinstated = event(
        4,
        CoreEvent::MemberCoverReinstated {
            policy: the_policy(),
            life: ic_core::LifeId("life-7".into()),
            reference: "ri-7".into(),
            layers: vec![("life".into(), 9)],
            effective: NaiveDate::from_ymd_opt(2027, 12, 1).unwrap(),
            arrears_minor: 3_000,
            billed_minor: 1_200,
        },
    );
    w.observe(9, vec![event(1, lender), grace(2, "life-7"), grace(3, "life-8"), reinstated]).await;
    let pass = w.pass().await;
    assert_eq!(pass.issued, 2, "{pass:?}");
    assert!(pass.errors.iter().any(|e| e.contains("member life-8 is linked to no person")), "{pass:?}");

    let notice = w.driver.document("required:9-2:LapseNotice").await.unwrap();
    assert_eq!(notice.owner, party("member-7"));
    assert_eq!(notice.notice.as_ref().unwrap().purpose, "member_lapse:life-7");
    assert_eq!(notice.lives, ["life-7"]);
    let to: Vec<_> = notice
        .deliveries
        .iter()
        .map(|d| (d.recipient.party.0.clone(), d.recipient.relationship.clone()))
        .collect();
    assert_eq!(
        to,
        vec![("member-7".to_string(), Some("member".to_string()))],
        "the scheme's lender is not copied"
    );
    assert_eq!(
        w.driver.read(&notice.id).await.unwrap().text,
        "Dear Mere Tipene. Policy KTL00000001 is unpaid since 15 November 2027."
    );
    let letter = w.driver.document("required:9-4:Endorsement").await.unwrap();
    assert_eq!(letter.owner, party("member-7"));
    assert_eq!(
        w.driver.read(&letter.id).await.unwrap().text,
        "Dear Mere Tipene. Your cover on KTL00000001 is back from 1 December 2027. life $200,000.00. Billed $12.00."
    );
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

/// Purpose: each bill on a policy of its own gets a premium notice from
/// the billing event: the premium before discounts, each discount by
/// name, the premium, the charges and the total, which is the bill's;
/// a product with no premium notice template sends none, and the bill's
/// trigger is dismissed rather than left waiting; a scheme's bill and a
/// credit raise none.
/// Why it matters: the notice must agree with the receivable to the cent,
/// and a product that sends no premium notices (paid by direct debit,
/// say) must not pile up a waiting document every month.
#[tokio::test]
async fn a_bill_gets_a_premium_notice_with_its_discount_lines() {
    let bill = |seq: u64, members: Option<u32>, total: i64| {
        event(
            seq,
            CoreEvent::PremiumDue {
                policy: the_policy(),
                receivable: 2,
                period_start: open(),
                period_end: NaiveDate::from_ymd_opt(2027, 12, 14).unwrap(),
                total_minor: total,
                members,
                by_cover_type: [("life".to_string(), 4_000), ("trauma".to_string(), 500)].into(),
                discounts: vec![ic_core::event::DiscountGiven {
                    code: "multi".into(),
                    name: "Multi-policy".into(),
                    posting_type: "discount".into(),
                    amount_minor: 500,
                }],
            },
        )
    };
    let w = World::new(&[("premium", DocumentKind::Invoice, PREMIUM)]).await;
    w.observe(9, vec![bill(1, None, 4_635), bill(2, Some(12), 9_000), bill(3, None, -100)]).await;
    let pass = w.pass().await;
    let notice = w.driver.document("required:9-1:Invoice").await.unwrap();
    assert_eq!(notice.data_class, DataClass::Financial);
    assert_eq!(
        w.driver.read(&notice.id).await.unwrap().text,
        "Premium for KTL00000001, 15 November 2027 to 14 December 2027: $50.00. Multi-policy -$5.00. Premium $45.00, charges $1.35, total $46.35."
    );
    // The policy's first bill and this one; nothing for the scheme or the
    // credit. Only the policy's schedule waits, for its template.
    let waiting = vec!["product term-life has no active PolicySchedule template; its documents are waiting".to_string()];
    assert_eq!((pass.issued, pass.left, pass.errors.clone()), (2, 1, waiting.clone()), "{pass:?}");

    let w = World::new(&[]).await;
    let pass = w.pass().await;
    assert_eq!((pass.issued, pass.left, pass.errors.clone()), (0, 2, waiting.clone()), "{pass:?}");
    assert!(w.driver.module.state().triggers.values().all(|t| t.kind != DocumentKind::Invoice));
    let pass = w.pass().await;
    assert_eq!((pass.issued, pass.left, pass.errors), (0, 1, waiting), "the bill is not asked about again");
}

/// Purpose: a discount lost from a future date (a wellness tier
/// withdrawn) is told to the policyholder as a premium change notice
/// with its own clock, naming the tier and the date; a tier awarded or
/// kept asks for nothing.
/// Why it matters: the premium goes up from the first bill on or after
/// that date, and the customer must hear of it before then (DSC-004).
/// Only the tier code may reach the letter, never what earned or lost it.
#[tokio::test]
async fn a_lost_discount_is_noticed_before_the_bill_it_raises() {
    let w = World::new(&[("discount", DocumentKind::PremiumChangeNotice, DISCOUNT)]).await;
    let changed = |seq: u64, held: bool| {
        event(
            seq,
            CoreEvent::EntitlementChanged {
                policy: the_policy(),
                code: "wellness".into(),
                tier: Some("gold".into()),
                life: None,
                held,
                from: NaiveDate::from_ymd_opt(2028, 1, 15).unwrap(),
                until: None,
                reference: "wl-7".into(),
            },
        )
    };
    w.observe(9, vec![changed(1, true), changed(2, false)]).await;
    let triggers = w.driver.module.state().triggers;
    assert_eq!(triggers.values().filter(|t| t.kind == DocumentKind::PremiumChangeNotice).count(), 1);
    w.pass().await;
    let notice = w.driver.document("required:9-2:PremiumChangeNotice").await.unwrap();
    assert_eq!(notice.notice.as_ref().unwrap().purpose, "discount_change");
    assert_eq!(notice.data_class, DataClass::Financial);
    assert_eq!(
        w.driver.read(&notice.id).await.unwrap().text,
        "Dear Aroha Ngata. Your gold discount on KTL00000001 ends on 15 January 2028."
    );
}

/// Purpose: a customer served by post only, with an attorney who may see
/// what they are sent, gets the arrears notice by post with proof of
/// delivery and the clock, and the attorney is a recipient of that same
/// notice; an attorney not yet in force, or whose authority has ended,
/// is not.
/// Why it matters: a customer in prison or in care can lose cover through
/// a notice nobody reads; the representative seeing it, within the scope
/// the customer master holds, is what keeps the policy in order (VUL-019).
#[tokio::test]
async fn a_post_only_customer_and_their_representative_both_get_the_notice() {
    let w = World::new(&[("lapse", DocumentKind::LapseNotice, LAPSE)]).await;
    w.driver.directory.remove("party-00000001", Channel::Email);
    w.driver
        .directory
        .set("party-00000001", Channel::Post, "Aroha Ngata", "Private Bag 1\nRimutaka");
    w.driver.directory.set("attorney-1", Channel::Post, "Hemi Ngata", "PO Box 1\nWellington");
    w.driver.directory.set("attorney-2", Channel::Post, "Former Attorney", "PO Box 2\nWellington");
    w.driver.directory.set("attorney-3", Channel::Post, "Future Attorney", "PO Box 3\nWellington");
    w.driver
        .directory
        .set_representative("party-00000001", "attorney-1", NaiveDate::from_ymd_opt(2027, 1, 1).unwrap(), None);
    w.driver
        .directory
        .set_representative("party-00000001", "attorney-2", NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(), Some(open()));
    w.driver
        .directory
        .set_representative("party-00000001", "attorney-3", NaiveDate::from_ymd_opt(2028, 1, 1).unwrap(), None);
    let grace = CoreEvent::StandingChanged {
        policy: the_policy(),
        from: Some(Standing::Paid),
        to: Some(Standing::InGrace),
        oldest_unpaid_due: Some(open()),
    };
    w.observe(9, vec![event(1, grace)]).await;
    let pass = w.pass().await;
    assert_eq!(pass.issued, 1, "{pass:?}");
    let notice = w.driver.document("required:9-1:LapseNotice").await.unwrap();
    assert!(notice.requires_proof());
    assert_eq!(notice.notice.as_ref().unwrap().purpose, "lapse");
    let to: Vec<(String, RecipientRole)> = notice.deliveries.iter().map(|d| (d.recipient.party.0.clone(), d.recipient.role)).collect();
    assert_eq!(
        to,
        vec![
            ("party-00000001".to_string(), RecipientRole::Addressee),
            ("attorney-1".to_string(), RecipientRole::Representative)
        ]
    );

    // The customer master is down: nothing is issued, and the next pass
    // tries again.
    w.driver.directory.set_down(true);
    let grace = CoreEvent::StandingChanged {
        policy: the_policy(),
        from: Some(Standing::Paid),
        to: Some(Standing::InGrace),
        oldest_unpaid_due: Some(open()),
    };
    w.observe(9, vec![event(2, grace)]).await;
    let pass = w.pass().await;
    assert!(pass.errors.iter().any(|e| e.contains("down")), "{pass:?}");
}

/// Purpose: a representative who reads another language gets the
/// document as a copy in their own language, rendered from the same data
/// as the original, naming its event but with no clock of its own, and in
/// the ordinary format when theirs is not offered; a representative with
/// no contact point, who has died, or already a recipient gets nothing more;
/// the policyholder's notice keeps its clock and clears the trigger.
/// Why it matters: a household where one reads Mandarin and the other
/// English must both understand what the insurer says, and say the same
/// thing to each (VUL-021); a second clock would let a translation decide
/// when cover lapses.
#[tokio::test]
async fn a_representative_gets_the_notice_in_their_own_language() {
    let mut lapse = template("lapse", DocumentKind::LapseNotice, LAPSE);
    lapse.minimum_notice_days = Some(14);
    lapse
        .bodies
        .insert("zh".into(), "{{name}}，您的保单 {{policy}} 自 {{oldest_unpaid_due|date}} 起未付。".into());
    lapse.controlling_statements.insert("zh".into(), "如有不同，以英文版本为准。".into());
    let w = World::with(vec![lapse]).await;
    w.driver.directory.set_contact(
        "lin",
        Channel::Email,
        Contact {
            name: "Lin Chen".into(),
            destination: "lin@example.nz".into(),
            language: Some("zh".into()),
            ..Contact::default()
        },
    );
    w.driver
        .directory
        .set_representative("party-00000001", "lin", NaiveDate::from_ymd_opt(2027, 1, 1).unwrap(), None);
    // Lin reads braille, which the template is not offered in: her copy
    // goes in the ordinary format rather than not at all.
    w.driver.directory.set_contact(
        "lin",
        Channel::Email,
        Contact {
            name: "Lin Chen".into(),
            destination: "lin@example.nz".into(),
            language: Some("zh".into()),
            format: Some("braille".into()),
            ..Contact::default()
        },
    );
    // A representative with no contact point, one who has died, and one
    // already a recipient get nothing more.
    let since = NaiveDate::from_ymd_opt(2027, 1, 1).unwrap();
    w.driver.directory.set_representative("party-00000001", "nowhere", since, None);
    w.driver.directory.set_contact(
        "gone",
        Channel::Email,
        Contact {
            name: "Gone".into(),
            destination: "gone@example.nz".into(),
            deceased: true,
            ..Contact::default()
        },
    );
    w.driver.directory.set_representative("party-00000001", "gone", since, None);
    w.driver.directory.set_representative("party-00000001", "party-00000001", since, None);
    let grace = CoreEvent::StandingChanged {
        policy: the_policy(),
        from: Some(Standing::Paid),
        to: Some(Standing::InGrace),
        oldest_unpaid_due: Some(open()),
    };
    w.observe(9, vec![event(1, grace)]).await;
    let pass = w.pass().await;
    assert_eq!(pass.issued, 1, "{pass:?}");
    let original = w.driver.document("required:9-1:LapseNotice").await.unwrap();
    let copy = w.driver.document("required:9-1:LapseNotice:copy:lin").await.unwrap();
    assert_eq!((original.language.as_str(), copy.language.as_str()), ("en", "zh"));
    assert_eq!(original.data_sha256, copy.data_sha256, "both render from the same data");
    assert!(original.notice.is_some() && copy.notice.is_none());
    assert_eq!(copy.trigger_event, original.trigger_event);
    assert_eq!(copy.deliveries.len(), 1);
    assert_eq!(copy.format, None);
    assert_eq!(original.deliveries.len(), 1, "nobody else is added to the original");
    assert_eq!(copy.deliveries[0].recipient.role, RecipientRole::Addressee);
    assert_eq!(copy.deliveries[0].recipient.relationship.as_deref(), Some("representative"));
    // The same data: the letter is about the policyholder, in Lin's language.
    assert!(w.driver.read(&copy.id).await.unwrap().text.contains("Aroha Ngata，您的保单 KTL00000001"));
    assert!(w.driver.module.state().triggers.values().all(|t| t.kind != DocumentKind::LapseNotice));
    // Only the policy's schedule still waits, for its template.
    assert_eq!(w.pass().await.issued, 0);
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
    // Two avoidance letters, the schedule waiting for its wording, and the
    // first bill's premium notice, which a product with no template for it
    // does not send.
    assert_eq!(pass.left, 4);
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
