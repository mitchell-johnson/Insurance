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

/// Purpose: the life index follows one person across a retail policy and a group scheme
/// through everything that changes what they are covered for: identification as a scheme
/// member, a cover expiring at its age limit, indexation adding a layer, layers merging,
/// and the scheme lapsing unpaid. At each point the sum at risk on the person equals what
/// the policies themselves say. A gap in the event sequence is an error, not a guess.
/// Why it matters: sum at risk per person is what reinsurance cessions and the underwriting
/// check on existing cover are computed from. Counting an expired or lapsed cover overstates
/// the insurer's exposure; missing a scheme member's cover understates it, and the first
/// anyone hears of that is a claim larger than the retention.
#[test]
fn the_life_index_follows_a_person_across_policies() {
    use ic_core::{LifeId, PartyId};
    use ic_readmodel::ProjectionError;
    use ic_sim::persona::{group_scheme, member};

    let mut cluster = Cluster::in_memory(1, 5);
    cluster.open(open()).unwrap();
    for activation in shipped_activations().unwrap() {
        cluster.activate_everywhere(activation).unwrap();
    }
    let mut view = Projections::new();
    let feed = |cluster: &mut Cluster, view: &mut Projections| {
        for event in cluster.take_events() {
            view.apply(&event).unwrap();
        }
    };
    let person = PartyId("party-00000001".to_string());

    // Aged 69 at issue: trauma cover (expiry age 70) has a year left.
    let retail = term_life(
        1,
        open(),
        d(1958, 3, 10),
        Sex::Female,
        false,
        500_000_00,
        Some(100_000_00),
        Frequency::Yearly,
        None,
    );
    let retail_id = retail.policy.clone();
    cluster.submit(0, "new-business", issue_command(retail)).unwrap();
    let scheme = group_scheme(2, open(), vec![member("m1", d(1958, 3, 10), "staff", 80_000_00)]);
    let scheme_id = scheme.policy.clone();
    cluster.submit(0, "new-business", issue_command(scheme)).unwrap();
    feed(&mut cluster, &mut view);

    // The scheme member is not yet known to be this person.
    assert_eq!(
        view.sum_at_risk(&person),
        BTreeMap::from([("life".to_string(), 500_000_00), ("trauma".to_string(), 100_000_00)])
    );
    assert_eq!(view.party_of(&scheme_id, &LifeId("m1".to_string())), None);
    assert_eq!(view.layers_on_party(&person).len(), 2);

    cluster
        .submit(
            0,
            "claims",
            Command::IdentifyMember {
                policy: scheme_id.clone(),
                life: LifeId("m1".to_string()),
                party: person.clone(),
            },
        )
        .unwrap();
    feed(&mut cluster, &mut view);
    assert_eq!(view.party_of(&scheme_id, &LifeId("m1".to_string())), Some(&person));
    let scheme_rows = view.layers_on_policy(&scheme_id);
    assert_eq!(scheme_rows.len(), 1);
    let (scheme_type, scheme_cover) = (scheme_rows[0].cover_type.clone(), scheme_rows[0].sum_insured_minor);
    assert_eq!(scheme_cover, 160_000_00, "twice an 80,000 salary");
    let mut expected = BTreeMap::from([("life".to_string(), 500_000_00), ("trauma".to_string(), 100_000_00)]);
    *expected.entry(scheme_type).or_insert(0) += scheme_cover;
    assert_eq!(view.sum_at_risk(&person), expected);

    // A year on: the retail policy is paid, the scheme never was and has
    // lapsed, trauma has expired and life cover has been indexed.
    cluster
        .submit(
            0,
            "cash-matching",
            Command::ApplyReceipt {
                policy: retail_id.clone(),
                receipt: "ahead".to_string(),
                amount: Money::new(500_000_00, NZD),
            },
        )
        .unwrap();
    for _ in 0..366 {
        assert!(cluster.end_of_day().unwrap().breaks.is_empty());
    }
    feed(&mut cluster, &mut view);
    assert_eq!(view.policy(&scheme_id).unwrap().status, ic_core::PolicyStatus::Lapsed);
    assert!(view.layers_on_policy(&scheme_id).iter().all(|r| !r.in_force));
    assert_eq!(view.sum_at_risk(&person), BTreeMap::from([("life".to_string(), 512_500_00)]));
    assert!(
        view.layers_on_policy(&retail_id).iter().all(|r| r.cover == "life"),
        "the expired cover is still indexed"
    );

    // Two more anniversaries, then the old layers are merged: fewer rows, the same cover.
    for _ in 0..800 {
        assert!(cluster.end_of_day().unwrap().breaks.is_empty());
    }
    feed(&mut cluster, &mut view);
    let today = cluster.business_date().unwrap();
    let covered = |cluster: &Cluster| cluster.state(0).policies[&retail_id].covers["life"].sum_insured_on(today);
    let rows_before = view.layers_on_policy(&retail_id).len();
    assert_eq!(view.sum_at_risk(&person)["life"], covered(&cluster));
    cluster
        .submit(
            0,
            "policy-servicing",
            Command::MergeLayers {
                policy: retail_id.clone(),
                cover: "life".to_string(),
            },
        )
        .unwrap();
    feed(&mut cluster, &mut view);
    assert!(view.layers_on_policy(&retail_id).len() < rows_before);
    assert_eq!(
        view.layers_on_policy(&retail_id).len(),
        cluster.state(0).policies[&retail_id].covers["life"].layers.len()
    );
    assert_eq!(view.sum_at_risk(&person)["life"], covered(&cluster));

    // An event that skips ahead is refused and changes nothing.
    cluster
        .submit(
            0,
            "policy-servicing",
            Command::DeclineIndexation {
                policy: retail_id,
                declined: true,
            },
        )
        .unwrap();
    let mut next = cluster.take_events();
    let at = view.checkpoint(0);
    let mut skipped = next.remove(0);
    skipped.seq += 1;
    assert_eq!(
        view.apply(&skipped),
        Err(ProjectionError::Gap {
            partition: 0,
            expected: at + 1,
            got: at + 2
        })
    );
    assert_eq!(view.checkpoint(0), at);
}

/// Purpose: a scheme member's cover in the life index follows the membership file. A
/// joiner appears with the formula's sum insured, a salary change replaces it, a leaver
/// disappears, the other members are untouched throughout, and the index with members in
/// it survives being saved and loaded.
/// Why it matters: scheme cover is a formula over member details, not layers on the
/// policy, so it reaches the index only through membership events. An index that kept a
/// leaver's cover, or the cover on an old salary, would feed the wrong sum at risk into
/// reinsurance accumulation for every employee of every scheme.
#[test]
fn scheme_members_cover_follows_the_membership_file() {
    use ic_core::command::MemberChange;
    use ic_core::policy::LifeFactors;
    use ic_core::{LifeId, PartyId};
    use ic_sim::persona::{group_scheme, member};

    let mut cluster = Cluster::in_memory(1, 5);
    cluster.open(open()).unwrap();
    for activation in shipped_activations().unwrap() {
        cluster.activate_everywhere(activation).unwrap();
    }
    let mut view = Projections::new();
    let feed = |cluster: &mut Cluster, view: &mut Projections| {
        for event in cluster.take_events() {
            view.apply(&event).unwrap();
        }
    };
    let scheme = group_scheme(1, open(), vec![member("m1", d(1980, 1, 1), "staff", 80_000_00)]);
    let scheme_id = scheme.policy.clone();
    cluster.submit(0, "new-business", issue_command(scheme)).unwrap();
    let membership = |cluster: &mut Cluster, change: MemberChange| {
        let command = Command::ApplyMembership {
            policy: scheme_id.clone(),
            changes: vec![change],
        };
        cluster.submit(0, "group-admin", command).unwrap();
    };
    let cover_of = |view: &Projections, life: &str| -> Vec<i64> {
        let rows = view.layers_on_policy(&scheme_id);
        rows.iter()
            .filter(|r| r.life == LifeId(life.to_string()))
            .map(|r| r.sum_insured_minor)
            .collect()
    };

    membership(
        &mut cluster,
        MemberChange::Joined {
            life: member("m2", d(1990, 6, 1), "staff", 50_000_00),
            effective: open(),
        },
    );
    feed(&mut cluster, &mut view);
    assert_eq!(cover_of(&view, "m1"), vec![160_000_00]);
    assert_eq!(cover_of(&view, "m2"), vec![100_000_00], "twice a 50,000 salary");

    // Identified, the joiner's cover counts against the person.
    let person = PartyId("party-00000042".to_string());
    let identify = Command::IdentifyMember {
        policy: scheme_id.clone(),
        life: LifeId("m2".to_string()),
        party: person.clone(),
    };
    cluster.submit(0, "claims", identify).unwrap();
    feed(&mut cluster, &mut view);
    assert_eq!(view.sum_at_risk(&person).values().sum::<i64>(), 100_000_00);

    membership(
        &mut cluster,
        MemberChange::Changed {
            life: LifeId("m2".to_string()),
            effective: open(),
            factors: LifeFactors {
                date_of_birth: Some(d(1990, 6, 1)),
                sex: None,
                smoker: None,
                category: Some("staff".to_string()),
                salary_minor: Some(60_000_00),
                ..Default::default()
            },
        },
    );
    feed(&mut cluster, &mut view);
    assert_eq!(cover_of(&view, "m2"), vec![120_000_00], "the old salary's cover is replaced, not added to");
    assert_eq!(view.sum_at_risk(&person).values().sum::<i64>(), 120_000_00);
    assert_eq!(cover_of(&view, "m1"), vec![160_000_00]);

    let saved = serde_json::to_string(&view).unwrap();
    let loaded: Projections = serde_json::from_str(&saved).unwrap();
    assert_eq!(loaded, view);
    assert_eq!(serde_json::to_string(&loaded).unwrap(), saved);

    membership(
        &mut cluster,
        MemberChange::Left {
            life: LifeId("m2".to_string()),
            effective: open(),
        },
    );
    feed(&mut cluster, &mut view);
    assert_eq!(cover_of(&view, "m2"), Vec::<i64>::new());
    assert!(view.sum_at_risk(&person).is_empty());
    assert_eq!(cover_of(&view, "m1"), vec![160_000_00]);
}
