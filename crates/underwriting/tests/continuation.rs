//! A scheme leaver continuing underwritten cover above the automatic limit.

mod common;

use common::*;
use ic_core::{LifeId, PolicyId};
use ic_underwriting::command::{Command, Effect, IssuedLayer};
use ic_underwriting::model::{CoverOutcome, DecisionSource, Entry, ProposalStatus, Stage};

/// Open, decide and issue a member's excess on the scheme: `smoker` gives it a loading.
fn excess(w: &mut World, id: &str, sum: i64, smoker: bool) {
    let mut open = w.open_command(id, &["L1"], &[("L1", "life", "group_life", sum)], false);
    open.product_code = "group-life".into();
    open.policy = PolicyId::from("SCHEME-1");
    open.entry = Entry::GroupExcess {
        policy: open.policy.clone(),
        takeover_accepted_minor: None,
        reinsurance_reference: None,
        delegated_authority: None,
    };
    w.ok("group-admin", Command::OpenProposal(Box::new(open)));
    let disclosed = w.disclose(id, "L1", &if smoker { yes("smoked_last_12_months") } else { clean() });
    w.automatic(&disclosed);
    if smoker {
        w.respond(id, "L1", ic_underwriting::command::OfferResponse::Accept);
    }
    w.ok(
        "issuer",
        Command::PolicyIssued {
            proposal: id.into(),
            layers: vec![IssuedLayer {
                life: "L1".into(),
                cover: "life".into(),
                layer: 2,
            }],
        },
    );
}

fn continuation(w: &World, id: &str, sum: i64, earlier: &[&str]) -> Command {
    let mut open = w.open_command(id, &["L1"], &[("L1", "life", "life", sum)], false);
    open.channel = "group-continuation".into();
    open.entry = Entry::GroupContinuation {
        policy: open.policy.clone(),
        scheme_policy: PolicyId::from("SCHEME-1"),
        scheme_life: LifeId::from("M7"),
        inception: w.today(),
        earlier: earlier.iter().map(|e| (*e).into()).collect(),
    };
    Command::OpenProposal(Box::new(open))
}

/// Purpose: a leaver continues cover above the automatic limit. The amount is accepted when
/// the proposal opens, with no disclosure and no offer, on the terms the member's earlier
/// excess decisions accepted (the heaviest loading where they differ), with the continuation
/// reason; capacity is reserved as for any retail cover, the terms go to the continuation
/// policy as a layer, and once it is written group administration is told it was issued.
/// Only group administration may open one, and only on earlier excess proposals that are
/// issued, on this member, and accepted at least the amount continued.
/// Why it matters: the member was underwritten once and accepted those terms; asking them
/// again, or offering different terms, breaks the scheme's promise. But nothing beyond what
/// was accepted may be carried over without underwriting, or the continuation becomes a way
/// round it.
#[test]
fn a_leaver_continues_the_excess_on_the_terms_already_accepted() {
    let mut w = World::new();
    excess(&mut w, "grp-uw:S1:1", 200_000_00, false);
    excess(&mut w, "grp-uw:S1:2", 100_000_00, true);

    assert_eq!(w.err("new-business", continuation(&w, "grp-cont:S1:9", 300_000_00, &["grp-uw:S1:1"])), "entry");
    let refused = |w: &mut World, sum: i64, earlier: &[&str]| w.err("group-admin", continuation(w, "grp-cont:S1:9", sum, earlier));
    assert_eq!(refused(&mut w, 300_000_00, &[]), "continuation");
    assert_eq!(refused(&mut w, 300_000_00, &["grp-uw:S1:1"]), "continuation", "more than was accepted");
    assert_eq!(refused(&mut w, 100_000_00, &["grp-uw:S9:9"]), "continuation", "unknown");
    let mut later = continuation(&w, "grp-cont:S1:9", 100_000_00, &["grp-uw:S1:1"]);
    if let Command::OpenProposal(o) = &mut later {
        if let Entry::GroupContinuation { inception, .. } = &mut o.entry {
            *inception = w.today() + chrono::Duration::days(1);
        }
    }
    assert_eq!(w.err("group-admin", later), "continuation", "not started before it is taken up");
    let mut stranger = continuation(&w, "grp-cont:S1:9", 100_000_00, &["grp-uw:S1:1"]);
    if let Command::OpenProposal(o) = &mut stranger {
        o.lives[0].party = "PTY-OTHER".into();
    }
    assert_eq!(w.err("group-admin", stranger), "continuation", "another member's excess");

    let opened = w.ok("group-admin", continuation(&w, "grp-cont:S1:9", 300_000_00, &["grp-uw:S1:1", "grp-uw:S1:2"]));
    let f = &w.p("grp-cont:S1:9").lives[&LifeId::from("L1")];
    assert!(f.disclosures.is_empty());
    let d = &f.decisions[0];
    assert!(matches!(&d.source, DecisionSource::Continuation { earlier } if earlier.len() == 2));
    assert_eq!(d.reasons, set(&["RC-0976"]));
    assert!(matches!(d.terms_for("life"), Some(CoverOutcome::Accept { loading_percent: 50, .. })));
    let applied = w.automatic(&opened);
    assert_eq!(w.stage("grp-cont:S1:9", "L1"), Stage::Accepted, "no offer for terms already accepted");
    let Some(Effect::ApplyTerms { covers, entry, .. }) = applied.effects.iter().find(|e| matches!(e, Effect::ApplyTerms { .. })) else {
        panic!("no terms: {:?}", applied.effects)
    };
    assert!(matches!(entry, Entry::GroupContinuation { .. }));
    assert_eq!((covers[0].sum_insured_minor, covers[0].loadings[0].as_str()), (300_000_00, "0.50"));

    let issued = w.ok(
        "issuer",
        Command::PolicyIssued {
            proposal: "grp-cont:S1:9".into(),
            layers: vec![],
        },
    );
    assert!(matches!(w.p("grp-cont:S1:9").status, ProposalStatus::Issued { .. }));
    assert!(issued
        .effects
        .iter()
        .any(|e| matches!(e, Effect::ProposalEnded { outcome, entry: Entry::GroupContinuation { .. }, .. } if outcome == "issued")));
    // Group administration may withdraw one it opened, and hears it ended.
    w.ok("group-admin", continuation(&w, "grp-cont:S1:10", 100_000_00, &["grp-uw:S1:1"]));
    let withdrawn = w.ok(
        "group-admin",
        Command::Withdraw {
            proposal: "grp-cont:S1:10".into(),
        },
    );
    assert!(withdrawn
        .effects
        .iter()
        .any(|e| matches!(e, Effect::ProposalEnded { outcome, .. } if outcome == "withdrawn")));
}

/// Purpose: a continuation carries every exclusion the member's earlier excess decisions
/// accepted, each once, running from the continuation's inception; and an earlier excess that
/// is decided but not yet issued is no ground for one.
/// Why it matters: an exclusion the member accepted on the scheme is part of the terms they were
/// underwritten on, so dropping it on leaving would cover a condition nobody agreed to cover;
/// and cover that never went into force on the scheme cannot be continued from it.
#[test]
fn a_continuation_keeps_the_earlier_exclusions_and_needs_issued_cover() {
    use ic_underwriting::codes::AppliedExclusion;
    use ic_underwriting::model::CoverTerms;
    let mut w = World::new();
    let excluded = |from| CoverTerms {
        cover: "life".into(),
        outcome: CoverOutcome::Accept {
            loading_percent: 0,
            exclusions: vec![AppliedExclusion {
                code: "EX-4001".into(),
                from,
                until: None,
            }],
            revised_sum_insured_minor: None,
        },
    };
    for id in ["grp-uw:S1:1", "grp-uw:S1:2"] {
        let mut open = w.open_command(id, &["L1"], &[("L1", "life", "group_life", 150_000_00)], false);
        open.product_code = "group-life".into();
        open.policy = PolicyId::from("SCHEME-1");
        open.entry = Entry::GroupExcess {
            policy: open.policy.clone(),
            takeover_accepted_minor: None,
            reinsurance_reference: None,
            delegated_authority: None,
        };
        w.ok("group-admin", Command::OpenProposal(Box::new(open)));
        w.disclose(id, "L1", &yes("heart_condition"));
        let case = w.take(id, "L1", "amy");
        let proposed = w.propose(&case, "amy", vec![excluded(w.today())]).unwrap();
        w.automatic(&proposed);
        w.respond(id, "L1", ic_underwriting::command::OfferResponse::Accept);
    }
    assert_eq!(
        w.err("group-admin", continuation(&w, "grp-cont:S1:9", 100_000_00, &["grp-uw:S1:1"])),
        "continuation",
        "decided but not issued"
    );
    for id in ["grp-uw:S1:1", "grp-uw:S1:2"] {
        w.ok(
            "issuer",
            Command::PolicyIssued {
                proposal: id.into(),
                layers: vec![IssuedLayer {
                    life: "L1".into(),
                    cover: "life".into(),
                    layer: 2,
                }],
            },
        );
    }
    w.ok("group-admin", continuation(&w, "grp-cont:S1:9", 300_000_00, &["grp-uw:S1:1", "grp-uw:S1:2"]));
    let d = &w.p("grp-cont:S1:9").lives[&LifeId::from("L1")].decisions[0];
    let Some(CoverOutcome::Accept { exclusions, .. }) = d.terms_for("life") else {
        panic!("not accepted: {d:?}")
    };
    assert_eq!(
        exclusions.iter().map(|e| (e.code.as_str(), e.from)).collect::<Vec<_>>(),
        vec![("EX-4001", w.today())]
    );
}
