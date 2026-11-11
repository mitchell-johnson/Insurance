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
    }
}

pub fn triggers(record: &EventRecord) -> Vec<Trigger> {
    match &record.event {
        // A schedule is issued with the policy and reissued after every
        // change to it: the sequence of schedules is its documentary history.
        Event::PolicyIssued { policy, .. }
        | Event::AnniversaryReached { policy, .. }
        | Event::LayerAdded { policy, .. }
        | Event::LayersMerged { policy, .. }
        | Event::CoverExpired { policy, .. }
        | Event::PolicyReinstated { policy } => vec![trigger(record, policy, DocumentKind::PolicySchedule, None)],
        Event::StandingChanged { policy, to, .. } => standing_notice(*to)
            .map(|(kind, purpose)| trigger(record, policy, kind, Some(purpose)))
            .into_iter()
            .collect(),
        Event::PolicyStatusChanged { policy, to, .. } => match to {
            PolicyStatus::Lapsed | PolicyStatus::Cancelled | PolicyStatus::Expired | PolicyStatus::Surrendered | PolicyStatus::Matured => {
                vec![trigger(record, policy, DocumentKind::GeneralCorrespondence, None)]
            }
            _ => vec![],
        },
        Event::ReceiptApplied { policy, .. } => vec![trigger(record, policy, DocumentKind::PremiumReceipt, None)],
        _ => vec![],
    }
}
