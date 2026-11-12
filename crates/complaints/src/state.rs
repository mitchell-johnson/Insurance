//! The complaints state machine.
//!
//! Deterministic: state changes only by applying a logged command, and
//! time moves only as commands carry it. Every handler validates before it
//! writes, so a refused command leaves nothing but its stored result.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use chrono::{DateTime, Duration, NaiveDate, Utc};
use ic_core::{PartyId, Reject};
use ic_log::StateMachine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::command::*;
use crate::model::*;

/// The most one command attaches, and the most one tick examines.
pub const MAX_CHUNK: usize = 500;

/// How long an idempotency key is remembered. A repeat of a command older
/// than this is judged on its merits, like a new one.
pub const KEEP_KEYS_DAYS: i64 = 90;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredResult {
    index: u64,
    error: Option<Reject>,
}

/// What applying one command produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Applied {
    pub index: u64,
    pub result: Result<(), Reject>,
    pub events: Vec<EventRecord>,
    /// The key had been seen before: this is the original result.
    pub duplicate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ComplaintsState {
    timers: BTreeMap<u32, Timers>,
    pub timers_version: u32,
    pub complaints: BTreeMap<String, Complaint>,
    pub conduct: BTreeMap<String, ConductCase>,
    /// Every complaint a party has made that is still on record.
    pub(crate) by_complainant: BTreeMap<PartyId, BTreeSet<String>>,
    /// Complaints not yet closed.
    pub(crate) open: BTreeSet<String>,
    pub(crate) open_conduct: BTreeSet<String>,
    /// Closed complaints by the date their record may be removed.
    removal: BTreeMap<NaiveDate, BTreeSet<String>>,
    /// Records removed at the end of their retention.
    pub removed: u64,
    pub reviews: BTreeMap<String, RootCauseReview>,
    unpublished: VecDeque<EventRecord>,
    next_seq: u64,
    pub now: Option<DateTime<Utc>>,
    /// Where the last tick stopped, so a long register is covered in turn.
    tick_cursor: Option<String>,
    results: BTreeMap<String, StoredResult>,
    /// The remembered keys, oldest first, with the day each was stored, so
    /// that the old ones can be forgotten.
    #[serde(default)]
    key_log: VecDeque<(NaiveDate, String)>,
}

/// The local date in New Zealand, which every timer is counted in.
pub fn nz_date(at: DateTime<Utc>) -> NaiveDate {
    at.with_timezone(&chrono_tz::Pacific::Auckland).date_naive()
}

pub(crate) fn reject<T>(code: &str, message: impl Into<String>) -> Result<T, Reject> {
    Err(Reject::new(code, message))
}

pub(crate) fn days(n: u32) -> Duration {
    Duration::days(n as i64)
}

pub(crate) struct Ctx {
    pub today: NaiveDate,
    pub actor: String,
    pub caller: String,
    pub events: Vec<Event>,
}

/// Where a timer stands on a date: nothing yet, or the level it has reached.
pub(crate) fn level(timers: &Timers, due: NaiveDate, today: NaiveDate) -> Option<Level> {
    if today > due + days(timers.conduct_officer_after_days) {
        Some(Level::ConductOfficer)
    } else if today > due {
        Some(Level::Lead)
    } else if today + days(timers.warning_days) >= due {
        Some(Level::Warning)
    } else {
        None
    }
}

impl StateMachine for ComplaintsState {
    type Command = Envelope;
    type Output = Applied;

    fn apply(&mut self, index: u64, env: Envelope) -> Applied {
        if let Some(stored) = self.results.get(&env.key) {
            return Applied {
                index: stored.index,
                result: stored.error.clone().map_or(Ok(()), Err),
                events: vec![],
                duplicate: true,
            };
        }
        // Time never runs backwards, whatever a proposer's clock says.
        let at = self.now.map_or(env.at, |now| now.max(env.at));
        let mut ctx = Ctx {
            today: nz_date(at),
            actor: env.actor.clone(),
            caller: env.caller.clone(),
            events: vec![],
        };
        // Letting time pass or acknowledging the stream twice changes
        // nothing the second time, and both are sent every few seconds for
        // ever: their keys are not kept.
        let remember = !matches!(env.command, Command::Tick | Command::MarkPublished { .. });
        let is_tick = matches!(env.command, Command::Tick);
        let today = ctx.today;
        let result = if !env.command.allowed_callers().contains(&env.caller.as_str()) {
            reject("not_allowed", format!("{} may not send {}", env.caller, env.command.name()))
        } else if env.actor.is_empty() {
            reject("no_actor", "a command names who is responsible for it")
        } else {
            self.dispatch(env.command, &mut ctx)
        };
        let events: Vec<EventRecord> = if result.is_ok() {
            self.now = Some(at);
            ctx.events
                .into_iter()
                .map(|event| {
                    self.next_seq += 1;
                    EventRecord {
                        id: format!("cmp-{}", self.next_seq),
                        seq: self.next_seq,
                        at,
                        log_index: index,
                        event,
                    }
                })
                .collect()
        } else {
            vec![]
        };
        self.unpublished.extend(events.iter().cloned());
        if remember {
            self.key_log.push_back((today, env.key.clone()));
            self.results.insert(
                env.key,
                StoredResult {
                    index,
                    error: result.clone().err(),
                },
            );
        } else if is_tick {
            self.forget_old_keys(today);
        }
        Applied {
            index,
            result,
            events,
            duplicate: false,
        }
    }
}

impl ComplaintsState {
    pub fn new() -> Self {
        Self::default()
    }

    /// A key guards against a command sent twice, which happens within
    /// minutes. Keys older than [`KEEP_KEYS_DAYS`] are forgotten as time
    /// passes, a bounded number at a time, so the state does not grow for ever.
    fn forget_old_keys(&mut self, today: NaiveDate) {
        for _ in 0..MAX_CHUNK {
            match self.key_log.front() {
                Some((stored, _)) if *stored + Duration::days(KEEP_KEYS_DAYS) < today => {}
                _ => break,
            }
            if let Some((_, key)) = self.key_log.pop_front() {
                self.results.remove(&key);
            }
        }
    }

    /// How many idempotency keys are remembered.
    pub fn remembered_keys(&self) -> usize {
        self.results.len()
    }

    fn dispatch(&mut self, command: Command, ctx: &mut Ctx) -> Result<(), Reject> {
        match command {
            Command::ActivateTimers(t) => self.activate_timers(t, ctx),
            Command::RecordComplaint(intake) => self.record(*intake, ctx),
            Command::RefineCategory { complaint, category } => {
                self.check_cause(category.cause.as_deref())?;
                self.open_mut(&complaint)?.category = category;
                Ok(())
            }
            Command::AddRepresentative { complaint, representative } => self.add_representative(&complaint, representative),
            Command::SetVulnerability {
                complaint,
                vulnerable,
                adjustments,
            } => {
                let c = self.open_mut(&complaint)?;
                c.vulnerable = vulnerable;
                c.adjustments = adjustments;
                Ok(())
            }
            Command::Assign {
                complaint,
                handler,
                authority,
                trained_for_vulnerability,
            } => self.assign(&complaint, handler, authority, trained_for_vulnerability, ctx),
            Command::RecordAcknowledgement { complaint, document } => self.acknowledge(&complaint, document, ctx),
            Command::RecordUpdate { complaint, document } => self.update(&complaint, document, ctx),
            Command::AskComplainant { complaint } => self.ask(&complaint, ctx),
            Command::RecordComplainantResponse { complaint } => {
                let c = self.open_mut(&complaint)?;
                if c.awaiting.take().is_none() {
                    return reject("not_awaiting", "nothing was asked of the complainant");
                }
                c.escalated.remove(&Timer::ComplainantResponse);
                Ok(())
            }
            Command::GrantAccess {
                complaint,
                to,
                classes,
                purpose,
            } => self.grant(&complaint, to, classes, purpose, ctx),
            Command::RevokeAccess { complaint, from } => {
                if self.open_mut(&complaint)?.grants.remove(&from).is_none() {
                    return reject("no_grant", "that person holds no access to this complaint");
                }
                ctx.events.push(Event::AccessRevoked { complaint, from });
                Ok(())
            }
            Command::Decide {
                complaint,
                outcome,
                reason_codes,
                document,
            } => self.decide(&complaint, outcome, reason_codes, document, ctx),
            Command::RecordOutcomeDelivered { complaint, on } => self.outcome_delivered(&complaint, on, ctx),
            Command::RecordRemediation {
                complaint,
                kind,
                reference,
                amount_minor,
                authorised_by,
                authority,
            } => self.remediate(&complaint, kind, reference, amount_minor, authorised_by, authority, ctx),
            Command::CompleteRemediation { complaint, reference } => {
                let today = ctx.today;
                let c = self.open_mut(&complaint)?;
                match c.remediation.iter_mut().find(|r| r.reference == reference && r.completed_on.is_none()) {
                    Some(r) => r.completed_on = Some(today),
                    None => return reject("no_remediation", "no remediation with that reference is outstanding"),
                }
                Ok(())
            }
            Command::RecordSystemicFinding { complaint, remediation_case } => {
                if remediation_case.is_empty() {
                    return reject("invalid", "a systemic finding names the remediation case opened for it");
                }
                let c = self.open_mut(&complaint)?;
                if c.systemic_case.is_some() {
                    return reject("already_recorded", "a systemic finding is already recorded");
                }
                c.systemic_case = Some(remediation_case.clone());
                ctx.events.push(Event::SystemicFindingRaised { complaint, remediation_case });
                Ok(())
            }
            Command::DeclareDeadlock { complaint } => self.deadlock(&complaint, false, ctx),
            Command::RecordDeadlockNotice { complaint, document } => {
                let c = self.open_mut(&complaint)?;
                match c.deadlock.as_mut().filter(|d| d.notice.is_none()) {
                    Some(d) if !document.is_empty() => d.notice = Some(document),
                    _ => return reject("no_deadlock", "no deadlock is waiting for its notice, or no document was named"),
                }
                ctx.events.push(Event::DeadlockNoticeSent { complaint });
                Ok(())
            }
            Command::Refer {
                complaint,
                forum,
                reference,
                response_due,
                documents_due,
            } => self.refer(&complaint, forum, reference, response_due, documents_due, ctx),
            Command::RecordForumResponse { complaint } => {
                let today = ctx.today;
                match self.referral_mut(&complaint)? {
                    r if r.responded_on.is_none() => r.responded_on = Some(today),
                    _ => return reject("already_recorded", "the response is already recorded"),
                }
                Ok(())
            }
            Command::ReleaseBundle {
                complaint,
                bundle,
                documents,
                reviewed_by,
            } => self.release_bundle(&complaint, bundle, documents, reviewed_by, ctx),
            Command::RecordForumDecision {
                complaint,
                outcome,
                binding,
                remediation_due,
            } => self.forum_decision(&complaint, outcome, binding, remediation_due, ctx),
            Command::CompleteForumRemediation { complaint } => self.complete_forum_remediation(&complaint, ctx),
            Command::Close { complaint, detail } => self.close(&complaint, detail, ctx),
            Command::RecordRootCauseReview { month, actions } => self.root_cause_review(month, actions, ctx),
            Command::Tick => {
                self.tick(ctx);
                Ok(())
            }
            Command::MarkPublished { through_seq } => {
                if through_seq > self.next_seq {
                    return reject("invalid", "no such event has been produced");
                }
                while self.unpublished.front().is_some_and(|e| e.seq <= through_seq) {
                    self.unpublished.pop_front();
                }
                Ok(())
            }
            conduct => self.dispatch_conduct(conduct, ctx),
        }
    }

    // --- configuration ---

    fn activate_timers(&mut self, t: Timers, ctx: &mut Ctx) -> Result<(), Reject> {
        if t.version != self.timers_version + 1 {
            return reject("version", format!("the next timer version is {}", self.timers_version + 1));
        }
        let periods = [
            t.acknowledge_days,
            t.update_every_days,
            t.decide_days,
            t.deadlock_days,
            t.complainant_response_days,
            t.retain_closed_days,
        ];
        if periods.contains(&0) || t.regulator_notification_days == 0 {
            return reject("invalid", "every period is at least one day");
        }
        if t.acknowledge_days > t.decide_days || t.decide_days > t.deadlock_days {
            return reject("invalid", "acknowledgement comes before the decision, and the decision before deadlock");
        }
        if t.front_line_limit_minor < 0 || t.ex_gratia_limits_minor.values().any(|v| *v < 0) {
            return reject("invalid", "a limit cannot be negative");
        }
        if t.cause_codes.is_empty() {
            return reject("invalid", "the cause codes are a controlled list and cannot be empty");
        }
        self.timers_version = t.version;
        ctx.events.push(Event::TimersActivated { version: t.version });
        self.timers.insert(t.version, t);
        Ok(())
    }

    /// The timers a complaint or case runs under.
    pub fn timers(&self, version: u32) -> Option<&Timers> {
        self.timers.get(&version)
    }

    fn current(&self) -> Result<&Timers, Reject> {
        match self.timers.get(&self.timers_version) {
            Some(t) => Ok(t),
            None => reject("no_timers", "no timer configuration is active"),
        }
    }

    fn check_cause(&self, cause: Option<&str>) -> Result<(), Reject> {
        match cause {
            Some(code) if !self.current()?.cause_codes.contains(code) => reject("unknown_cause", format!("`{code}` is not a cause code")),
            _ => Ok(()),
        }
    }

    // --- lookups ---

    pub(crate) fn open_mut(&mut self, id: &str) -> Result<&mut Complaint, Reject> {
        match self.complaints.get_mut(id) {
            None => reject("unknown_complaint", format!("there is no complaint {id}")),
            Some(c) if c.status == Status::Closed => reject("closed", "the complaint is closed"),
            Some(c) => Ok(c),
        }
    }

    fn referral_mut(&mut self, id: &str) -> Result<&mut Referral, Reject> {
        match self.open_mut(id)?.referral.as_mut() {
            Some(r) => Ok(r),
            None => reject("not_referred", "the complaint has not been referred"),
        }
    }

    /// The handler's own commands: the actor must be the complaint's owner.
    fn owned_mut(&mut self, id: &str, actor: &str) -> Result<&mut Complaint, Reject> {
        let c = self.open_mut(id)?;
        if c.owner.as_deref() != Some(actor) {
            return reject("not_owner", "only the person the complaint is assigned to may do this");
        }
        Ok(c)
    }

    // --- intake ---

    fn record(&mut self, intake: Intake, ctx: &mut Ctx) -> Result<(), Reject> {
        let timers = self.current()?.clone();
        if intake.complaint.is_empty() || intake.summary_document.is_empty() {
            return reject("invalid", "a complaint needs an id and the summary in the complainant's words");
        }
        if self.complaints.contains_key(&intake.complaint) {
            return reject("exists", "a complaint with that id is already recorded");
        }
        self.check_cause(intake.category.cause.as_deref())?;
        let migrating = ctx.caller == "migration";
        if !migrating && (intake.received_on.is_some() || intake.acknowledged_on.is_some()) {
            return reject("invalid", "only a migration carries earlier dates");
        }
        let received_on = intake.received_on.unwrap_or(ctx.today);
        if received_on > ctx.today || intake.acknowledged_on.is_some_and(|a| a < received_on || a > ctx.today) {
            return reject("invalid", "the dates are not in order");
        }
        let mut complaint = Complaint {
            id: intake.complaint.clone(),
            received_on,
            recorded_by: ctx.actor.clone(),
            channel: intake.channel,
            complainant: intake.complainant.clone(),
            complainant_kind: intake.complainant_kind,
            representatives: vec![],
            links: intake.links,
            category: intake.category,
            summary_document: intake.summary_document,
            vulnerable: intake.vulnerable,
            adjustments: intake.adjustments,
            timers_version: timers.version,
            owner: None,
            owner_authority: 0,
            status: Status::Open,
            acknowledged_on: intake.acknowledged_on,
            acknowledgement: None,
            updates: vec![],
            awaiting: None,
            outcome: None,
            remediation: vec![],
            systemic_case: None,
            deadlock: None,
            referral: None,
            closure: None,
            escalated: BTreeMap::new(),
            grants: BTreeMap::new(),
            migrated: migrating,
        };
        let mut closing = None;
        if let Some(front) = intake.resolved_on_the_spot {
            // Front-line resolution is for service matters, by someone who
            // is not the subject of the complaint, within a small authority.
            if complaint.category.process != Process::Service || complaint.concerns_clinical_decision() {
                return reject("not_front_line", "only a service matter may be resolved on the spot");
            }
            if complaint.involved().contains(ctx.actor.as_str()) {
                return reject("separation", "the person complained about cannot resolve the complaint");
            }
            if front.reason_codes.is_empty() || !timers.cause_codes.contains(&front.cause) {
                return reject("invalid", "an outcome needs reason codes and a cause code from the list");
            }
            if front.outcome == OutcomeKind::Withdrawn {
                return reject("invalid", "a complaint resolved on the spot is not a withdrawal");
            }
            if let Some((reference, amount)) = &front.put_right {
                if reference.is_empty() || *amount <= 0 || *amount > timers.front_line_limit_minor {
                    return reject("over_authority", "that is more than may be spent on the spot, or it names no command");
                }
                complaint.remediation.push(Remediation {
                    kind: RemediationKind::ExGratia,
                    reference: reference.clone(),
                    amount_minor: *amount,
                    authorised_by: ctx.actor.clone(),
                    recorded_on: ctx.today,
                    completed_on: Some(ctx.today),
                });
            }
            complaint.acknowledged_on = Some(ctx.today);
            complaint.outcome = Some(Outcome {
                kind: front.outcome,
                reason_codes: front.reason_codes,
                document: None,
                decided_on: ctx.today,
                decided_by: ctx.actor.clone(),
                delivered_on: None,
                front_line: true,
            });
            closing = Some(front.cause);
        }
        ctx.events.push(Event::ComplaintRecorded {
            complaint: complaint.id.clone(),
            complainant: complaint.complainant.clone(),
            process: complaint.category.process,
            vulnerable: complaint.vulnerable,
        });
        if complaint.acknowledged_on.is_none() {
            ctx.events.push(Event::AcknowledgementDue {
                complaint: complaint.id.clone(),
                by: received_on + days(timers.acknowledge_days),
            });
        }
        self.by_complainant
            .entry(complaint.complainant.clone())
            .or_default()
            .insert(complaint.id.clone());
        self.open.insert(complaint.id.clone());
        let id = complaint.id.clone();
        self.complaints.insert(id.clone(), complaint);
        if let Some(cause) = closing {
            self.finish(&id, cause, None, None, None, ctx);
        }
        Ok(())
    }

    fn add_representative(&mut self, id: &str, representative: Representative) -> Result<(), Reject> {
        if representative.authority_document.is_empty() || representative.relationship.is_empty() {
            return reject("invalid", "a representative is recorded with the complainant's authority");
        }
        let c = self.open_mut(id)?;
        if c.representatives.iter().any(|r| r.party == representative.party) {
            return reject("exists", "that representative is already recorded");
        }
        c.representatives.push(representative);
        Ok(())
    }

    // --- handling ---

    fn assign(&mut self, id: &str, handler: String, authority: u8, trained: bool, ctx: &mut Ctx) -> Result<(), Reject> {
        let today = ctx.today;
        let actor = ctx.actor.clone();
        let c = self.open_mut(id)?;
        if handler.is_empty() {
            return reject("invalid", "a complaint is assigned to a person");
        }
        if c.involved().contains(handler.as_str()) {
            return reject("separation", "the person who made the decision complained about never handles the complaint");
        }
        if authority < c.authority_needed() {
            return reject("authority", "the reviewer's authority is lower than the decision under review");
        }
        if c.vulnerable && !trained {
            return reject("not_trained", "a complaint with a vulnerability marker goes to trained staff");
        }
        // Access goes with the case: the last handler's ends here.
        if let Some(previous) = c.owner.take() {
            c.grants.remove(&previous);
        }
        c.grants.insert(
            handler.clone(),
            Grant {
                classes: BTreeSet::from([DataClass::Personal, DataClass::Financial, DataClass::InsuranceSensitive]),
                purpose: "handling the complaint".to_string(),
                granted_by: actor,
                on: today,
            },
        );
        c.owner = Some(handler.clone());
        c.owner_authority = authority;
        ctx.events.push(Event::ComplaintAssigned {
            complaint: id.to_string(),
            handler,
        });
        Ok(())
    }

    fn acknowledge(&mut self, id: &str, document: String, ctx: &mut Ctx) -> Result<(), Reject> {
        let today = ctx.today;
        let c = self.open_mut(id)?;
        if c.acknowledged_on.is_some() || document.is_empty() {
            return reject("already_recorded", "the complaint is already acknowledged, or no document was named");
        }
        c.acknowledged_on = Some(today);
        c.acknowledgement = Some(document);
        // The first update is counted from the acknowledgement.
        c.escalated.remove(&Timer::Update);
        ctx.events.push(Event::Acknowledged { complaint: id.to_string() });
        Ok(())
    }

    fn update(&mut self, id: &str, document: String, ctx: &mut Ctx) -> Result<(), Reject> {
        let today = ctx.today;
        let c = self.open_mut(id)?;
        if document.is_empty() {
            return reject("invalid", "an update is a document sent to the complainant");
        }
        c.updates.push(today);
        // The next update's timer starts fresh.
        c.escalated.remove(&Timer::Update);
        ctx.events.push(Event::ComplainantUpdated { complaint: id.to_string() });
        Ok(())
    }

    fn ask(&mut self, id: &str, ctx: &mut Ctx) -> Result<(), Reject> {
        let version = self.open_mut(id)?.timers_version;
        let timers = self.timers[&version].clone();
        let today = ctx.today;
        let c = self.open_mut(id)?;
        if c.awaiting.is_some() {
            return reject("already_awaiting", "the complainant has already been asked");
        }
        let extra = if c.vulnerable { timers.vulnerable_extra_days } else { 0 };
        c.awaiting = Some(Awaiting {
            asked_on: today,
            due: today + days(timers.complainant_response_days + extra),
        });
        Ok(())
    }

    fn grant(&mut self, id: &str, to: String, classes: BTreeSet<DataClass>, purpose: String, ctx: &mut Ctx) -> Result<(), Reject> {
        let today = ctx.today;
        let actor = ctx.actor.clone();
        let c = self.open_mut(id)?;
        if to.is_empty() || classes.is_empty() || purpose.is_empty() {
            return reject("invalid", "access is granted to a person, for named classes and a stated purpose");
        }
        if to == actor {
            return reject("separation", "nobody grants themselves access");
        }
        if c.involved().contains(to.as_str()) {
            return reject("separation", "the person complained about is not given the complaint file");
        }
        let clinical = classes.contains(&DataClass::Clinical);
        // Clinical information is seen only through the review task: by the
        // assigned reviewer, on a complaint about a claims or underwriting decision.
        if clinical && (!c.concerns_clinical_decision() || c.owner.as_deref() != Some(to.as_str())) {
            return reject(
                "clinical",
                "clinical access is only for the assigned reviewer of a claims or underwriting decision",
            );
        }
        c.grants.insert(
            to.clone(),
            Grant {
                classes,
                purpose: purpose.clone(),
                granted_by: actor,
                on: today,
            },
        );
        ctx.events.push(Event::AccessGranted {
            complaint: id.to_string(),
            to,
            clinical,
            purpose,
        });
        Ok(())
    }

    // --- outcome ---

    fn decide(&mut self, id: &str, outcome: OutcomeKind, reason_codes: Vec<String>, document: Option<String>, ctx: &mut Ctx) -> Result<(), Reject> {
        let today = ctx.today;
        let actor = ctx.actor.clone();
        let c = self.owned_mut(id, &actor)?;
        if !matches!(c.status, Status::Open | Status::Deadlocked) {
            return reject("already_decided", "the complaint already has its outcome or is with an external forum");
        }
        if reason_codes.is_empty() || reason_codes.iter().any(String::is_empty) {
            return reject("invalid", "an outcome carries reason codes");
        }
        let document = document.filter(|d| !d.is_empty());
        if document.is_none() && outcome != OutcomeKind::Withdrawn {
            return reject("invalid", "an outcome is written, with the reasons and the external scheme's details");
        }
        let overturned = if outcome.overturns() {
            c.links.decisions.iter().map(|d| d.decision.clone()).collect()
        } else {
            vec![]
        };
        c.outcome = Some(Outcome {
            kind: outcome,
            reason_codes,
            document,
            decided_on: today,
            decided_by: actor,
            delivered_on: None,
            front_line: false,
        });
        c.status = Status::Decided;
        ctx.events.push(Event::Decided {
            complaint: id.to_string(),
            outcome,
            overturned,
        });
        Ok(())
    }

    fn outcome_delivered(&mut self, id: &str, on: NaiveDate, ctx: &mut Ctx) -> Result<(), Reject> {
        let today = ctx.today;
        let c = self.open_mut(id)?;
        match c.outcome.as_mut().filter(|o| o.document.is_some() && o.delivered_on.is_none()) {
            Some(o) if on >= o.decided_on && on <= today => o.delivered_on = Some(on),
            Some(_) => return reject("invalid", "the delivery date is before the outcome or in the future"),
            None => return reject("no_outcome", "no written outcome is waiting for proof of delivery"),
        }
        ctx.events.push(Event::OutcomeDelivered { complaint: id.to_string(), on });
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn remediate(
        &mut self,
        id: &str,
        kind: RemediationKind,
        reference: String,
        amount: i64,
        authorised_by: String,
        authority: u8,
        ctx: &mut Ctx,
    ) -> Result<(), Reject> {
        let version = self.open_mut(id)?.timers_version;
        let timers = self.timers[&version].clone();
        let today = ctx.today;
        let c = self.open_mut(id)?;
        let by_outcome = c.outcome.as_ref().is_some_and(|o| o.kind.allows_remediation());
        let by_forum = c
            .referral
            .as_ref()
            .and_then(|r| r.decision.as_ref())
            .is_some_and(|d| d.binding && d.outcome.allows_remediation());
        if !by_outcome && !by_forum {
            return reject("no_basis", "remediation follows an outcome in the complainant's favour or a binding decision");
        }
        if reference.is_empty() || authorised_by.is_empty() || amount < 0 {
            return reject("invalid", "remediation names the command that did it and who authorised it");
        }
        if c.remediation.iter().any(|r| r.reference == reference) {
            return reject("exists", "that remediation is already recorded");
        }
        if c.involved().contains(authorised_by.as_str()) {
            return reject("separation", "the person complained about cannot authorise the remediation");
        }
        let money = matches!(kind, RemediationKind::Refund | RemediationKind::ExGratia);
        if money != (amount > 0) {
            return reject("invalid", "a payment has an amount, and nothing else does");
        }
        if kind == RemediationKind::ExGratia {
            // The highest limit at or below the authoriser's level.
            let limit = timers.ex_gratia_limits_minor.range(..=authority).next_back().map_or(0, |(_, v)| *v);
            if amount > limit {
                return reject("over_authority", "the goodwill payment is above the authoriser's limit");
            }
        }
        c.remediation.push(Remediation {
            kind,
            reference: reference.clone(),
            amount_minor: amount,
            authorised_by,
            recorded_on: today,
            completed_on: None,
        });
        ctx.events.push(Event::RemediationRecorded {
            complaint: id.to_string(),
            kind,
            reference,
            amount_minor: amount,
        });
        Ok(())
    }

    // --- deadlock and the external forums ---

    fn deadlock(&mut self, id: &str, automatic: bool, ctx: &mut Ctx) -> Result<(), Reject> {
        let today = ctx.today;
        let c = self.open_mut(id)?;
        if c.status != Status::Open {
            return reject("not_open", "only a complaint without an outcome can be deadlocked");
        }
        c.status = Status::Deadlocked;
        c.deadlock = Some(Deadlock {
            on: today,
            automatic,
            notice: None,
        });
        ctx.events.push(Event::DeadlockReached {
            complaint: id.to_string(),
            automatic,
        });
        Ok(())
    }

    fn refer(
        &mut self,
        id: &str,
        forum: Forum,
        reference: String,
        response_due: NaiveDate,
        documents_due: Option<NaiveDate>,
        ctx: &mut Ctx,
    ) -> Result<(), Reject> {
        let today = ctx.today;
        // A court or the privacy regulator is legal's matter.
        if forum != Forum::Scheme && ctx.caller != "legal" {
            return reject("not_allowed", "court proceedings and privacy complaints are recorded by legal");
        }
        let actor = ctx.actor.clone();
        let c = self.open_mut(id)?;
        if c.referral.is_some() {
            return reject("already_referred", "the complaint is already with an external forum");
        }
        if reference.is_empty() || response_due < today || documents_due.is_some_and(|d| d < today) {
            return reject("invalid", "a referral carries the forum's reference and deadlines that have not passed");
        }
        c.referral = Some(Referral {
            forum,
            reference: reference.clone(),
            referred_on: today,
            response_due,
            responded_on: None,
            documents_due,
            bundles: vec![],
            decision: None,
        });
        c.status = Status::Referred;
        if forum != Forum::Scheme {
            c.owner = Some(actor);
        }
        ctx.events.push(Event::Referred {
            complaint: id.to_string(),
            forum,
            reference,
        });
        Ok(())
    }

    fn release_bundle(&mut self, id: &str, bundle: String, documents: Vec<String>, reviewed_by: String, ctx: &mut Ctx) -> Result<(), Reject> {
        let today = ctx.today;
        let actor = ctx.actor.clone();
        if bundle.is_empty() || documents.is_empty() || documents.iter().any(String::is_empty) {
            return reject("invalid", "a bundle has an id and the documents released");
        }
        // What may be shared is checked by one person and released by another.
        if reviewed_by.is_empty() || reviewed_by == actor {
            return reject("separation", "a bundle is reviewed by someone other than the person releasing it");
        }
        let referral = self.referral_mut(id)?;
        if referral.bundles.iter().any(|b| b.id == bundle) {
            return reject("exists", "that bundle is already released");
        }
        let count = documents.len() as u32;
        referral.bundles.push(Bundle {
            id: bundle.clone(),
            documents,
            reviewed_by,
            released_by: actor,
            released_on: today,
        });
        ctx.events.push(Event::BundleReleased {
            complaint: id.to_string(),
            bundle,
            documents: count,
        });
        Ok(())
    }

    fn forum_decision(&mut self, id: &str, outcome: OutcomeKind, binding: bool, remediation_due: Option<NaiveDate>, ctx: &mut Ctx) -> Result<(), Reject> {
        let today = ctx.today;
        let referral = self.referral_mut(id)?;
        if referral.decision.is_some() {
            return reject("already_decided", "the forum's decision is already recorded");
        }
        // A binding decision in the complainant's favour is implemented as
        // remediation, against a date.
        let owed = binding && outcome.allows_remediation();
        if owed != remediation_due.is_some() || remediation_due.is_some_and(|d| d < today) {
            return reject(
                "invalid",
                "a binding decision to put something right carries a date to do it by, and no other does",
            );
        }
        referral.decision = Some(ForumDecision {
            on: today,
            outcome,
            binding,
            remediation_due,
            remediation_completed_on: None,
        });
        ctx.events.push(Event::ForumDecided {
            complaint: id.to_string(),
            outcome,
            binding,
        });
        Ok(())
    }

    fn complete_forum_remediation(&mut self, id: &str, ctx: &mut Ctx) -> Result<(), Reject> {
        let today = ctx.today;
        let c = self.open_mut(id)?;
        let done = !c.remediation.is_empty() && c.remediation.iter().all(|r| r.completed_on.is_some());
        match c
            .referral
            .as_mut()
            .and_then(|r| r.decision.as_mut())
            .filter(|d| d.remediation_due.is_some() && d.remediation_completed_on.is_none())
        {
            Some(d) if done => d.remediation_completed_on = Some(today),
            Some(_) => return reject("remediation_outstanding", "the remediation is not recorded as done"),
            None => return reject("nothing_owed", "no binding decision is waiting to be implemented"),
        }
        Ok(())
    }

    // --- closing ---

    fn close(&mut self, id: &str, detail: CloseDetail, ctx: &mut Ctx) -> Result<(), Reject> {
        self.check_cause(Some(&detail.cause))?;
        let actor = ctx.actor.clone();
        let c = self.open_mut(id)?;
        let forum = c.referral.as_ref().map(|r| r.decision.as_ref());
        match (&c.outcome, forum) {
            // With a forum, its decision ends the matter.
            (_, Some(None)) => return reject("with_forum", "the external forum has not decided"),
            (None, None) => return reject("no_outcome", "a complaint is closed with an outcome"),
            _ => {}
        }
        if c.outcome.as_ref().is_some_and(|o| o.document.is_some() && o.delivered_on.is_none()) {
            return reject("not_delivered", "the outcome letter has no proof of delivery");
        }
        if c.deadlock.as_ref().is_some_and(|d| d.notice.is_none()) {
            return reject("no_deadlock_notice", "the deadlock notice has not been sent");
        }
        if c.remediation.iter().any(|r| r.completed_on.is_none()) {
            return reject("remediation_outstanding", "remediation is still outstanding");
        }
        if forum
            .flatten()
            .is_some_and(|d| d.remediation_due.is_some() && d.remediation_completed_on.is_none())
        {
            return reject("remediation_outstanding", "the forum's decision has not been implemented");
        }
        if c.involved().contains(actor.as_str()) {
            return reject("separation", "the person complained about cannot close the complaint");
        }
        self.finish(id, detail.cause, detail.product_version, detail.rule_version, detail.template_version, ctx);
        Ok(())
    }

    /// Close a complaint whose conditions have been checked.
    fn finish(
        &mut self,
        id: &str,
        cause: String,
        product_version: Option<String>,
        rule_version: Option<String>,
        template_version: Option<String>,
        ctx: &mut Ctx,
    ) {
        let retain = self.timers[&self.complaints[id].timers_version].retain_closed_days;
        let c = self.complaints.get_mut(id).expect("the caller holds the complaint");
        let overturned = c.outcome.as_ref().is_some_and(|o| o.kind.overturns())
            || c.referral.as_ref().and_then(|r| r.decision.as_ref()).is_some_and(|d| d.outcome.overturns());
        // An overturned decision is linked to who made it and under which
        // rule and wording, for the quality loops in those functions.
        let decision = c.links.decisions.first().filter(|_| overturned);
        let mut staff = c.links.staff.clone();
        if overturned {
            staff.extend(c.links.decisions.iter().map(|d| d.decided_by.clone()));
        }
        staff.sort();
        staff.dedup();
        c.closure = Some(Closure {
            on: ctx.today,
            by: ctx.actor.clone(),
            cause: cause.clone(),
            product_version,
            rule_version: rule_version.or_else(|| decision.and_then(|d| d.rule_version.clone())),
            template_version: template_version.or_else(|| decision.and_then(|d| d.wording_version.clone())),
            adviser_engagement: c.links.adviser_engagement.clone(),
            staff,
        });
        c.status = Status::Closed;
        c.category.cause = Some(cause.clone());
        // Access was granted by the case and ends with it.
        c.grants.clear();
        c.awaiting = None;
        ctx.events.push(Event::ComplaintClosed {
            complaint: id.to_string(),
            cause,
            adviser_engagement: c.links.adviser_engagement.clone(),
        });
        self.open.remove(id);
        self.removal.entry(ctx.today + days(retain)).or_default().insert(id.to_string());
    }

    fn root_cause_review(&mut self, month: String, actions: Vec<Action>, ctx: &mut Ctx) -> Result<(), Reject> {
        let valid = NaiveDate::parse_from_str(&format!("{month}-01"), "%Y-%m-%d").is_ok_and(|first| first <= ctx.today) && month.len() == 7;
        if !valid {
            return reject("invalid", "a review is for a month that has begun, written as 2027-11");
        }
        if self.reviews.contains_key(&month) {
            return reject("exists", "that month's review is already recorded");
        }
        if actions.iter().any(|a| a.reference.is_empty() || a.cause.is_empty()) {
            return reject("invalid", "an action names its cause and the record of the change");
        }
        ctx.events.push(Event::RootCauseReviewed {
            month: month.clone(),
            actions: actions.len() as u32,
        });
        self.reviews.insert(
            month,
            RootCauseReview {
                reviewed_on: ctx.today,
                by: ctx.actor.clone(),
                actions,
            },
        );
        Ok(())
    }

    // --- time ---

    /// The letters owed that nobody has issued, at most `limit`: an
    /// acknowledgement for each open complaint without one, a deadlock
    /// notice for each deadlock without one, and a letter to each customer
    /// of a licence case not yet told or excluded. A migrated complaint was
    /// acknowledged, if at all, by the system it came from.
    pub fn letters_due(&self, limit: usize) -> Vec<LetterDue> {
        let mut due = vec![];
        let complaints = self.open.iter().filter_map(|id| self.complaints.get(id));
        for c in complaints {
            let letter = |kind, since, due| LetterDue {
                kind,
                subject: c.id.clone(),
                party: c.complainant.clone(),
                representatives: c.representatives.iter().map(|r| r.party.clone()).collect(),
                since,
                due,
                detail: BTreeMap::new(),
            };
            if c.acknowledged_on.is_none() && !c.migrated {
                let by = self.running(c).into_iter().find(|(t, _)| *t == Timer::Acknowledge).map(|(_, d)| d);
                due.push(letter(LetterKind::Acknowledgement, c.received_on, by));
            }
            if let Some(d) = c.deadlock.as_ref().filter(|d| d.notice.is_none()) {
                due.push(letter(LetterKind::DeadlockNotice, d.on, None));
            }
            if due.len() >= limit {
                due.truncate(limit);
                return due;
            }
        }
        let cases = self.open_conduct.iter().filter_map(|id| self.conduct.get(id));
        for case in cases.filter(|c| c.population_complete) {
            let ConductKind::LicenceEvent { status, effective, .. } = &case.kind else {
                // Other cases are written to by the people handling them.
                continue;
            };
            let detail: BTreeMap<String, String> = [("status".to_string(), status.clone()), ("effective".to_string(), effective.to_string())].into();
            for (party, _) in case.customers.iter().filter(|(_, told)| **told == Contact::Outstanding) {
                if due.len() >= limit {
                    return due;
                }
                due.push(LetterDue {
                    kind: LetterKind::LicenceNotice,
                    subject: case.id.clone(),
                    party: party.clone(),
                    representatives: vec![],
                    since: case.opened_on,
                    due: None,
                    detail: detail.clone(),
                });
            }
        }
        due
    }

    /// The timers running on a complaint: each with its due date.
    pub fn running(&self, c: &Complaint) -> Vec<(Timer, NaiveDate)> {
        let Some(t) = self.timers.get(&c.timers_version) else {
            return vec![];
        };
        let mut running = vec![];
        if c.status == Status::Closed {
            return running;
        }
        if c.acknowledged_on.is_none() {
            running.push((Timer::Acknowledge, c.received_on + days(t.acknowledge_days)));
        }
        if c.outcome.is_none() && c.referral.is_none() {
            let last = c.updates.last().copied().or(c.acknowledged_on).unwrap_or(c.received_on);
            running.push((Timer::Update, last + days(t.update_every_days)));
            running.push((Timer::Decide, c.received_on + days(t.decide_days)));
            if c.deadlock.is_none() {
                running.push((Timer::Deadlock, c.received_on + days(t.deadlock_days)));
            }
        }
        if let Some(a) = &c.awaiting {
            running.push((Timer::ComplainantResponse, a.due));
        }
        if let Some(r) = &c.referral {
            if r.responded_on.is_none() {
                running.push((Timer::ForumResponse, r.response_due));
            }
            if let Some(due) = r.documents_due.filter(|_| r.bundles.is_empty()) {
                running.push((Timer::ForumDocuments, due));
            }
            if let Some(due) = r
                .decision
                .as_ref()
                .filter(|d| d.remediation_completed_on.is_none())
                .and_then(|d| d.remediation_due)
            {
                running.push((Timer::ForumRemediation, due));
            }
        }
        running
    }

    fn tick(&mut self, ctx: &mut Ctx) {
        let today = ctx.today;
        // Open complaints, a bounded number, carrying on from last time.
        let start = self.tick_cursor.take();
        let ids: Vec<String> = match &start {
            Some(after) => self.open.range::<String, _>((std::ops::Bound::Excluded(after), std::ops::Bound::Unbounded)),
            None => self.open.range::<String, _>(..),
        }
        .take(MAX_CHUNK)
        .cloned()
        .collect();
        if ids.len() == MAX_CHUNK {
            self.tick_cursor = ids.last().cloned();
        }
        for id in ids {
            let c = &self.complaints[&id];
            let timers = self.timers[&c.timers_version].clone();
            let mut raised = vec![];
            let mut deadlocked = false;
            for (timer, due) in self.running(c) {
                // The scheme's period ending is not a warning to anyone: it
                // is the deadlock itself, so it cannot be let slide.
                if timer == Timer::Deadlock && today > due {
                    deadlocked = true;
                    continue;
                }
                let Some(mut reached) = level(&timers, due, today) else { continue };
                // The complainant owes nothing: a late answer is a prompt
                // to the handler, never an escalation.
                if timer == Timer::ComplainantResponse {
                    if today <= due {
                        continue;
                    }
                    reached = Level::Lead;
                }
                if c.escalated.get(&timer).is_none_or(|had| *had < reached) {
                    raised.push((timer, reached, due));
                }
            }
            let c = self.complaints.get_mut(&id).expect("listed as open");
            for (timer, reached, due) in raised {
                c.escalated.insert(timer, reached);
                ctx.events.push(Event::TimerEscalated {
                    subject: id.clone(),
                    timer,
                    level: reached,
                    due,
                });
            }
            if deadlocked {
                c.status = Status::Deadlocked;
                c.deadlock = Some(Deadlock {
                    on: today,
                    automatic: true,
                    notice: None,
                });
                ctx.events.push(Event::DeadlockReached {
                    complaint: id.clone(),
                    automatic: true,
                });
            }
        }
        self.tick_conduct(ctx);
        // Closed complaints past their retention are personal data the
        // insurer no longer has a reason to keep.
        let due: Vec<NaiveDate> = self.removal.range(..today).map(|(d, _)| *d).take(MAX_CHUNK).collect();
        for date in due {
            for id in self.removal.remove(&date).unwrap_or_default() {
                if let Some(c) = self.complaints.remove(&id) {
                    if let Some(set) = self.by_complainant.get_mut(&c.complainant) {
                        set.remove(&id);
                        if set.is_empty() {
                            self.by_complainant.remove(&c.complainant);
                        }
                    }
                    self.removed += 1;
                    ctx.events.push(Event::ComplaintRecordRemoved { complaint: id });
                }
            }
        }
    }

    // --- the stream and the hash ---

    /// Events not yet on the stream, oldest first.
    pub fn pending_events(&self, limit: usize) -> Vec<EventRecord> {
        self.unpublished.iter().take(limit).cloned().collect()
    }

    /// A hash of the whole state. Replicas that applied the same log agree.
    pub fn state_hash(&self) -> String {
        // Every map in the state has keys JSON can carry, so this cannot fail.
        hex::encode(Sha256::digest(serde_json::to_vec(self).expect("the state is serialisable")))
    }
}
