//! What the module holds about each document. No personal data: the text
//! and the data it was rendered from are in the vault, encrypted under the
//! owner's key. Here are IDs, hashes, dates and delivery results.

use chrono::{DateTime, NaiveDate, Utc};
use ic_core::{PartyId, PolicyId};
use serde::{Deserialize, Serialize};

use crate::kinds::DocumentKind;
use crate::template::{AlternateFormat, TemplateRef};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    /// The secure in-app inbox. Proven when the document is opened there.
    InApp,
    Push,
    Email,
    Sms,
    /// Printed post. Deemed delivered a set number of days after lodgement
    /// unless it is returned.
    Post,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecipientRole {
    /// The person the document is for. A notice's clock runs from proven
    /// delivery to every addressee.
    Addressee,
    /// Someone whose interest entitles them to the document, such as a
    /// lender the policy is assigned to.
    InterestedParty,
    Adviser,
    Representative,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recipient {
    pub party: PartyId,
    pub role: RecipientRole,
    /// The relationship the document is sent under (policyholder, life
    /// insured, lender). Its contact points for notices are the ones
    /// used, and proof of delivery is recorded against it.
    #[serde(default)]
    pub relationship: Option<String>,
    /// The channels to try, in order, from the party's preferences. Contact
    /// details are not here: they are fetched at send time.
    pub channels: Vec<Channel>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    /// The channel confirmed delivery (a delivery receipt, the document
    /// opened in the app, or post not returned in the deemed period).
    Delivered,
    Bounced {
        reason: String,
    },
    Failed {
        reason: String,
    },
    /// The customer master holds no contact point for this channel.
    NoContactPoint,
    /// Sent, and not confirmed in the time allowed.
    TimedOut,
    /// Post came back.
    Returned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    pub channel: Channel,
    pub requested_at: DateTime<Utc>,
    #[serde(default)]
    pub sent_at: Option<DateTime<Utc>>,
    /// The destination with most of it hidden, for the delivery record.
    #[serde(default)]
    pub masked_destination: Option<String>,
    #[serde(default)]
    pub provider_reference: Option<String>,
    #[serde(default)]
    pub outcome: Option<AttemptOutcome>,
    #[serde(default)]
    pub outcome_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    InProgress,
    Delivered { on: NaiveDate, channel: Channel },
    Undeliverable,
}

/// One recipient's delivery: the immutable record of every attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delivery {
    pub recipient: Recipient,
    /// The channels that will be tried. For a document that must be proven,
    /// post is always the last resort.
    pub chain: Vec<Channel>,
    pub attempts: Vec<Attempt>,
    pub status: DeliveryStatus,
}

/// The clock of a notice that must be given a set time before something.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    /// What the notice is notice of: `renewal`, `lapse`, `premium_change`.
    pub purpose: String,
    pub minimum_days: u32,
    /// When delivery to every addressee was proven.
    #[serde(default)]
    pub delivered_on: Option<NaiveDate>,
    /// The first day the thing the notice warns of may take effect.
    #[serde(default)]
    pub earliest_effect: Option<NaiveDate>,
    #[serde(default)]
    pub undeliverable: bool,
    /// The defect raised against this notice. A defective notice is treated
    /// as not given.
    #[serde(default)]
    pub defect: Option<String>,
    /// For a corrected notice: the defect and the notice it corrects.
    #[serde(default)]
    pub corrects: Option<Correction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Correction {
    pub defect: String,
    pub original: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Document {
    pub id: String,
    pub kind: DocumentKind,
    /// The template version that rendered it. None for a legacy document
    /// brought in as received.
    pub template: Option<TemplateRef>,
    pub template_hash: Option<String>,
    pub policy: Option<PolicyId>,
    /// Further policies one document covers: a household's renewals in one
    /// notice, or one message for every policy in arrears.
    pub other_policies: Vec<PolicyId>,
    pub data_class: crate::kinds::DataClass,
    /// The lives the document concerns.
    pub lives: Vec<String>,
    /// Whether delivery must be provable.
    pub proof: bool,
    /// The party whose key the document is encrypted under.
    pub owner: PartyId,
    pub language: String,
    pub format: Option<AlternateFormat>,
    /// SHA-256 of the document exactly as issued.
    pub content_sha256: String,
    /// SHA-256 of the data snapshot it was rendered from.
    pub data_sha256: Option<String>,
    /// The vault object.
    pub blob: String,
    /// The document's own key, wrapped by the owner's key. Gone once the
    /// document is destroyed, which makes the vault object unreadable.
    pub wrapped_key: Option<String>,
    pub bytes: u64,
    pub issued_at: DateTime<Utc>,
    pub issued_on: NaiveDate,
    /// The event that required the document.
    pub trigger_event: Option<String>,
    pub case: Option<String>,
    /// Why this version exists, where it replaces another.
    pub reason: Option<String>,
    pub supersedes: Option<String>,
    pub superseded_by: Option<String>,
    /// For a schedule: the wording versions it was issued under.
    pub wordings: Vec<TemplateRef>,
    /// The components (covers) the document concerns.
    pub components: Vec<String>,
    pub notice: Option<Notice>,
    pub deliveries: Vec<Delivery>,
    /// The first day the document may be destroyed, once known.
    pub destroy_after: Option<NaiveDate>,
    pub destroyed_on: Option<NaiveDate>,
    pub legacy: bool,
}

impl Document {
    pub fn requires_proof(&self) -> bool {
        self.proof
    }

    /// Every policy the document covers.
    pub fn policies(&self) -> impl Iterator<Item = &PolicyId> {
        self.policy.iter().chain(self.other_policies.iter())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionBasis {
    /// Kept for a period after it was issued.
    Issue,
    /// Kept for the life of the policy and a period after it ends.
    PolicyEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionRule {
    pub basis: RetentionBasis,
    pub days: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DefectCause {
    TemplateError,
    WrongRate,
    WrongDiscount,
    WrongFee,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DefectItem {
    /// No corrected notice yet.
    Outstanding,
    /// A corrected notice is issued and not yet proven delivered.
    Corrected { by: String },
    /// The corrected notice is proven delivered and the choice reopened.
    Reopened {
        by: String,
        reference: String,
        from: NaiveDate,
        until: NaiveDate,
    },
    /// Left out, with the reason.
    Excluded { reason: String },
}

/// A notice defect: raised by a remediation case against the notices that
/// went out with wrong content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Defect {
    pub id: String,
    pub case: String,
    pub description: String,
    pub cause: DefectCause,
    pub from: NaiveDate,
    pub to: NaiveDate,
    /// How long a choice made on the defective notice stays reopenable
    /// after the corrected notice is delivered.
    pub grace_days: u32,
    pub raised_by: String,
    pub notices: std::collections::BTreeMap<String, DefectItem>,
    pub closed: Option<DefectClosure>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefectClosure {
    pub by: String,
    pub on: NaiveDate,
    /// The control that should have caught it, as recorded at close.
    pub control: String,
}

/// Something the core must be told, held until the driver confirms it was.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreInstruction {
    RecordNoticeDelivery {
        policy: PolicyId,
        purpose: String,
        notice: String,
        delivered_on: NaiveDate,
        earliest_effect: NaiveDate,
    },
    OpenReopenWindow {
        policy: PolicyId,
        reopen_reference: String,
        purpose: String,
        from: NaiveDate,
        until: NaiveDate,
    },
}

/// A document the core's events say is now required.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trigger {
    /// `<event id>:<kind>`.
    pub id: String,
    pub event: String,
    pub policy: PolicyId,
    pub kind: DocumentKind,
    /// For a notice with a clock.
    pub purpose: Option<String>,
    pub business_date: NaiveDate,
}
