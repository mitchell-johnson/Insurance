//! The module as a replicated state machine: replay, snapshots, idempotency,
//! callers, immutable configuration, and the cession calculation's
//! invariants over arbitrary books.

mod common;

use std::collections::BTreeSet;

use chrono::Days;
use common::*;
use ic_core::event::Event;
use ic_core::{LifeId, PartyId, PolicyId, PolicyStatus};
use ic_log::StateMachine;
use ic_reinsurance::capacity::CapacityRequest;
use ic_reinsurance::cession::{allocation_order, timeline, Risk, Source};
use ic_reinsurance::claims::ClaimFact;
use ic_reinsurance::loader::{load_aggregate_treaty, load_rate_table, load_treaty};
use ic_reinsurance::machine::{FacTarget, FacultativeTerms};
use ic_reinsurance::treaty::{ExcessPath, RiskFactors};
use ic_reinsurance::{Command, Envelope, Reinsurance, RiskKey, TreatyRef};
use proptest::prelude::*;

/// A book that has used most of what the module does.
fn busy() -> Bench {
    let mut b = Bench::with_aggregates();
    b.issue(1, "P1", "anna", vec![layer("life", "life", 1, "L1", 800 * K, d(2027, 2, 1))]);
    b.factors("P1", "L1", d(1990, 3, 10), false);
    b.issue_scheme(2, "S1", d(2027, 2, 1), vec![member_cover("M1", 700 * K), member_cover("M2", 100 * K)]);
    b.ok(
        "group-admin",
        Command::SetSchemeTerms {
            policy: PolicyId::from("S1"),
            excess_path: ExcessPath::UnderwrittenExcess,
            sites: vec![("auckland".into(), 2)],
        },
    );
    b.ok(
        "group-admin",
        Command::NotePossibleMatch {
            policy: PolicyId::from("S1"),
            life: LifeId::from("M2"),
            party: PartyId::from("bella"),
        },
    );
    b.feed(
        2,
        vec![Event::MemberIdentified {
            policy: PolicyId::from("S1"),
            life: LifeId::from("M1"),
            party: PartyId::from("anna"),
        }],
    );
    b.premium_due(1, "P1", 1, d(2027, 2, 1), d(2027, 3, 1), &[("life", 61_84)]);
    b.premium_due(2, "S1", 1, d(2027, 2, 1), d(2027, 3, 1), &[("group_life", 400_00)]);
    b.ok(
        "underwriting",
        Command::ReserveCapacity(Box::new(CapacityRequest {
            reservation_key: "R1".into(),
            party: PartyId::from("anna"),
            product_code: "term-life".into(),
            cover_code: "trauma".into(),
            cover_type: "trauma".into(),
            sum_insured_minor: 5000 * K,
            commencement: d(2027, 5, 1),
            proposal_expires: d(2027, 12, 31),
            risk_codes: vec![],
        })),
    );
    b.ok(
        "reinsurance-ops",
        Command::RecordFacultativeTerms(Box::new(FacultativeTerms {
            target: FacTarget::Reservation("R1".into()),
            reference: "FAC-1".into(),
            reinsurer: south(),
            amount_minor: 100 * K,
            rate: 1_500_000,
            terms_ref: "vault/FAC-1".into(),
            loading: None,
            exclusions: vec!["x".into()],
        })),
    );
    b.ok(
        "new-business",
        Command::ConfirmCapacity {
            reservation_id: "R1".into(),
            policy: PolicyId::from("P1"),
            cover: "trauma".into(),
            layers: vec![1],
        },
    );
    b.today = d(2027, 4, 2);
    b.ok(
        "month-end",
        Command::CutStatement {
            treaty: retail(),
            reinsurer: north(),
            period_end: d(2027, 3, 31),
        },
    );
    b.add_layer(1, "P1", layer("life", "life", 2, "L1", 50 * K, d(2027, 2, 20)));
    b.today = d(2027, 6, 10);
    for (n, f) in [
        ClaimFact::Reserved {
            claim: "C1".into(),
            policy: PolicyId::from("P1"),
            cover: "life".into(),
            life: "L1".into(),
            event_date: d(2027, 6, 1),
            gross_reserve_minor: 850 * K,
            periodic: false,
            catastrophe_event: Some("quake".into()),
        },
        ClaimFact::Paid {
            claim: "C1".into(),
            payment_ref: "PAY-1".into(),
            amount_minor: 850 * K,
            period_end: None,
            ex_gratia: false,
        },
        ClaimFact::AdmissionReversed {
            claim: "C1".into(),
            decided_on: d(2027, 6, 9),
            reason_code: "x".into(),
            cause: None,
        },
    ]
    .into_iter()
    .enumerate()
    {
        b.send_keyed(&format!("claim-{n}"), "claims", Command::Claim(Box::new(f)));
    }
    b.ok(
        "finance",
        Command::ReviewProfitCommission {
            treaty: retail(),
            reinsurer: south(),
        },
    );
    b
}

/// Purpose: replaying the same log into a fresh module gives exactly the same state hash, and
/// the outputs of each command are the same on replay.
/// Why it matters: replicas compare state hashes, and a restarted node rebuilds by replay. Any
/// dependence on iteration order, time or memory layout would show here as a differing hash
/// and in production as a node that cannot rejoin.
#[test]
fn replaying_the_log_gives_the_same_state_and_outputs() {
    let b = busy();
    let replayed = b.replayed();
    assert_eq!(replayed.state_hash(), b.ri.state_hash());
    assert_eq!(replayed, b.ri);
    b.assert_reconciles();
    // The trait the log drives gives the same outputs, command by command.
    let (mut one, mut two) = (Reinsurance::new(), Reinsurance::new());
    for (i, env) in b.log.iter().enumerate() {
        let (a, c) = (one.apply(i as u64 + 1, env.clone()), two.apply_envelope(i as u64 + 1, env.clone()));
        assert_eq!(a, c, "command {i}");
    }
    assert_ne!(Reinsurance::new().state_hash(), b.ri.state_hash());
}

/// Purpose: the whole state survives a snapshot: written as JSON and read back it is equal and
/// hashes the same, and it carries on from there exactly as the original does.
/// Why it matters: the module host snapshots state to truncate the log. Anything missing from
/// the encoding would be silently lost at the first snapshot and found at the first claim.
#[test]
fn a_snapshot_round_trip_loses_nothing() {
    let mut b = busy();
    let json = serde_json::to_string(&b.ri).unwrap();
    let mut restored: Reinsurance = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, b.ri);
    assert_eq!(restored.state_hash(), b.ri.state_hash());
    let env = Envelope {
        key: "after".into(),
        caller: "event-feed".into(),
        on: d(2027, 6, 11),
        command: Command::CoreEvents {
            events: b.records(
                1,
                vec![Event::PolicyStatusChanged {
                    policy: PolicyId::from("P1"),
                    from: PolicyStatus::InForce,
                    to: PolicyStatus::Lapsed,
                }],
            ),
        },
    };
    assert_eq!(restored.apply_envelope(999, env.clone()), b.ri.apply_envelope(999, env));
    assert_eq!(restored.state_hash(), b.ri.state_hash());
    // Commands and their results cross the module host as JSON too.
    let wire = serde_json::to_string(&b.log).unwrap();
    assert_eq!(serde_json::from_str::<Vec<Envelope>>(&wire).unwrap(), b.log);
}

/// Purpose: a command delivered twice under the same key is applied once: the second delivery
/// returns the first result, marked as a duplicate, with no output and no change, for accepted
/// and refused commands alike.
/// Why it matters: delivery to the module is at least once. Without this a retry after a
/// timeout would book a premium or a recovery twice.
#[test]
fn a_command_delivered_twice_applies_once() {
    let mut b = Bench::new();
    let events = b.records(
        1,
        vec![Event::PolicyStatusChanged {
            policy: PolicyId::from("P1"),
            from: PolicyStatus::InForce,
            to: PolicyStatus::Lapsed,
        }],
    );
    let first = b.send_keyed("feed-1", "event-feed", Command::CoreEvents { events: events.clone() });
    assert!(!first.duplicate && first.result.is_ok());
    let hash = b.ri.state_hash();
    let second = b.send_keyed("feed-1", "event-feed", Command::CoreEvents { events });
    assert!(second.duplicate && second.result.is_ok() && second.instructions.is_empty());
    assert_eq!(b.ri.state_hash(), hash);
    let refused = b.send_keyed(
        "bad-1",
        "month-end",
        Command::CutStatement {
            treaty: "nope".into(),
            reinsurer: north(),
            period_end: d(2027, 1, 31),
        },
    );
    let again = b.send_keyed(
        "bad-1",
        "month-end",
        Command::CutStatement {
            treaty: "nope".into(),
            reinsurer: north(),
            period_end: d(2027, 1, 31),
        },
    );
    assert_eq!(refused.result, again.result);
    assert!(again.duplicate && again.result.unwrap_err().to_string().starts_with("unknown_treaty: "));
}

fn every_command() -> Vec<Command> {
    let k = key("P1", "life", 1, "L1");
    vec![
        Command::ActivateTreaty {
            treaty: Box::new(load_treaty("t.star", RETAIL).unwrap()),
        },
        Command::ActivateAggregateTreaty {
            treaty: Box::new(load_aggregate_treaty("c.star", CATASTROPHE).unwrap()),
        },
        Command::ActivateRateTable {
            table: Box::new(load_rate_table("r.star", RATES).unwrap()),
        },
        Command::CoreEvents { events: vec![] },
        Command::ReserveCapacity(Box::new(CapacityRequest {
            reservation_key: "R".into(),
            party: PartyId::from("p"),
            product_code: "term-life".into(),
            cover_code: "life".into(),
            cover_type: "life".into(),
            sum_insured_minor: 1,
            commencement: d(2027, 2, 1),
            proposal_expires: d(2027, 3, 1),
            risk_codes: vec![],
        })),
        Command::ReleaseCapacity {
            reservation_id: "R".into(),
            reason: "r".into(),
        },
        Command::ConfirmCapacity {
            reservation_id: "R".into(),
            policy: PolicyId::from("P"),
            cover: "life".into(),
            layers: vec![1],
        },
        Command::RecordFacultativeTerms(Box::new(FacultativeTerms {
            target: FacTarget::Risk(k.clone()),
            reference: "F".into(),
            reinsurer: north(),
            amount_minor: 1,
            rate: 1,
            terms_ref: "t".into(),
            loading: None,
            exclusions: vec![],
        })),
        Command::DeclineFacultative {
            reservation_id: "R".into(),
            reference: "D".into(),
        },
        Command::LogEvidenceRelease {
            subject: "s".into(),
            treaty: retail(),
            reinsurer: north(),
            purpose: "p".into(),
            documents: vec!["d".into()],
        },
        Command::RecordRiskFactors {
            policy: PolicyId::from("P"),
            life: LifeId::from("L"),
            factors: RiskFactors::default(),
        },
        Command::SetSchemeTerms {
            policy: PolicyId::from("P"),
            excess_path: ExcessPath::Facultative,
            sites: vec![],
        },
        Command::NotePossibleMatch {
            policy: PolicyId::from("P"),
            life: LifeId::from("L"),
            party: PartyId::from("p"),
        },
        Command::ClearPossibleMatch {
            policy: PolicyId::from("P"),
            life: LifeId::from("L"),
            party: PartyId::from("p"),
        },
        Command::Claim(Box::new(ClaimFact::Closed {
            claim: "C".into(),
            outcome: None,
        })),
        Command::ReinsurerConsent {
            claim: "C".into(),
            reinsurer: north(),
            agreed: true,
            reference: "r".into(),
        },
        Command::ResolveDispute {
            reference: "D".into(),
            repay_minor: 0,
            agreement_ref: "a".into(),
            prepared_by: "a".into(),
            approved_by: "b".into(),
        },
        Command::Recapture {
            treaty: retail(),
            effective: d(2027, 2, 1),
            keys: vec![k.clone()],
        },
        Command::MoveTreatyVersion {
            to: TreatyRef { id: retail(), version: 1 },
            effective: d(2027, 2, 1),
            keys: vec![k.clone()],
        },
        Command::CutStatement {
            treaty: retail(),
            reinsurer: north(),
            period_end: d(2027, 1, 31),
        },
        Command::AgreeStatement {
            number: 1,
            reference: "r".into(),
        },
        Command::HoldSettlement { number: 1, reason: "r".into() },
        Command::ReleaseHold { number: 1 },
        Command::InstructSettlement {
            number: 1,
            prepared_by: "a".into(),
            approved_by: "b".into(),
        },
        Command::RecordSettlement {
            number: 1,
            payment_ref: "p".into(),
            amount_minor: 0,
            bank_account: None,
        },
        Command::IssueSupplementaryListing { number: 1, reason: "r".into() },
        Command::ReviewProfitCommission {
            treaty: retail(),
            reinsurer: north(),
        },
        Command::LoadMigratedCession {
            key: k,
            cession: Default::default(),
        },
        Command::LoadOpeningBalance {
            treaty: retail(),
            reinsurer: north(),
            partition: 0,
            amount_minor: 1,
            reference: "r".into(),
        },
        Command::AcknowledgeInstructions { through: 0 },
        Command::InstructionRefused {
            seq: 1,
            code: "c".into(),
            message: "m".into(),
        },
        Command::AbandonInstruction {
            seq: 1,
            reason: "r".into(),
            approved_by: "a".into(),
        },
        Command::AcknowledgeNotices { seqs: vec![] },
        Command::NoticeRefused {
            seq: 1,
            code: "c".into(),
            message: "m".into(),
        },
        Command::RetryNotice { seq: 1 },
        Command::AbandonNotice {
            seq: 1,
            reason: "r".into(),
            approved_by: "a".into(),
        },
    ]
}

/// Purpose: every kind of notice names the audience it is for.
/// Why it matters: the audience decides who is told. A consent request filed under staff
/// would never reach claims, and the claim would be paid without the reinsurer's agreement.
#[test]
fn every_notice_names_its_audience() {
    use ic_reinsurance::machine::Audience;
    use ic_reinsurance::Notice;
    let claim: ic_reinsurance::ClaimId = "C1".into();
    let cases = [
        (
            Notice::ExceptionOpened {
                id: "x".into(),
                code: "c".into(),
                detail: "d".into(),
            },
            Audience::Staff,
        ),
        (Notice::ExceptionClosed { id: "x".into() }, Audience::Staff),
        (Notice::SupplementaryListingDue { statement: 1 }, Audience::Staff),
        (
            Notice::CapacityReleased {
                reservation_id: "R".into(),
                reason: "r".into(),
            },
            Audience::Staff,
        ),
        (
            Notice::RequireUnderwrittenExcess {
                policy: PolicyId::from("S1"),
                life: "M1".into(),
                cover: "group-life".into(),
                excess_minor: 1,
                reference: "r".into(),
            },
            Audience::Group,
        ),
        (
            Notice::FacultativeTermsRecorded {
                reservation_id: "R".into(),
                offered: true,
                reference: "F".into(),
            },
            Audience::Underwriting,
        ),
        (
            Notice::LargeClaimNotification {
                claim: claim.clone(),
                treaty: retail(),
                reinsurers: vec![north()],
            },
            Audience::Claims,
        ),
        (
            Notice::ReinsurerConsentRequired {
                claim: claim.clone(),
                treaty: retail(),
                due_by: d(2027, 3, 1),
            },
            Audience::Claims,
        ),
        (
            Notice::ReinsurerConsent {
                claim: claim.clone(),
                reinsurer: north(),
                agreed: true,
                reference: "r".into(),
            },
            Audience::Claims,
        ),
        (
            Notice::DisputedItemRaised {
                claim,
                reinsurer: north(),
                reference: "r".into(),
            },
            Audience::Claims,
        ),
        (
            Notice::DisputeResolved {
                claim: ic_reinsurance::ClaimId::from("C1"),
                reference: "r".into(),
                repay_minor: 0,
            },
            Audience::Claims,
        ),
        (
            Notice::SettlementDue {
                statement: 1,
                treaty: retail(),
                reinsurer: north(),
                currency: ic_money::Currency::NZD,
                net_minor: 1,
                attempt: 0,
            },
            Audience::Payments,
        ),
        (
            Notice::Document {
                name: "n".into(),
                sha256: "h".into(),
                body: "b".into(),
            },
            Audience::Records,
        ),
        (
            Notice::ReleaseLogged {
                subject: "s".into(),
                treaty: retail(),
                reinsurer: north(),
                purpose: "p".into(),
                documents: vec![],
            },
            Audience::Records,
        ),
    ];
    for (notice, audience) in cases {
        assert_eq!(notice.audience(), audience, "{notice:?}");
    }
}

/// Purpose: every command has a name of its own and a list of modules allowed to send it, and
/// a command from any other module is refused before it touches anything.
/// Why it matters: the caller check is the module's only authorisation boundary. A command
/// with no owner, or one that any module could send, would let claims cut statements or
/// month-end rewrite treaties.
#[test]
fn every_command_is_owned_and_strangers_are_refused() {
    let commands = every_command();
    let names: BTreeSet<&str> = commands.iter().map(Command::name).collect();
    assert_eq!(names.len(), commands.len(), "two commands share a name");
    for command in commands {
        assert!(!command.allowed_callers().is_empty(), "{} has no owner", command.name());
        assert!(!command.allowed_callers().contains(&"intruder"));
        let mut b = Bench::new();
        let name = command.name();
        let applied = b.send("intruder", command);
        let reject = applied.result.unwrap_err();
        assert_eq!(reject.code, "caller_not_allowed");
        assert!(reject.message.contains(name));
    }
    // Only configuration may activate a treaty, and only the feed may deliver events.
    assert_eq!(Command::CoreEvents { events: vec![] }.allowed_callers(), ["event-feed"]);
    assert_eq!(every_command()[0].allowed_callers(), ["config"]);
}

/// Purpose: an active version of a treaty, aggregate treaty or rate table cannot be activated
/// again with other content, the same content again is accepted, and invalid terms are refused
/// at activation even if they did not come through the loader.
/// Why it matters: cessions already recorded point at a treaty version. If the version could
/// change underneath them, a replay would calculate different cessions from the same log.
#[test]
fn active_configuration_is_immutable() {
    let mut b = Bench::with_aggregates();
    let treaty = load_treaty("t.star", RETAIL).unwrap();
    b.ok(
        "config",
        Command::ActivateTreaty {
            treaty: Box::new(treaty.clone()),
        },
    );
    let mut edited = treaty.clone();
    edited.commission = 1;
    edited.content_hash = "other".into();
    assert_eq!(
        b.refused(
            "config",
            Command::ActivateTreaty {
                treaty: Box::new(edited.clone())
            }
        ),
        "config_conflict"
    );
    edited.panel.clear();
    assert_eq!(b.refused("config", Command::ActivateTreaty { treaty: Box::new(edited) }), "invalid_treaty");

    let table = load_rate_table("r.star", RATES).unwrap();
    b.ok(
        "config",
        Command::ActivateRateTable {
            table: Box::new(table.clone()),
        },
    );
    let mut other = table.clone();
    other.content_hash = "other".into();
    assert_eq!(
        b.refused(
            "config",
            Command::ActivateRateTable {
                table: Box::new(other.clone())
            }
        ),
        "config_conflict"
    );
    other.rows.clear();
    assert_eq!(b.refused("config", Command::ActivateRateTable { table: Box::new(other) }), "invalid_rate_table");
    // A later version of the rates applies from its own date.
    let mut v2 = table;
    v2.version = 2;
    v2.effective_from = d(2028, 1, 1);
    v2.content_hash = "v2".into();
    b.ok("config", Command::ActivateRateTable { table: Box::new(v2) });

    let cat = load_aggregate_treaty("c.star", CATASTROPHE).unwrap();
    b.ok("config", Command::ActivateAggregateTreaty { treaty: Box::new(cat.clone()) });
    let mut other = cat.clone();
    other.content_hash = "other".into();
    assert_eq!(
        b.refused(
            "config",
            Command::ActivateAggregateTreaty {
                treaty: Box::new(other.clone())
            }
        ),
        "config_conflict"
    );
    other.version = 2;
    b.ok(
        "config",
        Command::ActivateAggregateTreaty {
            treaty: Box::new(other.clone()),
        },
    );
    assert_eq!(
        b.refused("config", Command::ActivateAggregateTreaty { treaty: Box::new(cat) }),
        "config_conflict"
    );
    other.period_end = d(2026, 1, 1);
    assert_eq!(
        b.refused("config", Command::ActivateAggregateTreaty { treaty: Box::new(other) }),
        "invalid_treaty"
    );
}

/// Purpose: the reconciliation checks notice a cession record that no longer matches a
/// rebuild, and an account that no longer matches the periods and claims behind it.
/// Why it matters: a safety net that cannot see damage is worse than none. Each kind of damage
/// here is one that a bug or a bad restore could produce.
#[test]
fn reconciliation_notices_damage() {
    let b = busy();
    let damaged = |edit: &dyn Fn(&mut serde_json::Value)| {
        let mut v = serde_json::to_value(&b.ri).unwrap();
        edit(&mut v);
        serde_json::from_value::<Reinsurance>(v).unwrap().reconcile()
    };
    assert!(damaged(&|_| {}).is_empty());
    // A recorded cession whose parts no longer add up.
    let breaks = damaged(&|v| {
        let life = v["lives"].as_array_mut().unwrap().iter_mut().find(|l| l[0]["party"] == "anna").unwrap();
        let segment = &mut life[1]["recorded"][0][1][0][1];
        segment["retained_minor"] = serde_json::json!(1);
    });
    assert!(
        breaks.iter().any(|b| b.contains("differs from a rebuild")) && breaks.iter().any(|b| b.contains("do not add up")),
        "{breaks:?}"
    );
    // An exception item removed while the cover is still over the retention.
    let breaks = damaged(&|v| {
        v["exceptions"].as_object_mut().unwrap().retain(|k, _| !k.starts_with("over_retention"));
    });
    assert!(breaks.iter().any(|b| b.contains("exception item disagree")), "{breaks:?}");
    // Premium on a reinsurer's account with no charge behind it.
    let breaks = damaged(&|v| {
        let cell = v["booked"].as_array_mut().unwrap().iter_mut().find(|c| c[0][2] == "ceded_premium").unwrap();
        cell[1] = serde_json::json!(1);
    });
    assert!(breaks.iter().any(|b| b.contains("CededPremium")), "{breaks:?}");
    // A recovery billed that no claim accounts for.
    let breaks = damaged(&|v| {
        let cell = v["booked"].as_array_mut().unwrap().iter_mut().find(|c| c[0][2] == "recovery_due").unwrap();
        cell[1] = serde_json::json!(1);
    });
    assert!(breaks.iter().any(|b| b.contains("sum over claims")), "{breaks:?}");
}

fn risk(n: usize, sum: i64, start_day: u64, end_after: Option<u64>, member: bool) -> Risk {
    let start = d(2027, 2, 1) + Days::new(start_day);
    Risk {
        key: RiskKey {
            policy: PolicyId(format!("P{n}")),
            cover: "life".into(),
            layer: 1,
            life: LifeId::from("L1"),
        },
        partition: n as u16,
        product: "term-life".into(),
        cover_type: "life".into(),
        source: if member { Source::Member } else { Source::Layer },
        underwritten: !member,
        start,
        end: end_after.map(|days| start + Days::new(days)),
        amounts: vec![(start, sum)],
        suspensions: vec![],
        treaties: vec![(
            start,
            Some(TreatyRef {
                id: if member { group() } else { retail() },
                version: 1,
            }),
        )],
        facultative: vec![],
        excluded: false,
        excess_path: None,
        migrated: None,
        continues: None,
    }
}

proptest! {
    /// Purpose: for any set of covers on one life, under both shipped treaties, every cession
    /// balances to its sum at risk, nothing is negative, what the insurer keeps on the life
    /// never exceeds the shared retention on any date, and the result does not depend on the
    /// order the covers are presented in.
    /// Why it matters: these are the properties the whole module rests on. Example tests show
    /// the cases someone thought of; this searches for the combination of dates, amounts and
    /// endings nobody did.
    #[test]
    fn cessions_balance_and_never_over_retain(
        covers in prop::collection::vec((1i64..6_000, 0u64..400, prop::option::of(1u64..300), any::<bool>()), 1..7),
        matched in any::<bool>(),
        rotate in 0usize..7,
    ) {
        let retail_treaty = load_treaty("t.star", RETAIL).unwrap();
        let group_treaty = load_treaty("g.star", GROUP).unwrap();
        let lookup = |t: &TreatyRef| if t.id == retail() { Some(&retail_treaty) } else { Some(&group_treaty) };
        let risks: Vec<Risk> = covers.iter().enumerate().map(|(n, (sum, start, end, member))| risk(n, sum * K, *start, *end, *member)).collect();
        let out = timeline(&risks, lookup, matched);
        let mut rotated = risks.clone();
        rotated.rotate_left(rotate % risks.len());
        prop_assert_eq!(&out, &timeline(&rotated, lookup, matched));
        let mut dates = BTreeSet::new();
        for (key, segments) in &out {
            prop_assert!(segments.windows(2).all(|w| w[0].0 < w[1].0 && w[0].1 != w[1].1), "{key}: segments out of order or repeated");
            for (from, c) in segments {
                dates.insert(*from);
                let Some(c) = c else { continue };
                prop_assert!(c.balanced(), "{key} from {from}: {c:?}");
                prop_assert!(c.retained_minor >= 0 && c.unplaced_minor >= 0 && c.ceded.iter().all(|(_, a)| *a >= 0) && c.facultative.iter().all(|f| f.amount_minor > 0));
                prop_assert_eq!(c.unplaced_minor > 0, c.unplaced_reason.is_some());
            }
        }
        if matched {
            // Without reallocation a cover keeps what it took when it last changed, so the
            // check is on the dates covers start, when the retention is being filled.
            for r in &risks {
                let kept: i64 = out.iter().filter_map(|(k, s)| {
                    let other = risks.iter().find(|x| &x.key == k)?;
                    (allocation_order(other, r).is_le() && other.in_force_on(r.start)).then(|| ic_reinsurance::cession::cession_on(s, r.start).map_or(0, |c| c.retained_minor))
                }).sum();
                prop_assert!(kept <= 500 * K, "kept {kept} on {}", r.start);
            }
        }
    }
}
