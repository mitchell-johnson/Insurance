//! The wording a cover is bound to, by the hash the cover holds.
//!
//! The core keeps only the hash of each definition a cover is bound to
//! (`DefinitionBinding { id, version, text_hash }`), so that a claim is
//! assessed against the exact words in force when the cover started. The
//! words themselves are in the definitions library (`products/definitions`),
//! built into this service, and served to staff by that hash: a hash the
//! library does not hold is never answered with another version's words.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_product::loader::load_definitions;
use serde::Serialize;

/// The definitions library, as built into the service. Every file in
/// `products/definitions` is here (a test checks it).
pub const SOURCES: &[(&str, &str)] = &[
    ("library.star", include_str!("../../../products/definitions/library.star")),
    ("aia.star", include_str!("../../../products/definitions/aia.star")),
    ("aia_wordings.star", include_str!("../../../products/definitions/aia_wordings.star")),
];

/// One definition's words.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Wording {
    pub id: String,
    pub version: u32,
    pub title: String,
    pub effective_from: NaiveDate,
    pub text_hash: String,
    pub text: String,
}

/// Every definition, by the hash of its text.
#[derive(Debug, Clone, Default)]
pub struct WordingLibrary {
    by_hash: BTreeMap<String, Wording>,
}

impl WordingLibrary {
    /// The library from definition modules: `(file, source)` each.
    pub fn load(sources: &[(&str, &str)]) -> Result<Self, String> {
        let mut by_hash = BTreeMap::new();
        for (file, source) in sources {
            for d in load_definitions(file, source).map_err(|e| e.to_string())? {
                by_hash.insert(
                    d.text_hash.clone(),
                    Wording {
                        id: d.id,
                        version: d.version,
                        title: d.title,
                        effective_from: d.effective_from,
                        text_hash: d.text_hash,
                        text: d.text,
                    },
                );
            }
        }
        Ok(Self { by_hash })
    }

    /// The library built into the service.
    pub fn shipped() -> Result<Self, String> {
        Self::load(SOURCES)
    }

    /// The words with this hash, if the library holds them.
    pub fn get(&self, text_hash: &str) -> Option<&Wording> {
        self.by_hash.get(text_hash)
    }

    pub fn len(&self) -> usize {
        self.by_hash.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_hash.is_empty()
    }
}
