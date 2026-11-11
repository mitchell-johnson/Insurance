//! Starlark to plain data. Starlark is an authoring language only: every
//! loader in this crate evaluates a module once, takes one named value out of
//! it as JSON and validates that into a typed Rust value. Nothing that runs
//! at decision time or at claim time touches Starlark.

use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use starlark::environment::{Globals, Module};
use starlark::eval::Evaluator;
use starlark::syntax::{AstModule, Dialect};

const PRELUDE: &str = include_str!("prelude.star");

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LoadError {
    #[error("starlark: {0}")]
    Starlark(String),
    #[error("{file}: `{symbol}` is not defined")]
    MissingSymbol { file: String, symbol: &'static str },
    #[error("{file}: does not match the schema: {error}")]
    Schema { file: String, error: String },
    #[error("{file}: invalid: {error}")]
    Invalid { file: String, error: String },
}

pub(crate) fn invalid(file: &str, error: impl Into<String>) -> LoadError {
    LoadError::Invalid {
        file: file.to_string(),
        error: error.into(),
    }
}

/// The version identity of a configuration source: its text and the
/// authoring kit it was written against.
pub fn content_hash(source: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"ic.underwriting.config.v1\0");
    h.update(Sha256::digest(PRELUDE.as_bytes()));
    h.update(source.as_bytes());
    hex::encode(h.finalize())
}

pub(crate) fn eval<T: DeserializeOwned>(file: &str, source: &str, symbol: &'static str) -> Result<T, LoadError> {
    let sl = |e: starlark::Error| LoadError::Starlark(e.to_string());
    let prelude = AstModule::parse("prelude.star", PRELUDE.to_owned(), &Dialect::Standard).map_err(sl)?;
    let ast = AstModule::parse(file, source.to_owned(), &Dialect::Standard).map_err(sl)?;
    let globals = Globals::standard();
    let json = Module::with_temp_heap(|module| -> Result<serde_json::Value, LoadError> {
        {
            let mut ev = Evaluator::new(&module);
            ev.eval_module(prelude, &globals).map_err(sl)?;
            ev.eval_module(ast, &globals).map_err(sl)?;
        }
        let v = module.get(symbol).ok_or_else(|| LoadError::MissingSymbol {
            file: file.to_string(),
            symbol,
        })?;
        v.to_json_value().map_err(|e| LoadError::Starlark(e.to_string()))
    })?;
    serde_json::from_value(json).map_err(|e| LoadError::Schema {
        file: file.to_string(),
        error: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Purpose: a source that is not valid Starlark, one that fails while running, one without
    /// the expected symbol, one whose value cannot become JSON and one of the wrong shape are
    /// each refused with an error that names what is wrong.
    /// Why it matters: underwriting rules and code lists are activated with dual approval; a
    /// broken file has to fail at load with a usable message, never half-load.
    #[test]
    fn broken_sources_are_refused_with_the_reason() {
        assert!(matches!(eval::<u32>("f.star", "X = (", "X"), Err(LoadError::Starlark(_))));
        assert!(matches!(eval::<u32>("f.star", "X = 1 // 0", "X"), Err(LoadError::Starlark(_))));
        assert_eq!(
            eval::<u32>("f.star", "Y = 1", "X"),
            Err(LoadError::MissingSymbol {
                file: "f.star".into(),
                symbol: "X"
            })
        );
        assert!(matches!(eval::<u32>("f.star", "def X():\n  pass", "X"), Err(LoadError::Starlark(_))));
        let e = eval::<u32>("f.star", "X = 'text'", "X").unwrap_err();
        assert!(matches!(e, LoadError::Schema { .. }));
        assert!(e.to_string().starts_with("f.star: does not match the schema"));
        assert_eq!(eval::<u32>("f.star", "X = dollars(2)", "X"), Ok(200));
    }

    /// Purpose: the content hash changes with any change to the source and is stable otherwise.
    /// Why it matters: every decision records the hash of the rules and questions it used; two
    /// different rule texts under one hash would make a decision impossible to reproduce.
    #[test]
    fn content_hash_identifies_the_source() {
        assert_eq!(content_hash("A = 1"), content_hash("A = 1"));
        assert_ne!(content_hash("A = 1"), content_hash("A = 2"));
        assert_eq!(content_hash("A = 1").len(), 64);
    }
}
