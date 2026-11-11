//! What a driver does with a life's answers before it proposes a
//! disclosure: check them against the question set, build the record that
//! is sealed in the clinical store, and run the rules. The state machine
//! then sees only the record's hash and what the rules said.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{Datelike, NaiveDate};
use ic_core::LifeId;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::model::{Proposal, VersionRef};
use crate::questions::{Answer, AnswerProblem};
use crate::rules::{Assessment, RuleInput};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PrepareError {
    #[error("{0} is not on the proposal")]
    UnknownLife(LifeId),
    #[error("no question set or rules are in force for the contract date")]
    NoRules,
    #[error("the life's date of birth is needed to run the rules")]
    NoDateOfBirth,
    #[error("the answers do not fit the question set")]
    Answers(Vec<AnswerProblem>),
}

/// A disclosure ready to seal, sign and submit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    /// The record to seal in the clinical store, as canonical JSON.
    pub record: Vec<u8>,
    /// SHA-256 of the record: what the life signs and the state holds.
    pub hash: String,
    pub question_set: VersionRef,
    pub rule_set: VersionRef,
    pub assessments: BTreeMap<String, Assessment>,
    pub entered_by_adviser: u32,
}

#[derive(Serialize)]
struct Record<'a> {
    proposal: &'a str,
    life: &'a str,
    question_set: &'a VersionRef,
    answers: BTreeMap<&'a str, RecordedAnswer<'a>>,
}

#[derive(Serialize)]
struct RecordedAnswer<'a> {
    question_version: u32,
    answer: &'a Answer,
    /// Typed by the adviser for the client, and marked as such for good.
    entered_by_adviser: bool,
}

/// Completed years between two dates.
pub fn age_on(date_of_birth: NaiveDate, on: NaiveDate) -> u32 {
    let years = on.year() - date_of_birth.year() - i32::from((on.month(), on.day()) < (date_of_birth.month(), date_of_birth.day()));
    years.max(0) as u32
}

/// Check the answers, build the record and run the rules for each cover on
/// the life. `complete` is false for a supplement such as confirmed
/// interview answers, which need not answer every question.
pub fn prepare_disclosure(
    config: &Config,
    proposal: &Proposal,
    life: &LifeId,
    answers: &BTreeMap<String, (u32, Answer)>,
    adviser_entered: &BTreeSet<String>,
    complete: bool,
) -> Result<Prepared, PrepareError> {
    let file = proposal.lives.get(life).ok_or_else(|| PrepareError::UnknownLife(life.clone()))?;
    let rules = config.rule_set_on(&proposal.product_code, proposal.received_on).ok_or(PrepareError::NoRules)?;
    let questions = config.question_set_on(&rules.question_set, proposal.received_on).ok_or(PrepareError::NoRules)?;
    let problems = questions.check(answers, complete);
    if !problems.is_empty() {
        return Err(PrepareError::Answers(problems));
    }
    let question_set = VersionRef {
        id: questions.id.clone(),
        version: questions.version,
        content_hash: questions.content_hash.clone(),
    };
    let record = Record {
        proposal: &proposal.id.0,
        life: &life.0,
        question_set: &question_set,
        answers: answers
            .iter()
            .map(|(id, (version, answer))| {
                let entered_by_adviser = adviser_entered.contains(id);
                (
                    id.as_str(),
                    RecordedAnswer {
                        question_version: *version,
                        answer,
                        entered_by_adviser,
                    },
                )
            })
            .collect(),
    };
    let record = serde_json::to_vec(&record).unwrap_or_default();
    let age = age_on(file.factors.date_of_birth.ok_or(PrepareError::NoDateOfBirth)?, proposal.received_on);
    let plain: BTreeMap<String, Answer> = answers.iter().map(|(id, (_, a))| (id.clone(), a.clone())).collect();
    let assessments = proposal
        .covers_on(life)
        .map(|c| {
            let input = RuleInput {
                answers: &plain,
                age,
                smoker: file.factors.smoker,
                cover_type: &c.cover_type,
                sum_insured_minor: c.sum_insured_minor,
                // What the insurer already holds on the person counts
                // whether or not the applicant was shown it.
                existing_on_life_minor: file.existing_cover.as_ref().map_or(0, |e| e.total(&c.cover_type)),
            };
            (c.code.clone(), rules.assess(&input))
        })
        .collect();
    Ok(Prepared {
        hash: hex::encode(Sha256::digest(&record)),
        record,
        question_set,
        rule_set: VersionRef {
            id: rules.id.clone(),
            version: rules.version,
            content_hash: rules.content_hash.clone(),
        },
        assessments,
        entered_by_adviser: answers.keys().filter(|id| adviser_entered.contains(*id)).count() as u32,
    })
}
