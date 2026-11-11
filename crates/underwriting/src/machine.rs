//! The underwriting state machine.
//!
//! Everything the module knows is in [`UnderwritingState`], and the only way
//! it changes is [`ic_log::StateMachine::apply`]. Applying is a pure function
//! of the state and the command: no clock, no randomness, no calls out. A
//! command that is refused leaves no trace, and a repeated key gets the
//! original answer.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, NaiveDate, Utc};
use ic_core::{LifeId, PartyId, PolicyId};
use ic_log::StateMachine;
use serde::{Deserialize, Serialize};

use crate::authority::{AuthorityProfile, CaseAction, Standing, Subject};
use crate::command::{Applied, Command, Effect, Envelope, Event, LicenceStatus, Reject};
use crate::config::Config;
use crate::model::{AdviserRef, LifeFile, Proposal, ProposalId};

/// How many results are kept to answer a repeated key with. A repeat of an
/// older key is still recognised and changes nothing; it is answered with an
/// empty result marked as a duplicate.
pub const RESULTS_KEPT: usize = 20_000;

/// A review of the disclosure behind a decision, opened from a claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Review {
    pub claim_ref: String,
    pub proposal: ProposalId,
    pub life: LifeId,
    pub decision: String,
    pub opened_by: String,
    pub closed: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnderwritingState {
    pub config: Config,
    pub(crate) proposals: BTreeMap<ProposalId, Proposal>,
    /// Proposals that can still change by the passing of time.
    pub(crate) open: BTreeSet<ProposalId>,
    pub(crate) seen: BTreeMap<String, u64>,
    pub(crate) results: BTreeMap<u64, Applied>,
    pub(crate) cases: BTreeMap<String, (ProposalId, LifeId)>,
    /// Open cases and who owns each.
    pub(crate) owners: BTreeMap<String, String>,
    pub(crate) evidence: BTreeMap<String, (ProposalId, LifeId)>,
    pub(crate) reservations: BTreeMap<String, (ProposalId, LifeId, String)>,
    pub(crate) decisions: BTreeMap<String, (ProposalId, LifeId)>,
    pub(crate) by_policy: BTreeMap<PolicyId, BTreeSet<ProposalId>>,
    pub(crate) by_adviser: BTreeMap<String, BTreeSet<ProposalId>>,
    /// Relationships underwriters have declared with people.
    pub(crate) conflicts: BTreeSet<(String, PartyId)>,
    pub(crate) licences: BTreeMap<String, LicenceStatus>,
    pub(crate) reviews: BTreeMap<String, Review>,
    pub(crate) delegate_refs: BTreeSet<(String, String)>,
    /// Sums insured standing accepted under each delegated authority.
    pub(crate) delegated_total: BTreeMap<String, i64>,
}

/// What a handler works with: the time the driver read, and what to hand back.
pub(crate) struct Cx {
    pub at: DateTime<Utc>,
    pub today: NaiveDate,
    pub caller: String,
    pub out: Applied,
}

impl Cx {
    pub fn event(&mut self, e: Event) {
        self.out.events.push(e);
    }
    pub fn effect(&mut self, e: Effect) {
        self.out.effects.push(e);
    }
}

pub(crate) fn reject<T>(code: &str, message: impl Into<String>) -> Result<T, Reject> {
    Err(Reject::new(code, message))
}

pub(crate) fn life_mut<'a>(p: &'a mut Proposal, life: &LifeId) -> Result<&'a mut LifeFile, Reject> {
    let id = p.id.clone();
    p.lives
        .get_mut(life)
        .ok_or_else(|| Reject::new("unknown_life", format!("{life} is not on proposal {id}")))
}

pub(crate) fn life_ref<'a>(p: &'a Proposal, life: &LifeId) -> Result<&'a LifeFile, Reject> {
    p.lives
        .get(life)
        .ok_or_else(|| Reject::new("unknown_life", format!("{life} is not on proposal {}", p.id)))
}

impl StateMachine for UnderwritingState {
    type Command = Envelope;
    type Output = Result<Applied, Reject>;

    fn apply(&mut self, index: u64, envelope: Envelope) -> Self::Output {
        if let Some(at) = self.seen.get(&envelope.key) {
            let original = self.results.get(at).cloned().unwrap_or_default();
            return Ok(Applied { duplicate: true, ..original });
        }
        if envelope.key.is_empty() {
            return reject("key", "a command needs an idempotency key");
        }
        if !envelope.command.allowed_callers().contains(&envelope.caller.as_str()) {
            return reject("caller", format!("{} may not send this command", envelope.caller));
        }
        let mut cx = Cx {
            at: envelope.at,
            today: envelope.today,
            caller: envelope.caller,
            out: Applied::default(),
        };
        self.dispatch(&mut cx, envelope.command)?;
        self.seen.insert(envelope.key, index);
        self.results.insert(index, cx.out.clone());
        while self.results.len() > RESULTS_KEPT {
            self.results.pop_first();
        }
        Ok(cx.out)
    }
}

impl UnderwritingState {
    /// Run `f` on a copy of the proposal and keep the copy only if `f`
    /// succeeds, so a refused command changes nothing.
    pub(crate) fn with<F>(&mut self, id: &ProposalId, cx: &mut Cx, f: F) -> Result<(), Reject>
    where
        F: FnOnce(&Self, &mut Proposal, &mut Cx) -> Result<(), Reject>,
    {
        let mut p = self
            .proposals
            .get(id)
            .cloned()
            .ok_or_else(|| Reject::new("unknown_proposal", format!("no proposal {id}")))?;
        f(self, &mut p, cx)?;
        self.store(p);
        Ok(())
    }

    /// As [`Self::with`], for a command that names a case.
    pub(crate) fn with_case<F>(&mut self, case: &str, cx: &mut Cx, f: F) -> Result<(), Reject>
    where
        F: FnOnce(&Self, &mut Proposal, &LifeId, &mut Cx) -> Result<(), Reject>,
    {
        let (id, life) = self
            .cases
            .get(case)
            .cloned()
            .ok_or_else(|| Reject::new("unknown_case", format!("no case {case}")))?;
        self.with(&id, cx, |st, p, cx| f(st, p, &life, cx))
    }

    /// Keep a proposal and bring every index into line with it.
    pub(crate) fn store(&mut self, p: Proposal) {
        let id = p.id.clone();
        let open = p.status.is_open();
        if open {
            self.open.insert(id.clone());
        } else {
            self.open.remove(&id);
        }
        for (life, f) in &p.lives {
            for case in &f.cases {
                self.cases.insert(case.id.clone(), (id.clone(), life.clone()));
                match (&case.owner, case.is_open()) {
                    (Some(owner), true) => self.owners.insert(case.id.clone(), owner.clone()),
                    _ => self.owners.remove(&case.id),
                };
            }
            for request in &f.evidence {
                self.evidence.insert(request.id.clone(), (id.clone(), life.clone()));
            }
            for (cover, r) in &f.capacity {
                self.reservations.insert(r.key.clone(), (id.clone(), life.clone(), cover.clone()));
            }
            for d in &f.decisions {
                self.decisions.insert(d.reference.clone(), (id.clone(), life.clone()));
            }
        }
        self.by_policy.entry(p.policy.clone()).or_default().insert(id.clone());
        if let Some(AdviserRef { adviser, fap }) = &p.adviser {
            for subject in [adviser, fap] {
                let set = self.by_adviser.entry(subject.clone()).or_default();
                if open {
                    set.insert(id.clone());
                } else {
                    set.remove(&id);
                }
            }
        }
        self.proposals.insert(id, p);
    }

    pub(crate) fn profile(&self, underwriter: &str) -> Result<&AuthorityProfile, Reject> {
        self.config
            .profiles
            .get(underwriter)
            .ok_or_else(|| Reject::new("no_profile", format!("{underwriter} has no authority profile")))
    }

    /// Whether the underwriter has declared a relationship with anyone on the proposal.
    pub(crate) fn conflicted(&self, underwriter: &str, p: &Proposal) -> bool {
        let has = |party: &PartyId| self.conflicts.contains(&(underwriter.to_string(), party.clone()));
        has(&p.policyholder) || p.lives.values().any(|f| has(&f.party))
    }

    /// An adviser may act only while they and the provider they act under
    /// are licensed. A subject the licensing feed has never mentioned is
    /// taken as licensed: the distribution module is the first gate.
    pub(crate) fn licensed(&self, adviser: &AdviserRef) -> bool {
        [&adviser.adviser, &adviser.fap]
            .iter()
            .all(|s| self.licences.get(*s).is_none_or(|l| *l == LicenceStatus::Active))
    }

    /// Open cases the underwriter owns.
    pub fn workload(&self, underwriter: &str) -> usize {
        self.owners.values().filter(|o| o.as_str() == underwriter).count()
    }

    /// Ask the authorisation policy. `subject` is one cover's terms, or the
    /// case itself for work that decides nothing.
    pub(crate) fn permits(
        &self,
        p: &Proposal,
        life: &LifeId,
        underwriter: &str,
        action: CaseAction,
        subject: &Subject<'_>,
        already_signed: bool,
    ) -> Result<bool, Reject> {
        let profile = self.profile(underwriter)?;
        let standing = Standing {
            touched: life_ref(p, life)?.underwriters().contains(underwriter),
            already_signed,
            conflict: self.conflicted(underwriter, p),
        };
        self.config
            .authority()?
            .permits(profile, action, subject, standing)
            .map_err(|e| Reject::new("policies", e.to_string()))
    }

    /// As [`Self::permits`], refusing when the answer is no.
    pub(crate) fn require(
        &self,
        p: &Proposal,
        life: &LifeId,
        underwriter: &str,
        action: CaseAction,
        subject: &Subject<'_>,
        already_signed: bool,
    ) -> Result<(), Reject> {
        if self.permits(p, life, underwriter, action, subject, already_signed)? {
            Ok(())
        } else {
            reject("not_authorised", format!("{underwriter} is not authorised for this"))
        }
    }

    pub fn proposal(&self, id: &ProposalId) -> Option<&Proposal> {
        self.proposals.get(id)
    }

    fn dispatch(&mut self, cx: &mut Cx, command: Command) -> Result<(), Reject> {
        use Command::*;
        match command {
            ActivateExclusions { library, approval } => self.activated(cx, |c, on| c.activate_exclusions(*library, approval, on)),
            ActivateReasons { library, approval } => self.activated(cx, |c, on| c.activate_reasons(*library, approval, on)),
            ActivateQuestionSet { set, approval } => self.activated(cx, |c, on| c.activate_question_set(*set, approval, on)),
            ActivateRuleSet {
                set,
                backtest_digest,
                approval,
            } => self.activated(cx, |c, on| c.activate_rule_set(*set, backtest_digest, approval, on)),
            ActivateProfile { profile, approval } => self.activated(cx, |c, on| c.activate_profile(*profile, approval, on)),
            ActivateSettings { settings, approval } => self.activated(cx, |c, on| c.activate_settings(*settings, approval, on)),
            ActivatePolicies { cedar, approval } => self.activated(cx, |c, on| c.activate_policies(cedar, approval, on)),
            ActivateDelegatedAuthority { authority, approval } => self.activated(cx, |c, on| c.activate_delegated(*authority, approval, on)),

            OpenProposal(open) => self.open_proposal(cx, *open),
            RecordConsent {
                proposal,
                life,
                kind,
                granted,
                signed,
            } => self.with(&proposal, cx, |st, p, cx| crate::intake::consent(st, p, cx, &life, kind, granted, signed)),
            PrefillExistingCover {
                proposal,
                life,
                matched,
                for_applicant,
                rows,
                reflects,
            } => self.with(&proposal, cx, |_, p, cx| {
                crate::intake::existing_cover(p, cx, &life, matched, for_applicant, rows, reflects)
            }),
            SubmitDisclosure(d) => self.with(&d.proposal.clone(), cx, |st, p, cx| crate::intake::disclosure(st, p, cx, *d)),
            RespondToOffer {
                proposal,
                life,
                offer,
                response,
                signed,
            } => self.with(&proposal, cx, |st, p, cx| crate::intake::respond(st, p, cx, &life, &offer, response, &signed)),
            Withdraw { proposal } => self.with(&proposal, cx, |_, p, cx| crate::intake::withdraw(p, cx)),
            Chaser { proposal, life, from } => self.with(&proposal, cx, |_, p, cx| crate::intake::chaser(p, cx, &life, from)),
            Requote { proposal, quote, signed } => self.with(&proposal, cx, |st, p, cx| crate::intake::requote(st, p, cx, quote, &signed)),
            AdviserLookup { proposal, adviser, purpose } => self.with(&proposal, cx, |st, p, cx| crate::intake::adviser_lookup(st, p, cx, &adviser, &purpose)),

            EvidenceAcknowledged { request, provider_ref } => self.with_evidence(&request, cx, |st, p, life, cx| {
                crate::external::acknowledged(st, p, life, cx, &request, provider_ref)
            }),
            EvidenceReceived {
                request,
                vault_ref,
                cost_minor,
            } => self.with_evidence(&request, cx, |st, p, life, cx| {
                crate::external::settled(st, p, life, cx, &request, Some((vault_ref, cost_minor)))
            }),
            EvidenceUnobtainable { request } => self.with_evidence(&request, cx, |st, p, life, cx| crate::external::settled(st, p, life, cx, &request, None)),

            DeclareConflict { underwriter, party } => self.declare_conflict(cx, underwriter, party),
            TakeCase { case, underwriter } => self.with_case(&case, cx, |st, p, life, cx| crate::casework::take(st, p, life, cx, &case, &underwriter)),
            HandOver { case, by, to } => self.with_case(&case, cx, |st, p, life, cx| crate::casework::hand_over(st, p, life, cx, &case, &by, &to)),
            MoveCase {
                case,
                underwriter,
                case_type,
                queue,
                reasons,
            } => self.with_case(&case, cx, |st, p, life, cx| {
                crate::casework::move_case(st, p, life, cx, &case, &underwriter, case_type, queue, reasons)
            }),
            RequestEvidence { case, underwriter, kinds } => self.with_case(&case, cx, |st, p, life, cx| {
                crate::casework::request_evidence(st, p, life, cx, &case, &underwriter, kinds)
            }),
            AskOpinion { case, underwriter, of } => self.with_case(&case, cx, |st, p, life, cx| {
                crate::casework::ask_opinion(st, p, life, cx, &case, &underwriter, &of)
            }),
            RecordOpinion { case, of, vault_ref } => {
                self.with_case(&case, cx, |_, p, life, cx| crate::casework::record_opinion(p, life, cx, &case, &of, vault_ref))
            }
            ScheduleInterview {
                proposal,
                life,
                scheduled_for,
                booked_by,
            } => self.with(&proposal, cx, |st, p, cx| {
                crate::casework::schedule_interview(st, p, cx, &life, scheduled_for, booked_by)
            }),
            RecordInterview {
                case,
                underwriter,
                notes_ref,
                notes_hash,
                recording_ref,
                present,
                adviser_present,
            } => self.with_case(&case, cx, |_, p, life, cx| {
                crate::casework::record_interview(p, life, cx, &case, &underwriter, notes_ref, notes_hash, recording_ref, present, adviser_present)
            }),
            ConfirmInterview(d) => self.with(&d.proposal.clone(), cx, |st, p, cx| crate::casework::confirm_interview(st, p, cx, *d)),
            ProposeDecision {
                case,
                underwriter,
                terms,
                reasons,
                withheld_reasons,
                notes_ref,
                evidence,
            } => self.with_case(&case, cx, |st, p, life, cx| {
                crate::casework::propose(st, p, life, cx, &case, &underwriter, terms, reasons, withheld_reasons, notes_ref, evidence)
            }),
            Countersign {
                case,
                underwriter,
                agree,
                reasons,
                withheld_reasons,
            } => self.with_case(&case, cx, |st, p, life, cx| {
                crate::casework::countersign(st, p, life, cx, &case, &underwriter, agree, reasons, withheld_reasons)
            }),
            ReleaseToReinsurer {
                case,
                underwriter,
                treaty,
                documents,
            } => self.with_case(&case, cx, |_, p, life, cx| {
                crate::casework::release_to_reinsurer(p, life, cx, &case, &underwriter, treaty, documents)
            }),
            RetryIssue { proposal, underwriter } => self.with(&proposal, cx, |st, p, cx| crate::external::retry_issue(st, p, cx, &underwriter)),

            CapacityAnswered { reservation, answer } => self.with_reservation(&reservation, cx, |st, p, life, cover, cx| {
                crate::external::capacity(st, p, life, cover, cx, answer)
            }),
            FacultativeTermsRecorded { reservation, outcome } => self.with_reservation(&reservation, cx, |_, p, life, cover, cx| {
                crate::external::facultative(p, life, cover, cx, outcome)
            }),

            InterimIssued { proposal } => self.with(&proposal, cx, |_, p, _| crate::external::interim_issued(p)),
            PolicyIssued { proposal } => self.with(&proposal, cx, |_, p, cx| crate::external::policy_issued(p, cx)),
            IssueRejected { proposal, code } => self.with(&proposal, cx, |_, p, cx| crate::external::issue_rejected(p, cx, code)),

            Sweep { after, max } => self.sweep(cx, after, max),
            LicenceChanged {
                subject,
                status,
                effective,
                source_ref,
            } => self.licence_changed(cx, subject, status, effective, &source_ref),

            OpenFileForClaim {
                claim_ref,
                policy,
                life,
                assessor,
            } => self.open_file_for_claim(cx, &claim_ref, &policy, &life, &assessor),
            OpenNonDisclosureReview {
                claim_ref,
                policy,
                life,
                opened_by,
            } => self.open_review(cx, claim_ref, &policy, &life, opened_by),
            RecordNonDisclosureOutcome {
                review,
                outcome,
                decided_by,
                countersigned_by,
                reasons,
                varied_terms,
            } => self.close_review(cx, &review, outcome, &decided_by, countersigned_by, reasons, varied_terms),

            DelegatedDecision(d) => self.delegated_decision(cx, *d),
            RequestDelegateFile {
                proposal,
                life,
                underwriter,
                purpose,
            } => self.with(&proposal, cx, |st, p, cx| {
                crate::special::request_delegate_file(st, p, cx, &life, &underwriter, &purpose)
            }),
            DelegateFileReceived {
                proposal,
                life,
                vault_ref,
                hash,
            } => self.with(&proposal, cx, |_, p, _| crate::special::delegate_file_received(p, &life, vault_ref, hash)),
            MigratedPolicy(m) => self.migrated_policy(cx, *m),

            DrawPeerReviewSample { period, from, to, seed } => self.draw_sample(cx, &period, from, to, seed),
            RecordPeerFinding {
                decision,
                reviewer,
                agrees,
                reasons,
            } => self.record_peer_finding(cx, &decision, &reviewer, agrees, reasons),
        }
    }

    fn activated(&mut self, cx: &mut Cx, f: impl FnOnce(&mut Config, NaiveDate) -> Result<crate::config::Activation, Reject>) -> Result<(), Reject> {
        let a = f(&mut self.config, cx.today)?;
        cx.event(Event::ConfigActivated {
            kind: a.kind,
            id: a.id,
            version: a.version,
            content_hash: a.content_hash,
            author: a.approval.author,
            approvers: a.approval.approvers,
        });
        Ok(())
    }

    fn with_evidence<F>(&mut self, request: &str, cx: &mut Cx, f: F) -> Result<(), Reject>
    where
        F: FnOnce(&Self, &mut Proposal, &LifeId, &mut Cx) -> Result<(), Reject>,
    {
        let (id, life) = self
            .evidence
            .get(request)
            .cloned()
            .ok_or_else(|| Reject::new("unknown_request", format!("no evidence request {request}")))?;
        self.with(&id, cx, |st, p, cx| f(st, p, &life, cx))
    }

    fn with_reservation<F>(&mut self, reservation: &str, cx: &mut Cx, f: F) -> Result<(), Reject>
    where
        F: FnOnce(&Self, &mut Proposal, &LifeId, &str, &mut Cx) -> Result<(), Reject>,
    {
        let (id, life, cover) = self
            .reservations
            .get(reservation)
            .cloned()
            .ok_or_else(|| Reject::new("unknown_reservation", format!("no reservation {reservation}")))?;
        let current = self
            .proposals
            .get(&id)
            .and_then(|p| p.lives.get(&life))
            .and_then(|f| f.capacity.get(&cover))
            .is_some_and(|r| r.key == reservation);
        if !current {
            return reject("stale_reservation", format!("reservation {reservation} has been released or replaced"));
        }
        self.with(&id, cx, |st, p, cx| f(st, p, &life, &cover, cx))
    }

    fn declare_conflict(&mut self, cx: &mut Cx, underwriter: String, party: PartyId) -> Result<(), Reject> {
        self.profile(&underwriter)?;
        self.conflicts.insert((underwriter.clone(), party));
        // Anything they hold on that person goes back to its queue.
        let held: Vec<String> = self.owners.iter().filter(|(_, o)| **o == underwriter).map(|(c, _)| c.clone()).collect();
        for case in held {
            let Some((id, life)) = self.cases.get(&case).cloned() else { continue };
            let Some(mut p) = self.proposals.get(&id).cloned() else { continue };
            if !self.conflicted(&underwriter, &p) {
                continue;
            }
            if let Some(c) = p.lives.get_mut(&life).and_then(|f| f.cases.iter_mut().find(|c| c.id == case)) {
                c.owner = None;
                cx.event(Event::CaseReleased {
                    case: case.clone(),
                    from: underwriter.clone(),
                });
            }
            self.store(p);
        }
        Ok(())
    }
}
