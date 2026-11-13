//! The letters the complaints register says are owed.
//!
//! The complaints module lists what nobody has yet issued: the
//! acknowledgement of a complaint, the notice of a deadlock, the letter to
//! each customer affected by a licence event. This step issues each from
//! its template, to the person and anyone who acts for them, and records
//! it on the complaint or the case, as a handler would have.
//!
//! Nothing is issued without wording: a kind whose template has no active
//! version is left for the people handling the cases, and the pass says
//! so. Every step is safe to repeat. The document id is made from the
//! complaint or case, so a letter is issued once however often a pass is
//! cut short.

use std::collections::BTreeSet;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use ic_complaints::model::{LetterDue, LetterKind};
use ic_complaints::module::Query;
use ic_complaints::{Command, Envelope};
use ic_core::PartyId;
use serde_json::json;

use crate::channels::{Channels, Contact, ContactDirectory, DirectoryError};
use crate::complaints_relay::Complaints;
use crate::crypto::KeyWrap;
use crate::driver::{Driver, DriverError, IssueRequest, Module};
use crate::kinds::DataClass;
use crate::model::{Channel, Document, Recipient, RecipientRole};
use crate::template::AlternateFormat;
use crate::vault::ObjectStore;

/// What this step needs of the documents driver: to issue a document, and
/// to learn how to address a person.
pub trait Letters: Send + Sync {
    fn issue(&self, request: IssueRequest) -> impl Future<Output = Result<Document, DriverError>> + Send;
    /// The person's name and needs, from whichever contact point they have.
    fn person(&self, party: &PartyId) -> impl Future<Output = Result<Option<Contact>, DirectoryError>> + Send;
}

impl<M: Module, S: ObjectStore, W: KeyWrap, C: Channels, D: ContactDirectory> Letters for Driver<M, S, W, C, D> {
    async fn issue(&self, request: IssueRequest) -> Result<Document, DriverError> {
        Driver::issue(self, request).await
    }

    async fn person(&self, party: &PartyId) -> Result<Option<Contact>, DirectoryError> {
        for channel in [Channel::Email, Channel::Post] {
            if let Some(contact) = self.directory.contact(party, None, channel).await? {
                return Ok(Some(contact));
            }
        }
        Ok(None)
    }
}

impl<T: Letters> Letters for Arc<T> {
    async fn issue(&self, request: IssueRequest) -> Result<Document, DriverError> {
        T::issue(self, request).await
    }

    async fn person(&self, party: &PartyId) -> Result<Option<Contact>, DirectoryError> {
        T::person(self, party).await
    }
}

/// The template each kind of letter is issued from. The wording, its
/// approvals and its languages are the template's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LetterTemplates {
    pub acknowledgement: String,
    pub deadlock_notice: String,
    pub licence_notice: String,
}

impl Default for LetterTemplates {
    fn default() -> Self {
        Self {
            acknowledgement: "complaint-acknowledgement".into(),
            deadlock_notice: "complaint-deadlock-notice".into(),
            licence_notice: "licence-event-notice".into(),
        }
    }
}

impl LetterTemplates {
    fn of(&self, kind: LetterKind) -> &str {
        match kind {
            LetterKind::Acknowledgement => &self.acknowledgement,
            LetterKind::DeadlockNotice => &self.deadlock_notice,
            LetterKind::LicenceNotice => &self.licence_notice,
        }
    }
}

/// What one pass did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LetterPass {
    /// Letters issued and recorded.
    pub issued: u32,
    /// Letters left for a person: no wording, nobody to address, or the
    /// person has died.
    pub left: u32,
    /// Delivery outcomes reported back to the module that asked.
    pub reported: u32,
    pub errors: Vec<String>,
}

pub struct LetterIssuer<M, L> {
    pub complaints: M,
    pub letters: L,
    pub templates: LetterTemplates,
    /// Who the commands are recorded as.
    pub actor: String,
    pub clock: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
    /// The most letters one pass takes.
    pub batch: usize,
}

/// The document a letter is, the same on every pass.
pub fn document_id(letter: &LetterDue) -> String {
    match letter.kind {
        LetterKind::Acknowledgement => format!("complaint-ack:{}", letter.subject),
        LetterKind::DeadlockNotice => format!("complaint-deadlock:{}", letter.subject),
        LetterKind::LicenceNotice => format!("licence-notice:{}:{}", letter.subject, letter.party),
    }
}

fn format_of(contact: &Contact) -> Option<AlternateFormat> {
    match contact.format.as_deref() {
        Some("large_print") => Some(AlternateFormat::LargePrint),
        Some("audio") => Some(AlternateFormat::Audio),
        Some("braille") => Some(AlternateFormat::Braille),
        _ => None,
    }
}

impl<M: Complaints, L: Letters> LetterIssuer<M, L> {
    fn request(&self, letter: &LetterDue, contact: &Contact) -> IssueRequest {
        let to = |party: &PartyId, role| Recipient {
            party: party.clone(),
            role,
            relationship: None,
            channels: vec![Channel::Email, Channel::Post],
        };
        let mut recipients = vec![to(&letter.party, RecipientRole::Addressee)];
        let mut seen: BTreeSet<&PartyId> = BTreeSet::from([&letter.party]);
        for representative in letter.representatives.iter().filter(|r| seen.insert(r)) {
            recipients.push(to(representative, RecipientRole::Representative));
        }
        let mut data = json!({
            "name": contact.name,
            "reference": letter.subject,
            "date": letter.since.to_string(),
        });
        if let Some(due) = letter.due {
            data["due"] = json!(due.to_string());
        }
        for (key, value) in &letter.detail {
            data[key] = json!(value);
        }
        IssueRequest {
            document: document_id(letter),
            caller: "complaints".into(),
            template: self.templates.of(letter.kind).to_string(),
            template_version: None,
            policy: None,
            other_policies: vec![],
            data_class: DataClass::Personal,
            lives: vec![],
            // A complainant can show they were never told; the insurer
            // must be able to show they were.
            prove_delivery: true,
            owner: letter.party.clone(),
            recipients,
            language: contact.language.clone().unwrap_or_else(|| "en".into()),
            format: format_of(contact),
            data,
            // A notice names what requires it: here, the register's entry.
            trigger_event: Some(format!("complaints:{}:{:?}", letter.subject, letter.kind)),
            case: Some(letter.subject.clone()),
            supersedes: None,
            reason: None,
            wordings: vec![],
            components: vec![],
            notice: None,
        }
    }

    async fn record(&self, letter: &LetterDue, document: String) -> Result<(), DriverError> {
        let command = match letter.kind {
            LetterKind::Acknowledgement => Command::RecordAcknowledgement {
                complaint: letter.subject.clone(),
                document,
            },
            LetterKind::DeadlockNotice => Command::RecordDeadlockNotice {
                complaint: letter.subject.clone(),
                document,
            },
            LetterKind::LicenceNotice => Command::RecordCustomerNotified {
                case: letter.subject.clone(),
                customer: letter.party.clone(),
                document,
            },
        };
        let at = (self.clock)();
        let applied = self
            .complaints
            .propose(Envelope {
                // The attempt is in the key, so a refusal that was right
                // once is not remembered against a later, different state.
                key: format!("letter:{}:{}", document_id(letter), at.timestamp()),
                caller: "documents".into(),
                actor: self.actor.clone(),
                at,
                command,
            })
            .await?;
        match applied.result {
            Ok(()) => Ok(()),
            // Someone recorded it between the question and now: it is done.
            Err(reject) if matches!(reject.code.as_str(), "already_recorded" | "no_deadlock" | "not_outstanding" | "closed") => Ok(()),
            Err(reject) => Err(DriverError::Refused(reject)),
        }
    }

    /// Issue one letter and record it. `Ok(false)` leaves it for a person.
    async fn one(&self, letter: &LetterDue, pass: &mut LetterPass) -> Result<bool, DriverError> {
        let contact = match self.letters.person(&letter.party).await {
            Ok(Some(contact)) if !contact.deceased => contact,
            Ok(Some(_)) => {
                pass.errors
                    .push(format!("{}: {} has died; the letter is left for a person", letter.subject, letter.party));
                return Ok(false);
            }
            Ok(None) => {
                pass.errors.push(format!(
                    "{}: {} has no contact point; the letter is left for a person",
                    letter.subject, letter.party
                ));
                return Ok(false);
            }
            Err(e) => return Err(DriverError::Unavailable(e.to_string())),
        };
        let request = self.request(letter, &contact);
        let document = match self.letters.issue(request.clone()).await {
            // The template is not offered in the format the person asked
            // for: the letter goes in the ordinary one rather than not at all.
            Err(DriverError::Refused(reject)) if reject.code == "no_format" => self.letters.issue(IssueRequest { format: None, ..request }).await?,
            other => other?,
        };
        self.record(letter, document.id).await?;
        Ok(true)
    }

    /// One pass over the letters that are due.
    pub async fn pass(&self) -> LetterPass {
        let mut pass = LetterPass::default();
        let due: Vec<LetterDue> = match self.complaints.ask(Query::LettersDue { limit: self.batch }).await {
            Ok(answer) => match serde_json::from_value(answer) {
                Ok(due) => due,
                Err(e) => {
                    pass.errors.push(format!("letters due: {e}"));
                    return pass;
                }
            },
            Err(e) => {
                pass.errors.push(format!("letters due: {e}"));
                return pass;
            }
        };
        // A kind with no wording is said once, not once a letter.
        let mut no_wording = BTreeSet::new();
        for letter in &due {
            if no_wording.contains(&letter.kind) {
                pass.left += 1;
                continue;
            }
            match self.one(letter, &mut pass).await {
                Ok(true) => pass.issued += 1,
                Ok(false) => pass.left += 1,
                Err(DriverError::NotFound(what)) if what.contains("template") => {
                    no_wording.insert(letter.kind);
                    pass.left += 1;
                    pass.errors.push(format!(
                        "template {} has no active version; its letters are left for a person",
                        self.templates.of(letter.kind)
                    ));
                }
                Err(e) => pass.errors.push(format!("{}: {e}", document_id(letter))),
            }
        }
        pass
    }
}

/// Run the step every `every` until `stop` completes.
pub async fn run_letters<M: Complaints, L: Letters>(
    issuer: LetterIssuer<M, L>,
    every: Duration,
    stop: impl Future<Output = ()>,
    mut seen: impl FnMut(&LetterPass),
) {
    tokio::pin!(stop);
    loop {
        let pass = issuer.pass().await;
        if pass.errors.is_empty() {
            tracing::debug!(issued = pass.issued, "complaint letters pass");
        } else {
            tracing::warn!(issued = pass.issued, left = pass.left, errors = ?pass.errors, "a complaint letters pass could not do everything");
        }
        seen(&pass);
        tokio::select! {
            () = &mut stop => return,
            () = tokio::time::sleep(every) => {}
        }
    }
}
