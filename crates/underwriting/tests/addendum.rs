//! The design addendum of 11 November 2026 as it lands on underwriting:
//! entry 2 (the existing-cover question filled from the life index) and
//! entry 4 (an adviser's licence is suspended while proposals are in flight).

mod common;

use std::collections::BTreeMap;

use common::*;
use ic_core::{LifeId, PolicyId};
use ic_underwriting::command::{Command, Effect, Event, LicenceStatus, NoticeKind, View};
use ic_underwriting::evidence::{EvidenceKind, EvidenceStatus};
use ic_underwriting::model::{AccessEnd, ConsentKind, ExistingCover, ProposalStatus, Stage};

fn scheme_cover() -> Vec<ExistingCover> {
    vec![ExistingCover {
        policy: PolicyId::from("GRP-7"),
        partition: 12,
        cover_type: "life".into(),
        sum_insured_minor: 300_000_00,
    }]
}

fn prefill(matched: bool, for_applicant: bool, rows: Vec<ExistingCover>) -> Command {
    Command::PrefillExistingCover {
        proposal: "P1".into(),
        life: "L1".into(),
        matched,
        for_applicant,
        rows,
        reflects: BTreeMap::from([(12, 4410)]),
    }
}

/// Purpose: addendum entry 2. A person who is a member of a group scheme applies for retail
/// cover on another partition. The existing-cover question is filled from the life index only
/// where the person is matched in the customer master and has consented; the underwriter's own
/// read needs the match but not the consent; the read records the partitions and sequences it
/// reflects; and it is taken before the disclosure, not after.
/// Why it matters: showing an unmatched person someone else's cover is a privacy breach, and an
/// underwriter blind to scheme cover would over-insure the life. The recorded sequences are
/// what lets a later reviewer see exactly what the decision knew.
#[test]
fn existing_cover_is_prefilled_only_for_a_matched_person_who_consents() {
    let mut w = World::new();
    w.open("P1", 1_300_000_00);
    assert_eq!(w.err("new-business", prefill(true, true, scheme_cover())), "consent_required");
    assert_eq!(w.err("new-business", prefill(false, false, scheme_cover())), "not_matched");
    let mut negative = scheme_cover();
    negative[0].sum_insured_minor = -1;
    assert_eq!(w.err("new-business", prefill(true, false, negative)), "rows");

    // The underwriter's read: no consent needed, and the applicant is shown nothing.
    let read = w.ok("new-business", prefill(true, false, scheme_cover()));
    assert!(read.events.is_empty());
    let seen = w.p("P1").lives[&LifeId::from("L1")].existing_cover.clone().unwrap();
    assert_eq!(
        (seen.prefilled, seen.total("life"), seen.total("trauma"), seen.reflects[&12], seen.read_at),
        (false, 300_000_00, 0, 4410, w.now)
    );

    // With consent the question is filled in for the applicant, and that fact is kept.
    w.consent("P1", "L1", ConsentKind::ExistingCoverPrefill);
    assert_eq!(w.err("new-business", prefill(false, true, scheme_cover())), "not_matched");
    let filled = w.ok("new-business", prefill(true, true, scheme_cover()));
    assert!(matches!(&filled.events[0], Event::ExistingCoverPrefilled { .. }));
    w.ok("new-business", prefill(true, false, scheme_cover()));
    assert!(
        w.p("P1").lives[&LifeId::from("L1")].existing_cover.as_ref().unwrap().prefilled,
        "a later read does not forget the applicant was shown it"
    );

    // The scheme amount counts in the rules: 1.3m alone asks for nothing, 1.6m in all needs financial evidence.
    w.consent("P1", "L1", ConsentKind::ObtainRecords);
    let disclosed = w.disclose("P1", "L1", &clean());
    assert!(matches!(
        &disclosed.effects[0],
        Effect::RequestEvidence {
            kind: EvidenceKind::FinancialStatement,
            ..
        }
    ));
    assert_eq!(w.err("new-business", prefill(true, false, scheme_cover())), "too_late");
    // Consent can be withdrawn; what was read stays as the record of what was known.
    w.ok(
        "new-business",
        Command::RecordConsent {
            proposal: "P1".into(),
            life: "L1".into(),
            kind: ConsentKind::ExistingCoverPrefill,
            granted: false,
            signed: None,
        },
    );
    assert!(!w.p("P1").lives[&LifeId::from("L1")].consented(ConsentKind::ExistingCoverPrefill));
    assert_eq!(
        w.err(
            "new-business",
            Command::RecordConsent {
                proposal: "P1".into(),
                life: "L9".into(),
                kind: ConsentKind::ExistingCoverPrefill,
                granted: false,
                signed: None
            }
        ),
        "unknown_life"
    );
}

fn lookup(w: &mut World, adviser: &str) -> Result<ic_underwriting::command::Applied, ic_underwriting::command::Reject> {
    w.send(
        "adviser-portal",
        Command::AdviserLookup {
            proposal: "P1".into(),
            adviser: adviser.into(),
            purpose: "help client complete application".into(),
        },
    )
}

fn adviser_proposal(w: &mut World) {
    let open = w.open_command(
        "P1",
        &["L1", "L2"],
        &[("L1", "life", "life", 500_000_00), ("L2", "life", "life", 500_000_00)],
        true,
    );
    w.ok("new-business", Command::OpenProposal(Box::new(open)));
}

/// Purpose: what an adviser can see. Consent is per life, per application, given with a signed
/// step. With it the adviser sees where that life's answers are and how evidence is coming
/// along; without it only the stage. Never reasons, notes or reports. Access ends when the
/// decision is made or consent is withdrawn, and does not come back once the policy is issued or
/// when a claim later opens the file; answers the adviser typed are allowed only under
/// consent; every look is logged with its purpose; another adviser sees nothing.
/// Why it matters: one partner's health answers must not reach the couple's adviser, or the
/// other partner through them, without that person's own consent.
#[test]
fn an_adviser_sees_a_lifes_answers_only_with_that_lifes_consent() {
    let mut w = World::new();
    adviser_proposal(&mut w);
    let consent = |signed| Command::RecordConsent {
        proposal: "P1".into(),
        life: "L1".into(),
        kind: ConsentKind::AdviserAccess,
        granted: true,
        signed,
    };
    assert_eq!(w.err("new-business", consent(None)), "signature");
    assert_eq!(
        w.err("new-business", consent(Some(signed("consent", &["P1", "L2", "adviser_access"])))),
        "signature"
    );
    w.consent("P1", "L1", ConsentKind::AdviserAccess);
    w.consent("P1", "L1", ConsentKind::ObtainRecords);

    // The adviser typed two answers for L1; for L2, who has not consented, that is refused.
    let mut d = w.disclosure("P1", "L1", &yes("diabetes"));
    d.entered_by_adviser = 2;
    w.ok("new-business", Command::SubmitDisclosure(Box::new(d)));
    let mut d2 = w.disclosure("P1", "L2", &clean());
    d2.entered_by_adviser = 1;
    assert_eq!(w.err("new-business", Command::SubmitDisclosure(Box::new(d2))), "adviser_not_permitted");
    let disclosed = w.disclose("P1", "L2", &clean());

    assert_eq!(lookup(&mut w, "adv-2").unwrap_err().code, "not_your_client");
    assert_eq!(
        w.err(
            "adviser-portal",
            Command::AdviserLookup {
                proposal: "P1".into(),
                adviser: "adv-1".into(),
                purpose: "".into()
            }
        ),
        "purpose"
    );
    let seen = lookup(&mut w, "adv-1").unwrap();
    let Some(View::Adviser { status, lives }) = &seen.view else {
        panic!("no view")
    };
    assert_eq!(status, "open");
    assert_eq!(
        (lives[0].stage.as_str(), lives[0].answers_ref.as_deref()),
        ("awaiting_evidence", Some("vault://P1/L1/disclosure"))
    );
    assert_eq!(lives[0].evidence, vec![(EvidenceKind::BloodTest, EvidenceStatus::Requested)]);
    assert_eq!(
        (lives[1].stage.as_str(), lives[1].answers_ref.clone(), lives[1].evidence.len()),
        ("awaiting_capacity", None, 0)
    );
    assert!(
        matches!(&seen.events[0], Event::FileAccessed { who, role, purpose, documents, .. } if who == "adv-1" && role == "adviser" && purpose.starts_with("help") && documents.len() == 1)
    );
    let text = serde_json::to_string(&seen).unwrap();
    assert!(!text.contains("RC-") && !text.contains("notes"), "no reasons and no notes");

    // L2 is decided: the outcome shows, by cover, and nothing more.
    w.automatic(&disclosed);
    let seen = lookup(&mut w, "adv-1").unwrap();
    let Some(View::Adviser { lives, .. }) = &seen.view else { panic!("no view") };
    assert_eq!(lives[1].outcome, Some(vec![("life".to_string(), "standard".to_string())]));
    assert_eq!(lives[0].outcome, None);

    // L1 withdraws consent: access ends at once, and comes back only with a new signed consent.
    let withdrawn = w.ok(
        "new-business",
        Command::RecordConsent {
            proposal: "P1".into(),
            life: "L1".into(),
            kind: ConsentKind::AdviserAccess,
            granted: false,
            signed: None,
        },
    );
    assert!(matches!(&withdrawn.events[0], Event::AdviserAccessEnded { why: AccessEnd::ConsentWithdrawn, adviser, .. } if adviser == "adv-1"));
    let Some(View::Adviser { lives, .. }) = lookup(&mut w, "adv-1").unwrap().view else {
        panic!("no view")
    };
    assert_eq!((lives[0].answers_ref.clone(), lives[0].evidence.len()), (None, 0));
    w.consent("P1", "L1", ConsentKind::AdviserAccess);
    let Some(View::Adviser { lives, .. }) = lookup(&mut w, "adv-1").unwrap().view else {
        panic!("no view")
    };
    assert!(lives[0].answers_ref.is_some());

    // The decision is made: the adviser's sight of the answers ends for good.
    w.ok(
        "evidence",
        Command::EvidenceReceived {
            request: "P1/L1/blood_test".into(),
            vault_ref: "v".into(),
            cost_minor: 0,
        },
    );
    let case = w.take("P1", "L1", "amy");
    let decided = w.propose(&case, "amy", vec![standard("life")]).unwrap();
    let effective = w.automatic(&decided);
    assert!(effective.events.iter().any(|e| matches!(
        e,
        Event::AdviserAccessEnded {
            why: AccessEnd::DecisionMade,
            ..
        }
    )));
    assert_eq!(
        w.err(
            "new-business",
            Command::RecordConsent {
                proposal: "P1".into(),
                life: "L1".into(),
                kind: ConsentKind::AdviserAccess,
                granted: true,
                signed: Some(signed("consent", &["P1", "L1", "adviser_access"]))
            }
        ),
        "closed"
    );
    let Some(View::Adviser { status, lives }) = lookup(&mut w, "adv-1").unwrap().view else {
        panic!("no view")
    };
    assert_eq!((status.as_str(), lives[0].answers_ref.clone()), ("issuing", None));

    // Issued, and later a claim: the claims assessor opens the file, and the
    // adviser's sight of the answers still does not come back.
    let policy = w.p("P1").policy.clone();
    w.ok(
        "issuer",
        Command::PolicyIssued {
            proposal: "P1".into(),
            layers: vec![],
        },
    );
    w.ok(
        "claims",
        Command::OpenFileForClaim {
            claim_ref: "CLM-1".into(),
            policy,
            life: "L1".into(),
            assessor: "claims-assessor-1".into(),
        },
    );
    let Some(View::Adviser { status, lives }) = lookup(&mut w, "adv-1").unwrap().view else {
        panic!("no view")
    };
    assert_eq!((status.as_str(), lives[0].answers_ref.clone(), lives[0].evidence.len()), ("issued", None, 0));

    // No adviser on the proposal: nothing to consent to.
    w.open("P2", 100_000_00);
    assert_eq!(
        w.err(
            "new-business",
            Command::RecordConsent {
                proposal: "P2".into(),
                life: "L1".into(),
                kind: ConsentKind::AdviserAccess,
                granted: true,
                signed: None
            }
        ),
        "no_adviser"
    );
    assert_eq!(
        w.err(
            "adviser-portal",
            Command::AdviserLookup {
                proposal: "P2".into(),
                adviser: "adv-1".into(),
                purpose: "x".into()
            }
        ),
        "not_your_client"
    );
}

/// Purpose: addendum entry 4. An adviser's licence, or their provider's, is suspended while
/// proposals are in flight. From that moment the adviser sees nothing and can start nothing;
/// each customer is told and may appoint another adviser; the proposals carry on to a decision
/// served directly; and reinstating the licence does not restore sight of any file without a
/// new consent.
/// Why it matters: an unlicensed person reading health answers or submitting business is a
/// regulatory breach, but the customer did nothing wrong and must not lose their place or their
/// interim cover because of it.
#[test]
fn a_suspended_adviser_loses_access_at_once_and_the_proposals_carry_on() {
    let mut w = World::new();
    adviser_proposal(&mut w);
    w.consent("P1", "L1", ConsentKind::AdviserAccess);
    w.disclose("P1", "L1", &clean());
    let change = |subject: &str, status, effective, source: &str| Command::LicenceChanged {
        subject: subject.into(),
        status,
        effective,
        source_ref: source.into(),
    };
    assert_eq!(
        w.err("adviser-licensing", change("adv-1", LicenceStatus::Suspended, d(2026, 11, 12), "FSPR-1")),
        "not_yet_effective"
    );
    assert_eq!(
        w.err("adviser-licensing", change("adv-1", LicenceStatus::Suspended, d(2026, 11, 11), "")),
        "source"
    );
    assert_eq!(
        w.err("adviser-licensing", change("", LicenceStatus::Suspended, d(2026, 11, 11), "FSPR-1")),
        "source"
    );

    let suspended = w.ok("adviser-licensing", change("adv-1", LicenceStatus::Suspended, d(2026, 11, 11), "FSPR-1"));
    assert!(matches!(
        &suspended.events[0],
        Event::LicenceRecorded {
            status: LicenceStatus::Suspended,
            ..
        }
    ));
    assert!(matches!(&suspended.events[1], Event::AdviserAccessEnded { why: AccessEnd::LicenceSuspended, life, .. } if life == &LifeId::from("L1")));
    assert_eq!(suspended.events.len(), 2, "L2 never consented, so had no access to end");
    assert!(matches!(
        &suspended.effects[0],
        Effect::Notify {
            kind: NoticeKind::AdviserUnavailable,
            life: None,
            ..
        }
    ));
    assert_eq!(suspended.effects.len(), 1, "one notice to the customer for the proposal");

    assert_eq!(lookup(&mut w, "adv-1").unwrap_err().code, "adviser_not_licensed");
    let open = w.open_command("P9", &["L1"], &[("L1", "life", "life", 1)], true);
    assert_eq!(w.err("new-business", Command::OpenProposal(Box::new(open))), "adviser_not_licensed");
    let consent = Command::RecordConsent {
        proposal: "P1".into(),
        life: "L2".into(),
        kind: ConsentKind::AdviserAccess,
        granted: true,
        signed: Some(signed("consent", &["P1", "L2", "adviser_access"])),
    };
    assert_eq!(w.err("new-business", consent.clone()), "adviser_not_licensed");
    let mut typed = w.disclosure("P1", "L2", &clean());
    typed.entered_by_adviser = 1;
    assert_eq!(w.err("new-business", Command::SubmitDisclosure(Box::new(typed))), "adviser_not_permitted");

    // A second notice of the same state changes nothing more; revocation after suspension likewise.
    let again = w.ok("adviser-licensing", change("adv-1", LicenceStatus::Revoked, d(2026, 11, 11), "FSPR-2"));
    assert!(again.effects.is_empty());

    // The proposal itself carries on, served directly, to issue.
    assert_eq!(w.p("P1").status, ProposalStatus::Open);
    let l2 = w.disclose("P1", "L2", &clean());
    w.automatic(&l2);
    let l1 = "P1/L1/life/1".to_string();
    let done = w.ok(
        "reinsurance",
        Command::CapacityAnswered {
            reservation: l1,
            answer: ic_underwriting::command::CapacityAnswer::Automatic,
        },
    );
    assert!(done.effects.iter().any(|e| matches!(e, Effect::IssuePolicy { .. })));
    assert_eq!((w.stage("P1", "L1"), w.p("P1").status.clone()), (Stage::Accepted, ProposalStatus::Issuing));

    // Reinstated: the adviser may act again, but sees no file without a new consent.
    w.open("P3", 100_000_00);
    w.ok("adviser-licensing", change("adv-1", LicenceStatus::Active, d(2026, 11, 11), "FSPR-3"));
    let Some(View::Adviser { lives, .. }) = lookup(&mut w, "adv-1").unwrap().view else {
        panic!("no view")
    };
    assert!(lives.iter().all(|l| l.answers_ref.is_none()));

    // The provider's licence lapsing has the same effect on every adviser acting under it.
    let mut w = World::new();
    adviser_proposal(&mut w);
    w.consent("P1", "L2", ConsentKind::AdviserAccess);
    let lapsed = w.ok("adviser-licensing", change("fap-1", LicenceStatus::Lapsed, d(2026, 11, 1), "FSPR-9"));
    assert!(lapsed.events.iter().any(|e| matches!(
        e,
        Event::AdviserAccessEnded {
            why: AccessEnd::LicenceSuspended,
            ..
        }
    )));
    assert_eq!(lookup(&mut w, "adv-1").unwrap_err().code, "adviser_not_licensed");
    // A proposal that has ended is not touched by a later licence change.
    let mut w = World::new();
    adviser_proposal(&mut w);
    w.consent("P1", "L1", ConsentKind::AdviserAccess);
    let withdrawn = w.ok("new-business", Command::Withdraw { proposal: "P1".into() });
    assert!(withdrawn.events.iter().any(|e| matches!(
        e,
        Event::AdviserAccessEnded {
            why: AccessEnd::ProposalEnded,
            ..
        }
    )));
    let later = w.ok("adviser-licensing", change("adv-1", LicenceStatus::Suspended, d(2026, 11, 11), "FSPR-1"));
    assert_eq!((later.events.len(), later.effects.len()), (1, 0));
}

/// Purpose: group administration can ask for a scheme member's cover above the automatic
/// acceptance limit to be underwritten and nothing else; accepted terms come back as terms to
/// apply to the scheme policy; and when the proposal ends with nothing to apply, whether
/// withdrawn by group administration, declined or lapsed, an effect says so with the outcome.
/// Group administration cannot withdraw a proposal it did not open.
/// Why it matters: the scheme honours the member's cover up to the limit while the excess is
/// decided. If a decline or a lapse produced no answer, the member's excess would sit pending
/// for ever and nobody would tell them they are not covered for it.
#[test]
fn group_administration_asks_for_excess_cover_and_always_gets_an_answer() {
    use ic_core::PolicyId;
    use ic_underwriting::model::Entry;

    let mut w = World::new();
    let excess = |w: &World, id: &str| {
        let mut open = w.open_command(id, &["L1"], &[("L1", "life", "life", 300_000_00)], false);
        open.product_code = "group-life".into();
        open.entry = Entry::GroupExcess {
            policy: open.policy.clone(),
            takeover_accepted_minor: None,
            reinsurance_reference: None,
        };
        open
    };
    let ended = |applied: &ic_underwriting::command::Applied| {
        applied.effects.iter().find_map(|e| match e {
            Effect::ProposalEnded { key, entry, outcome, .. } => Some((key.clone(), matches!(entry, Entry::GroupExcess { .. }), outcome.clone())),
            _ => None,
        })
    };

    // Only the excess door is open to group administration.
    let ordinary = w.open_command("grp-uw:S1:0", &["L1"], &[("L1", "life", "life", 300_000_00)], false);
    assert_eq!(w.err("group-admin", Command::OpenProposal(Box::new(ordinary.clone()))), "entry");
    w.ok("new-business", Command::OpenProposal(Box::new(ordinary)));
    assert_eq!(
        w.err(
            "group-admin",
            Command::Withdraw {
                proposal: "grp-uw:S1:0".into()
            }
        ),
        "entry"
    );

    // Accepted: terms to apply to the scheme policy, and no "ended" effect.
    let open = excess(&w, "grp-uw:S1:1");
    let policy: PolicyId = open.policy.clone();
    w.ok("group-admin", Command::OpenProposal(Box::new(open)));
    let disclosed = w.disclose("grp-uw:S1:1", "L1", &clean());
    let applied = w.automatic(&disclosed);
    assert!(matches!(&applied.effects[0], Effect::ApplyTerms { entry: Entry::GroupExcess {
            policy: p,
            takeover_accepted_minor: None,
            reinsurance_reference: None,
        }, covers, .. } if p == &policy && covers[0].sum_insured_minor == 300_000_00));
    assert_eq!(ended(&applied), None);

    // Not proceeded with: group administration withdraws its own proposal.
    let open = excess(&w, "grp-uw:S1:2");
    w.ok("group-admin", Command::OpenProposal(Box::new(open)));
    let withdrawn = w.ok(
        "group-admin",
        Command::Withdraw {
            proposal: "grp-uw:S1:2".into(),
        },
    );
    assert_eq!(ended(&withdrawn), Some(("grp-uw:S1:2/ended".into(), true, "withdrawn".into())));

    // Never answered: the sweep lapses it, and says so.
    let open = excess(&w, "grp-uw:S1:3");
    w.ok("group-admin", Command::OpenProposal(Box::new(open)));
    w.days(91);
    let swept = w.ok("timer", Command::Sweep { after: None, max: 100 });
    assert_eq!(ended(&swept), Some(("grp-uw:S1:3/ended".into(), true, "lapsed".into())));

    // Declined by two underwriters: the answer is a decline.
    let open = excess(&w, "grp-uw:S1:4");
    w.ok("group-admin", Command::OpenProposal(Box::new(open)));
    w.disclose("grp-uw:S1:4", "L1", &yes("heart_condition"));
    let case = w.take("grp-uw:S1:4", "L1", "amy");
    w.ok(
        "underwriting-console",
        Command::HandOver {
            case: case.clone(),
            by: "amy".into(),
            to: "sam".into(),
        },
    );
    w.propose(&case, "sam", vec![decline("life")]).unwrap();
    w.ok(
        "underwriting-console",
        Command::TakeCase {
            case: case.clone(),
            underwriter: "sue".into(),
        },
    );
    let declined = w.ok(
        "underwriting-console",
        Command::Countersign {
            case,
            underwriter: "sue".into(),
            agree: true,
            reasons: set(&["RC-0107"]),
            withheld_reasons: Default::default(),
        },
    );
    assert_eq!(ended(&declined), Some(("grp-uw:S1:4/ended".into(), true, "declined".into())));
}

/// Purpose: a scheme member's agreement that their employer may see a loaded premium is a
/// consent of its own, given by the member's signed step and only on a proposal for scheme
/// excess cover. Given, the accepted terms that go to group administration say so; not given,
/// or given and then withdrawn before the decision, they do not.
/// Why it matters: a loading on an itemised invoice tells an employer that an employee has a
/// health condition. The design allows itemised billing only with the member's consent, so the
/// consent has to be the member's own act, tied to this application, and its absence must
/// always read as "no".
#[test]
fn an_employer_sees_a_loading_only_with_the_members_signed_consent() {
    use ic_underwriting::model::{ConsentKind, Entry};

    let mut w = World::new();
    let consent = |id: &str, granted: bool, sign: Option<&str>| Command::RecordConsent {
        proposal: id.into(),
        life: "L1".into(),
        kind: ConsentKind::EmployerSeesLoading,
        granted,
        signed: sign.map(|step| signed("consent", &[id, "L1", step])),
    };
    let excess = |w: &mut World, id: &str| {
        let mut open = w.open_command(id, &["L1"], &[("L1", "life", "life", 300_000_00)], false);
        open.product_code = "group-life".into();
        open.entry = Entry::GroupExcess {
            policy: open.policy.clone(),
            takeover_accepted_minor: None,
            reinsurance_reference: None,
        };
        w.ok("group-admin", Command::OpenProposal(Box::new(open)));
    };
    let told_employer = |w: &mut World, id: &str| {
        let disclosed = w.disclose(id, "L1", &clean());
        let applied = w.automatic(&disclosed);
        applied
            .effects
            .iter()
            .find_map(|e| match e {
                Effect::ApplyTerms { employer_may_see_loading, .. } => Some(*employer_may_see_loading),
                _ => None,
            })
            .unwrap()
    };

    // Not on an ordinary application, and not without the member's own signature for this step.
    w.open("P1", 300_000_00);
    assert_eq!(w.err("new-business", consent("P1", true, Some("employer_sees_loading"))), "entry");
    excess(&mut w, "grp-uw:S1:1");
    assert_eq!(w.err("new-business", consent("grp-uw:S1:1", true, None)), "signature");
    assert_eq!(w.err("new-business", consent("grp-uw:S1:1", true, Some("adviser_access"))), "signature");

    // Given: the terms say the employer may see it.
    w.ok("new-business", consent("grp-uw:S1:1", true, Some("employer_sees_loading")));
    assert!(told_employer(&mut w, "grp-uw:S1:1"));

    // Never given, and given then withdrawn: they do not.
    excess(&mut w, "grp-uw:S1:2");
    assert!(!told_employer(&mut w, "grp-uw:S1:2"));
    excess(&mut w, "grp-uw:S1:3");
    w.ok("new-business", consent("grp-uw:S1:3", true, Some("employer_sees_loading")));
    w.ok("new-business", consent("grp-uw:S1:3", false, None));
    assert!(!told_employer(&mut w, "grp-uw:S1:3"));
}
