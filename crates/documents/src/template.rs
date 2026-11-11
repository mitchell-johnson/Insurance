//! Templates: configuration, versioned by content hash.
//!
//! A template is composed from parts, so a clause used by ten products is
//! one artefact. It has a primary language and may have translations, all
//! versioned together; the primary controls on any difference, and a
//! translated document says so.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::kinds::DocumentKind;
use crate::render::{self, Compiled, RenderError};

/// A shared piece of wording: a definition, a cover clause, a standard
/// paragraph. Immutable once registered under its version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Part {
    pub id: String,
    pub version: u32,
    /// Language code to source text.
    pub bodies: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PartRef {
    pub id: String,
    pub version: u32,
}

/// The product version a template describes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProductBinding {
    pub code: String,
    pub version: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlternateFormat {
    LargePrint,
    PlainText,
    Audio,
    Braille,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TemplateRef {
    pub id: String,
    pub version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Template {
    pub id: String,
    pub version: u32,
    pub kind: DocumentKind,
    pub title: String,
    /// The product versions this template describes. Empty for documents
    /// that belong to no product, such as a complaint outcome.
    #[serde(default)]
    pub products: Vec<ProductBinding>,
    /// The definitions library version the wording was written against.
    #[serde(default)]
    pub definitions_version: Option<String>,
    pub primary_language: String,
    /// Language code to source text. Must include the primary language.
    pub bodies: BTreeMap<String, String>,
    /// For each translation, the sentence saying which language controls.
    #[serde(default)]
    pub controlling_statements: BTreeMap<String, String>,
    #[serde(default)]
    pub parts: Vec<PartRef>,
    /// The other formats the document can be supplied in.
    pub alternate_formats: Vec<AlternateFormat>,
    /// For a mandatory notice with a notice period: the least number of
    /// days between proven delivery and what the notice warns of.
    #[serde(default)]
    pub minimum_notice_days: Option<u32>,
    /// When the wording is next due for review.
    pub review_by: NaiveDate,
    pub author: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    #[error("{0}")]
    Invalid(String),
    #[error("language `{language}`: {error}")]
    Render { language: String, error: RenderError },
}

fn invalid(message: impl Into<String>) -> TemplateError {
    TemplateError::Invalid(message.into())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

impl Part {
    pub fn content_hash(&self) -> String {
        sha256_hex(&serde_json::to_vec(self).unwrap_or_default())
    }
}

impl Template {
    pub fn reference(&self) -> TemplateRef {
        TemplateRef {
            id: self.id.clone(),
            version: self.version,
        }
    }

    /// The hash that identifies this exact wording: the template and every
    /// part it is composed from.
    pub fn content_hash(&self, parts: &BTreeMap<PartRef, Part>) -> String {
        let mut h = Sha256::new();
        h.update(serde_json::to_vec(self).unwrap_or_default());
        for r in &self.parts {
            h.update(parts.get(r).map(Part::content_hash).unwrap_or_default());
        }
        hex::encode(h.finalize())
    }

    pub fn languages(&self) -> impl Iterator<Item = &String> {
        self.bodies.keys()
    }

    /// Compile one language version, with parts in the same language where
    /// they have it and in the primary language where they do not.
    pub fn compile(&self, language: &str, parts: &BTreeMap<PartRef, Part>) -> Result<Compiled, TemplateError> {
        let source = self
            .bodies
            .get(language)
            .ok_or_else(|| invalid(format!("the template has no `{language}` version")))?;
        let mut sources = BTreeMap::new();
        for r in &self.parts {
            let part = parts
                .get(r)
                .ok_or_else(|| invalid(format!("part {} version {} is not registered", r.id, r.version)))?;
            let body = part
                .bodies
                .get(language)
                .or_else(|| part.bodies.get(&self.primary_language))
                .ok_or_else(|| invalid(format!("part {} has no `{}` version", r.id, self.primary_language)))?;
            sources.insert(r.id.clone(), body.clone());
        }
        render::compile(source, &sources).map_err(|error| TemplateError::Render {
            language: language.to_string(),
            error,
        })
    }

    /// Everything that can be checked about a template without data.
    /// Returns the fields the template requires.
    pub fn validate(&self, parts: &BTreeMap<PartRef, Part>, max_reading_grade_x10: u32) -> Result<BTreeSet<String>, TemplateError> {
        if self.id.is_empty() || self.version == 0 {
            return Err(invalid("a template needs an id and a version of one or more"));
        }
        if self.author.is_empty() || self.title.is_empty() {
            return Err(invalid("a template needs a title and an author"));
        }
        if self.alternate_formats.is_empty() {
            return Err(invalid("a template must be available in at least one alternate format"));
        }
        if self.minimum_notice_days.is_some() && !self.kind.requires_proof() {
            return Err(invalid("only a document that must be proven delivered can carry a notice period"));
        }
        let primary = self.compile(&self.primary_language, parts)?;
        let grade = render::reading_grade_x10(&primary.prose());
        if grade > max_reading_grade_x10 {
            return Err(invalid(format!(
                "the wording reads at grade {}.{}, above the limit of {}.{}",
                grade / 10,
                grade % 10,
                max_reading_grade_x10 / 10,
                max_reading_grade_x10 % 10
            )));
        }
        let fields = primary.fields();
        for language in self.languages().filter(|l| **l != self.primary_language) {
            let translated = self.compile(language, parts)?.fields();
            if translated != fields {
                return Err(invalid(format!("the `{language}` version does not use the same fields as the primary version")));
            }
            if self.controlling_statements.get(language).is_none_or(String::is_empty) {
                return Err(invalid(format!("the `{language}` version does not say which language controls")));
            }
        }
        Ok(fields.required)
    }

    /// Render the document. A pure function: the same template, parts,
    /// language and data always give the same text.
    pub fn render(&self, parts: &BTreeMap<PartRef, Part>, language: &str, data: &Value) -> Result<String, TemplateError> {
        let mut text = self.compile(language, parts)?.render(data).map_err(|error| TemplateError::Render {
            language: language.to_string(),
            error,
        })?;
        if language != self.primary_language {
            text.push_str("\n\n");
            text.push_str(self.controlling_statements.get(language).map(String::as_str).unwrap_or_default());
        }
        Ok(text)
    }
}
