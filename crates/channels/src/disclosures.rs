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

use ic_telemetry::Traced;
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
    let Some(given) = body["answers"].as_object() else {
        return Err("the answers are an object by question id, each [version, answer]: {\"heart_condition\": [2, {\"yes_no\": false}]}".to_string());
    };
    // Each answer is the question's version and one kind of answer; said here, as the intake would refuse it without words.
    const KINDS: [&str; 5] = ["yes_no", "number", "choice", "text", "money"];
    for (id, answer) in given {
        let fits = answer.as_array().is_some_and(|pair| {
            pair.len() == 2
                && pair[0].is_u64()
                && pair[1]
                    .as_object()
                    .is_some_and(|a| a.len() == 1 && a.keys().all(|k| KINDS.contains(&k.as_str())))
        });
        if !fits {
            return Err(format!(
                "the answer to {id} is [its version, {{kind: value}}], such as [2, {{\"yes_no\": true}}], [2, {{\"number\": 172}}], [2, {{\"choice\": \"none\"}}], [2, {{\"money\": 9500000}}] (cents) or [2, {{\"text\": \"...\"}}]: disclosure_questions gives each question's version and kind"
            ));
        }
    }
    if !body
        .get("adviser_entered")
        .is_none_or(|a| a.as_array().is_some_and(|ids| ids.iter().all(Value::is_string)))
    {
        return Err("adviser_entered lists the ids of the questions the adviser typed, or is left out".to_string());
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

/// The answers a life gives to an offer of terms, as the underwriting module names them.
pub const OFFER_RESPONSES: [&str; 3] = ["accept", "decline", "counter"];

/// The exact bytes a life signs to answer an offer of terms, as the
/// underwriting module checks them: `ic.underwriting.offer.v1`, the
/// proposal, the life, the offer and the answer, joined by zero bytes.
pub fn offer_message(proposal: &str, life: &str, offer: &str, response: &str) -> Vec<u8> {
    let mut m = b"ic.underwriting.offer.v1".to_vec();
    for part in [proposal, life, offer, response] {
        m.push(0);
        m.extend_from_slice(part.as_bytes());
    }
    m
}

/// The `respond_to_offer` command for a life's answer, from the page's body
/// (`{response, signature}`), with the enrolled device's signature over
/// [`offer_message`] checked here against its key.
pub fn offer_answer(proposal: &str, life: &str, offer: &str, body: &Value, device: &str, public_key: &str) -> Result<Value, String> {
    let response = body["response"]
        .as_str()
        .filter(|r| OFFER_RESPONSES.contains(r))
        .ok_or("answer accept, decline or counter")?;
    let signature = body["signature"].as_str().ok_or("send the device's signature")?;
    if !crate::identity::verify(public_key, &offer_message(proposal, life, offer, response), signature) {
        return Err("the signature is not this device's over the answer".to_string());
    }
    Ok(json!({ "respond_to_offer": {
        "proposal": proposal, "life": life, "offer": offer, "response": response,
        "signed": { "key_id": device, "public_key": public_key, "signature": signature, "alg": "ecdsa-p256" },
    } }))
}

/// The exact bytes the applicant signs to accept a new price after the
/// product changed, as the underwriting module checks them:
/// `ic.underwriting.requote.v1`, the proposal, the product's content hash and
/// the premium in cents, joined by zero bytes.
pub fn requote_message(proposal: &str, product_hash: &str, premium_minor: i64) -> Vec<u8> {
    let mut m = b"ic.underwriting.requote.v1".to_vec();
    for part in [proposal, product_hash, &premium_minor.to_string()] {
        m.push(0);
        m.extend_from_slice(part.as_bytes());
    }
    m
}

/// A period's premium in cents from the core's dry run (a `ModalPremium`):
/// every cover, the policy fee, tax and levies.
pub fn premium_total(premium: &Value) -> Result<i64, String> {
    let premium: ic_product::rating::ModalPremium = serde_json::from_value(premium.clone()).map_err(|e| format!("the core's quote was not understood: {e}"))?;
    Ok(premium
        .by_cover
        .values()
        .chain(premium.gst.values())
        .chain(premium.levies.values())
        .map(|m| m.minor)
        .sum::<i64>()
        + premium.policy_fee.minor)
}

/// One cover's terms in an offer, as the life reads them: what is offered,
/// any extra premium, and what is not covered, with each exclusion's kind
/// from the code list in force (`categories`, by code).
pub fn offered_terms(terms: &Value, categories: &Value) -> Vec<Value> {
    let kind = |code: &str| match categories[code]["category"].as_str().unwrap_or_default() {
        "condition" => "a medical condition",
        "body_system" => "a part of the body",
        "mental_health" => "mental health",
        "pursuit" => "a sport or pastime",
        "occupation" => "your work",
        "travel" => "travel or living overseas",
        "pre_existing" => "a condition you already have",
        _ => "a clause in your offer letter",
    };
    terms
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|t| {
            let o = &t["outcome"];
            let loading = o["loading_percent"].as_u64().unwrap_or(0);
            let exclusions: Vec<Value> = o["exclusions"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .filter_map(|e| e["code"].as_str())
                .map(|code| json!({ "code": code, "about": format!("Not covered: {}", kind(code)) }))
                .collect();
            let said = match o["outcome"].as_str().unwrap_or_default() {
                "accept" if loading == 0 && exclusions.is_empty() => "Offered at standard rates".to_string(),
                "accept" if loading == 0 => "Offered with an exclusion".to_string(),
                "accept" => format!("Offered with an extra {loading}% on its premium"),
                "decline" => "Not offered".to_string(),
                "defer" => format!("Not offered now: you may apply again after {}", o["until"].as_str().unwrap_or_default()),
                _ => "Offered on terms in your offer letter".to_string(),
            };
            let mut out = json!({ "cover": t["cover"], "outcome": o["outcome"], "said": said, "loading_percent": loading, "exclusions": exclusions });
            if let Some(revised) = o["revised_sum_insured_minor"].as_i64() {
                out["revised_sum_insured_minor"] = json!(revised);
            }
            out
        })
        .collect()
}

/// The new quote a requote binds the proposal to, from the core's dry run
/// of the policy it would issue as (an `ModalPremium`) and the partition's
/// register on the day: the product version in use today, the rate tables
/// in use for the proposal's group (global or scoped to it), and the first
/// period's premium with tax and levies, in cents.
pub fn requote_binding(premium: &Value, register: &[Value], product: &str, group: &str, today: chrono::NaiveDate) -> Result<Value, String> {
    let total = premium_total(premium)?;
    let in_use = |kind: &'static str| register.iter().filter(move |r| r["kind"] == kind && r["in_use"] == true);
    let product_hash = in_use("product")
        .find(|r| r["id"] == product)
        .and_then(|r| r["content_hash"].as_str())
        .ok_or_else(|| format!("no version of {product} is in use today"))?;
    let rate_tables: Vec<Value> = in_use("rate_table")
        .filter(|r| r["scope"].as_array().is_none_or(|groups| groups.iter().any(|g| g == group)))
        .map(|r| json!({ "id": r["id"], "version": r["version"], "content_hash": r["content_hash"] }))
        .collect();
    Ok(json!({ "product_hash": product_hash, "rate_tables": rate_tables, "premium_minor": total, "quoted_on": today }))
}

/// The `requote` command for the applicant's acceptance of `binding`, from
/// the page's body (`{product_hash, premium_minor, signature}`): what the
/// page shows must still be the price, and the enrolled device's signature
/// over [`requote_message`] is checked here against its key.
pub fn requote(proposal: &str, binding: &Value, body: &Value, device: &str, public_key: &str) -> Result<Value, String> {
    let (hash, premium) = (
        binding["product_hash"].as_str().unwrap_or_default(),
        binding["premium_minor"].as_i64().unwrap_or_default(),
    );
    if body["product_hash"] != hash || body["premium_minor"] != premium {
        return Err("the price has changed again since you looked: look again before accepting".to_string());
    }
    let signature = body["signature"].as_str().ok_or("send the device's signature")?;
    if !crate::identity::verify(public_key, &requote_message(proposal, hash, premium), signature) {
        return Err("the signature is not this device's over the new price".to_string());
    }
    Ok(json!({ "requote": {
        "proposal": proposal, "quote": binding,
        "signed": { "key_id": device, "public_key": public_key, "signature": signature, "alg": "ecdsa-p256" },
    } }))
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

/// The intake's answer as JSON. A refusal it gave as plain text, as a
/// request it could not read is, keeps its words as the message.
pub fn answered(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap_or_else(|_| {
        let text = String::from_utf8_lossy(bytes).trim().to_string();
        if text.is_empty() {
            Value::Null
        } else {
            json!({ "message": text })
        }
    })
}

/// A refusal of the answers in words: each problem the intake named, with
/// the question's own words where the channel holds them.
pub fn answer_problems(body: &mut Value) {
    let Some(problems) = body["problems"].as_array().filter(|p| !p.is_empty()) else {
        return;
    };
    let said: Vec<String> = problems
        .iter()
        .map(|p| {
            let id = p["question"].as_str().unwrap_or_default();
            let question =
                crate::questions::wording(id, p["expected"].as_u64().unwrap_or(0)).map_or_else(|| id.replace('_', " "), |(text, _)| format!("\"{text}\""));
            match p["problem"].as_str().unwrap_or_default() {
                "missing" => format!("{question} needs an answer"),
                "unknown_question" => format!("{id} is not one of the questions asked"),
                "wrong_version" => format!("{question} is version {}", p["expected"]),
                "out_of_range" => format!("the answer to {question} is outside what it takes"),
                _ => format!("the answer to {question} is not the kind it takes"),
            }
        })
        .collect();
    body["message"] = json!(format!("Some answers need attention: {}.", said.join("; ")));
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
            .traced()
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = response.status().as_u16();
        if status == 503 {
            return Err("the underwriting intake asked us to try again".to_string());
        }
        Ok((status, answered(&response.bytes().await.unwrap_or_default())))
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
