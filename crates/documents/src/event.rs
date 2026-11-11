//! Events: what the module tells the rest of the insurer. Opaque IDs,
//! dates and hashes only.

use chrono::{DateTime, NaiveDate, Utc};
use ic_core::{PartyId, PolicyId};
use serde::{Deserialize, Serialize};

use crate::kinds::DocumentKind;
use crate::model::Channel;
use crate::template::TemplateRef;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRecord {
    /// `doc-<seq>`. Consumers de-duplicate on it.
    pub id: String,
    /// Gapless.
    pub seq: u64,
    pub at: DateTime<Utc>,
    pub log_index: u64,
    pub event: Event,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    SettingsActivated {
        version: u32,
    },
    RetentionActivated {
        version: u32,
    },
    PartRegistered {
        id: String,
        version: u32,
        content_hash: String,
    },
    TemplateSubmitted {
        template: TemplateRef,
        content_hash: String,
    },
    TemplateApproved {
        template: TemplateRef,
        role: crate::command::ApprovalRole,
    },
    RenderingTested {
        template: TemplateRef,
        cases: u32,
        failures: u32,
    },
    TemplateActivated {
        template: TemplateRef,
        content_hash: String,
        effective_from: NaiveDate,
    },
    TemplateDisabled {
        template: TemplateRef,
        case: String,
    },

    DocumentIssued {
        document: String,
        kind: DocumentKind,
        policy: Option<PolicyId>,
        owner: PartyId,
        template: Option<TemplateRef>,
        content_sha256: String,
        supersedes: Option<String>,
    },
    DocumentSuperseded {
        document: String,
        by: String,
    },
    DeliveryRequested {
        document: String,
        party: PartyId,
        channel: Channel,
    },
    DeliveryConfirmed {
        document: String,
        party: PartyId,
        channel: Channel,
        on: NaiveDate,
    },
    DeliveryFailed {
        document: String,
        party: PartyId,
        channel: Channel,
    },
    /// The contact point bounced: the customer master should mark it
    /// unverified and raise a task to confirm it.
    ContactPointBounced {
        party: PartyId,
        channel: Channel,
    },
    /// An exception item: nobody could be reached on any channel.
    DocumentUndeliverable {
        document: String,
        party: PartyId,
    },

    /// Proof of delivery to every addressee of a notice with a clock.
    NoticeDelivered {
        notice: String,
        policy: Option<PolicyId>,
        purpose: String,
        delivered_on: NaiveDate,
        earliest_effect: NaiveDate,
    },
    /// An exception item. Whatever depends on the notice waits.
    NoticeUndeliverable {
        notice: String,
        policy: Option<PolicyId>,
        purpose: String,
    },
    NoticeDefectRaised {
        defect: String,
        case: String,
        notice: String,
        policy: Option<PolicyId>,
        purpose: String,
    },
    /// A corrected notice is proven delivered: the clock runs from it.
    NoticeClockRestarted {
        notice: String,
        policy: Option<PolicyId>,
        purpose: String,
        delivered_on: NaiveDate,
        earliest_effect: NaiveDate,
    },
    /// Choices made on the defective notice may be made again.
    ReopenPeriodOpened {
        reopen_reference: String,
        defect: String,
        policy: Option<PolicyId>,
        purpose: String,
        corrected_notice: String,
        from: NaiveDate,
        until: NaiveDate,
    },
    NoticeExcludedFromDefect {
        defect: String,
        notice: String,
    },
    NoticeDefectClosed {
        defect: String,
        notices: u32,
        excluded: u32,
    },

    DocumentRequired {
        trigger: String,
        policy: PolicyId,
        kind: DocumentKind,
    },
    TriggerDismissed {
        trigger: String,
    },
    LegalHoldPlaced {
        case: String,
    },
    LegalHoldReleased,
    PartyShredRequested {
        party: PartyId,
    },
    DocumentDestroyed {
        document: String,
    },
    PartyKeyDestroyed {
        party: PartyId,
    },
}
