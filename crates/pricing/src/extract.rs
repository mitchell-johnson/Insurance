//! The file sent to the fund administrator after a pricing date's orders
//! have executed on every partition: each policy's holding of the fund, and
//! the day's net flow, which is the instruction to buy or sell assets.
//!
//! ```text
//! H,unit-pricing,<extract id>,<fund>,<pricing date>,<price id or empty>
//! D,<policy>,<units>
//! F,<currency>,<applications>,<redemptions>,<net>,<orders>
//! T,<holders>,<units on issue>
//! ```
//!
//! Units have six decimal places and amounts two; a negative net is a
//! redemption from the fund. Policies are identified by policy number only:
//! no personal data leaves in this file.

use ic_money::{Currency, Money, MICRO_PER_UNIT};
use sha2::{Digest, Sha256};

use crate::state::Extract;

/// Units in micro-units written with six decimals.
pub fn units(micro: i64) -> String {
    let sign = if micro < 0 { "-" } else { "" };
    let a = micro.unsigned_abs();
    let per = MICRO_PER_UNIT as u64;
    format!("{sign}{}.{:06}", a / per, a % per)
}

fn amount(minor: i64, currency: Currency) -> String {
    // Display is "12.34 NZD"; the file carries the number alone.
    Money::new(minor, currency).to_string().split(' ').next().unwrap_or_default().to_string()
}

/// The blob name an extract is written under.
pub fn name(x: &Extract) -> String {
    format!("extracts/{}/{}.csv", x.fund.0, x.pricing_date)
}

/// Render an extract. The holdings must still be in it (not yet written).
pub fn render(x: &Extract, currency: Currency) -> Vec<u8> {
    let mut out = format!("H,unit-pricing,{},{},{},{}\n", x.id, x.fund.0, x.pricing_date, x.price.as_deref().unwrap_or(""));
    for (policy, micro) in &x.holdings {
        out.push_str(&format!("D,{policy},{}\n", units(*micro)));
    }
    let net = x.flow.applications_minor - x.flow.redemptions_minor;
    out.push_str(&format!(
        "F,{currency},{},{},{},{}\n",
        amount(x.flow.applications_minor, currency),
        amount(x.flow.redemptions_minor, currency),
        amount(net, currency),
        x.flow.orders
    ));
    out.push_str(&format!("T,{},{}\n", x.holders, units(x.units_on_issue_micro)));
    out.into_bytes()
}

/// The SHA-256 of a rendered file, recorded with the extract.
pub fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Flow;
    use chrono::NaiveDate;
    use ic_money::FundId;

    /// Purpose: an extract renders as header, one line per holding, the flow
    /// and a trailer with the totals, units at six places and money at two.
    /// Why it matters: this file is how the administrator's register and the
    /// core stay reconciled; a misplaced decimal would be a units break.
    #[test]
    fn renders_holdings_and_flow() {
        let x = Extract {
            id: "growth:2026-11-03".into(),
            fund: FundId("growth".into()),
            pricing_date: NaiveDate::from_ymd_opt(2026, 11, 3).unwrap_or_default(),
            price: Some("growth:2026-11-03.1".into()),
            holdings: [("P1".to_string(), 5_497_265i64), ("P2".to_string(), 1_000_000)].into(),
            units_on_issue_micro: 6_497_265,
            holders: 2,
            flow: Flow {
                applications_minor: 1_000_00,
                redemptions_minor: 1_500_00,
                orders: 3,
            },
            written: None,
        };
        let text = String::from_utf8(render(&x, Currency::NZD)).unwrap_or_default();
        assert_eq!(
            text,
            "H,unit-pricing,growth:2026-11-03,growth,2026-11-03,growth:2026-11-03.1\n\
             D,P1,5.497265\nD,P2,1.000000\n\
             F,NZD,1000.00,1500.00,-500.00,3\n\
             T,2,6.497265\n"
        );
        assert_eq!(name(&x), "extracts/growth/2026-11-03.csv");
        assert_eq!(digest(b"").len(), 64);
        assert_eq!(units(-1), "-0.000001");
    }
}
