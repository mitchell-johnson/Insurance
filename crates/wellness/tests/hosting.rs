//! The module as the ledger nodes host it: the core's events it reads, the
//! questions it answers, its outbox, its keys, and the products it treats
//! as eligible.

use chrono::NaiveDate;
use ic_core::event::Event;
use ic_core::{EventRecord, GroupId, LifeId, PartyId, PolicyId, PolicyStatus};
use ic_log::StateMachine;
use ic_product::discount::{DiscountCondition, DiscountStep};
use ic_product::{load_product, Feature};
use ic_raft::module::{CoreOutcome, CorePosting, Route};
use ic_wellness::{
    answer, fact, module_spec, outcome_command, react, AnniversaryStatus, Command, CoreFact, Envelope, Status, WellnessState, CORE_EVENTS, ELIGIBLE,
    ENTITLEMENT, KEY_DAYS, MODULE, OUTBOX, PROGRAMME,
};
use serde_json::json;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn record(seq: u64, event: Event) -> EventRecord {
    EventRecord {
        id: format!("0-{seq}"),
        partition: 0,
        seq,
        business_date: d(2027, 3, 1),
        log_index: seq,
        traceparent: None,
        event,
    }
}

fn env(key: &str, caller: &str, today: NaiveDate, command: Command) -> Envelope {
    Envelope {
        key: key.into(),
        caller: caller.into(),
        today,
        command,
    }
}

/// Purpose: the products the module treats as eligible are exactly the shipped products whose
/// discount chains give a discount for the `aia-vitality` entitlement.
/// Why it matters: the list and the products must agree. A product added to the Vitality
/// discount but not to the list would never get its members' discount; a product on the list
/// without the discount would have every posting refused by the core.
#[test]
fn the_eligible_products_are_the_ones_whose_discounts_name_vitality() {
    let dir = format!("{}/../../products", env!("CARGO_MANIFEST_DIR"));
    let mut found = vec![];
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("star") {
            continue;
        }
        let name = path.file_name().unwrap().to_str().unwrap().to_string();
        let source = std::fs::read_to_string(&path).unwrap();
        let Ok(product) = load_product(&name, &source) else {
            continue;
        };
        let names_vitality = product.features.iter().any(|f| match f {
            Feature::Discounts { chain, .. } => chain.iter().any(|step| {
                let discounts = match step {
                    DiscountStep::One { discount } => vec![discount],
                    DiscountStep::Additive { discounts } | DiscountStep::BestOf { discounts } => discounts.iter().collect(),
                };
                discounts
                    .iter()
                    .any(|d| matches!(&d.condition, DiscountCondition::Entitlement { code } if code == ENTITLEMENT))
            }),
            _ => false,
        });
        if names_vitality {
            found.push(product.code.clone());
        }
    }
    found.sort();
    let mut eligible: Vec<String> = ELIGIBLE.iter().map(|s| s.to_string()).collect();
    eligible.sort();
    assert_eq!(found, eligible);
}

/// Purpose: the module reads the core's issue, bill, end and business date events and nothing
/// else, each keyed by the event's ID; a policy moving to another partition or lapsing is not an
/// end; the host finds it by its name, reading events and posting to the core.
/// Why it matters: the module learns of policies only from these events. Reading a lapse as an
/// end would stop flexing a policy that is reinstated; a key not tied to the event would take an
/// event in twice when the host offers it again.
#[test]
fn the_module_reads_the_cores_events_it_follows() {
    let spec = module_spec().unwrap();
    assert_eq!((spec.name.as_str(), spec.events, spec.outbox.is_some()), (MODULE, true, true));
    let made = (spec.make)();
    assert_eq!(made.query(json!({"q": "outbox", "limit": 1})).unwrap(), json!([]));

    let issued = Event::PolicyIssued {
        policy: PolicyId::from("P1"),
        number: "N1".into(),
        group: GroupId::from("G1"),
        policyholder: PartyId::from("party-1"),
        product_code: "aia-living".into(),
        product_hash: "h".into(),
        inception: d(2027, 1, 10),
        lives: vec![],
        layers: vec![],
        members: vec![],
    };
    let envs = react(&record(7, issued));
    assert_eq!(
        (envs[0].key.as_str(), envs[0].caller.as_str(), envs[0].today),
        ("core-0-7", CORE_EVENTS, d(2027, 3, 1))
    );
    assert!(matches!(&envs[0].command, Command::Core { fact: CoreFact::Issued { inception, .. } } if *inception == d(2027, 1, 10)));
    let opened = Event::BusinessDateAdvanced {
        from: Some(d(2027, 3, 1)),
        to: d(2027, 3, 2),
        closing_head: ic_ledger::Hash32::ZERO,
    };
    assert_eq!(react(&record(8, opened))[0].today, d(2027, 3, 2), "the day a date change opens is the new date");
    let status = |to| Event::PolicyStatusChanged {
        policy: PolicyId::from("P1"),
        from: PolicyStatus::InForce,
        to,
    };
    for to in [
        PolicyStatus::Cancelled,
        PolicyStatus::Terminated,
        PolicyStatus::Expired,
        PolicyStatus::Surrendered,
    ] {
        assert!(matches!(fact(&status(to)), Some(CoreFact::Ended { .. })), "{to:?}");
    }
    for to in [PolicyStatus::Lapsed, PolicyStatus::Transferred, PolicyStatus::InForce] {
        assert_eq!(fact(&status(to)), None, "{to:?}");
    }
    assert!(react(&record(
        9,
        Event::PolicyStatusChanged {
            policy: PolicyId::from("P1"),
            from: PolicyStatus::InForce,
            to: PolicyStatus::Lapsed,
        }
    ))
    .is_empty());
}

/// Purpose: the module answers its questions (a member, the outbox, the refusals, its state
/// hash) and refuses one it does not know; the host reads the outbox as postings routed by
/// policy under the module's own key, and each kind of outcome becomes the command that records
/// it.
/// Why it matters: this is the whole contract with the host. A posting under a fresh key would
/// be applied twice when offered again; an outcome not recorded would be offered for ever.
#[test]
fn the_host_reads_the_outbox_and_records_each_outcome() {
    let mut s = WellnessState::new();
    let today = d(2027, 3, 1);
    let issued = CoreFact::Issued {
        policy: PolicyId::from("P1"),
        product: "aia-living".into(),
        inception: d(2027, 1, 10),
    };
    s.apply(1, env("e1", CORE_EVENTS, today, Command::Core { fact: issued }));
    let join = Command::Join {
        member: "V1".into(),
        policy: PolicyId::from("P1"),
        life: LifeId::from("life-1"),
        joined: today,
    };
    let applied = s.apply(2, env("e2", PROGRAMME, today, join));
    assert_eq!(applied.postings.len(), 1);

    assert_eq!(answer(&s, json!({"q": "member", "member": "V1"})).unwrap()["discount"], 10);
    assert_eq!(answer(&s, json!({"q": "state_hash"})).unwrap(), json!(s.state_hash()));
    assert!(answer(&s, json!({"q": "nonsense"})).is_err());
    let pending = (OUTBOX.pending)(10);
    let read = (OUTBOX.read)(answer(&s, pending).unwrap()).unwrap();
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].route, Route::Policy(PolicyId::from("P1")));
    assert_eq!(
        (read[0].envelope.key.as_str(), read[0].envelope.caller.as_str()),
        ("wellness:w-0000000001", "wellness")
    );
    assert!((OUTBOX.read)(json!("not a list")).is_err());

    let posting: &CorePosting = &read[0];
    for (outcome, applied) in [
        (
            CoreOutcome::Applied {
                partition: 0,
                log_index: 1,
                duplicate: false,
            },
            true,
        ),
        (CoreOutcome::Refused(ic_core::Reject::new("unknown_entitlement", "no")), false),
        (CoreOutcome::Unroutable("gone".into()), false),
    ] {
        let command = outcome_command(posting, &outcome, today);
        assert!(matches!(&command.command, Command::RecordCoreOutcome { applied: a, .. } if *a == applied));
        let value = (OUTBOX.outcome)(posting, &outcome);
        assert_eq!(value["command"]["type"], "record_core_outcome");
    }
    let refused = outcome_command(posting, &CoreOutcome::Unroutable("gone".into()), today);
    assert_eq!(s.apply(3, refused).result, Ok(()));
    assert!(s.outbox.is_empty());
    assert_eq!(s.refused[0].code, "unroutable");
    let refusals = answer(&s, json!({"q": "refused", "limit": 5})).unwrap();
    assert_eq!(refusals[0]["message"], "gone");
    // An outcome for a posting already recorded changes nothing.
    let again = outcome_command(posting, &CoreOutcome::Unroutable("gone".into()), today);
    let again = Envelope {
        key: "outcome-again".into(),
        ..again
    };
    assert_eq!(s.apply(4, again).result, Ok(()));
    assert_eq!(s.refused.len(), 1);
}

/// Purpose: a command offered again under the same key gets its first answer and changes
/// nothing; a key is forgotten after 35 days; the module's day never goes back; a join to a
/// policy that has ended and a Vitality anniversary reported before an earlier one are refused;
/// reporting the same anniversary again replaces it; each command comes from its one caller; a
/// bill offered twice is remembered once.
/// Why it matters: delivery to a module is at least once. A join applied twice would post the
/// Initial Discount twice; a status out of order would flex a member on an old year.
#[test]
fn keys_dates_and_refusals_keep_the_state_sound() {
    let mut s = WellnessState::new();
    let today = d(2027, 3, 1);
    for (i, policy) in ["P1", "P2"].iter().enumerate() {
        let issued = CoreFact::Issued {
            policy: PolicyId::from(*policy),
            product: "aia-living".into(),
            inception: d(2027, 1, 10),
        };
        s.apply(i as u64 + 1, env(&format!("i{i}"), CORE_EVENTS, today, Command::Core { fact: issued }));
    }
    let join = |member: &str, policy: &str| Command::Join {
        member: member.into(),
        policy: PolicyId::from(policy),
        life: LifeId::from("life-1"),
        joined: today,
    };
    let first = s.apply(3, env("j1", PROGRAMME, today, join("V1", "P1")));
    let second = s.apply(4, env("j1", PROGRAMME, today, join("V1", "P1")));
    assert!(second.duplicate && second.index == first.index && s.outbox.len() == 1);

    let ended = CoreFact::Ended { policy: PolicyId::from("P2") };
    s.apply(5, env("x1", CORE_EVENTS, today, Command::Core { fact: ended }));
    assert_eq!(
        s.apply(6, env("j2", PROGRAMME, today, join("V2", "P2"))).result.unwrap_err().code,
        "policy_ended"
    );

    let report = |at: NaiveDate| Command::ReportStatus {
        member: "V1".into(),
        earned: Status::Gold,
        anniversary: Some(AnniversaryStatus {
            vitality_anniversary: at,
            status: Status::Gold,
        }),
    };
    assert_eq!(s.apply(7, env("r1", PROGRAMME, today, report(d(2028, 3, 1)))).result, Ok(()));
    assert_eq!(s.apply(8, env("r2", PROGRAMME, today, report(d(2028, 3, 1)))).result, Ok(()));
    assert_eq!(s.members["V1"].anniversaries.len(), 1, "the same anniversary again replaces it");
    assert_eq!(
        s.apply(9, env("r3", PROGRAMME, today, report(d(2027, 3, 1)))).result.unwrap_err().code,
        "out_of_order"
    );
    // Each command has one caller; any other is refused by name.
    let commands = [
        report(d(2028, 3, 1)),
        Command::Leave {
            member: "V1".into(),
            on: today,
        },
        Command::Core {
            fact: CoreFact::DayOpened { today },
        },
        Command::RecordCoreOutcome {
            posting: "w-1".into(),
            applied: true,
            code: None,
            message: None,
        },
    ];
    for (n, command) in commands.into_iter().enumerate() {
        let name = command.name();
        let refused = s.apply(20 + n as u64, env(&format!("c{n}"), "someone", today, command)).result.unwrap_err();
        assert_eq!((refused.code.as_str(), refused.message.ends_with(name)), ("not_allowed", true));
    }
    // A bill offered twice is remembered once.
    for k in ["b1", "b2"] {
        let billed = CoreFact::Billed {
            policy: PolicyId::from("P1"),
            period_start: today,
            period_end: d(2027, 4, 1),
        };
        s.apply(30, env(k, CORE_EVENTS, today, Command::Core { fact: billed }));
    }
    assert_eq!(s.policies[&PolicyId::from("P1")].billed.len(), 1);
    let leave = Command::Leave {
        member: "V9".into(),
        on: today,
    };
    assert_eq!(s.apply(10, env("l1", PROGRAMME, today, leave)).result.unwrap_err().code, "unknown_member");

    // An earlier date from a sender does not take the module's day back.
    s.apply(
        11,
        env(
            "late",
            CORE_EVENTS,
            d(2027, 2, 1),
            Command::Core {
                fact: CoreFact::DayOpened { today: d(2027, 2, 1) },
            },
        ),
    );
    assert_eq!(s.today, Some(today));

    // After 35 days the first join's key is forgotten: offered again it is a fresh command.
    let later = today + chrono::Duration::days(KEY_DAYS + 1);
    s.apply(
        12,
        env(
            "tick",
            CORE_EVENTS,
            later,
            Command::Core {
                fact: CoreFact::DayOpened { today: later },
            },
        ),
    );
    let again = s.apply(13, env("j1", PROGRAMME, later, join("V1", "P1")));
    assert!(!again.duplicate);
    assert_eq!(again.result.unwrap_err().code, "already_member");
}
