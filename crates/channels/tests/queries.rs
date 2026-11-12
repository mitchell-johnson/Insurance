//! The questions the portal service asks the channel module, and the module
//! as the ledger nodes host it.

mod common;

use common::*;
use ic_channels::classes::DataClass;
use ic_channels::directory::{DirectoryCommand, LicenceStatus};
use ic_channels::feed::Row;
use ic_channels::identity::{proof_message, Enrolment, IdentityCommand};
use ic_channels::query::{answer, ask, module_spec, DeviceProof, Question, MODULE, STALE_AFTER_SECS};
use ic_channels::staff::{actions as staff_actions, Profile, Purpose, PurposeKind, StaffCommand, Template};
use ic_channels::state::{EVENT_RELAY, GATEWAY};
use ic_channels::workbench::{self, HoldReason, Item, ItemKind, WorkbenchCommand};
use ic_channels::Command;
use ic_core::PolicyId;
use ic_product::{Frequency, Sex};
use ic_sim::persona::term_life;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const KIRI: &str = "party-00000001";

fn retail(w: &mut World) -> PolicyId {
    w.issue(term_life(
        1,
        open(),
        d(1990, 3, 10),
        Sex::Female,
        false,
        500_000_00,
        None,
        Frequency::Monthly,
        None,
    ))
}

fn template(id: &str, actions: &[&str], classes: &[DataClass]) -> Template {
    Template {
        id: id.to_string(),
        actions: actions.iter().map(|a| a.to_string()).collect(),
        classes: classes.iter().copied().collect(),
        default_profile: "none".into(),
        ceilings: Default::default(),
        break_glass: false,
    }
}

/// An administrator, a service agent who sees personal data, a claims
/// assessor who also sees health-implying facts, and a finance officer.
fn staffed(w: &mut World) {
    w.send(
        module("entitlements"),
        Command::Staff(StaffCommand::Bootstrap {
            templates: vec![
                template("administrator", &[staff_actions::MANAGE_STAFF], &[DataClass::Ordinary]),
                template("service", &["policy.view"], &[DataClass::Ordinary, DataClass::Personal]),
                template(
                    "assessor",
                    &["policy.view"],
                    &[DataClass::Ordinary, DataClass::Personal, DataClass::InsuranceSensitive],
                ),
                template("finance", &[workbench::actions::VIEW], &[DataClass::Ordinary, DataClass::Financial]),
            ],
            profiles: vec![Profile {
                id: "none".into(),
                version: 1,
                limits: Default::default(),
            }],
            admin: p("alice"),
            admin_templates: vec!["administrator".into()],
        }),
    )
    .unwrap();
    for (who, role) in [("sue", "service"), ("ana", "assessor"), ("fin", "finance")] {
        w.send(
            staff("alice"),
            Command::Staff(StaffCommand::Employ {
                party: p(who),
                team: "ops".into(),
                manager: None,
                templates: vec![role.into()],
            }),
        )
        .unwrap();
    }
}

fn task() -> Option<Purpose> {
    Some(Purpose {
        kind: PurposeKind::Task,
        reference: "case-1".into(),
    })
}

/// Purpose: a session question resolves a sign-in to a party, says whether the session still stands, and raises assurance only on a valid device proof.
/// Why it matters: every request starts here; an unbound subject, a revoked session or a forged proof must each be visible to the gateway so it can refuse or step up.
#[test]
fn a_session_resolves_to_a_party_and_an_assurance() {
    let mut w = World::new();
    let phone = Key::new();
    w.dir(
        module("identity"),
        DirectoryCommand::BindSubject {
            subject: "sub-1".into(),
            party: p(KIRI),
        },
    )
    .unwrap();
    w.send(
        module("identity"),
        Command::Identity(IdentityCommand::EnrolDevice {
            device: "phone".into(),
            party: p(KIRI),
            public_key: phone.public.clone(),
            hardware_backed: true,
            enrolment: Enrolment::Verification { reference: "idv-1".into() },
        }),
    )
    .unwrap();
    let at = w.at;
    let session = |w: &World, subject: &str, issued, device| {
        ask(
            &w.ch,
            Question::Session {
                subject: subject.into(),
                issued,
                at,
                device,
            },
        )
    };
    assert_eq!(session(&w, "sub-9", at, None), json!({ "known": false }));

    let plain = session(&w, "sub-1", at, None);
    assert_eq!(
        (&plain["party"], &plain["valid"], &plain["assurance"], &plain["staff"]),
        (&json!(KIRI), &json!(true), &json!("token"), &json!(false))
    );

    let body = br#"{"a":1}"#;
    let proof = |key: &Key| DeviceProof {
        device: "phone".into(),
        proof: format!("{at}.{}", key.sign(&proof_message(at, "POST", "/app/v1/requests", body))),
        method: "POST".into(),
        path: "/app/v1/requests".into(),
        body_sha256: hex::encode(Sha256::digest(body)),
    };
    let proved = session(&w, "sub-1", at, Some(proof(&phone)));
    assert_eq!((&proved["assurance"], &proved["device"]), (&json!("device"), &json!("phone")));
    let forged = session(&w, "sub-1", at, Some(proof(&Key::new())));
    assert_eq!((&forged["assurance"], &forged["device"]), (&json!("token"), &Value::Null));

    // The identity service closes every session: older tokens stop working.
    w.send(module("identity"), Command::Identity(IdentityCommand::RevokeSessions { party: p(KIRI) }))
        .unwrap();
    assert_eq!(session(&w, "sub-1", at, None)["valid"], false);
    assert_eq!(session(&w, "sub-1", w.at + 1, None)["valid"], true);
    let devices = ask(&w.ch, Question::Devices { party: p(KIRI) });
    assert_eq!(devices[0]["id"], "phone");
    assert!(devices[0].get("public_key").is_none());
    assert_eq!(ask(&w.ch, Question::Devices { party: p("nobody") }), json!([]));
}

/// Purpose: a customer's questions return their own policies, rows, sync feed and requests; a stranger gets the same answer as for a policy that does not exist.
/// Why it matters: the queries are the read side of every customer screen, and "not yours" must be indistinguishable from "not there".
#[test]
fn a_customer_asks_about_their_own_things() {
    let mut w = World::new();
    let pol = retail(&mut w);
    let at = w.at;
    let policies = ask(&w.ch, Question::Policies { party: p(KIRI), at });
    assert_eq!(policies[0]["number"], "KTL00000001");
    assert_eq!(policies[0]["relationships"], json!(["policyholder", "life_insured"]));
    assert_eq!(ask(&w.ch, Question::Policies { party: p("stranger"), at }), json!([]));

    let view = ask(
        &w.ch,
        Question::Policy {
            viewer: p(KIRI),
            policy: pol.clone(),
            at,
        },
    );
    assert_eq!(view["fresh"], true);
    assert_eq!(view["rows"].as_array().unwrap().len(), 4);
    assert_eq!(
        ask(
            &w.ch,
            Question::Policy {
                viewer: p("stranger"),
                policy: pol.clone(),
                at
            }
        ),
        Value::Null
    );
    assert_eq!(
        ask(
            &w.ch,
            Question::Policy {
                viewer: p(KIRI),
                policy: PolicyId::from("nope"),
                at
            }
        ),
        Value::Null
    );

    // When the stream has been silent past the bound, the answer says so and sensitive rows are withheld.
    w.send(
        module("claims"),
        Command::PublishRows {
            rows: vec![Row {
                key: format!("{pol}/claims/c1"),
                policy: pol.clone(),
                kind: "claim".into(),
                subject: Some(p(KIRI)),
                sensitive: true,
                body: json!({}),
            }],
            remove: vec![],
        },
    )
    .unwrap();
    let late = w.ch.data.directory.heard + STALE_AFTER_SECS + 1;
    let stale = ask(
        &w.ch,
        Question::Policy {
            viewer: p(KIRI),
            policy: pol.clone(),
            at: late,
        },
    );
    assert_eq!((&stale["fresh"], stale["rows"].as_array().unwrap().len()), (&json!(false), 4));
    // The gateway's heartbeat says the stream is current again.
    w.at = late;
    w.send(module(GATEWAY), Command::Heartbeat).unwrap();
    assert_eq!(
        ask(
            &w.ch,
            Question::Policy {
                viewer: p(KIRI),
                policy: pol.clone(),
                at: w.at
            }
        )["rows"]
            .as_array()
            .unwrap()
            .len(),
        5
    );

    assert_eq!(ask(&w.ch, Question::Sync { party: p(KIRI), have: None })["mode"], "snapshot");
    assert_eq!(
        ask(
            &w.ch,
            Question::Sync {
                party: p("stranger"),
                have: None
            }
        ),
        json!({ "mode": "snapshot", "version": 0, "rows": [] })
    );

    w.send(
        party(KIRI),
        Command::SubmitRequest {
            id: "r1".into(),
            policy: Some(pol),
            kind: "cancel_policy".into(),
            module: "policy-servicing".into(),
            payload: json!({}),
            signed: true,
        },
    )
    .unwrap();
    assert_eq!(ask(&w.ch, Question::Requests { party: p(KIRI) })[0]["status"], "pending");
    assert_eq!(ask(&w.ch, Question::Requests { party: p("stranger") }), json!([]));
    assert_eq!(
        ask(
            &w.ch,
            Question::Pending {
                module: "policy-servicing".into()
            }
        )[0]["id"],
        "r1"
    );
    let status = ask(&w.ch, Question::Status);
    assert_eq!(
        (&status["policies"], &status["feeds"], &status["requests_pending"], &status["business_date"]),
        (&json!(1), &json!(1), &json!(1), &json!("2027-11-15"))
    );
}

/// Purpose: the adviser's book shows each engagement's standing, whether each policy can be transacted on, the read-only banner, and the adviser's own held money with its reason.
/// Why it matters: addendum entry 4 in the portal: an adviser under a lost licence must see why they are read-only and what is being held from them, without calling the insurer.
#[test]
fn an_advisers_book_shows_standing_and_held_money() {
    let mut w = World::new();
    let pol = retail(&mut w);
    w.engage("e1", "adv-1", "fap-1", None).unwrap();
    w.dir(
        party(KIRI),
        DirectoryCommand::GrantAdviserAuthority {
            policy: pol.clone(),
            adviser: p("adv-1"),
            engagement: "e1".into(),
        },
    )
    .unwrap();
    let at = w.at;
    let book = ask(&w.ch, Question::Book { adviser: p("adv-1"), at });
    assert_eq!((&book["read_only"], &book["policies"][0]["can_transact"]), (&json!(false), &json!(true)));
    assert_eq!(book["engagements"][0]["standing"], json!({ "state": "active" }));
    assert_eq!(book["held"]["entries"], json!([]));

    w.dir(
        module("compliance"),
        DirectoryCommand::RecordLicence {
            party: p("fap-1"),
            status: LicenceStatus::Revoked,
            effective: open(),
            source: "FMA".into(),
        },
    )
    .unwrap();
    w.send(
        module("commission"),
        Command::Workbench(WorkbenchCommand::Publish {
            party: p("adv-1"),
            items: vec![Item {
                id: "h1".into(),
                kind: ItemKind::Held,
                amount_minor: 412_50,
                currency: "NZD".into(),
                period: Some("2027-11".into()),
                book: Some("book-1".into()),
                reason: Some(HoldReason::SuspendedLicence),
                since: open(),
            }],
            documents: vec![],
            cases: vec![],
            conduct: None,
        }),
    )
    .unwrap();
    let book = ask(&w.ch, Question::Book { adviser: p("adv-1"), at });
    assert_eq!((&book["read_only"], &book["policies"][0]["can_transact"]), (&json!(true), &json!(false)));
    assert_eq!(book["engagements"][0]["standing"]["licence_of"], "fap-1");
    assert_eq!(book["held"]["held"], json!({ "NZD": 412_50 }));
    assert_eq!(
        ask(&w.ch, Question::HeldBalance { party: p("adv-1") })["entries"][0]["reason"],
        "suspended_licence"
    );
    let empty = ask(&w.ch, Question::Book { adviser: p("adv-9"), at });
    assert_eq!((&empty["policies"], &empty["engagements"]), (&json!([]), &json!([])));
}

/// Purpose: staff see a policy only when the four questions allow it, shaped to the data classes they hold; health-implying rows are withheld from those without the class, and they are told that something was withheld.
/// Why it matters: the staff console is the widest view in the platform; it must never show a service agent the existence of a claim, and a denial must explain itself so a gap becomes a request to a manager.
#[test]
fn staff_see_a_policy_shaped_to_their_classes() {
    let mut w = World::new();
    let pol = retail(&mut w);
    staffed(&mut w);
    w.send(
        module("claims"),
        Command::PublishRows {
            rows: vec![Row {
                key: format!("{pol}/claims/c1"),
                policy: pol.clone(),
                kind: "claim".into(),
                subject: Some(p(KIRI)),
                sensitive: true,
                body: json!({ "state": "received" }),
            }],
            remove: vec![],
        },
    )
    .unwrap();
    let at = w.at;
    let look = |w: &World, who: &str, purpose| {
        ask(
            &w.ch,
            Question::StaffPolicy {
                staff: p(who),
                policy: pol.clone(),
                scopes: vec![],
                purpose,
                at,
            },
        )
    };
    let service = look(&w, "sue", task());
    assert_eq!(
        (&service["found"], &service["withheld"], service["rows"].as_array().unwrap().len()),
        (&json!(true), &json!(1), 4)
    );
    let assessor = look(&w, "ana", task());
    assert_eq!((&assessor["withheld"], assessor["rows"].as_array().unwrap().len()), (&json!(0), 5));

    // No purpose, no role, not staff: refused with an explanation, and no rows.
    let no_purpose = look(&w, "sue", None);
    assert_eq!(no_purpose["decision"]["denied"], "why");
    assert!(no_purpose["explanation"].as_str().unwrap().contains("purpose"));
    assert!(no_purpose.get("rows").is_none());
    assert_eq!(look(&w, "fin", task())["decision"]["denied"], "what");
    assert_eq!(look(&w, KIRI, task())["decision"]["denied"], "not_staff");

    // The same question through the generic authorise path, as the gateway asks before any staff action.
    let request = json!({ "q": "authorise", "request": { "staff": "sue", "action": "policy.view", "class": "personal", "purpose": { "kind": "task", "reference": "case-1" }, "fresh": true, "at": at } });
    assert_eq!(answer(&w.ch, request).unwrap()["decision"]["allowed"], true);
    let own = ask(&w.ch, Question::OwnAccess { staff: p("sue"), at });
    assert_eq!(own["templates"], json!(["service"]));
    let admin = ask(&w.ch, Question::StaffAdmin { staff: p("alice"), at });
    assert_eq!((&admin["allowed"], admin["members"].as_array().unwrap().len()), (&json!(true), 4));
    assert_eq!(ask(&w.ch, Question::StaffAdmin { staff: p("sue"), at }), json!({ "allowed": false }));
    assert!(answer(&w.ch, json!({ "q": "nonsense" })).unwrap_err().contains("not a question"));
}

/// Purpose: the workbench list, one party's position and the proof of debt extract are answered only for staff who hold the financial role.
/// Why it matters: counterparty positions are financial-class data about named businesses and people; the query is the gate in front of the screen.
#[test]
fn the_workbench_is_answered_for_finance_only() {
    let mut w = World::new();
    staffed(&mut w);
    w.send(
        module("commission"),
        Command::Workbench(WorkbenchCommand::Publish {
            party: p("fap-1"),
            items: vec![Item {
                id: "h1".into(),
                kind: ItemKind::Held,
                amount_minor: 900_00,
                currency: "NZD".into(),
                period: None,
                book: None,
                reason: Some(HoldReason::Dispute),
                since: open(),
            }],
            documents: vec![],
            cases: vec![],
            conduct: None,
        }),
    )
    .unwrap();
    let at = w.at;
    let list = ask(
        &w.ch,
        Question::Workbench {
            staff: p("fin"),
            party: None,
            at,
        },
    );
    assert_eq!(
        list["parties"][0],
        json!({ "party": "fap-1", "status": "normal", "held": { "NZD": 900_00 }, "net": { "NZD": 900_00 }, "open_actions": 0 })
    );
    assert_eq!(list["control"]["held"][0]["total_minor"], 900_00);
    assert_eq!(list["settings"]["two_approver_above_minor"], 50_000_00);
    let one = ask(
        &w.ch,
        Question::Workbench {
            staff: p("fin"),
            party: Some(p("fap-1")),
            at,
        },
    );
    assert_eq!(one["position"]["held"], json!({ "NZD": 900_00 }));
    assert_eq!(
        ask(
            &w.ch,
            Question::Workbench {
                staff: p("fin"),
                party: Some(p("nobody")),
                at
            }
        )["position"],
        Value::Null
    );
    let extract = ask(
        &w.ch,
        Question::ProofOfDebt {
            staff: p("fin"),
            party: p("fap-1"),
            at,
        },
    );
    assert_eq!(extract["extract"]["owed_to_party"], json!({ "NZD": 900_00 }));

    for question in [
        Question::Workbench {
            staff: p("sue"),
            party: None,
            at,
        },
        Question::Workbench {
            staff: p("sue"),
            party: Some(p("fap-1")),
            at,
        },
        Question::ProofOfDebt {
            staff: p("sue"),
            party: p("fap-1"),
            at,
        },
    ] {
        let refused = ask(&w.ch, question);
        assert_eq!(refused["decision"]["allowed"], false);
        assert!(refused.get("position").is_none() && refused.get("extract").is_none() && refused.get("parties").is_none());
    }
}

/// Purpose: the module as a ledger node hosts it takes the core's events in through the host, answers questions, takes commands as JSON, and restores from a snapshot to the same answers.
/// Why it matters: this is the production wiring; if the hosted form dropped events, refused the JSON the portal sends, or lost state across a snapshot, every channel would be wrong at once.
#[test]
fn the_hosted_module_follows_events_and_survives_a_snapshot() {
    let spec = module_spec().unwrap();
    assert_eq!((spec.name.as_str(), spec.events), (MODULE, true));
    let mut hosted = (spec.make)();

    let issue = term_life(1, open(), d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
    let mut cluster = ic_sim::Cluster::in_memory(2, 7);
    cluster.open(open()).unwrap();
    for activation in ic_sim::config::shipped_activations().unwrap() {
        cluster.activate_everywhere(activation).unwrap();
    }
    let partition = cluster.partition_of(&issue.group);
    cluster.submit(partition, "new-business", ic_sim::persona::issue_command(issue)).unwrap();
    let events = cluster.take_events();
    for (i, event) in events.iter().enumerate() {
        hosted.on_event(i as u64 + 1, event);
    }
    // Offered again, as after a change of leader: taken in once.
    hosted.on_event(99, &events[0]);

    let sync = hosted.query(json!({ "q": "sync", "party": KIRI, "have": null })).unwrap();
    assert_eq!(sync["rows"].as_array().unwrap().len(), 4);
    assert!(hosted.query(json!({ "q": "nonsense" })).is_err());

    // A command as the portal sends it, and one the module cannot decode.
    let heartbeat = json!({ "key": "hb-1", "at": t0(), "actor": { "kind": "module", "id": GATEWAY }, "command": "heartbeat" });
    let reply = hosted.apply(100, heartbeat.clone()).unwrap();
    assert_eq!(reply, json!({ "result": { "Ok": [] }, "duplicate": false }));
    assert_eq!(hosted.apply(101, heartbeat).unwrap()["duplicate"], true);
    assert!(hosted.apply(102, json!({ "key": "x" })).is_err());
    let forbidden = json!({ "key": "hb-2", "at": t0(), "actor": { "kind": "party", "id": KIRI }, "command": "heartbeat" });
    assert_eq!(hosted.apply(103, forbidden).unwrap()["result"]["Err"]["code"], "forbidden");

    let saved = hosted.save().unwrap();
    let mut restored = (spec.make)();
    restored.load(saved.clone()).unwrap();
    assert_eq!(restored.save().unwrap(), saved);
    assert_eq!(
        restored.query(json!({ "q": "status" })).unwrap(),
        hosted.query(json!({ "q": "status" })).unwrap()
    );
    assert!(restored.load(json!("not a state")).is_err());

    // Each event becomes one command of the relay's, keyed by the event's ID.
    let direct = ic_channels::query::react(&events[0]);
    assert_eq!(direct[0].key, format!("event:{}", events[0].id));
    assert_eq!(direct[0].actor, module(EVENT_RELAY));
}
