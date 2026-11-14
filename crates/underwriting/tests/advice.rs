//! An adviser's new policy advised to the commission module.

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use common::*;
use ic_core::LifeId;
use ic_underwriting::advice::{accepted_premiums, key, loaded, standard_premiums};
use ic_underwriting::command::{Command, Effect, OfferResponse};
use ic_underwriting::driver::{DriverError, Forward, NothingConnected, Refusal};
use ic_underwriting::model::{ProposalStatus, Stage};
use ic_underwriting::notices::{advice_command, Letters, NoLetters, Notices, StaffFeed};
use ic_underwriting::questions::Answer;
use ic_underwriting::service::distribution_envelope;
use serde_json::{json, Value};

fn issued(w: &mut World, id: &str) -> Vec<Effect> {
    w.ok(
        "issuer",
        Command::PolicyIssued {
            proposal: id.into(),
            layers: vec![],
        },
    )
    .effects
}

fn advice(effects: &[Effect]) -> Vec<&Effect> {
    effects.iter().filter(|e| matches!(e, Effect::AdviseNewBusiness { .. })).collect()
}

/// Purpose: when an adviser's new policy is issued the commission module is told once, under
/// `uw-nb:{proposal}`, who wrote it, the provider they act under, the option they chose and each
/// cover's annual premium as charged. Where the quote gives only the first month's premium
/// (85.01), it is made yearly (1,020.12) and shared by amount: life 400,000 of 500,000 takes
/// 1,020.12 x 4/5 = 816.096, so 816.09, and trauma takes the 204.03 left. Both covers carry the
/// smoker's 50% loading, each rounded half up: 816.09 x 1.5 = 1,224.135, so 1,224.14, and
/// 204.03 x 1.5 = 306.045, so 306.05.
/// Why it matters: until this, retail new business wrote no commission layers at all, so an
/// adviser was never paid. A share that drops the odd cent, or a premium sent without its
/// loading, pays the adviser on a premium the customer is not charged.
#[test]
fn an_advisers_new_policy_is_advised_once_with_each_covers_yearly_premium() {
    let mut w = World::new();
    let mut open = w.open_command(
        "P1",
        &["L1"],
        &[("L1", "life", "life", 400_000_00), ("L1", "trauma", "trauma", 100_000_00)],
        true,
    );
    open.quote.premium_minor = 85_01;
    open.commission_option = Some("hybrid".into());
    w.ok("new-business", Command::OpenProposal(Box::new(open)));
    let answers = with(&[
        ("smoked_last_12_months", Answer::YesNo(true)),
        ("back_or_neck_last_5_years", Answer::YesNo(true)),
    ]);
    let disclosed = w.disclose("P1", "L1", &answers);
    w.automatic(&disclosed);
    w.respond("P1", "L1", OfferResponse::Accept);
    assert_eq!(w.p("P1").status, ProposalStatus::Issuing);
    let effects = issued(&mut w, "P1");
    let advised = advice(&effects);
    let [Effect::AdviseNewBusiness {
        key: k,
        policy,
        adviser,
        provider,
        option,
        replacement,
        premiums,
        ..
    }] = &advised[..]
    else {
        panic!("one advice: {effects:?}")
    };
    assert_eq!(
        (
            k.as_str(),
            policy.0.as_str(),
            adviser.as_str(),
            provider.as_str(),
            option.as_str(),
            *replacement
        ),
        ("uw-nb:P1", "POL-P1", "adv-1", "fap-1", "hybrid", false)
    );
    assert_eq!(premiums, &BTreeMap::from([("life".to_string(), 1224_14), ("trauma".to_string(), 306_05)]));
    assert_eq!(key("P1"), "uw-nb:P1");
}

/// Purpose: the quote's own yearly figure for each cover is used where it gives one; an
/// adviser who names no option is on upfront; a life whose cover was declined earns nothing; a
/// direct application is not advised; and an option that is not upfront, hybrid or level, or one
/// given with no adviser, is refused at the door.
/// Why it matters: the rating engine's per-cover premium is the true one, and sharing by amount
/// is only a fallback. Commission on a declined cover, or on a policy no adviser wrote, is money
/// paid for nothing; an unknown option would leave commission operations a layer it cannot rate.
#[test]
fn quoted_cover_premiums_and_the_default_option_are_used_and_direct_business_is_not_advised() {
    let mut w = World::new();
    let mut open = w.open_command(
        "P1",
        &["L1", "L2"],
        &[("L1", "life", "life", 300_000_00), ("L2", "life2", "life", 200_000_00)],
        true,
    );
    open.quote.cover_premiums_minor = BTreeMap::from([("life".to_string(), 900_00), ("life2".to_string(), 600_00)]);
    let mut bad = open.clone();
    bad.commission_option = Some("trail-only".into());
    assert_eq!(w.err("new-business", Command::OpenProposal(Box::new(bad))), "commission_option");
    w.ok("new-business", Command::OpenProposal(Box::new(open)));
    for life in ["L1", "L2"] {
        let disclosed = w.disclose("P1", life, &clean());
        w.automatic(&disclosed);
    }
    let mut declined = w.p("P1").clone();
    declined.lives.get_mut(&LifeId::from("L2")).unwrap().stage = Stage::Declined;
    assert_eq!(accepted_premiums(&declined), BTreeMap::from([("life".to_string(), 900_00)]));
    let effects = issued(&mut w, "P1");
    let [Effect::AdviseNewBusiness { option, premiums, .. }] = &advice(&effects)[..] else {
        panic!("one advice")
    };
    assert_eq!(option, "upfront");
    assert_eq!(premiums, &BTreeMap::from([("life".to_string(), 900_00), ("life2".to_string(), 600_00)]));

    let mut direct = w.open_command("P2", &["L1"], &[("L1", "life", "life", 300_000_00)], false);
    let mut unadvised = direct.clone();
    unadvised.commission_option = Some("level".into());
    assert_eq!(w.err("new-business", Command::OpenProposal(Box::new(unadvised))), "commission_option");
    direct.commission_option = None;
    w.ok("new-business", Command::OpenProposal(Box::new(direct)));
    let disclosed = w.disclose("P2", "L1", &clean());
    w.automatic(&disclosed);
    assert!(advice(&issued(&mut w, "P2")).is_empty(), "no adviser, no commission");
}

/// Purpose: the money helpers are exact: a loading adds its percentage rounded half up (100.01 at
/// 25% is 125.0125, so 125.01; 0.02 at 25% is 0.025, so 0.03), no loading leaves the premium as
/// it is, and with no premium quoted there is nothing to share.
/// Why it matters: commission is paid on these figures for the life of the policy, so a cent
/// lost or gained here is lost or gained every year.
#[test]
fn a_loading_rounds_half_up_and_no_premium_shares_nothing() {
    assert_eq!((loaded(100_01, 25), loaded(2, 25), loaded(100_01, 0)), (125_01, 3, 100_01));
    let mut w = World::new();
    let mut open = w.open_command("P1", &["L1"], &[("L1", "life", "life", 300_000_00)], true);
    open.quote.premium_minor = 0;
    w.ok("new-business", Command::OpenProposal(Box::new(open)));
    assert!(standard_premiums(w.p("P1")).is_empty());
    let disclosed = w.disclose("P1", "L1", &clean());
    w.automatic(&disclosed);
    assert!(advice(&issued(&mut w, "P1")).is_empty(), "nothing to pay commission on");
}

/// The commission module's answers, in order, and what it was sent.
#[derive(Clone, Default)]
struct Commission {
    answers: Arc<Mutex<Vec<Result<Result<(), Refusal>, DriverError>>>>,
    sent: Arc<Mutex<Vec<(String, Value)>>>,
}

impl Letters for Commission {
    async fn request(&self, _key: &str, _at: chrono::DateTime<chrono::Utc>, _command: Value) -> Result<Result<(), Refusal>, DriverError> {
        Ok(Ok(()))
    }

    async fn advise(&self, key: &str, _at: chrono::DateTime<chrono::Utc>, command: Value) -> Result<Result<(), Refusal>, DriverError> {
        self.sent.lock().unwrap().push((key.into(), command));
        self.answers.lock().unwrap().remove(0)
    }
}

#[derive(Clone, Default)]
struct Hub;

impl StaffFeed for Hub {
    async fn publish(&self, _id: &str, _kind: &str, _body: Value) -> Result<(), ic_azure::AzureError> {
        Ok(())
    }
}

fn sample() -> Effect {
    Effect::AdviseNewBusiness {
        key: "uw-nb:P1".into(),
        proposal: "P1".into(),
        policy: "POL-P1".into(),
        adviser: "adv-1".into(),
        provider: "fap-1".into(),
        option: "level".into(),
        replacement: true,
        premiums: BTreeMap::from([("life".to_string(), 900_00)]),
    }
}

/// Purpose: the advice goes to the commission module as `advise_new_business` in its own shape,
/// sent as the new business module under the proposal's key. Taken, or already taken by an
/// earlier try whose answer was lost, it is delivered; any other refusal parks it with the code;
/// a module that cannot be reached is tried again; with no commission module it is parked
/// saying so.
/// Why it matters: the commission module allows this command only from new business and keys
/// on the key alone, so the same key on every try is what makes a resend one attribution. A
/// refusal such as `invalid_new_business` will not change on a resend, so it waits for a person
/// rather than holding every effect behind it.
#[tokio::test]
async fn the_advice_reaches_the_commission_module_once_in_its_shape() {
    let commission = Commission::default();
    let notices = Notices {
        staff: Hub,
        evidence: Hub,
        delegates: Hub,
        letters: commission.clone(),
        clock: chrono::Utc::now,
    };
    *commission.answers.lock().unwrap() = vec![
        Ok(Ok(())),
        Ok(Err(Refusal::new("already_advised", "done"))),
        Ok(Err(Refusal::new("invalid_new_business", "no premium"))),
        Err(DriverError::Unavailable("down".into())),
    ];
    assert_eq!(notices.forward(1, &sample()).await, Ok(None));
    assert_eq!(notices.forward(1, &sample()).await, Ok(None), "already advised is delivered");
    assert_eq!(
        notices.forward(1, &sample()).await,
        Ok(Some("commission refused the new business: invalid_new_business: no premium".into()))
    );
    assert!(matches!(notices.forward(1, &sample()).await, Err(DriverError::Unavailable(_))));
    let expected = json!({"advise_new_business": {"policy": "POL-P1", "advice": {
        "adviser": "adv-1", "provider": "fap-1", "option": "level", "replacement": true, "premiums": {"life": 900_00}}}});
    let sent = commission.sent.lock().unwrap().clone();
    assert!(sent.iter().all(|(k, c)| k == "uw-nb:P1" && c == &expected));
    assert_eq!(advice_command(&sample()), Some(expected.clone()));
    assert_eq!(advice_command(&Effect::CancelEvidence { request: "r".into() }), None);
    assert_eq!(
        distribution_envelope("uw-nb:P1", expected.clone()),
        json!({"key": "uw-nb:P1", "actor": {"module": "new-business"}, "command": expected})
    );

    let none = Notices {
        staff: Hub,
        evidence: Hub,
        delegates: Hub,
        letters: NoLetters,
        clock: chrono::Utc::now,
    };
    assert_eq!(none.forward(2, &sample()).await, Ok(Some("commission is not connected to underwriting".into())));
    assert_eq!(
        NothingConnected.forward(2, &sample()).await,
        Ok(Some("commission is not connected to underwriting".into()))
    );
}
