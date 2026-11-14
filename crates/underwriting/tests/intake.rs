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

/// A module that cannot be reached.
#[derive(Clone)]
struct Down;

impl Underwriting for Down {
    async fn propose(&self, _envelope: Envelope) -> Result<Result<Applied, Reject>, DriverError> {
        Err(DriverError::Unavailable("no leader".into()))
    }
    async fn ask(&self, _query: Query) -> Result<Value, DriverError> {
        Err(DriverError::Unavailable("no leader".into()))
    }
}

/// Purpose: answers for a life with no date of birth are refused before anything is signed,
/// because the rules cannot run without the life's age; and an intake that cannot reach the
/// module says it is unavailable rather than treat the answers as unknown or wrong.
/// Why it matters: a disclosure signed and sealed that the rules can never run on strands the
/// applicant with no decision; and a client told its proposal does not exist when the module
/// was only briefly unreachable would give up instead of trying again.
#[tokio::test]
async fn answers_need_a_date_of_birth_and_a_module_that_answers() {
    let (intake, module, records) = intake();
    {
        let mut w = module.0.lock().unwrap();
        let mut open = w.open_command("P2", &["L1"], &[("L1", "life", "life", 100_000_00)], false);
        open.lives[0].factors.date_of_birth = None;
        w.ok("new-business", Command::OpenProposal(Box::new(open)));
    }
    let mut a = answers("L1");
    a.proposal = "P2".into();
    assert_eq!(intake.to_sign(&a).await, Err(IntakeError::NoDateOfBirth));
    let unreachable = Intake {
        module: Down,
        store: ClinicalStore {
            wrapper: DevWrapper,
            records: records.clone(),
        },
        clock,
    };
    assert!(matches!(unreachable.to_sign(&answers("L1")).await, Err(IntakeError::Unavailable(_))));
    assert!(records.names().is_empty());
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

/// Purpose: an evidence provider's adapter, and only one holding the app role `evidence`,
/// reports an order acknowledged, sends the report, which is sealed under the person's own name
/// before the module is told where it is, and may send it again without filing it twice; a
/// request nobody made, a report that is not a document or is empty, and a cost below zero are
/// refused before anything is stored; an order the provider cannot fill is reported with nothing
/// sealed. A delegate's file is taken only with the role `delegate-api`, sealed the same way, and
/// passed to the module with its hash.
/// Why it matters: these are the only ways a medical report or a delegate's file enters the
/// system. A report the module heard of before it was sealed could point at nothing; one sealed
/// under the wrong person would be shredded with someone else's records; and a provider that
/// retries after a lost reply must not leave two copies of someone's medical history.
#[tokio::test]
async fn evidence_and_delegate_files_are_sealed_before_the_module_hears_of_them() {
    use ic_underwriting::model::ConsentKind;
    let authority = tenant().await;
    let (intake, module, records) = intake();
    let request = {
        let mut w = module.0.lock().unwrap();
        w.consent("P1", "L1", ConsentKind::ObtainRecords);
        w.disclose("P1", "L1", &yes("diabetes"));
        w.p("P1").lives[&"L1".into()].evidence[0].id.clone()
    };
    let app = router(intake, Auth::entra_at(&authority, TENANT, AUDIENCE));
    let provider = token(&authority, &["evidence"]);

    let ack = json!({"outcome": "acknowledged", "request": request, "provider_ref": "LAB-7"});
    assert_eq!(
        call(&app, "/v1/evidence", Some(&token(&authority, &["new-business"])), &ack).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(call(&app, "/v1/evidence", Some(&provider), &ack).await.0, StatusCode::OK);

    let report = STANDARD.encode(b"%PDF a report");
    let unknown = json!({"outcome": "received", "request": "no-such", "report": report, "cost_minor": 12_000});
    assert_eq!(call(&app, "/v1/evidence", Some(&provider), &unknown).await.0, StatusCode::NOT_FOUND);
    let garbled = json!({"outcome": "received", "request": request, "report": "%%%", "cost_minor": 12_000});
    assert_eq!(call(&app, "/v1/evidence", Some(&provider), &garbled).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    let empty = json!({"outcome": "received", "request": request, "report": "", "cost_minor": 12_000});
    assert_eq!(call(&app, "/v1/evidence", Some(&provider), &empty).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    let negative = json!({"outcome": "received", "request": request, "report": report, "cost_minor": -1});
    assert_eq!(call(&app, "/v1/evidence", Some(&provider), &negative).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(records.names().iter().all(|n| !n.contains("evidence")), "nothing stored for a refusal");

    let received = json!({"outcome": "received", "request": request, "report": report, "cost_minor": 12_000});
    let (status, filed) = call(&app, "/v1/evidence", Some(&provider), &received).await;
    assert_eq!(status, StatusCode::OK, "{filed}");
    let vault_ref = filed["vault_ref"].as_str().unwrap().to_string();
    assert!(vault_ref.starts_with("clinical://PTY-L1/evidence/"), "{vault_ref}");
    assert_eq!(filed["duplicate"], json!(false));
    let (again_status, again) = call(&app, "/v1/evidence", Some(&provider), &received).await;
    assert_eq!(
        (again_status, &again["vault_ref"], &again["duplicate"]),
        (StatusCode::OK, &json!(vault_ref), &json!(true))
    );
    {
        let w = module.0.lock().unwrap();
        let e = &w.p("P1").lives[&"L1".into()].evidence[0];
        assert_eq!((e.vault_ref.as_deref(), e.provider_ref.as_deref()), (Some(vault_ref.as_str()), Some("LAB-7")));
    }

    let file = json!({"proposal": "P1", "life": "L1", "file": STANDARD.encode(b"the delegate's file")});
    assert_eq!(call(&app, "/v1/delegate-files", Some(&provider), &file).await.0, StatusCode::FORBIDDEN);
    let delegate = token(&authority, &["delegate-api"]);
    let mut nobody = file.clone();
    nobody["proposal"] = json!("P9");
    assert_eq!(call(&app, "/v1/delegate-files", Some(&delegate), &nobody).await.0, StatusCode::NOT_FOUND);
    let mut no_life = file.clone();
    no_life["life"] = json!("L9");
    assert_eq!(call(&app, "/v1/delegate-files", Some(&delegate), &no_life).await.0, StatusCode::NOT_FOUND);
    let (status, answered) = call(&app, "/v1/delegate-files", Some(&delegate), &file).await;
    // No delegate decided this life: the module refuses it, after sealing.
    assert_eq!(status, StatusCode::CONFLICT, "{answered}");
    let sent = module.0.lock().unwrap().log.last().unwrap().clone();
    assert_eq!(sent.caller, "delegate-api");
    assert!(matches!(sent.command, Command::DelegateFileReceived { ref vault_ref, .. } if vault_ref.starts_with("clinical://PTY-L1/delegate_file/")));

    // A provider that cannot get a second order says so; nothing is sealed for it.
    let second = {
        let mut w = module.0.lock().unwrap();
        w.open("P2", 500_000_00);
        w.consent("P2", "L1", ConsentKind::ObtainRecords);
        w.disclose("P2", "L1", &yes("diabetes"));
        w.p("P2").lives[&"L1".into()].evidence[0].id.clone()
    };
    let gone = json!({"outcome": "unobtainable", "request": second});
    let (status, filed) = call(&app, "/v1/evidence", Some(&provider), &gone).await;
    assert_eq!((status, &filed["vault_ref"]), (StatusCode::OK, &json!(null)));
}

/// Purpose: a quality sample of a delegate's decision from end to end. The delegate decides a
/// life inside its authority; an underwriter calls for the file under the audit access the
/// agreement gives, which the module logs (who, why) before asking the delegate; the delegate's
/// file arrives through the intake, is sealed under the life's own name, and the module records
/// where it is and its hash.
/// Why it matters: the insurer stays responsible for decisions made in its name. Sampling them
/// needs the file, and the file is health information: the request must be on the record before
/// anyone sees it, and on arrival it must be held like the insurer's own disclosures, so it can
/// be erased with the person's other records.
#[tokio::test]
async fn a_sampled_delegated_decision_brings_the_file_in_under_the_lifes_key() {
    use ic_underwriting::command::{DelegatedDecision, Effect, Event};
    use ic_underwriting::config::DelegatedAuthority;
    use ic_underwriting::evidence::EvidenceKind;
    let authority = tenant().await;
    let (intake, module, _) = intake();
    {
        let mut w = module.0.lock().unwrap();
        w.ok(
            "config",
            Command::ActivateDelegatedAuthority {
                authority: Box::new(DelegatedAuthority {
                    id: "acme".into(),
                    version: 1,
                    delegate: "acme-tpa".into(),
                    products: ["term-life".to_string()].into(),
                    per_life_minor: [("life".to_string(), 300_000_00)].into(),
                    aggregate_minor: 500_000_00,
                    max_loading_percent: 50,
                    may_exclude: false,
                    may_decline: true,
                    rule_set: Some(("individual".into(), 1)),
                    effective_from: d(2026, 1, 1),
                    effective_to: None,
                    reinsurer_agreement_ref: Some("RE-1".into()),
                }),
                approval: approval(),
            },
        );
        w.open("P2", 200_000_00);
        let decided = w.ok(
            "delegate-api",
            Command::DelegatedDecision(Box::new(DelegatedDecision {
                proposal: "P2".into(),
                life: "L1".into(),
                authority: "acme".into(),
                terms: vec![standard("life")],
                effective: d(2026, 11, 10),
                rule_version: Some(1),
                evidence: BTreeSet::from([EvidenceKind::GpReport]),
                decided_by: "tpa-underwriter-4".into(),
                delegate_ref: "REF-9".into(),
                consent_ref: "CONS-1".into(),
            })),
        );
        assert!(matches!(decided.view, Some(ic_underwriting::command::View::Delegated { accepted: true, .. })));
        let asked = w.ok(
            "underwriting-console",
            Command::RequestDelegateFile {
                proposal: "P2".into(),
                life: "L1".into(),
                underwriter: "sam".into(),
                purpose: "quality sampling".into(),
            },
        );
        assert!(matches!(&asked.events[0], Event::FileAccessed { who, purpose, .. } if who == "sam" && purpose == "quality sampling"));
        assert!(matches!(&asked.effects[0], Effect::RequestDelegateFile { delegate_ref, .. } if delegate_ref == "REF-9"));
    }

    let app = router(intake, Auth::entra_at(&authority, TENANT, AUDIENCE));
    let file = json!({"proposal": "P2", "life": "L1", "file": STANDARD.encode(b"the delegate's underwriting file")});
    let (status, filed) = call(&app, "/v1/delegate-files", Some(&token(&authority, &["delegate-api"])), &file).await;
    assert_eq!(status, StatusCode::OK, "{filed}");
    let vault_ref = filed["vault_ref"].as_str().unwrap();
    assert!(vault_ref.starts_with("clinical://PTY-L1/delegate_file/"), "{vault_ref}");
    let w = module.0.lock().unwrap();
    let held = w.p("P2").lives[&"L1".into()].delegate_file.clone().unwrap();
    assert_eq!((held.0.as_str(), held.1.as_str()), (vault_ref, filed["hash"].as_str().unwrap()));
}

/// Records orders published to a hub.
#[derive(Clone, Default)]
struct Hub(Arc<Mutex<Vec<(String, String, Value)>>>);

impl ic_underwriting::notices::StaffFeed for Hub {
    async fn publish(&self, id: &str, kind: &str, body: Value) -> Result<(), ic_azure::AzureError> {
        self.0.lock().unwrap().push((id.into(), kind.into(), body));
        Ok(())
    }
}

/// Purpose: an underwriter asks a previous insurer overseas for the life's claims history. The
/// request has a deadline and a status, goes to the evidence hub for the previous-insurer
/// adapter as an order naming only the request, the kind and the party, comes back through the
/// intake (acknowledged under the insurer's reference, then the history itself, sealed under the
/// life's name), and the decision records that it relied on the previous insurer's evidence.
/// Why it matters: a history from an overseas insurer is often the only record of an earlier
/// claim. If it were asked for outside the system nobody could chase it or see it was late, and
/// a decision that does not say what it relied on cannot be reviewed or defended later.
#[tokio::test]
async fn a_previous_insurers_history_is_ordered_tracked_and_relied_on() {
    use ic_underwriting::command::{Effect, Event};
    use ic_underwriting::driver::Forward;
    use ic_underwriting::evidence::{EvidenceKind, EvidenceStatus};
    use ic_underwriting::model::ConsentKind;
    use ic_underwriting::notices::{NoLetters, Notices};
    let authority = tenant().await;
    let (intake, module, _) = intake();
    let (request, effect, case) = {
        let mut w = module.0.lock().unwrap();
        w.consent("P1", "L1", ConsentKind::ObtainRecords);
        w.disclose("P1", "L1", &yes("heart_condition"));
        let case = w.take("P1", "L1", "amy");
        let asked = w.ok(
            "underwriting-console",
            Command::RequestEvidence {
                case: case.clone(),
                underwriter: "amy".into(),
                kinds: BTreeSet::from([EvidenceKind::PreviousInsurer]),
            },
        );
        let effect = asked.effects.iter().find(|e| matches!(e, Effect::RequestEvidence { .. })).cloned().unwrap();
        let deadline = asked.events.iter().find_map(|e| match e {
            Event::EvidenceRequested { deadline, .. } => Some(*deadline),
            _ => None,
        });
        assert!(deadline.is_some_and(|d| d > w.now), "the request has a deadline");
        let e = w.p("P1").lives[&"L1".into()]
            .evidence
            .iter()
            .find(|e| e.kind == EvidenceKind::PreviousInsurer)
            .unwrap()
            .clone();
        assert_eq!(e.status, EvidenceStatus::Requested);
        (e.id, effect, case)
    };

    let hub = Hub::default();
    let notices = Notices {
        staff: Hub::default(),
        evidence: hub.clone(),
        delegates: Hub::default(),
        letters: NoLetters,
        clock,
    };
    assert_eq!(notices.forward(1, &effect).await, Ok(None));
    let sent = hub.0.lock().unwrap().clone();
    assert_eq!(
        sent[0].2,
        json!({"order": "request", "request": request, "kind": "previous_insurer", "party": "PTY-L1"})
    );

    let app = router(intake, Auth::entra_at(&authority, TENANT, AUDIENCE));
    let adapter = token(&authority, &["evidence"]);
    let ack = json!({"outcome": "acknowledged", "request": request, "provider_ref": "PREV-INS-UK-31"});
    assert_eq!(call(&app, "/v1/evidence", Some(&adapter), &ack).await.0, StatusCode::OK);
    let history = json!({"outcome": "received", "request": request, "report": STANDARD.encode(b"claims history"), "cost_minor": 0});
    let (status, filed) = call(&app, "/v1/evidence", Some(&adapter), &history).await;
    assert_eq!(status, StatusCode::OK, "{filed}");

    let mut w = module.0.lock().unwrap();
    let e = w.p("P1").lives[&"L1".into()].evidence.iter().find(|e| e.id == request).unwrap().clone();
    assert_eq!((e.status, e.provider_ref.as_deref()), (EvidenceStatus::Received, Some("PREV-INS-UK-31")));
    w.ok(
        "underwriting-console",
        Command::ProposeDecision {
            case,
            underwriter: "amy".into(),
            terms: vec![standard("life")],
            reasons: set(&["RC-0101"]),
            withheld_reasons: BTreeSet::new(),
            notes_ref: Some("vault://notes".into()),
            evidence: BTreeSet::from([EvidenceKind::PreviousInsurer]),
        },
    );
    let decision = w.p("P1").lives[&"L1".into()].decisions.last().unwrap().clone();
    assert!(decision.evidence.contains(&EvidenceKind::PreviousInsurer));
}
