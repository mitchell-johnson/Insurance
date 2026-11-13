//! A world for the collections tests: a state, a calendar and the people
//! who send commands.
#![allow(dead_code)]

use chrono::{NaiveDate, TimeZone, Utc};
use ic_collections::machine::{Applied, CollectionsState, Command, CoreFact, Envelope, Event, CORE_EVENTS, CORE_OUTBOX, DELIVERY, STAFF};
use ic_collections::{Channel, EarlyCall, Role, Sequence, Step, StepKind};
use ic_core::{PartyId, PolicyId, Standing};
use ic_log::StateMachine;

pub const PRODUCT: &str = "TERM1";

pub fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

pub fn step(day: u32, kind: StepKind, channel: Channel) -> Step {
    Step {
        day,
        kind,
        channel,
        template: format!("collections/{}", kind.name()),
    }
}

/// The sequence of the design notes: reminders on day 3 and 10, a call on
/// day 17, suspension on day 31, final notice on day 45, lapse on day 60.
pub fn sequence() -> Sequence {
    Sequence {
        product: PRODUCT.to_string(),
        lapse_day: 60,
        minimum_notice_days: 14,
        steps: vec![
            step(3, StepKind::Reminder, Channel::Email),
            step(10, StepKind::Reminder, Channel::Sms),
            step(17, StepKind::CallTask, Channel::Queue),
            step(31, StepKind::SuspensionNotice, Channel::Post),
            step(45, StepKind::FinalNotice, Channel::Post),
        ],
        early_call: Some(EarlyCall {
            annual_premium_minor: 500_000,
            day: 7,
        }),
        call_escalate_days: 3,
        max_arrangement_instalments: 6,
    }
}

pub struct World {
    pub state: CollectionsState,
    pub index: u64,
    pub keys: u64,
    pub today: NaiveDate,
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

impl World {
    /// A world with the test sequence set, on 1 March 2027.
    pub fn new() -> Self {
        let mut world = Self {
            state: CollectionsState::new(),
            index: 0,
            keys: 0,
            today: d(2027, 3, 1),
        };
        let applied = world.staff("config-1", &[Role::Configuration], Command::SetSequence { sequence: sequence() });
        assert_eq!(applied.result, Ok(()));
        world
    }

    pub fn send(&mut self, caller: &str, actor: &str, roles: &[Role], command: Command) -> Applied {
        self.keys += 1;
        let key = format!("k-{}", self.keys);
        self.send_keyed(&key, caller, actor, roles, command)
    }

    pub fn send_keyed(&mut self, key: &str, caller: &str, actor: &str, roles: &[Role], command: Command) -> Applied {
        self.index += 1;
        let at = Utc.from_utc_datetime(&self.today.and_hms_opt(0, 0, 0).unwrap());
        self.state.apply(
            self.index,
            Envelope {
                key: key.to_string(),
                caller: caller.to_string(),
                actor: actor.to_string(),
                roles: roles.to_vec(),
                at,
                command,
            },
        )
    }

    pub fn staff(&mut self, actor: &str, roles: &[Role], command: Command) -> Applied {
        self.send(STAFF, actor, roles, command)
    }

    pub fn officer(&mut self, command: Command) -> Applied {
        self.staff("officer-1", &[Role::Collections], command)
    }

    pub fn core(&mut self, partition: u16, fact: CoreFact) -> Applied {
        let on = self.today;
        self.send(CORE_EVENTS, CORE_EVENTS, &[], Command::Core { partition, on, fact })
    }

    pub fn delivery(&mut self, command: Command) -> Applied {
        self.send(DELIVERY, "documents", &[], command)
    }

    pub fn outbox(&mut self, command: Command) -> Applied {
        self.send(CORE_OUTBOX, CORE_OUTBOX, &[], command)
    }

    /// Issue a policy on partition 0 with a monthly premium.
    pub fn issue(&mut self, policy: &str, party: &str, monthly_minor: i64) {
        self.issue_on(0, policy, party, monthly_minor);
    }

    pub fn issue_on(&mut self, partition: u16, policy: &str, party: &str, monthly_minor: i64) {
        let applied = self.core(
            partition,
            CoreFact::Issued {
                policy: PolicyId::from(policy),
                number: format!("N-{policy}"),
                party: PartyId::from(party),
                product: PRODUCT.to_string(),
            },
        );
        assert_eq!(applied.result, Ok(()));
        self.core(
            partition,
            CoreFact::PremiumDue {
                policy: PolicyId::from(policy),
                period_start: d(2027, 1, 1),
                period_end: d(2027, 1, 31),
                total_minor: monthly_minor,
            },
        );
    }

    /// The core says a premium due on `due` is unpaid.
    pub fn overdue(&mut self, policy: &str, due: NaiveDate) {
        self.core(
            0,
            CoreFact::Standing {
                policy: PolicyId::from(policy),
                to: Some(Standing::InGrace),
                oldest_unpaid_due: Some(due),
            },
        );
    }

    pub fn paid(&mut self, policy: &str) {
        self.core(
            0,
            CoreFact::Standing {
                policy: PolicyId::from(policy),
                to: Some(Standing::Paid),
                oldest_unpaid_due: None,
            },
        );
    }

    /// Partition `partition` closes `self.today`, and the calendar moves on
    /// a day. Returns the events of the close.
    pub fn close_day_on(&mut self, partition: u16) -> Vec<Event> {
        let closed = self.today;
        let applied = self.core(partition, CoreFact::DayClosed { closed });
        assert_eq!(applied.result, Ok(()));
        self.today = closed.succ_opt().unwrap();
        applied.events.into_iter().map(|r| r.event).collect()
    }

    pub fn close_day(&mut self) -> Vec<Event> {
        self.close_day_on(0)
    }

    /// Close days until `day` has been closed, gathering the events.
    pub fn close_through(&mut self, day: NaiveDate) -> Vec<Event> {
        let mut events = Vec::new();
        while self.today <= day {
            events.extend(self.close_day());
        }
        events
    }
}

/// The steps reached in a list of events, as (policy, day, kind).
pub fn reached(events: &[Event]) -> Vec<(String, u32, StepKind)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::StepReached { policy, day, kind } => Some((policy.0.clone(), *day, *kind)),
            _ => None,
        })
        .collect()
}
