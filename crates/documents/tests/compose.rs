//! The seam for an external document service: what it is sent, what it
//! answers, and that the documents module's controls (approved template
//! versions, the filled data, the encrypted vault and the issue record)
//! stay in force whatever composes the document.

#[path = "support/azure.rs"]
mod azure;
mod support;

use std::sync::{Arc, Mutex};

use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use azure::Fake;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use ic_documents::channels::{AzureChannels, Channels, MemoryDirectory, Outbound, SendError};
use ic_documents::command::{ApprovalRole, Command};
use ic_documents::compose::{composed_from, ComposeError, Composer, ExternalComposer, ExternalSettings, Rendition};
use ic_documents::crypto::{self, LocalWrap};
use ic_documents::driver::{Driver, DriverError, IssueRequest, LocalModule, Module};
use ic_documents::model::{Channel, RecipientRole};
use ic_documents::pdf::Fonts;
use ic_documents::render::RenderError;
use ic_documents::template::{sha256_hex, TemplateError};
use ic_documents::vault::{MemoryStore, ObjectStore};
use serde_json::{json, Value};
use support::*;

const PDF: &[u8] = b"%PDF-1.7\n% composed by the document service\n%%EOF\n";

/// Channels that remember what they were asked to send.
#[derive(Clone, Default)]
struct Recording {
    sent: Arc<Mutex<Vec<Outbound>>>,
}

impl Channels for Recording {
    async fn send(&self, message: &Outbound) -> Result<String, SendError> {
        self.sent.lock().unwrap().push(message.clone());
        Ok(format!("ref-{}", message.document))
    }
}

/// A stand-in document service: it records each request and gives the
/// answer the test sets.
#[derive(Clone)]
struct Service {
    base: String,
    asked: Arc<Mutex<Vec<(HeaderMap, Value)>>>,
    answer: Arc<Mutex<(u16, Value)>>,
}

impl Service {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let service = Self {
            base: format!("http://{}/", listener.local_addr().unwrap()),
            asked: Arc::default(),
            answer: Arc::new(Mutex::new((200, composed_answer()))),
        };
        let state = service.clone();
        let app = Router::new().route(
            "/v1/compose",
            post(move |headers: HeaderMap, Json(body): Json<Value>| {
                let state = state.clone();
                async move {
                    state.asked.lock().unwrap().push((headers, body));
                    let (status, answer) = state.answer.lock().unwrap().clone();
                    (StatusCode::from_u16(status).unwrap(), Json(answer))
                }
            }),
        );
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        service
    }

    fn answer(&self, status: u16, answer: Value) {
        *self.answer.lock().unwrap() = (status, answer);
    }

    fn asked(&self) -> Vec<(HeaderMap, Value)> {
        self.asked.lock().unwrap().clone()
    }

    fn composer(&self) -> Composer {
        let settings = ExternalSettings {
            base: self.base.clone(),
            scope: "api://compose/.default".into(),
        };
        Composer::from_settings(Some(&settings), Fake::credential())
    }
}

fn composed_answer() -> Value {
    json!({
        "text": "Kia ora Aroha. We received $123.45. Thank you.",
        "pdf": STANDARD.encode(PDF),
        "composer": { "name": "Compose Platform", "version": "24.3" },
    })
}

type TestDriver = Driver<LocalModule, MemoryStore, LocalWrap, Recording, MemoryDirectory>;

/// The usual active templates, and a driver composing through `composer`.
async fn driver(composer: Composer) -> TestDriver {
    let module = LocalModule::default();
    let ready = Kit::ready();
    // Templates only: the parties get their keys from the driver.
    for envelope in ready.log.iter().filter(|e| !matches!(e.command, Command::RegisterPartyKey { .. })) {
        assert!(module.propose(envelope.clone()).await.unwrap().result.is_ok());
    }
    let directory = MemoryDirectory::default();
    directory.set("aroha", Channel::Email, "Aroha Ngata", "aroha@example.nz");
    let now = ready.now;
    Driver {
        module,
        objects: MemoryStore::default(),
        wrap: LocalWrap::new(crypto::new_key()),
        channels: Recording::default(),
        directory,
        actor: "documents-service".into(),
        clock: Arc::new(move || now),
        batch: 50,
        composer,
    }
}

fn receipt(id: &str) -> IssueRequest {
    IssueRequest {
        document: id.into(),
        caller: "documents".into(),
        template: "receipt".into(),
        template_version: None,
        policy: Some(policy("pol-1")),
        other_policies: vec![],
        data_class: Default::default(),
        lives: vec![],
        prove_delivery: false,
        owner: party("aroha"),
        recipients: vec![recipient("aroha", RecipientRole::Addressee, &[Channel::Email])],
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
        copy_of: None,
    }
}

/// Purpose: with an external composer, the service is sent the approved
/// template version, its wording, the data and the filled text, under the
/// service's own token and an idempotency key; what it answers is the
/// document issued, stored, downloaded, verified and sent, and the record
/// says what composed it.
/// Why it matters: this is the seam itself. If the text or PDF the service
/// set were not the one stored and sent, the customer would get one
/// document and the record would hold another; if the record did not say
/// what composed it, an issued letter could not be traced back.
#[tokio::test]
async fn an_external_composition_is_the_document_issued() {
    let service = Service::start().await;
    let driver = driver(service.composer()).await;
    let document = driver.issue(receipt("r-1")).await.unwrap();

    let asked = service.asked();
    assert_eq!(asked.len(), 1);
    let (headers, body) = &asked[0];
    assert_eq!(headers["authorization"], format!("Bearer {}", azure::TOKEN));
    let template = document.template.clone().unwrap();
    let hash = document.template_hash.clone().unwrap();
    assert_eq!(
        headers["idempotency-key"],
        sha256_hex(format!("r-1|{hash}|en").as_bytes()),
        "the same document is composed once however often it is asked"
    );
    assert_eq!(body["document"], json!("r-1"));
    assert_eq!(body["template"], json!({ "id": "receipt", "version": template.version, "content_hash": hash }));
    assert_eq!(body["kind"], json!("premium_receipt"));
    assert_eq!(body["language"], json!("en"));
    assert_eq!(body["source"], json!("We received {{premium|money}}. Thank you."));
    assert_eq!(body["data"], sample_data());
    assert_eq!(body["filled"], json!("We received $123.45. Thank you."));

    assert_eq!(
        document.rendition,
        Some(Rendition {
            composer: "Compose Platform".into(),
            version: "24.3".into(),
            pdf_sha256: Some(sha256_hex(PDF)),
        })
    );
    let text = "Kia ora Aroha. We received $123.45. Thank you.";
    assert_eq!(
        document.content_sha256,
        sha256_hex(text.as_bytes()),
        "the record holds the composed text's hash"
    );
    let contents = driver.read("r-1").await.unwrap();
    assert_eq!(contents.text, text);
    assert_eq!(contents.data, sample_data());
    assert_eq!(driver.pdf("r-1", &Fonts::bundled()).await.unwrap(), PDF, "the download is the PDF as composed");
    assert!(driver.verify("r-1").await.unwrap());

    // Asked again, it is the same document, and the service is not asked.
    assert_eq!(driver.issue(receipt("r-1")).await.unwrap(), document);
    assert_eq!(service.asked().len(), 1);

    let sends = driver.deliver().await.unwrap();
    assert_eq!(sends.sent, 1);
    let sent = driver.channels.sent.lock().unwrap().clone();
    assert_eq!(sent[0].text, text);
    assert_eq!(sent[0].pdf.as_deref(), Some(PDF));
    assert_eq!(sent[0].pdf(&Fonts::bundled()).unwrap(), PDF, "the attachment is the PDF as composed");
}

/// Purpose: an answer with text and no PDF is issued as text, and its PDF
/// is set from that text when it is downloaded, as for the built-in
/// renderer.
/// Why it matters: many communications platforms compose only the text
/// of an email or SMS. The seam must not require a PDF it does not need.
#[tokio::test]
async fn a_composition_without_a_pdf_is_set_from_its_text() {
    let service = Service::start().await;
    service.answer(
        200,
        json!({ "text": "Kia ora Aroha.", "composer": { "name": "Compose Platform", "version": "24.3" } }),
    );
    let driver = driver(service.composer()).await;
    let document = driver.issue(receipt("r-1")).await.unwrap();
    assert_eq!(document.rendition.as_ref().unwrap().pdf_sha256, None);
    let pdf = driver.pdf("r-1", &Fonts::bundled()).await.unwrap();
    assert!(pdf.starts_with(b"%PDF-"));
    assert_ne!(pdf, PDF);
    assert!(driver.verify("r-1").await.unwrap());
}

/// Purpose: the built-in renderer, the default, issues the filled template
/// as the document and records no rendition.
/// Why it matters: a deployment that sets nothing must keep composing
/// documents as before, and its records must not claim an outside
/// composer.
#[tokio::test]
async fn the_built_in_renderer_is_the_default() {
    assert!(matches!(Composer::default(), Composer::BuiltIn));
    assert!(matches!(Composer::from_settings(None, Fake::credential()), Composer::BuiltIn));
    let driver = driver(Composer::from_settings(None, Fake::credential())).await;
    let document = driver.issue(receipt("r-1")).await.unwrap();
    assert_eq!(document.rendition, None);
    let contents = driver.read("r-1").await.unwrap();
    assert_eq!(contents.text, "We received $123.45. Thank you.");
    assert_eq!(contents.pdf, None);
    assert!(driver.verify("r-1").await.unwrap());
}

/// Purpose: a composer that is down, overloaded or slow leaves nothing
/// issued and nothing stored, and the same request succeeds once it is
/// back.
/// Why it matters: a mandatory notice must go eventually. An outage that
/// left a half-issued document, or one that turned a passing failure into
/// a refusal, would lose the notice or need someone to put it right.
#[tokio::test]
async fn an_unavailable_composer_issues_nothing_and_can_be_retried() {
    let service = Service::start().await;
    let driver = driver(service.composer()).await;
    for status in [503, 429, 408, 500] {
        service.answer(status, json!({}));
        let e = driver.issue(receipt("r-1")).await.unwrap_err();
        assert!(
            matches!(&e, DriverError::Unavailable(why) if why.contains(&status.to_string())),
            "{status}: {e}"
        );
    }
    assert!(matches!(driver.document("r-1").await, Err(DriverError::NotFound(_))));
    assert!(driver.objects.names().is_empty(), "nothing was stored");

    service.answer(200, composed_answer());
    assert!(driver.issue(receipt("r-1")).await.unwrap().rendition.is_some());

    // A composer that cannot be reached at all is unavailable too.
    let nowhere = ExternalSettings {
        base: "http://127.0.0.1:1".into(),
        scope: "api://compose/.default".into(),
    };
    let unreachable = self::driver(Composer::External(ExternalComposer::new(&nowhere, Fake::credential()))).await;
    assert!(matches!(unreachable.issue(receipt("r-2")).await, Err(DriverError::Unavailable(_))));
}

/// Purpose: a composer that refuses the document, or answers something
/// that is not a document, is a refusal: nothing is issued and the reason
/// is kept.
/// Why it matters: trying again will not help, so the caller must see a
/// refusal to put in front of someone, not a retry loop; and an answer
/// that is not a document must never be stored as one.
#[tokio::test]
async fn a_refusing_composer_issues_nothing() {
    let service = Service::start().await;
    let driver = driver(service.composer()).await;
    service.answer(422, json!({ "error": "unknown template" }));
    let e = driver.issue(receipt("r-1")).await.unwrap_err();
    assert!(
        matches!(&e, DriverError::Compose(ComposeError::Refused(why)) if why.contains("422") && why.contains("unknown template")),
        "{e}"
    );

    let mut bad = composed_answer();
    bad["pdf"] = json!(STANDARD.encode(b"<html>not a pdf</html>"));
    service.answer(200, bad);
    let e = driver.issue(receipt("r-1")).await.unwrap_err();
    assert_eq!(e.to_string(), "the document composer refused the document: the pdf is not a PDF");
    assert!(matches!(driver.document("r-1").await, Err(DriverError::NotFound(_))));
    assert!(driver.objects.names().is_empty());
}

/// Purpose: a 2xx answer whose body is not JSON is refused.
/// Why it matters: a proxy's error page with a success status must not be
/// mistaken for a composed document.
#[tokio::test]
async fn an_answer_that_is_not_json_is_refused() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new().route("/v1/compose", post(|| async { "<html>ok</html>" }));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let settings = ExternalSettings {
        base,
        scope: "api://compose/.default".into(),
    };
    let driver = driver(Composer::External(ExternalComposer::new(&settings, Fake::credential()))).await;
    let e = driver.issue(receipt("r-1")).await.unwrap_err();
    assert!(
        matches!(&e, DriverError::Compose(ComposeError::Refused(why)) if why.contains("not JSON")),
        "{e}"
    );
}

/// Purpose: the approved template is filled before the composer is asked,
/// so data missing a field stops the letter, and a template with no
/// active version cannot be composed at all.
/// Why it matters: the legal and product approval, the rendering tests
/// and the guard against draft wording are enforced on the template the
/// module activates. If the composer were asked first, a letter could go
/// out from wording nobody approved or with a gap where a figure belongs.
#[tokio::test]
async fn the_module_controls_hold_whatever_composes_the_document() {
    let service = Service::start().await;
    let driver = driver(service.composer()).await;
    let mut missing = receipt("r-1");
    missing.data = json!({ "name": "Aroha" });
    assert!(matches!(
        driver.issue(missing).await,
        Err(DriverError::Template(TemplateError::Render {
            error: RenderError::Missing(_),
            ..
        }))
    ));
    let mut unknown = receipt("r-2");
    unknown.template = "not-approved".into();
    assert!(matches!(driver.issue(unknown).await, Err(DriverError::NotFound(_))));
    assert!(service.asked().is_empty(), "the composer was never asked");

    // Draft wording cannot be activated, so it never reaches a composer.
    let mut kit = Kit::ready();
    let draft = template("draft", ic_documents::DocumentKind::PremiumReceipt, ic_documents::placeholders::MARKER);
    let reference = draft.reference();
    kit.ok("config", "author", Command::SubmitTemplate(Box::new(draft.clone())));
    kit.approve(&reference, &[ApprovalRole::Legal, ApprovalRole::Product, ApprovalRole::Compliance]);
    let case = kit.case_for(&draft, &sample_data());
    kit.ok(
        "config",
        "author",
        Command::RunRenderingTests {
            template: reference.clone(),
            cases: vec![case],
        },
    );
    let today = kit.today();
    let refused = kit.err(
        "config",
        "product",
        Command::ActivateTemplate {
            template: reference,
            effective_from: today,
        },
    );
    assert!(refused.contains("placeholder_wording"), "{refused}");
}

/// Purpose: the composer's answer is read strictly: it must have text, a
/// PDF must be base64 of a PDF, and it must say what composed it.
/// Why it matters: whatever passes here is issued as a customer's
/// document and recorded as composed by the name it gives. A blank
/// letter, a file that is not a PDF, or a document of unknown origin must
/// never get that far.
#[test]
fn the_composers_answer_is_read_strictly() {
    let refused = |answer: Value| match composed_from(&answer) {
        Err(ComposeError::Refused(why)) => why,
        other => panic!("{other:?}"),
    };
    let mut answer = composed_answer();
    answer["text"] = json!("   ");
    assert_eq!(refused(answer), "the answer has no text");
    let mut answer = composed_answer();
    answer.as_object_mut().unwrap().remove("text");
    assert_eq!(refused(answer), "the answer has no text");
    let mut answer = composed_answer();
    answer["pdf"] = json!(12);
    assert_eq!(refused(answer), "the pdf is not base64 text");
    let mut answer = composed_answer();
    answer["pdf"] = json!("%%% not base64");
    assert_eq!(refused(answer), "the pdf is not a PDF");
    let mut answer = composed_answer();
    answer["composer"] = json!({ "name": "Compose Platform" });
    assert_eq!(refused(answer), "the answer does not say what composed it");
    let mut answer = composed_answer();
    answer["pdf"] = Value::Null;
    let composed = composed_from(&answer).unwrap();
    assert_eq!(composed.pdf, None);
    assert_eq!(composed.rendition.unwrap().pdf_sha256, None);
    assert_eq!(
        ComposeError::Unavailable("timed out".into()).to_string(),
        "the document composer is unavailable: timed out"
    );
}

/// Purpose: an email and a print file carry the composed PDF exactly as
/// issued, and the print instruction still carries the name and address.
/// Why it matters: the customer keeps and prints what they are sent, and
/// the print house posts what it is given. Either one set again from the
/// text would differ from the document the record holds.
#[tokio::test]
async fn email_and_post_carry_the_composed_pdf() {
    let fake = Fake::start().await;
    let print = MemoryStore::default();
    let channels = AzureChannels::new(
        &format!("{}/", fake.base),
        "notices@insurer.example",
        "+6421000000",
        Fake::credential(),
        print.clone(),
    );
    let mut message = Outbound {
        document: "d1".into(),
        party: party("aroha"),
        channel: Channel::Email,
        kind: ic_documents::DocumentKind::PremiumReceipt,
        data_class: ic_documents::kinds::DataClass::Personal,
        title: "Your receipt".into(),
        text: "Kia ora Aroha.".into(),
        pdf: Some(PDF.to_vec()),
        format: None,
        language: "en".into(),
        contact: ic_documents::channels::Contact {
            name: "Aroha Ngata".into(),
            destination: "aroha@example.nz".into(),
            ..Default::default()
        },
        attempt: 1,
    };
    channels.send(&message).await.unwrap();
    let attachment = fake.lock().emails[0].1["attachments"][0]["contentInBase64"].clone();
    assert_eq!(STANDARD.decode(attachment.as_str().unwrap()).unwrap(), PDF);

    message.channel = Channel::Post;
    message.contact.destination = "12 Kowhai Street\nWellington 6035".into();
    let name = channels.send(&message).await.unwrap();
    let instruction: Value = serde_json::from_slice(&print.get(&name).await.unwrap().unwrap()).unwrap();
    assert_eq!(instruction["name"], json!("Aroha Ngata"));
    assert_eq!(instruction["address"], json!(["12 Kowhai Street", "Wellington 6035"]));
    let pages = print.get(instruction["pdf"].as_str().unwrap()).await.unwrap().unwrap();
    assert_eq!(pages, PDF);
}
