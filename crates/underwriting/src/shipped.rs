//! The configuration shipped with the platform, under `products/underwriting`.
//! It is compiled in so a deployment cannot start with a rule set that does
//! not load, and it still goes through activation like any other change.

use crate::authority::{load_profiles, AuthorityProfile};
use crate::codes::{load_exclusions, load_reasons, ExclusionLibrary, ExclusionWordings, ReasonLibrary};
use crate::load::LoadError;
use crate::questions::{load_question_set, QuestionSet};
use crate::rules::{load_rule_set, RuleSet};

pub const EXCLUSIONS: &str = include_str!("../../../products/underwriting/exclusions.star");
pub const REASONS: &str = include_str!("../../../products/underwriting/reasons.star");
pub const QUESTIONS_2026: &str = include_str!("../../../products/underwriting/questions_2026.star");
pub const QUESTIONS_2027: &str = include_str!("../../../products/underwriting/questions_2027.star");
pub const RULES_INDIVIDUAL: &str = include_str!("../../../products/underwriting/rules_individual.star");
pub const RULES_INDIVIDUAL_2027: &str = include_str!("../../../products/underwriting/rules_individual_2027.star");
pub const RULES_GROUP: &str = include_str!("../../../products/underwriting/rules_group.star");
pub const RULES_GROUP_2027: &str = include_str!("../../../products/underwriting/rules_group_2027.star");
pub const PROFILES: &str = include_str!("../../../products/underwriting/profiles.star");

#[derive(Debug, Clone)]
pub struct Shipped {
    pub exclusions: ExclusionLibrary,
    pub wordings: ExclusionWordings,
    pub reasons: ReasonLibrary,
    /// Oldest first: the duty of disclosure set, then the reasonable care set.
    pub question_sets: Vec<QuestionSet>,
    pub rule_sets: Vec<RuleSet>,
    /// Authority grades to make each underwriter's profile from.
    pub grades: Vec<AuthorityProfile>,
}

/// Load everything shipped and check the rule sets against what they read.
pub fn load() -> Result<Shipped, LoadError> {
    let (exclusions, wordings) = load_exclusions("exclusions.star", EXCLUSIONS)?;
    let reasons = load_reasons("reasons.star", REASONS)?;
    let question_sets = vec![
        load_question_set("questions_2026.star", QUESTIONS_2026)?,
        load_question_set("questions_2027.star", QUESTIONS_2027)?,
    ];
    // Oldest first: the set activated last wins on the dates both cover.
    // The individual and group sets name different products, so neither
    // ever stands in for the other.
    let files = [
        ("rules_individual.star", RULES_INDIVIDUAL),
        ("rules_individual_2027.star", RULES_INDIVIDUAL_2027),
        ("rules_group.star", RULES_GROUP),
        ("rules_group_2027.star", RULES_GROUP_2027),
    ];
    let mut rule_sets = Vec::new();
    for (file, source) in files {
        let rules = load_rule_set(file, source)?;
        // Checked against each version of its question set in force while it is.
        let overlaps = |q: &&QuestionSet| {
            q.id == rules.question_set && q.effective_to.is_none_or(|to| rules.effective_from < to) && rules.effective_to.is_none_or(|to| q.effective_from < to)
        };
        for questions in question_sets.iter().filter(overlaps) {
            rules
                .check_references(questions, &exclusions, &reasons)
                .map_err(|error| LoadError::Invalid { file: file.to_string(), error })?;
        }
        rule_sets.push(rules);
    }
    Ok(Shipped {
        exclusions,
        wordings,
        reasons,
        question_sets,
        rule_sets,
        grades: load_profiles("profiles.star", PROFILES)?,
    })
}
