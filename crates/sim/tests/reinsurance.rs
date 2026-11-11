//! The two commands the reinsurance module sends the core: the cession
//! recorded on a cover, and the entries reinsurance posts. The module that
//! works them out is tested in its own crate; these tests are the core's side
//! of the contract.

use chrono::NaiveDate;
use ic_core::accounts::{self, role};
use ic_core::event::Event;
use ic_core::reinsurance::{cession_on, reinsurer_balance, CessionRecord, CessionSegment, FacultativeShare, ReinsurancePosting};
use ic_core::{Command, GroupId, LifeId, PolicyId};
use ic_ledger::{Account, PartitionId, Side};
use ic_money::Currency::{AUD, NZD};
use ic_product::{Frequency, Sex};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, term_life};
use ic_sim::{Cluster, Delivery};

const TREATY: &str = "retail-life-surplus";
const NORTH: &str = "reinsurer-north";

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

/// Two partitions, opened on 1 March 2027, and a 800,000 term life policy.
fn book() -> (Cluster, PartitionId, PolicyId) {
    let mut c = Cluster::in_memory(2, 5);
    c.open(d(2027, 3, 1)).unwrap();
    for cmd in shipped_activations().unwrap() {
        c.activate_everywhere(cmd).unwrap();
    }
    let issue = term_life(1, d(2027, 3, 1), d(1987, 6, 15), Sex::Female, false, 800_000_00, None, Frequency::Monthly, None);
    let (p, id) = (c.partition_of(&issue.group), issue.policy.clone());
    c.submit(p, "new-business", issue_command(issue)).unwrap();
    c.take_events();
    (c, p, id)
}

fn life() -> LifeId {
    LifeId("life-1".into())
}

/// 800,000 at risk: `retained` kept, the rest ceded 60/40.
fn ceded(retained: i64) -> CessionRecord {
    let rest = 800_000_00 - retained;
    CessionRecord {
        treaty: Some(TREATY.into()),
        treaty_version: 1,
        sum_at_risk_minor: 800_000_00,
        retained_minor: retained,
        ceded: vec![(NORTH.into(), rest * 6 / 10), ("reinsurer-south".into(), rest * 4 / 10)],
        facultative: vec![],
        unplaced_minor: 0,
    }
}

fn segment(from: NaiveDate, cession: Option<CessionRecord>) -> CessionSegment {
    CessionSegment { from, cession }
}

fn record(policy: &PolicyId, from: NaiveDate, segments: Vec<CessionSegment>) -> Command {
    Command::RecordCession {
        policy: policy.clone(),
        cover: "life".into(),
        layer: 1,
        life: life(),
        from,
        segments,
    }
}

fn post(kind: ReinsurancePosting, policy: Option<&PolicyId>, amount_minor: i64) -> Command {
    Command::PostReinsurance {
        kind,
        treaty: TREATY.into(),
        reinsurer: NORTH.into(),
        currency: NZD,
        policy: policy.cloned(),
        amount_minor,
        value_date: d(2027, 3, 1),
        reference: "pol-00000001/life/1/life-1/2027-03-01".into(),
    }
}

/// What a refused command must leave alone: the policies and the ledger. The
/// record of the refusal itself, kept for the idempotency key, is expected.
fn books(c: &Cluster, p: PartitionId) -> (String, [u8; 32]) {
    let state = c.state(p);
    (serde_json::to_string(&state.policies).unwrap(), state.ledger.digest().0)
}

fn refused(c: &mut Cluster, p: PartitionId, command: Command) -> String {
    let before = books(c, p);
    let code = c.submit(p, "reinsurance", command).unwrap_err().code;
    assert!(books(c, p) == before, "a refused command changed the partition");
    code
}

fn natural(c: &Cluster, p: PartitionId, account: Account) -> i128 {
    c.state(p).ledger.natural_balance(&account.id)
}

/// Purpose: a cession sent by the reinsurance module is kept on the cover it belongs to,
/// by date, and a later change replaces the record from its date and keeps what came before.
/// Why it matters: a claim is recovered against the cession in force on the date of the
/// event. A change that rewrote earlier dates would change recoveries on claims already
/// paid; one that failed to replace later dates would leave two answers for one day.
#[test]
fn a_cession_is_recorded_by_date_and_a_change_keeps_what_came_before() {
    let (mut c, p, id) = book();
    let first = vec![segment(d(2027, 3, 1), Some(ceded(500_000_00))), segment(d(2028, 3, 1), None)];
    c.submit(p, "reinsurance", record(&id, d(2027, 3, 1), first)).unwrap();
    let events = c.take_events();
    assert!(matches!(
        &events[..],
        [rec] if matches!(&rec.event, Event::CessionRecorded { policy, cover, layer: 1, life: l, from }
            if *policy == id && cover == "life" && *l == life() && *from == d(2027, 3, 1))
    ));
    let on = |c: &Cluster, date| cession_on(&c.state(p).policies[&id], "life", 1, &life(), date).cloned();
    assert_eq!(on(&c, d(2027, 2, 28)), None, "nothing before the cover started");
    assert_eq!(on(&c, d(2027, 3, 1)), Some(ceded(500_000_00)));
    assert_eq!(on(&c, d(2028, 2, 29)), Some(ceded(500_000_00)));
    assert_eq!(on(&c, d(2028, 3, 1)), None, "the cover has ended");
    assert_eq!(
        cession_on(&c.state(p).policies[&id], "life", 2, &life(), d(2027, 3, 1)),
        None,
        "another layer has no record"
    );

    // From 1 July more is retained, with a facultative share and an amount unplaced.
    let mut changed = ceded(600_000_00);
    changed.ceded = vec![(NORTH.into(), 100_000_00)];
    changed.facultative = vec![FacultativeShare {
        reference: "FAC-1".into(),
        reinsurer: "reinsurer-south".into(),
        amount_minor: 60_000_00,
    }];
    changed.unplaced_minor = 40_000_00;
    c.submit(
        p,
        "reinsurance",
        record(&id, d(2027, 7, 1), vec![segment(d(2027, 7, 1), Some(changed.clone()))]),
    )
    .unwrap();
    assert_eq!(on(&c, d(2027, 6, 30)), Some(ceded(500_000_00)));
    assert_eq!(on(&c, d(2027, 7, 1)), Some(changed.clone()));
    assert_eq!(on(&c, d(2029, 1, 1)), Some(changed), "the end recorded earlier was replaced");
    assert_eq!(c.state(p).policies[&id].cessions["life/1/life-1"].len(), 2);

    // Taking the whole record back leaves nothing behind.
    c.submit(p, "reinsurance", record(&id, d(2027, 3, 1), vec![])).unwrap();
    assert!(c.state(p).policies[&id].cessions.is_empty());
    assert!(c.reconcile().is_empty());
}

/// Purpose: a cession that does not add up, is out of date order, or names a cover or policy
/// the partition does not hold is refused, as is one from any module but reinsurance.
/// Why it matters: the core does not work cessions out, so this check is all that stands
/// between a defect in the module and a policy record showing more ceded than is insured.
#[test]
fn a_cession_that_does_not_add_up_is_refused() {
    let (mut c, p, id) = book();
    let good = || segment(d(2027, 3, 1), Some(ceded(500_000_00)));
    let with = |change: fn(&mut CessionRecord)| {
        let mut r = ceded(500_000_00);
        change(&mut r);
        vec![segment(d(2027, 3, 1), Some(r))]
    };
    let cases: Vec<(&str, Command)> = vec![
        ("unknown_policy", record(&PolicyId("nope".into()), d(2027, 3, 1), vec![good()])),
        (
            "unknown_cover",
            Command::RecordCession {
                policy: id.clone(),
                cover: "funeral".into(),
                layer: 1,
                life: life(),
                from: d(2027, 3, 1),
                segments: vec![good()],
            },
        ),
        // The parts must add up to the sum at risk.
        ("invalid_cession", record(&id, d(2027, 3, 1), with(|r| r.retained_minor += 1))),
        // No part may be negative, even where the total still adds up.
        (
            "invalid_cession",
            record(
                &id,
                d(2027, 3, 1),
                with(|r| {
                    r.retained_minor = 900_000_00;
                    r.ceded = vec![(NORTH.into(), -100_000_00)];
                }),
            ),
        ),
        (
            "invalid_cession",
            record(
                &id,
                d(2027, 3, 1),
                with(|r| {
                    r.unplaced_minor = -1;
                    r.retained_minor += 1;
                }),
            ),
        ),
        (
            "invalid_cession",
            record(
                &id,
                d(2027, 3, 1),
                with(|r| {
                    r.retained_minor = -1;
                    r.unplaced_minor = 300_000_01;
                }),
            ),
        ),
        (
            "invalid_cession",
            record(
                &id,
                d(2027, 3, 1),
                with(|r| {
                    r.facultative = vec![FacultativeShare {
                        reference: "FAC-1".into(),
                        reinsurer: NORTH.into(),
                        amount_minor: -5,
                    }];
                    r.retained_minor += 5;
                }),
            ),
        ),
        // A segment may not start before the date the record is replaced from.
        ("invalid_cession", record(&id, d(2027, 4, 1), vec![good()])),
        // Segments are in date order, one per date.
        ("invalid_cession", record(&id, d(2027, 3, 1), vec![good(), segment(d(2027, 3, 1), None)])),
    ];
    for (expected, command) in cases {
        assert_eq!(refused(&mut c, p, command.clone()), expected, "{command:?}");
    }
    let err = c.submit(p, "claims", record(&id, d(2027, 3, 1), vec![good()])).unwrap_err();
    assert_eq!(err.code, "caller_not_allowed");
    assert!(c.state(p).policies[&id].cessions.is_empty());
}

/// Purpose: each kind of reinsurance entry debits and credits the accounts it says it does,
/// the reinsurer's account ends up holding what is owed to them, and the books reconcile.
/// Why it matters: these nine entries are the whole of reinsurance accounting in the core.
/// One with its sides the wrong way round would overstate both what the insurer owes its
/// reinsurers and its profit, and nothing else would catch it.
#[test]
fn each_reinsurance_entry_posts_to_its_accounts() {
    use ReinsurancePosting::*;
    let (mut c, p, id) = book();
    for (kind, policy, amount) in [
        (OpeningBalance, None, 1_000_00),
        (CededPremium, Some(&id), 50_00),
        (Commission, Some(&id), 10_00),
        (Recoverable, Some(&id), 300_000_00),
        (RecoveryDue, Some(&id), 180_000_00),
        (AggregateRecovery, None, 7_000_00),
        (ProfitCommission, None, 400_00),
        (DisputeSettlement, Some(&id), 2_000_00),
        (SettlementSweep, None, 900_00),
    ] {
        let applied = c.submit_keyed(p, &format!("ri-{kind:?}"), "reinsurance", post(kind, policy, amount));
        assert_eq!(applied.result, Ok(()), "{kind:?}");
        let entry = &applied.entries[0].body;
        assert_eq!((entry.caller.as_str(), entry.policy.as_deref()), ("reinsurance", policy.map(|p| p.0.as_str())));
        assert_eq!(entry.detail["treaty"], TREATY);
        assert_eq!(entry.detail["reinsurer"], NORTH);
        assert!(entry.postings.iter().all(|line| line.dims.posting_type.as_deref() == Some(entry.kind.as_str())));
        // An entry for a policy belongs to the policy's statutory fund; a treaty's own entry to none.
        assert!(entry.postings.iter().all(|line| line.dims.statutory_fund.is_some() == policy.is_some()));
        assert!(applied
            .events
            .iter()
            .any(|rec| matches!(&rec.event, Event::ReinsurancePosted { kind: k, amount_minor, policy: on, .. }
            if *k == kind && *amount_minor == amount && on.as_ref() == policy)));
    }
    // Owed to the reinsurer: opening 1,000 + premium 50 + dispute 2,000, less commission 10,
    // recovery due 180,000, aggregate recovery 7,000, profit commission 400 and the sweep 900.
    assert_eq!(reinsurer_balance(&c.state(p).ledger, TREATY, NORTH, NZD), -185_260_00);
    assert_eq!(natural(&c, p, accounts::reinsurance_premium(NZD)), 50_00);
    assert_eq!(natural(&c, p, accounts::reinsurance_commission(NZD)), 10_00);
    assert_eq!(natural(&c, p, accounts::reinsurance_recoverable(NZD)), 120_000_00);
    assert_eq!(natural(&c, p, accounts::reinsurance_recoveries(NZD)), 300_000_00 + 7_000_00 - 2_000_00);
    assert_eq!(natural(&c, p, accounts::reinsurance_profit_commission(NZD)), 400_00);
    assert_eq!(natural(&c, p, accounts::reinsurance_settlement_clearing(NZD)), 900_00);
    assert_eq!(natural(&c, p, accounts::reinsurance_migration_control(NZD)), 1_000_00);
    assert_eq!(
        c.state(p).ledger.role_total(role::REINSURER_ACCOUNT),
        185_260_00,
        "one reinsurer account, in debit"
    );
    assert_eq!(reinsurer_balance(&c.state(p).ledger, TREATY, "reinsurer-south", NZD), 0);
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());
    let close = c.end_of_day().unwrap();
    assert!(close.breaks.is_empty());
}

/// Purpose: a negative amount posts the opposite entry, and an entry for an earlier period
/// carries that period's value date while one dated ahead is valued today.
/// Why it matters: a backdated change to a cession is put right by posting the difference,
/// never by editing what was posted. The correction has to net the original to nothing and
/// land in the period it belongs to, or the reinsurer's quarter is misstated.
#[test]
fn a_negative_amount_reverses_and_the_value_date_is_the_periods() {
    let (mut c, p, id) = book();
    for _ in 0..40 {
        c.end_of_day().unwrap();
    }
    let today = c.business_date().unwrap();
    let charged = c.submit_keyed(p, "ri-1", "reinsurance", post(ReinsurancePosting::CededPremium, Some(&id), 50_00));
    assert_eq!(charged.entries[0].body.value_date, d(2027, 3, 1));
    assert_eq!(charged.entries[0].business_date, today);
    let back = c.submit_keyed(p, "ri-2", "reinsurance", post(ReinsurancePosting::CededPremium, Some(&id), -50_00));
    let lines = &back.entries[0].body.postings;
    let side_of = |account: Account| lines.iter().find(|l| l.account == account.id).map(|l| l.side);
    assert_eq!(side_of(accounts::reinsurance_premium(NZD)), Some(Side::Credit));
    assert_eq!(side_of(accounts::reinsurer_account(TREATY, NORTH, NZD)), Some(Side::Debit));
    assert_eq!(reinsurer_balance(&c.state(p).ledger, TREATY, NORTH, NZD), 0);
    assert_eq!(natural(&c, p, accounts::reinsurance_premium(NZD)), 0);

    let ahead = Command::PostReinsurance {
        kind: ReinsurancePosting::ProfitCommission,
        treaty: TREATY.into(),
        reinsurer: NORTH.into(),
        currency: NZD,
        policy: None,
        amount_minor: 1_00,
        value_date: d(2030, 1, 1),
        reference: "profit commission 2027".into(),
    };
    let applied = c.submit_keyed(p, "ri-3", "reinsurance", ahead);
    assert_eq!(applied.entries[0].body.value_date, today, "nothing is valued in the future");
    assert!(c.reconcile().is_empty());
}

/// Purpose: an entry with no amount, no reference, an account code that could not be a
/// ledger account, the wrong currency for its policy or a policy the partition does not hold
/// is refused, and so is one from any module but reinsurance.
/// Why it matters: account identifiers are written into the hash chain and cannot be
/// renamed. A treaty ID with a slash in it would open an account that reads as a
/// different treaty's for the life of the ledger.
#[test]
fn a_reinsurance_entry_that_is_not_well_formed_is_refused() {
    let (mut c, p, id) = book();
    let with = |change: fn(&mut Command)| {
        let mut command = post(ReinsurancePosting::CededPremium, None, 50_00);
        change(&mut command);
        command
    };
    macro_rules! set {
        ($field:ident = $value:expr) => {
            with(|c| {
                if let Command::PostReinsurance { $field, .. } = c {
                    *$field = $value;
                }
            })
        };
    }
    let cases: Vec<(&str, Command)> = vec![
        ("invalid_code", set!(treaty = "retail/life".into())),
        ("invalid_code", set!(treaty = String::new())),
        ("invalid_code", set!(reinsurer = "north reinsurance".into())),
        ("invalid_code", set!(reinsurer = "r".repeat(65))),
        ("invalid_amount", set!(amount_minor = 0)),
        ("invalid_amount", set!(reference = "  ".into())),
        ("unknown_policy", set!(policy = Some(PolicyId("nope".into())))),
        ("currency", {
            let mut command = post(ReinsurancePosting::CededPremium, Some(&id), 50_00);
            if let Command::PostReinsurance { currency, .. } = &mut command {
                *currency = AUD;
            }
            command
        }),
    ];
    for (expected, command) in cases {
        assert_eq!(refused(&mut c, p, command.clone()), expected, "{command:?}");
    }
    let err = c.submit(p, "finance", post(ReinsurancePosting::CededPremium, Some(&id), 50_00)).unwrap_err();
    assert_eq!(err.code, "caller_not_allowed");
    assert_eq!(c.state(p).ledger.role_total(role::REINSURER_ACCOUNT), 0);
    // The longest code allowed is accepted.
    c.submit(p, "reinsurance", set!(reinsurer = "r".repeat(64))).unwrap();
}

/// Purpose: while a policy is moving between partitions a change to its cession is refused
/// and a reinsurance entry for it still posts; once it has landed its cession record is on
/// the new partition, and the stub left behind takes entries but no cession.
/// Why it matters: premium already billed is owed to the reinsurer whether or not the policy
/// is in flight, so the entry must not be lost; the cession belongs to the policy record,
/// which only one partition may change at a time.
#[test]
fn a_policy_in_transfer_takes_entries_but_not_a_change_of_cession() {
    let (mut c, src, id) = book();
    c.submit(
        src,
        "reinsurance",
        record(&id, d(2027, 3, 1), vec![segment(d(2027, 3, 1), Some(ceded(500_000_00)))]),
    )
    .unwrap();
    let dst = 1 - src;
    let target_group = GroupId(
        (0..)
            .map(|i| format!("grp-new-{i}"))
            .find(|g| c.partition_of(&GroupId(g.clone())) == dst)
            .unwrap(),
    );
    c.submit(
        src,
        "transfer",
        Command::BeginTransfer {
            policy: id.clone(),
            transfer_id: "t-1".into(),
            target_group,
            target_partition: dst,
        },
    )
    .unwrap();
    let change = || record(&id, d(2027, 3, 1), vec![segment(d(2027, 3, 1), Some(ceded(400_000_00)))]);
    assert_eq!(refused(&mut c, src, change()), "policy_transferring");
    c.submit(src, "reinsurance", post(ReinsurancePosting::CededPremium, Some(&id), 50_00)).unwrap();

    while c.messages_in_flight() > 0 {
        c.relay_messages(|_| Delivery::Deliver);
    }
    assert_eq!(c.locate(&id), Some(dst));
    assert_eq!(
        cession_on(&c.state(dst).policies[&id], "life", 1, &life(), d(2027, 3, 1)),
        Some(&ceded(500_000_00))
    );
    assert_eq!(refused(&mut c, src, change()), "policy_transferred");
    c.submit(src, "reinsurance", post(ReinsurancePosting::CededPremium, Some(&id), -10_00)).unwrap();
    c.submit(dst, "reinsurance", change()).unwrap();
    assert_eq!(
        cession_on(&c.state(dst).policies[&id], "life", 1, &life(), d(2027, 3, 1)),
        Some(&ceded(400_000_00))
    );
    assert_eq!(reinsurer_balance(&c.state(src).ledger, TREATY, NORTH, NZD), 40_00);
    assert!(c.reconcile().is_empty(), "{:?}", c.reconcile());
}
