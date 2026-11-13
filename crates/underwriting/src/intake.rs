//! The applicant's side: opening a proposal, consent, disclosure, the
//! response to an offer, and what the adviser may see.

use std::collections::{BTreeMap, BTreeSet};

use ic_core::LifeId;

use crate::case::{CaseType, ChaserFrom, Queue};
use crate::codes::{reason, COVER_TYPES};
use crate::command::{AdviserLifeView, Effect, Event, NoticeKind, OfferResponse, OpenProposal, Recipient, Reject, SigningProof, SubmitDisclosure, View};
use crate::flow;
use crate::machine::{life_mut, life_ref, reject, Cx, UnderwritingState};
use crate::model::{
    AccessEnd, ConsentKind, DisclosureRef, ExistingCover, ExistingCoverRead, LifeFile, OfferStatus, Proposal, ProposalStatus, QuoteBinding, Signing, Stage,
    VersionRef,
};
use crate::signing::{message, SignedStep};

fn open_only(p: &Proposal) -> Result<(), Reject> {
    if p.status == ProposalStatus::Open {
        Ok(())
    } else {
        reject("closed", format!("proposal {} is {}", p.id, p.status.key()))
    }
}

impl UnderwritingState {
    pub(crate) fn open_proposal(&mut self, cx: &mut Cx, open: OpenProposal) -> Result<(), Reject> {
        if self.proposals.contains_key(&open.proposal) {
            return reject("duplicate_proposal", format!("proposal {} already exists", open.proposal));
        }
        let migration = cx.caller == "migration";
        if open.legacy_received_on.is_some() != migration {
            return reject("received_on", "only a migrated proposal carries the date it was first received, and it must");
        }
        let group_excess = matches!(
            open.entry,
            crate::model::Entry::GroupExcess { .. } | crate::model::Entry::GroupContinuation { .. }
        );
        let continuation = matches!(open.entry, crate::model::Entry::GroupContinuation { .. });
        if (cx.caller == "group-admin" && !group_excess) || (continuation && cx.caller != "group-admin") {
            return reject(
                "entry",
                "group administration opens a proposal only for a member's cover above the automatic acceptance limit, or its continuation, and nobody else opens a continuation",
            );
        }
        if let crate::model::Entry::GroupContinuation { inception, .. } = &open.entry {
            if *inception > cx.today {
                return reject("continuation", "a continuation starts no later than it is taken up");
            }
        }
        if let crate::model::Entry::GroupExcess {
            takeover_accepted_minor,
            reinsurance_reference,
            ..
        } = &open.entry
        {
            if takeover_accepted_minor.is_some_and(|m| m <= 0) || reinsurance_reference.as_ref().is_some_and(|r| r.trim().is_empty()) {
                return reject("takeover", "a takeover amount is above zero and a reinsurance reference is not blank");
            }
        }
        // Collections asks only when the core refused a reinstatement for
        // want of underwriting's decision.
        if cx.caller == "collections" && !matches!(open.entry, crate::model::Entry::Reinstatement { .. }) {
            return reject("entry", "collections opens a proposal only to reinstate a lapsed policy");
        }
        // A replacement keeps the cover in force and adds to it: it is more
        // cover on that policy, and it is advice, so an adviser gives it.
        if open.replacement && (!matches!(open.entry, crate::model::Entry::Increase { .. }) || open.adviser.is_none()) {
            return reject("replacement", "a replacement is advised more cover on a policy in force");
        }
        let continued = crate::continuation::earlier_terms(self, &open)?;
        if let crate::model::Entry::GroupExcess {
            delegated_authority: Some(authority),
            ..
        } = &open.entry
        {
            if self.config.delegated_on(authority, cx.today).is_none() {
                return reject("authority", format!("no delegated authority {authority} is in force"));
            }
        }
        let lives: BTreeSet<&LifeId> = open.lives.iter().map(|l| &l.life).collect();
        if open.lives.is_empty() || lives.len() != open.lives.len() {
            return reject("lives", "a proposal needs at least one life, each named once");
        }
        let covers: BTreeSet<(&LifeId, &str)> = open.covers.iter().map(|c| (&c.life, c.code.as_str())).collect();
        let sound = |c: &crate::model::ProposedCover| lives.contains(&c.life) && c.sum_insured_minor > 0 && COVER_TYPES.contains(&c.cover_type.as_str());
        if open.covers.is_empty()
            || covers.len() != open.covers.len()
            || !open.covers.iter().all(sound)
            || !lives.iter().all(|l| open.covers.iter().any(|c| &c.life == *l))
        {
            return reject(
                "covers",
                "every life needs cover, and every cover a life on the proposal, a known type and an amount",
            );
        }
        // An adviser whose licence is suspended can start nothing.
        if open.adviser.as_ref().is_some_and(|a| !self.licensed(a)) {
            return reject("adviser_not_licensed", "the adviser, or the provider they act under, is not licensed");
        }
        let received_on = open.legacy_received_on.unwrap_or(cx.today);
        if self.config.rule_set_on(&open.product_code, received_on).is_none()
            || self.config.exclusions_on(received_on).is_none()
            || self.config.reasons_on(received_on).is_none()
        {
            return reject(
                "no_rules",
                format!("no underwriting rules are active for {} on {received_on}", open.product_code),
            );
        }
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
            received_on,
            received_at: cx.at,
            expires_on: received_on + chrono::Duration::days(self.config.settings.proposal_days),
            status: ProposalStatus::Open,
            lives: open
                .lives
                .into_iter()
                .map(|l| (l.life.clone(), LifeFile::new(l.life, l.party, l.factors, Stage::AwaitingDisclosure)))
                .collect(),
            covers: open.covers,
            interim: None,
            migrated: migration,
            issue_attempts: 0,
            reoffers: vec![],
            replacement: open.replacement,
            delegated_to: None,
        };
        cx.event(Event::ProposalReceived {
            proposal: p.id.clone(),
            product_code: p.product_code.clone(),
            channel: p.channel.clone(),
            lives: p.lives.len() as u32,
            expires_on: p.expires_on,
        });
        // Interim cover is for someone with no cover yet, and a migrated
        // proposal's interim cover came across with it.
        if p.entry == crate::model::Entry::NewBusiness && !migration {
            flow::interim(&self.config, &mut p, cx);
        }
        flow::takeover(&self.config, &mut p, cx)?;
        crate::special::ask_delegate(&self.config, &mut p, cx);
        if let Some(terms) = continued {
            crate::continuation::decide(self, &mut p, terms, cx)?;
        }
        self.store(p);
        Ok(())
    }
}

pub(crate) fn consent(
    st: &UnderwritingState,
    p: &mut Proposal,
    cx: &mut Cx,
    life: &LifeId,
    kind: ConsentKind,
    granted: bool,
    signed: Option<SignedStep>,
) -> Result<(), Reject> {
    open_only(p)?;
    let proposal = p.id.clone();
    if !granted {
        if kind == ConsentKind::ReplacementDisclosure && life_ref(p, life)?.stage != Stage::AwaitingDisclosure {
            return reject("too_late", "the application was made on the replacement disclosure");
        }
        if kind == ConsentKind::AdviserAccess {
            flow::end_adviser_access(p, life, AccessEnd::ConsentWithdrawn, cx);
        }
        life_mut(p, life)?.consents.remove(&kind);
    } else {
        if kind == ConsentKind::AdviserAccess {
            // Consent is to this adviser, for this application, given with a
            // signed step, and only while there are answers still to help with.
            let Some(adviser) = &p.adviser else {
                return reject("no_adviser", "there is no adviser on the proposal");
            };
            if !st.licensed(adviser) {
                return reject("adviser_not_licensed", "the adviser, or the provider they act under, is not licensed");
            }
            if life_ref(p, life)?.effective_decision().is_some() {
                return reject("decided", "the decision has been made: there is nothing left to share");
            }
            let msg = message("consent", &[&proposal.0, &life.0, "adviser_access"]);
            if !signed.as_ref().is_some_and(|s| s.verifies(&msg)) {
                return reject("signature", "consent to adviser access needs the life's signed step");
            }
        }
        if kind == ConsentKind::EmployerSeesLoading {
            // Only a scheme member's own signed step lets the employer see it.
            if !matches!(p.entry, crate::model::Entry::GroupExcess { .. }) {
                return reject("entry", "only a scheme member's excess cover is billed through an employer");
            }
            let msg = message("consent", &[&proposal.0, &life.0, "employer_sees_loading"]);
            if !signed.as_ref().is_some_and(|s| s.verifies(&msg)) {
                return reject("signature", "consent to the employer seeing a loading needs the member's signed step");
            }
        }
        if kind == ConsentKind::ReplacementDisclosure {
            if !p.replacement {
                return reject("replacement", "the proposal is not a replacement");
            }
            let msg = message("consent", &[&proposal.0, &life.0, "replacement_disclosure"]);
            if !signed.as_ref().is_some_and(|s| s.verifies(&msg)) {
                return reject("signature", "the replacement disclosure is confirmed with the life's signed step");
            }
        }
        let f = life_mut(p, life)?;
        f.consents.insert(kind, cx.at);
        if kind == ConsentKind::AdviserAccess {
            f.adviser_access_ended = None;
        }
    }
    cx.event(Event::ConsentRecorded {
        proposal,
        life: life.clone(),
        kind,
        granted,
    });
    Ok(())
}

pub(crate) fn existing_cover(
    p: &mut Proposal,
    cx: &mut Cx,
    life: &LifeId,
    matched: bool,
    for_applicant: bool,
    rows: Vec<ExistingCover>,
    reflects: BTreeMap<u16, u64>,
) -> Result<(), Reject> {
    open_only(p)?;
    let proposal = p.id.clone();
    let f = life_mut(p, life)?;
    if f.stage != Stage::AwaitingDisclosure {
        return reject("too_late", "the disclosure has been given");
    }
    // Only a person matched in the customer master: a near match would show
    // one person another person's cover.
    if !matched {
        return reject("not_matched", "the person is not matched in the customer master");
    }
    if for_applicant && !f.consented(ConsentKind::ExistingCoverPrefill) {
        return reject("consent_required", "the life has not consented to the existing-cover question being filled in");
    }
    if rows.iter().any(|r| r.sum_insured_minor < 0) {
        return reject("rows", "an amount of existing cover is negative");
    }
    f.existing_cover = Some(ExistingCoverRead {
        rows,
        reflects,
        read_at: cx.at,
        prefilled: for_applicant || f.existing_cover.as_ref().is_some_and(|e| e.prefilled),
    });
    if for_applicant {
        cx.event(Event::ExistingCoverPrefilled { proposal, life: life.clone() });
    }
    Ok(())
}

/// Check a disclosure and put it on the life's file. `domain` separates a
/// form's signature from an interview confirmation's.
pub(crate) fn accept_disclosure(st: &UnderwritingState, p: &mut Proposal, cx: &mut Cx, d: &SubmitDisclosure, domain: &str) -> Result<(), Reject> {
    open_only(p)?;
    let cfg = &st.config;
    let known = |r: &VersionRef, id: &str, version: u32, hash: &str| r.id == id && r.version == version && r.content_hash == hash;
    if !cfg
        .question_set_on(&d.question_set.id, p.received_on)
        .is_some_and(|q| known(&d.question_set, &q.id, q.version, &q.content_hash))
    {
        return reject("question_set", "the disclosure does not answer the question set in force for the contract date");
    }
    if !cfg
        .rule_set_on(&p.product_code, p.received_on)
        .is_some_and(|r| known(&d.rule_set, &r.id, r.version, &r.content_hash) && r.question_set == d.question_set.id)
    {
        return reject("rule_set", "the assessment was not made with the rule set in force for the contract date");
    }
    if d.hash.len() != 64 || d.vault_ref.is_empty() {
        return reject("disclosure", "a disclosure needs its SHA-256 and where it is sealed");
    }
    let evidence_of_signing = match &d.signing {
        SigningProof::Device(s) => {
            if !s.verifies(&message(domain, &[&p.id.0, &d.life.0, &d.hash])) {
                return reject("signature", "the disclosure is not signed by the life's device key");
            }
            Signing::DeviceKey { key_id: s.key_id.clone() }
        }
        SigningProof::Migrated { legacy_ref } => {
            if !(p.migrated && cx.caller == "migration") || legacy_ref.is_empty() {
                return reject("signature", "only a migrated disclosure may stand on legacy evidence of signing");
            }
            Signing::Migrated {
                legacy_ref: legacy_ref.clone(),
            }
        }
    };
    let covers: BTreeSet<&str> = p.covers_on(&d.life).map(|c| c.code.as_str()).collect();
    if covers != d.assessments.keys().map(String::as_str).collect() {
        return reject("assessments", "there must be one assessment for each cover on the life");
    }
    let (Some(reasons), Some(exclusions)) = (cfg.reasons_on(p.received_on), cfg.exclusions_on(p.received_on)) else {
        return reject("no_rules", "the code lists are not active");
    };
    if d.assessments
        .values()
        .any(|a| reasons.first_unknown(&a.reasons).is_some() || a.exclusions.iter().any(|c| !exclusions.contains(c)))
    {
        return reject("unknown_code", "an assessment names a code that is not in the lists");
    }
    if let Some(tag) = d.risk_tags.iter().find(|t| !crate::questions::is_risk_tag(t)) {
        return reject("risk_tags", format!("{tag} is not a pursuit, occupation or travel tag"));
    }
    let adviser = p.adviser.is_some();
    let f = life_mut(p, &d.life)?;
    // An answer the adviser typed is marked, and is allowed only with the
    // life's consent to the adviser helping.
    if d.entered_by_adviser > 0 && !(adviser && f.consented(ConsentKind::AdviserAccess) && f.adviser_access_ended.is_none()) {
        return reject("adviser_not_permitted", "the life has not consented to the adviser entering answers");
    }
    f.disclosures.push(DisclosureRef {
        hash: d.hash.clone(),
        vault_ref: d.vault_ref.clone(),
        question_set: d.question_set.clone(),
        submitted_at: cx.at,
        evidence_of_signing,
        entered_by_adviser: d.entered_by_adviser,
        risk_tags: d.risk_tags.clone(),
    });
    f.assessments = d.assessments.clone();
    f.rule_set = Some(d.rule_set.clone());
    cx.event(Event::DisclosureSubmitted {
        proposal: p.id.clone(),
        life: d.life.clone(),
        hash: d.hash.clone(),
    });
    Ok(())
}

pub(crate) fn disclosure(st: &UnderwritingState, p: &mut Proposal, cx: &mut Cx, d: SubmitDisclosure) -> Result<(), Reject> {
    if life_ref(p, &d.life)?.stage != Stage::AwaitingDisclosure {
        return reject("not_awaiting_disclosure", "the life's disclosure is not awaited");
    }
    if p.delegated_to.is_some() {
        return reject("delegated", "the scheme's delegate decides this member's cover");
    }
    if p.replacement && !life_ref(p, &d.life)?.consented(ConsentKind::ReplacementDisclosure) {
        return reject("replacement_disclosure", "the life has not confirmed the replacement disclosure");
    }
    accept_disclosure(st, p, cx, &d, "disclosure")?;
    flow::route(&st.config, p, &d.life, cx, false)
}

pub(crate) fn respond(
    st: &UnderwritingState,
    p: &mut Proposal,
    cx: &mut Cx,
    life: &LifeId,
    offer: &str,
    response: OfferResponse,
    signed: &SignedStep,
) -> Result<(), Reject> {
    if crate::reoffer::is_reoffer(p, offer) {
        return crate::reoffer::respond(p, cx, life, offer, response, signed);
    }
    open_only(p)?;
    let proposal = p.id.clone();
    let f = life_mut(p, life)?;
    let Some(o) = f.offers.iter_mut().find(|o| o.id == offer && o.status == OfferStatus::Open) else {
        return reject("no_offer", format!("offer {offer} is not open"));
    };
    if !signed.verifies(&message("offer", &[&proposal.0, &life.0, offer, response.key()])) {
        return reject("signature", "the response is not signed by the life's device key");
    }
    cx.event(Event::OfferResponded {
        proposal,
        life: life.clone(),
        offer: offer.to_string(),
        response,
    });
    match response {
        OfferResponse::Accept => {
            o.status = OfferStatus::Accepted;
            o.responded_at = Some(cx.at);
            f.stage = Stage::Accepted;
            flow::settle(&st.config, p, cx)
        }
        OfferResponse::Decline => flow::offer_not_taken(&st.config, p, life, OfferStatus::Declined, cx),
        OfferResponse::Counter => {
            // Back to assessment with a person. The decision already made
            // stays on file until another replaces it.
            o.status = OfferStatus::Countered;
            o.responded_at = Some(cx.at);
            let queue = f
                .cases
                .iter()
                .rev()
                .map(|c| c.moves.first().map_or(c.queue, |m| m.1))
                .find(|q| *q != Queue::SeniorReview)
                .unwrap_or(Queue::Medical);
            flow::open_case(p, life, CaseType::Referral, queue, [reason::COUNTER_OFFER.to_string()].into(), cx)?;
            Ok(())
        }
    }
}

pub(crate) fn withdraw(p: &mut Proposal, cx: &mut Cx) -> Result<(), Reject> {
    if cx.caller == "group-admin" && !matches!(p.entry, crate::model::Entry::GroupExcess { .. } | crate::model::Entry::GroupContinuation { .. }) {
        return reject("entry", "group administration withdraws only the proposals it opened");
    }
    if !matches!(p.status, ProposalStatus::Open | ProposalStatus::IssueFailed { .. }) {
        return reject("closed", format!("proposal {} is {}", p.id, p.status.key()));
    }
    p.status = ProposalStatus::Withdrawn { at: cx.at };
    cx.event(Event::ProposalWithdrawn { proposal: p.id.clone() });
    flow::end_proposal(p, cx);
    cx.effect(Effect::Notify {
        to: Recipient::Party { party: p.policyholder.clone() },
        kind: NoticeKind::ProposalWithdrawn,
        proposal: p.id.clone(),
        life: None,
        reasons: vec![],
    });
    Ok(())
}

pub(crate) fn chaser(p: &mut Proposal, cx: &mut Cx, life: &LifeId, from: ChaserFrom) -> Result<(), Reject> {
    if from == ChaserFrom::Adviser && p.adviser.is_none() {
        return reject("no_adviser", "there is no adviser on the proposal");
    }
    let proposal = p.id.clone();
    let Some(case) = life_mut(p, life)?.cases.iter_mut().find(|c| c.is_open()) else {
        return reject("no_case", "nobody is working on the life to chase");
    };
    case.chasers.push((from, cx.at));
    cx.event(Event::ChaserRecorded {
        proposal,
        life: life.clone(),
        from,
    });
    Ok(())
}

pub(crate) fn requote(_st: &UnderwritingState, p: &mut Proposal, cx: &mut Cx, quote: QuoteBinding, signed: &SignedStep) -> Result<(), Reject> {
    let failed = matches!(p.status, ProposalStatus::IssueFailed { .. });
    if !failed && p.status != ProposalStatus::Open {
        return reject("closed", format!("proposal {} is {}", p.id, p.status.key()));
    }
    // The applicant accepts the new price: nobody is moved to it silently.
    if !signed.verifies(&message("requote", &[&p.id.0, &quote.product_hash, &quote.premium_minor.to_string()])) {
        return reject("signature", "the new quote is not accepted with a signed step");
    }
    p.quote = quote;
    if failed {
        flow::issue(p, cx)?;
    }
    Ok(())
}

/// What the adviser sees: status, and for a life who has consented, where
/// the answers are and how the evidence is coming along. Never medical
/// reports, underwriter notes or reasons. Every look is logged.
pub(crate) fn adviser_lookup(st: &UnderwritingState, p: &mut Proposal, cx: &mut Cx, adviser: &str, purpose: &str) -> Result<(), Reject> {
    let Some(on_file) = p.adviser.as_ref().filter(|a| a.adviser == adviser) else {
        return reject("not_your_client", "the adviser is not on this proposal");
    };
    if !st.licensed(on_file) {
        return reject("adviser_not_licensed", "the adviser, or the provider they act under, is not licensed");
    }
    if purpose.is_empty() {
        return reject("purpose", "a look at a file needs its purpose");
    }
    let mut documents = Vec::new();
    let lives = p
        .lives
        .values()
        .map(|f| {
            let shared = f.consented(ConsentKind::AdviserAccess) && f.adviser_access_ended.is_none();
            let answers_ref = f.disclosures.last().filter(|_| shared).map(|d| d.vault_ref.clone());
            documents.extend(answers_ref.clone());
            AdviserLifeView {
                life: f.life.clone(),
                stage: f.stage.key().to_string(),
                evidence: if shared {
                    f.evidence.iter().map(|e| (e.kind, e.status)).collect()
                } else {
                    vec![]
                },
                outcome: f
                    .effective_decision()
                    .map(|d| d.terms.iter().map(|t| (t.cover.clone(), t.outcome.key().to_string())).collect()),
                answers_ref,
            }
        })
        .collect();
    cx.event(Event::FileAccessed {
        who: adviser.to_string(),
        role: "adviser".to_string(),
        purpose: purpose.to_string(),
        proposal: p.id.clone(),
        life: None,
        documents,
    });
    cx.out.view = Some(View::Adviser {
        status: p.status.key().to_string(),
        lives,
    });
    Ok(())
}
