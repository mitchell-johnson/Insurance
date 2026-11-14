//! The one token check every service uses, against a stand-in tenant.

use axum::http::HeaderMap;
use ic_entra::testing::{forge, now, sign, sign_as, Tenant, KEY_ID};
use ic_entra::{bearer, issuer_for, AuthError, Validator, ENTRA_AUTHORITY};
use serde_json::{json, Value};

const TENANT: &str = "tenant-1";
const AUDIENCE: &str = "api://insurance-core";

fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in pairs {
        headers.insert(*name, value.parse().unwrap());
    }
    headers
}

fn with_bearer(token: &str) -> HeaderMap {
    headers(&[("authorization", &format!("Bearer {token}"))])
}

fn edit(claims: &Value, change: impl FnOnce(&mut serde_json::Map<String, Value>)) -> Value {
    let mut claims = claims.clone();
    change(claims.as_object_mut().unwrap());
    claims
}

/// Purpose: a token signed by the tenant, for this audience, in date, is accepted and yields the
/// caller's object ID, roles, client application and issue time; every other token is refused:
/// none, another scheme, garbage, no key ID, an unknown key ID, the wrong audience, the wrong
/// issuer, expired, no expiry, and one signed by an attacker's key under the tenant's key ID.
/// Why it matters: every service's front door is this one function. Each refusal is an attack
/// that works on a careless validator: a token minted for another service replayed here, a
/// token from another tenant, a stolen expired token, or a forgery.
#[tokio::test]
async fn only_a_token_from_the_tenant_for_this_audience_is_accepted() {
    let tenant = Tenant::start(TENANT, AUDIENCE).await;
    let validator = tenant.validator();
    let good = edit(&tenant.claims("object-1", &["claims", "portal"]), |c| {
        c.insert("azp".into(), json!("client-app"));
    });

    let claims = validator.validate(&with_bearer(&sign(&good))).await.unwrap();
    assert_eq!(claims.subject, "object-1");
    assert_eq!(claims.roles, vec!["claims", "portal"]);
    assert_eq!(claims.client.as_deref(), Some("client-app"));
    assert!(claims.issued.is_some_and(|t| (t - now()).abs() < 60));
    assert!(claims.has_role("portal") && !claims.has_role("staff"));

    // A version 1 token names its client in appid; a token with no object ID is known by its subject.
    let v1 = edit(&good, |c| {
        c.remove("azp");
        c.remove("oid");
        c.remove("iat");
        c.insert("appid".into(), json!("v1-client"));
    });
    let claims = validator.validate(&with_bearer(&sign(&v1))).await.unwrap();
    assert_eq!(
        (claims.subject.as_str(), claims.client.as_deref(), claims.issued),
        ("pairwise-object-1", Some("v1-client"), None)
    );

    let refused = |e: Result<_, AuthError>| matches!(e, Err(AuthError::Invalid(_)));
    assert_eq!(validator.validate(&HeaderMap::new()).await.unwrap_err(), AuthError::Missing);
    assert_eq!(
        validator.validate(&headers(&[("authorization", "Basic abc")])).await.unwrap_err(),
        AuthError::Missing
    );
    assert_eq!(
        validator.validate(&headers(&[("authorization", "Bearer ")])).await.unwrap_err(),
        AuthError::Missing
    );
    assert!(refused(validator.validate(&with_bearer("not-a-token")).await));
    assert!(refused(validator.validate(&with_bearer(&sign_as(None, &good))).await));
    assert!(refused(validator.validate(&with_bearer(&sign_as(Some("other-key"), &good))).await));
    for bad in [
        edit(&good, |c| {
            c.insert("aud".into(), json!("api://another-service"));
        }),
        edit(&good, |c| {
            c.insert("iss".into(), json!(format!("{}/another-tenant/v2.0", tenant.authority)));
        }),
        edit(&good, |c| {
            c.insert("exp".into(), json!(now() - 3600));
        }),
        edit(&good, |c| {
            c.remove("exp");
        }),
    ] {
        assert!(refused(validator.validate(&with_bearer(&sign(&bad))).await), "accepted {bad}");
    }
    assert!(refused(validator.validate(&with_bearer(&forge(&good))).await));
}

/// Purpose: the keys are fetched once and kept: many tokens cost one fetch, and a flood of
/// unknown key IDs costs at most one more fetch a minute.
/// Why it matters: a validator that fetched on every request, or on every unknown key ID, would
/// let anyone with a made-up token hammer the tenant and be throttled, refusing everyone.
#[tokio::test]
async fn keys_are_fetched_once_and_unknown_key_ids_do_not_cause_a_fetch_each() {
    let tenant = Tenant::start(TENANT, AUDIENCE).await;
    let validator = tenant.validator();
    for _ in 0..5 {
        validator.validate(&with_bearer(&tenant.token("o", &["r"]))).await.unwrap();
    }
    for i in 0..5 {
        let unknown = sign_as(Some(&format!("rotated-{i}")), &tenant.claims("o", &["r"]));
        assert!(validator.validate(&with_bearer(&unknown)).await.is_err());
    }
    assert_eq!(tenant.reads(), 1);
    // A clone shares the cache.
    validator.clone().validate(&with_bearer(&tenant.token("o", &["r"]))).await.unwrap();
    assert_eq!(tenant.reads(), 1);
}

/// Purpose: when the tenant cannot be reached, or answers with something that is not a key set,
/// the answer is "keys unavailable", not "invalid token".
/// Why it matters: services answer the first with 503 and a retry, the second with 401. Telling
/// a good caller its token is bad during a tenant outage would sign everyone out.
#[tokio::test]
async fn an_unreachable_tenant_is_unavailable_not_a_bad_token() {
    let unreachable = Validator::new("http://127.0.0.1:1", TENANT, AUDIENCE);
    let tenant = Tenant::start(TENANT, AUDIENCE).await;
    let token = tenant.token("o", &["r"]);
    assert!(matches!(unreachable.validate(&with_bearer(&token)).await, Err(AuthError::Keys(_))));
    // An authority that answers, but not with keys.
    let wrong_tenant = Validator::new(&tenant.authority, "no/such/path", AUDIENCE);
    assert!(matches!(wrong_tenant.validate(&with_bearer(&token)).await, Err(AuthError::Keys(_))));
    assert!(AuthError::Keys("down".into()).to_string().contains("unavailable"));
}

/// Purpose: a caller with one role acts as it; one with several names which in `x-ic-caller`,
/// and may name only a role it holds; one with none is refused.
/// Why it matters: the role a service acts on decides what it may read and write. A caller that
/// could name a role it was not granted would be granted it.
#[tokio::test]
async fn a_caller_acts_only_as_a_role_its_token_carries() {
    let tenant = Tenant::start(TENANT, AUDIENCE).await;
    let validator = tenant.validator();
    let one = validator.validate(&with_bearer(&tenant.token("o", &["claims"]))).await.unwrap();
    let several = validator.validate(&with_bearer(&tenant.token("o", &["claims", "portal"]))).await.unwrap();
    let none = validator.validate(&with_bearer(&tenant.token("o", &[]))).await.unwrap();
    let asking = |role: &str| headers(&[("x-ic-caller", role)]);

    assert_eq!(one.acting_as(&HeaderMap::new()).unwrap(), "claims");
    assert_eq!(several.acting_as(&asking("portal")).unwrap(), "portal");
    assert!(several.acting_as(&HeaderMap::new()).unwrap_err().to_string().contains("several roles"));
    assert!(one.acting_as(&asking("staff")).unwrap_err().to_string().contains("does not carry"));
    assert!(none.acting_as(&HeaderMap::new()).unwrap_err().to_string().contains("no role"));
}

/// Purpose: the issuer is worked out from the authority: a workforce tenant's is the authority
/// and tenant; an External ID tenant reached by its name issues as its tenant ID's own
/// ciamlogin.com host. The keys are always read at the authority given.
/// Why it matters: External ID tokens carry `https://<tenant id>.ciamlogin.com/<tenant id>/v2.0`.
/// Built from the friendly name instead, every customer, adviser and employer sign-in would be
/// refused on the day the portal goes live.
#[tokio::test]
async fn the_issuer_follows_the_kind_of_tenant() {
    assert_eq!(issuer_for(ENTRA_AUTHORITY, "t1"), "https://login.microsoftonline.com/t1/v2.0");
    assert_eq!(
        issuer_for("https://login.microsoftonline.us/", "t1"),
        "https://login.microsoftonline.us/t1/v2.0"
    );
    assert_eq!(issuer_for("https://contoso.ciamlogin.com", "t1"), "https://t1.ciamlogin.com/t1/v2.0");
    assert_eq!(issuer_for("https://contoso.ciamlogin.com/", "t1"), "https://t1.ciamlogin.com/t1/v2.0");
    // A path after the host is not a ciamlogin authority; nor is no scheme at all.
    assert_eq!(issuer_for("https://x.ciamlogin.com/p", "t1"), "https://x.ciamlogin.com/p/t1/v2.0");
    assert_eq!(issuer_for("not-a-url", "t1"), "not-a-url/t1/v2.0");

    let validator = Validator::new("https://contoso.ciamlogin.com", "t1", AUDIENCE);
    assert_eq!((validator.issuer(), validator.audience()), ("https://t1.ciamlogin.com/t1/v2.0", AUDIENCE));
    let debug = format!("{validator:?}");
    assert!(debug.contains("t1.ciamlogin.com") && debug.contains(AUDIENCE));
}

/// Purpose: the bearer token is read from `Authorization` with surrounding space trimmed, and
/// nothing else counts as one.
/// Why it matters: every service reads the token the same way, so a token one accepts another
/// does not refuse for its spacing, and a non-bearer scheme is never mistaken for a token.
#[test]
fn the_bearer_token_is_read_the_same_way_everywhere() {
    assert_eq!(bearer(&headers(&[("authorization", "  Bearer  abc  ")])), Some("abc"));
    assert_eq!(bearer(&headers(&[("authorization", "bearer abc")])), None);
    assert_eq!(bearer(&HeaderMap::new()), None);
    assert_eq!(KEY_ID, "test-key-1");
}
