//! Each letter goes to the contact point the person chose for the
//! relationship it is about, so one person with several roles is written
//! to in each role where they asked.

mod support;

use std::sync::{Arc, Mutex};

use ic_documents::channels::{Channels, Contact, MemoryDirectory, Outbound, SendError};
use ic_documents::command::NoticeSpec;
use ic_documents::crypto::{self, LocalWrap};
use ic_documents::driver::{Driver, IssueRequest, LocalModule, Module};
use ic_documents::model::{Channel, RecipientRole};
use ic_documents::vault::MemoryStore;
use support::*;

/// Accepts every message and remembers where it went.
#[derive(Clone, Default)]
struct Recording(Arc<Mutex<Vec<(String, String, String)>>>);

impl Channels for Recording {
    async fn send(&self, message: &Outbound) -> Result<String, SendError> {
        self.0
            .lock()
            .unwrap()
            .push((message.document.clone(), message.party.0.clone(), message.contact.destination.clone()));
        Ok(format!("ref-{}-{}", message.document, message.party))
    }
}

type TestDriver = Driver<LocalModule, MemoryStore, LocalWrap, Recording, MemoryDirectory>;

async fn driver() -> TestDriver {
    let module = LocalModule::default();
    let ready = Kit::ready();
    for envelope in &ready.log {
        assert!(module.propose(envelope.clone()).await.unwrap().result.is_ok());
    }
    let now = ready.now;
    Driver {
        module,
        objects: MemoryStore::default(),
        wrap: LocalWrap::new(crypto::new_key()),
        channels: Recording::default(),
        directory: MemoryDirectory::default(),
        actor: "documents-service".into(),
        clock: Arc::new(move || now),
        batch: 50,
    }
}

fn email(name: &str, address: &str) -> Contact {
    Contact {
        name: name.into(),
        destination: address.into(),
        ..Contact::default()
    }
}

/// A document to `to`, each as addressee under its relationship.
fn request(id: &str, template: &str, pol: &str, owner: &str, to: &[(&str, Option<&str>)]) -> IssueRequest {
    IssueRequest {
        document: id.into(),
        caller: "documents".into(),
        template: template.into(),
        template_version: None,
        policy: Some(policy(pol)),
        other_policies: vec![],
        data_class: Default::default(),
        lives: vec![],
        prove_delivery: false,
        owner: party(owner),
        recipients: to
            .iter()
            .map(|(p, relationship)| {
                let mut r = recipient(p, RecipientRole::Addressee, &[Channel::Email]);
                r.relationship = relationship.map(str::to_string);
                r
            })
            .collect(),
        language: "en".into(),
        format: None,
        data: sample_data(),
        trigger_event: None,
        case: None,
        supersedes: None,
        reason: None,
        wordings: vec![],
        components: vec![],
        notice: None,
    }
}

fn sent(driver: &TestDriver) -> Vec<(String, String, String)> {
    let mut sent = driver.channels.0.lock().unwrap().clone();
    sent.sort();
    sent
}

/// Purpose: one person who is a scheme member, a policyholder and a
/// claimant gets the scheme's premium document at the work address chosen
/// for the employment, the renewal notice at the home address chosen for
/// the family policy, and the claim letter at her own contact point,
/// because no relationship chose another for it.
/// Why it matters: a person with several hats must not have a claim
/// decision, which can carry health information, sent to an employer's
/// inbox because that is where scheme mail goes; nor miss a renewal
/// notice because it went to a work address she left.
#[tokio::test]
async fn one_person_is_written_to_in_each_role_where_she_chose() {
    let driver = driver().await;
    driver
        .directory
        .set_contact("mere", Channel::Email, email("Mere Parata", "mere@personal.example"));
    driver
        .directory
        .set_for("mere", "member", Channel::Email, email("Mere Parata", "mere.parata@employer.example"));
    driver
        .directory
        .set_for("mere", "policyholder", Channel::Email, email("Mere Parata", "parata.whanau@home.example"));

    driver
        .issue(request("scheme-1", "receipt", "pol-scheme", "mere", &[("mere", Some("member"))]))
        .await
        .unwrap();
    let mut renewal = request("renewal-1", "renewal", "pol-family", "mere", &[("mere", Some("policyholder"))]);
    renewal.trigger_event = Some("7-100".into());
    renewal.notice = Some(NoticeSpec {
        purpose: "renewal".into(),
        corrects: None,
    });
    driver.issue(renewal).await.unwrap();
    let mut claim = request("claim-1", "decision", "pol-family", "mere", &[("mere", Some("claimant"))]);
    claim.trigger_event = Some("9-200".into());
    driver.issue(claim).await.unwrap();

    assert_eq!(driver.deliver().await.unwrap().sent, 3);
    assert_eq!(
        sent(&driver),
        vec![
            ("claim-1".into(), "mere".into(), "mere@personal.example".into()),
            ("renewal-1".into(), "mere".into(), "parata.whanau@home.example".into()),
            ("scheme-1".into(), "mere".into(), "mere.parata@employer.example".into()),
        ]
    );
}

/// Purpose: a letter on a partnership's policy goes to each partner as an
/// addressee in their own right, each at the contact point they chose
/// for the partnership, while a letter on one partner's own policy goes to
/// her own; the same template serves both, with nobody treated as the
/// primary owner.
/// Why it matters: a co-owner is entitled to the notice in her own name.
/// A design that wrote only to a "first" owner would leave the other
/// unnotified of a lapse on cover she part owns, and a partnership letter
/// sent to a partner's home would put the business's affairs before her
/// family.
#[tokio::test]
async fn joint_owners_are_each_addressed_by_their_own_relationship() {
    let driver = driver().await;
    driver
        .directory
        .set_contact("mere", Channel::Email, email("Mere Parata", "mere@personal.example"));
    driver
        .directory
        .set_contact("hemi", Channel::Email, email("Hemi Walker", "hemi@personal.example"));
    driver
        .directory
        .set_for("mere", "partner", Channel::Email, email("Mere Parata", "mere@partnership.example"));

    driver
        .issue(request("own-1", "receipt", "pol-own", "mere", &[("mere", Some("policyholder"))]))
        .await
        .unwrap();
    driver
        .issue(request(
            "joint-1",
            "receipt",
            "pol-partnership",
            "mere",
            &[("mere", Some("partner")), ("hemi", Some("partner"))],
        ))
        .await
        .unwrap();
    assert_eq!(driver.deliver().await.unwrap().sent, 3);
    assert_eq!(
        sent(&driver),
        vec![
            ("joint-1".into(), "hemi".into(), "hemi@personal.example".into()),
            ("joint-1".into(), "mere".into(), "mere@partnership.example".into()),
            ("own-1".into(), "mere".into(), "mere@personal.example".into()),
        ]
    );
    let joint = driver.document("joint-1").await.unwrap();
    assert!(joint.deliveries.iter().all(|d| d.recipient.role == RecipientRole::Addressee));
}

/// Purpose: when a person changes the contact point their relationships
/// share, the next letter in every one of those relationships goes to the
/// new one; when they choose a different contact point for one
/// relationship, only that relationship's letters move.
/// Why it matters: contact points are held once, in the customer master,
/// and read at sending. A copy kept per policy would send the next notice
/// to an address the person told us they had left, and a change meant for
/// one relationship must not redirect letters about the others.
#[tokio::test]
async fn a_changed_contact_point_reaches_every_relationship_that_uses_it() {
    let driver = driver().await;
    let directory = &driver.directory;
    directory.set_contact("mere", Channel::Email, email("Mere Parata", "mere@old.example"));
    let send = |id: &str, relationship: &str| request(id, "receipt", "pol-own", "mere", &[("mere", Some(relationship))]);

    driver.issue(send("a", "policyholder")).await.unwrap();
    assert_eq!(driver.deliver().await.unwrap().sent, 1);

    directory.set_contact("mere", Channel::Email, email("Mere Parata", "mere@new.example"));
    driver.issue(send("b", "policyholder")).await.unwrap();
    driver.issue(send("c", "claimant")).await.unwrap();
    assert_eq!(driver.deliver().await.unwrap().sent, 2);

    directory.set_for("mere", "claimant", Channel::Email, email("Mere Parata", "mere@claims-only.example"));
    driver.issue(send("d", "claimant")).await.unwrap();
    driver.issue(send("e", "policyholder")).await.unwrap();
    assert_eq!(driver.deliver().await.unwrap().sent, 2);

    let to = |address: &str| address.to_string();
    let sent: Vec<(String, String)> = sent(&driver).into_iter().map(|(d, _, a)| (d, a)).collect();
    assert_eq!(
        sent,
        vec![
            ("a".into(), to("mere@old.example")),
            ("b".into(), to("mere@new.example")),
            ("c".into(), to("mere@new.example")),
            ("d".into(), to("mere@claims-only.example")),
            ("e".into(), to("mere@new.example")),
        ]
    );
}
