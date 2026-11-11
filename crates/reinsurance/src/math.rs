//! Exact integer arithmetic for shares and rates.
//!
//! A share is held in parts per million and a reinsurer's rate in millionths
//! of a currency unit per thousand of sum at risk per year. Both are written
//! as decimal strings in a treaty and become integers when it is loaded, so
//! the cession and the ceded premium are integer calculations with one
//! explicit rounding each.

use serde::{Deserialize, Deserializer};

/// A share in parts per million.
pub type Ppm = u32;
/// The whole: 100%.
pub const WHOLE: Ppm = 1_000_000;

/// `n / d` rounded half to even. `d` must be positive.
pub fn div_half_even(n: i128, d: i128) -> i128 {
    let q = n.div_euclid(d);
    let r = n.rem_euclid(d);
    match (2 * r).cmp(&d) {
        std::cmp::Ordering::Less => q,
        std::cmp::Ordering::Greater => q + 1,
        std::cmp::Ordering::Equal => q + (q & 1),
    }
}

/// A share of an amount, rounded half to even.
pub fn share_of(amount_minor: i64, share: Ppm) -> i64 {
    div_half_even(amount_minor as i128 * share as i128, WHOLE as i128) as i64
}

/// `amount * num / den`, rounded half to even. Zero when `den` is zero.
pub fn pro_rata(amount_minor: i64, num: i64, den: i64) -> i64 {
    if den == 0 {
        return 0;
    }
    let (n, d) = if den < 0 {
        (-(num as i128), -(den as i128))
    } else {
        (num as i128, den as i128)
    };
    div_half_even(amount_minor as i128 * n, d) as i64
}

/// Split a total by weights so that the parts add up to the total exactly.
/// Each part is rounded down and the remainder goes to the earliest parts,
/// one minor unit each: the same answer on every replica.
pub fn split_exact(total_minor: i64, weights: &[i64]) -> Vec<i64> {
    let sum: i128 = weights.iter().map(|w| *w as i128).sum();
    if sum <= 0 {
        return vec![0; weights.len()];
    }
    let sign = if total_minor < 0 { -1 } else { 1 };
    let total = (total_minor as i128).abs();
    let mut parts: Vec<i128> = weights.iter().map(|w| total * *w as i128 / sum).collect();
    let mut left = total - parts.iter().sum::<i128>();
    for (p, w) in parts.iter_mut().zip(weights) {
        if left == 0 {
            break;
        }
        if *w > 0 {
            *p += 1;
            left -= 1;
        }
    }
    parts.into_iter().map(|p| (p * sign) as i64).collect()
}

/// Parse a decimal with up to six places into millionths, exactly.
pub fn parse_micro(s: &str) -> Option<i64> {
    let (neg, body) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let (whole, frac) = body.split_once('.').unwrap_or((body, ""));
    if whole.is_empty() || frac.len() > 6 || !whole.bytes().chain(frac.bytes()).all(|b| b.is_ascii_digit()) {
        return None;
    }
    let whole: i64 = whole.parse().ok()?;
    let frac: i64 = format!("{frac:0<6}").parse().ok()?;
    let v = whole.checked_mul(1_000_000)?.checked_add(frac)?;
    Some(if neg { -v } else { v })
}

/// Millionths as written in a treaty ("0.40") or as stored (400000).
#[derive(Deserialize)]
#[serde(untagged)]
enum MicroRepr {
    Int(i64),
    Text(String),
}

fn micro<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    match MicroRepr::deserialize(d)? {
        MicroRepr::Int(v) => Ok(v),
        MicroRepr::Text(s) => parse_micro(&s).ok_or_else(|| serde::de::Error::custom(format!("`{s}` is not a decimal with at most six places"))),
    }
}

/// Deserialize a share: a decimal fraction as text, or parts per million.
pub fn de_ppm<'de, D: Deserializer<'de>>(d: D) -> Result<Ppm, D::Error> {
    let v = micro(d)?;
    Ppm::try_from(v)
        .ok()
        .filter(|p| *p <= WHOLE)
        .ok_or_else(|| serde::de::Error::custom("a share must be between 0 and 1"))
}

/// Deserialize a rate: a decimal as text, or millionths.
pub fn de_rate<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    let v = micro(d)?;
    if v < 0 {
        return Err(serde::de::Error::custom("a rate cannot be negative"));
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// Purpose: division rounds halves to the even neighbour for positive and negative amounts.
    /// Why it matters: every ceded amount, premium and recovery is rounded here. A bias in one
    /// direction would move money between the insurer and its reinsurers a cent at a time, and a
    /// sign mistake would show only on reversals.
    #[test]
    fn half_even_rounding() {
        assert_eq!(div_half_even(5, 2), 2);
        assert_eq!(div_half_even(7, 2), 4);
        assert_eq!(div_half_even(-5, 2), -2);
        assert_eq!(div_half_even(-7, 2), -4);
        assert_eq!(share_of(100_001, 500_000), 50_000);
        assert_eq!(share_of(100_003, 500_000), 50_002);
        assert_eq!(pro_rata(1_000, 1, 3), 333);
        assert_eq!(pro_rata(-1_000, 1, 3), -333);
        assert_eq!(pro_rata(1_000, -1, -3), 333);
        assert_eq!(pro_rata(1_000, 1, 0), 0);
    }

    /// Purpose: decimals written in a treaty become exact millionths, and anything that is not a
    /// plain decimal of at most six places is refused.
    /// Why it matters: a treaty's shares and rates are commercial terms. Silently truncating
    /// "0.3333333" or reading "1e-3" as something else would cede on terms nobody agreed.
    #[test]
    fn decimals_parse_exactly_or_not_at_all() {
        assert_eq!(parse_micro("0.4"), Some(400_000));
        assert_eq!(parse_micro("1"), Some(1_000_000));
        assert_eq!(parse_micro("12.345678"), Some(12_345_678));
        assert_eq!(parse_micro("-0.5"), Some(-500_000));
        for bad in ["", ".5", "0.1234567", "1e-3", "abc", "1.2.3", "99999999999999999999"] {
            assert_eq!(parse_micro(bad), None, "{bad}");
        }
    }

    #[derive(Deserialize)]
    struct Terms {
        #[serde(deserialize_with = "de_ppm")]
        share: Ppm,
        #[serde(deserialize_with = "de_rate")]
        rate: i64,
    }

    /// Purpose: shares and rates load from the decimal text a treaty author writes and from the
    /// integers the module stores, and out-of-range values are refused.
    /// Why it matters: the same field is read from Starlark at activation and from a snapshot at
    /// restart. If the two readings differed, a restarted replica would cede differently; a share
    /// above one or a negative rate would cede more than the cover or pay the insurer to cede.
    #[test]
    fn shares_and_rates_load_from_text_or_integers() {
        let t: Terms = serde_json::from_str(r#"{"share":"0.25","rate":"1.5"}"#).unwrap();
        assert_eq!((t.share, t.rate), (250_000, 1_500_000));
        let t: Terms = serde_json::from_str(r#"{"share":250000,"rate":1500000}"#).unwrap();
        assert_eq!((t.share, t.rate), (250_000, 1_500_000));
        assert!(serde_json::from_str::<Terms>(r#"{"share":"1.5","rate":"1"}"#).is_err());
        assert!(serde_json::from_str::<Terms>(r#"{"share":"-0.1","rate":"1"}"#).is_err());
        assert!(serde_json::from_str::<Terms>(r#"{"share":"0.1","rate":"-1"}"#).is_err());
        assert!(serde_json::from_str::<Terms>(r#"{"share":"x","rate":"1"}"#).is_err());
    }

    /// Purpose: a split with no positive weight gives every part nothing, and a negative total
    /// splits like the positive one with the sign carried.
    /// Why it matters: reversals split negative amounts across a panel. They must mirror the
    /// original split exactly, or a reversed premium would leave cents on a reinsurer's account.
    #[test]
    fn split_edges() {
        assert_eq!(split_exact(100, &[0, 0]), vec![0, 0]);
        assert_eq!(split_exact(100, &[]), Vec::<i64>::new());
        assert_eq!(split_exact(100, &[1, 1, 1]), vec![34, 33, 33]);
        assert_eq!(split_exact(-100, &[1, 1, 1]), vec![-34, -33, -33]);
        assert_eq!(split_exact(1, &[0, 5]), vec![0, 1]);
    }

    proptest! {
        /// Purpose: for any total and any weights with a positive sum, the parts of a split add
        /// up to the total and no part differs from its exact share by a whole minor unit or more.
        /// Why it matters: a cession is split across a panel of reinsurers and a claim across
        /// layers. If the parts did not add up, the retained and ceded amounts would not equal
        /// the sum at risk and reconciliation with the reinsurers would never close.
        #[test]
        fn split_adds_up(total in -1_000_000_000_000i64..1_000_000_000_000i64, weights in proptest::collection::vec(0i64..1_000_000, 1..6)) {
            prop_assume!(weights.iter().sum::<i64>() > 0);
            let parts = split_exact(total, &weights);
            prop_assert_eq!(parts.iter().sum::<i64>(), total);
            let sum: i128 = weights.iter().map(|w| *w as i128).sum();
            for (p, w) in parts.iter().zip(&weights) {
                let exact_times_sum = total as i128 * *w as i128;
                prop_assert!((*p as i128 * sum - exact_times_sum).abs() < sum);
            }
        }
    }
}
