//! A scheme's broker and the commission module: the layer written for each
//! scheme cover, and what happens when it cannot be.

mod common;

use std::collections::BTreeMap;

use common::*;
use ic_core::Reject;
use ic_group::command::GroupCommand;
use ic_group::event::{ExceptionKind, GroupEvent};
use ic_group::terms::SchemeBroker;

fn code(r: Result<(), Reject>) -> String {
    r.unwrap_err().code
}

fn broker(engagement: &str, premium: &[(&str, i64)]) -> SchemeBroker {
    SchemeBroker {
        engagement: engagement.into(),
        book: format!("{engagement}-schemes"),
        schedule: "standard".into(),
        option: "level".into(),
        expected_annual_premium_minor: premium.iter().map(|(c, m)| (c.to_string(), *m)).collect::<BTreeMap<_, _>>(),
    }
}

fn amend(h: &mut Harness, t: &ic_group::terms::SchemeTerms) -> Result<(), Reject> {
    h.staff(GroupCommand::AmendTerms {
        scheme: SCHEME.into(),
        terms: Box::new(t.clone()),
    })
}

/// Purpose: a scheme registered with a broker asks the commission module to write the
/// scheme cover's layer, layer 0, by the broker's engagement into the broker's book on the
/// agreed schedule and option with the scheme's expected annual premium; a direct scheme
/// asks nothing; a broker added later is sent once; repeating the same terms sends nothing
/// again; and a broker given without an expected premium for each scheme cover, or for a
/// cover the scheme does not have, or with blank names, is refused.
/// Why it matters: without the layer the broker earns nothing on the scheme however much
/// premium is paid, and a broker paid initial commission on a premium of zero, or twice,
/// is a cost nobody agreed to.
#[test]
fn a_schemes_broker_is_written_into_the_commission_module() {
    let direct = Harness::new(terms());
    assert!(direct.commission_layers().is_empty(), "a direct scheme earns nothing");

    let mut t = terms();
    t.broker = Some(broker("eng-broker-x", &[("group-life", 24_000_00)]));
    let h = Harness::new(t);
    let layers = h.commission_layers();
    assert_eq!(layers.len(), 1);
    let (_, key, layer) = &layers[0];
    assert_eq!(key, &format!("grp-dist:{SCHEME}:group-life"));
    assert_eq!(
        (&layer.policy, layer.cover.as_str(), layer.engagement.as_str(), layer.book.as_str()),
        (&h.policy, "group-life", "eng-broker-x", "eng-broker-x-schemes")
    );
    assert_eq!(
        (layer.schedule.as_str(), layer.option.as_str(), layer.annual_premium_minor),
        ("standard", "level", 24_000_00)
    );

    // Added after registration: sent once, and the same terms again send nothing.
    let mut h = Harness::new(terms());
    let mut t = terms();
    t.broker = Some(broker("eng-broker-x", &[("group-life", 24_000_00)]));
    amend(&mut h, &t).unwrap();
    assert_eq!(h.commission_layers().len(), 1);
    amend(&mut h, &t).unwrap();
    assert_eq!(h.commission_layers().len(), 1);

    // Terms that cannot be written.
    for wrong in [
        broker("eng-broker-x", &[]),
        broker("eng-broker-x", &[("group-life", 0)]),
        broker(" ", &[("group-life", 1)]),
        broker("eng-broker-x", &[("group-life", 1), ("group-tpd", 1)]),
        broker("eng-broker-x", &[("group-tpd", 1)]),
    ] {
        let mut t = terms();
        t.broker = Some(wrong.clone());
        let mut h = Harness::new(terms());
        assert_eq!(code(amend(&mut h, &t)), "invalid_terms", "{wrong:?}");
        assert!(h.commission_layers().is_empty());
    }
}

/// Purpose: a scheme's broker is not changed or removed by amending the scheme's terms:
/// that is refused with a code that says where it is done, and the layer already written
/// stays as it was.
/// Why it matters: a broker change is dated, and the outgoing broker may keep a share for
/// a handover period. Writing the layer again for the new broker would rewrite who placed
/// the scheme, a historical fact, and lose the outgoing broker's agreed share.
#[test]
fn a_brokers_change_is_not_made_by_amending_the_terms() {
    let mut t = terms();
    t.broker = Some(broker("eng-broker-x", &[("group-life", 24_000_00)]));
    let mut h = Harness::new(t.clone());
    let mut changed = t.clone();
    changed.broker = Some(broker("eng-broker-y", &[("group-life", 24_000_00)]));
    assert_eq!(code(amend(&mut h, &changed)), "broker_change");
    let mut removed = t.clone();
    removed.broker = None;
    assert_eq!(code(amend(&mut h, &removed)), "broker_change");
    assert_eq!(h.commission_layers().len(), 1);
    assert_eq!(h.scheme().terms.broker, t.broker);
}

/// Purpose: when the commission module refuses the scheme's layer for a reason waiting will
/// not change (the broker is not authorised for the product, the book or schedule does not
/// exist), the entry leaves the outbox, staff are given an exception naming the cover, and
/// the refusal is published with its code; hearing the same refusal again changes nothing.
/// Why it matters: a refused layer means the broker is not paid. Retried for ever it would
/// look like a delay; dropped silently it would be found only when the broker complains.
#[test]
fn a_refused_layer_is_put_in_front_of_staff_once() {
    let mut t = terms();
    t.broker = Some(broker("eng-broker-x", &[("group-life", 24_000_00)]));
    let mut h = Harness::new(t);
    let (_, key, _) = h.commission_layers()[0].clone();
    h.driver(GroupCommand::CommissionRefused {
        key: key.clone(),
        code: "not_authorised_to_advise".into(),
    })
    .unwrap();
    h.settle();
    assert!(h.commission_layers().is_empty());
    let open: Vec<_> = h.g.exceptions.values().filter(|e| e.kind == ExceptionKind::BrokerNotRecorded).collect();
    assert_eq!((open.len(), open[0].subject.as_str()), (1, "group-life"));
    assert!(h.saw(|e| matches!(e, GroupEvent::BrokerNotRecorded { cover, code, .. } if cover == "group-life" && code == "not_authorised_to_advise")));
    h.driver(GroupCommand::CommissionRefused {
        key,
        code: "not_authorised_to_advise".into(),
    })
    .unwrap();
    assert_eq!(h.g.exceptions.values().filter(|e| e.kind == ExceptionKind::BrokerNotRecorded).count(), 1);
}
