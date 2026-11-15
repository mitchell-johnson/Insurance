//! The delivery channels against a local stand-in for Azure Communication
//! Services, the print outbox and the customer master.

#[path = "support/azure.rs"]
mod azure;
mod support;

use azure::Fake;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use ic_documents::channels::{
    delivery_reports, mask, representatives_in, subscription_validation, AzureChannels, Channels, Contact, ContactDirectory, DirectoryError, HttpDirectory,
    MemoryDirectory, Outbound, SendError,
};
use ic_documents::kinds::DataClass;
use ic_documents::model::{AttemptOutcome, Channel};
use ic_documents::pdf::{layout, pdf, Fonts, Letter};
use ic_documents::template::AlternateFormat;
use ic_documents::vault::MemoryStore;
use ic_documents::DocumentKind;
use serde_json::{json, Value};
use support::{date, party};

fn message(channel: Channel, destination: &str) -> Outbound {
    Outbound {
        document: "d1".into(),
        party: party("aroha"),
        channel,
        kind: DocumentKind::RenewalNotice,
        data_class: DataClass::Personal,
        title: "Your renewal".into(),
        text: "Dear Aroha. Your premium will be $123.45.".into(),
        pdf: None,
        format: None,
        language: "en".into(),
        contact: Contact {
            name: "Aroha Ngata".into(),
            destination: destination.into(),
            ..Contact::default()
        },
        attempt: 1,
    }
}

fn channels(fake: &Fake, print: MemoryStore) -> AzureChannels<MemoryStore> {
    AzureChannels::new(&format!("{}/", fake.base), "notices@insurer.example", "+6421000000", Fake::credential(), print)
}

/// Purpose: the delivery record keeps enough of a destination to recognise
/// it and not enough to use it.
/// Why it matters: the delivery record is replicated, snapshotted and
/// reported on. A full email address or phone number there would put
/// contact details outside the customer master, where erasure and access
/// control would not reach them.
#[test]
fn destinations_are_masked_for_the_record() {
    assert_eq!(mask(Channel::Email, "aroha.ngata@example.nz"), "a***@example.nz");
    assert_eq!(mask(Channel::Email, "@example.nz"), "***@example.nz");
    assert_eq!(mask(Channel::Email, "not-an-address"), "***");
    assert_eq!(mask(Channel::Sms, "+64211234567"), "***567");
    assert_eq!(mask(Channel::Post, "12 Kowhai Street\nNgaio\nWellington 6035"), "*** Wellington 6035");
    assert_eq!(mask(Channel::Post, ""), "*** ");
    assert_eq!(mask(Channel::InApp, ""), "app");
    assert_eq!(mask(Channel::Push, "device"), "app");
}

/// Purpose: a message's idempotency ID is the same for a retry of the same
/// attempt and different for anything else, and sensitive text travels
/// only on channels that may carry it.
/// Why it matters: the ID is what stops a provider sending a lapse notice
/// twice after a crash. The text rule is what keeps health information out
/// of email and SMS.
#[test]
fn a_message_knows_its_identity_and_what_it_may_carry() {
    let email = message(Channel::Email, "a@example.nz");
    let id = email.idempotency_id();
    assert_eq!(id, message(Channel::Email, "other@example.nz").idempotency_id());
    assert_eq!(id.len(), 36);
    assert_eq!(id.split('-').map(str::len).collect::<Vec<_>>(), vec![8, 4, 4, 4, 12]);
    let mut second = email.clone();
    second.attempt = 2;
    assert_ne!(second.idempotency_id(), id);
    assert_ne!(message(Channel::Sms, "x").idempotency_id(), id);

    assert!(email.carries_text());
    let mut clinical = email.clone();
    clinical.data_class = DataClass::Clinical;
    assert!(!clinical.carries_text());
    clinical.channel = Channel::Post;
    assert!(clinical.carries_text());
    assert!(message(Channel::InApp, "").carries_text());
    assert!(!message(Channel::Sms, "x").carries_text());
    assert!(!message(Channel::Push, "x").carries_text());
    assert!(email.pointer().contains("Your renewal") && !email.pointer().contains("123.45"));
}

/// Purpose: an email is sent once through Communication Services however
/// often it is retried, carries the document or only a pointer according to
/// its sensitivity, and a refusal or an outage is told apart.
/// Why it matters: a refusal must move the notice to the next channel at
/// once; an outage must not, or every notice would fall back to post each
/// time the provider had a bad minute.
#[tokio::test]
async fn email_goes_through_communication_services_once() {
    let fake = Fake::start().await;
    let ch = channels(&fake, MemoryStore::default());
    let m = message(Channel::Email, "aroha@example.nz");

    let reference = ch.send(&m).await.unwrap();
    assert_eq!(reference, m.idempotency_id());
    assert_eq!(ch.send(&m).await.unwrap(), reference);
    {
        let inner = fake.lock();
        assert_eq!(inner.emails.len(), 1, "the retry was the same operation");
        let body = &inner.emails[0].1;
        assert_eq!(body["senderAddress"], json!("notices@insurer.example"));
        assert_eq!(body["recipients"]["to"][0]["address"], json!("aroha@example.nz"));
        assert_eq!(body["content"]["subject"], json!("Your renewal"));
        assert!(body["content"]["plainText"].as_str().unwrap().contains("$123.45"));
    }

    let mut sensitive = m.clone();
    sensitive.document = "d2".into();
    sensitive.data_class = DataClass::InsuranceSensitive;
    ch.send(&sensitive).await.unwrap();
    let text = fake.lock().emails[1].1["content"]["plainText"].as_str().unwrap().to_string();
    assert!(text.contains("secure inbox") && !text.contains("123.45"));

    // A provider that names no operation still gives a usable reference.
    fake.body("email", json!({}));
    assert_eq!(ch.send(&m).await.unwrap(), m.idempotency_id());

    fake.force("email", 400);
    assert!(matches!(ch.send(&m).await, Err(SendError::Permanent(r)) if r.contains("400")));
    for status in [429, 503] {
        fake.force("email", status);
        assert!(matches!(ch.send(&m).await, Err(SendError::Temporary(_))));
    }
    let nowhere = AzureChannels::new("http://127.0.0.1:1", "a@b", "+64", Fake::credential(), MemoryStore::default());
    assert!(matches!(nowhere.send(&m).await, Err(SendError::Temporary(_))));
}

/// Purpose: an SMS carries only a pointer to the inbox, asks for a delivery
/// report, and a number the provider refuses is a permanent failure.
/// Why it matters: SMS is read on lock screens and by whoever holds the
/// phone, so it must never carry the document. Without the delivery report
/// there is no proof, and a refused number that looked like success would
/// start a notice clock nobody could defend.
#[tokio::test]
async fn sms_carries_a_pointer_and_asks_for_a_report() {
    let fake = Fake::start().await;
    let ch = channels(&fake, MemoryStore::default());
    let m = message(Channel::Sms, "+64211234567");

    assert_eq!(ch.send(&m).await.unwrap(), "sms-1");
    {
        let inner = fake.lock();
        let body = &inner.sms[0];
        assert_eq!(body["from"], json!("+6421000000"));
        assert_eq!(body["smsRecipients"][0]["to"], json!("+64211234567"));
        assert_eq!(body["smsRecipients"][0]["repeatabilityRequestId"], json!(m.idempotency_id()));
        assert_eq!(body["smsSendOptions"]["enableDeliveryReport"], json!(true));
        assert!(!body["message"].as_str().unwrap().contains("123.45"));
    }

    fake.body(
        "sms",
        json!({ "value": [{ "to": "+64211234567", "successful": false, "errorMessage": "Invalid number" }] }),
    );
    assert_eq!(ch.send(&m).await, Err(SendError::Permanent("Invalid number".into())));
    fake.body("sms", json!({ "value": [{ "successful": false }] }));
    assert_eq!(ch.send(&m).await, Err(SendError::Permanent("the number was refused".into())));
    fake.body("sms", json!({ "value": [{ "successful": true }] }));
    assert!(matches!(ch.send(&m).await, Err(SendError::Temporary(_))));
    fake.force("sms", 500);
    assert!(matches!(ch.send(&m).await, Err(SendError::Temporary(_))));
}

/// Purpose: post is a print file in the outbox, written once per attempt
/// with the full text and address; the inbox needs no send; push is left to
/// the app services.
/// Why it matters: post is the last resort that makes every mandatory
/// notice provable, so the print house must get exactly one file per
/// letter. A push this service pretended to send would start a
/// confirmation wait for a message nobody sent.
#[tokio::test]
async fn post_is_a_print_file_and_the_inbox_needs_no_send() {
    let fake = Fake::start().await;
    let print = MemoryStore::default();
    let ch = channels(&fake, print.clone());
    let mut m = message(Channel::Post, "12 Kowhai Street\nNgaio\nWellington 6035");
    m.data_class = DataClass::Clinical;

    let name = ch.send(&m).await.unwrap();
    assert_eq!(name, "print/d1/aroha-1.json");
    assert_eq!(ch.send(&m).await.unwrap(), name, "a retry does not print twice");
    assert_eq!(print.names(), vec![name.clone(), "print/d1/aroha-1.pdf".to_string()]);
    let file: serde_json::Value = serde_json::from_slice(&ic_documents::vault::ObjectStore::get(&print, &name).await.unwrap().unwrap()).unwrap();
    assert_eq!(file["address"], json!(["12 Kowhai Street", "Ngaio", "Wellington 6035"]));
    assert_eq!(file["name"], json!("Aroha Ngata"));
    assert!(file["text"].as_str().unwrap().contains("$123.45"));
    // The instruction names the pages, which are addressed for the envelope.
    assert_eq!(file["pdf"], json!("print/d1/aroha-1.pdf"));
    let pages = ic_documents::vault::ObjectStore::get(&print, "print/d1/aroha-1.pdf").await.unwrap().unwrap();
    assert_eq!(pages, m.pdf(&Fonts::bundled()).unwrap());
    let address: Vec<String> = m.contact.destination.lines().map(str::to_string).collect();
    let laid = layout(
        &Letter {
            document: "d1",
            title: "Your renewal",
            name: "Aroha Ngata",
            address: &address,
            text: &m.text,
            language: "en",
            large_print: false,
        },
        &Fonts::bundled(),
    )
    .unwrap();
    assert_eq!(laid[0].lines[1].text, "12 Kowhai Street");

    print.set_down(true);
    m.attempt = 2;
    assert!(matches!(ch.send(&m).await, Err(SendError::Temporary(_))));

    assert_eq!(ch.send(&message(Channel::InApp, "")).await.unwrap(), "inbox:d1");
    assert!(matches!(ch.send(&message(Channel::Push, "")).await, Err(SendError::Permanent(_))));
    assert!(fake.lock().calls.is_empty(), "none of these called Communication Services");
}

/// Purpose: the contact point is asked of the customer master for the
/// relationship and channel of this send, a missing one is `None`, and an
/// outage is an error; the representatives who see what a party is sent
/// are those in force that day with a scope of `view` or `all`.
/// Why it matters: a notice sent to the contact point of the wrong
/// relationship reaches the wrong person. And "no contact point" moves a
/// notice on to post, so an outage mistaken for it would abandon email for
/// every customer at once.
#[tokio::test]
async fn contact_points_come_from_the_customer_master() {
    let fake = Fake::start().await;
    fake.lock()
        .contacts
        .insert("aroha/email".into(), json!({ "name": "Aroha Ngata", "destination": "aroha@example.nz" }));
    // The customer master also says what it knows of the person's needs.
    fake.lock().contacts.insert(
        "ben/email".into(),
        json!({ "name": "Ben", "destination": "ben@example.nz", "language": "mi", "format": "large_print", "deceased": true }),
    );
    let directory = HttpDirectory::new(&format!("{}/", fake.base), "api://customer-master/.default", Fake::credential());

    let found = directory.contact(&party("aroha"), Some("policyholder"), Channel::Email).await.unwrap().unwrap();
    assert_eq!(
        found,
        Contact {
            name: "Aroha Ngata".into(),
            destination: "aroha@example.nz".into(),
            ..Contact::default()
        }
    );
    let ben = directory.contact(&party("ben"), None, Channel::Email).await.unwrap().unwrap();
    assert_eq!(
        (ben.language.as_deref(), ben.format.as_deref(), ben.deceased),
        (Some("mi"), Some("large_print"), true)
    );
    fake.lock().contact_queries.pop();
    assert_eq!(directory.contact(&party("aroha"), None, Channel::Sms).await.unwrap(), None);
    {
        let inner = fake.lock();
        assert_eq!(inner.contact_queries[0]["relationship"], "policyholder");
        assert_eq!(inner.contact_queries[0]["purpose"], "notices");
        assert_eq!(inner.contact_queries[0]["channel"], "email");
        assert!(!inner.contact_queries[1].contains_key("relationship"));
        assert_eq!(inner.contact_queries[1]["channel"], "sms");
    }
    fake.lock().contacts.insert("aroha/post".into(), json!("not a contact"));
    assert!(matches!(
        directory.contact(&party("aroha"), None, Channel::Post).await,
        Err(DirectoryError::Unavailable(_))
    ));
    // Representatives: attorneys, guardians and authorised representatives
    // in force on the day whose scope lets them see what is sent.
    let rel = |party: &str, kind: &str, from: &str, to: Value, scope: Value| {
        json!({ "party": party, "id": "r", "kind": kind, "target": { "kind": "party", "id": "aroha" }, "valid_from": from, "valid_to": to,
                "recorded_at": "2027-01-01T00:00:00Z", "attributes": { "scope": scope } })
    };
    fake.lock().relationships.insert(
        "party:aroha".into(),
        json!({ "relationships": [
            rel("hemi", "attorney", "2027-01-01", Value::Null, json!(["view"])),
            rel("mere", "guardian", "2027-01-01", Value::Null, json!(["all"])),
            rel("hemi", "authorised_representative", "2027-01-01", Value::Null, json!(["all"])),
            rel("tama", "attorney", "2027-01-01", Value::Null, json!(["pay_premiums"])),
            rel("wiremu", "attorney", "2027-01-01", json!("2027-06-01"), json!(["all"])),
            rel("ana", "attorney", "2028-01-01", Value::Null, json!(["all"])),
            rel("rangi", "employer", "2027-01-01", Value::Null, json!(["all"])),
        ] }),
    );
    let on = date("2027-11-15");
    assert_eq!(directory.representatives(&party("aroha"), on).await.unwrap(), [party("hemi"), party("mere")]);
    assert_eq!(directory.representatives(&party("ben"), on).await.unwrap(), vec![]);
    assert_eq!(representatives_in(&json!({}), on), vec![]);
    fake.force("directory", 500);
    assert!(matches!(directory.contact(&party("aroha"), None, Channel::Email).await, Err(DirectoryError::Unavailable(m)) if m.contains("500")));
    assert!(directory.representatives(&party("aroha"), date("2027-11-15")).await.is_err());
    let nowhere = HttpDirectory::new("http://127.0.0.1:1", "s", Fake::credential());
    assert!(nowhere.representatives(&party("aroha"), on).await.is_err());
    assert!(nowhere.contact(&party("aroha"), None, Channel::InApp).await.is_err());
    assert!(nowhere.contact(&party("aroha"), None, Channel::Push).await.is_err());

    let memory = MemoryDirectory::default();
    memory.set("aroha", Channel::Email, "Aroha", "a@example.nz");
    assert!(memory.contact(&party("aroha"), None, Channel::Email).await.unwrap().is_some());
    memory.remove("aroha", Channel::Email);
    assert_eq!(memory.contact(&party("aroha"), None, Channel::Email).await.unwrap(), None);
    memory.set_down(true);
    assert!(memory.contact(&party("aroha"), None, Channel::Email).await.is_err());
    assert!(memory.representatives(&party("aroha"), date("2027-11-15")).await.is_err());
}

/// Purpose: provider delivery reports from Event Grid are read into
/// outcomes: delivered, bounced or failed, with reports that are not final
/// and events of other kinds left out.
/// Why it matters: this is the proof of delivery. A bounce read as a
/// delivery starts a notice period for a notice nobody received; a
/// delivery read as nothing leaves a good notice to time out and be
/// posted again.
#[test]
fn delivery_reports_are_read_from_event_grid() {
    let body = json!([
        { "eventType": "Microsoft.Communication.EmailDeliveryReportReceived", "data": { "messageId": "m1", "status": "Delivered" } },
        { "eventType": "Microsoft.Communication.EmailDeliveryReportReceived", "data": { "messageId": "m2", "status": "Bounced", "deliveryStatusDetails": { "statusMessage": "mailbox does not exist" } } },
        { "eventType": "Microsoft.Communication.EmailDeliveryReportReceived", "data": { "messageId": "m3", "status": "FilteredSpam" } },
        { "eventType": "Microsoft.Communication.EmailDeliveryReportReceived", "data": { "messageId": "m4", "status": "Expanded" } },
        { "type": "Microsoft.Communication.SMSDeliveryReportReceived", "data": { "messageId": "s1", "deliveryStatus": "Delivered" } },
        { "eventType": "Microsoft.Communication.SMSDeliveryReportReceived", "data": { "messageId": "s2", "deliveryStatus": "Failed", "deliveryStatusDetails": "No route" } },
        { "eventType": "Microsoft.Communication.SMSDeliveryReportReceived", "data": { "messageId": "s3" } },
        { "eventType": "Microsoft.Communication.SMSDeliveryReportReceived", "data": { "deliveryStatus": "Delivered" } },
        { "eventType": "Microsoft.Storage.BlobCreated", "data": { "messageId": "x", "status": "Delivered" } },
    ]);
    let reports = delivery_reports(&serde_json::to_vec(&body).unwrap()).unwrap();
    let got: Vec<(&str, &AttemptOutcome)> = reports.iter().map(|r| (r.provider_reference.as_str(), &r.outcome)).collect();
    assert_eq!(
        got,
        vec![
            ("m1", &AttemptOutcome::Delivered),
            (
                "m2",
                &AttemptOutcome::Bounced {
                    reason: "mailbox does not exist".into()
                }
            ),
            ("m3", &AttemptOutcome::Failed { reason: "FilteredSpam".into() }),
            ("s1", &AttemptOutcome::Delivered),
            ("s2", &AttemptOutcome::Failed { reason: "No route".into() }),
        ]
    );
    assert!(delivery_reports(b"{}").unwrap_err().contains("not an Event Grid delivery"));

    let handshake = json!([{ "eventType": "Microsoft.EventGrid.SubscriptionValidationEvent", "data": { "validationCode": "abc-123" } }]);
    assert_eq!(subscription_validation(&serde_json::to_vec(&handshake).unwrap()), Some("abc-123".into()));
    assert_eq!(subscription_validation(&serde_json::to_vec(&body).unwrap()), None);
    assert_eq!(subscription_validation(b"junk"), None);
}

/// Purpose: an email that may carry the text also carries the document as
/// a PDF, in large print when that was asked for and without a postal
/// address; an email that only points to the inbox carries none.
/// Why it matters: a customer keeps and prints the attachment, so it must
/// be the same document, and attaching it to a pointer email would send
/// health information by a channel that must not carry it.
#[tokio::test]
async fn an_email_carries_the_document_as_a_pdf() {
    let fake = Fake::start().await;
    let ch = channels(&fake, MemoryStore::default()).with_fonts(Fonts::bundled());
    let mut m = message(Channel::Email, "aroha@example.nz");
    m.format = Some(AlternateFormat::LargePrint);
    ch.send(&m).await.unwrap();
    let mut sensitive = m.clone();
    sensitive.document = "d2".into();
    sensitive.data_class = DataClass::InsuranceSensitive;
    ch.send(&sensitive).await.unwrap();

    let inner = fake.lock();
    let attachment = &inner.emails[0].1["attachments"][0];
    assert_eq!(attachment["name"], json!("d1.pdf"));
    assert_eq!(attachment["contentType"], json!("application/pdf"));
    let file = STANDARD.decode(attachment["contentInBase64"].as_str().unwrap()).unwrap();
    assert_eq!(
        file,
        pdf(
            &Letter {
                document: "d1",
                title: "Your renewal",
                name: "",
                address: &[],
                text: &m.text,
                language: "en",
                large_print: true,
            },
            &Fonts::bundled()
        )
        .unwrap()
    );
    assert!(inner.emails[1].1.get("attachments").is_none());
}

/// Purpose: when the text has a character the font cannot draw, the email
/// still goes with the text and the print instruction still goes with the
/// text and no pages.
/// Why it matters: the PDF is a convenience on top of the document. A
/// mandatory notice held back because of one character the font lacks
/// would miss its notice period; the text itself is always complete.
#[tokio::test]
async fn a_document_that_cannot_be_typeset_still_goes_as_text() {
    let fake = Fake::start().await;
    let print = MemoryStore::default();
    let ch = channels(&fake, print.clone());
    let mut m = message(Channel::Email, "aroha@example.nz");
    m.text = "您的保费".into();
    assert!(m.pdf(&Fonts::bundled()).is_err());
    ch.send(&m).await.unwrap();
    {
        let inner = fake.lock();
        assert_eq!(inner.emails[0].1["content"]["plainText"], json!("您的保费"));
        assert!(inner.emails[0].1.get("attachments").is_none());
    }

    m.channel = Channel::Post;
    m.contact.destination = "12 Kowhai Street\nWellington 6035".into();
    let name = ch.send(&m).await.unwrap();
    assert_eq!(print.names(), vec![name.clone()]);
    let file: serde_json::Value = serde_json::from_slice(&ic_documents::vault::ObjectStore::get(&print, &name).await.unwrap().unwrap()).unwrap();
    assert_eq!(file["pdf"], json!(null));
    assert_eq!(file["text"], json!("您的保费"));
}
