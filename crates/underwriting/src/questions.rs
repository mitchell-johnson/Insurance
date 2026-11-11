//! Question sets. A disclosure is answers against question versions, so what
//! was asked and what was answered can always be reproduced.
//!
//! Question sets are effective-dated. The duty of disclosure changes when the
//! Contracts of Insurance Act 2024 comes into force, so two regimes run side
//! by side: a contract uses the set in effect on its contract date.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::load::{content_hash, eval, invalid, LoadError};

/// The legal basis the questions are asked on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisclosureRegime {
    /// The duty to disclose every material fact, asked or not.
    DutyOfDisclosure,
    /// The duty to take reasonable care not to misrepresent in answering the
    /// questions asked.
    ReasonableCare,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum AnswerKind {
    YesNo,
    Number {
        min: i64,
        max: i64,
    },
    Choice {
        options: Vec<String>,
    },
    Text,
    /// An amount in minor units.
    Money,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Question {
    pub id: String,
    pub version: u32,
    pub kind: AnswerKind,
    pub required: bool,
    /// Whether the answer is clinical information.
    pub clinical: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    YesNo(bool),
    Number(i64),
    Choice(String),
    Text(String),
    Money(i64),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionSet {
    pub id: String,
    pub version: u32,
    pub effective_from: NaiveDate,
    /// First day the set no longer applies to new contracts.
    #[serde(default)]
    pub effective_to: Option<NaiveDate>,
    pub regime: DisclosureRegime,
    pub questions: Vec<Question>,
    #[serde(default)]
    pub content_hash: String,
}

/// What is wrong with a set of answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "problem", rename_all = "snake_case")]
pub enum AnswerProblem {
    UnknownQuestion { question: String },
    WrongVersion { question: String, expected: u32 },
    WrongKind { question: String },
    OutOfRange { question: String },
    Missing { question: String },
}

impl QuestionSet {
    pub fn question(&self, id: &str) -> Option<&Question> {
        self.questions.iter().find(|q| q.id == id)
    }

    pub fn applies_on(&self, contract_date: NaiveDate) -> bool {
        self.effective_from <= contract_date && self.effective_to.is_none_or(|t| contract_date < t)
    }

    /// Check answers, each given with the question version it answers.
    /// `complete` also requires every required question to be answered; a
    /// supplement, such as confirmed interview answers, need not be complete.
    pub fn check(&self, answers: &BTreeMap<String, (u32, Answer)>, complete: bool) -> Vec<AnswerProblem> {
        let mut problems = Vec::new();
        for (id, (version, answer)) in answers {
            let Some(q) = self.question(id) else {
                problems.push(AnswerProblem::UnknownQuestion { question: id.clone() });
                continue;
            };
            if q.version != *version {
                problems.push(AnswerProblem::WrongVersion {
                    question: id.clone(),
                    expected: q.version,
                });
                continue;
            }
            match (&q.kind, answer) {
                (AnswerKind::YesNo, Answer::YesNo(_)) | (AnswerKind::Text, Answer::Text(_)) => {}
                (AnswerKind::Number { min, max }, Answer::Number(n)) => {
                    if n < min || n > max {
                        problems.push(AnswerProblem::OutOfRange { question: id.clone() });
                    }
                }
                (AnswerKind::Money, Answer::Money(n)) => {
                    if *n < 0 {
                        problems.push(AnswerProblem::OutOfRange { question: id.clone() });
                    }
                }
                (AnswerKind::Choice { options }, Answer::Choice(c)) => {
                    if !options.contains(c) {
                        problems.push(AnswerProblem::OutOfRange { question: id.clone() });
                    }
                }
                _ => problems.push(AnswerProblem::WrongKind { question: id.clone() }),
            }
        }
        if complete {
            for q in self.questions.iter().filter(|q| q.required && !answers.contains_key(&q.id)) {
                problems.push(AnswerProblem::Missing { question: q.id.clone() });
            }
        }
        problems
    }
}

/// Load a question set. The module must define `QUESTIONS`.
pub fn load_question_set(file: &str, source: &str) -> Result<QuestionSet, LoadError> {
    let mut set: QuestionSet = eval(file, source, "QUESTIONS")?;
    set.content_hash = content_hash(source);
    if set.id.is_empty() || set.version == 0 || set.questions.is_empty() {
        return Err(invalid(file, "question set needs an id, a version and at least one question"));
    }
    if set.effective_to.is_some_and(|t| t <= set.effective_from) {
        return Err(invalid(file, "question set ends before it starts"));
    }
    let mut seen = std::collections::BTreeSet::new();
    for q in &set.questions {
        if q.id.is_empty() || q.version == 0 {
            return Err(invalid(file, "a question needs an id and a version"));
        }
        if !seen.insert(q.id.as_str()) {
            return Err(invalid(file, format!("question {} is declared twice", q.id)));
        }
        match &q.kind {
            AnswerKind::Number { min, max } if max < min => return Err(invalid(file, format!("question {}: range is inverted", q.id))),
            AnswerKind::Choice { options } if options.is_empty() => return Err(invalid(file, format!("question {}: no options", q.id))),
            _ => {}
        }
    }
    Ok(set)
}
