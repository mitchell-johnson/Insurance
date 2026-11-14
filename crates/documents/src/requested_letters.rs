//! The letters other modules ask for.
//!
//! Underwriting and reinsurance cannot issue a document themselves: they
//! record a `RequestLetter` with the facts and the codes the wording
//! explains, and this step issues each one from its template, to the
//! person or firm named. The document id is made from the request, and
//! the module marks the request issued with the document, so a letter is
//! issued once however often a pass is cut short.
//!
//! A letter is left waiting, and the pass says why, when its template has
//! no wording yet, or the person has no contact point or has died; a
//! person cancels it if it should never go.

use std::collections::BTreeSet;
use std::time::Duration;

use serde_json::json;

use crate::channels::{Channels, ContactDirectory};
use crate::complaint_letters::{LetterPass, Letters};
use crate::crypto::KeyWrap;
use crate::driver::{Driver, DriverError, IssueRequest, Module};
use crate::kinds::DataClass;
use crate::model::{requested_document, requested_kind, Channel, LetterRequestRecord, Recipient, RecipientRole};
use crate::module::Query;
use crate::template::AlternateFormat;
use crate::vault::ObjectStore;

fn format_of(format: Option<&str>) -> Option<AlternateFormat> {
    match format {
        Some("large_print") => Some(AlternateFormat::LargePrint),
        Some("audio") => Some(AlternateFormat::Audio),
        Some("braille") => Some(AlternateFormat::Braille),
        _ => None,
    }
}

impl<M: Module, S: ObjectStore, W: KeyWrap, C: Channels, D: ContactDirectory> Driver<M, S, W, C, D> {
    /// Issue one requested letter. `Ok(false)` leaves it waiting.
    async fn requested_letter(&self, record: &LetterRequestRecord, pass: &mut LetterPass) -> Result<bool, DriverError> {
        let letter = &record.request;
        // The module accepted it, so the kind was known then: a kind since
        // retired is for a person to cancel.
        let kind = requested_kind(&record.caller, &letter.kind).ok_or_else(|| DriverError::NotFound(format!("letter kind {}", letter.kind)))?;
        let party = letter.recipient.party();
        let contact = match Letters::person(self, party).await {
            Ok(Some(contact)) if !contact.deceased => contact,
            Ok(Some(_)) => {
                pass.errors
                    .push(format!("{}: {party} has died; the letter is left for a person", letter.request));
                return Ok(false);
            }
            Ok(None) => {
                pass.errors
                    .push(format!("{}: {party} has no contact point; the letter is left for a person", letter.request));
                return Ok(false);
            }
            Err(e) => return Err(DriverError::Unavailable(e.to_string())),
        };
        let mut data = json!({
            "name": contact.name,
            "reference": letter.subject,
            "date": letter.business_date.to_string(),
            "reasons": letter.reasons,
        });
        if let Some(policy) = &letter.policy {
            data["policy"] = json!(policy.0);
        }
        if let Some(life) = &letter.life {
            data["life"] = json!(life);
        }
        for (key, value) in &letter.detail {
            data[key] = json!(value);
        }
        let request = IssueRequest {
            document: requested_document(&letter.request),
            caller: "documents".into(),
            template: kind.template.to_string(),
            template_version: None,
            policy: letter.policy.clone(),
            other_policies: vec![],
            // A letter about a person's health is theirs alone.
            data_class: if kind.clinical { DataClass::Clinical } else { DataClass::Personal },
            lives: letter.life.iter().cloned().collect(),
            prove_delivery: kind.proven,
            owner: party.clone(),
            recipients: vec![Recipient {
                party: party.clone(),
                role: RecipientRole::Addressee,
                relationship: None,
                channels: vec![Channel::Email, Channel::Post],
            }],
            language: contact.language.clone().unwrap_or_else(|| "en".into()),
            format: format_of(contact.format.as_deref()),
            data,
            trigger_event: Some(format!("{}:{}", record.caller, letter.request)),
            case: Some(letter.subject.clone()),
            supersedes: None,
            reason: None,
            wordings: vec![],
            components: vec![],
            notice: None,
            copy_of: None,
        };
        if kind.clinical {
            // A letter about a person's health is theirs alone.
            self.issue_or_ordinary(request).await?;
        } else {
            self.issue_with_representatives(request, letter.business_date).await?;
        }
        Ok(true)
    }

    /// One pass over the letters other modules have asked for.
    pub async fn requested_letters(&self) -> LetterPass {
        let mut pass = LetterPass::default();
        let asked = self.module.ask(Query::LettersRequested { limit: self.batch }).await;
        let due = asked.and_then(|answer| serde_json::from_value::<Vec<LetterRequestRecord>>(answer).map_err(|e| DriverError::Answer(e.to_string())));
        let due = match due {
            Ok(due) => due,
            Err(e) => {
                pass.errors.push(format!("letters requested: {e}"));
                return pass;
            }
        };
        // A template with no wording is said once, not once a letter.
        let mut no_wording = BTreeSet::new();
        for record in &due {
            let template = requested_kind(&record.caller, &record.request.kind).map(|k| k.template);
            if template.is_some_and(|t| no_wording.contains(t)) {
                pass.left += 1;
                continue;
            }
            match self.requested_letter(record, &mut pass).await {
                Ok(true) => pass.issued += 1,
                Ok(false) => pass.left += 1,
                Err(DriverError::NotFound(what)) if what.contains("template") => {
                    let template = template.unwrap_or_default();
                    no_wording.insert(template);
                    pass.left += 1;
                    pass.errors
                        .push(format!("template {template} has no active version; its letters are left for a person"));
                }
                Err(e) => pass.errors.push(format!("{}: {e}", requested_document(&record.request.request))),
            }
        }
        pass
    }
}

/// Run the step every `every` until `stop` completes.
pub async fn run_requested_letters<M, S, W, C, D>(
    driver: std::sync::Arc<Driver<M, S, W, C, D>>,
    every: Duration,
    stop: impl std::future::Future<Output = ()>,
    mut seen: impl FnMut(&LetterPass),
) where
    M: Module,
    S: ObjectStore,
    W: KeyWrap,
    C: Channels,
    D: ContactDirectory,
{
    tokio::pin!(stop);
    loop {
        let pass = driver.requested_letters().await;
        if pass.errors.is_empty() {
            tracing::debug!(issued = pass.issued, "requested letters pass");
        } else {
            tracing::warn!(issued = pass.issued, left = pass.left, errors = ?pass.errors, "a requested letters pass could not do everything");
        }
        seen(&pass);
        tokio::select! {
            () = &mut stop => return,
            () = tokio::time::sleep(every) => {}
        }
    }
}
