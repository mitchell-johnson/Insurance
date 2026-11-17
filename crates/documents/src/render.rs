//! Rendering: a pure function of template source, parts and data.
//!
//! The template language is deliberately small, so that what a template
//! needs from the data can be read off the template itself:
//!
//! - `{{path.to.field}}` writes a value. `|money` formats integer minor
//!   units (a number, or the text of one) as dollars, `|date` formats an
//!   ISO date in words.
//! - `{{#each items}} ... {{/each}}` repeats for each element of a list.
//!   Inside, paths are relative to the element, `this` is the element, and
//!   `@root.path` reaches the top of the data.
//! - `{{#if field}} ... {{/if}}` and `{{#unless field}} ... {{/unless}}`.
//! - `{{> part-id}}` includes a shared part, such as a clause used by many
//!   products.
//!
//! A value the template needs and the data does not have is an error. A
//! document is never issued with a blank where a figure should be.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

/// Parts may include parts, this deep and no deeper. A cycle hits the limit.
const MAX_INCLUDE_DEPTH: u32 = 8;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RenderError {
    #[error("the template is malformed: {0}")]
    Syntax(String),
    #[error("the template includes part `{0}`, which it does not declare")]
    UnknownPart(String),
    #[error("parts are nested too deeply at `{0}` (a part may be including itself)")]
    IncludeDepth(String),
    #[error("the data has no value for `{0}`")]
    Missing(String),
    #[error("`{path}` cannot be written as {wanted}")]
    WrongType { path: String, wanted: &'static str },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Plain,
    Money,
    Date,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    Text(String),
    Var { path: String, format: Format },
    Each { path: String, body: Vec<Node> },
    If { path: String, negate: bool, body: Vec<Node> },
}

/// A parsed template with its parts already included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compiled {
    nodes: Vec<Node>,
}

/// The fields a template reads, as paths into the data. A list element's
/// fields are written `list[].field`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Fields {
    /// Without these, rendering fails.
    pub required: BTreeSet<String>,
    /// Read only inside a condition, or as a condition.
    pub optional: BTreeSet<String>,
}

enum Tag {
    Var(String, Format),
    Each(String),
    If(String, bool),
    Close(&'static str),
    Include(String),
}

fn syntax(message: impl Into<String>) -> RenderError {
    RenderError::Syntax(message.into())
}

fn valid_path(path: &str) -> bool {
    let body = path.strip_prefix("@root.").unwrap_or(path);
    !body.is_empty()
        && body
            .split('.')
            .all(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
}

fn path_of(text: &str) -> Result<String, RenderError> {
    let path = text.trim();
    if valid_path(path) {
        Ok(path.to_string())
    } else {
        Err(syntax(format!("`{path}` is not a field path")))
    }
}

fn tag_of(inner: &str) -> Result<Tag, RenderError> {
    let inner = inner.trim();
    if let Some(rest) = inner.strip_prefix("#each ") {
        Ok(Tag::Each(path_of(rest)?))
    } else if let Some(rest) = inner.strip_prefix("#if ") {
        Ok(Tag::If(path_of(rest)?, false))
    } else if let Some(rest) = inner.strip_prefix("#unless ") {
        Ok(Tag::If(path_of(rest)?, true))
    } else if let Some(rest) = inner.strip_prefix('/') {
        match rest.trim() {
            "each" => Ok(Tag::Close("each")),
            "if" => Ok(Tag::Close("if")),
            "unless" => Ok(Tag::Close("unless")),
            other => Err(syntax(format!("`{{{{/{other}}}}}` closes nothing"))),
        }
    } else if let Some(rest) = inner.strip_prefix('>') {
        let id = rest.trim();
        if id.is_empty() {
            return Err(syntax("an include names no part"));
        }
        Ok(Tag::Include(id.to_string()))
    } else {
        let (path, format) = match inner.split_once('|') {
            None => (inner, Format::Plain),
            Some((p, f)) => (
                p,
                match f.trim() {
                    "money" => Format::Money,
                    "date" => Format::Date,
                    other => return Err(syntax(format!("`{other}` is not a format"))),
                },
            ),
        };
        Ok(Tag::Var(path_of(path)?, format))
    }
}

struct Frame {
    /// The block this frame is the body of: `None` at the top of a source.
    block: Option<(Tag, &'static str)>,
    nodes: Vec<Node>,
}

fn parse_into(source: &str, parts: &BTreeMap<String, String>, depth: u32, name: &str) -> Result<Vec<Node>, RenderError> {
    if depth > MAX_INCLUDE_DEPTH {
        return Err(RenderError::IncludeDepth(name.to_string()));
    }
    let mut stack = vec![Frame { block: None, nodes: vec![] }];
    let mut rest = source;
    loop {
        let Some(open) = rest.find("{{") else {
            if !rest.is_empty() {
                stack.last_mut().expect("the top frame").nodes.push(Node::Text(rest.to_string()));
            }
            break;
        };
        if open > 0 {
            stack.last_mut().expect("the top frame").nodes.push(Node::Text(rest[..open].to_string()));
        }
        let after = &rest[open + 2..];
        let close = after.find("}}").ok_or_else(|| syntax("`{{` is never closed"))?;
        let tag = tag_of(&after[..close])?;
        rest = &after[close + 2..];
        match tag {
            Tag::Var(path, format) => stack.last_mut().expect("the top frame").nodes.push(Node::Var { path, format }),
            Tag::Include(id) => {
                let body = parts.get(&id).ok_or_else(|| RenderError::UnknownPart(id.clone()))?;
                let nodes = parse_into(body, parts, depth + 1, &id)?;
                stack.last_mut().expect("the top frame").nodes.extend(nodes);
            }
            Tag::Each(path) => stack.push(Frame {
                block: Some((Tag::Each(path), "each")),
                nodes: vec![],
            }),
            Tag::If(path, negate) => stack.push(Frame {
                block: Some((Tag::If(path, negate), if negate { "unless" } else { "if" })),
                nodes: vec![],
            }),
            Tag::Close(word) => {
                let frame = stack.pop().expect("the top frame");
                let Some((block, opened)) = frame.block else {
                    return Err(syntax(format!("`{{{{/{word}}}}}` has no opening tag")));
                };
                if opened != word {
                    return Err(syntax(format!("`{{{{#{opened}}}}}` is closed by `{{{{/{word}}}}}`")));
                }
                let node = match block {
                    Tag::Each(path) => Node::Each { path, body: frame.nodes },
                    Tag::If(path, negate) => Node::If {
                        path,
                        negate,
                        body: frame.nodes,
                    },
                    // Only blocks are pushed with a frame.
                    Tag::Var(..) | Tag::Close(_) | Tag::Include(_) => return Err(syntax("a tag that is not a block was opened")),
                };
                stack.last_mut().ok_or_else(|| syntax("a block closes above the top"))?.nodes.push(node);
            }
        }
    }
    let top = stack.pop().expect("the top frame");
    if let Some((_, opened)) = top.block {
        return Err(syntax(format!("`{{{{#{opened}}}}}` is never closed")));
    }
    Ok(top.nodes)
}

/// Parse a template, including the parts it names.
pub fn compile(source: &str, parts: &BTreeMap<String, String>) -> Result<Compiled, RenderError> {
    Ok(Compiled {
        nodes: parse_into(source, parts, 0, "template")?,
    })
}

fn collect(nodes: &[Node], scope: &str, conditional: bool, out: &mut Fields) {
    let full = |path: &str| match path.strip_prefix("@root.") {
        Some(root) => root.to_string(),
        None if path == "this" => scope.trim_end_matches('.').to_string(),
        None => format!("{scope}{path}"),
    };
    for node in nodes {
        match node {
            Node::Text(_) => {}
            Node::Var { path, .. } => {
                if conditional {
                    out.optional.insert(full(path));
                } else {
                    out.required.insert(full(path));
                }
            }
            Node::Each { path, body } => {
                let list = full(path);
                if conditional {
                    out.optional.insert(list.clone());
                } else {
                    out.required.insert(list.clone());
                }
                collect(body, &format!("{list}[]."), conditional, out);
            }
            Node::If { path, body, .. } => {
                out.optional.insert(full(path));
                collect(body, scope, true, out);
            }
        }
    }
}

impl Compiled {
    /// What the template reads from the data.
    pub fn fields(&self) -> Fields {
        let mut out = Fields::default();
        collect(&self.nodes, "", false, &mut out);
        // A field that is required somewhere is not optional.
        out.optional = out.optional.difference(&out.required).cloned().collect();
        out
    }

    /// Render against data. The same template and data always give the
    /// same text.
    pub fn render(&self, data: &Value) -> Result<String, RenderError> {
        let mut out = String::new();
        render_nodes(&self.nodes, data, data, &mut out)?;
        Ok(out)
    }

    /// The template's fixed words, without its fields: what a reader would
    /// see of the wording itself. Used to measure reading level.
    pub fn prose(&self) -> String {
        fn walk(nodes: &[Node], out: &mut String) {
            for node in nodes {
                match node {
                    Node::Text(t) => out.push_str(t),
                    Node::Var { .. } => out.push_str(" it "),
                    Node::Each { body, .. } | Node::If { body, .. } => walk(body, out),
                }
            }
        }
        let mut out = String::new();
        walk(&self.nodes, &mut out);
        out
    }
}

fn lookup<'a>(path: &str, root: &'a Value, scope: &'a Value) -> Option<&'a Value> {
    let (mut at, path) = match path.strip_prefix("@root.") {
        Some(rest) => (root, rest),
        None => (scope, path),
    };
    if path == "this" {
        return Some(at);
    }
    for segment in path.split('.') {
        at = at.as_object()?.get(segment)?;
    }
    Some(at)
}

fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::Object(_)) => true,
    }
}

/// Minor units as dollars and cents with thousands separators.
pub fn money(minor: i64) -> String {
    let sign = if minor < 0 { "-" } else { "" };
    let abs = minor.unsigned_abs();
    let whole = (abs / 100).to_string();
    let mut grouped = String::new();
    for (i, c) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(c);
    }
    format!("{sign}${grouped}.{:02}", abs % 100)
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// An ISO date in words: `2027-11-15` is `15 November 2027`.
pub fn date_in_words(iso: &str) -> Option<String> {
    use chrono::Datelike;
    let date = chrono::NaiveDate::parse_from_str(iso, "%Y-%m-%d").ok()?;
    Some(format!("{} {} {}", date.day(), MONTHS[date.month0() as usize], date.year()))
}

fn write_value(path: &str, format: Format, value: &Value, out: &mut String) -> Result<(), RenderError> {
    let wrong = |wanted| RenderError::WrongType {
        path: path.to_string(),
        wanted,
    };
    match format {
        // Whole minor units, as a number or as the text of one (a requested
        // letter's detail is text).
        Format::Money => {
            let minor = value.as_i64().or_else(|| value.as_str().and_then(|s| s.parse().ok()));
            out.push_str(&money(minor.ok_or_else(|| wrong("money (whole minor units)"))?))
        }
        Format::Date => out.push_str(&value.as_str().and_then(date_in_words).ok_or_else(|| wrong("a date (YYYY-MM-DD)"))?),
        Format::Plain => match value {
            Value::String(s) => out.push_str(s),
            Value::Number(n) => out.push_str(&n.to_string()),
            Value::Bool(b) => out.push_str(if *b { "yes" } else { "no" }),
            Value::Null | Value::Array(_) | Value::Object(_) => return Err(wrong("text")),
        },
    }
    Ok(())
}

fn render_nodes(nodes: &[Node], root: &Value, scope: &Value, out: &mut String) -> Result<(), RenderError> {
    for node in nodes {
        match node {
            Node::Text(text) => out.push_str(text),
            Node::Var { path, format } => {
                let value = lookup(path, root, scope).ok_or_else(|| RenderError::Missing(path.clone()))?;
                write_value(path, *format, value, out)?;
            }
            Node::Each { path, body } => {
                let list = lookup(path, root, scope).ok_or_else(|| RenderError::Missing(path.clone()))?;
                let items = list.as_array().ok_or_else(|| RenderError::WrongType {
                    path: path.clone(),
                    wanted: "a list",
                })?;
                for item in items {
                    render_nodes(body, root, item, out)?;
                }
            }
            Node::If { path, negate, body } => {
                if truthy(lookup(path, root, scope)) != *negate {
                    render_nodes(body, root, scope, out)?;
                }
            }
        }
    }
    Ok(())
}

/// The field paths a piece of data supplies, in the notation of [`Fields`].
pub fn supplied(data: &Value) -> BTreeSet<String> {
    fn walk(value: &Value, prefix: &str, out: &mut BTreeSet<String>) {
        match value {
            Value::Object(map) => {
                for (key, inner) in map {
                    let path = if prefix.is_empty() { key.clone() } else { format!("{prefix}.{key}") };
                    out.insert(path.clone());
                    walk(inner, &path, out);
                }
            }
            Value::Array(items) => {
                let path = format!("{prefix}[]");
                for item in items {
                    walk(item, &path, out);
                }
            }
            _ => {}
        }
    }
    let mut out = BTreeSet::new();
    walk(data, "", &mut out);
    out
}

fn syllables(word: &str) -> u64 {
    let mut count = 0;
    let mut in_vowels = false;
    for c in word.chars() {
        let vowel = matches!(c.to_ascii_lowercase(), 'a' | 'e' | 'i' | 'o' | 'u' | 'y');
        if vowel && !in_vowels {
            count += 1;
        }
        in_vowels = vowel;
    }
    if count > 1 && word.to_ascii_lowercase().ends_with('e') && !word.to_ascii_lowercase().ends_with("le") {
        count -= 1;
    }
    count.max(1)
}

/// The Flesch-Kincaid school grade of a text, times ten, in whole-number
/// arithmetic so every replica computes the same figure. A text with no
/// words has grade zero.
pub fn reading_grade_x10(text: &str) -> u32 {
    let words: Vec<&str> = text.split(|c: char| !c.is_alphabetic() && c != '\'').filter(|w| !w.is_empty()).collect();
    if words.is_empty() {
        return 0;
    }
    let sentences = text.split(['.', '!', '?', '\n']).filter(|s| s.chars().any(char::is_alphabetic)).count().max(1) as i64;
    let word_count = words.len() as i64;
    let syllable_count: i64 = words.iter().map(|w| syllables(w) as i64).sum();
    // 0.39 words per sentence + 11.8 syllables per word - 15.59, times ten.
    let grade = (39 * word_count) / (10 * sentences) + (118 * syllable_count) / word_count - 156;
    grade.max(0) as u32
}

/// The longest sentence of a text, split as [`reading_grade_x10`] splits
/// sentences: its number of words and the sentence. `None` for a text
/// with no words.
pub fn longest_sentence(text: &str) -> Option<(usize, &str)> {
    text.split(['.', '!', '?', '\n'])
        .map(|s| (s.split(|c: char| !c.is_alphabetic() && c != '\'').filter(|w| !w.is_empty()).count(), s.trim()))
        .filter(|(words, _)| *words > 0)
        .max_by_key(|(words, _)| *words)
}
