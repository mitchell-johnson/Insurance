//! A collection incident (COL-024): many collections fail together for a
//! reason that is not the payers' doing, such as payment details the
//! provider invalidated in bulk before a billing day. Failures on the
//! authorities named, inside the incident's dates, are tagged to it: they
//! are not counted as dishonours, do not suspend or end an authority and
//! are not presented again; collections is asked to get new details from
//! each payer; and the incident lists every policy affected, so the
//! remediation case can put right any standing that advanced. Payments
//! charges no dishonour fee in any case.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::machine::{Event, PaymentsState, Tx};
use crate::model::{Counterparty, Id, Instruction, ItemStatus, OutboxEntry, Reject, Role};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Incident {
    pub reason: String,
    pub authorities: BTreeSet<Id>,
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub declared_by: String,
    /// Failed collections tagged to it, with their policy and value date.
    #[serde(default)]
    pub items: BTreeMap<Id, (Option<ic_core::PolicyId>, NaiveDate)>,
}

impl PaymentsState {
    pub(crate) fn declare_incident(
        &mut self,
        tx: &mut Tx<'_>,
        id: Id,
        authorities: BTreeSet<Id>,
        from: NaiveDate,
        to: NaiveDate,
        reason: String,
    ) -> Result<(), Reject> {
        tx.need(Role::Reconciler)?;
        if id.is_empty() || reason.trim().is_empty() || authorities.is_empty() || from > to {
            return Err(Reject::new(
                "invalid_incident",
                "an incident has an ID, a reason, the authorities it hit and its dates",
            ));
        }
        if self.incidents.contains_key(&id) {
            return Err(Reject::new("duplicate_incident", format!("incident {id} already exists")));
        }
        if let Some(a) = authorities.iter().find(|a| !self.authorities.contains_key(*a)) {
            return Err(Reject::new("unknown_authority", format!("there is no authority {a}")));
        }
        let mut incident = Incident {
            reason,
            authorities,
            from,
            to,
            declared_by: tx.actor.id.clone(),
            items: BTreeMap::new(),
        };
        // Failures already recorded inside its dates are taken back as
        // dishonours and tagged.
        let mut earlier = Vec::new();
        for run in self
            .runs
            .values()
            .filter(|r| r.run_type.inbound() && r.value_date >= from && r.value_date <= to)
        {
            for (item_id, item) in &run.items {
                let Counterparty::Authority(a) = &item.new.counterparty else { continue };
                if matches!(item.status, ItemStatus::Returned(_)) && incident.authorities.contains(a) {
                    incident.items.insert(item_id.clone(), (item.new.policy.clone(), run.value_date));
                    earlier.push((a.clone(), item_id.clone()));
                }
            }
        }
        for (a, item) in &earlier {
            if let Some(authority) = self.authorities.get_mut(a) {
                authority.dishonours = authority.dishonours.saturating_sub(1);
            }
            self.represent.remove(item);
        }
        let asked: BTreeSet<Id> = earlier.into_iter().map(|(a, _)| a).collect();
        for a in &asked {
            self.ask_for_new_details(&id, a);
        }
        tx.emit(Event::IncidentDeclared { incident: id.clone() });
        self.incidents.insert(id, incident);
        Ok(())
    }

    /// The incident a failed collection on this authority belongs to, if any.
    pub(crate) fn incident_for(&self, authority: &str, value_date: NaiveDate) -> Option<Id> {
        self.incidents
            .iter()
            .find(|(_, i)| i.authorities.contains(authority) && value_date >= i.from && value_date <= i.to)
            .map(|(id, _)| id.clone())
    }

    /// Tag a failed collection to its incident and ask for new details.
    pub(crate) fn tag_to_incident(&mut self, incident: &str, authority: &str, item: &str, policy: Option<ic_core::PolicyId>, value_date: NaiveDate) {
        if let Some(i) = self.incidents.get_mut(incident) {
            i.items.insert(item.to_string(), (policy, value_date));
        }
        self.ask_for_new_details(incident, authority);
    }

    fn ask_for_new_details(&mut self, incident: &str, authority: &str) {
        let policy = self.authorities.get(authority).map(|a| a.policy.to_string()).unwrap_or_default();
        self.push_outbox(OutboxEntry {
            key: format!("incident-{incident}-{authority}"),
            instruction: Instruction::Notify {
                to: Role::Collections,
                subject: "new_details_needed".to_string(),
                detail: format!(
                    "incident {incident}: ask the payer on policy {policy} (authority {authority}) for new payment details; no dishonour is held against them"
                ),
            },
            about: None,
        });
    }
}
