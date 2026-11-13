//! One document in two languages: the same data, each saying which
//! language controls.

mod support;

use std::sync::Arc;

use ic_documents::channels::{Channels, MemoryDirectory, Outbound, SendError};
use ic_documents::command::{ApprovalRole, Command};
use ic_documents::crypto::{self, LocalWrap};
use ic_documents::driver::{Driver, IssueRequest, LocalModule, Module};
use ic_documents::model::{Channel, RecipientRole};
use ic_documents::vault::MemoryStore;
use ic_documents::DocumentKind;
use support::*;

#[derive(Clone, Default)]
struct Quiet;

impl Channels for Quiet {
    async fn send(&self, message: &Outbound) -> Result<String, SendError> {
        Ok(format!("ref-{}", message.document))
    }
}

/// Purpose: a schedule issued in Arabic and in English from the same data
/// carries the same data snapshot hash, the Arabic one ends with the
/// statement that the English version controls, and the English one
/// carries no such statement; a language the template lacks gets the
/// primary one, recorded as such.
/// Why it matters: a customer reading the Arabic schedule must be reading
/// the same cover as the English record the insurer relies on. If a
/// translation could be rendered from other data, or did not say which
/// version controls, a dispute over the wording would have two answers.
#[tokio::test]
async fn a_translation_is_rendered_from_the_same_data_and_says_what_controls() {
    let mut kit = Kit::ready();
    let mut t = template("schedule-2", DocumentKind::PolicySchedule, "Schedule for {{name}}. Premium {{premium|money}}.");
    t.bodies.insert("ar".into(), "جدول {{name}}. القسط {{premium|money}}.".into());
    t.controlling_statements.insert("ar".into(), "النسخة الإنجليزية هي المعتمدة.".into());
    let r = t.reference();
    kit.ok("config", "author", Command::SubmitTemplate(Box::new(t.clone())));
    kit.approve(&r, &[ApprovalRole::Legal, ApprovalRole::Product]);
    let en = kit.case_for(&t, &sample_data());
    let mut ar = en.clone();
    ar.language = "ar".into();
    ar.expected_sha256 = ic_documents::template::sha256_hex(t.render(&kit.state.parts, "ar", &ar.data).unwrap().as_bytes());
    kit.ok(
        "config",
        "author",
        Command::RunRenderingTests {
            template: r.clone(),
            cases: vec![en, ar],
        },
    );
    let today = kit.today();
    kit.ok(
        "config",
        "product",
        Command::ActivateTemplate {
            template: r,
            effective_from: today,
        },
    );

    let module = LocalModule::default();
    for envelope in &kit.log {
        assert!(module.propose(envelope.clone()).await.unwrap().result.is_ok());
    }
    let now = kit.now;
    let driver = Driver {
        module,
        objects: MemoryStore::default(),
        wrap: LocalWrap::new(crypto::new_key()),
        channels: Quiet,
        directory: MemoryDirectory::default(),
        actor: "documents-service".into(),
        clock: Arc::new(move || now),
        batch: 50,
    };
    let request = |id: &str, language: &str| IssueRequest {
        document: id.into(),
        caller: "documents".into(),
        template: "schedule-2".into(),
        template_version: None,
        policy: Some(policy("pol-1")),
        other_policies: vec![],
        data_class: Default::default(),
        lives: vec![],
        prove_delivery: false,
        owner: party("mere"),
        recipients: vec![recipient("mere", RecipientRole::Addressee, &[Channel::Email])],
        language: language.into(),
        format: None,
        data: sample_data(),
        trigger_event: None,
        case: None,
        supersedes: None,
        reason: None,
        wordings: vec![tref("wording", 1)],
        components: vec![],
        notice: None,
    };
    let english = driver.issue(request("s-en", "en")).await.unwrap();
    let arabic = driver.issue(request("s-ar", "ar")).await.unwrap();
    assert_eq!(english.data_sha256, arabic.data_sha256);
    assert!(english.data_sha256.is_some());
    assert_eq!((english.language.as_str(), arabic.language.as_str()), ("en", "ar"));
    let arabic_text = driver.read("s-ar").await.unwrap().text;
    assert!(arabic_text.starts_with("جدول Aroha."));
    assert!(arabic_text.ends_with("النسخة الإنجليزية هي المعتمدة."));
    assert!(!driver.read("s-en").await.unwrap().text.contains("المعتمدة"));
    // A language the template has no wording in is served in the primary
    // one, and recorded as such.
    let french = driver.issue(request("s-fr", "fr")).await.unwrap();
    assert_eq!(french.language, "en");
    assert_eq!(french.data_sha256, english.data_sha256);
}
