//! Group scheme cover beyond the salary formula: underwritten layers above
//! the automatic amount, a ceiling on one member's automatic cover, pools
//! billed from declarations, reinstating a member removed in error, and the
//! dry run an employer's preview is built on.

use chrono::NaiveDate;
use ic_core::command::{ExcessLayer, LifeSpec, MemberChange};
use ic_core::event::Event;
use ic_core::{preview_membership, Command, LayerOrigin, LifeFactors, LifeId, LifeKind, PartyId, PolicyId, PolicyStatus, PoolAggregate, Reject};
use ic_ledger::PartitionId;
use ic_money::DecStr;
use ic_product::{Frequency, Sex};
use ic_readmodel::Projections;
use ic_sim::config::shipped_activations;
use ic_sim::persona::{group_scheme, issue_command, member, term_life};
use ic_sim::Cluster;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn cluster(open: NaiveDate) -> Cluster {
    let mut c = Cluster::in_memory(1, 7);
    c.open(open).unwrap();
    for cmd in shipped_activations().unwrap() {
        c.activate_everywhere(cmd).unwrap();
    }
    c
}

fn days(c: &mut Cluster, n: u32) {
    for _ in 0..n {
        let close = c.end_of_day().unwrap();
        assert!(close.breaks.is_empty(), "reconciliation breaks on {}: {:?}", close.closed, close.breaks);
    }
}

fn life(id: &str) -> LifeId {
    LifeId(id.to_string())
}

/// A scheme from 1 November 2027: one staff member on 60,000 (120,000 of
/// cover, 11.00 a month) and one executive on 240,000, whose formula gives
/// 960,000 and whose automatic cover stops at the 750,000 limit (90.62).
fn scheme(c: &mut Cluster) -> (PartitionId, PolicyId) {
    let issue = group_scheme(
        20,
        d(2027, 11, 1),
        vec![
            member("staff-1", d(1985, 1, 1), "staff", 60_000_00),
            member("exec-1", d(1975, 1, 1), "executive", 240_000_00),
        ],
    );
    let (p, id) = (c.partition_of(&issue.group), issue.policy.clone());
    c.submit(p, "new-business", issue_command(issue)).unwrap();
    assert_eq!(owed(c, p, &id), 11_00 + 90_62);
    (p, id)
}

fn owed(c: &Cluster, p: PartitionId, id: &PolicyId) -> i64 {
    c.state(p).policies[id].outstanding_minor()
}

fn excess(id: &PolicyId, start: NaiveDate, loading: Option<&str>, itemised: bool) -> ExcessLayer {
    ExcessLayer {
        continues: None,
        policy: id.clone(),
        cover: "group-life".into(),
        life: life("exec-1"),
        start,
        sum_insured_minor: 210_000_00,
        loadings: loading.map(|l| DecStr::parse(l).unwrap()).into_iter().collect(),
        exclusions: vec![],
        loading_itemised: itemised,
        member_pays: Default::default(),
        reference: "uw-1".into(),
    }
}

fn add(c: &mut Cluster, p: PartitionId, layer: ExcessLayer) -> Result<(), Reject> {
    c.submit(p, "group-admin", Command::AddExcessLayer(Box::new(layer)))
}

fn membership(c: &mut Cluster, p: PartitionId, id: &PolicyId, changes: Vec<MemberChange>) -> Result<(), Reject> {
    c.submit(p, "group-admin", Command::ApplyMembership { policy: id.clone(), changes })
}

/// The lines and the unshown loading of the last complete invoice among the events.
fn last_invoice(events: &[Event]) -> (Vec<(LifeId, i64)>, i64) {
    events
        .iter()
        .rev()
        .find_map(|e| match e {
            Event::SchemeInvoiceChunk {
                lines,
                loading_minor,
                complete,
                ..
            } if *complete => Some((lines.clone(), *loading_minor)),
            _ => None,
        })
        .expect("an invoice was built")
}

fn events(c: &mut Cluster) -> Vec<Event> {
    c.take_events().into_iter().map(|r| r.event).collect()
}

/// Purpose: an underwritten layer above the automatic amount is billed with the scheme at
/// the scheme's rate from its start date, and its event says nothing about its terms.
/// Why it matters: cover above the limit that is accepted but never billed is premium the
/// insurer never collects; and a loading in an event would tell every consumer of the
/// stream something about a member's health.
#[test]
fn an_underwritten_layer_is_billed_with_the_scheme_from_its_start() {
    let mut c = cluster(d(2027, 11, 1));
    let (p, id) = scheme(&mut c);
    c.take_events();
    add(&mut c, p, excess(&id, d(2027, 11, 1), None, false)).unwrap();
    // 750,000 + 210,000 at 1.45 per thousand = 1,392 a year = 116.00 a month; 90.62 was billed.
    assert_eq!(owed(&c, p, &id), 11_00 + 116_00);
    let seen = events(&mut c);
    let added = seen.iter().find_map(|e| match e {
        Event::LayerAdded { layer, origin, .. } => Some((layer.clone(), *origin)),
        _ => None,
    });
    let (layer, origin) = added.expect("the layer is announced");
    assert_eq!(origin, LayerOrigin::UnderwrittenExcess);
    assert_eq!((layer.life, layer.sum_insured_minor, layer.start), (life("exec-1"), 210_000_00, d(2027, 11, 1)));
    assert!(seen
        .iter()
        .any(|e| matches!(e, Event::PremiumAdjusted { life: l, total_minor, .. } if *l == life("exec-1") && *total_minor == 116_00 - 90_62)));

    days(&mut c, 30);
    let (lines, loading) = last_invoice(&events(&mut c));
    assert_eq!(lines, vec![(life("exec-1"), 116_00), (life("staff-1"), 11_00)]);
    assert_eq!(loading, 0);
    assert_eq!(owed(&c, p, &id), 2 * (11_00 + 116_00));
}

/// Purpose: a loading on an underwritten layer is charged to the scheme but kept off the
/// member's invoice line unless the member agreed to show it, both on the invoice and on
/// a backdated adjustment.
/// Why it matters: an employer who can see one member's premium jump has been told that
/// member's health. The design allows the loading to reach the employer only inside one
/// total, or itemised with the member's recorded consent.
#[test]
fn a_loading_reaches_the_employer_only_in_a_total_unless_the_member_agreed() {
    let mut c = cluster(d(2027, 11, 1));
    let (p, id) = scheme(&mut c);
    c.take_events();
    add(&mut c, p, excess(&id, d(2027, 11, 1), Some("1"), false)).unwrap();
    // The layer at double rate: 1,087.50 + 609.00 = 1,696.50 a year = 141.38 a month.
    assert_eq!(owed(&c, p, &id), 11_00 + 141_38);
    let seen = events(&mut c);
    let shown: i64 = seen
        .iter()
        .filter_map(|e| match e {
            Event::PremiumAdjusted { total_minor, .. } => Some(*total_minor),
            _ => None,
        })
        .sum();
    let unshown: i64 = seen
        .iter()
        .filter_map(|e| match e {
            Event::LoadingAdjusted { total_minor, .. } => Some(*total_minor),
            _ => None,
        })
        .sum();
    assert_eq!((shown, unshown), (116_00 - 90_62, 141_38 - 116_00));
    let text = serde_json::to_string(&seen).unwrap();
    assert!(!text.contains("loadings"), "no event carries underwriting terms: {text}");

    days(&mut c, 30);
    let (lines, loading) = last_invoice(&events(&mut c));
    assert_eq!(lines, vec![(life("exec-1"), 116_00), (life("staff-1"), 11_00)]);
    assert_eq!(loading, 141_38 - 116_00);

    // With the member's agreement the same layer shows on the member's own line.
    let mut c = cluster(d(2027, 11, 1));
    let (p, id) = scheme(&mut c);
    add(&mut c, p, excess(&id, d(2027, 11, 1), Some("1"), true)).unwrap();
    c.take_events();
    days(&mut c, 30);
    let seen = events(&mut c);
    assert!(!seen.iter().any(|e| matches!(e, Event::LoadingAdjusted { .. })));
    let (lines, loading) = last_invoice(&seen);
    assert_eq!(lines, vec![(life("exec-1"), 141_38), (life("staff-1"), 11_00)]);
    assert_eq!(loading, 0);
}

/// Purpose: a layer started or ended part way through a billed period is charged or
/// credited for exactly the days it applies, and ending it is announced.
/// Why it matters: underwriting decisions arrive weeks after the cover date, and members
/// give cover up mid-month. Whole-period charging in either direction is a wrong invoice.
#[test]
fn a_layer_started_or_ended_mid_period_is_charged_for_its_days() {
    let mut c = cluster(d(2027, 11, 1));
    let (p, id) = scheme(&mut c);
    days(&mut c, 20); // 21 November
    let before = owed(&c, p, &id);
    add(&mut c, p, excess(&id, d(2027, 11, 16), None, false)).unwrap();
    // 15 of November's 30 days at 25.37 a month more.
    assert_eq!(owed(&c, p, &id) - before, (116_00 - 90_62) / 2);
    c.take_events();
    c.submit(
        p,
        "underwriting",
        Command::EndExcessLayer {
            policy: id.clone(),
            cover: "group-life".into(),
            layer: 1,
            effective: d(2027, 11, 16),
        },
    )
    .unwrap();
    assert_eq!(owed(&c, p, &id), before, "ended from its first day, the layer cost nothing");
    let seen = events(&mut c);
    assert!(seen.iter().any(|e| matches!(
        e,
        Event::LayerEnded { layer: 1, life: l, effective, sum_insured_minor: 210_000_00, .. } if *l == life("exec-1") && *effective == d(2027, 11, 16)
    )));
    days(&mut c, 10);
    assert_eq!(owed(&c, p, &id), before + 11_00 + 90_62, "December is billed without the ended layer");
}

/// Purpose: a leaver's underwritten layer ends with them, and reinstating a member removed
/// in error restores the member and the layer from the same day with no gap and no net
/// premium change.
/// Why it matters: this is the remedy for a membership file that removed people who had
/// not left. If cover had a gap, a claim in it would be declined for the employer's
/// mistake; if the invoice did not net to zero, the employer would pay for our correction.
#[test]
fn a_member_removed_in_error_comes_back_with_no_gap() {
    let mut c = cluster(d(2027, 11, 1));
    let (p, id) = scheme(&mut c);
    add(&mut c, p, excess(&id, d(2027, 11, 1), Some("1"), false)).unwrap();
    days(&mut c, 35); // December is billed
    let before = owed(&c, p, &id);
    let hash_of_lives = serde_json::to_string(&c.state(p).policies[&id].covers).unwrap();
    c.take_events();

    membership(
        &mut c,
        p,
        &id,
        vec![MemberChange::Left {
            life: life("exec-1"),
            effective: d(2027, 11, 16),
        }],
    )
    .unwrap();
    // Half of November and all of December come back, loading included.
    assert_eq!(before - owed(&c, p, &id), 141_38 / 2 + 141_38);
    let seen = events(&mut c);
    assert!(seen
        .iter()
        .any(|e| matches!(e, Event::LayerEnded { layer: 1, effective, .. } if *effective == d(2027, 11, 16))));
    assert_eq!(c.state(p).policies[&id].covers["group-life"].layers[0].end, Some(d(2027, 11, 16)));

    membership(&mut c, p, &id, vec![MemberChange::Reinstated { life: life("exec-1") }]).unwrap();
    assert_eq!(owed(&c, p, &id), before, "the correction nets to nothing");
    let pol = &c.state(p).policies[&id];
    assert_eq!(pol.lives[&life("exec-1")].left, None);
    assert!(pol.lives[&life("exec-1")].in_force_on(d(2027, 11, 20)), "cover was never absent");
    assert_eq!(serde_json::to_string(&pol.covers).unwrap(), hash_of_lives);
    let seen = events(&mut c);
    assert!(seen.iter().any(|e| matches!(
        e,
        Event::MemberJoined { life: l, effective, covers, .. } if *l == life("exec-1") && *effective == d(2027, 11, 16) && covers[0].sum_insured_minor == 750_000_00
    )));
    assert!(seen
        .iter()
        .any(|e| matches!(e, Event::LayerAdded { layer, origin: LayerOrigin::UnderwrittenExcess, .. } if layer.layer == 1)));

    // Only someone who has left can be reinstated, and only inside the backdating window.
    let again = membership(&mut c, p, &id, vec![MemberChange::Reinstated { life: life("exec-1") }]);
    assert_eq!(again.unwrap_err().code, "not_left");
    let nobody = membership(&mut c, p, &id, vec![MemberChange::Reinstated { life: life("nobody") }]);
    assert_eq!(nobody.unwrap_err().code, "not_left");
    membership(
        &mut c,
        p,
        &id,
        vec![MemberChange::Left {
            life: life("staff-1"),
            effective: d(2027, 11, 2),
        }],
    )
    .unwrap();
    // Paid well ahead, so the scheme is still in force when the window has closed.
    c.submit(
        p,
        "cash-matching",
        Command::ApplyReceipt {
            policy: id.clone(),
            receipt: "ahead".into(),
            amount: ic_money::Money::new(5_000_00, ic_money::Currency::NZD),
        },
    )
    .unwrap();
    days(&mut c, 400);
    let late = membership(&mut c, p, &id, vec![MemberChange::Reinstated { life: life("staff-1") }]);
    assert_eq!(late.unwrap_err().code, "backdating_limit");
}

/// Purpose: every way an underwritten layer command can be wrong is refused with its own
/// code and leaves the policy untouched.
/// Why it matters: these commands write cover and money. A layer on the wrong member, the
/// wrong policy or a date nobody approved must fail loudly, not land quietly.
#[test]
fn a_wrong_layer_command_is_refused_and_changes_nothing() {
    let mut c = cluster(d(2027, 11, 1));
    let (p, id) = scheme(&mut c);
    let retail = term_life(
        21,
        d(2027, 11, 1),
        d(1990, 3, 10),
        Sex::Female,
        false,
        500_000_00,
        None,
        Frequency::Monthly,
        None,
    );
    let retail_id = retail.policy.clone();
    c.submit(p, "new-business", issue_command(retail)).unwrap();
    let pooled = LifeSpec {
        id: life("pool-1"),
        kind: LifeKind::Pool {
            headcount: 10,
            total_salary_minor: 500_000_00,
        },
        factors: LifeFactors::default(),
    };
    membership(
        &mut c,
        p,
        &id,
        vec![MemberChange::Joined {
            life: pooled,
            effective: d(2027, 11, 1),
        }],
    )
    .unwrap();
    days(&mut c, 3);
    let written = |c: &Cluster| (serde_json::to_string(&c.state(p).policies).unwrap(), c.state(p).ledger.entry_count());
    let state_before = written(&c);

    let base = excess(&id, d(2027, 11, 1), None, false);
    let cases: Vec<(&str, ExcessLayer)> = vec![
        (
            "unknown_cover",
            ExcessLayer {
                cover: "trauma".into(),
                ..base.clone()
            },
        ),
        (
            "unknown_cover",
            ExcessLayer {
                policy: retail_id.clone(),
                cover: "life".into(),
                ..base.clone()
            },
        ),
        (
            "unknown_member",
            ExcessLayer {
                life: life("nobody"),
                ..base.clone()
            },
        ),
        (
            "unknown_member",
            ExcessLayer {
                life: life("pool-1"),
                ..base.clone()
            },
        ),
        (
            "future_dated",
            ExcessLayer {
                start: d(2027, 12, 1),
                ..base.clone()
            },
        ),
        (
            "backdating_limit",
            ExcessLayer {
                start: d(2027, 10, 1),
                ..base.clone()
            },
        ),
        (
            "sum_insured",
            ExcessLayer {
                sum_insured_minor: 0,
                ..base.clone()
            },
        ),
        (
            "invalid_loading",
            ExcessLayer {
                loadings: vec![DecStr::parse("-0.1").unwrap()],
                ..base.clone()
            },
        ),
        (
            "unknown_policy",
            ExcessLayer {
                policy: PolicyId("pol-none".into()),
                ..base.clone()
            },
        ),
    ];
    for (code, layer) in cases {
        assert_eq!(add(&mut c, p, layer).unwrap_err().code, code);
    }
    let end = |c: &mut Cluster, layer: u32, effective: NaiveDate| {
        c.submit(
            p,
            "group-admin",
            Command::EndExcessLayer {
                policy: id.clone(),
                cover: "group-life".into(),
                layer,
                effective,
            },
        )
    };
    assert_eq!(end(&mut c, 9, d(2027, 11, 2)).unwrap_err().code, "unknown_layer");
    let not_theirs = c.submit(p, "claims", Command::AddExcessLayer(Box::new(base.clone())));
    assert_eq!(not_theirs.unwrap_err().code, "caller_not_allowed");
    assert_eq!(written(&c), state_before, "nothing was written by any refusal");

    // A layer cannot be ended before it starts or twice, nor on a scheme that has ended.
    add(
        &mut c,
        p,
        ExcessLayer {
            start: d(2027, 11, 3),
            ..base.clone()
        },
    )
    .unwrap();
    assert_eq!(end(&mut c, 1, d(2027, 11, 2)).unwrap_err().code, "unknown_layer");
    end(&mut c, 1, d(2027, 11, 4)).unwrap();
    assert_eq!(end(&mut c, 1, d(2027, 11, 4)).unwrap_err().code, "unknown_layer");
    c.submit(p, "group-admin", Command::CancelPolicy { policy: id.clone() }).unwrap();
    assert_eq!(c.state(p).policies[&id].status, PolicyStatus::Cancelled);
    assert_eq!(add(&mut c, p, base).unwrap_err().code, "not_in_force");
}

/// Purpose: a ceiling on one member's automatic cover lowers that member's cover and
/// premium from its date, below what the category and the scheme limit would give.
/// Why it matters: where a reinsurer requires part of a member's cover to be underwritten,
/// the automatic cover has to stop at the ceiling at once; otherwise the insurer carries
/// risk it has not ceded and has not assessed.
#[test]
fn a_member_ceiling_caps_automatic_cover() {
    let mut c = cluster(d(2027, 11, 1));
    let (p, id) = scheme(&mut c);
    c.take_events();
    let capped = LifeFactors {
        auto_cover_cap_minor: Some(600_000_00),
        ..c.state(p).policies[&id].lives[&life("exec-1")].factors.as_of(d(2027, 11, 1)).unwrap().clone()
    };
    membership(
        &mut c,
        p,
        &id,
        vec![MemberChange::Changed {
            life: life("exec-1"),
            effective: d(2027, 11, 1),
            factors: capped,
        }],
    )
    .unwrap();
    // 600,000 at 1.45 per thousand = 870 a year = 72.50 a month.
    assert_eq!(owed(&c, p, &id), 11_00 + 72_50);
    let seen = events(&mut c);
    assert!(seen
        .iter()
        .any(|e| matches!(e, Event::MemberChanged { covers, .. } if covers[0].sum_insured_minor == 600_000_00)));
}

/// Purpose: a pool is billed on the aggregate the employer declared, a new declaration
/// adjusts periods already billed from its date, and a pool nobody has declared is not
/// billed at all.
/// Why it matters: pool schemes pay a deposit premium on an estimate and true it up on
/// declaration. If the pool were skipped, or the true-up ignored billed periods, the
/// scheme's premium would simply be wrong for as long as nobody noticed.
#[test]
fn a_pool_is_billed_on_its_declared_aggregate() {
    let mut c = cluster(d(2027, 11, 1));
    let pool = |headcount, total_salary_minor| LifeFactors {
        category: Some("staff".into()),
        pool: Some(PoolAggregate { headcount, total_salary_minor }),
        ..LifeFactors::default()
    };
    let spec = |id: &str, factors| LifeSpec {
        id: life(id),
        kind: LifeKind::Pool {
            headcount: 0,
            total_salary_minor: 0,
        },
        factors,
    };
    // 40 people on 3,000,000 between them: 6,000,000 of cover at 1.10 = 6,600 a year = 550.00 a month.
    let issue = group_scheme(22, d(2027, 11, 1), vec![spec("warehouse", pool(40, 3_000_000_00))]);
    let (p, id) = (c.partition_of(&issue.group), issue.policy.clone());
    let quoted = ic_core::quote(&c.state(p).catalog, issue.clone(), d(2027, 11, 1)).unwrap();
    c.submit(p, "new-business", issue_command(issue)).unwrap();
    assert_eq!(owed(&c, p, &id), 550_00);
    assert_eq!(quoted.total(), 550_00, "the quote and the first bill agree for a pool");
    let seen = events(&mut c);
    assert!(seen.iter().any(|e| matches!(e, Event::PremiumDue { members: Some(40), .. })));

    // An undeclared pool joins and is not billed.
    membership(
        &mut c,
        p,
        &id,
        vec![MemberChange::Joined {
            life: spec("casuals", LifeFactors::default()),
            effective: d(2027, 11, 1),
        }],
    )
    .unwrap();
    assert_eq!(owed(&c, p, &id), 550_00);

    // The declaration for the year arrives on 16 November: 44 people, 3,300,000.
    days(&mut c, 20);
    membership(
        &mut c,
        p,
        &id,
        vec![MemberChange::Changed {
            life: life("warehouse"),
            effective: d(2027, 11, 16),
            factors: pool(44, 3_300_000_00),
        }],
    )
    .unwrap();
    // 605.00 a month from the 16th: half of the 55.00 difference for November.
    assert_eq!(owed(&c, p, &id), 550_00 + 27_50);
    days(&mut c, 10);
    assert_eq!(owed(&c, p, &id), 550_00 + 27_50 + 605_00);

    // The per-person limits apply once per person: 2 people cannot hold more than 2 x 750,000.
    membership(
        &mut c,
        p,
        &id,
        vec![MemberChange::Joined {
            life: spec(
                "directors",
                LifeFactors {
                    category: Some("executive".into()),
                    ..pool(2, 1_000_000_00)
                },
            ),
            effective: d(2027, 12, 1),
        }],
    )
    .unwrap();
    // 4,000,000 by formula, 1,500,000 by the limit, at 1.45 = 2,175 a year = 181.25 a month.
    assert_eq!(owed(&c, p, &id), 550_00 + 27_50 + 605_00 + 181_25);
}

/// Purpose: the dry run of a membership chunk reports the cover and the premium effect
/// that applying the same chunk then produces, refuses what applying would refuse, and
/// writes nothing.
/// Why it matters: an employer approves a file on the strength of its preview. A preview
/// computed by different code, or one that leaves a trace, would make that approval
/// meaningless.
#[test]
fn the_dry_run_of_a_membership_chunk_matches_what_applying_it_does() {
    let mut c = cluster(d(2027, 11, 1));
    let (p, id) = scheme(&mut c);
    add(&mut c, p, excess(&id, d(2027, 11, 1), Some("1"), false)).unwrap();
    days(&mut c, 14);
    let changes = vec![
        MemberChange::Joined {
            life: member("staff-2", d(1990, 1, 1), "staff", 60_000_00),
            effective: d(2027, 11, 1),
        },
        MemberChange::Left {
            life: life("exec-1"),
            effective: d(2027, 11, 16),
        },
    ];
    let hash = c.state(p).state_hash();
    let preview = preview_membership(c.state(p), &id, changes.clone()).unwrap();
    assert_eq!(c.state(p).state_hash(), hash, "a dry run leaves no trace");
    assert_eq!(preview.changes.len(), 2);
    assert_eq!(preview.changes[0].covers[0].sum_insured_minor, 120_000_00);
    assert_eq!(preview.changes[0].adjustment_minor, 11_00);
    assert!(preview.changes[1].covers.is_empty());
    // The leaver's line shows the scheme rate only; the total carries the loading too.
    assert_eq!(preview.changes[1].adjustment_minor, -(116_00 / 2));
    assert_eq!(preview.adjustment_total_minor, 11_00 - 141_38 / 2);

    let before = owed(&c, p, &id);
    membership(&mut c, p, &id, changes.clone()).unwrap();
    assert_eq!(owed(&c, p, &id) - before, preview.adjustment_total_minor);

    // The same refusals as the command itself.
    let refused = |c: &Cluster, policy: &PolicyId, changes: Vec<MemberChange>| preview_membership(c.state(p), policy, changes).unwrap_err().code;
    assert_eq!(refused(&c, &id, changes.clone()), "member_exists");
    assert_eq!(refused(&c, &PolicyId("pol-none".into()), vec![]), "unknown_policy");
    let too_many = (0..101)
        .map(|i| MemberChange::Left {
            life: life(&format!("m{i}")),
            effective: d(2027, 11, 2),
        })
        .collect();
    assert_eq!(refused(&c, &id, too_many), "apply_budget");
    let unopened = Cluster::in_memory(1, 7);
    assert_eq!(preview_membership(unopened.state(0), &id, vec![]).unwrap_err().code, "business_date");
    c.submit(
        p,
        "transfer",
        Command::BeginTransfer {
            policy: id.clone(),
            transfer_id: "t1".into(),
            target_group: ic_core::GroupId("grp-elsewhere".into()),
            target_partition: 0,
        },
    )
    .unwrap();
    assert_eq!(refused(&c, &id, vec![]), "policy_transferring");
}

/// Purpose: a member who joins as a named person reaches the event stream and the life
/// index with that person attached, and group administration may issue and end policies.
/// Why it matters: reinsurance accumulates cover per person across policies. A named
/// joiner with no person on the event is invisible to it, and the insurer can exceed its
/// retention on one life without knowing. Continuation and scheme termination both need
/// group administration to be an allowed sender.
#[test]
fn a_named_joiner_carries_the_person_to_the_stream_and_the_index() {
    let mut c = cluster(d(2027, 11, 1));
    let (p, id) = scheme(&mut c);
    let mut model = Projections::new();
    for r in c.take_events() {
        model.apply(&r).unwrap();
    }
    let named = LifeSpec {
        id: life("E100"),
        kind: LifeKind::Named {
            party: PartyId("party-77".into()),
        },
        factors: LifeFactors {
            date_of_birth: Some(d(1988, 4, 4)),
            category: Some("staff".into()),
            salary_minor: Some(60_000_00),
            ..LifeFactors::default()
        },
    };
    let linked = LifeSpec {
        id: life("E101"),
        kind: LifeKind::UnnamedMember {
            member_ref: "E101".into(),
            linked_party: Some(PartyId("party-78".into())),
        },
        ..named.clone()
    };
    membership(
        &mut c,
        p,
        &id,
        vec![
            MemberChange::Joined {
                life: named,
                effective: d(2027, 11, 1),
            },
            MemberChange::Joined {
                life: linked,
                effective: d(2027, 11, 1),
            },
        ],
    )
    .unwrap();
    let records = c.take_events();
    let parties: Vec<Option<PartyId>> = records
        .iter()
        .filter_map(|r| match &r.event {
            Event::MemberJoined { party, .. } => Some(party.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(parties, vec![Some(PartyId("party-77".into())), Some(PartyId("party-78".into()))]);
    for r in &records {
        model.apply(r).unwrap();
    }
    assert_eq!(model.party_of(&id, &life("E100")), Some(&PartyId("party-77".into())));
    assert_eq!(model.layers_on_party(&PartyId("party-77".into())).len(), 1);

    // An underwritten layer shows in the index while it runs and leaves it when it ends.
    add(&mut c, p, excess(&id, d(2027, 11, 1), None, false)).unwrap();
    for r in c.take_events() {
        model.apply(&r).unwrap();
    }
    let layer_rows = |m: &Projections| m.layers_on_policy(&id).iter().filter(|r| r.layer == 1).count();
    assert_eq!(layer_rows(&model), 1);
    c.submit(
        p,
        "group-admin",
        Command::EndExcessLayer {
            policy: id.clone(),
            cover: "group-life".into(),
            layer: 1,
            effective: d(2027, 11, 1),
        },
    )
    .unwrap();
    for r in c.take_events() {
        model.apply(&r).unwrap();
    }
    assert_eq!(layer_rows(&model), 0);

    // Group administration issues the retail policy of a continuation option.
    let retail = term_life(
        23,
        d(2027, 11, 1),
        d(1990, 3, 10),
        Sex::Female,
        false,
        100_000_00,
        None,
        Frequency::Monthly,
        None,
    );
    c.submit(p, "group-admin", issue_command(retail)).unwrap();
}

/// Purpose: on a scheme priced per person rather than per thousand of cover, a pool is
/// charged once for each person declared, and an underwritten layer is refused.
/// Why it matters: a flat rate charged once for a pool of forty undercharges by a factor of
/// forty, and a layer on a cover with no price per amount would be cover given away.
#[test]
fn a_scheme_priced_per_person_charges_a_pool_per_head_and_takes_no_layers() {
    let mut c = cluster(d(2027, 11, 1));
    let mut flat = ic_product::load_product("group_life.star", include_str!("../../../products/group_life.star")).unwrap();
    flat.code = "group-life-flat".into();
    flat.content_hash = "flat-rate-test-version".into();
    flat.covers[0].rating.basis = ic_product::RateBasis::PerLife;
    c.activate_everywhere(Command::ActivateProduct { product: Box::new(flat) }).unwrap();
    let pooled = LifeSpec {
        id: life("warehouse"),
        kind: LifeKind::Pool {
            headcount: 0,
            total_salary_minor: 0,
        },
        factors: LifeFactors {
            category: Some("staff".into()),
            pool: Some(PoolAggregate {
                headcount: 40,
                total_salary_minor: 3_000_000_00,
            }),
            ..LifeFactors::default()
        },
    };
    let mut issue = group_scheme(24, d(2027, 11, 1), vec![pooled, member("exec-1", d(1975, 1, 1), "executive", 240_000_00)]);
    issue.product_code = "group-life-flat".into();
    let (p, id) = (c.partition_of(&issue.group), issue.policy.clone());
    c.submit(p, "new-business", issue_command(issue)).unwrap();
    // 40 x 1.10 a year for the pool and 1.45 for the executive: 45.45 a year = 3.79 a month.
    assert_eq!(owed(&c, p, &id), 3_79);
    assert_eq!(
        add(&mut c, p, excess(&id, d(2027, 11, 1), None, false)).unwrap_err().code,
        "not_rated_by_amount"
    );
}

/// Purpose: while a scheme invoice is being built across several commands, membership
/// changes, their dry run and layer commands are all refused with a code that says to
/// retry, and the invoice chunk event survives the log's encoding with its loading total.
/// Why it matters: a change landing between two chunks of one invoice would be priced in
/// some chunks and not others. The module retries on this code, so it must be the same
/// code everywhere and must never be mistaken for a permanent refusal.
#[test]
fn nothing_changes_membership_or_layers_while_an_invoice_is_being_built() {
    let mut c = cluster(d(2027, 11, 1));
    let (p, id) = scheme(&mut c);
    add(&mut c, p, excess(&id, d(2027, 11, 1), Some("1"), false)).unwrap();
    c.take_events();
    days(&mut c, 30);
    let chunk = events(&mut c).into_iter().find(|e| matches!(e, Event::SchemeInvoiceChunk { .. })).unwrap();
    let json = serde_json::to_value(&chunk).unwrap();
    assert_eq!(json["scheme_invoice_chunk"]["loading_minor"], 141_38 - 116_00);
    assert_eq!(serde_json::from_value::<Event>(json).unwrap(), chunk);
    let plain = serde_json::to_value(Event::SchemeInvoiceChunk {
        policy: id.clone(),
        period_index: 0,
        lines: vec![],
        complete: true,
        loading_minor: 0,
    })
    .unwrap();
    assert!(
        plain["scheme_invoice_chunk"].get("loading_minor").is_none(),
        "an invoice with no loading reads as it always did"
    );

    let half_built = ic_core::PendingInvoice {
        period_index: 2,
        cursor: Some(life("exec-1")),
        members: 1,
        premium: ic_core::premium::zero_premium(ic_money::Currency::NZD),
    };
    c.nodes.get_mut(&p).unwrap().replica.state.policies.get_mut(&id).unwrap().pending_invoice = Some(half_built);
    let leave = vec![MemberChange::Left {
        life: life("staff-1"),
        effective: d(2027, 11, 20),
    }];
    assert_eq!(preview_membership(c.state(p), &id, leave.clone()).unwrap_err().code, "invoice_in_progress");
    assert_eq!(membership(&mut c, p, &id, leave).unwrap_err().code, "invoice_in_progress");
    assert_eq!(
        add(&mut c, p, excess(&id, d(2027, 11, 5), None, false)).unwrap_err().code,
        "invoice_in_progress"
    );
}

/// Purpose: a member who joins a scheme after issue, and a member whose details change, are
/// published with their rating factors as last recorded, occupation class included.
/// Why it matters: the in-force, movement and experience extracts group lives by age, sex,
/// smoker status and occupation class. Members who joined after issue used to reach the
/// stream with no factors at all, so on a growing scheme most of the lives could not be
/// placed in any cell.
#[test]
fn a_joiner_and_a_changed_member_are_published_with_their_rating_factors() {
    let mut c = cluster(d(2027, 11, 15));
    let (p, id) = scheme(&mut c);
    let mut joiner = member("late-1", d(1985, 4, 2), "staff", 80_000_00);
    joiner.factors.sex = Some(ic_product::Sex::Male);
    joiner.factors.occupation_class = Some("2".into());
    let mut changed = joiner.factors.clone();
    changed.smoker = Some(true);
    c.take_events();
    membership(
        &mut c,
        p,
        &id,
        vec![MemberChange::Joined {
            life: joiner,
            effective: d(2027, 11, 15),
        }],
    )
    .unwrap();
    membership(
        &mut c,
        p,
        &id,
        vec![MemberChange::Changed {
            life: life("late-1"),
            effective: d(2027, 11, 15),
            factors: changed,
        }],
    )
    .unwrap();
    let seen = events(&mut c);
    let joined = seen.iter().find_map(|e| match e {
        Event::MemberJoined { info, .. } => info.clone(),
        _ => None,
    });
    let joined = joined.expect("the joiner carries its factors");
    assert_eq!(
        (joined.life, joined.date_of_birth, joined.sex, joined.smoker, joined.occupation_class.as_deref()),
        (life("late-1"), Some(d(1985, 4, 2)), Some(ic_product::Sex::Male), None, Some("2"))
    );
    let after = seen.iter().find_map(|e| match e {
        Event::MemberChanged { info, .. } => info.clone(),
        _ => None,
    });
    assert_eq!(after.expect("the change carries the new factors").smoker, Some(true));
}

/// Purpose: an underwritten layer written to replace another names the layer it carries
/// on, and the event says so; a layer number that is not that member's underwritten layer
/// on that cover is refused and writes nothing; a layer that replaces nothing says nothing.
/// Why it matters: reinsurance keeps a cession with the cover it follows. When a member
/// agrees to pay their own loading the scheme writes a replacement layer for the same cover,
/// and unless the event links the two the cession would restart on today's treaty terms.
#[test]
fn a_replacement_layer_names_the_layer_it_carries_on() {
    let mut c = cluster(d(2027, 11, 1));
    let (p, id) = scheme(&mut c);
    add(&mut c, p, excess(&id, d(2027, 11, 1), Some("0.50"), false)).unwrap();
    let first = events(&mut c)
        .into_iter()
        .find_map(|e| match e {
            Event::LayerAdded { layer, continues, .. } => Some((layer.layer, continues)),
            _ => None,
        })
        .unwrap();
    assert_eq!(first.1, None);

    let replacement = |continues| ExcessLayer {
        continues,
        ..excess(&id, d(2027, 11, 1), Some("0.50"), true)
    };
    let before = c.state(p).policies[&id].clone();
    assert_eq!(add(&mut c, p, replacement(Some(first.0 + 7))).unwrap_err().code, "unknown_layer");
    let of_another = ExcessLayer {
        life: life("staff-1"),
        ..replacement(Some(first.0))
    };
    assert_eq!(add(&mut c, p, of_another).unwrap_err().code, "unknown_layer");
    assert_eq!(c.state(p).policies[&id], before);

    events(&mut c);
    add(&mut c, p, replacement(Some(first.0))).unwrap();
    let linked = events(&mut c).into_iter().find_map(|e| match e {
        Event::LayerAdded { layer, continues, origin, .. } => Some((layer.layer, continues, origin)),
        _ => None,
    });
    assert_eq!(linked, Some((first.0 + 1, Some(first.0), LayerOrigin::UnderwrittenExcess)));
}

fn split(id: &PolicyId, new: &str, lives: &[&str], effective: NaiveDate) -> Command {
    Command::SplitScheme(Box::new(ic_core::SplitScheme {
        policy: id.clone(),
        new_policy: PolicyId(new.to_string()),
        new_number: "WPL-SPLIT-1".to_string(),
        lives: lives.iter().map(|l| life(l)).collect(),
        effective,
    }))
}

/// Purpose: part of a scheme moves to a new scheme from the first day of a billing period. The
/// member moves with the date they joined, their recorded details and their underwritten layer
/// on its own terms and number; the old scheme is credited for them on the period already
/// billed from that day and the new scheme bills the same amount for it, so nothing is billed
/// twice or dropped; the move is published as a split, never as a member leaving; and a split
/// on any other day, of every member, of someone not on the scheme, by another caller, or
/// added to after the new scheme has been billed, is refused and changes nothing.
/// Why it matters: when an employer sells a division its people must keep the cover they were
/// underwritten for without being asked for evidence again. Treated as leavers they would be
/// offered continuation and their loadings would be lost; billed on both schemes, two
/// employers would pay for the same people.
#[test]
fn a_split_moves_members_with_their_history_and_bills_each_scheme_its_own() {
    let mut c = cluster(d(2027, 11, 1));
    let (p, id) = scheme(&mut c);
    add(&mut c, p, excess(&id, d(2027, 11, 1), Some("0.5"), false)).unwrap();
    days(&mut c, 34);
    assert_eq!(c.business_date().unwrap(), d(2027, 12, 5));
    let billed = |c: &Cluster, id: &PolicyId| -> i64 { c.state(p).policies[id].receivables.iter().map(|r| r.total_minor).sum() };
    let before = c.state(p).policies[&id].clone();
    assert_eq!(before.periods_billed, 2);
    let total = billed(&c, &id);
    let new = PolicyId("pol-split-1".to_string());

    let refused = |c: &mut Cluster, command| c.submit(p, "group-admin", command).unwrap_err().code;
    assert_eq!(refused(&mut c, split(&id, "pol-split-1", &["exec-1"], d(2027, 12, 3))), "effective");
    assert_eq!(
        refused(&mut c, split(&id, "pol-split-1", &["exec-1", "staff-1"], d(2027, 12, 1))),
        "not_a_split"
    );
    assert_eq!(refused(&mut c, split(&id, "pol-split-1", &["nobody"], d(2027, 12, 1))), "unknown_member");
    assert_eq!(refused(&mut c, split(&id, &id.0, &["exec-1"], d(2027, 12, 1))), "invalid_split");
    assert_eq!(refused(&mut c, split(&id, "pol-split-1", &[], d(2027, 12, 1))), "apply_budget");
    assert!(c.submit(p, "policy-servicing", split(&id, "pol-split-1", &["exec-1"], d(2027, 12, 1))).is_err());
    assert_eq!(c.state(p).policies[&id], before, "a refused split changes nothing");
    assert!(!c.state(p).policies.contains_key(&new));

    c.take_events();
    c.submit(p, "group-admin", split(&id, "pol-split-1", &["exec-1"], d(2027, 12, 1))).unwrap();
    let seen = events(&mut c);
    assert!(seen.iter().any(|e| matches!(e, Event::SchemeSplit { new_policy, lives, layers, .. }
            if new_policy == &new && lives == &vec![life("exec-1")] && layers.len() == 1 && layers[0].1 == "group-life")));
    assert!(
        !seen.iter().any(|e| matches!(e, Event::MemberLeft { .. } | Event::LayerEnded { .. })),
        "moved, not left"
    );

    let s = c.state(p);
    let (old, child) = (&s.policies[&id], &s.policies[&new]);
    assert_eq!(old.lives[&life("exec-1")].left, Some(d(2027, 12, 1)));
    let member = &child.lives[&life("exec-1")];
    assert_eq!((member.joined, member.left), (d(2027, 11, 1), None));
    assert_eq!(member.factors, before.lives[&life("exec-1")].factors);
    let was = &before.covers["group-life"].layers[0];
    let now = &child.covers["group-life"].layers[0];
    assert_eq!((now.id, now.start, &now.loadings, now.end), (was.id, was.start, &was.loadings, None));
    assert_eq!(
        (child.inception, child.periods_billed, child.group.clone()),
        (old.inception, 1, old.group.clone())
    );
    assert_eq!(child.split_from.as_ref().map(|f| f.effective), Some(d(2027, 12, 1)));
    let credited = total - billed(&c, &id);
    assert!(credited > 90_62, "December's premium for the member and their layer is credited: {credited}");

    // Overnight the new scheme bills December for its member: exactly what the old one gave back.
    days(&mut c, 1);
    assert_eq!(billed(&c, &new), credited);
    assert_eq!(billed(&c, &id) + billed(&c, &new), total);
    assert_eq!(refused(&mut c, split(&id, "pol-split-1", &["staff-1"], d(2027, 12, 1))), "split_billed");
    days(&mut c, 30);
    assert_eq!(c.state(p).policies[&new].periods_billed, 3, "and it is billed with the calendar from then on");
}

/// Purpose: when a scheme learns on 10 December that one member left on 16 November and
/// that another's salary was lower from 1 November, a cover check for 20 November gives
/// today's answer (not a member, no cover; the lower sum insured) and beside it the answer
/// the record gave on 20 November itself (a member in force for the full amount). A member
/// nothing has changed for gets no second answer, and neither does a date after the news.
/// Why it matters: a claim is assessed on what is true, but a claim already accepted on
/// what the record said at the time has to be explained when a backdated change undoes it.
/// Without the earlier answer nobody can tell a claim that was wrong from one that was right
/// when it was made.
#[test]
fn a_cover_check_also_says_what_the_record_showed_on_the_event_date() {
    let mut c = cluster(d(2027, 11, 1));
    let (p, id) = scheme(&mut c);
    let join = member("staff-2", d(1990, 5, 5), "staff", 60_000_00);
    let change = |c: &mut Cluster, changes| c.submit(p, "group-admin", Command::ApplyMembership { policy: id.clone(), changes });
    change(
        &mut c,
        vec![MemberChange::Joined {
            life: join,
            effective: d(2027, 11, 1),
        }],
    )
    .unwrap();
    days(&mut c, 39); // 10 December
    let lower = LifeFactors {
        salary_minor: Some(200_000_00),
        ..c.state(p).policies[&id].lives[&life("exec-1")].factors.as_of(d(2027, 11, 1)).unwrap().clone()
    };
    change(
        &mut c,
        vec![
            MemberChange::Left {
                life: life("staff-1"),
                effective: d(2027, 11, 16),
            },
            MemberChange::Changed {
                life: life("exec-1"),
                effective: d(2027, 11, 1),
                factors: lower,
            },
        ],
    )
    .unwrap();
    let check = |c: &Cluster, who: &str, on| ic_core::cover_facts(c.state(p), &id.0, "group-life", Some(who), on).unwrap();
    let then = |f: &ic_core::claims_module::port::CoverFacts| f.known_then.as_ref().map(|k| (k.in_force, k.life_on_policy, k.sum_insured_minor));
    let event = d(2027, 11, 20);

    let left = check(&c, "staff-1", event);
    assert_eq!((left.in_force, left.life_on_policy, left.sum_insured_minor), (false, false, 0));
    assert_eq!(then(&left), Some((true, true, 120_000_00)));

    // 4 x 200,000 is 800,000, still held to the 750,000 limit: nothing to tell apart.
    assert_eq!(then(&check(&c, "exec-1", event)), None);
    assert_eq!(then(&check(&c, "staff-2", event)), None);
    // Asked about a day after the news, the record then and now are the same.
    assert_eq!(then(&check(&c, "staff-1", d(2027, 12, 10))), None);

    // A change that does move the amount is told apart the same way.
    let lower = LifeFactors {
        salary_minor: Some(100_000_00),
        ..c.state(p).policies[&id].lives[&life("exec-1")].factors.as_of(d(2027, 11, 1)).unwrap().clone()
    };
    change(
        &mut c,
        vec![MemberChange::Changed {
            life: life("exec-1"),
            effective: d(2027, 11, 1),
            factors: lower,
        }],
    )
    .unwrap();
    let exec = check(&c, "exec-1", event);
    assert_eq!((exec.in_force, exec.sum_insured_minor), (true, 400_000_00));
    assert_eq!(then(&exec), Some((true, true, 750_000_00)));
}

fn receipt(c: &mut Cluster, p: PartitionId, id: &PolicyId, reference: &str, minor: i64) {
    let receipt = Command::ApplyReceipt {
        policy: id.clone(),
        receipt: reference.into(),
        amount: ic_money::Money::new(minor, ic_money::Currency::NZD),
    };
    c.submit(p, "cash-matching", receipt).unwrap();
}

/// What is still owed on each of the scheme's bills, oldest first.
fn open_bills(c: &Cluster, p: PartitionId, id: &PolicyId) -> Vec<(NaiveDate, i64)> {
    c.state(p).policies[id].receivables.iter().map(|r| (r.due, r.outstanding_minor)).collect()
}

/// Purpose: matrix GRP-013. An employer remits 80% of what two invoices come to: the money
/// settles the older invoice first and the rest goes to the newer one, the scheme's standing is
/// counted from the newer invoice's own due date, and no member of the scheme is put in arrears
/// by the shortfall.
/// Why it matters: a short remittance is the employer's debt, not any employee's. Spreading it
/// across members would send arrears notices to people whose premium was deducted from their
/// pay; settling the newest invoice first would leave the oldest unpaid and lapse the scheme
/// sooner than its terms allow.
#[test]
fn a_short_remittance_settles_the_oldest_invoice_first_and_puts_no_member_in_arrears() {
    let mut c = cluster(d(2027, 11, 1));
    let (p, id) = scheme(&mut c);
    days(&mut c, 31);
    let bills = open_bills(&c, p, &id);
    assert_eq!(bills.iter().map(|(due, _)| *due).collect::<Vec<_>>(), vec![d(2027, 11, 1), d(2027, 12, 1)]);
    let total: i64 = bills.iter().map(|(_, owed)| owed).sum();
    let paid = total * 8 / 10;
    receipt(&mut c, p, &id, "remit-1", paid);
    let after = open_bills(&c, p, &id);
    assert_eq!(after[0].1, 0, "the older invoice is settled first");
    assert_eq!(after[1].1, total - paid);
    let policy = &c.state(p).policies[&id];
    assert_eq!(policy.oldest_unpaid_due(), Some(d(2027, 12, 1)));
    assert!(policy.member_standing.is_empty(), "no member is in arrears for the employer's shortfall");
    days(&mut c, 46);
    assert_eq!(c.state(p).policies[&id].status, PolicyStatus::InForce);
    assert!(c.state(p).policies[&id].member_standing.is_empty());
}

/// Purpose: matrix GRP-015. One remittance pays two invoices for the whole scheme and is then
/// recalled as a duplicate: the recall reverses its allocation by new entries, both invoices are
/// owed again, and the scheme's standing is worked out from the older invoice's original due
/// date, so it is in grace at once rather than given a fresh period.
/// Why it matters: a recall must not restart the lapse clock, or a scheme could stay covered
/// indefinitely by paying and recalling. Rewriting the original receipt entry instead of
/// reversing it would break the ledger's history that the bank reconciliation relies on.
#[test]
fn a_recalled_remittance_reopens_both_invoices_from_their_original_due_dates() {
    let mut c = cluster(d(2027, 11, 1));
    let (p, id) = scheme(&mut c);
    days(&mut c, 31);
    let total: i64 = open_bills(&c, p, &id).iter().map(|(_, owed)| owed).sum();
    receipt(&mut c, p, &id, "remit-dup", total);
    assert!(open_bills(&c, p, &id).iter().all(|(_, owed)| *owed == 0));
    let entries_before = c.state(p).ledger.entry_count();
    days(&mut c, 20);
    c.take_events();
    let recall = Command::ReverseReceipt {
        policy: id.clone(),
        receipt: "remit-dup".into(),
    };
    c.submit(p, "cash-matching", recall).unwrap();
    assert!(c.state(p).ledger.entry_count() > entries_before, "the recall is a new entry");
    assert_eq!(open_bills(&c, p, &id).iter().map(|(_, owed)| owed).sum::<i64>(), total);
    let policy = &c.state(p).policies[&id];
    assert_eq!(policy.oldest_unpaid_due(), Some(d(2027, 11, 1)), "counted from the original due date");
    assert_eq!(policy.standing, Some(ic_core::Standing::InGrace));
    let changed = c
        .take_events()
        .into_iter()
        .any(|e| matches!(e.event, Event::StandingChanged { to: Some(ic_core::Standing::InGrace), oldest_unpaid_due: Some(due), .. } if due == d(2027, 11, 1)));
    assert!(changed, "the standing change names the original due date");
}

/// Purpose: matrix GRP-025. A pool is billed a deposit premium on its estimate for the year and
/// the deposit is earned day by day, to the cent over the year. The year-end declaration comes
/// in higher than the estimate and is backdated to the start of the year: an adjustment is
/// raised for every month already billed at the difference, and the premium it adds for months
/// already past is recognised as earned at the next day's close, in the current period.
/// Why it matters: a deposit earned in lumps would misstate every month's income, and an
/// adjustment for last year recognised back in last year's closed periods would reopen
/// accounts already reported.
#[test]
fn a_pool_deposit_is_earned_daily_and_the_year_end_declaration_is_recognised_now() {
    let mut c = cluster(d(2027, 11, 1));
    let pool = |headcount, total_salary_minor| LifeFactors {
        category: Some("staff".into()),
        pool: Some(PoolAggregate { headcount, total_salary_minor }),
        ..LifeFactors::default()
    };
    let spec = LifeSpec {
        id: life("warehouse"),
        kind: LifeKind::Pool {
            headcount: 0,
            total_salary_minor: 0,
        },
        factors: pool(40, 3_000_000_00),
    };
    let issue = group_scheme(23, d(2027, 11, 1), vec![spec]);
    let (p, id) = (c.partition_of(&issue.group), issue.policy.clone());
    c.submit(p, "new-business", issue_command(issue)).unwrap();
    receipt(&mut c, p, &id, "deposit", 20_000_00);
    c.take_events();

    let mut earned: i128 = 0;
    let mut by_day = 0usize;
    while c.business_date().unwrap() < d(2028, 11, 1) {
        days(&mut c, 1);
        for e in c.take_events() {
            if let Event::PremiumEarned { policy, amount_fine, .. } = e.event {
                if policy == id {
                    earned += amount_fine;
                    by_day += 1;
                }
            }
        }
    }
    assert_eq!(earned, 12 * 550_00 * ic_money::FINE_PER_MINOR, "the year's deposit earned exactly");
    assert!(by_day >= 360, "earned day by day, {by_day} times");

    // The declaration: 44 people on 3,300,000 for the whole year, 605.00 a month.
    let owed_before: i64 = c.state(p).policies[&id].receivables.iter().map(|r| r.total_minor).sum();
    let declared = MemberChange::Changed {
        life: life("warehouse"),
        effective: d(2027, 11, 1),
        factors: pool(44, 3_300_000_00),
    };
    membership(&mut c, p, &id, vec![declared]).unwrap();
    let billed_after: i64 = c.state(p).policies[&id].receivables.iter().map(|r| r.total_minor).sum();
    assert_eq!(
        billed_after - owed_before,
        13 * 55_00,
        "the difference for every month billed, November 2028 included"
    );
    let today = c.business_date().unwrap();
    days(&mut c, 1);
    let catch_up = c.take_events().into_iter().find_map(|e| match e.event {
        Event::PremiumEarned { policy, through, amount_fine } if policy == id => Some((through, amount_fine)),
        _ => None,
    });
    let (through, amount) = catch_up.expect("earned at the close");
    assert_eq!(through, today, "recognised in the current period");
    assert!(
        amount >= 12 * 55_00 * ic_money::FINE_PER_MINOR,
        "the past months' difference is earned at once: {amount}"
    );
}
