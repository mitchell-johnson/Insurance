//! Which documents the core's events require.
//!
//! A pure function of one event. The notice event is the trigger, so a
//! notice is rendered after the fact it reports and can never say
//! something the ledger does not.

use ic_core::{Event, EventRecord, PolicyStatus};

use crate::kinds::DocumentKind;
use crate::model::Trigger;
use crate::state::standing_notice;

fn trigger(record: &EventRecord, policy: &ic_core::PolicyId, kind: DocumentKind, purpose: Option<&str>) -> Trigger {
    Trigger {
        id: format!("{}:{kind:?}", record.id),
        event: record.id.clone(),
        policy: policy.clone(),
        kind,
        purpose: purpose.map(str::to_string),
        business_date: record.business_date,
        copy_to: vec![],
    }
}

/// The event a schedule answers: one per policy per business date, however
/// many changes the day brought.
pub fn schedule_event(policy: &ic_core::PolicyId, on: chrono::NaiveDate) -> String {
    format!("schedule:{}:{on}", policy.0)
}

fn schedule(record: &EventRecord, policy: &ic_core::PolicyId) -> Trigger {
    let event = schedule_event(policy, record.business_date);
    Trigger {
        id: format!("{event}:{:?}", DocumentKind::PolicySchedule),
        event,
        ..trigger(record, policy, DocumentKind::PolicySchedule, None)
    }
}

pub fn triggers(record: &EventRecord) -> Vec<Trigger> {
    match &record.event {
        // A schedule is issued with the policy and reissued after every
        // change to it: the sequence of schedules is its documentary history.
        // Cover taken off by a claim or by the owner ends or reduces layers;
        // a reversed admission writes it back with a layer added.
        Event::PolicyIssued { policy, .. }
        | Event::AnniversaryReached { policy, .. }
        | Event::LayerAdded { policy, .. }
        | Event::LayerReduced { policy, .. }
        | Event::LayerEnded { policy, .. }
        | Event::LayersMerged { policy, .. }
        | Event::CoverExpired { policy, .. }
        | Event::PolicyReinstated { policy } => vec![schedule(record, policy)],
        Event::StandingChanged { policy, to, .. } => standing_notice(*to)
            .map(|(kind, purpose)| trigger(record, policy, kind, Some(purpose)))
            .into_iter()
            .collect(),
        Event::PolicyStatusChanged {
            policy,
            to: PolicyStatus::Lapsed | PolicyStatus::Cancelled | PolicyStatus::Expired | PolicyStatus::Surrendered | PolicyStatus::Matured,
            ..
        } => vec![trigger(record, policy, DocumentKind::GeneralCorrespondence, None)],
        // The decision itself, not the change of status that goes with it,
        // so a policy avoided is written to once, with the avoidance letter.
        Event::PolicyAvoided { policy, .. } => vec![trigger(record, policy, DocumentKind::AvoidanceNotice, None)],
        Event::ReceiptApplied { policy, .. } => vec![trigger(record, policy, DocumentKind::PremiumReceipt, None)],
        _ => vec![],
    }
}
