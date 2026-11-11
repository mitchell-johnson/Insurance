//! The module and the core together: the core's events go to the module, the
//! module's instructions come back as core commands, and the two records are
//! compared. The core here is the simulator's cluster, with real partitions,
//! real billing and the real reconciliation.

mod common;

use std::collections::BTreeMap;

use chrono::NaiveDate;
use common::*;
use ic_core::reinsurance::{cession_key, cession_on, reinsurer_balance};
use ic_core::{accounts, GroupId, PartyId, PolicyId};
use ic_ledger::PartitionId;
use ic_money::Currency::NZD;
use ic_product::{Frequency, Sex};
use ic_reinsurance::account::StatementStatus;
use ic_reinsurance::delivery::{CoreDelivery, CALLER};
use ic_reinsurance::treaty::RiskFactors;
use ic_reinsurance::{Command, Instruction, InstructionBody, RiskKey};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, named_life, term_life};
use ic_sim::{Cluster, Delivery};

const PARTITIONS: PartitionId = 4;

/// The core and the module, and the driver between them.
struct World {
    core: Cluster,
    bench: Bench,
    /// Everything delivered to the core, to deliver again.
    delivered: Vec<CoreDelivery>,
    /// Cessions the core refused because the policy was moving.
    dropped: u32,
}

impl World {
    fn new(open: NaiveDate) -> Self {
        let mut core = Cluster::in_memory(PARTITIONS, 11);
        core.open(open).unwrap();
        for cmd in shipped_activations().unwrap() {
            core.activate_everywhere(cmd).unwrap();
        }
        let mut bench = Bench::new();
        bench.today = open;
        let mut w = World {
            core,
            bench,
            delivered: vec![],
            dropped: 0,
        };
        w.pump();
        w
    }

    /// Carry the core's events to the module and the module's instructions
    /// back to the core, until neither has anything more to say.
    fn pump(&mut self) {
        loop {
            let events = self.core.take_events();
            if events.is_empty() {
                break;
            }
            self.bench.today = self.core.business_date().unwrap();
            let applied = self.bench.ok("event-feed", Command::CoreEvents { events });
            self.deliver(&applied.instructions);
        }
    }

    /// The driver's rule: a cession for a policy that is moving between
    /// partitions, or has moved, is dropped when the core refuses it, because
    /// the module sends the whole record again when the transfer lands or is
    /// abandoned. Anything else the core refuses is a defect.
    fn deliver(&mut self, instructions: &[Instruction]) {
        for i in instructions {
            let delivery = self.bench.ri.to_core(i, self.bench.today).expect("an instruction for a known treaty");
            let applied = self
                .core
                .submit_keyed(delivery.partition, &delivery.key, delivery.caller, delivery.command.clone());
            match applied.result.as_ref().map_err(|e| e.code.as_str()) {
                Ok(()) => {
                    assert!(!applied.duplicate, "instruction {} was delivered twice", i.id);
                    self.delivered.push(delivery);
                }
                Err("policy_transferring" | "policy_transferred") if matches!(i.body, InstructionBody::RecordCession { .. }) => self.dropped += 1,
                Err(code) => panic!("the core refused {:?}: {code}", i.body),
            }
        }
    }

    /// A module command from a person or another module, and its instructions delivered.
    fn module(&mut self, caller: &str, command: Command) {
        let applied = self.bench.ok(caller, command);
        self.deliver(&applied.instructions);
        self.pump();
    }

    /// Issue a term life policy for `party` on whichever partition its group lives on.
    fn issue(&mut self, n: u64, party: &str, inception: NaiveDate, sum: i64) -> (PartitionId, PolicyId) {
        let dob = NaiveDate::from_ymd_opt(1987, 6, 15).unwrap();
        let mut issue = term_life(n, inception, dob, Sex::Female, false, sum, None, Frequency::Monthly, None);
        issue.policyholder = PartyId(party.to_string());
        issue.lives = vec![named_life("life-1", party, dob, Sex::Female, false)];
        let (p, id) = (self.core.partition_of(&issue.group), issue.policy.clone());
        self.bench.ok(
            "new-business",
            Command::RecordRiskFactors {
                policy: id.clone(),
                life: "life-1".into(),
                factors: RiskFactors {
                    date_of_birth: Some(dob),
                    sex: Some(Sex::Female),
                    smoker: Some(false),
                },
            },
        );
        self.core.submit(p, "new-business", issue_command(issue)).unwrap();
        self.pump();
        (p, id)
    }

    fn days(&mut self, n: u32) {
        for _ in 0..n {
            let close = self.core.end_of_day().unwrap();
            assert!(close.breaks.is_empty(), "reconciliation breaks on {}: {:?}", close.closed, close.breaks);
            self.pump();
        }
    }

    /// Every cover the module has sent a cession for.
    fn keys(&self) -> Vec<RiskKey> {
        let mut keys: Vec<RiskKey> = self
            .bench
            .instructions
            .iter()
            .filter_map(|i| match &i.body {
                InstructionBody::RecordCession { key, .. } => Some(key.clone()),
                _ => None,
            })
            .collect();
        keys.sort();
        keys.dedup();
        keys
    }

    /// The cession on every cover is the same in the core as in the module.
    fn assert_cessions_agree(&self) {
        let keys = self.keys();
        assert!(!keys.is_empty());
        for key in keys {
            let p = self.core.locate(&key.policy).unwrap();
            let policy = &self.core.state(p).policies[&key.policy];
            let core = policy.cessions.get(&cession_key(&key.cover, key.layer, &key.life)).cloned().unwrap_or_default();
            let module = self.bench.ri.cession_record(&key);
            assert_eq!(core.len(), module.len(), "{key}: segment count");
            for (c, (from, m)) in core.iter().zip(module) {
                assert_eq!(c.from, *from, "{key}: segment date");
                let shape = |retained: i64, ceded: Vec<(String, i64)>, facultative: i64, unplaced: i64| (retained, ceded, facultative, unplaced);
                let in_core = c.cession.as_ref().map(|c| {
                    shape(
                        c.retained_minor,
                        c.ceded.clone(),
                        c.facultative.iter().map(|f| f.amount_minor).sum(),
                        c.unplaced_minor,
                    )
                });
                let in_module = m.as_ref().map(|m| {
                    shape(
                        m.retained_minor,
                        m.ceded.iter().map(|(r, a)| (r.to_string(), *a)).collect(),
                        m.facultative_ceded(),
                        m.unplaced_minor,
                    )
                });
                assert_eq!(in_core, in_module, "{key} from {from}");
            }
        }
    }

    /// What the ledgers hold for a reinsurer across every partition: on the
    /// reinsurer's account, and swept to settlement clearing.
    fn ledger(&self, reinsurer: &str) -> (i128, i128) {
        let mut account = 0;
        for p in 0..PARTITIONS {
            account += reinsurer_balance(&self.core.state(p).ledger, "retail-life-surplus", reinsurer, NZD);
        }
        (account, self.clearing())
    }

    fn clearing(&self) -> i128 {
        (0..PARTITIONS)
            .map(|p| self.core.state(p).ledger.natural_balance(&accounts::reinsurance_settlement_clearing(NZD).id))
            .sum()
    }

    fn hashes(&self) -> BTreeMap<PartitionId, String> {
        (0..PARTITIONS).map(|p| (p, hex::encode(self.core.state(p).state_hash().0))).collect()
    }
}

/// Two policies on one life, on two partitions: 400,000 then 300,000 of cover
/// against a 500,000 retention.
fn two_partitions() -> (World, (PartitionId, PolicyId), (PartitionId, PolicyId)) {
    let mut w = World::new(d(2027, 3, 1));
    let first = w.issue(1, "party-kiri", d(2027, 3, 1), 400_000_00);
    // The second policy must live on another partition for the test to mean anything.
    let n = (2..50)
        .find(|n| {
            let group = GroupId(format!("grp-{n:08}"));
            w.core.partition_of(&group) != first.0
        })
        .unwrap();
    let second = w.issue(n, "party-kiri", d(2027, 3, 1), 300_000_00);
    assert_ne!(first.0, second.0);
    (w, first, second)
}

/// Purpose: one life with policies on two partitions has its retention counted once, and
/// the cession the module works out is what each partition's policy record then says.
/// Why it matters: no partition can see the other's cover, so only the module can keep the
/// insurer inside its retention. If the core's record differed from the module's, a claim
/// would be recovered against a cession nobody worked out.
#[test]
fn a_life_on_two_partitions_is_ceded_once_and_recorded_on_both() {
    let (w, (pa, a), (pb, b)) = two_partitions();
    let life = "life-1".into();
    let on = d(2027, 3, 1);
    let first = cession_on(&w.core.state(pa).policies[&a], "life", 1, &life, on).expect("the first policy's cession");
    assert_eq!((first.retained_minor, first.ceded.iter().map(|(_, c)| c).sum::<i64>()), (400_000_00, 0));
    let second = cession_on(&w.core.state(pb).policies[&b], "life", 1, &life, on).expect("the second policy's cession");
    assert_eq!(second.treaty.as_deref(), Some("retail-life-surplus"));
    assert_eq!(
        (second.treaty_version, second.sum_at_risk_minor, second.retained_minor),
        (1, 300_000_00, 100_000_00)
    );
    assert_eq!(
        second.ceded,
        vec![("reinsurer-north".to_string(), 120_000_00), ("reinsurer-south".to_string(), 80_000_00)]
    );
    assert!(cession_on(&w.core.state(pb).policies[&b], "life", 1, &life, d(2027, 2, 28)).is_none());
    w.assert_cessions_agree();
    w.bench.assert_reconciles();
    assert!(w.core.reconcile().is_empty());
}

/// Purpose: the premium the module charges for a billed period reaches the ledger of the
/// partition that holds the policy, and every partition's ledger adds up to the balance
/// the module holds for the reinsurer, day after day.
/// Why it matters: the reinsurer's statement is cut from the module and the money is paid
/// from the ledger. A difference between them is money paid that was never stated, or
/// stated and never paid.
#[test]
fn ceded_premium_reaches_the_ledger_and_the_two_balances_agree() {
    let (mut w, _, (pb, _)) = two_partitions();
    let north_owed = w.bench.ri.balance(&retail(), &north());
    assert!(north_owed > 0, "the first period's premium less commission is owed to the reinsurer");
    assert_eq!(w.ledger("reinsurer-north"), (north_owed as i128, 0));
    // The whole balance is on the partition of the policy that cedes.
    assert_eq!(
        reinsurer_balance(&w.core.state(pb).ledger, "retail-life-surplus", "reinsurer-north", NZD),
        north_owed as i128
    );
    let ledger = &w.core.state(pb).ledger;
    let premium = ledger.natural_balance(&accounts::reinsurance_premium(NZD).id);
    let commission = ledger.natural_balance(&accounts::reinsurance_commission(NZD).id);
    assert_eq!(
        premium as i64,
        w.bench.posted(ic_reinsurance::PostingKind::CededPremium, &north()) + w.bench.posted(ic_reinsurance::PostingKind::CededPremium, &south())
    );
    assert_eq!(premium - commission, w.ledger("reinsurer-north").0 + w.ledger("reinsurer-south").0);
    // A fifth of the premium, rounded for each reinsurer.
    assert!((commission * 5 - premium).abs() <= 5, "commission {commission} on premium {premium}");

    w.days(35);
    let later = w.bench.ri.balance(&retail(), &north());
    assert!(later > north_owed, "a second period has been billed");
    assert_eq!(w.ledger("reinsurer-north").0, later as i128);
    assert_eq!(w.ledger("reinsurer-south").0, w.bench.ri.balance(&retail(), &south()) as i128);
    w.assert_cessions_agree();
    w.bench.assert_reconciles();
}

/// Purpose: a statement that is agreed and sent for payment moves exactly its net amount
/// from the reinsurer's account to settlement clearing on the partitions that hold it,
/// and what was billed after the cut stays on the account.
/// Why it matters: this is the step where reinsurance money leaves. Sweeping the balance
/// of the day, not of the cut, would pay the reinsurer for a period they have not been
/// sent a statement for.
#[test]
fn an_agreed_statement_sweeps_its_net_amount_to_settlement_clearing() {
    let (mut w, _, _) = two_partitions();
    w.days(33);
    assert!(w.bench.today > d(2027, 4, 1));
    w.module(
        "month-end",
        Command::CutStatement {
            treaty: retail(),
            reinsurer: north(),
            period_end: d(2027, 3, 31),
        },
    );
    let net = w.bench.ri.statement(1).unwrap().net_minor;
    let before = w.ledger("reinsurer-north").0;
    assert!(net > 0 && before > net as i128, "April has been billed since the period closed");
    w.module(
        "reinsurance-ops",
        Command::AgreeStatement {
            number: 1,
            reference: "north/2027Q1".into(),
        },
    );
    assert_eq!(w.clearing(), 0, "nothing moves until the settlement is instructed");
    w.module(
        "finance",
        Command::InstructSettlement {
            number: 1,
            prepared_by: "kim".into(),
            approved_by: "lee".into(),
        },
    );
    assert!(matches!(w.bench.ri.statement(1).unwrap().status, StatementStatus::Instructed));
    assert_eq!(w.ledger("reinsurer-north"), (before - net as i128, net as i128));
    // Until the payment is confirmed the module still shows the statement as owed.
    assert_eq!(w.bench.ri.balance(&retail(), &north()) as i128, before);
    assert!(w.core.reconcile().is_empty());
    w.days(1);
}

/// Purpose: every instruction delivered a second time, as after a timeout, is recognised
/// by its key and changes nothing on any partition.
/// Why it matters: the driver retries until it hears back. Without this a lost reply
/// would post the reinsurer's premium twice.
#[test]
fn delivering_every_instruction_again_changes_nothing() {
    let (mut w, _, _) = two_partitions();
    w.days(3);
    assert!(w.delivered.len() >= 4);
    let before = w.hashes();
    for delivery in w.delivered.clone() {
        assert_eq!(delivery.caller, CALLER);
        let applied = w.core.submit_keyed(delivery.partition, &delivery.key, delivery.caller, delivery.command);
        assert!(applied.duplicate && applied.result.is_ok() && applied.entries.is_empty());
    }
    assert_eq!(w.hashes(), before);
    assert!(w.core.take_events().is_empty());
}

/// Purpose: a cession changed while its policy is moving between partitions is on the
/// policy's record where it lands, and the premium for it posts in the meantime.
/// Why it matters: the old partition refuses changes to a policy in flight and the new
/// one has not heard of it yet. Without the record being sent again on landing, the
/// change would be lost and the core would show as retained what has been ceded.
#[test]
fn a_cession_changed_during_a_transfer_is_on_the_record_where_the_policy_lands() {
    let mut w = World::new(d(2027, 3, 1));
    let (src, b) = w.issue(7, "party-kiri", d(2027, 3, 1), 300_000_00);
    let life = "life-1".into();
    assert_eq!(
        cession_on(&w.core.state(src).policies[&b], "life", 1, &life, d(2027, 3, 1))
            .unwrap()
            .retained_minor,
        300_000_00
    );
    let target_group = GroupId(
        (0..)
            .map(|i| format!("grp-new-{i}"))
            .find(|g| w.core.partition_of(&GroupId(g.clone())) != src)
            .unwrap(),
    );
    let dst = w.core.partition_of(&target_group);
    w.core
        .submit(
            src,
            "transfer",
            ic_core::Command::BeginTransfer {
                policy: b.clone(),
                transfer_id: "t-1".into(),
                target_group,
                target_partition: dst,
            },
        )
        .unwrap();
    w.pump();
    // While the policy is frozen, an application on the same life that ranks ahead of it
    // is issued: it takes 400,000 of the retention, so this policy now cedes 200,000.
    w.issue(1, "party-kiri", d(2027, 3, 1), 400_000_00);
    assert_eq!(w.dropped, 1, "the old partition refused the change");
    assert_eq!(
        cession_on(&w.core.state(src).policies[&b], "life", 1, &life, d(2027, 3, 1))
            .unwrap()
            .retained_minor,
        300_000_00
    );
    assert!(
        w.bench.ri.balance(&retail(), &north()) > 0,
        "the premium for the new cession posted all the same"
    );

    while w.core.messages_in_flight() > 0 {
        w.core.relay_messages(|_| Delivery::Deliver);
    }
    w.pump();
    assert_eq!(w.core.locate(&b), Some(dst));
    assert_eq!(w.dropped, 1);
    let landed = cession_on(&w.core.state(dst).policies[&b], "life", 1, &life, d(2027, 3, 1)).unwrap();
    assert_eq!(
        (landed.retained_minor, landed.ceded.iter().map(|(_, a)| a).sum::<i64>()),
        (100_000_00, 200_000_00)
    );
    w.assert_cessions_agree();

    // Later billing posts where the policy now is, and the ledgers still add up.
    w.days(35);
    assert!(reinsurer_balance(&w.core.state(dst).ledger, "retail-life-surplus", "reinsurer-north", NZD) > 0);
    assert_eq!(w.ledger("reinsurer-north").0, w.bench.ri.balance(&retail(), &north()) as i128);
    w.bench.assert_reconciles();
}

/// Purpose: an instruction for a treaty the module does not hold has no core command.
/// Why it matters: the entry needs the treaty's currency. Guessing one would post a
/// reinsurer's balance in the wrong currency; the driver must stop and raise it instead.
#[test]
fn an_instruction_for_an_unknown_treaty_is_not_translated() {
    let b = Bench::new();
    let instruction = Instruction {
        id: "ri-1".into(),
        partition: 0,
        body: InstructionBody::Sweep {
            treaty: "no-such-treaty".into(),
            reinsurer: north(),
            statement: 1,
            cut_seq: 0,
            amount_minor: 1,
        },
    };
    assert_eq!(b.ri.to_core(&instruction, d(2027, 4, 2)), None);
    assert_eq!(b.ri.treaty_currency(&retail()), Some(NZD));
    let aggregates = Bench::with_aggregates();
    assert_eq!(aggregates.ri.treaty_currency(&"catastrophe-2027".into()), Some(NZD));
}

/// Purpose: every kind of entry the module can instruct, and a cession with a facultative
/// share, is accepted by the core when translated, and the facultative share is on the
/// policy record afterwards.
/// Why it matters: the module and the core name their entries separately. A kind mapped
/// to the wrong entry would post a recovery as premium, and only running each one through
/// the real core shows the two lists still line up.
#[test]
fn every_kind_of_instruction_is_one_the_core_accepts() {
    use ic_reinsurance::PostingKind::*;
    let mut w = World::new(d(2027, 3, 1));
    let (p, id) = w.issue(1, "party-kiri", d(2027, 3, 1), 800_000_00);
    let mut instructions: Vec<Instruction> = [
        CededPremium,
        Commission,
        Recoverable,
        RecoveryDue,
        AggregateRecovery,
        ProfitCommission,
        DisputeSettlement,
        OpeningBalance,
    ]
    .into_iter()
    .enumerate()
    .map(|(n, kind)| Instruction {
        id: format!("test-{n}"),
        partition: p,
        body: InstructionBody::Post {
            kind,
            treaty: retail(),
            reinsurer: north(),
            policy: Some(id.clone()),
            amount_minor: 100 + n as i64,
            value_date: d(2027, 3, 1),
            reference: format!("test {n}"),
        },
    })
    .collect();
    let cession: ic_reinsurance::cession::Cession = serde_json::from_value(serde_json::json!({
        "treaty": {"id": "retail-life-surplus", "version": 1},
        "sum_at_risk_minor": 800_000_00,
        "retained_minor": 500_000_00,
        "ceded": [["reinsurer-north", 200_000_00]],
        "facultative": [{"reference": "FAC-9", "reinsurer": "reinsurer-south", "amount_minor": 100_000_00, "rate": 900_000}],
    }))
    .unwrap();
    instructions.push(Instruction {
        id: "test-cession".into(),
        partition: p,
        body: InstructionBody::RecordCession {
            key: key(&id.0, "life", 1, "life-1"),
            from: d(2027, 3, 1),
            segments: vec![(d(2027, 3, 1), Some(cession))],
        },
    });
    let kinds_before = w.delivered.len();
    w.deliver(&instructions);
    assert_eq!(w.delivered.len(), kinds_before + 9);
    let posted: Vec<String> = w
        .core
        .take_events()
        .into_iter()
        .filter_map(|rec| match rec.event {
            ic_core::event::Event::ReinsurancePosted { kind, reference, .. } if reference.starts_with("test ") => Some(format!("{kind:?}")),
            _ => None,
        })
        .collect();
    assert_eq!(
        posted,
        [
            "CededPremium",
            "Commission",
            "Recoverable",
            "RecoveryDue",
            "AggregateRecovery",
            "ProfitCommission",
            "DisputeSettlement",
            "OpeningBalance"
        ]
    );
    let recorded = cession_on(&w.core.state(p).policies[&id], "life", 1, &"life-1".into(), d(2027, 3, 1)).unwrap();
    assert_eq!(recorded.facultative.len(), 1);
    assert_eq!(
        (
            recorded.facultative[0].reference.as_str(),
            recorded.facultative[0].reinsurer.as_str(),
            recorded.facultative[0].amount_minor
        ),
        ("FAC-9", "reinsurer-south", 100_000_00)
    );
    assert!(w.core.reconcile().is_empty());
}
