//! Persona tests for the product lines: rows of the persona test matrix
//! (`docs/testing/personas/`) that cross the core and a module, run against the
//! real core. Everyone here is invented.

use chrono::NaiveDate;
use ic_core::claims_module as claims;
use ic_core::command::ExcessLayer;
use ic_core::{Command, LifeId, PolicyId, Reject};
use ic_money::{Currency::NZD, Money};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{group_scheme, issue_command, member};
use ic_sim::Cluster;

use claims::command::Notification;
use claims::config::{Profile, Staff};
use claims::{Actor, AuthorityBook, Channel, ClaimType, Flags, Line, Notifier, Queue, Role, Rules};

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn actor() -> Actor {
    Actor {
        staff: "sam".to_string(),
        purpose: "working the claim".to_string(),
    }
}

fn authority() -> AuthorityBook {
    let senior = Profile {
        lines: [Line::LumpSum].into(),
        max_admission_minor: [(ClaimType::Death, i64::MAX)].into(),
        max_payment_minor: i64::MAX,
        max_reserve_minor: i64::MAX,
        senior: true,
        ..Profile::default()
    };
    let sam = Staff {
        profile: "senior".to_string(),
        roles: [Role::Assessor].into(),
        queues: [Queue::Group, Queue::LumpSum, Queue::ComplexLarge].into(),
        wip_limit: 0,
        vulnerable_trained: true,
        related_parties: Default::default(),
    };
    AuthorityBook {
        version: 1,
        profiles: [("senior".to_string(), senior)].into(),
        staff: [("sam".to_string(), sam)].into(),
        prepared_by: "config-ann".into(),
        approved_by: vec!["config-bob".into(), "config-cat".into()],
    }
}

fn wrap(command: claims::Command) -> Command {
    Command::Claims(Box::new(command))
}

fn cluster(open: NaiveDate) -> Cluster {
    let mut c = Cluster::in_memory(1, 42);
    c.open(open).unwrap();
    for cmd in shipped_activations().unwrap() {
        c.activate_everywhere(cmd).unwrap();
    }
    let rules = Rules {
        version: 1,
        escalation_days: [1, 3, 7],
        large_claim_minor: i64::MAX,
        two_signature_admission_minor: i64::MAX,
        prepared_by: "config-ann".into(),
        approved_by: vec!["config-bob".into(), "config-cat".into()],
        ..Rules::default()
    };
    c.activate_everywhere(wrap(claims::Command::ActivateRules { rules: Box::new(rules) })).unwrap();
    c.activate_everywhere(wrap(claims::Command::ActivateAuthority { book: Box::new(authority()) }))
        .unwrap();
    c
}

fn run(c: &mut Cluster, command: claims::Command) -> Result<(), Reject> {
    c.submit(0, "claims", wrap(command))
}

/// The sum insured the claims module's cover check is given for a scheme member's death
/// claim on `event_date`.
fn checked_sum_insured(c: &mut Cluster, claim: &str, policy: &PolicyId, life: &str, event_date: NaiveDate) -> i64 {
    run(
        c,
        claims::Command::Notify(Box::new(Notification {
            claim: claim.into(),
            policy: policy.0.clone(),
            cover: "group-life".into(),
            life: Some(life.into()),
            claim_type: ClaimType::Death,
            event_date,
            channel: Channel::EmployerPortal,
            notifier: Notifier::Employer,
            claimant: Some("party-estate".into()),
            claimed_minor: None,
            flags: Flags {
                group: true,
                ..Flags::default()
            },
            health: None,
            underwriters: vec![],
            catastrophe_event: None,
            cause_code: None,
        })),
    )
    .unwrap();
    run(
        c,
        claims::Command::Assign {
            claim: claim.into(),
            to: "sam".into(),
            actor: actor(),
        },
    )
    .unwrap();
    run(
        c,
        claims::Command::CoverCheck {
            claim: claim.into(),
            relied_on: vec![],
            actor: actor(),
        },
    )
    .unwrap();
    let facts = c.state(0).claims.claims[claim].cover_check.clone().unwrap().facts;
    assert!(facts.in_force, "the member was covered on the event date");
    facts.sum_insured_minor
}

/// Purpose: matrix CLM-025 and GRP-009 (E1 Kai Fresh Supermarkets' pilot, whose formula cover is
/// above the scheme's automatic acceptance limit). Without an underwritten layer the cover check
/// gives the claim only the automatic amount; once underwriting has accepted the excess and group
/// administration has written it as a layer, the same check gives the automatic amount plus the
/// layer.
/// Why it matters: the amount above the automatic limit is paid only where it was underwritten.
/// A check that ignored the limit would pay cover nobody accepted; one that ignored the accepted
/// layer would short-pay a family for cover the employer was billed for.
#[test]
fn a_pilots_claim_is_checked_on_the_automatic_amount_plus_any_underwritten_layer() {
    let mut c = cluster(d(2027, 11, 1));
    // An executive's cover is four times salary: 960,000 on 240,000, against a 750,000 limit.
    let issue = group_scheme(30, d(2027, 11, 1), vec![member("pilot-1", d(1979, 4, 4), "executive", 240_000_00)]);
    let id = issue.policy.clone();
    c.submit(0, "new-business", issue_command(issue)).unwrap();
    c.submit(
        0,
        "cash-matching",
        Command::ApplyReceipt {
            policy: id.clone(),
            receipt: "r-1".into(),
            amount: Money::new(10_000_00, NZD),
        },
    )
    .unwrap();
    c.end_of_day().unwrap();
    let today = c.business_date().unwrap();
    assert_eq!(checked_sum_insured(&mut c, "clm-1", &id, "pilot-1", today), 750_000_00);

    let layer = ExcessLayer {
        continues: None,
        policy: id.clone(),
        cover: "group-life".into(),
        life: LifeId("pilot-1".into()),
        start: d(2027, 11, 1),
        sum_insured_minor: 210_000_00,
        loadings: vec![],
        exclusions: vec![],
        loading_itemised: false,
        member_pays: Default::default(),
        reference: "uw-1".into(),
    };
    c.submit(0, "group-admin", Command::AddExcessLayer(Box::new(layer))).unwrap();
    assert_eq!(checked_sum_insured(&mut c, "clm-2", &id, "pilot-1", today), 960_000_00);
}
