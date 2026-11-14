//! The driver that carries the module's effects to where they take effect.
//!
//! The state machine decides; the driver carries. Each pass reads the oldest
//! effects from the module's outbox and delivers them in order: capacity to
//! the reinsurance module, policies to the core, a scheme member's excess
//! decision to group administration, and everything else to whatever is
//! connected for it. What a receiver answers comes back to the module as a
//! command, and what has been delivered is acknowledged.
//!
//! Every delivery carries a key taken from the effect, so a driver that
//! stops anywhere is restarted and carries on: each receiver applies a
//! repeat once. An effect nothing can take, or that its receiver refuses, is
//! parked in the module with the reason, where a person can see it and put
//! it back. Nothing is dropped and nothing waits behind it. Only a receiver
//! that cannot be reached stops a pass, and the next pass starts again from
//! the same effect.

use std::future::Future;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use ic_core::event::Event as CoreEvent;
use ic_core::{home_partition, GroupId, PolicyId};
use ic_group::command::{ExcessDecision, GroupCommand};
use ic_ledger::PartitionId;
use ic_reinsurance::capacity::{CapacityRequest, Outcome};
use ic_reinsurance::treaty::RiskFactors;
use serde_json::Value;

use crate::command::{Applied, CapacityAnswer, Command, Effect, Envelope, IssuedLayer, NonDisclosureOutcome, PremiumOnAvoidance, Reject, View};
use crate::model::{Entry, Proposal, ProposalId};
use crate::progress::{Pending, ProgressRow};
use crate::special::Query;

/// The callers the driver acts as when it brings an answer back.
pub const DRIVER: &str = "effects-driver";
const AS_REINSURANCE: &str = "reinsurance";
const AS_ISSUER: &str = "issuer";
const AS_TIMER: &str = "timer";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DriverError {
    /// Something could not be reached. Nothing is known to have happened:
    /// the next pass does the same again.
    #[error("unavailable: {0}")]
    Unavailable(String),
    #[error("unexpected answer: {0}")]
    Answer(String),
}

/// A receiver's own "no", as opposed to not being reachable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: String,
    pub message: String,
}

impl Refusal {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

/// The underwriting module, wherever it runs.
pub trait Underwriting: Send + Sync {
    fn propose(&self, envelope: Envelope) -> impl Future<Output = Result<Result<Applied, Reject>, DriverError>> + Send;
    fn ask(&self, query: Query) -> impl Future<Output = Result<Value, DriverError>> + Send;
}

/// The reinsurance module.
pub trait Reinsurer: Send + Sync {
    fn propose(&self, envelope: ic_reinsurance::Envelope) -> impl Future<Output = Result<Result<(), Refusal>, DriverError>> + Send;
    /// The capacity answer for a reservation as it stands. `None` once the
    /// reservation has been released or its cover has arrived.
    fn capacity(&self, reservation: &str) -> impl Future<Output = Result<Option<Outcome>, DriverError>> + Send;
}

/// What the core made of a command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoreAnswer {
    pub refused: Option<Refusal>,
    pub events: Vec<CoreEvent>,
}

/// The core's partitions.
pub trait Core: Send + Sync {
    fn send(&self, partition: PartitionId, key: &str, caller: &str, command: ic_core::Command) -> impl Future<Output = Result<CoreAnswer, DriverError>> + Send;

    /// Lapsed policies whose reinstatement the core refused for want of
    /// underwriting's decision, as collections lists them, each as the core
    /// holds it now. With nothing to ask, there are none.
    fn lapsed_for_underwriting(&self) -> impl Future<Output = Result<Vec<ic_core::policy::Policy>, DriverError>> + Send {
        async { Ok(vec![]) }
    }
}

/// The group administration module.
pub trait Schemes: Send + Sync {
    fn propose(&self, envelope: ic_group::command::Envelope) -> impl Future<Output = Result<Result<(), Refusal>, DriverError>> + Send;
}

/// The channel module, which keeps the portals' read models.
pub trait Channels: Send + Sync {
    /// Publish rows under the module's own name. `key` is the same on every try.
    fn publish(&self, key: &str, at: DateTime<Utc>, rows: &[ProgressRow]) -> impl Future<Output = Result<Result<(), Refusal>, DriverError>> + Send;
}

/// Everything with no module of its own yet behind it: evidence providers,
/// delegates and correspondence. `Ok(None)` is taken; `Ok(Some(reason))` is
/// why it could not be.
pub trait Forward: Send + Sync {
    /// `seq` is the effect's number in the outbox, the same on every try,
    /// for a receiver that de-duplicates on it.
    fn forward(&self, seq: u64, effect: &Effect) -> impl Future<Output = Result<Option<String>, DriverError>> + Send;
}

/// Nothing is connected: every effect offered is parked with the reason.
#[derive(Debug, Clone, Copy, Default)]
pub struct NothingConnected;

impl Forward for NothingConnected {
    async fn forward(&self, _seq: u64, effect: &Effect) -> Result<Option<String>, DriverError> {
        Ok(Some(
            match effect {
                Effect::RequestEvidence { .. } | Effect::ChaseEvidence { .. } | Effect::CancelEvidence { .. } => "no evidence provider is connected",
                Effect::RequestDelegateFile { .. } | Effect::RequestDelegatedDecision { .. } => "no delegate is connected",
                _ => "correspondence is not connected to underwriting",
            }
            .to_string(),
        ))
    }
}

/// What one pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pass {
    /// Effects delivered and acknowledged.
    pub delivered: u32,
    /// Effects whose answer the module no longer wanted: the proposal had
    /// moved on while the effect was in flight.
    pub stale: u32,
    /// Effects parked, with the number each had and the reason.
    pub parked: Vec<(u64, String)>,
    /// Proposals whose progress was published to the portals.
    pub published: u32,
    /// Proposals whose progress the channel module refused.
    pub progress_refused: u32,
}

/// What a day's sweep did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Swept {
    pub pages: u32,
    pub lapsed: u32,
    pub chased: u32,
    pub escalated: u32,
    /// Reinstatement proposals opened for lapsed policies collections
    /// listed, and those it listed that could not be opened, with why.
    pub reinstatements: u32,
    pub not_reinstated: Vec<(PolicyId, String)>,
    /// Why collections' list could not be read, when it could not. The
    /// sweep itself stands; tomorrow's tries again.
    pub reinstatements_unavailable: Option<String>,
}

enum Step {
    Delivered,
    Stale,
    Park(String),
}

/// Where a group excess proposal came from: `grp-uw:{scheme}:{case}`.
pub fn excess_case(proposal: &ProposalId) -> Option<(String, u64)> {
    let rest = proposal.to_string();
    let rest = rest.strip_prefix("grp-uw:")?;
    let (scheme, case) = rest.rsplit_once(':')?;
    if scheme.is_empty() {
        return None;
    }
    Some((scheme.to_string(), case.parse().ok()?))
}

fn answer_of(outcome: &Outcome) -> CapacityAnswer {
    match outcome {
        Outcome::Automatic { .. } => CapacityAnswer::Automatic,
        Outcome::FacultativeRequired { .. } => CapacityAnswer::FacultativeRequired,
        Outcome::Excluded { .. } => CapacityAnswer::Excluded,
    }
}

/// The layers of an issued policy, from the core's reply.
fn issued_layers(events: &[CoreEvent]) -> Vec<IssuedLayer> {
    events
        .iter()
        .filter_map(|e| match e {
            CoreEvent::PolicyIssued { layers, .. } => Some(layers),
            _ => None,
        })
        .flatten()
        .map(|l| IssuedLayer {
            life: l.life.clone(),
            cover: l.cover.clone(),
            layer: l.layer,
        })
        .collect()
}

/// The layers added to a policy in force, from the core's reply.
fn added_layers(events: &[CoreEvent]) -> impl Iterator<Item = IssuedLayer> + '_ {
    events.iter().filter_map(|e| match e {
        CoreEvent::LayerAdded { layer, .. } => Some(IssuedLayer {
            life: layer.life.clone(),
            cover: layer.cover.clone(),
            layer: layer.layer,
        }),
        _ => None,
    })
}

pub struct Driver<U, R, C, S, F, P> {
    pub underwriting: U,
    pub reinsurer: R,
    pub core: C,
    pub schemes: S,
    pub forward: F,
    pub channels: P,
    /// Effects read from the outbox at a time.
    pub batch: usize,
    /// The time, read once per command sent to the module.
    pub clock: fn() -> DateTime<Utc>,
}

impl<U: Underwriting, R: Reinsurer, C: Core, S: Schemes, F: Forward, P: Channels> Driver<U, R, C, S, F, P> {
    fn today(&self) -> NaiveDate {
        (self.clock)().date_naive()
    }

    /// Bring an answer back to the module. `Ok(false)` when the module no
    /// longer wants it.
    async fn tell(&self, key: String, caller: &str, command: Command) -> Result<Result<Applied, Reject>, DriverError> {
        let at = (self.clock)();
        self.underwriting
            .propose(Envelope {
                key,
                caller: caller.to_string(),
                at,
                today: at.date_naive(),
                command,
            })
            .await
    }

    /// The driver's own housekeeping, which the module never refuses.
    async fn housekeeping(&self, key: String, command: Command) -> Result<(), DriverError> {
        self.tell(key, DRIVER, command)
            .await?
            .map(|_| ())
            .map_err(|e| DriverError::Answer(format!("the module refused the driver: {}: {}", e.code, e.message)))
    }

    async fn reinsure(&self, key: String, caller: &str, command: ic_reinsurance::Command) -> Result<Result<(), Refusal>, DriverError> {
        self.reinsurer
            .propose(ic_reinsurance::Envelope {
                key,
                caller: caller.to_string(),
                on: self.today(),
                command,
            })
            .await
    }

    async fn group_of(&self, proposal: &ProposalId) -> Result<Option<GroupId>, DriverError> {
        Ok(self.proposal(proposal).await?.map(|p| p.group))
    }

    async fn proposal(&self, proposal: &ProposalId) -> Result<Option<Proposal>, DriverError> {
        let answer = self.underwriting.ask(Query::Proposal { proposal: proposal.clone() }).await?;
        serde_json::from_value(answer).map_err(|e| DriverError::Answer(e.to_string()))
    }

    /// Carry the outbox until it is empty. What was delivered before a stop
    /// is acknowledged.
    pub async fn deliver(&self) -> Result<Pass, DriverError> {
        let mut pass = Pass::default();
        loop {
            let answer = self
                .underwriting
                .ask(Query::Outbox {
                    after: 0,
                    max: self.batch.max(1),
                })
                .await?;
            let outbox: Vec<(u64, Effect)> = serde_json::from_value(answer).map_err(|e| DriverError::Answer(e.to_string()))?;
            if outbox.is_empty() {
                self.publish_progress(&mut pass).await?;
                return Ok(pass);
            }
            let mut through = None;
            let mut stopped = None;
            for (seq, effect) in &outbox {
                match self.one(*seq, effect).await {
                    Ok(Step::Delivered) => pass.delivered += 1,
                    Ok(Step::Stale) => pass.stale += 1,
                    Ok(Step::Park(reason)) => {
                        // Parking takes the effect out of the outbox itself.
                        if let Err(e) = self
                            .housekeeping(
                                format!("park:{seq}"),
                                Command::ParkEffect {
                                    effect: *seq,
                                    reason: reason.clone(),
                                },
                            )
                            .await
                        {
                            stopped = Some(e);
                            break;
                        }
                        pass.parked.push((*seq, reason));
                        continue;
                    }
                    Err(e) => {
                        stopped = Some(e);
                        break;
                    }
                }
                through = Some(*seq);
            }
            if let Some(through) = through {
                self.housekeeping(format!("ack:{through}"), Command::AcknowledgeEffects { through }).await?;
            }
            if let Some(e) = stopped {
                return Err(e);
            }
        }
    }

    /// Publish the progress of every proposal waiting for it, then tell the
    /// module which version went. The channel module upserts rows by key, so
    /// a repeat is harmless. A refusal will not change on a retry: it is
    /// logged and counted, and the version is let go so it does not hold up
    /// the rest.
    async fn publish_progress(&self, pass: &mut Pass) -> Result<(), DriverError> {
        let answer = self.underwriting.ask(Query::Progress { max: self.batch.max(1) }).await?;
        let pending: Vec<Pending> = serde_json::from_value(answer).map_err(|e| DriverError::Answer(e.to_string()))?;
        for p in pending {
            let key = format!("{}/progress/{}", p.proposal, p.version);
            match self.channels.publish(&key, (self.clock)(), &p.rows).await? {
                Ok(()) => pass.published += 1,
                Err(r) => {
                    tracing::warn!(proposal = %p.proposal, code = %r.code, message = %r.message, "the channel module refused a proposal's progress");
                    pass.progress_refused += 1;
                }
            }
            self.housekeeping(
                format!("published:{key}"),
                Command::ProgressPublished {
                    proposal: p.proposal,
                    version: p.version,
                },
            )
            .await?;
        }
        Ok(())
    }

    /// The day's sweep: lapse what has expired, chase evidence, escalate
    /// what nobody has touched, a page at a time until the open proposals
    /// are done. Each page has a key of the day and where it starts, so a
    /// sweep repeated on the same day, by this driver or another, does the
    /// day's work once.
    pub async fn sweep(&self, page: u32) -> Result<Swept, DriverError> {
        let today = self.today();
        let mut total = Swept::default();
        let mut after: Option<ProposalId> = None;
        loop {
            let key = format!("sweep:{today}:{}", after.as_ref().map(ToString::to_string).unwrap_or_default());
            let command = Command::Sweep {
                after: after.clone(),
                max: page.clamp(1, 1000),
            };
            let applied = self
                .tell(key, AS_TIMER, command)
                .await?
                .map_err(|e| DriverError::Answer(format!("the module refused the sweep: {}: {}", e.code, e.message)))?;
            let Some(View::Sweep {
                next,
                lapsed,
                chased,
                escalated,
            }) = applied.view
            else {
                // A repeat older than the results the module keeps: that day's sweep was done long ago.
                break;
            };
            total.pages += 1;
            total.lapsed += lapsed;
            total.chased += chased;
            total.escalated += escalated;
            match next {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        if let Err(e) = self.reinstatements(today, &mut total).await {
            total.reinstatements_unavailable = Some(e.to_string());
        }
        Ok(total)
    }

    /// Open a reinstatement proposal, as collections, for each lapsed policy
    /// collections lists. The proposal's ID comes from the policy and its
    /// lapse, so a policy already opened is refused `duplicate_proposal`
    /// and counted as done; any other refusal is kept with the policy for
    /// the operator and does not stop the rest.
    async fn reinstatements(&self, today: NaiveDate, total: &mut Swept) -> Result<(), DriverError> {
        for policy in self.core.lapsed_for_underwriting().await? {
            let Some(open) = crate::reinstatement::reinstatement_proposal(&policy, today) else {
                total
                    .not_reinstated
                    .push((policy.id.clone(), "not lapsed, or no cover on a named life to underwrite".into()));
                continue;
            };
            let key = format!("collections:{}", open.proposal);
            match self.tell(key, "collections", Command::OpenProposal(Box::new(open))).await? {
                Ok(applied) if !applied.duplicate => total.reinstatements += 1,
                // Opened before, under this key or by anyone under another.
                Ok(_) => {}
                Err(r) if r.code == "duplicate_proposal" => {}
                Err(r) => total.not_reinstated.push((policy.id.clone(), format!("{}: {}", r.code, r.message))),
            }
        }
        Ok(())
    }

    pub async fn cycle(&self, swept: &mut Option<NaiveDate>, page: u32) -> (Result<Pass, DriverError>, Option<Result<Swept, DriverError>>) {
        let today = self.today();
        let sweep = if *swept == Some(today) {
            None
        } else {
            let done = self.sweep(page).await;
            if done.is_ok() {
                *swept = Some(today);
            }
            Some(done)
        };
        (self.deliver().await, sweep)
    }

    /// Run cycles until `stop` resolves. `seen` is shown each cycle's results.
    pub async fn run(
        &self,
        every: Duration,
        page: u32,
        stop: impl Future<Output = ()>,
        mut seen: impl FnMut(&Result<Pass, DriverError>, &Option<Result<Swept, DriverError>>),
    ) {
        tokio::pin!(stop);
        let mut swept = None;
        loop {
            let (pass, sweep) = self.cycle(&mut swept, page).await;
            match &pass {
                Ok(p) if p.parked.is_empty() => tracing::debug!(delivered = p.delivered, stale = p.stale, "underwriting delivery pass"),
                Ok(p) => tracing::warn!(delivered = p.delivered, parked = ?p.parked, "underwriting effects were parked"),
                Err(e) => tracing::warn!(error = %e, "an underwriting delivery pass could not finish"),
            }
            match &sweep {
                None => {}
                Some(Ok(s)) => tracing::info!(
                    pages = s.pages,
                    lapsed = s.lapsed,
                    chased = s.chased,
                    escalated = s.escalated,
                    "underwriting daily sweep"
                ),
                Some(Err(e)) => tracing::warn!(error = %e, "the underwriting daily sweep could not finish"),
            }
            seen(&pass, &sweep);
            tokio::select! {
                () = &mut stop => return,
                () = tokio::time::sleep(every) => {}
            }
        }
    }

    async fn one(&self, seq: u64, effect: &Effect) -> Result<Step, DriverError> {
        Ok(match effect {
            Effect::ReserveCapacity {
                reservation,
                party,
                product_code,
                cover_code,
                cover_type,
                sum_insured_minor,
                commencement,
                proposal_expires,
                risk_codes,
                excess_reference,
            } => {
                let request = CapacityRequest {
                    reservation_key: reservation.clone(),
                    party: party.clone(),
                    product_code: product_code.clone(),
                    cover_code: cover_code.clone(),
                    cover_type: cover_type.clone(),
                    sum_insured_minor: *sum_insured_minor,
                    commencement: *commencement,
                    proposal_expires: *proposal_expires,
                    risk_codes: risk_codes.clone(),
                    excess_reference: excess_reference.clone(),
                };
                let reserved = self
                    .reinsure(
                        format!("uw:{reservation}:reserve"),
                        "underwriting",
                        ic_reinsurance::Command::ReserveCapacity(Box::new(request)),
                    )
                    .await?;
                match reserved {
                    Ok(()) => {}
                    // Reserved on an earlier pass under another key: the answer is still wanted.
                    Err(r) if r.code == "reservation_exists" => {}
                    Err(r) => return Ok(Step::Park(format!("reinsurance refused the reservation: {}: {}", r.code, r.message))),
                }
                let Some(outcome) = self.reinsurer.capacity(reservation).await? else {
                    // Released already: the module asked for that itself, later in the outbox.
                    return Ok(Step::Stale);
                };
                let told = self
                    .tell(
                        format!("cap:{reservation}"),
                        AS_REINSURANCE,
                        Command::CapacityAnswered {
                            reservation: reservation.clone(),
                            answer: answer_of(&outcome),
                        },
                    )
                    .await?;
                match told {
                    Ok(_) => Step::Delivered,
                    Err(e) if matches!(e.code.as_str(), "unknown_reservation" | "stale_reservation" | "closed") => Step::Stale,
                    Err(e) => Step::Park(format!("the module refused the capacity answer: {}: {}", e.code, e.message)),
                }
            }
            Effect::ReleaseCapacity { reservation, reason } => {
                let released = self
                    .reinsure(
                        format!("uw:{reservation}:release"),
                        "underwriting",
                        ic_reinsurance::Command::ReleaseCapacity {
                            reservation_id: reservation.clone(),
                            reason: reason.clone(),
                        },
                    )
                    .await?;
                match released {
                    Ok(()) => Step::Delivered,
                    // Never reserved, or gone already: there is nothing to release.
                    Err(r) if matches!(r.code.as_str(), "unknown_reservation" | "reservation_released" | "reservation_not_held") => Step::Stale,
                    Err(r) => Step::Park(format!("reinsurance refused the release: {}: {}", r.code, r.message)),
                }
            }
            Effect::ConfirmCapacity {
                reservation,
                policy,
                cover,
                layers,
                ..
            } => {
                let confirmed = self
                    .reinsure(
                        format!("uw:{reservation}:confirm"),
                        "underwriting",
                        ic_reinsurance::Command::ConfirmCapacity {
                            reservation_id: reservation.clone(),
                            policy: policy.clone(),
                            cover: cover.clone(),
                            layers: layers.clone(),
                        },
                    )
                    .await?;
                match confirmed {
                    Ok(()) => Step::Delivered,
                    Err(r) if r.code == "reservation_confirmed" => Step::Stale,
                    Err(r) => Step::Park(format!("reinsurance refused the confirmation: {}: {}", r.code, r.message)),
                }
            }
            Effect::RecordRiskFactors {
                policy,
                life,
                date_of_birth,
                sex,
                smoker,
            } => {
                let recorded = self
                    .reinsure(
                        format!("uw:{policy}:{life}:factors"),
                        "new-business",
                        ic_reinsurance::Command::RecordRiskFactors {
                            policy: policy.clone(),
                            life: life.clone(),
                            factors: RiskFactors {
                                date_of_birth: *date_of_birth,
                                sex: *sex,
                                smoker: *smoker,
                            },
                        },
                    )
                    .await?;
                match recorded {
                    Ok(()) => Step::Delivered,
                    Err(r) => Step::Park(format!("reinsurance refused the rating factors: {}: {}", r.code, r.message)),
                }
            }
            Effect::IssuePolicy {
                key,
                proposal,
                expected_product_hash,
                command,
                ..
            } => {
                let issue = ic_core::Command::IssueQuoted {
                    expected_product_hash: expected_product_hash.clone(),
                    issue: command.clone(),
                };
                let answer = self.core.send(home_partition(&command.group), key, "new-business", issue).await?;
                let outcome = match answer.refused {
                    None => Ok(issued_layers(&answer.events)),
                    Some(r) => Err(r.code),
                };
                self.issue_answer(key, proposal, outcome).await?
            }
            Effect::ApplyTerms {
                key,
                proposal,
                entry: entry @ (Entry::Increase { .. } | Entry::Alteration { .. } | Entry::GroupContinuation { .. } | Entry::InsurabilityOption { .. }),
                policy,
                covers,
                ..
            } => {
                let increase = !matches!(entry, Entry::Alteration { .. });
                let what = match entry {
                    Entry::Increase { .. } => "increase",
                    Entry::GroupContinuation { .. } => "continuation",
                    Entry::InsurabilityOption { .. } => "option",
                    _ => "alteration",
                };
                // A continued amount covers the leaver from the continuation's inception,
                // even when it is taken up later: the core backdates the layer to it.
                let start = match entry {
                    Entry::GroupContinuation { inception, .. } => Some(*inception),
                    _ => None,
                };
                let Some(held) = self.proposal(proposal).await? else {
                    return Ok(Step::Park(format!("proposal {proposal} is not known to the module")));
                };
                let group = held.group.clone();
                let mut layers = Vec::new();
                let mut refused = None;
                for cover in covers {
                    let Some(life) = &cover.life else {
                        return Ok(Step::Park(format!("the {what} on {} names no life", cover.code)));
                    };
                    // Keyed by the proposal and cover, not the attempt: a cover the core has
                    // written is not written again when the rest is tried again.
                    let layer = if let Entry::InsurabilityOption {
                        event, event_date, evidence, ..
                    } = entry
                    {
                        // The core checks the option's window, age, uses and limits, and
                        // writes the layer on the terms of the life's original cover.
                        ic_core::Command::ExerciseOption(Box::new(ic_core::command::OptionExercise {
                            policy: policy.clone(),
                            cover: cover.code.clone(),
                            life: life.clone(),
                            event: event.clone(),
                            event_date: *event_date,
                            sum_insured_minor: cover.sum_insured_minor,
                            evidence: evidence.clone(),
                            reference: key.clone(),
                        }))
                    } else if increase {
                        ic_core::Command::AddUnderwrittenLayer(Box::new(ic_core::command::UnderwrittenLayer {
                            start,
                            policy: policy.clone(),
                            cover: cover.code.clone(),
                            life: life.clone(),
                            sum_insured_minor: cover.sum_insured_minor,
                            loadings: cover.loadings.clone(),
                            exclusions: cover.exclusions.clone(),
                            reference: key.clone(),
                        }))
                    } else {
                        // The cover keeps its sum insured: an alteration changes the terms
                        // on what the life already holds.
                        // A change of occupation reaches the core as the life's new class too,
                        // so the rewritten layers are rated on it.
                        ic_core::Command::AlterUnderwrittenTerms(Box::new(ic_core::command::AlteredTerms {
                            occupation_class: held.lives.get(life).and_then(|f| f.factors.occupation_class.clone()),
                            policy: policy.clone(),
                            cover: cover.code.clone(),
                            life: life.clone(),
                            loadings: cover.loadings.clone(),
                            exclusions: cover.exclusions.clone(),
                            reference: key.clone(),
                        }))
                    };
                    let sent = format!("{proposal}/{what}/{life}/{}", cover.code);
                    let answer = self.core.send(home_partition(&group), &sent, "underwriting", layer).await?;
                    match answer.refused {
                        None => layers.extend(added_layers(&answer.events)),
                        // The cover is already on these terms: there is nothing to write.
                        Some(r) if !increase && r.code == "no_change" => {}
                        Some(r) => {
                            refused = Some(r.code);
                            break;
                        }
                    }
                }
                let outcome = refused.map_or(Ok(layers), Err);
                self.issue_answer(key, proposal, outcome).await?
            }
            Effect::IssueInterim { key, proposal, command } => {
                let answer = self
                    .core
                    .send(
                        home_partition(&command.group),
                        key,
                        "new-business",
                        ic_core::Command::IssuePolicy(command.clone()),
                    )
                    .await?;
                if let Some(r) = answer.refused {
                    return Ok(Step::Park(format!("the core refused interim cover: {}: {}", r.code, r.message)));
                }
                match self
                    .tell(format!("{key}/issued"), AS_ISSUER, Command::InterimIssued { proposal: proposal.clone() })
                    .await?
                {
                    Ok(_) => Step::Delivered,
                    Err(e) => Step::Park(format!(
                        "interim cover is in force but the module refused to record it: {}: {}",
                        e.code, e.message
                    )),
                }
            }
            Effect::EndInterim { key, proposal, policy } => {
                let Some(group) = self.group_of(proposal).await? else {
                    return Ok(Step::Park(format!("proposal {proposal} is not known to the module")));
                };
                let answer = self
                    .core
                    .send(
                        home_partition(&group),
                        key,
                        "underwriting",
                        ic_core::Command::CancelPolicy { policy: policy.clone() },
                    )
                    .await?;
                match answer.refused {
                    None => Step::Delivered,
                    // Never issued, or ended already.
                    Some(r) if matches!(r.code.as_str(), "unknown_policy" | "not_in_force") => Step::Stale,
                    Some(r) => Step::Park(format!("the core refused to end interim cover: {}: {}", r.code, r.message)),
                }
            }
            Effect::ApplyTerms {
                key,
                proposal,
                entry: Entry::GroupExcess { .. },
                covers,
                employer_may_see_loading,
                member_pays_loading,
                ..
            } => {
                let (Some((scheme, case)), [cover]) = (excess_case(proposal), covers.as_slice()) else {
                    return Ok(Step::Park("not one excess cover on a proposal group administration opened".to_string()));
                };
                let decision = ExcessDecision::Accepted {
                    sum_insured_minor: cover.sum_insured_minor,
                    loadings: cover.loadings.clone(),
                    exclusions: cover.exclusions.clone(),
                    member_consents_to_itemising: *employer_may_see_loading,
                    member_pays_loading: *member_pays_loading,
                };
                self.excess_decision(key, scheme, case, decision).await?
            }
            // Only an expiry or a decline ends an offer, and either way the
            // terms in place stand: group administration hears it once.
            Effect::ReofferEnded { key, proposal, .. } => {
                let Some((scheme, case)) = excess_case(proposal) else {
                    return Ok(Step::Park("not a proposal group administration opened".to_string()));
                };
                self.excess_decision(key, scheme, case, ExcessDecision::ReofferDeclined).await?
            }
            Effect::ProposalEnded {
                key,
                proposal,
                entry: Entry::GroupExcess { .. },
                outcome,
                ..
            } => {
                let Some((scheme, case)) = excess_case(proposal) else {
                    return Ok(Step::Park("not a proposal group administration opened".to_string()));
                };
                let decision = if outcome == "declined" {
                    ExcessDecision::Declined
                } else {
                    ExcessDecision::NotProceeded
                };
                self.excess_decision(key, scheme, case, decision).await?
            }
            Effect::ApplyTerms {
                key,
                proposal,
                entry: Entry::Reinstatement { .. },
                policy,
                covers,
                ..
            } => {
                let Some(found) = self.proposal(proposal).await? else {
                    return Ok(Step::Park(format!("proposal {proposal} is not known to the module")));
                };
                let mut terms = Vec::new();
                for cover in covers {
                    let Some(life) = &cover.life else {
                        return Ok(Step::Park(format!("the reinstatement of {} names no life", cover.code)));
                    };
                    terms.push(ic_core::policy::LayerTerms {
                        cover: cover.code.clone(),
                        life: life.clone(),
                        loadings: cover.loadings.clone(),
                        exclusions: cover.exclusions.clone(),
                    });
                }
                // The decision is recorded against the lapse; the policy comes back when
                // the arrears are paid and the reinstatement is asked for. It can be relied
                // on for as long as the proposal's own terms can.
                let decision = ic_core::Command::ApproveReinstatement(Box::new(ic_core::command::ReinstatementTerms {
                    policy: policy.clone(),
                    reference: key.clone(),
                    valid_until: found.expires_on,
                    terms,
                }));
                let sent = format!("{proposal}/reinstatement");
                let answer = self.core.send(home_partition(&found.group), &sent, "underwriting", decision).await?;
                let outcome = answer.refused.map_or(Ok(Vec::new()), |r| Err(r.code));
                self.issue_answer(key, proposal, outcome).await?
            }
            // Group administration reads a continuation's end from the outbox itself.
            Effect::ProposalEnded {
                entry: Entry::GroupContinuation { .. },
                ..
            } => Step::Delivered,
            Effect::ProposalEnded { .. } => Step::Park("policy servicing is not connected to underwriting".to_string()),
            Effect::NonDisclosureRemedy {
                key,
                proposal,
                policy,
                outcome,
                review,
                terms,
                premium,
                ..
            } => {
                let group = match proposal {
                    Some(proposal) => self.group_of(proposal).await?,
                    None => None,
                };
                let Some(group) = group else {
                    return Ok(Step::Park(
                        "the remedy does not say which proposal, so where the policy is held is not known".to_string(),
                    ));
                };
                let remedy = match (outcome, premium) {
                    (NonDisclosureOutcome::Avoid, Some(premium)) => ic_core::Command::AvoidPolicy {
                        policy: policy.clone(),
                        case: review.clone(),
                        premium: match premium {
                            PremiumOnAvoidance::Refund => ic_core::policy::PremiumOnAvoidance::Refund,
                            PremiumOnAvoidance::Retain => ic_core::policy::PremiumOnAvoidance::Retain,
                        },
                    },
                    (NonDisclosureOutcome::Vary, _) => ic_core::Command::VaryForNonDisclosure {
                        policy: policy.clone(),
                        case: review.clone(),
                        terms: terms.clone(),
                    },
                    _ => return Ok(Step::Park(format!("a {} remedy with nothing the core can act on", outcome.key()))),
                };
                // Keyed by the effect: the core does nothing twice for the same case either.
                let answer = self.core.send(home_partition(&group), key, "underwriting", remedy).await?;
                match answer.refused {
                    None => Step::Delivered,
                    Some(r) => Step::Park(format!("the core refused the remedy: {}: {}", r.code, r.message)),
                }
            }
            other => match self.forward.forward(seq, other).await? {
                None => Step::Delivered,
                Some(reason) => Step::Park(reason),
            },
        })
    }

    /// Bring the core's answer to an issue back: the layers it wrote, or
    /// the code it refused with, which the module keeps for a retry.
    async fn issue_answer(&self, key: &str, proposal: &ProposalId, outcome: Result<Vec<IssuedLayer>, String>) -> Result<Step, DriverError> {
        let (suffix, back) = match outcome {
            Ok(layers) => (
                "issued",
                Command::PolicyIssued {
                    proposal: proposal.clone(),
                    layers,
                },
            ),
            Err(code) => (
                "rejected",
                Command::IssueRejected {
                    proposal: proposal.clone(),
                    code,
                },
            ),
        };
        Ok(match self.tell(format!("{key}/{suffix}"), AS_ISSUER, back).await? {
            Ok(_) => Step::Delivered,
            Err(e) if e.code == "not_issuing" => Step::Stale,
            Err(e) => Step::Park(format!("the module refused the core's answer: {}: {}", e.code, e.message)),
        })
    }

    async fn excess_decision(&self, key: &str, scheme: String, case: u64, decision: ExcessDecision) -> Result<Step, DriverError> {
        let sent = self
            .schemes
            .propose(ic_group::command::Envelope {
                key: format!("uw:{key}"),
                caller: "underwriting".to_string(),
                actor: DRIVER.to_string(),
                today: self.today(),
                command: GroupCommand::RecordExcessDecision { scheme, case, decision },
            })
            .await?;
        Ok(match sent {
            Ok(()) => Step::Delivered,
            Err(r) => Step::Park(format!("group administration refused the decision: {}: {}", r.code, r.message)),
        })
    }
}
