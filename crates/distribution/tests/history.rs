//! Keeping the module's state bounded: old settled history let go of.

mod common;

use common::*;
use ic_core::Command as CoreCommand;
use ic_distribution::host::{ask, Question};
use ic_distribution::state::Settings;
use ic_distribution::Command;

fn short_history() -> Settings {
    Settings {
        history_days: 60,
        ..Settings::default()
    }
}

/// Purpose: once a month, history older than the setting that nothing can
/// reach is let go of: a policy cancelled long ago with everything settled
/// leaves whole, movements already on a statement fold into a brought-forward
/// balance, and old finished records go. A policy still in force keeps every
/// allocation. Balances, the proof of debt's totals, the reconciliation with
/// the core and a replica replaying the log all still agree afterwards.
/// Why it matters: the module is replicated and snapshotted. Kept line by
/// line for ever, its state grows with every premium and every snapshot gets
/// slower; let go of carelessly, a clawback or a statement would come out
/// wrong. This test pins both sides.
#[test]
fn old_settled_history_is_let_go_of_and_every_balance_still_agrees() {
    let (mut w, old, _) = advised_in(worked_example_in(World::with(1, short_history())));
    let (live, _) = w.issue(2, 300_000_00, None);
    w.write(&live, "life", 1, "eng-a-one", "a-at-fap-one", 600_00).unwrap();
    w.pay(&live, "l1");
    w.days(40);
    w.core(&old, "policy-servicing", CoreCommand::CancelPolicy { policy: old.clone() }).unwrap();
    w.sync();
    w.cut("a-co").unwrap();
    let movements_cut = w.dist.payees[&party("a-co")].statement_cut;
    w.send(
        finance(),
        Command::AttachDocument {
            party: party("fap-one"),
            kind: "correspondence".into(),
            document_ref: "old-letter".into(),
        },
    )
    .unwrap();
    let as_at = w.today();
    let live_allocations: Vec<String> = w.dist.allocations.values().filter(|a| a.policy == live).map(|a| a.reference.clone()).collect();

    for month in 2..6 {
        w.days(30);
        w.pay(&live, &format!("l{month}"));
    }
    w.sync();

    let h = &w.dist.history;
    assert!(h.compacted_before.is_some(), "compaction ran");
    assert!(h.policies_let_go.contains(&old), "the cancelled policy was let go: {:?}", h.last_run);
    assert!(!w.dist.policies.contains_key(&old));
    assert!(!w.dist.allocations.values().any(|a| a.policy == old));
    assert!(!w.dist.exposures.values().any(|e| e.policy == old));
    assert!(w.dist.policies.contains_key(&live), "a policy in force stays");
    assert!(live_allocations.iter().all(|r| w.dist.allocations.contains_key(r)), "its allocations all stay");
    let cutoff = h.compacted_before.unwrap();
    assert!(
        !w.dist
            .movements
            .iter()
            .any(|m| m.party == party("a-co") && m.date < cutoff && m.seq <= movements_cut),
        "movements on a statement before the cutoff were folded"
    );
    assert!(h.brought_forward.contains_key(&party("a-co")));
    assert!(!w.dist.documents.contains_key(&party("fap-one")), "an old document record goes");
    assert!(h.last_run.contains_key("documents"));
    w.assert_reconciled();
    let answer = ask(&w.dist, Question::History).unwrap();
    assert_eq!(answer["compacted_before"], serde_json::json!(w.dist.history.compacted_before));

    // A replica that replays the same log lets go of the same things, and one
    // that keeps everything has the same balances and the same proof of debt.
    let replay = |settings: Settings| {
        let mut replica = ic_distribution::DistributionState::new(settings);
        for (index, envelope) in &w.log {
            use ic_log::StateMachine;
            replica.apply(*index, envelope.clone());
        }
        replica
    };
    assert_eq!(replay(w.dist.settings.clone()).state_hash(), w.dist.state_hash());
    let full = replay(Settings {
        history_days: 0,
        ..w.dist.settings.clone()
    });
    assert!(full.policies.contains_key(&old) && full.movements.len() > w.dist.movements.len());
    assert_eq!(full.positions, w.dist.positions, "balances do not change");
    let today = w.today();
    for who in ["a-co", "fap-one"] {
        let (kept, compacted) = (full.proof_of_debt(&party(who), today), w.dist.proof_of_debt(&party(who), today));
        assert_eq!(
            (kept.owed_by_party_minor, kept.owed_to_party_minor, kept.net_minor),
            (compacted.owed_by_party_minor, compacted.owed_to_party_minor, compacted.net_minor),
            "the proof of debt for {who} still adds up"
        );
    }
    assert_eq!(w.dist.proof_of_debt(&party("a-co"), as_at).lines[0].0, "0000-brought-forward");

    // Business carries on: the live policy still earns.
    let payable = w.payable("a-co") + w.held("a-co");
    w.days(30);
    w.pay(&live, "l9");
    assert!(w.payable("a-co") + w.held("a-co") > payable);
    w.assert_reconciled();
}

/// Purpose: with history kept for ever (zero), nothing is let go of however
/// much time passes; a policy with commission still held from it is never
/// let go of, whatever its age.
/// Why it matters: the setting is how the business chooses to keep detail
/// longer, and held money is never forgotten, because the workbench must
/// still be able to release or write it off line by line.
#[test]
fn history_kept_for_ever_or_still_held_is_not_let_go_of() {
    let forever = Settings {
        history_days: 0,
        ..Settings::default()
    };
    let (mut w, old, _) = advised_in(worked_example_in(World::with(1, forever)));
    w.days(40);
    w.core(&old, "policy-servicing", CoreCommand::CancelPolicy { policy: old.clone() }).unwrap();
    w.days(120);
    w.sync();
    assert!(w.dist.history.compacted_before.is_none());
    assert!(w.dist.policies.contains_key(&old));

    // FAP One's share is held while its licence is suspended.
    let (mut w, old, _) = advised_in(worked_example_in(World::with(1, short_history())));
    let today = w.today();
    w.licence("fap-one", ic_distribution::structure::LicenceState::Suspended, today).unwrap();
    w.days(31);
    w.pay(&old, "r2");
    assert!(w.held("fap-one") > 0);
    w.days(40);
    w.core(&old, "policy-servicing", CoreCommand::CancelPolicy { policy: old.clone() }).unwrap();
    w.days(120);
    w.sync();
    assert!(w.dist.history.compacted_before.is_some());
    assert!(w.dist.policies.contains_key(&old), "held money keeps its policy");
    w.assert_reconciled();
}
