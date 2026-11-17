//! The group client against a stand-in HTTP service, and the rules the
//! portal applies before anything reaches the group service.

use std::sync::{Arc, Mutex};

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Router;
use ic_azure::credential::AzureCredential;
use ic_channels::group::{command, employer_member, name, named_scheme, question, scheme_of, Group, Hosted};
use serde_json::{json, Value};

type Seen = Arc<Mutex<Vec<(String, Vec<(String, String)>, Vec<u8>)>>>;

async fn answer(State(seen): State<Seen>, request: Request) -> Response {
    let path = request.uri().path().to_string();
    let headers = request
        .headers()
        .iter()
        .filter(|(n, _)| n.as_str() == "authorization" || n.as_str().starts_with("x-ic-"))
        .map(|(n, v)| (n.to_string(), v.to_str().unwrap().to_string()))
        .collect();
    let body = axum::body::to_bytes(request.into_body(), usize::MAX).await.unwrap().to_vec();
    let busy = body.windows(4).any(|w| w == b"busy");
    seen.lock().unwrap().push((path.clone(), headers, body));
    if busy {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match path.as_str() {
        "/v1/schemes/harbour/files/nov.csv" => (StatusCode::ACCEPTED, axum::Json(json!({ "status": "quarantined" }))).into_response(),
        "/v1/commands" => axum::Json(json!({ "applied": true })).into_response(),
        "/v1/questions" => axum::Json(json!({ "schemes": ["harbour"] })).into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Purpose: an upload carries the bytes with the employer portal's caller name and the administrator as actor, a command and a question
/// go as the group service's JSON with the portal's token, staff's and a member's commands go under their own caller names, a 503 or an unreachable service is an error to retry, and a body that is not
/// JSON is answered as null.
/// Why it matters: the group service decides who may act by these names; a missing actor would be refused, and a wrong caller name would
/// either be refused or let the portal act under staff's role.
#[tokio::test]
async fn the_group_service_is_called_as_the_employer_portal() {
    let seen = Seen::default();
    let log = seen.clone();
    let app = Router::new().fallback(answer).with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let group = Hosted {
        base,
        scope: "api://ic-group/.default".into(),
        credential: AzureCredential::static_token("t"),
        http: reqwest::Client::new(),
    };

    assert_eq!(group.upload("harbour", "nov.csv", "hr-1", b"a,b".to_vec()).await.unwrap().0, 202);
    let approve = json!({ "approve_file": { "scheme": "harbour", "file": "nov.csv" } });
    assert_eq!(
        group.command("hr-1:k1", "hr-1", approve.clone()).await.unwrap(),
        (200, json!({ "applied": true }))
    );
    assert_eq!(named_scheme(&group.ask(scheme_of("pol-1")).await.unwrap().1).as_deref(), Some("harbour"));
    assert_eq!(group.upload("harbour", "other.csv", "hr-1", b"x".to_vec()).await.unwrap(), (404, Value::Null));
    assert!(group.ask(json!({ "ask": "busy" })).await.unwrap_err().contains("try again"));

    let seen = seen.lock().unwrap().clone();
    let has = |i: usize, n: &str, v: &str| seen[i].1.iter().any(|(a, b)| a == n && b == v);
    assert!(has(0, "authorization", "Bearer t") && has(0, "x-ic-caller", "employer-portal") && has(0, "x-ic-actor", "hr-1"));
    assert_eq!(seen[0].2, b"a,b");
    let sent: Value = serde_json::from_slice(&seen[1].2).unwrap();
    assert_eq!(
        sent,
        json!({ "key": "hr-1:k1", "caller": "employer-portal", "actor": "hr-1", "command": approve })
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&seen[2].2).unwrap(),
        json!({ "ask": "scheme_of", "policy": "pol-1" })
    );

    // Staff's commands go as group-admin and the member's as member-portal; a trial of a mapping goes to its own path.
    let freeze = json!({ "freeze": { "scheme": "harbour" } });
    assert_eq!(group.staff_command("st-1:k", "st-1", freeze.clone()).await.unwrap().0, 200);
    let claim = json!({ "self_register": { "scheme": "harbour", "employee_id": "E9", "party": "kiri" } });
    assert_eq!(group.member_command("kiri:k", "kiri", claim.clone()).await.unwrap().0, 200);
    assert_eq!(group.try_mapping("harbour", json!({ "sample": "" })).await.unwrap().0, 404);
    let later: Vec<Value> = log.lock().unwrap()[5..]
        .iter()
        .map(|(_, _, body)| serde_json::from_slice(body).unwrap())
        .collect();
    assert_eq!(log.lock().unwrap()[7].0, "/v1/schemes/harbour/mappings/try");
    assert_eq!(later[0]["caller"], "group-admin");
    assert_eq!((&later[1]["caller"], &later[1]["command"]), (&json!("member-portal"), &claim));

    let gone = Hosted {
        base: "http://127.0.0.1:9".into(),
        ..group
    };
    assert!(gone.ask(scheme_of("pol-1")).await.is_err());
}

/// Purpose: an administrator may ask only about their own scheme's standing, invoices, a named file and a named member, and may send
/// only approve_file, withdraw_file and declare_pool, always on the scheme the portal found; names with paths in them are refused; and
/// a member reaches the employer with only the listed fields, whatever else the answer carries.
/// Why it matters: the scheme on a command is the portal's to set, never the page's, or one employer could approve another's file; and
/// the member filter is the portal's own guarantee that a loading, an exclusion or a reason never reaches an employer.
#[test]
fn what_an_administrator_may_ask_and_send() {
    let none = |_: &str| None;
    let given = |v: &'static str| move |_: &str| Some(v.to_string());
    assert_eq!(question("scheme", "h", none).unwrap(), json!({ "ask": "scheme", "scheme": "h" }));
    assert_eq!(question("invoices", "h", none).unwrap()["ask"], "invoices");
    assert_eq!(question("file", "h", given("nov.csv")).unwrap()["file"], "nov.csv");
    assert_eq!(question("member", "h", given("E001")).unwrap()["employee"], "E001");
    assert!(question("member", "h", none).unwrap_err().contains("?employee="));
    assert!(question("file", "h", given("../x")).is_err());
    assert!(question("exceptions", "h", none).is_err());

    let sent = json!({ "withdraw_file": { "file": "nov.csv", "scheme": "theirs" } });
    assert_eq!(command("h", &sent).unwrap(), json!({ "withdraw_file": { "file": "nov.csv", "scheme": "h" } }));
    assert!(command("h", &json!({ "freeze": {} })).unwrap_err().contains("not a command"));
    assert!(command("h", &json!({ "declare_pool": 1 })).unwrap_err().contains("takes an object"));
    assert!(command("h", &json!([])).is_err());

    for ok in ["nov.csv", "2027-11_full"] {
        assert_eq!(name(ok), Some(ok));
    }
    for bad in ["", "a/b", "..", "a b", &"x".repeat(65)] {
        assert_eq!(name(bad), None, "{bad}");
    }
    assert_eq!(named_scheme(&json!({ "schemes": [] })), None);

    let member = employer_member(&json!({ "employee": "E1", "loading": 1, "covers": [{ "cover": "c", "exclusion": "x" }] }));
    assert_eq!(member, json!({ "employee": "E1", "covers": [{ "cover": "c" }], "above_automatic": [] }));
}

/// Purpose: a change an administrator enters by hand becomes one change-file row: a leaver or a new salary, never a joiner; its
/// file is named from the request key; and the second-administrator rule names only the kinds both asked for and in the file.
/// Why it matters: found by the scenario run, SCN-05 (F-SCN05-5): a hand change was stored as a request nothing
/// acted on, and the rule a scheme chose for a second administrator was never applied to the files that carried those changes.
#[test]
fn a_hand_change_is_one_row_of_a_change_file() {
    use ic_channels::group::{change_words, entered_change, entered_file, needs_second_admin};
    let leaver = json!({ "kind": "remove_member", "employee_id": " E002 ", "effective": "2027-12-01" });
    let (kind, row) = entered_change(&leaver).unwrap();
    assert_eq!(
        (kind, row),
        (
            "remove_member",
            json!({ "row": 1, "employee_id": "E002", "change": "leave", "effective": "2027-12-01", "attrs": {} })
        )
    );
    let salary = json!({ "kind": "change_salary", "employee_id": "E001", "effective": "2027-12-01", "salary_minor": 70_000_00 });
    assert_eq!(entered_change(&salary).unwrap().1["attrs"], json!({ "salary_minor": 70_000_00 }));
    assert!(entered_change(&json!({ "kind": "add_member" })).unwrap_err().contains("membership file"));
    assert_eq!(entered_file("api:hr-1:Key/With-Dashes"), "entered-apihr1KeyWithDashes");
    assert_eq!(entered_file(&"k".repeat(60)).len(), "entered-".len() + 40);

    let file = json!({ "summary": { "joiners": 1, "leavers": 2, "changes": 0 } });
    assert_eq!(needs_second_admin(&file, &json!(["add_member", "change_salary"])), vec!["add_member"]);
    assert_eq!(needs_second_admin(&file, &json!(null)), Vec::<&str>::new());
    let words: Vec<&str> = ["add_member", "remove_member", "change_salary"].into_iter().map(change_words).collect();
    assert_eq!(words, ["adding a member", "removing a member", "changing a salary"]);
}
