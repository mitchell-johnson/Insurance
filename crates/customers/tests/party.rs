//! What each command does to a party, decided without a database.

use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use ic_customers::event::{EventBody, PublicEvent, Recorded};
use ic_customers::model::{Field, Lifecycle, PartyId, PartyKind, RelationshipKind, RiskRating, Target, VerificationOutcome};
use ic_customers::party::{self, Change, FactInput, Party};
use serde_json::json;

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 9, 0, 0).unwrap()
}

fn d(s: &str) -> NaiveDate {
    s.parse().unwrap()
}

fn fact(field: Field, value: serde_json::Value) -> FactInput {
    FactInput {
        field,
        value,
        valid_from: None,
        valid_to: None,
    }
}

/// A party built from the events `create` returns, then any more.
fn build(kind: PartyKind, facts: &[FactInput]) -> Party {
    let id = PartyId("pty_t".into());
    let mut p = Party::new(id.clone());
    for (n, body) in party::create(&id, kind, facts, None, "staff", now()).unwrap().into_iter().enumerate() {
        p.apply(&Recorded {
            seq: n as u64 + 1,
            recorded_at: now(),
            caller: "staff".into(),
            actor: "u".into(),
            body,
        });
    }
    p
}

fn person() -> Party {
    build(
        PartyKind::Person,
        &[fact(Field::FamilyName, json!("Ngata")), fact(Field::DateOfBirth, json!("1980-01-01"))],
    )
}

/// Apply what a command decides, and return the event kinds.
fn run(p: &mut Party, change: Change) -> Result<Vec<&'static str>, String> {
    let events = p.decide(&change, "staff", now()).map_err(|r| r.code)?;
    let kinds = events.iter().map(|e| e.kind()).collect();
    for body in events {
        let seq = p.version + 1;
        p.apply(&Recorded {
            seq,
            recorded_at: now(),
            caller: "staff".into(),
            actor: "u".into(),
            body,
        });
    }
    Ok(kinds)
}

fn relationship(id: &str, kind: RelationshipKind, target: Target) -> Change {
    Change::AddRelationship {
        id: id.into(),
        kind,
        target,
        valid_from: d("2026-01-01"),
        valid_to: None,
        attributes: Default::default(),
    }
}

/// Purpose: creation needs the defining name of the kind of party, checks every fact, and
/// gives a group of the party's own unless one is named.
/// Why it matters: a party without a name cannot be matched or written to, and the group
/// decides which partition every policy of theirs lives on.
#[test]
fn creation_is_checked() {
    let id = PartyId("pty_abc".into());
    let ok = party::create(&id, PartyKind::Person, &[fact(Field::FamilyName, json!("X"))], None, "staff", now()).unwrap();
    assert!(matches!(&ok[0], EventBody::Created { group, .. } if group == "grp_abc"));
    let named = party::create(
        &id,
        PartyKind::Person,
        &[fact(Field::FamilyName, json!("X"))],
        Some("grp_house".into()),
        "staff",
        now(),
    )
    .unwrap();
    assert!(matches!(&named[0], EventBody::Created { group, .. } if group == "grp_house"));
    let code = |kind, facts: &[FactInput], group: Option<&str>, source: &str| {
        party::create(&id, kind, facts, group.map(str::to_string), source, now()).unwrap_err().code
    };
    assert_eq!(code(PartyKind::Person, &[fact(Field::GivenName, json!("X"))], None, "staff"), "invalid");
    assert_eq!(code(PartyKind::Organisation, &[fact(Field::Email, json!("a@b.nz"))], None, "staff"), "invalid");
    assert_eq!(code(PartyKind::Organisation, &[fact(Field::FamilyName, json!("X"))], None, "staff"), "invalid");
    assert_eq!(code(PartyKind::Person, &[fact(Field::OrganisationName, json!("X"))], None, "staff"), "invalid");
    assert_eq!(
        code(PartyKind::Person, &[fact(Field::FamilyName, json!("X"))], Some("bad group"), "staff"),
        "invalid"
    );
    assert_eq!(code(PartyKind::Person, &[fact(Field::FamilyName, json!("X"))], None, "bad source"), "invalid");
    assert_eq!(code(PartyKind::Person, &[], None, "staff"), "invalid");
    let many: Vec<FactInput> = (0..51).map(|_| fact(Field::FamilyName, json!("X"))).collect();
    assert_eq!(code(PartyKind::Person, &many, None, "staff"), "invalid");
    assert_eq!(
        code(PartyKind::Person, &[fact(Field::DateOfDeath, json!("2020-01-01"))], None, "staff"),
        "invalid"
    );
    assert_eq!(
        code(PartyKind::Person, &[fact(Field::DateOfBirth, json!("2999-01-01"))], None, "staff"),
        "invalid"
    );
    assert_eq!(code(PartyKind::Person, &[fact(Field::Email, json!("nope"))], None, "staff"), "invalid");
    let mut future = fact(Field::FamilyName, json!("X"));
    future.valid_from = Some(d("2030-01-01"));
    assert_eq!(code(PartyKind::Person, &[future], None, "staff"), "invalid");
    let mut backwards = fact(Field::FamilyName, json!("X"));
    backwards.valid_from = Some(d("2020-01-01"));
    backwards.valid_to = Some(d("2019-01-01"));
    assert_eq!(code(PartyKind::Person, &[backwards], None, "staff"), "invalid");
}

/// Purpose: facts stated about the past only hold for their period, a cleared fact
/// disappears from that date, and statements are chosen by when they were recorded.
/// Why it matters: a former address added today must not replace the current one, and a
/// phone number given up must stop being used for notices.
#[test]
fn facts_are_read_on_both_timelines() {
    let mut p = person();
    let mut former = fact(Field::PostalAddress, json!(["1 Old Rd"]));
    former.valid_from = Some(d("2010-01-01"));
    former.valid_to = Some(d("2020-01-01"));
    run(
        &mut p,
        Change::Record {
            facts: vec![fact(Field::PostalAddress, json!(["2 New Rd"]))],
            reason: None,
        },
    )
    .unwrap();
    run(
        &mut p,
        Change::Record {
            facts: vec![former],
            reason: None,
        },
    )
    .unwrap();
    let today = now() + Duration::days(1);
    assert_eq!(p.facts(today.date_naive(), today)[&Field::PostalAddress].value, json!(["2 New Rd"]));
    assert_eq!(p.facts(d("2015-01-01"), today)[&Field::PostalAddress].value, json!(["1 Old Rd"]));
    run(
        &mut p,
        Change::Record {
            facts: vec![fact(Field::PostalAddress, json!(null))],
            reason: Some("moved overseas".into()),
        },
    )
    .unwrap();
    assert!(!p.facts(today.date_naive(), today).contains_key(&Field::PostalAddress));
    assert_eq!(p.current(Field::FamilyName, today), Some(json!("Ngata")));
    // A statement for a date already stated needs a reason, and an over-long one is refused.
    let mut again = fact(Field::FamilyName, json!("Ngata-Smith"));
    again.valid_from = Some(party::SINCE_ALWAYS);
    assert_eq!(
        run(
            &mut p,
            Change::Record {
                facts: vec![again.clone()],
                reason: None
            }
        )
        .unwrap_err(),
        "reason_required"
    );
    assert_eq!(
        run(
            &mut p,
            Change::Record {
                facts: vec![again.clone()],
                reason: Some("x".repeat(2_001))
            }
        )
        .unwrap_err(),
        "invalid"
    );
    assert_eq!(
        run(
            &mut p,
            Change::Record {
                facts: vec![again],
                reason: Some("  ".into())
            }
        )
        .unwrap_err(),
        "invalid"
    );
}

/// Purpose: relationships are checked for the right kind of target, dates and size, a
/// dependant needs the date they control their own health information, and adding a
/// policy relationship makes a prospect active.
/// Why it matters: the portal's access and the channels directory are built from these;
/// a malformed relationship would grant or hide access wrongly.
#[test]
fn relationships_are_checked() {
    let mut p = person();
    let policy = || Target::Policy("P1".into());
    assert_eq!(
        run(&mut p, relationship("r", RelationshipKind::Owner, Target::Scheme("S".into()))).unwrap_err(),
        "invalid"
    );
    assert_eq!(
        run(&mut p, relationship("r", RelationshipKind::Owner, Target::Policy("bad id".into()))).unwrap_err(),
        "invalid"
    );
    assert_eq!(run(&mut p, relationship("bad id", RelationshipKind::Owner, policy())).unwrap_err(), "invalid");
    let me = Target::Party(p.id.clone());
    assert_eq!(run(&mut p, relationship("r", RelationshipKind::Attorney, me)).unwrap_err(), "invalid");
    let mut backwards = relationship("r", RelationshipKind::Owner, policy());
    if let Change::AddRelationship { valid_to, .. } = &mut backwards {
        *valid_to = Some(d("2025-01-01"));
    }
    assert_eq!(run(&mut p, backwards).unwrap_err(), "invalid");
    let mut huge = relationship("r", RelationshipKind::Owner, policy());
    if let Change::AddRelationship { attributes, .. } = &mut huge {
        attributes.insert("note".into(), json!("x".repeat(5_000)));
    }
    assert_eq!(run(&mut p, huge).unwrap_err(), "invalid");
    assert_eq!(run(&mut p, relationship("d", RelationshipKind::Dependant, policy())).unwrap_err(), "invalid");
    let mut dependant = relationship("d", RelationshipKind::Dependant, policy());
    if let Change::AddRelationship { attributes, .. } = &mut dependant {
        attributes.insert("controls_from".into(), json!("2040-01-01"));
    }
    assert_eq!(run(&mut p, dependant).unwrap(), vec!["relationship_added", "activated"]);
    assert_eq!(p.lifecycle, Some(Lifecycle::Active));
    // A relationship to a party does not make anyone a customer.
    let mut q = person();
    assert_eq!(
        run(&mut q, relationship("a", RelationshipKind::Partner, Target::Party(PartyId("pty_o".into())))).unwrap(),
        vec!["relationship_added"]
    );
    // Ending: not before it starts, a repeat changes nothing, an unknown one is not found.
    let end = |id: &str, on: &str| Change::EndRelationship {
        id: id.into(),
        valid_to: d(on),
    };
    assert_eq!(run(&mut p, end("d", "2025-01-01")).unwrap_err(), "invalid");
    assert_eq!(run(&mut p, end("x", "2026-07-01")).unwrap_err(), "not_found");
    assert_eq!(run(&mut p, end("d", "2026-07-01")).unwrap(), vec!["relationship_ended"]);
    assert_eq!(run(&mut p, end("d", "2026-07-01")).unwrap(), Vec::<&str>::new());
    // A future end can still be moved.
    assert_eq!(run(&mut p, end("d", "2026-08-01")).unwrap(), vec!["relationship_ended"]);
    // Exiting needs every relationship over, including ones ending later.
    assert_eq!(run(&mut p, Change::Exit).unwrap_err(), "relationships_in_force");
    assert_eq!(run(&mut q, Change::Exit).unwrap_err(), "relationships_in_force");
    let mut r = person();
    assert_eq!(run(&mut r, Change::Exit).unwrap(), vec!["exited"]);
    assert_eq!(run(&mut r, Change::Exit).unwrap(), Vec::<&str>::new());
    assert_eq!(run(&mut r, Change::Activate).unwrap(), vec!["activated"]);
    assert_eq!(run(&mut r, Change::Activate).unwrap(), Vec::<&str>::new());
}

/// Purpose: the limit on relationships per party holds.
/// Why it matters: an unbounded stream makes every read of that party slower for ever; a
/// party with ten thousand relationships is an error to look at, not to keep accepting.
#[test]
fn relationships_per_party_are_bounded() {
    let mut p = person();
    for n in 0..10_000 {
        let rel = ic_customers::model::Relationship {
            id: format!("r{n}"),
            kind: RelationshipKind::Beneficiary,
            target: Target::Policy("P".into()),
            valid_from: d("2026-01-01"),
            valid_to: None,
            recorded_at: now(),
            attributes: Default::default(),
        };
        p.relationships.insert(rel.id.clone(), rel);
    }
    assert_eq!(
        run(&mut p, relationship("one-more", RelationshipKind::Owner, Target::Policy("P".into()))).unwrap_err(),
        "limit"
    );
}

/// Purpose: consents, verifications, risk ratings, holds and groups are recorded with their
/// checks, and a repeated hold or group changes nothing.
/// Why it matters: these drive due diligence reviews, erasure and partitioning; a future
/// dated consent or a verification from tomorrow would be a false record.
#[test]
fn due_diligence_and_holds_are_recorded() {
    let mut p = person();
    let consent = |valid_from: Option<&str>| Change::SetConsent {
        purpose: "marketing".into(),
        granted: true,
        valid_from: valid_from.map(d),
    };
    assert_eq!(run(&mut p, consent(Some("2030-01-01"))).unwrap_err(), "invalid");
    assert_eq!(run(&mut p, consent(None)).unwrap(), vec!["consent_set"]);
    assert!(p.consents_on(now().date_naive())["marketing"]);
    let verify = |at: DateTime<Utc>, provider: &str| Change::RecordVerification {
        method: "document".into(),
        provider: provider.into(),
        outcome: VerificationOutcome::Passed,
        evidence_ref: "vault:doc-1".into(),
        verified_at: at,
    };
    assert_eq!(run(&mut p, verify(now() + Duration::days(1), "idv")).unwrap_err(), "invalid");
    assert_eq!(run(&mut p, verify(now(), "bad provider")).unwrap_err(), "invalid");
    assert_eq!(run(&mut p, verify(now(), "idv")).unwrap(), vec!["verified"]);
    assert_eq!(p.verifications.len(), 1);
    let rate = |by: &str| Change::SetRiskRating {
        rating: RiskRating::High,
        review_by: d(by),
    };
    assert_eq!(run(&mut p, rate("2026-06-01")).unwrap_err(), "invalid");
    assert_eq!(run(&mut p, rate("2027-06-01")).unwrap(), vec!["risk_rated"]);
    let hold = |r: &str| Change::PlaceHold { reference: r.into() };
    assert_eq!(run(&mut p, hold("LH-1")).unwrap(), vec!["hold_placed"]);
    assert_eq!(run(&mut p, hold("LH-1")).unwrap(), Vec::<&str>::new());
    assert_eq!(run(&mut p, Change::ReleaseHold { reference: "LH-2".into() }).unwrap_err(), "not_found");
    assert_eq!(run(&mut p, Change::ReleaseHold { reference: "LH-1".into() }).unwrap(), vec!["hold_released"]);
    let group = |g: &str| Change::AssignGroup { group: g.into() };
    assert_eq!(run(&mut p, group("grp_t")).unwrap(), Vec::<&str>::new());
    assert_eq!(run(&mut p, group("grp_house")).unwrap(), vec!["group_assigned"]);
    assert_eq!(p.group, "grp_house");
    assert_eq!(
        run(
            &mut p,
            Change::DeclineCorrection {
                field: Field::GivenName,
                statement: "".into()
            }
        )
        .unwrap_err(),
        "invalid"
    );
}

/// Purpose: only a person dies, a death after death or before birth is refused, a deceased
/// party cannot be activated, and a retracted death returns the person to active when a
/// relationship is open.
/// Why it matters: the death marker restricts every policy of the person; it must be right.
#[test]
fn deaths_follow_the_rules() {
    let mut org = build(PartyKind::Organisation, &[fact(Field::OrganisationName, json!("Acme"))]);
    let die = |on: &str| Change::MarkDeceased { date_of_death: d(on) };
    assert_eq!(run(&mut org, die("2026-01-01")).unwrap_err(), "invalid");
    let mut p = person();
    run(&mut p, relationship("o", RelationshipKind::Owner, Target::Policy("P".into()))).unwrap();
    assert_eq!(run(&mut p, die("1970-01-01")).unwrap_err(), "invalid");
    assert_eq!(run(&mut p, die("2026-05-01")).unwrap(), vec!["asserted", "deceased"]);
    assert_eq!(run(&mut p, Change::Activate).unwrap_err(), "deceased");
    assert_eq!(run(&mut p, Change::RetractDeath { reason: "".into() }).unwrap_err(), "invalid");
    assert_eq!(
        run(&mut p, Change::RetractDeath { reason: "error".into() }).unwrap(),
        vec!["asserted", "death_retracted"]
    );
    assert_eq!(p.lifecycle, Some(Lifecycle::Active));
}

/// Purpose: merging and unmerging refuse missing, erased, already merged, mismatched and
/// self merges, and a party that has absorbed others cannot itself be absorbed; an erased
/// or merged party takes no commands.
/// Why it matters: merge chains and cross-kind merges would make an ID resolve to the wrong
/// record, and a wrong record is a privacy breach.
#[test]
fn merges_follow_the_rules() {
    let a = person();
    let mut b = person();
    b.id = PartyId("pty_b".into());
    let org = build(PartyKind::Organisation, &[fact(Field::OrganisationName, json!("Acme"))]);
    let mut org = org;
    org.id = PartyId("pty_org".into());
    let code = |r: Result<(EventBody, EventBody), party::Reject>| r.unwrap_err().code;
    assert_eq!(code(party::merge(&a, &Party::new(PartyId("none".into())), "same person")), "not_found");
    assert_eq!(code(party::merge(&a, &a, "same person")), "invalid");
    assert_eq!(code(party::merge(&a, &org, "same person")), "invalid");
    let mut absorbing = a.clone();
    absorbing.absorbed.insert(PartyId("pty_c".into()));
    assert_eq!(code(party::merge(&b, &absorbing, "same person")), "invalid");
    let mut merged = b.clone();
    merged.merged_into = Some(a.id.clone());
    assert_eq!(code(party::merge(&a, &merged, "same person")), "merged");
    assert_eq!(merged.decide(&Change::Activate, "staff", now()).unwrap_err().code, "merged");
    let mut erased = b.clone();
    erased.shredded = true;
    assert_eq!(code(party::merge(&a, &erased, "same person")), "shredded");
    assert_eq!(erased.decide(&Change::Activate, "staff", now()).unwrap_err().code, "shredded");
    assert_eq!(
        Party::new(PartyId("x".into())).decide(&Change::Activate, "staff", now()).unwrap_err().code,
        "not_found"
    );
    assert_eq!(code(party::unmerge(&a, &b)), "not_merged");
    let mut s = a.clone();
    s.absorbed.insert(b.id.clone());
    s.shredded = true;
    assert_eq!(code(party::unmerge(&s, &merged)), "shredded");
}

/// Purpose: every command has the name the policy and the audit use, every event has its
/// public kind, and public events never carry values.
/// Why it matters: a command whose name the policy does not know is denied by default, and
/// a value in a public event would publish personal data to every consumer.
#[test]
fn names_and_public_events() {
    let changes = [
        (Change::Record { facts: vec![], reason: None }, "record"),
        (
            Change::DeclineCorrection {
                field: Field::Email,
                statement: "s".into(),
            },
            "decline_correction",
        ),
        (relationship("r", RelationshipKind::Owner, Target::Policy("P".into())), "add_relationship"),
        (
            Change::EndRelationship {
                id: "r".into(),
                valid_to: d("2026-01-02"),
            },
            "end_relationship",
        ),
        (
            Change::SetConsent {
                purpose: "p".into(),
                granted: true,
                valid_from: None,
            },
            "set_consent",
        ),
        (
            Change::RecordVerification {
                method: "m".into(),
                provider: "p".into(),
                outcome: VerificationOutcome::Referred,
                evidence_ref: "e".into(),
                verified_at: now(),
            },
            "record_verification",
        ),
        (
            Change::SetRiskRating {
                rating: RiskRating::Low,
                review_by: d("2027-01-01"),
            },
            "set_risk_rating",
        ),
        (Change::Activate, "activate"),
        (
            Change::MarkDeceased {
                date_of_death: d("2026-01-01"),
            },
            "mark_deceased",
        ),
        (Change::RetractDeath { reason: "r".into() }, "retract_death"),
        (Change::Exit, "exit"),
        (Change::AssignGroup { group: "g".into() }, "assign_group"),
        (Change::PlaceHold { reference: "h".into() }, "place_hold"),
        (Change::ReleaseHold { reference: "h".into() }, "release_hold"),
    ];
    for (c, name) in changes {
        assert_eq!(c.name(), name);
    }
    let id = PartyId("pty_t".into());
    let rec = |body: EventBody| Recorded {
        seq: 7,
        recorded_at: now(),
        caller: "staff".into(),
        actor: "u".into(),
        body,
    };
    for (body, kind, priority) in [
        (EventBody::Exited, "exited", false),
        (EventBody::Unmerged { survivor: PartyId("s".into()) }, "unmerged", true),
        (EventBody::Released { other: PartyId("o".into()) }, "released", false),
        (EventBody::DeathRetracted { reason: "r".into() }, "death_retracted", false),
    ] {
        let e = PublicEvent::of(&id, &rec(body), None);
        assert_eq!((e.kind.as_str(), e.priority), (kind, priority));
        assert_eq!(e.id, "pty_t:7");
    }
    let ended = PublicEvent::of(
        &id,
        &rec(EventBody::RelationshipEnded {
            id: "gone".into(),
            valid_to: d("2026-01-02"),
        }),
        None,
    );
    assert!(ended.relationship.is_none() && ended.priority);
    let text = serde_json::to_string(&PublicEvent::of(
        &id,
        &rec(EventBody::Asserted {
            assertions: vec![ic_customers::model::Assertion {
                field: Field::Email,
                value: Some(json!("secret@x.nz")),
                valid_from: d("2026-01-01"),
                valid_to: None,
                recorded_at: now(),
                reason: None,
                source: "s".into(),
            }],
        }),
        None,
    ))
    .unwrap();
    assert!(text.contains("\"email\"") && !text.contains("secret"));
    // Names and kinds are stable text.
    for f in Field::ALL {
        assert_eq!(Field::parse(f.as_str()), Some(*f));
        assert_eq!(f.to_string(), f.as_str());
    }
    assert_eq!(Field::parse("shoe_size"), None);
    for k in RelationshipKind::ALL {
        assert_eq!(RelationshipKind::parse(k.as_str()), Some(*k));
        assert_eq!(k.to_string(), k.as_str());
    }
    for l in [Lifecycle::Prospect, Lifecycle::Active, Lifecycle::Deceased, Lifecycle::Exited] {
        assert_eq!(Lifecycle::parse(l.as_str()), Some(l));
    }
    assert_eq!(Lifecycle::parse("zombie"), None);
    assert_eq!(Target::from_parts("car", "1"), None);
    assert!(PartyId::plausible(&PartyId::generate().0));
    assert_eq!(PartyId("p".into()).to_string(), "p");
}
