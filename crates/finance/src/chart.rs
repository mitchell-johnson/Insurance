//! The chart of accounts.
//!
//! The core ledger knows accounts by role: `premium_receivable`,
//! `earned_premium`, and so on, one set per policy or per partition. The
//! chart maps each role to one finance account, so that the finance ledger
//! holds one balance where the core holds thousands. Accounts fed this way
//! are control accounts and are locked against manual journals.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use ic_ledger::AccountClass;
use serde::{Deserialize, Serialize};

use crate::config::{content_hash, eval, ConfigError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChartAccount {
    pub code: String,
    pub name: String,
    pub class: AccountClass,
    /// Locked: only the feed from the core posts here.
    #[serde(default)]
    pub control: bool,
    /// The core account roles consolidated into this account.
    #[serde(default)]
    pub roles: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chart {
    pub version: u32,
    pub effective_from: NaiveDate,
    /// Where the year-end journal closes income and expense to.
    pub retained_earnings: String,
    pub accounts: Vec<ChartAccount>,
    #[serde(default)]
    pub content_hash: String,
}

fn invalid(message: impl Into<String>) -> ConfigError {
    ConfigError::Invalid(message.into())
}

impl Chart {
    /// Load a chart module. The module must define `CHART`.
    pub fn load(file: &str, source: &str) -> Result<Self, ConfigError> {
        let mut chart: Chart = eval(file, source, "CHART")?;
        chart.content_hash = content_hash(source);
        chart.validate()?;
        Ok(chart)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.version == 0 || self.accounts.is_empty() {
            return Err(invalid("a chart needs a version of at least 1 and at least one account"));
        }
        let mut codes = BTreeSet::new();
        let mut roles = BTreeSet::new();
        for a in &self.accounts {
            if a.code.is_empty() || a.code.contains('/') || a.name.is_empty() {
                return Err(invalid(format!("account `{}` needs a code without `/` and a name", a.code)));
            }
            if !codes.insert(a.code.as_str()) {
                return Err(invalid(format!("account {} is declared twice", a.code)));
            }
            if !a.roles.is_empty() && !a.control {
                return Err(invalid(format!("account {} is fed by the core, so it must be a control account", a.code)));
            }
            for role in &a.roles {
                if !roles.insert(role.as_str()) {
                    return Err(invalid(format!("role {role} is mapped to two accounts")));
                }
            }
        }
        match self.account(&self.retained_earnings) {
            Some(a) if a.class == AccountClass::Liability && !a.control => Ok(()),
            _ => Err(invalid("retained_earnings must name a liability account that is not a control account")),
        }
    }

    pub fn account(&self, code: &str) -> Option<&ChartAccount> {
        self.accounts.iter().find(|a| a.code == code)
    }

    /// The account a core role is consolidated into.
    pub fn account_for_role(&self, role: &str) -> Option<&ChartAccount> {
        self.accounts.iter().find(|a| a.roles.iter().any(|r| r == role))
    }

    /// Whether this chart may replace `current`: it must be the next
    /// version, keep every account with its class and its lock, and keep
    /// every role where it is mapped. Balances already posted would
    /// otherwise change meaning.
    pub fn may_replace(&self, current: &Chart) -> Result<(), ConfigError> {
        if self.version != current.version + 1 {
            return Err(invalid(format!("chart version {} cannot follow version {}", self.version, current.version)));
        }
        let mine: BTreeMap<&str, &ChartAccount> = self.accounts.iter().map(|a| (a.code.as_str(), a)).collect();
        for old in &current.accounts {
            let Some(new) = mine.get(old.code.as_str()) else {
                return Err(invalid(format!("account {} cannot be removed", old.code)));
            };
            if new.class != old.class || new.control != old.control {
                return Err(invalid(format!("account {} cannot change its class or its lock", old.code)));
            }
            if let Some(role) = old.roles.iter().find(|r| !new.roles.contains(r)) {
                return Err(invalid(format!("role {role} cannot be moved off account {}", old.code)));
            }
        }
        if self.retained_earnings != current.retained_earnings {
            return Err(invalid("the retained earnings account cannot change"));
        }
        Ok(())
    }
}
