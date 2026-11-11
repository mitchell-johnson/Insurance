//! Configuration state: everything activated through the log.
//!
//! Products, rate tables, definitions and index rates are versioned,
//! effective-dated and never removed while a policy may refer to them.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_money::DecStr;
use ic_product::{Definition, DefinitionBinding, ProductDefinition, RateTable};
use serde::{Deserialize, Serialize};

use crate::GroupId;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalog {
    /// Product versions by content hash.
    products: BTreeMap<String, ProductDefinition>,
    /// Product code -> (effective_from, version) -> content hash.
    #[serde(with = "versions_as_pairs")]
    product_versions: BTreeMap<String, BTreeMap<(NaiveDate, u32), String>>,
    /// Rate tables by ID and version.
    rate_tables: BTreeMap<String, BTreeMap<u32, RateTable>>,
    /// Definitions by ID and version. The core keeps the hash, not the wording.
    definitions: BTreeMap<String, BTreeMap<u32, Definition>>,
    /// Index rates (for example CPI for indexation) by code and effective date.
    index_rates: BTreeMap<String, BTreeMap<NaiveDate, DecStr>>,
}

impl Catalog {
    /// Whether a product may be activated. A version is immutable once it is
    /// active: policies are priced from it for as long as they live, so the
    /// same content may be activated again (a repeated request) and anything
    /// else under a hash or version already in use is refused.
    pub fn check_product(&self, p: &ProductDefinition) -> Result<(), String> {
        if p.content_hash.is_empty() {
            return Err(format!("product {} carries no content hash; load it from its source", p.code));
        }
        if self.products.get(&p.content_hash).is_some_and(|active| active != p) {
            return Err(format!("another product is active under the content hash of {} version {}", p.code, p.version));
        }
        let bound = self.product_versions.get(&p.code).and_then(|v| v.get(&(p.effective_from, p.version)));
        if bound.is_some_and(|hash| *hash != p.content_hash) {
            return Err(format!("product {} version {} is already active with other content", p.code, p.version));
        }
        Ok(())
    }

    /// Whether a rate table may be activated: as for products, a version is
    /// immutable once active.
    pub fn check_rate_table(&self, t: &RateTable) -> Result<(), String> {
        if t.content_hash.is_empty() {
            return Err(format!("rate table {} carries no content hash; load it from its source", t.id));
        }
        let active = self.rate_tables.get(&t.id).and_then(|v| v.get(&t.version));
        if active.is_some_and(|active| active != t) {
            return Err(format!("rate table {} version {} is already active with other content", t.id, t.version));
        }
        Ok(())
    }

    /// Whether a set of definitions may be activated: covers are bound to a
    /// version's wording by its hash, so a version cannot change its wording.
    pub fn check_definitions(&self, defs: &[Definition]) -> Result<(), String> {
        for d in defs {
            if d.text_hash.is_empty() {
                return Err(format!("definition {} carries no hash of its wording; load it from its source", d.id));
            }
            let active = self.definitions.get(&d.id).and_then(|v| v.get(&d.version));
            if active.is_some_and(|a| a.text_hash != d.text_hash || a.effective_from != d.effective_from) {
                return Err(format!("definition {} version {} is already active with other wording", d.id, d.version));
            }
        }
        Ok(())
    }

    /// Whether an index rate may be activated. A rate that has taken effect
    /// has been used to index policies and cannot be changed; a correction
    /// is a new rate from a later date. A rate still in the future can be.
    pub fn check_index_rate(&self, code: &str, effective_from: NaiveDate, rate: &DecStr, today: NaiveDate) -> Result<(), String> {
        let active = self.index_rates.get(code).and_then(|r| r.get(&effective_from));
        if effective_from <= today && active.is_some_and(|a| a != rate) {
            return Err(format!("index rate {code} from {effective_from} is already in effect at another value"));
        }
        Ok(())
    }

    pub fn activate_product(&mut self, p: ProductDefinition) {
        self.product_versions
            .entry(p.code.clone())
            .or_default()
            .insert((p.effective_from, p.version), p.content_hash.clone());
        self.products.insert(p.content_hash.clone(), p);
    }

    pub fn product(&self, hash: &str) -> Option<&ProductDefinition> {
        self.products.get(hash)
    }

    /// The product version in force for new business on a date.
    pub fn product_on(&self, code: &str, on: NaiveDate) -> Option<&ProductDefinition> {
        self.product_versions
            .get(code)?
            .iter()
            .rev()
            .find(|((from, _), _)| *from <= on)
            .and_then(|(_, hash)| self.products.get(hash))
    }

    pub fn activate_rate_table(&mut self, t: RateTable) {
        self.rate_tables.entry(t.id.clone()).or_default().insert(t.version, t);
    }

    /// The table version in force on a date for a group. A table scoped to a
    /// named set of groups wins over a global one for those groups, and is
    /// invisible to everyone else.
    pub fn rate_table_on(&self, id: &str, on: NaiveDate, group: &GroupId) -> Option<&RateTable> {
        let versions = self.rate_tables.get(id)?;
        let pick = |scoped: bool| {
            versions
                .values()
                .filter(|t| t.effective_from <= on)
                .filter(|t| match &t.scope {
                    Some(groups) => scoped && groups.contains(&group.0),
                    None => !scoped,
                })
                .max_by_key(|t| (t.effective_from, t.version))
        };
        pick(true).or_else(|| pick(false))
    }

    /// Scoped tables that must travel with a group or policy when it moves.
    pub fn scoped_tables_for(&self, group: &GroupId) -> Vec<RateTable> {
        self.rate_tables
            .values()
            .flat_map(|v| v.values())
            .filter(|t| t.scope.as_ref().is_some_and(|g| g.contains(&group.0)))
            .cloned()
            .collect()
    }

    pub fn activate_definitions(&mut self, defs: Vec<Definition>) {
        for mut d in defs {
            d.text.clear();
            self.definitions.entry(d.id.clone()).or_default().insert(d.version, d);
        }
    }

    /// Bind a cover to the definition version in force at its start date.
    pub fn bind_definition(&self, id: &str, cover_start: NaiveDate) -> Option<DefinitionBinding> {
        self.definitions
            .get(id)?
            .values()
            .filter(|d| d.effective_from <= cover_start)
            .max_by_key(|d| (d.effective_from, d.version))
            .map(|d| DefinitionBinding {
                id: d.id.clone(),
                version: d.version,
                text_hash: d.text_hash.clone(),
            })
    }

    pub fn activate_index_rate(&mut self, code: String, effective_from: NaiveDate, rate: DecStr) {
        self.index_rates.entry(code).or_default().insert(effective_from, rate);
    }

    pub fn index_rate_on(&self, code: &str, on: NaiveDate) -> Option<&DecStr> {
        self.index_rates.get(code)?.range(..=on).next_back().map(|(_, r)| r)
    }
}

/// JSON object keys must be strings, and this map is keyed by a pair. It is
/// encoded as a list of entries so the catalog can be part of the state hash
/// and of a snapshot.
mod versions_as_pairs {
    use std::collections::BTreeMap;

    use chrono::NaiveDate;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    type Versions = BTreeMap<String, BTreeMap<(NaiveDate, u32), String>>;

    pub fn serialize<S: Serializer>(map: &Versions, s: S) -> Result<S::Ok, S::Error> {
        let pairs: BTreeMap<&String, Vec<(&(NaiveDate, u32), &String)>> = map.iter().map(|(code, versions)| (code, versions.iter().collect())).collect();
        pairs.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Versions, D::Error> {
        let pairs: BTreeMap<String, Vec<((NaiveDate, u32), String)>> = Deserialize::deserialize(d)?;
        Ok(pairs.into_iter().map(|(code, versions)| (code, versions.into_iter().collect())).collect())
    }
}
