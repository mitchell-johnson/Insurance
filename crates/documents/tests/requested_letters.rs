//! Letters other modules ask for: underwriting's decisions and
//! reinsurance's statements, recorded by the module and issued by the
//! driver.

mod support;

use std::sync::Arc;

use chrono::{DateTime, Utc};
use ic_documents::channels::{Channels, Contact, MemoryDirectory, Outbound, SendError};
use ic_documents::command::Command;
use ic_documents::crypto::{self, LocalWrap};
use ic_documents::driver::{Driver, LocalModule, Module};
use ic_documents::kinds::DataClass;
use ic_documents::model::{Channel, LetterRecipient, LetterRequest, LetterRequestStatus, REQUESTED_KINDS};
use ic_documents::module::Query;
use ic_documents::requested_letters::run_requested_letters;
use ic_documents::vault::MemoryStore;
use ic_documents::{DocumentKind, Event};
use serde_json::{json, Value};
use support::*;

#[derive(Clone, Default)]
struct Quiet;

impl Channels for Quiet {
    async fn send(&self, message: &Outbound) -> Result<String, SendError> {
        Ok(format!("ref-{}", message.document))
    }
}

type TestDriver = Driver<LocalModule, MemoryStore, LocalWrap, Quiet, MemoryDirectory>;

fn letter(request: &str, kind: &str, to: LetterRecipient) -> LetterRequest {
    LetterRequest {
        request: request.into(),
        kind: kind.into(),
        recipient: to,
        policy: None,
        subject: "prop-1".into(),
        life: Some("life-1".into()),
        reasons: vec!["bmi_over_limit".into()],
        detail: Default::default(),
        business_date: date("2027-11-15"),
    }
}

fn to(p: &str) -> LetterRecipient {
    LetterRecipient::Party { party: party(p) }
}

fn adviser(p: &str) -> LetterRecipient {
    LetterRecipient::Adviser { adviser: party(p) }
}

/// Purpose: the module takes a letter request only from a module allowed
/// that kind, never sends a health letter to an adviser, accepts the same
/// request sent again but not a different one under the same id, and
/// reports where each request stands.
/// Why it matters: underwriting retries until it hears back, so a resend
/// must change nothing; a request id reused for other content would send
/// one person's decision under another's reference; and a decline sent to
/// an adviser would tell another adult about someone's health.
#[test]
fn the_module_takes_each_request_once_and_by_the_rules() {
    let mut kit = Kit::ready();
    let declined = letter("uw:1", "declined", to("aroha"));
    let events = kit.ok("underwriting", "uw-driver", Command::RequestLetter(declined.clone()));
    assert_eq!(
        events,
        vec![Event::LetterRequested {
            request: "uw:1".into(),
            caller: "underwriting".into(),
            kind: "declined".into()
        }]
    );
    // Sent again under a new key: nothing changes.
    assert!(kit.ok("underwriting", "uw-driver", Command::RequestLetter(declined.clone())).is_empty());
    let mut other = declined.clone();
    other.reasons.clear();
    assert_eq!(kit.err("underwriting", "uw-driver", Command::RequestLetter(other)), "request_conflict");

    assert_eq!(
        kit.err(
            "underwriting",
            "uw-driver",
            Command::RequestLetter(letter("uw:2", "offer_made", adviser("tama")))
        ),
        "clinical_to_adviser"
    );
    assert!(!kit
        .ok(
            "underwriting",
            "uw-driver",
            Command::RequestLetter(letter("uw:3", "adviser_unavailable", adviser("tama")))
        )
        .is_empty());
    assert_eq!(
        kit.err(
            "underwriting",
            "uw-driver",
            Command::RequestLetter(letter("uw:4", "reinsurance_statement", to("aroha")))
        ),
        "unknown_letter_kind"
    );
    assert_eq!(
        kit.err("underwriting", "uw-driver", Command::RequestLetter(letter(" ", "declined", to("aroha")))),
        "no_request"
    );
    assert_eq!(
        kit.err("finance", "f", Command::RequestLetter(letter("f:1", "declined", to("aroha")))),
        "not_allowed"
    );
    let mut statement = letter("ri:7", "reinsurance_statement", to("reinsurer-a"));
    statement.subject = "stmt-7".into();
    kit.ok("reinsurance", "ri-driver", Command::RequestLetter(statement));

    let pending: Vec<String> = kit.state.letters_requested(10).iter().map(|r| r.request.request.clone()).collect();
    assert_eq!(pending, ["uw:1", "uw:3", "ri:7"]);
    assert_eq!(kit.state.letters_requested(1).len(), 1);
    assert_eq!(kit.state.letter_requests["uw:1"].caller, "underwriting");
    // Every kind names its own template.
    for kind in REQUESTED_KINDS {
        let prefix = match kind.caller {
            "underwriting" => "uw-",
            "reinsurance" => "ri-",
            "claims" => "cl-",
            _ => "grp-",
        };
        assert!(kind.template.starts_with(prefix), "{}", kind.template);
    }
}

/// Purpose: group administration may ask for the two leave-of-absence
/// letters to a member, with the day absence began and the day cover ends,
/// each one proven delivered; no other module may ask for them, and group
/// may cancel its own.
/// Why it matters: a member posted overseas loses cover on a set day if
/// they are not back (persona GRP-038). Being told that day, provably, is
/// what lets them arrange their return or other cover; a claim refused
/// after the limit without that notice is a complaint the insurer loses.
#[test]
fn group_asks_for_the_leave_of_absence_letters() {
    let mut kit = Kit::ready();
    let mut started = letter("grp:abs:1", "absence_started", to("aroha"));
    started.policy = Some(policy("scheme-1"));
    started.subject = "scheme-1".into();
    started.reasons.clear();
    started.detail = [
        ("since".to_string(), "2027-11-01".to_string()),
        ("cover_ends".to_string(), "2028-11-01".to_string()),
    ]
    .into();
    kit.ok("group", "group-driver", Command::RequestLetter(started.clone()));
    let kind = REQUESTED_KINDS.iter().find(|k| k.kind == "absence_started").unwrap();
    assert!(kind.proven && !kind.clinical);
    assert_eq!(kit.state.letter_requests["grp:abs:1"].request.detail["cover_ends"], "2028-11-01");
    assert_eq!(
        kit.err("underwriting", "uw", Command::RequestLetter(letter("uw:9", "absence_started", to("aroha")))),
        "unknown_letter_kind"
    );
    kit.ok(
        "group",
        "group-driver",
        Command::RequestLetter(letter("grp:abs:2", "absence_limit_reached", to("aroha"))),
    );
    kit.ok(
        "group",
        "group-driver",
        Command::CancelLetterRequest {
            request: "grp:abs:2".into(),
            reason: "back at work".into(),
        },
    );
    assert!(matches!(kit.state.letter_requests["grp:abs:2"].status, LetterRequestStatus::Cancelled { .. }));
}

/// Purpose: a waiting request can be cancelled, with a reason, by the
/// module that asked or by servicing, and then is not issued.
/// Why it matters: a letter can become wrong before it goes (the proposal
/// was reopened, the reinsurer's statement was cut again); without a
/// cancel the service would send it the moment wording existed, and
/// another module cancelling it would silence a letter it does not own.
#[test]
fn a_waiting_letter_is_cancelled_by_its_owner_or_servicing() {
    let mut kit = Kit::ready();
    kit.ok("underwriting", "uw", Command::RequestLetter(letter("uw:1", "proposal_lapsed", to("aroha"))));
    kit.ok("underwriting", "uw", Command::RequestLetter(letter("uw:2", "proposal_withdrawn", to("aroha"))));
    let cancel = |request: &str, reason: &str| Command::CancelLetterRequest {
        request: request.into(),
        reason: reason.into(),
    };
    assert_eq!(kit.err("underwriting", "uw", cancel("uw:1", " ")), "no_reason");
    assert_eq!(kit.err("underwriting", "uw", cancel("uw:9", "gone")), "unknown_request");
    assert_eq!(kit.err("reinsurance", "ri", cancel("uw:1", "not mine")), "not_yours");
    assert_eq!(
        kit.ok("underwriting", "uw", cancel("uw:1", "proposal reopened")),
        vec![Event::LetterRequestCancelled { request: "uw:1".into() }]
    );
    assert_eq!(kit.err("underwriting", "uw", cancel("uw:1", "again")), "not_pending");
    kit.ok("servicing", "kiri", cancel("uw:2", "customer asked us to stop"));
    assert!(kit.state.letters_requested(10).is_empty());
    assert_eq!(
        kit.state.letter_requests["uw:2"].status,
        LetterRequestStatus::Cancelled {
            reason: "customer asked us to stop".into()
        }
    );
}

/// Purpose: settled requests are forgotten once the keys would be, a
/// waiting one is kept however old, and a state saved before letters
/// existed still loads.
/// Why it matters: underwriting sends a letter for every proposal, so the
/// replicated state would grow for ever; but forgetting a letter still
/// waiting would lose a decline the customer is owed.
#[test]
fn settled_requests_are_forgotten_and_waiting_ones_kept() {
    use ic_documents::state::KEEP_KEYS_DAYS;
    let mut kit = Kit::ready();
    kit.ok("underwriting", "uw", Command::RequestLetter(letter("uw:1", "proposal_lapsed", to("aroha"))));
    kit.ok("underwriting", "uw", Command::RequestLetter(letter("uw:2", "policy_issued", to("aroha"))));
    kit.ok(
        "underwriting",
        "uw",
        Command::CancelLetterRequest {
            request: "uw:1".into(),
            reason: "reopened".into(),
        },
    );
    kit.advance_days(KEEP_KEYS_DAYS + 1);
    kit.tick();
    assert!(!kit.state.letter_requests.contains_key("uw:1"));
    assert_eq!(kit.state.letters_requested(10).len(), 1, "a waiting letter is never forgotten");
    kit.tick();
    assert!(kit.state.letter_requests.contains_key("uw:2"));

    let mut saved = serde_json::to_value(&kit.state).unwrap();
    let object = saved.as_object_mut().unwrap();
    object.remove("letter_requests").unwrap();
    object.remove("letter_log").unwrap();
    let older: ic_documents::DocumentsState = serde_json::from_value(saved).unwrap();
    assert!(older.letter_requests.is_empty());
}

const DECLINED: &str = "Dear {{name}}. We cannot offer cover on proposal {{reference}}. You may ask us to review this.";
const ISSUED: &str = "Dear {{name}}. Your policy from proposal {{reference}} is in force.";
const STATEMENT: &str = "Statement {{reference}} for treaty {{treaty}}.";

fn letter_data() -> Value {
    json!({ "name": "Aroha", "reference": "prop-1", "date": "2027-11-15", "reasons": ["bmi_over_limit"], "life": "life-1", "treaty": "QS-1" })
}

struct World {
    driver: Arc<TestDriver>,
    now: DateTime<Utc>,
    keys: std::sync::atomic::AtomicU32,
}

impl World {
    async fn new(templates: &[(&str, DocumentKind, &str)]) -> Self {
        let mut kit = Kit::new();
        let now = kit.now;
        for (id, kind, body) in templates {
            // A letter asked for by its id describes no product, as its shipped wording does.
            let mut t = template(id, *kind, body);
            t.products.clear();
            kit.activate(t, &letter_data());
        }
        let module = LocalModule::default();
        for envelope in &kit.log {
            assert!(module.propose(envelope.clone()).await.unwrap().result.is_ok());
        }
        let directory = MemoryDirectory::default();
        directory.set_contact(
            "aroha",
            Channel::Email,
            Contact {
                name: "Aroha Ngata".into(),
                destination: "aroha@example.nz".into(),
                language: None,
                format: Some("braille".into()),
                deceased: false,
            },
        );
        directory.set("reinsurer-a", Channel::Email, "Reinsurer A", "statements@reinsurer.example");
        let driver = Arc::new(Driver {
            module,
            objects: MemoryStore::default(),
            wrap: LocalWrap::new(crypto::new_key()),
            channels: Quiet,
            directory,
            actor: "documents-driver".into(),
            clock: Arc::new(move || now),
            batch: 100,
            composer: Default::default(),
        });
        Self {
            driver,
            now,
            keys: Default::default(),
        }
    }

    async fn all() -> Self {
        Self::new(&[
            ("uw-declined", DocumentKind::GeneralCorrespondence, DECLINED),
            ("uw-policy-issued", DocumentKind::GeneralCorrespondence, ISSUED),
            ("ri-reinsurance-statement", DocumentKind::ReinsurerStatement, STATEMENT),
        ])
        .await
    }

    async fn ask(&self, caller: &str, request: LetterRequest) {
        let n = self.keys.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let applied = self
            .driver
            .module
            .propose(ic_documents::Envelope {
                key: format!("ask-{n}"),
                caller: caller.into(),
                actor: "driver".into(),
                at: self.now,
                command: Command::RequestLetter(request),
            })
            .await
            .unwrap();
        assert_eq!(applied.result, Ok(()));
    }

    fn status(&self, request: &str) -> LetterRequestStatus {
        self.driver.module.state().letter_requests[request].status.clone()
    }
}

/// Purpose: a requested decline is issued from its template to the person
/// as a clinical document with proof of delivery, in the ordinary format
/// when the template has none of the one they asked for, and the request
/// is marked issued with the document; the next pass does nothing.
/// Why it matters: this is how a person learns their cover was declined
/// and that they may ask for a review. Sent twice, it would alarm them; not
/// marked, underwriting could not show it was sent; stored as anything
/// less than clinical, staff could open someone's health decision.
#[tokio::test]
async fn a_requested_decline_is_issued_once_as_clinical() {
    let world = World::all().await;
    world.ask("underwriting", letter("uw:1", "declined", to("aroha"))).await;
    let pass = world.driver.requested_letters().await;
    assert_eq!((pass.issued, pass.left, pass.errors.clone()), (1, 0, vec![]));
    assert_eq!(
        world.status("uw:1"),
        LetterRequestStatus::Issued {
            document: "letter:uw:1".into()
        }
    );
    let document = world.driver.document("letter:uw:1").await.unwrap();
    assert_eq!(document.data_class, DataClass::Clinical);
    assert!(document.proof && document.format.is_none());
    assert_eq!(document.lives, ["life-1"]);
    assert_eq!(document.trigger_event.as_deref(), Some("underwriting:uw:1"));
    assert_eq!(document.owner, party("aroha"));
    assert_eq!(
        world.driver.read("letter:uw:1").await.unwrap().text,
        "Dear Aroha Ngata. We cannot offer cover on proposal prop-1. You may ask us to review this."
    );
    let again = world.driver.requested_letters().await;
    assert_eq!((again.issued, again.left), (0, 0));
    let answer = world.driver.module.ask(Query::LetterRequest { request: "uw:1".into() }).await.unwrap();
    assert_eq!(answer["status"], json!({ "issued": { "document": "letter:uw:1" } }));
}

/// Purpose: a reinsurer's statement is issued to the firm with its detail
/// in the wording and the policy left out, and an ordinary letter goes as
/// personal, not clinical, without proof.
/// Why it matters: the statement is what the reinsurer settles against,
/// so the treaty it names must be the one sent; and marking every letter
/// clinical would hide routine correspondence from the staff who answer
/// the customer's questions about it.
#[tokio::test]
async fn statements_and_ordinary_letters_carry_their_own_class() {
    let world = World::all().await;
    let mut statement = letter("ri:7", "reinsurance_statement", to("reinsurer-a"));
    statement.subject = "stmt-7".into();
    statement.life = None;
    statement.detail.insert("treaty".into(), "QS-1".into());
    world.ask("reinsurance", statement).await;
    let mut issued = letter("uw:2", "policy_issued", to("aroha"));
    issued.policy = Some(policy("pol-1"));
    world.ask("underwriting", issued).await;
    let pass = world.driver.requested_letters().await;
    assert_eq!((pass.issued, pass.errors.clone()), (2, vec![]));
    assert_eq!(world.driver.read("letter:ri:7").await.unwrap().text, "Statement stmt-7 for treaty QS-1.");
    let statement = world.driver.document("letter:ri:7").await.unwrap();
    assert_eq!((statement.kind, statement.proof), (DocumentKind::ReinsurerStatement, true));
    let letter = world.driver.document("letter:uw:2").await.unwrap();
    assert_eq!((letter.data_class, letter.proof), (DataClass::Personal, false));
    assert_eq!(letter.policy, Some(policy("pol-1")));
}

/// Purpose: a letter whose template has no wording, whose person has no
/// contact point or has died, waits and is reported (a missing template
/// once, however many letters need it); and an outage of the module or
/// the customer master is reported and loses nothing.
/// Why it matters: wording is written by people and arrives after the
/// code. Letters must queue until it does, not fail for good; a letter in
/// a dead person's name causes distress; and one pass that failed must not
/// drop what the next can send.
#[tokio::test]
async fn letters_wait_for_wording_and_people_and_survive_outages() {
    let world = World::new(&[("uw-policy-issued", DocumentKind::GeneralCorrespondence, ISSUED)]).await;
    world.ask("underwriting", letter("uw:1", "proposal_lapsed", to("aroha"))).await;
    world.ask("underwriting", letter("uw:2", "proposal_lapsed", to("aroha"))).await;
    world.ask("underwriting", letter("uw:3", "policy_issued", to("nobody"))).await;
    world.ask("underwriting", letter("uw:4", "policy_issued", to("mere"))).await;
    world.driver.directory.set_contact(
        "mere",
        Channel::Post,
        Contact {
            name: "Mere Parata".into(),
            destination: "1 Queen St".into(),
            deceased: true,
            ..Default::default()
        },
    );
    let pass = world.driver.requested_letters().await;
    assert_eq!((pass.issued, pass.left), (0, 4));
    assert_eq!(pass.errors.iter().filter(|e| e.contains("uw-proposal-lapsed")).count(), 1);
    assert!(pass.errors.iter().any(|e| e.contains("nobody has no contact point")));
    assert!(pass.errors.iter().any(|e| e.contains("mere has died")));
    assert_eq!(world.driver.module.state().letters_requested(10).len(), 4);

    world.driver.module.set_down(true);
    let down = world.driver.requested_letters().await;
    assert!(down.errors[0].starts_with("letters requested"));
    world.driver.module.set_down(false);
    // The customer master is down: each letter says so and waits.
    world.driver.directory.set_down(true);
    let down = world.driver.requested_letters().await;
    assert_eq!(down.issued, 0);
    assert!(down.errors.iter().any(|e| e.starts_with("letter:uw:3") && e.contains("unavailable")));
    world.driver.directory.set_down(false);

    let stop = async { tokio::time::sleep(std::time::Duration::from_millis(30)).await };
    let mut passes = 0;
    run_requested_letters(world.driver.clone(), std::time::Duration::from_millis(5), stop, |_| passes += 1).await;
    assert!(passes >= 2);
    assert_eq!(world.driver.module.state().letters_requested(10).len(), 4, "nothing was lost");
}

/// Purpose: claims may ask for the overpayment repayment letter only with
/// the repayment reference payments holds for the overpayment and the
/// amount; a request missing either, or with one blank, is refused.
/// Why it matters: a credit that quotes the reference is matched to the
/// overpayment automatically. A letter without it leaves the person's
/// repayment sitting unmatched, and they are chased for money they paid.
#[test]
fn an_overpayment_letter_needs_its_repayment_reference() {
    let mut kit = Kit::ready();
    let mut repay = letter("cl:op:1", "overpayment_repayment", to("aroha"));
    repay.reasons.clear();
    repay.subject = "claim-1".into();
    assert_eq!(kit.err("claims", "claims-driver", Command::RequestLetter(repay.clone())), "missing_detail");
    repay.detail.insert("amount".into(), "1250.00".into());
    repay.detail.insert("repayment_reference".into(), " ".into());
    assert_eq!(kit.err("claims", "claims-driver", Command::RequestLetter(repay.clone())), "missing_detail");
    repay.detail.insert("repayment_reference".into(), "OP123456".into());
    kit.ok("claims", "claims-driver", Command::RequestLetter(repay));
    assert_eq!(kit.state.letter_requests["cl:op:1"].request.detail["repayment_reference"], "OP123456");
}

/// Purpose: a person's representative (an attorney in force, able to see
/// what they are sent) is a recipient of an ordinary letter to them, but
/// never of a decline, which is about their health.
/// Why it matters: someone who cannot manage their own post relies on
/// their attorney seeing it (VUL-019); a health decision goes to the
/// person alone, whoever else acts for them.
#[tokio::test]
async fn a_representative_sees_ordinary_letters_and_never_clinical_ones() {
    let world = World::all().await;
    world.driver.directory.set("attorney-1", Channel::Post, "Hemi Ngata", "PO Box 1\nWellington");
    world.driver.directory.set_representative("aroha", "attorney-1", date("2020-01-01"), None);
    world.ask("underwriting", letter("uw:1", "declined", to("aroha"))).await;
    world.ask("underwriting", letter("uw:2", "policy_issued", to("aroha"))).await;
    let pass = world.driver.requested_letters().await;
    assert_eq!((pass.issued, pass.errors.clone()), (2, vec![]), "{pass:?}");
    let parties = |d: &ic_documents::model::Document| d.deliveries.iter().map(|d| d.recipient.party.0.clone()).collect::<Vec<_>>();
    assert_eq!(parties(&world.driver.document("letter:uw:1").await.unwrap()), ["aroha"]);
    assert_eq!(parties(&world.driver.document("letter:uw:2").await.unwrap()), ["aroha", "attorney-1"]);
}
