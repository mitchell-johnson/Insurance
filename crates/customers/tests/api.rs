//! The customer master end to end: the HTTP API over a real PostgreSQL
//! database, Key Vault key wrapping and Entra ID tokens.

mod common;

use axum::http::StatusCode;
use common::{world, Call};
use serde_json::{json, Value};

macro_rules! world_or_skip {
    () => {
        match world().await {
            Some(w) => w,
            None => return,
        }
    };
}

/// Purpose: the group service's match call, exactly as `HttpCustomerMaster` sends it,
/// creates an unknown joiner, links the same joiner when an IRD number confirms them, and
/// returns only a possible match on name and birth date alone.
/// Why it matters: this is the contract group schemes already depend on (addendum entry 2):
/// a confident match links, nobody creates, anything between goes to a person.
#[tokio::test]
async fn the_group_match_contract_holds() {
    let w = world_or_skip!();
    let joiner = json!({
        "source": "group-scheme:S1",
        "given_name": "Aroha",
        "family_name": "Ngata",
        "date_of_birth": "1980-02-03",
        "email": null,
        "national_id": "49091850",
        "create_if_none": true,
    });
    let (status, created) = w.post("/v1/parties/match", "group-admin", joiner.clone()).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["outcome"], "created");
    let party = created["party"].as_str().unwrap().to_string();
    // The same request again (a retry after a timeout) returns the same person.
    let (_, again) = w.post("/v1/parties/match", "group-admin", joiner).await;
    assert_eq!(again, created);
    // Another scheme's file, the same person with their IRD number: linked.
    let (_, linked) = w
        .post(
            "/v1/parties/match",
            "group-admin",
            json!({ "source": "group-scheme:S2", "given_name": "Aroha", "family_name": "Ngata",
                    "date_of_birth": "1980-02-03", "national_id": "49-091-850", "create_if_none": true }),
        )
        .await;
    assert_eq!(linked["outcome"], "linked");
    assert_eq!(linked["party"], party);
    // Name and birth date without the IRD number: a person decides.
    let (_, possible) = w
        .post(
            "/v1/parties/match",
            "group-admin",
            json!({ "source": "group-scheme:S3", "given_name": "Aroha", "family_name": "Ngata",
                    "date_of_birth": "1980-02-03", "email": "not-an-email", "create_if_none": true }),
        )
        .await;
    assert_eq!(possible["outcome"], "possible");
    assert_eq!(possible["candidates"], json!([party]));
    assert_eq!(possible["ignored"], json!(["email"]));
    // Nobody like this, and no creation asked for.
    let (_, none) = w
        .post(
            "/v1/parties/match",
            "group-admin",
            json!({ "source": "group-scheme:S1", "family_name": "Zzyzx" }),
        )
        .await;
    assert_eq!(none["outcome"], "none");
    // The documents service may not match people.
    let (status, _) = w.post("/v1/parties/match", "documents", json!({ "source": "x", "family_name": "Ngata" })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Malformed requests are refused before anything is looked up.
    for bad in [
        json!({ "source": "x", "family_name": "--" }),
        json!({ "source": "bad source", "family_name": "Ngata" }),
        json!({ "family_name": "Ngata" }),
    ] {
        let (status, _) = w.post("/v1/parties/match", "group-admin", bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}

/// Purpose: the documents service's contact-point call returns the name and destination for
/// each channel, with language and format, and 404 when the party has none; it is refused
/// for a purpose other than notices.
/// Why it matters: this is the route `HttpDirectory` already calls; any other status is
/// treated as an outage and would hold every notice.
#[tokio::test]
async fn the_documents_contact_point_contract_holds() {
    let w = world_or_skip!();
    let p = w
        .person(
            "Mere",
            "Walker",
            "1975-05-06",
            &[
                ("preferred_name", json!("Mere")),
                ("email", json!("Mere@Example.nz")),
                ("postal_address", json!(["1 Queen St", "Auckland 1010"])),
                ("preferred_language", json!("mi")),
                ("presentation_format", json!("large_print")),
            ],
        )
        .await;
    let path = |channel: &str| format!("/v1/parties/{p}/contact-points?purpose=notices&channel={channel}&relationship=policyholder");
    let (status, email) = w.get(&path("email"), "documents", "notices").await;
    assert_eq!(status, StatusCode::OK, "{email}");
    assert_eq!(email["name"], "Mere Walker");
    assert_eq!(email["destination"], "mere@example.nz");
    assert_eq!(email["language"], "mi");
    assert_eq!(email["format"], "large_print");
    let (_, post) = w.get(&path("post"), "documents", "notices").await;
    assert_eq!(post["destination"], "1 Queen St\nAuckland 1010");
    let (_, app) = w.get(&path("in_app"), "documents", "notices").await;
    assert_eq!(app["destination"], p);
    let (status, _) = w.get(&path("sms"), "documents", "notices").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = w.get(&path("fax"), "documents", "notices").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = w
        .get(
            &format!("/v1/parties/{p}/contact-points?channel=email&relationship=bad%20one"),
            "documents",
            "notices",
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = w.get(&format!("/v1/parties/{p}/contact-points?channel=email"), "documents", "marketing").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = w.get(&path("email").replace(&p, "pty_nobody"), "documents", "notices").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // An organisation is addressed by its name.
    let (status, org) = w
        .post(
            "/v1/parties",
            "staff",
            json!({ "kind": "organisation", "facts": [
                { "field": "organisation_name", "value": "Acme Ltd" },
                { "field": "email", "value": "accounts@acme.nz" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{org}");
    let org = org["party"].as_str().unwrap();
    let (_, c) = w.get(&format!("/v1/parties/{org}/contact-points?channel=email"), "documents", "notices").await;
    assert_eq!(c["name"], "Acme Ltd");
    // Each read is in the audit trail, with only the fields used.
    let (status, audit) = w.get(&format!("/v1/parties/{p}/audit"), "privacy", "audit").await;
    assert_eq!(status, StatusCode::OK);
    let lines = audit["audit"].as_array().unwrap();
    assert!(lines
        .iter()
        .any(|l| l["line"]["action"] == "contact_points" && l["line"]["decision"] == "allow"));
    assert!(lines.iter().any(|l| l["line"]["decision"] == "deny"));
    let (status, _) = w.get(&format!("/v1/parties/{p}/audit"), "staff", "audit").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = w.get("/v1/parties/bad%20id/audit", "privacy", "audit").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Purpose: reads release fields by purpose, mask what is withheld, and answer on both
/// timelines: what was true on a date, and what we knew at a time.
/// Why it matters: backdated changes and corrections are the norm in insurance; a claim
/// assessor must be able to see what the record said when a decision was made.
#[tokio::test]
async fn records_have_two_timelines_and_purpose_based_reads() {
    let w = world_or_skip!();
    let p = w
        .person("Tama", "Rangi", "1990-01-01", &[("ird_number", json!("49091850")), ("email", json!("t@x.nz"))])
        .await;
    let (status, before) = w.get(&format!("/v1/parties/{p}"), "staff", "servicing").await;
    assert_eq!(status, StatusCode::OK);
    let learned = chrono::Utc::now();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    // A change of name from 1 March 2026, learned today.
    let (status, out) = w
        .command(
            &p,
            "staff",
            json!({ "command": "record", "facts": [{ "field": "family_name", "value": "Rangi-Smith", "valid_from": "2026-03-01" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let (_, now) = w.get(&format!("/v1/parties/{p}"), "staff", "servicing").await;
    assert_eq!(now["facts"]["family_name"]["value"], "Rangi-Smith");
    let (_, then) = w.get(&format!("/v1/parties/{p}?as_of=2026-02-01"), "staff", "servicing").await;
    assert_eq!(then["facts"]["family_name"]["value"], "Rangi");
    let known = learned.to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
    let (_, knew) = w.get(&format!("/v1/parties/{p}?known_at={known}"), "staff", "servicing").await;
    assert_eq!(knew["facts"]["family_name"]["value"], "Rangi");
    assert_eq!(before["version"].as_u64().unwrap() + 1, now["version"].as_u64().unwrap());
    // A correction for the same date needs a reason.
    let (status, e) = w
        .command(
            &p,
            "staff",
            json!({ "command": "record", "facts": [{ "field": "family_name", "value": "Rangi-Smyth", "valid_from": "2026-03-01" }] }),
        )
        .await;
    assert_eq!(
        (status, e["error"]["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("reason_required"))
    );
    let (status, _) = w
        .command(
            &p,
            "staff",
            json!({ "command": "record", "reason": "misspelt on the form",
                "facts": [{ "field": "family_name", "value": "Rangi-Smyth", "valid_from": "2026-03-01" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, h) = w.get(&format!("/v1/parties/{p}/history"), "staff", "servicing").await;
    let names: Vec<&Value> = h["statements"].as_array().unwrap().iter().filter(|s| s["field"] == "family_name").collect();
    assert_eq!(names.len(), 3);
    // Underwriting sees identity and contact, with the IRD number masked.
    let (_, uw) = w.get(&format!("/v1/parties/{p}"), "underwriting", "underwriting").await;
    assert!(uw["facts"]["date_of_birth"].is_object());
    assert!(uw["facts"].get("ird_number").is_none());
    assert_eq!(uw["masked"]["ird_number"], "***850");
    // Payments sees names only, and an undeclared purpose sees nothing.
    let (_, pay) = w.get(&format!("/v1/parties/{p}"), "payments", "payee_verification").await;
    assert!(pay["facts"].get("email").is_none() && pay["facts"]["given_name"].is_object());
    assert_eq!(pay["masked"]["email"], "t***@x.nz");
    assert!(pay["withheld"].as_array().unwrap().contains(&json!("date_of_birth")));
    let (status, _) = w.get(&format!("/v1/parties/{p}"), "payments", "curiosity").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = w.get(&format!("/v1/parties/{p}/history"), "payments", "curiosity").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // A declined correction is attached with the person's statement.
    let (status, _) = w
        .command(
            &p,
            "staff",
            json!({ "command": "decline_correction", "field": "date_of_birth", "statement": "I was born on 2 January" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, r) = w.get(&format!("/v1/parties/{p}"), "staff", "servicing").await;
    assert_eq!(r["declined_corrections"][0]["statement"], "I was born on 2 January");
}

/// Purpose: the person themself, through the portal, can read their record and change their
/// contact details and consents, and nothing else; another customer cannot read it at all.
/// Why it matters: relationship-scoped access is what keeps one customer's record from
/// another, and identity facts must change only through staff with evidence.
#[tokio::test]
async fn the_portal_sees_only_its_own_person() {
    let w = world_or_skip!();
    let me = w.person("Ana", "Lee", "1985-07-07", &[]).await;
    let other = w.person("Bo", "Lee", "1986-08-08", &[]).await;
    let as_party = |party: &str, method: &'static str, path: String, body: Option<Value>| Call {
        method,
        path,
        role: "portal",
        purpose: Some("self_service"),
        body,
        headers: vec![("x-ic-party", party.to_string()), ("x-ic-actor", "app-session-1".into())],
    };
    let (status, mine) = w.send(as_party(&me, "GET", format!("/v1/parties/{me}"), None)).await;
    assert_eq!(status, StatusCode::OK, "{mine}");
    let (status, _) = w.send(as_party(&other, "GET", format!("/v1/parties/{me}"), None)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let contact = json!({ "command": "record", "facts": [{ "field": "mobile_phone", "value": "+64 21 555 0101" }] });
    let (status, _) = w.send(as_party(&me, "POST", format!("/v1/parties/{me}/commands"), Some(contact.clone()))).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = w.send(as_party(&other, "POST", format!("/v1/parties/{me}/commands"), Some(contact))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let rename = json!({ "command": "record", "facts": [{ "field": "family_name", "value": "Smith" }] });
    let (status, _) = w.send(as_party(&me, "POST", format!("/v1/parties/{me}/commands"), Some(rename))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let consent = json!({ "command": "set_consent", "purpose": "existing_cover_prefill", "granted": true });
    let (status, _) = w.send(as_party(&me, "POST", format!("/v1/parties/{me}/commands"), Some(consent))).await;
    assert_eq!(status, StatusCode::OK);
    let (_, mine) = w.send(as_party(&me, "GET", format!("/v1/parties/{me}"), None)).await;
    assert_eq!(mine["consents"]["existing_cover_prefill"], true);
    assert_eq!(mine["facts"]["mobile_phone"]["value"], "+64215550101");
    // An access request returns the whole stream.
    let mut export = as_party(&me, "GET", format!("/v1/parties/{me}/export"), None);
    export.purpose = Some("subject_access");
    let (status, all) = w.send(export).await;
    assert_eq!(status, StatusCode::OK);
    assert!(all["streams"][&me].as_array().unwrap().len() >= 4);
    let (status, _) = w.get(&format!("/v1/parties/{me}/export"), "documents", "subject_access").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // A malformed acting party is refused.
    let (status, _) = w.send(as_party("not a party!", "GET", format!("/v1/parties/{me}"), None)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// Purpose: an adviser and an employer administrator see a client or member only through a
/// relationship to a policy or scheme the person is also related to, and lose it when the
/// relationship ends.
/// Why it matters: advisers and employers reach personal data only through relationships,
/// and an ended relationship must take access away.
#[tokio::test]
async fn relationships_scope_advisers_and_employers() {
    let w = world_or_skip!();
    let client = w.person("Kiri", "Hohaia", "1970-03-03", &[("email", json!("k@x.nz"))]).await;
    let adviser = w.person("Sam", "Adviser", "1965-01-01", &[]).await;
    let admin = w.person("Pat", "Payroll", "1980-01-01", &[]).await;
    let add =
        |id: &str, kind: &str, target: Value| json!({ "command": "add_relationship", "id": id, "kind": kind, "target": target, "valid_from": "2026-01-01" });
    for (who, id, kind, target) in [
        (&client, "own-P1", "owner", json!({ "kind": "policy", "id": "P1" })),
        (&adviser, "adv-P1", "adviser", json!({ "kind": "policy", "id": "P1" })),
        (&client, "mem-S1", "scheme_member", json!({ "kind": "scheme", "id": "S1" })),
        (&admin, "adm-S1", "employer_administrator", json!({ "kind": "scheme", "id": "S1" })),
    ] {
        let (status, out) = w.command(who, "staff", add(id, kind, target)).await;
        assert_eq!(status, StatusCode::OK, "{out}");
    }
    // Adding the same relationship again changes nothing; other details conflict.
    let (_, again) = w
        .command(&client, "staff", add("own-P1", "owner", json!({ "kind": "policy", "id": "P1" })))
        .await;
    assert_eq!(again["events"], json!([]));
    let (status, _) = w
        .command(&client, "staff", add("own-P1", "owner", json!({ "kind": "policy", "id": "P2" })))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let read_as = |party: &str, purpose: &'static str| Call {
        method: "GET",
        path: format!("/v1/parties/{client}"),
        role: "portal",
        purpose: Some(purpose),
        body: None,
        headers: vec![("x-ic-party", party.to_string())],
    };
    let (status, seen) = w.send(read_as(&adviser, "advice")).await;
    assert_eq!(status, StatusCode::OK, "{seen}");
    assert!(seen["facts"]["email"].is_object());
    let (_, seen) = w.send(read_as(&admin, "scheme_administration")).await;
    assert!(seen["facts"]["family_name"].is_object() && seen["facts"].get("email").is_none());
    let (status, _) = w.send(read_as(&admin, "advice")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = w.send(read_as("pty_nobody", "advice")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Who is related to the policy, and what the client holds.
    let (status, rels) = w.get("/v1/relationships?target=policy:P1", "portal", "authorisation").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rels["relationships"].as_array().unwrap().len(), 2);
    let (_, of) = w.get(&format!("/v1/parties/{client}/relationships"), "documents", "notices").await;
    assert_eq!(of["relationships"].as_array().unwrap().len(), 2);
    let (status, _) = w.get("/v1/relationships?target=car:1", "portal", "x").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = w.get("/v1/relationships?target=policy:P1", "operations", "x").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = w.get(&format!("/v1/parties/{client}/relationships"), "operations", "x").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // The adviser's engagement ends: access goes with it.
    let today = chrono::Utc::now().date_naive();
    let (status, _) = w
        .command(
            &adviser,
            "staff",
            json!({ "command": "end_relationship", "id": "adv-P1", "valid_to": today.to_string() }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = w.send(read_as(&adviser, "advice")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, e) = w
        .command(
            &adviser,
            "staff",
            json!({ "command": "end_relationship", "id": "adv-P1", "valid_to": (today + chrono::Days::new(1)).to_string() }),
        )
        .await;
    assert_eq!((status, e["error"]["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("ended")));
    // The end went out on the priority channel, with what ended.
    let outbox = w.app.store.outbox(100).await.unwrap();
    let (_, first) = &outbox[0];
    assert!(first.priority);
    assert_eq!(first.kind, "relationship_ended");
    assert_eq!(first.relationship.as_ref().unwrap().id, "adv-P1");
    // Relationships to parties need the other party to exist.
    let (status, _) = w
        .command(&client, "staff", add("att", "attorney", json!({ "kind": "party", "id": "pty_nobody" })))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = w
        .command(&client, "staff", add("att", "attorney", json!({ "kind": "party", "id": adviser })))
        .await;
    assert_eq!(status, StatusCode::OK);
}

/// Purpose: a death is marked once, repeats are harmless, a different date is refused, and a
/// death recorded in error is retracted with a reason; only allowed services may do either.
/// Why it matters: the death marker starts the estate process and restricts the person's
/// policies, so it must be exact, idempotent and reversible only on the record.
#[tokio::test]
async fn deaths_are_marked_and_retracted() {
    let w = world_or_skip!();
    let p = w.person("Hemi", "Parata", "1950-01-01", &[]).await;
    let mark = |d: &str| json!({ "command": "mark_deceased", "date_of_death": d });
    let (status, out) = w.command(&p, "claims", mark("2026-10-01")).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["events"], json!(["asserted", "deceased"]));
    let (_, again) = w.command(&p, "claims", mark("2026-10-01")).await;
    assert_eq!(again["events"], json!([]));
    let (status, _) = w.command(&p, "claims", mark("2026-10-02")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = w.command(&p, "documents", mark("2026-10-01")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (_, r) = w.get(&format!("/v1/parties/{p}"), "claims", "claims").await;
    assert_eq!(r["lifecycle"], "deceased");
    assert_eq!(r["facts"]["date_of_death"]["value"], "2026-10-01");
    let (_, c) = w.get(&format!("/v1/parties/{p}/contact-points?channel=in_app"), "documents", "notices").await;
    assert_eq!(c["deceased"], true);
    let (status, out) = w
        .command(
            &p,
            "claims",
            json!({ "command": "retract_death", "reason": "wrong person on the notification" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let (_, r) = w.get(&format!("/v1/parties/{p}"), "claims", "claims").await;
    assert_eq!(r["lifecycle"], "prospect");
    assert!(r["facts"].get("date_of_death").is_none());
    let (status, _) = w.command(&p, "claims", json!({ "command": "retract_death", "reason": "again" })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    for bad in [mark("2999-01-01"), mark("1900-01-01")] {
        let (status, _) = w.command(&p, "claims", bad).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }
}

/// Purpose: creating someone who may already exist is refused with the candidates, creating
/// them anyway opens a review, and a merge makes the duplicate resolve to the survivor until
/// it is undone.
/// Why it matters: merges are events and the absorbed ID keeps working for ever, so nothing
/// in the core has to change; a wrong merge must be reversible.
#[tokio::test]
async fn duplicates_are_reviewed_merged_and_unmerged() {
    let w = world_or_skip!();
    let a = w.person("Wiremu", "Tane", "1960-06-06", &[("email", json!("w@x.nz"))]).await;
    let facts = json!([
        { "field": "given_name", "value": "Wiremu" },
        { "field": "family_name", "value": "Tane" },
        { "field": "date_of_birth", "value": "1960-06-06" }]);
    let (status, dup) = w.post("/v1/parties", "staff", json!({ "kind": "person", "facts": facts })).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(dup["candidates"], json!([a]));
    let (status, b) = w
        .post("/v1/parties", "staff", json!({ "kind": "person", "facts": facts, "allow_duplicate": true }))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let b = b["party"].as_str().unwrap().to_string();
    let (_, reviews) = w.get("/v1/reviews", "staff", "servicing").await;
    let review = reviews["reviews"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(reviews["reviews"][0]["candidates"], json!([a]));
    let (status, _) = w.get("/v1/reviews", "documents", "notices").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, out) = w.post("/v1/merges", "staff", json!({ "survivor": a, "absorbed": b, "review": review })).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let (status, r) = w.get(&format!("/v1/parties/{b}"), "staff", "servicing").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(r["party"], a);
    assert_eq!(r["asked_for"], b);
    let (status, e) = w.command(&b, "staff", json!({ "command": "activate" })).await;
    assert_eq!((status, e["error"]["code"].as_str()), (StatusCode::CONFLICT, Some("merged")));
    // The merged party is found as the survivor when matching.
    let (_, m) = w
        .post(
            "/v1/parties/match",
            "claims",
            json!({ "source": "claims", "given_name": "Wiremu", "family_name": "Tane", "date_of_birth": "1960-06-06", "email": "w@x.nz" }),
        )
        .await;
    assert_eq!(m, json!({ "outcome": "linked", "party": a, "ignored": [] }));
    for (body, code) in [
        (json!({ "survivor": a, "absorbed": a }), StatusCode::UNPROCESSABLE_ENTITY),
        (json!({ "survivor": a, "absorbed": b }), StatusCode::CONFLICT),
        (json!({ "survivor": a, "absorbed": "pty_none" }), StatusCode::NOT_FOUND),
        (json!({ "survivor": "pty_none", "absorbed": a }), StatusCode::NOT_FOUND),
    ] {
        let (status, _) = w.post("/v1/merges", "staff", body).await;
        assert_eq!(status, code);
    }
    let (status, _) = w.post("/v1/merges", "documents", json!({ "survivor": a, "absorbed": b })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = w.post("/v1/merges", "staff", json!({ "survivor": a })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, out) = w.post("/v1/unmerges", "staff", json!({ "survivor": a, "absorbed": b })).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let (_, r) = w.get(&format!("/v1/parties/{b}"), "staff", "servicing").await;
    assert_eq!(r["party"], b);
    let (status, _) = w.post("/v1/unmerges", "staff", json!({ "survivor": a, "absorbed": b })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    // A review is closed once.
    let (status, _) = w.post(&format!("/v1/reviews/{review}/dismiss"), "staff", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = w.post(&format!("/v1/reviews/{review}/dismiss"), "documents", json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Search finds both by name.
    let (status, s) = w
        .post("/v1/parties/search", "staff", json!({ "given_name": "Wiremu", "family_name": "Tane" }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(s["results"].as_array().unwrap().len(), 2);
    let (status, _) = w.post("/v1/parties/search", "documents", json!({ "family_name": "Tane" })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = w.post("/v1/parties/search", "staff", json!({ "family_name": 3 })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = w.post("/v1/parties/search", "staff", json!({ "family_name": "--" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// Purpose: a dismissed review is closed and leaves both parties as they are.
/// Why it matters: two different people with the same name and birthday are common; saying so
/// must take them off the duplicate list without touching either record.
#[tokio::test]
async fn reviews_can_be_dismissed() {
    let w = world_or_skip!();
    let a = w.person("Jo", "Brown", "1991-01-01", &[]).await;
    let b = w.person("Jo", "Brown", "1991-01-01", &[]).await;
    let (_, reviews) = w.get("/v1/reviews", "staff", "servicing").await;
    let review = reviews["reviews"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(reviews["reviews"][0]["party"], b);
    let (status, _) = w.post(&format!("/v1/reviews/{review}/dismiss"), "staff", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let (_, reviews) = w.get("/v1/reviews", "staff", "servicing").await;
    assert_eq!(reviews["reviews"], json!([]));
    let (status, _) = w.get(&format!("/v1/parties/{a}"), "staff", "servicing").await;
    assert_eq!(status, StatusCode::OK);
}

/// Purpose: commands honour `If-Match` and `Idempotency-Key`, refuse malformed input and
/// unknown parties, and change nothing when refused.
/// Why it matters: optimistic concurrency stops two edits overwriting each other, and a retry
/// after a timeout must apply once.
#[tokio::test]
async fn commands_are_concurrency_safe_and_idempotent() {
    let w = world_or_skip!();
    let p = w.person("Ari", "Ngaio", "2000-01-01", &[]).await;
    let send = |headers: Vec<(&'static str, String)>, body: Value| Call {
        method: "POST",
        path: format!("/v1/parties/{p}/commands"),
        role: "staff",
        purpose: None,
        body: Some(body),
        headers,
    };
    let consent = json!({ "command": "set_consent", "purpose": "marketing", "granted": false });
    let (status, e) = w.send(send(vec![("if-match", "\"1\"".into())], consent.clone())).await;
    assert_eq!((status, e["error"]["code"].as_str()), (StatusCode::CONFLICT, Some("version_conflict")));
    let (status, _) = w.send(send(vec![("if-match", "two".into())], consent.clone())).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, first) = w
        .send(send(vec![("if-match", "2".into()), ("idempotency-key", "k1".into())], consent.clone()))
        .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let (_, second) = w.send(send(vec![("idempotency-key", "k1".into())], consent)).await;
    assert_eq!(first, second);
    let (status, e) = w
        .send(send(
            vec![("idempotency-key", "k1".into())],
            json!({ "command": "set_consent", "purpose": "marketing", "granted": true }),
        ))
        .await;
    assert_eq!((status, e["error"]["code"].as_str()), (StatusCode::CONFLICT, Some("idempotency_conflict")));
    let (status, _) = w.command(&p, "staff", json!({ "command": "fly" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = w.command("pty_nobody", "staff", json!({ "command": "activate" })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = w.command("bad%20id", "staff", json!({ "command": "activate" })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = w.get("/v1/parties/bad%20id", "staff", "servicing").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Creation with an idempotency key returns the first party on a retry.
    let create = |key: &str| Call {
        method: "POST",
        path: "/v1/parties".into(),
        role: "claims",
        purpose: None,
        body: Some(json!({ "kind": "person", "facts": [{ "field": "family_name", "value": "Claimant" }] })),
        headers: vec![("idempotency-key", key.to_string())],
    };
    let (s1, c1) = w.send(create("c1")).await;
    let (s2, c2) = w.send(create("c1")).await;
    assert_eq!((s1, s2), (StatusCode::CREATED, StatusCode::CREATED));
    assert_eq!(c1["party"], c2["party"]);
    let (status, _) = w
        .post(
            "/v1/parties",
            "documents",
            json!({ "kind": "person", "facts": [{ "field": "family_name", "value": "X" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = w.post("/v1/parties", "staff", json!({ "kind": "robot", "facts": [] })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = w
        .post(
            "/v1/parties",
            "staff",
            json!({ "kind": "person", "facts": [{ "field": "given_name", "value": "Only" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

/// Purpose: requests without a valid token, with the wrong audience, or naming a role the
/// token lacks are refused, and a token with several roles must name one.
/// Why it matters: no module trusts another by default; the called module decides, from a
/// verified identity.
#[tokio::test]
async fn callers_must_prove_who_they_are() {
    use tower::ServiceExt;
    let w = world_or_skip!();
    let p = w.person("Rua", "Toa", "1999-09-09", &[]).await;
    let path = format!("/v1/parties/{p}");
    let raw = |headers: Vec<(&str, String)>| {
        let mut req = axum::http::Request::builder().uri(&path);
        for (k, v) in headers {
            req = req.header(k, v);
        }
        req.body(axum::body::Body::empty()).unwrap()
    };
    let status = |req: axum::http::Request<axum::body::Body>| async { w.router.clone().oneshot(req).await.unwrap().status() };
    assert_eq!(status(raw(vec![])).await, StatusCode::UNAUTHORIZED);
    assert_eq!(status(raw(vec![("authorization", "Bearer nonsense".into())])).await, StatusCode::UNAUTHORIZED);
    let wrong_aud = common::token_with(&w.authority, &["staff"], "api://other");
    assert_eq!(
        status(raw(vec![("authorization", format!("Bearer {wrong_aud}"))])).await,
        StatusCode::UNAUTHORIZED
    );
    let two = common::token_with(&w.authority, &["staff", "claims"], common::AUDIENCE);
    let bearer = format!("Bearer {two}");
    assert_eq!(status(raw(vec![("authorization", bearer.clone())])).await, StatusCode::UNAUTHORIZED);
    let ok = raw(vec![
        ("authorization", bearer.clone()),
        ("x-ic-caller", "staff".into()),
        ("x-ic-purpose", "servicing".into()),
    ]);
    assert_eq!(status(ok).await, StatusCode::OK);
    let lacking = raw(vec![("authorization", bearer.clone()), ("x-ic-caller", "privacy".into())]);
    assert_eq!(status(lacking).await, StatusCode::UNAUTHORIZED);
    let none = common::token_with(&w.authority, &[], common::AUDIENCE);
    assert_eq!(status(raw(vec![("authorization", format!("Bearer {none}"))])).await, StatusCode::UNAUTHORIZED);
    let bad_purpose = raw(vec![
        ("authorization", bearer.clone()),
        ("x-ic-caller", "staff".into()),
        ("x-ic-purpose", "a b".into()),
    ]);
    assert_eq!(status(bad_purpose).await, StatusCode::BAD_REQUEST);
    let bad_actor = raw(vec![("authorization", bearer), ("x-ic-caller", "staff".into()), ("x-ic-actor", "a b".into())]);
    assert_eq!(status(bad_actor).await, StatusCode::BAD_REQUEST);
    // Health needs no token; readiness checks the database.
    let req = axum::http::Request::builder().uri("/healthz").body(axum::body::Body::empty()).unwrap();
    assert_eq!(status(req).await, StatusCode::OK);
    let req = axum::http::Request::builder().uri("/readyz").body(axum::body::Body::empty()).unwrap();
    assert_eq!(status(req).await, StatusCode::OK);
}

/// Purpose: the routing question answers whether a request should go to a person first,
/// true once staff record a vulnerability marker, without ever showing the marker; callers
/// the policy does not name are refused.
/// Why it matters: a vulnerable customer's request must reach a trained person, and the
/// marker itself is the most sensitive fact the master holds.
#[tokio::test]
async fn vulnerable_people_are_routed_to_a_person() {
    let w = world_or_skip!();
    let p = w.person("Aroha", "Wiremu", "1940-04-04", &[]).await;
    let path = format!("/v1/parties/{p}/routing");
    let (status, out) = w.get(&path, "portal", "servicing").await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["route_to_person"], false);
    let mark = json!({ "command": "record", "facts": [{ "field": "vulnerability", "value": true }] });
    let (status, _) = w.command(&p, "documents", mark.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, out) = w.command(&p, "staff", mark).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let (_, out) = w.get(&path, "claims", "claims").await;
    assert_eq!(out["route_to_person"], true);
    assert_eq!(out.as_object().unwrap().len(), 2, "only the answer, never the marker: {out}");
    let (_, r) = w.get(&format!("/v1/parties/{p}"), "staff", "servicing").await;
    assert!(r["facts"].get("vulnerability").is_none());
    let (status, _) = w.get(&path, "commission", "commission").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// Purpose: when a query fails inside the database, the relationship, review and audit
/// lists answer 503 rather than an empty list.
/// Why it matters: an empty answer reads as "no relationships" or "nothing to review", which
/// a caller would act on; a fault must look like a fault so the caller retries.
#[tokio::test]
async fn database_faults_are_not_empty_answers() {
    let w = world_or_skip!();
    let p = w.person("Fau", "Lt", "1980-08-08", &[]).await;
    w.sql("alter table relationships rename to relationships_gone").await.unwrap();
    let (status, _) = w.get(&format!("/v1/parties/{p}/relationships"), "staff", "servicing").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let (status, _) = w.get("/v1/relationships?target=policy:P1", "staff", "servicing").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    w.sql("alter table reviews rename to reviews_gone").await.unwrap();
    let (status, _) = w.get("/v1/reviews", "staff", "servicing").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    // Erasing again after a restore: a database fault is an outage, not "nothing to do".
    let party = ic_customers::model::PartyId(p.clone());
    w.app.register.record(&[party], chrono::Utc::now()).await.unwrap();
    w.sql("alter table party_keys rename to party_keys_gone").await.unwrap();
    let (status, _) = w.post("/v1/admin/reshred", "operations", json!({ "since": "2026-01-01" })).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    w.sql("alter table audit rename to audit_gone").await.unwrap();
    let (status, _) = w.get(&format!("/v1/parties/{p}/audit"), "privacy", "audit").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    // A refusal that cannot be logged is an outage too.
    let (status, _) = w.get("/v1/reviews", "documents", "notices").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

/// Purpose: creating a record for someone an IRD number already identifies is refused with
/// the existing party, and an organisation is created with its registration number indexed.
/// Why it matters: a second record for a known person splits their policies and consents
/// across two IDs; the strong identifier is the surest sign.
#[tokio::test]
async fn a_confident_match_refuses_a_second_record() {
    let w = world_or_skip!();
    let p = w.person("Tama", "Hohepa", "1975-05-05", &[("ird_number", json!("49091850"))]).await;
    let again = json!({ "kind": "person", "facts": [
        { "field": "given_name", "value": "Tama" },
        { "field": "family_name", "value": "Hohepa" },
        { "field": "date_of_birth", "value": "1975-05-05" },
        { "field": "ird_number", "value": "49091850" },
    ] });
    let (status, out) = w.post("/v1/parties", "staff", again).await;
    assert_eq!(status, StatusCode::CONFLICT, "{out}");
    assert_eq!(out["candidates"], json!([p]));
    let org = json!({ "kind": "organisation", "facts": [
        { "field": "organisation_name", "value": "Kauri Holdings Limited" },
        { "field": "registration_number", "value": "nz1234567" },
    ] });
    let (status, out) = w.post("/v1/parties", "staff", org).await;
    assert_eq!(status, StatusCode::CREATED, "{out}");
}

/// Purpose: a break-glass read is served and its audit line is marked as break-glass.
/// Why it matters: in an emergency staff must reach a record the policy would otherwise
/// refuse, and every such read must stand out for review.
#[tokio::test]
async fn break_glass_reads_are_marked() {
    let w = world_or_skip!();
    let p = w.person("Ara", "Moana", "1960-06-06", &[]).await;
    let (status, out) = w.get(&format!("/v1/parties/{p}"), "break-glass", "break_glass").await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let (_, trail) = w.get(&format!("/v1/parties/{p}/audit"), "privacy", "audit").await;
    let lines = trail["audit"].as_array().unwrap();
    assert!(lines.iter().any(|l| l["line"]["break_glass"] == true), "{trail}");
}

/// Purpose: when Entra ID's signing keys cannot be fetched, calls are answered 503, not 401.
/// Why it matters: a 401 tells a caller its credentials are wrong and it should stop; an
/// outage at the identity provider must read as retryable.
#[tokio::test]
async fn an_identity_outage_is_retryable() {
    let w = world_or_skip!();
    let pool = ic_customers::db::open(w.db.connector.clone(), 1).await.unwrap();
    let credential = ic_azure::credential::AzureCredential::static_token(ic_azure::testing::TOKEN);
    let app = std::sync::Arc::new(ic_customers::service::App {
        store: ic_customers::store::Store::new(
            pool,
            ic_customers::keys::KeyRing::new(ic_azure::keywrap::KeyVaultWrapper::new(&w.azure.base, "customers-kek", credential)),
            ic_customers::crypto::IndexKey::new(common::INDEX_KEY.as_bytes()).unwrap(),
        ),
        access: ic_customers::access::Access::shipped(),
        auth: ic_customers::auth::Entra::new("http://127.0.0.1:9", common::TENANT, common::AUDIENCE),
        register: ic_customers::retention::Register::new(ic_azure::blob::MemoryWormStore::default()),
    });
    let elsewhere = common::World {
        router: ic_customers::service::router(app.clone()),
        app,
        azure: ic_azure::testing::FakeAzure::start().await,
        authority: w.authority.clone(),
        db: common::Database::new(common::server().unwrap()).await,
    };
    let (status, _) = elsewhere.get("/v1/reviews", "staff", "servicing").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}
