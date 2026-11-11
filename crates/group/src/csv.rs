//! Delimited text as employers send it: quoted fields, embedded line breaks,
//! a byte order mark, Windows or Unix line endings.

/// The largest file the module will parse. A file beyond these is refused
/// whole; a scheme that large is set up as several policies.
pub const MAX_ROWS: usize = 250_000;
pub const MAX_FIELDS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CsvError {
    #[error("the file is not valid UTF-8 text")]
    NotText,
    #[error("a quoted field starting on line {0} is never closed")]
    UnclosedQuote(usize),
    #[error("the file has more than {MAX_ROWS} rows")]
    TooManyRows,
    #[error("line {0} has more than {MAX_FIELDS} fields")]
    TooManyFields(usize),
}

/// Parse delimited text into rows of fields. Rows that are entirely empty
/// are dropped. Fields are trimmed unless they were quoted.
pub fn parse(bytes: &[u8], delimiter: char) -> Result<Vec<Vec<String>>, CsvError> {
    let text = std::str::from_utf8(bytes).map_err(|_| CsvError::NotText)?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut was_quoted = false;
    let mut line = 1usize;
    let mut quote_line = 1usize;
    let mut chars = text.chars().peekable();

    fn end_field(row: &mut Vec<String>, field: &mut String, was_quoted: &mut bool) {
        let value = if *was_quoted { std::mem::take(field) } else { field.trim().to_string() };
        field.clear();
        *was_quoted = false;
        row.push(value);
    }

    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                _ => {
                    if c == '\n' {
                        line += 1;
                    }
                    field.push(c);
                }
            }
            continue;
        }
        match c {
            '"' if field.trim().is_empty() => {
                field.clear();
                quoted = true;
                was_quoted = true;
                quote_line = line;
            }
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' => {
                end_field(&mut row, &mut field, &mut was_quoted);
                if row.len() > MAX_FIELDS {
                    return Err(CsvError::TooManyFields(line));
                }
                line += 1;
                let finished = std::mem::take(&mut row);
                if finished.iter().any(|f| !f.is_empty()) {
                    if rows.len() == MAX_ROWS {
                        return Err(CsvError::TooManyRows);
                    }
                    rows.push(finished);
                }
            }
            c if c == delimiter => end_field(&mut row, &mut field, &mut was_quoted),
            _ => field.push(c),
        }
    }
    if quoted {
        return Err(CsvError::UnclosedQuote(quote_line));
    }
    end_field(&mut row, &mut field, &mut was_quoted);
    if row.len() > MAX_FIELDS {
        return Err(CsvError::TooManyFields(line));
    }
    if row.iter().any(|f| !f.is_empty()) {
        if rows.len() == MAX_ROWS {
            return Err(CsvError::TooManyRows);
        }
        rows.push(row);
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(text: &str) -> Vec<Vec<String>> {
        parse(text.as_bytes(), ',').unwrap()
    }

    /// Purpose: quoted fields keep commas, doubled quotes and line breaks; unquoted fields are
    /// trimmed; a byte order mark, Windows line endings, blank lines and a missing final newline
    /// are all tolerated.
    /// Why it matters: membership files come out of payroll systems and spreadsheets in every
    /// dialect. A parser that splits "Ngata, Aroha" in two shifts every later column and corrupts
    /// the member's salary and category silently.
    #[test]
    fn parses_what_spreadsheets_and_payroll_systems_write() {
        let text = "\u{feff}id, name ,note\r\n1,\"Ngata, Aroha\",\"said \"\"hi\"\"\"\r\n\r\n2,\"two\nlines\",  \n ,,\n3,x,y";
        assert_eq!(
            rows(text),
            vec![
                vec!["id", "name", "note"],
                vec!["1", "Ngata, Aroha", "said \"hi\""],
                vec!["2", "two\nlines", ""],
                vec!["3", "x", "y"],
            ]
        );
        assert_eq!(parse(b"a;b\n1;2\n", ';').unwrap(), vec![vec!["a", "b"], vec!["1", "2"]]);
        assert_eq!(rows(""), Vec::<Vec<String>>::new());
        // A quote in the middle of an unquoted field is just a character.
        assert_eq!(rows("5' 10\" tall,x"), vec![vec!["5' 10\" tall", "x"]]);
        // A quoted field keeps its spaces.
        assert_eq!(rows("\" a \",b"), vec![vec![" a ", "b"]]);
    }

    /// Purpose: a file that is not text, has a quote that never closes, or is beyond the size
    /// limits is refused with an error that says which.
    /// Why it matters: a truncated upload often ends inside a quoted field. Refusing it whole is the
    /// only safe reading; applying the rows before the break would look like a mass of leavers.
    #[test]
    fn refuses_files_it_cannot_read_safely() {
        assert_eq!(parse(&[0xff, 0xfe, 0x00], ','), Err(CsvError::NotText));
        assert_eq!(parse(b"a,b\n1,\"open\n2,3\n", ','), Err(CsvError::UnclosedQuote(2)));
        let wide = vec!["x"; MAX_FIELDS + 1].join(",");
        assert_eq!(parse(format!("a\n{wide}\n").as_bytes(), ','), Err(CsvError::TooManyFields(2)));
        assert_eq!(parse(wide.as_bytes(), ','), Err(CsvError::TooManyFields(1)));
        let long = "x\n".repeat(MAX_ROWS + 1);
        assert_eq!(parse(long.as_bytes(), ','), Err(CsvError::TooManyRows));
        let long_no_newline = format!("{}x", "x\n".repeat(MAX_ROWS));
        assert_eq!(parse(long_no_newline.as_bytes(), ','), Err(CsvError::TooManyRows));
        assert_eq!(CsvError::UnclosedQuote(2).to_string(), "a quoted field starting on line 2 is never closed");
    }
}
