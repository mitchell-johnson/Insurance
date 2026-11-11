//! A small world to drive the state machine in: the shipped configuration
//! activated, a handful of underwriters, a device key for applicants, and a
//! clock the test moves by hand.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, NaiveDate, Utc};
use ic_core::{GroupId, LifeFactors, LifeId, PartyId, PolicyId};
use ic_log::StateMachine;
use ic_product::{Frequency, Sex};
use ic_underwriting::authority::AuthorityProfile;
use ic_underwriting::command::{
    Applied, Approval, CapacityAnswer, Command, Effect, Envelope, OfferResponse, OpenProposal, Reject, SigningProof, SubmitDisclosure,
};
use ic_underwriting::model::{
    AdviserRef, ConsentKind, CoverOutcome, CoverTerms, Entry, Proposal, ProposalId, ProposedCover, ProposedLife, QuoteBinding, Stage,
};
use ic_underwriting::prepare::{prepare_disclosure, Prepared};
use ic_underwriting::questions::Answer;
use ic_underwriting::signing::{message, sign, SignedStep};
use ic_underwriting::{shipped, UnderwritingState};

pub const DEVICE: [u8; 32] = [7; 32];

pub fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

pub fn approval() -> Approval {
    Approval {
        author: "ann".into(),
        approvers: vec!["bob".into(), "cy".into()],
        change_ref: "CHG-1".into(),
    }
}

pub fn set(codes: &[&str]) -> BTreeSet<String> {
    codes.iter().map(|c| c.to_string()).collect()
}

pub fn signed(domain: &str, parts: &[&str]) -> SignedStep {
    sign("device-1", &DEVICE, &message(domain, parts))
}

pub fn standard(cover: &str) -> CoverTerms {
    CoverTerms {
        cover: cover.into(),
        outcome: CoverOutcome::standard(),
    }
}

pub fn accept(cover: &str, loading: u32, revised: Option<i64>) -> CoverTerms {
    CoverTerms {
        cover: cover.into(),
        outcome: CoverOutcome::Accept {
            loading_percent: loading,
            exclusions: vec![],
            revised_sum_insured_minor: revised,
        },
    }
}

pub fn decline(cover: &str) -> CoverTerms {
    CoverTerms {
        cover: cover.into(),
        outcome: CoverOutcome::Decline,
    }
}

/// Answers that pass every shipped rule.
pub fn clean() -> BTreeMap<String, (u32, Answer)> {
    let no = Answer::YesNo(false);
    [
        ("height_cm", Answer::Number(175)),
        ("weight_kg", Answer::Number(80)),
        ("smoked_last_12_months", no.clone()),
        ("alcohol_units_per_week", Answer::Number(5)),
        ("heart_condition", no.clone()),
        ("cancer_history", no.clone()),
        ("diabetes", no.clone()),
        ("mental_health_last_5_years", no.clone()),
        ("back_or_neck_last_5_years", no.clone()),
        ("awaiting_tests_or_results", no.clone()),
        ("family_history_before_60", no.clone()),
        ("hazardous_pursuits", no.clone()),
        ("hazardous_occupation", no.clone()),
        ("travel_high_risk_12_months", no.clone()),
        ("lived_overseas_last_5_years", no.clone()),
        ("previous_decline_or_terms", no),
        ("annual_income", Answer::Money(100_000_00)),
        ("existing_cover_elsewhere", Answer::Money(0)),
    ]
    .map(|(k, v)| (k.to_string(), (1, v)))
    .into()
}

pub fn with(extra: &[(&str, Answer)]) -> BTreeMap<String, (u32, Answer)> {
    let mut a = clean();
    a.extend(extra.iter().map(|(k, v)| (k.to_string(), (1, v.clone()))));
    a
}

pub fn yes(question: &str) -> BTreeMap<String, (u32, Answer)> {
    with(&[(question, Answer::YesNo(true))])
}

pub struct World {
    pub s: UnderwritingState,
    pub index: u64,
    pub now: DateTime<Utc>,
    /// Every command applied, for replay.
    pub log: Vec<Envelope>,
}

impl World {
    /// The shipped configuration, and five underwriters: `ash` an
    /// assistant, `amy` and `abe` underwriters, `sam` and `sue` seniors.
    pub fn new() -> Self {
        let mut w = World {
            s: UnderwritingState::default(),
            index: 0,
            now: DateTime::parse_from_rfc3339("2026-11-11T09:00:00Z").unwrap().with_timezone(&Utc),
            log: vec![],
        };
        let s = shipped::load().unwrap();
        w.ok(
            "config",
            Command::ActivateExclusions {
                library: Box::new(s.exclusions),
                approval: approval(),
            },
        );
        w.ok(
            "config",
            Command::ActivateReasons {
                library: Box::new(s.reasons),
                approval: approval(),
            },
        );
        for set in s.question_sets {
            w.ok(
                "config",
                Command::ActivateQuestionSet {
                    set: Box::new(set),
                    approval: approval(),
                },
            );
        }
        for set in s.rule_sets {
            w.ok(
                "config",
                Command::ActivateRuleSet {
                    set: Box::new(set),
                    backtest_digest: None,
                    approval: approval(),
                },
            );
        }
        for (who, grade) in [("ash", 0), ("amy", 1), ("abe", 1), ("sam", 2), ("sue", 2), ("cat", 3)] {
            let profile = AuthorityProfile {
                underwriter: who.into(),
                ..s.grades[grade].clone()
            };
            w.ok(
                "config",
                Command::ActivateProfile {
                    profile: Box::new(profile),
                    approval: approval(),
                },
            );
        }
        w
    }

    pub fn today(&self) -> NaiveDate {
        self.now.date_naive()
    }

    pub fn days(&mut self, n: i64) {
        self.now += Duration::days(n);
    }

    pub fn hours(&mut self, n: i64) {
        self.now += Duration::hours(n);
    }

    pub fn send_keyed(&mut self, key: &str, caller: &str, command: Command) -> Result<Applied, Reject> {
        self.index += 1;
        let envelope = Envelope {
            key: key.to_string(),
            caller: caller.to_string(),
            at: self.now,
            today: self.today(),
            command,
        };
        self.log.push(envelope.clone());
        self.s.apply(self.index, envelope)
    }

    pub fn send(&mut self, caller: &str, command: Command) -> Result<Applied, Reject> {
        let key = format!("k{}", self.index + 1);
        self.send_keyed(&key, caller, command)
    }

    pub fn ok(&mut self, caller: &str, command: Command) -> Applied {
        self.send(caller, command).unwrap()
    }

    /// The code a command is refused with. The state must be untouched.
    pub fn err(&mut self, caller: &str, command: Command) -> String {
        let before = self.s.clone();
        let code = self.send(caller, command).unwrap_err().code;
        assert!(self.s == before, "a refused command changed the state");
        code
    }

    pub fn open_command(&self, id: &str, lives: &[&str], covers: &[(&str, &str, &str, i64)], adviser: bool) -> OpenProposal {
        OpenProposal {
            proposal: id.into(),
            entry: Entry::NewBusiness,
            policy: PolicyId::from(format!("POL-{id}").as_str()),
            policy_number: format!("N-{id}"),
            group: GroupId::from("G1"),
            policyholder: PartyId::from(format!("PTY-{}", lives[0]).as_str()),
            product_code: "term-life".into(),
            frequency: Frequency::Monthly,
            channel: if adviser { "adviser" } else { "direct" }.into(),
            adviser: adviser.then(|| AdviserRef {
                adviser: "adv-1".into(),
                fap: "fap-1".into(),
            }),
            quote: QuoteBinding {
                product_hash: "prodhash-1".into(),
                rate_tables: vec![],
                premium_minor: 85_00,
                quoted_on: self.today(),
            },
            lives: lives
                .iter()
                .map(|l| ProposedLife {
                    life: LifeId::from(*l),
                    party: PartyId::from(format!("PTY-{l}").as_str()),
                    factors: LifeFactors {
                        date_of_birth: Some(d(1986, 5, 1)),
                        sex: Some(Sex::Female),
                        smoker: Some(false),
                        ..Default::default()
                    },
                })
                .collect(),
            covers: covers
                .iter()
                .map(|(life, code, cover_type, sum)| ProposedCover {
                    code: code.to_string(),
                    cover_type: cover_type.to_string(),
                    life: LifeId::from(*life),
                    sum_insured_minor: *sum,
                })
                .collect(),
            legacy_received_on: None,
        }
    }

    /// One life, `L1`, with life cover of the given amount.
    pub fn open(&mut self, id: &str, sum: i64) -> Applied {
        let open = self.open_command(id, &["L1"], &[("L1", "life", "life", sum)], false);
        self.ok("new-business", Command::OpenProposal(Box::new(open)))
    }

    pub fn p(&self, id: &str) -> &Proposal {
        self.s.proposal(&ProposalId::from(id)).unwrap()
    }

    pub fn stage(&self, id: &str, life: &str) -> Stage {
        self.p(id).lives[&LifeId::from(life)].stage
    }

    pub fn consent(&mut self, id: &str, life: &str, kind: ConsentKind) {
        let signed = (kind == ConsentKind::AdviserAccess).then(|| signed("consent", &[id, life, "adviser_access"]));
        self.ok(
            "new-business",
            Command::RecordConsent {
                proposal: id.into(),
                life: life.into(),
                kind,
                granted: true,
                signed,
            },
        );
    }

    pub fn prepared(&self, id: &str, life: &str, answers: &BTreeMap<String, (u32, Answer)>) -> Prepared {
        prepare_disclosure(&self.s.config, self.p(id), &LifeId::from(life), answers, &BTreeSet::new(), true).unwrap()
    }

    pub fn disclosure(&self, id: &str, life: &str, answers: &BTreeMap<String, (u32, Answer)>) -> SubmitDisclosure {
        let prepared = self.prepared(id, life, answers);
        SubmitDisclosure {
            proposal: id.into(),
            life: life.into(),
            signing: SigningProof::Device(signed("disclosure", &[id, life, &prepared.hash])),
            hash: prepared.hash,
            vault_ref: format!("vault://{id}/{life}/disclosure"),
            question_set: prepared.question_set,
            rule_set: prepared.rule_set,
            entered_by_adviser: 0,
            assessments: prepared.assessments,
        }
    }

    pub fn disclose(&mut self, id: &str, life: &str, answers: &BTreeMap<String, (u32, Answer)>) -> Applied {
        let d = self.disclosure(id, life, answers);
        self.ok("new-business", Command::SubmitDisclosure(Box::new(d)))
    }

    /// The reservation keys waiting for an answer in a result's effects.
    pub fn reservations(applied: &Applied) -> Vec<String> {
        applied
            .effects
            .iter()
            .filter_map(|e| match e {
                Effect::ReserveCapacity { reservation, .. } => Some(reservation.clone()),
                _ => None,
            })
            .collect()
    }

    /// Answer every reservation a result asked for as automatic capacity.
    pub fn automatic(&mut self, applied: &Applied) -> Applied {
        let mut last = Applied::default();
        for reservation in Self::reservations(applied) {
            last = self.ok(
                "reinsurance",
                Command::CapacityAnswered {
                    reservation,
                    answer: CapacityAnswer::Automatic,
                },
            );
        }
        last
    }

    pub fn case_of(&self, id: &str, life: &str) -> String {
        self.p(id).lives[&LifeId::from(life)].open_case().expect("an open case").id.clone()
    }

    pub fn take(&mut self, id: &str, life: &str, underwriter: &str) -> String {
        let case = self.case_of(id, life);
        self.ok(
            "underwriting-console",
            Command::TakeCase {
                case: case.clone(),
                underwriter: underwriter.into(),
            },
        );
        case
    }

    pub fn propose(&mut self, case: &str, underwriter: &str, terms: Vec<CoverTerms>) -> Result<Applied, Reject> {
        self.send(
            "underwriting-console",
            Command::ProposeDecision {
                case: case.into(),
                underwriter: underwriter.into(),
                terms,
                reasons: set(&["RC-0101"]),
                withheld_reasons: BTreeSet::new(),
                notes_ref: Some("vault://notes".into()),
                evidence: BTreeSet::new(),
            },
        )
    }

    pub fn respond(&mut self, id: &str, life: &str, response: OfferResponse) -> Applied {
        let offer = self.p(id).lives[&LifeId::from(life)].open_offer().expect("an open offer").id.clone();
        let signed = signed("offer", &[id, life, &offer, response.key()]);
        self.ok(
            "new-business",
            Command::RespondToOffer {
                proposal: id.into(),
                life: life.into(),
                offer,
                response,
                signed,
            },
        )
    }
}
