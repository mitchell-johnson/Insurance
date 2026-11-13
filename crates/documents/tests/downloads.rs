//! The download route: a document as a PDF, to the portal for the customer
//! it belongs to and to staff for a stated reason, signed in with Entra ID.

mod support;

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::routing::get;
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ic_documents::auth::Entra;
use ic_documents::channels::{Channels, MemoryDirectory, Outbound, SendError};
use ic_documents::command::Command;
use ic_documents::crypto::{self, LocalWrap};
use ic_documents::driver::{Driver, IssueRequest, LocalModule, Module};
use ic_documents::kinds::DataClass;
use ic_documents::model::{Channel, RecipientRole};
use ic_documents::pdf::Fonts;
use ic_documents::service::{load_fonts, router, router_with, serve, Config, Downloads};
use ic_documents::vault::MemoryStore;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::RsaPrivateKey;
use serde_json::{json, Value};
use support::*;

const KEY: &str = "0123456789abcdef0123456789abcdef";
const TENANT: &str = "tenant-1";
const AUDIENCE: &str = "api://ic-documents";

#[derive(Clone, Default)]
struct Accepting;

impl Channels for Accepting {
    async fn send(&self, message: &Outbound) -> Result<String, SendError> {
        Ok(format!("ref-{}", message.document))
    }
}

type TestDriver = Driver<LocalModule, MemoryStore, LocalWrap, Accepting, MemoryDirectory>;

struct Signing {
    key: EncodingKey,
    jwk: Value,
}

fn signing() -> &'static Signing {
    static KEY: OnceLock<Signing> = OnceLock::new();
    KEY.get_or_init(|| {
        let private = RsaPrivateKey::new(&mut rand::rngs::OsRng, 2048).unwrap();
        let pem = private.to_pkcs1_pem(rsa::pkcs1::LineEnding::LF).unwrap();
        Signing {
            key: EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap(),
            jwk: json!({
                "kid": "k1",
                "kty": "RSA",
                "n": URL_SAFE_NO_PAD.encode(private.n().to_bytes_be()),
                "e": URL_SAFE_NO_PAD.encode(private.e().to_bytes_be()),
            }),
        }
    })
}

/// A stand-in tenant that publishes the signing key.
async fn tenant() -> String {
    let app = Router::new().route("/{tenant}/discovery/v2.0/keys", get(|| async { Json(json!({ "keys": [signing().jwk] })) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let authority = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    authority
}

fn token(authority: &str, roles: &[&str]) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let claims = json!({
        "iss": format!("{authority}/{TENANT}/v2.0"),
        "aud": AUDIENCE,
        "exp": now + 600,
        "sub": "workload",
        "oid": "object-1",
        "roles": roles,
    });
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("k1".into());
    encode(&header, &claims, &signing().key).unwrap()
}

fn request(id: &str, owner: &str, others: &[&str], class: DataClass) -> IssueRequest {
    let mut recipients = vec![recipient(owner, RecipientRole::Addressee, &[Channel::Email])];
    recipients.extend(others.iter().map(|p| recipient(p, RecipientRole::Representative, &[Channel::Email])));
    IssueRequest {
        document: id.into(),
        caller: "documents".into(),
        template: "receipt".into(),
        template_version: None,
        policy: Some(policy("pol-1")),
        other_policies: vec![],
        data_class: class,
        lives: vec![],
        prove_delivery: false,
        owner: party(owner),
        recipients,
        language: "en".into(),
        format: None,
        data: sample_data(),
        trigger_event: None,
        case: None,
        supersedes: None,
        reason: None,
        wordings: vec![],
        components: vec![],
        notice: None,
    }
}

/// A driver with three documents: one for Mere copied to her adviser
/// Tama, one clinical, and one for Hemi.
async fn driver() -> Arc<TestDriver> {
    let module = LocalModule::default();
    let ready = Kit::ready();
    for envelope in &ready.log {
        assert!(module.propose(envelope.clone()).await.unwrap().result.is_ok());
    }
    let now = ready.now;
    let driver = Arc::new(Driver {
        module,
        objects: MemoryStore::default(),
        wrap: LocalWrap::new(crypto::new_key()),
        channels: Accepting,
        directory: MemoryDirectory::default(),
        actor: "documents-service".into(),
        clock: Arc::new(move || now),
        batch: 50,
    });
    driver.issue(request("mere-1", "mere", &["tama"], DataClass::Personal)).await.unwrap();
    driver.issue(request("mere-clinical", "mere", &[], DataClass::Clinical)).await.unwrap();
    driver.issue(request("hemi.1", "hemi", &[], DataClass::Personal)).await.unwrap();
    // A claim letter to someone whose name the typeface cannot draw.
    let mut chinese = request("wang-1", "wang", &[], DataClass::Personal);
    chinese.template = "decision".into();
    chinese.trigger_event = Some("9-1".into());
    chinese.data["name"] = json!("王芳");
    driver.issue(chinese).await.unwrap();
    driver
}

struct Site {
    base: String,
    authority: String,
    http: reqwest::Client,
    _stop: tokio::sync::oneshot::Sender<()>,
}

impl Site {
    async fn start(driver: Arc<TestDriver>, downloads: bool) -> Self {
        let authority = tenant().await;
        let routes = if downloads {
            router_with(
                driver,
                KEY,
                Some(Downloads {
                    entra: Entra::new(&authority, TENANT, AUDIENCE),
                    fonts: Fonts::bundled(),
                }),
            )
        } else {
            router(driver, KEY)
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(serve(listener, routes, async {
            let _ = stopped.await;
        }));
        Self {
            base,
            authority,
            http: reqwest::Client::new(),
            _stop: stop,
        }
    }

    /// Ask for a document; the token is for `roles` unless `token` is given.
    async fn get(&self, id: &str, roles: &[&str], headers: &[(&str, &str)]) -> reqwest::Response {
        let mut request = self.http.get(format!("{}/v1/documents/{id}/pdf", self.base));
        if !roles.is_empty() {
            request = request.bearer_auth(token(&self.authority, roles));
        }
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        request.send().await.unwrap()
    }

    async fn status(&self, id: &str, roles: &[&str], headers: &[(&str, &str)]) -> u16 {
        self.get(id, roles, headers).await.status().as_u16()
    }
}

/// Purpose: the portal downloads a document as a PDF for the customer it
/// was issued to, or for someone it was copied to, and gets "not found"
/// for anyone else's document, as for one that does not exist.
/// Why it matters: this route hands a customer's correspondence to the
/// internet-facing portal. If the party were not checked against the
/// document, one customer could read another's letters by changing a
/// number; if a refusal differed from "not found", the portal could learn
/// which documents exist.
#[tokio::test]
async fn the_portal_downloads_only_the_customers_own_documents() {
    let site = Site::start(driver().await, true).await;
    let response = site.get("mere-1", &["portal"], &[("x-ic-party", "mere")]).await;
    assert_eq!(response.status().as_u16(), 200);
    let headers = response.headers().clone();
    assert_eq!(headers["content-type"], "application/pdf");
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(headers["content-disposition"], "attachment; filename=\"mere-1.pdf\"");
    assert!(response.bytes().await.unwrap().starts_with(b"%PDF-"));

    // The adviser it was copied to may have it too.
    assert_eq!(site.status("mere-1", &["portal"], &[("x-ic-party", "tama")]).await, 200);
    // Someone else's, and one that does not exist, look the same.
    let other = site.get("hemi.1", &["portal"], &[("x-ic-party", "mere")]).await;
    assert_eq!(other.status().as_u16(), 404);
    let missing = site.get("nothing", &["portal"], &[("x-ic-party", "mere")]).await;
    assert_eq!(missing.status().as_u16(), 404);
    assert_eq!(other.json::<Value>().await.unwrap(), missing.json::<Value>().await.unwrap());
    // The portal must say whom it asks for.
    assert_eq!(site.status("mere-1", &["portal"], &[]).await, 400);
    // A name that cannot be a file name is made into one.
    let response = site.get("hemi.1", &["portal"], &[("x-ic-party", "hemi")]).await;
    assert_eq!(response.headers()["content-disposition"], "attachment; filename=\"hemi_1.pdf\"");
}

/// Purpose: staff download a document only as a named person for a stated
/// purpose, never a clinical one, and no other role may download at all.
/// Why it matters: staff can reach every customer's documents, so each
/// download must say who and why for the privacy audit. Clinical records
/// are kept to the claims team's own tools, and a service holding some
/// other role (the webhook, a batch job) has no reason to read letters.
#[tokio::test]
async fn staff_say_who_and_why_and_other_roles_are_refused() {
    let site = Site::start(driver().await, true).await;
    let named = [("x-ic-actor", "kiri@insurer"), ("x-ic-purpose", "complaint")];
    assert_eq!(site.status("hemi.1", &["staff"], &named).await, 200);
    assert_eq!(site.status("hemi.1", &["staff"], &named[..1]).await, 400);
    assert_eq!(site.status("hemi.1", &["staff"], &named[1..]).await, 400);
    assert_eq!(site.status("mere-clinical", &["staff"], &named).await, 403);
    // The portal reaches the customer's own clinical document.
    assert_eq!(site.status("mere-clinical", &["portal"], &[("x-ic-party", "mere")]).await, 200);
    assert_eq!(site.status("hemi.1", &["batch"], &named).await, 403);
    // A token with several roles names the one it acts as.
    assert_eq!(site.status("hemi.1", &["staff", "portal"], &named).await, 401);
    let as_staff = [named[0], named[1], ("x-ic-caller", "staff")];
    assert_eq!(site.status("hemi.1", &["staff", "portal"], &as_staff).await, 200);
}

/// Purpose: without a valid Entra token the route refuses, and a document
/// destroyed under the retention or privacy rules is gone, not an error.
/// Why it matters: an unsigned or forged request reaching a letter would
/// be a breach; a destroyed document answered as a fault would page the
/// support team for the system working as designed.
#[tokio::test]
async fn tokens_are_required_and_destroyed_documents_are_gone() {
    let driver = driver().await;
    let site = Site::start(driver.clone(), true).await;
    assert_eq!(site.status("hemi.1", &[], &[("x-ic-party", "hemi")]).await, 401);
    let forged = site
        .http
        .get(format!("{}/v1/documents/hemi.1/pdf", site.base))
        .bearer_auth("not.a.token")
        .header("x-ic-party", "hemi")
        .send()
        .await
        .unwrap();
    assert_eq!(forged.status().as_u16(), 401);

    let shred = ic_documents::Envelope {
        key: "shred".into(),
        caller: "privacy".into(),
        actor: "privacy-officer".into(),
        at: (driver.clock)(),
        command: Command::ShredParty { party: party("hemi") },
    };
    assert!(driver.module.propose(shred).await.unwrap().result.is_ok());
    assert_eq!(site.status("hemi.1", &["portal"], &[("x-ic-party", "hemi")]).await, 410);
}

/// Purpose: while the documents module, the vault or the tenant's signing
/// keys cannot be reached the route answers "unavailable", a document the
/// typeface cannot set is a server fault, and with downloads not
/// configured the route does not exist.
/// Why it matters: an outage must read as one, so the portal tells the
/// customer to try later instead of that the letter is missing; and a
/// deployment without Entra settings must not serve documents unchecked.
#[tokio::test]
async fn outages_are_unavailable_and_an_unconfigured_route_is_absent() {
    let driver = driver().await;
    let site = Site::start(driver.clone(), true).await;
    driver.module.set_down(true);
    assert_eq!(site.status("hemi.1", &["portal"], &[("x-ic-party", "hemi")]).await, 503);
    driver.module.set_down(false);

    // A tenant whose keys cannot be fetched.
    let routes = router_with(
        driver.clone(),
        KEY,
        Some(Downloads {
            entra: Entra::new("http://127.0.0.1:9", TENANT, AUDIENCE),
            fonts: Fonts::bundled(),
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(serve(listener, routes, std::future::pending()));
    let response = reqwest::Client::new()
        .get(format!("{base}/v1/documents/hemi.1/pdf"))
        .bearer_auth(token("http://127.0.0.1:9", &["portal"]))
        .header("x-ic-party", "hemi")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 503);

    // The vault cannot be read: unavailable, not missing.
    driver.objects.set_down(true);
    assert_eq!(site.status("hemi.1", &["portal"], &[("x-ic-party", "hemi")]).await, 503);
    driver.objects.set_down(false);
    // A document the typeface cannot set is a fault on our side.
    assert_eq!(site.status("wang-1", &["portal"], &[("x-ic-party", "wang")]).await, 500);

    let off = Site::start(driver, false).await;
    assert_eq!(off.status("hemi.1", &["portal"], &[("x-ic-party", "hemi")]).await, 404);
}

fn settings() -> BTreeMap<&'static str, String> {
    BTreeMap::from([
        ("IC_CLUSTER_NODES", "node-0:7000".to_string()),
        ("IC_DOCUMENTS_VAULT_ENDPOINT", "https://stdocs.blob.core.windows.net".to_string()),
        ("IC_DOCUMENTS_KEY_VAULT", "https://kv-docs.vault.azure.net".to_string()),
        ("IC_COMMUNICATION_ENDPOINT", "https://acs-docs.communication.azure.com".to_string()),
        ("IC_DOCUMENTS_EMAIL_FROM", "notices@insurer.example".to_string()),
        ("IC_DOCUMENTS_SMS_FROM", "+6421000000".to_string()),
        ("IC_CUSTOMER_MASTER_URL", "https://customers.internal".to_string()),
        ("IC_CUSTOMER_MASTER_SCOPE", "api://customer-master/.default".to_string()),
        ("IC_EVENT_HUBS_NAMESPACE", "https://evh-core.servicebus.windows.net".to_string()),
        ("IC_DOCUMENTS_WEBHOOK_KEY", KEY.to_string()),
    ])
}

/// Purpose: the Entra and typeface settings are each optional but come in
/// pairs, the authority defaults to Entra's public one, and a set
/// typeface is read and checked before the service starts.
/// Why it matters: a tenant without an audience would accept tokens meant
/// for any service; a regular face without its bold would set headings in
/// the wrong font on every letter; and a font file that is missing or not
/// a usable TrueType face must stop the deployment, not every PDF after it.
#[test]
fn download_and_typeface_settings_come_in_pairs() {
    let env = settings();
    let config = Config::from_lookup(|k| env.get(k).cloned()).unwrap();
    assert_eq!((config.entra, config.fonts), (None, None));

    let mut set = env.clone();
    set.insert("IC_ENTRA_TENANT", TENANT.into());
    set.insert("IC_DOCUMENTS_AUDIENCE", AUDIENCE.into());
    set.insert("IC_DOCUMENTS_FONT_REGULAR", "/fonts/r.ttf".into());
    set.insert("IC_DOCUMENTS_FONT_BOLD", "/fonts/b.ttf".into());
    let config = Config::from_lookup(|k| set.get(k).cloned()).unwrap();
    let entra = config.entra.unwrap();
    assert_eq!(
        (entra.authority.as_str(), entra.tenant.as_str(), entra.audience.as_str()),
        ("https://login.microsoftonline.com", TENANT, AUDIENCE)
    );
    assert_eq!(config.fonts, Some(("/fonts/r.ttf".into(), "/fonts/b.ttf".into())));
    set.insert("IC_ENTRA_AUTHORITY", "https://login.example".into());
    assert_eq!(
        Config::from_lookup(|k| set.get(k).cloned()).unwrap().entra.unwrap().authority,
        "https://login.example"
    );

    for alone in [
        "IC_ENTRA_TENANT",
        "IC_DOCUMENTS_AUDIENCE",
        "IC_DOCUMENTS_FONT_REGULAR",
        "IC_DOCUMENTS_FONT_BOLD",
    ] {
        let mut half = env.clone();
        half.insert(alone, "x".into());
        assert!(Config::from_lookup(|k| half.get(k).cloned()).unwrap_err().contains("together"), "{alone}");
    }

    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/fonts");
    let regular = format!("{dir}/LiberationSans-Regular.ttf");
    let bold = format!("{dir}/LiberationSans-Bold.ttf");
    assert!(load_fonts(None).is_ok());
    assert!(load_fonts(Some(&(regular.clone(), bold.clone()))).is_ok());
    assert!(load_fonts(Some(&(regular.clone(), format!("{dir}/missing.ttf"))))
        .unwrap_err()
        .contains("missing.ttf"));
    let not_a_font = format!("{dir}/OFL.txt");
    assert!(load_fonts(Some(&(not_a_font, bold))).unwrap_err().contains("typeface"));
}

fn signed(authority: &str, roles: &[&str], kid: Option<&str>) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let claims = json!({ "iss": format!("{authority}/{TENANT}/v2.0"), "aud": AUDIENCE, "exp": now + 600, "sub": "workload", "roles": roles });
    let mut header = Header::new(Algorithm::RS256);
    header.kid = kid.map(str::to_string);
    encode(&header, &claims, &signing().key).unwrap()
}

/// Purpose: the token check names the role a token acts as, falls back to
/// the subject when there is no object ID, and refuses a token with no
/// role, a role it does not carry, no key ID, or a key the tenant does
/// not publish.
/// Why it matters: the role decides what a caller may download. A token
/// read as holding a role it lacks, or one accepted without a key the
/// tenant vouches for, would let any workload in the tenant read letters.
#[tokio::test]
async fn the_token_check_refuses_what_it_cannot_vouch_for() {
    use ic_documents::auth::{AuthError, Caller};
    let authority = tenant().await;
    let entra = Entra::new(&format!("{authority}/"), TENANT, AUDIENCE);
    let check = |token: String, caller: Option<&'static str>| {
        let entra = entra.clone();
        async move {
            let mut headers = axum::http::HeaderMap::new();
            headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
            if let Some(caller) = caller {
                headers.insert("x-ic-caller", caller.parse().unwrap());
            }
            entra.authenticate(&headers).await
        }
    };
    assert_eq!(
        check(signed(&authority, &["portal"], Some("k1")), None).await,
        Ok(Caller {
            role: "portal".into(),
            subject: "workload".into()
        })
    );
    let refused = |r: Result<Caller, AuthError>| match r {
        Err(AuthError::Invalid(why)) => why,
        other => panic!("expected a refusal, got {other:?}"),
    };
    assert!(refused(check(signed(&authority, &[], Some("k1")), None).await).contains("no role"));
    assert!(refused(check(signed(&authority, &["portal"], Some("k1")), Some("staff")).await).contains("staff"));
    assert!(refused(check(signed(&authority, &["portal"], None), None).await).contains("key id"));
    assert!(refused(check(signed(&authority, &["portal"], Some("k9")), None).await).contains("unknown key"));
    assert_eq!(entra.authenticate(&axum::http::HeaderMap::new()).await, Err(AuthError::Missing));
}
