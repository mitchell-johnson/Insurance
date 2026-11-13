//! The customer master's HTTP API.
//!
//! Every route authenticates the calling service from its Entra ID token,
//! takes the purpose the caller declares (`x-ic-purpose`, or `purpose` in
//! the query) and the person it acts for (`x-ic-actor`, and `x-ic-party`
//! when that person is a party, as the portal sends), asks the access
//! policy, and writes the decision to the audit trail. Values are released
//! field by field; anything not released is withheld or masked.
//!
//! Routes, all under `/v1`:
//!
//! | Route | What |
//! |---|---|
//! | `POST /parties/match` | Is this person known? Optionally create them. |
//! | `POST /parties` | Create a party, refusing likely duplicates unless told. |
//! | `GET /parties/{id}` | The party's facts released to the caller, on either timeline. |
//! | `GET /parties/{id}/history` | Every statement of the released fields, with both dates. |
//! | `POST /parties/{id}/commands` | Change a party (`If-Match` for the version; `Idempotency-Key`). |
//! | `GET /parties/{id}/contact-points` | Where to send a notice on a channel. |
//! | `GET /parties/{id}/relationships` | The party's relationships. |
//! | `GET /relationships?target=policy:P1` | Who is related to a policy, scheme or party. |
//! | `GET /parties/{id}/routing` | Whether a request should go to a person first. |
//! | `GET /parties/{id}/export` | Everything held on the party (access requests). |
//! | `GET /parties/{id}/audit` | Who read or changed the party. |
//! | `POST /parties/search` | Fuzzy search by name and date of birth. |
//! | `POST /merges`, `POST /unmerges` | Merge a duplicate, or undo it. |
//! | `GET /reviews`, `POST /reviews/{id}/dismiss` | Possible duplicates for a person. |
//! | `POST /admin/verify/{id}`, `POST /admin/rewrap`, `POST /admin/reshred`, `GET /admin/index` | Operations. |

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, NaiveDate, Utc};
use hmac::{Hmac, Mac};
use ic_azure::blob::WormStore;
use ic_azure::keywrap::KeyWrapper;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Sha256;

use crate::access::{mask, Access, Ask};
use crate::auth::{AuthError, Entra};
use crate::matching::{self, Probe, Resolution};
use crate::model::{Field, PartyId, PartyKind, Target};
use crate::party::{Change, FactInput, Party, Reject};
use crate::retention::Register;
use crate::store::{AuditLine, Idempotency, Outcome, Store, StoreError, Who};

/// The service's parts.
pub struct App<W, S> {
    pub store: Store<W>,
    pub access: Access,
    pub auth: Entra,
    pub register: Register<S>,
}

type Answer = (StatusCode, Json<Value>);
/// A handler's answer: refusals travel as `Err`, so `?` ends a handler early.
type Reply = Result<Answer, Answer>;

fn refused(status: StatusCode, code: &str, message: &str) -> Answer {
    (status, Json(json!({ "error": { "code": code, "message": message } })))
}

fn rejected(r: &Reject) -> Answer {
    let status = match r.code.as_str() {
        "not_found" => StatusCode::NOT_FOUND,
        "shredded" => StatusCode::GONE,
        "version_conflict" | "conflict" | "idempotency_conflict" | "already_deceased" | "merged" => StatusCode::CONFLICT,
        _ => StatusCode::UNPROCESSABLE_ENTITY,
    };
    refused(status, &r.code, &r.message)
}

fn failed(e: StoreError) -> Answer {
    match e {
        StoreError::Refused(r) => rejected(&r),
        StoreError::Unavailable(m) => {
            tracing::warn!(error = %m, "a dependency is unavailable");
            refused(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "try again shortly")
        }
        StoreError::Integrity(m) => {
            tracing::error!(alert = true, error = %m, "stored data failed an integrity check");
            refused(StatusCode::INTERNAL_SERVER_ERROR, "integrity", "stored data failed an integrity check")
        }
    }
}

fn forbidden() -> Answer {
    refused(StatusCode::FORBIDDEN, "forbidden", "the caller may not do this for this purpose")
}

/// The caller of one request.
struct Ctx {
    who: Who,
    purpose: String,
    /// The party the caller acts for, as the portal says.
    acting: Option<PartyId>,
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok()).map(str::trim).filter(|v| !v.is_empty())
}

fn clean(text: &str) -> Option<String> {
    let ok = (1..=128).contains(&text.len()) && text.chars().all(|c| c.is_ascii_alphanumeric() || "_-:.@".contains(c));
    ok.then(|| text.to_string())
}

impl<W: KeyWrapper, S: WormStore> App<W, S> {
    async fn ctx(&self, headers: &HeaderMap, purpose: Option<&str>) -> Result<Ctx, Answer> {
        let caller = match self.auth.authenticate(headers).await {
            Ok(c) => c,
            Err(AuthError::Keys(e)) => {
                tracing::warn!(error = %e, "Entra signing keys unavailable");
                return Err(refused(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "try again shortly"));
            }
            Err(e) => return Err(refused(StatusCode::UNAUTHORIZED, "unauthorised", &e.to_string())),
        };
        let purpose = header(headers, "x-ic-purpose").or(purpose).unwrap_or("unspecified");
        let purpose = clean(purpose).ok_or_else(|| refused(StatusCode::BAD_REQUEST, "invalid", "the purpose is not well formed"))?;
        let actor = match header(headers, "x-ic-actor") {
            Some(a) => clean(a).ok_or_else(|| refused(StatusCode::BAD_REQUEST, "invalid", "x-ic-actor is not well formed"))?,
            None => caller.subject.clone(),
        };
        let acting = match header(headers, "x-ic-party") {
            Some(p) if PartyId::plausible(p) => Some(PartyId(p.to_string())),
            Some(_) => return Err(refused(StatusCode::BAD_REQUEST, "invalid", "x-ic-party is not a party ID")),
            None => None,
        };
        Ok(Ctx {
            who: Who { caller: caller.role, actor },
            purpose,
            acting,
        })
    }

    fn ask(&self, ctx: &Ctx) -> Ask {
        Ask {
            caller: ctx.who.caller.clone(),
            purpose: ctx.purpose.clone(),
            ..Ask::default()
        }
    }

    /// The ask for a request about `party`: whether the actor is the party,
    /// and the roles the actor holds to anything the party is related to.
    async fn ask_about(&self, ctx: &Ctx, party: &PartyId) -> Result<Ask, Answer> {
        let mut ask = self.ask(ctx);
        let Some(acting) = &ctx.acting else {
            return Ok(ask);
        };
        let acting = self.store.resolve(acting).await.map_err(failed)?;
        if acting.as_ref() == Some(party) {
            ask.self_access = true;
            return Ok(ask);
        }
        if let Some(acting) = acting {
            let today = Utc::now().date_naive();
            let theirs = self.store.relationships_of(party).await.map_err(failed)?;
            let targets: Vec<Target> = theirs
                .iter()
                .filter(|r| r.relationship.in_force(today))
                .map(|r| r.relationship.target.clone())
                .collect();
            let mine = self.store.relationships_of(&acting).await.map_err(failed)?;
            let mut roles: Vec<String> = mine
                .iter()
                .filter(|r| r.relationship.in_force(today) && targets.contains(&r.relationship.target))
                .map(|r| r.relationship.kind.as_str().to_string())
                .collect();
            roles.sort();
            roles.dedup();
            ask.actor_roles = roles;
        }
        Ok(ask)
    }

    async fn audit(&self, ctx: &Ctx, party: Option<&PartyId>, action: &str, fields: Vec<String>, decision: &str) -> Result<(), Answer> {
        let break_glass = ctx.who.caller == "break-glass";
        if break_glass {
            tracing::error!(alert = true, actor = %ctx.who.actor, party = ?party, action, "break-glass access");
        }
        let line = AuditLine {
            caller: ctx.who.caller.clone(),
            actor: ctx.who.actor.clone(),
            purpose: ctx.purpose.clone(),
            party: party.cloned(),
            action: action.to_string(),
            fields,
            decision: decision.to_string(),
            policy_version: self.access.version().to_string(),
            break_glass,
        };
        // A read that cannot be logged is not served.
        self.store.audit(&line, Utc::now()).await.map_err(failed)
    }

    /// Refuse, and log the refusal.
    async fn deny(&self, ctx: &Ctx, party: Option<&PartyId>, action: &str) -> Answer {
        tracing::warn!(caller = %ctx.who.caller, purpose = %ctx.purpose, action, "access denied");
        match self.audit(ctx, party, action, vec![], "deny").await {
            Ok(()) => forbidden(),
            Err(answer) => answer,
        }
    }

    /// The party a path names, following merges, loaded.
    async fn find(&self, id: &str) -> Result<(PartyId, Party), Answer> {
        if !PartyId::plausible(id) {
            return Err(refused(StatusCode::NOT_FOUND, "not_found", "no such party"));
        }
        let resolved = self
            .store
            .resolve(&PartyId(id.to_string()))
            .await
            .map_err(failed)?
            .ok_or_else(|| refused(StatusCode::NOT_FOUND, "not_found", "no such party"))?;
        let party = self
            .store
            .load(&resolved)
            .await
            .map_err(failed)?
            .ok_or_else(|| refused(StatusCode::NOT_FOUND, "not_found", "no such party"))?;
        if party.shredded {
            return Err(refused(StatusCode::GONE, "shredded", "the party's record has been erased"));
        }
        Ok((resolved, party))
    }
}

// ---- Reading ---------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ReadQuery {
    #[serde(default)]
    purpose: Option<String>,
    /// What was true on this date. Defaults to today.
    #[serde(default)]
    as_of: Option<NaiveDate>,
    /// As we knew it at this time. Defaults to now.
    #[serde(default)]
    known_at: Option<DateTime<Utc>>,
}

async fn read<W: KeyWrapper, S: WormStore>(State(app): State<Arc<App<W, S>>>, Path(id): Path<String>, Query(q): Query<ReadQuery>, headers: HeaderMap) -> Reply {
    let ctx = app.ctx(&headers, q.purpose.as_deref()).await?;
    let (resolved, party) = app.find(&id).await?;
    let ask = app.ask_about(&ctx, &resolved).await?;
    let now = Utc::now();
    let facts = party.facts(q.as_of.unwrap_or(now.date_naive()), q.known_at.unwrap_or(now));
    let released = app.access.readable(&ask, Field::ALL);
    // A marker the caller may not see is left out without a trace: not
    // masked, not listed as withheld.
    let held: Vec<Field> = facts.keys().copied().filter(|f| *f != Field::Vulnerability || released.contains(f)).collect();
    if released.is_empty() {
        return Err(app.deny(&ctx, Some(&resolved), "read").await);
    }
    let mut out = BTreeMap::new();
    let mut masked = BTreeMap::new();
    let mut withheld = Vec::new();
    for f in held {
        let fact = &facts[&f];
        if released.contains(&f) {
            out.insert(
                f.as_str(),
                json!({ "value": fact.value, "valid_from": fact.valid_from, "recorded_at": fact.recorded_at }),
            );
        } else if let Some(m) = mask(f, &fact.value) {
            masked.insert(f.as_str(), m);
        } else {
            withheld.push(f.as_str());
        }
    }
    let fields: Vec<String> = out.keys().map(|k| k.to_string()).collect();
    app.audit(&ctx, Some(&resolved), "read", fields, "allow").await?;
    let consents = party.consents_on(now.date_naive());
    Ok((
        StatusCode::OK,
        Json(json!({
            "party": resolved,
            "asked_for": id,
            "kind": party.kind,
            "lifecycle": party.lifecycle,
            "group": party.group,
            "version": party.version,
            "facts": out,
            "masked": masked,
            "withheld": withheld,
            "consents": consents,
            "verified": party.verifications.iter().any(|v| v.outcome == crate::model::VerificationOutcome::Passed),
            "risk_rating": party.ratings.last().map(|r| r.rating),
            "legal_hold": !party.holds.is_empty(),
            "declined_corrections": party.declined.iter().filter(|d| released.contains(&d.field)).collect::<Vec<_>>(),
        })),
    ))
}

async fn history<W: KeyWrapper, S: WormStore>(
    State(app): State<Arc<App<W, S>>>,
    Path(id): Path<String>,
    Query(q): Query<ReadQuery>,
    headers: HeaderMap,
) -> Reply {
    let ctx = app.ctx(&headers, q.purpose.as_deref()).await?;
    let (resolved, party) = app.find(&id).await?;
    let ask = app.ask_about(&ctx, &resolved).await?;
    let released = app.access.readable(&ask, Field::ALL);
    if released.is_empty() {
        return Err(app.deny(&ctx, Some(&resolved), "history").await);
    }
    let statements: Vec<_> = party.assertions.iter().filter(|a| released.contains(&a.field)).collect();
    let mut fields: Vec<String> = statements.iter().map(|a| a.field.as_str().to_string()).collect();
    fields.sort();
    fields.dedup();
    app.audit(&ctx, Some(&resolved), "history", fields, "allow").await?;
    Ok((StatusCode::OK, Json(json!({ "party": resolved, "statements": statements }))))
}

#[derive(Debug, Deserialize)]
struct ContactQuery {
    #[serde(default)]
    purpose: Option<String>,
    channel: String,
    #[serde(default)]
    relationship: Option<String>,
}

/// The documents service's contract: `{name, destination}` for a channel,
/// or 404 when the party has none. Language and format ride along.
async fn contact_points<W: KeyWrapper, S: WormStore>(
    State(app): State<Arc<App<W, S>>>,
    Path(id): Path<String>,
    Query(q): Query<ContactQuery>,
    headers: HeaderMap,
) -> Reply {
    let ctx = app.ctx(&headers, q.purpose.as_deref()).await?;
    let fields: &[Field] = match q.channel.as_str() {
        "email" => &[Field::Email],
        "sms" => &[Field::MobilePhone],
        "post" => &[Field::PostalAddress, Field::ResidentialAddress],
        "in_app" | "push" => &[],
        _ => return Err(refused(StatusCode::BAD_REQUEST, "invalid", "channel is email, sms, post, in_app or push")),
    };
    if q.relationship.as_deref().is_some_and(|r| clean(r).is_none()) {
        return Err(refused(StatusCode::BAD_REQUEST, "invalid", "relationship is not well formed"));
    }
    let (resolved, party) = app.find(&id).await?;
    let ask = app.ask_about(&ctx, &resolved).await?;
    let names = [Field::PreferredName, Field::GivenName, Field::FamilyName, Field::OrganisationName];
    let wanted: Vec<Field> = names.iter().chain(fields).copied().collect();
    let released = app.access.readable(&ask, &wanted);
    if released.len() != wanted.len() {
        return Err(app.deny(&ctx, Some(&resolved), "contact_points").await);
    }
    let now = Utc::now();
    let facts = party.facts(now.date_naive(), now);
    let text = |f: Field| facts.get(&f).and_then(|v| v.value.as_str().map(str::to_string));
    let name = match party.kind {
        Some(PartyKind::Organisation) => text(Field::OrganisationName),
        _ => {
            let first = text(Field::PreferredName).or_else(|| text(Field::GivenName));
            text(Field::FamilyName).map(|family| first.map_or(family.clone(), |g| format!("{g} {family}")))
        }
    };
    let destination = match q.channel.as_str() {
        "in_app" | "push" => Some(resolved.0.clone()),
        _ => fields.iter().find_map(|f| {
            facts.get(f).map(|v| match &v.value {
                Value::Array(lines) => lines.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n"),
                other => other.as_str().unwrap_or_default().to_string(),
            })
        }),
    };
    let mut used: Vec<String> = released.iter().filter(|f| facts.contains_key(f)).map(|f| f.as_str().to_string()).collect();
    used.sort();
    app.audit(&ctx, Some(&resolved), "contact_points", used, "allow").await?;
    match (name, destination) {
        (Some(name), Some(destination)) => Ok((
            StatusCode::OK,
            Json(json!({
                "name": name,
                "destination": destination,
                "language": facts.get(&Field::PreferredLanguage).map(|f| f.value.clone()),
                "format": facts.get(&Field::PresentationFormat).map(|f| f.value.clone()),
                "deceased": party.lifecycle == Some(crate::model::Lifecycle::Deceased),
            })),
        )),
        _ => Err(refused(
            StatusCode::NOT_FOUND,
            "no_contact_point",
            "the party has no contact point on that channel",
        )),
    }
}

async fn relationships_of<W: KeyWrapper, S: WormStore>(State(app): State<Arc<App<W, S>>>, Path(id): Path<String>, headers: HeaderMap) -> Reply {
    let ctx = app.ctx(&headers, None).await?;
    let (resolved, _) = app.find(&id).await?;
    if !app.access.allows(&app.ask(&ctx), "relationships") {
        return Err(app.deny(&ctx, Some(&resolved), "relationships").await);
    }
    match app.store.relationships_of(&resolved).await {
        Ok(rows) => Ok((StatusCode::OK, Json(json!({ "party": resolved, "relationships": rows })))),
        Err(e) => Err(failed(e)),
    }
}

#[derive(Debug, Deserialize)]
struct TargetQuery {
    /// `policy:P1`, `scheme:S1` or `party:pty_...`.
    target: String,
}

async fn relationships_to<W: KeyWrapper, S: WormStore>(State(app): State<Arc<App<W, S>>>, Query(q): Query<TargetQuery>, headers: HeaderMap) -> Reply {
    let ctx = app.ctx(&headers, None).await?;
    let Some(target) = q.target.split_once(':').and_then(|(k, id)| Target::from_parts(k, id)) else {
        return Err(refused(StatusCode::BAD_REQUEST, "invalid", "target is policy:<id>, scheme:<id> or party:<id>"));
    };
    if !app.access.allows(&app.ask(&ctx), "relationships") {
        return Err(app.deny(&ctx, None, "relationships").await);
    }
    match app.store.relationships_to(&target).await {
        Ok(rows) => Ok((StatusCode::OK, Json(json!({ "target": target, "relationships": rows })))),
        Err(e) => Err(failed(e)),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RoutingQuery {
    /// What the person (or someone for them) is about to do: one of the
    /// representative scopes except `all`.
    #[serde(default)]
    act: Option<String>,
}

/// Acts that move value out or redirect it, where a marked person is paused
/// for a conversation or a specialist looks first.
const RISKY_ACTS: [&str; 5] = [
    "surrender",
    "change_beneficiary",
    "change_bank_details",
    "change_payment_method",
    "change_cover",
];

async fn routing<W: KeyWrapper, S: WormStore>(
    State(app): State<Arc<App<W, S>>>,
    Path(id): Path<String>,
    Query(q): Query<RoutingQuery>,
    headers: HeaderMap,
) -> Reply {
    let ctx = app.ctx(&headers, None).await?;
    if q.act
        .as_deref()
        .is_some_and(|a| !crate::model::REPRESENTATIVE_SCOPES.contains(&a) || a == "all")
    {
        return Err(refused(StatusCode::BAD_REQUEST, "invalid", "act is one of the representative scopes"));
    }
    let (resolved, party) = app.find(&id).await?;
    if !app.access.allows(&app.ask(&ctx), "routing") {
        return Err(app.deny(&ctx, Some(&resolved), "routing").await);
    }
    let marker = party.current(Field::Vulnerability, Utc::now()).filter(Value::is_object);
    let mut answer = json!({ "party": resolved, "route_to_person": marker.is_some() });
    let mut fields = vec![];
    if let Some(m) = &marker {
        // What to do differently is for anyone serving the person; the kind
        // only for those the policy lets read the marker.
        answer["adjustments"] = m["adjustments"].clone();
        answer["review_on"] = m["review_on"].clone();
        // For an act that moves value: a conversation with a trained person
        // first where the person asked for one, and a specialist's review
        // where the marker is a risk of financial abuse. Neither says why,
        // and neither is a refusal: the person's decision stands afterwards.
        if let Some(act) = q.act.as_deref().filter(|a| RISKY_ACTS.contains(a)) {
            let asked = m["adjustments"]
                .as_array()
                .is_some_and(|a| a.iter().any(|v| v == "pause_before_large_withdrawal"));
            answer["act"] = json!(act);
            answer["pause_for_conversation"] = json!(asked || m["kind"] == "financial_abuse_risk");
            answer["specialist_review"] = json!(m["kind"] == "financial_abuse_risk");
        }
        let ask = app.ask_about(&ctx, &resolved).await?;
        if !app.access.readable(&ask, &[Field::Vulnerability]).is_empty() {
            answer["kind"] = m["kind"].clone();
            fields.push(Field::Vulnerability.as_str().to_string());
        }
    }
    app.audit(&ctx, Some(&resolved), "routing", fields, "allow").await?;
    Ok((StatusCode::OK, Json(answer)))
}

#[derive(Debug, Deserialize)]
struct AuthorityQuery {
    /// The party acting for this one.
    actor: String,
    /// What they want to do: one of `REPRESENTATIVE_SCOPES` except `all`.
    act: String,
    /// The day it would happen. Defaults to today.
    #[serde(default)]
    on: Option<NaiveDate>,
    /// The act would benefit the actor: they become the beneficiary, the
    /// money goes to their account.
    #[serde(default)]
    benefits_actor: bool,
}

/// May `actor` do `act` for this party on `on`? `allow` within an
/// authority in force that covers it; `refuse` with no authority, before
/// it starts or after it ends, or outside its scope; `route` to a person
/// when the act would benefit the representative. Every answer is logged
/// with the actor.
async fn authority<W: KeyWrapper, S: WormStore>(
    State(app): State<Arc<App<W, S>>>,
    Path(id): Path<String>,
    Query(q): Query<AuthorityQuery>,
    headers: HeaderMap,
) -> Reply {
    let ctx = app.ctx(&headers, None).await?;
    if !crate::model::REPRESENTATIVE_SCOPES.contains(&q.act.as_str()) || q.act == "all" {
        return Err(refused(StatusCode::BAD_REQUEST, "invalid", "act is one of the representative scopes"));
    }
    if !PartyId::plausible(&q.actor) {
        return Err(refused(StatusCode::BAD_REQUEST, "invalid", "actor is a party ID"));
    }
    let (resolved, party) = app.find(&id).await?;
    if !app.access.allows(&app.ask(&ctx), "authority") {
        return Err(app.deny(&ctx, Some(&resolved), "authority").await);
    }
    let on = q.on.unwrap_or_else(|| Utc::now().date_naive());
    let target = Target::Party(resolved.clone());
    // Every representative relationship the actor has to the party that has
    // not ended, including one not yet activated.
    let named: Vec<_> = app
        .store
        .relationships_of(&PartyId(q.actor.clone()))
        .await
        .map_err(failed)?
        .into_iter()
        .map(|r| r.relationship)
        .filter(|r| r.kind.represents() && r.target == target && r.valid_to.is_none_or(|to| to > on))
        .collect();
    let held: Vec<_> = named.iter().filter(|r| r.in_force(on)).cloned().collect();
    let covers = |r: &&crate::model::Relationship| {
        r.attributes
            .get("scope")
            .and_then(Value::as_array)
            .is_some_and(|s| s.iter().any(|v| v == q.act.as_str() || v == "all"))
    };
    let (decision, reason, by) = match held.iter().find(covers) {
        // While capacity is in question no representative acts; the
        // question goes to a person (legal), whatever the authority says.
        _ if party.capacity_in_question && !named.is_empty() => ("route", "capacity_in_question", named.iter().find(covers).or(named.first())),
        Some(r) if q.benefits_actor && crate::model::SELF_BENEFIT_ACTS.contains(&q.act.as_str()) => ("route", "benefits_representative", Some(r)),
        Some(r) => ("allow", "within_scope", Some(r)),
        None if held.is_empty() => ("refuse", "no_authority", None),
        None => ("refuse", "outside_scope", held.first()),
    };
    app.audit(&ctx, Some(&resolved), &format!("authority:{}:{}:{decision}", q.actor, q.act), vec![], "allow")
        .await?;
    Ok((
        StatusCode::OK,
        Json(json!({
            "party": resolved,
            "actor": q.actor,
            "act": q.act,
            "on": on,
            "decision": decision,
            "reason": reason,
            "relationship": by.map(|r| json!({
                "id": r.id,
                "kind": r.kind,
                "basis": r.attributes.get("basis"),
                "document": r.attributes.get("document"),
                "scope": r.attributes.get("scope"),
                "valid_from": r.valid_from,
                "valid_to": r.valid_to,
            })),
        })),
    ))
}

async fn export<W: KeyWrapper, S: WormStore>(
    State(app): State<Arc<App<W, S>>>,
    Path(id): Path<String>,
    Query(q): Query<ReadQuery>,
    headers: HeaderMap,
) -> Reply {
    let ctx = app.ctx(&headers, q.purpose.as_deref()).await?;
    let (resolved, party) = app.find(&id).await?;
    let ask = app.ask_about(&ctx, &resolved).await?;
    if !app.access.allows(&ask, "export") {
        return Err(app.deny(&ctx, Some(&resolved), "export").await);
    }
    let mut streams = BTreeMap::new();
    for p in std::iter::once(&resolved).chain(party.absorbed.iter()) {
        if let Some(records) = app.store.records(p).await.map_err(failed)? {
            streams.insert(p.0.clone(), records);
        }
    }
    app.audit(&ctx, Some(&resolved), "export", vec!["*".into()], "allow").await?;
    Ok((StatusCode::OK, Json(json!({ "party": resolved, "streams": streams }))))
}

async fn audit_trail<W: KeyWrapper, S: WormStore>(State(app): State<Arc<App<W, S>>>, Path(id): Path<String>, headers: HeaderMap) -> Reply {
    let ctx = app.ctx(&headers, None).await?;
    if !PartyId::plausible(&id) {
        return Err(refused(StatusCode::NOT_FOUND, "not_found", "no such party"));
    }
    let party = PartyId(id);
    if !app.access.allows(&app.ask(&ctx), "audit") {
        return Err(app.deny(&ctx, Some(&party), "audit").await);
    }
    match app.store.audit_of(&party, 1_000).await {
        Ok(lines) => {
            let lines: Vec<Value> = lines.into_iter().map(|(at, l)| json!({ "at": at, "line": l })).collect();
            Ok((StatusCode::OK, Json(json!({ "party": party, "audit": lines }))))
        }
        Err(e) => Err(failed(e)),
    }
}

/// Whether due diligence on a party is complete before it owns a policy:
/// its own identity check, screening and current risk rating, and for an
/// organisation (a trust, a company, a partnership) the same for every
/// trustee, director and beneficial owner in force today. A referred check
/// goes to a specialist; nothing is complete while any item is open.
async fn due_diligence<W: KeyWrapper, S: WormStore>(State(app): State<Arc<App<W, S>>>, Path(id): Path<String>, headers: HeaderMap) -> Reply {
    use crate::model::RelationshipKind;
    let ctx = app.ctx(&headers, None).await?;
    let (resolved, party) = app.find(&id).await?;
    if !app.access.allows(&app.ask(&ctx), "due_diligence") {
        return Err(app.deny(&ctx, Some(&resolved), "due_diligence").await);
    }
    let today = Utc::now().date_naive();
    let mut open: Vec<String> = party.due_diligence(today).into_iter().map(String::from).collect();
    let mut people = Vec::new();
    for row in app.store.relationships_to(&Target::Party(resolved.clone())).await.map_err(failed)? {
        let r = &row.relationship;
        if matches!(
            r.kind,
            RelationshipKind::Trustee | RelationshipKind::Director | RelationshipKind::BeneficialOwner
        ) && r.in_force(today)
        {
            let lacks = match app.store.load(&row.party).await.map_err(failed)? {
                Some(p) => p.due_diligence(today),
                None => vec!["identity", "screening", "risk_rating"],
            };
            open.extend(lacks.iter().map(|item| format!("{}:{}:{item}", r.kind.as_str(), row.party.0)));
            people.push(json!({ "party": row.party, "kind": r.kind, "open": lacks }));
        }
    }
    let referred = open.iter().any(|o| o.ends_with("_referred"));
    app.audit(&ctx, Some(&resolved), "due_diligence", vec![], "allow").await?;
    Ok((
        StatusCode::OK,
        Json(json!({
            "party": resolved,
            "complete": open.is_empty(),
            "open": open,
            "to_specialist": referred,
            "connected": people,
        })),
    ))
}

/// Whether a person's own hold on money going out stands, and when a
/// removal they asked for takes effect. Payments and servicing ask before
/// paying out or changing a payout account; the core and payments also
/// take the events.
async fn spending_hold<W: KeyWrapper, S: WormStore>(State(app): State<Arc<App<W, S>>>, Path(id): Path<String>, headers: HeaderMap) -> Reply {
    let ctx = app.ctx(&headers, None).await?;
    let (resolved, party) = app.find(&id).await?;
    let ask = app.ask_about(&ctx, &resolved).await?;
    if !app.access.allows(&ask, "spending_hold") {
        return Err(app.deny(&ctx, Some(&resolved), "spending_hold").await);
    }
    app.audit(&ctx, Some(&resolved), "spending_hold", vec![], "allow").await?;
    let hold = party.spending_hold.as_ref();
    Ok((
        StatusCode::OK,
        Json(json!({
            "party": resolved,
            "held": hold.is_some(),
            "placed_at": hold.map(|h| h.placed_at),
            "removal": hold.and_then(|h| h.removal_requested_at).map(|at| json!({
                "requested_at": at,
                "ends_at": at + app.store.hold_cooling_off(),
            })),
        })),
    ))
}

/// Every action one person took, or was asked about, with dates: what a
/// guardian or attorney asks for to show what they did under their
/// authority. Read by privacy, like a party's own trail.
async fn actions_by<W: KeyWrapper, S: WormStore>(State(app): State<Arc<App<W, S>>>, Path(actor): Path<String>, headers: HeaderMap) -> Reply {
    let ctx = app.ctx(&headers, None).await?;
    if !PartyId::plausible(&actor) {
        return Err(refused(StatusCode::NOT_FOUND, "not_found", "no such party"));
    }
    let actor = PartyId(actor);
    if !app.access.allows(&app.ask(&ctx), "audit") {
        return Err(app.deny(&ctx, Some(&actor), "actions").await);
    }
    app.audit(&ctx, Some(&actor), "actions", vec![], "allow").await?;
    let lines = app.store.audit_by(&actor, 5_000).await.map_err(failed)?;
    let lines: Vec<Value> = lines.into_iter().map(|(at, l)| json!({ "at": at, "line": l })).collect();
    Ok((StatusCode::OK, Json(json!({ "actor": actor, "actions": lines }))))
}

// ---- Matching, creating and searching ------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MatchRequest {
    /// Where the person came from: `group-scheme:S1`, `claims`, `migration`.
    source: String,
    #[serde(flatten)]
    probe: Probe,
    #[serde(default)]
    create_if_none: bool,
}

async fn score_all<W: KeyWrapper, S: WormStore>(app: &App<W, S>, n: &matching::Normal) -> Result<Vec<matching::Score>, StoreError> {
    let keys = matching::probe_keys(n, app.store.index_key());
    let mut scores = Vec::new();
    for id in app.store.candidates(&keys).await? {
        if let Some(p) = app.store.load(&id).await? {
            scores.push(matching::score(n, &p));
        }
    }
    Ok(scores)
}

fn probe_facts(probe: &Probe, n: &matching::Normal) -> Vec<FactInput> {
    let fact = |field, value: Value| FactInput {
        field,
        value,
        valid_from: None,
        valid_to: None,
    };
    let mut facts = vec![fact(Field::FamilyName, Value::String(probe.family_name.clone()))];
    if let Some(g) = probe.given_name.as_ref().filter(|_| n.given.is_some()) {
        facts.push(fact(Field::GivenName, Value::String(g.clone())));
    }
    if let Some(d) = n.dob {
        facts.push(fact(Field::DateOfBirth, Value::String(d.to_string())));
    }
    for (field, v) in [(Field::Email, &n.email), (Field::MobilePhone, &n.mobile), (Field::IrdNumber, &n.ird)] {
        if let Some(v) = v {
            facts.push(fact(field, Value::String(v.clone())));
        }
    }
    facts
}

/// Answer the group service's question. A retry of a request that created
/// someone returns the same person.
async fn match_party<W: KeyWrapper, S: WormStore>(State(app): State<Arc<App<W, S>>>, headers: HeaderMap, body: Bytes) -> Reply {
    let ctx = app.ctx(&headers, None).await?;
    let request: MatchRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return Err(refused(StatusCode::BAD_REQUEST, "invalid", &e.to_string())),
    };
    let Some(source) = clean(&request.source) else {
        return Err(refused(StatusCode::BAD_REQUEST, "invalid", "source is not well formed"));
    };
    let ask = app.ask(&ctx);
    if !app.access.allows(&ask, "match") || (request.create_if_none && !app.access.allows_command(&ask, "create")) {
        return Err(app.deny(&ctx, None, "match").await);
    }
    let n = match request.probe.normal() {
        Ok(n) => n,
        Err(e) => return Err(refused(StatusCode::BAD_REQUEST, "invalid", &e)),
    };
    // The key is a keyed hash of the request: it identifies a repeat
    // without keeping the identity in the clear.
    let mut mac =
        <Hmac<Sha256> as Mac>::new_from_slice(&app.store.index_key().digest("match-request", &ctx.who.caller)).expect("HMAC takes a key of any length");
    mac.update(&body);
    let idem = Idempotency {
        caller: ctx.who.caller.clone(),
        key: format!("match:{}", hex::encode(mac.finalize().into_bytes())),
        request: body.to_vec(),
    };
    let earlier = app.store.recall(&idem).await.map_err(failed)?;
    let resolution = if let Some(outcome) = earlier.and_then(|v| serde_json::from_value::<Outcome>(v).ok()) {
        Resolution::Created { party: outcome.party }
    } else {
        let scores = score_all(&app, &n).await.map_err(failed)?;
        match matching::conclude(scores) {
            Some(r) => r,
            None if request.create_if_none => {
                let who = Who {
                    caller: ctx.who.caller.clone(),
                    actor: source.clone(),
                };
                let o = app
                    .store
                    .create(PartyKind::Person, &probe_facts(&request.probe, &n), None, &who, Some(&idem), &[], Utc::now())
                    .await
                    .map_err(failed)?;
                Resolution::Created { party: o.party }
            }
            None => Resolution::None,
        }
    };
    let party = match &resolution {
        Resolution::Linked { party } | Resolution::Created { party } => Some(party.clone()),
        _ => None,
    };
    let outcome = match &resolution {
        Resolution::Linked { .. } => "linked",
        Resolution::Created { .. } => "created",
        Resolution::Possible { .. } => "possible",
        Resolution::None => "none",
    };
    app.audit(&ctx, party.as_ref(), "match", vec![], outcome).await?;
    let mut answer = serde_json::to_value(&resolution).unwrap_or_default();
    answer["ignored"] = json!(n.ignored);
    Ok((StatusCode::OK, Json(answer)))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateRequest {
    kind: PartyKind,
    facts: Vec<FactInput>,
    #[serde(default)]
    group: Option<String>,
    /// Create even though possible duplicates exist; they go to review.
    #[serde(default)]
    allow_duplicate: bool,
}

fn probe_from_facts(facts: &[FactInput]) -> Option<Probe> {
    let text = |f: Field| facts.iter().find(|x| x.field == f).and_then(|x| x.value.as_str().map(str::to_string));
    Some(Probe {
        given_name: text(Field::GivenName),
        family_name: text(Field::FamilyName)?,
        date_of_birth: text(Field::DateOfBirth).and_then(|d| d.parse().ok()),
        email: text(Field::Email),
        mobile_phone: text(Field::MobilePhone),
        national_id: text(Field::IrdNumber),
    })
}

async fn create<W: KeyWrapper, S: WormStore>(State(app): State<Arc<App<W, S>>>, headers: HeaderMap, body: Bytes) -> Reply {
    let ctx = app.ctx(&headers, None).await?;
    let request: CreateRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return Err(refused(StatusCode::BAD_REQUEST, "invalid", &e.to_string())),
    };
    let mut ask = app.ask(&ctx);
    ask.classes = classes(&request.facts);
    if !app.access.allows_command(&ask, "create") {
        return Err(app.deny(&ctx, None, "create").await);
    }
    let idem = header(&headers, "idempotency-key").map(|k| Idempotency {
        caller: ctx.who.caller.clone(),
        key: k.to_string(),
        request: body.to_vec(),
    });
    if let Some(idem) = &idem {
        if let Some(answer) = app.store.recall(idem).await.map_err(failed)? {
            return Ok((StatusCode::CREATED, Json(answer)));
        }
    }
    let mut review = Vec::new();
    if request.kind == PartyKind::Person {
        if let Some(n) = probe_from_facts(&request.facts).and_then(|p| p.normal().ok()) {
            let scores = score_all(&app, &n).await.map_err(failed)?;
            let found = match matching::conclude(scores) {
                Some(Resolution::Linked { party }) => vec![party],
                Some(Resolution::Possible { candidates }) => candidates,
                _ => vec![],
            };
            if !found.is_empty() && !request.allow_duplicate {
                return Err((
                    StatusCode::CONFLICT,
                    Json(json!({ "error": { "code": "possible_duplicate", "message": "this person may already be known" }, "candidates": found })),
                ));
            }
            review = found;
        }
    }
    match app
        .store
        .create(request.kind, &request.facts, request.group, &ctx.who, idem.as_ref(), &review, Utc::now())
        .await
    {
        Ok(o) => {
            app.audit(&ctx, Some(&o.party), "create", ask.classes.clone(), "allow").await?;
            Ok((StatusCode::CREATED, Json(serde_json::to_value(o).unwrap_or_default())))
        }
        Err(e) => Err(failed(e)),
    }
}

fn classes(facts: &[FactInput]) -> Vec<String> {
    let mut c: Vec<String> = facts.iter().map(|f| f.field.class().as_str().to_string()).collect();
    c.sort();
    c.dedup();
    c
}

async fn search<W: KeyWrapper, S: WormStore>(State(app): State<Arc<App<W, S>>>, headers: HeaderMap, body: Bytes) -> Reply {
    let ctx = app.ctx(&headers, None).await?;
    if !app.access.allows(&app.ask(&ctx), "search") {
        return Err(app.deny(&ctx, None, "search").await);
    }
    let probe: Probe = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(e) => return Err(refused(StatusCode::BAD_REQUEST, "invalid", &e.to_string())),
    };
    let n = match probe.normal() {
        Ok(n) => n,
        Err(e) => return Err(refused(StatusCode::BAD_REQUEST, "invalid", &e)),
    };
    let mut scores = score_all(&app, &n).await.map_err(failed)?;
    scores.retain(|s| s.score > 0);
    scores.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.party.cmp(&b.party)));
    app.audit(&ctx, None, "search", vec![], "allow").await?;
    Ok((StatusCode::OK, Json(json!({ "results": scores }))))
}

// ---- Changing ------------------------------------------------------------

async fn command<W: KeyWrapper, S: WormStore>(State(app): State<Arc<App<W, S>>>, Path(id): Path<String>, headers: HeaderMap, body: Bytes) -> Reply {
    let ctx = app.ctx(&headers, None).await?;
    let change: Change = match serde_json::from_slice(&body) {
        Ok(c) => c,
        Err(e) => return Err(refused(StatusCode::BAD_REQUEST, "invalid", &e.to_string())),
    };
    let expected = match header(&headers, "if-match").map(|v| v.trim_matches('"').parse::<u64>()) {
        Some(Ok(v)) => Some(v),
        Some(Err(_)) => return Err(refused(StatusCode::BAD_REQUEST, "invalid", "If-Match is the version number")),
        None => None,
    };
    if !PartyId::plausible(&id) {
        return Err(refused(StatusCode::NOT_FOUND, "not_found", "no such party"));
    }
    let party = PartyId(id);
    let mut ask = app.ask_about(&ctx, &party).await?;
    if let Change::Record { facts, .. } = &change {
        ask.classes = classes(facts);
    }
    if !app.access.allows_command(&ask, change.name()) {
        return Err(app.deny(&ctx, Some(&party), change.name()).await);
    }
    let idem = header(&headers, "idempotency-key").map(|k| Idempotency {
        caller: ctx.who.caller.clone(),
        key: k.to_string(),
        request: [party.0.as_bytes(), b"\0", &body].concat(),
    });
    match app.store.change(&party, &change, expected, &ctx.who, idem.as_ref(), Utc::now()).await {
        Ok(o) => {
            app.audit(&ctx, Some(&party), change.name(), ask.classes.clone(), "allow").await?;
            Ok((StatusCode::OK, Json(serde_json::to_value(o).unwrap_or_default())))
        }
        Err(e) => Err(failed(e)),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MergeRequest {
    survivor: PartyId,
    absorbed: PartyId,
    #[serde(default)]
    review: Option<String>,
}

async fn merge<W: KeyWrapper, S: WormStore>(State(app): State<Arc<App<W, S>>>, headers: HeaderMap, body: Bytes) -> Reply {
    merging(app, headers, body, true).await
}

async fn unmerge<W: KeyWrapper, S: WormStore>(State(app): State<Arc<App<W, S>>>, headers: HeaderMap, body: Bytes) -> Reply {
    merging(app, headers, body, false).await
}

async fn merging<W: KeyWrapper, S: WormStore>(app: Arc<App<W, S>>, headers: HeaderMap, body: Bytes, merge: bool) -> Reply {
    let ctx = app.ctx(&headers, None).await?;
    let action = if merge { "merge" } else { "unmerge" };
    let request: MergeRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return Err(refused(StatusCode::BAD_REQUEST, "invalid", &e.to_string())),
    };
    if !app.access.allows(&app.ask(&ctx), action) {
        return Err(app.deny(&ctx, Some(&request.absorbed), action).await);
    }
    let now = Utc::now();
    let result = if merge {
        app.store
            .merge(&request.survivor, &request.absorbed, request.review.as_deref(), &ctx.who, now)
            .await
    } else {
        app.store.unmerge(&request.survivor, &request.absorbed, &ctx.who, now).await
    };
    match result {
        Ok(o) => {
            app.audit(&ctx, Some(&request.absorbed), action, vec![], "allow").await?;
            Ok((StatusCode::OK, Json(serde_json::to_value(o).unwrap_or_default())))
        }
        Err(e) => Err(failed(e)),
    }
}

async fn reviews<W: KeyWrapper, S: WormStore>(State(app): State<Arc<App<W, S>>>, headers: HeaderMap) -> Reply {
    let ctx = app.ctx(&headers, None).await?;
    if !app.access.allows(&app.ask(&ctx), "reviews") {
        return Err(app.deny(&ctx, None, "reviews").await);
    }
    match app.store.open_reviews(500).await {
        Ok(r) => Ok((StatusCode::OK, Json(json!({ "reviews": r })))),
        Err(e) => Err(failed(e)),
    }
}

async fn dismiss<W: KeyWrapper, S: WormStore>(State(app): State<Arc<App<W, S>>>, Path(id): Path<String>, headers: HeaderMap) -> Reply {
    let ctx = app.ctx(&headers, None).await?;
    if !app.access.allows(&app.ask(&ctx), "reviews") {
        return Err(app.deny(&ctx, None, "reviews").await);
    }
    match app.store.dismiss_review(&id, &ctx.who, Utc::now()).await {
        Ok(()) => Ok((StatusCode::OK, Json(json!({ "review": id, "status": "dismissed" })))),
        Err(e) => Err(failed(e)),
    }
}

// ---- Operations ----------------------------------------------------------

async fn operator<W: KeyWrapper, S: WormStore>(app: &App<W, S>, headers: &HeaderMap, action: &str) -> Result<Ctx, Answer> {
    let ctx = app.ctx(headers, None).await?;
    if !app.access.allows(&app.ask(&ctx), "operate") {
        return Err(app.deny(&ctx, None, action).await);
    }
    app.audit(&ctx, None, action, vec![], "allow").await?;
    Ok(ctx)
}

async fn verify<W: KeyWrapper, S: WormStore>(State(app): State<Arc<App<W, S>>>, Path(id): Path<String>, headers: HeaderMap) -> Reply {
    operator(&app, &headers, "verify").await?;
    match app.store.verify_chain(&PartyId(id.clone())).await {
        Ok(n) => Ok((StatusCode::OK, Json(json!({ "party": id, "links": n, "intact": true })))),
        Err(e) => Err(failed(e)),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RewrapRequest {
    /// The full key ID of the current key-encryption key version.
    current_key_id: String,
    #[serde(default)]
    limit: Option<i64>,
}

async fn rewrap<W: KeyWrapper, S: WormStore>(State(app): State<Arc<App<W, S>>>, headers: HeaderMap, Json(r): Json<RewrapRequest>) -> Reply {
    operator(&app, &headers, "rewrap").await?;
    match app.store.rewrap_all(&r.current_key_id, r.limit.unwrap_or(1_000).clamp(1, 10_000)).await {
        Ok(n) => Ok((StatusCode::OK, Json(json!({ "rewrapped": n })))),
        Err(e) => Err(failed(e)),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReshredRequest {
    /// The day the restored backup was taken.
    since: NaiveDate,
}

/// Where a change of blind index key stands, so an operator knows when the
/// next key may be promoted.
async fn index_status<W: KeyWrapper, S: WormStore>(State(app): State<Arc<App<W, S>>>, headers: HeaderMap) -> Reply {
    operator(&app, &headers, "index").await?;
    let status = app.store.index_status().await.map_err(failed)?;
    Ok((StatusCode::OK, Json(json!(status))))
}

async fn reshred<W: KeyWrapper, S: WormStore>(State(app): State<Arc<App<W, S>>>, headers: HeaderMap, Json(r): Json<ReshredRequest>) -> Reply {
    operator(&app, &headers, "reshred").await?;
    match crate::work::reshred(&app, r.since).await {
        Ok((listed, erased)) => Ok((StatusCode::OK, Json(json!({ "listed": listed, "erased_again": erased })))),
        Err(e) => Err(refused(StatusCode::SERVICE_UNAVAILABLE, "unavailable", &e)),
    }
}

async fn healthz() -> Reply {
    Ok((StatusCode::OK, Json(json!({ "status": "ok" }))))
}

async fn readyz<W: KeyWrapper, S: WormStore>(State(app): State<Arc<App<W, S>>>) -> Reply {
    match app.store.ready().await {
        Ok(()) => Ok((StatusCode::OK, Json(json!({ "status": "ready" })))),
        Err(e) => Err(failed(e)),
    }
}

/// Every route, over the service's parts.
pub fn router<W: KeyWrapper + 'static, S: WormStore + 'static>(app: Arc<App<W, S>>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz::<W, S>))
        .route("/v1/parties", post(create::<W, S>))
        .route("/v1/parties/match", post(match_party::<W, S>))
        .route("/v1/parties/search", post(search::<W, S>))
        .route("/v1/parties/{id}", get(read::<W, S>))
        .route("/v1/parties/{id}/history", get(history::<W, S>))
        .route("/v1/parties/{id}/commands", post(command::<W, S>))
        .route("/v1/parties/{id}/contact-points", get(contact_points::<W, S>))
        .route("/v1/parties/{id}/relationships", get(relationships_of::<W, S>))
        .route("/v1/parties/{id}/routing", get(routing::<W, S>))
        .route("/v1/parties/{id}/authority", get(authority::<W, S>))
        .route("/v1/parties/{id}/export", get(export::<W, S>))
        .route("/v1/parties/{id}/audit", get(audit_trail::<W, S>))
        .route("/v1/actors/{actor}/actions", get(actions_by::<W, S>))
        .route("/v1/parties/{id}/spending-hold", get(spending_hold::<W, S>))
        .route("/v1/parties/{id}/due-diligence", get(due_diligence::<W, S>))
        .route("/v1/relationships", get(relationships_to::<W, S>))
        .route("/v1/merges", post(merge::<W, S>))
        .route("/v1/unmerges", post(unmerge::<W, S>))
        .route("/v1/reviews", get(reviews::<W, S>))
        .route("/v1/reviews/{id}/dismiss", post(dismiss::<W, S>))
        .route("/v1/admin/verify/{id}", post(verify::<W, S>))
        .route("/v1/admin/rewrap", post(rewrap::<W, S>))
        .route("/v1/admin/reshred", post(reshred::<W, S>))
        .route("/v1/admin/index", get(index_status::<W, S>))
        .layer(axum::extract::DefaultBodyLimit::max(256 * 1024))
        .with_state(app)
}

/// Serve until `stop` completes, then finish the requests in flight.
pub async fn serve(listener: tokio::net::TcpListener, router: Router, stop: impl Future<Output = ()> + Send + 'static) -> std::io::Result<()> {
    axum::serve(listener, router).with_graceful_shutdown(stop).await
}
