//! The documents the core's events require, issued from the policy.
//!
//! The module turns the core's events into triggers: a schedule after
//! every change to cover, a lapse or suspension notice when standing
//! changes, the letter that a policy has ended, the avoidance letter, a
//! receipt. A trigger only says a document is due. This step answers each
//! one: it reads the policy from the partition that holds it, finds the
//! active template for the document's kind and the policy's product, and
//! issues the document to the policyholder, with a copy to every party
//! whose interest the trigger lists. Issuing it clears the trigger.
//!
//! A document is left, and the pass says why, when the product has no
//! active template of that kind, or the policyholder has no contact point
//! or has died. A person can dismiss a trigger that should not be
//! answered. Every step is safe to repeat: the document id is made from
//! the trigger.

use std::collections::BTreeSet;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use chrono::NaiveDate;
use ic_core::policy::{Policy, ReceivableKind};
use ic_core::PolicyId;
use ic_raft::ClusterClient;
use serde_json::{json, Value};

use crate::channels::{Channels, Contact, ContactDirectory};
use crate::command::{Command, NoticeSpec};
use crate::complaint_letters::{LetterPass, Letters};
use crate::crypto::KeyWrap;
use crate::driver::{Driver, DriverError, IssueRequest, Module};
use crate::kinds::{DataClass, DocumentKind};
use crate::model::{Channel, Recipient, RecipientRole, Trigger};
use crate::module::Query;
use crate::template::{AlternateFormat, TemplateRef};
use crate::vault::ObjectStore;

/// A policy as the partition that holds it has it, with what billing says
/// is owed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyRecord {
    pub policy: Policy,
    pub outstanding_minor: Option<i64>,
    pub oldest_unpaid_due: Option<NaiveDate>,
}

/// What this step needs of the core: a policy, wherever it is held.
pub trait Policies: Send + Sync {
    fn policy(&self, policy: &PolicyId) -> impl Future<Output = Result<Option<PolicyRecord>, String>> + Send;
}

/// The partitions, through the cluster.
#[derive(Clone)]
pub struct ClusterPolicies {
    client: ClusterClient,
    partitions: u16,
}

impl ClusterPolicies {
    pub fn new(client: ClusterClient, partitions: u16) -> Self {
        Self { client, partitions }
    }
}

impl Policies for ClusterPolicies {
    async fn policy(&self, policy: &PolicyId) -> Result<Option<PolicyRecord>, String> {
        for partition in 0..self.partitions {
            let Some(entry) = self.client.locate(partition, policy).await.map_err(|e| e.to_string())? else {
                continue;
            };
            let Some(view) = self.client.policy_by_number(partition, &entry.number).await.map_err(|e| e.to_string())? else {
                continue;
            };
            return Ok(Some(PolicyRecord {
                policy: view.policy,
                outstanding_minor: view.billing.as_ref().map(|b| b.outstanding_minor),
                oldest_unpaid_due: view.billing.and_then(|b| b.oldest_unpaid_due),
            }));
        }
        Ok(None)
    }
}

/// The document a trigger becomes, the same on every pass.
pub fn document_id(trigger: &Trigger) -> String {
    format!("required:{}", trigger.id)
}

fn text<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// What a template is given for a policy on a date: its number, product,
/// status, covers and sums insured, the premium last billed and what is
/// owed, and the facts of the event. Never a loading, an exclusion or
/// anything else about health.
pub fn policy_data(record: &PolicyRecord, on: NaiveDate) -> Value {
    let p = &record.policy;
    let covers: Vec<Value> = p
        .covers
        .values()
        .map(|c| {
            let layers: Vec<Value> = c
                .layers
                .iter()
                .filter(|l| l.in_force_on(on))
                .map(|l| json!({ "layer": l.id, "life": l.life.0, "start": l.start.to_string(), "sum_insured": l.sum_insured_minor }))
                .collect();
            json!({
                "cover": c.code,
                "type": c.cover_type,
                "status": text(&c.status),
                "start": c.start.to_string(),
                "sum_insured": c.sum_insured_on(on),
                "layers": layers,
            })
        })
        .collect();
    let premium = p.receivables.iter().rev().find(|r| r.kind == ReceivableKind::Period).map(|r| r.total_minor);
    json!({
        "policy": p.number,
        "product": p.product_code,
        "status": text(&p.status),
        "inception": p.inception.to_string(),
        "frequency": text(&p.frequency),
        "currency": text(&p.currency),
        "covers": covers,
        "premium": premium,
        "outstanding": record.outstanding_minor,
        "oldest_unpaid_due": record.oldest_unpaid_due.map(|d| d.to_string()),
    })
}

fn format_of(contact: &Contact) -> Option<AlternateFormat> {
    match contact.format.as_deref() {
        Some("large_print") => Some(AlternateFormat::LargePrint),
        Some("audio") => Some(AlternateFormat::Audio),
        Some("braille") => Some(AlternateFormat::Braille),
        _ => None,
    }
}

/// Money owed or paid is financial; a schedule or letter is personal.
fn class_of(kind: DocumentKind) -> DataClass {
    match kind {
        DocumentKind::LapseNotice
        | DocumentKind::SuspensionNotice
        | DocumentKind::PremiumReceipt
        | DocumentKind::Invoice
        | DocumentKind::PremiumChangeNotice => DataClass::Financial,
        _ => DataClass::Personal,
    }
}

/// Why a trigger was left this pass.
enum Left {
    /// The product has no active template of the kind: said once a pass.
    NoTemplate(DocumentKind, String),
    Said(String),
    /// A premium notice for a product that sends none: dismissed.
    NotSent,
}

impl<M: Module, S: ObjectStore, W: KeyWrap, C: Channels, D: ContactDirectory> Driver<M, S, W, C, D> {
    async fn template_for_kind(&self, kind: DocumentKind, product: &str, on: NaiveDate) -> Result<Option<TemplateRef>, DriverError> {
        let question = Query::TemplateForKind {
            kind,
            product: product.to_string(),
            on,
        };
        let answer = self.module.ask(question).await?;
        serde_json::from_value(answer).map_err(|e| DriverError::Answer(e.to_string()))
    }

    /// Answer one trigger. `Ok(Err(left))` leaves it.
    async fn required_document<P: Policies>(&self, policies: &P, trigger: &Trigger) -> Result<Result<(), Left>, DriverError> {
        let record = policies
            .policy(&trigger.policy)
            .await
            .map_err(DriverError::Unavailable)?
            .ok_or_else(|| DriverError::NotFound(format!("policy {}", trigger.policy.0)))?;
        let product = record.policy.product_code.clone();
        let on = trigger.business_date;
        let Some(template) = self.template_for_kind(trigger.kind, &product, on).await? else {
            // A product sends premium notices by having a template for
            // them; a bill on one that has none is not a letter owed.
            if trigger.kind == DocumentKind::Invoice {
                let reason = format!("product {product} has no premium notice on {on}");
                let dismiss = Command::DismissTrigger {
                    trigger: trigger.id.clone(),
                    reason,
                };
                self.send(format!("dismiss:{}", trigger.id), "documents", dismiss).await?;
                return Ok(Err(Left::NotSent));
            }
            return Ok(Err(Left::NoTemplate(trigger.kind, product)));
        };
        // A schedule names the wording it is issued under.
        let wordings = if trigger.kind == DocumentKind::PolicySchedule {
            match self.template_for_kind(DocumentKind::PolicyWording, &product, on).await? {
                Some(wording) => vec![wording],
                None => return Ok(Err(Left::NoTemplate(DocumentKind::PolicyWording, product))),
            }
        } else {
            vec![]
        };
        let holder = record.policy.policyholder.clone();
        let contact = match Letters::person(self, &holder).await {
            Ok(Some(contact)) if !contact.deceased => contact,
            Ok(Some(_)) => return Ok(Err(Left::Said(format!("{holder} has died; the document is left for a person")))),
            Ok(None) => return Ok(Err(Left::Said(format!("{holder} has no contact point; the document is left for a person")))),
            Err(e) => return Err(DriverError::Unavailable(e.to_string())),
        };
        let mut data = policy_data(&record, on);
        data["name"] = json!(contact.name);
        data["date"] = json!(on.to_string());
        for (key, value) in &trigger.detail {
            data[key] = value.clone();
        }
        let to = |party: &ic_core::PartyId, role, relationship: &str| Recipient {
            party: party.clone(),
            role,
            relationship: Some(relationship.to_string()),
            channels: vec![Channel::Email, Channel::Post],
        };
        let mut recipients = vec![to(&holder, RecipientRole::Addressee, "policyholder")];
        let mut seen = BTreeSet::from([holder.clone()]);
        for party in trigger.copy_to.iter().filter(|p| seen.insert((*p).clone())) {
            recipients.push(to(party, RecipientRole::InterestedParty, "interested_party"));
        }
        let request = IssueRequest {
            document: document_id(trigger),
            caller: "documents".into(),
            template: template.id,
            template_version: Some(template.version),
            policy: Some(trigger.policy.clone()),
            other_policies: vec![],
            data_class: class_of(trigger.kind),
            lives: vec![],
            prove_delivery: false,
            owner: holder,
            recipients,
            language: contact.language.clone().unwrap_or_else(|| "en".into()),
            format: format_of(&contact),
            data,
            trigger_event: Some(trigger.event.clone()),
            case: None,
            supersedes: None,
            reason: None,
            wordings,
            components: vec![],
            notice: trigger.purpose.clone().map(|purpose| NoticeSpec { purpose, corrects: None }),
            copy_of: None,
        };
        // The policyholder's representatives see what they are sent; a
        // format not offered falls back to the ordinary one.
        self.issue_with_representatives(request, on).await?;
        Ok(Ok(()))
    }

    /// One pass over the documents the core's events require.
    pub async fn required_documents<P: Policies>(&self, policies: &P) -> LetterPass {
        let mut pass = LetterPass::default();
        let asked = self.module.ask(Query::Triggers { limit: self.batch }).await;
        let due = asked.and_then(|answer| serde_json::from_value::<Vec<Trigger>>(answer).map_err(|e| DriverError::Answer(e.to_string())));
        let due = match due {
            Ok(due) => due,
            Err(e) => {
                pass.errors.push(format!("triggers: {e}"));
                return pass;
            }
        };
        // A product with no template of a kind is said once, not once a policy.
        let mut no_template = BTreeSet::new();
        for trigger in &due {
            match self.required_document(policies, trigger).await {
                Ok(Ok(())) => pass.issued += 1,
                Ok(Err(Left::NoTemplate(kind, product))) => {
                    pass.left += 1;
                    if no_template.insert((kind, product.clone())) {
                        pass.errors
                            .push(format!("product {product} has no active {kind:?} template; its documents are waiting"));
                    }
                }
                Ok(Err(Left::NotSent)) => pass.left += 1,
                Ok(Err(Left::Said(why))) => {
                    pass.left += 1;
                    pass.errors.push(format!("{}: {why}", trigger.id));
                }
                Err(e) => pass.errors.push(format!("{}: {e}", trigger.id)),
            }
        }
        pass
    }
}

/// Run the step every `every` until `stop` completes.
pub async fn run_required_documents<P, M, S, W, C, D>(
    policies: P,
    driver: Arc<Driver<M, S, W, C, D>>,
    every: Duration,
    stop: impl Future<Output = ()>,
    mut seen: impl FnMut(&LetterPass),
) where
    P: Policies,
    M: Module,
    S: ObjectStore,
    W: KeyWrap,
    C: Channels,
    D: ContactDirectory,
{
    tokio::pin!(stop);
    loop {
        let pass = driver.required_documents(&policies).await;
        if pass.errors.is_empty() {
            tracing::debug!(issued = pass.issued, "required documents pass");
        } else {
            tracing::warn!(issued = pass.issued, left = pass.left, errors = ?pass.errors, "a required documents pass could not do everything");
        }
        seen(&pass);
        tokio::select! {
            () = &mut stop => return,
            () = tokio::time::sleep(every) => {}
        }
    }
}
