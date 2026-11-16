//! Wellness: AIA Vitality membership, and the Premium Flex discount each life
//! holds on its eligible cover.
//!
//! The AIA Vitality programme tells this module who has joined, the status
//! each member has earned, the status they reached at each Vitality
//! anniversary, and who has left. The module works out each life's AIA
//! Vitality Discount under AIA's Premium Adjustment Rules (September 2023)
//! and tells the core with one `SetEntitlement` per change: entitlement
//! `aia-vitality` at the tier that is the discount in whole percent. The
//! products' discount chains turn the tier into the discount on the bill
//! (`by_tier` in each product file), so the core never sees the programme.
//!
//! The rules, as this module applies them:
//!
//! - **Initial Discount.** A member's eligible cover is discounted 10% from
//!   the day they join.
//! - **Premium Flex** at each policy anniversary after joining, decided two
//!   weeks after the anniversary and applied from the anniversary:
//!   - in the first 10 months of membership the discount does not fall: it
//!     rises if the member has reached Platinum and otherwise stays;
//!   - in months 11 and 12 the member's Earned Status flexes it;
//!   - after that, the status reached at the member's Vitality anniversary
//!     in the year to the decision day (the Anniversary Status) flexes the discount that applied at the
//!     previous policy anniversary (the Baseline Discount).
//! - **Flex by status:** Platinum +2 points up to 20%; Gold keeps it; Silver
//!   −1 and Bronze −2, never below 0%. No status reported counts as Bronze,
//!   the status every member starts and resets to.
//! - **Leaving** removes the discount from the next premium due date.
//!
//! Only the products whose discount chains name `aia-vitality` are eligible
//! ([`ELIGIBLE`]); Cover for Life and Essentials Life are not. The module is
//! hosted on the ledger nodes ([`module_spec`]), reads the core's events for
//! policies, premiums and business dates, and the host carries its outbox to
//! the core.

use std::collections::{BTreeMap, VecDeque};

use chrono::{Datelike, Months, NaiveDate};
use ic_core::entitlement::SetEntitlement;
use ic_core::event::Event as CoreEvent;
use ic_core::{LifeId, PolicyId, PolicyStatus};
use ic_log::StateMachine;
use ic_raft::module::{hosted_reading, CoreOutcome, CorePosting, ModuleSpec, ModuleState, Outbox, Route};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// The module's name on the ledger nodes.
pub const MODULE: &str = "wellness";
/// The caller the AIA Vitality programme's feed comes through.
pub const PROGRAMME: &str = "vitality";
/// The caller that carries the core's events in.
pub const CORE_EVENTS: &str = "core-events";
/// The caller that reports what the core made of a posting.
pub const CORE_OUTBOX: &str = "core-outbox";
/// The caller the core knows this module as.
pub const CORE_CALLER: &str = "wellness";
/// The entitlement the products' discount chains give the AIA Vitality Discount for.
pub const ENTITLEMENT: &str = "aia-vitality";
/// The products with cover eligible for the AIA Vitality Discount.
pub const ELIGIBLE: [&str; 5] = [
    "aia-living",
    "aia-living-business",
    "aia-start-up-income",
    "aia-private-health",
    "aia-cancer-care",
];
/// The Initial Discount, in percent.
pub const INITIAL_PERCENT: u32 = 10;
/// The most the discount can reach, in percent.
pub const MAX_PERCENT: u32 = 20;
/// The status that sets Premium Flex is read this many days after the policy anniversary.
pub const DECISION_DAYS: u64 = 14;
/// How many days a command's key is remembered, so a retry gets the first answer.
pub const KEY_DAYS: i64 = 35;
/// The most items one answer lists.
pub const MAX_ANSWER: usize = 500;

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

/// An AIA Vitality status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Bronze,
    Silver,
    Gold,
    Platinum,
}

/// The discount a status makes of a baseline, in percent (Premium Adjustment Rules, rule 4).
pub fn flex(baseline: u32, status: Status) -> u32 {
    match status {
        Status::Platinum => (baseline + 2).min(MAX_PERCENT),
        Status::Gold => baseline,
        Status::Silver => baseline.saturating_sub(1),
        Status::Bronze => baseline.saturating_sub(2),
    }
}

/// Whole months from `from` to `to`.
pub fn whole_months(from: NaiveDate, to: NaiveDate) -> u32 {
    let mut months = (to.year() - from.year()) * 12 + to.month() as i32 - from.month() as i32;
    if to.day() < from.day() {
        months -= 1;
    }
    u32::try_from(months).unwrap_or(0)
}

/// The `years`-th anniversary of a date; 29 February falls on 28 February.
pub fn anniversary(inception: NaiveDate, years: u32) -> NaiveDate {
    inception.checked_add_months(Months::new(12 * years)).unwrap_or(inception)
}

/// The status a member reached at one of their Vitality anniversaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnniversaryStatus {
    pub vitality_anniversary: NaiveDate,
    pub status: Status,
}

/// One Premium Flex decision, kept for good.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flexed {
    /// The policy anniversary it applies from.
    pub anniversary: NaiveDate,
    pub decided_on: NaiveDate,
    /// The status it was read from, and which one: `earned` or `anniversary`.
    pub status: Status,
    pub read: String,
    pub baseline: u32,
    pub discount: u32,
}

/// One life's membership.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub member: String,
    pub policy: PolicyId,
    pub life: LifeId,
    pub joined: NaiveDate,
    pub left: Option<NaiveDate>,
    /// The status earned so far in the current Vitality year.
    pub earned: Status,
    /// The status at each Vitality anniversary reported, oldest first.
    pub anniversaries: Vec<AnniversaryStatus>,
    /// The discount held now, in percent.
    pub discount: u32,
    pub flexed: Vec<Flexed>,
}

/// What the module knows of a policy from the core's events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    pub product: String,
    pub inception: NaiveDate,
    pub ended: bool,
    /// The latest periods billed, as (first day, the day after the last), oldest first, as the
    /// core's bills give them. A premium can be billed before it is due, so the next premium
    /// due is the first of these to start after a day, or the end of the last of them.
    pub billed: Vec<(NaiveDate, NaiveDate)>,
}

impl Policy {
    /// How many billed periods are remembered.
    const KEPT: usize = 4;

    /// The next premium due date after `day`, as far as the bills show it.
    pub fn next_due(&self, day: NaiveDate) -> Option<NaiveDate> {
        let next = self.billed.iter().map(|(start, _)| *start).find(|start| *start > day);
        next.or_else(|| self.billed.last().map(|(_, end)| *end).filter(|d| *d > day))
    }
}

/// A command for the core, waiting for the host to carry it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Posting {
    pub id: String,
    pub policy: PolicyId,
    pub command: ic_core::Command,
}

/// A posting the core refused, kept for operations to look at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refused {
    pub posting: Posting,
    pub code: String,
    pub message: String,
}

/// A fact from the core, as `react` reads it from the core's events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "fact", rename_all = "snake_case")]
pub enum CoreFact {
    Issued {
        policy: PolicyId,
        product: String,
        inception: NaiveDate,
    },
    Billed {
        policy: PolicyId,
        period_start: NaiveDate,
        period_end: NaiveDate,
    },
    Ended {
        policy: PolicyId,
    },
    DayOpened {
        today: NaiveDate,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    /// The programme accepted a life on a policy as a member.
    Join {
        member: String,
        policy: PolicyId,
        life: LifeId,
        joined: NaiveDate,
    },
    /// The programme reports a member's status: what they have earned this
    /// Vitality year, and the status they reached at a Vitality anniversary.
    ReportStatus {
        member: String,
        earned: Status,
        #[serde(default)]
        anniversary: Option<AnniversaryStatus>,
    },
    /// The member's membership ended.
    Leave { member: String, on: NaiveDate },
    /// A fact from the core.
    Core { fact: CoreFact },
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
            Command::Join { .. } => "join",
            Command::ReportStatus { .. } => "report_status",
            Command::Leave { .. } => "leave",
            Command::Core { .. } => "core",
            Command::RecordCoreOutcome { .. } => "record_core_outcome",
        }
    }

    /// The one caller that may send this command.
    pub fn caller(&self) -> &'static str {
        match self {
            Command::Join { .. } | Command::ReportStatus { .. } | Command::Leave { .. } => PROGRAMME,
            Command::Core { .. } => CORE_EVENTS,
            Command::RecordCoreOutcome { .. } => CORE_OUTBOX,
        }
    }
}

/// A command and who sent it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    /// Idempotency key: the same key is answered once.
    pub key: String,
    pub caller: String,
    /// The day the sender says it is. The module's day never goes back.
    pub today: NaiveDate,
    pub command: Command,
}

/// What a command did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Applied {
    pub index: u64,
    pub result: Result<(), Reject>,
    /// Postings for the core this command made.
    pub postings: Vec<String>,
    pub duplicate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Answered {
    index: u64,
    error: Option<Reject>,
}

/// The module's whole state. Every map is ordered, so the saved state is
/// the same on every member.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WellnessState {
    pub members: BTreeMap<String, Member>,
    pub policies: BTreeMap<PolicyId, Policy>,
    pub outbox: BTreeMap<String, Posting>,
    pub refused: Vec<Refused>,
    pub today: Option<NaiveDate>,
    pub next_id: u64,
    answered: BTreeMap<String, Answered>,
    keys: VecDeque<(NaiveDate, String)>,
}

impl WellnessState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state_hash(&self) -> String {
        let bytes = serde_json::to_vec(self).unwrap_or_default();
        hex::encode(Sha256::digest(bytes))
    }

    fn post(&mut self, policy: &PolicyId, command: SetEntitlement, made: &mut Vec<String>) {
        self.next_id += 1;
        let id = format!("w-{:010}", self.next_id);
        self.outbox.insert(
            id.clone(),
            Posting {
                id: id.clone(),
                policy: policy.clone(),
                command: ic_core::Command::SetEntitlement(Box::new(command)),
            },
        );
        made.push(id);
    }

    fn entitlement(member: &Member, tier: Option<u32>, from: NaiveDate, reference: String) -> SetEntitlement {
        SetEntitlement {
            policy: member.policy.clone(),
            code: ENTITLEMENT.to_string(),
            tier: tier.map(|t| t.to_string()),
            life: Some(member.life.clone()),
            held: tier.is_some(),
            from,
            until: None,
            reference,
        }
    }

    fn dispatch(&mut self, command: Command, today: NaiveDate, made: &mut Vec<String>) -> Result<(), Reject> {
        match command {
            Command::Join { member, policy, life, joined } => {
                let Some(p) = self.policies.get(&policy) else {
                    return Err(Reject::new("unknown_policy", format!("no policy {policy} has been issued")));
                };
                if !ELIGIBLE.contains(&p.product.as_str()) {
                    return Err(Reject::new(
                        "not_eligible",
                        format!("{} has no cover eligible for the AIA Vitality Discount", p.product),
                    ));
                }
                if p.ended {
                    return Err(Reject::new("policy_ended", format!("policy {policy} has ended")));
                }
                if joined < p.inception {
                    return Err(Reject::new("before_inception", "a member joins on or after the policy starts"));
                }
                if self.members.get(&member).is_some_and(|m| m.left.is_none()) {
                    return Err(Reject::new("already_member", format!("{member} is a member already")));
                }
                let record = Member {
                    member: member.clone(),
                    policy: policy.clone(),
                    life,
                    joined,
                    left: None,
                    earned: Status::Bronze,
                    anniversaries: vec![],
                    discount: INITIAL_PERCENT,
                    flexed: vec![],
                };
                let command = Self::entitlement(&record, Some(INITIAL_PERCENT), joined, format!("vitality:{member}:initial"));
                self.members.insert(member, record);
                self.post(&policy, command, made);
                Ok(())
            }
            Command::ReportStatus { member, earned, anniversary } => {
                let Some(m) = self.members.get_mut(&member).filter(|m| m.left.is_none()) else {
                    return Err(Reject::new("unknown_member", format!("{member} is not a member")));
                };
                m.earned = earned;
                if let Some(a) = anniversary {
                    if m.anniversaries.last().is_some_and(|last| last.vitality_anniversary > a.vitality_anniversary) {
                        return Err(Reject::new("out_of_order", "a Vitality anniversary is reported after a later one"));
                    }
                    m.anniversaries.retain(|x| x.vitality_anniversary != a.vitality_anniversary);
                    m.anniversaries.push(a);
                }
                Ok(())
            }
            Command::Leave { member, on } => {
                let Some(m) = self.members.get(&member).filter(|m| m.left.is_none()).cloned() else {
                    return Err(Reject::new("unknown_member", format!("{member} is not a member")));
                };
                // The discount comes off from the next premium due date.
                let next_due = self.policies.get(&m.policy).and_then(|p| p.next_due(on)).unwrap_or(on);
                let command = Self::entitlement(&m, None, next_due, format!("vitality:{member}:left"));
                if let Some(record) = self.members.get_mut(&member) {
                    record.left = Some(on);
                }
                self.post(&m.policy, command, made);
                Ok(())
            }
            Command::Core { fact } => {
                match fact {
                    CoreFact::Issued { policy, product, inception } => {
                        self.policies.entry(policy).or_insert(Policy {
                            product,
                            inception,
                            ended: false,
                            billed: vec![],
                        });
                    }
                    CoreFact::Billed {
                        policy,
                        period_start,
                        period_end,
                    } => {
                        if let Some(p) = self.policies.get_mut(&policy) {
                            if !p.billed.contains(&(period_start, period_end)) {
                                p.billed.push((period_start, period_end));
                                p.billed.sort();
                                let extra = p.billed.len().saturating_sub(Policy::KEPT);
                                p.billed.drain(..extra);
                            }
                        }
                    }
                    CoreFact::Ended { policy } => {
                        if let Some(p) = self.policies.get_mut(&policy) {
                            p.ended = true;
                        }
                    }
                    CoreFact::DayOpened { .. } => self.flex_due(today, made),
                }
                Ok(())
            }
            Command::RecordCoreOutcome {
                posting,
                applied,
                code,
                message,
            } => {
                let Some(item) = self.outbox.remove(&posting) else {
                    return Ok(());
                };
                if !applied {
                    self.refused.push(Refused {
                        posting: item,
                        code: code.unwrap_or_default(),
                        message: message.unwrap_or_default(),
                    });
                }
                Ok(())
            }
        }
    }

    /// Premium Flex for every member whose policy anniversary was two weeks ago or more and not
    /// yet flexed.
    fn flex_due(&mut self, today: NaiveDate, made: &mut Vec<String>) {
        let mut postings = vec![];
        for m in self.members.values_mut().filter(|m| m.left.is_none()) {
            let Some(p) = self.policies.get(&m.policy).filter(|p| !p.ended) else {
                continue;
            };
            // The latest anniversary whose decision day has come.
            let decided = today - chrono::Days::new(DECISION_DAYS);
            let years = whole_months(p.inception, decided) / 12;
            if years == 0 {
                continue;
            }
            let at = anniversary(p.inception, years);
            if at <= m.joined || m.flexed.last().is_some_and(|f| f.anniversary >= at) {
                continue;
            }
            let months = whole_months(m.joined, at);
            let baseline = m.discount;
            let (status, read, discount) = if months < 10 {
                // The first ten months: the discount only rises.
                let discount = if m.earned == Status::Platinum {
                    flex(baseline, Status::Platinum)
                } else {
                    baseline
                };
                (m.earned, "earned", discount)
            } else if months < 12 {
                (m.earned, "earned", flex(baseline, m.earned))
            } else {
                // The status reached at the Vitality anniversary in the year to the decision day.
                let year_before = decided.checked_sub_months(Months::new(12)).unwrap_or(decided);
                let status = m
                    .anniversaries
                    .iter()
                    .rev()
                    .find(|a| a.vitality_anniversary <= decided && a.vitality_anniversary > year_before)
                    .map_or(Status::Bronze, |a| a.status);
                (status, "anniversary", flex(baseline, status))
            };
            m.flexed.push(Flexed {
                anniversary: at,
                decided_on: today,
                status,
                read: read.to_string(),
                baseline,
                discount,
            });
            if discount != baseline {
                m.discount = discount;
                let reference = format!("vitality:{}:flex:{at}", m.member);
                postings.push((m.policy.clone(), Self::entitlement(m, Some(discount), at, reference)));
            }
        }
        for (policy, command) in postings {
            self.post(&policy, command, made);
        }
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

impl StateMachine for WellnessState {
    type Command = Envelope;
    type Output = Applied;

    fn apply(&mut self, index: u64, env: Envelope) -> Applied {
        if let Some(done) = self.answered.get(&env.key) {
            return Applied {
                index: done.index,
                result: done.error.clone().map_or(Ok(()), Err),
                postings: vec![],
                duplicate: true,
            };
        }
        let today = self.today.map_or(env.today, |d| d.max(env.today));
        let mut made = vec![];
        let result = if env.caller != env.command.caller() {
            Err(Reject::new("not_allowed", format!("{} may not send {}", env.caller, env.command.name())))
        } else {
            // Every handler checks before it changes anything, so a refusal leaves the state as it was.
            self.dispatch(env.command, today, &mut made)
        };
        if result.is_ok() {
            self.today = Some(today);
        }
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
            postings: made,
            duplicate: false,
        }
    }
}

/// The questions operations and the host ask.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "q", rename_all = "snake_case")]
pub enum Question {
    Member {
        member: String,
    },
    /// The members on a policy, joined or left: what staff look up when a
    /// policyholder asks about their discount, since they know the policy
    /// and not the programme's member number.
    Policy {
        policy: PolicyId,
    },
    Outbox {
        limit: usize,
    },
    Refused {
        limit: usize,
    },
    StateHash,
}

fn value<T: Serialize>(v: T) -> Result<Value, String> {
    serde_json::to_value(v).map_err(|e| e.to_string())
}

pub fn answer(state: &WellnessState, question: Value) -> Result<Value, String> {
    let question: Question = serde_json::from_value(question).map_err(|e| format!("not a question of this module: {e}"))?;
    match question {
        Question::Member { member } => value(state.members.get(&member)),
        Question::Policy { policy } => value(state.members.values().filter(|m| m.policy == policy).collect::<Vec<_>>()),
        Question::Outbox { limit } => value(state.outbox.values().take(limit.min(MAX_ANSWER)).collect::<Vec<_>>()),
        Question::Refused { limit } => value(state.refused.iter().rev().take(limit.min(MAX_ANSWER)).collect::<Vec<_>>()),
        Question::StateHash => value(state.state_hash()),
    }
}

/// The module's reading of one core event, if it is one the module follows.
pub fn fact(event: &CoreEvent) -> Option<CoreFact> {
    Some(match event {
        CoreEvent::PolicyIssued {
            policy,
            product_code,
            inception,
            ..
        } => CoreFact::Issued {
            policy: policy.clone(),
            product: product_code.clone(),
            inception: *inception,
        },
        CoreEvent::PremiumDue {
            policy,
            period_start,
            period_end,
            ..
        } => CoreFact::Billed {
            policy: policy.clone(),
            period_start: *period_start,
            period_end: *period_end,
        },
        CoreEvent::PolicyStatusChanged {
            policy,
            to:
                PolicyStatus::Surrendered
                | PolicyStatus::Matured
                | PolicyStatus::Expired
                | PolicyStatus::Cancelled
                | PolicyStatus::Terminated
                | PolicyStatus::Avoided,
            ..
        } => CoreFact::Ended { policy: policy.clone() },
        CoreEvent::BusinessDateAdvanced { to, .. } => CoreFact::DayOpened { today: *to },
        _ => return None,
    })
}

/// Turn a core event into the module's command. The key is the event's ID,
/// so an event offered twice is taken in once.
pub fn react(record: &ic_core::EventRecord) -> Vec<Envelope> {
    let Some(fact) = fact(&record.event) else {
        return Vec::new();
    };
    let today = match fact {
        CoreFact::DayOpened { today } => today,
        _ => record.business_date,
    };
    vec![Envelope {
        key: format!("core-{}", record.id),
        caller: CORE_EVENTS.to_string(),
        today,
        command: Command::Core { fact },
    }]
}

/// The core command for one outbox item, under the module's key for it so
/// a second offer is applied once.
pub fn core_envelope(posting: &Posting, salt: String) -> ic_core::Envelope {
    ic_core::Envelope {
        key: format!("{CORE_CALLER}:{}", posting.id),
        caller: CORE_CALLER.to_string(),
        salt,
        traceparent: None,
        command: posting.command.clone(),
    }
}

fn outbox_pending(limit: usize) -> Value {
    json!({ "q": "outbox", "limit": limit })
}

fn outbox_read(answer: Value) -> Result<Vec<CorePosting>, String> {
    let items: Vec<Posting> = serde_json::from_value(answer).map_err(|e| format!("not an outbox answer: {e}"))?;
    Ok(items
        .iter()
        .map(|p| CorePosting {
            id: p.id.clone(),
            route: Route::Policy(p.policy.clone()),
            envelope: core_envelope(p, ic_raft::client::fresh_salt()),
        })
        .collect())
}

/// The command that tells the module what the core made of a posting.
pub fn outcome_command(posting: &CorePosting, outcome: &CoreOutcome, today: NaiveDate) -> Envelope {
    let (applied, code, message) = match outcome {
        CoreOutcome::Applied { .. } => (true, None, None),
        CoreOutcome::Refused(reject) => (false, Some(reject.code.clone()), Some(reject.message.clone())),
        CoreOutcome::Unroutable(why) => (false, Some("unroutable".to_string()), Some(why.clone())),
    };
    Envelope {
        key: format!("outcome:{}", posting.id),
        caller: CORE_OUTBOX.to_string(),
        today,
        command: Command::RecordCoreOutcome {
            posting: posting.id.clone(),
            applied,
            code,
            message,
        },
    }
}

fn outbox_outcome(posting: &CorePosting, outcome: &CoreOutcome) -> Value {
    // The module's day never goes back, so the earliest date is harmless here.
    serde_json::to_value(outcome_command(posting, outcome, NaiveDate::MIN)).unwrap_or(Value::Null)
}

/// How the host carries the module's entitlements to the core.
pub const OUTBOX: Outbox = Outbox {
    pending: outbox_pending,
    read: outbox_read,
    outcome: outbox_outcome,
};

fn make() -> Box<dyn ModuleState> {
    hosted_reading(WellnessState::new(), answer, react)
}

/// The module as a node hosts it: listed in `ic_node::modules()`.
pub fn module_spec() -> Result<ModuleSpec, String> {
    Ok(ModuleSpec::new(MODULE, make)?.reading_events().posting_to_core(OUTBOX))
}
