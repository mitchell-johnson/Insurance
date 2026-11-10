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
    product_versions: BTreeMap<String, BTreeMap<(NaiveDate, u32), String>>,
    /// Rate tables by ID and version.
    rate_tables: BTreeMap<String, BTreeMap<u32, RateTable>>,
    /// Definitions by ID and version. The core keeps the hash, not the wording.
    definitions: BTreeMap<String, BTreeMap<u32, Definition>>,
    /// Index rates (for example CPI for indexation) by code and effective date.
    index_rates: BTreeMap<String, BTreeMap<NaiveDate, DecStr>>,
}

impl Catalog {
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

    /// Number of configuration objects held: measured per partition, because
    /// configuration state grows slowly and never shrinks.
    pub fn size(&self) -> usize {
        self.products.len()
            + self.rate_tables.values().map(BTreeMap::len).sum::<usize>()
            + self.definitions.values().map(BTreeMap::len).sum::<usize>()
            + self.index_rates.values().map(BTreeMap::len).sum::<usize>()
    }
}
