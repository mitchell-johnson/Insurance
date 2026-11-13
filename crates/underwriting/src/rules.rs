//! Underwriting rules as code.
//!
//! A rule set is written in Starlark and validated at load into the typed
//! tree below. Running it is a pure function over the structured answers and
//! the rating factors: the same inputs and the same rule version always give
//! the same assessment, which is what makes a decision reproducible and a
//! changed rule set testable against history before it is activated.
//!
//! A rule returns the reason codes that produced its outcome, so every
//! assessment explains itself.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::case::Queue;
use crate::codes::{ExclusionLibrary, ReasonLibrary};
use crate::evidence::EvidenceKind;
use crate::load::{content_hash, eval, invalid, LoadError};
use crate::questions::{Answer, QuestionSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "if", rename_all = "snake_case", deny_unknown_fields)]
pub enum Condition {
    Always,
    All {
        of: Vec<Condition>,
    },
    Any {
        of: Vec<Condition>,
    },
    Not {
        cond: Box<Condition>,
    },
    AnswerIs {
        question: String,
        value: Answer,
    },
    /// A number or money answer at or above the value. False when unanswered.
    AnswerAtLeast {
        question: String,
        value: i64,
    },
    AnswerAtMost {
        question: String,
        value: i64,
    },
    AnswerMissing {
        question: String,
    },
    AgeAtLeast {
        years: u32,
    },
    AgeAtMost {
        years: u32,
    },
    Smoker {
        is: bool,
    },
    CoverTypeIn {
        types: Vec<String>,
    },
    /// The sum insured proposed on this cover.
    SumInsuredAbove {
        minor: i64,
    },
    /// Everything on the life for this cover type: cover already held with
    /// the insurer, from the life index, plus the amount proposed.
    TotalOnLifeAbove {
        minor: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    /// Accept with an extra premium, a percentage of the base premium.
    Load {
        percent: u32,
    },
    /// Accept with an exclusion from the code list.
    Exclude {
        code: String,
    },
    Evidence {
        kind: EvidenceKind,
    },
    Refer {
        queue: Queue,
    },
    TeleInterview,
    Decline,
    Defer {
        months: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// An opaque code (`UR-` and four digits). It is what a decision, the
    /// log and the capacity request carry, so it must not name a condition.
    pub id: String,
    /// What the rule is for, in the rule set only: underwriters read it in
    /// the console against the code.
    #[serde(default)]
    pub about: String,
    pub when: Condition,
    pub then: Action,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSet {
    pub id: String,
    pub version: u32,
    pub effective_from: NaiveDate,
    #[serde(default)]
    pub effective_to: Option<NaiveDate>,
    /// The question set the rules read answers from.
    pub question_set: String,
    /// The products the rules apply to.
    pub products: Vec<String>,
    pub rules: Vec<Rule>,
    #[serde(default)]
    pub content_hash: String,
}

/// What a rule set is run over for one life on one cover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleInput<'a> {
    pub answers: &'a BTreeMap<String, Answer>,
    pub age: u32,
    pub smoker: Option<bool>,
    pub cover_type: &'a str,
    pub sum_insured_minor: i64,
    pub existing_on_life_minor: i64,
}

/// The single most serious thing an assessment calls for, in the order the
/// workflow acts on them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    /// Accept at standard rates with no person involved.
    Clean,
    /// Accept with a loading or an exclusion the rules set themselves.
    Terms,
    NeedsEvidence,
    TeleInterview,
    Refer,
    Defer,
    Decline,
}

/// The outcome of the rules for one life on one cover.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assessment {
    /// The rules that fired, in rule set order.
    pub fired: Vec<String>,
    pub reasons: BTreeSet<String>,
    pub loading_percent: u32,
    pub exclusions: BTreeSet<String>,
    pub evidence: BTreeSet<EvidenceKind>,
    pub queues: BTreeSet<Queue>,
    pub tele_interview: bool,
    pub defer_months: Option<u32>,
    pub decline: bool,
}

impl Disposition {
    pub fn key(self) -> &'static str {
        match self {
            Disposition::Clean => "clean",
            Disposition::Terms => "terms",
            Disposition::NeedsEvidence => "needs_evidence",
            Disposition::TeleInterview => "tele_interview",
            Disposition::Refer => "refer",
            Disposition::Defer => "defer",
            Disposition::Decline => "decline",
        }
    }
}

impl Assessment {
    pub fn disposition(&self) -> Disposition {
        if self.decline {
            Disposition::Decline
        } else if self.defer_months.is_some() {
            Disposition::Defer
        } else if !self.queues.is_empty() {
            Disposition::Refer
        } else if self.tele_interview {
            Disposition::TeleInterview
        } else if !self.evidence.is_empty() {
            Disposition::NeedsEvidence
        } else if self.loading_percent > 0 || !self.exclusions.is_empty() {
            Disposition::Terms
        } else {
            Disposition::Clean
        }
    }
}

impl Condition {
    fn holds(&self, i: &RuleInput<'_>) -> bool {
        let number = |q: &str| match i.answers.get(q) {
            Some(Answer::Number(n)) | Some(Answer::Money(n)) => Some(*n),
            _ => None,
        };
        match self {
            Condition::Always => true,
            Condition::All { of } => of.iter().all(|c| c.holds(i)),
            Condition::Any { of } => of.iter().any(|c| c.holds(i)),
            Condition::Not { cond } => !cond.holds(i),
            Condition::AnswerIs { question, value } => i.answers.get(question) == Some(value),
            Condition::AnswerAtLeast { question, value } => number(question).is_some_and(|n| n >= *value),
            Condition::AnswerAtMost { question, value } => number(question).is_some_and(|n| n <= *value),
            Condition::AnswerMissing { question } => !i.answers.contains_key(question),
            Condition::AgeAtLeast { years } => i.age >= *years,
            Condition::AgeAtMost { years } => i.age <= *years,
            Condition::Smoker { is } => i.smoker == Some(*is),
            Condition::CoverTypeIn { types } => types.iter().any(|t| t == i.cover_type),
            Condition::SumInsuredAbove { minor } => i.sum_insured_minor > *minor,
            Condition::TotalOnLifeAbove { minor } => i.sum_insured_minor.saturating_add(i.existing_on_life_minor) > *minor,
        }
    }

    fn questions<'a>(&'a self, out: &mut Vec<&'a str>) {
        match self {
            Condition::All { of } | Condition::Any { of } => of.iter().for_each(|c| c.questions(out)),
            Condition::Not { cond } => cond.questions(out),
            Condition::AnswerIs { question, .. }
            | Condition::AnswerAtLeast { question, .. }
            | Condition::AnswerAtMost { question, .. }
            | Condition::AnswerMissing { question } => out.push(question),
            _ => {}
        }
    }
}

impl RuleSet {
    pub fn applies_on(&self, contract_date: NaiveDate) -> bool {
        self.effective_from <= contract_date && self.effective_to.is_none_or(|t| contract_date < t)
    }

    /// Run the rules for one life on one cover. Every rule is evaluated;
    /// loadings add, and the most serious action sets the disposition.
    pub fn assess(&self, input: &RuleInput<'_>) -> Assessment {
        let mut a = Assessment::default();
        for rule in self.rules.iter().filter(|r| r.when.holds(input)) {
            a.fired.push(rule.id.clone());
            a.reasons.extend(rule.reasons.iter().cloned());
            match &rule.then {
                Action::Load { percent } => a.loading_percent = a.loading_percent.saturating_add(*percent),
                Action::Exclude { code } => {
                    a.exclusions.insert(code.clone());
                }
                Action::Evidence { kind } => {
                    a.evidence.insert(*kind);
                }
                Action::Refer { queue } => {
                    a.queues.insert(*queue);
                }
                Action::TeleInterview => a.tele_interview = true,
                Action::Decline => a.decline = true,
                Action::Defer { months } => a.defer_months = Some(a.defer_months.map_or(*months, |m| m.max(*months))),
            }
        }
        a
    }

    /// Check the rule set against the libraries it refers to. Run at load of
    /// the shipped configuration and again before activation.
    pub fn check_references(&self, questions: &QuestionSet, exclusions: &ExclusionLibrary, reasons: &ReasonLibrary) -> Result<(), String> {
        if questions.id != self.question_set {
            return Err(format!("rule set {} reads question set {}, not {}", self.id, self.question_set, questions.id));
        }
        for rule in &self.rules {
            let mut asked = Vec::new();
            rule.when.questions(&mut asked);
            if let Some(q) = asked.into_iter().find(|q| questions.question(q).is_none()) {
                return Err(format!("rule {}: question {q} is not in {} v{}", rule.id, questions.id, questions.version));
            }
            if let Some(code) = reasons.first_unknown(&rule.reasons) {
                return Err(format!("rule {}: reason {code} is not in the reason list", rule.id));
            }
            if let Action::Exclude { code } = &rule.then {
                if !exclusions.contains(code) {
                    return Err(format!("rule {}: exclusion {code} is not in the code list", rule.id));
                }
            }
        }
        Ok(())
    }
}

/// Load a rule set. The module must define `RULES`.
pub fn load_rule_set(file: &str, source: &str) -> Result<RuleSet, LoadError> {
    let mut set: RuleSet = eval(file, source, "RULES")?;
    set.content_hash = content_hash(source);
    if set.id.is_empty() || set.version == 0 || set.products.is_empty() {
        return Err(invalid(file, "rule set needs an id, a version and the products it applies to"));
    }
    if set.effective_to.is_some_and(|t| t <= set.effective_from) {
        return Err(invalid(file, "rule set ends before it starts"));
    }
    let mut seen = BTreeSet::new();
    for r in &set.rules {
        if r.id.is_empty() || !seen.insert(r.id.as_str()) {
            return Err(invalid(file, format!("rule id `{}` is empty or declared twice", r.id)));
        }
        // A rule's id travels in the log with every disclosure it fired on:
        // a descriptive id such as `diabetes` would put a condition there.
        let opaque = r.id.strip_prefix("UR-").is_some_and(|n| n.len() == 4 && n.bytes().all(|b| b.is_ascii_digit()));
        if !opaque || r.about.trim().is_empty() {
            return Err(invalid(
                file,
                format!("rule `{}` needs an opaque id (UR- and four digits) and says what it is about", r.id),
            ));
        }
        if r.reasons.is_empty() {
            return Err(invalid(file, format!("rule {} gives no reason code", r.id)));
        }
        match r.then {
            Action::Load { percent } if percent == 0 || percent > 500 => {
                return Err(invalid(file, format!("rule {}: loading must be between 1 and 500 percent", r.id)));
            }
            Action::Defer { months: 0 } => return Err(invalid(file, format!("rule {}: deferral needs a period", r.id))),
            _ => {}
        }
    }
    Ok(set)
}

/// The mix of outcomes a rule set gives over a set of proposals.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeMix {
    pub by_disposition: BTreeMap<Disposition, u64>,
}

/// One anonymised historical proposal: the answers and rating factors of one
/// life on one cover, with nothing that identifies the person.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BacktestCase {
    pub answers: BTreeMap<String, Answer>,
    pub age: u32,
    pub smoker: Option<bool>,
    pub cover_type: String,
    pub sum_insured_minor: i64,
    pub existing_on_life_minor: i64,
}

/// How a changed rule set would shift outcomes over history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BacktestReport {
    pub current: (u32, String),
    pub proposed: (u32, String),
    pub cases: u64,
    pub before: OutcomeMix,
    pub after: OutcomeMix,
    /// How many cases moved from one disposition to another, keyed
    /// `before>after`.
    pub moved: BTreeMap<String, u64>,
}

impl BacktestReport {
    /// The hash recorded on the activation, tying the approval to what the
    /// approvers were shown.
    pub fn digest(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(
            format!(
                "{:?}|{:?}|{}|{:?}|{:?}|{:?}",
                self.current, self.proposed, self.cases, self.before, self.after, self.moved
            )
            .as_bytes(),
        );
        hex::encode(h.finalize())
    }
}

/// Run both rule sets over history. A batch job: nothing here touches a
/// workflow or a proposal.
pub fn backtest(current: &RuleSet, proposed: &RuleSet, history: &[BacktestCase]) -> BacktestReport {
    let mut report = BacktestReport {
        current: (current.version, current.content_hash.clone()),
        proposed: (proposed.version, proposed.content_hash.clone()),
        cases: history.len() as u64,
        before: OutcomeMix::default(),
        after: OutcomeMix::default(),
        moved: BTreeMap::new(),
    };
    for case in history {
        let input = RuleInput {
            answers: &case.answers,
            age: case.age,
            smoker: case.smoker,
            cover_type: &case.cover_type,
            sum_insured_minor: case.sum_insured_minor,
            existing_on_life_minor: case.existing_on_life_minor,
        };
        let before = current.assess(&input).disposition();
        let after = proposed.assess(&input).disposition();
        *report.before.by_disposition.entry(before).or_insert(0) += 1;
        *report.after.by_disposition.entry(after).or_insert(0) += 1;
        if before != after {
            *report.moved.entry(format!("{}>{}", before.key(), after.key())).or_insert(0) += 1;
        }
    }
    report
}
