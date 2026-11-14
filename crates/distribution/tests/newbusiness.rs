//! New business attributed to its adviser: the module writes the layers.

mod common;

use std::collections::BTreeMap;

use common::*;
use ic_core::PolicyId;
use ic_distribution::config::Change;
use ic_distribution::newbusiness::NewBusiness;
use ic_distribution::{BookId, Command, EngagementId};

fn advice(premiums: &[(&str, i64)], replacement: bool) -> NewBusiness {
    NewBusiness {
        adviser: party("adviser-a"),
        provider: party("fap-one"),
        option: "upfront".into(),
        replacement,
        premiums: premiums.iter().map(|(c, p)| (c.to_string(), *p)).collect::<BTreeMap<_, _>>(),
    }
}

fn advise(w: &mut World, policy: &PolicyId, advice: NewBusiness) -> Result<Vec<ic_distribution::Event>, ic_core::Reject> {
    let r = w.send(
        module("new-business"),
        Command::AdviseNewBusiness {
            policy: policy.clone(),
            advice,
        },
    );
    w.sync();
    r
}

fn named_schedule(w: &mut World) {
    let change = Change::SetProductSchedule {
        product: "term-life".into(),
        cover_type: None,
        schedule: Some("standard".into()),
    };
    w.activate("term-life-schedule", vec![change]).unwrap();
}

/// Purpose: new business names the adviser, the provider, the option and each cover's premium,
/// and the module writes every layer of the policy: the engagement between that adviser and
/// that provider, the book it owns, and the schedule set for the product. It works whether the
/// advice arrives after the policy is issued or before, and the policy then earns commission
/// as a layer written by hand does. A declared replacement earns trail only.
/// Why it matters: without it nothing writes the layers of retail business, and advisers sell
/// policies that never pay them. New business cannot know this module's engagements and books,
/// so the module has to resolve them itself.
#[test]
fn new_business_advice_writes_the_policys_layers_before_or_after_issue() {
    let mut w = worked_example();
    named_schedule(&mut w);
    // After issue.
    let (policy, _) = w.issue(1, 500_000_00, None);
    advise(&mut w, &policy, advice(&[("life", 1_200_00)], false)).unwrap();
    let layer = &w.dist.layers[&format!("{policy}/life/1")];
    assert_eq!(layer.writing, vec![(EngagementId::from("eng-a-one"), ic_distribution::rules::WHOLE)]);
    assert_eq!(layer.books_on(layer.start), &[(BookId::from("a-at-fap-one"), ic_distribution::rules::WHOLE)]);
    let terms = layer.terms.clone().unwrap();
    assert_eq!(
        (terms.schedule.as_str(), terms.option.as_str(), terms.annual_premium_minor),
        ("standard", "upfront", 1_200_00)
    );
    w.pay(&policy, "r1");
    assert!(w.payable("a-co") > 0, "the policy earns commission");
    assert_eq!(code(advise(&mut w, &policy, advice(&[("life", 1_200_00)], false))), "already_advised");

    // Before issue: it waits for the policy.
    let early = PolicyId("pol-00000002".into());
    advise(&mut w, &early, advice(&[("life", 900_00), ("trauma", 300_00)], true)).unwrap();
    assert!(w.dist.new_business.contains_key(&early));
    assert_eq!(code(advise(&mut w, &early, advice(&[("life", 1)], false))), "already_advised");
    let (issued, _) = w.issue(2, 400_000_00, Some(100_000_00));
    assert_eq!(issued, early);
    assert!(w.dist.new_business.is_empty(), "taken when the policy arrived");
    for cover in ["life", "trauma"] {
        let terms = w.dist.layers[&format!("{early}/{cover}/1")].terms.clone().unwrap();
        assert_eq!(terms.rates.initial_ppm, 0, "a declared replacement earns trail only");
    }
    assert!(w.dist.tasks.values().all(|t| t.kind != "new_business_unattributed"));
}

/// Purpose: advice the module cannot act on is refused when it is malformed, and when it cannot
/// be resolved (no engagement between the adviser and provider, no schedule for the product,
/// no premium for a cover) the layers it can write are written and a task names each one it
/// could not, with the reason, for commission operations.
/// Why it matters: the advice arrives on its own, often before the policy, so a refusal later
/// would reach nobody. An unwritten layer is unpaid commission, and someone has to be told.
#[test]
fn advice_that_cannot_be_resolved_raises_a_task_naming_each_layer() {
    let mut w = worked_example();
    let (policy, _) = w.issue(1, 500_000_00, Some(100_000_00));
    assert_eq!(code(advise(&mut w, &policy, advice(&[], false))), "invalid_new_business");
    assert_eq!(code(advise(&mut w, &policy, advice(&[("life", 0)], false))), "invalid_new_business");
    let blank = NewBusiness {
        option: " ".into(),
        ..advice(&[("life", 1)], false)
    };
    assert_eq!(code(advise(&mut w, &policy, blank)), "invalid_new_business");
    assert_eq!(
        code(w.send(
            module("claims"),
            Command::AdviseNewBusiness {
                policy: policy.clone(),
                advice: advice(&[("life", 1)], false)
            }
        )),
        "caller_not_allowed"
    );

    // No schedule is named for the product yet.
    advise(&mut w, &policy, advice(&[("life", 1_200_00)], false)).unwrap();
    let task = |w: &World| {
        w.dist
            .tasks
            .values()
            .filter(|t| t.kind == "new_business_unattributed")
            .map(|t| t.detail.clone())
            .collect::<Vec<_>>()
    };
    let found = task(&w);
    assert_eq!(found.len(), 1);
    assert!(
        found[0].contains("life/1") && found[0].contains("no_schedule") && found[0].contains("trauma/1"),
        "{found:?}"
    );

    // With a schedule: the life layer is written; trauma has no premium; another provider has no engagement.
    named_schedule(&mut w);
    let (second, _) = w.issue(2, 500_000_00, Some(100_000_00));
    advise(&mut w, &second, advice(&[("life", 1_200_00)], false)).unwrap();
    assert!(w.dist.layers[&format!("{second}/life/1")].terms.is_some());
    assert!(task(&w).iter().any(|t| t.contains("trauma/1") && t.contains("no_premium")));
    let (third, _) = w.issue(3, 500_000_00, None);
    let elsewhere = NewBusiness {
        provider: party("fap-two"),
        ..advice(&[("life", 1_200_00)], false)
    };
    advise(&mut w, &third, elsewhere).unwrap();
    assert!(task(&w).iter().any(|t| t.contains("no_engagement")));

    // An engagement with no book of its own.
    w.engage("eng-a-two", "adviser-a", "fap-two", d(2020, 1, 1));
    let (fourth, _) = w.issue(4, 500_000_00, None);
    let bookless = NewBusiness {
        provider: party("fap-two"),
        ..advice(&[("life", 1_200_00)], false)
    };
    advise(&mut w, &fourth, bookless).unwrap();
    assert!(task(&w).iter().any(|t| t.contains("no_book")));
}

/// Purpose: a product schedule can be set for one cover type of a product, and that wins over
/// the product's own schedule for layers of that cover type.
/// Why it matters: income cover is paid on different terms from life cover on the same policy;
/// one schedule per product would pay one of them wrongly.
#[test]
fn a_cover_type_schedule_wins_over_the_products() {
    let mut w = worked_example();
    named_schedule(&mut w);
    let mut other = schedule();
    other.id = "trauma-terms".into();
    let change = Change::SetProductSchedule {
        product: "term-life".into(),
        cover_type: Some("trauma".into()),
        schedule: Some("trauma-terms".into()),
    };
    w.activate("trauma", vec![Change::ActivateSchedule(other), change]).unwrap();
    let (policy, _) = w.issue(1, 500_000_00, Some(100_000_00));
    advise(&mut w, &policy, advice(&[("life", 1_200_00), ("trauma", 400_00)], false)).unwrap();
    let schedule_of = |cover: &str| w.dist.layers[&format!("{policy}/{cover}/1")].terms.clone().unwrap().schedule;
    assert_eq!(
        (schedule_of("life"), schedule_of("trauma")),
        ("standard".to_string(), "trauma-terms".to_string())
    );
}
