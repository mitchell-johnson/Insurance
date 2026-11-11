//! Building entries from signed amounts, so that credits, refunds and
//! reversals take the same code path as the forward flow.

use std::collections::BTreeMap;

use ic_ledger::{Account, AccountUnit, Amount, Dimensions, Ledger, Posting, Side};
use ic_money::{AccrualAmount, Money, UnitQty};

use crate::machine::Reject;

#[derive(Default)]
pub(crate) struct PostingSet {
    /// (account, dims) -> signed amount in the account's unit, debits positive.
    items: Vec<(Account, Dimensions, i128)>,
}

impl PostingSet {
    pub fn new() -> Self {
        Self::default()
    }
    /// Debit a signed amount (a negative amount is a credit).
    pub fn debit(&mut self, account: Account, amount: i128, dims: Dimensions) {
        if amount == 0 {
            return;
        }
        if let Some(e) = self.items.iter_mut().find(|(a, d, _)| a.id == account.id && *d == dims) {
            e.2 += amount;
        } else {
            self.items.push((account, dims, amount));
        }
    }
    pub fn credit(&mut self, account: Account, amount: i128, dims: Dimensions) {
        self.debit(account, -amount, dims);
    }
    pub fn is_empty(&self) -> bool {
        self.items.iter().all(|(_, _, v)| *v == 0)
    }

    /// Open any accounts not yet open and return typed postings.
    pub fn into_postings(self, ledger: &mut Ledger) -> Result<Vec<Posting>, Reject> {
        let mut out = Vec::with_capacity(self.items.len());
        for (account, dims, signed) in self.items {
            if signed == 0 {
                continue;
            }
            let abs = signed.abs();
            let amount = match &account.unit {
                AccountUnit::Minor(c) => Amount::Money(Money::new(i64::try_from(abs).map_err(|_| Reject::new("overflow", "amount too large"))?, *c)),
                AccountUnit::Fine(c) => Amount::Accrual(AccrualAmount::new(abs, *c)),
                AccountUnit::Units(f) => Amount::Units(UnitQty::new(
                    i64::try_from(abs).map_err(|_| Reject::new("overflow", "amount too large"))?,
                    f.clone(),
                )),
            };
            let id = account.id.clone();
            ledger.open_account(account)?;
            out.push(Posting {
                account: id,
                side: if signed > 0 { Side::Debit } else { Side::Credit },
                amount,
                dims,
            });
        }
        Ok(out)
    }
}

/// `n / d` rounded half to even, exact in integers.
/// A negative divisor is normal: a credit adjustment has a negative total.
pub(crate) fn div_half_even(n: i128, d: i128) -> i128 {
    debug_assert!(d != 0);
    let (n, d) = if d < 0 { (-n, -d) } else { (n, d) };
    let q = n.div_euclid(d);
    let r = n.rem_euclid(d);
    match (2 * r).cmp(&d) {
        std::cmp::Ordering::Less => q,
        std::cmp::Ordering::Greater => q + 1,
        std::cmp::Ordering::Equal => q + (q & 1),
    }
}

/// Scale every value of a map by `num / den`, half-even.
pub(crate) fn scale_map(m: &BTreeMap<String, i64>, num: i128, den: i128) -> BTreeMap<String, i64> {
    m.iter().map(|(k, v)| (k.clone(), div_half_even(*v as i128 * num, den) as i64)).collect()
}

#[cfg(test)]
mod tests {
    use super::div_half_even;

    /// Purpose: integer division rounds halves to the even neighbour, for every combination of
    /// signs.
    /// Why it matters: premium is split across covers and periods with this function. Half-even has
    /// no bias, so splits do not drift in one direction; a sign mistake would show only on refunds
    /// and credit adjustments, which divide negative amounts.
    #[test]
    fn half_even() {
        assert_eq!(div_half_even(5, 2), 2);
        assert_eq!(div_half_even(7, 2), 4);
        assert_eq!(div_half_even(-5, 2), -2);
        assert_eq!(div_half_even(-7, 2), -4);
        assert_eq!(div_half_even(10, 3), 3);
        assert_eq!(div_half_even(-10, 3), -3);
        // A credit adjustment divides by a negative total: same answers, sign carried.
        assert_eq!(div_half_even(-5, -2), 2);
        assert_eq!(div_half_even(5, -2), -2);
        assert_eq!(div_half_even(7, -2), -4);
    }
}
