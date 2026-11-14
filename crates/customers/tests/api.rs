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
        .command(&client, "staff", add("att", "partner", json!({ "kind": "party", "id": "pty_nobody" })))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = w
        .command(&client, "staff", add("att", "partner", json!({ "kind": "party", "id": adviser })))
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

/// Purpose: a vulnerability marker records a kind, a review date and adjustments, never a
/// diagnosis; the routing question then answers "go to a person first" with the
/// adjustments for anyone serving the person, the kind only for trained support staff, and
/// the person sees their own marker in the app; other readers see no trace of it, not even
/// a withheld field; only staff may set or remove it.
/// Why it matters: a vulnerable customer's request must reach a trained person who knows
/// what helps (matrix VUL-001, VUL-002), and the marker is the most sensitive fact the
/// master holds: a label on a list, or a field marked as hidden, tells everyone it exists.
#[tokio::test]
async fn vulnerable_people_are_routed_to_a_person() {
    let w = world_or_skip!();
    let p = w.person("Aroha", "Wiremu", "1940-04-04", &[]).await;
    let path = format!("/v1/parties/{p}/routing");
    let (status, out) = w.get(&path, "portal", "servicing").await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out, json!({ "party": p, "route_to_person": false }));
    let marker = json!({ "kind": "bereavement", "review_on": "2027-05-01", "adjustments": ["more_time", "speak_to_trained_person"] });
    let mark = json!({ "command": "record", "facts": [{ "field": "vulnerability", "value": marker }] });
    let (status, _) = w.command(&p, "documents", mark.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let with_diagnosis = json!({ "command": "record", "facts": [{ "field": "vulnerability",
        "value": { "kind": "health", "review_on": "2027-05-01", "adjustments": [], "diagnosis": "x" } }] });
    let (status, _) = w.command(&p, "staff", with_diagnosis).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "no diagnosis can be recorded");
    let (status, out) = w.command(&p, "staff", mark).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    // Anyone serving her: go to a person, and what helps. Not the kind.
    let (_, out) = w.get(&path, "claims", "claims").await;
    assert_eq!(
        out,
        json!({ "party": p, "route_to_person": true, "review_on": "2027-05-01",
                "adjustments": ["more_time", "speak_to_trained_person"] })
    );
    // Trained support staff: the kind too.
    let (_, out) = w.get(&path, "staff", "vulnerability_support").await;
    assert_eq!(out["kind"], "bereavement");
    // An ordinary read shows no trace of it.
    let (_, r) = w.get(&format!("/v1/parties/{p}"), "staff", "servicing").await;
    assert!(!r.to_string().contains("vulnerability"), "{r}");
    let (_, r) = w.get(&format!("/v1/parties/{p}"), "staff", "vulnerability_support").await;
    assert_eq!(r["facts"]["vulnerability"]["value"], marker);
    // She sees her own marker in the app.
    let mine = Call {
        method: "GET",
        path: format!("/v1/parties/{p}?purpose=self_service"),
        role: "portal",
        purpose: None,
        body: None,
        headers: vec![("x-ic-party", p.clone())],
    };
    let (status, r) = w.send(mine).await;
    assert_eq!(status, StatusCode::OK, "{r}");
    assert_eq!(r["facts"]["vulnerability"]["value"], marker);
    // Removing it is a statement like any other; the history keeps what was.
    let remove = json!({ "command": "record", "reason": "reviewed with her: no longer needed",
        "facts": [{ "field": "vulnerability", "value": false }] });
    let (status, out) = w.command(&p, "staff", remove).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let (_, out) = w.get(&path, "claims", "claims").await;
    assert_eq!(out["route_to_person"], false);
    let (_, h) = w.get(&format!("/v1/parties/{p}/history"), "staff", "vulnerability_support").await;
    assert_eq!(h["statements"].as_array().unwrap().iter().filter(|s| s["field"] == "vulnerability").count(), 2);
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

/// Purpose: a family member's worry about a customer is recorded as a third-party concern,
/// who raised it and what about and nothing more; the answer to the staff member says
/// nothing about the person; and it opens a prompt for a trained person to consider a
/// marker, which can be dismissed but never merged. Values outside the lists are refused.
/// Why it matters: a daughter worried about her father (matrix VUL-006), or children asking
/// to limit their parents' withdrawals (VUL-004), have no authority over the policy; the
/// insurer must listen without disclosing anything, and a person, not a rule, decides on a
/// marker.
#[tokio::test]
async fn a_third_party_concern_opens_a_prompt_and_discloses_nothing() {
    let w = world_or_skip!();
    let p = w.person("Hone", "Rakau", "1938-03-03", &[]).await;
    let concern = json!({ "command": "record_concern", "from": "family", "about": "financial_abuse" });
    let (status, out) = w.command(&p, "staff", concern.clone()).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["events"], json!(["concern_raised"]));
    assert_eq!(out.as_object().unwrap().len(), 3, "party, version and events only: {out}");
    let (status, _) = w
        .command(&p, "claims", json!({ "command": "record_concern", "from": "carer", "about": "capacity" }))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = w.command(&p, "documents", concern).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    for bad in [
        json!({ "command": "record_concern", "from": "neighbour's cousin", "about": "wellbeing" }),
        json!({ "command": "record_concern", "from": "family", "about": "he has dementia" }),
    ] {
        let (status, _) = w.command(&p, "staff", bad).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }
    // The children have no authority to limit their parent's withdrawals.
    let child = w.person("Rangi", "Child", "1975-01-01", &[]).await;
    let (_, out) = w
        .get(
            &format!("/v1/parties/{p}/authority?actor={child}&act=surrender"),
            "policy-servicing",
            "servicing",
        )
        .await;
    assert_eq!(
        (out["decision"].clone(), out["reason"].clone()),
        (json!("refuse"), json!("no_authority")),
        "{out}"
    );
    let (_, r) = w.get("/v1/reviews", "staff", "servicing").await;
    let prompts: Vec<&Value> = r["reviews"].as_array().unwrap().iter().filter(|r| r["kind"] == "consider_marker").collect();
    assert_eq!(prompts.len(), 2, "{r}");
    assert_eq!(prompts[0]["party"], p);
    assert_eq!(prompts[0]["reason"], "financial_abuse");
    let id = prompts[0]["id"].as_str().unwrap();
    let other = w.person("Hone", "Rakau", "1938-03-03", &[]).await;
    let (status, _) = w.post("/v1/merges", "staff", json!({ "survivor": p, "absorbed": other, "review": id })).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "a prompt is not a duplicate review");
    let (status, _) = w.post(&format!("/v1/reviews/{id}/dismiss"), "staff", json!({})).await;
    assert_eq!(status, StatusCode::OK);
}

/// Purpose: a representative (an attorney under an activated enduring power, a welfare
/// guardian under a court order, someone holding a written authority) is a relationship
/// that needs its basis, document and scope; the authority question then allows an act
/// within scope while the authority is in force, refuses before it starts, after it ends,
/// outside its scope or with no authority at all, and routes to a person an act that would
/// benefit the representative; each answer names the actor in the audit.
/// Why it matters: an attorney acting the day before activation (matrix VUL-009), a
/// partner limited to paying premiums changing the bank account (VUL-013), or an attorney
/// making herself the beneficiary (VUL-010) must each be stopped, while authority properly
/// given works for its dated period (VUL-008, VUL-014).
#[tokio::test]
async fn representatives_act_only_within_their_authority() {
    let w = world_or_skip!();
    let donor = w.person("Beverley", "Ngata", "1941-02-02", &[]).await;
    let attorney = w.person("Fiona", "Ngata", "1970-07-07", &[]).await;
    let partner = w.person("Sam", "Ngata", "1969-06-06", &[]).await;
    let stranger = w.person("Kim", "Other", "1980-01-01", &[]).await;
    let add = |id: &str, kind: &str, from: &str, to: Option<&str>, scope: Value, basis: &str| {
        json!({ "command": "add_relationship", "id": id, "kind": kind,
                "target": { "kind": "party", "id": donor }, "valid_from": from, "valid_to": to,
                "attributes": { "basis": basis, "document": "doc_123", "scope": scope } })
    };
    // Each representative needs a basis, a document and a scope from the lists.
    for bad in [
        json!({ "command": "add_relationship", "id": "r0", "kind": "attorney", "target": { "kind": "party", "id": donor }, "valid_from": "2026-03-01" }),
        add("r0", "attorney", "2026-03-01", None, json!(["everything"]), "enduring_power_of_attorney"),
        add("r0", "attorney", "2026-03-01", None, json!("view"), "enduring_power_of_attorney"),
        add("r0", "attorney", "2026-03-01", None, json!(["view"]), "a handshake"),
        json!({ "command": "add_relationship", "id": "r0", "kind": "attorney",
                "target": { "kind": "party", "id": donor }, "valid_from": "2026-03-01",
                "attributes": { "basis": "enduring_power_of_attorney", "scope": ["view"] } }),
    ] {
        let (status, _) = w.command(&attorney, "staff", bad).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }
    let epa = add(
        "epa-1",
        "attorney",
        "2026-03-01",
        None,
        json!(["view", "change_payment_method", "change_beneficiary"]),
        "enduring_power_of_attorney",
    );
    let (status, out) = w.command(&attorney, "staff", epa).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let limited = add(
        "auth-1",
        "authorised_representative",
        "2026-01-01",
        Some("2026-07-01"),
        json!(["pay_premiums"]),
        "written_authority",
    );
    let (status, out) = w.command(&partner, "staff", limited).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let ask =
        |actor: &str, act: &str, on: &str, benefits: bool| format!("/v1/parties/{donor}/authority?actor={actor}&act={act}&on={on}&benefits_actor={benefits}");
    let world = &w;
    let decide = |path: String| async move {
        let (status, out) = world.get(&path, "policy-servicing", "servicing").await;
        assert_eq!(status, StatusCode::OK, "{out}");
        (out["decision"].as_str().unwrap().to_string(), out["reason"].as_str().unwrap().to_string(), out)
    };
    // The day before activation: refused. From activation: within scope.
    assert_eq!(decide(ask(&attorney, "change_payment_method", "2026-02-28", false)).await.1, "no_authority");
    let (d, _, out) = decide(ask(&attorney, "change_payment_method", "2026-03-01", false)).await;
    assert_eq!(d, "allow");
    assert_eq!(out["relationship"]["basis"], "enduring_power_of_attorney");
    assert_eq!(out["relationship"]["document"], "doc_123");
    // Making herself the beneficiary: to a person first.
    assert_eq!(decide(ask(&attorney, "change_beneficiary", "2026-04-01", true)).await.0, "route");
    assert_eq!(decide(ask(&attorney, "change_beneficiary", "2026-04-01", false)).await.0, "allow");
    assert_eq!(decide(ask(&attorney, "surrender", "2026-04-01", false)).await.1, "outside_scope");
    // Limited to premiums, for its dated period.
    assert_eq!(decide(ask(&partner, "pay_premiums", "2026-06-30", false)).await.0, "allow");
    assert_eq!(decide(ask(&partner, "change_bank_details", "2026-06-30", false)).await.1, "outside_scope");
    assert_eq!(decide(ask(&partner, "change_cover", "2026-06-30", false)).await.0, "refuse");
    assert_eq!(decide(ask(&partner, "pay_premiums", "2026-07-01", false)).await.1, "no_authority");
    assert_eq!(decide(ask(&stranger, "view", "2026-04-01", false)).await.1, "no_authority");
    // Malformed questions and callers the policy does not name.
    for bad in [
        ask(&attorney, "all", "2026-04-01", false),
        ask(&attorney, "fly", "2026-04-01", false),
        ask("n%2Fbody", "view", "2026-04-01", false),
    ] {
        let (status, _) = w.get(&bad, "policy-servicing", "servicing").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let (status, _) = w.get(&ask(&attorney, "view", "2026-04-01", false), "documents", "notices").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Every answer is in the audit, with the actor.
    let (_, trail) = w.get(&format!("/v1/parties/{donor}/audit"), "privacy", "audit").await;
    let lines = trail["audit"].to_string();
    assert!(lines.contains(&format!("authority:{attorney}:change_beneficiary:route")), "{lines}");
    // Both of the attorney's requests are on record with her as the actor.
    let (_, report) = w.get(&format!("/v1/actors/{attorney}/actions"), "privacy", "audit").await;
    let actions = report.to_string();
    assert!(actions.contains(&format!("authority:{attorney}:change_payment_method:allow")), "{report}");
    assert!(actions.contains(&format!("authority:{attorney}:change_beneficiary:route")), "{report}");
}

/// Purpose: a death opens a prompt for a person to consider a bereavement marker for
/// whoever is left (the other owner of a joint policy, and a partner recorded either way),
/// never a marker itself; an ended co-ownership, an unrelated owner and the person who
/// died get no prompt, and a second death record does not open a second prompt while one
/// is open. Persona row VUL-015.
/// Why it matters: a bereaved joint owner is often vulnerable at exactly the moment the
/// insurer contacts them about the claim, and a marker set without a person's judgement
/// would label someone who never asked for it.
#[tokio::test]
async fn a_death_prompts_a_person_to_consider_the_bereaved() {
    let w = world_or_skip!();
    let died = w.person("Wiremu", "Kingi", "1950-01-01", &[]).await;
    let joint = w.person("Mere", "Kingi", "1952-02-02", &[]).await;
    let partner = w.person("Aroha", "Kingi", "1955-03-03", &[]).await;
    let former = w.person("Rangi", "Kingi", "1951-04-04", &[]).await;
    let stranger = w.person("Sam", "Other", "1960-05-05", &[]).await;
    let own = |id: &str, policy: &str, to: Option<&str>| {
        json!({ "command": "add_relationship", "id": id, "kind": "owner", "target": { "kind": "policy", "id": policy },
                "valid_from": "2020-01-01", "valid_to": to })
    };
    for (who, rel) in [
        (&died, own("o1", "JP1", None)),
        (&joint, own("o2", "JP1", None)),
        (&former, own("o3", "JP1", Some("2021-01-01"))),
        (&stranger, own("o4", "P9", None)),
        (
            &partner,
            json!({ "command": "add_relationship", "id": "pt", "kind": "partner",
                    "target": { "kind": "party", "id": died }, "valid_from": "2020-01-01" }),
        ),
    ] {
        let (status, out) = w.command(who, "staff", rel).await;
        assert_eq!(status, StatusCode::OK, "{out}");
    }
    let (status, out) = w
        .command(&died, "claims", json!({ "command": "mark_deceased", "date_of_death": "2026-10-01" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let prompted = || async {
        let (_, r) = w.get("/v1/reviews", "staff", "servicing").await;
        let mut out: Vec<(String, String)> = r["reviews"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["kind"] == "consider_marker")
            .map(|r| (r["party"].as_str().unwrap().to_string(), r["reason"].as_str().unwrap().to_string()))
            .collect();
        out.sort();
        out
    };
    let mut expected = vec![(joint.clone(), "bereavement".to_string()), (partner.clone(), "bereavement".to_string())];
    expected.sort();
    assert_eq!(prompted().await, expected);
    // No marker is set by the prompt.
    let (_, record) = w.get(&format!("/v1/parties/{joint}"), "staff", "vulnerability_support").await;
    assert!(record["facts"].get("vulnerability").is_none(), "{record}");
    // Withdrawn and recorded again: still one prompt each while it is open.
    let (status, _) = w.command(&died, "claims", json!({ "command": "retract_death", "reason": "wrong date" })).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = w
        .command(&died, "claims", json!({ "command": "mark_deceased", "date_of_death": "2026-10-02" }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(prompted().await, expected);
}

/// Purpose: when someone questions whether a person can make their own decisions, every
/// representative's act is routed to a person (reason `capacity_in_question`) whatever
/// their authority says, before or after it is activated, while someone with no
/// authority is still refused; other services go on reading the record; only legal
/// settles the question, with an outcome from the list, and then authority applies as
/// before. Persona row VUL-018.
/// Why it matters: an attorney acting on a power whose trigger is disputed, or before
/// legal has looked, can move money the person never agreed to; stopping cover or claims
/// while the question is open would harm the person instead.
#[tokio::test]
async fn a_capacity_question_pauses_representatives() {
    let w = world_or_skip!();
    let donor = w.person("Beverley", "Ngata", "1941-02-02", &[]).await;
    let attorney = w.person("Fiona", "Ngata", "1970-07-07", &[]).await;
    let stranger = w.person("Kim", "Other", "1980-01-01", &[]).await;
    let epa = json!({ "command": "add_relationship", "id": "epa", "kind": "attorney",
        "target": { "kind": "party", "id": donor }, "valid_from": "2026-03-01",
        "attributes": { "basis": "enduring_power_of_attorney", "document": "doc_9", "scope": ["change_payment_method"] } });
    let (status, out) = w.command(&attorney, "staff", epa).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let decide = |actor: String, on: &'static str| {
        let w = &w;
        let path = format!("/v1/parties/{donor}/authority?actor={actor}&act=change_payment_method&on={on}");
        async move {
            let (status, out) = w.get(&path, "policy-servicing", "servicing").await;
            assert_eq!(status, StatusCode::OK, "{out}");
            (out["decision"].as_str().unwrap().to_string(), out["reason"].as_str().unwrap().to_string())
        }
    };
    assert_eq!(decide(attorney.clone(), "2026-04-01").await, ("allow".into(), "within_scope".into()));
    // Documents cannot raise it; staff can, once.
    let (status, _) = w.command(&donor, "documents", json!({ "command": "question_capacity" })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, out) = w.command(&donor, "staff", json!({ "command": "question_capacity" })).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(out["events"], json!(["capacity_questioned"]));
    let (_, out) = w.command(&donor, "claims", json!({ "command": "question_capacity" })).await;
    assert_eq!(out["events"], json!([]), "asked again, nothing new");
    for on in ["2026-02-01", "2026-04-01"] {
        assert_eq!(decide(attorney.clone(), on).await, ("route".into(), "capacity_in_question".into()), "{on}");
    }
    assert_eq!(decide(stranger.clone(), "2026-04-01").await, ("refuse".into(), "no_authority".into()));
    // Cover and claims go on: documents still reach the person.
    let (status, _) = w
        .get(
            &format!("/v1/parties/{donor}/contact-points?purpose=notices&channel=post"),
            "documents",
            "notices",
        )
        .await;
    assert_ne!(status, StatusCode::FORBIDDEN);
    // Only legal settles it, with an outcome from the list.
    let (status, _) = w
        .command(&donor, "staff", json!({ "command": "settle_capacity", "outcome": "lacks_capacity" }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = w.command(&donor, "legal", json!({ "command": "settle_capacity", "outcome": "unsure" })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, out) = w
        .command(&donor, "legal", json!({ "command": "settle_capacity", "outcome": "lacks_capacity" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(decide(attorney.clone(), "2026-04-01").await, ("allow".into(), "within_scope".into()));
    let (status, _) = w
        .command(&donor, "legal", json!({ "command": "settle_capacity", "outcome": "has_capacity" }))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "nothing open to settle");
}

/// Purpose: a welfare guardian's actions under a court order are limited to its scope,
/// and privacy can produce a dated report of everything the guardian did as the actor
/// and every authority question asked about them, across the people they act for; no
/// one else can read it. Persona row VUL-011.
/// Why it matters: a guardian must be able to account to the court and the family for
/// every action taken, and the insurer must be able to show it acted only within the
/// order.
#[tokio::test]
async fn a_guardian_gets_a_report_of_their_actions() {
    let w = world_or_skip!();
    let michael = w.person("Michael", "Hohaia", "1990-01-01", &[]).await;
    let guardian = w.person("Huia", "Hohaia", "1962-02-02", &[]).await;
    let order = json!({ "command": "add_relationship", "id": "wg", "kind": "guardian",
        "target": { "kind": "party", "id": michael }, "valid_from": "2026-01-01",
        "attributes": { "basis": "court_order", "document": "doc_order", "scope": ["view", "change_contact_details"] } });
    let (status, out) = w.command(&guardian, "staff", order).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    // A change she sends as the actor, through servicing.
    let (status, out) = w
        .send(Call {
            method: "POST",
            path: format!("/v1/parties/{michael}/commands"),
            role: "staff",
            purpose: None,
            body: Some(json!({ "command": "record", "facts": [{ "field": "email", "value": "michael@example.nz" }] })),
            headers: vec![("x-ic-actor", guardian.clone())],
        })
        .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    // Within the order's scope, and outside it.
    for (act, decision) in [("change_contact_details", "allow"), ("surrender", "refuse")] {
        let (_, out) = w
            .get(
                &format!("/v1/parties/{michael}/authority?actor={guardian}&act={act}&on=2026-06-01"),
                "policy-servicing",
                "servicing",
            )
            .await;
        assert_eq!(out["decision"], decision, "{act}");
    }
    let (status, report) = w.get(&format!("/v1/actors/{guardian}/actions"), "privacy", "audit").await;
    assert_eq!(status, StatusCode::OK, "{report}");
    let actions: Vec<String> = report["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["line"]["action"].as_str().unwrap().to_string())
        .collect();
    assert!(actions.contains(&"record".to_string()), "{actions:?}");
    assert!(actions.contains(&format!("authority:{guardian}:change_contact_details:allow")), "{actions:?}");
    assert!(actions.contains(&format!("authority:{guardian}:surrender:refuse")), "{actions:?}");
    assert!(report["actions"].as_array().unwrap().iter().all(|a| a["at"].is_string()), "every line is dated");
    let (status, _) = w.get(&format!("/v1/actors/{guardian}/actions"), "staff", "audit").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = w.get("/v1/actors/n%2Fbody/actions", "privacy", "audit").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Purpose: gender is kept as the person tells it, in any words (here "takatāpui" and
/// "non-binary"), apart from anything used to rate cover: it is read back exactly, a
/// change is history like any other fact, an over-long value is refused, and it plays
/// no part in finding or matching the person. Decision 1 in docs/decisions.md
/// (persona row PRD-011, whose rating side is underwriting's).
/// Why it matters: nobody should have to pick a category that is not theirs to be
/// insured, and a gender recorded for respect must never leak into price or identity
/// matching.
#[tokio::test]
async fn gender_is_recorded_in_the_persons_words_and_never_matched() {
    let w = world_or_skip!();
    let p = w.person("Tama", "Rewi", "1988-08-08", &[("gender", json!("takatāpui"))]).await;
    let (status, record) = w.get(&format!("/v1/parties/{p}"), "staff", "servicing").await;
    assert_eq!(status, StatusCode::OK, "{record}");
    assert_eq!(record["facts"]["gender"]["value"], "takatāpui");
    let id = ic_customers::model::PartyId(p.clone());
    let before = ic_customers::matching::party_keys(&w.app.store.load(&id).await.unwrap().unwrap(), w.app.store.index_key());
    let (status, out) = w
        .command(
            &p,
            "staff",
            json!({ "command": "record", "facts": [{ "field": "gender", "value": "non-binary" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let after = ic_customers::matching::party_keys(&w.app.store.load(&id).await.unwrap().unwrap(), w.app.store.index_key());
    assert_eq!(before, after, "gender is not an index or matching key");
    let (_, record) = w.get(&format!("/v1/parties/{p}"), "staff", "servicing").await;
    assert_eq!(record["facts"]["gender"]["value"], "non-binary");
    let (status, _) = w
        .command(
            &p,
            "staff",
            json!({ "command": "record", "facts": [{ "field": "gender", "value": "x".repeat(101) }] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

/// Purpose: due diligence on a new owner is complete only when its own identity check,
/// screening and a current risk rating have passed, and, for a trust, the same for every
/// trustee in force; a referred screening sends it to a specialist; the signing rule is
/// kept on the ownership relationship. Persona rows JNT-031 (a new trust with three
/// trustees) and JNT-030 (a new owner overseas).
/// Why it matters: issuing to, or transferring ownership to, someone not yet checked is
/// how a policy is used to move money or taken from a vulnerable owner; the issuing side
/// must be able to ask one question and get every gap back.
#[tokio::test]
async fn due_diligence_covers_an_owner_and_every_trustee() {
    let w = world_or_skip!();
    let (status, trust) = w
        .post(
            "/v1/parties",
            "staff",
            json!({ "kind": "organisation", "facts": [
            { "field": "organisation_name", "value": "Ngata Whanau Trust" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{trust}");
    let trust = trust["party"].as_str().unwrap().to_string();
    let mut trustees = vec![];
    for (given, dob) in [("Aroha", "1960-01-01"), ("Hemi", "1962-02-02"), ("Mere", "1965-03-03")] {
        let t = w.person(given, "Ngata", dob, &[]).await;
        let (status, out) = w
            .command(
                &t,
                "staff",
                json!({ "command": "add_relationship", "id": format!("tr-{given}"), "kind": "trustee",
                "target": { "kind": "party", "id": trust }, "valid_from": "2026-01-01" }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{out}");
        trustees.push(t);
    }
    let check = || async {
        let (status, out) = w.get(&format!("/v1/parties/{trust}/due-diligence"), "underwriting", "new_business").await;
        assert_eq!(status, StatusCode::OK, "{out}");
        out
    };
    let first = check().await;
    assert_eq!(first["complete"], false);
    assert_eq!(first["open"].as_array().unwrap().len(), 4 * 3, "the trust and each trustee lack all three");
    let verify = |method: &str, outcome: &str, n: usize| {
        json!({ "command": "record_verification", "method": method, "provider": "idv", "outcome": outcome,
                "evidence_ref": format!("ev-{method}-{n}"), "verified_at": chrono::Utc::now() - chrono::Duration::minutes(10 - n as i64) })
    };
    let rate = json!({ "command": "set_risk_rating", "rating": "low", "review_by": "2030-01-01" });
    for (n, p) in std::iter::once(&trust).chain(trustees.iter()).enumerate() {
        w.command(p, "identity", verify("document_check", "passed", n)).await;
        let screening = if n == 3 { "referred" } else { "passed" };
        let (status, out) = w.command(p, "compliance", verify("screening", screening, n)).await;
        assert_eq!(status, StatusCode::OK, "{out}");
        let (status, out) = w.command(p, "compliance", rate.clone()).await;
        assert_eq!(status, StatusCode::OK, "{out}");
    }
    let referred = check().await;
    assert_eq!(referred["complete"], false);
    assert_eq!(referred["to_specialist"], true);
    assert_eq!(referred["open"], json!([format!("trustee:{}:screening_referred", trustees[2])]));
    // The specialist clears it; a later passed screening completes the trust.
    w.command(&trustees[2], "compliance", verify("screening", "passed", 9)).await;
    let done = check().await;
    assert_eq!((done["complete"].clone(), done["to_specialist"].clone()), (json!(true), json!(false)), "{done}");
    // The trust owns the policy under a signing rule kept on the ownership.
    let (status, out) = w
        .command(
            &trust,
            "staff",
            json!({ "command": "add_relationship", "id": "own", "kind": "owner",
            "target": { "kind": "policy", "id": "TP1" }, "valid_from": "2026-02-01",
            "attributes": { "signing_rule": "any_two_trustees" } }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let (_, rels) = w.get(&format!("/v1/parties/{trust}/relationships"), "staff", "servicing").await;
    assert!(rels.to_string().contains("any_two_trustees"), "{rels}");
    // A new owner overseas, unchecked: nothing complete; documents may not ask.
    let newcomer = w.person("Joe", "Smith", "1985-05-05", &[]).await;
    let (_, out) = w.get(&format!("/v1/parties/{newcomer}/due-diligence"), "policy-servicing", "servicing").await;
    assert_eq!(out["open"], json!(["identity", "screening", "risk_rating"]));
    let (status, _) = w.get(&format!("/v1/parties/{newcomer}/due-diligence"), "documents", "notices").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// Purpose: changes to a trust's trustees are dated on the resolution: one who resigned
/// stops being a trustee from that date and one appointed starts from it, so the trust's
/// due diligence lists only the trustees in force, each with their own checks (a trustee
/// abroad verified remotely counts like any other). A request signed by the resigned
/// trustee the next day finds them absent. Persona rows JNT-023 and JNT-025 (a partner
/// joining as owner with due diligence).
/// Why it matters: a trustee who has resigned has no power to bind the trust, and an
/// appointment not dated on its resolution would let the wrong person sign, or leave the
/// right one unable to.
#[tokio::test]
async fn trustee_changes_are_dated_on_the_resolution() {
    let w = world_or_skip!();
    let (_, trust) = w
        .post(
            "/v1/parties",
            "staff",
            json!({ "kind": "organisation", "facts": [
            { "field": "organisation_name", "value": "Rewi Family Trust" }] }),
        )
        .await;
    let trust = trust["party"].as_str().unwrap().to_string();
    let resigned = w.person("Pita", "Rewi", "1950-01-01", &[]).await;
    let stays = w.person("Ani", "Rewi", "1955-01-01", &[]).await;
    let appointed = w.person("Tui", "Rewi", "1980-01-01", &[]).await;
    let trustee = |id: &str, from: &str, to: Option<&str>| {
        json!({ "command": "add_relationship", "id": id, "kind": "trustee", "target": { "kind": "party", "id": trust },
                "valid_from": from, "valid_to": to })
    };
    w.command(&resigned, "staff", trustee("t1", "2020-01-01", None)).await;
    w.command(&stays, "staff", trustee("t2", "2020-01-01", None)).await;
    // The resolution of 1 March 2026: one resigns, one is appointed (in Sydney, verified remotely).
    let (status, out) = w
        .command(
            &resigned,
            "staff",
            json!({ "command": "end_relationship", "id": "t1", "valid_to": "2026-03-01" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    w.command(&appointed, "staff", trustee("t3", "2026-03-01", None)).await;
    w.command(
        &appointed,
        "identity",
        json!({ "command": "record_verification", "method": "remote_video", "provider": "idv", "outcome": "passed",
                "evidence_ref": "ev-syd", "verified_at": chrono::Utc::now() }),
    )
    .await;
    let (_, dd) = w.get(&format!("/v1/parties/{trust}/due-diligence"), "policy-servicing", "servicing").await;
    let mut in_force: Vec<String> = dd["connected"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["party"].as_str().unwrap().to_string())
        .collect();
    in_force.sort();
    let mut expected = vec![stays.clone(), appointed.clone()];
    expected.sort();
    assert_eq!(in_force, expected, "the resigned trustee is not among those who may sign");
    let tui = dd["connected"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["party"] == appointed.as_str())
        .unwrap()
        .clone();
    assert_eq!(tui["open"], json!(["screening", "risk_rating"]), "the remote check counts as an identity check");
    // The history keeps both, dated.
    let (_, rels) = w.get(&format!("/v1/relationships?target=party:{trust}"), "staff", "servicing").await;
    let text = rels.to_string();
    assert!(text.contains("2026-03-01") && text.contains(&resigned), "{rels}");
}

/// Purpose: asked about a particular act, routing tells whoever serves a marked person to
/// pause a value-moving act (a surrender, a beneficiary or bank change) for a conversation
/// where the person asked for one, and to have a specialist review it first where the
/// marker is a risk of financial abuse, without saying which; other acts, and unmarked
/// people, are not paused; an act that is not one of the known ones is refused. Persona
/// rows VUL-003 (large surrender paused), VUL-005 (full surrender to a new overseas
/// account) and VUL-007 (beneficiary changed to a tradesperson), the master's part.
/// Why it matters: the moment money leaves is when a person at risk is most easily
/// exploited, and the people serving them need to know to slow down without being told
/// the person's circumstances.
#[tokio::test]
async fn risky_acts_by_a_marked_person_are_paused_or_reviewed() {
    let w = world_or_skip!();
    let mark = |kind: &str, adjustments: Value| {
        json!({ "command": "record", "facts": [{ "field": "vulnerability",
            "value": { "kind": kind, "review_on": "2027-05-01", "adjustments": adjustments } }] })
    };
    let asked = w.person("Grace", "Tane", "1938-01-01", &[]).await;
    w.command(&asked, "staff", mark("resilience", json!(["pause_before_large_withdrawal"]))).await;
    let at_risk = w.person("Harold", "Smith", "1935-01-01", &[]).await;
    w.command(&at_risk, "staff", mark("financial_abuse_risk", json!(["speak_to_trained_person"])))
        .await;
    let plain = w.person("Ivy", "Brown", "1980-01-01", &[]).await;
    let route = |p: &str, act: &str| {
        let path = format!("/v1/parties/{p}/routing?act={act}");
        let w = &w;
        async move {
            let (status, out) = w.get(&path, "policy-servicing", "servicing").await;
            (status, out)
        }
    };
    let (_, out) = route(&asked, "surrender").await;
    assert_eq!(
        (out["pause_for_conversation"].clone(), out["specialist_review"].clone()),
        (json!(true), json!(false)),
        "{out}"
    );
    let (_, out) = route(&asked, "view").await;
    assert!(out.get("pause_for_conversation").is_none(), "{out}");
    for act in ["change_beneficiary", "change_bank_details", "surrender"] {
        let (_, out) = route(&at_risk, act).await;
        assert_eq!(
            (out["pause_for_conversation"].clone(), out["specialist_review"].clone()),
            (json!(true), json!(true)),
            "{act}"
        );
        assert!(out.get("kind").is_none(), "the reason is not told: {out}");
    }
    let (_, out) = route(&plain, "surrender").await;
    assert_eq!(out, json!({ "party": plain, "route_to_person": false }));
    let (status, _) = route(&plain, "everything").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// Purpose: a person can choose a contact point for one relationship (letters about the
/// claim they make to go to their solicitor's postal address, say, or the trust's email
/// for policies they hold as trustee); notices about that relationship go there, notices
/// about anything else go to their own; the choice is checked like the person's own
/// details, one per relationship and channel, and can be set by the person in the app.
/// Persona row DOC-009, the master's part.
/// Why it matters: a person who asks for claim letters to go elsewhere, often because
/// home is not safe or not where they are, must not have them go home anyway; and the
/// choice must not redirect every other notice.
#[tokio::test]
async fn a_contact_point_can_be_chosen_for_one_relationship() {
    let w = world_or_skip!();
    let p = w.person("Sione", "Fifita", "1975-05-05", &[("email", json!("sione@example.nz"))]).await;
    let choose = |choices: Value| json!({ "command": "record", "facts": [{ "field": "relationship_contacts", "value": choices }] });
    for bad in [
        json!([{ "relationship": "claimant", "channel": "email", "destination": "not an email" }]),
        json!([{ "relationship": "Claimant!", "channel": "email", "destination": "a@b.nz" }]),
        json!([{ "relationship": "claimant", "channel": "fax", "destination": "1" }]),
        json!([{ "relationship": "claimant", "channel": "email", "destination": "a@b.nz", "extra": 1 }]),
        json!([{ "relationship": "claimant", "channel": "email", "destination": "a@b.nz" },
               { "relationship": "claimant", "channel": "email", "destination": "c@d.nz" }]),
        json!("claimant"),
    ] {
        let (status, _) = w.command(&p, "staff", choose(bad.clone())).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
    }
    // Chosen in the app by the person.
    let (status, out) = w
        .send(Call {
            method: "POST",
            path: format!("/v1/parties/{p}/commands"),
            role: "portal",
            purpose: Some("self_service"),
            body: Some(choose(json!([
                { "relationship": "claimant", "channel": "email", "destination": "Claims@Lawyers.example.nz" },
                { "relationship": "claimant", "channel": "post", "destination": ["Lawyers Ltd", "1 Queen St", "Auckland 1010"] }
            ]))),
            headers: vec![("x-ic-party", p.clone()), ("x-ic-actor", "app-session-1".into())],
        })
        .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let contact = |channel: &str, relationship: Option<&str>| {
        let rel = relationship.map(|r| format!("&relationship={r}")).unwrap_or_default();
        let path = format!("/v1/parties/{p}/contact-points?purpose=notices&channel={channel}{rel}");
        let w = &w;
        async move { w.get(&path, "documents", "notices").await }
    };
    assert_eq!(contact("email", Some("claimant")).await.1["destination"], "claims@lawyers.example.nz");
    assert_eq!(
        contact("post", Some("claimant")).await.1["destination"],
        "Lawyers Ltd\n1 Queen St\nAuckland 1010"
    );
    assert_eq!(contact("email", Some("policyholder")).await.1["destination"], "sione@example.nz");
    assert_eq!(contact("email", None).await.1["destination"], "sione@example.nz");
    let (status, _) = contact("post", Some("policyholder")).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "no address of his own, and the claim's is not used for other notices"
    );
}

/// Purpose: a change of legal name is one change to one record: every relationship the
/// person holds keeps pointing at them and shows the new name, the earlier name stays in
/// the history, and notices go out under the new name. A person under guardianship sits
/// in their own group, apart from their guardian's. Persona rows SVC-017 and VUL-012,
/// the master's part.
/// Why it matters: a name copied onto each policy would leave some documents in the old
/// name; and a dependent adult's money and policy must be theirs, not mixed into the
/// guardian's.
#[tokio::test]
async fn a_name_change_reaches_every_relationship_and_groups_stay_apart() {
    let w = world_or_skip!();
    let p = w.person("Jane", "Doe", "1970-01-01", &[("email", json!("jane@example.nz"))]).await;
    for (id, policy) in [("o1", "NP1"), ("o2", "NP2")] {
        w.command(
            &p,
            "staff",
            json!({ "command": "add_relationship", "id": id, "kind": "owner",
            "target": { "kind": "policy", "id": policy }, "valid_from": "2020-01-01" }),
        )
        .await;
    }
    let (status, out) = w
        .command(
            &p,
            "staff",
            json!({ "command": "record", "facts": [
            { "field": "family_name", "value": "Smith" }, { "field": "given_name", "value": "Janet" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    for policy in ["NP1", "NP2"] {
        let (_, rels) = w.get(&format!("/v1/relationships?target=policy:{policy}"), "staff", "servicing").await;
        assert_eq!(rels["relationships"][0]["party"], p.as_str(), "{rels}");
    }
    let (_, c) = w
        .get(
            &format!("/v1/parties/{p}/contact-points?purpose=notices&channel=email&relationship=policyholder"),
            "documents",
            "notices",
        )
        .await;
    assert_eq!(c["name"], "Janet Smith");
    let (_, history) = w.get(&format!("/v1/parties/{p}/history"), "staff", "servicing").await;
    assert!(history.to_string().contains("Doe"), "the earlier name is kept: {history}");
    // A dependent adult in his own group, apart from his guardian's.
    let michael = w.person("Michael", "Hohaia", "1990-01-01", &[]).await;
    let guardian = w.person("Huia", "Hohaia", "1962-02-02", &[]).await;
    for (who, group) in [(&michael, "grp-michael"), (&guardian, "grp-huia")] {
        let (status, out) = w.command(who, "staff", json!({ "command": "assign_group", "group": group })).await;
        assert_eq!(status, StatusCode::OK, "{out}");
    }
    let (_, m) = w.get(&format!("/v1/parties/{michael}"), "staff", "servicing").await;
    let (_, g) = w.get(&format!("/v1/parties/{guardian}"), "staff", "servicing").await;
    assert_eq!((m["group"].clone(), g["group"].clone()), (json!("grp-michael"), json!("grp-huia")));
}

/// Purpose: how a person wants to be written to is theirs alone: two people on one policy
/// keep their own language (Mandarin and English), format and channel, and each notice's
/// contact point carries that person's; a post-only person gets their address with their
/// format, and their representative acts only within scope. Persona rows VUL-021 and
/// VUL-019, the master's part.
/// Why it matters: a household is not one reader; sending one partner's letter in the
/// other's language, or emailing someone who can only read post, means they are not told.
#[tokio::test]
async fn presentation_preferences_are_per_person() {
    let w = world_or_skip!();
    let lin = w
        .person(
            "Lin",
            "Wong",
            "1960-06-06",
            &[("email", json!("lin@example.nz")), ("preferred_language", json!("zh-CN"))],
        )
        .await;
    let kevin = w
        .person(
            "Kevin",
            "Wong",
            "1958-05-05",
            &[("email", json!("kevin@example.nz")), ("preferred_language", json!("en"))],
        )
        .await;
    let email = |p: &str| format!("/v1/parties/{p}/contact-points?purpose=notices&channel=email&relationship=policyholder");
    assert_eq!(w.get(&email(&lin), "documents", "notices").await.1["language"], "zh-CN");
    assert_eq!(w.get(&email(&kevin), "documents", "notices").await.1["language"], "en");
    // Post only, in large print, with an attorney who may only view.
    let nan = w
        .person(
            "Nan",
            "Te Huia",
            "1932-02-02",
            &[
                ("postal_address", json!(["12 Kowhai Rd", "Gisborne 4010"])),
                ("notice_channel", json!("post")),
                ("presentation_format", json!("large_print")),
            ],
        )
        .await;
    let (_, c) = w
        .get(
            &format!("/v1/parties/{nan}/contact-points?purpose=notices&channel=post"),
            "documents",
            "notices",
        )
        .await;
    assert_eq!(
        (c["destination"].clone(), c["format"].clone()),
        (json!("12 Kowhai Rd\nGisborne 4010"), json!("large_print"))
    );
    let (status, _) = w
        .get(
            &format!("/v1/parties/{nan}/contact-points?purpose=notices&channel=email"),
            "documents",
            "notices",
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "no email to send to");
    let son = w.person("Rawiri", "Te Huia", "1960-03-03", &[]).await;
    w.command(
        &son,
        "staff",
        json!({ "command": "add_relationship", "id": "epa", "kind": "attorney",
        "target": { "kind": "party", "id": nan }, "valid_from": "2026-01-01",
        "attributes": { "basis": "enduring_power_of_attorney", "document": "doc_7", "scope": ["view"] } }),
    )
    .await;
    for (act, decision) in [("view", "allow"), ("change_contact_details", "refuse")] {
        let (_, out) = w
            .get(
                &format!("/v1/parties/{nan}/authority?actor={son}&act={act}&on=2026-06-01"),
                "policy-servicing",
                "servicing",
            )
            .await;
        assert_eq!(out["decision"], decision, "{act}");
    }
}

/// Purpose: a premium payer is recorded as a relationship from the payer to the owner, and
/// payments can read one relationship by its reference (party and id) with whether it is in
/// force, but a role without relationship access cannot, and an unknown one is not found.
/// Why it matters: payments clears a third-party payer AML case (FIN-015, FIN-016) by naming
/// such a relationship and never re-opens it, so it must check the reference is real and
/// links the two people before accepting it.
#[tokio::test]
async fn a_premium_payer_relationship_can_be_checked_by_its_reference() {
    let w = world_or_skip!();
    let owner = w.person("Mere", "Tane", "2004-02-02", &[]).await;
    let parent = w.person("Hemi", "Tane", "1972-07-07", &[]).await;
    let (status, out) = w
        .command(
            &parent,
            "staff",
            json!({ "command": "add_relationship", "id": "pays-mere", "kind": "premium_payer",
                    "target": { "kind": "party", "id": owner }, "valid_from": "2026-01-01",
                    "attributes": { "basis": "parent" } }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let (status, rel) = w.get(&format!("/v1/parties/{parent}/relationships/pays-mere"), "payments", "payments").await;
    assert_eq!(status, StatusCode::OK, "{rel}");
    assert_eq!(
        (rel["party"].clone(), rel["kind"].clone(), rel["target"].clone(), rel["in_force"].clone()),
        (json!(parent), json!("premium_payer"), json!({ "kind": "party", "id": owner }), json!(true))
    );
    let (status, _) = w.get(&format!("/v1/parties/{parent}/relationships/nope"), "payments", "payments").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = w.get(&format!("/v1/parties/{parent}/relationships/pays-mere"), "identity", "payments").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Ended, it is still found, and says so.
    w.command(
        &parent,
        "staff",
        json!({ "command": "end_relationship", "id": "pays-mere", "valid_to": "2026-03-01" }),
    )
    .await;
    let (_, rel) = w.get(&format!("/v1/parties/{parent}/relationships/pays-mere"), "payments", "payments").await;
    assert_eq!((rel["valid_to"].clone(), rel["in_force"].clone()), (json!("2026-03-01"), json!(false)));
}

/// Purpose: a member on a working holiday visa who leaves the employer and the country is
/// recorded as non-resident from the day they left, with their overseas address and tax
/// residency, while what was true before stays readable as of any earlier date; an unknown
/// residency status is refused (persona row GRP-037, whose leaver and continuation side is
/// group's).
/// Why it matters: continuation cover, tax on payments and where notices go all turn on
/// residency on a given day, and a claim dated before the move must still see the person
/// as resident then.
#[tokio::test]
async fn a_member_who_leaves_the_country_is_recorded_non_resident_from_that_day() {
    let w = world_or_skip!();
    let p = w
        .person(
            "Lena",
            "Weber",
            "1999-03-03",
            &[("residency_status", json!("working_holiday_visa")), ("tax_residency", json!(["NZ"]))],
        )
        .await;
    let left = "2026-10-15";
    let (status, out) = w
        .command(
            &p,
            "staff",
            json!({ "command": "record", "facts": [
                { "field": "residency_status", "value": "non_resident", "valid_from": left },
                { "field": "tax_residency", "value": ["DE"], "valid_from": left },
                { "field": "residential_address", "value": ["Hauptstr. 5", "10115 Berlin", "Germany"], "valid_from": left }
            ] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let (status, now) = w.get(&format!("/v1/parties/{p}"), "staff", "servicing").await;
    assert_eq!(status, StatusCode::OK, "{now}");
    assert_eq!(now["facts"]["residency_status"]["value"], "non_resident");
    assert_eq!(now["facts"]["residential_address"]["value"][2], "Germany");
    let (_, before) = w.get(&format!("/v1/parties/{p}?as_of=2026-10-14"), "staff", "servicing").await;
    assert_eq!(before["facts"]["residency_status"]["value"], "working_holiday_visa", "{before}");
    let (status, _) = w
        .command(
            &p,
            "staff",
            json!({ "command": "record", "facts": [{ "field": "residency_status", "value": "tourist" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}
