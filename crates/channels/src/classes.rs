//! Data classes. Every field a channel returns has a class, and a caller
//! sees a field only if they hold its class. A field above the caller's
//! classes is left out and named in the response's `masked` list, so the
//! caller knows something is there and that unmasking is a separate action.
//!
//! A response is built only through [`Shaper`], whose one way to add a field
//! takes the field's class. There is no way to put an unclassified field in
//! a response.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataClass {
    /// Nothing about a person: a product code, a status, a date.
    Ordinary,
    /// Who someone is and how to reach them.
    Personal,
    /// Money: premiums, balances, commission, bank details.
    Financial,
    /// Insurance facts that imply health: loadings, exclusion codes, the
    /// existence and type of a claim, waiver status, health benefit categories.
    InsuranceSensitive,
    /// Disclosures, medical evidence, diagnoses, decline reasons, assessor notes.
    Clinical,
}

impl DataClass {
    pub fn key(self) -> &'static str {
        match self {
            DataClass::Ordinary => "ordinary",
            DataClass::Personal => "personal",
            DataClass::Financial => "financial",
            DataClass::InsuranceSensitive => "insurance_sensitive",
            DataClass::Clinical => "clinical",
        }
    }
}

/// Builds one response object for a caller holding `held` classes.
#[derive(Debug)]
pub struct Shaper<'a> {
    held: &'a BTreeSet<DataClass>,
    body: Map<String, Value>,
    masked: Vec<String>,
}

impl<'a> Shaper<'a> {
    pub fn new(held: &'a BTreeSet<DataClass>) -> Self {
        Self {
            held,
            body: Map::new(),
            masked: Vec::new(),
        }
    }

    /// Add a field. It appears only if the caller holds its class.
    ///
    /// There is no other way to add one, so a field without a class does not
    /// compile:
    ///
    /// ```compile_fail
    /// use ic_channels::classes::{DataClass, Shaper};
    /// let held = [DataClass::Ordinary].into();
    /// let response = Shaper::new(&held).field("new_field", 1).finish();
    /// ```
    ///
    /// ```
    /// use ic_channels::classes::{DataClass, Shaper};
    /// let held = [DataClass::Ordinary].into();
    /// let response = Shaper::new(&held).field("new_field", DataClass::Ordinary, 1).finish();
    /// assert_eq!(response["new_field"], 1);
    /// ```
    pub fn field(mut self, name: &str, class: DataClass, value: impl Serialize) -> Self {
        if self.held.contains(&class) {
            // A value that cannot be represented is treated as absent rather
            // than failing the whole response.
            self.body.insert(name.to_string(), serde_json::to_value(value).unwrap_or(Value::Null));
        } else {
            self.masked.push(name.to_string());
        }
        self
    }

    /// The classes of the fields that were left out.
    pub fn any_masked(&self) -> bool {
        !self.masked.is_empty()
    }

    /// The response. `masked` is present only when something was left out.
    pub fn finish(mut self) -> Value {
        if !self.masked.is_empty() {
            self.body.insert("masked".to_string(), Value::from(self.masked));
        }
        Value::Object(self.body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Purpose: a field whose class the caller holds is returned, one whose class they do not hold
    /// is left out and named under `masked`, and a response with nothing left out has no `masked`.
    /// Why it matters: a collections officer must see the amount owed and must not see a loading.
    /// Saying a field was masked, rather than silently dropping it, is what lets staff ask for
    /// access properly instead of assuming the data does not exist.
    #[test]
    fn fields_above_the_callers_classes_are_masked_and_named() {
        let held: BTreeSet<DataClass> = [DataClass::Ordinary, DataClass::Financial].into();
        let shaped = Shaper::new(&held)
            .field("status", DataClass::Ordinary, "in_force")
            .field("outstanding_minor", DataClass::Financial, 1250)
            .field("loading", DataClass::InsuranceSensitive, "1.5")
            .field("diagnosis", DataClass::Clinical, "x");
        assert!(shaped.any_masked());
        assert_eq!(
            shaped.finish(),
            json!({ "status": "in_force", "outstanding_minor": 1250, "masked": ["loading", "diagnosis"] })
        );
        let open = Shaper::new(&held).field("status", DataClass::Ordinary, "lapsed");
        assert!(!open.any_masked());
        assert_eq!(open.finish(), json!({ "status": "lapsed" }));
    }

    /// Purpose: every class has a distinct, stable key.
    /// Why it matters: the keys are written to the access log and used in Cedar policy. Two classes
    /// sharing a key would let a grant for one stand in for the other.
    #[test]
    fn class_keys_are_distinct() {
        let all = [
            DataClass::Ordinary,
            DataClass::Personal,
            DataClass::Financial,
            DataClass::InsuranceSensitive,
            DataClass::Clinical,
        ];
        let keys: BTreeSet<&str> = all.iter().map(|c| c.key()).collect();
        assert_eq!(keys.len(), all.len());
        for c in all {
            assert_eq!(serde_json::to_value(c).unwrap(), json!(c.key()));
        }
    }
}
