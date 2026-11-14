//! Policies loaded from another system: commission from the old agent code.

mod common;

use std::collections::BTreeMap;

use common::*;
use ic_core::event::Event as CoreEvent;
use ic_core::{EventRecord, PolicyId};
use ic_distribution::config::Change;
use ic_distribution::host::{ask, Question};
use ic_distribution::migration::{LegacyAgent, MigrateCommission};
use ic_distribution::newbusiness::NewBusiness;
use ic_distribution::{BookId, Command, EngagementId};

fn migrated(w: &mut World, policy: &PolicyId, partition: u16) {
    let seq = w.dist.checkpoints[&partition] + 1;
    let record = EventRecord {
        id: format!("{partition}-{seq}"),
        partition,
        seq,
        business_date: w.today(),
        log_index: 0,
        traceparent: None,
        event: CoreEvent::PolicyMigrated {
            policy: policy.clone(),
            cohort: "cohort-1".into(),
            cutover: w.today(),
            periods_billed: 30,
            standing: None,
            outstanding_minor: 0,
            oldest_unpaid_due: None,
            assumptions: vec![],
        },
    };
    w.send(module("event-relay"), Command::Ingest(Box::new(record))).unwrap();
}

fn migrate(policy: &PolicyId, code: &str, premiums: &[(&str, i64)], last_paid: &[(&str, i64)]) -> Command {
    Command::MigrateCommission(Box::new(MigrateCommission {
        policy: policy.clone(),
        agent_code: code.into(),
        option: "upfront".into(),
        premiums: premiums.iter().map(|(c, p)| (c.to_string(), *p)).collect(),
        last_paid: last_paid.iter().map(|(p, a)| (party(p), *a)).collect::<BTreeMap<_, _>>(),
    }))
}

fn map(code: &str, engagement: &str, book: &str) -> Change {
    Change::MapLegacyAgent {
        code: code.into(),
        agent: Some(LegacyAgent {
            engagement: EngagementId::from(engagement),
            book: BookId::from(book),
        }),
    }
}

/// Purpose: (MIG-010) a policy loaded from another system is not a new sale. The migration load
/// maps the old system's single agent code to an engagement and a book, and the module writes
/// every layer from it: writing, servicing and the book's entitlement, with that assumption
/// recorded. The layers earn trail only, so no initial commission is paid and no clawback clock
/// starts at the cutover, including layers written before the core said the policy was
/// migrated. The module's renewal commission is checked against what the old system last paid
/// each payee: a difference beyond the tolerance raises a task, and agreement is recorded.
/// Anything the load cannot resolve is refused, and sending it again once fixed completes it.
/// Why it matters: paying initial commission again on a migrated book pays advisers twice for
/// sales made years ago, and a mapping nobody checks against the old payments pays the wrong
/// people for as long as the policies last.
#[test]
fn a_migrated_policy_is_written_from_its_agent_code_and_checked_against_what_was_last_paid() {
    let mut w = worked_example();
    let (policy, partition) = w.issue(1, 500_000_00, None);
    let (advised, advised_partition) = w.issue(2, 400_000_00, None);
    w.activate(
        "term-life",
        vec![Change::SetProductSchedule {
            product: "term-life".into(),
            cover_type: None,
            schedule: Some("standard".into()),
        }],
    )
    .unwrap();
    let advice = NewBusiness {
        adviser: party("adviser-a"),
        provider: party("fap-one"),
        option: "upfront".into(),
        replacement: false,
        premiums: [("life".to_string(), 900_00)].into(),
    };
    w.send(
        module("migration"),
        Command::AdviseNewBusiness {
            policy: advised.clone(),
            advice,
        },
    )
    .unwrap();
    assert!(w.dist.layers[&format!("{advised}/life/1")].terms.as_ref().unwrap().rates.initial_ppm > 0);
    let send = |w: &mut World, c: Command| w.send(module("migration"), c);
    assert_eq!(code(send(&mut w, migrate(&policy, "AG1", &[("life", 1_200_00)], &[]))), "not_migrated");

    // The core says both were loaded from another system. (Injected by hand, so last.)
    migrated(&mut w, &policy, partition);
    migrated(&mut w, &advised, advised_partition);
    assert_eq!(
        w.dist.layers[&format!("{advised}/life/1")].terms.as_ref().unwrap().rates.initial_ppm,
        0,
        "written before it was known"
    );

    assert_eq!(code(send(&mut w, migrate(&policy, "AG1", &[("life", 1_200_00)], &[]))), "unknown_agent_code");
    assert_eq!(code(w.activate("bad-1", vec![map("AG1", "nope", "a-at-fap-one")])), "unknown_engagement");
    assert_eq!(code(w.activate("bad-2", vec![map("AG1", "eng-a-one", "nope")])), "unknown_book");
    w.activate("agents", vec![map("AG1", "eng-a-one", "a-at-fap-one")]).unwrap();
    assert_eq!(code(send(&mut w, migrate(&policy, "AG1", &[], &[]))), "no_premium");
    assert_eq!(code(send(&mut w, migrate(&policy, "AG1", &[("life", 0)], &[]))), "invalid_migration");
    assert_eq!(
        code(w.send(module("new-business"), migrate(&policy, "AG1", &[("life", 1)], &[]))),
        "caller_not_allowed"
    );

    // Written, but what the old system last paid does not agree.
    send(&mut w, migrate(&policy, "AG1", &[("life", 1_200_00)], &[("a-co", 1_00)])).unwrap();
    let layer = &w.dist.layers[&format!("{policy}/life/1")];
    assert_eq!(layer.writing, vec![(EngagementId::from("eng-a-one"), ic_distribution::rules::WHOLE)]);
    assert_eq!(layer.terms.as_ref().unwrap().rates.initial_ppm, 0, "trail only");
    assert_eq!(w.dist.policies[&policy].servicing, Some(EngagementId::from("eng-a-one")));
    let record = w.dist.migrations[&policy].clone();
    assert!(!record.agrees);
    assert!(record.assumptions[0].contains("AG1"));
    assert!(w
        .dist
        .tasks
        .values()
        .any(|t| t.kind == "migration_commission_mismatch" && t.detail.contains("a-co")));
    let computed: Vec<(String, i64)> = record.checks.iter().map(|c| (c.payee.0.clone(), c.computed_minor)).collect();
    assert!(computed.iter().all(|(_, a)| *a > 0), "renewal for each payee of the rule: {computed:?}");
    assert_eq!(computed.iter().map(|(_, a)| a).sum::<i64>(), 1_200_00 / 10, "renewal only, at 10%");

    // Sent again with what the old system really paid, within a dollar: it agrees.
    let paid: Vec<(&str, i64)> = computed.iter().map(|(p, a)| (p.as_str(), a + 50)).collect();
    send(&mut w, migrate(&policy, "AG1", &[("life", 1_200_00)], &paid)).unwrap();
    assert!(w.dist.migrations[&policy].agrees);
    let answer = ask(&w.dist, Question::Migration { policy: policy.clone() }).unwrap();
    assert_eq!(answer["agent_code"], "AG1");
    w.activate(
        "unmap",
        vec![Change::MapLegacyAgent {
            code: "AG1".into(),
            agent: None,
        }],
    )
    .unwrap();
    assert!(w.dist.config.legacy_agents.is_empty());
}
