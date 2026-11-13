//! Decisions that arrive as data (delegated and migrated), the claims
//! module's way in, peer review, and the questions other modules ask.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use ic_core::{LifeId, PolicyId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::authority::{CaseAction, Subject};
use crate::case::{Case, CaseType, Queue};
use crate::codes::{reason, ExclusionLibrary};
use crate::command::{ClaimFile, DelegatedDecision, Effect, Event, MigratedPolicy, NonDisclosureOutcome, Reject, TermsOnCover, View};
use crate::flow;
use crate::machine::{life_mut, life_ref, reject, Cx, Review, UnderwritingState};
use crate::model::{
    CoverOutcome, CoverTerms, Decision, DecisionSource, DecisionStatus, DisclosureRef, LifeFile, PeerFinding, Proposal, ProposalId, ProposalStatus, Signature,
    Signing, Stage, VersionRef,
};

fn no_subject(p: &Proposal) -> Subject<'_> {
    Subject {
        product: &p.product_code,
        cover_type: "",
        sum_insured_minor: 0,
        loading_percent: 0,
        has_exclusions: false,
        outcome: "none",
    }
}

/// Why a delegate's decision is outside its authority, if it is.
fn outside_authority(st: &UnderwritingState, p: &Proposal, d: &DelegatedDecision, today: NaiveDate) -> Option<String> {
    let cfg = &st.config;
    let Some(a) = cfg.delegated_on(&d.authority, d.effective) else {
        return Some("no authority in force on the decision date".into());
    };
    if !a.products.contains(&p.product_code) {
        return Some("the product is outside the authority".into());
    }
    if d.effective > today || (today - d.effective).num_days() > cfg.settings.delegated_tolerance_days {
        return Some("the effective date is outside tolerance".into());
    }
    if d.consent_ref.is_empty() || d.decided_by.is_empty() {
        return Some("the member's consent and the deciding underwriter must be named".into());
    }
    if a.rule_set.as_ref().is_some_and(|(_, min)| d.rule_version.is_none_or(|v| v < *min)) {
        return Some("the decision was not made with the rule version the authority requires".into());
    }
    let library = cfg.exclusions_on(p.received_on)?;
    let mut accepted = 0i64;
    for t in &d.terms {
        let cover = p.cover(&d.life, &t.cover)?;
        match &t.outcome {
            CoverOutcome::Accept {
                loading_percent,
                exclusions,
                revised_sum_insured_minor,
            } => {
                let sum = revised_sum_insured_minor.unwrap_or(cover.sum_insured_minor);
                if a.per_life_minor.get(&cover.cover_type).is_none_or(|limit| sum > *limit) {
                    return Some(format!("{} is above the per-life ceiling", t.cover));
                }
                if *loading_percent > a.max_loading_percent || (!exclusions.is_empty() && !a.may_exclude) {
                    return Some(format!("the terms on {} are outside the authority", t.cover));
                }
                if exclusions.iter().any(|e| !library.contains(&e.code)) {
                    return Some(format!("an exclusion on {} is not in our code list", t.cover));
                }
                accepted += sum;
            }
            CoverOutcome::Decline if a.may_decline => {}
            _ => return Some(format!("the outcome on {} is outside the authority", t.cover)),
        }
    }
    let standing = st.delegated_total.get(&a.id).copied().unwrap_or(0);
    (standing + accepted > a.aggregate_minor).then(|| "the authority's aggregate limit would be exceeded".into())
}

pub(crate) fn request_delegate_file(
    st: &UnderwritingState,
    p: &mut Proposal,
    cx: &mut Cx,
    life: &LifeId,
    underwriter: &str,
    purpose: &str,
) -> Result<(), Reject> {
    st.profile(underwriter)?;
    if st.conflicted(underwriter, p) || purpose.is_empty() {
        return reject(
            "not_authorised",
            "a file is requested for a stated purpose by someone with no relationship to the application",
        );
    }
    let delegated = life_ref(p, life)?.decisions.iter().rev().find_map(|d| match &d.source {
        DecisionSource::Delegated { delegate_ref, .. } => Some(delegate_ref.clone()),
        _ => None,
    });
    let Some(delegate_ref) = delegated else {
        return reject("not_delegated", "no delegate decided this life");
    };
    cx.event(Event::FileAccessed {
        who: underwriter.to_string(),
        role: "underwriter".to_string(),
        purpose: purpose.to_string(),
        proposal: p.id.clone(),
        life: Some(life.clone()),
        documents: vec![delegate_ref.clone()],
    });
    cx.effect(Effect::RequestDelegateFile {
        proposal: p.id.clone(),
        life: life.clone(),
        delegate_ref,
    });
    Ok(())
}

pub(crate) fn delegate_file_received(p: &mut Proposal, life: &LifeId, vault_ref: String, hash: String) -> Result<(), Reject> {
    if vault_ref.is_empty() || hash.len() != 64 {
        return reject("disclosure", "a received file needs its SHA-256 and where it is sealed");
    }
    // Only a delegate who decided the life has a file to send for it.
    let f = life_mut(p, life)?;
    if !f.decisions.iter().any(|d| matches!(d.source, DecisionSource::Delegated { .. })) {
        return reject("not_delegated", "no delegate decided this life");
    }
    f.delegate_file = Some((vault_ref, hash));
    Ok(())
}

/// One item on a queue, for the underwriter's worklist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueItem {
    pub case: String,
    pub proposal: ProposalId,
    pub life: LifeId,
    pub case_type: CaseType,
    pub owner: Option<String>,
    pub priority: i64,
    /// Why the case is here.
    pub reasons: BTreeSet<String>,
}

/// Management figures for a period.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub decisions: u64,
    /// Decided by the rules with no person involved.
    pub straight_through: u64,
    pub by_outcome: BTreeMap<String, u64>,
    pub evidence_cost_minor: i64,
    pub peer_reviewed: u64,
    pub peer_disagreed: u64,
    /// Hours from proposal to decision, summed, for the average.
    pub turnaround_hours: i64,
}

/// A question another module or the console asks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "query", rename_all = "snake_case")]
pub enum Query {
    Proposal {
        proposal: ProposalId,
    },
    /// The rule set and question set the proposal's disclosures are made
    /// against, or null when none is in force for it.
    DisclosureBasis {
        proposal: ProposalId,
    },
    TermsOn {
        policy: PolicyId,
        life: LifeId,
        cover: String,
    },
    UnderwritersOf {
        policy: PolicyId,
        life: LifeId,
    },
    Queue {
        queue: Queue,
        now: DateTime<Utc>,
    },
    Workload {
        underwriter: String,
    },
    Report {
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    },
    Exclusions {
        on: NaiveDate,
    },
    /// Effects waiting for delivery after number `after`, oldest first.
    Outbox {
        after: u64,
        max: usize,
    },
    /// Effects set aside as undeliverable, with the reason for each.
    Parked,
    /// Proposals whose progress rows are waiting to be published.
    Progress {
        max: usize,
    },
    /// Where an evidence request belongs, for the intake filing its answer.
    EvidenceOrder {
        request: String,
    },
}

/// An evidence request, as the intake needs it to file the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceOrder {
    pub proposal: ProposalId,
    pub life: LifeId,
    pub party: ic_core::PartyId,
    pub kind: crate::evidence::EvidenceKind,
    pub outstanding: bool,
}

/// Answer a query from the state. The function the module host calls.
pub fn answer(state: &UnderwritingState, query: serde_json::Value) -> Result<serde_json::Value, String> {
    let json = |v: Result<serde_json::Value, serde_json::Error>| v.map_err(|e| e.to_string());
    match serde_json::from_value(query).map_err(|e| e.to_string())? {
        Query::Proposal { proposal } => json(serde_json::to_value(state.proposal(&proposal))),
        Query::DisclosureBasis { proposal } => json(serde_json::to_value(
            state.proposal(&proposal).and_then(|p| crate::prepare::Basis::for_proposal(&state.config, p)),
        )),
        Query::TermsOn { policy, life, cover } => json(serde_json::to_value(state.terms_on(&policy, &life, &cover))),
        Query::UnderwritersOf { policy, life } => json(serde_json::to_value(state.underwriters_of(&policy, &life))),
        Query::Queue { queue, now } => json(serde_json::to_value(state.queue(queue, now))),
        Query::Workload { underwriter } => json(serde_json::to_value(state.workload(&underwriter))),
        Query::Report { from, to } => json(serde_json::to_value(state.report(from, to))),
        Query::Exclusions { on } => json(serde_json::to_value(state.exclusions(on))),
        Query::EvidenceOrder { request } => json(serde_json::to_value(state.evidence_order(&request))),
        Query::Outbox { after, max } => json(serde_json::to_value(state.outbox(after, max))),
        Query::Parked => json(serde_json::to_value(state.parked())),
        Query::Progress { max } => json(serde_json::to_value(state.progress_pending(max))),
    }
}

impl UnderwritingState {
    pub fn evidence_order(&self, request: &str) -> Option<EvidenceOrder> {
        let (proposal, life) = self.evidence.get(request)?;
        let f = self.proposals.get(proposal)?.lives.get(life)?;
        let e = f.evidence.iter().find(|e| e.id == request)?;
        Some(EvidenceOrder {
            proposal: proposal.clone(),
            life: life.clone(),
            party: f.party.clone(),
            kind: e.kind,
            outstanding: e.outstanding(),
        })
    }
}

impl UnderwritingState {
    fn files_on<'a>(&'a self, policy: &PolicyId, life: &'a LifeId) -> impl Iterator<Item = (&'a Proposal, &'a LifeFile)> {
        self.by_policy
            .get(policy)
            .into_iter()
            .flatten()
            .filter_map(|id| self.proposals.get(id))
            .filter_map(move |p| p.lives.get(life).map(|f| (p, f)))
    }

    /// The decision in force on a cover: the newest that speaks to it.
    fn decision_on<'a>(&'a self, policy: &PolicyId, life: &'a LifeId, cover: &str) -> Option<(&'a Proposal, &'a Decision)> {
        self.files_on(policy, life)
            .filter_map(|(p, f)| f.effective_decision().filter(|d| d.terms_for(cover).is_some()).map(|d| (p, d)))
            .max_by_key(|(_, d)| d.effective_at)
    }

    /// The terms on a cover, for the claims module's cover check.
    pub fn terms_on(&self, policy: &PolicyId, life: &LifeId, cover: &str) -> Option<TermsOnCover> {
        let (_, d) = self.decision_on(policy, life, cover)?;
        let outcome = d.terms_for(cover)?;
        let (loading_percent, exclusions) = match outcome {
            CoverOutcome::Accept {
                loading_percent, exclusions, ..
            } => (*loading_percent, exclusions.clone()),
            _ => (0, vec![]),
        };
        Some(TermsOnCover {
            decision: d.reference.clone(),
            loading_percent,
            exclusions,
            unknown_terms: *outcome == CoverOutcome::UnknownTerms,
        })
    }

    /// Everyone who underwrote the life on the policy. The claims module
    /// never offers a claim on the life to any of them.
    pub fn underwriters_of(&self, policy: &PolicyId, life: &LifeId) -> BTreeSet<String> {
        self.files_on(policy, life).flat_map(|(_, f)| f.underwriters()).collect()
    }

    /// The exclusion code list in force on a date, for matching at claim.
    pub fn exclusions(&self, on: NaiveDate) -> Option<&ExclusionLibrary> {
        self.config.exclusions_on(on)
    }

    /// A queue's open cases, most urgent first.
    pub fn queue(&self, queue: Queue, now: DateTime<Utc>) -> Vec<QueueItem> {
        let mut items: Vec<QueueItem> = self
            .cases
            .iter()
            .filter_map(|(case, (id, life))| {
                let p = self.proposals.get(id)?;
                let c = p.lives.get(life)?.cases.iter().find(|c| &c.id == case && c.is_open() && c.queue == queue)?;
                Some(QueueItem {
                    case: case.clone(),
                    proposal: id.clone(),
                    life: life.clone(),
                    case_type: c.case_type,
                    owner: c.owner.clone(),
                    priority: c.priority(p.largest_sum_on(life), p.interim_in_force(), now),
                    reasons: c.reasons.clone(),
                })
            })
            .collect();
        items.sort_by(|a, b| b.priority.cmp(&a.priority).then_with(|| a.case.cmp(&b.case)));
        items
    }

    /// Management figures for decisions that took effect in `[from, to)`.
    pub fn report(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Report {
        let mut r = Report::default();
        for p in self.proposals.values() {
            for f in p.lives.values() {
                r.evidence_cost_minor += f
                    .evidence
                    .iter()
                    .filter(|e| e.requested_at >= from && e.requested_at < to)
                    .map(|e| e.cost_minor)
                    .sum::<i64>();
                for d in f
                    .decisions
                    .iter()
                    .filter(|d| d.status == DecisionStatus::Effective && d.effective_at.is_some_and(|t| t >= from && t < to))
                {
                    r.decisions += 1;
                    r.straight_through += u64::from(d.source == DecisionSource::Rules);
                    r.turnaround_hours += d.effective_at.map_or(0, |t| (t - p.received_at).num_hours().max(0));
                    r.peer_reviewed += u64::from(d.peer_review.is_some());
                    r.peer_disagreed += u64::from(d.peer_review.as_ref().is_some_and(|f| !f.agrees));
                    for t in &d.terms {
                        *r.by_outcome.entry(t.outcome.key().to_string()).or_insert(0) += 1;
                    }
                }
            }
        }
        r
    }

    /// A delegate's decision arrives as data. Inside the authority it is
    /// written as the life's decision and the capacity step runs as for our
    /// own. Outside it, nothing is bound and the life goes to our queue.
    pub(crate) fn delegated_decision(&mut self, cx: &mut Cx, d: DelegatedDecision) -> Result<(), Reject> {
        if self.delegate_refs.contains(&(d.authority.clone(), d.delegate_ref.clone())) || d.delegate_ref.is_empty() {
            return reject("duplicate", "the delegate's reference is missing or has been received before");
        }
        let mut accepted = None;
        let key = (d.authority.clone(), d.delegate_ref.clone());
        self.with(&d.proposal.clone(), cx, |st, p, cx| {
            if p.status != ProposalStatus::Open || life_ref(p, &d.life)?.stage != Stage::AwaitingDisclosure {
                return reject("not_awaiting", "the life is not waiting for a decision");
            }
            // A code our lists do not hold is not something the delegate can
            // bind us to: like a decision above the authority, it is
            // rejected with its reason and the life comes to our queue.
            let why = match crate::casework::check_terms(st, p, &d.life, &d.terms, cx) {
                Err(e) if e.code == "unknown_code" => Some(e.message),
                Err(e) => return Err(Reject::new("terms", e.message)),
                Ok(()) => outside_authority(st, p, &d, cx.today),
            };
            if let Some(why) = why {
                // Ours to decide from here: the member may now disclose to us.
                p.delegated_to = None;
                cx.event(Event::DelegatedDecisionRejected {
                    proposal: p.id.clone(),
                    life: d.life.clone(),
                    delegate_ref: d.delegate_ref.clone(),
                    why: why.clone(),
                });
                flow::open_case(
                    p,
                    &d.life,
                    CaseType::Referral,
                    Queue::Medical,
                    [reason::DELEGATED_REJECTED.to_string()].into(),
                    cx,
                )?;
                cx.out.view = Some(View::Delegated {
                    accepted: false,
                    why: Some(why),
                });
                return Ok(());
            }
            let version = st.config.delegated_on(&d.authority, d.effective).map_or(0, |a| a.version);
            let source = DecisionSource::Delegated {
                authority: d.authority.clone(),
                authority_version: version,
                delegate_ref: d.delegate_ref.clone(),
                consent_ref: d.consent_ref.clone(),
                decided_by: d.decided_by.clone(),
            };
            let f = life_ref(p, &d.life)?;
            let mut decision = flow::new_decision(&st.config, p, f, source, d.terms.clone(), [reason::DELEGATED.to_string()].into(), cx);
            decision.evidence = d.evidence.clone();
            accepted = Some(
                p.covers_on(&d.life)
                    .filter_map(|c| decision.accepted_sum(&c.code, c.sum_insured_minor))
                    .sum::<i64>(),
            );
            life_mut(p, &d.life)?.decisions.push(decision);
            cx.out.view = Some(View::Delegated { accepted: true, why: None });
            flow::after_signed(&st.config, p, &d.life, cx)
        })?;
        self.delegate_refs.insert(key);
        if let Some(sum) = accepted {
            *self.delegated_total.entry(d.authority).or_insert(0) += sum;
        }
        Ok(())
    }

    /// A policy in force in a legacy system, or taken over from another
    /// insurer: its underwriting terms are carried as data. A decision is
    /// never invented: where the original terms are unknown they are marked
    /// unknown and a person is asked, and they are never read as standard.
    pub(crate) fn migrated_policy(&mut self, cx: &mut Cx, m: MigratedPolicy) -> Result<(), Reject> {
        let open = m.open;
        if self.proposals.contains_key(&open.proposal) {
            return reject("duplicate_proposal", format!("proposal {} already exists", open.proposal));
        }
        let Some(library) = self.config.exclusions_on(cx.today) else {
            return reject("no_rules", "the exclusion list is not active");
        };
        if m.legacy_ref.is_empty() || m.original_decision_date > cx.today || open.lives.len() != m.lives.len() || open.lives.is_empty() {
            return reject(
                "migration",
                "a migrated policy names its legacy record, a past decision date and terms for every life",
            );
        }
        let made_at = m.original_decision_date.and_time(NaiveTime::MIN).and_utc();
        let mut p = Proposal {
            id: open.proposal,
            entry: open.entry,
            policy: open.policy,
            policy_number: open.policy_number,
            group: open.group,
            policyholder: open.policyholder,
            product_code: open.product_code,
            frequency: open.frequency,
            channel: open.channel,
            adviser: open.adviser,
            quote: open.quote,
            // The contract date decides which disclosure regime applies at claim.
            received_on: m.commencement,
            received_at: made_at,
            expires_on: m.commencement,
            status: ProposalStatus::Issued { at: cx.at },
            lives: BTreeMap::new(),
            covers: open.covers,
            interim: None,
            migrated: true,
            issue_attempts: 0,
            reoffers: vec![],
            replacement: false,
            delegated_to: None,
        };
        for l in open.lives {
            let Some(legacy) = m.lives.iter().find(|x| x.life == l.life) else {
                return reject("migration", format!("no terms were supplied for {}", l.life));
            };
            let covers: BTreeSet<&str> = p.covers_on(&l.life).map(|c| c.code.as_str()).collect();
            if covers.is_empty() || covers != legacy.terms.iter().map(|t| t.cover.as_str()).collect() || covers.len() != legacy.terms.len() {
                return reject("terms", format!("the terms for {} do not match its covers", l.life));
            }
            // A legacy clause with no code of ours is carried word for word
            // under a verbatim code: it is never dropped or guessed at.
            let coded = |t: &CoverTerms| match &t.outcome {
                CoverOutcome::Accept { exclusions, .. } => exclusions.iter().all(|e| library.contains(&e.code)),
                _ => true,
            };
            if !legacy.terms.iter().all(coded) {
                return reject("unknown_code", "a migrated exclusion is not in the code list");
            }
            let unknown = legacy.terms.iter().any(|t| t.outcome == CoverOutcome::UnknownTerms);
            let why = if unknown { reason::MIGRATED_UNKNOWN_TERMS } else { reason::MIGRATED };
            let mut f = LifeFile::new(l.life.clone(), l.party, l.factors, Stage::Accepted);
            if let (Some(vault_ref), Some(hash)) = (&legacy.file_ref, &legacy.file_hash) {
                f.disclosures.push(DisclosureRef {
                    hash: hash.clone(),
                    vault_ref: vault_ref.clone(),
                    question_set: VersionRef {
                        id: "legacy".to_string(),
                        version: 0,
                        content_hash: String::new(),
                    },
                    submitted_at: made_at,
                    evidence_of_signing: Signing::Migrated {
                        legacy_ref: m.legacy_ref.clone(),
                    },
                    entered_by_adviser: 0,
                    // A legacy file's answers are not read here.
                    risk_tags: BTreeSet::new(),
                });
            }
            let source = DecisionSource::Migrated {
                legacy_ref: m.legacy_ref.clone(),
                legacy_underwriter: legacy.legacy_underwriter.clone(),
            };
            let mut decision = flow::new_decision(&self.config, &p, &f, source, legacy.terms.clone(), [why.to_string()].into(), cx);
            decision.status = DecisionStatus::Effective;
            decision.made_at = made_at;
            decision.effective_at = Some(made_at);
            decision.versions.rule_set = None;
            cx.event(Event::DecisionMade {
                proposal: p.id.clone(),
                life: l.life.clone(),
                decision: decision.reference.clone(),
            });
            f.decisions.push(decision);
            if unknown {
                let id = format!("{}/{}/case/1", p.id, l.life);
                f.cases
                    .push(Case::open(id.clone(), CaseType::Referral, Queue::SeniorReview, [why.to_string()].into(), cx.at));
                cx.event(Event::Referred {
                    proposal: p.id.clone(),
                    life: l.life.clone(),
                    case: id,
                    case_type: CaseType::Referral,
                    queue: Queue::SeniorReview,
                });
            }
            p.lives.insert(l.life, f);
        }
        self.store(p);
        Ok(())
    }

    /// Open the underwriting file to a claims assessor: the decision, its
    /// terms and versions, and where the disclosures are. Logged with the
    /// claim as its purpose. Never to someone who underwrote the life.
    /// An underwriter reads a life's disclosures: someone accredited for the
    /// product with no relationship to anyone on it, who holds or has worked
    /// a case on the life, or a senior. The access is an event before any
    /// record is read.
    pub(crate) fn open_disclosures(&mut self, cx: &mut Cx, proposal: &ProposalId, life: &LifeId, underwriter: &str, purpose: &str) -> Result<(), Reject> {
        if purpose.trim().is_empty() {
            return reject("purpose", "a disclosure is opened for a stated purpose");
        }
        let p = self
            .proposals
            .get(proposal)
            .ok_or_else(|| Reject::new("unknown_proposal", format!("no proposal {proposal}")))?;
        self.require(p, life, underwriter, CaseAction::Work, &no_subject(p), false)?;
        let f = life_ref(p, life)?;
        let on_it = f
            .cases
            .iter()
            .any(|c| c.owner.as_deref() == Some(underwriter) || c.touched_by.contains(underwriter));
        if !on_it && !self.profile(underwriter)?.senior {
            return reject("not_authorised", format!("{underwriter} has no case on this life"));
        }
        if f.disclosures.is_empty() {
            return reject("no_disclosure", "the life has given no disclosure");
        }
        cx.event(Event::FileAccessed {
            who: underwriter.to_string(),
            role: "underwriter".to_string(),
            purpose: purpose.to_string(),
            proposal: proposal.clone(),
            life: Some(life.clone()),
            documents: f.disclosures.iter().map(|d| d.vault_ref.clone()).collect(),
        });
        cx.out.view = Some(View::Disclosures(f.disclosures.clone()));
        Ok(())
    }

    pub(crate) fn open_file_for_claim(&mut self, cx: &mut Cx, claim_ref: &str, policy: &PolicyId, life: &LifeId, assessor: &str) -> Result<(), Reject> {
        if claim_ref.is_empty() || assessor.is_empty() {
            return reject("purpose", "a file is opened for a named claim by a named assessor");
        }
        if self.underwriters_of(policy, life).contains(assessor) {
            return reject(
                "separation_of_duties",
                format!("{assessor} underwrote this life and cannot assess a claim on it"),
            );
        }
        let mut files = Vec::new();
        for (p, f) in self.files_on(policy, life) {
            let Some(d) = f.effective_decision() else { continue };
            let disclosures: Vec<_> = f
                .disclosures
                .iter()
                .map(|x| (x.vault_ref.clone(), x.hash.clone(), matches!(x.evidence_of_signing, Signing::Migrated { .. })))
                .collect();
            cx.event(Event::FileAccessed {
                who: assessor.to_string(),
                role: "claims_assessor".to_string(),
                purpose: format!("claim {claim_ref}"),
                proposal: p.id.clone(),
                life: Some(life.clone()),
                documents: disclosures.iter().map(|x| x.0.clone()).collect(),
            });
            files.push(ClaimFile {
                proposal: p.id.clone(),
                decision: d.reference.clone(),
                terms: d.terms.clone(),
                versions: d.versions.clone(),
                disclosures,
                underwriters: f.underwriters(),
            });
        }
        if files.is_empty() {
            return reject("no_file", "there is no underwriting decision on this life for this policy");
        }
        cx.out.view = Some(View::ClaimFiles(files));
        Ok(())
    }

    /// Open a review of the disclosure behind the decision in force. It is a
    /// case of its own, for a senior who never touched the life.
    pub(crate) fn open_review(&mut self, cx: &mut Cx, claim_ref: String, policy: &PolicyId, life: &LifeId, opened_by: String) -> Result<(), Reject> {
        let id = format!("ndr/{claim_ref}/{life}");
        if claim_ref.is_empty() || opened_by.is_empty() || self.reviews.contains_key(&id) {
            return reject(
                "duplicate",
                "a review needs its claim and who opened it, and is opened once for a claim and life",
            );
        }
        let newest = self
            .files_on(policy, life)
            .filter_map(|(p, f)| f.effective_decision().map(|d| (p, d)))
            .max_by_key(|(_, d)| d.effective_at);
        let Some((p, d)) = newest else {
            return reject("no_file", "there is no underwriting decision on this life for this policy");
        };
        let regime = d
            .versions
            .question_set
            .as_ref()
            .and_then(|q| self.config.question_set(q))
            .map_or_else(|| self.config.regime_on(p.received_on), |q| q.regime);
        let review = Review {
            claim_ref: claim_ref.clone(),
            proposal: p.id.clone(),
            life: life.clone(),
            decision: d.reference.clone(),
            opened_by,
            closed: false,
        };
        let mut p = p.clone();
        life_mut(&mut p, life)?.cases.push(Case::open(
            id.clone(),
            CaseType::NonDisclosureReview,
            Queue::SeniorReview,
            [reason::NON_DISCLOSURE_REVIEW.to_string()].into(),
            cx.at,
        ));
        self.store(p);
        self.reviews.insert(id.clone(), review);
        cx.event(Event::NonDisclosureReviewOpened { review: id.clone(), claim_ref });
        cx.out.view = Some(View::Review { review: id, regime });
        Ok(())
    }

    /// Close a review. A remedy needs a second senior and is a command to
    /// the core in its own right; the decision is never edited.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn close_review(
        &mut self,
        cx: &mut Cx,
        id: &str,
        outcome: NonDisclosureOutcome,
        decided_by: &str,
        countersigned_by: Option<String>,
        reasons: BTreeSet<String>,
        varied_terms: Vec<CoverTerms>,
        premium: Option<crate::command::PremiumOnAvoidance>,
    ) -> Result<(), Reject> {
        let Some(review) = self.reviews.get(id).filter(|r| !r.closed).cloned() else {
            return reject("unknown_review", format!("no open review {id}"));
        };
        let life = review.life.clone();
        let mut result = review.decision.clone();
        self.with(&review.proposal, cx, |st, p, cx| {
            let f = life_ref(p, &life)?;
            if !f.cases.iter().any(|c| c.id == id && c.is_open() && c.owner.as_deref() == Some(decided_by)) {
                return reject("not_owner", format!("{decided_by} does not hold the review"));
            }
            let library = st.config.reasons_on(cx.today).or_else(|| st.config.reasons_on(p.received_on));
            if reasons.is_empty() || library.is_none_or(|l| l.first_unknown(&reasons).is_some()) {
                return reject("reasons", "the outcome of a review records reasons from the list");
            }
            let mut signers = vec![decided_by.to_string()];
            if outcome != NonDisclosureOutcome::NoAction {
                let Some(second) = countersigned_by.as_deref().filter(|s| *s != decided_by) else {
                    return reject("countersign_required", "a remedy for non-disclosure needs a second senior");
                };
                st.require(p, &life, second, CaseAction::ReviewNonDisclosure, &no_subject(p), false)?;
                signers.push(second.to_string());
            }
            let varying = outcome == NonDisclosureOutcome::Vary;
            if varying {
                crate::casework::check_terms(st, p, &life, &varied_terms, cx)?;
            }
            if varying != !varied_terms.is_empty() || (varying && !varied_terms.iter().all(|t| t.outcome.is_accept())) {
                return reject("terms", "a variation carries the terms that would have been offered, and nothing else does");
            }
            if (outcome == NonDisclosureOutcome::Avoid) != premium.is_some() {
                return reject(
                    "premium",
                    "an avoidance records whether the premiums paid are refunded or kept, and nothing else does",
                );
            }
            let mut terms = Vec::new();
            for t in &varied_terms {
                if let CoverOutcome::Accept {
                    loading_percent, exclusions, ..
                } = &t.outcome
                {
                    terms.push(ic_core::policy::LayerTerms {
                        cover: t.cover.clone(),
                        life: life.clone(),
                        loadings: if *loading_percent > 0 {
                            vec![flow::loading(*loading_percent)?]
                        } else {
                            vec![]
                        },
                        exclusions: exclusions.iter().map(|e| e.code.clone()).collect(),
                    });
                }
            }
            let mut signatures = Vec::new();
            for s in &signers {
                signatures.push(Signature {
                    underwriter: s.clone(),
                    profile_version: st.profile(s)?.version,
                    reasons: reasons.clone(),
                    withheld_reasons: BTreeSet::new(),
                    at: cx.at,
                });
            }
            let varied = varying.then(|| {
                let mut d = flow::new_decision(&st.config, p, f, DecisionSource::Underwriter, varied_terms.clone(), BTreeSet::new(), cx);
                d.status = DecisionStatus::Effective;
                d.effective_at = Some(cx.at);
                d.signatures = signatures;
                d.non_disclosure = Some(id.to_string());
                d
            });
            let f = life_mut(p, &life)?;
            if let Some(c) = f.cases.iter_mut().find(|c| c.id == id) {
                c.close(cx.at);
                cx.event(Event::CaseClosed { case: id.to_string() });
            }
            if let Some(d) = f.decisions.iter_mut().find(|d| d.reference == review.decision) {
                d.non_disclosure = Some(id.to_string());
                if varying {
                    d.status = DecisionStatus::Superseded;
                }
            }
            if let Some(d) = varied {
                result = d.reference.clone();
                f.decisions.push(d);
            }
            if outcome != NonDisclosureOutcome::NoAction {
                cx.effect(Effect::NonDisclosureRemedy {
                    key: format!("{id}/remedy"),
                    proposal: Some(p.id.clone()),
                    policy: p.policy.clone(),
                    life: life.clone(),
                    outcome,
                    review: id.to_string(),
                    decision: varying.then(|| result.clone()),
                    terms,
                    premium,
                });
            }
            Ok(())
        })?;
        if let Some(r) = self.reviews.get_mut(id) {
            r.closed = true;
        }
        cx.event(Event::NonDisclosureReviewClosed {
            review: id.to_string(),
            claim_ref: review.claim_ref,
            outcome,
            decision: result.clone(),
        });
        cx.out.view = Some(View::NonDisclosure { decision: result });
        Ok(())
    }

    /// Draw the period's peer review sample: weighted to declines and
    /// non-standard terms, and repeatable from the seed. Only decisions that
    /// took effect in the period are read, through `effective`.
    pub(crate) fn draw_sample(&mut self, cx: &mut Cx, period: &str, from: DateTime<Utc>, to: DateTime<Utc>, seed: u64) -> Result<(), Reject> {
        if period.is_empty() || to <= from {
            return reject("period", "a sample is drawn for a named period with a start before its end");
        }
        if !self.effective_indexed {
            for (at, reference) in self
                .proposals
                .values()
                .flat_map(|p| p.lives.values().flat_map(|f| f.decisions.iter()))
                .filter_map(|d| Some((d.effective_at?, d.reference.clone())))
                .collect::<Vec<_>>()
            {
                self.effective.entry(at).or_default().insert(reference);
            }
            self.effective_indexed = true;
        }
        let rates = self.config.settings.sample.clone();
        let mut drawn = Vec::new();
        let candidates: Vec<String> = self.effective.range(from..to).flat_map(|(_, refs)| refs.iter().cloned()).collect();
        for reference in candidates {
            let Some((id, life)) = self.decisions.get(&reference) else {
                continue;
            };
            let Some(d) = self
                .proposals
                .get_mut(id)
                .and_then(|p| p.lives.get_mut(life))
                .and_then(|f| f.decisions.iter_mut().find(|d| d.reference == reference))
            else {
                continue;
            };
            if d.status != DecisionStatus::Effective || d.sampled_for.is_some() || matches!(d.source, DecisionSource::Migrated { .. }) {
                continue;
            }
            let rate = if d.any_decline() {
                rates.decline_per_mille
            } else if d.all_standard() {
                rates.standard_per_mille
            } else {
                rates.non_standard_per_mille
            };
            let digest = Sha256::digest(format!("{seed}|{}", d.reference).as_bytes());
            let roll = u64::from_be_bytes([digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6], digest[7]]) % 1000;
            if roll < u64::from(rate) {
                d.sampled_for = Some(period.to_string());
                drawn.push(reference);
            }
        }
        drawn.sort();
        for decision in &drawn {
            cx.event(Event::PeerReviewSampled {
                decision: decision.clone(),
                period: period.to_string(),
            });
        }
        cx.out.view = Some(View::Sample { decisions: drawn });
        Ok(())
    }

    /// A senior who never touched the case records what they found.
    pub(crate) fn record_peer_finding(&mut self, cx: &mut Cx, decision: &str, reviewer: &str, agrees: bool, reasons: BTreeSet<String>) -> Result<(), Reject> {
        let Some((id, life)) = self.decisions.get(decision).cloned() else {
            return reject("unknown_decision", format!("no decision {decision}"));
        };
        self.with(&id, cx, |st, p, cx| {
            st.require(p, &life, reviewer, CaseAction::PeerReview, &no_subject(p), false)?;
            let library = st.config.reasons_on(cx.today).or_else(|| st.config.reasons_on(p.received_on));
            if (!agrees && reasons.is_empty()) || library.is_none_or(|l| l.first_unknown(&reasons).is_some()) {
                return reject("reasons", "a disagreement records reasons from the list");
            }
            let Some(d) = life_mut(p, &life)?
                .decisions
                .iter_mut()
                .find(|d| d.reference == decision && d.sampled_for.is_some() && d.peer_review.is_none())
            else {
                return reject("not_sampled", "the decision is not waiting for peer review");
            };
            d.peer_review = Some(PeerFinding {
                reviewer: reviewer.to_string(),
                agrees,
                reasons,
                at: cx.at,
            });
            cx.event(Event::PeerReviewRecorded {
                decision: decision.to_string(),
                agrees,
            });
            Ok(())
        })
    }
}

/// A scheme member's excess under the broker's delegated authority goes to
/// the delegate to decide, when the authority covers the product and the
/// amount is within its ceiling for the cover. Anything else is ours to
/// underwrite, as for any excess.
pub(crate) fn ask_delegate(cfg: &crate::config::Config, p: &mut Proposal, cx: &mut Cx) {
    let crate::model::Entry::GroupExcess {
        delegated_authority: Some(authority),
        takeover_accepted_minor,
        ..
    } = &p.entry
    else {
        return;
    };
    let Some(a) = cfg.delegated_on(authority, cx.today) else { return };
    let within = a.products.contains(&p.product_code)
        && p.covers
            .iter()
            .all(|c| a.per_life_minor.get(&c.cover_type).is_some_and(|limit| c.sum_insured_minor <= *limit));
    // A taken-over excess already decided as the proposal opened needs no one.
    let decided = takeover_accepted_minor.is_some() && p.lives.values().all(|f| !f.decisions.is_empty());
    if !within || decided {
        return;
    }
    let authority = authority.clone();
    for c in &p.covers {
        let Some(f) = p.lives.get(&c.life) else { continue };
        cx.effect(Effect::RequestDelegatedDecision {
            proposal: p.id.clone(),
            life: c.life.clone(),
            party: f.party.clone(),
            cover: c.code.clone(),
            cover_type: c.cover_type.clone(),
            sum_insured_minor: c.sum_insured_minor,
            authority: authority.clone(),
        });
    }
    p.delegated_to = Some(authority);
}
