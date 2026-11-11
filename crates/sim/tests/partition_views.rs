//! Read models kept per partition, as the ledger nodes keep them, against
//! one read model of everything, as a single consumer of the stream keeps it.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_cashmatch::{Candidates, IncomingPayment, Matcher};
use ic_core::event::EventRecord;
use ic_core::{Command, GroupId, PolicyId};
use ic_ledger::PartitionId;
use ic_money::{Currency::NZD, Money};
use ic_product::{Frequency, Sex};
use ic_readmodel::Projections;
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, term_life};
use ic_sim::{Cluster, Delivery};

const PARTITIONS: PartitionId = 3;
const MONTHLY: i64 = 61_84;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn open() -> NaiveDate {
    d(2027, 11, 15)
}

/// The global read model and one per partition, fed the same events.
struct Views {
    global: Projections,
    per_partition: BTreeMap<PartitionId, Projections>,
}

impl Views {
    fn new() -> Self {
        Self {
            global: Projections::new(),
            per_partition: (0..PARTITIONS).map(|p| (p, Projections::new())).collect(),
        }
    }

    fn feed(&mut self, events: Vec<EventRecord>) {
        for event in events {
            self.global.apply(&event).unwrap();
            self.per_partition.get_mut(&event.partition).unwrap().apply(&event).unwrap();
        }
    }

    /// The directory entries of every partition's own view, by policy.
    fn union(&self) -> Vec<ic_readmodel::DirectoryEntry> {
        let mut entries: Vec<_> = self.per_partition.values().flat_map(|v| v.directory().cloned()).collect();
        entries.sort_by(|a, b| a.policy.cmp(&b.policy));
        entries
    }

    fn merged_candidates(&self, matcher: &Matcher, payment: &IncomingPayment) -> Candidates {
        self.per_partition
            .values()
            .map(|v| matcher.candidates(v, payment))
            .fold(Candidates::default(), Candidates::merge)
    }
}

fn payment(id: &str, minor: i64, reference: Option<&str>, account: Option<&str>) -> IncomingPayment {
    IncomingPayment {
        id: id.to_string(),
        amount: Money::new(minor, NZD),
        reference: reference.map(str::to_string),
        payer_account: account.map(str::to_string),
        received_on: open(),
    }
}

/// A cluster with six policies spread over the partitions, one of them
/// moved to another partition and one of them paid.
fn business() -> (Cluster, Views, PolicyId, PartitionId, PartitionId) {
    let mut cluster = Cluster::in_memory(PARTITIONS, 11);
    cluster.open(open()).unwrap();
    for activation in shipped_activations().unwrap() {
        cluster.activate_everywhere(activation).unwrap();
    }
    let mut views = Views::new();
    let mut homes = BTreeMap::new();
    for n in 1..=6u64 {
        let issue = term_life(n, open(), d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
        let (partition, policy) = (cluster.partition_of(&issue.group), issue.policy.clone());
        cluster.submit(partition, "new-business", issue_command(issue)).unwrap();
        homes.insert(policy, partition);
    }
    views.feed(cluster.take_events());

    // Move the first policy to a group on another partition.
    let (moved, source) = homes.iter().next().map(|(policy, partition)| (policy.clone(), *partition)).unwrap();
    let target_group = (0..)
        .map(|i| GroupId(format!("grp-new-{i}")))
        .find(|g| cluster.partition_of(g) != source)
        .unwrap();
    let target = cluster.partition_of(&target_group);
    cluster
        .submit(
            source,
            "transfer",
            Command::BeginTransfer {
                policy: moved.clone(),
                transfer_id: "t-1".to_string(),
                target_group,
                target_partition: target,
            },
        )
        .unwrap();
    while cluster.messages_in_flight() > 0 {
        cluster.relay_messages(|_| Delivery::Deliver);
    }
    views.feed(cluster.take_events());

    // Pay the last policy in full.
    let (paid, home) = homes.iter().next_back().map(|(policy, partition)| (policy.clone(), *partition)).unwrap();
    cluster
        .submit(
            home,
            "cash-matching",
            Command::ApplyReceipt {
                policy: paid,
                receipt: "r-1".to_string(),
                amount: Money::new(MONTHLY, NZD),
            },
        )
        .unwrap();
    views.feed(cluster.take_events());
    (cluster, views, moved, source, target)
}

/// Purpose: after a policy moves between partitions, the source partition's own read model
/// no longer lists it, the target's lists it with its number, billing position and layers,
/// and the per-partition directories together equal the single global directory.
/// Why it matters: each ledger node answers "do you hold this policy number" from its own
/// partition's read model. If the source kept listing a policy that had left, or the target
/// never learned of one that arrived, the gateway would route commands and payments to a
/// partition where the policy is only a stub, or report that an in-force policy does not
/// exist.
#[test]
fn a_partitions_own_view_follows_a_policy_that_moves() {
    let (cluster, views, moved, source, target) = business();
    let number = cluster.state(target).policies[&moved].number.clone();

    let at_source = &views.per_partition[&source];
    assert_eq!(at_source.policy(&moved), None);
    assert_eq!(at_source.by_number(&number), None);
    assert_eq!(at_source.billing(&moved), None);
    assert!(at_source.layers_on_policy(&moved).is_empty());

    let at_target = &views.per_partition[&target];
    let entry = at_target.by_number(&number).expect("the target does not list the policy that landed");
    assert_eq!(entry.policy, moved);
    assert_eq!(entry.partition, target);
    assert_eq!(Some(entry), views.global.policy(&moved));
    assert_eq!(at_target.billing(&moved), views.global.billing(&moved));
    assert_eq!(at_target.billing(&moved).unwrap().outstanding_minor, MONTHLY);
    assert_eq!(at_target.layers_on_policy(&moved), views.global.layers_on_policy(&moved));

    let mut global: Vec<_> = views.global.directory().cloned().collect();
    global.sort_by(|a, b| a.policy.cmp(&b.policy));
    assert_eq!(views.union(), global);
    assert_eq!(global.len(), 6);
    for entry in &global {
        assert_eq!(
            views.per_partition[&entry.partition].billing(&entry.policy),
            views.global.billing(&entry.policy)
        );
    }
}

/// Purpose: a read model written out and read back is the same read model, including the
/// tables keyed by a pair, and goes on applying events from where it stopped.
/// Why it matters: the nodes carry each partition's read models in the Raft snapshot. A
/// snapshot is JSON, whose object keys must be strings, so a table keyed by (policy, life)
/// needs its own encoding. If a restored read model differed from the one saved, a node
/// that restarted or was rebuilt from a snapshot would answer policy and payment queries
/// differently from its peers.
#[test]
fn a_read_model_survives_being_saved_and_loaded() {
    let (mut cluster, views, moved, _, target) = business();
    for (partition, view) in &views.per_partition {
        let saved = serde_json::to_string(view).unwrap();
        let loaded: Projections = serde_json::from_str(&saved).unwrap();
        assert_eq!(&loaded, view, "partition {partition}");
        assert_eq!(serde_json::to_string(&loaded).unwrap(), saved, "the encoding is not stable");
    }
    let saved = serde_json::to_string(&views.global).unwrap();
    let mut loaded: Projections = serde_json::from_str(&saved).unwrap();
    assert_eq!(loaded, views.global);
    assert!(!loaded.layers_on_policy(&moved).is_empty());

    // The loaded model carries on: the next event applies, an old one is a duplicate.
    let mut live = views.global.clone();
    cluster
        .submit(
            target,
            "cash-matching",
            Command::ApplyReceipt {
                policy: moved.clone(),
                receipt: "r-2".to_string(),
                amount: Money::new(MONTHLY, NZD),
            },
        )
        .unwrap();
    for event in cluster.take_events() {
        assert_eq!(loaded.apply(&event), Ok(true));
        assert_eq!(loaded.apply(&event), Ok(false));
        live.apply(&event).unwrap();
    }
    assert_eq!(loaded, live);
    assert_eq!(loaded.billing(&moved).unwrap().outstanding_minor, 0);
}

/// Purpose: for payments that match by reference, by payer authority, by amount and date
/// uniquely, ambiguously, and not at all, merging each partition's candidates gives the same
/// decision as matching against one read model of every partition.
/// Why it matters: the gateway cannot hold every policy, so it asks each partition for
/// candidates and merges them. Cash matching decides where customers' money goes: the
/// distributed answer must be the same answer, above all for "exactly one policy fits",
/// which no single partition can know.
#[test]
fn merged_partition_candidates_decide_as_one_read_model_does() {
    let (cluster, views, moved, _, target) = business();
    let number = cluster.state(target).policies[&moved].number.clone();
    let mut matcher = Matcher::default();
    matcher.authorities.insert("acct-moved".to_string(), moved.clone());
    matcher.authorities.insert("acct-unknown".to_string(), PolicyId("pol-nowhere".to_string()));

    let cases = [
        // By reference, to the policy that moved.
        payment("p-1", MONTHLY, Some(&format!("premium {number}")), None),
        // By the payer's authority.
        payment("p-2", MONTHLY, None, Some("acct-moved")),
        // An authority for a policy nobody holds falls through to amount and
        // date, where five unpaid policies fit: no match.
        payment("p-3", MONTHLY, None, Some("acct-unknown")),
        // Five fit by amount and date.
        payment("p-4", MONTHLY, Some("thanks"), None),
        // Nothing fits.
        payment("p-5", 1, None, None),
    ];
    for case in &cases {
        let merged = matcher.decide(views.merged_candidates(&matcher, case), case);
        assert_eq!(merged, matcher.match_payment(&views.global, case), "{}", case.id);
    }
    let by_reference = views.merged_candidates(&matcher, &cases[0]);
    assert_eq!(by_reference.reference.as_ref().map(|(_, m)| (&m.policy, m.partition)), Some((&moved, target)));
    assert_eq!(views.merged_candidates(&matcher, &cases[3]).amount_and_date_count, 5);

    // With every policy but one paid, amount and date identify it, and both
    // ways of asking agree on which.
    let (mut cluster, mut views) = (cluster, views);
    let unpaid: Vec<_> = views
        .global
        .directory()
        .filter(|e| views.global.billing(&e.policy).is_some_and(|b| b.outstanding_minor > 0))
        .map(|e| (e.policy.clone(), e.partition))
        .collect();
    assert_eq!(unpaid.len(), 5);
    for (n, (policy, partition)) in unpaid.iter().skip(1).enumerate() {
        cluster
            .submit(
                *partition,
                "cash-matching",
                Command::ApplyReceipt {
                    policy: policy.clone(),
                    receipt: format!("settle-{n}"),
                    amount: Money::new(MONTHLY, NZD),
                },
            )
            .unwrap();
    }
    views.feed(cluster.take_events());
    let lone = payment("p-6", MONTHLY, None, None);
    let candidates = views.merged_candidates(&matcher, &lone);
    assert_eq!(candidates.amount_and_date_count, 1);
    assert_eq!(candidates.amount_and_date.as_ref().map(|m| &m.policy), Some(&unpaid[0].0));
    assert_eq!(matcher.decide(candidates, &lone), matcher.match_payment(&views.global, &lone));

    // Between two references, the longer policy number wins whichever side it is on.
    let short = Candidates {
        reference: views.merged_candidates(&matcher, &cases[0]).reference.map(|(_, m)| (3, m)),
        ..Candidates::default()
    };
    let long = views.merged_candidates(&matcher, &cases[0]);
    assert_eq!(short.clone().merge(long.clone()).reference, long.reference);
    assert_eq!(long.clone().merge(short).reference, long.reference);
}
