//! The driver against scripted receivers: every refusal, every answer that
//! arrives too late, and every way the module itself can let the driver
//! down. `tests/driver.rs` shows the same driver against the real modules.

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, NaiveDate, Utc};
use common::*;
use ic_core::command::{CoverSpec, IssuePolicy};
use ic_core::{GroupId, PolicyId};
use ic_group::command::{ExcessDecision, GroupCommand};
use ic_ledger::PartitionId;
use ic_reinsurance::capacity::Outcome;
use ic_underwriting::command::{Applied, CapacityAnswer, Command, Effect, Envelope, NonDisclosureOutcome, NoticeKind, PremiumOnAvoidance, Recipient, Reject};
use ic_underwriting::driver::{excess_case, Core, CoreAnswer, Driver, DriverError, Forward, NothingConnected, Refusal, Reinsurer, Schemes, Underwriting};
use ic_underwriting::evidence::EvidenceKind;
use ic_underwriting::model::Entry;
use ic_underwriting::special::Query;
use serde_json::Value;

#[derive(Default)]
struct Script {
    outbox: BTreeMap<u64, Effect>,
    /// The outbox answers with something that is not an outbox.
    garbled: bool,
    /// What the module says about a proposal.
    proposal: Value,
    /// Refusals by the key of the command refused, for every receiver.
    refuse: BTreeMap<String, (String, String)>,
    capacity: Option<Outcome>,
    told: Vec<(String, String, Command)>,
    parked: Vec<(u64, String)>,
    acknowledged: Vec<u64>,
    reinsurance: Vec<(String, String)>,
    core: Vec<(String, String, ic_core::Command)>,
    group: Vec<ic_group::command::Envelope>,
}

/// Every receiver at once, doing what the script says.
#[derive(Clone, Default)]
struct Scripted(Arc<Mutex<Script>>);

impl Scripted {
    fn with<T>(&self, f: impl FnOnce(&mut Script) -> T) -> T {
        f(&mut self.0.lock().unwrap())
    }
    fn refusal(&self, key: &str) -> Option<(String, String)> {
        self.with(|s| s.refuse.get(key).cloned())
    }
}

impl Underwriting for Scripted {
    async fn propose(&self, envelope: Envelope) -> Result<Result<Applied, Reject>, DriverError> {
        if let Some((code, message)) = self.refusal(&envelope.key) {
            return Ok(Err(Reject { code, message }));
        }
        self.with(|s| {
            match &envelope.command {
                Command::AcknowledgeEffects { through } => {
                    s.outbox.retain(|n, _| n > through);
                    s.acknowledged.push(*through);
                }
                Command::ParkEffect { effect, reason } => {
                    s.outbox.remove(effect);
                    s.parked.push((*effect, reason.clone()));
                }
                other => s.told.push((envelope.key.clone(), envelope.caller.clone(), other.clone())),
            }
            Ok(Ok(Applied::default()))
        })
    }
    async fn ask(&self, query: Query) -> Result<Value, DriverError> {
        Ok(self.with(|s| match query {
            Query::Outbox { .. } if s.garbled => Value::String("not an outbox".into()),
            Query::Outbox { after, max } => serde_json::to_value(s.outbox.range(after + 1..).take(max).collect::<Vec<_>>()).unwrap(),
            _ => s.proposal.clone(),
        }))
    }
}

impl Reinsurer for Scripted {
    async fn propose(&self, envelope: ic_reinsurance::Envelope) -> Result<Result<(), Refusal>, DriverError> {
        self.with(|s| s.reinsurance.push((envelope.key.clone(), envelope.caller.clone())));
        Ok(match self.refusal(&envelope.key) {
            Some((code, message)) => Err(Refusal::new(&code, message)),
            None => Ok(()),
        })
    }
    async fn capacity(&self, _reservation: &str) -> Result<Option<Outcome>, DriverError> {
        Ok(self.with(|s| s.capacity.clone()))
    }
}

impl Core for Scripted {
    async fn send(&self, _partition: PartitionId, key: &str, caller: &str, command: ic_core::Command) -> Result<CoreAnswer, DriverError> {
        self.with(|s| s.core.push((key.to_string(), caller.to_string(), command)));
        Ok(CoreAnswer {
            refused: self.refusal(key).map(|(code, message)| Refusal::new(&code, message)),
            events: vec![],
        })
    }
}

impl Schemes for Scripted {
    async fn propose(&self, envelope: ic_group::command::Envelope) -> Result<Result<(), Refusal>, DriverError> {
        self.with(|s| s.group.push(envelope));
        Ok(Ok(()))
    }
}

fn clock() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2027-02-01T09:00:00Z").unwrap().with_timezone(&Utc)
}

fn driver(script: &Scripted) -> Driver<Scripted, Scripted, Scripted, Scripted, NothingConnected> {
    Driver {
        underwriting: script.clone(),
        reinsurer: script.clone(),
        core: script.clone(),
        schemes: script.clone(),
        forward: NothingConnected,
        batch: 2,
        clock,
    }
}

fn automatic() -> Option<Outcome> {
    Some(Outcome::Automatic {
        retained_minor: 1,
        ceded_minor: 0,
    })
}

fn reserve() -> Effect {
    Effect::ReserveCapacity {
        reservation: "R1".into(),
        party: "party-1".into(),
        product_code: "term-life".into(),
        cover_code: "life".into(),
        cover_type: "life".into(),
        sum_insured_minor: 500_000_00,
        commencement: d(2027, 2, 1),
        proposal_expires: d(2027, 5, 1),
        risk_codes: vec!["pursuit.diving".into()],
    }
}

fn release() -> Effect {
    Effect::ReleaseCapacity {
        reservation: "R1".into(),
        reason: "withdrawn".into(),
    }
}

fn confirm() -> Effect {
    Effect::ConfirmCapacity {
        reservation: "R1".into(),
        policy: "POL-1".into(),
        life: "L1".into(),
        cover: "life".into(),
        layers: vec![1],
    }
}

fn factors() -> Effect {
    Effect::RecordRiskFactors {
        policy: "POL-1".into(),
        life: "L1".into(),
        date_of_birth: Some(d(1985, 3, 2)),
        sex: None,
        smoker: Some(false),
    }
}

fn issue_command() -> Box<IssuePolicy> {
    Box::new(IssuePolicy {
        policy: "POL-1".into(),
        number: "N-1".into(),
        group: GroupId::from("grp-1"),
        policyholder: "party-1".into(),
        product_code: "term-life".into(),
        inception: d(2027, 2, 1),
        frequency: ic_product::definition::Frequency::Monthly,
        lives: vec![],
        covers: vec![],
        migrated: false,
    })
}

fn issue() -> Effect {
    Effect::IssuePolicy {
        key: "P1/issue/1".into(),
        proposal: "P1".into(),
        expected_product_hash: "h".into(),
        migrated: false,
        command: issue_command(),
    }
}

fn interim() -> Effect {
    Effect::IssueInterim {
        key: "P1/interim".into(),
        proposal: "P1".into(),
        command: issue_command(),
    }
}

fn end_interim() -> Effect {
    Effect::EndInterim {
        key: "P1/interim/end".into(),
        proposal: "P1".into(),
        policy: "POL-1-interim".into(),
    }
}

fn cover(sum: i64) -> CoverSpec {
    CoverSpec {
        code: "life".into(),
        life: Some("L1".into()),
        sum_insured_minor: sum,
        loadings: vec![ic_money::DecStr::parse("0.5").unwrap()],
        exclusions: vec!["EX-BACK".into()],
    }
}

fn terms(proposal: &str, entry: Entry, covers: Vec<CoverSpec>) -> Effect {
    Effect::ApplyTerms {
        key: format!("{proposal}/terms"),
        proposal: proposal.into(),
        entry,
        policy: "POL-1".into(),
        effective: d(2027, 2, 1),
        covers,
        employer_may_see_loading: false,
    }
}

fn ended(proposal: &str, entry: Entry, outcome: &str) -> Effect {
    Effect::ProposalEnded {
        key: format!("{proposal}/ended"),
        proposal: proposal.into(),
        entry,
        policy: "POL-1".into(),
        outcome: outcome.into(),
    }
}

fn excess() -> Entry {
    Entry::GroupExcess { policy: "POL-1".into() }
}

fn alteration() -> Entry {
    Entry::Alteration { policy: "POL-1".into() }
}

fn reinstatement() -> Entry {
    Entry::Reinstatement { policy: "POL-1".into() }
}

fn remedy(proposal: Option<&str>, outcome: NonDisclosureOutcome, premium: Option<PremiumOnAvoidance>) -> Effect {
    Effect::NonDisclosureRemedy {
        key: "nd/1".into(),
        proposal: proposal.map(Into::into),
        policy: "POL-1".into(),
        life: "L1".into(),
        outcome,
        review: "ndr/CLM-1/L1".into(),
        decision: None,
        terms: vec![ic_core::policy::LayerTerms {
            cover: "life".into(),
            life: "L1".into(),
            loadings: vec![ic_money::DecStr::parse("0.5").unwrap()],
            exclusions: vec![],
        }],
        premium,
    }
}

fn increase() -> Entry {
    Entry::Increase { policy: "POL-1".into() }
}

/// A proposal as the module answers the question, taken from the real module.
fn a_proposal() -> Value {
    let mut w = World::new();
    w.open("P1", 500_000_00);
    serde_json::to_value(Some(w.p("P1").clone())).unwrap()
}

/// One effect in the outbox, carried once with the given refusals in play.
async fn carry(effect: Effect, capacity: Option<Outcome>, proposal: Value, refuse: &[(&str, &str)]) -> (ic_underwriting::driver::Pass, Scripted) {
    let script = Scripted::default();
    script.with(|s| {
        s.outbox.insert(7, effect);
        s.capacity = capacity;
        s.proposal = proposal;
        for (key, code) in refuse {
            s.refuse.insert(key.to_string(), (code.to_string(), "said the receiver".to_string()));
        }
    });
    let pass = driver(&script).deliver().await.unwrap();
    assert!(script.with(|s| s.outbox.is_empty()), "the outbox is always emptied");
    (pass, script)
}

/// Purpose: for every effect the driver routes, a refusal the driver has no answer for (from
/// reinsurance, the core, group administration's naming, or the module itself when an answer
/// is brought back) parks that effect with a reason that names who refused and the code they
/// gave. Effects with no receiver yet (an ended proposal for policy servicing), and a remedy
/// that does not say where its policy is held or gives the core nothing to act on, are parked
/// with a reason that says so.
/// Why it matters: a parked effect is the only trace of an instruction that did not take
/// effect. The reason is what the person looking at it has to go on; a missing or wrong one
/// sends them to the wrong team, and an effect that was neither delivered nor parked would be
/// an accepted applicant or a remedy silently lost.
#[tokio::test]
async fn a_refusal_parks_the_effect_with_who_refused_and_why() {
    let known = a_proposal();
    let cases: Vec<(Effect, Option<Outcome>, Value, Vec<(&str, &str)>, &str)> = vec![
        (
            reserve(),
            automatic(),
            Value::Null,
            vec![("uw:R1:reserve", "no_treaty")],
            "reinsurance refused the reservation: no_treaty",
        ),
        (
            reserve(),
            automatic(),
            Value::Null,
            vec![("cap:R1", "odd")],
            "the module refused the capacity answer: odd",
        ),
        (
            release(),
            None,
            Value::Null,
            vec![("uw:R1:release", "odd")],
            "reinsurance refused the release: odd",
        ),
        (
            confirm(),
            None,
            Value::Null,
            vec![("uw:R1:confirm", "layer_mismatch")],
            "reinsurance refused the confirmation: layer_mismatch",
        ),
        (
            factors(),
            None,
            Value::Null,
            vec![("uw:POL-1:L1:factors", "odd")],
            "reinsurance refused the rating factors: odd",
        ),
        (
            issue(),
            None,
            Value::Null,
            vec![("P1/issue/1/issued", "odd")],
            "the module refused the core's answer: odd",
        ),
        (
            interim(),
            None,
            Value::Null,
            vec![("P1/interim", "unknown_product")],
            "the core refused interim cover: unknown_product",
        ),
        (
            interim(),
            None,
            Value::Null,
            vec![("P1/interim/issued", "odd")],
            "interim cover is in force but the module refused to record it: odd",
        ),
        (end_interim(), None, Value::Null, vec![], "proposal P1 is not known to the module"),
        (
            end_interim(),
            None,
            known.clone(),
            vec![("P1/interim/end", "odd")],
            "the core refused to end interim cover: odd",
        ),
        (
            terms("P1", reinstatement(), vec![cover(1)]),
            None,
            Value::Null,
            vec![],
            "proposal P1 is not known to the module",
        ),
        (
            terms("P1", reinstatement(), vec![CoverSpec { life: None, ..cover(1) }]),
            None,
            known.clone(),
            vec![],
            "the reinstatement of life names no life",
        ),
        (
            terms("P1", alteration(), vec![CoverSpec { life: None, ..cover(1) }]),
            None,
            known.clone(),
            vec![],
            "the alteration on life names no life",
        ),
        (
            terms("P1", increase(), vec![cover(1)]),
            None,
            Value::Null,
            vec![],
            "proposal P1 is not known to the module",
        ),
        (
            terms("P1", increase(), vec![CoverSpec { life: None, ..cover(1) }]),
            None,
            known.clone(),
            vec![],
            "the increase on life names no life",
        ),
        (
            ended("P1", increase(), "declined"),
            None,
            Value::Null,
            vec![],
            "policy servicing is not connected to underwriting",
        ),
        (
            remedy(None, NonDisclosureOutcome::Avoid, Some(PremiumOnAvoidance::Refund)),
            None,
            known.clone(),
            vec![],
            "the remedy does not say which proposal",
        ),
        (
            remedy(Some("P1"), NonDisclosureOutcome::Avoid, Some(PremiumOnAvoidance::Refund)),
            None,
            Value::Null,
            vec![],
            "the remedy does not say which proposal",
        ),
        (
            remedy(Some("P1"), NonDisclosureOutcome::Avoid, None),
            None,
            known.clone(),
            vec![],
            "a avoid remedy with nothing the core can act on",
        ),
        (
            remedy(Some("P1"), NonDisclosureOutcome::Vary, None),
            None,
            known.clone(),
            vec![("nd/1", "claim_in_payment")],
            "the core refused the remedy: claim_in_payment",
        ),
        (
            terms("P1", excess(), vec![cover(1)]),
            None,
            Value::Null,
            vec![],
            "not one excess cover on a proposal group administration opened",
        ),
        (
            terms("grp-uw:acme:4", excess(), vec![cover(1), cover(2)]),
            None,
            Value::Null,
            vec![],
            "not one excess cover on a proposal group administration opened",
        ),
        (
            ended("P1", excess(), "lapsed"),
            None,
            Value::Null,
            vec![],
            "not a proposal group administration opened",
        ),
    ];
    for (effect, capacity, proposal, refuse, reason) in cases {
        let (pass, script) = carry(effect.clone(), capacity, proposal, &refuse).await;
        assert_eq!((pass.delivered, pass.stale), (0, 0), "{effect:?}");
        assert_eq!(pass.parked.len(), 1, "{effect:?}");
        assert_eq!(pass.parked[0].0, 7);
        assert!(pass.parked[0].1.starts_with(reason), "{effect:?}: {:?}", pass.parked[0].1);
        assert_eq!(
            script.with(|s| s.parked.clone()),
            pass.parked,
            "the module was told to park it, with the same reason"
        );
        assert!(script.with(|s| s.acknowledged.is_empty()), "a parked effect is not acknowledged as delivered");
    }
}

/// Purpose: a refusal that only says the work is already done, or that the proposal has moved
/// on, is not a fault. A reservation that already exists still has its answer fetched and
/// brought back; a reservation already released, a release with nothing to release, a
/// confirmation already made, an issue answer for a proposal no longer issuing, an interim
/// policy already ended and a capacity answer for a closed proposal are each counted as stale,
/// acknowledged, and not parked.
/// Why it matters: the driver repeats itself after every restart and effects are carried some
/// time after they were decided, so "already done" is the commonest answer there is. Parking
/// those would bury the real faults, and stopping on them would block every proposal behind.
#[tokio::test]
async fn done_already_or_no_longer_wanted_is_not_a_fault() {
    let known = a_proposal();

    // The reservation was made on an earlier pass: the answer is still fetched and brought back.
    let (pass, script) = carry(reserve(), automatic(), Value::Null, &[("uw:R1:reserve", "reservation_exists")]).await;
    assert_eq!((pass.delivered, pass.stale, pass.parked.len()), (1, 0, 0));
    let told = script.with(|s| s.told.clone());
    assert!(
        matches!(&told[..], [(key, caller, Command::CapacityAnswered { reservation, answer: CapacityAnswer::Automatic })]
            if key == "cap:R1" && caller == "reinsurance" && reservation == "R1"),
        "{told:?}"
    );

    let stale: Vec<(Effect, Option<Outcome>, Value, Vec<(&str, &str)>)> = vec![
        (reserve(), None, Value::Null, vec![]),
        (reserve(), automatic(), Value::Null, vec![("cap:R1", "closed")]),
        (reserve(), automatic(), Value::Null, vec![("cap:R1", "stale_reservation")]),
        (reserve(), automatic(), Value::Null, vec![("cap:R1", "unknown_reservation")]),
        (release(), None, Value::Null, vec![("uw:R1:release", "unknown_reservation")]),
        (release(), None, Value::Null, vec![("uw:R1:release", "reservation_released")]),
        (confirm(), None, Value::Null, vec![("uw:R1:confirm", "reservation_confirmed")]),
        (issue(), None, Value::Null, vec![("P1/issue/1/issued", "not_issuing")]),
        (end_interim(), None, known.clone(), vec![("P1/interim/end", "not_in_force")]),
        (end_interim(), None, known, vec![("P1/interim/end", "unknown_policy")]),
    ];
    for (effect, capacity, proposal, refuse) in stale {
        let (pass, script) = carry(effect.clone(), capacity, proposal, &refuse).await;
        assert_eq!((pass.delivered, pass.stale, pass.parked.len()), (0, 1, 0), "{effect:?}");
        assert_eq!(script.with(|s| s.acknowledged.clone()), vec![7], "{effect:?}");
    }
}

/// Purpose: what the driver sends is what the receiver expects. Each reinsurance answer maps
/// to the module's own (automatic, facultative, excluded); capacity commands go as caller
/// "underwriting" and rating factors as "new-business"; the issue goes to the core as a quoted
/// issue from "new-business" and interim cover is ended as "underwriting"; a group excess
/// answer carries the sum, loadings and exclusions for an acceptance, and a decline or a lapse
/// as declined or not proceeded with; a non-disclosure remedy goes to the core as an avoidance
/// with the seniors' choice on the premiums, or a variation with its terms, the review as the case.
/// Why it matters: every receiver checks its caller and refuses the wrong one, and each of
/// these mappings is a place where a wrong word turns a decline into "not proceeded with" or a
/// facultative referral into an automatic acceptance. Those are decisions about a person's
/// cover, carried wrongly with nothing failing.
#[tokio::test]
async fn each_receiver_is_sent_what_it_expects_from_the_caller_it_allows() {
    for (outcome, expected) in [
        (
            Outcome::FacultativeRequired {
                reason_code: "above_automatic_limit".into(),
                retained_minor: 1,
                excess_minor: 2,
            },
            CapacityAnswer::FacultativeRequired,
        ),
        (
            Outcome::Excluded {
                reason_code: "excluded_risk".into(),
            },
            CapacityAnswer::Excluded,
        ),
    ] {
        let (_, script) = carry(reserve(), Some(outcome), Value::Null, &[]).await;
        let told = script.with(|s| s.told.clone());
        assert!(
            matches!(&told[..], [(_, _, Command::CapacityAnswered { answer, .. })] if *answer == expected),
            "{told:?}"
        );
    }

    for (effect, key, caller) in [
        (reserve(), "uw:R1:reserve", "underwriting"),
        (release(), "uw:R1:release", "underwriting"),
        (confirm(), "uw:R1:confirm", "underwriting"),
        (factors(), "uw:POL-1:L1:factors", "new-business"),
    ] {
        let (pass, script) = carry(effect, automatic(), Value::Null, &[]).await;
        assert_eq!(pass.delivered, 1);
        assert_eq!(script.with(|s| s.reinsurance.clone()), vec![(key.to_string(), caller.to_string())]);
    }

    let (_, script) = carry(issue(), None, Value::Null, &[]).await;
    let sent = script.with(|s| s.core.clone());
    assert!(
        matches!(&sent[..], [(key, caller, ic_core::Command::IssueQuoted { expected_product_hash, .. })]
            if key == "P1/issue/1" && caller == "new-business" && expected_product_hash == "h"),
        "{sent:?}"
    );
    let told = script.with(|s| s.told.clone());
    assert!(
        matches!(&told[..], [(key, caller, Command::PolicyIssued { .. })] if key == "P1/issue/1/issued" && caller == "issuer"),
        "{told:?}"
    );

    // An increase: one command a cover as "underwriting", keyed by proposal, life and cover;
    // the first refusal stops it and goes back as the core's answer.
    let two = terms(
        "P1",
        increase(),
        vec![
            cover(1),
            CoverSpec {
                code: "trauma".into(),
                ..cover(2)
            },
        ],
    );
    let (_, script) = carry(two.clone(), None, a_proposal(), &[]).await;
    let sent = script.with(|s| s.core.clone());
    let keys: Vec<_> = sent.iter().map(|(key, caller, _)| (key.as_str(), caller.as_str())).collect();
    assert_eq!(keys, [("P1/increase/L1/life", "underwriting"), ("P1/increase/L1/trauma", "underwriting")]);
    assert!(matches!(&sent[1].2, ic_core::Command::AddUnderwrittenLayer(l) if l.cover == "trauma" && l.sum_insured_minor == 2 && l.reference == "P1/terms"));
    let told = script.with(|s| s.told.clone());
    assert!(
        matches!(&told[..], [(key, caller, Command::PolicyIssued { .. })] if key == "P1/terms/issued" && caller == "issuer"),
        "{told:?}"
    );
    let (pass, script) = carry(two, None, a_proposal(), &[("P1/increase/L1/life", "sum_insured")]).await;
    assert_eq!(
        (pass.delivered, script.with(|s| s.core.len())),
        (1, 1),
        "the second cover is not sent after a refusal"
    );
    let told = script.with(|s| s.told.clone());
    assert!(
        matches!(&told[..], [(key, _, Command::IssueRejected { code, .. })] if key == "P1/terms/rejected" && code == "sum_insured"),
        "{told:?}"
    );

    // An alteration: the same, as a change of terms on the cover held. A cover already on
    // the terms is nothing to write, and the rest still goes.
    let two = terms(
        "P1",
        alteration(),
        vec![
            cover(1),
            CoverSpec {
                code: "trauma".into(),
                ..cover(2)
            },
        ],
    );
    let (_, script) = carry(two.clone(), None, a_proposal(), &[("P1/alteration/L1/life", "no_change")]).await;
    let sent = script.with(|s| s.core.clone());
    let keys: Vec<_> = sent.iter().map(|(key, caller, _)| (key.as_str(), caller.as_str())).collect();
    assert_eq!(keys, [("P1/alteration/L1/life", "underwriting"), ("P1/alteration/L1/trauma", "underwriting")]);
    assert!(
        matches!(&sent[1].2, ic_core::Command::AlterUnderwrittenTerms(t) if t.cover == "trauma" && t.loadings == cover(2).loadings && t.reference == "P1/terms")
    );
    let told = script.with(|s| s.told.clone());
    assert!(
        matches!(&told[..], [(key, caller, Command::PolicyIssued { .. })] if key == "P1/terms/issued" && caller == "issuer"),
        "{told:?}"
    );
    let (_, script) = carry(two, None, a_proposal(), &[("P1/alteration/L1/life", "billing_behind")]).await;
    let told = script.with(|s| s.told.clone());
    assert!(
        matches!(&told[..], [(key, _, Command::IssueRejected { code, .. })] if key == "P1/terms/rejected" && code == "billing_behind"),
        "{told:?}"
    );
    assert_eq!(script.with(|s| s.core.len()), 1);

    // A reinstatement: one decision for the policy as "underwriting", good for as long as
    // the proposal is, answered like an issue either way.
    let back = terms("P1", reinstatement(), vec![cover(1)]);
    let expires: NaiveDate = serde_json::from_value(a_proposal()["expires_on"].clone()).unwrap();
    let (_, script) = carry(back.clone(), None, a_proposal(), &[]).await;
    let sent = script.with(|s| s.core.clone());
    assert!(
        matches!(&sent[..], [(key, caller, ic_core::Command::ApproveReinstatement(r))]
            if key == "P1/reinstatement" && caller == "underwriting" && r.reference == "P1/terms" && r.valid_until == expires
                && r.terms.len() == 1 && r.terms[0].cover == "life" && r.terms[0].loadings == cover(1).loadings),
        "{sent:?}"
    );
    let told = script.with(|s| s.told.clone());
    assert!(
        matches!(&told[..], [(key, caller, Command::PolicyIssued { layers, .. })] if key == "P1/terms/issued" && caller == "issuer" && layers.is_empty()),
        "{told:?}"
    );
    let (_, script) = carry(back, None, a_proposal(), &[("P1/reinstatement", "window_closed")]).await;
    let told = script.with(|s| s.told.clone());
    assert!(
        matches!(&told[..], [(key, _, Command::IssueRejected { code, .. })] if key == "P1/terms/rejected" && code == "window_closed"),
        "{told:?}"
    );

    // A non-disclosure remedy: one command to the core under the effect's own key, the review
    // as the case. The core does nothing twice for a case, so a repeat is safe.
    for (effect, expected) in [
        (
            remedy(Some("P1"), NonDisclosureOutcome::Avoid, Some(PremiumOnAvoidance::Retain)),
            "avoid_policy",
        ),
        (remedy(Some("P1"), NonDisclosureOutcome::Vary, None), "vary_for_non_disclosure"),
    ] {
        let (pass, script) = carry(effect, None, a_proposal(), &[]).await;
        assert_eq!(pass.delivered, 1);
        let sent = script.with(|s| s.core.clone());
        assert_eq!(sent.len(), 1);
        let (key, caller, command) = &sent[0];
        assert_eq!((key.as_str(), caller.as_str(), command.name()), ("nd/1", "underwriting", expected));
        match command {
            ic_core::Command::AvoidPolicy { case, premium, .. } => {
                assert_eq!((case.as_str(), *premium), ("ndr/CLM-1/L1", ic_core::policy::PremiumOnAvoidance::Retain))
            }
            ic_core::Command::VaryForNonDisclosure { case, terms, .. } => assert!(case == "ndr/CLM-1/L1" && terms[0].loadings[0].as_str() == "0.5"),
            other => panic!("{other:?}"),
        }
    }
    let (_, script) = carry(
        remedy(Some("P1"), NonDisclosureOutcome::Avoid, Some(PremiumOnAvoidance::Refund)),
        None,
        a_proposal(),
        &[],
    )
    .await;
    assert!(matches!(
        &script.with(|s| s.core.clone())[0].2,
        ic_core::Command::AvoidPolicy {
            premium: ic_core::policy::PremiumOnAvoidance::Refund,
            ..
        }
    ));

    let (_, script) = carry(interim(), None, Value::Null, &[]).await;
    let told = script.with(|s| s.told.clone());
    assert!(
        matches!(&told[..], [(key, caller, Command::InterimIssued { .. })] if key == "P1/interim/issued" && caller == "issuer"),
        "{told:?}"
    );

    let (_, script) = carry(end_interim(), None, a_proposal(), &[]).await;
    let sent = script.with(|s| s.core.clone());
    let interim_policy = PolicyId::from("POL-1-interim");
    assert!(
        matches!(&sent[..], [(key, caller, ic_core::Command::CancelPolicy { policy })] if key == "P1/interim/end" && caller == "underwriting" && *policy == interim_policy),
        "{sent:?}"
    );

    let decisions = [
        (
            terms("grp-uw:north:shore:12", excess(), vec![cover(300_000_00)]),
            ExcessDecision::Accepted {
                sum_insured_minor: 300_000_00,
                loadings: vec![ic_money::DecStr::parse("0.5").unwrap()],
                exclusions: vec!["EX-BACK".into()],
                member_consents_to_itemising: false,
            },
        ),
        (
            match terms("grp-uw:north:shore:12", excess(), vec![cover(300_000_00)]) {
                Effect::ApplyTerms {
                    key,
                    proposal,
                    entry,
                    policy,
                    effective,
                    covers,
                    ..
                } => Effect::ApplyTerms {
                    key,
                    proposal,
                    entry,
                    policy,
                    effective,
                    covers,
                    employer_may_see_loading: true,
                },
                other => other,
            },
            ExcessDecision::Accepted {
                sum_insured_minor: 300_000_00,
                loadings: vec![ic_money::DecStr::parse("0.5").unwrap()],
                exclusions: vec!["EX-BACK".into()],
                member_consents_to_itemising: true,
            },
        ),
        (ended("grp-uw:north:shore:12", excess(), "declined"), ExcessDecision::Declined),
        (ended("grp-uw:north:shore:12", excess(), "lapsed"), ExcessDecision::NotProceeded),
        (ended("grp-uw:north:shore:12", excess(), "withdrawn"), ExcessDecision::NotProceeded),
    ];
    for (effect, expected) in decisions {
        let (pass, script) = carry(effect, None, Value::Null, &[]).await;
        assert_eq!(pass.delivered, 1);
        let sent = script.with(|s| s.group.clone());
        assert_eq!(sent.len(), 1);
        assert!(sent[0].key.starts_with("uw:grp-uw:north:shore:12/") && sent[0].caller == "underwriting" && sent[0].today == d(2027, 2, 1));
        assert_eq!(
            sent[0].command,
            GroupCommand::RecordExcessDecision {
                scheme: "north:shore".into(),
                case: 12,
                decision: expected
            }
        );
    }
}

/// Purpose: the module itself lets the driver down. An outbox answer that is not an outbox, a
/// proposal answer that is not a proposal, a refusal to park and a refusal to acknowledge each
/// end the pass with an error that says what was wrong, and nothing is marked delivered that
/// was not. A batch size of zero still carries one effect at a time, and a long outbox is
/// carried batch by batch with a park in the middle not disturbing what is acknowledged.
/// Why it matters: these are version skew and misconfiguration, which appear on deployment
/// day. The safe behaviour is to stop loudly with the outbox intact, so that the instruction
/// is still there when the fault is fixed. A zero batch must not spin for ever doing nothing.
#[tokio::test]
async fn a_module_that_misbehaves_stops_the_pass_with_the_outbox_intact() {
    // Not an outbox.
    let script = Scripted::default();
    script.with(|s| s.garbled = true);
    assert!(matches!(driver(&script).deliver().await, Err(DriverError::Answer(_))));

    // Not a proposal.
    let script = Scripted::default();
    script.with(|s| {
        s.outbox.insert(1, end_interim());
        s.proposal = Value::String("not a proposal".into());
    });
    assert!(matches!(driver(&script).deliver().await, Err(DriverError::Answer(_))));
    assert_eq!(script.with(|s| s.outbox.len()), 1);

    // The module refuses to park: the pass stops, and what went before is acknowledged.
    let script = Scripted::default();
    script.with(|s| {
        s.outbox.insert(1, release());
        s.outbox.insert(2, terms("P1", alteration(), vec![]));
        s.refuse.insert("park:2".into(), ("caller_not_allowed".into(), "not the driver".into()));
    });
    let stopped = driver(&script).deliver().await.unwrap_err();
    assert!(matches!(&stopped, DriverError::Answer(m) if m.contains("caller_not_allowed")), "{stopped:?}");
    assert_eq!(
        script.with(|s| (s.acknowledged.clone(), s.outbox.keys().copied().collect::<Vec<_>>())),
        (vec![1], vec![2])
    );

    // The module refuses the acknowledgement.
    let script = Scripted::default();
    script.with(|s| {
        s.outbox.insert(1, release());
        s.refuse.insert("ack:1".into(), ("caller_not_allowed".into(), "not the driver".into()));
    });
    assert!(matches!(driver(&script).deliver().await, Err(DriverError::Answer(_))));
    assert_eq!(script.with(|s| s.outbox.len()), 1);

    // Five effects, a park in the middle, one at a time.
    let script = Scripted::default();
    script.with(|s| {
        for n in [1, 2, 4, 5] {
            s.outbox.insert(n, release());
        }
        s.outbox.insert(3, terms("P1", alteration(), vec![]));
    });
    let mut one_at_a_time = driver(&script);
    one_at_a_time.batch = 0;
    let pass = one_at_a_time.deliver().await.unwrap();
    assert_eq!((pass.delivered, pass.parked.len()), (4, 1));
    assert_eq!(
        script.with(|s| (s.acknowledged.clone(), s.parked[0].0, s.outbox.len())),
        (vec![1, 2, 4, 5], 3, 0)
    );
}

/// Purpose: a sweep the module refuses ends with an error that carries the refusal, and a sweep
/// whose day was done so long ago that the module no longer keeps its result ends at once with
/// nothing counted.
/// Why it matters: a refused sweep means the timer's caller or page size is wrong for this
/// module, and it must be loud, since nothing lapses until it is fixed. A repeat with no result
/// kept must not be read as "no more pages after an empty one" and loop.
#[tokio::test]
async fn a_refused_sweep_is_an_error_and_a_forgotten_one_is_done() {
    let script = Scripted::default();
    let done = driver(&script).sweep(50).await.unwrap();
    assert_eq!((done.pages, done.lapsed, done.chased, done.escalated), (0, 0, 0, 0));
    let told = script.with(|s| s.told.clone());
    assert!(
        matches!(&told[..], [(key, caller, Command::Sweep { after: None, max: 50 })] if key == "sweep:2027-02-01:" && caller == "timer"),
        "{told:?}"
    );

    script.with(|s| s.refuse.insert("sweep:2027-02-01:".into(), ("caller".into(), "not the timer".into())));
    let refused = driver(&script).sweep(5_000).await.unwrap_err();
    assert!(matches!(&refused, DriverError::Answer(m) if m.contains("not the timer")), "{refused:?}");
}

/// Purpose: a group excess proposal's name gives the scheme and the case, with the case taken
/// after the last colon so that a scheme whose own name has colons still parses. Anything else
/// (another prefix, no case, a case that is not a number, no scheme) is not one.
/// Why it matters: the name is the only link from an underwriting decision back to the
/// member's case in group administration. Parsed wrongly, a decision would be recorded against
/// another member's case.
#[test]
fn a_group_excess_proposal_names_its_scheme_and_case() {
    assert_eq!(excess_case(&"grp-uw:acme:7".into()), Some(("acme".to_string(), 7)));
    assert_eq!(excess_case(&"grp-uw:north:shore:12".into()), Some(("north:shore".to_string(), 12)));
    for not in ["P1", "grp-uw:acme", "grp-uw:acme:x", "grp-uw::3", "uw:acme:7", "grp-uw:acme:-1"] {
        assert_eq!(excess_case(&not.into()), None, "{not}");
    }
}

/// Purpose: with nothing connected, the reason an effect is parked names the receiver that is
/// missing: an evidence provider for a request, a chase or a cancellation; a delegate for a
/// file request; correspondence for a notice.
/// Why it matters: the reason is what tells operations which connection to set up before
/// putting the effect back. "Not connected" with no receiver named would leave them guessing.
#[tokio::test]
async fn nothing_connected_says_which_receiver_is_missing() {
    let evidence = "no evidence provider is connected";
    let cases = [
        (
            Effect::RequestEvidence {
                request: "E1".into(),
                kind: EvidenceKind::GpReport,
                party: "party-1".into(),
            },
            evidence,
        ),
        (Effect::ChaseEvidence { request: "E1".into() }, evidence),
        (Effect::CancelEvidence { request: "E1".into() }, evidence),
        (
            Effect::RequestDelegateFile {
                proposal: "P1".into(),
                life: "L1".into(),
                delegate_ref: "D-9".into(),
            },
            "no delegate is connected",
        ),
        (
            Effect::Notify {
                to: Recipient::UnderwritingManager,
                kind: NoticeKind::CaseEscalated,
                proposal: "P1".into(),
                life: None,
                reasons: vec![],
            },
            "correspondence is not connected to underwriting",
        ),
    ];
    for (effect, reason) in cases {
        assert_eq!(NothingConnected.forward(&effect).await, Ok(Some(reason.to_string())), "{effect:?}");
        let (pass, _) = carry(effect, None, Value::Null, &[]).await;
        assert_eq!(pass.parked, vec![(7, reason.to_string())]);
    }
}
