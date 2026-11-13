//! The module's configuration: banks, channels, accounts, routes and rules.
//!
//! Configuration is data, versioned and immutable once active. A version
//! that would leave a voter region unable to reach a bank is refused: the
//! region that holds the leaders after an evacuation must be able to submit
//! a direct debit file and an urgent payment without anyone provisioning
//! anything first.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{Datelike, NaiveDate, NaiveTime, Weekday};
use ic_money::Currency;
use serde::{Deserialize, Serialize};

use crate::model::{AccountDetails, Id, Purpose, Reject, RunType};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    /// Files with a signed manifest, answered by acknowledgement files.
    File,
    /// Idempotent requests answered at once, with status to follow.
    Api,
}

/// How one region reaches one channel. The secrets themselves stay in Key
/// Vault; these are the names they are held under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    pub url: String,
    pub credential: String,
    pub signing_key: String,
    pub certificate_expires: NaiveDate,
    /// The address the bank sees the region's traffic come from.
    pub egress_address: String,
    /// The bank has confirmed that address is on its allow list.
    pub whitelisted: bool,
    /// The secret holding the client certificate and its private key, in
    /// PEM, that this region presents to the bank (mutual TLS). Required for
    /// a bank spoken to in ISO 20022.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_certificate: Option<String>,
    /// The secret holding the certificate authority, in PEM, that the
    /// bank's own certificate is issued by, where it is a private one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bank_authority: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channel {
    pub bank: Id,
    pub kind: ChannelKind,
    /// The last moment of a business day the bank accepts work for that day.
    pub cutoff: NaiveTime,
    pub endpoints: BTreeMap<String, Endpoint>,
    /// What the channel's files and requests are written in. A channel
    /// configured before formats existed speaks this system's own.
    #[serde(default, skip_serializing_if = "WireFormat::is_own")]
    pub format: WireFormat,
}

/// The layout a bank receives batches in and sends its answers in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireFormat {
    /// This system's own pipe-separated file and JSON messages, spoken by
    /// the fake bank and by nothing else.
    #[default]
    Own,
    /// ISO 20022: `pain.001` and `pain.008` out, `pain.002` and `camt.053`
    /// back.
    Iso20022,
}

impl WireFormat {
    pub fn is_own(&self) -> bool {
        *self == WireFormat::Own
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BankAccount {
    pub bank: Id,
    pub fund: String,
    pub purpose: Purpose,
    pub currency: Currency,
    /// The account as its bank knows it: holder, number and BIC. Required
    /// for every account at a bank whose channels speak ISO 20022, which
    /// names the insurer's own account in every message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_bank: Option<crate::iso20022::OrderingAccount>,
}

/// How one type of run reaches a bank, and the controls on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Route {
    pub primary: Id,
    /// The second bank, used when the route is redirected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secondary: Option<Id>,
    /// The API channel for urgent payments out of hours.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub urgent: Option<Id>,
    /// Distinct approvers a run needs.
    pub approvals: u8,
    pub item_limit_minor: i64,
    pub run_limit_minor: i64,
    /// For all runs of this type with the same value date.
    pub day_limit_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rules {
    /// Hours a changed account waits before it can be paid.
    pub cooling_off_hours: u32,
    /// The same account and amount within this many days is held as a
    /// possible duplicate.
    pub duplicate_window_days: u32,
    /// Dishonours in a row that suspend an authority.
    pub dishonours_before_suspension: u32,
    /// A run at or above this total is pre-advised to the bank and treasury.
    pub pre_advice_threshold_minor: i64,
    /// Approvers who may approve an urgent run out of hours.
    pub on_call: BTreeSet<String>,
    /// A region is ready only if its last passed drill is this recent.
    pub drill_max_age_days: u32,
    /// One of the insurer's own accounts, paid a token amount by each
    /// evacuation drill.
    pub drill_account: AccountDetails,
    pub drill_amount_minor: i64,
    /// Every region that can hold leaders.
    pub voter_regions: BTreeSet<String>,
    /// Days the banks are closed, beyond weekends.
    pub holidays: BTreeSet<NaiveDate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub banks: BTreeMap<Id, String>,
    pub channels: BTreeMap<Id, Channel>,
    pub accounts: BTreeMap<Id, BankAccount>,
    pub routes: BTreeMap<RunType, Route>,
    pub funds: BTreeSet<String>,
    pub rules: Rules,
}

fn bad(message: impl Into<String>) -> Reject {
    Reject::new("invalid_config", message)
}

/// Bank account codes become part of a ledger account ID in the core.
fn valid_code(code: &str) -> bool {
    !code.is_empty() && code.len() <= 64 && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl Config {
    /// The format a channel speaks and, where that format names it, the
    /// insurer's account as the bank knows it.
    pub fn wire(&self, channel: &str, account: &str) -> (WireFormat, Option<crate::iso20022::OrderingAccount>) {
        let format = self.channels.get(channel).map(|c| c.format).unwrap_or_default();
        let ordering = self.accounts.get(account).and_then(|a| a.at_bank.clone());
        (format, ordering.filter(|_| !format.is_own()))
    }

    pub fn is_business_day(&self, date: NaiveDate) -> bool {
        !matches!(date.weekday(), Weekday::Sat | Weekday::Sun) && !self.rules.holidays.contains(&date)
    }

    /// The account a run of this type on this channel is paid from or
    /// collected into.
    pub fn account_for(&self, channel: &str, run_type: RunType, fund: &str, currency: Currency) -> Option<&Id> {
        let bank = &self.channels.get(channel)?.bank;
        self.accounts
            .iter()
            .find(|(_, a)| &a.bank == bank && a.fund == fund && a.purpose == run_type.purpose() && a.currency == currency)
            .map(|(code, _)| code)
    }

    /// What stops each voter region reaching each channel on `today`.
    /// Empty means every region can submit through every channel.
    pub fn provisioning_gaps(&self, today: NaiveDate) -> Vec<String> {
        let mut gaps = Vec::new();
        for (id, channel) in &self.channels {
            for region in &self.rules.voter_regions {
                match channel.endpoints.get(region) {
                    None => gaps.push(format!("channel {id} has no endpoint in {region}")),
                    Some(e) if !e.whitelisted => gaps.push(format!("channel {id}: the bank has not whitelisted {region}")),
                    Some(e) if e.certificate_expires <= today => gaps.push(format!("channel {id}: the certificate in {region} has expired")),
                    Some(e) if channel.format == WireFormat::Iso20022 && e.client_certificate.is_none() => {
                        gaps.push(format!("channel {id}: {region} presents no client certificate to the bank"))
                    }
                    Some(_) => {}
                }
            }
        }
        gaps
    }

    pub fn validate(&self, today: NaiveDate) -> Result<(), Reject> {
        if self.rules.voter_regions.is_empty() {
            return Err(bad("at least one voter region is needed"));
        }
        if self.rules.drill_amount_minor <= 0 || self.rules.drill_account.number.is_empty() {
            return Err(bad("the evacuation drill needs an account to pay and a token amount"));
        }
        if self.funds.is_empty() {
            return Err(bad("at least one statutory fund is needed"));
        }
        for (id, channel) in &self.channels {
            if !self.banks.contains_key(&channel.bank) {
                return Err(bad(format!("channel {id} names an unknown bank")));
            }
        }
        for (code, account) in &self.accounts {
            let iso = self.channels.values().any(|c| c.bank == account.bank && c.format == WireFormat::Iso20022);
            let named = account
                .at_bank
                .as_ref()
                .is_some_and(|a| !a.name.trim().is_empty() && !a.number.trim().is_empty());
            if iso && !named {
                return Err(bad(format!(
                    "account {code} is at a bank spoken to in ISO 20022 and needs its holder and number"
                )));
            }
            if !valid_code(code) {
                return Err(bad(format!("account code {code:?} is not 1 to 64 letters, digits, hyphens or underscores")));
            }
            if !self.banks.contains_key(&account.bank) || !self.funds.contains(&account.fund) {
                return Err(bad(format!("account {code} names an unknown bank or fund")));
            }
        }
        for (run_type, route) in &self.routes {
            if route.approvals == 0 {
                return Err(bad(format!("{run_type:?} runs need at least one approver")));
            }
            if route.item_limit_minor <= 0 || route.run_limit_minor < route.item_limit_minor || route.day_limit_minor < route.run_limit_minor {
                return Err(bad(format!("{run_type:?} limits must rise from item to run to day")));
            }
            for id in std::iter::once(&route.primary).chain(&route.secondary).chain(&route.urgent) {
                let channel = self.channels.get(id).ok_or_else(|| bad(format!("{run_type:?} names unknown channel {id}")))?;
                if route.urgent.as_ref() == Some(id) && channel.kind != ChannelKind::Api {
                    return Err(bad(format!("the urgent channel for {run_type:?} must be an API channel")));
                }
                // Every fund can be paid through every channel the route may use.
                for fund in &self.funds {
                    if !self
                        .accounts
                        .values()
                        .any(|a| a.bank == channel.bank && &a.fund == fund && a.purpose == run_type.purpose())
                    {
                        return Err(bad(format!("{run_type:?} on channel {id} has no account for fund {fund}")));
                    }
                }
            }
            if route
                .secondary
                .as_ref()
                .is_some_and(|s| self.channels[s].bank == self.channels[&route.primary].bank)
            {
                return Err(bad(format!("the secondary channel for {run_type:?} must be at another bank")));
            }
        }
        if let Some(gap) = self.provisioning_gaps(today).into_iter().next() {
            return Err(Reject::new("region_not_provisioned", gap));
        }
        Ok(())
    }
}
