//! The persona library: synthetic policyholders, families and employers.
//! Everything here is invented. No real person or insurer is represented.

use chrono::NaiveDate;
use ic_core::command::{CoverSpec, IssuePolicy, LifeSpec};
use ic_core::{Command, GroupId, LifeFactors, LifeId, LifeKind, PartyId, PolicyId};
use ic_money::DecStr;
use ic_product::{Frequency, Sex};
use rand::rngs::StdRng;
use rand::Rng;

/// How a persona pays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayBehaviour {
    /// Pays on the due date.
    OnTime,
    /// Pays this many days late, every time.
    Late(u32),
    /// Pays on time, but one payment in the year bounces and is paid again a week later.
    OneDishonour,
    /// Stops paying after this many periods and never comes back.
    StopsAfter(u32),
    /// Stops after this many periods, lapses, then pays the arrears and reinstates.
    LapsesThenReinstates(u32),
    /// Pays a year's worth up front.
    PaysAhead,
    /// Cancels this many days after inception.
    CancelsAfter(u32),
}

#[derive(Debug, Clone)]
pub struct Persona {
    pub policy: PolicyId,
    pub number: String,
    pub group: GroupId,
    pub behaviour: PayBehaviour,
    pub issue: IssuePolicy,
}

pub fn named_life(id: &str, party: &str, dob: NaiveDate, sex: Sex, smoker: bool) -> LifeSpec {
    LifeSpec {
        id: LifeId(id.to_string()),
        kind: LifeKind::Named {
            party: PartyId(party.to_string()),
        },
        factors: LifeFactors {
            date_of_birth: Some(dob),
            sex: Some(sex),
            smoker: Some(smoker),
            category: None,
            salary_minor: None,
            ..Default::default()
        },
    }
}

pub fn member(member_ref: &str, dob: NaiveDate, category: &str, salary_minor: i64) -> LifeSpec {
    LifeSpec {
        id: LifeId(member_ref.to_string()),
        kind: LifeKind::UnnamedMember {
            member_ref: member_ref.to_string(),
            linked_party: None,
        },
        factors: LifeFactors {
            date_of_birth: Some(dob),
            sex: None,
            smoker: None,
            category: Some(category.to_string()),
            salary_minor: Some(salary_minor),
            ..Default::default()
        },
    }
}

/// A unit-linked savings policy: one investment account and nothing to bill.
/// What the owner pays in is a contribution, applied as a receipt and then
/// allocated to funds on the product's menu.
pub fn invest_plan(n: u64, inception: NaiveDate, dob: NaiveDate, sex: Sex) -> IssuePolicy {
    IssuePolicy {
        policy: PolicyId(format!("pol-{n:08}")),
        number: format!("KIP{n:08}"),
        group: GroupId(format!("grp-{n:08}")),
        policyholder: PartyId(format!("party-{n:08}")),
        product_code: "invest-plan".to_string(),
        inception,
        frequency: Frequency::Yearly,
        lives: vec![named_life("life-1", &format!("party-{n:08}"), dob, sex, false)],
        covers: vec![CoverSpec {
            code: "account".to_string(),
            life: Some(LifeId("life-1".to_string())),
            sum_insured_minor: 0,
            loadings: vec![],
            exclusions: vec![],
        }],
        migrated: false,
    }
}

/// A retail term life policy on one life, with optional trauma cover.
#[allow(clippy::too_many_arguments)]
pub fn term_life(
    n: u64,
    inception: NaiveDate,
    dob: NaiveDate,
    sex: Sex,
    smoker: bool,
    life_cover_minor: i64,
    trauma_cover_minor: Option<i64>,
    frequency: Frequency,
    loading: Option<&str>,
) -> IssuePolicy {
    let life = LifeId("life-1".to_string());
    let loadings: Vec<DecStr> = loading.and_then(|l| DecStr::parse(l).ok()).into_iter().collect();
    let mut covers = vec![CoverSpec {
        code: "life".to_string(),
        life: Some(life.clone()),
        sum_insured_minor: life_cover_minor,
        loadings: loadings.clone(),
        exclusions: vec![],
    }];
    if let Some(t) = trauma_cover_minor {
        covers.push(CoverSpec {
            code: "trauma".to_string(),
            life: Some(life.clone()),
            sum_insured_minor: t,
            loadings,
            exclusions: vec![],
        });
    }
    IssuePolicy {
        policy: PolicyId(format!("pol-{n:08}")),
        number: format!("KTL{n:08}"),
        group: GroupId(format!("grp-{n:08}")),
        policyholder: PartyId(format!("party-{n:08}")),
        product_code: "term-life".to_string(),
        inception,
        frequency,
        lives: vec![named_life("life-1", &format!("party-{n:08}"), dob, sex, smoker)],
        covers,
        migrated: false,
    }
}

/// A health plan for a family: each person is a life with their own cover layer.
pub fn health_family(n: u64, inception: NaiveDate, dobs: &[NaiveDate], frequency: Frequency) -> IssuePolicy {
    let lives: Vec<LifeSpec> = dobs
        .iter()
        .enumerate()
        .map(|(i, dob)| named_life(&format!("life-{}", i + 1), &format!("party-{n:08}-{}", i + 1), *dob, Sex::Female, false))
        .collect();
    let covers = lives
        .iter()
        .map(|l| CoverSpec {
            code: "hospital".to_string(),
            life: Some(l.id.clone()),
            sum_insured_minor: 0,
            loadings: vec![],
            exclusions: vec![],
        })
        .collect();
    IssuePolicy {
        policy: PolicyId(format!("pol-{n:08}")),
        number: format!("EVH{n:08}"),
        group: GroupId(format!("grp-{n:08}")),
        policyholder: PartyId(format!("party-{n:08}-1")),
        product_code: "health-plan".to_string(),
        inception,
        frequency,
        lives,
        covers,
        migrated: false,
    }
}

/// A mix-and-match health policy: each person is a life with the covers
/// chosen for them, which need not be the same from one person to the next.
pub fn health_flex(n: u64, inception: NaiveDate, people: &[(NaiveDate, &[&str])], frequency: Frequency) -> IssuePolicy {
    let lives: Vec<LifeSpec> = people
        .iter()
        .enumerate()
        .map(|(i, (dob, _))| named_life(&format!("life-{}", i + 1), &format!("party-{n:08}-{}", i + 1), *dob, Sex::Female, false))
        .collect();
    let covers = lives
        .iter()
        .zip(people)
        .flat_map(|(l, (_, chosen))| {
            chosen.iter().map(|code| CoverSpec {
                code: code.to_string(),
                life: Some(l.id.clone()),
                sum_insured_minor: 0,
                loadings: vec![],
                exclusions: vec![],
            })
        })
        .collect();
    IssuePolicy {
        policy: PolicyId(format!("pol-{n:08}")),
        number: format!("FLX{n:08}"),
        group: GroupId(format!("grp-{n:08}")),
        policyholder: PartyId(format!("party-{n:08}-1")),
        product_code: "health-flex".to_string(),
        inception,
        frequency,
        lives,
        covers,
        migrated: false,
    }
}

/// A group life scheme with its first members.
pub fn group_scheme(n: u64, inception: NaiveDate, first_members: Vec<LifeSpec>) -> IssuePolicy {
    IssuePolicy {
        policy: PolicyId(format!("pol-{n:08}")),
        number: format!("WPL{n:08}"),
        group: GroupId(format!("grp-{n:08}")),
        policyholder: PartyId(format!("employer-{n:08}")),
        product_code: "group-life".to_string(),
        inception,
        frequency: Frequency::Monthly,
        lives: first_members,
        covers: vec![CoverSpec {
            code: "group-life".to_string(),
            life: None,
            sum_insured_minor: 0,
            loadings: vec![],
            exclusions: vec![],
        }],
        migrated: false,
    }
}

pub fn issue_command(p: IssuePolicy) -> Command {
    Command::IssuePolicy(Box::new(p))
}

fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap_or(NaiveDate::MIN)
}

/// Draw one retail persona. Deterministic for a given generator state.
pub fn random_persona(rng: &mut StdRng, n: u64, inception: NaiveDate) -> Persona {
    let dob = date(rng.gen_range(1962..=2004), rng.gen_range(1..=12), rng.gen_range(1..=28));
    let sex = if rng.gen_bool(0.5) { Sex::Female } else { Sex::Male };
    let smoker = rng.gen_bool(0.12);
    let frequency = match rng.gen_range(0..100) {
        0..=79 => Frequency::Monthly,
        80..=94 => Frequency::Fortnightly,
        _ => Frequency::Yearly,
    };
    let behaviour = match rng.gen_range(0..100) {
        0..=59 => PayBehaviour::OnTime,
        60..=71 => PayBehaviour::Late(rng.gen_range(3..=45)),
        72..=77 => PayBehaviour::OneDishonour,
        78..=84 => PayBehaviour::StopsAfter(rng.gen_range(1..=6)),
        85..=90 => PayBehaviour::LapsesThenReinstates(rng.gen_range(1..=4)),
        91..=94 => PayBehaviour::PaysAhead,
        _ => PayBehaviour::CancelsAfter(rng.gen_range(2..=200)),
    };
    let issue = if rng.gen_bool(0.25) {
        let mut dobs = vec![dob];
        for _ in 0..rng.gen_range(0..=3) {
            dobs.push(date(rng.gen_range(1965..=2026), rng.gen_range(1..=12), rng.gen_range(1..=28)));
        }
        health_family(n, inception, &dobs, frequency)
    } else {
        let life_cover = rng.gen_range(1..=20) * 50_000_00;
        let trauma = rng.gen_bool(0.4).then(|| rng.gen_range(1..=10) * 20_000_00);
        let loading = rng.gen_bool(0.1).then_some("0.50");
        term_life(n, inception, dob, sex, smoker, life_cover, trauma, frequency, loading)
    };
    Persona {
        policy: issue.policy.clone(),
        number: issue.number.clone(),
        group: issue.group.clone(),
        behaviour,
        issue,
    }
}

impl Persona {
    /// Every shipped product is in New Zealand dollars.
    pub fn issue_currency(&self) -> ic_money::Currency {
        ic_money::Currency::NZD
    }
}
