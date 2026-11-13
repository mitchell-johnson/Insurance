//! Signing a disclosure in the app: the person insured answers the
//! questions, the underwriting intake (`docs/underwriting.md`, "The
//! disclosure intake") says exactly what to sign, the app signs it with its
//! enrolled device key, and the intake seals the answers in the clinical
//! store and tells the underwriting module.
//!
//! The portal adds what only it knows: that the person signed in is the
//! life the answers are for (from the `proposal_life` row underwriting
//! publishes, whose subject is that person), and that the signature comes
//! from a device they enrolled. It checks the signature itself before
//! anything leaves, and passes the device's enrolled key so that the intake
//! and the module can check it again. Answers pass through and are never
//! kept or logged here.

use std::future::Future;
use std::pin::Pin;

use base64::Engine;
use serde_json::{json, Value};

/// What the intake answered: its HTTP status and body.
pub type Answered = (u16, Value);

type Asked<'a> = Pin<Box<dyn Future<Output = Result<Answered, String>> + Send + 'a>>;

/// The steps a life signs through the app.
pub const STEPS: [&str; 2] = ["disclosure", "interview"];

/// The underwriting intake, as the portal calls it.
pub trait Intake: Send + Sync {
    /// What to sign for these answers (`/v1/disclosures/to-sign`).
    fn to_sign(&self, body: Value) -> Asked<'_>;
    /// The signed answers (`/v1/disclosures`).
    fn submit(&self, body: Value) -> Asked<'_>;
    /// An underwriter's reading of a life's disclosures
    /// (`/v1/disclosures/open`), which the module allows and records first.
    fn open(&self, body: Value) -> Asked<'_>;
}

/// The intake's request from the page's body, for this proposal and life.
/// Only the fields the intake takes are passed on.
pub fn answers(proposal: &str, life: &str, body: &Value) -> Result<Value, String> {
    let step = body["step"]
        .as_str()
        .filter(|s| STEPS.contains(s))
        .ok_or("the step is disclosure or interview")?;
    if !body["answers"].is_object() {
        return Err("the answers are an object of question IDs".to_string());
    }
    Ok(json!({
        "proposal": proposal, "life": life, "step": step, "answers": body["answers"],
        "adviser_entered": body.get("adviser_entered").cloned().unwrap_or_else(|| json!([])),
    }))
}

/// The signed request, once the device's signature over `message` checks
/// out against its enrolled key. Refused if it does not.
pub fn signed(mut request: Value, body: &Value, device: &str, public_key: &str) -> Result<Value, String> {
    let message = body["message"].as_str().ok_or("send the message you signed, as the intake gave it")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(message)
        .map_err(|_| "the message is not base64".to_string())?;
    let signature = body["signature"].as_str().ok_or("send the device's signature")?;
    if !crate::identity::verify(public_key, &bytes, signature) {
        return Err("the signature is not this device's over that message".to_string());
    }
    request["signature"] = json!({ "key_id": device, "public_key": public_key, "signature": signature, "alg": "ecdsa-p256" });
    Ok(request)
}

/// The consents a life gives or withdraws on a proposal, as the
/// underwriting module names them.
pub const CONSENTS: [&str; 5] = [
    "adviser_access",
    "existing_cover_prefill",
    "obtain_records",
    "employer_sees_loading",
    "replacement_disclosure",
];

/// The consents given only with the life's signed step: letting an adviser
/// help with the answers, letting the employer see a loading (which implies
/// health), and confirming a replacement disclosure.
pub const SIGNED_CONSENTS: [&str; 3] = ["adviser_access", "employer_sees_loading", "replacement_disclosure"];

/// The exact bytes a life signs to give a consent, as the underwriting
/// module checks them: `ic.underwriting.consent.v1`, the proposal, the life
/// and the kind, joined by zero bytes.
pub fn consent_message(proposal: &str, life: &str, kind: &str) -> Vec<u8> {
    let mut m = b"ic.underwriting.consent.v1".to_vec();
    for part in [proposal, life, kind] {
        m.push(0);
        m.extend_from_slice(part.as_bytes());
    }
    m
}

/// The `record_consent` command for a life's consent, from the page's body
/// (`{granted, signature}`). Giving a consent that needs the life's signed
/// step needs the enrolled device's signature over [`consent_message`],
/// checked here against its key; `signer` is that device and key.
pub fn consent(proposal: &str, life: &str, kind: &str, body: &Value, signer: Option<(&str, &str)>) -> Result<Value, String> {
    if !CONSENTS.contains(&kind) {
        return Err(format!("{kind} is not a consent a life gives"));
    }
    let granted = body["granted"].as_bool().ok_or("say whether you give it (granted: true or false)")?;
    let signed = if granted && SIGNED_CONSENTS.contains(&kind) {
        let (device, public_key) = signer.ok_or("this consent is given from your enrolled device")?;
        let signature = body["signature"].as_str().ok_or("send the device's signature")?;
        if !crate::identity::verify(public_key, &consent_message(proposal, life, kind), signature) {
            return Err("the signature is not this device's over the consent".to_string());
        }
        json!({ "key_id": device, "public_key": public_key, "signature": signature, "alg": "ecdsa-p256" })
    } else {
        Value::Null
    };
    Ok(json!({ "record_consent": { "proposal": proposal, "life": life, "kind": kind, "granted": granted, "signed": signed } }))
}

/// The intake service, reached with the portal's own Entra token for the
/// intake's audience, which carries the app roles `new-business` (a life's
/// signed answers) and `underwriting-console` (an underwriter's reading).
pub struct Hosted {
    pub base: String,
    pub scope: String,
    pub credential: ic_azure::credential::AzureCredential,
    pub http: reqwest::Client,
}

impl Hosted {
    async fn call(&self, path: &str, body: Value) -> Result<Answered, String> {
        let token = self.credential.token(&self.scope).await.map_err(|e| e.to_string())?;
        let response = self
            .http
            .post(format!("{}{path}", self.base.trim_end_matches('/')))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = response.status().as_u16();
        if status == 503 {
            return Err("the underwriting intake asked us to try again".to_string());
        }
        Ok((status, response.json::<Value>().await.unwrap_or(Value::Null)))
    }
}

impl Intake for Hosted {
    fn to_sign(&self, body: Value) -> Asked<'_> {
        Box::pin(self.call("/v1/disclosures/to-sign", body))
    }

    fn submit(&self, body: Value) -> Asked<'_> {
        Box::pin(self.call("/v1/disclosures", body))
    }

    fn open(&self, body: Value) -> Asked<'_> {
        Box::pin(self.call("/v1/disclosures/open", body))
    }
}
