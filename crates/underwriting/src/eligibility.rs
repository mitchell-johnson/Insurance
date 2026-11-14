//! What an application must be, product by product, before it is taken.
//!
//! These are the product's own conditions on who may apply for what, not
//! health underwriting: an occupation class the rating needs, an entry age
//! that depends on the occupation, an option only some occupations may
//! choose, a term to an age that must run long enough, and a limit on the
//! sum insured that depends on the life's age. An application outside them
//! is refused at the door with the reason, so the adviser corrects it; no
//! decision is recorded against the life. Each check is a business setting,
//! activated with the settings and their approvals.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::command::{OpenProposal, Reject};
use crate::config::InterimSettings;
use crate::machine::reject;
use crate::model::Entry;
use crate::prepare::age_on;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Eligibility {
    /// The products the check applies to.
    pub products: BTreeSet<String>,
    pub check: Check,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "check", rename_all = "snake_case", deny_unknown_fields)]
pub enum Check {
    /// A cover of these types is rated on the life's occupation class, so
    /// the life must have one.
    OccupationRequired { cover_types: BTreeSet<String> },
    /// A cover of these types is taken up to `max_age` at entry, or up to
    /// `max_age_for_classes` for a life in one of `classes`.
    EntryAgeByOccupation {
        cover_types: BTreeSet<String>,
        max_age: u32,
        classes: BTreeSet<String>,
        max_age_for_classes: u32,
    },
    /// The option choice (`axis=choice`) on a cover of these types is only
    /// for a life in one of `classes`.
    ChoiceForOccupations {
        cover_types: BTreeSet<String>,
        choice: String,
        classes: BTreeSet<String>,
    },
    /// On this cover, a choice `to-N` on the axis (a term to age N) must
    /// run at least `years` from the life's age at entry.
    TermToAgeAtLeast { cover: String, axis: String, years: u32 },
    /// A cover of these types on a life aged `from_age` to `to_age` takes
    /// no more than `max_minor`.
    SumByAge {
        cover_types: BTreeSet<String>,
        from_age: u32,
        to_age: u32,
        max_minor: i64,
    },
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

/// The checks the AIA products came with (docs/aia-products.md). Class 1 is
/// clerical, managerial and professional work, and classes 1 and 2 are the
/// occupations accepted for income cover to 70 (confirmed 14 Nov 2026).
pub fn shipped() -> Vec<Eligibility> {
    let occupational = set(&["aia-living", "aia-living-business", "aia-start-up-income", "aia-starter"]);
    vec![
        Eligibility {
            products: occupational.clone(),
            check: Check::OccupationRequired {
                cover_types: set(&["tpd", "income_protection", "accidental_injury"]),
            },
        },
        Eligibility {
            products: occupational.clone(),
            check: Check::EntryAgeByOccupation {
                cover_types: set(&["tpd"]),
                max_age: 55,
                classes: set(&["1"]),
                max_age_for_classes: 60,
            },
        },
        Eligibility {
            products: occupational,
            check: Check::ChoiceForOccupations {
                cover_types: set(&["income_protection"]),
                choice: "period=to-70".into(),
                classes: set(&["1", "2"]),
            },
        },
        Eligibility {
            products: set(&["aia-living"]),
            check: Check::TermToAgeAtLeast {
                cover: "family-protection".into(),
                axis: "term".into(),
                years: 15,
            },
        },
        Eligibility {
            products: set(&["aia-living", "aia-living-business"]),
            check: Check::SumByAge {
                cover_types: set(&["trauma"]),
                from_age: 0,
                to_age: 15,
                max_minor: 250_000_00,
            },
        },
        Eligibility {
            products: set(&["aia-essentials-life"]),
            check: Check::SumByAge {
                cover_types: set(&["life"]),
                from_age: 61,
                to_age: 70,
                max_minor: 500_000_00,
            },
        },
    ]
}

/// AIA Living proposals have AIA's own interim cover: life cover up to
/// $500,000 while the application is underwritten.
pub fn shipped_interim() -> BTreeMap<String, InterimSettings> {
    [(
        "aia-living".to_string(),
        InterimSettings {
            product_code: "aia-interim".into(),
            cover_code: "life".into(),
            max_sum_insured_minor: 500_000_00,
        },
    )]
    .into()
}

/// The choice made on `axis`, from a cover's option code.
fn chosen<'a>(option: Option<&'a str>, axis: &str) -> Option<&'a str> {
    option?
        .split(',')
        .find_map(|pair| pair.split_once('=').filter(|(a, _)| *a == axis).map(|(_, c)| c))
}

/// Refused `eligibility` with the reason when the application is outside
/// one of its product's checks. Only new business is checked: more cover on
/// a policy in force was taken on these terms when the policy was.
pub(crate) fn check(checks: &[Eligibility], open: &OpenProposal, today: NaiveDate) -> Result<(), Reject> {
    if open.entry != Entry::NewBusiness {
        return Ok(());
    }
    for e in checks.iter().filter(|e| e.products.contains(&open.product_code)) {
        for c in &open.covers {
            let Some(life) = open.lives.iter().find(|l| l.life == c.life) else { continue };
            let class = life.factors.occupation_class.as_deref();
            let age = life.factors.date_of_birth.map(|dob| age_on(dob, today));
            let refuse = |why: String| reject("eligibility", format!("{} on {}: {why}", c.code, c.life));
            match &e.check {
                Check::OccupationRequired { cover_types } if cover_types.contains(&c.cover_type) && class.is_none() => {
                    return refuse("the life's occupation class is needed".into());
                }
                Check::EntryAgeByOccupation {
                    cover_types,
                    max_age,
                    classes,
                    max_age_for_classes,
                } if cover_types.contains(&c.cover_type) => {
                    let limit = if class.is_some_and(|k| classes.contains(k)) {
                        *max_age_for_classes
                    } else {
                        *max_age
                    };
                    if age.is_some_and(|a| a > limit) {
                        return refuse(format!("taken up to age {limit} for this occupation"));
                    }
                }
                Check::ChoiceForOccupations { cover_types, choice, classes } if cover_types.contains(&c.cover_type) => {
                    let Some((axis, wanted)) = choice.split_once('=') else { continue };
                    if chosen(c.option.as_deref(), axis) == Some(wanted) && !class.is_some_and(|k| classes.contains(k)) {
                        return refuse(format!("{choice} is not offered for this occupation"));
                    }
                }
                Check::TermToAgeAtLeast { cover, axis, years } if cover == &c.code => {
                    let to = chosen(c.option.as_deref(), axis)
                        .and_then(|x| x.strip_prefix("to-"))
                        .and_then(|n| n.parse::<u32>().ok());
                    if let (Some(to), Some(age)) = (to, age) {
                        if to < age + years {
                            return refuse(format!("a term to age {to} must run at least {years} years"));
                        }
                    }
                }
                Check::SumByAge {
                    cover_types,
                    from_age,
                    to_age,
                    max_minor,
                } if cover_types.contains(&c.cover_type) && age.is_some_and(|a| (*from_age..=*to_age).contains(&a)) && c.sum_insured_minor > *max_minor => {
                    return refuse(format!("at most {max_minor} at ages {from_age} to {to_age}"));
                }
                _ => {}
            }
        }
    }
    Ok(())
}
