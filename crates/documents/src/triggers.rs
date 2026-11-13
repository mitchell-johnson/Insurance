//! Which documents the core's events require.
//!
//! A pure function of one event. The notice event is the trigger, so a
//! notice is rendered after the fact it reports and can never say
//! something the ledger does not.

use std::collections::BTreeMap;

use ic_core::{Event, EventRecord, PolicyStatus};
use serde_json::json;

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
        detail: BTreeMap::new(),
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
        Event::StandingChanged {
            policy, to, oldest_unpaid_due, ..
        } => standing_notice(*to)
            .map(|(kind, purpose)| {
                let mut t = trigger(record, policy, kind, Some(purpose));
                if let Some(due) = oldest_unpaid_due {
                    t.detail.insert("oldest_unpaid_due".into(), json!(due.to_string()));
                }
                t
            })
            .into_iter()
            .collect(),
        Event::PolicyStatusChanged {
            policy,
            to: to @ (PolicyStatus::Lapsed | PolicyStatus::Cancelled | PolicyStatus::Expired | PolicyStatus::Surrendered | PolicyStatus::Matured),
            ..
        } => {
            let mut t = trigger(record, policy, DocumentKind::GeneralCorrespondence, None);
            t.detail.insert("ended".into(), json!(to));
            vec![t]
        }
        // The decision itself, not the change of status that goes with it,
        // so a policy avoided is written to once, with the avoidance letter.
        Event::PolicyAvoided { policy, .. } => vec![trigger(record, policy, DocumentKind::AvoidanceNotice, None)],
        Event::ReceiptApplied {
            policy,
            receipt,
            allocated_minor,
            to_suspense_minor,
            ..
        } => {
            let mut t = trigger(record, policy, DocumentKind::PremiumReceipt, None);
            t.detail.insert("receipt".into(), json!(receipt));
            t.detail.insert("allocated".into(), json!(allocated_minor));
            t.detail.insert("to_suspense".into(), json!(to_suspense_minor));
            vec![t]
        }
        // A premium notice for each bill on a policy of its own. A scheme's
        // bill is the employer's invoice, which group schemes sends.
        Event::PremiumDue {
            policy,
            receivable,
            period_start,
            period_end,
            total_minor,
            members: None,
            by_cover_type,
            discounts,
        } if *total_minor > 0 => {
            let premium: i64 = by_cover_type.values().sum();
            let discounted: i64 = discounts.iter().map(|d| d.amount_minor).sum();
            let lines: Vec<_> = discounts.iter().map(|d| json!({ "name": d.name, "amount": d.amount_minor })).collect();
            let mut t = trigger(record, policy, DocumentKind::Invoice, None);
            t.detail.insert("receivable".into(), json!(receivable));
            t.detail.insert("period_start".into(), json!(period_start.to_string()));
            t.detail.insert("period_end".into(), json!(period_end.to_string()));
            t.detail.insert("base".into(), json!(premium + discounted));
            t.detail.insert("discounts".into(), json!(lines));
            t.detail.insert("net_premium".into(), json!(premium));
            t.detail.insert("charges".into(), json!(total_minor - premium));
            t.detail.insert("total".into(), json!(total_minor));
            vec![t]
        }
        _ => vec![],
    }
}
