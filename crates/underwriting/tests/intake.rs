//! The disclosure intake: answers in, a sealed record and a submitted
//! disclosure out, directly and over HTTP with Entra tokens.

mod common;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use chrono::{DateTime, Utc};
use common::*;
use http_body_util::BodyExt;
use ic_api::auth::Auth;
use ic_azure::keywrap::DevWrapper;
use ic_azure::records::MemoryRecords;
use ic_log::StateMachine;
use ic_underwriting::clinical::ClinicalStore;
use ic_underwriting::command::{Applied, Command, Envelope, Reject};
use ic_underwriting::disclosures::{Answers, Intake, IntakeError, Signed, Step, ToSign};
use ic_underwriting::driver::{DriverError, Underwriting};
use ic_underwriting::intake_service::{router, serve, IntakeConfig};
use ic_underwriting::model::Stage;
use ic_underwriting::signing::message;
use ic_underwriting::special::{answer, Query};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::RsaPrivateKey;
use serde_json::{json, Value};
use tower::ServiceExt;

/// The module in this process.
#[derive(Clone)]
struct Module(Arc<Mutex<World>>);

impl Underwriting for Module {
    async fn propose(&self, envelope: Envelope) -> Result<Result<Applied, Reject>, DriverError> {
        let mut w = self.0.lock().unwrap();
        w.index += 1;
        let index = w.index;
        w.log.push(envelope.clone());
        Ok(w.s.apply(index, envelope))
    }
    async fn ask(&self, query: Query) -> Result<Value, DriverError> {
        answer(&self.0.lock().unwrap().s, serde_json::to_value(&query).unwrap()).map_err(DriverError::Answer)
    }
}

fn clock() -> DateTime<Utc> {
    Utc::now()
}

fn intake() -> (Intake<Module, DevWrapper, MemoryRecords>, Module, MemoryRecords) {
    let mut w = World::new();
    w.open("P1", 500_000_00);
    let module = Module(Arc::new(Mutex::new(w)));
    let records = MemoryRecords::default();
    let intake = Intake {
        module: module.clone(),
        store: ClinicalStore {
            wrapper: DevWrapper,
            records: records.clone(),
        },
        clock,
    };
    (intake, module, records)
}

fn answers(life: &str) -> Answers {
    Answers {
        proposal: "P1".into(),
        life: life.into(),
        step: Step::Disclosure,
        answers: clean(),
        adviser_entered: BTreeSet::new(),
    }
}

fn sign(a: &Answers, hash: &str) -> Signed {
    Signed {
        answers: a.clone(),
        signature: signed("disclosure", &["P1", &a.life.0, hash]),
    }
}

/// Purpose: the intake says what the life signs without storing anything; refuses a signature
/// that is not the life's over these answers before anything is written; seals the signed record
/// under the life's own name and submits it, so the module moves the life on with the record's
/// hash and reference and never an answer; takes a repeat as the same submission; and reports
/// answers that do not fit, a proposal or life it does not know, and a refusal by the module.
/// Why it matters: this is the only way a disclosure enters production. A record written before
/// its signature was checked would be health data held on nobody's authority; a repeat that
/// failed because the record already existed would strand a client whose reply was lost; and the
/// module must never hold the answers themselves.
#[tokio::test]
async fn signed_answers_are_sealed_and_submitted_once() {
    let (intake, module, records) = intake();
    let a = answers("L1");
    let ToSign { hash, message: to_sign } = intake.to_sign(&a).await.unwrap();
    assert_eq!(STANDARD.decode(to_sign).unwrap(), message("disclosure", &["P1", "L1", &hash]));
    assert!(records.names().is_empty(), "nothing is stored before the signature");

    let forged = Signed {
        answers: a.clone(),
        signature: signed("disclosure", &["P1", "L1", "another hash"]),
    };
    assert_eq!(intake.submit(&forged).await, Err(IntakeError::Signature));
    assert!(records.names().is_empty());

    let done = intake.submit(&sign(&a, &hash)).await.unwrap();
    assert_eq!((done.hash.as_str(), done.duplicate), (hash.as_str(), false));
    assert_eq!(done.vault_ref, format!("clinical://PTY-L1/disclosure/{hash}"));
    let (record, access) = intake.store.get(&done.vault_ref, "amy", "underwriting").await.unwrap();
    assert!(String::from_utf8(record).unwrap().contains("\"answers\""));
    assert_eq!(access.who, "amy");
    {
        let w = module.0.lock().unwrap();
        let life = &w.p("P1").lives[&"L1".into()];
        assert_ne!(life.stage, Stage::AwaitingDisclosure);
        let logged = serde_json::to_string(w.log.last().unwrap()).unwrap();
        assert!(
            logged.contains(&hash) && !logged.contains("smoked_last_12_months"),
            "the module sees the hash, not the answers"
        );
    }

    let again = intake.submit(&sign(&a, &hash)).await.unwrap();
    assert_eq!((again.vault_ref, again.duplicate), (done.vault_ref, true));

    let mut wrong = a.clone();
    wrong
        .answers
        .insert("no_such_question".into(), (1, ic_underwriting::questions::Answer::YesNo(true)));
    assert!(matches!(intake.to_sign(&wrong).await, Err(IntakeError::Answers(p)) if !p.is_empty()));
    let mut elsewhere = a.clone();
    elsewhere.proposal = "P9".into();
    assert_eq!(intake.to_sign(&elsewhere).await, Err(IntakeError::UnknownProposal("P9".into())));
    assert_eq!(intake.to_sign(&answers("L9")).await, Err(IntakeError::UnknownLife("L9".into())));

    // Withdrawn, the proposal takes no more answers: the module's refusal is passed on.
    module.0.lock().unwrap().ok("new-business", Command::Withdraw { proposal: "P1".into() });
    let mut interview = a.clone();
    interview.step = Step::Interview;
    let ToSign { hash, .. } = intake.to_sign(&interview).await.unwrap();
    let late = Signed {
        answers: interview,
        signature: signed("interview", &["P1", "L1", &hash]),
    };
    assert!(matches!(intake.submit(&late).await, Err(IntakeError::Refused(_))));
}

// --- over HTTP ---

const TENANT: &str = "tenant-1";
const AUDIENCE: &str = "api://underwriting-intake";

fn signing_key() -> &'static (EncodingKey, Value) {
    static KEY: OnceLock<(EncodingKey, Value)> = OnceLock::new();
    KEY.get_or_init(|| {
        let private = RsaPrivateKey::new(&mut rand::rngs::OsRng, 2048).unwrap();
        let pem = private.to_pkcs1_pem(rsa::pkcs1::LineEnding::LF).unwrap();
        (
            EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap(),
            json!({"kid": "k1", "kty": "RSA", "n": URL_SAFE_NO_PAD.encode(private.n().to_bytes_be()), "e": URL_SAFE_NO_PAD.encode(private.e().to_bytes_be())}),
        )
    })
}

async fn tenant() -> String {
    let app = Router::new().route("/{tenant}/discovery/v2.0/keys", get(|| async { Json(json!({"keys": [signing_key().1]})) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let authority = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    authority
}

fn token(authority: &str, roles: &[&str]) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let claims = json!({"iss": format!("{authority}/{TENANT}/v2.0"), "aud": AUDIENCE, "exp": now + 600, "sub": "svc", "oid": "svc", "roles": roles});
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("k1".into());
    encode(&header, &claims, &signing_key().0).unwrap()
}

async fn call(app: &Router, path: &str, bearer: Option<&str>, body: &Value) -> (StatusCode, Value) {
    let mut request = Request::post(path).header("content-type", "application/json");
    if let Some(t) = bearer {
        request = request.header("authorization", format!("Bearer {t}"));
    }
    let response = app.clone().oneshot(request.body(Body::from(body.to_string())).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// Purpose: over HTTP, only a service holding a token for this audience with the app role
/// `new-business` is served; the answers come back as the bytes to sign, the signed answers as
/// the sealed reference; answers that do not fit are a 422 listing the problems, an unknown
/// proposal a 404, a bad signature a 400 and the module's refusal a 409 with its code. The
/// settings name every required variable and default the rest; the process starts, serves and
/// stops, and refuses to start without its settings or an address it can listen on.
/// Why it matters: the intake writes health data. A token minted for another application, or
/// one without the role, must never reach it; and the portal can only show an adviser what to
/// fix if a mismatch comes back as its problems rather than as a failure.
#[tokio::test]
async fn the_intake_serves_only_new_business_over_http() {
    let authority = tenant().await;
    let (intake, module, _) = intake();
    let app = router(intake, Auth::entra_at(&authority, TENANT, AUDIENCE));
    let ok = token(&authority, &["new-business"]);
    let a = serde_json::to_value(answers("L1")).unwrap();

    assert_eq!(call(&app, "/v1/disclosures/to-sign", None, &a).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        call(&app, "/v1/disclosures/to-sign", Some(&token(&authority, &["claims"])), &a).await.0,
        StatusCode::FORBIDDEN
    );
    let (status, sign) = call(&app, "/v1/disclosures/to-sign", Some(&ok), &a).await;
    assert_eq!(status, StatusCode::OK);
    let hash = sign["hash"].as_str().unwrap().to_string();

    let mut bad = a.clone();
    bad["answers"]["no_such_question"] = json!([1, {"yes_no": true}]);
    let (status, problems) = call(&app, "/v1/disclosures/to-sign", Some(&ok), &bad).await;
    assert_eq!((status, problems["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("answers")));
    assert!(!problems["problems"].as_array().unwrap().is_empty());
    let mut unknown = a.clone();
    unknown["proposal"] = json!("P9");
    assert_eq!(call(&app, "/v1/disclosures/to-sign", Some(&ok), &unknown).await.0, StatusCode::NOT_FOUND);

    let mut forged = serde_json::to_value(sign_with(&a, "x")).unwrap();
    assert_eq!(call(&app, "/v1/disclosures", Some(&ok), &forged).await.0, StatusCode::BAD_REQUEST);
    forged = serde_json::to_value(sign_with(&a, &hash)).unwrap();
    let (status, done) = call(&app, "/v1/disclosures", Some(&ok), &forged).await;
    assert_eq!((status, done["hash"].as_str()), (StatusCode::OK, Some(hash.as_str())));

    module.0.lock().unwrap().ok("new-business", Command::Withdraw { proposal: "P1".into() });
    let mut interview = a.clone();
    interview["step"] = json!("interview");
    let (_, sign) = call(&app, "/v1/disclosures/to-sign", Some(&ok), &interview).await;
    let mut late: Value = interview.clone();
    late["signature"] = serde_json::to_value(signed("interview", &["P1", "L1", sign["hash"].as_str().unwrap()])).unwrap();
    let (status, refused) = call(&app, "/v1/disclosures", Some(&ok), &late).await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert!(refused["code"].is_string());

    let health = app.clone().oneshot(Request::get("/healthz").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(health.status(), StatusCode::OK);

    let required = [
        "IC_NODES",
        "IC_ENTRA_TENANT_ID",
        "IC_UNDERWRITING_INTAKE_AUDIENCE",
        "IC_CLINICAL_BLOB_ENDPOINT",
        "IC_CLINICAL_KEY_VAULT_URL",
        "IC_CLINICAL_WRAPPING_KEY",
    ];
    let all = |missing: &str| IntakeConfig::from_lookup(|k| (required.contains(&k) && k != missing).then(|| format!("v-{k}")));
    let config = all("").unwrap();
    assert_eq!(
        (config.listen.as_str(), config.container.as_str(), config.authority.as_str()),
        ("0.0.0.0:8080", "clinical", ic_api::auth::ENTRA_AUTHORITY)
    );
    for key in required {
        assert!(all(key).unwrap_err().contains(key));
    }

    // The process: refuses to start without its settings or where it cannot listen, and serves
    // until told to stop. The shipped binary refuses the same way from its environment.
    let no_file = |_: &str| -> std::io::Result<String> { Err(std::io::Error::other("no file")) };
    assert!(serve(|_| None, no_file, async {}).await.unwrap_err().contains("IC_NODES"));
    let env = |listen: &'static str| {
        move |k: &str| match k {
            "IC_NODES" => Some("127.0.0.1:1".to_string()),
            "IC_CLUSTER_TOKEN" => Some("test-cluster-token-0123456789abcdef".to_string()),
            "IC_UNDERWRITING_INTAKE_LISTEN" => Some(listen.to_string()),
            k if required.contains(&k) => Some(format!("https://{}.example", k.to_lowercase())),
            _ => None,
        }
    };
    assert!(serve(env("not an address"), no_file, async {}).await.unwrap_err().contains("cannot listen"));
    serve(env("127.0.0.1:0"), no_file, tokio::time::sleep(std::time::Duration::from_millis(200)))
        .await
        .unwrap();
    let refused = std::process::Command::new(env!("CARGO_BIN_EXE_ic-underwriting-intake"))
        .env_clear()
        .envs(std::env::vars().filter(|(k, _)| k == "LLVM_PROFILE_FILE"))
        .output()
        .unwrap();
    assert!(!refused.status.success() && String::from_utf8_lossy(&refused.stderr).contains("IC_NODES"));
}

fn sign_with(a: &Value, hash: &str) -> Value {
    let mut signed_answers = a.clone();
    signed_answers["signature"] = serde_json::to_value(signed("disclosure", &["P1", "L1", hash])).unwrap();
    signed_answers
}

/// Purpose: an underwriter who holds or has worked the life's case, or a senior, reads the
/// life's disclosures with a purpose; the module records who read which records and why before
/// any is read, and a repeat of the same reading is the same reading. Someone with no case on
/// the life, a reading with no purpose, and a record that no longer has the hash it was
/// submitted with are all refused. Over HTTP only the staff console's role may read.
/// Why it matters: these are health answers. Every reading must be by someone with a reason to
/// read and leave a record that names them, and an underwriter must never decide on a record
/// that was changed after the life signed it.
#[tokio::test]
async fn an_underwriter_on_the_case_reads_the_disclosure_and_the_reading_is_logged() {
    let (intake, module, records) = intake();
    let mut a = answers("L1");
    a.answers = yes("heart_condition");
    let ToSign { hash, .. } = intake.to_sign(&a).await.unwrap();
    let done = intake.submit(&sign(&a, &hash)).await.unwrap();
    module.0.lock().unwrap().take("P1", "L1", "amy");

    let read = |request: &str, who: &str, purpose: &str| ic_underwriting::disclosures::OpenRequest {
        request: request.into(),
        proposal: "P1".into(),
        life: "L1".into(),
        underwriter: who.into(),
        purpose: purpose.into(),
    };
    let opened = intake.open(&read("r1", "amy", "assess the case")).await.unwrap();
    assert_eq!(
        (opened.len(), opened[0].vault_ref.as_str(), opened[0].hash.as_str()),
        (1, done.vault_ref.as_str(), hash.as_str())
    );
    assert_eq!(opened[0].record["answers"]["heart_condition"]["answer"], json!({"yes_no": true}));
    let logged = {
        let w = module.0.lock().unwrap();
        let at = w.log.iter().position(|e| e.key == "open:r1").unwrap();
        let mut replica = w.s.clone();
        (at, replica.apply(u64::MAX, w.log[at].clone()))
    };
    let applied = logged.1.unwrap();
    assert!(applied.duplicate, "the reading was applied once, in the log");
    assert!(
        matches!(&applied.events[0], ic_underwriting::command::Event::FileAccessed { who, purpose, documents, .. } if who == "amy" && purpose == "assess the case" && documents == &vec![done.vault_ref.clone()])
    );
    assert_eq!(
        intake.open(&read("r1", "amy", "assess the case")).await.unwrap(),
        opened,
        "a repeat is the same reading"
    );
    assert!(intake.open(&read("r2", "sam", "senior review")).await.is_ok(), "a senior may read");
    assert!(matches!(intake.open(&read("r3", "abe", "curious")).await, Err(IntakeError::Refused(r)) if r.code == "not_authorised"));
    assert!(matches!(intake.open(&read("r4", "amy", " ")).await, Err(IntakeError::Refused(r)) if r.code == "purpose"));

    let name = records.names().into_iter().find(|n| n.contains("/disclosure/")).unwrap();
    records.overwrite(&name, b"not the record".to_vec());
    assert!(matches!(intake.open(&read("r5", "amy", "assess the case")).await, Err(IntakeError::Store(_))));

    let authority = tenant().await;
    let app = router(intake, Auth::entra_at(&authority, TENANT, AUDIENCE));
    let body = serde_json::to_value(read("r6", "amy", "assess the case")).unwrap();
    assert_eq!(
        call(&app, "/v1/disclosures/open", Some(&token(&authority, &["new-business"])), &body).await.0,
        StatusCode::FORBIDDEN
    );
    let (status, refused) = call(&app, "/v1/disclosures/open", Some(&token(&authority, &["underwriting-console"])), &body).await;
    assert_eq!(
        (status, refused["code"].as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("unavailable")),
        "the record was tampered with"
    );
}
