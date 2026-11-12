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
    /// Opaque risk codes an answer stands for, by the answer (`yes`, or a
    /// choice's option). Only on questions that are not clinical, and only
    /// under the roots reinsurance treaties name: `pursuit`, `occupation`
    /// and `travel`. Sent to reinsurance with a capacity request.
    #[serde(default)]
    pub risk_tags: BTreeMap<String, String>,
}

/// The families of risk code a question may stand for.
pub const RISK_ROOTS: &[&str] = &["pursuit", "occupation", "travel"];

/// Whether a code is a lower-case dotted tag under one of [`RISK_ROOTS`],
/// with at least one segment below the root.
pub fn is_risk_tag(tag: &str) -> bool {
    let mut segments = tag.split('.');
    let root_ok = segments.next().is_some_and(|r| RISK_ROOTS.contains(&r));
    let rest: Vec<&str> = segments.collect();
    root_ok
        && !rest.is_empty()
        && rest
            .iter()
            .all(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'))
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

    /// The risk codes a set of answers stands for.
    pub fn risk_tags(&self, answers: &BTreeMap<String, (u32, Answer)>) -> std::collections::BTreeSet<String> {
        answers
            .iter()
            .filter_map(|(id, (_, answer))| {
                let q = self.question(id)?;
                let key = match answer {
                    Answer::YesNo(true) => "yes",
                    Answer::Choice(c) => c.as_str(),
                    _ => return None,
                };
                q.risk_tags.get(key).cloned()
            })
            .collect()
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
        if !q.risk_tags.is_empty() && q.clinical {
            return Err(invalid(file, format!("question {}: a clinical answer is never sent as a risk code", q.id)));
        }
        for (answer, tag) in &q.risk_tags {
            let fits = match &q.kind {
                AnswerKind::YesNo => answer == "yes",
                AnswerKind::Choice { options } => options.contains(answer),
                _ => false,
            };
            if !fits {
                return Err(invalid(file, format!("question {}: no answer {answer} to tag", q.id)));
            }
            if !is_risk_tag(tag) {
                return Err(invalid(file, format!("question {}: {tag} is not a pursuit, occupation or travel tag", q.id)));
            }
        }
    }
    Ok(set)
}
