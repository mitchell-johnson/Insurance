//! Numeric types for the insurance core.
//!
//! Frozen forever (see the design notes, "Numeric types"):
//!
//! * [`Money`] is an `i64` of minor units plus a currency.
//! * [`AccrualAmount`] is an `i128` of millionths of a minor unit, used by
//!   accrual and earning accounts so a day's fraction is posted in full.
//! * [`UnitQty`] is an `i64` of micro-units of one fund. Units are a unit of
//!   account in their own right and never mix with currency.
//! * Rates and maths use [`Dec`] (`fastnum::D256`). A decimal becomes a ledger
//!   amount only through the explicit rounding functions in this crate.

use std::fmt;
use std::str::FromStr;

use fastnum::decimal::Context;
pub use fastnum::decimal::RoundingMode as FastRounding;
pub use fastnum::D256 as Dec;
use serde::{Deserialize, Serialize};

/// Fine units per minor unit (millionths of a cent).
pub const FINE_PER_MINOR: i128 = 1_000_000;
/// Micro-units per whole fund unit.
pub const MICRO_PER_UNIT: i64 = 1_000_000;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MoneyError {
    #[error("unknown currency {0}")]
    UnknownCurrency(String),
    #[error("currency mismatch: {0} vs {1}")]
    CurrencyMismatch(Currency, Currency),
    #[error("arithmetic overflow")]
    Overflow,
    #[error("invalid decimal: {0}")]
    InvalidDecimal(String),
}

/// ISO 4217 currency. The registry fixes each currency's minor unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Currency {
    NZD,
    AUD,
    USD,
}

impl Currency {
    /// Number of decimal places in the minor unit.
    pub const fn minor_digits(self) -> u32 {
        match self {
            Currency::NZD | Currency::AUD | Currency::USD => 2,
        }
    }

    pub const fn code(self) -> &'static str {
        match self {
            Currency::NZD => "NZD",
            Currency::AUD => "AUD",
            Currency::USD => "USD",
        }
    }
}

impl fmt::Display for Currency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl FromStr for Currency {
    type Err = MoneyError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "NZD" => Ok(Currency::NZD),
            "AUD" => Ok(Currency::AUD),
            "USD" => Ok(Currency::USD),
            other => Err(MoneyError::UnknownCurrency(other.to_string())),
        }
    }
}

/// A product's rounding mode for turning decimals or fine units into cents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoundingMode {
    HalfUp,
    HalfEven,
    Down,
    Up,
}

impl RoundingMode {
    fn fast(self) -> FastRounding {
        match self {
            RoundingMode::HalfUp => FastRounding::HalfUp,
            RoundingMode::HalfEven => FastRounding::HalfEven,
            RoundingMode::Down => FastRounding::Down,
            RoundingMode::Up => FastRounding::Up,
        }
    }
}

/// Whole minor units of one currency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Money {
    pub minor: i64,
    pub currency: Currency,
}

impl Money {
    pub const fn new(minor: i64, currency: Currency) -> Self {
        Self { minor, currency }
    }
    pub const fn zero(currency: Currency) -> Self {
        Self { minor: 0, currency }
    }
    pub fn checked_add(self, o: Money) -> Result<Money, MoneyError> {
        self.same(o)?;
        self.minor
            .checked_add(o.minor)
            .map(|m| Money::new(m, self.currency))
            .ok_or(MoneyError::Overflow)
    }
    pub fn checked_sub(self, o: Money) -> Result<Money, MoneyError> {
        self.same(o)?;
        self.minor
            .checked_sub(o.minor)
            .map(|m| Money::new(m, self.currency))
            .ok_or(MoneyError::Overflow)
    }
    pub const fn neg(self) -> Money {
        Money::new(-self.minor, self.currency)
    }
    pub const fn is_zero(self) -> bool {
        self.minor == 0
    }
    pub fn min(self, o: Money) -> Money {
        if self.minor <= o.minor {
            self
        } else {
            o
        }
    }
    /// Exact conversion into fine units.
    pub const fn to_fine(self) -> AccrualAmount {
        AccrualAmount::new(self.minor as i128 * FINE_PER_MINOR, self.currency)
    }
    pub fn to_dec(self) -> Dec {
        Dec::from(self.minor) / pow10(self.currency.minor_digits())
    }
    fn same(self, o: Money) -> Result<(), MoneyError> {
        if self.currency == o.currency {
            Ok(())
        } else {
            Err(MoneyError::CurrencyMismatch(self.currency, o.currency))
        }
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let d = self.currency.minor_digits();
        let scale = 10i64.pow(d);
        let sign = if self.minor < 0 { "-" } else { "" };
        let a = self.minor.unsigned_abs();
        write!(
            f,
            "{sign}{}.{:0width$} {}",
            a / scale as u64,
            a % scale as u64,
            self.currency,
            width = d as usize
        )
    }
}

/// Millionths of a minor unit. Used by accrual and earning accounts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AccrualAmount {
    pub fine: i128,
    pub currency: Currency,
}

impl AccrualAmount {
    pub const fn new(fine: i128, currency: Currency) -> Self {
        Self { fine, currency }
    }
    pub const fn zero(currency: Currency) -> Self {
        Self { fine: 0, currency }
    }
    pub fn checked_add(self, o: AccrualAmount) -> Result<AccrualAmount, MoneyError> {
        if self.currency != o.currency {
            return Err(MoneyError::CurrencyMismatch(self.currency, o.currency));
        }
        self.fine
            .checked_add(o.fine)
            .map(|v| AccrualAmount::new(v, self.currency))
            .ok_or(MoneyError::Overflow)
    }
    pub fn checked_sub(self, o: AccrualAmount) -> Result<AccrualAmount, MoneyError> {
        self.checked_add(AccrualAmount::new(-o.fine, o.currency))
    }
    /// Round to whole minor units by the product's rounding mode. Returns the
    /// rounded money and the remainder (`self - rounded`) in fine units, which
    /// the caller posts to a rounding account so the entry still balances.
    pub fn round_to_money(self, mode: RoundingMode) -> Result<(Money, AccrualAmount), MoneyError> {
        let d = dec_i128(self.fine) / dec_i128(FINE_PER_MINOR);
        let minor = round_integer(d, mode)?;
        let minor = i64::try_from(minor).map_err(|_| MoneyError::Overflow)?;
        let m = Money::new(minor, self.currency);
        let rem = self.fine - m.to_fine().fine;
        Ok((m, AccrualAmount::new(rem, self.currency)))
    }
}

/// Identifier of a unit-linked fund. Units of different funds never mix.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FundId(pub String);

/// Micro-units of one fund.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct UnitQty {
    pub micro: i64,
    pub fund: FundId,
}

impl UnitQty {
    pub fn new(micro: i64, fund: FundId) -> Self {
        Self { micro, fund }
    }
    /// Currency value at a unit price, rounded to cents. Never stored as a balance.
    pub fn value_at(&self, price: Dec, currency: Currency, mode: RoundingMode) -> Result<Money, MoneyError> {
        let units = Dec::from(self.micro) / Dec::from(MICRO_PER_UNIT);
        round_to_money(units * price, currency, mode)
    }
}

/// Exact conversion of an integer into a decimal.
pub fn dec_i128(v: i128) -> Dec {
    // i128 has at most 39 digits, well inside D256's 76.
    match Dec::from_str(&v.to_string(), Context::default()) {
        Ok(d) => d,
        Err(_) => Dec::NAN,
    }
}

fn pow10(n: u32) -> Dec {
    Dec::from(10i64.pow(n))
}

fn round_integer(d: Dec, mode: RoundingMode) -> Result<i128, MoneyError> {
    if d.is_nan() || d.is_infinite() {
        return Err(MoneyError::InvalidDecimal(d.to_string()));
    }
    d.with_rounding_mode(mode.fast()).round(0).to_i128().map_err(|_| MoneyError::Overflow)
}

/// Parse a decimal from its string form (rates, factors, prices).
pub fn dec(s: &str) -> Result<Dec, MoneyError> {
    Dec::from_str(s, Context::default()).map_err(|_| MoneyError::InvalidDecimal(s.to_string()))
}

/// The only way a decimal amount in major units becomes [`Money`]: one rounding
/// at the product's rounding mode.
pub fn round_to_money(amount: Dec, currency: Currency, mode: RoundingMode) -> Result<Money, MoneyError> {
    let minor = round_integer(amount * pow10(currency.minor_digits()), mode)?;
    i64::try_from(minor).map(|m| Money::new(m, currency)).map_err(|_| MoneyError::Overflow)
}

/// The only way a decimal amount in minor units becomes an [`AccrualAmount`]:
/// quantised to fine units with round-half-even.
pub fn round_minor_to_fine(amount_minor: Dec, currency: Currency) -> Result<AccrualAmount, MoneyError> {
    let fine = round_integer(amount_minor * dec_i128(FINE_PER_MINOR), RoundingMode::HalfEven)?;
    Ok(AccrualAmount::new(fine, currency))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn display() {
        assert_eq!(Money::new(-12345, Currency::NZD).to_string(), "-123.45 NZD");
        assert_eq!(Money::new(5, Currency::NZD).to_string(), "0.05 NZD");
    }

    #[test]
    fn rounding_modes() {
        let d = dec("12.345").unwrap();
        assert_eq!(round_to_money(d, Currency::NZD, RoundingMode::HalfUp).unwrap().minor, 1235);
        assert_eq!(round_to_money(d, Currency::NZD, RoundingMode::HalfEven).unwrap().minor, 1234);
        assert_eq!(round_to_money(d, Currency::NZD, RoundingMode::Down).unwrap().minor, 1234);
    }

    #[test]
    fn fine_quantisation() {
        // a third of a cent
        let f = round_minor_to_fine(Dec::from(1i64) / Dec::from(3i64), Currency::NZD).unwrap();
        assert_eq!(f.fine, 333_333);
    }

    #[test]
    fn mismatched_currency() {
        let a = Money::new(1, Currency::NZD);
        let b = Money::new(1, Currency::AUD);
        assert!(a.checked_add(b).is_err());
    }

    #[test]
    fn unit_value() {
        let u = UnitQty::new(2_500_000, FundId("growth".into()));
        let v = u.value_at(dec("1.2345").unwrap(), Currency::NZD, RoundingMode::HalfEven).unwrap();
        assert_eq!(v.minor, 309); // 2.5 * 1.2345 = 3.08625
    }

    /// Purpose: currency codes round-trip through text, and an unknown code is refused.
    /// Why it matters: codes arrive as text in product files and commands; a code we do
    /// not support must fail loudly at the edge instead of defaulting to NZD.
    #[test]
    fn currency_codes_round_trip_and_unknown_codes_are_refused() {
        for c in [Currency::NZD, Currency::AUD, Currency::USD] {
            assert_eq!(c.to_string().parse::<Currency>().unwrap(), c);
            assert_eq!(c.code(), c.to_string());
        }
        assert_eq!("EUR".parse::<Currency>(), Err(MoneyError::UnknownCurrency("EUR".to_string())));
    }

    /// Purpose: subtraction, negation and `min` on money give exact integer results.
    /// Why it matters: refunds, reversals and "pay the lesser of suspense and the bill"
    /// are built from these three operations; an error here misstates a customer's balance.
    #[test]
    fn money_subtracts_negates_and_takes_the_lesser() {
        let ten = Money::new(1000, Currency::NZD);
        let three = Money::new(300, Currency::NZD);
        assert_eq!(ten.checked_sub(three).unwrap(), Money::new(700, Currency::NZD));
        assert_eq!(ten.neg(), Money::new(-1000, Currency::NZD));
        assert_eq!(ten.min(three), three);
        assert_eq!(three.min(ten), three);
        assert_eq!(Money::zero(Currency::AUD).minor, 0);
    }

    /// Purpose: arithmetic that would exceed the integer range, or mix currencies, is an error.
    /// Why it matters: money must never wrap around or silently add dollars of different
    /// countries; both would corrupt the ledger without tripping the zero-sum check.
    #[test]
    fn overflow_and_mixed_currencies_are_errors_not_wraparound() {
        let max = Money::new(i64::MAX, Currency::NZD);
        let one = Money::new(1, Currency::NZD);
        assert_eq!(max.checked_add(one), Err(MoneyError::Overflow));
        assert_eq!(Money::new(i64::MIN, Currency::NZD).checked_sub(one), Err(MoneyError::Overflow));
        assert!(one.checked_sub(Money::new(1, Currency::USD)).is_err());

        let fine_max = AccrualAmount::new(i128::MAX, Currency::NZD);
        assert_eq!(fine_max.checked_add(AccrualAmount::new(1, Currency::NZD)), Err(MoneyError::Overflow));
        assert!(AccrualAmount::zero(Currency::NZD).checked_add(AccrualAmount::zero(Currency::AUD)).is_err());
        // An amount too large for cents is refused when it is rounded to money.
        assert_eq!(fine_max.round_to_money(RoundingMode::HalfEven).map(|(m, _)| m), Err(MoneyError::Overflow));
        assert_eq!(
            round_to_money(dec("1e30").unwrap(), Currency::NZD, RoundingMode::HalfUp),
            Err(MoneyError::Overflow)
        );
    }

    /// Purpose: accrual amounts add and subtract exactly in fine units.
    /// Why it matters: daily earning is a running sum of fine units; any loss here shows up
    /// as unearned premium that never reaches zero at the end of a period.
    #[test]
    fn accrual_amounts_add_and_subtract_exactly() {
        let a = AccrualAmount::new(1_500_000, Currency::NZD);
        let b = AccrualAmount::new(333_333, Currency::NZD);
        assert_eq!(a.checked_add(b).unwrap().fine, 1_833_333);
        assert_eq!(a.checked_sub(b).unwrap().fine, 1_166_667);
        assert_eq!(Money::new(7, Currency::NZD).to_fine().fine, 7 * FINE_PER_MINOR);
    }

    /// Purpose: rounding up always moves away from zero toward the next cent.
    /// Why it matters: `Up` is the one mode the other rounding test does not exercise, and a
    /// product that rounds levies up would under-collect if it behaved like `Down`.
    #[test]
    fn rounding_up_goes_to_the_next_cent() {
        assert_eq!(round_to_money(dec("12.341").unwrap(), Currency::NZD, RoundingMode::Up).unwrap().minor, 1235);
    }

    /// Purpose: text that is not a finite decimal is refused wherever a decimal is parsed.
    /// Why it matters: rates and loadings arrive as strings in configuration and commands.
    /// A NaN or infinity that got through would poison every premium computed from it.
    #[test]
    fn only_finite_decimals_are_accepted() {
        assert!(dec("twelve").is_err());
        assert!(DecStr::parse("0.125").is_ok());
        assert!(DecStr::parse("NaN").is_err());
        assert!(DecStr::parse("Infinity").is_err());
        assert!(DecStr::parse("abc").is_err());
    }

    /// Purpose: a decimal string keeps its exact text through JSON, and bad text fails to decode.
    /// Why it matters: commands are hashed into the chain as JSON. "0.50" and "0.5" must stay
    /// distinct strings so a replayed log produces identical bytes.
    #[test]
    fn decimal_strings_keep_their_exact_text_through_json() {
        let d = DecStr::parse("0.50").unwrap();
        assert_eq!(d.as_str(), "0.50");
        assert_eq!(d.get(), dec("0.5").unwrap());
        let json = serde_json::to_string(&d).unwrap();
        assert_eq!(json, "\"0.50\"");
        assert_eq!(serde_json::from_str::<DecStr>(&json).unwrap(), d);
        assert!(serde_json::from_str::<DecStr>("\"oops\"").is_err());
    }

    /// Purpose: any 128-bit integer converts to a decimal without loss.
    /// Why it matters: fine-unit balances are i128 and the decimal library has no direct
    /// conversion; this guards the string-based bridge at its extremes.
    #[test]
    fn large_integers_convert_to_decimals_exactly() {
        assert_eq!(dec_i128(i128::MAX).to_string(), i128::MAX.to_string());
        assert_eq!(dec_i128(-42).to_string(), "-42");
    }

    proptest! {
        #[test]
        fn round_to_money_remainder_is_exact(fine in -10_000_000_000_000i128..10_000_000_000_000i128) {
            let a = AccrualAmount::new(fine, Currency::NZD);
            for mode in [RoundingMode::HalfUp, RoundingMode::HalfEven, RoundingMode::Down, RoundingMode::Up] {
                let (m, rem) = a.round_to_money(mode).unwrap();
                prop_assert_eq!(m.to_fine().fine + rem.fine, fine);
                prop_assert!(rem.fine.abs() < FINE_PER_MINOR);
            }
        }
    }
}

/// A decimal carried as its exact string form in configuration, commands and
/// state, so encoding is canonical. Validated when constructed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DecStr(String);

impl DecStr {
    pub fn parse(s: &str) -> Result<Self, MoneyError> {
        let d = dec(s)?;
        if d.is_nan() || d.is_infinite() {
            return Err(MoneyError::InvalidDecimal(s.to_string()));
        }
        Ok(Self(s.to_string()))
    }
    pub fn get(&self) -> Dec {
        // Validated at construction.
        dec(&self.0).unwrap_or(Dec::NAN)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for DecStr {
    type Error = MoneyError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        DecStr::parse(&s)
    }
}
impl From<DecStr> for String {
    fn from(d: DecStr) -> String {
        d.0
    }
}
