//! The wording of each definition a cover can be bound to, by version.
//!
//! The core keeps only the hash of a definition's text, and a claim's cover
//! check names the definitions it relied on as `id@version`. An assessor
//! needs the words themselves, and the words of the version the cover was
//! bound to, not today's. This library holds every version of every
//! definition library the service is given and serves one by ID and version.
//!
//! The core refuses to activate a second text under an ID and version it
//! already holds, so an ID and a version name one text. Each answer carries
//! the hash of the text served, which is the hash the cover's binding holds.

use std::collections::BTreeMap;

use ic_product::Definition;
use serde_json::{json, Value};

/// The definition libraries shipped in this repository, which the platform
/// activates in the core. A deployment adds its own with `IC_DEFINITIONS`.
pub const SHIPPED: [(&str, &str); 3] = [
    ("definitions/library.star", include_str!("../../../products/definitions/library.star")),
    ("definitions/aia.star", include_str!("../../../products/definitions/aia.star")),
    ("definitions/aia_wordings.star", include_str!("../../../products/definitions/aia_wordings.star")),
];

/// Every version of every definition, with its text.
#[derive(Debug, Default)]
pub struct Wordings {
    held: BTreeMap<(String, u32), Definition>,
}

impl Wordings {
    /// Load definition libraries, each a file name and its source. A second
    /// text for an ID and version already loaded is an error, as it is in
    /// the core: serving either would risk showing words the cover was not
    /// bound to. The same text twice is accepted.
    pub fn load<'a>(libraries: impl IntoIterator<Item = (&'a str, &'a str)>) -> Result<Self, String> {
        let mut held: BTreeMap<(String, u32), Definition> = BTreeMap::new();
        for (file, source) in libraries {
            for d in ic_product::load_definitions(file, source).map_err(|e| e.to_string())? {
                let key = (d.id.clone(), d.version);
                if held.get(&key).is_some_and(|had| had.text_hash != d.text_hash) {
                    return Err(format!(
                        "{file}: definition {} version {} is already loaded with different wording",
                        d.id, d.version
                    ));
                }
                held.insert(key, d);
            }
        }
        Ok(Wordings { held })
    }

    /// The libraries shipped in this repository.
    pub fn shipped() -> Result<Self, String> {
        Self::load(SHIPPED)
    }

    /// One definition as it was at one version, with its text.
    pub fn get(&self, id: &str, version: u32) -> Option<Value> {
        let d = self.held.get(&(id.to_string(), version))?;
        let versions: Vec<u32> = self
            .held
            .range((id.to_string(), 0)..=(id.to_string(), u32::MAX))
            .map(|((_, v), _)| *v)
            .collect();
        let mut out = row(d);
        out["text"] = json!(d.text);
        out["versions"] = json!(versions);
        Some(out)
    }

    /// Every definition and version, without the text.
    pub fn list(&self) -> Vec<Value> {
        self.held.values().map(row).collect()
    }
}

fn row(d: &Definition) -> Value {
    json!({ "id": d.id, "version": d.version, "effective_from": d.effective_from, "title": d.title, "text_hash": d.text_hash })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: &str =
        r#"DEFINITIONS = [definition("cancer", 1, "2027-01-01", "Cancer", "first words"), definition("cancer", 2, "2028-07-01", "Cancer", "second words")]"#;

    /// Purpose: a definition is served as it was at the version asked for,
    /// with the hash of exactly that text and the versions that exist.
    /// Why it matters: an old claim is assessed against the words its cover
    /// was bound to; serving the newest text for an old version would have an
    /// assessor decide a claim on wording the customer never bought.
    #[test]
    fn each_version_keeps_its_own_words() {
        let w = Wordings::load([("one.star", ONE)]).unwrap();
        let first = w.get("cancer", 1).unwrap();
        let second = w.get("cancer", 2).unwrap();
        assert_eq!(first["text"], "first words");
        assert_eq!(second["text"], "second words");
        assert_ne!(first["text_hash"], second["text_hash"]);
        assert_eq!(first["versions"], json!([1, 2]));
        assert_eq!(first["effective_from"], "2027-01-01");
        assert_eq!(w.get("cancer", 3), None);
        assert_eq!(w.get("cance", 1), None);
        let listed = w.list();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].get("text"), None, "the list does not carry the text");
    }

    /// Purpose: two libraries that give one ID and version different words
    /// are refused, the same words twice are accepted, and a library that
    /// does not parse is refused with its file named.
    /// Why it matters: the core holds one hash per ID and version. If the
    /// portal held two texts it could show an assessor the wrong one with no
    /// sign that anything was off.
    #[test]
    fn one_id_and_version_is_one_text() {
        let other = r#"DEFINITIONS = [definition("cancer", 1, "2027-01-01", "Cancer", "other words")]"#;
        let refused = Wordings::load([("one.star", ONE), ("two.star", other)]).unwrap_err();
        assert_eq!(refused, "two.star: definition cancer version 1 is already loaded with different wording");
        assert_eq!(Wordings::load([("one.star", ONE), ("again.star", ONE)]).unwrap().list().len(), 2);
        assert!(Wordings::load([("bad.star", "DEFINITIONS = [")]).unwrap_err().contains("bad.star"));
    }

    /// Purpose: the libraries shipped in the repository load, and an AIA
    /// wording is there in full under its document code.
    /// Why it matters: these are the definitions every shipped cover is
    /// bound to. A library that stopped loading would leave assessors with
    /// links that answer "not found" on every claim.
    #[test]
    fn the_shipped_libraries_load() {
        let w = Wordings::shipped().unwrap();
        let umbrella = w.get("aia-wording-1100-al-umb", 1).unwrap();
        assert!(umbrella["text"].as_str().unwrap().contains("AIA Living Umbrella Policy Wording"));
        assert_eq!(umbrella["text_hash"].as_str().unwrap().len(), 64);
        assert!(w.get("cancer", 2).is_some());
    }
}
