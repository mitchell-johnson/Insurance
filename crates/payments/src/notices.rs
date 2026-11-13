//! Work for people. Every `Notify` the module raises is also kept here as
//! an open notice for its role until someone holding that role closes it
//! with what they did. The pump still writes each one to the structured
//! log, where alerting pages the role; this list is what the staff console
//! shows, so nothing rests on someone reading a log.

use std::collections::BTreeMap;

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

use crate::machine::{Event, PaymentsState, Tx};
use crate::model::{Reject, Role};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    pub to: Role,
    pub subject: String,
    pub detail: String,
    pub raised_at: NaiveDateTime,
    /// Who closed it, when, and what they did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed: Option<(String, NaiveDateTime, String)>,
}

/// Open notices, oldest first, and closed ones until they are forgotten.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notices {
    pub next: u64,
    pub items: BTreeMap<u64, Notice>,
}

/// How long a closed notice is kept to be looked up.
pub const KEEP_CLOSED_DAYS: i64 = 400;

impl PaymentsState {
    /// Keep a notice for its role. Called with every `Notify` the outbox takes.
    pub(crate) fn raise_notice(&mut self, to: Role, subject: &str, detail: &str) {
        let now = self.clock.unwrap_or_default();
        // Closed notices older than the keeping period are forgotten.
        self.notices
            .items
            .retain(|_, n| n.closed.as_ref().is_none_or(|(_, at, _)| (now - *at).num_days() <= KEEP_CLOSED_DAYS));
        self.notices.next += 1;
        self.notices.items.insert(
            self.notices.next,
            Notice {
                to,
                subject: subject.to_string(),
                detail: detail.to_string(),
                raised_at: now,
                closed: None,
            },
        );
    }

    /// Someone holding the notice's role says what they did about it.
    pub(crate) fn close_notice(&mut self, tx: &mut Tx<'_>, id: u64, action: String) -> Result<(), Reject> {
        let notice = self
            .notices
            .items
            .get_mut(&id)
            .ok_or_else(|| Reject::new("unknown_notice", format!("there is no notice {id}")))?;
        tx.need(notice.to)?;
        if action.trim().is_empty() {
            return Err(Reject::new("action_required", "a notice is closed with what was done about it"));
        }
        if notice.closed.is_some() {
            return Err(Reject::new("notice_closed", "the notice was closed already"));
        }
        notice.closed = Some((tx.actor.id.clone(), tx.now, action));
        tx.emit(Event::NoticeClosed { notice: id });
        Ok(())
    }

    /// Open notices for a role, oldest first.
    pub fn open_notices(&self, role: Role) -> Vec<(u64, &Notice)> {
        self.notices
            .items
            .iter()
            .filter(|(_, n)| n.to == role && n.closed.is_none())
            .map(|(id, n)| (*id, n))
            .collect()
    }
}
