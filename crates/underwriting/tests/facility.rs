//! New policies under a conversion facility of a policy in force.

mod common;

use chrono::NaiveDate;
use common::*;
use ic_core::command::{CoverSpec, IssuePolicy, LifeSpec};
use ic_core::policy::{LifeKind, PolicyStatus};
use ic_core::{GroupId, LifeId, PolicyId};
use ic_money::DecStr;
use ic_underwriting::case::Queue;
use ic_underwriting::command::{Applied, Command, Effect, Event, NoticeKind, OpenProposal, Reject};
use ic_underwriting::facility::{is_anniversary, verify};
use ic_underwriting::model::{DecisionSource, Entry, Facility, ProposalStatus, Stage};

/// A child born on 10 March 2005 asks for their own policy. The parent's policy started on
/// 1 April 2020, so the anniversary after the child turned 21 (10 March 2026) is 1 April 2026.
fn child(w: &World, id: &str, covers: &[(&str, &str, i64)]) -> OpenProposal {
    let specs: Vec<(&str, &str, &str, i64)> = covers.iter().map(|(c, t, s)| ("L1", *c, *t, *s)).collect();
    let mut open = w.open_command(id, &["L1"], &specs, false);
    open.product_code = "aia-living".into();
    open.lives[0].factors.date_of_birth = Some(d(2005, 3, 10));
    open.entry = Entry::Facility {
        facility: Facility::ChildrensTraumaConversion,
        from_policy: "POL-PARENT".into(),
        from_group: "grp-parent".into(),
        from_life: "L1".into(),
        from_cover: "critical-conditions".into(),
        trigger_date: d(2026, 4, 1),
        evidence: "vault://claims-search/child-1".into(),
    };
    open
}

/// A business that started trading on `since` moves its Start-Up Income.
fn business(w: &World, id: &str, cover: &str, since: NaiveDate) -> OpenProposal {
    let mut open = w.open_command(id, &["L1"], &[("L1", cover, "income_protection", 3_000_00)], false);
    open.product_code = "aia-living-business".into();
    open.entry = Entry::Facility {
        facility: Facility::BusinessContinuity,
        from_policy: "POL-START-UP".into(),
        from_group: "grp-start-up".into(),
        from_life: "L1".into(),
        from_cover: "start-up-income".into(),
        trigger_date: since,
        evidence: "vault://accounts/three-years".into(),
    };
    open
}

fn open(w: &mut World, open: OpenProposal) -> Result<Applied, Reject> {
    w.send("new-business", Command::OpenProposal(Box::new(open)))
}

fn check(w: &mut World, case: &str, accepted: bool, note: &str) -> Applied {
    w.ok(
        "underwriting-console",
        Command::CheckOptionEvidence {
            case: case.into(),
            underwriter: "amy".into(),
            accepted,
            note: note.into(),
        },
    )
}

fn at(w: &mut World, day: NaiveDate) {
    w.now = day.and_hms_opt(9, 0, 0).unwrap().and_utc();
}

/// Purpose: a child's conversion opens only as the facility allows: on AIA Living, from the
/// parent's Critical Conditions, for standalone Critical Conditions or accelerated Critical
/// Conditions with an equal amount of Life Cover, for no more than $50,000, on the policy
/// anniversary after the child turns 21 and within 60 days of it, for one life, with where
/// the claims search is kept. Each other request, and a waiting period reduction made as a
/// facility, is refused at the door as `facility`.
/// Why it matters: the conversion is cover with no health questions. Each limit is the
/// wording's, and a request outside one would be cover the insurer never offered without
/// evidence: a 30-year-old taking it, or $100,000 of it, or Life Cover larger than the trauma.
#[test]
fn a_childs_conversion_opens_only_inside_the_facilitys_limits() {
    let mut w = World::new();
    at(&mut w, d(2026, 4, 20));
    let refused = |w: &mut World, o: OpenProposal| open(w, o).unwrap_err().code;
    let standalone = [("critical-conditions", "trauma", 50_000_00)];
    let accelerated = [("critical-conditions-accelerated", "trauma", 40_000_00), ("life", "life", 40_000_00)];

    let mut wrong_product = child(&w, "C0", &standalone);
    wrong_product.product_code = "aia-living-business".into();
    assert_eq!(refused(&mut w, wrong_product), "facility");
    let mut wrong_source = child(&w, "C0", &standalone);
    if let Entry::Facility { from_cover, .. } = &mut wrong_source.entry {
        *from_cover = "income-agreed-value".into();
    }
    assert_eq!(refused(&mut w, wrong_source), "facility", "not a Critical Conditions cover");
    let unequal = [("critical-conditions-accelerated", "trauma", 40_000_00), ("life", "life", 45_000_00)];
    assert_eq!(
        {
            let o = child(&w, "C0", &unequal);
            refused(&mut w, o)
        },
        "facility",
        "Life Cover larger than the trauma"
    );
    assert_eq!(
        {
            let o = child(&w, "C0", &[("life", "life", 40_000_00)]);
            refused(&mut w, o)
        },
        "facility",
        "Life Cover alone"
    );
    let too_much = [("critical-conditions", "trauma", 50_000_01)];
    assert_eq!(
        {
            let o = child(&w, "C0", &too_much);
            refused(&mut w, o)
        },
        "facility",
        "over $50,000"
    );
    let mut no_birth = child(&w, "C0", &standalone);
    no_birth.lives[0].factors.date_of_birth = None;
    assert_eq!(refused(&mut w, no_birth), "facility");
    let mut too_old = child(&w, "C0", &standalone);
    too_old.lives[0].factors.date_of_birth = Some(d(2004, 3, 10));
    assert_eq!(refused(&mut w, too_old), "facility", "the anniversary after they turned 22");
    let mut too_young = child(&w, "C0", &standalone);
    too_young.lives[0].factors.date_of_birth = Some(d(2005, 4, 2));
    assert_eq!(refused(&mut w, too_young), "facility", "not yet 21 on the anniversary");
    let mut blank = child(&w, "C0", &standalone);
    if let Entry::Facility { evidence, .. } = &mut blank.entry {
        *evidence = " ".into();
    }
    assert_eq!(refused(&mut w, blank), "facility");
    let mut shorter = child(&w, "C0", &standalone);
    if let Entry::Facility { facility, .. } = &mut shorter.entry {
        *facility = Facility::WaitingPeriodReduction;
    }
    assert_eq!(refused(&mut w, shorter), "facility", "a shorter wait has its own entry");
    let mut same = child(&w, "C0", &standalone);
    same.policy = PolicyId::from("POL-PARENT");
    assert_eq!(refused(&mut w, same), "facility", "the child's policy is a new one");

    at(&mut w, d(2026, 3, 31));
    assert_eq!(
        {
            let o = child(&w, "C0", &standalone);
            refused(&mut w, o)
        },
        "facility",
        "before the anniversary"
    );
    at(&mut w, d(2026, 6, 1));
    assert_eq!(
        {
            let o = child(&w, "C0", &standalone);
            refused(&mut w, o)
        },
        "facility",
        "61 days after it"
    );
    at(&mut w, d(2026, 5, 31));
    {
        let o = child(&w, "C1", &standalone);
        open(&mut w, o)
    }
    .unwrap();
    {
        let o = child(&w, "C2", &accelerated);
        open(&mut w, o)
    }
    .unwrap();
}

/// Purpose: an accepted child's conversion opens a case on the financial queue (RC-0979), asks
/// no health questions, and once the person holding it accepts the claims search, the covers
/// are decided with no underwriting (source `facility`) and not offered back, and the policy is
/// issued as a new policy on AIA Living naming the facility it came from. A conversion the
/// person refuses ends withdrawn and the policyholder is told why (RC-0981). Nothing ends on
/// the parent's policy when the child's is issued.
/// Why it matters: whether the child claimed, or could have claimed, the Children's Trauma or
/// the Children's and Maternity Benefit is in the claims records and needs a person; a child
/// who could have claimed is exactly the risk the facility excludes. The driver needs the
/// facility on the issue to check the parent's policy in the core before it writes anything.
#[test]
fn a_childs_conversion_is_issued_once_a_person_accepts_the_claims_search() {
    let mut w = World::new();
    at(&mut w, d(2026, 4, 20));
    let opened = {
        let o = child(&w, "C1", &[("critical-conditions", "trauma", 50_000_00)]);
        open(&mut w, o)
    }
    .unwrap();
    assert!(opened.events.iter().any(|e| matches!(e, Event::Referred { queue: Queue::Financial, .. })));
    let f = &w.p("C1").lives[&LifeId::from("L1")];
    assert!(f.open_case().unwrap().reasons.contains("RC-0979"));
    let d1 = w.disclosure("C1", "L1", &clean());
    assert_eq!(w.err("new-business", Command::SubmitDisclosure(Box::new(d1))), "not_awaiting_disclosure");

    let case = w.take("C1", "L1", "amy");
    let checked = check(&mut w, &case, true, "");
    assert!(checked.events.iter().any(|e| matches!(
        e,
        Event::FacilityChecked { facility: Facility::ChildrensTraumaConversion, accepted: true, by, .. } if by == "amy"
    )));
    let decision = &w.p("C1").lives[&LifeId::from("L1")].decisions[0];
    assert!(matches!(&decision.source, DecisionSource::Facility { facility: Facility::ChildrensTraumaConversion, checked_by } if checked_by == "amy"));
    assert_eq!(decision.reasons, set(&["RC-0979"]));
    let applied = w.automatic(&checked);
    assert_eq!(w.stage("C1", "L1"), Stage::Accepted, "nothing to offer back");
    let Some(Effect::IssuePolicy { command, facility, .. }) = applied.effects.iter().find(|e| matches!(e, Effect::IssuePolicy { .. })) else {
        panic!("no issue: {:?}", applied.effects)
    };
    assert_eq!((command.product_code.as_str(), command.covers[0].sum_insured_minor), ("aia-living", 50_000_00));
    assert!(matches!(facility.as_deref(), Some(Entry::Facility { from_policy, .. }) if from_policy.0 == "POL-PARENT"));
    let issued = w.ok(
        "issuer",
        Command::PolicyIssued {
            proposal: "C1".into(),
            layers: vec![],
        },
    );
    assert!(!issued.effects.iter().any(|e| matches!(e, Effect::EndConverted { .. })));

    {
        let o = child(&w, "C2", &[("critical-conditions", "trauma", 50_000_00)]);
        open(&mut w, o)
    }
    .unwrap();
    let case = w.take("C2", "L1", "amy");
    let refused = check(&mut w, &case, false, "a Children's Trauma claim was paid for this child in 2019");
    assert!(matches!(w.p("C2").status, ProposalStatus::Withdrawn { .. }));
    assert!(refused.events.iter().any(|e| matches!(e, Event::FacilityChecked { accepted: false, .. })));
    assert!(refused.effects.iter().any(|e| matches!(
        e,
        Effect::Notify { kind: NoticeKind::ProposalWithdrawn, reasons, .. } if reasons == &vec!["RC-0981".to_string()]
    )));
}

/// Purpose: Start-Up Income moves only to one Business Continuity cover on AIA Living
/// Business, and only once the business has traded three years; accepted by the person who
/// checks the accounts, it is issued as a new policy, and once that is in force the Start-Up
/// policy is ended in the core under the proposal's key.
/// Why it matters: Start-Up Income is for businesses too young to show the profit Business
/// Continuity needs; a move before three years would be Business Continuity with no evidence
/// for a business that cannot yet qualify. Left running, the Start-Up policy would bill and
/// pay a second income benefit for the same business.
#[test]
fn start_up_income_moves_to_business_continuity_after_three_years_of_trading() {
    let mut w = World::new();
    let today = w.today();
    let three = today.checked_sub_months(chrono::Months::new(36)).unwrap();
    let refused = |w: &mut World, o: OpenProposal| open(w, o).unwrap_err().code;
    assert_eq!(
        {
            let o = business(&w, "B0", "business-continuity-agreed-value", three + chrono::Duration::days(1));
            refused(&mut w, o)
        },
        "facility",
        "a day short of three years"
    );
    assert_eq!(
        {
            let o = business(&w, "B0", "rural-continuity", three);
            refused(&mut w, o)
        },
        "facility"
    );
    let mut wrong_product = business(&w, "B0", "business-continuity-agreed-value", three);
    wrong_product.product_code = "aia-living".into();
    assert_eq!(refused(&mut w, wrong_product), "facility");
    let mut two = business(&w, "B0", "business-continuity-agreed-value", three);
    two.covers.push(two.covers[0].clone());
    two.covers[1].code = "business-continuity-indemnity".into();
    assert_eq!(refused(&mut w, two), "facility", "one cover");

    {
        let o = business(&w, "B1", "business-continuity-indemnity", three);
        open(&mut w, o)
    }
    .unwrap();
    let case = w.take("B1", "L1", "amy");
    let checked = check(&mut w, &case, true, "");
    let applied = w.automatic(&checked);
    assert!(applied.effects.iter().any(|e| matches!(e, Effect::IssuePolicy { facility: Some(_), .. })));
    let issued = w.ok(
        "issuer",
        Command::PolicyIssued {
            proposal: "B1".into(),
            layers: vec![],
        },
    );
    let ended: Vec<_> = issued.effects.iter().filter(|e| matches!(e, Effect::EndConverted { .. })).collect();
    assert!(
        matches!(ended[..], [Effect::EndConverted { key, group, policy, .. }] if key == "B1/end-converted" && group.0 == "grp-start-up" && policy.0 == "POL-START-UP"),
        "{ended:?}"
    );
}

/// Purpose: a policy's anniversaries fall on its day and month in later years; one that
/// started on 29 February has its anniversary on 28 February in a year with no 29th; the
/// start itself is not an anniversary.
/// Why it matters: a child's window opens on the anniversary, and the driver refuses a
/// conversion whose stated anniversary is not one of the parent's policy.
#[test]
fn a_policys_anniversaries_fall_on_its_day_and_month() {
    assert!(is_anniversary(d(2020, 4, 1), d(2026, 4, 1)));
    assert!(!is_anniversary(d(2020, 4, 1), d(2026, 4, 2)));
    assert!(!is_anniversary(d(2020, 4, 1), d(2020, 4, 1)));
    assert!(is_anniversary(d(2024, 2, 29), d(2025, 2, 28)));
    assert!(is_anniversary(d(2024, 2, 29), d(2028, 2, 29)));
    assert!(!is_anniversary(d(2024, 2, 29), d(2028, 2, 28)));
}

/// A policy in force in a ledger: `cover` of `sum` on L1, with the given loading.
fn held(product: &str, cover: &str, kind: &str, sum: i64, inception: NaiveDate, loading: Option<&str>) -> ic_core::policy::Policy {
    let mut cluster = ic_sim::Cluster::in_memory(1, 7);
    cluster.open(inception).unwrap();
    for activation in ic_sim::config::shipped_activations().unwrap() {
        cluster.activate_everywhere(activation).unwrap();
    }
    let definition = cluster.state(0).catalog.product_on(product, inception).unwrap().clone();
    let spec = definition.cover(cover).unwrap();
    let option = if spec.option_axes.is_empty() {
        None
    } else {
        let parts: Vec<(&str, &str)> = spec.option_axes.iter().map(|a| (a.code.as_str(), a.choices[0].code.as_str())).collect();
        Some(ic_product::option_code(&parts))
    };
    let issue = IssuePolicy {
        continuation_of: None,
        policy: "POL-SOURCE".into(),
        number: "N-SOURCE".into(),
        group: GroupId::from("grp-source"),
        policyholder: "PTY-L1".into(),
        product_code: product.into(),
        inception,
        frequency: ic_product::Frequency::Monthly,
        lives: vec![LifeSpec {
            id: "L1".into(),
            kind: LifeKind::Named { party: "PTY-L1".into() },
            factors: ic_core::LifeFactors {
                date_of_birth: Some(d(1980, 1, 1)),
                sex: Some(ic_product::Sex::Female),
                smoker: Some(false),
                occupation_class: Some("1".into()),
                ..Default::default()
            },
        }],
        covers: vec![CoverSpec {
            excess_option: option,
            code: cover.into(),
            life: Some("L1".into()),
            sum_insured_minor: sum,
            loadings: loading.map(|l| vec![DecStr::parse(l).unwrap()]).unwrap_or_default(),
            exclusions: if loading.is_some() { vec!["EX-1001".into()] } else { vec![] },
        }],
        migrated: false,
    };
    let _ = kind;
    let applied = cluster.submit_keyed(0, "issue", "new-business", ic_core::Command::IssuePolicy(Box::new(issue)));
    assert!(applied.result.is_ok(), "{:?}", applied.result);
    cluster.state(0).policies[&PolicyId::from("POL-SOURCE")].clone()
}

fn new_policy(cover: &str, sum: i64) -> IssuePolicy {
    IssuePolicy {
        continuation_of: None,
        policy: "POL-NEW".into(),
        number: "N-NEW".into(),
        group: GroupId::from("grp-new"),
        policyholder: "PTY-L1".into(),
        product_code: "x".into(),
        inception: d(2028, 1, 1),
        frequency: ic_product::Frequency::Monthly,
        lives: vec![],
        covers: vec![CoverSpec {
            excess_option: None,
            code: cover.into(),
            life: Some("L1".into()),
            sum_insured_minor: sum,
            loadings: vec![],
            exclusions: vec![],
        }],
        migrated: false,
    }
}

fn facility(facility: Facility, from_cover: &str, from_life: &str, trigger: NaiveDate) -> Entry {
    Entry::Facility {
        facility,
        from_policy: "POL-SOURCE".into(),
        from_group: "grp-source".into(),
        from_life: from_life.into(),
        from_cover: from_cover.into(),
        trigger_date: trigger,
        evidence: "vault://x".into(),
    }
}

/// Purpose: before a child's policy is issued, the parent's policy as the core holds it must
/// be in force with the Critical Conditions cover on the parent's life, the child's amount must
/// be within half of it (a $100,000 cover allows $50,000, not $50,000.01), and the stated
/// anniversary must be one of that policy's. Each failure is refused with its own code.
/// Why it matters: the request's facts come from a screen; the core holds the truth. A
/// conversion from a cover that has ended, or for more than the parent's cover allows, is
/// cover with no evidence the wording does not give.
#[test]
fn a_childs_conversion_is_checked_against_the_parents_policy_in_the_core() {
    let parent = held("aia-living", "critical-conditions", "trauma", 100_000_00, d(2027, 2, 1), None);
    let today = d(2028, 2, 20);
    let entry = facility(Facility::ChildrensTraumaConversion, "critical-conditions", "L1", d(2028, 2, 1));
    assert_eq!(verify(&entry, &parent, &mut new_policy("critical-conditions", 50_000_00), today), Ok(()));
    assert_eq!(
        verify(&entry, &parent, &mut new_policy("critical-conditions", 50_000_01), today),
        Err("facility_amount".into())
    );
    let off = facility(Facility::ChildrensTraumaConversion, "critical-conditions", "L1", d(2028, 2, 2));
    assert_eq!(
        verify(&off, &parent, &mut new_policy("critical-conditions", 10_000_00), today),
        Err("facility_not_an_anniversary".into())
    );
    let other_life = facility(Facility::ChildrensTraumaConversion, "critical-conditions", "L2", d(2028, 2, 1));
    assert_eq!(
        verify(&other_life, &parent, &mut new_policy("critical-conditions", 10_000_00), today),
        Err("facility_cover_not_held".into())
    );
    let other_cover = facility(Facility::ChildrensTraumaConversion, "critical-conditions-accelerated", "L1", d(2028, 2, 1));
    assert_eq!(
        verify(&other_cover, &parent, &mut new_policy("critical-conditions", 10_000_00), today),
        Err("facility_cover_not_held".into())
    );
    let mut ended = parent.clone();
    ended.status = PolicyStatus::Cancelled;
    assert_eq!(
        verify(&entry, &ended, &mut new_policy("critical-conditions", 10_000_00), today),
        Err("facility_policy_not_in_force".into())
    );
    assert_eq!(verify(&Entry::NewBusiness, &ended, &mut new_policy("life", 1), today), Ok(()), "not a facility");
    let shorter = facility(Facility::WaitingPeriodReduction, "critical-conditions", "L1", d(2028, 2, 1));
    assert_eq!(
        verify(&shorter, &parent, &mut new_policy("critical-conditions", 1), today),
        Err("facility_not_a_new_policy".into()),
        "a shorter wait writes no new policy"
    );
}

/// Purpose: Business Continuity is issued for no more monthly benefit than the Start-Up cover
/// it moves from, and carries that cover's loadings and exclusions (once each).
/// Why it matters: the move is with no evidence on the terms the life was accepted on. A
/// larger benefit would be new cover with no underwriting, and dropping a loading or an
/// exclusion would give the business owner cover the insurer refused them.
#[test]
fn business_continuity_takes_the_start_up_covers_terms_and_no_more_benefit() {
    let start_up = held(
        "aia-start-up-income",
        "start-up-income",
        "income_protection",
        3_000_00,
        d(2027, 2, 1),
        Some("0.50"),
    );
    let today = d(2027, 3, 1);
    let entry = facility(Facility::BusinessContinuity, "start-up-income", "L1", d(2023, 1, 1));
    let mut moved = new_policy("business-continuity-agreed-value", 3_000_00);
    assert_eq!(verify(&entry, &start_up, &mut moved, today), Ok(()));
    assert_eq!(
        (moved.covers[0].loadings.clone(), moved.covers[0].exclusions.clone()),
        (vec![DecStr::parse("0.50").unwrap()], vec!["EX-1001".to_string()])
    );
    assert_eq!(verify(&entry, &start_up, &mut moved, today), Ok(()));
    assert_eq!(moved.covers[0].loadings.len() + moved.covers[0].exclusions.len(), 2, "once each");
    assert_eq!(
        verify(&entry, &start_up, &mut new_policy("business-continuity-agreed-value", 3_000_01), today),
        Err("facility_amount".into())
    );
}
