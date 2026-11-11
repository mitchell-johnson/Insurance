//! Loading finance configuration from Starlark into typed, validated values.

use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use starlark::environment::{Globals, Module};
use starlark::eval::Evaluator;
use starlark::syntax::{AstModule, Dialect};

const PRELUDE: &str = include_str!("prelude.star");

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("starlark: {0}")]
    Starlark(String),
    #[error("{file}: `{symbol}` is not defined")]
    MissingSymbol { file: String, symbol: &'static str },
    #[error("{file}: does not match the schema: {error}")]
    Schema { file: String, error: String },
    #[error("{0}")]
    Invalid(String),
}

/// The hash a configuration version is activated by. The authoring kit is
/// part of what the source means.
pub fn content_hash(source: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"ic.finance.config.v1\0");
    h.update(Sha256::digest(PRELUDE.as_bytes()));
    h.update(source.as_bytes());
    hex::encode(h.finalize())
}

/// Evaluate a module and read one symbol from it as typed data.
pub fn eval<T: DeserializeOwned>(file: &str, source: &str, symbol: &'static str) -> Result<T, ConfigError> {
    let sl = |e: starlark::Error| ConfigError::Starlark(e.to_string());
    let prelude = AstModule::parse("prelude.star", PRELUDE.to_owned(), &Dialect::Standard).map_err(sl)?;
    let ast = AstModule::parse(file, source.to_owned(), &Dialect::Standard).map_err(sl)?;
    let globals = Globals::standard();
    let json = Module::with_temp_heap(|module| -> Result<serde_json::Value, ConfigError> {
        {
            let mut ev = Evaluator::new(&module);
            ev.eval_module(prelude, &globals).map_err(sl)?;
            ev.eval_module(ast, &globals).map_err(sl)?;
        }
        let v = module.get(symbol).ok_or_else(|| ConfigError::MissingSymbol {
            file: file.to_string(),
            symbol,
        })?;
        v.to_json_value().map_err(|e| ConfigError::Starlark(e.to_string()))
    })?;
    serde_json::from_value(json).map_err(|e| ConfigError::Schema {
        file: file.to_string(),
        error: e.to_string(),
    })
}
