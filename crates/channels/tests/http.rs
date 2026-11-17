//! The portal service over HTTP, against the channel module in process.

mod common;

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use base64::Engine;
use common::*;
use http_body_util::BodyExt;
use ic_api::auth::Auth;
use ic_channels::classes::DataClass;
use ic_channels::complaints::actions as workbench_complaints;
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
    state: Arc<AppState>,
    local: Arc<Local>,
    regions: Arc<Fixed>,
    clock: Arc<AtomicI64>,
    keys: AtomicU64,
    phone: Key,
    retail: PolicyId,
    scheme: PolicyId,
    pricing: Arc<common::pricing::PricingModule>,
    claims: Arc<common::claims::ClaimsCore>,
    reinsurance: Arc<common::reinsurance::ReinsuranceModule>,
    underwriting: Arc<common::underwriting::UnderwritingModule>,
    payments: Arc<common::payments::PaymentsModule>,
    collections: Arc<common::collections::CollectionsModule>,
    correspondence: Arc<common::correspondence::DocumentsModule>,
    wellness: Arc<common::wellness::WellnessModule>,
    master: Arc<common::customers::MasterDouble>,
    commission: Arc<common::commission::CommissionModule>,
    intake: Arc<common::intake::IntakeDouble>,
    documents: Arc<common::documents::DocumentsDouble>,
    group: Arc<common::group::GroupDouble>,
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
    portal_with(|employer| employer)
}

/// The portal, calling the group service `group_service` makes from the employer's double.
fn portal_with(group_service: impl FnOnce(Arc<common::group::GroupDouble>) -> Arc<dyn ic_channels::group::Group>) -> Portal {
    portal_signed_in_by(group_service, Auth::Dev, Auth::Dev)
}

/// The shipped definition libraries, loaded once for every test.
fn shipped_wordings() -> Arc<ic_channels::wordings::Wordings> {
    static LOADED: std::sync::OnceLock<Arc<ic_channels::wordings::Wordings>> = std::sync::OnceLock::new();
    LOADED.get_or_init(|| Arc::new(ic_channels::wordings::Wordings::shipped().unwrap())).clone()
}

/// The portal, checking callers with `customers` and `workforce`.
fn portal_signed_in_by(
    group_service: impl FnOnce(Arc<common::group::GroupDouble>) -> Arc<dyn ic_channels::group::Group>,
    customers: Auth,
    workforce: Auth,
) -> Portal {
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
        ("sub-hr2", "hr-2"),
        ("sub-alice", "alice"),
        ("sub-sue", "sue"),
        ("sub-fin1", "fin-1"),
        ("sub-fin2", "fin-2"),
        ("sub-up1", "up-1"),
        ("sub-up2", "up-2"),
        ("sub-lead", "lead-1"),
        ("sub-front", "front-1"),
        ("sub-cmp1", "cmp-1"),
        ("sub-cmp2", "cmp-2"),
        ("sub-lex", "lex-1"),
        ("sub-con", "con-1"),
        ("sub-gl1", "gl-1"),
        ("sub-gl2", "gl-2"),
        ("sub-gllead", "gl-lead"),
        ("sub-cl1", "cl-1"),
        ("sub-cllead", "cl-lead"),
        ("sub-clnew", "cl-new"),
        ("sub-ri1", "ri-1"),
        ("sub-ri2", "ri-2"),
        ("sub-riview", "ri-view"),
        ("sub-aud", "aud-1"),
        ("sub-wbcon", "wb-con"),
        ("sub-rec", "rec-1"),
        ("sub-dadmin", "da-1"),
        ("sub-dview", "dv-1"),
        ("sub-amy", "amy"),
        ("sub-abe", "abe"),
        ("sub-sam", "sam"),
        ("sub-cat", "cat"),
        ("sub-fap", "fap-1"),
        ("sub-paymk", "pay-mk"),
        ("sub-payap", "pay-ap"),
        ("sub-paypm", "pay-pm"),
        ("sub-paybad", "pay-bad"),
        ("sub-payview", "pay-view"),
        ("sub-paycmp", "pay-cmp"),
        ("sub-colof", "col-of"),
        ("sub-colof2", "col-of2"),
        ("sub-colhs", "col-hs"),
        ("sub-colcfg", "col-cfg"),
        ("sub-colview", "col-view"),
        ("sub-svc", "svc-1"),
        ("sub-docof", "doc-of"),
        ("sub-docla", "doc-la"),
        ("sub-docview", "doc-view"),
        ("sub-grp1", "grp-1"),
        ("sub-grpview", "grp-view"),
        ("sub-hcs", "hcs-1"),
        ("sub-nb1", "nb-1"),
        ("sub-cd1", "cd-1"),
        ("sub-prv", "prv-1"),
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
                template(
                    "administrator",
                    &[staff_actions::MANAGE_STAFF, ic_channels::state::assisted::APPROVE],
                    &[DataClass::Ordinary],
                ),
                template(
                    "service",
                    &[
                        "policy.view",
                        ic_channels::state::assisted::ENTER,
                        staff_actions::FIND_CUSTOMER,
                        ic_channels::vitality::actions::VIEW,
                    ],
                    &[DataClass::Ordinary, DataClass::Personal],
                ),
                template(
                    "hardship",
                    &["policy.view", staff_actions::VULNERABILITY_KIND],
                    &[DataClass::Ordinary, DataClass::Personal],
                ),
                template(
                    "finance",
                    &[workbench::actions::VIEW, workbench::actions::PREPARE, workbench::actions::APPROVE],
                    &[DataClass::Ordinary, DataClass::Financial],
                ),
                template(
                    "unit-pricing",
                    &[ic_channels::pricing::actions::PREPARE, ic_channels::pricing::actions::APPROVE],
                    &[DataClass::Ordinary],
                ),
                template(
                    "unit-pricing-lead",
                    &[ic_channels::pricing::actions::VIEW, ic_channels::pricing::actions::LEAD],
                    &[DataClass::Ordinary],
                ),
                template(
                    "frontline",
                    &[workbench_complaints::RECORD, staff_actions::FIND_CUSTOMER],
                    &[DataClass::Ordinary, DataClass::Personal],
                ),
                template(
                    "complaints-team",
                    &[workbench_complaints::HANDLE, workbench_complaints::TRAINED, staff_actions::FIND_CUSTOMER],
                    &[DataClass::Ordinary, DataClass::Personal],
                ),
                Template {
                    default_profile: "junior".into(),
                    ..template(
                        "complaints-junior",
                        &[workbench_complaints::HANDLE, staff_actions::FIND_CUSTOMER],
                        &[DataClass::Ordinary, DataClass::Personal],
                    )
                },
                template(
                    "complaints-legal",
                    &[workbench_complaints::LEGAL, staff_actions::FIND_CUSTOMER],
                    &[DataClass::Ordinary, DataClass::Personal],
                ),
                template(
                    "conduct",
                    &[workbench_complaints::CONDUCT, staff_actions::FIND_CUSTOMER],
                    &[DataClass::Ordinary, DataClass::Personal],
                ),
                template("ledger-operator", &[ic_channels::finance::actions::OPERATE], &[DataClass::Financial]),
                template("ledger-approver", &[ic_channels::finance::actions::APPROVE], &[DataClass::Financial]),
                template(
                    "ledger-lead",
                    &[ic_channels::finance::actions::LEAD, ic_channels::finance::actions::ACTUARY],
                    &[DataClass::Financial],
                ),
                template("ledger-reader", &[ic_channels::finance::actions::VIEW], &[DataClass::Financial]),
                template(
                    "claims",
                    &[ic_channels::claims::actions::WORK, "policy.view", staff_actions::FIND_CUSTOMER],
                    &[DataClass::InsuranceSensitive, DataClass::Personal],
                ),
                template("ri-ops", &[ic_channels::reinsurance::actions::OPERATE], &[DataClass::Financial]),
                template(
                    "ri-lead",
                    &[ic_channels::reinsurance::actions::OPERATE, ic_channels::reinsurance::actions::APPROVE],
                    &[DataClass::Financial],
                ),
                template("ri-reader", &[ic_channels::reinsurance::actions::VIEW], &[DataClass::Financial]),
                template("records", &[ic_channels::documents::actions::DOWNLOAD], &[DataClass::Ordinary]),
                template(
                    "distribution-admin",
                    &[ic_channels::commission::actions::VIEW, ic_channels::commission::actions::STRUCTURE],
                    &[DataClass::Ordinary],
                ),
                template("underwriter", &[ic_channels::underwriting::actions::WORK], &[DataClass::InsuranceSensitive]),
                template(
                    "senior-underwriter",
                    &[ic_channels::underwriting::actions::WORK, ic_channels::underwriting::actions::SENIOR],
                    &[DataClass::InsuranceSensitive],
                ),
                template("payments-maker", &[ic_channels::payments::actions::MAKER], &[DataClass::Financial]),
                template("payments-approver", &[ic_channels::payments::actions::APPROVER], &[DataClass::Financial]),
                template("payee-maintainer", &[ic_channels::payments::actions::PAYEES], &[DataClass::Financial]),
                // Both: the payments module refuses anything this person sends.
                template(
                    "payments-conflicted",
                    &[ic_channels::payments::actions::APPROVER, ic_channels::payments::actions::PAYEES],
                    &[DataClass::Financial],
                ),
                template("payments-reader", &[ic_channels::payments::actions::VIEW], &[DataClass::Financial]),
                template("payments-compliance", &[ic_channels::payments::actions::COMPLIANCE], &[DataClass::Financial]),
                template("collections-officer", &[ic_channels::collections::actions::OFFICER], &[DataClass::Financial]),
                template(
                    "collections-hardship",
                    &[ic_channels::collections::actions::HARDSHIP],
                    &[DataClass::Financial, DataClass::InsuranceSensitive],
                ),
                template(
                    "collections-config",
                    &[ic_channels::collections::actions::CONFIGURATION],
                    &[DataClass::Ordinary],
                ),
                template("collections-reader", &[ic_channels::collections::actions::VIEW], &[DataClass::Financial]),
                template("servicing-person", &[ic_channels::servicing::actions::CARRY_OUT], &[DataClass::Ordinary]),
                template(
                    "documents-officer",
                    &[
                        ic_channels::correspondence::actions::LEGAL,
                        ic_channels::correspondence::actions::REMEDIATION,
                        ic_channels::correspondence::actions::TRIGGERS,
                        ic_channels::correspondence::actions::DELIVERY,
                        ic_channels::correspondence::actions::CONFIGURATION,
                        ic_channels::correspondence::actions::APPROVE_PRODUCT,
                    ],
                    &[DataClass::Ordinary],
                ),
                template(
                    "documents-legal-approver",
                    &[ic_channels::correspondence::actions::APPROVE_LEGAL],
                    &[DataClass::Ordinary],
                ),
                template("documents-reader", &[ic_channels::correspondence::actions::VIEW], &[DataClass::Ordinary]),
                // Group administrators find a scheme's administrator by name, so they may search the customer master.
                template(
                    "group-admin",
                    &[ic_channels::group::actions::ADMIN, staff_actions::FIND_CUSTOMER],
                    &[DataClass::Ordinary, DataClass::Personal],
                ),
                template("group-reader", &[ic_channels::group::actions::VIEW], &[DataClass::Ordinary]),
                template("commission-reader", &[ic_channels::commission::actions::VIEW], &[DataClass::Ordinary]),
                template(
                    "new-business",
                    &[staff_actions::QUOTE, staff_actions::CORE_POLICY],
                    &[DataClass::Ordinary, DataClass::Personal, DataClass::InsuranceSensitive],
                ),
                template(
                    "customer-desk",
                    &[
                        staff_actions::FIND_CUSTOMER,
                        staff_actions::VIEW_CUSTOMER,
                        staff_actions::CREATE_CUSTOMER,
                        staff_actions::CHANGE_CUSTOMER,
                        staff_actions::MERGE_CUSTOMERS,
                        staff_actions::EXPORT_CUSTOMER,
                    ],
                    &[DataClass::Ordinary, DataClass::Personal],
                ),
                template(
                    "privacy",
                    &[staff_actions::AUDIT_CUSTOMERS, staff_actions::SETTLE_CAPACITY],
                    &[DataClass::Ordinary, DataClass::Personal],
                ),
                template(
                    "conduct-officer",
                    &[workbench::actions::VIEW, workbench::actions::APPROVE_CONDUCT],
                    &[DataClass::Ordinary, DataClass::Financial],
                ),
            ],
            profiles: vec![
                Profile {
                    id: "p".into(),
                    version: 1,
                    limits: [
                        (workbench::LIMIT.to_string(), 100_000_00),
                        (ic_channels::complaints::AUTHORITY.to_string(), 3),
                        (ic_channels::state::assisted::LIMIT.to_string(), 500_00),
                    ]
                    .into(),
                },
                Profile {
                    id: "junior".into(),
                    version: 1,
                    limits: [(ic_channels::complaints::AUTHORITY.to_string(), 1)].into(),
                },
            ],
            admin: p("alice"),
            admin_templates: vec!["administrator".into()],
        }),
    )
    .unwrap();
    for (who, role) in [
        ("sue", "service"),
        ("fin-1", "finance"),
        ("fin-2", "finance"),
        ("up-1", "unit-pricing"),
        ("up-2", "unit-pricing"),
        ("lead-1", "unit-pricing-lead"),
        ("front-1", "frontline"),
        ("cmp-1", "complaints-team"),
        ("cmp-2", "complaints-junior"),
        ("lex-1", "complaints-legal"),
        ("con-1", "conduct"),
        ("gl-1", "ledger-operator"),
        ("gl-2", "ledger-approver"),
        ("gl-lead", "ledger-lead"),
        ("aud-1", "ledger-reader"),
        ("cl-1", "claims"),
        ("cl-lead", "claims"),
        // Given the claims action here but not yet in the claims module's authority book.
        ("cl-new", "claims"),
        ("ri-1", "ri-ops"),
        ("ri-2", "ri-lead"),
        ("ri-view", "ri-reader"),
        ("wb-con", "conduct-officer"),
        ("rec-1", "records"),
        ("da-1", "distribution-admin"),
        ("dv-1", "commission-reader"),
        ("amy", "underwriter"),
        ("abe", "underwriter"),
        ("sam", "senior-underwriter"),
        ("cat", "senior-underwriter"),
        ("pay-mk", "payments-maker"),
        ("pay-ap", "payments-approver"),
        ("pay-pm", "payee-maintainer"),
        ("pay-bad", "payments-conflicted"),
        ("pay-view", "payments-reader"),
        ("pay-cmp", "payments-compliance"),
        ("col-of", "collections-officer"),
        ("col-of2", "collections-officer"),
        ("col-hs", "collections-hardship"),
        ("col-cfg", "collections-config"),
        ("col-view", "collections-reader"),
        ("svc-1", "servicing-person"),
        ("doc-of", "documents-officer"),
        ("doc-la", "documents-legal-approver"),
        ("doc-view", "documents-reader"),
        ("grp-1", "group-admin"),
        ("grp-view", "group-reader"),
        ("hcs-1", "hardship"),
        ("nb-1", "new-business"),
        ("cd-1", "customer-desk"),
        ("prv-1", "privacy"),
    ] {
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
                status: None,
            }],
            documents: vec![],
            cases: vec![],
            conduct: None,
        }),
    )
    .unwrap();
    // The relay reports the stream current once setup is done, as it does while the portal runs.
    w.send(module(ic_channels::state::EVENT_RELAY), Command::Heartbeat).unwrap();
    let pricing = Arc::new(common::pricing::seeded(w.at));
    let complaints = Arc::new(common::complaints::seeded(w.at));
    let finance = Arc::new(common::finance::seeded());
    let claims = Arc::new(common::claims::seeded());
    let reinsurance = Arc::new(common::reinsurance::seeded());
    let underwriting = Arc::new(common::underwriting::seeded());
    let payments = Arc::new(common::payments::seeded());
    let collections = Arc::new(common::collections::seeded());
    let correspondence = Arc::new(common::correspondence::seeded());
    let wellness = Arc::new(common::wellness::seeded(&retail));
    let commission = Arc::new(common::commission::seeded());
    let intake = Arc::new(common::intake::IntakeDouble::default());
    let documents = Arc::new(common::documents::DocumentsDouble::default());
    let group = Arc::new(common::group::GroupDouble::with("harbour", &scheme.0));
    let master = Arc::new(common::customers::MasterDouble::with(
        KIRI,
        json!({ "given_name": "Kiri", "family_name": "Parata", "date_of_birth": "1988-04-12", "email": "kiri@example.nz" }),
    ));
    let clock = Arc::new(AtomicI64::new(w.at + 10));
    let local = Arc::new(Local::new(w.ch));
    let regions = Arc::new(Fixed(Mutex::new(healthy())));
    let tick = clock.clone();
    let state = Arc::new(AppState {
        wordings: shipped_wordings(),
        public_url: None,
        returns: Arc::new(ic_channels::finance::Returns::load(ic_channels::finance::SHIPPED_RETURNS).unwrap()),
        channel: local.clone(),
        customers,
        workforce,
        clock: Arc::new(move || tick.load(Ordering::SeqCst)),
        regions: regions.clone(),
        ui: UiConfig::Dev,
        pricing: pricing.clone(),
        complaints: complaints.clone(),
        commission: commission.clone(),
        finance: finance.clone(),
        claims: claims.clone(),
        reinsurance: reinsurance.clone(),
        payments: payments.clone(),
        collections: collections.clone(),
        correspondence: correspondence.clone(),
        wellness: wellness.clone(),
        underwriting: underwriting.clone(),
        master: Some(master.clone()),
        intake: Some(intake.clone()),
        documents: Some(documents.clone()),
        group: Some(group_service(group.clone())),
    });
    Portal {
        app: router(state.clone()),
        state,
        local,
        regions,
        clock,
        keys: AtomicU64::new(0),
        phone,
        retail,
        scheme,
        pricing,
        claims,
        reinsurance,
        underwriting,
        payments,
        collections,
        correspondence,
        wellness,
        master,
        commission,
        intake,
        documents,
        group,
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

    /// A request of Kiri's with her phone's signature over exactly it, as the app sends one
    /// that gives up cover, changes what is collected or moves money.
    async fn signed(&self, mut request: Value) -> Value {
        let policy = request["policy"].as_str().map(ic_core::PolicyId::from);
        let action = ic_channels::http::request_action(
            request["id"].as_str().unwrap(),
            policy.as_ref(),
            request["kind"].as_str().unwrap(),
            &request.get("payload").cloned().unwrap_or(Value::Null),
        );
        let ask = json!({ "action": action }).to_string();
        let headers = self.proof(&self.phone, "POST", "/app/v1/challenges", &ask);
        let (status, challenge) = self
            .send("POST", "/app/v1/challenges", &headers, Some(serde_json::from_str(&ask).unwrap()))
            .await;
        assert_eq!(status, StatusCode::OK, "{challenge}");
        let message = format!(
            "{}\n{}\n{}",
            challenge["challenge"].as_str().unwrap(),
            challenge["nonce"].as_str().unwrap(),
            challenge["action_hash"].as_str().unwrap()
        );
        request["signed"] = json!({ "challenge": challenge["challenge"], "signature": self.phone.sign(message.as_bytes()) });
        request
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
    // The service's clock, which a browser dates its device proofs by.
    assert_eq!(me["now"], portal.now());

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

/// Purpose: under Entra sign-in, when a token was issued comes only from the verified token:
/// a token issued before the identity service closed the person's sessions is refused even
/// when the request also carries `x-dev-issued` with a later time, and a token issued after
/// is accepted. A token that does not say when it was issued counts as the oldest possible.
/// Why it matters: "sign out everywhere", staff offboarding and recovering from a lost phone
/// all work by refusing tokens issued before a moment. If a header the caller writes could
/// move a token's issue time, anyone holding a stolen token could walk past all three.
#[tokio::test]
async fn under_entra_only_the_token_says_when_it_was_issued() {
    use ic_entra::testing::{sign, Tenant};
    let tenant = Tenant::start("customer-tenant", "api://insurance-portal").await;
    let entra = || Auth::entra_at(&tenant.authority, &tenant.tenant, &tenant.audience);
    let portal = portal_signed_in_by(|employer| employer, entra(), entra());
    let token = |issued: Option<i64>| {
        let mut claims = tenant.claims("sub-kiri", &[]);
        match issued {
            Some(at) => claims["iat"] = json!(at),
            None => drop(claims.as_object_mut().unwrap().remove("iat")),
        }
        format!("Bearer {}", sign(&claims))
    };
    let now = portal.now();
    let old = token(Some(now - 5));
    assert_eq!(
        portal.send("GET", "/app/v1/me", &[("authorization", old.clone())], None).await.0,
        StatusCode::OK
    );

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
    let revoked = |problem: (StatusCode, Value)| (problem.0, problem.1["title"].as_str().map(str::to_string));
    let refused = (StatusCode::UNAUTHORIZED, Some("session_revoked".to_string()));
    assert_eq!(
        revoked(portal.send("GET", "/app/v1/me", &[("authorization", old.clone())], None).await),
        refused
    );
    let lying = [
        ("authorization", old),
        ("x-dev-issued", (now + 60).to_string()),
        ("x-dev-subject", "sub-kiri".to_string()),
    ];
    assert_eq!(revoked(portal.send("GET", "/app/v1/me", &lying, None).await), refused);
    assert_eq!(
        revoked(portal.send("GET", "/app/v1/me", &[("authorization", token(None))], None).await),
        refused
    );
    assert_eq!(
        portal.send("GET", "/app/v1/me", &[("authorization", token(Some(now + 1)))], None).await.0,
        StatusCode::OK
    );
    // Development headers alone are no sign-in at all.
    assert_eq!(portal.get("sub-kiri", "/app/v1/me").await.0, StatusCode::UNAUTHORIZED);
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

/// Purpose: a payment the customer asks for is refused unless their enrolled device signed exactly that payment, and a refused one
/// leaves no request behind; signed over a fresh challenge it is accepted; and a signature made for another amount is not enough.
/// Why it matters: with no signal the app shows what the device holds but cannot get a challenge, so it cannot sign. A payment keyed
/// in offline and sent later unsigned, or one whose amount changed after signing, must never reach the payments module as the
/// customer's instruction.
#[tokio::test]
async fn a_payment_is_taken_only_signed_on_the_customers_device() {
    use ic_channels::http::request_action;
    let portal = portal();
    let policy = portal.retail.clone();
    let payload = json!({ "amount_minor": 120_00, "currency": "NZD" });
    let payment = |id: &str| json!({ "id": id, "policy": policy, "kind": "make_payment", "module": "payments", "payload": payload });
    let (status, problem) = portal.write("POST", "sub-kiri", "/app/v1/requests", payment("pay-1")).await;
    assert_eq!(
        (status, problem["title"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("step_up_required")),
        "{problem}"
    );
    assert_eq!(portal.get("sub-kiri", "/app/v1/requests").await.1, json!([]), "a refused payment was kept");

    // Connected: the device asks for a challenge over this payment and signs it.
    let sign = |action: Value| {
        let portal = &portal;
        async move {
            let ask = json!({ "action": action }).to_string();
            let headers = portal.proof(&portal.phone, "POST", "/app/v1/challenges", &ask);
            let (status, challenge) = portal
                .send("POST", "/app/v1/challenges", &headers, Some(serde_json::from_str(&ask).unwrap()))
                .await;
            assert_eq!(status, StatusCode::OK, "{challenge}");
            let message = format!(
                "{}\n{}\n{}",
                challenge["challenge"].as_str().unwrap(),
                challenge["nonce"].as_str().unwrap(),
                challenge["action_hash"].as_str().unwrap()
            );
            json!({ "challenge": challenge["challenge"], "signature": portal.phone.sign(message.as_bytes()) })
        }
    };
    // Signed for a smaller amount, then sent for this one.
    let other = sign(request_action(
        "pay-1",
        Some(&policy),
        "make_payment",
        &json!({ "amount_minor": 1_00, "currency": "NZD" }),
    ))
    .await;
    let mut altered = payment("pay-1");
    altered["signed"] = other;
    let (status, problem) = portal.write("POST", "sub-kiri", "/app/v1/requests", altered).await;
    assert_eq!(
        (status, problem["title"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("step_up_failed")),
        "{problem}"
    );

    let mut signed = payment("pay-1");
    signed["signed"] = sign(request_action("pay-1", Some(&policy), "make_payment", &payload)).await;
    let (status, done) = portal.write("POST", "sub-kiri", "/app/v1/requests", signed).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{done}");
    let mine = portal.get("sub-kiri", "/app/v1/requests").await.1;
    assert_eq!((mine.as_array().unwrap().len(), mine[0]["status"].as_str()), (1, Some("pending")), "{mine}");

    // A change that moves no money still needs no signature from the owner.
    let plain = json!({ "id": "r-plain", "policy": policy, "kind": "decline_indexation", "module": "policy-servicing", "payload": {} });
    assert_eq!(portal.write("POST", "sub-kiri", "/app/v1/requests", plain).await.0, StatusCode::ACCEPTED);
}

/// Purpose: the customer appoints an adviser, the adviser prepares a change, and the customer approves it only with a signature from their enrolled device over exactly that approval. Once approved, the request is routed by the customer master's answer for the customer, not the adviser.
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

    // The adviser's provider sees the adviser and the policy; the adviser has no practice of their own.
    let (status, practice) = portal.get("sub-fap", "/adviser/v1/practice").await;
    assert_eq!(
        (status, &practice["advisers"][0]["adviser"], &practice["policies"]),
        (StatusCode::OK, &json!("adv-1"), &json!(1)),
        "{practice}"
    );
    assert_eq!(portal.get("sub-adv", "/adviser/v1/practice").await.1["advisers"], json!([]));

    // A customer who is no adviser is refused the portal, not shown an empty book.
    let (status, problem) = portal.get("sub-kiri", "/adviser/v1/book").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{problem}");
    assert_eq!(portal.get("sub-fap", "/adviser/v1/book").await.0, StatusCode::OK);

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

    // Kiri needs a person to handle her changes. The adviser prepared this one; once she approves it, it is routed as hers.
    let routed = || async { portal.local.with(|state| state.data.requests.get("r1").map(|r| r.route_to_person)) };
    assert_eq!(routed().await, Some(false));
    portal.master.vulnerable.lock().unwrap().push(KIRI.to_string());
    let (status, result) = portal.write("POST", "sub-kiri", "/app/v1/requests/r1/approve", signed.clone()).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert!(result["effects"].as_array().unwrap().iter().any(|e| e.get("request_ready").is_some()));
    assert_eq!(routed().await, Some(true));
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

/// Purpose: over HTTP the policyholder lets a relative look and ends it; the relative reads the policy and can neither let anyone else in, ask for a change, appoint or remove an adviser, give consent nor approve a request.
/// Why it matters: this is the route the app's "share with family" screen calls. A relative who could extend their own access or submit a cancellation would be a second policyholder nobody agreed to.
#[tokio::test]
async fn a_relative_is_let_in_to_look_and_no_more() {
    let portal = portal();
    let policy = &portal.retail;
    for (subject, who) in [("sub-daughter", "daughter")] {
        let bind = json!({ "directory": { "bind_subject": { "subject": subject, "party": who } } });
        let identity = [
            ("x-dev-subject", "svc".to_string()),
            ("x-dev-roles", "identity".to_string()),
            ("x-module", "identity".to_string()),
            ("idempotency-key", format!("bind-{who}")),
        ];
        assert_eq!(portal.send("POST", "/internal/v1/commands", &identity, Some(bind)).await.0, StatusCode::OK);
    }
    let path = format!("/app/v1/policies/{policy}/delegates/daughter");
    assert_eq!(portal.get("sub-daughter", &format!("/app/v1/policies/{policy}")).await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        portal.write("PUT", "sub-daughter", &path, json!({ "access": "viewer" })).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        portal.write("PUT", "sub-kiri", &path, json!({ "access": "owner" })).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(portal.write("PUT", "sub-kiri", &path, json!({ "access": "viewer" })).await.0, StatusCode::OK);

    let (status, view) = portal.get("sub-daughter", &format!("/app/v1/policies/{policy}")).await;
    assert_eq!((status, &view["relationships"]), (StatusCode::OK, &json!(["viewer"])));
    assert_eq!(
        portal.get("sub-daughter", "/app/v1/me").await.1["policies"][0]["relationships"],
        json!(["viewer"])
    );
    let request = json!({ "id": "r-d", "policy": policy, "kind": "cancel_policy", "module": "policy-servicing" });
    assert_eq!(portal.write("POST", "sub-daughter", "/app/v1/requests", request).await.0, StatusCode::FORBIDDEN);
    let friend = format!("/app/v1/policies/{policy}/delegates/friend");
    assert_eq!(
        portal.write("PUT", "sub-daughter", &friend, json!({ "access": "viewer" })).await.0,
        StatusCode::FORBIDDEN
    );
    // Every other command on the policy is refused to a viewer too (AUD-011).
    let before = portal.get("sub-kiri", &format!("/app/v1/policies/{policy}")).await.1;
    for (method, path, body) in [
        (
            "POST",
            format!("/app/v1/policies/{policy}/advisers"),
            json!({ "adviser": "adv-1", "engagement": "e1" }),
        ),
        ("DELETE", format!("/app/v1/policies/{policy}/advisers/adv-1"), json!({})),
        (
            "PUT",
            format!("/app/v1/policies/{policy}/consent"),
            json!({ "party": "daughter", "given": true }),
        ),
        ("POST", "/app/v1/requests/r-none/approve".to_string(), json!({})),
    ] {
        let (status, answer) = portal.write(method, "sub-daughter", &path, body).await;
        assert!(
            status == StatusCode::FORBIDDEN || status == StatusCode::NOT_FOUND || status == StatusCode::BAD_REQUEST,
            "{method} {path}: {status} {answer}"
        );
    }
    assert_eq!(portal.get("sub-kiri", &format!("/app/v1/policies/{policy}")).await.1, before, "nothing changed");

    assert_eq!(portal.write("PUT", "sub-kiri", &path, json!({ "access": null })).await.0, StatusCode::OK);
    assert_eq!(portal.get("sub-daughter", &format!("/app/v1/policies/{policy}")).await.0, StatusCode::NOT_FOUND);
}

/// Purpose: an employer administrator sees their schemes and manages the scheme's administrators and its second-administrator rule; nobody else can.
/// Why it matters: the employer portal is run by the employer's own staff; a member or an outsider changing who administers a scheme could alter everyone's cover. A change to the members sent as a request is refused, since nothing acts on one (found by the scenario run, SCN-05, F-SCN05-5).
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
    // A change to the members goes to the group service, not into a request
    // nobody acts on (found by the scenario run, SCN-05, F-SCN05-5).
    let request = json!({ "id": "r1", "policy": scheme, "kind": "remove_member", "module": "group-admin" });
    let (status, result) = portal.write("POST", "sub-hr", "/app/v1/requests", request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(result["detail"].as_str().unwrap().contains("Changes to members"), "{result}");
}

/// Purpose: the staff console is for staff, shows a policy only for a stated case, explains a refusal, and passes staff access commands to the module under the caller's own identity.
/// Why it matters: staff see more than any customer; every view must go through the four questions, and the reason for a refusal is what lets someone ask their manager for the right access.
#[tokio::test]
async fn the_staff_console_asks_the_four_questions() {
    let portal = portal();
    let policy = &portal.retail;
    // Someone signed in to the workforce but not on the staff directory is told so, not refused:
    // every workforce page asks, and an operations engineer's must still open.
    let (status, me) = portal.get("sub-kiri", "/staff/v1/me").await;
    assert_eq!((status, &me), (StatusCode::OK, &json!({ "staff": false })));
    // So is a workforce sign-in the insurer has never registered (an engineer bound to no party).
    let (status, me) = portal.get("sub-nobody-registered", "/staff/v1/me").await;
    assert_eq!((status, &me), (StatusCode::OK, &json!({ "staff": false })));
    let (status, problem) = portal.get("sub-kiri", &format!("/staff/v1/policies/{policy}?case=case-1&scope=")).await;
    assert_eq!(
        (status, problem["detail"].as_str().unwrap_or_default().contains("no staff role")),
        (StatusCode::FORBIDDEN, true)
    );
    // Your own access: your roles, the profile that sets your limits, and (below) your access log with each refusal's reason.
    let me = portal.get("sub-sue", "/staff/v1/me").await.1;
    assert_eq!(
        (&me["staff"], &me["templates"], &me["profile"]["id"], &me["team"]),
        (&json!(true), &json!(["service"]), &json!("p"), &json!("ops")),
        "{me}"
    );

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
    // No role allows it: the refusal names the action, so the grant asked for is the right one
    // (found by the scenario run, F-SCN16-6).
    let (status, problem) = portal.get("sub-fin1", &format!("/staff/v1/policies/{policy}?case=c")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(problem["detail"].as_str().unwrap().contains("The action you need is "), "{problem}");

    // Administration.
    assert_eq!(portal.get("sub-alice", "/staff/v1/admin").await.1["members"].as_array().unwrap().len(), 51);
    assert_eq!(portal.get("sub-sue", "/staff/v1/admin").await.0, StatusCode::FORBIDDEN);
    // A profile change waits for its second person and is listed for the administrator (SCN2-24, F2-24-1),
    // and a command that is not understood is answered with the commands there are (F2-24-4).
    let profile = json!({ "set_profile": { "party": "sue", "profile": null, "approver": null } });
    let (status, proposed) = portal.write("POST", "sub-alice", "/staff/v1/access", profile).await;
    assert_eq!(status, StatusCode::OK, "{proposed}");
    let admin = portal.get("sub-alice", "/staff/v1/admin").await.1;
    assert_eq!(admin["pending_changes"][0]["proposed_by"], "alice", "{admin}");
    assert_eq!(admin["changes_i_may_approve"], json!([]));
    let (status, problem) = portal.write("POST", "sub-alice", "/staff/v1/access", json!({ "emply": {} })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(problem["detail"].as_str().unwrap().contains("- employ: "), "{problem}");
    let path = json!({ "assign_path": { "party": "sue", "scope": "queue:claims" } });
    assert_eq!(portal.write("POST", "sub-sue", "/staff/v1/access", path.clone()).await.0, StatusCode::FORBIDDEN);
    assert_eq!(portal.write("POST", "sub-alice", "/staff/v1/access", path).await.0, StatusCode::OK);
    assert_eq!(
        portal.get("sub-sue", &format!("/staff/v1/policies/{policy}?case=c&scope=queue:claims")).await.0,
        StatusCode::OK
    );

    // A request for a customer on the phone says why and names the case, and waits for the customer.
    let request = json!({ "id": "r1", "policy": policy, "kind": "change_frequency", "module": "policy-servicing",
        "reason": "customer rang", "case": "case-8" });
    assert_eq!(
        portal.write("POST", "sub-kiri", "/staff/v1/requests", request.clone()).await.0,
        StatusCode::FORBIDDEN
    );
    let mut unexplained = request.clone();
    unexplained["reason"] = json!("");
    assert_eq!(
        portal.write("POST", "sub-sue", "/staff/v1/requests", unexplained).await.1["title"],
        "bad_request"
    );
    assert_eq!(portal.write("POST", "sub-sue", "/staff/v1/requests", request).await.0, StatusCode::ACCEPTED);
    let r1 = portal.local.with(|s| serde_json::to_value(&s.data.requests["r1"]).unwrap());
    assert_eq!(
        (&r1["status"], &r1["from"], &r1["assisted"]["case"]),
        (&json!("awaiting_approval"), &json!("customer"), &json!("case-8"))
    );
    // Above her limit it waits for a second member of staff first, who is not her.
    let large = json!({ "id": "r2", "policy": policy, "kind": "change_sum_insured", "module": "policy-servicing",
        "payload": { "amount_minor": 900_00 }, "reason": "customer rang", "case": "case-8" });
    assert_eq!(portal.write("POST", "sub-sue", "/staff/v1/requests", large).await.0, StatusCode::ACCEPTED);
    assert_eq!(
        portal.local.with(|s| serde_json::to_value(&s.data.requests["r2"]).unwrap())["from"],
        "second_staff"
    );
    // It is on the second person's queue with what it is and why it is there, and not on the queue of the person who entered it.
    let queue = portal.get("sub-alice", "/staff/v1/requests").await.1;
    assert_eq!(queue["may_approve"], true);
    let row = &queue["rows"][0];
    assert_eq!(
        (
            row["id"].as_str(),
            row["entered_by"].as_str(),
            row["above_limit"].as_bool(),
            row["case"].as_str(),
            row["payload"]["amount_minor"].as_i64()
        ),
        (Some("r2"), Some("sue"), Some(true), Some("case-8"), Some(900_00)),
        "{queue}"
    );
    assert_eq!(queue["rows"].as_array().unwrap().len(), 1, "a change waiting for the customer is not on it");
    assert_eq!(portal.get("sub-sue", "/staff/v1/requests").await.1, json!({ "may_approve": false, "rows": [] }));
    let second = "/staff/v1/requests/r2/second-approval";
    assert_eq!(portal.write("POST", "sub-sue", second, json!({})).await.1["title"], "maker_checker");
    assert_eq!(portal.write("POST", "sub-alice", second, json!({})).await.0, StatusCode::ACCEPTED);
    assert_eq!(portal.local.with(|s| serde_json::to_value(&s.data.requests["r2"]).unwrap())["from"], "customer");
    assert_eq!(
        portal.get("sub-alice", "/staff/v1/requests").await.1["rows"],
        json!([]),
        "once agreed it leaves the queue"
    );
}

/// Purpose: staff find a customer from the name and birth date a caller gives, or a policy from its number typed in part; each
/// person comes with the policies they own, the search and each person listed are asked of the four questions under the case in
/// hand, someone the member of staff is connected to is listed as closed, and a customer master that cannot be reached still
/// leaves policy numbers searchable.
/// Why it matters: a caller knows their name and birth date, never a party or policy reference. Without this the console asks
/// for references nobody on a call can know; with it done carelessly, anyone with a staff sign-in could browse every customer.
#[tokio::test]
async fn staff_find_customers_and_policies_from_what_they_know() {
    let portal = portal();
    let policy = portal.retail.clone();
    portal.master.add(
        "party-77",
        json!({ "given_name": "Rewi", "family_name": "Parata", "date_of_birth": "1950-01-01" }),
    );
    let find = |q: &str| format!("/staff/v1/find?q={}&case=CALL-1", q.replace(' ', "%20").replace('/', "%2F"));

    // Without a case there is no search: the four questions ask why.
    let (status, problem) = portal.get("sub-sue", "/staff/v1/find?q=Kiri%20Parata").await;
    assert_eq!(
        (status, problem["detail"].as_str().unwrap().contains("purpose")),
        (StatusCode::FORBIDDEN, true),
        "{problem}"
    );
    assert_eq!(portal.get("sub-sue", &find("K")).await.0, StatusCode::BAD_REQUEST);

    let (status, found) = portal.get("sub-sue", &find("Kiri Parata 12/4/1988")).await;
    assert_eq!(status, StatusCode::OK, "{found}");
    let people = found["people"].as_array().unwrap();
    assert_eq!(people.len(), 2, "both Paratas are scored: {found}");
    let kiri = people.iter().find(|p| p["party"] == KIRI).unwrap();
    assert_eq!(
        (kiri["name"].as_str(), kiri["born"].as_str(), kiri["strong"].as_bool()),
        (Some("Kiri Parata"), Some("1988-04-12"), Some(true))
    );
    assert_eq!(
        (kiri["policies"][0]["policy"].as_str(), kiri["policies"][0]["as"].as_str()),
        (Some(policy.0.as_str()), Some("owner"))
    );
    let rewi = people.iter().find(|p| p["party"] == "party-77").unwrap();
    assert_eq!((rewi["name"].as_str(), rewi["policies"].clone()), (Some("Rewi Parata"), json!([])));
    assert_eq!(found["policies"], json!([]), "two words are not a policy number");
    // A family name alone that matches nobody says what to add (F-11-8, found by the scenario run).
    let (_, lone) = portal.get("sub-sue", &find("Nobodyhere")).await;
    assert_eq!(
        lone["hint"], "nobody matched the family name alone: add the given name or the date of birth",
        "{lone}"
    );
    // The master was asked to search with the probe the words make, as staff.
    let probe = portal.master.desks.lock().unwrap().iter().find_map(|(who, d)| match d {
        ic_channels::customers::Desk::Search(p) if who == "sue" => Some(p.clone()),
        _ => None,
    });
    assert_eq!(
        probe,
        Some(json!({ "family_name": "Parata", "given_name": "Kiri", "date_of_birth": "1988-04-12" }))
    );
    // The search and each person listed are in her access log, under the case.
    let me = portal.get("sub-sue", "/staff/v1/me").await.1;
    let lines: Vec<(String, String)> = me["recent"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["action"].as_str().unwrap_or_default().to_string(),
                r["record"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    for record in ["customers", &format!("party:{KIRI}"), "party:party-77"] {
        assert!(lines.contains(&("customers.find".to_string(), record.to_string())), "{record} in {lines:?}");
    }

    // A policy by its reference typed in part, with what it is.
    let part: String = policy.0.chars().take(policy.0.len() - 1).collect();
    let by_number = portal.get("sub-sue", &find(&part)).await.1;
    let hit = &by_number["policies"][0];
    assert_eq!(
        (hit["policy"].as_str(), hit["status"].is_string()),
        (Some(policy.0.as_str()), true),
        "{by_number}"
    );
    assert_eq!(by_number["searched_people"], false, "a reference is not a name");
    // A birth date alone says what else is needed.
    assert_eq!(
        portal.get("sub-sue", &find("12/4/1988")).await.1["hint"],
        "add the person's family name to search the customer master"
    );

    // Once she declares her connection to Kiri, Kiri is listed as closed: no name, no policies.
    let declare = json!({ "declare_connection": { "party": KIRI } });
    assert_eq!(portal.write("POST", "sub-sue", "/staff/v1/access", declare).await.0, StatusCode::OK);
    let closed = portal.get("sub-sue", &find("Kiri Parata")).await.1;
    let kiri = closed["people"].as_array().unwrap().iter().find(|p| p["party"] == KIRI).unwrap().clone();
    assert_eq!(
        (kiri["closed"].as_bool(), kiri["name"].is_null(), kiri["policies"].clone()),
        (Some(true), true, json!([]))
    );

    // With the master down, policy numbers are still found and the page is told why nobody was.
    portal.master.down.store(true, Ordering::SeqCst);
    let down = portal.get("sub-sue", &find("Kiri Parata")).await.1;
    assert_eq!(
        (down["customer_master_unavailable"].as_bool(), down["people"].clone()),
        (Some(true), json!([])),
        "{down}"
    );
    // Kiri's policy is closed to Sue now, so the number finds it only as closed.
    let numbers = portal.get("sub-sue", &find(&part)).await.1;
    assert!(numbers["policies"].as_array().unwrap().iter().any(|p| p["closed"] == true), "{numbers}");

    // Someone without the finding action is refused.
    assert_eq!(portal.get("sub-fin1", &find("Kiri Parata")).await.0, StatusCode::FORBIDDEN);
}

/// Purpose: the colleagues list offers everyone current on the staff directory with their team and roles, or only those who hold an action today.
/// Why it matters: work is handed to a person by picking them, not by typing a reference; a list that offered someone without the action would hand them work they cannot take.
#[tokio::test]
async fn colleagues_are_offered_by_what_they_may_do() {
    let portal = portal();
    let all = portal.get("sub-sue", "/staff/v1/colleagues").await.1;
    let names: Vec<&str> = all["people"].as_array().unwrap().iter().filter_map(|p| p["party"].as_str()).collect();
    assert!(names.contains(&"sue") && names.contains(&"alice"), "{all}");
    let sue = all["people"].as_array().unwrap().iter().find(|p| p["party"] == "sue").unwrap();
    assert_eq!((sue["team"].as_str(), sue["roles"].clone()), (Some("ops"), json!(["service"])));
    let approvers = portal
        .get("sub-sue", &format!("/staff/v1/colleagues?action={}", ic_channels::state::assisted::APPROVE))
        .await
        .1;
    let names: Vec<&str> = approvers["people"].as_array().unwrap().iter().filter_map(|p| p["party"].as_str()).collect();
    assert!(names.contains(&"alice") && !names.contains(&"sue"), "{approvers}");
    assert_eq!(portal.get("sub-kiri", "/staff/v1/colleagues").await.0, StatusCode::FORBIDDEN);
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

    // An allowed read and a refused one of the same policy in the same second each keep their line.
    portal.clock.fetch_add(1, Ordering::SeqCst);
    assert_eq!(
        portal.get("sub-sue", &format!("/staff/v1/policies/{policy}?case=case-8")).await.0,
        StatusCode::OK
    );
    assert_eq!(portal.get("sub-sue", &format!("/staff/v1/policies/{policy}")).await.0, StatusCode::FORBIDDEN);
    let log = recent(&portal.get("sub-sue", "/staff/v1/me").await.1);
    assert_eq!(log.len(), 4, "{log:?}");
    assert_eq!(log.iter().filter(|line| line["decision"]["allowed"] == false).count(), 2);

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
    assert_eq!((log.len(), log[0]["flag"].as_str()), (5, Some("personal")));
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
    assert_eq!(recent(&portal.get("sub-sue", "/staff/v1/me").await.1).len(), 5);
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

    let request = json!({ "id": "r1", "policy": portal.retail, "kind": "claim_evidence", "module": "claims" });
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
    for path in [
        "/",
        "/customer",
        "/adviser",
        "/employer",
        "/staff",
        "/pricing",
        "/complaints",
        "/finance",
        "/claims",
        "/reinsurance",
        "/underwriting",
        "/payments",
        "/collections",
        "/documents",
        "/records",
        "/replacements",
        "/ops",
        "/status",
    ] {
        let response = portal
            .app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let csp = response.headers()["content-security-policy"].to_str().unwrap().to_string();
        assert!(
            csp.starts_with("default-src 'none'")
                && csp.contains("frame-ancestors 'none'")
                && csp.contains("connect-src 'self';")
                && csp.contains("font-src 'self';"),
            "{csp}"
        );
        assert!(!csp.contains("unsafe"), "{csp}");
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        assert!(response.headers()["content-type"].to_str().unwrap().starts_with("text/html"));
        // Every inline script on the page is one the policy names by hash, so the browser will run it and nothing else.
        let body = String::from_utf8(response.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
        let inline = body.matches("<script>").count();
        assert_eq!(csp.matches("'sha256-").count(), inline, "{path}");
        assert_eq!(inline, 1, "{path}");
    }
    for (path, kind, has) in [
        ("/ui/app.js", "text/javascript", "const portal"),
        ("/ui/app.css", "text/css", "box-sizing"),
        ("/ui/theme.js", "text/javascript", "dataset.theme"),
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
    // The typeface is served from here too, so the pages' policy can name this service alone for fonts.
    let font = portal
        .app
        .clone()
        .oneshot(Request::builder().uri("/ui/inter.woff2").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(font.status(), StatusCode::OK);
    assert_eq!(font.headers()["content-type"], "font/woff2");
    assert_eq!(font.headers()["x-content-type-options"], "nosniff");
    assert!(font.into_body().collect().await.unwrap().to_bytes().starts_with(b"wOF2"));
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

/// Purpose: the unit pricing console end to end against that module's own code: only staff with a unit pricing action get in; the
/// queue, funds, valuations and refused files are shown; a price is prepared, refused to its own preparer, approved by a second
/// person, and found in force once the core has it; a held price is approved only with each failed check accepted; a lead suspends,
/// resumes and raises a price error; and each command goes as the person with the roles their staff templates give.
/// Why it matters: the unit pricing module takes people's commands only from this console and trusts the roles it states. A console
/// that let anyone in, stated roles nobody holds, or sent anything but the people's commands would undo that module's two-person
/// control over the price every unit-linked policy trades at.
#[tokio::test]
async fn the_unit_pricing_console() {
    let portal = portal();
    let base = "/staff/v1/unit-pricing";
    let ask = |who: &'static str, rest: &str| {
        let path = format!("{base}/{rest}");
        let portal = &portal;
        async move { portal.get(who, &path).await }
    };
    let tell = |who: &'static str, command: Value| {
        let portal = &portal;
        async move { portal.write("POST", who, "/staff/v1/unit-pricing/commands", command).await }
    };
    let day = common::pricing::first_day();

    // Who gets in.
    // The refusal names the permission to ask for (F2-06-7).
    assert!(ask("sub-sue", "work").await.1["detail"].as_str().unwrap().contains("unit_pricing.view"));
    for outsider in ["sub-kiri", "sub-sue", "sub-fin1"] {
        assert_eq!(ask(outsider, "work").await.0, StatusCode::FORBIDDEN, "{outsider}");
        assert_eq!(
            tell(outsider, json!({ "prepare_price": { "fund": "growth", "pricing_date": day } })).await.0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        ask("sub-up1", "me").await.1,
        json!({ "view": true, "roles": ["unit_pricing_preparer", "unit_pricing_approver"] })
    );
    assert_eq!(ask("sub-lead", "me").await.1, json!({ "view": true, "roles": ["unit_pricing_lead"] }));

    // What is shown before anyone acts.
    let (status, work) = ask("sub-lead", "work").await;
    assert_eq!((status, work["prices"].as_array().map(Vec::len)), (StatusCode::OK, Some(0)));
    assert_eq!(work["refused_files"]["valuations/bad.csv"], "line 3: bad trailer");
    let funds = ask("sub-lead", "funds").await.1;
    assert_eq!(
        funds.as_array().unwrap().iter().map(|f| f["fund"].as_str().unwrap()).collect::<Vec<_>>(),
        ["balanced", "cash", "growth"]
    );
    let valuation = ask("sub-up1", &format!("valuations?fund=growth&pricing_date={day}")).await;
    assert_eq!(valuation.0, StatusCode::OK, "{valuation:?}");
    assert_eq!(ask("sub-up1", "status").await.1["partitions"], 2);
    // Only the console's questions, each with its fields; something not there is not found.
    for bad in ["outbox?limit=5", "events?after=0&limit=5", "price", "prices?fund=growth"] {
        assert_eq!(ask("sub-up1", bad).await.0, StatusCode::BAD_REQUEST, "{bad}");
    }
    // Late prices are as at the books' date unless another is asked for, one row per fund (F2-06-6).
    let (status, late) = ask("sub-up1", "late?as_at=").await;
    assert_eq!(status, StatusCode::OK, "{late}");
    assert!(
        late.as_array().unwrap().iter().all(|row| row["fund"].is_string() && row["days"].is_u64()),
        "{late}"
    );
    assert_eq!(ask("sub-up1", "price?id=nope").await.0, StatusCode::NOT_FOUND);
    // A question that module cannot read is its refusal, not an outage.
    let (status, problem) = ask("sub-up1", "valuations?fund=growth&pricing_date=never").await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::BAD_REQUEST, Some("refused")));

    // Only people's commands, and only in a role the person holds.
    let prepare = json!({ "prepare_price": { "fund": "growth", "pricing_date": day } });
    for (not_theirs, status) in [
        (json!({ "record_partitions": { "count": 9 } }), StatusCode::BAD_REQUEST),
        (
            json!({ "record_delivery": { "publication": "x", "delivery": "applied" } }),
            StatusCode::BAD_REQUEST,
        ),
        (json!({ "prepare_price": {}, "republish": {} }), StatusCode::BAD_REQUEST),
        (json!("prepare_price"), StatusCode::BAD_REQUEST),
        (json!({ "republish": { "price": "x" } }), StatusCode::FORBIDDEN),
    ] {
        assert_eq!(tell("sub-up1", not_theirs.clone()).await.0, status, "{not_theirs}");
    }
    let (status, problem) = tell("sub-lead", prepare.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(problem["detail"].as_str().unwrap().contains("unit_pricing_preparer"));
    let subject_only = [("x-dev-subject", "sub-up1".to_string())];
    let unkeyed = portal.send("POST", "/staff/v1/unit-pricing/commands", &subject_only, Some(prepare.clone()));
    assert_eq!(unkeyed.await.0, StatusCode::BAD_REQUEST);
    // A command that module cannot read is its refusal to take it, not a success.
    assert_eq!(tell("sub-up1", json!({ "prepare_price": { "fund": 7 } })).await.0, StatusCode::BAD_REQUEST);
    // The console sends the books' date: a price for a day that has not come is refused before anything is prepared (F2-06-4).
    let (status, problem) = tell("sub-up1", json!({ "prepare_price": { "fund": "growth", "pricing_date": d(2030, 1, 7) } })).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::CONFLICT, Some("not_yet")), "{problem}");
    assert!(problem["detail"].as_str().unwrap().contains("the business date is"), "{problem}");

    // Prepare, with the same key twice: one price.
    let keyed = [("x-dev-subject", "sub-up1".to_string()), ("idempotency-key", "prep-1".to_string())];
    let (status, first) = portal.send("POST", "/staff/v1/unit-pricing/commands", &keyed, Some(prepare.clone())).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let again = portal.send("POST", "/staff/v1/unit-pricing/commands", &keyed, Some(prepare)).await.1;
    assert_eq!(first, again);
    let price = first["output"]["prepared"]["price"].as_str().unwrap().to_string();
    assert_eq!(first["output"]["prepared"]["status"], "prepared");
    let work = ask("sub-up2", "work").await.1;
    assert_eq!((&work["prices"][0]["id"], &work["prices"][0]["prepared_by"]), (&json!(price), &json!("up-1")));

    // The preparer cannot approve it; a second person can. That module's refusal is passed on with its code.
    let approve = json!({ "approve_price": { "price": price } });
    let (status, problem) = tell("sub-up1", approve.clone()).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::CONFLICT, Some("separation")));
    assert_eq!(tell("sub-up2", approve).await.0, StatusCode::OK);
    let detail = ask("sub-lead", &format!("price?id={price}")).await.1;
    assert_eq!((&detail["status"], &detail["approved_by"]), (&json!("activating"), &json!("up-2")));
    portal.pricing.deliver_all(portal.now());
    let in_force = ask("sub-lead", &format!("prices?fund=growth&from={day}&to={day}")).await.1;
    assert_eq!(
        (in_force.as_array().unwrap().len(), &in_force[0]["price"], &in_force[0]["version"]),
        (1, &json!(price), &json!(1))
    );

    // The next day's valuation jumps a fifth: held, and approved only with each failed check accepted.
    let next = d(2027, 11, 15);
    portal.pricing.valuation(next, "1212000", portal.now());
    portal.pricing.core_units(Some(day), portal.now());
    let held = tell("sub-up2", json!({ "prepare_price": { "fund": "growth", "pricing_date": next } })).await.1;
    assert_eq!(held["output"]["prepared"]["status"], "held", "{held}");
    let held_id = held["output"]["prepared"]["price"].as_str().unwrap();
    let (status, problem) = tell("sub-up1", json!({ "approve_price": { "price": held_id } })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{problem}");
    let waiting = ask("sub-up1", "work").await.1;
    let accepted: serde_json::Map<String, Value> = waiting["prices"][0]["struck"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["passed"] == false)
        .map(|c| (c["kind"].as_str().unwrap().to_string(), json!("market rally, confirmed with the administrator")))
        .collect();
    assert!(accepted.contains_key("previous_move"));
    let (status, done) = tell("sub-up1", json!({ "approve_price": { "price": held_id, "accepted": accepted } })).await;
    assert_eq!(status, StatusCode::OK, "{done}");
    portal.pricing.deliver_all(portal.now());

    // The lead: suspend and resume, and a price error over the first price.
    let lead = |command: Value| tell("sub-lead", command);
    assert_eq!(
        lead(json!({ "suspend_fund": { "fund": "cash", "from": next, "reason": "administrator outage" } }))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(ask("sub-lead", "funds").await.1[1]["suspensions"][0]["by"], "lead-1");
    assert_eq!(
        lead(json!({ "resume_fund": { "fund": "cash", "from": d(2027, 11, 16), "reason": "restored" } }))
            .await
            .0,
        StatusCode::OK
    );
    let raise = json!({ "raise_price_error": { "error": "err-1", "prices": [price], "cause": "a bond was mispriced", "detected_on": next } });
    assert_eq!(tell("sub-up1", raise.clone()).await.0, StatusCode::FORBIDDEN);
    let (status, raised) = lead(raise).await;
    assert_eq!(status, StatusCode::OK, "{raised}");
    let errors = ask("sub-lead", "errors").await.1;
    assert_eq!((errors.as_array().unwrap().len(), &errors[0]["raised_by"]), (1, &json!("lead-1")));
    assert_eq!(ask("sub-lead", "error?id=err-1").await.1["cause"], "a bond was mispriced");
    assert_eq!(ask("sub-lead", "error?id=err-2").await.0, StatusCode::NOT_FOUND);
    // It cannot be closed before the correction is made.
    assert_eq!(
        lead(json!({ "close_error": { "error": "err-1", "summary": "done" } })).await.0,
        StatusCode::CONFLICT
    );
    let late = ask("sub-lead", "late?as_at=2027-11-30").await;
    assert!(late.0 == StatusCode::OK && late.1.is_array(), "{late:?}");

    // That module has no leader: the console says so, for a question and for a command.
    let now = portal.now();
    let down = router(Arc::new(AppState {
        wordings: Arc::default(),
        public_url: None,
        returns: Arc::default(),
        channel: portal.local.clone(),
        customers: Auth::Dev,
        workforce: Auth::Dev,
        clock: Arc::new(move || now),
        regions: portal.regions.clone(),
        ui: UiConfig::Dev,
        pricing: Arc::new(common::pricing::Down),
        complaints: Arc::new(common::pricing::Down),
        commission: Arc::new(common::commission::seeded()),
        finance: Arc::new(common::pricing::Down),
        claims: Arc::new(common::claims::Down),
        reinsurance: Arc::new(common::pricing::Down),
        payments: Arc::new(common::pricing::Down),
        collections: Arc::new(common::pricing::Down),
        correspondence: Arc::new(common::pricing::Down),
        wellness: Arc::new(common::pricing::Down),
        underwriting: Arc::new(common::pricing::Down),
        master: None,
        intake: None,
        documents: None,
        group: None,
    }));
    for (method, path, body) in [
        ("GET", "/staff/v1/unit-pricing/work", None),
        ("POST", "/staff/v1/unit-pricing/commands", Some(json!({ "republish": { "price": "x" } }))),
    ] {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("x-dev-subject", "sub-lead")
            .header("idempotency-key", "k");
        if body.is_some() {
            request = request.header("content-type", "application/json");
        }
        let bytes = body.map(|b| b.to_string()).unwrap_or_default();
        let response = down.clone().oneshot(request.body(Body::from(bytes)).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{path}");
        let problem: Value = serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(problem["title"], "unit_pricing_unavailable");
    }
}

/// Purpose: a bundle released to an external forum names as its reviewer a colleague who works complaints or legal, never a typed
/// name that is nobody, nor a colleague outside those teams; and the report's count of open conduct cases is shown only to the
/// conduct function and legal.
/// Why it matters: found by the scenario run, SCN-21 (F-21-7, F-21-12). Legal released a bundle "reviewed by" a made-up name and it
/// was accepted, so the second check on what a forum is sent proved nothing; and the complaints team saw a trace of conduct work.
#[tokio::test]
async fn a_bundle_is_reviewed_by_a_colleague_and_conduct_counts_stay_with_conduct() {
    let portal = portal();
    let tell = |who: &'static str, seat: &'static str, command: Value| {
        let portal = &portal;
        async move {
            portal
                .write("POST", who, "/staff/v1/complaints/commands", json!({ "as": seat, "command": command }))
                .await
        }
    };
    for (who, seat, command) in [
        (
            "sub-front",
            "intake",
            json!({ "record_complaint": { "complaint": "c-9", "channel": "call", "complainant": KIRI, "complainant_kind": "policyholder",
                "category": { "process": "service" }, "summary_document": "doc-c-9", "vulnerable": false } }),
        ),
        ("sub-cmp2", "complaints", json!({ "assign": { "complaint": "c-9", "handler": "cmp-1" } })),
        (
            "sub-cmp1",
            "complaints",
            json!({ "refer": { "complaint": "c-9", "forum": "scheme", "reference": "IFSO-1", "response_due": "2099-01-01", "documents_due": null } }),
        ),
    ] {
        let (status, done) = tell(who, seat, command).await;
        assert_eq!(status, StatusCode::OK, "{done}");
    }
    let release =
        |bundle: &str, reviewer: &str| json!({ "release_bundle": { "complaint": "c-9", "bundle": bundle, "documents": ["doc-c-9"], "reviewed_by": reviewer } });
    for nobody in ["zz-nobody", "fin-1"] {
        let (status, problem) = tell("sub-lex", "legal", release("b-1", nobody)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{problem}");
        assert!(problem["detail"].as_str().unwrap().contains("pick the colleague who reviewed it"), "{problem}");
    }
    let (status, done) = tell("sub-lex", "legal", release("b-1", "cmp-2")).await;
    assert_eq!(status, StatusCode::OK, "{done}");

    let open = json!({ "open_conduct_case": { "case": "b-9", "kind": "breach", "owner": "con-1", "summary_document": null, "notify_regulator": false } });
    assert_eq!(tell("sub-con", "conduct", open).await.0, StatusCode::OK);
    let report = |who: &'static str| {
        let portal = &portal;
        async move { portal.get(who, "/staff/v1/complaints/report?from=2020-01-01&to=2099-12-31").await.1 }
    };
    let team = report("sub-cmp2").await;
    assert!(team.get("open_conduct_cases").is_none() && team["received"] == 1, "{team}");
    assert_eq!(report("sub-con").await["open_conduct_cases"], 1);
    // Front-line staff read what they pick from: the cause codes and the limit for putting a matter right on the spot.
    let settings = portal.get("sub-front", "/staff/v1/complaints/settings").await.1;
    assert!(settings["cause_codes"].is_array() && settings["front_line_limit_minor"].is_i64(), "{settings}");
}

/// Purpose: the complaints and conduct workbench end to end against that module's own code: a seat follows from a person's staff
/// actions; front-line staff record and nothing more; the complaints team reads the queue, the file, the register and the report;
/// conduct cases are open only to the conduct function and legal; a handler's authority and training and a remediation's authority
/// are written in from the staff records whatever the person typed; and that module's own refusals come back with their codes.
/// Why it matters: the complaints module believes the calling service about who may act and at what authority. If the workbench let
/// a person pick their own seat or type their own authority, an untrained junior could be given a vulnerable complainant's file and
/// approve goodwill payments beyond their limit, and conduct cases would be readable by the people they concern.
#[tokio::test]
async fn the_complaints_and_conduct_workbench() {
    let portal = portal();
    let ask = |who: &'static str, rest: &str| {
        let path = format!("/staff/v1/complaints/{rest}");
        let portal = &portal;
        async move { portal.get(who, &path).await }
    };
    let tell = |who: &'static str, seat: &'static str, command: Value| {
        let portal = &portal;
        async move {
            portal
                .write("POST", who, "/staff/v1/complaints/commands", json!({ "as": seat, "command": command }))
                .await
        }
    };
    let intake = |id: &str, vulnerable: bool| {
        json!({ "record_complaint": { "complaint": id, "channel": "call", "complainant": KIRI, "complainant_kind": "policyholder",
            "category": { "process": "service" }, "summary_document": format!("doc-{id}"), "vulnerable": vulnerable } })
    };

    // Seats.
    for outsider in ["sub-kiri", "sub-fin1", "sub-up1"] {
        assert_eq!(ask(outsider, "me").await.0, StatusCode::FORBIDDEN, "{outsider}");
        assert_eq!(tell(outsider, "intake", intake("c-0", false)).await.0, StatusCode::FORBIDDEN);
    }
    assert_eq!(ask("sub-front", "me").await.1, json!({ "seats": ["intake"], "authority": 3, "trained": false }));
    assert_eq!(
        ask("sub-cmp1", "me").await.1,
        json!({ "seats": ["intake", "complaints"], "authority": 3, "trained": true })
    );
    assert_eq!(
        ask("sub-cmp2", "me").await.1,
        json!({ "seats": ["intake", "complaints"], "authority": 1, "trained": false })
    );
    assert_eq!(ask("sub-lex", "me").await.1["seats"], json!(["legal"]));
    assert_eq!(ask("sub-con", "me").await.1["seats"], json!(["conduct"]));

    // The front line records a complaint, twice with one key, and can do nothing else.
    let keyed = [("x-dev-subject", "sub-front".to_string()), ("idempotency-key", "c-1".to_string())];
    let body = json!({ "as": "intake", "command": intake("c-1", true) });
    let (status, first) = portal.send("POST", "/staff/v1/complaints/commands", &keyed, Some(body.clone())).await;
    assert_eq!((status, &first["duplicate"]), (StatusCode::OK, &json!(false)), "{first}");
    assert_eq!(
        portal.send("POST", "/staff/v1/complaints/commands", &keyed, Some(body.clone())).await.1["duplicate"],
        true
    );
    let unkeyed = [("x-dev-subject", "sub-front".to_string())];
    assert_eq!(
        portal.send("POST", "/staff/v1/complaints/commands", &unkeyed, Some(body)).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        tell("sub-front", "complaints", json!({ "declare_deadlock": { "complaint": "c-1" } })).await.0,
        StatusCode::FORBIDDEN
    );
    for question in ["queue", "complaint?id=c-1", "register?from=2027-11-01&to=2027-11-30", "open_conduct_cases"] {
        assert_eq!(ask("sub-front", question).await.0, StatusCode::FORBIDDEN, "{question}");
    }
    // What is not a person's command is refused before it is sent, and that module refuses a command in the wrong seat.
    for (seat, command) in [
        ("scheduler", json!("tick")),
        ("complaints", json!("tick")),
        ("complaints", json!({ "tick": null })),
        ("complaints", json!({ "mark_published": { "through_seq": 9 } })),
        ("complaints", json!({ "assign": {}, "close": {} })),
    ] {
        assert_eq!(tell("sub-cmp1", seat, command.clone()).await.0, StatusCode::BAD_REQUEST, "{seat} {command}");
    }
    assert_eq!(
        tell(
            "sub-con",
            "conduct",
            json!({ "attach_population": { "case": "b-1", "customers": [], "advisers": [], "policies": [] } })
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let (status, problem) = tell("sub-con", "conduct", json!({ "declare_deadlock": { "complaint": "c-1" } })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{problem}");
    assert_eq!(
        tell("sub-cmp1", "complaints", json!({ "declare_deadlock": { "complaint": 7 } })).await.0,
        StatusCode::BAD_REQUEST
    );

    // The queue shows the vulnerable complainant needs a trained handler.
    let (status, queue) = ask("sub-cmp1", "queue").await;
    assert_eq!(
        (status, &queue[0]["complaint"], &queue[0]["needs_trained_handler"]),
        (StatusCode::OK, &json!("c-1"), &json!(true)),
        "{queue}"
    );

    // Assignment: authority and training come from the staff records, whatever is typed.
    let assign = |handler: &str| json!({ "assign": { "complaint": "c-1", "handler": handler, "authority": 9, "trained_for_vulnerability": true } });
    let (status, problem) = tell("sub-cmp1", "complaints", assign("cmp-2")).await;
    assert_eq!(status, StatusCode::CONFLICT, "an untrained handler for a vulnerable complainant: {problem}");
    assert_eq!(tell("sub-cmp1", "complaints", assign("fin-1")).await.0, StatusCode::FORBIDDEN);
    let (status, done) = tell("sub-cmp2", "complaints", assign("cmp-1")).await;
    assert_eq!(status, StatusCode::OK, "{done}");

    // The file is open to the handler and closed to a colleague the case has not let in.
    let (status, file) = ask("sub-cmp1", "complaint?id=c-1").await;
    assert_eq!(status, StatusCode::OK, "{file}");
    assert!(
        file.to_string().contains("\"cmp-1\"") && !file.to_string().contains("\"authority\":9"),
        "{file}"
    );
    let (status, problem) = ask("sub-cmp2", "complaint?id=c-1").await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::FORBIDDEN, Some("refused")));
    assert_eq!(ask("sub-cmp1", "complaint").await.0, StatusCode::BAD_REQUEST);
    assert!(ask("sub-cmp1", "running?id=c-1").await.1.is_array());
    assert_eq!(ask("sub-cmp1", "running?id=nope").await.0, StatusCode::NOT_FOUND);
    assert_eq!(ask("sub-cmp1", &format!("complaints_of?party={KIRI}")).await.1, json!(["c-1"]));
    assert_eq!(ask("sub-lex", "subject_record?id=c-1").await.0, StatusCode::OK);
    let register = ask("sub-cmp2", "register?from=2027-11-01&to=2027-11-30").await.1;
    assert_eq!((register.as_array().unwrap().len(), &register[0]["complaint"]), (1, &json!("c-1")));
    assert_eq!(ask("sub-cmp2", "report?from=2027-11-01&to=2027-11-30").await.0, StatusCode::OK);
    for bad in [
        "register?from=yesterday&to=2027-11-30",
        "report?from=2027-11-01",
        "pending_events?limit=5",
        "state_hash",
    ] {
        assert_eq!(ask("sub-cmp1", bad).await.0, StatusCode::BAD_REQUEST, "{bad}");
    }

    // Goodwill: the authoriser's level is theirs, not what was typed. The junior's limit is 500.00.
    for (who, command) in [
        ("sub-cmp1", json!({ "record_acknowledgement": { "complaint": "c-1", "document": "ack-1" } })),
        (
            "sub-cmp1",
            json!({ "decide": { "complaint": "c-1", "outcome": "upheld", "reason_codes": ["delay"], "document": "out-1" } }),
        ),
    ] {
        let (status, done) = tell(who, "complaints", command).await;
        assert_eq!(status, StatusCode::OK, "{done}");
    }
    let goodwill = |reference: &str, by: Option<&str>| json!({ "record_remediation": { "complaint": "c-1", "kind": "ex_gratia", "reference": reference, "amount_minor": 100_000, "authorised_by": by, "authority": 9 } });
    let (status, problem) = tell("sub-cmp1", "complaints", goodwill("pay-1", Some("cmp-2"))).await;
    assert_eq!(status, StatusCode::CONFLICT, "beyond the junior's limit: {problem}");
    assert_eq!(tell("sub-cmp1", "complaints", goodwill("pay-1", Some("fin-1"))).await.0, StatusCode::FORBIDDEN);
    let mut own = goodwill("pay-1", None);
    own["record_remediation"].as_object_mut().unwrap().remove("authorised_by");
    let (status, done) = tell("sub-cmp1", "complaints", own).await;
    assert_eq!(status, StatusCode::OK, "{done}");
    let file = ask("sub-cmp1", "complaint?id=c-1").await.1;
    assert_eq!(file["remediation"][0]["authorised_by"], "cmp-1", "{file}");

    // Conduct cases: the conduct function and legal only.
    let open = json!({ "open_conduct_case": { "case": "b-1", "kind": "breach", "owner": "con-1", "summary_document": null, "notify_regulator": false } });
    let (status, done) = tell("sub-con", "conduct", open).await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(ask("sub-con", "open_conduct_cases").await.1, json!(["b-1"]));
    assert_eq!(ask("sub-lex", "open_conduct_cases").await.0, StatusCode::OK);
    assert_eq!(ask("sub-con", "conduct_case?id=b-1").await.1["owner"], "con-1");
    assert_eq!(ask("sub-con", "conduct_case?id=b-2").await.0, StatusCode::NOT_FOUND);
    for question in ["open_conduct_cases", "conduct_case?id=b-1"] {
        assert_eq!(ask("sub-cmp1", question).await.0, StatusCode::FORBIDDEN, "{question}");
    }
    // The conduct function also reads the register.
    assert_eq!(ask("sub-con", "queue").await.0, StatusCode::OK);

    // That module has no leader.
    let now = portal.now();
    let down = router(Arc::new(AppState {
        wordings: Arc::default(),
        public_url: None,
        returns: Arc::default(),
        channel: portal.local.clone(),
        customers: Auth::Dev,
        workforce: Auth::Dev,
        clock: Arc::new(move || now),
        regions: portal.regions.clone(),
        ui: UiConfig::Dev,
        pricing: Arc::new(common::pricing::Down),
        complaints: Arc::new(common::pricing::Down),
        commission: Arc::new(common::commission::seeded()),
        finance: Arc::new(common::pricing::Down),
        claims: Arc::new(common::claims::Down),
        reinsurance: Arc::new(common::pricing::Down),
        payments: Arc::new(common::pricing::Down),
        collections: Arc::new(common::pricing::Down),
        correspondence: Arc::new(common::pricing::Down),
        wellness: Arc::new(common::pricing::Down),
        underwriting: Arc::new(common::pricing::Down),
        master: None,
        intake: None,
        documents: None,
        group: None,
    }));
    for (method, path, body) in [
        ("GET", "/staff/v1/complaints/queue", None),
        (
            "POST",
            "/staff/v1/complaints/commands",
            Some(json!({ "as": "complaints", "command": { "declare_deadlock": { "complaint": "c-1" } } })),
        ),
    ] {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header("x-dev-subject", "sub-cmp1")
            .header("idempotency-key", "k");
        let bytes = body.map(|b| b.to_string()).unwrap_or_default();
        let response = down.clone().oneshot(request.body(Body::from(bytes)).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{path}");
        let problem: Value = serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(problem["title"], "complaints_unavailable");
    }
}

/// Purpose: reinsurance asks what the insurer keeps on one person's life on a date, by the person, whatever policies insure them;
/// the question needs the person and a real date, and the MCP tool's description names the fields each question takes.
/// A balance of a treaty that does not exist is not found rather than 0, and a date sent as a number is told it is text.
/// Why it matters: found by the scenario run, SCN-22 (F-SCN22-1, F-SCN22-5). A large life above the automatic limit could not be
/// looked at by screen or by MCP: the console asked only per reinsurer, and an assistant was not told which fields any question needed.
/// An auditor was told a misspelt treaty's balance was 0, and a numeric date got "input contains invalid characters" (F2-18-6, F2-18-7).
#[tokio::test]
async fn reinsurance_reads_a_lifes_accumulation() {
    let portal = portal();
    let (status, kept) = portal.get("sub-riview", "/staff/v1/reinsurance/retained?party=anna&on=2027-11-15").await;
    assert_eq!((status, kept.is_i64()), (StatusCode::OK, true), "{kept}");
    // A balance of a treaty nobody has is not 0; a date given as a number is told the form (F2-18-6, F2-18-7).
    let (status, problem) = portal.get("sub-riview", "/staff/v1/reinsurance/balance?treaty=no-such&reinsurer=no-such").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{problem}");
    assert!(
        problem["detail"].as_str().unwrap().contains("the exposure lists each reinsurer's treaties"),
        "{problem}"
    );
    for (bad, why) in [
        ("retained?on=2027-11-15", "needs party"),
        ("retained?party=anna&on=soon", "is a date"),
        ("exposure?on=20271115", "on is a date written as text, YYYY-MM-DD"),
    ] {
        let (status, problem) = portal.get("sub-riview", &format!("/staff/v1/reinsurance/{bad}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
        assert!(problem["detail"].as_str().unwrap().contains(why), "{problem}");
    }
    let tools = ic_channels::mcp::catalogue();
    let ask = tools.iter().find(|t| t.name == "reinsurance_question").unwrap();
    for named in ["`period_end`", "`on`", "`reinsurer`", "`treaty`", "`claim`", "`party`"] {
        assert!(ask.query.contains(named), "{named}");
    }
}

/// Purpose: staff see the reinsurance module's open exceptions and act on them: one person sends what they may do alone, an
/// abandonment is prepared by one person and sent only when a different person signs it, as `approved_by`, and a signature
/// whose send was interrupted is finished by signing again.
/// Why it matters: abandoning a notice or an instruction leaves a reinsurer untold or two ledgers apart. The reinsurance
/// module takes the console's word for who approved it, so if the console let one person do both halves, or let the page type
/// the approver, the two-person rule would exist only on paper.
#[tokio::test]
async fn the_reinsurance_exceptions_console() {
    let portal = portal();
    let ask = |who: &'static str, rest: &str| {
        let path = format!("/staff/v1/reinsurance/{rest}");
        let portal = &portal;
        async move { portal.get(who, &path).await }
    };
    let tell = |who: &'static str, command: Value| {
        let portal = &portal;
        async move { portal.write("POST", who, "/staff/v1/reinsurance/commands", command).await }
    };
    let decide = |who: &'static str, verb: &'static str, id: String| {
        let portal = &portal;
        async move { portal.write("POST", who, &format!("/staff/v1/reinsurance/{verb}/{id}"), json!({})).await }
    };

    // Only people with a reinsurance action get in; a reader cannot act.
    assert_eq!(ask("sub-gl1", "me").await.0, StatusCode::FORBIDDEN);
    let (status, me) = ask("sub-riview", "me").await;
    assert_eq!((status, &me["operate"], &me["approve"]), (StatusCode::OK, &json!(false), &json!(false)), "{me}");
    let (status, exceptions) = ask("sub-riview", "exceptions").await;
    assert_eq!((status, exceptions["delivery:1"]["code"].is_string()), (StatusCode::OK, true), "{exceptions}");
    let abandon = json!({ "abandon_instruction": { "seq": 1, "reason": "the policy was entered in error and cancelled" } });
    assert_eq!(tell("sub-riview", abandon.clone()).await.0, StatusCode::FORBIDDEN);

    // What the page may not send.
    for bad in [
        json!({ "abandon_instruction": { "seq": 1, "reason": "x", "approved_by": "ri-1" } }),
        json!({ "abandon_instruction": { "seq": 1, "reason": " " } }),
        json!({ "acknowledge_instructions": { "through": 1 } }),
        json!({ "retry_notice": 1 }),
        json!("exceptions"),
    ] {
        assert_eq!(tell("sub-ri1", bad.clone()).await.0, StatusCode::BAD_REQUEST, "{bad}");
    }

    // Facultative terms, a decline, an evidence release and a claim fact reach the module from one person; it decides.
    for sent in [
        json!({ "decline_facultative": { "reservation_id": "none", "reference": "FAC-1" } }),
        json!({ "claim": { "admitted": { "claim": "none", "admitted_minor": 100 } } }),
    ] {
        let (status, answer) = tell("sub-ri1", sent.clone()).await;
        assert_ne!(status, StatusCode::BAD_REQUEST, "{sent}: {answer}");
        assert_ne!(status, StatusCode::FORBIDDEN, "{sent}: {answer}");
    }
    assert_eq!(
        tell(
            "sub-riview",
            json!({ "decline_facultative": { "reservation_id": "none", "reference": "FAC-1" } })
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );

    // One person prepares an abandonment; it waits, and its preparer cannot sign it.
    let (status, prepared) = tell("sub-ri1", abandon).await;
    assert_eq!(status, StatusCode::OK, "{prepared}");
    let id = prepared["prepared"].as_str().unwrap().to_string();
    let waiting = ask("sub-ri2", "me").await.1["pending"].clone();
    assert_eq!((&waiting[0]["id"], &waiting[0]["by"]), (&json!(id), &json!("ri-1")), "{waiting}");
    assert!(portal.reinsurance.open("delivery:1"), "nothing reached the module yet");
    let (status, problem) = decide("sub-ri1", "sign", id.clone()).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::CONFLICT, Some("separation")), "{problem}");
    assert_eq!(decide("sub-riview", "sign", id.clone()).await.0, StatusCode::FORBIDDEN);
    assert_eq!(decide("sub-ri2", "withdraw", id.clone()).await.0, StatusCode::FORBIDDEN);
    assert_eq!(decide("sub-ri2", "approve", id.clone()).await.0, StatusCode::NOT_FOUND);

    // The partitions' module has no leader when the second person signs: it stays signed, and signing again finishes it.
    let now = portal.now();
    let down = router(Arc::new(AppState {
        wordings: Arc::default(),
        public_url: None,
        returns: Arc::default(),
        channel: portal.local.clone(),
        customers: Auth::Dev,
        workforce: Auth::Dev,
        clock: Arc::new(move || now),
        regions: portal.regions.clone(),
        ui: UiConfig::Dev,
        pricing: Arc::new(common::pricing::Down),
        complaints: Arc::new(common::pricing::Down),
        commission: Arc::new(common::commission::seeded()),
        finance: Arc::new(common::pricing::Down),
        claims: Arc::new(common::claims::Down),
        reinsurance: Arc::new(common::pricing::Down),
        payments: Arc::new(common::pricing::Down),
        collections: Arc::new(common::pricing::Down),
        correspondence: Arc::new(common::pricing::Down),
        wellness: Arc::new(common::pricing::Down),
        underwriting: Arc::new(common::pricing::Down),
        master: None,
        intake: None,
        documents: None,
        group: None,
    }));
    for (path, body) in [
        ("/staff/v1/reinsurance/exceptions", None),
        ("/staff/v1/reinsurance/commands", Some(json!({ "retry_notice": { "seq": 4 } }))),
        (&*format!("/staff/v1/reinsurance/sign/{id}"), Some(json!({}))),
    ] {
        let request = Request::builder()
            .method(if body.is_some() { "POST" } else { "GET" })
            .uri(path)
            .header("x-dev-subject", "sub-ri2")
            .header("idempotency-key", "k");
        let response = down
            .clone()
            .oneshot(request.body(Body::from(body.map(|b| b.to_string()).unwrap_or_default())).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{path}");
    }
    assert_eq!(ask("sub-ri2", "me").await.1["pending"][0]["signed_by"], "ri-2");
    assert_eq!(decide("sub-ri1", "withdraw", id.clone()).await.1["title"], "signed");
    let (status, sent) = decide("sub-ri2", "sign", id.clone()).await;
    assert_eq!(status, StatusCode::OK, "{sent}");
    assert!(!portal.reinsurance.open("delivery:1"), "the abandonment closed the exception");
    assert_eq!(ask("sub-ri2", "me").await.1["pending"], json!([]));
    let abandoned = ask("sub-riview", "abandoned").await.1;
    assert!(abandoned.to_string().contains("ri-2"), "the signer is the approver: {abandoned}");
    assert_eq!(decide("sub-ri2", "sign", id).await.0, StatusCode::NOT_FOUND);

    // A prepared abandonment the module refuses is finished with; one taken back never goes.
    let (_, prepared) = tell("sub-ri1", json!({ "abandon_notice": { "seq": 77, "reason": "no such notice" } })).await;
    let refused = prepared["prepared"].as_str().unwrap().to_string();
    assert_eq!(decide("sub-ri2", "sign", refused).await.0, StatusCode::CONFLICT);
    assert_eq!(ask("sub-ri2", "me").await.1["pending"], json!([]));
    let (_, prepared) = tell("sub-ri1", json!({ "abandon_notice": { "seq": 78, "reason": "raised in error" } })).await;
    let taken_back = prepared["prepared"].as_str().unwrap().to_string();
    assert_eq!(decide("sub-ri1", "withdraw", taken_back).await.0, StatusCode::OK);
    assert_eq!(ask("sub-ri2", "me").await.1["pending"], json!([]));

    // What one person sends alone goes straight to the module, which decides.
    let (status, problem) = tell("sub-ri1", json!({ "retry_notice": { "seq": 999 } })).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::CONFLICT, Some("not_refused")), "{problem}");
    // Resuming recoveries on a claim reopened after a reversed admission is one person's to do; the module checks the claim.
    let (status, problem) = tell("sub-ri1", json!({ "resume_recoveries": { "claim": "C1", "reference": "reviewed" } })).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::CONFLICT, Some("not_reopened")), "{problem}");
    // Offering a returned settlement again is one person's to do; the module checks the statement.
    let (status, problem) = tell("sub-ri1", json!({ "reoffer_settlement": { "number": 999, "reference": "file 12" } })).await;
    assert_eq!(
        (status, problem["title"].as_str()),
        (StatusCode::CONFLICT, Some("unknown_statement")),
        "{problem}"
    );

    // The other questions.
    assert_eq!(ask("sub-riview", "notices").await.0, StatusCode::OK);
    assert_eq!(ask("sub-riview", "abandoned_notices").await.0, StatusCode::OK);
    assert!(ask("sub-riview", "sweep_check?period_end=2027-02-28").await.1["green"].is_boolean());
    for bad in ["sweep_check", "statement?number=first", "statement", "reconcile", "sweep_check?period_end=soon"] {
        assert_eq!(ask("sub-riview", bad).await.0, StatusCode::BAD_REQUEST, "{bad}");
    }
}

/// Purpose: reinsurance operations run the reinsurer accounts from the console: a statement is cut and agreed by one
/// person, and its settlement is prepared by one person and instructed only when a different person signs, with the preparer
/// and the signer named to the module as `prepared_by` and `approved_by`; the reinsurers, their statements, balances and a
/// claim's consent can be read.
/// Why it matters: before this, nothing in the system cut a reinsurer's statement or instructed its settlement, so no
/// reinsurer was ever paid or billed. The module checks two people itself, but only by the names it is given, so the console
/// must name them from the sign-in and never from the page. A held settlement is not prepared at all (found by the scenario
/// run, SCN2-08, F2-08-11: it was prepared, then lost when the signature was refused).
#[tokio::test]
async fn the_reinsurer_accounts_are_run_from_the_console() {
    let portal = portal();
    let ask = |who: &'static str, rest: String| {
        let portal = &portal;
        async move { portal.get(who, &format!("/staff/v1/reinsurance/{rest}")).await }
    };
    let tell = |who: &'static str, command: Value| {
        let portal = &portal;
        async move { portal.write("POST", who, "/staff/v1/reinsurance/commands", command).await }
    };

    // The reinsurers the insurer deals with, and one's statements, before any is cut.
    let (status, exposure) = ask("sub-riview", "exposure?on=2027-02-01".into()).await;
    assert_eq!(status, StatusCode::OK, "{exposure}");
    assert!(exposure.get("reinsurer-north").is_some(), "the ceded premium is owed to the panel: {exposure}");
    assert_eq!(ask("sub-riview", "statements?reinsurer=reinsurer-north".into()).await.1, json!([]));
    assert_eq!(ask("sub-riview", "statements".into()).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(
        ask("sub-riview", "statements?reinsurer=reinsurer-north&after=first".into()).await.0,
        StatusCode::BAD_REQUEST
    );

    // One person cuts the statement for the month and agrees it with the reinsurer's confirmation.
    let cut = json!({ "cut_statement": { "treaty": "retail-life-surplus", "reinsurer": "reinsurer-north", "period_end": "2027-02-28" } });
    let (status, answer) = tell("sub-ri1", cut).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    let (_, statements) = ask("sub-riview", "statements?reinsurer=reinsurer-north".into()).await;
    let number = statements[0]["number"].as_u64().unwrap_or_else(|| panic!("a statement was cut: {statements}"));
    let (status, answer) = tell("sub-ri1", json!({ "agree_statement": { "number": number, "reference": "north-conf-0227" } })).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(
        ask("sub-riview", "balance?treaty=retail-life-surplus&reinsurer=reinsurer-north".into()).await.0,
        StatusCode::OK
    );
    // A claim no reinsurer was asked about has no consent to show.
    assert_eq!(ask("sub-riview", "consent_status?claim=C1".into()).await.0, StatusCode::NOT_FOUND);

    // A held settlement is refused before it is prepared, saying to release the hold first.
    let (status, answer) = tell("sub-ri1", json!({ "hold_settlement": { "number": number, "reason": "query on the listing" } })).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    let (status, problem) = tell(
        "sub-ri1",
        json!({ "instruct_settlement": { "number": number, "reason": "February settlement" } }),
    )
    .await;
    assert_eq!(
        (status, problem["title"].as_str()),
        (StatusCode::CONFLICT, Some("settlement_held")),
        "{problem}"
    );
    assert!(problem["detail"].as_str().unwrap().contains("release the hold first"), "{problem}");
    assert_eq!(ask("sub-ri2", "me".into()).await.1["pending"], json!([]));
    let (status, answer) = tell("sub-ri1", json!({ "release_hold": { "number": number } })).await;
    assert_eq!(status, StatusCode::OK, "{answer}");

    // The page names neither the preparer nor the approver; a settlement needs a reason for the signer.
    for bad in [
        json!({ "instruct_settlement": { "number": number, "reason": "monthly", "prepared_by": "ri-2" } }),
        json!({ "instruct_settlement": { "number": number } }),
        json!({ "load_opening_balance": { "number": number } }),
    ] {
        assert_eq!(tell("sub-ri1", bad.clone()).await.0, StatusCode::BAD_REQUEST, "{bad}");
    }
    let (status, prepared) = tell(
        "sub-ri1",
        json!({ "instruct_settlement": { "number": number, "reason": "February settlement" } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{prepared}");
    let id = prepared["prepared"].as_str().unwrap().to_string();
    let waiting = ask("sub-ri2", "me".into()).await.1["pending"][0].clone();
    assert_eq!(waiting["command"]["instruct_settlement"]["prepared_by"], "ri-1", "{waiting}");
    let (status, sent) = portal.write("POST", "sub-ri2", &format!("/staff/v1/reinsurance/sign/{id}"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "the module accepted the two names: {sent}");
    let (_, statement) = ask("sub-riview", format!("statement?number={number}")).await;
    assert_eq!(statement["status"], "instructed", "{statement}");

    // A dispute is settled the same way, and the module decides whether there is one.
    let resolve = json!({ "resolve_dispute": { "reference": "d-1", "repay_minor": 0, "agreement_ref": "letter 4", "reason": "agreed with north" } });
    let (_, prepared) = tell("sub-ri1", resolve).await;
    let id = prepared["prepared"].as_str().unwrap().to_string();
    let (status, problem) = portal.write("POST", "sub-ri2", &format!("/staff/v1/reinsurance/sign/{id}"), json!({})).await;
    assert_eq!(
        (status, problem["title"].as_str()),
        (StatusCode::CONFLICT, Some("unknown_dispute")),
        "{problem}"
    );
}

/// Purpose: a member of claims staff finds work across every partition, reads a claim and acts on it as themselves, with the
/// purpose they gave, on the partition that holds the claim; anything only other modules send, and any attempt to act as a
/// colleague, goes no further.
/// Why it matters: the claims module trusts the caller `claims` to name the person acting. A console that let the page choose
/// the actor would let one assessor decide under another's authority, and one that sent a claim to the wrong partition would be
/// refused or, worse, answered from stale records.
#[tokio::test]
async fn the_claims_console() {
    let portal = portal();
    let ask = |who: &'static str, rest: &str| {
        let path = format!("/staff/v1/claims/{rest}");
        let portal = &portal;
        async move { portal.get(who, &path).await }
    };
    let tell = |who: &'static str, purpose: &'static str, command: Value| {
        let portal = &portal;
        async move {
            portal
                .write("POST", who, "/staff/v1/claims/commands", json!({ "purpose": purpose, "command": command }))
                .await
        }
    };

    // Only people with the claims action get in, and the page learns who it is acting as.
    assert_eq!(ask("sub-gl1", "me").await.0, StatusCode::FORBIDDEN);
    let (status, me) = ask("sub-cl1", "me").await;
    assert_eq!((status, &me["staff"], &me["partitions"]), (StatusCode::OK, &json!("cl-1"), &json!(2)));

    // An assessor reads the wording a cover is bound to, at the version it was bound to; others do not.
    let (status, listed) = ask("sub-cl1", "definitions").await;
    let rows = listed["rows"].as_array().unwrap();
    assert_eq!(status, StatusCode::OK);
    assert!(rows.iter().any(|r| r["id"] == "cancer" && r["version"] == 2 && r["text"].is_null()), "{listed}");
    let (status, first) = ask("sub-cl1", "definitions/cancer/1").await;
    let (_, second) = ask("sub-cl1", "definitions/cancer/2").await;
    assert_eq!((status, &first["versions"]), (StatusCode::OK, &json!([1, 2])));
    assert!(first["text"].as_str().unwrap().ends_with("version 1."), "{first}");
    assert!(second["text"].as_str().unwrap().ends_with("version 2."), "{second}");
    assert_ne!(first["text_hash"], second["text_hash"]);
    let (_, umbrella) = ask("sub-cl1", "definitions/aia-wording-1100-al-umb/1").await;
    assert!(umbrella["text"].as_str().unwrap().contains("AIA Living Umbrella Policy Wording"));
    for missing in ["definitions/cancer/9", "definitions/cancer/one", "definitions/no-such/1"] {
        assert_eq!(ask("sub-cl1", missing).await.0, StatusCode::NOT_FOUND, "{missing}");
    }
    assert_eq!(ask("sub-gl1", "definitions/cancer/1").await.0, StatusCode::FORBIDDEN);
    assert_eq!(ask("sub-gl1", "definitions").await.0, StatusCode::FORBIDDEN);

    // A phone notification goes to the partition holding the policy.
    let notify = json!({ "notify": { "claim": "c-1", "policy": "pol-c1", "cover": "trauma", "life": "life-1", "claim_type": "trauma",
        "event_date": "2027-02-20", "channel": "phone", "notifier": "policyholder", "claimant": "party-claimant" } });
    let (status, done) = tell("sub-cllead", "", notify.clone()).await;
    assert_eq!((status, &done["partition"]), (StatusCode::OK, &json!(1)), "{done}");
    assert_eq!(portal.claims.instructions(0), 0, "nothing reached the partition without the policy");
    let mut unknown = notify.clone();
    unknown["notify"]["policy"] = json!("pol-nowhere");
    assert_eq!(tell("sub-cllead", "", unknown).await.1["title"], "unknown_policy");

    // The queue gathers every partition's, with where each claim lives.
    let (status, queue) = ask("sub-cl1", "queue?queue=lump_sum").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (&queue["rows"][0]["claim"], &queue["rows"][0]["partition"]),
        (&json!("c-1"), &json!(1)),
        "{queue}"
    );
    assert_eq!(ask("sub-cl1", "queue?queue=nowhere").await.0, StatusCode::BAD_REQUEST);

    // The lead assigns the claim. Whatever actor the page sends, the actor is who signed in.
    let assign = json!({ "assign": { "claim": "c-1", "to": "cl-1", "actor": { "staff": "cl-1", "purpose": "pretending" } } });
    assert_eq!(tell("sub-cllead", "", assign.clone()).await.1["title"], "bad_request");
    let (status, done) = tell("sub-cllead", "allocating new lump sum work", assign).await;
    assert_eq!(status, StatusCode::OK, "{done}");
    let (status, claim) = ask("sub-cl1", "claim?claim=c-1").await;
    assert_eq!(
        (status, &claim["partition"], &claim["policy"], &claim["view"]["owner"]),
        (StatusCode::OK, &json!(1), &json!("pol-c1"), &json!("cl-1")),
        "{claim}"
    );
    // Staff work from the claim's own view: who owns it, its queue and money, nothing clinical.
    let (status, work) = ask("sub-cl1", "work?claim=c-1").await;
    assert_eq!(
        (status, &work["partition"], &work["view"]["owner"]),
        (StatusCode::OK, &json!(1), &json!("cl-1")),
        "{work}"
    );
    // The page links each wording from these: the cover's, and what each decision relied on.
    assert!(
        work["view"]["definitions"].is_array() && work["view"]["decision_definitions"].is_object(),
        "{work}"
    );
    // The payee is picked from the payments register, never typed: the portal fills in who it is
    // and the checks it passed, and refuses a payee that does not fit (F-11-1).
    let (status, payees) = ask("sub-cl1", "payees?text=party-p1").await;
    assert_eq!(
        (status, &payees["rows"][0]["payee"], &payees["rows"][0]["ending"]),
        (StatusCode::OK, &json!("p1"), &json!("4567")),
        "{payees}"
    );
    let verify = |kind: &str, payee: &str| json!({ "verify_payee": { "claim": "c-1", "account_ref": payee, "kind": kind } });
    let (status, refused) = tell("sub-cl1", "paying the claimant", verify("estate", "p1")).await;
    assert_eq!((status, &refused["title"]), (StatusCode::CONFLICT, &json!("payee_not_payable")), "{refused}");
    let (status, refused) = tell("sub-cl1", "paying the claimant", verify("beneficiary", "p-nobody")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    let (status, done) = tell("sub-cl1", "paying the claimant", verify("beneficiary", "p1")).await;
    assert_eq!(status, StatusCode::OK, "{done}");
    let (_, work) = ask("sub-cl1", "work?claim=c-1").await;
    assert_eq!(work["view"]["payee"][0], "party-p1", "{work}");
    // Investigation cases are answered for whoever signed in, gathered from every partition.
    let (status, cases) = ask("sub-cl1", "cases").await;
    assert_eq!((status, cases["rows"].is_array()), (StatusCode::OK, true), "{cases}");
    assert_eq!(ask("sub-cl1", "work?claim=c-404").await.0, StatusCode::NOT_FOUND);
    // The policy's claims as an adviser or employer would see them: status, never cause.
    let (status, policy) = ask("sub-cl1", "policy?policy=pol-c1").await;
    assert_eq!(
        (status, &policy["rows"][0]["partition"], &policy["rows"][0]["status"]["claim"]),
        (StatusCode::OK, &json!(1), &json!("c-1")),
        "{policy}"
    );
    assert_eq!(ask("sub-cl1", "policy").await.0, StatusCode::BAD_REQUEST);
    // The assessor records the cause as themselves; a colleague who does not own the claim cannot.
    let cause = json!({ "record_cause": { "claim": "c-1", "cause_code": "CC-0003" } });
    let (status, problem) = tell("sub-cllead", "assessing", cause.clone()).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::FORBIDDEN, Some("not_owner")), "{problem}");
    assert_eq!(tell("sub-cl1", "assessing the claim", cause).await.0, StatusCode::OK);
    // Someone the claims module's authority book does not know is refused by it.
    let (status, problem) = tell("sub-clnew", "assessing", json!({ "record_contact": { "claim": "c-1" } })).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::FORBIDDEN, Some("unknown_staff")), "{problem}");
    assert_eq!(
        tell("sub-cl1", "x", json!({ "record_contact": { "claim": "c-404" } })).await.1["title"],
        "unknown_claim"
    );

    // A repeated key is the first answer.
    let keyed = [("x-dev-subject", "sub-cl1".to_string()), ("idempotency-key", "contact-1".to_string())];
    let body = json!({ "purpose": "first contact", "command": { "record_contact": { "claim": "c-1" } } });
    assert_eq!(
        portal.send("POST", "/staff/v1/claims/commands", &keyed, Some(body.clone())).await.1["duplicate"],
        false
    );
    assert_eq!(portal.send("POST", "/staff/v1/claims/commands", &keyed, Some(body)).await.1["duplicate"], true);

    // What is not a person's command, or not a command, never leaves the console.
    for not_theirs in [
        json!({ "acknowledge": { "claim": "c-1", "delivery_ref": "d-1" } }),
        json!({ "record_death": { "party": "party-life", "date_of_death": "2027-02-01" } }),
        json!({ "drain_due": {} }),
        json!({ "approve_run": { "run": "r-1" } }),
        json!({ "record_contact": { "claim": "c-1" }, "escalate": { "claim": "c-1" } }),
        json!({ "record_contact": "c-1" }),
        json!({ "opened": { "claim": "c-1" } }),
        json!({ "identify_life": { "party": "p-1" } }),
    ] {
        assert_eq!(tell("sub-cl1", "testing", not_theirs.clone()).await.0, StatusCode::BAD_REQUEST, "{not_theirs}");
    }
    // A run is on the partition the person names.
    let run = json!({ "purpose": "monthly run", "partition": 9, "command": { "approve_run": { "run": "r-1" } } });
    assert_eq!(
        portal.write("POST", "sub-cllead", "/staff/v1/claims/commands", run).await.0,
        StatusCode::BAD_REQUEST
    );
    let run = json!({ "purpose": "monthly run", "partition": 1, "command": { "approve_run": { "run": "r-1" } } });
    assert_eq!(
        portal.write("POST", "sub-cllead", "/staff/v1/claims/commands", run).await.1["title"],
        "unknown_run"
    );
    // An investigation case names no claim: it goes to the partition the cases list gave,
    // and without one it goes nowhere.
    let close = |partition: Option<u32>| json!({ "purpose": "case review", "partition": partition, "command": { "close_investigation": { "case": "inv-1", "outcome": "cleared" } } });
    assert_eq!(
        portal.write("POST", "sub-cllead", "/staff/v1/claims/commands", close(None)).await.0,
        StatusCode::BAD_REQUEST
    );
    let (status, refused) = portal.write("POST", "sub-cllead", "/staff/v1/claims/commands", close(Some(1))).await;
    assert_ne!(status, StatusCode::BAD_REQUEST, "{refused}");
    assert!(refused["title"].is_string(), "the claims module decided it: {refused}");

    // The book and report come by partition; a remittance is asked of the run's own.
    let (status, book) = ask("sub-cl1", "book").await;
    assert_eq!((status, book["partitions"].as_array().map(Vec::len)), (StatusCode::OK, Some(2)), "{book}");
    assert_eq!(ask("sub-cl1", "report").await.1["partitions"][1]["partition"], 1);
    assert_eq!(ask("sub-cl1", "remittance?run=r-1").await.0, StatusCode::BAD_REQUEST);
    assert_eq!(ask("sub-cl1", "remittance?run=r-1&partition=1").await, (StatusCode::OK, json!({})));
    assert_eq!(ask("sub-cl1", "claim").await.0, StatusCode::BAD_REQUEST);
    assert_eq!(ask("sub-cl1", "everything").await.0, StatusCode::BAD_REQUEST);

    // The partitions have no leader.
    let now = portal.now();
    let down = router(Arc::new(AppState {
        wordings: Arc::default(),
        public_url: None,
        returns: Arc::default(),
        channel: portal.local.clone(),
        customers: Auth::Dev,
        workforce: Auth::Dev,
        clock: Arc::new(move || now),
        regions: portal.regions.clone(),
        ui: UiConfig::Dev,
        pricing: Arc::new(common::pricing::Down),
        complaints: Arc::new(common::pricing::Down),
        commission: Arc::new(common::commission::seeded()),
        finance: Arc::new(common::pricing::Down),
        claims: Arc::new(common::claims::Down),
        reinsurance: Arc::new(common::pricing::Down),
        payments: Arc::new(common::pricing::Down),
        collections: Arc::new(common::pricing::Down),
        correspondence: Arc::new(common::pricing::Down),
        wellness: Arc::new(common::pricing::Down),
        underwriting: Arc::new(common::pricing::Down),
        master: None,
        intake: None,
        documents: None,
        group: None,
    }));
    for (method, path, body) in [
        ("GET", "/staff/v1/claims/queue?queue=lump_sum", None),
        ("GET", "/staff/v1/claims/remittance?run=r-1&partition=0", None),
        (
            "POST",
            "/staff/v1/claims/commands",
            Some(json!({ "purpose": "p", "command": { "record_contact": { "claim": "c-1" } } })),
        ),
        ("POST", "/staff/v1/claims/commands", Some(notify_body())),
        (
            "POST",
            "/staff/v1/claims/commands",
            Some(json!({ "purpose": "p", "partition": 0, "command": { "approve_run": { "run": "r-1" } } })),
        ),
    ] {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header("x-dev-subject", "sub-cl1")
            .header("idempotency-key", "k");
        let response = down
            .clone()
            .oneshot(request.body(Body::from(body.map(|b| b.to_string()).unwrap_or_default())).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{path}");
        // Not connected says what to do next, not only what is wrong (found by the scenario run, F-24-4).
        if path.ends_with("disclosures") {
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let problem: Value = serde_json::from_slice(&body).unwrap();
            assert!(problem["detail"].as_str().unwrap().contains("the platform team connects it"), "{problem}");
        }
    }
}

fn notify_body() -> Value {
    json!({ "purpose": "", "command": { "notify": { "claim": "c-9", "policy": "pol-c0", "cover": "life", "claim_type": "death",
        "event_date": "2027-02-20", "channel": "phone", "notifier": "executor" } } })
}

/// Purpose: the finance console end to end against the finance ledger's own code: only staff with a finance action get in and a
/// reader cannot act; the close dashboard, checks, work and trial balance are shown; a journal is proposed with exact amounts,
/// refused to someone without the approver's role and to its own maker, and posted when a second person approves it; a policy's
/// accounting group is found from its number alone; commands that
/// ledger takes only from its own services never leave the console; and its refusals come back with their codes.
/// Why it matters: manual journals and the close are how people change and sign the books. The ledger trusts the console about who
/// the person is and which roles they hold, so a console that let a reader post, stated roles nobody holds, or rounded an amount in
/// the browser would break the controls auditors test first. The dashboard needed a `today` nobody should have to give, and the
/// MCP descriptions did not name the period's form or the commission questions' fields (found by the scenario run, F2-18-5).
#[tokio::test]
async fn the_finance_console() {
    let portal = portal();
    let ask = |who: &'static str, rest: &str| {
        let path = format!("/staff/v1/finance/{rest}");
        let portal = &portal;
        async move { portal.get(who, &path).await }
    };
    let tell = |who: &'static str, command: Value| {
        let portal = &portal;
        async move { portal.write("POST", who, "/staff/v1/finance/commands", command).await }
    };
    let journal = |id: &str, lines: Value| {
        json!({ "propose_journal": { "journal": { "id": id, "kind": "accrual", "period": "2027-11", "value_date": "2027-11-30", "lines": lines,
            "reason": "month-end accrual", "evidence": ["vault://evidence/1"] } } })
    };
    let lines = |minor: Value| {
        json!([
            { "account": "6500", "currency": "NZD", "signed_minor": minor, "fund": "SF1-life" },
            { "account": "2500", "currency": "NZD", "signed_fine": -25_000_000_000_i64, "fund": "SF1-life" },
        ])
    };

    // Who gets in, and who may act.
    for outsider in ["sub-kiri", "sub-sue", "sub-cmp1"] {
        assert_eq!(ask(outsider, "status").await.0, StatusCode::FORBIDDEN, "{outsider}");
        assert_eq!(tell(outsider, journal("j0", lines(json!(250_00)))).await.0, StatusCode::FORBIDDEN);
    }
    assert_eq!(ask("sub-aud", "me").await.1, json!({ "view": true, "roles": [] }));
    assert_eq!(ask("sub-gllead", "me").await.1["roles"], json!(["finance_lead", "appointed_actuary"]));
    let (status, problem) = tell("sub-aud", journal("j0", lines(json!(250_00)))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(problem["detail"].as_str().unwrap().contains("no role"));

    // What is shown.
    let (status, state) = ask("sub-aud", "status").await;
    assert_eq!((status, &state["configured"]), (StatusCode::OK, &json!(true)), "{state}");
    assert!(state["chart"].is_number() && state["close_config"].is_number());
    assert!(ask("sub-aud", "work").await.1["closing"].is_array());
    let (status, dashboard) = ask("sub-aud", "dashboard?period=2027-11&today=2027-12-03").await;
    assert_eq!(status, StatusCode::OK, "{dashboard}");
    assert!(!dashboard["steps"].as_array().unwrap().is_empty());
    // Without a date the close is read as of the books' date: nobody has to say what today is (F2-18-5).
    let (status, dashboard) = ask("sub-aud", "dashboard?period=2027-11").await;
    assert_eq!(status, StatusCode::OK, "{dashboard}");
    let tools = ic_channels::mcp::catalogue();
    let described = |name: &str| tools.iter().find(|t| t.name == name).unwrap().query;
    assert!(described("finance_question").contains("YYYY-MM-ADJ"));
    assert!(described("commission_question").contains("clawback_exposure: `date`"));
    for fine in [
        "checks?period=2027-11",
        "trial_balance?period=2027-11&fund=SF1-life",
        "movements?period=2027-11",
        "aged_suspense?as_at=2027-11-30",
        "accounting_groups",
        "tax_reconciliation?period=2027-11",
        "proposals",
    ] {
        assert_eq!(ask("sub-aud", fine).await.0, StatusCode::OK, "{fine}");
    }
    // A return is filed on a definition the service loaded from reviewed source, named, never typed.
    let (status, definitions) = ask("sub-aud", "return_definitions").await;
    let listed = &definitions["rows"][0];
    assert_eq!((status, &listed["name"]), (StatusCode::OK, &json!("insurer-quarterly@1")), "{definitions}");
    assert_eq!(listed["lines"][0], json!({ "code": "A1", "label": "Cash and settlement balances" }));
    assert_eq!(listed["hash"].as_str().map(str::len), Some(64));
    let file = |definition: Value| json!({ "file_return": { "id": "q-2027-4", "definition": definition, "period": "2027-11" } });
    for typed in [
        json!({ "id": "mine", "version": 1, "lines": [] }),
        json!("insurer-quarterly"),
        json!("insurer-quarterly@9"),
        json!("nothing@1"),
    ] {
        let (status, problem) = tell("sub-gl1", file(typed.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{typed}");
        assert!(problem["detail"].as_str().unwrap().contains("as it is listed"), "{problem}");
    }
    // The named one reaches the ledger whole, which refuses only because the period is still open.
    let (status, problem) = tell("sub-gl1", file(json!("insurer-quarterly@1"))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{problem}");
    let (status, problem) = tell(
        "sub-gllead",
        json!({ "restate_return": { "id": "q-2027-4", "definition": "insurer-quarterly@1", "reason": "late claim" } }),
    )
    .await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::CONFLICT, Some("return")), "{problem}");
    // A figure on a return nobody has filed is the ledger's refusal.
    assert_eq!(ask("sub-aud", "return_line?id=q-2027-4&code=A1").await.0, StatusCode::BAD_REQUEST);
    assert_eq!(ask("sub-aud", "return_line?id=q-2027-4").await.0, StatusCode::BAD_REQUEST);
    // One policy's accounting group: the partition is a number.
    // A policy not yet placed has no group: the ledger answers nothing rather than a guess.
    assert_eq!(ask("sub-aud", "accounting_group?policy=nope&partition=1").await, (StatusCode::OK, Value::Null));
    assert_eq!(ask("sub-aud", "accounting_group?policy=nope&partition=one").await.0, StatusCode::BAD_REQUEST);
    // Finance names a policy by its number; the console finds where it is kept. A number nobody holds is said to be unknown.
    let typed = portal.retail.0.to_uppercase();
    assert_eq!(ask("sub-aud", &format!("accounting_group?policy={typed}")).await, (StatusCode::OK, Value::Null));
    let (status, problem) = ask("sub-aud", "accounting_group?policy=POL-NOBODY").await;
    assert_eq!(
        (status, problem["detail"].as_str().unwrap().contains("POL-NOBODY")),
        (StatusCode::NOT_FOUND, true),
        "{problem}"
    );
    // Only the console's questions, each with its fields; one the ledger cannot read is its refusal.
    for bad in [
        "rate_table?id=life-base",
        "dashboard",
        "checks?period=November",
        "extract?kind=x&period=2027-11&limit=many",
        "journal?id=nope",
    ] {
        assert_eq!(ask("sub-aud", bad).await.0, StatusCode::BAD_REQUEST, "{bad}");
    }

    // The dry run is not a question to read: it is posted with the two table files.
    assert_eq!(ask("sub-aud", "table_dry_run?as_at=2027-11-30").await.0, StatusCode::METHOD_NOT_ALLOWED);

    // What is not a person's command never leaves the console.
    for not_theirs in [
        json!({ "configure": { "partitions": 9, "year_end_month": 6 } }),
        json!({ "ingest": { "partition": 0, "events": [] } }),
        json!({ "propose_chart": { "chart": {} } }),
        json!({ "propose_close_config": { "config": {} } }),
        json!({ "report_aged_items": { "kind": "suspense", "as_at": "2027-11-30", "items": [], "total_minor": 0, "count": 0 } }),
        json!({ "approve_journal": { "id": "j1" }, "close_period": { "period": "2027-11" } }),
        json!("close_period"),
        journal("j0", lines(json!(9_300_000_000_000_i64))),
        journal("j0", lines(json!("250.00"))),
    ] {
        assert_eq!(tell("sub-gl1", not_theirs.clone()).await.0, StatusCode::BAD_REQUEST, "{not_theirs}");
    }
    assert_eq!(tell("sub-gl1", json!({ "approve_journal": { "id": 7 } })).await.0, StatusCode::BAD_REQUEST);

    // A check people do is reported from the console; one a module owns is not, and the ledger says so.
    let report = |check: &str| json!({ "report_check": { "period": "2027-11", "check": check, "outcome": { "result": "green" } } });
    assert_eq!(tell("sub-gl1", report("tax_reconciliation")).await.0, StatusCode::OK);
    let (status, problem) = tell("sub-gl1", report("commission_sweep")).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::CONFLICT, Some("caller")), "{problem}");

    // A journal: proposed in minor units, kept exactly, posted only by a second person with the approver's role.
    let keyed = [("x-dev-subject", "sub-gl1".to_string()), ("idempotency-key", "j1".to_string())];
    let proposal = journal("j1", lines(json!(250_00)));
    let (status, first) = portal.send("POST", "/staff/v1/finance/commands", &keyed, Some(proposal.clone())).await;
    assert_eq!((status, &first["duplicate"]), (StatusCode::OK, &json!(false)), "{first}");
    assert_eq!(
        portal.send("POST", "/staff/v1/finance/commands", &keyed, Some(proposal.clone())).await.1["duplicate"],
        true
    );
    let unkeyed = [("x-dev-subject", "sub-gl1".to_string())];
    assert_eq!(
        portal.send("POST", "/staff/v1/finance/commands", &unkeyed, Some(proposal)).await.0,
        StatusCode::BAD_REQUEST
    );
    let held = ask("sub-gl2", "journal?id=j1").await.1;
    assert_eq!(
        (&held["maker"], &held["status"]["status"], &held["draft"]["lines"][0]["signed_fine"]),
        (&json!("gl-1"), &json!("proposed"), &json!(25_000_000_000_i64)),
        "{held}"
    );
    let approve = json!({ "approve_journal": { "id": "j1" } });
    let (status, problem) = tell("sub-gl1", approve.clone()).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::FORBIDDEN, Some("authority")), "{problem}");
    assert_eq!(tell("sub-gl2", approve).await.0, StatusCode::OK);
    assert_eq!(ask("sub-aud", "journal?id=j1").await.1["status"]["status"], "approved");
    let balance = ask("sub-aud", "trial_balance?period=2027-11").await.1;
    assert!(balance.as_array().unwrap().iter().any(|row| row["account"] == "6500"), "{balance}");
    // The ledger keeps maker and checker apart whatever roles one person holds.
    assert_eq!(tell("sub-gllead", journal("j2", lines(json!(250_00)))).await.0, StatusCode::OK);
    // A command with no fields goes by its name alone; a reader may not send it.
    assert_eq!(tell("sub-aud", json!("classify_policies")).await.0, StatusCode::FORBIDDEN);
    assert_eq!(tell("sub-gllead", json!("classify_policies")).await.0, StatusCode::OK);
    assert_eq!(tell("sub-gllead", json!(["classify_policies"])).await.0, StatusCode::BAD_REQUEST);
    // A cut-over total comes in minor units and reaches the ledger on its own scale: here the ledger
    // refuses it only because the account is not a migration control account.
    let cutover =
        |minor: Value| json!({ "record_cutover_total": { "account": "1999", "unit": { "Currency": "NZD" }, "total_minor": minor, "evidence": "legacy TB" } });
    let (status, problem) = tell("sub-gllead", cutover(json!(1_000_00))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{problem}");
    assert!(problem.to_string().contains("migration control"), "{problem}");
    assert_eq!(tell("sub-gllead", cutover(json!(i64::MAX))).await.0, StatusCode::BAD_REQUEST);
    let (status, problem) = tell("sub-gllead", json!({ "approve_journal": { "id": "j2" } })).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::CONFLICT, Some("separation")), "{problem}");

    // The ledger has no leader.
    let now = portal.now();
    let down = router(Arc::new(AppState {
        wordings: Arc::default(),
        public_url: None,
        returns: Arc::default(),
        channel: portal.local.clone(),
        customers: Auth::Dev,
        workforce: Auth::Dev,
        clock: Arc::new(move || now),
        regions: portal.regions.clone(),
        ui: UiConfig::Dev,
        pricing: Arc::new(common::pricing::Down),
        complaints: Arc::new(common::pricing::Down),
        commission: Arc::new(common::commission::seeded()),
        finance: Arc::new(common::pricing::Down),
        claims: Arc::new(common::claims::Down),
        reinsurance: Arc::new(common::pricing::Down),
        payments: Arc::new(common::pricing::Down),
        collections: Arc::new(common::pricing::Down),
        correspondence: Arc::new(common::pricing::Down),
        wellness: Arc::new(common::pricing::Down),
        underwriting: Arc::new(common::pricing::Down),
        master: None,
        intake: None,
        documents: None,
        group: None,
    }));
    for (method, path, body) in [
        ("GET", "/staff/v1/finance/work", None),
        ("POST", "/staff/v1/finance/commands", Some(json!({ "close_period": { "period": "2027-11" } }))),
    ] {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header("x-dev-subject", "sub-gllead")
            .header("idempotency-key", "k");
        let bytes = body.map(|b| b.to_string()).unwrap_or_default();
        let response = down.clone().oneshot(request.body(Body::from(bytes)).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{path}");
        let problem: Value = serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(problem["title"], "finance_unavailable");
    }
}

/// Purpose: serve the seeded portal on a real port so the pages can be driven in a browser by `tests/browser/check.js`.
/// Why it matters: the pages are script; only a browser shows whether they run under the content security policy and render what the service returns.
#[tokio::test]
#[ignore = "serves until killed; started by tests/browser/check.js"]
async fn serve_for_a_browser() {
    let addr = std::env::var("IC_BROWSER_ADDR").unwrap_or_else(|_| "127.0.0.1:8099".into());
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    // Staff's group console works against the group module itself, beside the employer's double.
    let staff = Arc::new(common::group_module::seeded());
    let mut portal = portal_with(|employer| Arc::new(common::group_module::Beside { employer, staff }));
    // The group was paid an override, on its first statement.
    let fap: ic_distribution::Statement = serde_json::from_value(json!({ "payee": "fap-1", "number": 1, "date": "2027-11-30", "cut_seq": 9,
        "opening_minor": 0, "lines": [{ "date": "2027-11-15", "reference": "r1", "kind": "commission", "ledger": "commission_payable",
        "policy": "pol-1", "book": null, "layer": null, "component": { "override": "group" }, "earner": "e1", "amount_minor": 30_00 }],
        "closing_minor": 30_00, "held_minor": 0, "held_reasons": [], "retention_minor": 0, "advance_outstanding_minor": 0,
        "advance_recovered_minor": 0, "payment_held": null, "payments": [], "hash": "h" }))
    .unwrap();
    portal.commission.0.lock().unwrap().0.statements.insert("fap-1.1".into(), fap);
    // A marker that has reached its review date, and a prompt to consider one.
    *portal.master.reviews.lock().unwrap() = vec![
        ("rv-1".to_string(), KIRI.to_string(), "review_due".to_string()),
        ("rv-2".to_string(), "party-7".to_string(), "death_of_partner".to_string()),
    ];
    let marker = json!({ "kind": "bereavement", "review_on": "2027-11-01", "adjustments": ["more_time", "plain_language"] });
    portal.master.markers.lock().unwrap().insert(KIRI.to_string(), marker);
    // Kiri appoints the adviser, so the adviser's book has a policy in it.
    // Her life cover is on a claims partition, so a claim she tells us about is registered.
    portal.claims.cover(0, &portal.retail.to_string());
    let appoint = format!("/app/v1/policies/{}/advisers", portal.retail);
    let (status, _) = portal
        .write("POST", "sub-kiri", &appoint, json!({ "adviser": "adv-1", "engagement": "e1" }))
        .await;
    assert_eq!(status, StatusCode::OK);
    // The adviser prepared two changes for Kiri to approve: one that moves money and one that does not.
    for (id, kind) in [("req-cover", "decline_indexation"), ("req-money", "withdrawal")] {
        let prepared = json!({ "id": id, "policy": portal.retail, "kind": kind, "module": "policy-servicing", "payload": { "note": "as we discussed" } });
        let (status, body) = portal.write("POST", "sub-adv", "/app/v1/requests", prepared).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    }
    // Underwriting has a proposal waiting on the applicant for this policy.
    let now = portal.now();
    let retail = portal.retail.clone();
    portal.local.with(|state| {
        use ic_log::StateMachine;
        let rows = json!({ "publish_rows": { "rows": [{ "key": format!("{retail}/underwriting/prop-1"), "policy": retail, "kind": "proposal", "subject": null,
            "sensitive": false, "body": { "proposal": "prop-1", "policy_number": "KTL00000001", "entry": "adviser", "status": "open",
            "waiting_on": "applicant", "received_on": "2027-11-10", "expires_on": "2028-02-08" } },
            // The product changed before the issue: Kiri is asked to accept a new price.
            { "key": format!("{retail}/underwriting/prop-1/L1"), "policy": retail, "kind": "proposal_life", "subject": KIRI, "sensitive": true,
              "body": { "proposal": "prop-1", "life": "L1", "stage": "accepted", "waiting_on": null,
                "requote": { "code": "product_changed", "quoted_minor": 1100, "application": application(78, "term-life") } } }], "remove": [] } });
        // And the documents module issued the policy schedule.
        let document = json!({ "publish_rows": { "rows": [{ "key": format!("{retail}/documents/doc-1"), "policy": retail, "kind": "document", "subject": null,
            "sensitive": false, "body": { "document": "doc-1", "title": "Policy schedule", "kind": "schedule", "issued_on": "2027-11-01" } }], "remove": [] } });
        for (n, (by, rows)) in [("underwriting", rows), ("documents", document)].into_iter().enumerate() {
            let command: Command = serde_json::from_value(rows).unwrap();
            let output = state.apply(
                9_000 + n as u64,
                ic_channels::Envelope {
                    key: format!("published-{n}"),
                    at: now,
                    actor: module(by),
                    command,
                },
            );
            assert!(output.result.is_ok(), "{:?}", output.result);
        }
    });
    // One of the adviser's layers was treated as a replacement.
    portal.commission.matches(
        json!([{ "layer": "pol-new/life/1", "policy": "pol-new", "writing": ["e1"], "matched_minor": 500_000_00,
        "sum_insured_minor": 600_000_00, "reason": "same_group", "on": "2027-11-10", "replaced": ["pol-old/life/1"], "disputed": null, "overturned": false }]),
    );
    println!("policy {}", portal.retail);
    // Kiri's phone, for the browser check: it signs what the phone app would, here an
    // approval of a new browser (`enrol\n{device}\n{key}`). Never part of the service.
    let phone = Arc::new(std::mem::replace(&mut portal.phone, Key::new()));
    let signs = axum::routing::post(move |body: String| async move { phone.sign(body.as_bytes()) });
    axum::serve(listener, portal.app.clone().route("/test/v1/phone-signs", signs)).await.unwrap();
}

/// Purpose: a customer reads their own record from the customer master, changes contact details and consents only from an enrolled
/// device that proved its key, cannot send any other command, has their version and key passed on, gets the master's own answer
/// (including a stale-version refusal), takes a copy of their record from a device, puts a hold on their own money going out and
/// asks for it to end with a fresh identity check, and is told plainly when the master is down or not configured.
/// Why it matters: this is the Privacy Act self-service route; the portal must only ever ask about the person signed in, must not
/// widen what a person may change beyond the master's own policy, and a retried change must carry the same key so it applies once.
#[tokio::test]
async fn a_person_keeps_their_own_customer_record() {
    let portal = &portal();
    let (status, record) = portal.get("sub-kiri", "/app/v1/profile").await;
    assert_eq!((status, &record["facts"]["given_name"]["value"]), (StatusCode::OK, &json!("Kiri")));
    // The adviser signed in reads their own party, never Kiri's.
    assert_eq!(portal.get("sub-adv", "/app/v1/profile").await.0, StatusCode::NOT_FOUND);
    assert_ne!(portal.master.calls.lock().unwrap()[1].party, KIRI);

    // A change needs the device, not just a token.
    let change = json!({ "command": "record", "facts": [{ "field": "email", "value": "kiri@new.nz" }] });
    let (status, problem) = portal.write("POST", "sub-kiri", "/app/v1/profile/commands", change.clone()).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::UNAUTHORIZED, Some("device_required")));
    let send = |body: Value, version: Option<&str>| {
        let text = body.to_string();
        let mut headers = portal.proof(&portal.phone, "POST", "/app/v1/profile/commands", &text);
        if let Some(v) = version {
            headers.push(("if-match", v.to_string()));
        }
        async move { (portal.send("POST", "/app/v1/profile/commands", &headers, Some(body)).await, headers) }
    };
    // Only contact details and consents are the person's own to change.
    for refused in [json!({ "command": "merge" }), json!({ "fields": {} })] {
        let ((status, _), _) = send(refused, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    let before = portal.master.calls.lock().unwrap().len();
    let ((status, answer), headers) = send(change.clone(), Some("1")).await;
    assert_eq!((status, &answer["version"]), (StatusCode::OK, &json!("2")));
    // The key is scoped to the person, so two people cannot collide on one.
    let call = portal.master.calls.lock().unwrap()[before].clone();
    let key = headers.iter().find(|(n, _)| *n == "idempotency-key").unwrap().1.clone();
    assert_eq!(
        (call.what, call.party.as_str(), call.version.as_deref(), call.key),
        ("command", KIRI, Some("1"), Some(format!("{KIRI}:{key}")))
    );
    assert_eq!(portal.get("sub-kiri", "/app/v1/profile").await.1["facts"]["email"]["value"], "kiri@new.nz");
    // A change made against a version the person no longer sees is the master's refusal, passed through.
    let ((status, problem), _) = send(json!({ "command": "set_consent", "consent": "marketing", "given": false }), Some("1")).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::PRECONDITION_FAILED, Some("version_mismatch")));

    // A copy of the record needs the device too.
    assert_eq!(portal.get("sub-kiri", "/app/v1/profile/export").await.1["title"], "device_required");
    let proved = portal.proof(&portal.phone, "GET", "/app/v1/profile/export", "");
    let (status, export) = portal.send("GET", "/app/v1/profile/export", &proved, None).await;
    assert_eq!((status, &export["party"]), (StatusCode::OK, &json!(KIRI)));
    // A readable copy of everything held, not the master's event stream alone (found by the scenario run, SCN-08).
    assert_eq!(export["your_details"]["party"], KIRI, "{export}");
    assert!(export["history_of_your_details"].is_object(), "{export}");
    assert_eq!(export["policies"][0]["policy"], json!(portal.retail.0), "{export}");
    assert!(export["requests"].is_array() && export["devices"].is_array(), "{export}");

    // A hold on your own money going out: on at once, off only after a fresh identity check and the cooling-off period.
    assert_eq!(portal.get("sub-kiri", "/app/v1/profile/spending-hold").await.1["held"], false);
    let ((status, _), _) = send(json!({ "command": "place_spending_hold" }), None).await;
    assert_eq!(status, StatusCode::OK);
    let hold = portal.get("sub-kiri", "/app/v1/profile/spending-hold").await.1;
    assert_eq!((&hold["held"], &hold["removal"]), (&json!(true), &Value::Null), "{hold}");
    assert_eq!(
        portal.master.calls.lock().unwrap().last().unwrap().party,
        KIRI,
        "the hold asked about is the person's own"
    );
    let ((status, problem), _) = send(json!({ "command": "request_hold_removal" }), None).await;
    assert_eq!(
        (status, problem["title"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("verification_needed"))
    );
    let ((status, _), _) = send(json!({ "command": "request_hold_removal", "verification": "idv-2" }), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(portal.get("sub-kiri", "/app/v1/profile/spending-hold").await.1["removal"]["ends_at"].is_string());
    let ((status, _), _) = send(json!({ "command": "cancel_hold_removal" }), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(portal.get("sub-kiri", "/app/v1/profile/spending-hold").await.1["removal"], Value::Null);
    // Legal lifts it; a person cannot send that.
    let ((status, _), _) = send(json!({ "command": "override_spending_hold", "reference": "court-1" }), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // An unreachable master is a 503 the app can retry.
    portal.master.down.store(true, Ordering::SeqCst);
    let (status, problem) = portal.get("sub-kiri", "/app/v1/profile").await;
    assert_eq!(
        (status, problem["title"].as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("customer_master_unavailable"))
    );

    // And a portal deployed without one says so.
    let now = portal.now();
    let bare = router(Arc::new(AppState {
        wordings: Arc::default(),
        public_url: None,
        returns: Arc::default(),
        channel: portal.local.clone(),
        customers: Auth::Dev,
        workforce: Auth::Dev,
        clock: Arc::new(move || now),
        regions: portal.regions.clone(),
        ui: UiConfig::Dev,
        pricing: portal.pricing.clone(),
        complaints: Arc::new(common::pricing::Down),
        commission: Arc::new(common::commission::seeded()),
        finance: Arc::new(common::pricing::Down),
        claims: portal.claims.clone(),
        reinsurance: portal.reinsurance.clone(),
        payments: portal.payments.clone(),
        collections: portal.collections.clone(),
        correspondence: portal.correspondence.clone(),
        wellness: portal.wellness.clone(),
        underwriting: portal.underwriting.clone(),
        master: None,
        intake: None,
        documents: None,
        group: None,
    }));
    let request = Request::builder()
        .uri("/app/v1/profile")
        .header("x-dev-subject", "sub-kiri")
        .body(Body::empty())
        .unwrap();
    assert_eq!(bare.oneshot(request).await.unwrap().status(), StatusCode::SERVICE_UNAVAILABLE);
}

/// Purpose: staff with the workbench's view read the replacement matches; only the conduct officer may decide a disputed one, with
/// a purpose and a decision that names its layer; the decision reaches the commission module's own state machine as that person,
/// passes its `case.manage` authority check and is refused only for the layer, which here was never matched.
/// Why it matters: deciding a dispute takes commission back from an adviser or pays it; the portal must not let anyone else decide,
/// and must describe the person so that the commission module's own policy can check them again.
#[tokio::test]
async fn the_conduct_officer_decides_disputed_replacements() {
    let portal = portal();
    let (status, view) = portal.get("sub-fin1", "/staff/v1/replacements").await;
    assert_eq!((status, &view["decide"], &view["matches"]), (StatusCode::OK, &json!(false), &json!([])));
    assert_eq!(portal.get("sub-sue", "/staff/v1/replacements").await.0, StatusCode::FORBIDDEN);
    assert_eq!(portal.get("sub-wbcon", "/staff/v1/replacements").await.1["decide"], true);

    let decide = |who: &'static str, body: Value| portal.write("POST", who, "/staff/v1/replacements/decide", body);
    let body = json!({ "policy": "pol-9", "cover": "life", "layer": 1, "replacement": true, "purpose": "dispute D-1 reviewed" });
    assert_eq!(decide("sub-fin1", body.clone()).await.0, StatusCode::FORBIDDEN);
    assert_eq!(decide("sub-sue", body.clone()).await.0, StatusCode::FORBIDDEN);
    for bad in [
        json!({ "policy": "pol-9", "cover": "life", "layer": 1, "replacement": true }),
        json!({ "policy": "pol-9", "cover": "life", "layer": "first", "replacement": true, "purpose": "x" }),
        json!({ "policy": "", "cover": "life", "layer": 1, "replacement": true, "purpose": "x" }),
        json!({ "policy": "pol-9", "cover": "life", "layer": 1, "replacement": "yes", "purpose": "x" }),
    ] {
        assert_eq!(decide("sub-wbcon", bad.clone()).await.0, StatusCode::BAD_REQUEST, "{bad}");
    }
    let (status, problem) = decide("sub-wbcon", body).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::CONFLICT, Some("unknown_layer")), "{problem}");

    // A commission module that cannot be reached is said plainly.
    let now = portal.now();
    let down = router(Arc::new(AppState {
        wordings: Arc::default(),
        public_url: None,
        returns: Arc::default(),
        channel: portal.local.clone(),
        customers: Auth::Dev,
        workforce: Auth::Dev,
        clock: Arc::new(move || now),
        regions: portal.regions.clone(),
        ui: UiConfig::Dev,
        pricing: portal.pricing.clone(),
        complaints: Arc::new(common::pricing::Down),
        commission: Arc::new(common::pricing::Down),
        finance: Arc::new(common::pricing::Down),
        claims: portal.claims.clone(),
        reinsurance: portal.reinsurance.clone(),
        payments: portal.payments.clone(),
        collections: portal.collections.clone(),
        correspondence: portal.correspondence.clone(),
        wellness: portal.wellness.clone(),
        underwriting: portal.underwriting.clone(),
        master: None,
        intake: None,
        documents: None,
        group: None,
    }));
    for (method, body) in [
        ("GET", None),
        (
            "POST",
            Some(json!({ "policy": "p", "cover": "c", "layer": 1, "replacement": false, "purpose": "x" })),
        ),
    ] {
        let path = if body.is_some() {
            "/staff/v1/replacements/decide"
        } else {
            "/staff/v1/replacements"
        };
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header("x-dev-subject", "sub-wbcon")
            .header("idempotency-key", "k")
            .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
            .unwrap();
        assert_eq!(down.clone().oneshot(request).await.unwrap().status(), StatusCode::SERVICE_UNAVAILABLE, "{path}");
    }
}

/// Purpose: an adviser sees the replacement matches on business they wrote and no one else's, without the cover it replaced;
/// disputes one with a reason, which is recorded as their request, carried to the commission module under the portal's own caller
/// naming the adviser, and resolved with that module's answer, including a refusal; cannot dispute a layer
/// that is not theirs, one already disputed, or without a reason; cannot raise a dispute through the general request route; and an
/// adviser with no engagements sees none.
/// Why it matters: a match takes initial commission from the adviser who wrote the business, so they must be told and able to
/// dispute it, but a dispute must not be raised against another adviser's business or by going around the check.
#[tokio::test]
async fn an_adviser_disputes_their_own_replacement_match() {
    let portal = portal();
    let row = |layer: &str, writing: &str, disputed: Value| {
        json!({ "layer": layer, "policy": layer.split('/').next(), "writing": [writing], "matched_minor": 500_000_00, "sum_insured_minor": 600_000_00,
                "reason": "same_group", "on": "2027-11-10", "replaced": ["pol-old/life/1"], "disputed": disputed, "overturned": false })
    };
    portal.commission.matches(json!([
        row("pol-new/life/1", "e1", Value::Null),
        row("pol-other/life/1", "e-someone-else", Value::Null),
        row("pol-done/life/2", "e1", json!("dispute:1")),
    ]));
    let (status, mine) = portal.get("sub-adv", "/adviser/v1/replacements").await;
    assert_eq!(status, StatusCode::OK);
    let layers: Vec<&str> = mine.as_array().unwrap().iter().map(|m| m["layer"].as_str().unwrap()).collect();
    assert_eq!(layers, ["pol-new/life/1", "pol-done/life/2"]);
    assert!(mine[0].get("replaced").is_none() && mine[0].get("writing").is_none());
    assert_eq!(portal.get("sub-kiri", "/adviser/v1/replacements").await.1, json!([]));

    let dispute = |body: Value| portal.write("POST", "sub-adv", "/adviser/v1/replacements/dispute", body);
    for bad in [
        json!({ "layer": "pol-other/life/1", "reason": "mine" }),
        json!({ "layer": "pol-done/life/2", "reason": "again" }),
        json!({ "layer": "pol-new/life/1", "reason": "  " }),
        json!({ "reason": "no layer" }),
    ] {
        assert_eq!(dispute(bad.clone()).await.0, StatusCode::BAD_REQUEST, "{bad}");
    }
    let (status, done) = dispute(json!({ "layer": "pol-new/life/1", "reason": "the customer asked for new cover after a review" })).await;
    assert_eq!((status, &done["disputed"]), (StatusCode::OK, &json!("dispute:pol-new/life/1")), "{done}");
    // The commission module was told, by the portal under its own caller, naming the adviser and the request as the reference.
    let sent = portal.commission.disputes();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["actor"], json!({ "module": "adviser-portal" }));
    assert_eq!(
        sent[0]["command"]["dispute_replacement"],
        json!({ "policy": "pol-new", "cover": "life", "layer": 1, "reference": "dispute:pol-new/life/1", "adviser": "adv-1" })
    );
    let (_, requests) = portal.get("sub-adv", "/app/v1/requests").await;
    let raised = requests
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["kind"] == "dispute_replacement")
        .unwrap()
        .clone();
    assert_eq!(
        (&raised["id"], &raised["module"], &raised["status"]),
        (&json!("dispute:pol-new/life/1"), &json!("distribution"), &json!("applied"))
    );
    assert_eq!(
        raised["payload"],
        json!({ "policy": "pol-new", "cover": "life", "layer": 1, "reason": "the customer asked for new cover after a review" })
    );
    // A second dispute of the same layer is one request already made.
    let (status, problem) = dispute(json!({ "layer": "pol-new/life/1", "reason": "twice" })).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::CONFLICT, Some("duplicate")));
    // A dispute the commission module refuses is resolved with its refusal, which the adviser is told.
    portal
        .commission
        .matches(json!([row("pol-new/life/1", "e1", Value::Null), row("pol-new2/life/1", "e1", Value::Null)]));
    // The stand-in refuses a layer it has already been sent; make it count this one as sent.
    portal.commission.2.lock().unwrap()[0]["command"]["dispute_replacement"]["policy"] = json!("pol-new2");
    let (status, problem) = dispute(json!({ "layer": "pol-new2/life/1", "reason": "not one" })).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::CONFLICT, Some("already_disputed")));
    let (_, requests) = portal.get("sub-adv", "/app/v1/requests").await;
    let refused = requests
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "dispute:pol-new2/life/1")
        .unwrap()
        .clone();
    assert_eq!((&refused["status"], &refused["code"]), (&json!("rejected"), &json!("already_disputed")));
    // The commission module's own state machine takes the envelope from this caller: it gets as far as the layer, unknown there.
    use ic_channels::forward::Source;
    let real = common::commission::seeded();
    let envelope = ic_channels::replacements::dispute_envelope("dispute:pol-new/life/1", &raised["payload"], &p("adv-1"));
    let output = real.tell(envelope).await.unwrap();
    assert_eq!(output["result"]["Err"]["code"], "unknown_layer", "{output}");
    // The general route does not take a dispute, so the check above cannot be gone around.
    let around =
        json!({ "id": "x", "kind": "dispute_replacement", "module": "new-business", "payload": { "policy": "pol-other", "cover": "life", "layer": 1 } });
    assert_eq!(portal.write("POST", "sub-adv", "/app/v1/requests", around).await.0, StatusCode::BAD_REQUEST);
}

/// Purpose: the policy's adviser and a scheme's administrator see each claim on it with its status and dates and nothing else; the
/// policyholder's other helpers, and anyone with no part in the policy, get 404, as for a policy that does not exist.
/// Why it matters: Mitchell decided advisers and employers see a claim's status but never its cause; the route must carry no more
/// than the claims module's status view and must not tell a stranger that a claim exists.
#[tokio::test]
async fn advisers_and_employers_see_claim_status_and_nothing_more() {
    let portal = portal();
    let appoint = format!("/app/v1/policies/{}/advisers", portal.retail);
    assert_eq!(
        portal
            .write("POST", "sub-kiri", &appoint, json!({ "adviser": "adv-1", "engagement": "e1" }))
            .await
            .0,
        StatusCode::OK
    );
    portal.claims.cover(0, &portal.retail.to_string());
    let notify = json!({ "notify": { "claim": "c-9", "policy": portal.retail, "cover": "life", "life": "life-1", "claim_type": "death",
        "event_date": "2027-02-20", "channel": "phone", "notifier": "policyholder", "claimant": "party-claimant" } });
    let headers = [("x-dev-subject", "sub-cllead".to_string()), ("idempotency-key", "n-9".to_string())];
    let (status, done) = portal
        .send("POST", "/staff/v1/claims/commands", &headers, Some(json!({ "command": notify, "purpose": "" })))
        .await;
    assert_eq!(status, StatusCode::OK, "{done}");

    let path = format!("/app/v1/policies/{}/claims", portal.retail);
    let (status, view) = portal.get("sub-adv", &path).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let row = view["rows"][0].as_object().unwrap();
    assert_eq!(
        row.keys().map(String::as_str).collect::<Vec<_>>(),
        ["claim", "status", "notified_on", "closed_on"]
    );
    assert_eq!(row["claim"], "c-9");
    // The policyholder sees the same status of a claim on their own policy.
    assert_eq!(portal.get("sub-kiri", &path).await.1["rows"][0]["claim"], "c-9");
    let scheme = format!("/app/v1/policies/{}/claims", portal.scheme);
    assert_eq!(portal.get("sub-hr", &scheme).await, (StatusCode::OK, json!({ "rows": [] })));
    for (who, path) in [
        ("sub-hr", path.clone()),
        ("sub-adv", scheme.clone()),
        ("sub-kiri", "/app/v1/policies/nope/claims".to_string()),
    ] {
        assert_eq!(portal.get(who, &path).await.0, StatusCode::NOT_FOUND, "{who} {path}");
    }
}

/// Purpose: the person insured on a proposal gets what to sign for their answers, signs it on their enrolled device, and the portal
/// checks that signature before passing the answers to the underwriting intake with the device's enrolled key; anyone who is not
/// that life gets 404, a signature without a proven device or over another message is refused before anything leaves, a step that
/// is not one the app signs is refused, and a portal with no intake says so.
/// Why it matters: a disclosure is the basis of the cover and of any later non-disclosure review; it must be the life's own, signed
/// on a device bound to them, and clinical answers must never go to the intake on someone else's behalf.
#[tokio::test]
async fn the_person_insured_signs_their_disclosure() {
    let portal = &portal();
    let now = portal.now();
    let retail = portal.retail.clone();
    portal.local.with(|state| {
        use ic_log::StateMachine;
        let rows = json!({ "publish_rows": { "rows": [{ "key": format!("{retail}/underwriting/prop-1/life-1"), "policy": retail, "kind": "proposal_life",
            "subject": KIRI, "sensitive": true, "body": { "proposal": "prop-1", "life": "life-1", "stage": "disclosure", "waiting_on": "applicant" } }], "remove": [] } });
        let command: Command = serde_json::from_value(rows).unwrap();
        let output = state.apply(9_001, ic_channels::Envelope { key: "life-row".into(), at: now, actor: module("underwriting"), command });
        assert!(output.result.is_ok(), "{:?}", output.result);
    });
    let base = "/app/v1/proposals/prop-1/lives/life-1/disclosures";
    let answers = json!({ "step": "disclosure", "answers": { "q-smoker": [1, { "yes_no": false }] }, "adviser_entered": [] });

    // What to sign: only for the life, and only for a step the app signs.
    let (status, to_sign) = portal.write("POST", "sub-kiri", &format!("{base}/to-sign"), answers.clone()).await;
    assert_eq!(status, StatusCode::OK, "{to_sign}");
    assert_eq!(portal.intake.received.lock().unwrap()[0]["proposal"], "prop-1");
    assert_eq!(
        portal.write("POST", "sub-adv", &format!("{base}/to-sign"), answers.clone()).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        portal
            .write("POST", "sub-kiri", "/app/v1/proposals/prop-1/lives/life-2/disclosures/to-sign", answers.clone())
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let mut offer = answers.clone();
    offer["step"] = json!("offer");
    assert_eq!(
        portal.write("POST", "sub-kiri", &format!("{base}/to-sign"), offer).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        portal
            .write("POST", "sub-kiri", &format!("{base}/to-sign"), json!({ "step": "disclosure", "answers": [] }))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );

    // Signing: a proven device, and its signature over exactly that message.
    use base64::Engine;
    let message = base64::engine::general_purpose::STANDARD.decode(to_sign["message"].as_str().unwrap()).unwrap();
    let mut signed = answers.clone();
    signed["message"] = to_sign["message"].clone();
    signed["signature"] = json!(portal.phone.sign(&message));
    let send = |body: Value| {
        let text = body.to_string();
        let headers = portal.proof(&portal.phone, "POST", base, &text);
        async move { portal.send("POST", base, &headers, Some(body)).await }
    };
    let (status, problem) = portal.write("POST", "sub-kiri", base, signed.clone()).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::UNAUTHORIZED, Some("device_required")));
    let mut forged = signed.clone();
    forged["signature"] = json!(portal.phone.sign(b"something else"));
    let before = portal.intake.received.lock().unwrap().len();
    let (status, problem) = send(forged).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::BAD_REQUEST, Some("bad_signature")));
    let mut garbled = signed.clone();
    garbled["message"] = json!("not base64!");
    assert_eq!(send(garbled).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(portal.intake.received.lock().unwrap().len(), before, "nothing reached the intake");
    let (status, done) = send(signed).await;
    assert_eq!(
        (status, &done["vault_ref"]),
        (StatusCode::OK, &json!("clinical://party/disclosure/hash-1")),
        "{done}"
    );
    let sent = portal.intake.received.lock().unwrap().last().unwrap().clone();
    assert_eq!(
        (&sent["signature"]["key_id"], &sent["signature"]["public_key"]),
        (&json!("phone"), &json!(portal.phone.public))
    );
    assert!(sent.get("message").is_none(), "only the intake's own fields are passed on");

    // A portal with no intake configured.
    let bare = router(Arc::new(AppState {
        wordings: Arc::default(),
        public_url: None,
        returns: Arc::default(),
        channel: portal.local.clone(),
        customers: Auth::Dev,
        workforce: Auth::Dev,
        clock: Arc::new(move || now),
        regions: portal.regions.clone(),
        ui: UiConfig::Dev,
        pricing: portal.pricing.clone(),
        complaints: Arc::new(common::pricing::Down),
        commission: portal.commission.clone(),
        finance: Arc::new(common::pricing::Down),
        claims: portal.claims.clone(),
        reinsurance: portal.reinsurance.clone(),
        payments: portal.payments.clone(),
        collections: portal.collections.clone(),
        correspondence: portal.correspondence.clone(),
        wellness: portal.wellness.clone(),
        underwriting: portal.underwriting.clone(),
        master: None,
        intake: None,
        documents: None,
        group: None,
    }));
    let request = Request::builder()
        .method("POST")
        .uri(format!("{base}/to-sign"))
        .header("x-dev-subject", "sub-kiri")
        .header("idempotency-key", "k")
        .body(Body::from(answers.to_string()))
        .unwrap();
    assert_eq!(bare.oneshot(request).await.unwrap().status(), StatusCode::SERVICE_UNAVAILABLE);
}

/// Purpose: a customer downloads their own document as a PDF and gets 404 for anyone else's or for an ID that could not be one; a
/// member of staff with `documents.download` downloads with a purpose, which is passed on, and is refused a clinical document; staff
/// without the action or without a purpose get nothing; a destroyed document is 410; and a documents module that is down or not
/// configured is 503.
/// Why it matters: the documents module decides by the party or the staff member and purpose the portal names, so the portal must
/// name only the person signed in, and must not hand back anything but the file it was given, with no caching.
#[tokio::test]
async fn documents_are_downloaded_by_their_owner_or_by_staff_with_a_purpose() {
    let portal = portal();
    let response = portal
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/app/v1/documents/doc-1/pdf")
                .header("x-dev-subject", "sub-kiri")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "application/pdf");
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["content-disposition"], "attachment; filename=\"doc-1.pdf\"");
    assert_eq!(response.into_body().collect().await.unwrap().to_bytes().as_ref(), b"%PDF-1.7 schedule");
    assert_eq!(
        portal.documents.asked.lock().unwrap()[0],
        ("doc-1".to_string(), ic_channels::documents::Asker::Party(KIRI.to_string()))
    );
    for (who, path) in [("sub-adv", "doc-1"), ("sub-kiri", "doc-2"), ("sub-kiri", "..%2Fsecret"), ("sub-kiri", "a%20b")] {
        assert_eq!(
            portal.get(who, &format!("/app/v1/documents/{path}/pdf")).await.0,
            StatusCode::NOT_FOUND,
            "{who} {path}"
        );
    }
    assert_eq!(portal.get("sub-kiri", "/app/v1/documents/doc-gone/pdf").await.0, StatusCode::GONE);
    assert_eq!(portal.get("sub-kiri", "/app/v1/documents/doc-odd/pdf").await.0, StatusCode::BAD_GATEWAY);
    assert_eq!(
        portal.get("sub-kiri", "/app/v1/documents/doc-busy/pdf").await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );

    let (status, _) = portal.get("sub-rec", "/staff/v1/documents/doc-1/pdf?purpose=complaint%20C-1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        portal.documents.asked.lock().unwrap().last().unwrap().1,
        ic_channels::documents::Asker::Staff {
            actor: "rec-1".into(),
            purpose: "complaint C-1".into()
        }
    );
    assert_eq!(
        portal.get("sub-rec", "/staff/v1/documents/doc-clinical/pdf?purpose=x").await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(portal.get("sub-rec", "/staff/v1/documents/doc-1/pdf").await.0, StatusCode::BAD_REQUEST);
    assert_eq!(
        portal.get("sub-rec", "/staff/v1/documents/doc-1/pdf?purpose=%20").await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(portal.get("sub-sue", "/staff/v1/documents/doc-1/pdf?purpose=x").await.0, StatusCode::FORBIDDEN);

    let now = portal.now();
    let bare = router(Arc::new(AppState {
        wordings: Arc::default(),
        public_url: None,
        returns: Arc::default(),
        channel: portal.local.clone(),
        customers: Auth::Dev,
        workforce: Auth::Dev,
        clock: Arc::new(move || now),
        regions: portal.regions.clone(),
        ui: UiConfig::Dev,
        pricing: portal.pricing.clone(),
        complaints: Arc::new(common::pricing::Down),
        commission: portal.commission.clone(),
        finance: Arc::new(common::pricing::Down),
        claims: portal.claims.clone(),
        reinsurance: portal.reinsurance.clone(),
        payments: portal.payments.clone(),
        collections: portal.collections.clone(),
        correspondence: portal.correspondence.clone(),
        wellness: portal.wellness.clone(),
        underwriting: portal.underwriting.clone(),
        master: None,
        intake: None,
        documents: None,
        group: None,
    }));
    let request = Request::builder()
        .uri("/app/v1/documents/doc-1/pdf")
        .header("x-dev-subject", "sub-kiri")
        .body(Body::empty())
        .unwrap();
    assert_eq!(bare.oneshot(request).await.unwrap().status(), StatusCode::SERVICE_UNAVAILABLE);
}

/// Purpose: at handover between scheme administrators, the joiner is added by the leaver and sees the scheme at once; the joiner then
/// removes the leaver, which itself tells the host to re-check the leaver's sessions now (the priority revocation), and the
/// leaver loses the scheme straight away; the identity service closes the leaver's sessions, after which a token issued
/// before then is refused; and the last administrator cannot be removed by the scheme's own administrators.
/// Why it matters: an administrator who has left must not keep seeing member data or make changes through a session still open, and
/// the scheme must never be left with nobody to run it.
#[tokio::test]
async fn an_employer_administrator_hands_over_at_noon() {
    let portal = portal();
    let scheme = portal.scheme.clone();
    let admin = |who: &'static str, party: &'static str, on: bool| {
        let path = format!("/employer/v1/schemes/{scheme}/admins/{party}");
        let portal = &portal;
        async move { portal.write("PUT", who, &path, json!({ "admin": on })).await }
    };
    assert_eq!(portal.get("sub-hr2", "/employer/v1/schemes").await.1, json!([]));
    assert_eq!(admin("sub-hr", "hr-2", true).await.0, StatusCode::OK);
    assert_eq!(portal.get("sub-hr2", "/employer/v1/schemes").await.1.as_array().unwrap().len(), 1);
    // The joiner removes the leaver; the removal itself tells the host to re-check the leaver's sessions now.
    let now = portal.now();
    portal.local.with(|state| {
        use ic_log::StateMachine;
        let output = state.apply(
            10_000,
            ic_channels::Envelope {
                key: "hr-1-leaves".into(),
                at: now,
                actor: party("hr-2"),
                command: Command::Directory(DirectoryCommand::SetEmployerAdmin {
                    policy: scheme.clone(),
                    party: p("hr-1"),
                    admin: false,
                }),
            },
        );
        assert!(output.result.unwrap().contains(&ic_channels::Effect::Revoked { party: p("hr-1") }));
    });
    assert_eq!(portal.get("sub-hr", "/employer/v1/schemes").await.1, json!([]));
    assert_eq!(portal.get("sub-hr", &format!("/app/v1/policies/{scheme}")).await.0, StatusCode::NOT_FOUND);
    // The leaver can no longer act on the scheme, and the joiner cannot leave it with nobody.
    assert_eq!(admin("sub-hr", "hr-1", true).await.0, StatusCode::FORBIDDEN);
    let (status, problem) = admin("sub-hr2", "hr-2", false).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("last_admin")));

    // The priority revocation: the leaver's sessions are closed, and an earlier token is refused.
    portal.local.with(|state| {
        use ic_log::StateMachine;
        let output = state.apply(
            10_001,
            ic_channels::Envelope {
                key: "revoke-hr-1".into(),
                at: now,
                actor: module("identity"),
                command: Command::Identity(IdentityCommand::RevokeSessions { party: p("hr-1") }),
            },
        );
        assert!(output.result.is_ok());
    });
    let old = [("x-dev-subject", "sub-hr".to_string()), ("x-dev-issued", (now - 5).to_string())];
    let (status, problem) = portal.send("GET", "/employer/v1/schemes", &old, None).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::UNAUTHORIZED, Some("session_revoked")));
}

/// Purpose: staff who open a policy whose owner the customer master says to serve with care see the adjustments in effect and when
/// they are reviewed, under no label; only a specialist whose role needs it sees why; nothing is shown for anyone else; and when the
/// master cannot answer, staff are told it is unknown.
/// Why it matters: an officer who cannot see that a customer needs plain language or more time will not give it. Showing every
/// officer why (bereavement, illness, financial abuse) would spread the most private fact held about the person to people who
/// do not need it.
#[tokio::test]
async fn staff_who_open_a_policy_see_how_to_help_and_only_a_specialist_sees_why() {
    let portal = portal();
    let open = |who: &'static str| {
        let path = format!("/staff/v1/policies/{}?case=case-v", portal.retail);
        let portal = &portal;
        async move { portal.get(who, &path).await }
    };
    let (status, view) = open("sub-sue").await;
    assert_eq!(status, StatusCode::OK);
    assert!(view.get("approach").is_none() && view.get("may_see_kind").is_none(), "{view}");

    portal.master.vulnerable.lock().unwrap().push(KIRI.to_string());
    let officer = open("sub-sue").await.1;
    assert_eq!(
        officer["approach"],
        json!({ "adjustments": ["plain_language", "more_time"], "review_on": null })
    );
    let specialist = open("sub-hcs").await.1;
    assert_eq!(specialist["approach"]["kind"], "bereavement");
    assert_eq!(specialist["approach"]["adjustments"], json!(["plain_language", "more_time"]));

    portal.master.down.store(true, Ordering::SeqCst);
    assert_eq!(open("sub-sue").await.1["approach"], json!({ "unavailable": true }));
}

/// Purpose: a person trained in vulnerability support sees the markers that have reached their review date and the prompts to
/// consider one, each with how the person is served now; renews, changes or removes a marker, or closes a prompt with none needed;
/// the master's refusal of a kind outside its list is passed on and leaves the prompt open; and anyone else is told nothing.
/// Why it matters: a marker nobody reviews either lapses while the person still needs care or stays for years after they stopped
/// needing it. The review list names people who are vulnerable, so showing it to every officer would spread exactly what the
/// marker's kind is kept from them for.
#[tokio::test]
async fn a_trained_person_reviews_vulnerability_markers() {
    use ic_channels::customers::Care;
    let portal = portal();
    let master = &portal.master;
    *master.reviews.lock().unwrap() = vec![
        ("rv-1".to_string(), KIRI.to_string(), "review_due".to_string()),
        ("rv-2".to_string(), "p-new".to_string(), "death_of_partner".to_string()),
        ("rv-3".to_string(), "p-dup".to_string(), "duplicate".to_string()),
    ];
    master.markers.lock().unwrap().insert(
        KIRI.to_string(),
        json!({ "kind": "bereavement", "review_on": "2027-11-01", "adjustments": ["more_time"] }),
    );

    let (status, problem) = portal.get("sub-sue", "/staff/v1/care/reviews").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{problem}");
    assert!(master.care.lock().unwrap().is_empty(), "the master was asked for someone untrained");

    let (status, list) = portal.get("sub-hcs", "/staff/v1/care/reviews").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let rows = list["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "a duplicate to merge is not a marker review: {list}");
    assert_eq!(
        rows[0]["marker"],
        json!({ "kind": "bereavement", "review_on": "2027-11-01", "adjustments": ["more_time"] })
    );
    assert_eq!(
        (rows[1]["party"].as_str(), rows[1]["reason"].as_str(), rows[1]["marker"].is_null()),
        (Some("p-new"), Some("death_of_partner"), true)
    );

    let decide = |review: &'static str, body: Value| {
        let portal = &portal;
        async move { portal.write("POST", "sub-hcs", &format!("/staff/v1/care/reviews/{review}"), body).await }
    };
    // Renewed to a later date: the marker moves and the prompt closes.
    let renew = json!({ "party": KIRI, "outcome": "renew", "kind": "bereavement", "review_on": "2028-05-01", "adjustments": ["more_time", "plain_language"] });
    let (status, done) = decide("rv-1", renew).await;
    assert_eq!((status, done["outcome"].as_str()), (StatusCode::OK, Some("renew")), "{done}");
    assert_eq!(master.markers.lock().unwrap()[KIRI]["review_on"], "2028-05-01");
    let sent = master.care.lock().unwrap().iter().rev().find_map(|(who, c)| match c {
        Care::Marker { body, .. } => Some((who.clone(), body.clone())),
        _ => None,
    });
    let (who, body) = sent.unwrap();
    assert_eq!(who, "hcs-1", "the master records the trained person, not the portal");
    assert_eq!(body["reason"], "renew at review rv-1", "the reason says nothing about the person");

    // What is sent must be a whole marker, and an outcome the portal knows.
    for bad in [
        json!({ "party": "p-new", "outcome": "change", "review_on": "2028-01-01", "adjustments": [] }),
        json!({ "party": "p-new", "outcome": "change", "kind": "illness", "review_on": "soon", "adjustments": [] }),
        json!({ "party": "p-new", "outcome": "change", "kind": "illness", "review_on": "2028-01-01", "adjustments": "more_time" }),
        json!({ "party": "p-new", "outcome": "forget" }),
        json!({ "outcome": "remove" }),
    ] {
        assert_eq!(decide("rv-2", bad.clone()).await.0, StatusCode::BAD_REQUEST, "{bad}");
    }
    // The master's own refusal is passed on, and the prompt stays open to decide again.
    let (status, problem) = decide(
        "rv-2",
        json!({ "party": "p-new", "outcome": "change", "kind": "diagnosis", "review_on": "2028-01-01", "adjustments": [] }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    assert_eq!(master.reviews.lock().unwrap().iter().filter(|r| r.0 == "rv-2").count(), 1);

    // A prompt looked at with no marker needed changes nothing on the person.
    assert_eq!(decide("rv-2", json!({ "party": "p-new", "outcome": "no_marker" })).await.0, StatusCode::OK);
    assert!(!master.markers.lock().unwrap().contains_key("p-new"));
    assert_eq!(
        decide("rv-2", json!({ "party": "p-new", "outcome": "no_marker" })).await.0,
        StatusCode::NOT_FOUND,
        "a closed prompt is gone"
    );

    // Removed: the person is served as anyone else from then on.
    master
        .reviews
        .lock()
        .unwrap()
        .push(("rv-4".to_string(), KIRI.to_string(), "review_due".to_string()));
    assert_eq!(decide("rv-4", json!({ "party": KIRI, "outcome": "remove" })).await.0, StatusCode::OK);
    assert_eq!(master.markers.lock().unwrap()[KIRI], json!(false));
    let left = portal.get("sub-hcs", "/staff/v1/care/reviews").await.1;
    assert_eq!(left["rows"], json!([]));
    assert_eq!((left["kinds"].as_array().unwrap().len(), left["adjustments"].as_array().unwrap().len()), (7, 9));

    assert_eq!(
        portal
            .write("POST", "sub-sue", "/staff/v1/care/reviews/rv-9", json!({ "party": KIRI, "outcome": "remove" }))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    master.down.store(true, Ordering::SeqCst);
    assert_eq!(portal.get("sub-hcs", "/staff/v1/care/reviews").await.0, StatusCode::SERVICE_UNAVAILABLE);
}

/// Purpose: when someone phones about a policy, the member of staff is told to speak to an owner or to someone holding an authority
/// from the owner, to refer the call where the master says a person must decide, and otherwise to give one set of words that confirm
/// nothing, the same for a stranger, a relative with no authority, a caller nobody identified and a master that cannot answer; each
/// contact is in the access log with who called and the outcome; and staff who cannot open the policy cannot check a caller on it.
/// Why it matters: a son who phones for his mother's policy details must learn nothing, not even that she has a policy, unless she
/// gave him authority. Words that differed between "no such policy" and "not allowed" would tell him, and a contact left unrecorded
/// could not be shown to her or to the regulator later.
#[tokio::test]
async fn a_caller_with_no_authority_is_told_nothing_and_the_contact_is_recorded() {
    let portal = portal();
    let policy = portal.retail.to_string();
    let path = format!("/staff/v1/policies/{policy}/caller");
    let check = |caller: Value| {
        let (portal, path) = (&portal, path.clone());
        async move {
            portal.clock.fetch_add(1, Ordering::SeqCst);
            portal.write("POST", "sub-sue", &path, json!({ "caller": caller, "case": "case-call" })).await
        }
    };
    let refused = check(json!("party-peter")).await.1;
    assert_eq!(
        (refused["outcome"].as_str(), refused["checked"].as_bool()),
        (Some("refuse"), Some(true)),
        "{refused}"
    );
    let words = refused["say"].as_str().unwrap().to_string();
    assert!(words.contains("can't confirm whether we hold a policy"), "{words}");
    assert_eq!(
        portal.master.asked_for.lock().unwrap()[0],
        ("sue".to_string(), KIRI.to_string(), "party-peter".to_string())
    );

    // A caller nobody identified hears the same words, and the master is not asked about no one.
    for nobody in [Value::Null, json!("  ")] {
        let unidentified = check(nobody).await.1;
        assert_eq!(
            (unidentified["outcome"].as_str(), unidentified["say"].as_str()),
            (Some("refuse"), Some(words.as_str()))
        );
    }
    assert_eq!(portal.master.asked_for.lock().unwrap().len(), 1);

    // The owner, and someone the owner gave an authority to.
    let owner = check(json!(KIRI)).await.1;
    assert_eq!(owner["outcome"], "discuss");
    assert!(owner["say"].as_str().unwrap().contains("policyholder"));
    portal
        .master
        .authorities
        .lock()
        .unwrap()
        .insert((KIRI.to_string(), "party-attorney".to_string()), "allow");
    let attorney = check(json!("party-attorney")).await.1;
    assert_eq!(attorney["outcome"], "discuss");
    assert!(attorney["say"].as_str().unwrap().contains("authority"));
    // While capacity is in question the master sends it to a person, and so does the portal.
    portal
        .master
        .authorities
        .lock()
        .unwrap()
        .insert((KIRI.to_string(), "party-attorney".to_string()), "route");
    let referred = check(json!("party-attorney")).await.1;
    assert_eq!(referred["outcome"], "refer");
    assert!(referred["say"].as_str().unwrap().contains("legal team"));

    // A master that cannot answer refuses in the same words, and staff are told it was not checked.
    portal.master.down.store(true, Ordering::SeqCst);
    let unknown = check(json!("party-attorney")).await.1;
    assert_eq!(
        (unknown["outcome"].as_str(), unknown["say"].as_str(), unknown["checked"].as_bool()),
        (Some("refuse"), Some(words.as_str()), Some(false))
    );
    portal.master.down.store(false, Ordering::SeqCst);

    // Every contact is in the log beside the read of the policy, with who called and what they were told.
    let me = portal.get("sub-sue", "/staff/v1/me").await.1;
    let contacts: Vec<&str> = me["recent"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|l| l["action"] == "contact.caller_check")
        .filter_map(|l| l["record"].as_str())
        .collect();
    assert_eq!(contacts.len(), 7, "{me}");
    assert!(
        contacts.contains(&format!("policy:{policy}/caller/party-peter/refuse").as_str()),
        "{contacts:?}"
    );
    assert!(contacts.contains(&format!("policy:{policy}/caller/unidentified/refuse").as_str()));
    assert!(contacts.contains(&format!("policy:{policy}/caller/party-attorney/refer").as_str()));
    assert!(me["recent"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l["action"] == "policy.view" && l["purpose"]["reference"] == "case-call"));

    // No reason to open the policy, no check; a reference that is not one is refused; and a policy that is not there is 404.
    portal.clock.fetch_add(1, Ordering::SeqCst);
    assert_eq!(
        portal.write("POST", "sub-sue", &path, json!({ "caller": "party-peter" })).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(check(json!("../admin")).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(
        portal
            .write("POST", "sub-sue", "/staff/v1/policies/nope/caller", json!({ "case": "case-call" }))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

/// Purpose: an attorney whose enduring power has been activated signs in as themselves and sees the policies of the person they act
/// for, without that person's health information; the person keeps their own sign-in and still sees everything of their own; with
/// no authority, or for a person who does not exist, the answer is the same 404; while the authority is under review nothing is
/// shown; and an outage of the customer master shows nothing rather than everything.
/// Why it matters: an attorney handed the customer's own sign-in would be indistinguishable from the customer in every record, and
/// the customer would lose the sight of their own affairs that the law leaves them. A relative with no authority must not be able to
/// tell from the answer whether the person is a customer at all.
#[tokio::test]
async fn an_attorney_sees_the_persons_policies_under_their_own_sign_in() {
    let portal = portal();
    let policy = portal.retail.to_string();
    let list = format!("/app/v1/represented/{KIRI}");
    let one = format!("/app/v1/represented/{KIRI}/policies/{policy}");
    // The adviser's sign-in stands in for the attorney: a person with a party of their own and no relationship to the policy.
    assert_eq!(portal.get("sub-adv", &list).await.0, StatusCode::NOT_FOUND);
    assert_eq!(portal.get("sub-adv", &one).await.0, StatusCode::NOT_FOUND);
    assert_eq!(portal.get("sub-adv", "/app/v1/represented/nobody").await.0, StatusCode::NOT_FOUND);

    // Underwriting holds a loading on the person's cover: theirs to see, and nobody else's.
    let (retail, now) = (portal.retail.clone(), portal.now());
    portal.local.with(|state| {
        use ic_log::StateMachine;
        let rows = json!({ "publish_rows": { "rows": [{ "key": format!("{retail}/underwriting/loading"), "policy": retail, "kind": "loading",
            "subject": KIRI, "sensitive": true, "body": { "code": "L150" } }], "remove": [] } });
        let command: Command = serde_json::from_value(rows).unwrap();
        let output = state.apply(
            9_500,
            ic_channels::Envelope {
                key: "attorney-loading".into(),
                at: now,
                actor: module("underwriting"),
                command,
            },
        );
        assert!(output.result.is_ok(), "{:?}", output.result);
    });
    portal
        .master
        .authorities
        .lock()
        .unwrap()
        .insert((KIRI.to_string(), "adv-1".to_string()), "allow");
    let (status, mine) = portal.get("sub-adv", &list).await;
    assert_eq!(status, StatusCode::OK, "{mine}");
    assert_eq!(
        (mine["party"].as_str(), mine["authority"]["basis"].as_str()),
        (Some(KIRI), Some("enduring_power_of_attorney"))
    );
    assert!(mine["policies"].as_array().unwrap().iter().any(|p| p["policy"] == policy.as_str()), "{mine}");
    let (status, view) = portal.get("sub-adv", &one).await;
    assert_eq!((status, view["on_behalf_of"].as_str()), (StatusCode::OK, Some(KIRI)), "{view}");
    // The person's own view holds more: what is sensitive stays theirs.
    let own = portal.get("sub-kiri", &format!("/app/v1/policies/{policy}")).await.1;
    let (shown, theirs) = (view["rows"].as_array().unwrap().len(), own["rows"].as_array().unwrap().len());
    assert_eq!(shown + view["withheld"].as_u64().unwrap() as usize, theirs, "{view}");
    assert_eq!(view["withheld"], 1, "{view}");
    assert!(own["rows"].as_array().unwrap().iter().any(|r| r["kind"] == "loading"), "{own}");
    assert!(view["rows"].as_array().unwrap().iter().all(|r| r["kind"] != "health" && r["kind"] != "loading"));
    // The authority is over one person: nothing of the attorney's reach extends to a policy that person is not on.
    assert_eq!(
        portal.get("sub-adv", &format!("/app/v1/represented/{KIRI}/policies/{}", portal.scheme)).await.0,
        StatusCode::NOT_FOUND
    );
    // The attorney's own app is unchanged: the person's policy is not among their own.
    assert_eq!(portal.get("sub-adv", &format!("/app/v1/policies/{policy}")).await.0, StatusCode::NOT_FOUND);

    portal
        .master
        .authorities
        .lock()
        .unwrap()
        .insert((KIRI.to_string(), "adv-1".to_string()), "route");
    let (status, problem) = portal.get("sub-adv", &one).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::FORBIDDEN, Some("authority_under_review")));
    portal.master.down.store(true, Ordering::SeqCst);
    assert_eq!(portal.get("sub-adv", &list).await.0, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        portal.get("sub-kiri", &format!("/app/v1/policies/{policy}")).await.0,
        StatusCode::OK,
        "the person lost their own view"
    );
}

/// Purpose: the finance lead prices a proposed rate table against the book from the two reviewed table files; the core's loadings
/// for the date are gathered from every partition, page by page, and handed to the ledger, and the answer carries totals with no
/// policy's loading in it; a file that is not a rate table, a missing date or no cover code is refused before anything is read;
/// people without the role are refused; and when the core cannot be read the run still answers and says the loaded totals are missing.
/// Why it matters: a new table is approved on what it does to the premiums people actually pay, and those include loadings.
/// Loadings imply health, so they may pass through for this one sum but must never come back out by policy, and an actuary who
/// was shown base totals as if they were loaded would sign off the wrong movement.
#[tokio::test]
async fn a_proposed_rate_table_is_priced_against_the_book() {
    let portal = portal();
    let table = include_str!("../../../products/rates/life_base.star");
    let ask = |who: &'static str, body: Value| {
        let portal = &portal;
        async move { portal.write("POST", who, "/staff/v1/finance/table_dry_run", body).await }
    };
    let run = json!({ "as_at": "2027-10-31", "covers": ["life"], "current": table, "proposed": table });
    for outsider in ["sub-sue", "sub-aud", "sub-gl1"] {
        assert_eq!(ask(outsider, run.clone()).await.0, StatusCode::FORBIDDEN, "{outsider}");
    }
    for (bad, said) in [
        (json!({ "covers": ["life"], "current": table, "proposed": table }), "date"),
        (
            json!({ "as_at": "2027-10-31", "covers": [" "], "current": table, "proposed": table }),
            "cover code",
        ),
        (json!({ "as_at": "2027-10-31", "covers": ["life"], "current": table }), "proposed"),
        (
            json!({ "as_at": "2027-10-31", "covers": ["life"], "current": table, "proposed": "RATE_TABLE = 7" }),
            "does not load",
        ),
    ] {
        let (status, problem) = ask("sub-gllead", bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{problem}");
        assert!(problem["detail"].as_str().unwrap().contains(said), "{problem}");
    }

    let (status, answer) = ask("sub-gllead", run.clone()).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(answer["loadings_read"], true);
    assert_eq!(
        (answer["as_at"].as_str(), answer["passed"].as_bool()),
        (Some("2027-10-31"), Some(true)),
        "{answer}"
    );
    assert!(answer["loaded_current_annual_minor"].is_number() && answer["cells"].is_array());
    // What came back names no policy the loadings were on.
    let text = answer.to_string();
    assert!(!text.contains("pol-a") && !text.contains("pol-z"), "{text}");
    // The stand-in core was read to the end on both partitions: three loaded layers went to the ledger and none stayed here.
    let rows = vec![json!({ "partition": 0, "policy": "pol-a", "cover": "life", "layer": 1, "loadings": ["0.5"] })];
    let question = ic_channels::finance::dry_run(&run, rows).unwrap();
    assert_eq!(
        (question["q"].as_str(), question["request"]["current"]["id"].as_str()),
        (Some("table_dry_run"), Some("life-base"))
    );
    assert_eq!(question["request"]["loadings"][0]["policy"], "pol-a");
    assert_eq!(
        (question["request"]["per_life"].as_bool(), question["request"]["age_at_start"].as_bool()),
        (Some(false), Some(false))
    );
}

/// Purpose: when no core partition answers, the dry run is still priced on base premiums and the answer says the loadings were
/// not read.
/// Why it matters: the actuary must be able to see a table's movement during a core outage, and must not mistake that answer for
/// one that includes loadings.
#[tokio::test]
async fn a_dry_run_without_the_core_says_the_loadings_are_missing() {
    let portal = portal();
    let table = include_str!("../../../products/rates/life_base.star");
    // The stand-in core has no leader for this date, part way through the book.
    let run = json!({ "as_at": "1999-01-01", "covers": ["life"], "current": table, "proposed": table });
    let (status, answer) = portal.write("POST", "sub-gllead", "/staff/v1/finance/table_dry_run", run).await;
    assert_eq!((status, answer["loadings_read"].as_bool()), (StatusCode::OK, Some(false)), "{answer}");
    assert_eq!(answer["loaded_covers"], 0);
}

/// Purpose: a request from a person the customer master routes to a person first is marked so on the request the owning module
/// picks up, with nothing about why; anyone else's is not; and when the master cannot answer, the request is marked too.
/// Why it matters: a vulnerable customer's request must reach someone trained before anything happens, and an outage of the master
/// must not quietly send such a request down the automated path.
#[tokio::test]
async fn a_vulnerable_customers_request_goes_to_a_person() {
    let portal = portal();
    portal.master.vulnerable.lock().unwrap().push(KIRI.to_string());
    let ask =
        |id: &str| json!({ "id": id, "policy": portal.retail, "kind": "decline_indexation", "module": "policy-servicing", "payload": { "declined": true } });
    assert_eq!(portal.write("POST", "sub-kiri", "/app/v1/requests", ask("r-v1")).await.0, StatusCode::ACCEPTED);
    let mine = portal.get("sub-kiri", "/app/v1/requests").await.1;
    assert_eq!(mine[0]["route_to_person"], true);
    assert!(mine[0].get("kind_of_marker").is_none() && mine[0].get("adjustments").is_none());
    assert!(portal.master.calls.lock().unwrap().iter().any(|c| c.what == "routing" && c.party == KIRI));

    let other = json!({ "id": "r-h1", "policy": portal.scheme, "kind": "claim_evidence", "module": "claims" });
    assert_eq!(portal.write("POST", "sub-hr", "/app/v1/requests", other).await.0, StatusCode::ACCEPTED);
    assert!(portal.get("sub-hr", "/app/v1/requests").await.1[0].get("route_to_person").is_none());

    portal.master.down.store(true, Ordering::SeqCst);
    portal.master.vulnerable.lock().unwrap().clear();
    assert_eq!(portal.write("POST", "sub-kiri", "/app/v1/requests", ask("r-v2")).await.0, StatusCode::ACCEPTED);
    let mine = portal.get("sub-kiri", "/app/v1/requests").await.1;
    assert!(mine.as_array().unwrap().iter().all(|r| r["route_to_person"] == true), "{mine}");
}

/// Purpose: a disputed match the commission module accepts a decision on is decided from the page; a reinsurance question about
/// something that does not exist is 404, and the ledger's own refusal for a journal it does not hold is passed on as it is.
/// Why it matters: these are the answers a person acts on, done or nothing there; a console that called a missing statement an error,
/// or hid the ledger's own words, would send people to the wrong team.
#[tokio::test]
async fn consoles_report_done_and_missing() {
    let portal = portal();
    portal.commission.matches(json!([]));
    let body = json!({ "policy": "pol-9", "cover": "life", "layer": 1, "replacement": false, "purpose": "dispute D-1 upheld" });
    let (status, done) = portal.write("POST", "sub-wbcon", "/staff/v1/replacements/decide", body).await;
    assert_eq!((status, &done["accepted"]), (StatusCode::OK, &json!(true)), "{done}");

    let ri = portal.get("sub-riview", "/staff/v1/reinsurance/statement?number=999").await;
    assert_eq!(ri.0, StatusCode::NOT_FOUND, "{ri:?}");
    let gl = portal.get("sub-aud", "/staff/v1/finance/journal?id=nope").await;
    assert_eq!((gl.0, gl.1["detail"].as_str()), (StatusCode::BAD_REQUEST, Some("there is no journal nope")));
}

/// Purpose: an employer administrator uploads the scheme's membership file, reads its preview, approves it, declares a pool, reads the
/// scheme's standing and invoices, and opens one member's record, all on the scheme the group service holds for their policy and as
/// themselves; the record carries the member's category, salary and cover and never a loading, a reason, an exclusion or the person;
/// anyone who does not administer the policy is told it is not there; staff's commands are not passed on; and a group service that is
/// down or not configured is a 503.
/// Why it matters: persona row GRP-032. The employer runs the scheme's membership but must never learn an employee's health; and a file
/// sent to another employer's scheme, or approved under someone else's name, would change people's cover without their employer.
#[tokio::test]
async fn an_employer_runs_the_scheme_membership() {
    let portal = &portal();
    let at = |rest: &str| format!("/employer/v1/schemes/{}/group/{rest}", portal.scheme);
    let hr = |rest: &str| {
        let path = at(rest);
        async move { portal.get("sub-hr", &path).await }
    };
    let upload = |who: &str, file: &str, body: Option<Value>| {
        let path = at(&format!("files/{file}"));
        let who = who.to_string();
        async move { portal.send("POST", &path, &[("x-dev-subject", who)], body).await }
    };

    // The file goes to the scheme on this policy, under the administrator's name.
    let csv = json!("employee_id,salary\nE001,80000\n");
    let (status, stored) = upload("sub-hr", "nov.csv", Some(csv.clone())).await;
    assert_eq!(
        (status, &stored["status"], &stored["scheme"]),
        (StatusCode::ACCEPTED, &json!("quarantined"), &json!("harbour"))
    );
    assert_eq!(portal.group.files.lock().unwrap()[&("harbour".to_string(), "nov.csv".to_string())].0, "hr-1");
    assert_eq!(upload("sub-hr", "nov.csv", None).await.0, StatusCode::BAD_REQUEST, "an empty file");
    assert_eq!(upload("sub-hr", "..", Some(csv.clone())).await.0, StatusCode::BAD_REQUEST);
    // Someone who does not administer the scheme learns nothing, not even that it is there.
    assert_eq!(upload("sub-kiri", "nov.csv", Some(csv.clone())).await.0, StatusCode::NOT_FOUND);
    assert_eq!(portal.get("sub-kiri", &at("scheme")).await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        portal.get("sub-hr", "/employer/v1/schemes/pol-nowhere/group/scheme").await.0,
        StatusCode::NOT_FOUND
    );

    // The preview, the standing and the invoices.
    let (status, file) = hr("file?file=nov.csv").await;
    assert_eq!((status, &file["status"]), (StatusCode::OK, &json!("quarantined")), "{file}");
    assert_eq!(hr("file?file=dec.csv").await.0, StatusCode::NOT_FOUND);
    assert_eq!(hr("file").await.0, StatusCode::BAD_REQUEST, "which file");
    assert_eq!(hr("file?file=a%2Fb").await.0, StatusCode::BAD_REQUEST);
    assert_eq!(hr("scheme").await.1["members"], 2);
    assert_eq!(hr("invoices").await.1["invoices"][0]["invoice"], "inv-1");
    assert_eq!(hr("exceptions").await.0, StatusCode::BAD_REQUEST, "open exceptions are staff's");

    // A member's record: category, salary and cover, never health or who they are.
    let (status, member) = hr("member?employee=E001").await;
    assert_eq!(
        (status, &member["category"], &member["in_force"]),
        (StatusCode::OK, &json!("staff"), &json!(true)),
        "{member}"
    );
    assert_eq!(member["covers"][0]["automatic_minor"], 200_000_00);
    assert_eq!(member["above_automatic"][0]["state"], "in_place");
    let text = member.to_string();
    for never in ["loading", "party", "exclusion", "reason"] {
        assert!(!text.contains(never), "{never} reached the employer: {text}");
    }
    assert_eq!(hr("member?employee=E999").await.0, StatusCode::NOT_FOUND);

    // The administrator approves the file and declares a pool, as themselves.
    let approve = json!({ "approve_file": { "file": "nov.csv", "scheme": "someone-elses" } });
    let (status, done) = portal.write("POST", "sub-hr", &at("commands"), approve).await;
    assert_eq!((status, &done["applied"]), (StatusCode::OK, &json!(true)), "{done}");
    let (key, actor, command) = portal.group.commands.lock().unwrap()[0].clone();
    assert!(key.starts_with("api:employer-portal:hr-1:"), "{key}");
    assert_eq!(
        (actor.as_str(), &command["approve_file"]["scheme"]),
        ("hr-1", &json!("harbour")),
        "the scheme is the policy's"
    );
    assert_eq!(
        portal.group.files.lock().unwrap()[&("harbour".to_string(), "nov.csv".to_string())].2,
        "approved"
    );
    let pool = json!({ "declare_pool": { "pool": "seed", "category": "staff", "effective": "2027-12-01", "headcount": 10, "total_salary_minor": 1 } });
    assert_eq!(portal.write("POST", "sub-hr", &at("commands"), pool).await.0, StatusCode::OK);
    let plan = json!({ "choose_health_plan": { "employee_id": "E001", "plan": "core", "tier": "single", "effective": "2027-12-01" } });
    assert_ne!(
        portal.write("POST", "sub-hr", &at("commands"), plan).await.0,
        StatusCode::BAD_REQUEST,
        "an employer records a member's plan"
    );
    // The group service's refusal is passed on as it gave it.
    let missing = json!({ "withdraw_file": { "file": "dec.csv" } });
    assert_eq!(
        portal.write("POST", "sub-hr", &at("commands"), missing).await,
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({ "code": "unknown_file", "title": "unknown_file", "detail": "the group service refused this without saying why (422)" })
        )
    );
    // Staff's commands, and anything that is not one command, are not sent at all.
    for bad in [
        json!({ "freeze": { "scheme": "harbour" } }),
        json!({ "a": {}, "b": {} }),
        json!({ "approve_file": "nov.csv" }),
    ] {
        assert_eq!(portal.write("POST", "sub-hr", &at("commands"), bad).await.0, StatusCode::BAD_REQUEST);
    }
    assert_eq!(
        portal
            .write("POST", "sub-kiri", &at("commands"), json!({ "withdraw_file": { "file": "nov.csv" } }))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(portal.group.commands.lock().unwrap().len(), 2);

    // A group service that cannot be reached.
    portal.group.down.store(true, Ordering::SeqCst);
    assert_eq!(hr("scheme").await.0, StatusCode::SERVICE_UNAVAILABLE);
    portal.group.down.store(false, Ordering::SeqCst);
    // A policy with no scheme on the group service is not found; a service that fails is a bad gateway.
    portal.group.unmapped.store(true, Ordering::SeqCst);
    assert_eq!(hr("scheme").await.0, StatusCode::NOT_FOUND);
    portal.group.unmapped.store(false, Ordering::SeqCst);
    portal.group.broken.store(true, Ordering::SeqCst);
    assert_eq!(hr("scheme").await.0, StatusCode::BAD_GATEWAY);
    portal.group.broken.store(false, Ordering::SeqCst);

    // A portal deployed without one says so.
    let now = portal.now();
    let bare = router(Arc::new(AppState {
        wordings: Arc::default(),
        public_url: None,
        returns: Arc::default(),
        channel: portal.local.clone(),
        customers: Auth::Dev,
        workforce: Auth::Dev,
        clock: Arc::new(move || now),
        regions: portal.regions.clone(),
        ui: UiConfig::Dev,
        pricing: portal.pricing.clone(),
        complaints: Arc::new(common::pricing::Down),
        commission: Arc::new(common::commission::seeded()),
        finance: Arc::new(common::pricing::Down),
        claims: portal.claims.clone(),
        reinsurance: portal.reinsurance.clone(),
        payments: portal.payments.clone(),
        collections: portal.collections.clone(),
        correspondence: portal.correspondence.clone(),
        wellness: portal.wellness.clone(),
        underwriting: portal.underwriting.clone(),
        master: None,
        intake: None,
        documents: None,
        group: None,
    }));
    let request = Request::builder()
        .uri(at("scheme"))
        .header("x-dev-subject", "sub-hr")
        .body(Body::empty())
        .unwrap();
    assert_eq!(bare.oneshot(request).await.unwrap().status(), StatusCode::SERVICE_UNAVAILABLE);
}

/// Purpose: an administrator sees who administers the scheme and their own reference, names only a colleague who has
/// signed in, and removes a member or changes a salary by hand as a change file the group service checks; a kind of change
/// the employer asked a second administrator for waits for a colleague, and the person who entered it cannot approve it.
/// Why it matters: found by the scenario run, SCN-05 (F-SCN05-1, F-SCN05-5): a hand change was stored as a request nothing
/// acted on, the second-administrator rule was never applied to files, a wrong employee ID was accepted, and an administrator
/// had to type a colleague's internal reference, any text being taken.
#[tokio::test]
async fn an_administrator_changes_a_member_by_hand() {
    use common::group_module::harness::{attrs, row, terms, Harness};
    use ic_group::mapping::FileKind;
    let mut h = Harness::new(terms());
    h.apply(
        "f0",
        FileKind::Full,
        vec![
            row(1, "E001", attrs("staff", 60_000_00, "akl")),
            row(2, "E002", attrs("staff", 50_000_00, "akl")),
        ],
    );
    let today = h.today().to_string();
    let module = Arc::new(common::group_module::GroupModule(std::sync::Mutex::new(h)));
    let inner = module.clone();
    let portal = &portal_with(move |_| Arc::new(common::group_module::ForEmployer(inner)));
    let scheme = &portal.scheme;
    let at = |rest: &str| format!("/employer/v1/schemes/{scheme}/{rest}");
    let change = |who: &'static str, sent: Value| {
        let path = at("group/changes");
        async move { portal.write("POST", who, &path, sent).await }
    };

    // Who administers the scheme, and the reference to give a colleague.
    let (status, admins) = portal.get("sub-hr", &at("admins")).await;
    assert_eq!(
        (status, &admins["you"], &admins["second_admin"]),
        (StatusCode::OK, &json!("hr-1"), &json!([])),
        "{admins}"
    );
    assert!(admins["admins"].as_array().unwrap().iter().any(|a| a == "hr-1"), "{admins}");
    assert_eq!(portal.get("sub-kiri", &at("admins")).await.0, StatusCode::NOT_FOUND);
    // Only someone who has signed in can be named.
    let (status, refused) = portal.write("PUT", "sub-hr", &at("admins/hr-9"), json!({ "admin": true })).await;
    assert_ne!(status, StatusCode::OK);
    assert_eq!(refused["title"], "unknown_person", "{refused}");
    assert_eq!(
        portal.write("PUT", "sub-hr", &at("admins/hr-2"), json!({ "admin": true })).await.0,
        StatusCode::OK
    );
    let set = json!({ "changes": ["remove_member"] });
    assert_eq!(portal.write("PUT", "sub-hr", &at("second-admin"), set).await.0, StatusCode::OK);

    // What cannot be a hand change is refused before anything is sent.
    for bad in [
        json!({ "kind": "add_member", "employee_id": "E003", "effective": today }),
        json!({ "kind": "change_salary", "employee_id": "E001", "effective": today }),
        json!({ "kind": "remove_member", "employee_id": "E001", "effective": "soon" }),
        json!({ "kind": "remove_member", "employee_id": "E/1", "effective": today }),
    ] {
        assert_eq!(change("sub-hr", bad).await.0, StatusCode::BAD_REQUEST);
    }
    // A wrong employee ID is the group service's refusal, naming the member.
    let (status, refused) = change("sub-hr", json!({ "kind": "remove_member", "employee_id": "E0O2", "effective": today })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(refused.to_string().contains("E0O2 is not a member"), "{refused}");

    // A removal waits for a colleague: the person who entered it cannot approve it.
    let (status, sent) = change("sub-hr", json!({ "kind": "remove_member", "employee_id": "E002", "effective": today })).await;
    assert_eq!((status, &sent["status"]), (StatusCode::OK, &json!("awaiting_approval")), "{sent}");
    assert!(sent["why"].as_str().unwrap().contains("removing a member"), "{sent}");
    let file = sent["file"].as_str().unwrap().to_string();
    let approve = json!({ "approve_file": { "file": file } });
    let (status, refused) = portal.write("POST", "sub-hr", &at("group/commands"), approve.clone()).await;
    assert_eq!((status, &refused["title"]), (StatusCode::CONFLICT, &json!("second_administrator")), "{refused}");
    assert_eq!(portal.write("POST", "sub-hr2", &at("group/commands"), approve).await.0, StatusCode::OK);
    let (_, member) = portal.get("sub-hr", &at("group/member?employee=E002")).await;
    assert_ne!(member["in_force"], true, "{member}");
    // An unknown file is the group service's to answer.
    let nope = json!({ "approve_file": { "file": "nope" } });
    assert_eq!(
        portal.write("POST", "sub-hr", &at("group/commands"), nope).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );

    // A salary change the employer did not ask a second administrator for is approved at once.
    let salary = |n: i64| json!({ "kind": "change_salary", "employee_id": "E001", "effective": today, "salary_minor": n });
    let (status, sent) = change("sub-hr", salary(70_000_00)).await;
    assert_eq!((status, &sent["status"]), (StatusCode::OK, &json!("approved")), "{sent}");

    // Where the scheme's own terms want a second approver for every file, it waits and says why.
    module.0.lock().unwrap().g.schemes.get_mut("harbour").unwrap().terms.second_approver = true;
    let (status, sent) = change("sub-hr", salary(75_000_00)).await;
    assert_eq!((status, &sent["status"]), (StatusCode::OK, &json!("awaiting_approval")), "{sent}");
    assert!(sent["why"].as_str().unwrap().contains("another administrator"), "{sent}");
    // The employer's own rule does not cover salaries, so the group service is the one to refuse its sender.
    let approve = json!({ "approve_file": { "file": sent["file"] } });
    let (status, refused) = portal.write("POST", "sub-hr", &at("group/commands"), approve).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert!(refused.to_string().contains("another administrator must approve this"), "{refused}");
}

/// Purpose: every action and question the commission page offers is one the console route accepts.
/// Why it matters: the page once offered appointing a policy's adviser, which the route always refuses; a form that can only fail teaches staff to distrust the screen.
#[test]
fn the_commission_page_offers_only_what_its_route_accepts() {
    let page = ic_channels::ui::COMMISSION;
    let offered = |list: &str| -> Vec<String> {
        let from = page.find(&format!("const {list} = {{")).expect("the list is on the page");
        page[from..]
            .lines()
            .skip(1)
            .take_while(|line| !line.starts_with("};"))
            .filter_map(|line| {
                line.strip_prefix("  ")
                    .filter(|rest| !rest.starts_with(' '))
                    .and_then(|rest| rest.split_once(':'))
                    .map(|(name, _)| name.to_string())
            })
            .collect()
    };
    let actions = offered("ACTIONS");
    assert!(actions.len() >= 10, "{actions:?}");
    for action in &actions {
        assert!(
            ic_channels::commission::COMMANDS.contains(&action.as_str()),
            "the page offers {action}, which the route refuses"
        );
    }
    let questions = offered("QUESTIONS");
    assert!(questions.contains(&"position".to_string()), "{questions:?}");
}

/// Purpose: a member of staff whose templates give the commission module's distribution admin role opens the commission console,
/// asks its questions with the fields each needs, and sends a structure command as themselves with a purpose, which the commission
/// module's own policy allows; someone who may only look is refused by that module; and commands that have their own paths
/// (workbench actions, replacement decisions, policy assignment), commands with no purpose and people with no commission action at
/// all go no further than the portal.
/// Why it matters: the commission module trusts the console to describe the person truly, and decides by the roles it is told. A
/// console that let the page name the roles, or carried a workbench action round the two-person rule there, would let one person
/// move money or change who is paid.
#[tokio::test]
async fn the_commission_console() {
    let portal = &portal();
    let ask = |who: &'static str, rest: &str| {
        let path = format!("/staff/v1/commission/{rest}");
        async move { portal.get(who, &path).await }
    };
    let tell = |who: &'static str, purpose: &'static str, command: Value| async move {
        portal
            .write("POST", who, "/staff/v1/commission/commands", json!({ "purpose": purpose, "command": command }))
            .await
    };

    let (status, me) = ask("sub-dadmin", "me").await;
    assert_eq!((status, &me["roles"]), (StatusCode::OK, &json!(["distribution_admin"])), "{me}");
    assert_eq!(ask("sub-dview", "me").await.1["roles"], json!([]));
    assert_eq!(ask("sub-gl1", "me").await.0, StatusCode::FORBIDDEN);

    // Questions, each with its fields.
    let (status, answer) = ask("sub-dview", "status").await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(ask("sub-dview", "statement?payee=adv-1").await.0, StatusCode::BAD_REQUEST, "which statement");
    assert_eq!(ask("sub-dview", "statement?payee=adv-1&number=first").await.0, StatusCode::BAD_REQUEST);
    assert_eq!(ask("sub-dview", "outbox").await.0, StatusCode::BAD_REQUEST, "the driver's, not people's");
    assert_eq!(
        ask("sub-dview", "statement?payee=adv-1&number=1").await.0,
        StatusCode::NOT_FOUND,
        "no statement cut yet"
    );
    let (status, held) = ask("sub-dview", "held_control?date=2027-11-01").await;
    assert_eq!(status, StatusCode::OK, "{held}");
    assert_eq!(ask("sub-dview", "case?id=nope").await.0, StatusCode::NOT_FOUND);

    // The administrator adds an adviser, as themselves.
    let add = json!({ "add_party": { "party": "adv-9", "kind": "adviser" } });
    assert_eq!(tell("sub-dadmin", "", add.clone()).await.0, StatusCode::BAD_REQUEST, "no purpose");
    let (status, done) = tell("sub-dadmin", "onboarding adviser 9", add.clone()).await;
    assert_eq!((status, &done["accepted"]), (StatusCode::OK, &json!(true)), "{done}");
    // Someone who may only look is refused before anything is sent, and told who acts there (F2-18-2).
    let other = json!({ "add_party": { "party": "adv-10", "kind": "adviser" } });
    let (status, problem) = tell("sub-dview", "trying", other).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{problem}");
    assert!(problem["detail"].as_str().unwrap().contains("distribution administrator"), "{problem}");
    // Not the console's to send.
    for command in [
        json!({ "propose_action": {} }),
        json!({ "decide_replacement": {} }),
        json!({ "write_layer": {} }),
        json!({ "a": {}, "b": {} }),
    ] {
        assert_eq!(tell("sub-dadmin", "x", command).await.0, StatusCode::BAD_REQUEST);
    }
    assert_eq!(tell("sub-gl1", "x", add).await.0, StatusCode::FORBIDDEN);
}

/// Purpose: underwriters work cases from the underwriting console: they read queues and proposals, take a case and
/// decide it, and every command reaches the underwriting module naming whoever signed in, whatever the page typed. Peer
/// review and non-disclosure remedies are a senior's; a remedy is prepared by one senior and sent only when a different
/// senior signs it, named as `countersigned_by`. Reading a life's disclosures goes through the intake with a purpose,
/// and parked effects can be put back. A date with no reason or exclusion library in force offers an empty one.
/// Why it matters: that module believes the console about who is acting, so a page that could name someone else would
/// defeat its authority, conflict and separation checks; and a remedy changes or ends someone's cover on a claim, which
/// must never rest on one person.
#[tokio::test]
async fn the_underwriting_console() {
    use ic_underwriting::command::Command as Uw;
    let portal = portal();
    let ask = |who: &'static str, rest: String| {
        let portal = &portal;
        async move { portal.get(who, &format!("/staff/v1/underwriting/{rest}")).await }
    };
    let tell = |who: &'static str, command: Value| {
        let portal = &portal;
        async move { portal.write("POST", who, "/staff/v1/underwriting/commands", command).await }
    };
    let decide = |who: &'static str, verb: &'static str, id: String| {
        let portal = &portal;
        async move { portal.write("POST", who, &format!("/staff/v1/underwriting/{verb}/{id}"), json!({})).await }
    };
    let last = || portal.underwriting.log().last().cloned().unwrap();

    // Who gets in, and as what.
    assert_eq!(ask("sub-gl1", "me".into()).await.0, StatusCode::FORBIDDEN);
    let (status, me) = ask("sub-amy", "me".into()).await;
    assert_eq!((status, &me["staff"], &me["senior"]), (StatusCode::OK, &json!("amy"), &json!(false)), "{me}");
    assert_eq!(ask("sub-sam", "me".into()).await.1["senior"], true);

    // Questions: only the console's, each with its fields.
    assert_eq!(ask("sub-amy", "outbox".into()).await.0, StatusCode::BAD_REQUEST, "the driver's, not people's");
    assert_eq!(ask("sub-amy", "queue".into()).await.0, StatusCode::BAD_REQUEST, "which queue");
    assert_eq!(ask("sub-amy", "proposal?proposal=nope".into()).await.0, StatusCode::NOT_FOUND);
    let (status, proposal) = ask("sub-amy", "proposal?proposal=P2".into()).await;
    assert_eq!((status, &proposal["id"]), (StatusCode::OK, &json!("P2")), "{proposal}");
    let case = portal.underwriting.case("P2", "L1").expect("P2 waits for a person");
    let mut queued = false;
    for queue in ["medical", "financial", "occupational", "tele_interview", "senior_review", "reinsurer_liaison"] {
        let (status, items) = ask("sub-amy", format!("queue?queue={queue}")).await;
        assert_eq!(status, StatusCode::OK, "{items}");
        queued |= items.as_array().unwrap().iter().any(|i| i["case"] == case.as_str());
    }
    assert!(queued, "the case is in a queue");
    assert_eq!(ask("sub-amy", "workload".into()).await.1, json!(0));
    // A date before any library was in force offers nothing to pick, rather than a missing page.
    for library in ["reasons", "exclusions"] {
        assert_eq!(
            ask("sub-amy", format!("{library}?on=1900-01-01")).await,
            (StatusCode::OK, json!({ "entries": {}, "in_force": false })),
            "{library}"
        );
    }

    // The person acting is whoever signed in, whatever the page says.
    let (status, taken) = tell("sub-amy", json!({ "take_case": { "case": case, "underwriter": "sam" } })).await;
    assert_eq!((status, &taken["accepted"]), (StatusCode::OK, &json!(true)), "{taken}");
    assert_eq!((portal.underwriting.held_by("amy"), portal.underwriting.held_by("sam")), (1, 0));
    let sent = last();
    assert_eq!(sent.caller, "underwriting-console");
    assert!(matches!(&sent.command, Uw::TakeCase { underwriter, .. } if underwriter == "amy"));
    assert_eq!(ask("sub-amy", "workload".into()).await.1, json!(1));
    // The module refuses what this person may not do.
    let (status, problem) = tell("sub-abe", json!({ "take_case": { "case": case } })).await;
    assert!(status == StatusCode::CONFLICT || status == StatusCode::FORBIDDEN, "{status} {problem}");
    let standard = json!({ "propose_decision": { "case": case, "terms": [{ "cover": "life", "outcome": { "outcome": "accept" } }],
        "reasons": ["RC-0101"], "withheld_reasons": [], "notes_ref": "vault://notes", "evidence": [] } });
    let (status, decided) = tell("sub-amy", standard).await;
    assert_eq!(
        (status, decided["events"].as_array().map(Vec::is_empty)),
        (StatusCode::OK, Some(false)),
        "{decided}"
    );

    // What the page may not send.
    for bad in [
        json!({ "sweep": { "max": 1 } }),
        json!({ "open_disclosures": {} }),
        json!({ "take_case": "x" }),
        json!("queue"),
    ] {
        assert_eq!(tell("sub-amy", bad.clone()).await.0, StatusCode::BAD_REQUEST, "{bad}");
    }
    // A senior's commands.
    let draw = json!({ "draw_peer_review_sample": { "period": "2026-11", "from": "2026-11-01T00:00:00Z", "to": "2026-12-01T00:00:00Z", "seed": 7 } });
    assert_eq!(tell("sub-amy", draw.clone()).await.0, StatusCode::FORBIDDEN);
    let (status, drawn) = tell("sub-cat", draw).await;
    assert_eq!(status, StatusCode::OK, "{drawn}");

    // A non-disclosure remedy: prepared by one senior, sent when another signs.
    let review = "ndr/CLM-1/L1";
    assert_eq!(tell("sub-sam", json!({ "take_case": { "case": review } })).await.0, StatusCode::OK);
    let remedy = |outcome: &str| {
        json!({ "record_non_disclosure_outcome": { "review": review, "outcome": outcome, "reasons": ["RC-0980"],
            "varied_terms": [{ "cover": "life", "outcome": { "outcome": "accept", "loading_percent": 50 } }] } })
    };
    assert_eq!(tell("sub-amy", remedy("vary")).await.0, StatusCode::FORBIDDEN, "a senior's");
    let mut typed = remedy("vary");
    typed["record_non_disclosure_outcome"]["countersigned_by"] = json!("cat");
    assert_eq!(tell("sub-sam", typed).await.0, StatusCode::BAD_REQUEST, "the second senior is not typed");
    let (status, prepared) = tell("sub-sam", remedy("vary")).await;
    assert_eq!(status, StatusCode::OK, "{prepared}");
    let id = prepared["prepared"].as_str().unwrap().to_string();
    let waiting = ask("sub-cat", "me".into()).await.1["pending"].clone();
    assert_eq!((&waiting[0]["id"], &waiting[0]["by"]), (&json!(id), &json!("sam")), "{waiting}");
    assert!(
        !matches!(last().command, Uw::RecordNonDisclosureOutcome { .. }),
        "nothing reached the module yet"
    );
    assert_eq!(decide("sub-sam", "sign", id.clone()).await.1["title"], "separation");
    assert_eq!(decide("sub-amy", "sign", id.clone()).await.0, StatusCode::FORBIDDEN, "a senior signs");
    assert_eq!(decide("sub-cat", "withdraw", id.clone()).await.0, StatusCode::FORBIDDEN, "only its preparer");
    assert_eq!(decide("sub-cat", "approve", id.clone()).await.0, StatusCode::NOT_FOUND);
    assert_eq!(decide("sub-cat", "sign", "nope".into()).await.0, StatusCode::NOT_FOUND);
    let (status, sent) = decide("sub-cat", "sign", id.clone()).await;
    assert_eq!(status, StatusCode::OK, "{sent}");
    assert!(
        matches!(last().command, Uw::RecordNonDisclosureOutcome { decided_by, countersigned_by: Some(second), .. } if decided_by == "sam" && second == "cat")
    );
    assert_eq!(ask("sub-cat", "me".into()).await.1["pending"], json!([]));
    // One the module refuses is finished with; one taken back never goes.
    let (_, again) = tell("sub-sam", remedy("no_action")).await;
    let refused = again["prepared"].as_str().unwrap().to_string();
    assert_eq!(decide("sub-cat", "sign", refused).await.0, StatusCode::CONFLICT, "the review is closed");
    assert_eq!(ask("sub-cat", "me".into()).await.1["pending"], json!([]));
    let (_, again) = tell("sub-sam", remedy("no_action")).await;
    let taken_back = again["prepared"].as_str().unwrap().to_string();
    assert_eq!(decide("sub-sam", "withdraw", taken_back).await.0, StatusCode::OK);
    assert_eq!(ask("sub-cat", "me".into()).await.1["pending"], json!([]));

    // Reading a life's disclosures: through the intake, as the person signed in, with a purpose.
    let read = |who: &'static str, body: Value| {
        let portal = &portal;
        async move { portal.write("POST", who, "/staff/v1/underwriting/disclosures", body).await }
    };
    let (status, records) = read(
        "sub-amy",
        json!({ "proposal": "P2", "life": "L1", "purpose": "assess the case", "underwriter": "sam" }),
    )
    .await;
    assert_eq!((status, records[0]["hash"].as_str()), (StatusCode::OK, Some("hash-1")), "{records}");
    let asked = portal.intake.received.lock().unwrap().last().cloned().unwrap();
    assert_eq!((&asked["underwriter"], &asked["purpose"]), (&json!("amy"), &json!("assess the case")));
    assert_eq!(
        read("sub-amy", json!({ "proposal": "P2", "life": "L1", "purpose": " " })).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        read("sub-abe", json!({ "proposal": "P2", "life": "L1", "purpose": "curious" })).await.0,
        StatusCode::FORBIDDEN
    );

    // Parked effects, and putting one back.
    let parked = ask("sub-amy", "parked".into()).await.1;
    let effect = parked[0][0].as_u64().expect("one effect is parked");
    assert_eq!(tell("sub-amy", json!({ "requeue_effect": { "effect": effect } })).await.0, StatusCode::OK);
    assert!(matches!(last().command, Uw::RequeueEffect { by, .. } if by == "amy"));
    assert_eq!(ask("sub-amy", "parked".into()).await.1, json!([]));
    // More time for a life, and the proof for an option, are sent as the person signed in.
    assert_eq!(
        tell("sub-amy", json!({ "set_more_time": { "proposal": "P2", "life": "L1", "more_time": true } }))
            .await
            .0,
        StatusCode::OK
    );
    assert!(matches!(last().command, Uw::SetMoreTime { more_time: true, .. }));
    let (status, problem) = tell(
        "sub-amy",
        json!({ "check_option_evidence": { "case": "none", "underwriter": "someone", "accepted": true } }),
    )
    .await;
    assert_ne!(status, StatusCode::BAD_REQUEST, "{problem}");
    assert!(matches!(last().command, Uw::CheckOptionEvidence { underwriter, .. } if underwriter == "amy"));
    let (status, report) = ask("sub-sam", "report?from=2026-01-01T00:00:00Z&to=2030-01-01T00:00:00Z".into()).await;
    assert_eq!((status, report["decisions"].as_u64().map(|n| n >= 1)), (StatusCode::OK, Some(true)), "{report}");

    // The module unreachable, and no intake: the console says so.
    let now = portal.now();
    let down = router(Arc::new(AppState {
        wordings: Arc::default(),
        public_url: None,
        returns: Arc::default(),
        channel: portal.local.clone(),
        customers: Auth::Dev,
        workforce: Auth::Dev,
        clock: Arc::new(move || now),
        regions: portal.regions.clone(),
        ui: UiConfig::Dev,
        pricing: Arc::new(common::pricing::Down),
        complaints: Arc::new(common::pricing::Down),
        commission: Arc::new(common::pricing::Down),
        finance: Arc::new(common::pricing::Down),
        claims: Arc::new(common::claims::Down),
        reinsurance: Arc::new(common::pricing::Down),
        payments: Arc::new(common::pricing::Down),
        collections: Arc::new(common::pricing::Down),
        correspondence: Arc::new(common::pricing::Down),
        wellness: Arc::new(common::pricing::Down),
        underwriting: Arc::new(common::pricing::Down),
        master: None,
        intake: None,
        documents: None,
        group: None,
    }));
    for (path, body) in [
        ("/staff/v1/underwriting/parked", None),
        ("/staff/v1/underwriting/commands", Some(json!({ "retry_issue": { "proposal": "P2" } }))),
        (
            "/staff/v1/underwriting/disclosures",
            Some(json!({ "proposal": "P2", "life": "L1", "purpose": "assess" })),
        ),
    ] {
        let request = Request::builder()
            .method(if body.is_some() { "POST" } else { "GET" })
            .uri(path)
            .header("x-dev-subject", "sub-amy")
            .header("idempotency-key", "k");
        let response = down
            .clone()
            .oneshot(request.body(Body::from(body.map(|b| b.to_string()).unwrap_or_default())).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{path}");
    }
}

/// Purpose: staff move money from the payments console: every command reaches the payments module as the person signed in,
/// with the roles their staff actions give them there, at New Zealand time. A maker prepares and checks a run, the maker cannot
/// approve it, an approver can; someone holding both approval and payee maintenance is refused by the module; a reader can read
/// runs but not bank details (a run's account numbers come to them masked) and cannot act; and the drivers' commands and
/// questions are never sent from here.
/// Why it matters: payments trusts the roles it is told. If the console let a page name the actor or claim roles, or sent a
/// bank's own answers as a person, the separation between who prepares, who approves and who decides who may be paid would be
/// gone. A read-only auditor was sent every payee's full account number in a run's answer (found by the scenario run, F2-18-1).
#[tokio::test]
async fn the_payments_console() {
    use ic_payments::model::{Counterparty, NewItem, RunType};
    use ic_payments::Command as Pay;
    let portal = portal();
    let ask = |who: &'static str, rest: &str| {
        let path = format!("/staff/v1/payments/{rest}");
        let portal = &portal;
        async move { portal.get(who, &path).await }
    };
    let tell = |who: &'static str, command: Value| {
        let portal = &portal;
        async move { portal.write("POST", who, "/staff/v1/payments/commands", command).await }
    };
    let run = |c: Pay| serde_json::to_value(c).unwrap();

    // Who gets in, and with which roles.
    assert_eq!(ask("sub-gl1", "me").await.0, StatusCode::FORBIDDEN);
    let (status, me) = ask("sub-payview", "me").await;
    assert_eq!((status, &me["roles"]), (StatusCode::OK, &json!([])), "{me}");
    assert_eq!(ask("sub-paymk", "me").await.1["roles"], json!(["maker"]));

    // A maker prepares a run, adds a payment and checks it; the module hears it from them, as a maker, at New Zealand time.
    let prepare = run(Pay::PrepareRun {
        run: "r9".into(),
        run_type: RunType::LumpSumClaims,
        fund: "sf1".into(),
        currency: ic_money::Currency::NZD,
        value_date: d(2027, 11, 16),
        urgent: false,
    });
    let (status, done) = tell("sub-paymk", prepare.clone()).await;
    assert_eq!((status, &done["accepted"]), (StatusCode::OK, &json!(true)), "{done}");
    let sent = portal.payments.received().last().cloned().unwrap();
    assert_eq!((sent.actor.id.as_str(), sent.actor.roles.len()), ("pay-mk", 1));
    assert_eq!(sent.at, ic_channels::payments::nz(portal.now()));
    let item = NewItem {
        item: "i9".into(),
        source: "claims:c9".into(),
        counterparty: Counterparty::Payee("p1".into()),
        amount_minor: 1_000_00,
        policy: None,
        payable: None,
        refund: None,
        represents: None,
    };
    assert_eq!(
        tell(
            "sub-paymk",
            run(Pay::AddRunItems {
                run: "r9".into(),
                items: vec![item]
            })
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(tell("sub-paymk", run(Pay::CheckRun { run: "r9".into() })).await.0, StatusCode::OK);
    // The maker cannot approve their own run; someone who also maintains payees cannot approve at all; an approver can.
    let approve = run(Pay::ApproveRun { run: "r9".into() });
    let (status, problem) = tell("sub-paymk", approve.clone()).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::FORBIDDEN, Some("not_permitted")), "{problem}");
    let (status, problem) = tell("sub-paybad", approve.clone()).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::FORBIDDEN, Some("role_conflict")), "{problem}");
    let (status, approved) = tell("sub-payap", approve).await;
    assert_eq!(status, StatusCode::OK, "{approved}");
    let (status, totals) = ask("sub-payview", "control_totals?run=r9").await;
    assert_eq!((status, &totals["unsent"]), (StatusCode::OK, &json!(0)), "{totals}");
    assert_eq!(ask("sub-payview", "run?run=r9").await.1["makers"], json!(["pay-mk"]));
    // The run's items carry the account each is paid to: in full for the maker, masked for a reader (F2-18-1).
    let (_, made) = ask("sub-paymk", "run?run=r9").await;
    assert!(made.to_string().contains("0101234567"), "{made}");
    let (_, read) = ask("sub-payview", "run?run=r9").await;
    assert!(!read.to_string().contains("0101234567") && read.to_string().contains("******4567"), "{read}");
    // The module's own refusals come back with its code.
    let (status, problem) = tell("sub-paymk", prepare).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::CONFLICT, Some("duplicate_run")), "{problem}");

    // What is not people's to send, or to read.
    assert_eq!(tell("sub-payview", run(Pay::CheckRun { run: "r9".into() })).await.0, StatusCode::FORBIDDEN);
    for bad in [
        run(Pay::ScreenPayee {
            payee: "p1".into(),
            clear: true,
        }),
        run(Pay::Prune { before: d(2027, 1, 1) }),
        json!({ "type": "nope" }),
        json!("prepare_run"),
    ] {
        assert_eq!(tell("sub-paymk", bad.clone()).await.0, StatusCode::BAD_REQUEST, "{bad}");
    }
    assert_eq!(
        ask("sub-payview", "payee?payee=p1").await.0,
        StatusCode::BAD_REQUEST,
        "bank details are not a reader's"
    );
    let (status, payee) = ask("sub-paypm", "payee?payee=p1").await;
    assert_eq!((status, payee["party"].as_str()), (StatusCode::OK, Some("party-p1")), "{payee}");
    assert_eq!(ask("sub-paypm", "config").await.0, StatusCode::BAD_REQUEST, "the connectivity administrator's");
    assert_eq!(ask("sub-paymk", "outbox").await.0, StatusCode::BAD_REQUEST, "the drivers'");
    assert_eq!(ask("sub-paymk", "run").await.0, StatusCode::BAD_REQUEST, "which run");
    assert_eq!(ask("sub-paymk", "run?run=nope").await.0, StatusCode::NOT_FOUND);
    let (status, waiting) = ask("sub-payview", "waiting").await;
    assert_eq!((status, waiting.is_object() || waiting.is_array()), (StatusCode::OK, true), "{waiting}");
    let (status, book) = ask("sub-payview", "reconciliation?account=anz-sf1-claims").await;
    assert_eq!((status, book["closing_minor"].as_i64()), (StatusCode::OK, Some(10_000_000_00)), "{book}");
    assert_eq!(ask("sub-payview", "readiness").await.0, StatusCode::OK);
    // The lists, without account details: open runs, open exceptions (all, or one account's) and a party's payees.
    let (status, runs) = ask("sub-payview", "open_runs").await;
    assert!(status == StatusCode::OK && runs.to_string().contains("r9"), "{runs}");
    assert_eq!(ask("sub-payview", "open_exceptions").await.0, StatusCode::OK);
    // Matches applied by hand that wait for a second reconciler; one nobody proposed is not there.
    assert_eq!(ask("sub-payview", "matches_to_approve").await, (StatusCode::OK, json!({})));
    assert_eq!(ask("sub-payview", "manual_match?exception=exc-none").await.0, StatusCode::NOT_FOUND);
    assert_eq!(ask("sub-payview", "manual_match").await.0, StatusCode::BAD_REQUEST);
    // Proposing one is a person's command: it reaches the module, which refuses a maker who is no reconciler.
    let (status, problem) = tell(
        "sub-paymk",
        json!({ "type": "propose_receipt_match", "exception": "exc-none", "parts": [["pol-c1", 120_00]] }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{problem}");
    assert_eq!(ask("sub-payview", "open_exceptions?account=anz-sf1-claims").await.0, StatusCode::OK);
    let (status, party) = ask("sub-payview", "party?party=party-p1").await;
    assert_eq!((status, &party["payees"][0]["payee"]), (StatusCode::OK, &json!("p1")), "{party}");
    assert!(!party.to_string().contains("0101234567"), "no account number: {party}");
    // Notices: a role's own list, and only for someone who holds the role.
    let (status, notices) = ask("sub-paymk", "notices?role=maker").await;
    assert_eq!((status, notices.is_array()), (StatusCode::OK, true), "{notices}");
    assert_eq!(
        ask("sub-paymk", "notices?role=approver").await.0,
        StatusCode::BAD_REQUEST,
        "not a role the maker holds"
    );
    assert_eq!(ask("sub-payview", "notices?role=maker").await.0, StatusCode::BAD_REQUEST);
    // Misdirected money and the other new lookups reach the module.
    assert_eq!(ask("sub-payview", "open_misdirections").await, (StatusCode::OK, json!({})));
    assert_eq!(ask("sub-payview", "misdirection?misdirection=m-none").await.0, StatusCode::NOT_FOUND);
    assert_eq!(ask("sub-payview", "incident?incident=i-none").await.0, StatusCode::NOT_FOUND);
    assert_eq!(ask("sub-payview", "tax_remittance?remittance=t-none").await.0, StatusCode::NOT_FOUND);
    assert_eq!(ask("sub-payview", "funding_forecast?from=2027-01-01&to=2027-01-31").await.0, StatusCode::OK);
    // A notice that does not exist is refused by the module, not by the console's list.
    let (status, problem) = tell("sub-paymk", json!({ "type": "close_notice", "notice": 999, "action": "looked" })).await;
    assert_ne!(status, StatusCode::BAD_REQUEST, "close_notice is a console command: {problem}");

    // The module unreachable: the console says so.
    let now = portal.now();
    let down = router(Arc::new(AppState {
        wordings: Arc::default(),
        public_url: None,
        returns: Arc::default(),
        channel: portal.local.clone(),
        customers: Auth::Dev,
        workforce: Auth::Dev,
        clock: Arc::new(move || now),
        regions: portal.regions.clone(),
        ui: UiConfig::Dev,
        pricing: Arc::new(common::pricing::Down),
        complaints: Arc::new(common::pricing::Down),
        commission: Arc::new(common::pricing::Down),
        finance: Arc::new(common::pricing::Down),
        claims: Arc::new(common::claims::Down),
        reinsurance: Arc::new(common::pricing::Down),
        payments: Arc::new(common::pricing::Down),
        collections: Arc::new(common::pricing::Down),
        correspondence: Arc::new(common::pricing::Down),
        wellness: Arc::new(common::pricing::Down),
        underwriting: Arc::new(common::pricing::Down),
        master: None,
        intake: None,
        documents: None,
        group: None,
    }));
    for (path, body) in [
        ("/staff/v1/payments/waiting", None),
        ("/staff/v1/payments/commands", Some(json!({ "type": "check_run", "run": "r9" }))),
    ] {
        let request = Request::builder()
            .method(if body.is_some() { "POST" } else { "GET" })
            .uri(path)
            .header("x-dev-subject", "sub-paymk")
            .header("idempotency-key", "k");
        let response = down
            .clone()
            .oneshot(request.body(Body::from(body.map(|b| b.to_string()).unwrap_or_default())).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{path}");
    }
}

/// Purpose: compliance works cases of premium paid by someone other than the owner from the payments console: only someone
/// holding the compliance role is told of the cases or may clear one, and the payments module (its real state machine) hears
/// the clearance from them as compliance.
/// Why it matters: who pays whose premium is an anti-money-laundering matter; a maker or reader who could read the cases would
/// see who funds whom, and one who could clear a case would close an alert they have no standing to judge.
#[tokio::test]
async fn compliance_works_third_party_payer_cases() {
    let portal = portal();
    assert_eq!(portal.get("sub-paycmp", "/staff/v1/payments/me").await.1["roles"], json!(["compliance"]));
    let (status, cases) = portal.get("sub-paycmp", "/staff/v1/payments/payer_cases").await;
    assert_eq!((status, cases), (StatusCode::OK, json!([])));
    assert_eq!(portal.get("sub-paymk", "/staff/v1/payments/payer_cases").await.0, StatusCode::BAD_REQUEST);
    assert_eq!(
        portal.get("sub-payview", "/staff/v1/payments/payer_case?case=c1").await.0,
        StatusCode::BAD_REQUEST
    );
    let (status, problem) = portal.get("sub-paycmp", "/staff/v1/payments/payer_case?case=c1").await;
    assert_eq!((status, &problem["detail"]), (StatusCode::NOT_FOUND, &json!("there is no case c1")));

    let clear = json!({ "type": "clear_payer", "case": "c1", "relationship": "party-a1/rel-1", "note": "a parent pays a child's premium" });
    let (status, problem) = portal.write("POST", "sub-paymk", "/staff/v1/payments/commands", clear.clone()).await;
    assert_eq!((status, &problem["title"]), (StatusCode::FORBIDDEN, &json!("not_permitted")), "{problem}");
    let (status, problem) = portal.write("POST", "sub-paycmp", "/staff/v1/payments/commands", clear).await;
    assert_eq!((status, &problem["title"]), (StatusCode::CONFLICT, &json!("unknown_case")), "{problem}");
    let sent = portal.payments.received().last().cloned().unwrap();
    assert_eq!(sent.actor.id.as_str(), "pay-cmp");
}

/// Purpose: the life on a proposal answers an offer of terms from the app: what to sign is the underwriting module's exact
/// message for that offer and answer, the answer goes only with the enrolled device's signature over it, from the caller
/// `customer-app`, and the module (its real state machine) checks it again and refuses an offer that is not open; someone who
/// is not the life, an unknown answer, a forged signature or a token without a proven device is refused before anything leaves.
/// Why it matters: nothing sent a life's answer to an offer of terms, so loaded or excluded terms could never be accepted;
/// accepting terms binds the life to them, so it must be their own act on a device bound to them.
#[tokio::test]
async fn the_life_answers_an_offer_of_terms() {
    use base64::Engine;
    let portal = &portal();
    let now = portal.now();
    let retail = portal.retail.clone();
    portal.local.with(|state| {
        use ic_log::StateMachine;
        let rows = json!({ "publish_rows": { "rows": [{ "key": format!("{retail}/underwriting/P3/L1"), "policy": retail, "kind": "proposal_life",
            "subject": KIRI, "sensitive": true, "body": { "proposal": "P3", "life": "L1", "stage": "offer_open", "waiting_on": "applicant",
                "offer": { "offer": "O-1", "expires_on": "2027-12-15" } } }], "remove": [] } });
        let command: Command = serde_json::from_value(rows).unwrap();
        let output = state.apply(
            9_002,
            ic_channels::Envelope {
                key: "life-row-offer".into(),
                at: now,
                actor: module("underwriting"),
                command,
            },
        );
        assert!(output.result.is_ok(), "{:?}", output.result);
    });
    // The life sees the offer waiting on their application; nobody else does.
    let (status, mine) = portal.get("sub-kiri", "/app/v1/proposals").await;
    assert_eq!(status, StatusCode::OK, "{mine}");
    assert_eq!((&mine[0]["proposal"], &mine[0]["offer"]["offer"]), (&json!("P3"), &json!("O-1")), "{mine}");
    assert_eq!(portal.get("sub-adv", "/app/v1/proposals").await.1, json!([]));
    let path = "/app/v1/proposals/P3/lives/L1/offers/O-1";
    let send = |body: Value| {
        let text = body.to_string();
        let headers = portal.proof(&portal.phone, "POST", path, &text);
        async move { portal.send("POST", path, &headers, Some(body)).await }
    };

    let (status, to_sign) = portal.get("sub-kiri", &format!("{path}/accept/to-sign")).await;
    assert_eq!(status, StatusCode::OK, "{to_sign}");
    let message = base64::engine::general_purpose::STANDARD.decode(to_sign["message"].as_str().unwrap()).unwrap();
    assert_eq!(message, ic_underwriting::signing::message("offer", &["P3", "L1", "O-1", "accept"]));
    assert_eq!(portal.get("sub-adv", &format!("{path}/accept/to-sign")).await.0, StatusCode::NOT_FOUND);
    assert_eq!(portal.get("sub-kiri", &format!("{path}/maybe/to-sign")).await.0, StatusCode::NOT_FOUND);

    let before = portal.underwriting.log().len();
    let (status, problem) = portal.write("POST", "sub-kiri", path, json!({ "response": "accept" })).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::UNAUTHORIZED, Some("device_required")));
    assert_eq!(send(json!({ "response": "accept" })).await.1["title"], "bad_answer", "unsigned");
    assert_eq!(
        send(json!({ "response": "maybe", "signature": portal.phone.sign(&message) })).await.1["title"],
        "bad_answer"
    );
    let decline = ic_underwriting::signing::message("offer", &["P3", "L1", "O-1", "decline"]);
    assert_eq!(
        send(json!({ "response": "accept", "signature": portal.phone.sign(&decline) })).await.1["title"],
        "bad_answer",
        "signed for another answer"
    );
    assert_eq!(portal.underwriting.log().len(), before, "nothing reached the module");

    // Signed: it reaches the module from the app, which refuses an offer that is not open on its proposal.
    let (status, problem) = send(json!({ "response": "accept", "signature": portal.phone.sign(&message) })).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::CONFLICT, Some("no_offer")), "{problem}");
    let sent = portal.underwriting.log().last().cloned().unwrap();
    assert_eq!(sent.caller, "customer-app");
    assert!(matches!(&sent.command, ic_underwriting::command::Command::RespondToOffer { offer, .. } if offer == "O-1"));
}

/// Purpose: when the core refused to issue a proposal because the product changed, the life sees on their application that a
/// new price waits; the portal prices the policy the proposal would issue as, today, on the product version and rate tables in
/// use (never a rate table scoped to another group), shows the price accepted and the new one with the exact bytes to sign, and
/// sends the module the signed acceptance with that binding; a price the page did not show, a forged signature, someone who is
/// not the life, or a proposal with no requote is refused before anything leaves.
/// Why it matters: nothing put a changed product's new price in front of the applicant, so the proposal sat refused; and a
/// new price must be the one the core charges today and the one the applicant signed, never one the page made up.
#[tokio::test]
async fn the_applicant_accepts_a_new_price_when_the_product_changed() {
    use base64::Engine;
    let portal = &portal();
    let now = portal.now();
    let retail = portal.retail.clone();
    let priced = application(77, "term-life");
    let group = priced["group"].as_str().unwrap().to_string();
    portal.local.with(|state| {
        use ic_log::StateMachine;
        let rows = json!({ "publish_rows": { "rows": [{ "key": format!("{retail}/underwriting/P4/L1"), "policy": retail, "kind": "proposal_life",
            "subject": KIRI, "sensitive": true, "body": { "proposal": "P4", "life": "L1", "stage": "accepted", "waiting_on": null,
                "requote": { "code": "product_changed", "quoted_minor": 1100, "application": priced } } }], "remove": [] } });
        let command: Command = serde_json::from_value(rows).unwrap();
        let output = state.apply(
            9_003,
            ic_channels::Envelope {
                key: "life-row-requote".into(),
                at: now,
                actor: module("underwriting"),
                command,
            },
        );
        assert!(output.result.is_ok(), "{:?}", output.result);
    });
    // Two more whose new price the core cannot give: a product it does not know, and a day not yet open.
    for (n, product) in [(5u64, "none"), (6, "later")] {
        portal.local.with(|state| {
            use ic_log::StateMachine;
            let rows = json!({ "publish_rows": { "rows": [{ "key": format!("{retail}/underwriting/P{n}/L1"), "policy": retail, "kind": "proposal_life",
                "subject": KIRI, "sensitive": true, "body": { "proposal": format!("P{n}"), "life": "L1", "stage": "accepted",
                    "requote": { "code": "product_changed", "quoted_minor": 1, "application": application(70 + n, product) } } }], "remove": [] } });
            let command: Command = serde_json::from_value(rows).unwrap();
            let output = state.apply(
                9_003 + n,
                ic_channels::Envelope {
                    key: format!("life-row-requote-{n}"),
                    at: now,
                    actor: module("underwriting"),
                    command,
                },
            );
            assert!(output.result.is_ok(), "{:?}", output.result);
        });
    }
    assert_eq!(
        portal.get("sub-kiri", "/app/v1/proposals/P5/lives/L1/requote").await.1["title"],
        "unknown_product"
    );
    assert_eq!(
        portal.get("sub-kiri", "/app/v1/proposals/P6/lives/L1/requote").await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    let mine = portal.get("sub-kiri", "/app/v1/proposals").await.1;
    let mine: Vec<&Value> = mine.as_array().unwrap().iter().filter(|a| a["proposal"] == "P4").collect();
    assert_eq!(
        mine[0]["requote"],
        json!({ "code": "product_changed", "quoted_minor": 1100 }),
        "the life sees prices, not the application: {mine:?}"
    );

    let path = "/app/v1/proposals/P4/lives/L1/requote";
    let (status, shown) = portal.get("sub-kiri", path).await;
    assert_eq!(status, StatusCode::OK, "{shown}");
    assert_eq!(
        (shown["was_minor"].as_i64(), shown["premium_minor"].as_i64(), shown["product_hash"].as_str()),
        (Some(1100), Some(1234), Some("hash-2-term-life"))
    );
    let message = base64::engine::general_purpose::STANDARD.decode(shown["message"].as_str().unwrap()).unwrap();
    assert_eq!(message, ic_underwriting::signing::message("requote", &["P4", "hash-2-term-life", "1234"]));
    assert_eq!(portal.get("sub-adv", path).await.0, StatusCode::NOT_FOUND);
    assert_eq!(portal.get("sub-kiri", "/app/v1/proposals/P3/lives/L1/requote").await.0, StatusCode::NOT_FOUND);

    let send = |body: Value| {
        let text = body.to_string();
        let headers = portal.proof(&portal.phone, "POST", path, &text);
        async move { portal.send("POST", path, &headers, Some(body)).await }
    };
    let before = portal.underwriting.log().len();
    let signature = portal.phone.sign(&message);
    let unproven = portal
        .write(
            "POST",
            "sub-kiri",
            path,
            json!({ "product_hash": "hash-2-term-life", "premium_minor": 1234, "signature": signature }),
        )
        .await;
    assert_eq!((unproven.0, unproven.1["title"].as_str()), (StatusCode::UNAUTHORIZED, Some("device_required")));
    let (status, problem) = send(json!({ "product_hash": "hash-2-term-life", "premium_minor": 999, "signature": signature })).await;
    assert_eq!(
        (status, problem["title"].as_str()),
        (StatusCode::CONFLICT, Some("price_changed")),
        "a price the page did not show"
    );
    let other = portal
        .phone
        .sign(&ic_underwriting::signing::message("requote", &["P4", "hash-2-term-life", "999"]));
    assert_eq!(
        send(json!({ "product_hash": "hash-2-term-life", "premium_minor": 1234, "signature": other }))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(portal.underwriting.log().len(), before, "nothing reached the module");

    // Signed over the price shown: it reaches the module from the app with the binding the portal worked out.
    send(json!({ "product_hash": "hash-2-term-life", "premium_minor": 1234, "signature": signature })).await;
    let sent = portal.underwriting.log().last().cloned().unwrap();
    assert_eq!(sent.caller, "customer-app");
    let ic_underwriting::command::Command::Requote { proposal, quote, .. } = &sent.command else {
        panic!("{:?}", sent.command)
    };
    assert_eq!(
        (proposal.0.as_str(), quote.product_hash.as_str(), quote.premium_minor),
        ("P4", "hash-2-term-life", 1234)
    );
    let tables: Vec<&str> = quote.rate_tables.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(tables, ["term-rates"], "not another group's table (group {group})");
}

/// Purpose: the life on a proposal gives and withdraws their own consents from the app: what to sign is the underwriting
/// module's exact message, a consent that needs the life's signed step goes only with the enrolled device's signature over it,
/// the module (its real state machine) records it from the caller `customer-app` and checks the signature again, and anyone
/// who is not the life, an unknown kind, a forged signature or a token without a proven device is refused before anything leaves.
/// Why it matters: these consents let an adviser see a life's answers, let an employer see a loading that implies health, and
/// confirm a replacement disclosure; each must be the life's own act on a device bound to them.
#[tokio::test]
async fn the_life_gives_their_own_consents() {
    use base64::Engine;
    use ic_underwriting::command::Command as Uw;
    let portal = &portal();
    let now = portal.now();
    let retail = portal.retail.clone();
    portal.local.with(|state| {
        use ic_log::StateMachine;
        let rows = json!({ "publish_rows": { "rows": [{ "key": format!("{retail}/underwriting/P3/L1"), "policy": retail, "kind": "proposal_life",
            "subject": KIRI, "sensitive": true, "body": { "proposal": "P3", "life": "L1", "stage": "disclosure", "waiting_on": "underwriter" } }], "remove": [] } });
        let command: Command = serde_json::from_value(rows).unwrap();
        let output = state.apply(9_002, ic_channels::Envelope { key: "life-row-p2".into(), at: now, actor: module("underwriting"), command });
        assert!(output.result.is_ok(), "{:?}", output.result);
    });
    let base = "/app/v1/proposals/P3/lives/L1/consents";
    let send = |kind: &'static str, body: Value| {
        let path = format!("{base}/{kind}");
        let text = body.to_string();
        let headers = portal.proof(&portal.phone, "POST", &path, &text);
        async move { portal.send("POST", &path, &headers, Some(body)).await }
    };

    // What to sign is the module's own message, and only the life may ask.
    let (status, to_sign) = portal.get("sub-kiri", &format!("{base}/adviser_access/to-sign")).await;
    assert_eq!((status, &to_sign["signed"]), (StatusCode::OK, &json!(true)), "{to_sign}");
    let message = base64::engine::general_purpose::STANDARD.decode(to_sign["message"].as_str().unwrap()).unwrap();
    assert_eq!(message, ic_underwriting::signing::message("consent", &["P3", "L1", "adviser_access"]));
    assert_eq!(portal.get("sub-adv", &format!("{base}/adviser_access/to-sign")).await.0, StatusCode::NOT_FOUND);
    assert_eq!(portal.get("sub-kiri", &format!("{base}/nonsense/to-sign")).await.0, StatusCode::NOT_FOUND);

    // Refused before anything leaves.
    let before = portal.underwriting.log().len();
    let (status, problem) = portal
        .write("POST", "sub-kiri", &format!("{base}/obtain_records"), json!({ "granted": true }))
        .await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::UNAUTHORIZED, Some("device_required")));
    assert_eq!(send("adviser_access", json!({ "granted": true })).await.1["title"], "bad_consent", "unsigned");
    let forged = json!({ "granted": true, "signature": portal.phone.sign(b"something else") });
    assert_eq!(send("adviser_access", forged).await.1["title"], "bad_consent");
    assert_eq!(send("nonsense", json!({ "granted": true })).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(send("obtain_records", json!({ "granted": "yes" })).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(portal.underwriting.log().len(), before, "nothing reached the module");

    // Given: one with the signed step, one without; the module records both from the app.
    let (status, done) = send("adviser_access", json!({ "granted": true, "signature": portal.phone.sign(&message) })).await;
    assert_eq!(status, StatusCode::OK, "{done}");
    let sent = portal.underwriting.log().last().cloned().unwrap();
    assert_eq!(sent.caller, "customer-app");
    assert!(matches!(
        &sent.command,
        Uw::RecordConsent {
            granted: true,
            signed: Some(_),
            ..
        }
    ));
    assert_eq!(send("obtain_records", json!({ "granted": true })).await.0, StatusCode::OK);
    // Withdrawn without a signature.
    assert_eq!(send("adviser_access", json!({ "granted": false })).await.0, StatusCode::OK);
    // The module's own refusal: this proposal is not a replacement.
    let message = ic_underwriting::signing::message("consent", &["P3", "L1", "replacement_disclosure"]);
    let (status, problem) = send("replacement_disclosure", json!({ "granted": true, "signature": portal.phone.sign(&message) })).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::CONFLICT, Some("replacement")), "{problem}");

    // Before signing a replacement disclosure the life reads what it means. This proposal replaces nothing, so there is no statement.
    let statement = format!("{base}/replacement_disclosure/statement");
    let (status, problem) = portal.get("sub-kiri", &statement).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::CONFLICT, Some("replacement")), "{problem}");
    // Underwriting publishes what the proposal adds; the portal sets it beside the cover and premium the core holds.
    portal.local.with(|state| {
        use ic_log::StateMachine;
        let rows = json!({ "publish_rows": { "rows": [{ "key": format!("{retail}/underwriting/P3/L1"), "policy": retail, "kind": "proposal_life",
            "subject": KIRI, "sensitive": true, "body": { "proposal": "P3", "life": "L1", "stage": "disclosure", "waiting_on": "applicant",
                "replacement": { "cover": "life", "sum_insured_minor": 250_000_00i64, "premium_minor": 91_20 } } }], "remove": [] } });
        let command: Command = serde_json::from_value(rows).unwrap();
        let output = state.apply(
            9_003,
            ic_channels::Envelope {
                key: "life-row-p3-replacement".into(),
                at: now,
                actor: module("underwriting"),
                command,
            },
        );
        assert!(output.result.is_ok(), "{:?}", output.result);
    });
    let (status, shown) = portal.get("sub-kiri", &statement).await;
    assert_eq!(status, StatusCode::OK, "{shown}");
    assert_eq!(
        (&shown["replaced_policy"], &shown["gained"], &shown["lost"], &shown["new_premium_minor"]),
        (
            &json!(retail),
            &json!({ "cover": "life", "sum_insured_minor": 250_000_00i64 }),
            &json!([]),
            &json!(91_20)
        ),
        "{shown}"
    );
    assert_eq!(
        (shown["kept"][0]["cover"].as_str(), shown["kept"][0]["sum_insured_minor"].as_i64()),
        (Some("life"), Some(500_000_00)),
        "{shown}"
    );
    assert!(shown["old_premium"]["total_minor"].as_i64().is_some_and(|total| total > 0), "{shown}");
    // It is the life's own: the adviser on the policy is told there is no such thing.
    assert_eq!(portal.get("sub-adv", &statement).await.0, StatusCode::NOT_FOUND);

    // A new policy that replaces this one: what the life holds on the old policy is what they lose, and nothing is kept.
    portal.local.with(|state| {
        use ic_log::StateMachine;
        let rows = json!({ "publish_rows": { "rows": [{ "key": "pol-new/underwriting/P4/L1", "policy": "pol-new", "kind": "proposal_life",
            "subject": KIRI, "sensitive": true, "body": { "proposal": "P4", "life": "L1", "stage": "disclosure", "waiting_on": "applicant",
                "replacement": { "cover": "life", "sum_insured_minor": 600_000_00i64, "premium_minor": 120_00, "replaced_policy": retail } } }], "remove": [] } });
        let command: Command = serde_json::from_value(rows).unwrap();
        let envelope = ic_channels::Envelope { key: "life-row-p4".into(), at: now, actor: module("underwriting"), command };
        let output = state.apply(9_004, envelope);
        assert!(output.result.is_ok(), "{:?}", output.result);
    });
    let (status, shown) = portal
        .get("sub-kiri", "/app/v1/proposals/P4/lives/L1/consents/replacement_disclosure/statement")
        .await;
    assert_eq!(
        (status, &shown["replaced_policy"], &shown["kept"]),
        (StatusCode::OK, &json!(retail), &json!([])),
        "{shown}"
    );
    assert_eq!(shown["lost"][0]["sum_insured_minor"], 500_000_00, "{shown}");
    assert!(shown["old_premium"]["total_minor"].as_i64().is_some_and(|total| total > 0), "{shown}");

    // A life the customer master says needs more time: underwriting is told when they come to sign, and never why.
    let asked = |portal: &Portal| {
        portal
            .underwriting
            .log()
            .iter()
            .filter(|e| matches!(&e.command, Uw::SetMoreTime { more_time: true, .. }))
            .count()
    };
    assert_eq!(asked(portal), 0, "nobody is marked yet");
    portal.master.vulnerable.lock().unwrap().push(KIRI.to_string());
    assert_eq!(portal.get("sub-kiri", &format!("{base}/adviser_access/to-sign")).await.0, StatusCode::OK);
    let sent = portal
        .underwriting
        .log()
        .into_iter()
        .rev()
        .find(|e| matches!(&e.command, Uw::SetMoreTime { .. }))
        .unwrap();
    assert_eq!(sent.caller, "underwriting-console");
    assert!(matches!(&sent.command, Uw::SetMoreTime { proposal, life, more_time: true } if proposal.to_string() == "P3" && life.to_string() == "L1"));
}

/// Purpose: a scheme member links their membership to themselves from the customer page, by the employee ID their employer
/// gave and the birth date the customer master holds for them, on a proven device; a wrong birth date reads the same as no
/// such member, a membership linked to them is said to be theirs, and the command reaches the group module as `member-portal`.
/// Once they leave, they see the continuation offered and take it up: only a product and cover the offer lists, an amount up
/// to it, and only an offer open to them.
/// Why it matters: the group module took both commands from the member's app, and nothing sent them, so a leaver could not
/// use the continuation option the scheme gives them. A membership claimed on an employee ID alone would let anyone who
/// learned a colleague's ID act on their cover. A member knows the scheme by its policy number, not an internal code (found by
/// the scenario run, SCN-09, F-SCN09-2, F-SCN09-4).
#[tokio::test]
async fn a_member_claims_their_membership_and_keeps_cover_on_leaving() {
    use common::group_module::harness::{attrs, change};
    use ic_group::command::Resolution;
    use ic_group::mapping::FileKind;
    use ic_group::record::RowChange;
    let module = Arc::new(common::group_module::seeded());
    let born = chrono::NaiveDate::from_ymd_opt(1988, 4, 12).unwrap();
    let mut kiri = attrs("staff", 80_000_00, "akl");
    kiri.date_of_birth = Some(born);
    // A retail policy is rated on sex and smoking, which the employer's file gives.
    kiri.sex = Some(ic_product::Sex::Female);
    kiri.smoker = Some(false);
    {
        let mut h = module.0.lock().unwrap();
        let today = h.today();
        h.upload("f-kiri", FileKind::Changes, vec![change(1, "E009", RowChange::Join, today, kiri.clone())])
            .unwrap();
        h.approve("f-kiri").unwrap();
        // The customer master could not be sure who E009 is, so nobody is linked yet.
        h.resolve("f-kiri", |_| Some(Resolution::Possible { candidates: vec![] }));
        h.settle();
        // E010 shares Kiri's birth date but is already someone else.
        h.upload("f-twin", FileKind::Changes, vec![change(1, "E010", RowChange::Join, today, kiri.clone())])
            .unwrap();
        h.approve("f-twin").unwrap();
        h.resolve("f-twin", |_| {
            Some(Resolution::Linked {
                party: ic_core::PartyId("someone-else".into()),
            })
        });
        h.settle();
    }
    let portal = portal_with(|employer| {
        Arc::new(common::group_module::Beside {
            employer,
            staff: module.clone(),
        })
    });
    let signed = |path: &'static str, body: Value| {
        let text = body.to_string();
        let headers = portal.proof(&portal.phone, "POST", path, &text);
        let portal = &portal;
        async move { portal.send("POST", path, &headers, Some(body)).await }
    };
    let claim = |employee: &str| json!({ "scheme": "harbour", "employee_id": employee });
    let (status, problem) = portal.write("POST", "sub-kiri", "/app/v1/memberships", claim("E009")).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::UNAUTHORIZED, Some("device_required")));
    // E001's birth date is not Kiri's: the same answer as an ID nobody has.
    let (status, wrong) = signed("/app/v1/memberships", claim("E001")).await;
    assert_eq!((status, wrong["title"].as_str()), (StatusCode::NOT_FOUND, Some("no_such_member")));
    assert_eq!(signed("/app/v1/memberships", claim("E404")).await.1["title"], "no_such_member");
    let (status, taken) = signed("/app/v1/memberships", claim("E010")).await;
    assert_eq!((status, taken["title"].as_str()), (StatusCode::CONFLICT, Some("already_identified")));
    // A scheme the group module does not know is its own answer, passed on.
    let unknown = json!({ "scheme": "nope", "employee_id": "E009" });
    assert_eq!(signed("/app/v1/memberships", unknown).await.0, StatusCode::NOT_FOUND);
    // A member knows the scheme by its number, as their employer gives it, not the group service's own name; another
    // policy's number is no scheme (found by the scenario run, SCN-09, F-SCN09-2).
    let retail = portal.get("sub-kiri", &format!("/app/v1/policies/{}", portal.retail)).await.1;
    let retail_number = retail["number"].as_str().unwrap_or(&portal.retail.0).to_string();
    let not_a_scheme = json!({ "scheme": retail_number, "employee_id": "E009" });
    assert_eq!(signed("/app/v1/memberships", not_a_scheme).await.0, StatusCode::NOT_FOUND);
    let number = portal.get("sub-hr", "/employer/v1/schemes").await.1[0]["number"].clone();
    let by_number = json!({ "scheme": number, "employee_id": "E009" });
    let (status, claimed) = signed("/app/v1/memberships", by_number).await;
    assert_eq!((status, &claimed), (StatusCode::OK, &json!({ "claimed": true, "already": false })), "{claimed}");
    module.0.lock().unwrap().settle();
    assert_eq!(
        signed("/app/v1/memberships", claim("E009")).await.1,
        json!({ "claimed": true, "already": true })
    );
    let linked = module.0.lock().unwrap().scheme().membership.active("E009").and_then(|m| m.party.clone());
    assert_eq!(linked.map(|p| p.0), Some(KIRI.to_string()));

    // Nothing to continue while still a member; once Kiri leaves, the offer is hers alone.
    assert_eq!(portal.get("sub-kiri", "/app/v1/memberships/continuations").await.1, json!([]));
    {
        let mut h = module.0.lock().unwrap();
        h.days(3);
        let today = h.today();
        h.apply("f-left", FileKind::Changes, vec![change(1, "E009", RowChange::Leave, today, kiri)]);
    }
    // The scheme is shown on the portal's group policy, which the member knows by its number.
    let held = std::mem::replace(
        &mut module.0.lock().unwrap().g.schemes.get_mut("harbour").unwrap().policy,
        portal.scheme.clone(),
    );
    let (status, open) = portal.get("sub-kiri", "/app/v1/memberships/continuations").await;
    module.0.lock().unwrap().g.schemes.get_mut("harbour").unwrap().policy = held;
    assert_eq!(status, StatusCode::OK, "{open}");
    let offer = open[0].clone();
    assert_eq!((&offer["scheme"], &offer["number"]), (&json!("harbour"), &number), "{open}");
    assert_eq!(portal.get("sub-adv", "/app/v1/memberships/continuations").await.1, json!([]));
    let path: &'static str = Box::leak(format!("/app/v1/memberships/continuations/harbour/{}", offer["offer"]).into_boxed_str());
    let product = offer["products"][0].clone();
    let cover = offer["covers"][0].clone();
    let choose =
        |product: &Value, cover: &Value, sum: i64| json!({ "product_code": product, "cover": cover, "frequency": "monthly", "sum_insured_minor": sum });
    let max = offer["max_sum_insured_minor"].as_i64().unwrap();
    let unproven = portal.write("POST", "sub-kiri", path, choose(&product, &cover, max)).await;
    assert_eq!((unproven.0, unproven.1["title"].as_str()), (StatusCode::UNAUTHORIZED, Some("device_required")));
    assert_eq!(signed(path, choose(&json!("health-plan"), &cover, max)).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(signed(path, choose(&product, &json!("trauma-x"), max)).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(signed(path, choose(&product, &cover, max + 1)).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(
        signed("/app/v1/memberships/continuations/harbour/999", choose(&product, &cover, max)).await.0,
        StatusCode::NOT_FOUND
    );
    let (status, taken) = signed(path, choose(&product, &cover, max)).await;
    assert_eq!(status, StatusCode::OK, "{taken}");
    module.0.lock().unwrap().settle();
    let h = module.0.lock().unwrap();
    let o = h.scheme().offers.values().find(|o| o.employee_id == "E009").unwrap();
    assert!(matches!(o.status, ic_group::state::OfferStatus::TakenUp { .. }), "{:?}", o.status);
}

/// Purpose: group administration staff work a scheme from the group console against the group module's own state machine:
/// they see the queue of exceptions, decide a possible match so it leaves the queue, freeze and unfreeze a scheme, try a
/// mapping against a sample without storing it, and every command reaches the module under the caller `group-admin` as the
/// person signed in. A reader reads but cannot act; the employer's and the driver's own commands are not sent from here; a
/// module refusal comes back as 409 with its code.
/// Why it matters: the group service trusts the portal's caller name for staff. If the console let a reader act, let a page
/// send the driver's commands (results from the core, scans, the customer master's answers), or hid the module's refusal,
/// staff could change membership nobody had the right to change, or think a change was made that was not. Staff also need the
/// scheme by its policy number and the people a possible match might be by name and birth date (found by the scenario run,
/// SCN-19, F-SCN19-2, and SCN-05, F-SCN05-4).
#[tokio::test]
async fn the_group_console() {
    let module = Arc::new(common::group_module::seeded());
    let portal = portal_with(|_| module.clone());
    let ask = |who: &'static str, rest: &str| {
        let path = format!("/staff/v1/group/{rest}");
        let portal = &portal;
        async move { portal.get(who, &path).await }
    };
    let tell = |who: &'static str, command: Value| {
        let portal = &portal;
        async move { portal.write("POST", who, "/staff/v1/group/commands", command).await }
    };

    // Who gets in, and who may act.
    assert_eq!(ask("sub-gl1", "me").await.0, StatusCode::FORBIDDEN);
    assert_eq!(ask("sub-grpview", "me").await.1["admin"], json!(false));
    assert_eq!(ask("sub-grp1", "me").await.1["admin"], json!(true));

    // The queue: the possible match on E002, for the whole book and for the scheme; a bad scheme name is refused here.
    let (status, open) = ask("sub-grpview", "exceptions").await;
    assert_eq!(status, StatusCode::OK, "{open}");
    let kinds: Vec<&Value> = open["exceptions"].as_array().unwrap().iter().map(|e| &e["kind"]).collect();
    assert!(kinds.contains(&&json!("possible_match")), "{open}");
    assert_eq!(ask("sub-grpview", "exceptions?scheme=harbour").await.1, open);
    assert_eq!(ask("sub-grpview", "exceptions?scheme=a/b").await.0, StatusCode::BAD_REQUEST);

    // The scheme, found from its policy; one file; a member; the mapping. An unknown scheme is 404, an unknown question 400.
    let policy = module.0.lock().unwrap().policy.0.clone();
    let (status, found) = ask("sub-grpview", &format!("scheme_of?policy={policy}")).await;
    assert_eq!((status, &found["schemes"]), (StatusCode::OK, &json!(["harbour"])));
    // Staff know a policy by its number: the scheme is found from that too, in any case
    // (found by the scenario run, SCN-19, F-SCN19-2).
    module.0.lock().unwrap().g.by_policy.insert(portal.scheme.clone(), "harbour".to_string());
    let number = portal.get("sub-hr", "/employer/v1/schemes").await.1[0]["number"]
        .as_str()
        .unwrap()
        .to_lowercase();
    let (status, found) = ask("sub-grpview", &format!("scheme_of?policy={number}")).await;
    assert_eq!((status, &found["schemes"]), (StatusCode::OK, &json!(["harbour"])), "{found}");
    // The people a possible match might be, by name and birth date, for those who may find
    // customers under the case in hand (found by the scenario run, SCN-05, F-SCN05-4).
    let (status, possible) = ask("sub-grp1", "possible_match?scheme=harbour&employee=E002&case=GRP-1").await;
    assert_eq!(status, StatusCode::OK, "{possible}");
    let candidate = &possible["candidates"][0];
    assert_eq!((&candidate["party"], candidate.get("closed")), (&json!("party-x"), None), "{possible}");
    assert!(candidate.get("name").is_some(), "{possible}");
    let (_, closed) = ask("sub-grpview", "possible_match?scheme=harbour&employee=E002").await;
    assert_eq!(closed["candidates"][0], json!({ "party": "party-x", "closed": true }), "{closed}");
    portal.master.down.store(true, Ordering::SeqCst);
    let (_, unread) = ask("sub-grp1", "possible_match?scheme=harbour&employee=E002&case=GRP-1").await;
    assert_eq!(unread["candidates"][0], json!({ "party": "party-x" }), "{unread}");
    portal.master.down.store(false, Ordering::SeqCst);
    let (status, scheme) = ask("sub-grpview", "scheme?scheme=harbour").await;
    assert_eq!((status, scheme["possible_matches"].as_u64()), (StatusCode::OK, Some(1)), "{scheme}");
    assert_eq!(ask("sub-grpview", "file?scheme=harbour&file=f1").await.1["file"], "f1");
    assert_eq!(ask("sub-grpview", "member?scheme=harbour&employee=E001").await.1["employee"], "E001");
    assert_eq!(ask("sub-grpview", "mapping?scheme=harbour").await.1["mapping"]["version"], 1);
    let (status, problem) = ask("sub-grpview", "scheme?scheme=nope").await;
    assert_eq!((status, &problem["title"]), (StatusCode::NOT_FOUND, &json!("not_found")), "{problem}");
    assert_eq!(ask("sub-grpview", "outbox").await.0, StatusCode::BAD_REQUEST);
    assert_eq!(ask("sub-grpview", "file?scheme=harbour").await.0, StatusCode::BAD_REQUEST);

    // A reader cannot act; nor can anyone send the driver's or a member's commands, or something that is not a command.
    let freeze = json!({ "freeze": { "scheme": "harbour", "reason": "wrong salaries in f1" } });
    assert_eq!(tell("sub-grpview", freeze.clone()).await.0, StatusCode::FORBIDDEN);
    for not_ours in [
        json!({ "record_scan": { "scheme": "harbour", "file": "f1", "clean": true } }),
        json!({ "self_register": { "scheme": "harbour" } }),
        json!({ "receive_file": { "scheme": "harbour", "file": "f9", "content_hash": "h" } }),
        json!(["freeze"]),
    ] {
        assert_eq!(tell("sub-grp1", not_ours.clone()).await.0, StatusCode::BAD_REQUEST, "{not_ours}");
    }

    // Staff decide the possible match: it leaves the queue, and the module heard it from grp-1 as group-admin.
    let decide = json!({ "resolve_match": { "scheme": "harbour", "employee_id": "E002", "decision": { "decision": "same_person", "party": "party-x" } } });
    let (status, done) = tell("sub-grp1", decide.clone()).await;
    assert_eq!((status, &done["applied"]), (StatusCode::OK, &json!(true)), "{done}");
    assert!(!ask("sub-grp1", "exceptions").await.1["exceptions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["kind"] == "possible_match"));
    // Deciding it again is the module's refusal, as 409 with its code.
    let (status, problem) = tell("sub-grp1", decide).await;
    assert_eq!(status, StatusCode::CONFLICT, "{problem}");
    assert!(problem["title"].as_str().is_some_and(|c| !c.is_empty()), "{problem}");

    // A member's health plan and a scheme split are staff's to send; the module decides (this scheme has no health menu).
    let plan = json!({ "choose_health_plan": { "scheme": "harbour", "employee_id": "E001", "plan": "core", "tier": "single", "effective": "2027-12-01" } });
    let (status, problem) = tell("sub-grp1", plan).await;
    assert_eq!(status, StatusCode::CONFLICT, "{problem}");
    assert!(ic_channels::group::staff_command(&json!({ "split_scheme": {} })).is_ok());

    // Freeze and unfreeze.
    assert_eq!(tell("sub-grp1", freeze).await.0, StatusCode::OK);
    assert_eq!(ask("sub-grp1", "scheme?scheme=harbour").await.1["frozen"], "wrong salaries in f1");
    assert_eq!(tell("sub-grp1", json!({ "unfreeze": { "scheme": "harbour" } })).await.0, StatusCode::OK);
    assert!(ask("sub-grp1", "scheme?scheme=harbour").await.1["frozen"].is_null());

    // A mapping trial, by anyone who may read: the answer says what the file would do, and nothing is stored.
    let next = serde_json::to_value(common::group_module::harness::mapping(2)).unwrap();
    let trial = json!({ "mapping": next, "sample": "Emp No\nE001\nE002\nE003\n" });
    let (status, tried) = portal
        .write("POST", "sub-grpview", "/staff/v1/group/schemes/harbour/mappings/try", trial.clone())
        .await;
    assert_eq!((status, &tried["mapping"]["version"]), (StatusCode::OK, &json!(2)), "{tried}");
    assert_eq!(ask("sub-grp1", "mapping?scheme=harbour").await.1["mapping"]["version"], 1);
    let path = |scheme: &str| format!("/staff/v1/group/schemes/{scheme}/mappings/try");
    assert_eq!(
        portal.write("POST", "sub-grpview", &path("a.."), trial.clone()).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        portal.write("POST", "sub-grpview", &path("harbour"), json!({ "sample": "x" })).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(portal.write("POST", "sub-gl1", &path("harbour"), trial).await.0, StatusCode::FORBIDDEN);

    // Group administration names a scheme's first administrator, once; after that the employer's own manage the list.
    let first = |who: &'static str, policy: String, party: &'static str| {
        let portal = &portal;
        async move {
            portal
                .write("PUT", who, &format!("/staff/v1/group/schemes/{policy}/first-admin"), json!({ "party": party }))
                .await
        }
    };
    let (status, problem) = first("sub-grp1", portal.scheme.to_string(), "hr-9").await;
    assert_eq!(
        (status, problem["title"].as_str()),
        (StatusCode::CONFLICT, Some("has_administrators")),
        "{problem}"
    );
    assert_eq!(first("sub-grpview", portal.retail.to_string(), "hr-9").await.0, StatusCode::FORBIDDEN);
    assert_eq!(first("sub-grp1", "pol-nowhere".to_string(), "hr-9").await.1["title"], "unknown_policy");
    let (status, done) = first("sub-grp1", portal.retail.to_string(), "hr-9").await;
    assert_eq!(status, StatusCode::OK, "{done}");
    portal
        .local
        .with(|state| assert!(state.data.directory.employer_admins[&portal.retail].contains(&ic_core::PartyId::from("hr-9"))));
    assert_eq!(first("sub-grp1", portal.retail.to_string(), "hr-10").await.0, StatusCode::CONFLICT);
    // The candidates for a possible match are asked of the group service by scheme and employee.
    assert_eq!(ask("sub-grp1", "possible_match?scheme=harbour").await.0, StatusCode::BAD_REQUEST);
}

/// Purpose: group administration asks underwriting to offer a member the loading the employer pays in one total: the
/// console lists whose loading is in the total with the proposal behind each, only an administrator may ask, a case with no
/// loading in the total and a request without a reason go nowhere, and the command names the proposal the group module gave.
/// Why it matters: underwriting's re-offer existed with nothing to send it, so a small scheme's invoice kept showing one
/// member's loading as the employer's whole total, from which the employer can tell whose health it reflects.
#[tokio::test]
async fn a_loading_is_reoffered_to_the_member_from_the_group_console() {
    let module = Arc::new(common::group_module::seeded());
    let portal = portal_with(|_| module.clone());
    let (status, listed) = portal.get("sub-grpview", "/staff/v1/group/loadings_in_total?scheme=harbour").await;
    assert_eq!((status, &listed["cases"]), (StatusCode::OK, &json!([])), "{listed}");
    let reoffer = |who: &'static str, body: Value| {
        let portal = &portal;
        async move { portal.write("POST", who, "/staff/v1/group/reoffer", body).await }
    };
    let body = json!({ "scheme": "harbour", "case": 1, "reason": "one holder left in the total" });
    assert_eq!(reoffer("sub-grpview", body.clone()).await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        reoffer("sub-grp1", body).await.0,
        StatusCode::BAD_REQUEST,
        "harbour has no loading in its total"
    );
    assert_eq!(
        reoffer("sub-grp1", json!({ "scheme": "nope", "case": 1, "reason": "x" })).await.0,
        StatusCode::NOT_FOUND
    );

    // The command, from a list that holds the case.
    let listed = json!({ "cases": [{ "case": 7, "employee": "E005", "cover": "life", "proposal": "grp-uw:harbour:3" }] });
    assert_eq!(
        ic_channels::group::reoffer(&listed, 7, " one holder left ").unwrap(),
        json!({ "reoffer_excess_terms": { "proposal": "grp-uw:harbour:3", "reason": "one holder left" } })
    );
    assert!(ic_channels::group::reoffer(&listed, 8, "why").is_err());
    assert!(ic_channels::group::reoffer(&listed, 7, " ").is_err());

    // A case the group module lists goes to underwriting as the staff caller; a proposal it does not know is its refusal.
    let listing = Arc::new(common::group_module::Listing(module.clone()));
    let portal = portal_with(|_| listing.clone());
    let body = json!({ "scheme": "harbour", "case": 7, "reason": "one holder left in the total" });
    let (status, refused) = portal.write("POST", "sub-grp1", "/staff/v1/group/reoffer", body).await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
}

/// Purpose: a refusal of the portal itself by the group service (its token lacks the `group-admin` role) is the portal's
/// fault, 502, not the person's; any other answer passes with the service's status; and a service that cannot be reached is
/// 503.
/// Why it matters: a 403 there would tell staff they lack a right they hold, and send them to the wrong people to fix it.
#[tokio::test]
async fn the_group_console_says_whose_fault_a_refusal_is() {
    let portal = portal();
    let double = portal.group.clone();
    let (status, problem) = portal
        .write("POST", "sub-grp1", "/staff/v1/group/commands", json!({ "unfreeze": { "scheme": "harbour" } }))
        .await;
    assert_eq!((status, &problem["title"]), (StatusCode::BAD_GATEWAY, &json!("role_not_held")), "{problem}");
    assert_eq!(portal.get("sub-grp1", "/staff/v1/group/invoices?scheme=harbour").await.0, StatusCode::OK);
    double.down.store(true, Ordering::SeqCst);
    assert_eq!(
        portal.get("sub-grp1", "/staff/v1/group/scheme?scheme=harbour").await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

/// Purpose: an adviser reads their own commission statement and a group the one paid to it, with totals by what earned each line
/// (base, each override level, fees); nobody can name another payee's statement, and one not yet cut is 404.
/// Why it matters: a dealer group checks its overrides against its own figures every month; it must see its statement and its
/// totals, while an adviser must never read the group's (or a colleague's) pay.
#[tokio::test]
async fn an_adviser_and_their_group_read_their_own_statements() {
    let portal = portal();
    let statement = |payee: &str, lines: Value| -> ic_distribution::Statement {
        serde_json::from_value(json!({ "payee": payee, "number": 1, "date": "2027-11-30", "cut_seq": 9, "opening_minor": 0,
            "lines": lines, "closing_minor": 0, "held_minor": 0, "held_reasons": [], "retention_minor": 0,
            "advance_outstanding_minor": 0, "advance_recovered_minor": 0, "payment_held": null, "payments": [], "hash": "h" }))
        .unwrap()
    };
    let line = |component: Value, amount: i64| {
        json!({ "date": "2027-11-15", "reference": "r", "kind": "commission", "ledger": "commission_payable", "policy": "pol-1",
            "book": null, "layer": null, "component": component, "earner": "e1", "amount_minor": amount })
    };
    {
        let mut module = portal.commission.0.lock().unwrap();
        let statements = &mut module.0.statements;
        statements.insert(
            "adv-1.1".into(),
            statement("adv-1", json!([line(json!("base"), 120_00), line(Value::Null, 10_00)])),
        );
        statements.insert(
            "fap-1.1".into(),
            statement(
                "fap-1",
                json!([
                    line(json!({ "override": "group" }), 30_00),
                    line(json!({ "override": "group" }), 5_00),
                    line(json!({ "fixed_fee": "referral" }), 50_00)
                ]),
            ),
        );
    }
    let (status, own) = portal.get("sub-adv", "/adviser/v1/statements/1").await;
    assert_eq!(
        (status, &own["payee"], &own["totals"]),
        (StatusCode::OK, &json!("adv-1"), &json!({ "base": 120_00, "commission": 10_00 })),
        "{own}"
    );
    let (status, group) = portal.get("sub-fap", "/adviser/v1/statements/1").await;
    assert_eq!(status, StatusCode::OK, "{group}");
    assert_eq!(group["totals"], json!({ "override:group": 35_00, "fee:referral": 50_00 }));
    assert!(group.get("cut_seq").is_none(), "the module's bookkeeping stays inside it");
    // The payee is always the person signed in: there is no way to ask for another's.
    let (status, missing) = portal.get("sub-adv", "/adviser/v1/statements/2").await;
    assert_eq!((status, &missing["title"]), (StatusCode::NOT_FOUND, &json!("no_such_statement")));
    assert!(missing["detail"].as_str().unwrap().contains("no statement number 2"), "{missing}");
    let (status, bad) = portal.get("sub-adv", "/adviser/v1/statements/x").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(bad["detail"].as_str().unwrap().contains("whole number"), "{bad}");
    // Over MCP the number may be given as a number (found by the scenario run, SCN-02).
    let by_number = call_tool(&portal, "sub-adv", "adviser_statement", json!({ "number": 1 })).await;
    assert_eq!(by_number["isError"], json!(false), "{by_number}");
}

/// Purpose: an assistant that puts a query field at the top level is told it goes in the query, and one that calls a tool not
/// open to it is told so before anything about its arguments.
/// Why it matters: `case` at the top level was refused as "not an argument of quote" though the description named it, and an
/// adviser calling an employer tool was told "party is required" before being told she may not act (found by the scenario run,
/// SCN-02 and SCN-04).
#[tokio::test]
async fn an_assistant_is_told_where_its_arguments_go() {
    let portal = portal();
    let misplaced = call_tool(&portal, "sub-sue", "find_customers", json!({ "case": "CALL-1", "body": {} })).await;
    assert_eq!(
        misplaced["content"][0]["text"], r#"case goes in the query: {"query": {"case": "..."}}"#,
        "{misplaced}"
    );
    let stray = call_tool(&portal, "sub-sue", "find_customers", json!({ "nonsense": 1 })).await;
    assert_eq!(stray["content"][0]["text"], "nonsense is not an argument of find_customers");
    let scheme = portal.scheme.to_string();
    let not_hers = call_tool(&portal, "sub-adv", "set_scheme_admin", json!({ "policy": scheme })).await;
    assert!(
        not_hers["content"][0]["text"].as_str().unwrap().starts_with("this tool is not open to you"),
        "{not_hers}"
    );
    // A staff tool names the actions that open it (SCN2-12, F2-12-6).
    let not_his = call_tool(&portal, "sub-sue", "customer_export", json!({})).await;
    assert!(
        not_his["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("this tool is not open to you: it needs a role that gives customers.export"),
        "{not_his}"
    );
    let hers = call_tool(&portal, "sub-hr", "set_scheme_admin", json!({ "policy": scheme })).await;
    assert_eq!(hers["content"][0]["text"], "party is required", "{hers}");
    // Arguments that would do reach the route, which refuses and logs it, and the assistant is told the same way what the
    // tool needs (F2-01-6); a module's tools are named by the module.
    let core = call_tool(&portal, "sub-amy", "core_policy", json!({ "number": "KTL00000071" })).await;
    let said = core["content"][0]["text"].as_str().unwrap();
    assert!(
        said.starts_with("this tool is not open to you: it needs a role that gives policy.core_view"),
        "{said}"
    );
    let named = |name: &str| ic_channels::mcp::not_yours(&ic_channels::mcp::catalogue().into_iter().find(|t| t.name == name).unwrap());
    assert!(named("payments_question").contains("any payments action"));
    assert!(named("finance_command").contains("a role that acts in finance, not only views it"));
    assert!(named("adviser_statement").contains("an adviser's engagement or practice"));
    assert!(named("set_scheme_admin").contains("being a scheme's administrator"));
    assert!(named("policy_claims").contains("an adviser's engagement or being a scheme's administrator"));
    assert!(named("status").contains("a sign-in of the kind it is for"));
}

/// Purpose: what a write answers says only what concerns the person who wrote: their own feed's new version (so the app syncs to
/// at least that and reads its own write) and nothing about anyone else on the policy, such as a relative whose feed also moved.
/// Why it matters: a write's answer that listed every feed it moved would tell one person who else can see the policy; without
/// the version the app could show a stale screen straight after the customer's own change.
#[tokio::test]
async fn a_write_answers_with_the_writers_own_version_and_nobody_elses() {
    let portal = portal();
    let policy = portal.retail.clone();
    let viewer = format!("/app/v1/policies/{policy}/delegates/daughter");
    assert_eq!(portal.write("PUT", "sub-kiri", &viewer, json!({ "access": "viewer" })).await.0, StatusCode::OK);
    let version_of = |who: &str| portal.local.with(|s| s.data.feeds.get(&ic_core::PartyId::from(who)).map(|f| f.version));
    let before = version_of("daughter").expect("the relative follows the policy");

    // Kiri declines indexation in the app; the policy's rows move for her and for her daughter.
    let request = json!({ "id": "r9", "policy": policy, "kind": "decline_indexation", "module": "policy-servicing" });
    let (status, answer) = portal.write("POST", "sub-kiri", "/app/v1/requests", request).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{answer}");
    let version = answer["version"].as_u64().expect("the writer's own version");
    assert_eq!(Some(version), version_of(KIRI));
    assert!(version_of("daughter") > Some(before), "the daughter's feed moved too");
    assert!(!answer.to_string().contains("daughter"), "and Kiri's answer does not say so: {answer}");
    // Syncing from just before that version brings her request.
    let changes = portal
        .local
        .with(|s| serde_json::to_value(s.data.feeds[&ic_core::PartyId::from(KIRI)].sync(Some(version - 1))).unwrap());
    assert!(changes.to_string().contains("request/r9"), "{changes}");
}

/// Purpose: the connection drops after the app submits and before the outcome: the request shows as pending, the app's retry with
/// the same idempotency key creates nothing new and says it is a repeat, and the outcome appears on the request when the owning
/// module reports it (AUD-008).
/// Why it matters: people on a train lose signal mid-change all the time; a retry that made a second cancellation or a second
/// payment change, or a screen stuck without the outcome, is exactly the failure the asynchronous contract exists to prevent.
#[tokio::test]
async fn a_retry_after_a_dropped_connection_is_one_request_and_the_outcome_follows() {
    let portal = portal();
    let keyed = [("x-dev-subject", "sub-kiri".to_string()), ("idempotency-key", "phone-1".to_string())];
    let request = json!({ "id": "r7", "policy": portal.retail, "kind": "decline_indexation", "module": "policy-servicing" });
    let (status, first) = portal.send("POST", "/app/v1/requests", &keyed, Some(request.clone())).await;
    assert_eq!((status, &first["duplicate"]), (StatusCode::ACCEPTED, &json!(false)));
    // The answer never reached the phone; it sends the same again.
    let (status, again) = portal.send("POST", "/app/v1/requests", &keyed, Some(request)).await;
    assert_eq!((status, &again["duplicate"]), (StatusCode::ACCEPTED, &json!(true)));
    let (_, mine) = portal.get("sub-kiri", "/app/v1/requests").await;
    let mine: Vec<&Value> = mine.as_array().unwrap().iter().filter(|r| r["id"] == "r7").collect();
    assert_eq!((mine.len(), &mine[0]["status"]), (1, &json!("pending")));

    // The owning module reports the outcome; the request shows it.
    let module = [
        ("x-dev-subject", "svc".to_string()),
        ("x-dev-roles", "policy-servicing".to_string()),
        ("x-module", "policy-servicing".to_string()),
        ("idempotency-key", "done-r7".to_string()),
    ];
    let resolve = json!({ "resolve_request": { "id": "r7", "reference": "chg-1", "rejected": null } });
    let (status, done) = portal.send("POST", "/internal/v1/commands", &module, Some(resolve)).await;
    assert_eq!(status, StatusCode::OK, "{done}");
    let (_, mine) = portal.get("sub-kiri", "/app/v1/requests").await;
    let r7 = mine.as_array().unwrap().iter().find(|r| r["id"] == "r7").unwrap().clone();
    assert_eq!((&r7["status"], &r7["reference"]), (&json!("applied"), &json!("chg-1")), "{r7}");
}

// ---- The MCP endpoint.

/// One JSON-RPC message to `/mcp` as the given caller.
async fn mcp(portal: &Portal, who: &[(&str, String)], message: Value) -> (StatusCode, Value) {
    portal.send("POST", "/mcp", who, Some(message)).await
}

fn as_subject(subject: &str) -> Vec<(&'static str, String)> {
    vec![("x-dev-subject", subject.to_string())]
}

/// Call one tool and hand back its result.
async fn call_tool(portal: &Portal, subject: &str, name: &str, arguments: Value) -> Value {
    let (status, reply) = mcp(
        portal,
        &as_subject(subject),
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": name, "arguments": arguments } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    reply["result"].clone()
}

/// Purpose: the MCP endpoint speaks the protocol: it negotiates a version it supports, answers ping, takes notifications without a
/// reply, refuses batches, unknown methods and text that is not JSON with JSON-RPC errors, offers no server stream on GET, refuses a
/// protocol version it does not speak and a browser origin other than its own, and asks anyone not signed in to sign in.
/// Why it matters: an MCP client that cannot complete the handshake cannot reach any function, and one that is told the wrong thing
/// about what went wrong cannot recover. Refusing other origins stops a web page elsewhere driving the endpoint through a browser.
#[tokio::test]
async fn the_mcp_endpoint_speaks_the_protocol() {
    let portal = portal();
    let kiri = as_subject("sub-kiri");
    let (status, reply) = mcp(&portal, &kiri, json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "1" } } })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(reply["result"]["capabilities"]["tools"]["listChanged"], false);
    assert_eq!(reply["result"]["serverInfo"]["name"], "insurance-core");
    let (_, newest) = mcp(
        &portal,
        &kiri,
        json!({ "jsonrpc": "2.0", "id": 2, "method": "initialize", "params": { "protocolVersion": "1999-01-01" } }),
    )
    .await;
    assert_eq!(newest["result"]["protocolVersion"], ic_channels::mcp::VERSIONS[0]);

    assert_eq!(
        mcp(&portal, &kiri, json!({ "jsonrpc": "2.0", "id": "p", "method": "ping" })).await.1,
        json!({ "jsonrpc": "2.0", "id": "p", "result": {} })
    );
    assert_eq!(
        mcp(&portal, &kiri, json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).await.0,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        mcp(&portal, &kiri, json!([{ "jsonrpc": "2.0", "id": 1, "method": "ping" }])).await.1["error"]["code"],
        -32600
    );
    assert_eq!(
        mcp(&portal, &kiri, json!({ "jsonrpc": "2.0", "id": 3, "method": "resources/list" })).await.1["error"]["code"],
        -32601
    );
    assert_eq!(mcp(&portal, &kiri, json!({ "jsonrpc": "2.0", "id": 4 })).await.1["error"]["code"], -32600);
    assert_eq!(portal.send("POST", "/mcp", &kiri, None).await.1["error"]["code"], -32700);
    assert_eq!(portal.send("GET", "/mcp", &kiri, None).await.0, StatusCode::METHOD_NOT_ALLOWED);

    let mut old = kiri.clone();
    old.push(("mcp-protocol-version", "2024-01-01".to_string()));
    assert_eq!(
        mcp(&portal, &old, json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" })).await.0,
        StatusCode::BAD_REQUEST
    );
    let mut current = kiri.clone();
    current.push(("mcp-protocol-version", "2025-06-18".to_string()));
    assert_eq!(
        mcp(&portal, &current, json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" })).await.0,
        StatusCode::OK
    );
    let mut elsewhere = kiri.clone();
    elsewhere.push(("origin", "https://evil.example".to_string()));
    assert_eq!(
        mcp(&portal, &elsewhere, json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" })).await.0,
        StatusCode::FORBIDDEN
    );
    let mut local = kiri.clone();
    local.push(("origin", "http://localhost:6274".to_string()));
    assert_eq!(
        mcp(&portal, &local, json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" })).await.0,
        StatusCode::OK
    );

    let (status, _) = mcp(&portal, &[], json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" })).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // A sign-in the insurer does not know is no one.
    assert_eq!(
        mcp(&portal, &as_subject("sub-nobody"), json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

/// Purpose: each caller is offered the tools of the sign-ins they hold and, within them, of what they are: a customer the app and not
/// the adviser, employer or staff tools; an adviser and an advice provider theirs; a scheme's administrator the employer tools; a member
/// of staff only the staff tools their templates' actions reach, never the customer app's; an operator the regions; every tool has a
/// valid name, a title, a description and an input schema whose required arguments it declares; the hints say an erasure is
/// destructive and a check read-only; and the operator's tool works for them.
/// Why it matters: a model shown tools it can never use wastes the person's time and invites it to try; one that is not shown a tool
/// the person may use cannot do that part of their job. A schema that does not match the call leaves the model guessing, and a hint
/// that calls an erasure harmless lets it act without asking. Every staff member was offered 116 tools and every customer the adviser
/// and employer ones (found by the scenario run, SCN-02, SCN-13, SCN-16 and SCN-23).
#[tokio::test]
async fn each_caller_is_offered_the_tools_of_their_sign_in() {
    let portal = portal();
    let list = |who: Vec<(&'static str, String)>| {
        let portal = &portal;
        async move {
            let (status, reply) = mcp(portal, &who, json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })).await;
            assert_eq!(status, StatusCode::OK, "{reply}");
            reply["result"]["tools"].as_array().unwrap().clone()
        }
    };
    let names = |tools: &[Value]| {
        tools
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect::<std::collections::BTreeSet<_>>()
    };
    let customer = list(as_subject("sub-kiri")).await;
    let staff = list(as_subject("sub-sue")).await;
    let ops = list(vec![("x-dev-subject", "ops-1".to_string()), ("x-dev-roles", OPERATIONS.to_string())]).await;
    let (customer, staff, ops) = (names(&customer), names(&staff), names(&ops));
    assert!(customer.contains("my_policy") && customer.contains("make_request") && customer.contains("my_proposals"));
    assert!(!customer.contains("staff_policy") && !customer.contains("finance_command") && !customer.contains("regions"));
    // Within a sign-in, only what the person's relationships reach: a customer is not an adviser or an employer.
    assert!(!customer.contains("adviser_book") && !customer.contains("employer_schemes") && !customer.contains("policy_claims"));
    let adviser = names(&list(as_subject("sub-adv")).await);
    assert!(adviser.contains("adviser_book") && adviser.contains("dispute_replacement") && adviser.contains("policy_claims"));
    assert!(!adviser.contains("employer_schemes") && !adviser.contains("scheme_command"));
    let provider = names(&list(as_subject("sub-fap")).await);
    assert!(provider.contains("adviser_practice"), "an advice provider is offered its practice");
    let employer = names(&list(as_subject("sub-hr")).await);
    assert!(employer.contains("employer_schemes") && employer.contains("scheme_command") && employer.contains("policy_claims"));
    assert!(!employer.contains("adviser_book"));
    // A member of staff is offered the staff tools their roles reach, and none of the customer app's.
    assert!(staff.contains("staff_policy") && staff.contains("find") && staff.contains("enter_change_for_customer") && staff.contains("staff_me"));
    assert!(!staff.contains("claims_command") && !staff.contains("finance_question") && !staff.contains("merge_customers"));
    assert!(!staff.contains("my_account") && !staff.contains("approve_request") && !staff.contains("regions"));
    let claims = names(&list(as_subject("sub-cl1")).await);
    assert!(claims.contains("claims_command") && claims.contains("claims_question") && !claims.contains("finance_command"));
    let records = names(&list(as_subject("sub-rec")).await);
    assert!(records.contains("staff_document") && !records.contains("staff_policy") && !records.contains("documents_command"));
    let refused = call_tool(&portal, "sub-sue", "my_account", json!({})).await;
    assert!(refused["content"][0]["text"].as_str().unwrap().contains("sign-in you do not hold"), "{refused}");
    // A reader is offered a module's questions and none of its writes; someone who acts there is offered both (F2-18-2).
    for (reader, question, writes) in [
        ("sub-payview", "payments_question", &["payments_command"][..]),
        ("sub-aud", "finance_question", &["finance_command", "rate_table_dry_run"][..]),
        ("sub-riview", "reinsurance_question", &["reinsurance_command", "reinsurance_decide"][..]),
        ("sub-dview", "commission_question", &["commission_command"][..]),
        ("sub-colview", "collections_question", &["collections_command"][..]),
        ("sub-docview", "documents_question", &["documents_command"][..]),
        ("sub-grpview", "group_question", &["group_command", "group_reoffer", "first_scheme_admin"][..]),
    ] {
        let offered = names(&list(as_subject(reader)).await);
        assert!(offered.contains(question), "{reader}: {offered:?}");
        for write in writes {
            assert!(!offered.contains(*write), "{reader} is offered {write}");
        }
    }
    for (actor, write) in [
        ("sub-paymk", "payments_command"),
        ("sub-ri1", "reinsurance_command"),
        ("sub-dadmin", "commission_command"),
        ("sub-colof", "collections_command"),
        ("sub-docla", "documents_command"),
        ("sub-grp1", "group_command"),
    ] {
        assert!(names(&list(as_subject(actor)).await).contains(write), "{actor} is not offered {write}");
    }
    // Called anyway, a reader's write is refused for the role before anything about what was sent.
    let (status, problem) = portal
        .write("POST", "sub-dview", "/staff/v1/commission/commands", json!({ "command": {} }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{problem}");
    assert!(problem["detail"].as_str().unwrap().contains("distribution administrator"), "{problem}");
    let (status, problem) = portal
        .write("POST", "sub-riview", "/staff/v1/reinsurance/sign/nothing-waiting", json!({}))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{problem}");
    let annotations = |name: &str| {
        ic_channels::mcp::catalogue()
            .into_iter()
            .find(|t| t.name == name)
            .map(|t| t.listed()["annotations"].clone())
            .unwrap()
    };
    assert_eq!(annotations("reshred_customers")["destructiveHint"], true, "an erasure is destructive");
    assert_eq!(annotations("merge_customers")["destructiveHint"], true);
    assert_eq!(annotations("verify_customer")["readOnlyHint"], true, "a check changes nothing");
    assert_eq!(annotations("verify_customer")["idempotentHint"], true);
    assert_eq!(annotations("quote")["readOnlyHint"], true);
    assert_eq!(annotations("my_account")["destructiveHint"], false);
    assert_eq!(
        ops,
        [
            "customer_index",
            "reconciliation",
            "regions",
            "reshred_customers",
            "rewrap_customers",
            "status",
            "verify_customer"
        ]
        .map(String::from)
        .into()
    );

    let every = ic_channels::mcp::catalogue();
    assert_eq!(
        names(&every.iter().map(|t| t.listed()).collect::<Vec<_>>()).len(),
        every.len(),
        "two tools share a name"
    );
    for tool in every.iter().map(|t| t.listed()) {
        let name = tool["name"].as_str().unwrap();
        assert!(name.len() <= 64 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'), "{name}");
        assert!(
            !tool["title"].as_str().unwrap().is_empty() && tool["description"].as_str().unwrap().len() > 20,
            "{name}"
        );
        let schema = &tool["inputSchema"];
        assert_eq!(schema["type"], "object");
        for required in schema["required"].as_array().unwrap() {
            assert!(
                schema["properties"].get(required.as_str().unwrap()).is_some(),
                "{name} requires {required} it does not describe"
            );
        }
        assert_eq!(
            tool["annotations"]["readOnlyHint"],
            tool["inputSchema"]["properties"].get("idempotency_key").is_none(),
            "{name}"
        );
    }
    let counts = ic_channels::mcp::counts();
    assert_eq!(counts.values().sum::<usize>(), every.len());

    let ops = vec![("x-dev-subject", "ops-1".to_string()), ("x-dev-roles", OPERATIONS.to_string())];
    let (_, reply) = mcp(
        &portal,
        &ops,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "regions", "arguments": {} } }),
    )
    .await;
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    assert!(
        reply["result"]["structuredContent"]["regions"].is_object() || reply["result"]["structuredContent"]["regions"].is_array(),
        "{reply}"
    );
}

/// Purpose: a tool call goes through the same route and checks as the screens: the customer reads their own policy and gets the
/// route's own refusal for someone else's; a staff tool is refused to a customer before anything is asked; staff opening a policy
/// without a case are refused by the access rules and allowed with one; a write is done once for one idempotency key; and
/// arguments that are missing, unknown, outside a fixed set or the wrong shape are refused before any route is called.
/// Why it matters: the MCP user must be able to do exactly what they could do on the screens, no more and no less. A second path with
/// its own checks would sooner or later let someone do what the screens refuse.
#[tokio::test]
async fn a_tool_call_passes_the_same_checks_as_the_screens() {
    let portal = portal();
    let policy = portal.retail.to_string();
    let mine = call_tool(&portal, "sub-kiri", "my_policy", json!({ "policy": policy })).await;
    assert_eq!(mine["isError"], false, "{mine}");
    assert_eq!(mine["structuredContent"]["policy"], policy.as_str());
    assert!(mine["content"][0]["text"].as_str().unwrap().contains(&policy));
    let other = call_tool(&portal, "sub-adv", "my_policy", json!({ "policy": policy })).await;
    assert_eq!(other["isError"], true);
    assert!(other["content"][0]["text"].as_str().unwrap().starts_with("Refused (404"), "{other}");

    let staff_only = call_tool(&portal, "sub-kiri", "staff_policy", json!({ "policy": policy })).await;
    assert_eq!(staff_only["isError"], true);
    assert!(staff_only["content"][0]["text"].as_str().unwrap().contains("sign-in you do not hold"));
    let no_case = call_tool(&portal, "sub-sue", "staff_policy", json!({ "policy": policy })).await;
    assert_eq!(no_case["isError"], true, "{no_case}");
    assert!(no_case["content"][0]["text"].as_str().unwrap().contains("403"), "{no_case}");
    let with_case = call_tool(&portal, "sub-sue", "staff_policy", json!({ "policy": policy, "query": { "case": "case-mcp" } })).await;
    assert_eq!(with_case["isError"], false, "{with_case}");
    // The opening is in the person's own access log, as on the screens.
    let me = call_tool(&portal, "sub-sue", "staff_me", json!({})).await;
    assert!(
        me["structuredContent"]["recent"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["purpose"]["reference"] == "case-mcp"),
        "{me}"
    );

    // A write, retried with the same key, is one request.
    let request = json!({ "id": "r-mcp", "policy": policy, "kind": "decline_indexation", "module": "policy-servicing", "payload": { "declined": true } });
    let first = call_tool(&portal, "sub-kiri", "make_request", json!({ "body": request, "idempotency_key": "once" })).await;
    assert_eq!(
        (first["isError"].as_bool(), first["structuredContent"]["duplicate"].as_bool()),
        (Some(false), Some(false)),
        "{first}"
    );
    let again = call_tool(&portal, "sub-kiri", "make_request", json!({ "body": request, "idempotency_key": "once" })).await;
    assert_eq!(again["structuredContent"]["duplicate"], true, "{again}");
    let listed = call_tool(&portal, "sub-kiri", "my_requests", json!({})).await;
    assert_eq!(listed["structuredContent"]["result"].as_array().unwrap().len(), 1, "{listed}");
    // Money is refused without the phone's signature, as it is to the app.
    let pay = json!({ "id": "p-mcp", "policy": policy, "kind": "make_payment", "module": "payments", "payload": { "amount_minor": 100 } });
    let unsigned = call_tool(&portal, "sub-kiri", "make_request", json!({ "body": pay })).await;
    assert!(unsigned["content"][0]["text"].as_str().unwrap().contains("step_up_required"), "{unsigned}");

    for (name, arguments, said) in [
        ("my_policy", json!({}), "policy is required"),
        ("my_policy", json!({ "policy": policy, "colour": "red" }), "colour is not an argument"),
        ("my_policy", json!({ "policy": policy, "query": { "a": "b" } }), "takes no query"),
        ("claims_question", json!({ "question": "everything" }), "question is one of"),
        ("make_request", json!({}), "body is required"),
        ("make_request", json!({ "body": {}, "idempotency_key": "" }), "idempotency_key"),
        ("my_policy", json!("policy"), "arguments are an object"),
        (
            "staff_policy",
            json!({ "policy": policy, "query": { "case": ["x"] } }),
            "query case is a string",
        ),
    ] {
        let subject = if name.starts_with("staff") {
            "sub-sue"
        } else if name.starts_with("claims") {
            "sub-cl1"
        } else {
            "sub-kiri"
        };
        let refused = call_tool(&portal, subject, name, arguments).await;
        assert_eq!(refused["isError"], true, "{name}");
        assert!(refused["content"][0]["text"].as_str().unwrap().contains(said), "{name}: {refused}");
    }
    let (_, unknown) = mcp(
        &portal,
        &as_subject("sub-kiri"),
        json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": { "name": "drop_tables" } }),
    )
    .await;
    assert_eq!(unknown["error"]["code"], -32602);
    // A value cannot steer the call to another route.
    let steered = call_tool(&portal, "sub-kiri", "my_policy", json!({ "policy": "../../staff/v1/me" })).await;
    assert_eq!(steered["isError"], true);
    assert!(steered["content"][0]["text"].as_str().unwrap().contains("404"), "{steered}");
}

/// Purpose: a document comes back whole as an embedded resource with its type, a query value and a number are carried, and a file
/// for upload is decoded from base64 and refused when it is not.
/// Why it matters: a PDF squeezed into text is useless and a file that arrives altered is worse; the person must get exactly the bytes
/// the documents module gave.
#[tokio::test]
async fn files_go_both_ways_whole() {
    let portal = portal();
    let pdf = call_tool(&portal, "sub-kiri", "my_document", json!({ "id": "doc-1" })).await;
    assert_eq!(pdf["isError"], false, "{pdf}");
    let resource = &pdf["content"][0]["resource"];
    assert_eq!(resource["mimeType"], "application/pdf");
    let bytes = base64::engine::general_purpose::STANDARD.decode(resource["blob"].as_str().unwrap()).unwrap();
    assert!(bytes.starts_with(b"%PDF"), "{:?}", &bytes[..8.min(bytes.len())]);
    let staff = call_tool(
        &portal,
        "sub-rec",
        "staff_document",
        json!({ "id": "doc-1", "query": { "purpose": "complaint C-1" } }),
    )
    .await;
    assert_eq!(staff["isError"], false, "{staff}");

    let finance = call_tool(
        &portal,
        "sub-gllead",
        "finance_question",
        json!({ "question": "extract", "query": { "kind": "in_force", "period": "2027-11", "limit": 5 } }),
    )
    .await;
    assert!(finance["content"][0]["text"].is_string());
    let bad = call_tool(
        &portal,
        "sub-hr",
        "upload_membership_file",
        json!({ "policy": portal.scheme.to_string(), "file": "f1", "content_base64": "%%%" }),
    )
    .await;
    assert!(bad["content"][0]["text"].as_str().unwrap().contains("not base64"), "{bad}");
    let csv = base64::engine::general_purpose::STANDARD.encode("employee,name\nE1,Ana\n");
    let uploaded = call_tool(
        &portal,
        "sub-hr",
        "upload_membership_file",
        json!({ "policy": portal.scheme.to_string(), "file": "f1", "content_base64": csv }),
    )
    .await;
    assert!(uploaded["content"][0]["text"].is_string(), "{uploaded}");

    // A route that answers with nothing, and one that answers with text, read as such.
    use ic_channels::mcp::{catalogue, result};
    let tool = catalogue().into_iter().find(|t| t.name == "status").unwrap();
    assert_eq!(result(&tool, "/x", StatusCode::NO_CONTENT, "", b"")["isError"], false);
    assert_eq!(result(&tool, "/x", StatusCode::OK, "text/plain", b"ok")["content"][0]["text"], "ok");
    assert_eq!(
        result(&tool, "/x", StatusCode::BAD_GATEWAY, "text/plain", b"down")["content"][0]["text"],
        "Refused (502): down"
    );
    assert_eq!(
        result(&tool, "/x", StatusCode::OK, "application/json", b"[1]")["structuredContent"],
        json!({ "result": [1] })
    );
}

/// Purpose: every route the portal serves is either a tool or deliberately left out with a reason, and every tool names a route
/// that exists with its method.
/// Why it matters: Mitchell asked that every function be usable through MCP. A route added to the portal without a tool would be a
/// function the MCP user silently cannot reach; a tool for a route that is gone would fail every time it is called.
#[test]
fn every_route_is_a_tool_or_left_out_for_a_reason() {
    let source = include_str!("../src/http.rs");
    let start = source.find("pub fn router(").unwrap();
    let routes_text = &source[start..];
    let mut served = std::collections::BTreeSet::new();
    for chunk in routes_text.split(".route(").skip(1) {
        let path = chunk.split('"').nth(1).unwrap().to_string();
        let end = chunk.find("\n        .").unwrap_or(chunk.len());
        let handlers = &chunk[..end];
        for (method, mark) in [("GET", "get("), ("POST", "post("), ("PUT", "put("), ("DELETE", "delete(")] {
            if handlers.contains(&format!("({mark}")) || handlers.contains(&format!(" {mark}")) || handlers.contains(&format!(".{mark}")) {
                served.insert((method.to_string(), path.clone()));
            }
        }
    }
    assert!(served.len() > 90, "the route list was not read: {served:?}");
    let tools: std::collections::BTreeSet<(String, String)> = ic_channels::mcp::catalogue()
        .iter()
        .map(|t| (t.method.to_string(), t.path.to_string()))
        .collect();
    let left_out = |method: &str, path: &str| {
        ic_channels::mcp::EXCLUDED.iter().any(|(m, p, _)| {
            (*m == method || *m == "*")
                && (*p == path
                    || p.strip_suffix('*').is_some_and(|prefix| path.starts_with(prefix))
                    || p.split(", ")
                        .any(|q| q == path || q.strip_suffix('*').is_some_and(|prefix| path.starts_with(prefix))))
        })
    };
    let missing: Vec<_> = served
        .iter()
        .filter(|(m, p)| !tools.contains(&(m.clone(), p.clone())) && !left_out(m, p))
        .collect();
    assert!(missing.is_empty(), "routes with no tool and no reason: {missing:?}");
    let stale: Vec<_> = tools.iter().filter(|t| !served.contains(*t)).collect();
    assert!(stale.is_empty(), "tools for routes that are not served: {stale:?}");
}

/// Purpose: a refusal passed on from another service reaches the person in its own words: the customer master's `{"error": {code,
/// message}}` gains the `title` and `detail` the pages and assistants read and keeps its own body; the group service's refusal of an
/// employer likewise, with plain words where it gave none; a sign-in with no customer record is told what to do; and an MCP result
/// reads either shape and never shows an empty reason.
/// Why it matters: customers and employers saw "The request failed (422)." and assistants "Refused (422 ): " while the reason was in
/// the body all along (found by the scenario run, SCN-05, SCN-07, SCN-08 and SCN-10).
#[tokio::test]
async fn a_refusal_from_another_service_is_said_in_its_own_words() {
    let portal = portal();
    let body = json!({ "command": "request_hold_removal" }).to_string();
    let headers = portal.proof(&portal.phone, "POST", "/app/v1/profile/commands", &body);
    let (status, refused) = portal
        .send("POST", "/app/v1/profile/commands", &headers, Some(serde_json::from_str(&body).unwrap()))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert_eq!(refused["title"], "verification_needed");
    assert!(refused["detail"].as_str().unwrap().contains("identity check"), "{refused}");
    assert_eq!(
        refused["error"]["code"], "verification_needed",
        "the service's own body is kept for callers that read it"
    );

    // The group service gave a code and no words: the employer is told it was refused, with the code.
    let scheme = portal.scheme.to_string();
    let (status, refused) = portal
        .write(
            "POST",
            "sub-hr",
            &format!("/employer/v1/schemes/{scheme}/group/commands"),
            json!({ "approve_file": { "file": "never-sent.csv" } }),
        )
        .await;
    assert_eq!(
        (status, &refused["title"]),
        (StatusCode::UNPROCESSABLE_ENTITY, &json!("unknown_file")),
        "{refused}"
    );
    assert_eq!(refused["detail"], "the group service refused this without saying why (422)");
    let tool = call_tool(
        &portal,
        "sub-hr",
        "scheme_command",
        json!({ "policy": scheme, "body": { "approve_file": { "file": "never-sent.csv" } } }),
    )
    .await;
    assert_eq!(
        tool["content"][0]["text"],
        "Refused (422 unknown_file): the group service refused this without saying why (422)"
    );

    // Someone signed in with no customer record is told so, and what to do, on the page and over MCP.
    let (status, problem) = portal.get("sub-adv", "/app/v1/profile").await;
    assert_eq!((status, &problem["title"]), (StatusCode::NOT_FOUND, &json!("no_customer_record")), "{problem}");
    let tool = call_tool(&portal, "sub-adv", "my_profile", json!({})).await;
    assert!(tool["content"][0]["text"].as_str().unwrap().contains("call us"), "{tool}");

    // An answer in another service's own shape, and one with nothing in it, read as words.
    let my_policy = ic_channels::mcp::catalogue().into_iter().find(|t| t.name == "my_policy").unwrap();
    let said = |status: StatusCode, body: &str| {
        ic_channels::mcp::result(&my_policy, "/p", status, "application/json", body.as_bytes())["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(
        said(
            StatusCode::CONFLICT,
            r#"{"error":{"code":"duplicate_file","message":"the same content was received as file a.csv"}}"#
        ),
        "Refused (409 duplicate_file): the same content was received as file a.csv"
    );
    assert_eq!(said(StatusCode::NOT_FOUND, "{}"), "Refused (404): the service refused this without saying why");
    assert_eq!(said(StatusCode::CONFLICT, r#"{"error":"frozen"}"#), "Refused (409 frozen): frozen");
}

/// Purpose: every page is served with the books' business date in a meta tag that the shared script reads for `portal.today()`, and
/// with none while the core has reported no day; the page's own script still runs under its policy.
/// Why it matters: date fields, references and default periods came from the browser's clock, a year away from the books, so a run
/// was dated before the policies it paid and a period was offered that the books had not reached (found by the scenario run, SCN-01,
/// SCN-15, SCN-17 and SCN-20).
#[tokio::test]
async fn pages_are_told_the_books_business_date() {
    let portal = portal();
    let page = |path: &'static str| {
        let app = portal.app.clone();
        async move {
            let response = app.oneshot(Request::builder().uri(path).body(Body::empty()).unwrap()).await.unwrap();
            let csp = response.headers()["content-security-policy"].to_str().unwrap().to_string();
            let body = String::from_utf8(response.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
            (csp, body)
        }
    };
    let today = portal.local.with(|state| state.data.today).expect("the seeded books have a business date");
    let (csp, body) = page("/finance").await;
    assert!(
        body.contains(&format!("<head><meta name=\"business-date\" content=\"{today}\">")),
        "{}",
        &body[..300]
    );
    assert_eq!(csp.matches("'sha256-").count(), 1, "the page's own script is still the one the policy names");
    assert!(ic_channels::ui::SCRIPT.contains(r#"meta[name="business-date"]"#), "the shared script reads it");
    portal.local.with(|state| state.data.today = None);
    let (_, body) = page("/finance").await;
    assert!(!body.contains("business-date"), "no day reported, no date claimed");
}

/// Purpose: what a customer approves and asks for is said in words with who prepared it: a servicing request's row carries its
/// choices, who prepared it and whether it was declined; once the customer approves or declines a change someone else prepared, it
/// stays in their own requests; each request names its policy's number; and the adviser who prepared one sees it declined, not
/// merely cancelled, while one they withdrew themselves is not.
/// Why it matters: customers approved "change frequency" without being told to what, then saw nothing of what they had approved,
/// and advisers read a declined change as a cancelled policy (found by the scenario run, SCN-01 and SCN-13).
#[tokio::test]
async fn a_change_is_shown_in_words_to_whoever_approves_it_and_stays_in_view() {
    let portal = portal();
    let policy = portal.retail.to_string();
    let appoint = json!({ "adviser": "adv-1", "engagement": "e1" });
    assert_eq!(
        portal
            .write("POST", "sub-kiri", &format!("/app/v1/policies/{policy}/advisers"), appoint)
            .await
            .0,
        StatusCode::OK
    );
    for (id, kind, payload) in [
        ("w1", "change_frequency", json!({ "frequency": "fortnightly", "note": "cheaper for you" })),
        ("w2", "decline_indexation", json!({ "declined": true })),
    ] {
        let request = json!({ "id": id, "policy": policy, "kind": kind, "module": "policy-servicing", "payload": payload });
        assert_eq!(portal.write("POST", "sub-adv", "/app/v1/requests", request).await.0, StatusCode::ACCEPTED);
    }
    let (_, view) = portal.get("sub-kiri", &format!("/app/v1/policies/{policy}")).await;
    let row = |id: &str| view["rows"].as_array().unwrap().iter().find(|r| r["body"]["id"] == id).unwrap()["body"].clone();
    assert_eq!(row("w1")["payload"]["frequency"], "fortnightly", "{view}");
    assert_eq!((&row("w1")["prepared_by"], &row("w1")["declined"]), (&json!("adviser"), &json!(false)));

    // Kiri approves the first, signed, and declines the second: both stay in her own list, and the adviser sees which was which.
    let ask = json!({ "action": approval_action("w1") }).to_string();
    let headers = portal.proof(&portal.phone, "POST", "/app/v1/challenges", &ask);
    let (_, challenge) = portal
        .send("POST", "/app/v1/challenges", &headers, Some(serde_json::from_str(&ask).unwrap()))
        .await;
    let message = format!(
        "{}\n{}\n{}",
        challenge["challenge"].as_str().unwrap(),
        challenge["nonce"].as_str().unwrap(),
        challenge["action_hash"].as_str().unwrap()
    );
    let signed = json!({ "signed": { "challenge": challenge["challenge"], "signature": portal.phone.sign(message.as_bytes()) } });
    assert_eq!(
        portal.write("POST", "sub-kiri", "/app/v1/requests/w1/approve", signed).await.0,
        StatusCode::ACCEPTED
    );
    assert_eq!(portal.write("DELETE", "sub-kiri", "/app/v1/requests/w2", json!(null)).await.0, StatusCode::OK);
    let (_, mine) = portal.get("sub-kiri", "/app/v1/requests").await;
    let find = |list: &Value, id: &str| list.as_array().unwrap().iter().find(|r| r["id"] == id).cloned().unwrap_or(Value::Null);
    assert_eq!(find(&mine, "w1")["approved_by"], KIRI, "{mine}");
    assert_eq!(find(&mine, "w2")["declined_by"], KIRI, "{mine}");
    let number = portal.local.with(|state| state.data.view.policy(&portal.retail).unwrap().number.clone());
    assert_eq!(find(&mine, "w1")["number"], number.as_str());
    let (_, theirs) = portal.get("sub-adv", "/app/v1/requests").await;
    assert_eq!(
        (&find(&theirs, "w2")["status"], &find(&theirs, "w2")["declined_by"]),
        (&json!("cancelled"), &json!(KIRI))
    );
    // Withdrawn by the one who asked is not a decline.
    let request = json!({ "id": "w3", "policy": policy, "kind": "decline_indexation", "module": "policy-servicing", "payload": {} });
    portal.write("POST", "sub-adv", "/app/v1/requests", request).await;
    assert_eq!(portal.write("DELETE", "sub-adv", "/app/v1/requests/w3", json!(null)).await.0, StatusCode::OK);
    let (_, theirs) = portal.get("sub-adv", "/app/v1/requests").await;
    assert_eq!(find(&theirs, "w3")["declined_by"], Value::Null);
    assert_eq!(
        find(&portal.get("sub-kiri", "/app/v1/requests").await.1, "w3"),
        Value::Null,
        "nothing of hers to show"
    );
}

/// Purpose: staff open a policy, and check a caller on it, by the number the customer reads out in any case, and the access log
/// names the policy by its reference; a search finds a policy by the end of its number; and a number search lists a policy closed
/// to the searcher (their own, or a declared connection's) only as closed.
/// Why it matters: a typed number opened nothing while the last policy stayed on screen, so a caller check was recorded on the wrong
/// policy; callers read out the last digits; and a connection's policy must not be shown to the person connected (found by the
/// scenario run, SCN-13).
#[tokio::test]
async fn staff_reach_a_policy_by_the_number_the_caller_reads_out() {
    let portal = portal();
    let number = portal.local.with(|state| state.data.view.policy(&portal.retail).unwrap().number.clone());
    let (status, view) = portal
        .get("sub-sue", &format!("/staff/v1/policies/{}?case=CALL-9", number.to_lowercase()))
        .await;
    assert_eq!((status, &view["policy"]), (StatusCode::OK, &json!(portal.retail.0)), "{view}");
    let (status, checked) = portal
        .write(
            "POST",
            "sub-sue",
            &format!("/staff/v1/policies/{number}/caller"),
            json!({ "caller": KIRI, "case": "CALL-9" }),
        )
        .await;
    assert_eq!((status, &checked["outcome"]), (StatusCode::OK, &json!("discuss")), "{checked}");
    let (_, me) = portal.get("sub-sue", "/staff/v1/me").await;
    let caller = format!("policy:{}/caller", portal.retail.0);
    assert!(
        me["recent"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["record"].as_str().is_some_and(|r| r.starts_with(&caller))),
        "the contact is logged against the policy's reference: {me}"
    );

    let find = |q: String| {
        let portal = &portal;
        async move { portal.get("sub-sue", &format!("/staff/v1/find?q={q}&case=CALL-9")).await.1 }
    };
    let ending: String = number.chars().rev().take(3).collect::<Vec<_>>().into_iter().rev().collect();
    let found = find(ending.clone()).await;
    assert!(
        found["policies"].as_array().unwrap().iter().any(|p| p["number"] == number.as_str()),
        "{ending}: {found}"
    );
    assert!(ic_channels::find::numbered(&ic_readmodel::Projections::default(), "ab").is_empty());

    // Sue declares she knows Kiri: a search by the number says the policy is closed, and gives nothing of it.
    portal.local.with(|state| {
        state.data.staff.connections.entry(p("sue")).or_default().insert(p(KIRI));
    });
    let found = find(number.clone()).await;
    assert_eq!(found["policies"], json!([{ "closed": true }]), "{found}");
}

/// Purpose: a one-off grant and a break-glass grant widen the staff tools a person is offered as they widen what the routes let them
/// do, and a profile uplift (which gives no action) does not.
/// Why it matters: a person given an action for one task must be able to see the tool for it, and one who holds none must not be
/// shown tools that can only refuse them (found by the scenario run, SCN-13 and SCN-16).
#[tokio::test]
async fn grants_widen_the_tools_offered() {
    use ic_channels::staff::{Grant, GrantKind, GrantState, Source};
    let portal = portal();
    let offered = |subject: &'static str| {
        let portal = &portal;
        async move {
            let (_, reply) = mcp(portal, &as_subject(subject), json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })).await;
            reply["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["name"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        }
    };
    assert!(!offered("sub-sue").await.contains(&"claims_command".to_string()));
    let now = portal.now();
    let grant = |id: &str, kind: GrantKind| Grant {
        id: id.into(),
        holder: p("sue"),
        kind,
        reason: "covering the claims desk".into(),
        requested_by: p("sue"),
        requested_at: now - 10,
        approver: Some(p("alice")),
        from: now - 10,
        to: now + 3600,
        state: GrantState::Active,
        source: Source::BuiltIn,
        reviewed: None,
    };
    portal.local.with(|state| {
        let g = grant(
            "g-one",
            GrantKind::OneOff {
                action: "customers.export".into(),
                record: "party:x".into(),
            },
        );
        state.data.staff.grants.insert(g.id.clone(), g);
        let g = grant("g-glass", GrantKind::BreakGlass { template: "claims".into() });
        state.data.staff.grants.insert(g.id.clone(), g);
        let g = grant("g-up", GrantKind::ProfileUplift { limits: Default::default() });
        state.data.staff.grants.insert(g.id.clone(), g);
    });
    let now_offered = offered("sub-sue").await;
    assert!(now_offered.contains(&"customer_export".to_string()), "{now_offered:?}");
    assert!(now_offered.contains(&"claims_command".to_string()), "{now_offered:?}");
    assert!(!now_offered.contains(&"finance_command".to_string()));
}

/// Purpose: an assistant is told each payments and reinsurance question's fields from the module's own list, the payments commands
/// by name, the employer's scheme commands in the shape the route takes, that money is in cents, and what a missing body should
/// hold; a misspelt payments command is told the commands there are.
/// Why it matters: with `{...}` for a body and half the fields undescribed, an assistant guessed every field, sent commands in the
/// wrong shape and read $1,650.50 as $165,050 (found by the scenario run, SCN-07, SCN-10, SCN-16 and SCN-22).
#[tokio::test]
async fn an_assistant_is_told_the_fields_the_commands_and_the_units() {
    let portal = portal();
    let listed = |name: &str| ic_channels::mcp::catalogue().into_iter().find(|t| t.name == name).unwrap().listed();
    let query = |name: &str| listed(name)["inputSchema"]["properties"]["query"]["description"].as_str().unwrap().to_string();
    let body = |name: &str| listed(name)["inputSchema"]["properties"]["body"]["description"].as_str().unwrap().to_string();
    let payments = query("payments_question");
    assert!(
        payments.contains("control_totals: `run`") && payments.contains("funding_forecast: `from`, `to`"),
        "{payments}"
    );
    let reinsurance = query("reinsurance_question");
    assert!(
        reinsurance.contains("exposure: `on`") && reinsurance.contains("balance: `treaty`, `reinsurer`"),
        "{reinsurance}"
    );
    assert!(body("payments_command").contains("approve_run"));
    assert!(listed("payments_question")["description"].as_str().unwrap().contains("cents"));
    assert!(body("scheme_command").contains(r#"{"declare_pool": {"pool""#));
    let (_, hello) = mcp(&portal, &as_subject("sub-kiri"), json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" })).await;
    assert!(hello["result"]["instructions"].as_str().unwrap().contains("cents"), "{hello}");
    let missing = call_tool(&portal, "sub-kiri", "approve_request", json!({ "id": "r-1" })).await;
    assert!(
        missing["content"][0]["text"].as_str().unwrap().contains(r#"body is required: {"signed""#),
        "{missing}"
    );

    let (status, refused) = portal
        .write("POST", "sub-paymk", "/staff/v1/payments/commands", json!({ "type": "aprove_run" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(refused["detail"].as_str().unwrap().contains("approve_run"), "{refused}");
}

/// Purpose: a signature over a challenge the device was never given is refused as a signature that did not check out, and a step that
/// needs the device says where to do it.
/// Why it matters: an assistant told "that is not yours to do" took a forgery for a missing permission, and one told only that a key
/// was needed could not tell the person what to do (found by the scenario run, SCN-10).
#[tokio::test]
async fn a_device_refusal_says_what_to_do() {
    let portal = portal();
    let (status, problem) = portal.get("sub-kiri", "/app/v1/profile/export").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(problem["detail"].as_str().unwrap().contains("set up this browser under Signing"), "{problem}");
    let appoint = json!({ "adviser": "adv-1", "engagement": "e1" });
    portal
        .write("POST", "sub-kiri", &format!("/app/v1/policies/{}/advisers", portal.retail), appoint)
        .await;
    let request = json!({ "id": "f1", "policy": portal.retail, "kind": "decline_indexation", "module": "policy-servicing", "payload": {} });
    assert_eq!(portal.write("POST", "sub-adv", "/app/v1/requests", request).await.0, StatusCode::ACCEPTED);
    let forged = json!({ "signed": { "challenge": "never-issued", "signature": "AAAA" } });
    let (status, problem) = portal.write("POST", "sub-kiri", "/app/v1/requests/f1/approve", forged).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{problem}");
    assert!(problem["detail"].as_str().unwrap().contains("sign it again on your phone"), "{problem}");
}

/// Purpose: with Entra sign-in the endpoint publishes its protected resource metadata, naming itself and both tenants' issuers and
/// scopes, and a caller without a token is pointed to it; on a laptop there is none.
/// Why it matters: an MCP client discovers where to sign the person in from this document. Without it no client can sign in; with
/// the wrong issuer a staff member would be sent to the customers' tenant.
#[tokio::test]
async fn the_mcp_endpoint_says_where_to_sign_in() {
    let portal = portal();
    assert_eq!(
        portal.send("GET", "/.well-known/oauth-protected-resource/mcp", &[], None).await.0,
        StatusCode::NOT_FOUND
    );
    let sign_in = |authority: &str, scope: &str| SignIn {
        authority: authority.into(),
        tenant: "t1".into(),
        client_id: "spa".into(),
        scope: scope.into(),
    };
    let now = portal.now();
    let app = router(Arc::new(AppState {
        wordings: Arc::default(),
        public_url: Some("https://portal.example.co.nz".into()),
        returns: Arc::default(),
        channel: portal.local.clone(),
        customers: Auth::entra("cust-tenant", "api://app"),
        workforce: Auth::entra("staff-tenant", "api://portal"),
        clock: Arc::new(move || now),
        regions: portal.regions.clone(),
        ui: UiConfig::Entra {
            customers: sign_in("https://contoso.ciamlogin.com", "api://app/portal"),
            workforce: sign_in("https://login.microsoftonline.com", "api://portal/.default"),
        },
        pricing: portal.pricing.clone(),
        complaints: Arc::new(common::pricing::Down),
        commission: Arc::new(common::commission::seeded()),
        finance: Arc::new(common::pricing::Down),
        claims: portal.claims.clone(),
        reinsurance: portal.reinsurance.clone(),
        payments: portal.payments.clone(),
        collections: portal.collections.clone(),
        correspondence: portal.correspondence.clone(),
        wellness: portal.wellness.clone(),
        underwriting: portal.underwriting.clone(),
        master: None,
        intake: None,
        documents: None,
        group: None,
    }));
    let get = |path: &'static str| {
        let app = app.clone();
        async move {
            let response = app.oneshot(Request::builder().uri(path).body(Body::empty()).unwrap()).await.unwrap();
            let status = response.status();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            (status, serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null))
        }
    };
    let (status, meta) = get("/.well-known/oauth-protected-resource/mcp").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(meta["resource"], "https://portal.example.co.nz/mcp");
    assert_eq!(
        meta["authorization_servers"],
        json!(["https://login.microsoftonline.com/t1/v2.0", "https://contoso.ciamlogin.com/t1/v2.0"])
    );
    assert_eq!(meta["scopes_supported"], json!(["api://portal/.default", "api://app/portal"]));
    assert_eq!(get("/.well-known/oauth-protected-resource").await.1, meta);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.headers()["www-authenticate"],
        "Bearer resource_metadata=\"https://portal.example.co.nz/.well-known/oauth-protected-resource/mcp\""
    );
    // Its own origin may call from a browser; no other may.
    let from = |origin: &'static str| {
        let app = app.clone();
        async move {
            app.oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header("origin", origin)
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
        }
    };
    assert_eq!(from("https://portal.example.co.nz").await, StatusCode::UNAUTHORIZED);
    assert_eq!(from("http://localhost:6274").await, StatusCode::FORBIDDEN);
}

/// An application for a test of new business: a term life policy the
/// portal has not seen, under `product`.
fn application(n: u64, product: &str) -> Value {
    let mut app = serde_json::to_value(term_life(
        n,
        open(),
        d(1985, 6, 1),
        Sex::Male,
        false,
        250_000_00,
        None,
        Frequency::Monthly,
        None,
    ))
    .unwrap();
    app["product_code"] = json!(product);
    app
}

/// The access log lines a member of staff has written, as (action, record).
async fn logged(portal: &Portal, subject: &str) -> Vec<(String, String, bool)> {
    let me = portal.get(subject, "/staff/v1/me").await.1;
    me["recent"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|l| {
            (
                l["action"].as_str().unwrap_or_default().to_string(),
                l["record"].as_str().unwrap_or_default().to_string(),
                l["allowed"].as_bool().or_else(|| l["decision"]["allowed"].as_bool()).unwrap_or(false),
            )
        })
        .collect()
}

/// Purpose: a member of new business staff prices an application through the portal; the core's refusal and a closed date come back as such, and there is no route to issue a policy by hand.
/// Why it matters: pricing had no screen and no tool. Issuing must not be offered here: the core trusts its new business caller to send only underwritten terms, so a hand-typed application could put cover in force with no disclosure assessed.
#[tokio::test]
async fn new_business_quotes_but_never_issues_by_hand() {
    let portal = portal();
    let quote = "/staff/v1/new-business/quote?case=nb-77";
    let (status, priced) = portal.write("POST", "sub-nb1", quote, application(71, "term-life")).await;
    assert_eq!(status, StatusCode::OK, "{priced}");
    assert_eq!((&priced["total_minor"], &priced["total"]), (&json!(1234), &json!("$12.34")), "{priced}");
    assert_eq!(priced["frequency"], "monthly");
    // The core's refusal is its own; a date not yet open is a wait, not a failure.
    let refused = portal.write("POST", "sub-nb1", quote, application(72, "none")).await;
    assert_eq!(
        (refused.0, refused.1["title"].as_str()),
        (StatusCode::CONFLICT, Some("unknown_product")),
        "{}",
        refused.1
    );
    assert_eq!(
        portal.write("POST", "sub-nb1", quote, application(73, "later")).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    // Something that is not an application is refused before anything is asked.
    assert_eq!(
        portal.write("POST", "sub-nb1", quote, json!({ "policy": "x" })).await.0,
        StatusCode::BAD_REQUEST
    );
    // Someone whose role does not include new business may not.
    assert_eq!(
        portal.write("POST", "sub-sue", quote, application(71, "term-life")).await.0,
        StatusCode::FORBIDDEN
    );

    // Issuing is not offered: a policy is issued from underwriting's decision, never typed in.
    assert_eq!(
        portal
            .write("POST", "sub-nb1", "/staff/v1/new-business/policies", application(71, "term-life"))
            .await
            .0,
        StatusCode::NOT_FOUND
    );

    // Each was asked by the four questions and is in the access log, refusals too.
    let lines = logged(&portal, "sub-nb1").await;
    assert!(
        lines.iter().any(|(a, r, _)| a == "new_business.quote" && r == "application:KTL00000071"),
        "{lines:?}"
    );
}

/// Purpose: the core's own view of a policy is read across the partitions for staff who hold the insurance-sensitive class, is the same refusal for a policy that is not there as for one the person may not see, and the operations reconciliation joins every partition's breaks.
/// Why it matters: the core's covers, loadings and billing were reachable only through the core API, with app roles no person holds; staff and MCP users need them, under the access log. Operations need the reconciliation the close relies on without logging in to a node.
#[tokio::test]
async fn the_core_view_and_reconciliation_are_reached_through_the_portal() {
    let portal = portal();
    let (status, view) = portal.get("sub-nb1", "/staff/v1/core/policies/pol-c1?case=nb-77").await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["entry"]["partition"], 1);
    assert_eq!(
        portal.get("sub-nb1", "/staff/v1/core/policies/pol-zz?case=nb-77").await.0,
        StatusCode::NOT_FOUND
    );
    // Without the class, refused whether or not the policy exists.
    assert_eq!(portal.get("sub-sue", "/staff/v1/core/policies/pol-c1?case=c").await.0, StatusCode::FORBIDDEN);
    assert_eq!(portal.get("sub-sue", "/staff/v1/core/policies/pol-zz?case=c").await.0, StatusCode::FORBIDDEN);
    assert!(logged(&portal, "sub-nb1")
        .await
        .iter()
        .any(|(a, r, _)| a == "policy.core_view" && r == "policy:pol-c1"));

    let ops = [("x-dev-subject", "sub-ops".to_string()), ("x-dev-roles", "operations".to_string())];
    let (status, recon) = portal.send("GET", "/ops/v1/reconciliation", &ops, None).await;
    assert_eq!(status, StatusCode::OK, "{recon}");
    assert_eq!(
        (recon["clean"].as_bool(), recon["business_date"].as_str(), recon["unfinished"].as_u64()),
        (Some(false), Some("2027-11-15"), Some(1))
    );
    assert_eq!(
        recon["breaks"][0],
        json!({ "partition": 1, "check": "suspense_clear", "detail": "12.00 in suspense" })
    );
    assert_eq!(portal.get("sub-sue", "/ops/v1/reconciliation").await.0, StatusCode::FORBIDDEN);
}

/// Purpose: the reconciliation finds money in transit with no transfer moving, waits on a partition that is not open, and refuses a total it cannot read.
/// Why it matters: these are the checks the close makes across partitions; a reconciliation that missed the in-transit break, or showed a date before every partition had one, would tell operations the book balanced when it did not.
#[test]
fn reconciliation_joins_the_partitions() {
    use ic_channels::claims::reconciliation;
    let view = |p: u64, date: Value, transit: &str, moving: bool| json!({ "partition": p, "business_date": date, "breaks": [], "unfinished": [], "transfer_in_transit": transit, "transfer_in_flight": moving });
    let stuck = reconciliation(&[view(0, json!("2027-11-15"), "500", false), view(1, json!("2027-11-15"), "-200", false)]).unwrap();
    assert_eq!(stuck["clean"], false);
    assert_eq!(stuck["breaks"][0]["check"], "transfer_in_transit");
    let moving = reconciliation(&[view(0, json!("2027-11-15"), "500", true), view(1, Value::Null, "0", false)]).unwrap();
    assert_eq!((moving["clean"].as_bool(), moving["business_date"].is_null()), (Some(true), true));
    assert!(reconciliation(&[view(0, json!("2027-11-15"), "lots", false)])
        .unwrap_err()
        .contains("unreadable"));
}

/// Purpose: staff find, read, change and merge people in the customer master through the portal: each is decided by the four questions and logged, then sent to the master in the role and with the purpose the master's own policy expects, naming the person as the actor.
/// Why it matters: the customer master's staff work had no screen and no tool. Sending it in the wrong role or purpose would be refused by the master or, worse, logged as the wrong kind of access; doing it without the portal's check would let anyone with a staff sign-in search every customer. A possible duplicate names who it may be and the next step, found by the scenario run, F2-01-5.
#[tokio::test]
async fn staff_work_in_the_customer_master() {
    use ic_channels::customers::Desk;
    let portal = portal();
    let asked = |n: usize| portal.master.desks.lock().unwrap()[n].clone();

    let (status, found) = portal
        .write("POST", "sub-cd1", "/staff/v1/customers/search?case=dup-1", json!({ "family_name": "Ngata" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{found}");
    assert_eq!(
        (found["path"].as_str(), found["as"].as_str(), found["purpose"].as_str()),
        (Some("/v1/parties/search"), Some("staff"), Some("servicing"))
    );
    assert_eq!(asked(0), ("cd-1".to_string(), Desk::Search(json!({ "family_name": "Ngata" }))));

    let read = portal
        .get(
            "sub-cd1",
            "/staff/v1/customers/kiri?case=c-1&as_of=2027-11-01&known_at=2027-11-02T10:00:00%2B13:00",
        )
        .await
        .1;
    assert_eq!(
        read["path"],
        "/v1/parties/kiri?purpose=servicing&as_of=2027-11-01&known_at=2027-11-01T21:00:00Z"
    );
    assert_eq!(portal.get("sub-cd1", "/staff/v1/customers/kiri?as_of=soon").await.0, StatusCode::BAD_REQUEST);
    assert_eq!(
        portal.get("sub-cd1", "/staff/v1/customers/kiri/history?case=c-1").await.1["path"],
        "/v1/parties/kiri/history?purpose=servicing"
    );
    for (path, master) in [
        ("relationships", "/v1/parties/kiri/relationships"),
        ("due-diligence", "/v1/parties/kiri/due-diligence"),
        ("export", "/v1/parties/kiri/export?purpose=subject_access"),
    ] {
        let (status, body) = portal.get("sub-cd1", &format!("/staff/v1/customers/kiri/{path}?case=c-1")).await;
        assert_eq!((status, body["path"].as_str()), (StatusCode::OK, Some(master)), "{path}");
    }
    // The master's own refusal is passed on.
    assert_eq!(portal.get("sub-cd1", "/staff/v1/customers/nobody?case=c-1").await.0, StatusCode::NOT_FOUND);

    // Writes carry the person's key, scoped to staff, to the master.
    let headers = [("x-dev-subject", "sub-cd1".to_string()), ("idempotency-key", "chg-1".to_string())];
    let change = json!({ "command": "record", "facts": [] });
    let changed = portal
        .send("POST", "/staff/v1/customers/kiri/commands?case=c-1", &headers, Some(change))
        .await
        .1;
    assert_eq!(
        (changed["path"].as_str(), changed["key"].as_str()),
        (Some("/v1/parties/kiri/commands"), Some("staff:chg-1"))
    );
    // A record is not closed while the person has cover with us (SCN2-14, F2-14-2): Kiri owns a policy in force.
    let exit = |key: &str| [("x-dev-subject", "sub-cd1".to_string()), ("idempotency-key", key.to_string())];
    let (status, refused) = portal
        .send(
            "POST",
            &format!("/staff/v1/customers/{KIRI}/commands?case=c-1"),
            &exit("x-1"),
            Some(json!({ "command": "exit" })),
        )
        .await;
    assert_eq!((status, refused["title"].as_str()), (StatusCode::CONFLICT, Some("cover_in_force")), "{refused}");
    assert!(refused["detail"].as_str().unwrap().contains("seven years"));
    let closed = portal
        .send(
            "POST",
            "/staff/v1/customers/kiri/commands?case=c-1",
            &exit("x-2"),
            Some(json!({ "command": "exit" })),
        )
        .await
        .1;
    assert_eq!(closed["path"], "/v1/parties/kiri/commands");
    // Capacity is settled by someone holding that action, and asked of the master as legal (SCN2-23, F2-23-6).
    let settle = json!({ "command": "settle_capacity", "outcome": "lacks_capacity" });
    let (status, _) = portal
        .send("POST", "/staff/v1/customers/kiri/commands?case=c-1", &exit("s-1"), Some(settle.clone()))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let legal = [("x-dev-subject", "sub-prv".to_string()), ("idempotency-key", "s-2".to_string())];
    let settled = portal
        .send("POST", "/staff/v1/customers/kiri/commands?case=legal-1", &legal, Some(settle))
        .await
        .1;
    assert_eq!(
        (settled["as"].as_str(), settled["purpose"].as_str()),
        (Some("legal"), Some("capacity")),
        "{settled}"
    );
    let created = portal
        .write("POST", "sub-cd1", "/staff/v1/customers?case=c-1", json!({ "kind": "person", "facts": [] }))
        .await
        .1;
    assert_eq!(created["path"], "/v1/parties");
    // The master's own reason for refusing reaches the person, not a stock phrase.
    let (status, twin) = portal
        .write(
            "POST",
            "sub-cd1",
            "/staff/v1/customers?case=c-1",
            json!({ "kind": "person", "facts": [{ "field": "family_name", "value": "Twin" }] }),
        )
        .await;
    assert_eq!((status, twin["title"].as_str()), (StatusCode::CONFLICT, Some("possible_duplicate")), "{twin}");
    // It names who it may be and what to do next (F2-01-5).
    let said = twin["detail"].as_str().unwrap_or_default();
    assert!(
        said.starts_with("this person may already be known: ") && said.contains("match_customer") && said.contains("allow_duplicate: true"),
        "{said}"
    );
    let merged = portal
        .write(
            "POST",
            "sub-cd1",
            "/staff/v1/customers/merges?case=dup-1",
            json!({ "survivor": "a", "absorbed": "b" }),
        )
        .await
        .1;
    assert_eq!((merged["path"].as_str(), merged["purpose"].as_str()), (Some("/v1/merges"), Some("duplicates")));
    assert_eq!(
        portal
            .write(
                "POST",
                "sub-cd1",
                "/staff/v1/customers/unmerges?case=dup-1",
                json!({ "survivor": "a", "absorbed": "b" })
            )
            .await
            .1["path"],
        "/v1/unmerges"
    );
    assert_eq!(portal.get("sub-cd1", "/staff/v1/customers/reviews?case=dup-1").await.1["path"], "/v1/reviews");
    assert_eq!(
        portal
            .write("POST", "sub-cd1", "/staff/v1/customers/reviews/r-9/dismiss?case=dup-1", json!({}))
            .await
            .1["path"],
        "/v1/reviews/r-9/dismiss"
    );
    assert_eq!(
        portal
            .write(
                "POST",
                "sub-cd1",
                "/staff/v1/customers/match?case=c-1",
                json!({ "source": "claims", "family_name": "Ngata" })
            )
            .await
            .1["path"],
        "/v1/parties/match"
    );
    // A body that is not an object, and a write with no key, are refused here.
    assert_eq!(
        portal.write("POST", "sub-cd1", "/staff/v1/customers/search?case=c", json!([1])).await.0,
        StatusCode::BAD_REQUEST
    );
    let keyless = [("x-dev-subject", "sub-cd1".to_string())];
    assert_eq!(
        portal.send("POST", "/staff/v1/customers/merges?case=c", &keyless, Some(json!({}))).await.0,
        StatusCode::BAD_REQUEST
    );

    // Nobody works their own record.
    assert_eq!(portal.get("sub-cd1", "/staff/v1/customers/cd-1?case=c-1").await.0, StatusCode::FORBIDDEN);
    // The audit is the privacy officer's, asked as the privacy role.
    assert_eq!(portal.get("sub-cd1", "/staff/v1/customers/kiri/audit?case=c-1").await.0, StatusCode::FORBIDDEN);
    let audit = portal.get("sub-prv", "/staff/v1/customers/kiri/audit?case=sar-1").await.1;
    assert_eq!(
        (audit["path"].as_str(), audit["as"].as_str()),
        (Some("/v1/parties/kiri/audit"), Some("privacy"))
    );
    assert_eq!(
        portal.get("sub-prv", "/staff/v1/actors/sue/actions?case=sar-1").await.1["path"],
        "/v1/actors/sue/actions"
    );
    // Someone without these actions is refused before the master is asked.
    let before = portal.master.desks.lock().unwrap().len();
    assert_eq!(portal.get("sub-sue", "/staff/v1/customers/kiri?case=c").await.0, StatusCode::FORBIDDEN);
    assert_eq!(portal.master.desks.lock().unwrap().len(), before);
    let lines = logged(&portal, "sub-cd1").await;
    assert!(lines.iter().any(|(a, r, _)| a == "customers.view" && r == "party:kiri"), "{lines:?}");
    assert!(lines.iter().any(|(a, r, _)| a == "customers.find" && r == "customers"), "{lines:?}");

    // A master that cannot be reached says so.
    portal.master.down.store(true, Ordering::SeqCst);
    assert_eq!(
        portal.get("sub-cd1", "/staff/v1/customers/kiri?case=c-1").await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    portal.master.down.store(false, Ordering::SeqCst);
}

/// Purpose: operations work in the customer master (index status, chain checks, re-encryption, erasing again after a restore) needs the operations role and is sent to the master as operations; staff work is not reachable that way.
/// Why it matters: these are maintenance tasks with no person's record as their subject; they belong to operations alone, and the master decides them on the operations role.
#[tokio::test]
async fn operations_work_in_the_customer_master() {
    let portal = portal();
    let ops = [("x-dev-subject", "sub-ops".to_string()), ("x-dev-roles", "operations".to_string())];
    let index = portal.send("GET", "/ops/v1/customers/index", &ops, None).await.1;
    assert_eq!((index["path"].as_str(), index["as"].as_str()), (Some("/v1/admin/index"), Some("operations")));
    assert_eq!(
        portal.send("POST", "/ops/v1/customers/kiri/verify", &ops, None).await.1["path"],
        "/v1/admin/verify/kiri"
    );
    let rewrap = portal
        .send("POST", "/ops/v1/customers/rewrap", &ops, Some(json!({ "current_key_id": "k2" })))
        .await
        .1;
    assert_eq!(
        (rewrap["path"].as_str(), rewrap["body"]["current_key_id"].as_str()),
        (Some("/v1/admin/rewrap"), Some("k2"))
    );
    assert_eq!(
        portal
            .send("POST", "/ops/v1/customers/reshred", &ops, Some(json!({ "since": "2027-11-01" })))
            .await
            .1["path"],
        "/v1/admin/reshred"
    );
    assert_eq!(
        portal.send("POST", "/ops/v1/customers/reshred", &ops, Some(json!("x"))).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(portal.get("sub-cd1", "/ops/v1/customers/index").await.0, StatusCode::FORBIDDEN);
    // Operations work has no staff action, so the staff desk refuses it outright.
    assert_eq!(ic_channels::customers::Desk::Index.action(), None);
}

/// Purpose: when the customer master refuses the portal's own sign-in, the operator is told it is the portal's grant on the master
/// that is missing, as a fault of ours (502), not that their own token lacks a role; a body the master cannot read is said as such;
/// and a re-encryption or erasure sent without the field it needs is refused here naming that field.
/// Why it matters: found by the scenario run, SCN-23 (F-23-1, F-23-2). The engineer was told "the token does not carry the role
/// operations" about his own sign-in when the portal's identity lacked the grant, and sent after the wrong problem; and `{}` gave
/// only "the customer master did not take it".
#[tokio::test]
async fn customer_master_refusals_say_whose_fault_and_what_is_missing() {
    let portal = portal();
    let ops = [("x-dev-subject", "sub-ops".to_string()), ("x-dev-roles", "operations".to_string())];
    let (status, problem) = portal.send("POST", "/ops/v1/customers/ungranted/verify", &ops, None).await;
    assert_eq!(
        (status, problem["title"].as_str()),
        (StatusCode::BAD_GATEWAY, Some("customer_master_grant")),
        "{problem}"
    );
    let detail = problem["detail"].as_str().unwrap();
    assert!(
        detail.contains("portal's identity needs the operations role") && detail.contains("Nothing is wrong with your own sign-in"),
        "{detail}"
    );
    let (status, problem) = portal.send("POST", "/ops/v1/customers/garbled/verify", &ops, None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    assert!(problem["detail"].as_str().unwrap().contains("could not read the request"), "{problem}");
    for (path, body, named) in [
        ("/ops/v1/customers/rewrap", json!({}), "current_key_id is needed"),
        ("/ops/v1/customers/reshred", json!({}), "since is needed"),
        ("/ops/v1/customers/reshred", json!({ "since": "yesterday" }), "since is a date"),
    ] {
        let (status, problem) = portal.send("POST", path, &ops, Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        assert!(problem["detail"].as_str().unwrap().contains(named), "{problem}");
    }
}

/// Purpose: a member of staff's own standing carries the books' business date, the same date the channel module reports, so every
/// page's dates start from the books and not from the browser's clock.
/// Why it matters: found by the scenario run, SCN-17, SCN-18 and SCN-20 (F-SCN17-3, F-SCN18-6, F-20-6). The finance, pricing and
/// commission consoles filled every date from the browser, so a journal's value date fell outside its period and a suspension was
/// recorded as effective a year before it was opened.
#[tokio::test]
async fn staff_pages_take_their_dates_from_the_books() {
    let portal = portal();
    let ops = [("x-dev-subject", "ops-1".to_string()), ("x-dev-roles", OPERATIONS.to_string())];
    let books = portal.send("GET", "/ops/v1/regions", &ops, None).await.1["channels"]["business_date"].clone();
    assert!(books.is_string(), "{books}");
    assert_eq!(portal.get("sub-sue", "/staff/v1/me").await.1["business_date"], books);
}

/// Purpose: a tool's hints say what it does, not only what its method suggests: the record chain check changes nothing and takes
/// no idempotency key, erasing again after a restore is destructive; the regions answer gives the moment last heard as a time a
/// person can read; and a wrong protocol version is answered with the versions spoken.
/// Why it matters: found by the scenario run, SCN-23 (F-23-3, F-23-5, F-23-6). An assistant asks before a destructive call only if
/// told it is one, so an erasure marked harmless could be run unasked; and a raw epoch or an unexplained refusal leaves the operator
/// to work out what the answer means.
#[tokio::test]
async fn operations_tools_say_what_they_do_and_answer_in_readable_terms() {
    let portal = portal();
    let ops = vec![("x-dev-subject", "ops-1".to_string()), ("x-dev-roles", OPERATIONS.to_string())];
    let (_, listed) = mcp(&portal, &ops, json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })).await;
    let tool = |name: &str| {
        listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .cloned()
            .unwrap()
    };
    let verify = tool("verify_customer");
    assert_eq!(verify["annotations"]["readOnlyHint"], true, "{verify}");
    assert!(verify["inputSchema"]["properties"].get("idempotency_key").is_none(), "{verify}");
    let reshred = tool("reshred_customers");
    assert_eq!(
        (&reshred["annotations"]["destructiveHint"], &reshred["annotations"]["readOnlyHint"]),
        (&json!(true), &json!(false)),
        "{reshred}"
    );
    assert_eq!(tool("rewrap_customers")["annotations"]["destructiveHint"], false);

    let (_, console) = portal.send("GET", "/ops/v1/regions", &ops, None).await;
    let heard = console["channels"]["heard"].as_i64().unwrap();
    assert_eq!(
        console["channels"]["heard_at"]
            .as_str()
            .map(|t| chrono::DateTime::parse_from_rfc3339(t).unwrap().timestamp()),
        (heard > 0).then_some(heard),
        "{console}"
    );

    let mut wrong = ops.clone();
    wrong.push(("mcp-protocol-version", "1999-01-01".to_string()));
    let (status, refused) = mcp(&portal, &wrong, json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(refused["detail"].as_str().unwrap().contains("2025-06-18"), "{refused}");
}

/// Purpose: a customer's "Tell us about a claim" is carried to claims as soon as it is filed and closed with the claim's reference;
/// one claims refuses (a health claim, which is made with the invoice) is closed with claims' own reason; one filed while no
/// partition answers stays pending and is carried by the next sweep, and sweeping again changes nothing.
/// Why it matters: the customer is told "the claims team will be in touch"; a notice that is lost, carried twice or left pending
/// after a restart would leave a death or illness unanswered, so the portal must carry each once, by its own ID, from any start.
#[tokio::test]
async fn a_customers_claim_notice_reaches_claims_once() {
    let portal = &portal();
    portal.claims.cover(0, &portal.retail.to_string());
    let notice = |id: &str, claim_type: &str| {
        json!({ "id": id, "policy": portal.retail, "kind": "notify_claim", "module": "claims",
            "payload": { "cover": "life", "life": "life-1", "event_date": "2027-02-20", "claim_type": claim_type } })
    };
    let mine = |id: &'static str| async move {
        let (_, all) = portal.get("sub-kiri", "/app/v1/requests").await;
        all.as_array().unwrap().iter().find(|r| r["id"] == id).cloned().unwrap()
    };
    assert_eq!(
        portal.write("POST", "sub-kiri", "/app/v1/requests", notice("n-1", "death")).await.0,
        StatusCode::ACCEPTED
    );
    let carried = mine("n-1").await;
    assert_eq!((&carried["status"], &carried["reference"]), (&json!("applied"), &json!("app-n-1")), "{carried}");
    let path = format!("/app/v1/policies/{}/claims", portal.retail);
    assert_eq!(portal.get("sub-kiri", &path).await.1["rows"][0]["claim"], "app-n-1");

    assert_eq!(
        portal.write("POST", "sub-kiri", "/app/v1/requests", notice("n-2", "health")).await.0,
        StatusCode::ACCEPTED
    );
    let refused = mine("n-2").await;
    assert_eq!(
        (&refused["status"], &refused["code"]),
        (&json!("rejected"), &json!("health_by_invoice")),
        "{refused}"
    );
    // Claims' own refusal, for a cover the policy does not have, closes it too.
    let mut trauma = notice("n-5", "trauma");
    trauma["payload"]["cover"] = json!("trauma");
    portal.write("POST", "sub-kiri", "/app/v1/requests", trauma).await;
    assert_eq!(mine("n-5").await["status"], "rejected");

    // No partition answers: the notice waits.
    let mut down = (*portal.state).clone();
    down.claims = Arc::new(common::claims::Down);
    let down = router(Arc::new(down));
    let down = &down;
    let filed = Request::builder()
        .method("POST")
        .uri("/app/v1/requests")
        .header("x-dev-subject", "sub-kiri")
        .header("idempotency-key", "n-3")
        .body(Body::from(notice("n-3", "death").to_string()))
        .unwrap();
    assert_eq!(down.clone().oneshot(filed).await.unwrap().status(), StatusCode::ACCEPTED);
    assert_eq!(mine("n-3").await["status"], "pending");
    assert_eq!(portal.state.carry_claim_notices().await, Ok(1));
    assert_eq!(mine("n-3").await["reference"], "app-n-3");
    assert_eq!(portal.state.carry_claim_notices().await, Ok(0));
    assert_eq!(portal.get("sub-kiri", &path).await.1["rows"].as_array().unwrap().len(), 2);

    // Every partition answers and none holds the policy: the customer is told, not left waiting.
    let filed = Request::builder()
        .method("POST")
        .uri("/app/v1/requests")
        .header("x-dev-subject", "sub-kiri")
        .header("idempotency-key", "n-4")
        .body(Body::from(notice("n-4", "death").to_string()))
        .unwrap();
    assert_eq!(down.clone().oneshot(filed).await.unwrap().status(), StatusCode::ACCEPTED);
    let mut elsewhere = (*portal.state).clone();
    elsewhere.claims = Arc::new(common::claims::seeded());
    assert_eq!(elsewhere.carry_claim_notices().await, Ok(1));
    assert_eq!(mine("n-4").await["code"], "policy_not_found");
}

/// Purpose: staff with `wellness.view` read a policy's AIA Vitality members and their discount, and the discount changes the
/// core refused, from the wellness module; each read is logged; someone without the action, or a question the console does
/// not ask, is refused before the module is asked.
/// Why it matters: a policyholder who asks why their premium moved is asking about their Vitality discount. Staff had no way to
/// see it, and the discounts the core refused were only on the nodes. The discount is the module's alone, so the console only reads.
#[tokio::test]
async fn staff_read_a_policys_vitality_discount() {
    let portal = portal();
    let policy = portal.retail.0.clone();
    let (status, members) = portal.get("sub-sue", &format!("/staff/v1/wellness/policy?policy={policy}&case=c-1")).await;
    assert_eq!(status, StatusCode::OK, "{members}");
    assert_eq!((members[0]["member"].as_str(), members[0]["discount"].as_u64()), (Some("V-1"), Some(10)));
    assert_eq!(portal.get("sub-sue", "/staff/v1/wellness/policy?policy=nobody&case=c-1").await.1, json!([]));
    assert_eq!(
        portal.get("sub-sue", "/staff/v1/wellness/member?member=V-1&case=c-1").await.1["policy"],
        json!(policy)
    );
    assert_eq!(
        portal.get("sub-sue", "/staff/v1/wellness/member?member=V-9&case=c-1").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(portal.get("sub-sue", "/staff/v1/wellness/refused?case=c-1").await.1, json!([]));
    assert!(portal.get("sub-sue", "/staff/v1/wellness/outbox").await.1.is_array());
    assert_eq!(portal.get("sub-sue", "/staff/v1/wellness/policy").await.0, StatusCode::BAD_REQUEST);
    assert_eq!(portal.get("sub-sue", "/staff/v1/wellness/state_hash").await.0, StatusCode::BAD_REQUEST);
    assert_eq!(
        portal.get("sub-gl1", &format!("/staff/v1/wellness/policy?policy={policy}")).await.0,
        StatusCode::FORBIDDEN
    );
    let me = portal.get("sub-sue", "/staff/v1/me").await.1;
    assert!(
        me["recent"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["action"] == "wellness.view" && r["record"] == format!("policy:{policy}")),
        "{me}"
    );
}

/// Purpose: staff reach the documents module from the documents console. Each list a person works from (templates, legal
/// holds, open defects, triggers, requested letters, sends in progress) and each lookup reaches the module; a command goes as
/// the caller the person's own staff actions name, with the person signed in as its actor and as the author of a template they
/// submit; an approval needs that approval's own action; a reader may read but not act; someone with no documents action is
/// kept out; and a driver's command is refused before it reaches the module.
/// Why it matters: the documents module had no screen: legal holds, notice defects, template approvals and dismissed triggers
/// could only be sent by hand to the nodes. The module trusts the caller and actor it is told, so the console must be the one
/// to decide them from who signed in.
#[tokio::test]
async fn the_documents_console() {
    let portal = portal();
    let ask = |who: &'static str, rest: &str| {
        let path = format!("/staff/v1/documents/{rest}");
        let portal = &portal;
        async move { portal.get(who, &path).await }
    };
    let tell = |who: &'static str, command: Value| {
        let portal = &portal;
        async move { portal.write("POST", who, "/staff/v1/documents/commands", command).await }
    };

    // Who gets in, and what they may send.
    assert_eq!(ask("sub-gl1", "me").await.0, StatusCode::FORBIDDEN);
    let (status, me) = ask("sub-docview", "me").await;
    assert_eq!((status, &me["callers"], &me["view"]), (StatusCode::OK, &json!([]), &json!(true)), "{me}");
    let me = ask("sub-docof", "me").await.1;
    assert_eq!(me["callers"], json!(["legal", "remediation", "documents", "servicing", "config"]));
    assert_eq!(me["approvals"], json!(["product"]));

    // The lists and lookups.
    let (status, templates) = ask("sub-docview", "templates").await;
    assert_eq!(status, StatusCode::OK, "{templates}");
    let letter = templates.as_array().unwrap().iter().find(|t| t["template"]["id"] == "letter").unwrap().clone();
    assert_eq!((&letter["status"], &letter["author"]), (&json!("draft"), &json!("author")));
    for question in ["legal_holds", "open_defects", "triggers", "letters_requested", "pending_sends", "settings"] {
        let (status, answer) = ask("sub-docview", question).await;
        assert_eq!(status, StatusCode::OK, "{question}: {answer}");
    }
    assert_eq!(ask("sub-docview", "template?id=letter&version=1").await.1["status"], "draft");
    assert_eq!(ask("sub-docview", "template?id=letter&version=one").await.0, StatusCode::BAD_REQUEST);
    assert_eq!(ask("sub-docview", "template?id=letter").await.0, StatusCode::BAD_REQUEST);
    assert_eq!(ask("sub-docview", "template_for?id=renewal").await.1["template"]["id"], "renewal");
    assert_eq!(ask("sub-docview", "document?id=none").await.0, StatusCode::NOT_FOUND);
    assert_eq!(ask("sub-docview", "defect?id=none").await.0, StatusCode::NOT_FOUND);
    assert_eq!(ask("sub-docview", "letter_request?request=none").await.0, StatusCode::NOT_FOUND);
    assert_eq!(ask("sub-docview", "policy_documents?policy=pol-1").await.0, StatusCode::OK);
    assert_eq!(ask("sub-docview", "part_usage?part=none").await.0, StatusCode::OK);
    assert_eq!(ask("sub-docview", "retention?kind=renewal_notice").await.0, StatusCode::OK);
    assert_eq!(ask("sub-docview", "report?from=2027-11-01&to=2027-11-30").await.0, StatusCode::OK);
    assert_eq!(
        ask("sub-docview", "notices_by_template?id=renewal&version=1&from=2027-11-01&to=2027-11-30").await,
        (StatusCode::OK, json!([]))
    );
    assert_eq!(ask("sub-docview", "everything").await.0, StatusCode::BAD_REQUEST);

    // A reader acts on nothing; a legal hold goes as legal, by the person signed in.
    let hold = json!({ "type": "place_legal_hold", "scope": { "policy": "pol-9" }, "case": "LIT-1", "reason": "a dispute" });
    assert_eq!(tell("sub-docview", hold.clone()).await.0, StatusCode::FORBIDDEN);
    let (status, done) = tell("sub-docof", hold).await;
    assert_eq!((status, &done["accepted"]), (StatusCode::OK, &json!(true)), "{done}");
    let sent = portal.correspondence.received().last().unwrap().clone();
    assert_eq!((sent.caller.as_str(), sent.actor.as_str()), ("legal", "doc-of"));
    let holds = ask("sub-docview", "legal_holds").await.1;
    assert_eq!(
        (&holds[0]["scope"], &holds[0]["case"], &holds[0]["by"]),
        (&json!({ "policy": "pol-9" }), &json!("LIT-1"), &json!("doc-of"))
    );

    // Approvals: each by the action for it, and the module keeps one person to one approval.
    let approve = |role: &str| json!({ "type": "approve_template", "template": { "id": "letter", "version": 1 }, "role": role });
    assert_eq!(
        tell("sub-docof", approve("legal")).await.0,
        StatusCode::FORBIDDEN,
        "doc-of gives no legal approval"
    );
    assert_eq!(tell("sub-docof", approve("product")).await.0, StatusCode::OK);
    assert_eq!(tell("sub-docla", approve("legal")).await.0, StatusCode::OK);
    assert_eq!(portal.correspondence.received().last().unwrap().caller, "config");
    let approvals = ask("sub-docview", "template?id=letter&version=1").await.1["approvals"].clone();
    assert_eq!(approvals, json!({ "legal": "doc-la", "product": "doc-of" }));

    // A template submitted from the console is the signed-in person's, whatever the page said.
    let mut draft = serde_json::to_value(common::correspondence::kit::template(
        "notice",
        ic_documents::DocumentKind::GeneralCorrespondence,
        "Kia ora {{name}}.",
    ))
    .unwrap();
    draft["author"] = json!("someone-else");
    draft["type"] = json!("submit_template");
    let (status, done) = tell("sub-docof", draft).await;
    assert_eq!(status, StatusCode::OK, "{done}");
    match &portal.correspondence.received().last().unwrap().command {
        ic_documents::command::Command::SubmitTemplate(t) => assert_eq!(t.author, "doc-of"),
        other => panic!("not a submission: {other:?}"),
    }

    // A defect goes as remediation; disabling, which config and remediation both may send, goes as the first held.
    let defect = json!({ "type": "raise_notice_defect", "defect": "d1", "case": "REM-1", "description": "wrong rate", "cause": "wrong_rate", "from": "2027-11-01", "to": "2027-11-14" });
    assert_eq!(tell("sub-docof", defect).await.0, StatusCode::OK);
    assert_eq!(ask("sub-docview", "open_defects").await.1[0]["id"], "d1");
    let disable = json!({ "type": "disable_template", "template": { "id": "lapse", "version": 1 }, "reason": "wrong", "case": "REM-1" });
    let (status, done) = tell("sub-docof", disable).await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(portal.correspondence.received().last().unwrap().caller, "remediation");

    // The module's own refusals come back; what is not a person's to send, or not theirs, never reaches it.
    let (status, problem) = tell("sub-docof", json!({ "type": "dismiss_trigger", "trigger": "none", "reason": "not needed" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{problem}");
    let sent = portal.correspondence.received().len();
    assert_eq!(
        tell("sub-docof", json!({ "type": "shred_party", "party": "p1" })).await.0,
        StatusCode::FORBIDDEN,
        "no privacy action"
    );
    // A misspelt command is told the ones this person may send (SCN2-14, F2-14-5).
    let (status, problem) = tell("sub-docof", json!({ "type": "tick" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        problem["detail"].as_str().unwrap().contains("yours are: ") && problem["detail"].as_str().unwrap().contains("raise_notice_defect"),
        "{problem}"
    );
    let (_, problem) = tell("sub-docview", json!({ "type": "tick" })).await;
    assert!(problem["detail"].as_str().unwrap().ends_with("yours are: none"), "{problem}");
    assert_eq!(tell("sub-docof", json!("place_legal_hold")).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(portal.correspondence.received().len(), sent);
}

/// Purpose: on the documents console a policy's documents are found by the number on its letters (and any policy is
/// offered by its number as it is typed), a document comes back without its wrapped key or vault path, a draft is rendered
/// for a sample customer with its hash, the kinds and shipped wordings to write from are listed, undeliverable documents
/// are listed, a legal hold is refused without a reason, corrected notices and a returned letter go as the callers that
/// send them, and `me` says whether the person may download.
/// Why it matters: the records officer typed the policy number and was told no document existed; the lookup showed key
/// material; the product owner had to retype letters as JSON and compute a hash by hand; a hold showed no reason; nothing
/// could correct a notice or record a returned letter (found by the scenario run, F2-11-1, F2-11-7, F2-10-1, F2-10-3,
/// F2-11-11, F2-11-2, F2-11-4 and F2-11-10).
#[tokio::test]
async fn the_documents_console_finds_by_number_renders_samples_and_keeps_keys_back() {
    let portal = portal();
    let ask = |rest: String| {
        let portal = &portal;
        async move { portal.get("sub-docof", &format!("/staff/v1/documents/{rest}")).await }
    };
    let tell = |command: Value| {
        let portal = &portal;
        async move { portal.write("POST", "sub-docof", "/staff/v1/documents/commands", command).await }
    };
    // A schedule issued on the retail policy.
    {
        let mut held = portal.correspondence.0.lock().unwrap();
        let mut spec = held.0.issue_spec("s1", ic_documents::DocumentKind::PolicySchedule, "schedule");
        spec.policy = Some(portal.retail.clone());
        spec.wordings = vec![common::correspondence::kit::tref("wording", 1)];
        held.0.issue(spec);
    }
    assert_eq!(ask("me".into()).await.1["download"], json!(false));
    let (status, found) = ask(format!("policies?q={}", portal.retail)).await;
    assert_eq!(status, StatusCode::OK, "{found}");
    let number = found[0]["number"].as_str().unwrap().to_string();
    assert_eq!(ask("policies".into()).await.0, StatusCode::BAD_REQUEST);
    let (status, docs) = ask(format!("policy_documents?policy={}", number.to_lowercase())).await;
    assert_eq!((status, docs.as_array().map(Vec::len)), (StatusCode::OK, Some(1)), "{docs}");
    assert!(
        docs[0].get("wrapped_key").is_none() && docs[0].get("blob").is_none() && docs[0]["content_sha256"].is_string(),
        "{docs}"
    );
    let doc = ask("document?id=s1".into()).await.1;
    assert!(doc.get("wrapped_key").is_none() && doc["id"] == "s1", "{doc}");
    assert_eq!(
        ask(format!("policy_documents?policy={}", portal.retail)).await.1.as_array().map(Vec::len),
        Some(1)
    );

    let (status, sample) = ask("render_sample?id=letter&version=1".into()).await;
    assert_eq!((status, &sample["text"]), (StatusCode::OK, &json!("Dear Aroha Sample.")), "{sample}");
    assert_eq!(sample["sha256"].as_str().map(str::len), Some(64));
    let mine = ask("render_sample?id=letter&version=1&language=en&data=%7B%22name%22%3A%22Ben%22%7D".into())
        .await
        .1;
    assert_eq!(mine["text"], "Dear Ben.");
    assert_eq!(ask("render_sample?id=letter&version=1&data=nope".into()).await.0, StatusCode::BAD_REQUEST);
    let authoring = ask("authoring".into()).await.1;
    assert!(
        authoring["placeholders"].as_array().unwrap().iter().any(|p| p["id"] == "policy-wording"),
        "{authoring}"
    );
    assert_eq!(ask("undeliverable".into()).await, (StatusCode::OK, json!([])));

    let (status, problem) = tell(json!({ "type": "place_legal_hold", "scope": { "policy": "pol-9" }, "case": "LIT-1" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(problem["detail"].as_str().unwrap().contains("say why the hold is placed"), "{problem}");
    assert_eq!(
        tell(json!({ "type": "release_legal_hold", "scope": { "policy": "pol-9" }, "reason": " " }))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let defect = json!({ "type": "raise_notice_defect", "defect": "d9", "case": "REM-9", "description": "wrong number", "cause": "template_error",
        "from": "2027-11-01", "to": "2027-11-30", "template": { "id": "letter", "version": 1 } });
    assert_eq!(tell(defect).await.0, StatusCode::OK);
    let (status, _) = tell(json!({ "type": "correct_defect_notices", "defect": "d9" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "no notice is attached yet");
    assert_eq!(portal.correspondence.received().last().unwrap().caller, "remediation");
    let (status, _) = tell(json!({ "type": "record_returned_post", "document": "s1", "party": "aroha" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "no post is out");
    assert_eq!(portal.correspondence.received().last().unwrap().caller, "servicing");

    // A member of staff without the download action is told who downloads.
    let (status, problem) = portal.get("sub-docof", "/staff/v1/documents/s1/pdf?purpose=dispute").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(problem["detail"].as_str().unwrap().contains("documents.download"), "{problem}");
}

/// Purpose: staff work collections from the collections console. An officer reads the day's queue as an officer and records
/// what a customer said, proposes an arrangement and applies for hardship; the module hears each as the person signed in, as
/// caller `staff`, with the roles their staff actions give them. Someone in the hardship team decides the case, never the
/// officer who applied; a configuration person loads the best-estimate sequence shipped for a product and sets it; a reader
/// reads but cannot act or read another team's tasks; someone with no collections action is kept out; and a command that is
/// not people's to send is refused before it reaches the module.
/// Why it matters: the collections module had no screen, route or tool, so arrears were never worked by a person, hardship
/// was never decided and no product had a dunning sequence. The module trusts the roles it is told, so the console must be the
/// one to say who the person is.
#[tokio::test]
async fn the_collections_console() {
    let portal = portal();
    let ask = |who: &'static str, rest: &str| {
        let path = format!("/staff/v1/collections/{rest}");
        let portal = &portal;
        async move { portal.get(who, &path).await }
    };
    let tell = |who: &'static str, command: Value| {
        let portal = &portal;
        async move { portal.write("POST", who, "/staff/v1/collections/commands", command).await }
    };

    // Who gets in, and with which roles.
    assert_eq!(ask("sub-gl1", "me").await.0, StatusCode::FORBIDDEN);
    let (status, me) = ask("sub-colview", "me").await;
    assert_eq!((status, &me["roles"], &me["view"]), (StatusCode::OK, &json!([]), &json!(true)), "{me}");
    assert_eq!(ask("sub-colof", "me").await.1["roles"], json!(["collections"]));
    assert_eq!(ask("sub-colhs", "me").await.1["roles"], json!(["hardship"]));

    // The queue, as an officer reads it, with what an officer is not shown.
    let (status, queue) = ask("sub-colof", "queue").await;
    assert_eq!(status, StatusCode::OK, "{queue}");
    assert_eq!(queue["items"][0]["policy"], "pol-c1");
    assert_eq!(queue["withheld"][0], "hardship_note");
    assert_eq!(ask("sub-colview", "policy?policy=pol-c1").await.1["number"], "N-pol-c1");
    assert_eq!(ask("sub-colview", "policy?policy=pol-none").await.0, StatusCode::NOT_FOUND);
    let (status, tasks) = ask("sub-colof", "tasks?team=collections").await;
    assert_eq!(status, StatusCode::OK, "{tasks}");
    let task = tasks[0]["id"].as_str().unwrap().to_string();
    assert_eq!(tasks[0]["kind"], "call");
    // A reader sees the officers' tasks, not the hardship team's; a team that does not exist is refused.
    assert_eq!(ask("sub-colview", "tasks?team=hardship").await.0, StatusCode::BAD_REQUEST);
    assert_eq!(ask("sub-colview", "tasks?team=everyone").await.0, StatusCode::BAD_REQUEST);
    assert_eq!(ask("sub-colview", "tasks").await.0, StatusCode::BAD_REQUEST);
    assert_eq!(ask("sub-colview", "everything").await.0, StatusCode::BAD_REQUEST);
    for question in [
        "notices_due",
        "notices_sent",
        "unsequenced",
        "sequences",
        "reinstatements_for_underwriting",
        "hardship_cases",
    ] {
        assert_eq!(ask("sub-colview", question).await.0, StatusCode::OK, "{question}");
    }

    // A reader cannot act; an officer records an outcome and the module hears it from them.
    let outcome = json!({ "type": "record_outcome", "task": task, "reason": "arrangement", "note": "will pay over two months" });
    assert_eq!(tell("sub-colview", outcome.clone()).await.0, StatusCode::FORBIDDEN);
    let (status, done) = tell("sub-colof", outcome).await;
    assert_eq!((status, &done["accepted"]), (StatusCode::OK, &json!(true)), "{done}");
    let sent = portal.collections.received().last().cloned().unwrap();
    assert_eq!((sent.caller.as_str(), sent.actor.as_str()), ("staff", "col-of"));
    assert_eq!(sent.roles, vec![ic_collections::Role::Collections]);
    assert!(sent.key.starts_with("console:"));
    let arrangement = json!({ "type": "propose_arrangement", "policy": "pol-c1", "arrangement": "arr-1",
        "instalments": [{ "due": "2027-11-30", "amount_minor": 5_000 }, { "due": "2027-12-31", "amount_minor": 5_000 }] });
    let (status, done) = tell("sub-colof", arrangement).await;
    assert_eq!(status, StatusCode::OK, "{done}");

    // Hardship: an officer applies, the same person may not decide, the hardship team does.
    let apply = json!({ "type": "apply_hardship", "case": "hs-1", "party": "party-c1", "policies": ["pol-c1"], "relief": "holiday",
        "from": "2027-11-20", "to": "2028-02-19", "discloses_health": true, "note": "off work after an operation" });
    assert_eq!(tell("sub-colof", apply).await.0, StatusCode::OK);
    let (_, cases) = ask("sub-colof", "hardship_cases").await;
    assert_eq!((cases[0]["id"].as_str(), cases[0]["note"].as_str()), (Some("hs-1"), Some("")), "{cases}");
    assert_eq!(ask("sub-colhs", "hardship_cases").await.1[0]["note"], "off work after an operation");
    assert_eq!(ask("sub-colhs", "hardship_case?case=hs-1").await.1["applied_by"], "col-of");
    let decide = json!({ "type": "decide_hardship", "case": "hs-1", "approve": true, "note": "medical certificate seen" });
    let (status, problem) = tell("sub-colof", decide.clone()).await;
    assert_eq!((status, problem["title"].as_str()), (StatusCode::CONFLICT, Some("not_allowed")), "{problem}");
    let (status, done) = tell("sub-colhs", decide).await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(ask("sub-colhs", "tasks?team=hardship").await.0, StatusCode::OK);

    // Sequences: the shipped best estimate is offered, and only a configuration person sets one.
    let (status, shipped) = ask("sub-colcfg", "shipped_sequence?product=term-life").await;
    assert_eq!((status, &shipped["lapse_day"]), (StatusCode::OK, &json!(91)), "{shipped}");
    assert_eq!(ask("sub-colcfg", "shipped_sequence?product=no-such-product").await.0, StatusCode::NOT_FOUND);
    let set = json!({ "type": "set_sequence", "sequence": shipped });
    assert_eq!(tell("sub-colof", set.clone()).await.1["title"], "not_allowed");
    let (status, done) = tell("sub-colcfg", set).await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(ask("sub-colview", "sequences").await.1["term-life"]["lapse_day"], 91);

    // What is not people's to send never reaches the module.
    let before = portal.collections.received().len();
    let core = json!({ "type": "record_notice_sent", "notice": "n-1" });
    assert_eq!(tell("sub-colof", core).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(portal.collections.received().len(), before);
}

/// Purpose: the best-estimate dunning sequence shipped for each product names a shipped product, every shipped product
/// has one, and each one's lapse day is the day the core lapses that product (its arrears `lapse_days` plus one).
/// Why it matters: a sequence whose lapse day is not the core's would count days to lapse wrongly on the queue and could
/// put the final notice closer to the real lapse than the law allows; a product with none sends nothing.
#[test]
fn the_shipped_sequences_follow_each_products_lapse_day() {
    let products: Vec<_> = ic_sim::config::shipped_activations()
        .unwrap()
        .into_iter()
        .filter_map(|c| match c {
            ic_core::Command::ActivateProduct { product } => Some(*product),
            _ => None,
        })
        .collect();
    let sequences = ic_collections::shipped_sequences().unwrap();
    assert_eq!(sequences.len(), products.len());
    for product in &products {
        let sequence = sequences
            .iter()
            .find(|s| s.product == product.code)
            .unwrap_or_else(|| panic!("no sequence for {}", product.code));
        let lapse = product.arrears().unwrap().lapse_days + 1;
        assert_eq!(sequence.lapse_day, lapse, "{}", product.code);
        assert!(ic_channels::collections::shipped_sequence(&product.code).is_some());
    }
    assert_eq!(ic_channels::collections::reading_role(&["configuration".into()]), "collections");
    assert_eq!(ic_channels::collections::reading_role(&["team_lead".into(), "hardship".into()]), "hardship");
}

/// Purpose: a cancellation an adviser submitted is attributed to the adviser's engagement in the commission module, as
/// `policy-servicing` and before the core is asked, and then carried to the core; one the customer submitted is not
/// attributed; a change that gives up no cover is not attributed.
/// Why it matters: clawback by cause (an agreement's `clawback_by_cause`) puts the clawback on whoever caused the
/// cancellation only if the commission module is told who that was. Nothing told it, so the switch did nothing.
#[tokio::test]
async fn an_advisers_cancellation_is_attributed_to_their_engagement() {
    use ic_channels::state::{Request, RequestStatus};
    let portal = portal();
    let policy = portal.retail.clone();
    let pending = |id: &str, party: &str, kind: &str| Request {
        id: id.into(),
        party: p(party),
        policy: Some(policy.clone()),
        kind: kind.into(),
        module: "policy-servicing".into(),
        payload: json!({}),
        signed: true,
        submitted: 0,
        status: RequestStatus::Pending,
        approved_by: Some(p(KIRI)),
        signed_by: Default::default(),
        route_to_person: false,
        assisted: None,
        declined_by: None,
    };
    portal.local.with(|state| {
        state.data.directory.authorities.entry(policy.clone()).or_default().insert(
            p("adv-1"),
            ic_channels::directory::Authority {
                engagement: "e1".into(),
                granted_by: p(KIRI),
                granted: 0,
            },
        );
        state.data.requests.insert("a-1".into(), pending("a-1", "adv-1", "decline_indexation"));
        state.data.requests.insert("a-2".into(), pending("a-2", "adv-1", "cancel_policy"));
    });
    portal.claims.cover(0, &policy.to_string());
    assert_eq!(portal.state.carry_servicing_requests().await, Ok(2));
    let told = portal.commission.attributions();
    assert_eq!(told.len(), 1, "only the cancellation gives up cover: {told:?}");
    assert_eq!(told[0]["actor"], json!({ "module": "policy-servicing" }));
    assert_eq!(told[0]["command"]["attribute_alteration"], json!({ "policy": policy, "engagement": "e1" }));
    let serde_ok: Result<ic_distribution::state::Envelope, _> = serde_json::from_value(told[0].clone());
    assert!(serde_ok.is_ok(), "the commission module reads it: {serde_ok:?}");
    assert_eq!(portal.claims.servicing(0).len(), 2, "both reached the core");

    // The customer's own cancellation names no engagement.
    portal.local.with(|state| {
        state.data.requests.insert("a-3".into(), pending("a-3", KIRI, "cancel_policy"));
    });
    assert_eq!(portal.state.carry_servicing_requests().await, Ok(1));
    assert_eq!(portal.commission.attributions().len(), 1);
}

/// Purpose: a change the customer asked for in the app (decline indexation, change how often they pay, cancel) is carried to
/// the core as caller `policy-servicing` once it is pending, under a key made from the request so carrying twice changes the
/// policy once, and the request is closed with the core's answer, applied or refused with the core's own code. A request the
/// customer master sent to a person first, or of a kind the portal cannot carry out, waits; a member of staff who may carry
/// them out sees it (who asked and for what), carries it out or declines it in words the customer reads; anyone else is
/// refused. A request whose policy no partition answers for waits for the next sweep.
/// Why it matters: these requests were recorded, signed and shown as pending, and nothing ever carried them to the core: a
/// customer's cancellation or change of frequency simply never happened.
#[tokio::test]
async fn customers_changes_are_carried_to_the_core() {
    use ic_core::Command as Core;
    let portal = &portal();
    let file =
        |id: &str, kind: &str, payload: Value| json!({ "id": id, "policy": portal.retail, "kind": kind, "module": "policy-servicing", "payload": payload });
    let mine = |id: &'static str| async move {
        let (_, all) = portal.get("sub-kiri", "/app/v1/requests").await;
        all.as_array().unwrap().iter().find(|r| r["id"] == id).cloned().unwrap()
    };
    // Not yet placed on a partition: it waits, and is carried once a partition answers for it.
    portal
        .write(
            "POST",
            "sub-kiri",
            "/app/v1/requests",
            portal.signed(file("s-0", "cancel_policy", json!({}))).await,
        )
        .await;
    assert_eq!(mine("s-0").await["status"], "pending");
    assert_eq!(portal.state.carry_servicing_requests().await, Ok(0));
    portal.claims.cover(0, &portal.retail.to_string());
    assert_eq!(portal.state.carry_servicing_requests().await, Ok(1));
    assert_eq!(mine("s-0").await["status"], "applied");

    let (status, _) = portal
        .write(
            "POST",
            "sub-kiri",
            "/app/v1/requests",
            portal.signed(file("s-1", "change_frequency", json!({ "frequency": "yearly" }))).await,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let carried = mine("s-1").await;
    assert_eq!((&carried["status"], &carried["reference"]), (&json!("applied"), &json!("s-1")), "{carried}");
    portal
        .write(
            "POST",
            "sub-kiri",
            "/app/v1/requests",
            file("s-2", "decline_indexation", json!({ "declined": false })),
        )
        .await;
    let sent = portal.claims.servicing(0);
    assert!(sent.iter().all(|(caller, _)| caller == "policy-servicing"));
    assert!(matches!(&sent[0].1, Core::CancelPolicy { .. }));
    assert!(matches!(&sent[1].1, Core::ChangeBilling(c) if c.frequency == Some(ic_product::Frequency::Yearly) && c.reference == "s-1"));
    assert!(matches!(&sent[2].1, Core::DeclineIndexation { declined: false, .. }));
    // Carrying again sends nothing new.
    assert_eq!(portal.state.carry_servicing_requests().await, Ok(0));
    assert_eq!(portal.claims.servicing(0).len(), 3);

    // The core's refusal closes the request with its code; a payload the core could not read is refused before it is sent.
    portal.claims.refuse_servicing(0, "indexation_compulsory");
    portal
        .write("POST", "sub-kiri", "/app/v1/requests", file("s-3", "decline_indexation", json!({})))
        .await;
    let refused = mine("s-3").await;
    assert_eq!(
        (&refused["status"], &refused["code"]),
        (&json!("rejected"), &json!("indexation_compulsory")),
        "{refused}"
    );
    portal
        .write(
            "POST",
            "sub-kiri",
            "/app/v1/requests",
            portal.signed(file("s-4", "change_frequency", json!({ "frequency": "daily" }))).await,
        )
        .await;
    assert_eq!(mine("s-4").await["code"], "bad_frequency");

    // A kind the portal cannot carry out waits for a person, who may decline it.
    portal
        .write("POST", "sub-kiri", "/app/v1/requests", file("s-5", "change_sum_insured", json!({ "to": 1 })))
        .await;
    assert_eq!(mine("s-5").await["status"], "pending");
    assert_eq!(portal.get("sub-sue", "/staff/v1/servicing/waiting").await.0, StatusCode::FORBIDDEN);
    let (status, waiting) = portal.get("sub-svc", "/staff/v1/servicing/waiting").await;
    assert_eq!(
        (status, &waiting["rows"][0]["id"], &waiting["rows"][0]["kind"]),
        (StatusCode::OK, &json!("s-5"), &json!("change_sum_insured")),
        "{waiting}"
    );
    let (status, _) = portal
        .write("POST", "sub-svc", "/staff/v1/servicing/s-5/decline", json!({ "reason": " " }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = portal
        .write(
            "POST",
            "sub-svc",
            "/staff/v1/servicing/s-5/decline",
            json!({ "reason": "We will call you to talk this through." }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let declined = mine("s-5").await;
    assert_eq!(
        (&declined["status"], &declined["code"]),
        (&json!("rejected"), &json!("declined_by_service")),
        "{declined}"
    );
    assert_eq!(
        portal.write("POST", "sub-svc", "/staff/v1/servicing/s-5/carry-out", json!({})).await.0,
        StatusCode::NOT_FOUND
    );

    // A carried-out request goes as the sweep would send it; one the core refuses comes back with the core's words.
    portal
        .write("POST", "sub-kiri", "/app/v1/requests", file("s-6", "change_sum_insured", json!({})))
        .await;
    let (status, carried) = portal
        .write("POST", "sub-svc", "/staff/v1/servicing/s-6/carry-out", json!({ "conversation": "call-9" }))
        .await;
    assert_eq!(
        (status, &carried["applied"], &carried["code"]),
        (StatusCode::OK, &json!(false), &json!("not_carried")),
        "{carried}"
    );
    assert_eq!(
        portal.write("POST", "sub-sue", "/staff/v1/servicing/s-6/carry-out", json!({})).await.0,
        StatusCode::FORBIDDEN
    );
}

/// Purpose: each servicing request kind becomes the core command it names, from the payload the screens send, and a request
/// missing what the core needs is refused with a code that says what: no policy, no change, a frequency or billing day that is
/// not one.
/// Why it matters: the customer reads these codes on their request; a malformed payload must never reach the core as a
/// different change from the one the customer signed. Reducing cover, the commonest service change, could not be asked for at all
/// (found by the scenario run, SCN-13).
#[test]
fn servicing_requests_become_the_core_commands_they_name() {
    use ic_channels::servicing::{carried_by_sweep, command};
    use ic_core::Command as Core;
    let request = |kind: &str, payload: Value| json!({ "id": "r", "policy": "pol-1", "kind": kind, "payload": payload });
    assert!(matches!(
        command(&request("decline_indexation", json!({}))),
        Ok(Core::DeclineIndexation { declined: true, .. })
    ));
    assert!(
        matches!(command(&request("change_frequency", json!({ "billing_day": 15 }))), Ok(Core::ChangeBilling(c)) if c.billing_day == Some(15) && c.frequency.is_none())
    );
    let code = |r: Value| command(&r).unwrap_err().0;
    assert_eq!(code(request("change_frequency", json!({}))), "nothing_to_change");
    assert_eq!(code(request("change_frequency", json!({ "billing_day": "15th" }))), "bad_billing_day");
    assert_eq!(code(request("change_frequency", json!({ "frequency": 12 }))), "bad_frequency");
    assert_eq!(code(json!({ "id": "r", "kind": "cancel_policy" })), "no_policy");
    assert_eq!(code(request("surrender", json!({}))), "not_carried");
    // Reducing cover names the cover, the life and what it keeps (found by the scenario run, SCN-13).
    assert!(matches!(
        command(&request("reduce_cover", json!({ "cover": "life", "life": "life-1", "sum_insured_minor": 25_000_000 }))),
        Ok(Core::ReduceCover(r)) if r.cover == "life" && r.life.0 == "life-1" && r.sum_insured_minor == 25_000_000
    ));
    assert_eq!(
        code(request("reduce_cover", json!({ "life": "life-1", "sum_insured_minor": 1 }))),
        "nothing_to_reduce"
    );
    assert_eq!(
        code(request("reduce_cover", json!({ "cover": "life", "sum_insured_minor": 1 }))),
        "nothing_to_reduce"
    );
    assert_eq!(
        code(request("reduce_cover", json!({ "cover": "life", "life": "life-1", "sum_insured_minor": -1 }))),
        "bad_amount"
    );
    assert!(carried_by_sweep(&request("reduce_cover", json!({}))));
    assert!(
        ic_channels::servicing::ATTRIBUTED.contains(&"reduce_cover"),
        "giving cover up may claw commission back"
    );
    assert!(ic_channels::state::needs_signature("reduce_cover"), "giving cover up is signed on the device");
    assert!(carried_by_sweep(&request("cancel_policy", json!({}))));
    let mut cared = request("cancel_policy", json!({}));
    cared["route_to_person"] = json!(true);
    assert!(!carried_by_sweep(&cared));
    assert!(!carried_by_sweep(&request("surrender", json!({}))));
}

/// Purpose: a claim's payee is accepted only as a payee the payments register
/// holds that passed the bank's name check, was confirmed by a second payee
/// maintainer and screened clear, and whose kind fits how the claim pays it;
/// the party, name check and screening the claim records come from that record,
/// whatever the page sent.
/// Why it matters: found by the scenario run (SCN-11, F-11-1, a blocker). An
/// assessor could type any party and any "match" or "clear" references and
/// release a claim to them; the fraud control rested on her say-so.
#[test]
fn a_claims_payee_comes_from_the_payments_register() {
    use ic_channels::claims::payee_from_register;
    let good = json!({ "party": "party-p1", "kind": "beneficiary", "name_check": "match", "confirmed_by": "pat", "screened_clear": true, "closed": false });
    let fields = |kind: &str, id: &str| {
        json!({ "claim": "c-1", "kind": kind, "account_ref": id, "party": "typed", "name_check_ref": "match", "screening_ref": "clear" })
            .as_object()
            .unwrap()
            .clone()
    };
    let mut ok = fields("beneficiary", " p1 ");
    payee_from_register(&mut ok, &good).unwrap();
    assert_eq!((&ok["party"], &ok["account_ref"]), (&json!("party-p1"), &json!("p1")));
    assert_eq!(ok["name_check_ref"], "payments:p1:name_check:match:confirmed_by:pat");
    assert_eq!(ok["screening_ref"], "payments:p1:screened_clear");
    let with = |key: &str, value: Value| {
        let mut p = good.clone();
        p[key] = value;
        p
    };
    let refused = |kind: &str, id: &str, payee: &Value| payee_from_register(&mut fields(kind, id), payee).unwrap_err();
    assert!(refused("beneficiary", "", &good).contains("pick the payee from the payments register"));
    assert!(refused("beneficiary", "p9", &Value::Null).contains("there is no payee p9 in payments"));
    assert!(refused("beneficiary", "p1", &with("closed", json!(true))).contains("is closed"));
    assert!(refused("beneficiary", "p1", &with("name_check", json!("no_match"))).contains("name check"));
    assert!(refused("beneficiary", "p1", &with("confirmed_by", Value::Null)).contains("second payee maintainer"));
    assert!(refused("beneficiary", "p1", &with("screened_clear", Value::Null)).contains("screened clear"));
    assert!(refused("estate", "p1", &good).contains("does not pay a claim's estate"));
    assert!(refused("provider", "p1", &good).contains("does not pay a claim's provider"));
    assert!(refused("policyholder", "p1", &with("kind", json!("reinsurer"))).contains("as a reinsurer account"));
    let mut estate = fields("estate", "e1");
    payee_from_register(&mut estate, &with("kind", json!("estate"))).unwrap();
    let mut provider = fields("provider", "h1");
    payee_from_register(&mut provider, &with("kind", json!("provider"))).unwrap();
}

/// Purpose: an assessor refers a claim for possible non-disclosure from the claims console; the
/// portal asks underwriting, as the claims module, to open its review on the life's file, and the
/// claim's task carries the review's reference; a claim with no life, or a life underwriting has
/// no file for, is refused with why; referring twice is the same review; and when a second senior
/// signs the review's outcome, the claim hears it and its task closes.
/// Why it matters: found by the scenario run (SCN-24, F-24-1 a blocker, and F-24-2). Claims had
/// no way to refer a claim to underwriting, underwriting could not start a review on an in-force
/// policy, and nothing carried a review's outcome back, so a claim waited on a task nobody could
/// close.
#[tokio::test]
async fn a_claim_is_referred_to_underwriting_and_hears_the_outcome() {
    use ic_underwriting::command::Command as Uw;
    let portal = portal();
    let claims = |who: &'static str, purpose: &'static str, command: Value| {
        let portal = &portal;
        async move {
            portal
                .write("POST", who, "/staff/v1/claims/commands", json!({ "purpose": purpose, "command": command }))
                .await
        }
    };
    let notify = |claim: &str, policy: &str, life: Option<&str>| {
        json!({ "notify": { "claim": claim, "policy": policy, "cover": "life", "life": life, "claim_type": "death",
            "event_date": "2027-02-20", "channel": "phone", "notifier": "beneficiary", "claimant": "party-claimant" } })
    };
    let refer = |claim: &str| json!({ "open_task": { "claim": claim, "task": "typed-by-the-page", "kind": "non_disclosure_review" } });
    for (claim, policy, life) in [
        ("c-nd", "POL-P1", Some("L1")),
        ("c-nolife", "POL-P1", None),
        ("c-nofile", "pol-c1", Some("life-1")),
    ] {
        assert_eq!(claims("sub-cllead", "", notify(claim, policy, life)).await.0, StatusCode::OK, "{claim}");
        let assign = json!({ "assign": { "claim": claim, "to": "cl-1" } });
        assert_eq!(claims("sub-cllead", "allocating", assign).await.0, StatusCode::OK, "{claim}");
    }

    let (status, refused) = claims("sub-cl1", "a question over the disclosure", refer("c-nolife")).await;
    assert_eq!((status, &refused["title"]), (StatusCode::CONFLICT, &json!("no_life")), "{refused}");
    let (status, refused) = claims("sub-cl1", "a question over the disclosure", refer("c-nofile")).await;
    assert_eq!((status, &refused["title"]), (StatusCode::CONFLICT, &json!("underwriting_refused")), "{refused}");
    assert_eq!(
        claims("sub-cl1", "a question over the disclosure", refer("c-404")).await.0,
        StatusCode::NOT_FOUND
    );

    let (status, done) = claims("sub-cl1", "a question over the disclosure", refer("c-nd")).await;
    assert_eq!(status, StatusCode::OK, "{done}");
    let review = "ndr/c-nd/L1";
    assert!(matches!(
        &portal.underwriting.log().last().unwrap().command,
        Uw::OpenNonDisclosureReview { claim_ref, life, opened_by, .. } if claim_ref == "c-nd" && life.to_string() == "L1" && opened_by == "cl-1"
    ));
    let (_, work) = portal.get("sub-cl1", "/staff/v1/claims/work?claim=c-nd").await;
    assert!(
        work["view"]["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["id"] == review && t["kind"] == "non_disclosure_review"),
        "{work}"
    );
    // Referred again: underwriting has the review already, and the claim has the task.
    let (status, again) = claims("sub-cl1", "a question over the disclosure", refer("c-nd")).await;
    assert_eq!((status, &again["title"]), (StatusCode::CONFLICT, &json!("task_exists")), "{again}");

    // Underwriting decides: one senior prepares, a second signs, and the claim hears it.
    let tell = |who: &'static str, command: Value| portal.write("POST", who, "/staff/v1/underwriting/commands", command);
    assert_eq!(tell("sub-sam", json!({ "take_case": { "case": review } })).await.0, StatusCode::OK);
    let outcome =
        json!({ "record_non_disclosure_outcome": { "review": review, "outcome": "avoid", "reasons": ["RC-0980"], "varied_terms": [], "premium": "refund" } });
    let (status, prepared) = tell("sub-sam", outcome).await;
    assert_eq!(status, StatusCode::OK, "{prepared}");
    let id = prepared["prepared"].as_str().unwrap().to_string();
    let (status, sent) = portal.write("POST", "sub-cat", &format!("/staff/v1/underwriting/sign/{id}"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "{sent}");
    let (_, work) = portal.get("sub-cl1", "/staff/v1/claims/work?claim=c-nd").await;
    let task = work["view"]["tasks"].as_array().unwrap().iter().find(|t| t["id"] == review).cloned().unwrap();
    assert!(task["completed_on"].is_string(), "{work}");
}

/// Purpose: a review's reference names the claim it was opened for, and only an outcome of a
/// review opened for a claim is told to claims, under the underwriting caller.
/// Why it matters: found by the scenario run (SCN-24, F-24-1). The outcome must reach the claim
/// that asked, and nothing else may be mistaken for one.
#[test]
fn a_review_outcome_is_told_to_the_claim_it_was_opened_for() {
    use ic_channels::claims::{review_claim, review_id, review_outcome};
    assert_eq!(review_id("CLM-1", "L1"), "ndr/CLM-1/L1");
    assert_eq!(review_claim("ndr/CLM-1/L1"), Some("CLM-1"));
    assert_eq!(review_claim("ndr//L1"), None);
    assert_eq!(review_claim("case-1"), None);
    let (claim, envelope) = review_outcome(&json!({ "record_non_disclosure_outcome": { "review": "ndr/CLM-1/L1", "outcome": "vary" } })).unwrap();
    assert_eq!((claim.as_str(), envelope.caller.as_str()), ("CLM-1", "underwriting"));
    assert!(review_outcome(&json!({ "take_case": { "case": "x" } })).is_none());
    assert!(review_outcome(&json!({ "record_non_disclosure_outcome": { "review": "ndr/CLM-1/L1", "outcome": "maybe" } })).is_none());
}

/// Purpose: a payments question that finds nothing names what was asked for, by the reference
/// typed, and a question with no reference says payments has none to show; a payee picker asks
/// payments for registered payees by text, and only someone with a payments role may ask.
/// Why it matters: found by the scenario run (SCN-16, F-SCN16-8, and SCN-15, F-15-7). A bare
/// "no such resource" left the approver guessing whether the run or the page was wrong, and
/// payees were typed as references nobody can know.
#[test]
fn a_payments_question_that_finds_nothing_says_what() {
    use ic_channels::payments::{nothing_found, question};
    use std::collections::BTreeMap;
    let fields = BTreeMap::from([("run".to_string(), " run-9 ".to_string())]);
    assert_eq!(nothing_found("run", &fields), "there is no run run-9");
    assert_eq!(nothing_found("readiness", &BTreeMap::new()), "payments has no readiness to show");
    assert_eq!(nothing_found("open_runs", &fields), "payments has no open runs to show");
    let text = BTreeMap::from([("text".to_string(), "smith".to_string())]);
    let at = 1_800_000_000;
    assert_eq!(
        question("find_payees", &text, &["maker".to_string()], at).unwrap(),
        json!({ "q": "find_payees", "text": "smith" })
    );
    assert!(question("find_payees", &text, &[], at).is_err());
}

/// A row underwriting publishes for Kiri as a life on a proposal, applied to the portal's read model.
fn kiri_on(portal: &Portal, proposal: &str, life: &str, body: Value, index: u64) {
    let now = portal.now();
    let retail = portal.retail.clone();
    portal.local.with(|state| {
        use ic_log::StateMachine;
        let rows = json!({ "publish_rows": { "rows": [{ "key": format!("{retail}/underwriting/{proposal}/{life}"), "policy": retail, "kind": "proposal_life",
            "subject": KIRI, "sensitive": true, "body": body }], "remove": [] } });
        let command: Command = serde_json::from_value(rows).unwrap();
        let output = state.apply(
            index,
            ic_channels::Envelope {
                key: format!("row-{index}"),
                at: now,
                actor: module("underwriting"),
                command,
            },
        );
        assert!(output.result.is_ok(), "{:?}", output.result);
    });
}

/// Purpose: a new business officer quotes a walk-in from what they know (the product, the cover and its amount, how often
/// they would pay, and the life's birth date, sex and smoking) with no identifier typed and no person named; the product's
/// own conditions are asked of underwriting first, so cover above what the life's age allows is refused saying the amount
/// and the limit in dollars; the answer gives the total in dollars and how GST applies; and a missing fact is named.
/// Why it matters: the quote priced $600,000 of cover the product does not take at 61, made the officer invent a policy,
/// number, group and purpose, and answered with an internal field and no total, found by the scenario run, F2-01-1,
/// F2-01-3 and F2-01-4.
#[tokio::test]
async fn a_walk_in_is_quoted_from_what_the_officer_knows() {
    let portal = portal();
    let quote = "/staff/v1/new-business/quote";
    let walk_in = |sum: i64| {
        json!({ "product_code": "aia-essentials-life", "frequency": "monthly",
            "life": { "date_of_birth": "1965-06-01", "sex": "female", "smoker": false },
            "covers": [{ "code": "life", "sum_insured_minor": sum }] })
    };
    let (status, priced) = portal.write("POST", "sub-nb1", quote, walk_in(400_000_00)).await;
    assert_eq!(status, StatusCode::OK, "{priced}");
    assert_eq!((&priced["total"], &priced["frequency"]), (&json!("$12.34"), &json!("monthly")), "{priced}");
    assert_eq!(priced["gst"], "none: life and health cover is exempt from GST");
    assert!(priced.get("partition").is_none(), "{priced}");

    let (status, refused) = portal.write("POST", "sub-nb1", quote, walk_in(600_000_00)).await;
    assert_eq!((status, refused["title"].as_str()), (StatusCode::CONFLICT, Some("eligibility")), "{refused}");
    let said = refused["detail"].as_str().unwrap_or_default();
    assert!(said.contains("$600,000.00") && said.contains("$500,000.00"), "{said}");

    let mut no_birth = walk_in(400_000_00);
    no_birth["life"]["date_of_birth"] = json!(" ");
    let (status, problem) = portal.write("POST", "sub-nb1", quote, no_birth).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{problem}");
    assert!(problem["detail"].as_str().unwrap_or_default().contains("date_of_birth"), "{problem}");

    // Logged as a quote for nobody in particular, with no purpose asked.
    let lines = logged(&portal, "sub-nb1").await;
    assert!(
        lines
            .iter()
            .any(|(a, r, allowed)| a == "new_business.quote" && r == "quote:walk-in" && *allowed),
        "{lines:?}"
    );
}

/// Purpose: the life on a proposal reads the questions to answer from the question set in force, every one worded for them
/// with the shape of its answer, the duty they answer under, and the consents the proposal asks of them (the adviser's
/// access where an adviser is named); a proposal underwriting does not hold has no questions; and refusing a consent is
/// taken from a sign-in without an enrolled device, while giving one still needs the device.
/// Why it matters: no screen or tool took health answers or consents, so a customer could not complete her application,
/// and asking for a device's signature to say no kept people from refusing, found by the scenario run, F2-02-1, F2-02-2 and
/// F2-02-3.
#[tokio::test]
async fn the_life_reads_the_questions_and_may_refuse_a_consent_anywhere() {
    let portal = &portal();
    kiri_on(
        portal,
        "P3",
        "L1",
        json!({ "proposal": "P3", "life": "L1", "stage": "awaiting_disclosure", "waiting_on": "applicant" }),
        9_101,
    );
    kiri_on(
        portal,
        "P9",
        "L1",
        json!({ "proposal": "P9", "life": "L1", "stage": "awaiting_disclosure", "waiting_on": "applicant" }),
        9_102,
    );
    let (status, asked) = portal.get("sub-kiri", "/app/v1/proposals/P3/lives/L1/questions").await;
    assert_eq!(status, StatusCode::OK, "{asked}");
    let questions = asked["questions"].as_array().unwrap();
    assert!(!questions.is_empty(), "{asked}");
    for q in questions {
        assert!(!q["text"].as_str().unwrap_or_default().is_empty(), "every question is worded: {q}");
        assert!(q["answer"].is_string() || q["answer"].is_object(), "{q}");
    }
    assert!(asked["duty"].as_str().is_some_and(|d| !d.is_empty()), "{asked}");
    let kinds: Vec<&str> = asked["consents"].as_array().unwrap().iter().filter_map(|c| c["kind"].as_str()).collect();
    assert!(kinds.contains(&"obtain_records") && kinds.contains(&"adviser_access"), "{kinds:?}");
    assert_eq!(portal.get("sub-adv", "/app/v1/proposals/P3/lives/L1/questions").await.0, StatusCode::NOT_FOUND);
    let (status, none) = portal.get("sub-kiri", "/app/v1/proposals/P9/lives/L1/questions").await;
    assert_eq!((status, none["title"].as_str()), (StatusCode::CONFLICT, Some("no_questions")), "{none}");

    let consent = "/app/v1/proposals/P3/lives/L1/consents/obtain_records";
    let (status, problem) = portal.write("POST", "sub-kiri", consent, json!({ "granted": true })).await;
    assert_eq!(
        (status, problem["title"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("device_required")),
        "{problem}"
    );
    let before = portal.underwriting.log().len();
    let (status, refused) = portal.write("POST", "sub-kiri", consent, json!({ "granted": false })).await;
    assert_ne!(status, StatusCode::UNAUTHORIZED, "{refused}");
    let sent = portal.underwriting.log();
    assert_eq!(sent.len(), before + 1, "the refusal reached the module");
    assert_eq!(sent.last().unwrap().caller, "customer-app");
}

/// Purpose: before answering an offer, the life reads its terms in words (an exclusion by its kind), the date it is open
/// until, whether they may still ask us to look again, and the first premium on those terms priced by the core on the books'
/// date; an offer that is not the one open is not found; and the list of applications carries no application to price.
/// Why it matters: the customer accepted an offer without ever seeing its terms or price, found by the scenario run, F2-02-7;
/// and "Ask us to look again" was offered after it was used, F2-02-9.
#[tokio::test]
async fn the_life_reads_an_offer_before_answering() {
    let portal = &portal();
    let terms = json!([{ "cover": "life", "outcome": { "outcome": "accept", "loading_percent": 50,
        "exclusions": [{ "code": "EX-5001", "from": "2026-11-11", "until": null }] } }]);
    kiri_on(
        portal,
        "P3",
        "L1",
        json!({ "proposal": "P3", "life": "L1", "stage": "offer_open", "waiting_on": "applicant",
            "offer": { "offer": "O-1", "expires_on": "2027-12-15", "terms": terms, "look_again": false, "application": application(81, "term-life") } }),
        9_103,
    );
    let (status, offer) = portal.get("sub-kiri", "/app/v1/proposals/P3/lives/L1/offers/O-1").await;
    assert_eq!(status, StatusCode::OK, "{offer}");
    assert_eq!((&offer["look_again"], &offer["expires_on"]), (&json!(false), &json!("2027-12-15")), "{offer}");
    assert_eq!(offer["terms"][0]["said"], "Offered with an extra 50% on its premium");
    assert_eq!(offer["terms"][0]["exclusions"][0]["about"], "Not covered: your work");
    assert_eq!((&offer["premium_minor"], &offer["frequency"]), (&json!(1234), &json!("monthly")), "{offer}");
    assert_eq!(
        portal.get("sub-kiri", "/app/v1/proposals/P3/lives/L1/offers/O-2").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(portal.get("sub-adv", "/app/v1/proposals/P3/lives/L1/offers/O-1").await.0, StatusCode::NOT_FOUND);
    let (_, mine) = portal.get("sub-kiri", "/app/v1/proposals").await;
    let listed = mine.as_array().unwrap().iter().find(|p| p["proposal"] == "P3").cloned().unwrap();
    assert!(listed["offer"].get("application").is_none(), "{listed}");
    assert_eq!(listed["offer"]["look_again"], false);
}

/// Purpose: an underwriter declares a conflict with the person on a proposal, saying how they are connected; from then on
/// the proposal is closed to them with a reason that says so, and its case leaves their queue; a conflict with no reason
/// is refused, saying what to give.
/// Why it matters: a conflicted underwriter could still read the whole proposal, a privacy breach, and the refusal did
/// not say why, found by the scenario run, F2-03-1 and F2-03-3.
#[tokio::test]
async fn a_conflicted_underwriter_reads_nothing_of_the_case() {
    let portal = portal();
    let case = portal.underwriting.case("P2", "L1").expect("P2 waits for a person");
    let in_queue = |items: &Value| items.as_array().unwrap().iter().any(|i| i["case"] == case.as_str());
    let medical = |who: &'static str| {
        let portal = &portal;
        async move { portal.get(who, "/staff/v1/underwriting/queue?queue=occupational").await.1 }
    };
    assert!(in_queue(&medical("sub-amy").await), "the case waits in the occupational queue");
    let declare = |reason: Value| json!({ "declare_conflict": { "party": "PTY-L1", "reason": reason } });
    let (status, problem) = portal.write("POST", "sub-amy", "/staff/v1/underwriting/commands", declare(json!(" "))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{problem}");
    assert!(problem["detail"].as_str().unwrap_or_default().contains("reason"), "{problem}");
    let (status, declared) = portal
        .write("POST", "sub-amy", "/staff/v1/underwriting/commands", declare(json!("my neighbour")))
        .await;
    assert_eq!(status, StatusCode::OK, "{declared}");

    let (status, closed) = portal.get("sub-amy", "/staff/v1/underwriting/proposal?proposal=P2").await;
    assert_eq!((status, closed["title"].as_str()), (StatusCode::FORBIDDEN, Some("conflict")), "{closed}");
    assert!(
        closed.get("lives").is_none() && closed["detail"].as_str().is_some_and(|d| d.contains("conflict")),
        "{closed}"
    );
    assert!(!in_queue(&medical("sub-amy").await), "nor is its case in their queue");
    // A colleague still reads it, and sees it waiting.
    assert_eq!(portal.get("sub-abe", "/staff/v1/underwriting/proposal?proposal=P2").await.0, StatusCode::OK);
    assert!(in_queue(&medical("sub-abe").await));
}
