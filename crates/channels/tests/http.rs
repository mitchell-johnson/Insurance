//! The portal service over HTTP, against the channel module in process.

mod common;

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use common::*;
use http_body_util::BodyExt;
use ic_api::auth::Auth;
use ic_channels::classes::DataClass;
use ic_channels::directory::DirectoryCommand;
use ic_channels::http::{approval_action, router, AppState, Local, OPERATIONS};
use ic_channels::identity::{action_hash, proof_message, Enrolment, IdentityCommand};
use ic_channels::ops::{Fixed, RegionHealth, RegionReport};
use ic_channels::staff::{actions as staff_actions, Profile, StaffCommand, Template};
use ic_channels::ui::{origin, SignIn, UiConfig};
use ic_channels::workbench::{self, HoldReason, Item, ItemKind, WorkbenchCommand};
use ic_channels::Command;
use ic_core::PolicyId;
use ic_product::{Frequency, Sex};
use ic_sim::persona::{group_scheme, member, term_life};
use serde_json::{json, Value};
use tower::ServiceExt;

const KIRI: &str = "party-00000001";

struct Portal {
    app: Router,
    local: Arc<Local>,
    regions: Arc<Fixed>,
    clock: Arc<AtomicI64>,
    keys: AtomicU64,
    phone: Key,
    retail: PolicyId,
    scheme: PolicyId,
}

fn healthy() -> RegionReport {
    let region = |name: &str, home, leaders| RegionHealth {
        name: name.into(),
        home,
        nodes: 2,
        reachable: 2,
        leaders,
    };
    RegionReport {
        regions: vec![region("auckland", true, 4), region("sydney", false, 0)],
        partitions: 4,
        partitions_with_leader: 4,
        quorum: true,
        at_home: true,
    }
}

fn template(id: &str, actions: &[&str], classes: &[DataClass]) -> Template {
    Template {
        id: id.to_string(),
        actions: actions.iter().map(|a| a.to_string()).collect(),
        classes: classes.iter().copied().collect(),
        default_profile: "p".into(),
        ceilings: [(workbench::LIMIT.to_string(), 1_000_000_00)].into(),
        break_glass: false,
    }
}

/// A retail customer with an enrolled phone, an adviser, a scheme with an
/// administrator, and staff: an administrator, a service agent and two
/// finance officers.
fn portal() -> Portal {
    let mut w = World::new();
    let retail = w.issue(term_life(
        1,
        open(),
        d(1990, 3, 10),
        Sex::Female,
        false,
        500_000_00,
        None,
        Frequency::Monthly,
        None,
    ));
    let scheme = w.issue(group_scheme(3, open(), vec![member("m-1", d(1980, 1, 1), "staff", 80_000_00)]));
    for (subject, who) in [
        ("sub-kiri", KIRI),
        ("sub-adv", "adv-1"),
        ("sub-hr", "hr-1"),
        ("sub-alice", "alice"),
        ("sub-sue", "sue"),
        ("sub-fin1", "fin-1"),
        ("sub-fin2", "fin-2"),
    ] {
        w.dir(
            module("identity"),
            DirectoryCommand::BindSubject {
                subject: subject.into(),
                party: p(who),
            },
        )
        .unwrap();
    }
    let phone = Key::new();
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
    w.engage("e1", "adv-1", "fap-1", None).unwrap();
    w.dir(
        module("group-admin"),
        DirectoryCommand::SetEmployerAdmin {
            policy: scheme.clone(),
            party: p("hr-1"),
            admin: true,
        },
    )
    .unwrap();
    w.send(
        module("entitlements"),
        Command::Staff(StaffCommand::Bootstrap {
            templates: vec![
                template("administrator", &[staff_actions::MANAGE_STAFF], &[DataClass::Ordinary]),
                template("service", &["policy.view"], &[DataClass::Ordinary, DataClass::Personal]),
                template(
                    "finance",
                    &[workbench::actions::VIEW, workbench::actions::PREPARE, workbench::actions::APPROVE],
                    &[DataClass::Ordinary, DataClass::Financial],
                ),
            ],
            profiles: vec![Profile {
                id: "p".into(),
                version: 1,
                limits: [(workbench::LIMIT.to_string(), 100_000_00)].into(),
            }],
            admin: p("alice"),
            admin_templates: vec!["administrator".into()],
        }),
    )
    .unwrap();
    for (who, role) in [("sue", "service"), ("fin-1", "finance"), ("fin-2", "finance")] {
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
    w.send(
        module("commission"),
        Command::Workbench(WorkbenchCommand::Publish {
            party: p("adv-1"),
            items: vec![Item {
                id: "h1".into(),
                kind: ItemKind::Held,
                amount_minor: 900_00,
                currency: "NZD".into(),
                period: Some("2027-11".into()),
                book: None,
                reason: Some(HoldReason::SuspendedLicence),
                since: open(),
            }],
            documents: vec![],
            cases: vec![],
        }),
    )
    .unwrap();
    let clock = Arc::new(AtomicI64::new(w.at + 10));
    let local = Arc::new(Local::new(w.ch));
    let regions = Arc::new(Fixed(Mutex::new(healthy())));
    let tick = clock.clone();
    let app = router(Arc::new(AppState {
        channel: local.clone(),
        customers: Auth::Dev,
        workforce: Auth::Dev,
        clock: Arc::new(move || tick.load(Ordering::SeqCst)),
        regions: regions.clone(),
        ui: UiConfig::Dev,
    }));
    Portal {
        app,
        local,
        regions,
        clock,
        keys: AtomicU64::new(0),
        phone,
        retail,
        scheme,
    }
}

impl Portal {
    fn now(&self) -> i64 {
        self.clock.load(Ordering::SeqCst)
    }

    async fn send(&self, method: &str, path: &str, headers: &[(&str, String)], body: Option<Value>) -> (StatusCode, Value) {
        let bytes = body.map(|b| b.to_string()).unwrap_or_default();
        let mut request = Request::builder().method(method).uri(path);
        for (name, value) in headers {
            request = request.header(*name, value);
        }
        let response = self.app.clone().oneshot(request.body(Body::from(bytes)).unwrap()).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let value = serde_json::from_slice(&bytes).unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
        (status, value)
    }

    /// A read as `subject`.
    async fn get(&self, subject: &str, path: &str) -> (StatusCode, Value) {
        self.send("GET", path, &[("x-dev-subject", subject.to_string())], None).await
    }

    /// A write as `subject`, with a fresh idempotency key.
    async fn write(&self, method: &str, subject: &str, path: &str, body: Value) -> (StatusCode, Value) {
        let key = format!("key-{}", self.keys.fetch_add(1, Ordering::SeqCst));
        self.send(method, path, &[("x-dev-subject", subject.to_string()), ("idempotency-key", key)], Some(body))
            .await
    }

    /// Headers proving the phone's key for one request.
    fn proof(&self, key: &Key, method: &str, path: &str, body: &str) -> Vec<(&'static str, String)> {
        let now = self.now();
        let signature = key.sign(&proof_message(now, method, path.split('?').next().unwrap(), body.as_bytes()));
        vec![
            ("x-dev-subject", "sub-kiri".to_string()),
            ("x-device-id", "phone".to_string()),
            ("x-device-proof", format!("{now}.{signature}")),
            ("idempotency-key", format!("key-{}", self.keys.fetch_add(1, Ordering::SeqCst))),
        ]
    }
}

/// Purpose: the service answers its probes, refuses a caller it cannot identify, one it does not know, and one whose session was closed.
/// Why it matters: these are the outer gates; a caller with a valid Entra token but no binding to a party, or a token older than a revocation, must get nothing.
#[tokio::test]
async fn callers_are_identified_before_anything_else() {
    let portal = portal();
    assert_eq!(portal.send("GET", "/healthz", &[], None).await.0, StatusCode::OK);
    assert_eq!(portal.send("GET", "/readyz", &[], None).await.0, StatusCode::OK);

    let (status, problem) = portal.send("GET", "/app/v1/me", &[], None).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::UNAUTHORIZED, Some("unauthenticated")));
    assert_eq!(problem["type"], "urn:insurance-core:problem:unauthenticated");
    let (status, problem) = portal.get("sub-unknown", "/app/v1/me").await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::FORBIDDEN, Some("forbidden")));

    let (status, me) = portal.get("sub-kiri", "/app/v1/me").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (&me["party"], &me["staff"], &me["policies"][0]["number"]),
        (&json!(KIRI), &json!(false), &json!("KTL00000001"))
    );

    // The identity service closes Kiri's sessions: a token issued before then is refused, a later one accepted.
    let before = portal.now() - 5;
    let now = portal.now();
    portal.local.with(|state| {
        use ic_log::StateMachine;
        let output = state.apply(
            10_000,
            ic_channels::Envelope {
                key: "revoke".into(),
                at: now,
                actor: module("identity"),
                command: Command::Identity(IdentityCommand::RevokeSessions { party: p(KIRI) }),
            },
        );
        assert!(output.result.is_ok());
    });
    let old = [("x-dev-subject", "sub-kiri".to_string()), ("x-dev-issued", before.to_string())];
    let (status, problem) = portal.send("GET", "/app/v1/me", &old, None).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::UNAUTHORIZED, Some("session_revoked")));
    let new = [("x-dev-subject", "sub-kiri".to_string()), ("x-dev-issued", (now + 1).to_string())];
    assert_eq!(portal.send("GET", "/app/v1/me", &new, None).await.0, StatusCode::OK);
    // A bearer token that carries no issue time is treated as the oldest possible.
    let bare = [("x-dev-subject", "sub-kiri".to_string()), ("authorization", "Bearer a.e30.c".to_string())];
    assert_eq!(portal.send("GET", "/app/v1/me", &bare, None).await.0, StatusCode::UNAUTHORIZED);
    let dated = [
        ("x-dev-subject", "sub-kiri".to_string()),
        ("authorization", format!("Bearer a.{}.c", b64url(&json!({ "iat": now + 1 }).to_string()))),
    ];
    assert_eq!(portal.send("GET", "/app/v1/me", &dated, None).await.0, StatusCode::OK);
}

fn b64url(text: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(text)
}

/// Purpose: a customer reads their own policy, gets 404 for anyone else's, syncs only from an enrolled device that proved its key, and manages their requests with idempotency keys.
/// Why it matters: this is the customer app's API; offline data must go only to a bound device, and a retried write must not be applied twice.
#[tokio::test]
async fn the_customer_app_reads_syncs_and_requests() {
    let portal = portal();
    let (status, view) = portal.get("sub-kiri", &format!("/app/v1/policies/{}", portal.retail)).await;
    assert_eq!((status, view["rows"].as_array().unwrap().len()), (StatusCode::OK, 4));
    assert_eq!(
        portal.get("sub-adv", &format!("/app/v1/policies/{}", portal.retail)).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(portal.get("sub-kiri", "/app/v1/policies/nope").await.0, StatusCode::NOT_FOUND);

    // Sync: a token alone is not enough; a proof from another key is not enough.
    let (status, problem) = portal.get("sub-kiri", "/app/v1/sync").await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::UNAUTHORIZED, Some("device_required")));
    let forged = portal.proof(&Key::new(), "GET", "/app/v1/sync", "");
    assert_eq!(portal.send("GET", "/app/v1/sync", &forged, None).await.0, StatusCode::UNAUTHORIZED);
    let proved = portal.proof(&portal.phone, "GET", "/app/v1/sync?have=0", "");
    let (status, sync) = portal.send("GET", "/app/v1/sync?have=0", &proved, None).await;
    assert_eq!((status, &sync["mode"]), (StatusCode::OK, &json!("changes")));
    let version = sync["version"].as_u64().unwrap();
    let proved = portal.proof(&portal.phone, "GET", "/app/v1/sync", "");
    assert_eq!(portal.send("GET", "/app/v1/sync", &proved, None).await.1["mode"], "snapshot");

    // A request: the write needs an idempotency key and a body that parses.
    let request = json!({ "id": "r1", "policy": portal.retail, "kind": "decline_indexation", "module": "policy-servicing", "payload": { "declined": true } });
    let no_key = [("x-dev-subject", "sub-kiri".to_string())];
    assert_eq!(
        portal.send("POST", "/app/v1/requests", &no_key, Some(request.clone())).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        portal.write("POST", "sub-kiri", "/app/v1/requests", json!({ "id": 7 })).await.0,
        StatusCode::BAD_REQUEST
    );
    let keyed = [("x-dev-subject", "sub-kiri".to_string()), ("idempotency-key", "same".to_string())];
    let (status, first) = portal.send("POST", "/app/v1/requests", &keyed, Some(request.clone())).await;
    assert_eq!((status, &first["duplicate"]), (StatusCode::ACCEPTED, &json!(false)));
    let (status, again) = portal.send("POST", "/app/v1/requests", &keyed, Some(request.clone())).await;
    assert_eq!((status, &again["duplicate"]), (StatusCode::ACCEPTED, &json!(true)));
    // The same request under a new key is a conflict, not a second request.
    let (status, problem) = portal.write("POST", "sub-kiri", "/app/v1/requests", request).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::CONFLICT, Some("duplicate")));

    let (_, list) = portal.get("sub-kiri", "/app/v1/requests").await;
    assert_eq!((list.as_array().unwrap().len(), &list[0]["status"]), (1, &json!("pending")));
    // The request reached the device's feed.
    let proved = portal.proof(&portal.phone, "GET", &format!("/app/v1/sync?have={version}"), "");
    let (_, sync) = portal.send("GET", &format!("/app/v1/sync?have={version}"), &proved, None).await;
    assert_eq!(sync["changes"][0]["key"], format!("{}/request/r1", portal.retail));

    assert_eq!(
        portal.write("DELETE", "sub-adv", "/app/v1/requests/r1", json!(null)).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(portal.write("DELETE", "sub-kiri", "/app/v1/requests/r1", json!(null)).await.0, StatusCode::OK);
    assert_eq!(
        portal.write("DELETE", "sub-kiri", "/app/v1/requests/r1", json!(null)).await.0,
        StatusCode::CONFLICT
    );
    // Another party's policy is a 404 on write too.
    let other = json!({ "id": "r9", "policy": portal.scheme, "kind": "x", "module": "claims" });
    assert_eq!(portal.write("POST", "sub-kiri", "/app/v1/requests", other).await.0, StatusCode::NOT_FOUND);
}

/// Purpose: the customer appoints an adviser, the adviser prepares a change, and the customer approves it only with a signature from their enrolled device over exactly that approval.
/// Why it matters: this is transaction signing end to end over HTTP; an approval without the device's signature, or with a signature for something else, must not release the adviser's request.
#[tokio::test]
async fn an_advisers_request_needs_the_customers_signed_approval() {
    let portal = portal();
    let policy = &portal.retail;
    assert_eq!(
        portal
            .write(
                "POST",
                "sub-adv",
                &format!("/app/v1/policies/{policy}/advisers"),
                json!({ "adviser": "adv-1", "engagement": "e1" })
            )
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        portal
            .write(
                "POST",
                "sub-kiri",
                &format!("/app/v1/policies/{policy}/advisers"),
                json!({ "adviser": "adv-1", "engagement": "e1" })
            )
            .await
            .0,
        StatusCode::OK
    );

    let (status, book) = portal.get("sub-adv", "/adviser/v1/book").await;
    assert_eq!(
        (status, &book["policies"][0]["can_transact"], &book["held"]["held"]["NZD"]),
        (StatusCode::OK, &json!(true), &json!(900_00))
    );
    assert_eq!(
        portal.get("sub-adv", "/adviser/v1/held-balance").await.1["entries"][0]["reason"],
        "suspended_licence"
    );
    assert_eq!(portal.get("sub-adv", &format!("/app/v1/policies/{policy}")).await.0, StatusCode::OK);

    let request = json!({ "id": "r1", "policy": policy, "kind": "decline_indexation", "module": "policy-servicing", "payload": {} });
    assert_eq!(portal.write("POST", "sub-adv", "/app/v1/requests", request).await.0, StatusCode::ACCEPTED);

    // Unsigned: the module asks for step-up.
    let (status, problem) = portal.write("POST", "sub-kiri", "/app/v1/requests/r1/approve", json!({})).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::UNAUTHORIZED, Some("step_up_required")));

    // A challenge is given only to a proven device.
    let ask = json!({ "action": approval_action("r1") }).to_string();
    let (status, problem) = portal
        .write("POST", "sub-kiri", "/app/v1/challenges", json!({ "action": approval_action("r1") }))
        .await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::UNAUTHORIZED, Some("device_required")));
    let headers = portal.proof(&portal.phone, "POST", "/app/v1/challenges", &ask);
    let (status, challenge) = portal
        .send("POST", "/app/v1/challenges", &headers, Some(serde_json::from_str(&ask).unwrap()))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(challenge["action_hash"], action_hash(&approval_action("r1")));
    let message = format!(
        "{}\n{}\n{}",
        challenge["challenge"].as_str().unwrap(),
        challenge["nonce"].as_str().unwrap(),
        challenge["action_hash"].as_str().unwrap()
    );

    // Signed by another key, or the right signature offered for another request.
    let forged = json!({ "signed": { "challenge": challenge["challenge"], "signature": Key::new().sign(message.as_bytes()) } });
    let (status, problem) = portal.write("POST", "sub-kiri", "/app/v1/requests/r1/approve", forged).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::UNAUTHORIZED, Some("step_up_failed")));
    let signed = json!({ "signed": { "challenge": challenge["challenge"], "signature": portal.phone.sign(message.as_bytes()) } });
    assert_eq!(
        portal.write("POST", "sub-kiri", "/app/v1/requests/r2/approve", signed.clone()).await.0,
        StatusCode::UNAUTHORIZED
    );

    let (status, result) = portal.write("POST", "sub-kiri", "/app/v1/requests/r1/approve", signed.clone()).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert!(result["effects"].as_array().unwrap().iter().any(|e| e.get("request_ready").is_some()));
    // The challenge is spent.
    assert_eq!(
        portal.write("POST", "sub-kiri", "/app/v1/requests/r1/approve", signed).await.0,
        StatusCode::UNAUTHORIZED
    );

    // The customer ends the adviser's access.
    assert_eq!(
        portal
            .write("DELETE", "sub-kiri", &format!("/app/v1/policies/{policy}/advisers/adv-1"), json!(null))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(portal.get("sub-adv", &format!("/app/v1/policies/{policy}")).await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        portal
            .write("PUT", "sub-kiri", &format!("/app/v1/policies/{policy}/consent"), json!({ "consent": true }))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        portal
            .write("PUT", "sub-adv", &format!("/app/v1/policies/{policy}/consent"), json!({ "consent": true }))
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

/// Purpose: a customer lists their devices, adds one approved by a device they hold, and revokes one.
/// Why it matters: device management is self-service; adding a device must need the existing device's signature over the new key.
#[tokio::test]
async fn devices_are_managed_by_their_owner() {
    let portal = portal();
    let (_, devices) = portal.get("sub-kiri", "/app/v1/devices").await;
    assert_eq!(devices.as_array().unwrap().len(), 1);
    let laptop = Key::new();
    let enrol =
        |signature: String| json!({ "device": "laptop", "public_key": laptop.public, "hardware_backed": false, "approver": "phone", "signature": signature });
    let (status, problem) = portal.write("POST", "sub-kiri", "/app/v1/devices", enrol("AAAA".into())).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_signature")));
    let signature = portal.phone.sign(format!("enrol\nlaptop\n{}", laptop.public).as_bytes());
    assert_eq!(
        portal.write("POST", "sub-adv", "/app/v1/devices", enrol(signature.clone())).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(portal.write("POST", "sub-kiri", "/app/v1/devices", enrol(signature)).await.0, StatusCode::OK);
    assert_eq!(portal.get("sub-kiri", "/app/v1/devices").await.1.as_array().unwrap().len(), 2);
    assert_eq!(
        portal.write("DELETE", "sub-adv", "/app/v1/devices/laptop", json!(null)).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        portal.write("DELETE", "sub-kiri", "/app/v1/devices/laptop", json!(null)).await.0,
        StatusCode::OK
    );
    assert!(!portal.get("sub-kiri", "/app/v1/devices").await.1[0]["revoked"].is_null());

    // The phone is lost. After fresh verification the identity service enrols its replacement as a recovery.
    let replacement = Key::new();
    let recover = json!({ "identity": { "enrol_device": { "device": "phone-2", "party": KIRI, "public_key": replacement.public, "hardware_backed": true,
        "enrolment": { "via": "recovery", "reference": "idv-9" } } } });
    let identity = [
        ("x-dev-subject", "svc".to_string()),
        ("x-dev-roles", "identity".to_string()),
        ("x-module", "identity".to_string()),
        ("idempotency-key", "recover-1".to_string()),
    ];
    let (status, answer) = portal.send("POST", "/internal/v1/commands", &identity, Some(recover)).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    // The old phone is told to wipe, whatever token it presents; a session from before the recovery is closed.
    portal.clock.fetch_add(5, Ordering::SeqCst);
    let mut old = portal.proof(&portal.phone, "GET", "/app/v1/sync", "");
    old.push(("x-dev-issued", (portal.now() + 1).to_string()));
    let (status, problem) = portal.send("GET", "/app/v1/sync", &old, None).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::UNAUTHORIZED, Some("device_revoked")));
    let stale = [("x-dev-subject", "sub-kiri".to_string()), ("x-dev-issued", (portal.now() - 60).to_string())];
    assert_eq!(portal.send("GET", "/app/v1/me", &stale, None).await.1["title"], "session_revoked");
    // The replacement syncs after signing in again.
    let now = portal.now();
    let signature = replacement.sign(&proof_message(now, "GET", "/app/v1/sync", b""));
    let fresh = [
        ("x-dev-subject", "sub-kiri".to_string()),
        ("x-dev-issued", (now + 1).to_string()),
        ("x-device-id", "phone-2".to_string()),
        ("x-device-proof", format!("{now}.{signature}")),
    ];
    assert_eq!(portal.send("GET", "/app/v1/sync", &fresh, None).await.0, StatusCode::OK);
}

/// Purpose: an employer administrator sees their schemes and manages the scheme's administrators and its second-administrator rule; nobody else can.
/// Why it matters: the employer portal is run by the employer's own staff; a member or an outsider changing who administers a scheme could alter everyone's cover.
#[tokio::test]
async fn the_employer_portal_serves_administrators() {
    let portal = portal();
    let scheme = &portal.scheme;
    let (status, schemes) = portal.get("sub-hr", "/employer/v1/schemes").await;
    assert_eq!(
        (status, schemes.as_array().unwrap().len(), &schemes[0]["policy"]),
        (StatusCode::OK, 1, &json!(scheme))
    );
    assert_eq!(portal.get("sub-kiri", "/employer/v1/schemes").await.1, json!([]));
    let (_, view) = portal.get("sub-hr", &format!("/app/v1/policies/{scheme}")).await;
    assert!(view["rows"].as_array().unwrap().iter().any(|r| r["kind"] == "scheme"));

    assert_eq!(
        portal
            .write(
                "PUT",
                "sub-kiri",
                &format!("/employer/v1/schemes/{scheme}/admins/{KIRI}"),
                json!({ "admin": true })
            )
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        portal
            .write("PUT", "sub-hr", &format!("/employer/v1/schemes/{scheme}/admins/hr-2"), json!({ "admin": true }))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        portal
            .write(
                "PUT",
                "sub-hr",
                &format!("/employer/v1/schemes/{scheme}/second-admin"),
                json!({ "changes": ["remove_member"] })
            )
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        portal
            .write(
                "PUT",
                "sub-kiri",
                &format!("/employer/v1/schemes/{scheme}/second-admin"),
                json!({ "changes": [] })
            )
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let request = json!({ "id": "r1", "policy": scheme, "kind": "remove_member", "module": "group-admin" });
    let (status, result) = portal.write("POST", "sub-hr", "/app/v1/requests", request).await;
    assert_eq!(
        (status, result["effects"].as_array().unwrap().len()),
        (StatusCode::ACCEPTED, 0),
        "it waits for the second administrator"
    );
}

/// Purpose: the staff console is for staff, shows a policy only for a stated case, explains a refusal, and passes staff access commands to the module under the caller's own identity.
/// Why it matters: staff see more than any customer; every view must go through the four questions, and the reason for a refusal is what lets someone ask their manager for the right access.
#[tokio::test]
async fn the_staff_console_asks_the_four_questions() {
    let portal = portal();
    let policy = &portal.retail;
    let (status, problem) = portal.get("sub-kiri", "/staff/v1/me").await;
    assert_eq!(
        (status, problem["detail"].as_str().unwrap().contains("no staff role")),
        (StatusCode::FORBIDDEN, true)
    );
    assert_eq!(portal.get("sub-sue", "/staff/v1/me").await.1["templates"], json!(["service"]));

    let (status, view) = portal.get("sub-sue", &format!("/staff/v1/policies/{policy}?case=case-1&scope=")).await;
    assert_eq!((status, &view["withheld"]), (StatusCode::OK, &json!(0)));
    let (status, problem) = portal.get("sub-sue", &format!("/staff/v1/policies/{policy}")).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::FORBIDDEN, Some("not_authorised")));
    assert!(problem["detail"].as_str().unwrap().contains("purpose"));
    assert_eq!(portal.get("sub-sue", "/staff/v1/policies/nope?case=case-1").await.0, StatusCode::NOT_FOUND);
    // A scope the person has no path to.
    assert_eq!(
        portal
            .get("sub-sue", &format!("/staff/v1/policies/{policy}?case=c&scope=queue:claims,team:x"))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        portal.get("sub-fin1", &format!("/staff/v1/policies/{policy}?case=c")).await.0,
        StatusCode::FORBIDDEN
    );

    // Administration.
    assert_eq!(portal.get("sub-alice", "/staff/v1/admin").await.1["members"].as_array().unwrap().len(), 4);
    assert_eq!(portal.get("sub-sue", "/staff/v1/admin").await.0, StatusCode::FORBIDDEN);
    let path = json!({ "assign_path": { "party": "sue", "scope": "queue:claims" } });
    assert_eq!(portal.write("POST", "sub-sue", "/staff/v1/access", path.clone()).await.0, StatusCode::FORBIDDEN);
    assert_eq!(portal.write("POST", "sub-alice", "/staff/v1/access", path).await.0, StatusCode::OK);
    assert_eq!(
        portal.get("sub-sue", &format!("/staff/v1/policies/{policy}?case=c&scope=queue:claims")).await.0,
        StatusCode::OK
    );

    // A request for a customer on the phone waits for the customer.
    let request = json!({ "id": "r1", "policy": policy, "kind": "change_frequency", "module": "policy-servicing" });
    assert_eq!(
        portal.write("POST", "sub-kiri", "/staff/v1/requests", request.clone()).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(portal.write("POST", "sub-sue", "/staff/v1/requests", request).await.0, StatusCode::ACCEPTED);
    assert_eq!(
        portal.local.with(|s| serde_json::to_value(&s.data.requests["r1"]).unwrap())["status"],
        "awaiting_approval"
    );
}

/// Purpose: a staff read of a policy or of the workbench leaves a line in the access log, a refusal does too, and nobody can write the log by hand.
/// Why it matters: the access log is how misuse of customer data is found and how a person sees what was done in their name; a read that left no line, or a line anyone could forge, would make it worthless.
#[tokio::test]
async fn every_staff_read_is_in_the_access_log() {
    let portal = portal();
    let policy = &portal.retail;
    let recent = |me: &Value| me["recent"].as_array().cloned().unwrap_or_default();
    assert!(recent(&portal.get("sub-sue", "/staff/v1/me").await.1).is_empty());

    assert_eq!(
        portal.get("sub-sue", &format!("/staff/v1/policies/{policy}?case=case-7")).await.0,
        StatusCode::OK
    );
    portal.clock.fetch_add(1, Ordering::SeqCst);
    assert_eq!(portal.get("sub-sue", &format!("/staff/v1/policies/{policy}")).await.0, StatusCode::FORBIDDEN);
    let log = recent(&portal.get("sub-sue", "/staff/v1/me").await.1);
    // Newest first.
    assert_eq!(log.len(), 2);
    assert_eq!(
        (log[1]["action"].as_str(), log[1]["record"].as_str(), log[1]["decision"]["allowed"].as_bool()),
        (Some("policy.view"), Some(format!("policy:{policy}").as_str()), Some(true))
    );
    assert_eq!(log[1]["purpose"]["reference"], "case-7");
    assert_eq!(
        (log[0]["decision"]["allowed"].as_bool(), log[0]["decision"]["denied"].as_str()),
        (Some(false), Some("why"))
    );

    // The same read retried within the second is one line, not two.
    assert_eq!(portal.get("sub-sue", &format!("/staff/v1/policies/{policy}")).await.0, StatusCode::FORBIDDEN);
    assert_eq!(recent(&portal.get("sub-sue", "/staff/v1/me").await.1).len(), 2);

    // The workbench list, one party and the proof of debt are each logged against the party read.
    for path in ["/staff/v1/workbench", "/staff/v1/workbench/adv-1", "/staff/v1/workbench/adv-1/proof-of-debt"] {
        portal.clock.fetch_add(1, Ordering::SeqCst);
        assert_eq!(portal.get("sub-fin1", path).await.0, StatusCode::OK, "{path}");
    }
    let records: Vec<String> = recent(&portal.get("sub-fin1", "/staff/v1/me").await.1)
        .iter()
        .map(|r| r["record"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(records, ["counterparty:adv-1", "counterparty:adv-1", "counterparty:*"]);

    // Sue is also a customer: she declares her connection to Kiri, and from then on Kiri's policy is not hers to open.
    portal.clock.fetch_add(1, Ordering::SeqCst);
    let declare = json!({ "declare_connection": { "party": KIRI } });
    assert_eq!(portal.write("POST", "sub-sue", "/staff/v1/access", declare).await.0, StatusCode::OK);
    let (status, problem) = portal.get("sub-sue", &format!("/staff/v1/policies/{policy}?case=case-8")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(problem["detail"].as_str().unwrap().contains("personal connection"));
    let log = recent(&portal.get("sub-sue", "/staff/v1/me").await.1);
    assert_eq!((log.len(), log[0]["flag"].as_str()), (3, Some("personal")));
    // She cannot take the declaration back herself.
    let end = json!({ "end_connection": { "staff": "sue", "party": KIRI } });
    assert_eq!(portal.write("POST", "sub-sue", "/staff/v1/access", end).await.0, StatusCode::FORBIDDEN);

    // Kiri joins the staff. Her own policy is for the customer app, never for the staff console.
    let employ = json!({ "employ": { "party": KIRI, "team": "ops", "manager": null, "templates": ["service"] } });
    assert_eq!(portal.write("POST", "sub-alice", "/staff/v1/access", employ).await.0, StatusCode::OK);
    let (status, _) = portal.get("sub-kiri", &format!("/staff/v1/policies/{policy}?case=case-9")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(recent(&portal.get("sub-kiri", "/staff/v1/me").await.1)[0]["flag"], "personal");
    assert_eq!(portal.get("sub-kiri", &format!("/app/v1/policies/{policy}")).await.0, StatusCode::OK);

    // Nobody writes the log by hand, not even about themselves.
    let forged = json!({ "record": {
        "request": { "staff": "sue", "action": "policy.view", "class": "personal", "decides": false, "fresh": true, "at": portal.now() },
        "decision": { "allowed": true, "denied": null, "template": null, "profile": null, "grant": null, "classes": [] },
    } });
    let (status, problem) = portal.write("POST", "sub-sue", "/staff/v1/access", forged).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::FORBIDDEN, Some("forbidden")));
    assert_eq!(recent(&portal.get("sub-sue", "/staff/v1/me").await.1).len(), 3);
}

/// Purpose: finance staff work the held balance workbench over HTTP: the list, one party, the proof of debt, and prepare then approve by a second person; the console cannot send the commands that belong to the owning modules.
/// Why it matters: addendum entry 4's screen; the HTTP layer must not offer a way round maker-checker or let a console user publish a position.
#[tokio::test]
async fn the_workbench_over_http() {
    let portal = portal();
    let (status, list) = portal.get("sub-fin1", "/staff/v1/workbench").await;
    assert_eq!((status, &list["parties"][0]["party"]), (StatusCode::OK, &json!("adv-1")));
    assert_eq!(portal.get("sub-sue", "/staff/v1/workbench").await.0, StatusCode::FORBIDDEN);
    assert_eq!(portal.get("sub-fin1", "/staff/v1/workbench/adv-1").await.1["held"]["NZD"], 900_00);
    assert_eq!(portal.get("sub-fin1", "/staff/v1/workbench/nobody").await.0, StatusCode::NOT_FOUND);
    assert_eq!(portal.get("sub-sue", "/staff/v1/workbench/adv-1").await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        portal.get("sub-fin1", "/staff/v1/workbench/adv-1/proof-of-debt").await.1["owed_to_party"]["NZD"],
        900_00
    );
    assert_eq!(
        portal.get("sub-fin1", "/staff/v1/workbench/nobody/proof-of-debt").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(portal.get("sub-sue", "/staff/v1/workbench/adv-1/proof-of-debt").await.0, StatusCode::FORBIDDEN);

    let prepare = json!({ "prepare": { "id": "a1", "party": "adv-1", "currency": "NZD", "action": { "action": "release", "amount_minor": 500_00, "account": "acct-1", "new_account": false }, "reason_code": "licence_restored", "document": "vault:1" } });
    assert_eq!(portal.write("POST", "sub-fin1", "/staff/v1/workbench/actions", prepare).await.0, StatusCode::OK);
    let approve = json!({ "approve": { "id": "a1", "legal": false } });
    let (status, problem) = portal.write("POST", "sub-fin1", "/staff/v1/workbench/actions", approve.clone()).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::FORBIDDEN, Some("not_authorised")));
    let (status, result) = portal.write("POST", "sub-fin2", "/staff/v1/workbench/actions", approve.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["effects"][0]["workbench"]["ready"]["module"], "commission");
    assert_eq!(
        portal.write("POST", "sub-fin2", "/staff/v1/workbench/actions", approve).await.0,
        StatusCode::CONFLICT
    );

    for command in [
        json!({ "publish": { "party": "adv-1", "items": [] } }),
        json!({ "record_status": { "party": "adv-1", "status": "deceased", "effective": "2027-11-15", "source": "x" } }),
        json!({ "resolve": { "id": "a1", "reference": "x", "failed": null } }),
    ] {
        assert_eq!(
            portal.write("POST", "sub-fin1", "/staff/v1/workbench/actions", command).await.0,
            StatusCode::FORBIDDEN
        );
    }
}

/// Purpose: another module acts under its own name only when its token carries that module's role, and reads the requests waiting for it.
/// Why it matters: the module name is the authority for publishing rows and positions; a service able to name any module could publish claims or resolve another module's requests.
#[tokio::test]
async fn other_modules_act_under_their_own_role() {
    let portal = portal();
    let as_module = |roles: &str, module: Option<&str>| {
        let mut headers = vec![
            ("x-dev-subject", "svc".to_string()),
            ("x-dev-roles", roles.to_string()),
            ("idempotency-key", format!("m-{roles}-{module:?}")),
        ];
        headers.extend(module.map(|m| ("x-module", m.to_string())));
        headers
    };
    let rows = json!({ "publish_rows": { "rows": [{ "key": format!("{}/claims/c1", portal.retail), "policy": portal.retail, "kind": "claim", "subject": KIRI, "sensitive": true, "body": { "state": "received" } }], "remove": [] } });
    assert_eq!(
        portal
            .send("POST", "/internal/v1/commands", &as_module("claims", None), Some(rows.clone()))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        portal
            .send("POST", "/internal/v1/commands", &as_module("commission", Some("claims")), Some(rows.clone()))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        portal
            .send(
                "POST",
                "/internal/v1/commands",
                &as_module("portal-gateway", Some("portal-gateway")),
                Some(json!("heartbeat"))
            )
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        portal.send("POST", "/internal/v1/commands", &[], Some(rows.clone())).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (status, result) = portal
        .send("POST", "/internal/v1/commands", &as_module("claims,other", Some("claims")), Some(rows))
        .await;
    assert_eq!((status, &result["accepted"]), (StatusCode::OK, &json!(true)));
    assert_eq!(
        portal.get("sub-kiri", &format!("/app/v1/policies/{}", portal.retail)).await.1["rows"]
            .as_array()
            .unwrap()
            .len(),
        5
    );

    let request = json!({ "id": "r1", "policy": portal.retail, "kind": "notify_claim", "module": "claims" });
    portal.write("POST", "sub-kiri", "/app/v1/requests", request).await;
    let (status, pending) = portal.send("GET", "/internal/v1/pending", &as_module("claims", Some("claims")), None).await;
    assert_eq!((status, &pending[0]["id"]), (StatusCode::OK, &json!("r1")));
    assert_eq!(
        portal
            .send("GET", "/internal/v1/pending", &as_module("claims", Some("commission")), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}

/// Purpose: when the home region is lost the operations console shows which region is down and where the leaders are, and the public status page says in plain words that things are slower and nothing is lost; with no quorum it says services are unavailable.
/// Why it matters: addendum entry 6: customers and operators must be told the truth about a region loss without a sign-in, in words a customer understands.
#[tokio::test]
async fn region_loss_is_shown_to_operators_and_said_plainly_to_customers() {
    let portal = portal();
    let (status, page) = portal.send("GET", "/status.json", &[], None).await;
    assert_eq!(
        (status, &page["level"], &page["headline"]),
        (StatusCode::OK, &json!("operational"), &json!("All services are working normally"))
    );

    let ops = [("x-dev-subject", "ops-1".to_string()), ("x-dev-roles", OPERATIONS.to_string())];
    assert_eq!(
        portal.send("GET", "/ops/v1/regions", &[("x-dev-subject", "sub-sue".to_string())], None).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(portal.send("GET", "/ops/v1/regions", &[], None).await.0, StatusCode::UNAUTHORIZED);
    let (status, console) = portal.send("GET", "/ops/v1/regions", &ops, None).await;
    assert_eq!(
        (status, &console["regions"]["at_home"], &console["channels"]["policies"]),
        (StatusCode::OK, &json!(true), &json!(2))
    );

    // Auckland is lost; Sydney leads everything.
    {
        let mut report = portal.regions.0.lock().unwrap();
        report.regions[0].reachable = 0;
        report.regions[0].leaders = 0;
        report.regions[1].leaders = 4;
        report.at_home = false;
    }
    let (_, page) = portal.send("GET", "/status.json", &[], None).await;
    assert_eq!((&page["level"], &page["headline"]), (&json!("degraded"), &json!("Slower than usual")));
    assert!(page["detail"][1].as_str().unwrap().contains("data centres is unavailable"));
    assert!(page["detail"][2].as_str().unwrap().contains("Nothing has been lost"));
    let (_, console) = portal.send("GET", "/ops/v1/regions", &ops, None).await;
    assert_eq!(
        (
            &console["regions"]["regions"][0]["reachable"],
            &console["regions"]["regions"][1]["leaders"],
            &console["status"]["level"]
        ),
        (&json!(0), &json!(4), &json!("degraded"))
    );
    // The customer's own data is still served while degraded.
    assert_eq!(portal.get("sub-kiri", "/app/v1/me").await.0, StatusCode::OK);

    // No quorum.
    portal.regions.0.lock().unwrap().quorum = false;
    let (_, page) = portal.send("GET", "/status.json", &[], None).await;
    assert_eq!(page["level"], "outage");
    assert!(page["detail"][1].as_str().unwrap().contains("Your cover is not affected"));
}

/// Purpose: every web page is served with a content security policy that forbids framing and outside resources.
/// Why it matters: the pages handle bearer tokens; a page that could be framed or made to load script from elsewhere is how tokens are stolen.
#[tokio::test]
async fn pages_are_served_locked_down() {
    let portal = portal();
    for path in ["/", "/adviser", "/employer", "/staff", "/ops", "/status"] {
        let response = portal
            .app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let csp = response.headers()["content-security-policy"].to_str().unwrap().to_string();
        assert!(
            csp.starts_with("default-src 'none'") && csp.contains("frame-ancestors 'none'") && csp.contains("connect-src 'self';"),
            "{csp}"
        );
        assert!(!csp.contains("unsafe"), "{csp}");
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        assert!(response.headers()["content-type"].to_str().unwrap().starts_with("text/html"));
        // Every inline script on the page is one the policy names by hash, so the browser will run it and nothing else.
        let body = String::from_utf8(response.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
        let inline = body.matches("<script>").count();
        assert_eq!(csp.matches("'sha256-").count(), inline, "{path}");
        assert_eq!(inline, usize::from(path != "/"), "{path}");
    }
    for (path, kind, has) in [
        ("/ui/app.js", "text/javascript", "const portal"),
        ("/ui/app.css", "text/css", "box-sizing"),
        ("/ui/config.json", "application/json", r#"{"mode":"dev"}"#),
    ] {
        let response = portal
            .app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert!(response.headers()["content-type"].to_str().unwrap().starts_with(kind), "{path}");
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        let body = String::from_utf8(response.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
        assert!(body.contains(has), "{path}");
    }
}

/// Purpose: with Entra sign-in the pages are told where to sign in, and may call those authorities and nowhere else.
/// Why it matters: the browser exchanges its sign-in code at the authority; a policy that forgot it would break sign-in, and one that allowed more would let a page send tokens elsewhere.
#[tokio::test]
async fn pages_may_call_the_sign_in_authorities_only() {
    let sign_in = |authority: &str, client: &str| SignIn {
        authority: authority.into(),
        tenant: "t".into(),
        client_id: client.into(),
        scope: "api://portal/.default".into(),
    };
    let ui = UiConfig::Entra {
        customers: sign_in("https://contoso.ciamlogin.com", "spa-c"),
        workforce: sign_in("https://login.microsoftonline.com", "spa-w"),
    };
    let policy = ui.policy(ic_channels::ui::STAFF);
    assert!(
        policy.contains("connect-src 'self' https://contoso.ciamlogin.com https://login.microsoftonline.com;"),
        "{policy}"
    );
    // One authority for both populations is listed once.
    let same = UiConfig::Entra {
        customers: sign_in("https://login.example/", "a"),
        workforce: sign_in("https://login.example/", "b"),
    };
    assert!(same.policy("").contains("connect-src 'self' https://login.example;"));
    assert_eq!(same.policy("<script>never closed"), same.policy(""));
    for bad in ["login.example", "ftp://login.example", "https://", "https://a b", "https://a;script-src *"] {
        assert_eq!(origin(bad), None, "{bad}");
    }
    assert_eq!(origin("http://127.0.0.1:9000/x").as_deref(), Some("http://127.0.0.1:9000"));
    let config = serde_json::to_value(&ui).unwrap();
    assert_eq!(
        (config["mode"].as_str(), config["workforce"]["client_id"].as_str()),
        (Some("entra"), Some("spa-w"))
    );
    assert_eq!(config["customers"]["authority"], "https://contoso.ciamlogin.com");
}

/// Purpose: serve the seeded portal on a real port so the pages can be driven in a browser by `tests/browser/check.js`.
/// Why it matters: the pages are script; only a browser shows whether they run under the content security policy and render what the service returns.
#[tokio::test]
#[ignore = "serves until killed; started by tests/browser/check.js"]
async fn serve_for_a_browser() {
    let addr = std::env::var("IC_BROWSER_ADDR").unwrap_or_else(|_| "127.0.0.1:8099".into());
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    let portal = portal();
    // Kiri appoints the adviser, so the adviser's book has a policy in it.
    let appoint = format!("/app/v1/policies/{}/advisers", portal.retail);
    let (status, _) = portal
        .write("POST", "sub-kiri", &appoint, json!({ "adviser": "adv-1", "engagement": "e1" }))
        .await;
    assert_eq!(status, StatusCode::OK);
    println!("policy {}", portal.retail);
    axum::serve(listener, portal.app.clone()).await.unwrap();
}
