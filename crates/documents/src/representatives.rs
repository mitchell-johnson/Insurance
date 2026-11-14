//! Copies for the people who represent a customer.
//!
//! An attorney, guardian or authorised representative whose scope lets
//! them see what the customer is sent (`view` or `all`, from the customer
//! master) gets what the customer gets: on the document itself when they
//! read its language, or as a copy rendered from the same data in their
//! own language and format (`copy_of`). A copy names the event its
//! original answers but runs no notice clock and clears no trigger; the
//! original does both. Nothing clinical is copied:
//! a letter about a person's health is theirs alone.

use std::collections::BTreeSet;

use chrono::NaiveDate;
use ic_core::PartyId;

use crate::channels::{Channels, Contact, ContactDirectory};
use crate::complaint_letters::Letters;
use crate::crypto::KeyWrap;
use crate::driver::{Driver, DriverError, IssueRequest, Module};
use crate::model::{Channel, Recipient, RecipientRole};
use crate::template::AlternateFormat;
use crate::vault::ObjectStore;

/// The relationship a representative's contact point is looked up under.
pub const RELATIONSHIP: &str = "representative";

/// The document id of a representative's copy.
pub fn copy_id(document: &str, representative: &PartyId) -> String {
    format!("{document}:copy:{}", representative.0)
}

fn format_of(contact: &Contact) -> Option<AlternateFormat> {
    match contact.format.as_deref() {
        Some("large_print") => Some(AlternateFormat::LargePrint),
        Some("audio") => Some(AlternateFormat::Audio),
        Some("braille") => Some(AlternateFormat::Braille),
        _ => None,
    }
}

fn recipient(party: &PartyId, role: RecipientRole) -> Recipient {
    Recipient {
        party: party.clone(),
        role,
        relationship: Some(RELATIONSHIP.to_string()),
        channels: vec![Channel::Email, Channel::Post],
    }
}

impl<M: Module, S: ObjectStore, W: KeyWrap, C: Channels, D: ContactDirectory> Driver<M, S, W, C, D> {
    /// Issue `request` with its owner's representatives on `on`: those who
    /// read its language are added to it, and each other is sent a copy in
    /// their own language first, so that a failure leaves the original
    /// (and the trigger it answers) to be tried again with its copies.
    /// A representative already among the recipients, or with no contact
    /// point, or who has died, gets nothing more.
    pub async fn issue_with_representatives(&self, mut request: IssueRequest, on: NaiveDate) -> Result<(), DriverError> {
        let representatives = self
            .directory
            .representatives(&request.owner, on)
            .await
            .map_err(|e| DriverError::Unavailable(e.to_string()))?;
        let already: BTreeSet<PartyId> = request.recipients.iter().map(|r| r.party.clone()).collect();
        for party in representatives.iter().filter(|p| !already.contains(*p)) {
            let contact = match Letters::person(self, party).await {
                Ok(Some(contact)) if !contact.deceased => contact,
                Ok(_) => continue,
                Err(e) => return Err(DriverError::Unavailable(e.to_string())),
            };
            let language = contact.language.clone().unwrap_or_else(|| "en".into());
            if language == request.language {
                request.recipients.push(recipient(party, RecipientRole::Representative));
                continue;
            }
            let copy = IssueRequest {
                document: copy_id(&request.document, party),
                // The copy is addressed to them, as their representative.
                recipients: vec![recipient(party, RecipientRole::Addressee)],
                language,
                format: format_of(&contact),
                notice: None,
                copy_of: Some(request.document.clone()),
                ..request.clone()
            };
            self.issue_or_ordinary(copy).await?;
        }
        self.issue_or_ordinary(request).await
    }

    /// Issue, in the ordinary format when the one asked for is not offered.
    pub async fn issue_or_ordinary(&self, request: IssueRequest) -> Result<(), DriverError> {
        match self.issue(request.clone()).await {
            Err(DriverError::Refused(reject)) if reject.code == "no_format" => self.issue(IssueRequest { format: None, ..request }).await.map(|_| ()),
            other => other.map(|_| ()),
        }
    }
}
