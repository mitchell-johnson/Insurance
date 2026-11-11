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
    let rule_sets = vec![load_rule_set("rules_individual.star", RULES_INDIVIDUAL)?];
    for rules in &rule_sets {
        for questions in question_sets.iter().filter(|q| q.id == rules.question_set) {
            rules.check_references(questions, &exclusions, &reasons).map_err(|error| LoadError::Invalid {
                file: "rules_individual.star".to_string(),
                error,
            })?;
        }
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
