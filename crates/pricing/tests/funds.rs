//! The fund definitions the system ships with, as the unit pricing module
//! takes them.

use chrono::{TimeZone, Utc};
use ic_pricing::command::{activation, Output};
use ic_pricing::fund::{FundRules, PricingTerms};
use ic_pricing::PricingState;

/// Purpose: every shipped fund file (`products/funds/`) activates in the
/// module with its pricing terms read, and activating the same files again
/// is answered as before and changes nothing.
/// Why it matters: the core and this module must hold identical fund
/// definitions; a term the module refuses, or a reload that made a second
/// version, would leave a fund that the core trades but nobody can price.
#[test]
fn shipped_funds_activate_and_reload_harmlessly() {
    let funds = ic_sim::config::shipped_funds().expect("the shipped fund files load");
    assert!(!funds.is_empty());
    let at = Utc.with_ymd_and_hms(2026, 11, 3, 6, 0, 0).unwrap();
    let mut state = PricingState::default();
    for fund in &funds {
        PricingTerms::from_map(&fund.pricing).unwrap_or_else(|e| panic!("{}: {e}", fund.id.0));
        assert!(fund.terms().is_ok(), "{}", fund.id.0);
        assert_eq!(state.apply_envelope(activation(fund.clone(), at)), Output::Done, "{}", fund.id.0);
    }
    let hash = state.state_hash();
    for fund in &funds {
        assert_eq!(state.apply_envelope(activation(fund.clone(), at)), Output::Done);
    }
    assert_eq!(state.state_hash(), hash, "a reload changed nothing");
    assert_eq!(state.funds.len(), funds.len());
}
