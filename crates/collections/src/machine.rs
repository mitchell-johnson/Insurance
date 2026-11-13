//! The module's commands, events and state, and the one place a command is
//! applied.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use chrono::{DateTime, NaiveDate, Utc};
use ic_core::{PartyId, PolicyId, PolicyStatus, Standing};
use ic_log::StateMachine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{Sequence, StepKind};
use crate::model::{HardshipCase, Notice, Posting, Reason, Relief, Role, Task, Tracked};

/// The caller that carries the core's events in.
pub const CORE_EVENTS: &str = "core-events";
/// The caller that reports what the core made of a posting.
pub const CORE_OUTBOX: &str = "core-outbox";
/// The caller that reports what became of a notice.
pub const DELIVERY: &str = "delivery";
/// The caller people's commands come through.
pub const STAFF: &str = "staff";

/// How many days a command's key is remembered, so a retry is answered with
/// the first answer.
pub const KEY_DAYS: i64 = 35;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reject {
    pub code: String,
    pub message: String,
}

impl Reject {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

/// A command and who sent it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    /// Idempotency key: the same key is answered once.
    pub key: String,
    pub caller: String,
    /// The person or service responsible.
    pub actor: String,
    #[serde(default)]
    pub roles: Vec<Role>,
    pub at: DateTime<Utc>,
    pub command: Command,
}

/// A fact from the core, as `react` reads it from the core's events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "fact", rename_all = "snake_case")]
pub enum CoreFact {
    Issued {
        policy: PolicyId,
        number: String,
        party: PartyId,
        product: String,
    },
    PremiumDue {
        policy: PolicyId,
        period_start: NaiveDate,
        period_end: NaiveDate,
        total_minor: i64,
    },
    Standing {
        policy: PolicyId,
        to: Option<Standing>,
        oldest_unpaid_due: Option<NaiveDate>,
    },
    /// The core's balance moved: the oldest unpaid due date, which a
    /// payment or a reversal can move without changing standing.
    Balance {
        policy: PolicyId,
        oldest_unpaid_due: Option<NaiveDate>,
    },
    Status {
        policy: PolicyId,
        to: PolicyStatus,
    },
    ArrangementMade {
        policy: PolicyId,
        arrangement: String,
        arrears_minor: i64,
        dues: Vec<NaiveDate>,
    },
    ArrangementCompleted {
        policy: PolicyId,
        arrangement: String,
    },
    ArrangementBroken {
        policy: PolicyId,
        arrangement: String,
        outstanding_minor: i64,
    },
    /// A partition closed a business day: its policies' steps for that day
    /// are due, now that its end-of-day run has settled their standing.
    DayClosed {
        closed: NaiveDate,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    /// A fact from one of the core's partitions, on its business date.
    Core {
        partition: u16,
        on: NaiveDate,
        fact: CoreFact,
    },
    /// Set or replace a product's dunning sequence.
    SetSequence {
        sequence: Sequence,
    },
    /// What the customer said when a task was worked.
    RecordOutcome {
        task: String,
        reason: Reason,
        note: String,
    },
    /// Agree an arrangement to clear the overdue premium by instalments.
    ProposeArrangement {
        policy: PolicyId,
        arrangement: String,
        instalments: Vec<ic_core::arrangement::Instalment>,
    },
    /// End an arrangement the customer cannot keep.
    EndArrangement {
        policy: PolicyId,
    },
    ApplyHardship {
        case: String,
        party: PartyId,
        policies: Vec<PolicyId>,
        relief: Relief,
        from: NaiveDate,
        to: NaiveDate,
        discloses_health: bool,
        note: String,
    },
    DecideHardship {
        case: String,
        approve: bool,
        note: String,
    },
    /// End relief before its date: normal billing resumes today.
    EndHardship {
        case: String,
    },
    /// The documents service took a notice.
    RecordNoticeSent {
        notice: String,
    },
    /// The documents service knows whether a notice arrived.
    RecordNoticeDelivery {
        notice: String,
        delivered: bool,
    },
    /// What the core made of a posting.
    RecordCoreOutcome {
        posting: String,
        applied: bool,
        #[serde(default)]
        code: Option<String>,
        #[serde(default)]
        message: Option<String>,
    },
}

impl Command {
    pub fn name(&self) -> &'static str {
        match self {
            Command::Core { .. } => "core",
            Command::SetSequence { .. } => "set_sequence",
            Command::RecordOutcome { .. } => "record_outcome",
            Command::ProposeArrangement { .. } => "propose_arrangement",
            Command::EndArrangement { .. } => "end_arrangement",
            Command::ApplyHardship { .. } => "apply_hardship",
            Command::DecideHardship { .. } => "decide_hardship",
            Command::EndHardship { .. } => "end_hardship",
            Command::RecordNoticeSent { .. } => "record_notice_sent",
            Command::RecordNoticeDelivery { .. } => "record_notice_delivery",
            Command::RecordCoreOutcome { .. } => "record_core_outcome",
        }
    }

    /// The caller a command must come through, and the roles that may send
    /// it (any of them; none listed means no role is needed).
    fn allowed(&self) -> (&'static str, &'static [Role]) {
        match self {
            Command::Core { .. } => (CORE_EVENTS, &[]),
            Command::RecordCoreOutcome { .. } => (CORE_OUTBOX, &[]),
            Command::RecordNoticeSent { .. } | Command::RecordNoticeDelivery { .. } => (DELIVERY, &[]),
            Command::SetSequence { .. } => (STAFF, &[Role::Configuration]),
            Command::RecordOutcome { .. } | Command::ProposeArrangement { .. } | Command::EndArrangement { .. } => {
                (STAFF, &[Role::Collections, Role::TeamLead])
            }
            Command::ApplyHardship { .. } => (STAFF, &[Role::Collections, Role::Hardship]),
            Command::DecideHardship { .. } | Command::EndHardship { .. } => (STAFF, &[Role::Hardship]),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    SequenceSet {
        product: String,
        hash: String,
    },
    /// A policy reached a step of the sequence.
    StepReached {
        policy: PolicyId,
        day: u32,
        kind: StepKind,
    },
    /// A step's day passed while the policy came in later than it, or while
    /// its reminders were held: it is not sent late.
    StepPassed {
        policy: PolicyId,
        day: u32,
        kind: StepKind,
    },
    /// The policy is out of arrears, or no longer in force: its sequence
    /// stops.
    SequenceStopped {
        policy: PolicyId,
        why: String,
    },
    NoticeRaised {
        notice: String,
        party: PartyId,
        kind: StepKind,
        policies: Vec<PolicyId>,
    },
    NoticeUndeliverable {
        notice: String,
        task: String,
    },
    TaskOpened {
        task: String,
        party: PartyId,
        kind: crate::model::TaskKind,
        policies: Vec<PolicyId>,
    },
    TaskEscalated {
        task: String,
    },
    OutcomeRecorded {
        task: String,
        reason: Reason,
    },
    ArrangementProposed {
        policy: PolicyId,
        arrangement: String,
        instalments: u32,
    },
    /// The core refused an arrangement or a relief: a task to agree another.
    PostingRefused {
        policy: PolicyId,
        posting: String,
        code: String,
        message: String,
    },
    ArrangementKept {
        policy: PolicyId,
        arrangement: String,
    },
    ArrangementCompleted {
        policy: PolicyId,
        arrangement: String,
    },
    ArrangementBroken {
        policy: PolicyId,
        arrangement: String,
        outstanding_minor: i64,
    },
    HardshipApplied {
        case: String,
        party: PartyId,
    },
    HardshipDecided {
        case: String,
        approved: bool,
    },
    HardshipEnded {
        case: String,
        on: NaiveDate,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRecord {
    pub seq: u64,
    pub log_index: u64,
    pub on: NaiveDate,
    pub event: Event,
}

/// What a command came to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Applied {
    pub index: u64,
    pub result: Result<(), Reject>,
    pub events: Vec<EventRecord>,
    pub duplicate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Answered {
    index: u64,
    error: Option<Reject>,
}

/// The module's whole state. Every map is ordered, so the saved state is the
/// same on every member.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionsState {
    pub sequences: BTreeMap<String, Sequence>,
    pub policies: BTreeMap<PolicyId, Tracked>,
    /// Policies with something overdue, so a closed day visits only them.
    pub in_arrears: BTreeSet<PolicyId>,
    pub notices: BTreeMap<String, Notice>,
    pub tasks: BTreeMap<String, Task>,
    /// Open tasks, oldest first by ID.
    pub open_tasks: BTreeSet<String>,
    pub hardship: BTreeMap<String, HardshipCase>,
    /// Postings to the core not yet answered.
    pub outbox: BTreeMap<String, Posting>,
    /// Products seen on a policy with no sequence set: nothing is sent for
    /// them until one is.
    pub unsequenced: BTreeSet<String>,
    pub today: Option<NaiveDate>,
    pub next_seq: u64,
    pub next_id: u64,
    answered: BTreeMap<String, Answered>,
    keys: VecDeque<(NaiveDate, String)>,
}

/// The day a command happens on: New Zealand's, and never earlier than the
/// day already reached.
fn nz_date(at: DateTime<Utc>) -> NaiveDate {
    at.with_timezone(&chrono_tz::Pacific::Auckland).date_naive()
}

/// The context one command is applied in.
pub struct Ctx {
    pub today: NaiveDate,
    pub actor: String,
    pub roles: Vec<Role>,
    pub events: Vec<Event>,
}

impl Ctx {
    pub fn emit(&mut self, event: Event) {
        self.events.push(event);
    }
}

impl CollectionsState {
    pub fn new() -> Self {
        Self::default()
    }

    /// A new ID for something this module made, in the order made.
    pub fn next(&mut self, prefix: &str) -> String {
        self.next_id += 1;
        format!("{prefix}-{}", self.next_id)
    }

    /// A hash of the whole state, for comparing members.
    pub fn state_hash(&self) -> String {
        let bytes = serde_json::to_vec(self).unwrap_or_default();
        hex::encode(Sha256::digest(bytes))
    }

    fn dispatch(&mut self, command: Command, ctx: &mut Ctx) -> Result<(), Reject> {
        match command {
            Command::Core { partition, on, fact } => {
                self.observe(partition, on, fact, ctx);
                Ok(())
            }
            Command::SetSequence { sequence } => self.set_sequence(sequence, ctx),
            Command::RecordOutcome { task, reason, note } => self.record_outcome(&task, reason, note, ctx),
            Command::ProposeArrangement {
                policy,
                arrangement,
                instalments,
            } => self.propose_arrangement(policy, arrangement, instalments, ctx),
            Command::EndArrangement { policy } => self.end_arrangement(policy, ctx),
            Command::ApplyHardship {
                case,
                party,
                policies,
                relief,
                from,
                to,
                discloses_health,
                note,
            } => self.apply_hardship(
                HardshipCase {
                    id: case,
                    party,
                    policies,
                    relief,
                    from,
                    to,
                    applied_by: ctx.actor.clone(),
                    applied_on: ctx.today,
                    discloses_health,
                    note,
                    decision: None,
                    ended_early: None,
                },
                ctx,
            ),
            Command::DecideHardship { case, approve, note } => self.decide_hardship(&case, approve, note, ctx),
            Command::EndHardship { case } => self.end_hardship(&case, ctx),
            Command::RecordNoticeSent { notice } => self.notice_sent(&notice),
            Command::RecordNoticeDelivery { notice, delivered } => self.notice_delivery(&notice, delivered, ctx),
            Command::RecordCoreOutcome {
                posting,
                applied,
                code,
                message,
            } => self.core_outcome(&posting, applied, code, message, ctx),
        }
    }

    fn set_sequence(&mut self, sequence: Sequence, ctx: &mut Ctx) -> Result<(), Reject> {
        sequence.validate()?;
        let hash = hex::encode(Sha256::digest(serde_json::to_vec(&sequence).unwrap_or_default()));
        self.unsequenced.remove(&sequence.product);
        ctx.emit(Event::SequenceSet {
            product: sequence.product.clone(),
            hash,
        });
        self.sequences.insert(sequence.product.clone(), sequence);
        Ok(())
    }

    fn forget_old_keys(&mut self, today: NaiveDate) {
        while let Some((on, key)) = self.keys.front().cloned() {
            if (today - on).num_days() <= KEY_DAYS {
                break;
            }
            self.keys.pop_front();
            self.answered.remove(&key);
        }
    }
}

impl StateMachine for CollectionsState {
    type Command = Envelope;
    type Output = Applied;

    fn apply(&mut self, index: u64, env: Envelope) -> Applied {
        if let Some(done) = self.answered.get(&env.key) {
            return Applied {
                index: done.index,
                result: done.error.clone().map_or(Ok(()), Err),
                events: vec![],
                duplicate: true,
            };
        }
        let today = self.today.map_or(nz_date(env.at), |d| d.max(nz_date(env.at)));
        let mut ctx = Ctx {
            today,
            actor: env.actor.clone(),
            roles: env.roles.clone(),
            events: vec![],
        };
        let (caller, roles) = env.command.allowed();
        let result = if env.caller != caller {
            Err(Reject::new("not_allowed", format!("{} may not send {}", env.caller, env.command.name())))
        } else if !roles.is_empty() && !env.roles.iter().any(|r| roles.contains(r)) {
            Err(Reject::new("not_allowed", format!("this role may not send {}", env.command.name())))
        } else if env.actor.trim().is_empty() {
            Err(Reject::new("no_actor", "a command names who is responsible for it"))
        } else {
            // Every handler checks before it changes anything, so a refusal
            // leaves the state as it was.
            self.dispatch(env.command, &mut ctx)
        };
        let events = if result.is_ok() {
            self.today = Some(ctx.today);
            ctx.events
                .into_iter()
                .map(|event| {
                    self.next_seq += 1;
                    EventRecord {
                        seq: self.next_seq,
                        log_index: index,
                        on: ctx.today,
                        event,
                    }
                })
                .collect()
        } else {
            vec![]
        };
        self.forget_old_keys(today);
        self.keys.push_back((today, env.key.clone()));
        self.answered.insert(
            env.key,
            Answered {
                index,
                error: result.clone().err(),
            },
        );
        Applied {
            index,
            result,
            events,
            duplicate: false,
        }
    }
}
