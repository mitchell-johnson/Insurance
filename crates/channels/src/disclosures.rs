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

/// The intake service, reached with the portal's own Entra token for the
/// intake's audience, which carries the app role `new-business`.
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
}
