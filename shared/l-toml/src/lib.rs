//! A small TOML reader and writer.
//!
//! Enough of TOML v1.0.0 to read and write `l.toml` and `l.lock`: tables,
//! arrays of tables, inline tables, arrays, strings, integers, floats,
//! booleans and comments. Datetimes are read as strings, since the manifest
//! format has no use for them.
//!
//! The toolchain has no third-party dependencies, so this lives here rather
//! than coming from a crate.

use std::collections::BTreeMap;
use std::fmt;

/// A TOML value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    String(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    Array(Vec<Value>),
    /// Keys are ordered so that serialisation is deterministic, which matters
    /// for lockfiles and reproducible builds (SPEC §63).
    Table(BTreeMap<String, Value>),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_integer(&self) -> Option<i64> {
        match self {
            Value::Integer(i) => Some(*i),
            _ => None,
        }
    }

    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            Value::Integer(i) => Some(*i as f64),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Boolean(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_table(&self) -> Option<&BTreeMap<String, Value>> {
        match self {
            Value::Table(t) => Some(t),
            _ => None,
        }
    }

    pub fn as_table_mut(&mut self) -> Option<&mut BTreeMap<String, Value>> {
        match self {
            Value::Table(t) => Some(t),
            _ => None,
        }
    }

    /// Look up a dotted key such as `package.name`.
    pub fn get(&self, path: &str) -> Option<&Value> {
        let mut current = self;
        for segment in path.split('.') {
            current = current.as_table()?.get(segment)?;
        }
        Some(current)
    }

    pub fn get_str(&self, path: &str) -> Option<&str> {
        self.get(path)?.as_str()
    }

    /// The name of this value's type, for error messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::String(_) => "string",
            Value::Integer(_) => "integer",
            Value::Float(_) => "float",
            Value::Boolean(_) => "boolean",
            Value::Array(_) => "array",
            Value::Table(_) => "table",
        }
    }

    /// An empty table.
    pub fn table() -> Value {
        Value::Table(BTreeMap::new())
    }
}

/// A parse failure, with the line it occurred on.
#[derive(Clone, Debug, PartialEq)]
pub struct Error {
    pub message: String,
    pub line: usize,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for Error {}

type Result<T> = std::result::Result<T, Error>;

/// Parse a TOML document into a table.
pub fn parse(src: &str) -> Result<Value> {
    Parser::new(src).parse()
}

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
    line: usize,
    root: BTreeMap<String, Value>,
    /// The table new key/value pairs are inserted into, as a key path.
    current: Vec<String>,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Parser {
            src: src.as_bytes(),
            pos: 0,
            line: 1,
            root: BTreeMap::new(),
            current: Vec::new(),
        }
    }

    fn err<T>(&self, message: impl Into<String>) -> Result<T> {
        Err(Error { message: message.into(), line: self.line })
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.pos += 1;
        if c == b'\n' {
            self.line += 1;
        }
        Some(c)
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.bump();
            true
        } else {
            false
        }
    }

    /// Skip spaces and tabs, but not newlines.
    fn skip_spaces(&mut self) {
        while matches!(self.peek(), Some(b' ') | Some(b'\t')) {
            self.pos += 1;
        }
    }

    /// Skip whitespace, newlines and comments.
    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(b' ') | Some(b'\t') | Some(b'\r') => {
                    self.pos += 1;
                }
                Some(b'\n') => {
                    self.bump();
                }
                Some(b'#') => {
                    while let Some(c) = self.peek() {
                        if c == b'\n' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                _ => return,
            }
        }
    }

    /// Consume to the end of the current line, allowing a trailing comment.
    fn finish_line(&mut self) -> Result<()> {
        self.skip_spaces();
        if self.peek() == Some(b'#') {
            while let Some(c) = self.peek() {
                if c == b'\n' {
                    break;
                }
                self.pos += 1;
            }
        }
        match self.peek() {
            None => Ok(()),
            Some(b'\n') => {
                self.bump();
                Ok(())
            }
            Some(b'\r') => {
                self.pos += 1;
                self.eat(b'\n');
                Ok(())
            }
            Some(c) => self.err(format!("unexpected `{}` after value", c as char)),
        }
    }

    fn parse(mut self) -> Result<Value> {
        loop {
            self.skip_trivia();
            if self.peek().is_none() {
                break;
            }

            if self.peek() == Some(b'[') {
                self.parse_table_header()?;
                continue;
            }

            let key = self.parse_key_path()?;
            self.skip_spaces();
            if !self.eat(b'=') {
                return self.err("expected `=` after key");
            }
            self.skip_spaces();
            let value = self.parse_value()?;
            self.insert(&key, value)?;
            self.finish_line()?;
        }
        Ok(Value::Table(self.root))
    }

    /// `[table]` or `[[array of tables]]`.
    fn parse_table_header(&mut self) -> Result<()> {
        self.bump(); // `[`
        let array = self.eat(b'[');

        self.skip_spaces();
        let path = self.parse_key_path()?;
        self.skip_spaces();

        if !self.eat(b']') {
            return self.err("expected `]` to close table header");
        }
        if array && !self.eat(b']') {
            return self.err("expected `]]` to close array-of-tables header");
        }
        self.finish_line()?;

        if path.is_empty() {
            return self.err("table header cannot be empty");
        }

        if array {
            self.push_array_table(&path)?;
        } else {
            self.ensure_table(&path)?;
        }
        self.current = path;
        Ok(())
    }

    /// Walk to `path`, creating tables as needed.
    fn ensure_table(&mut self, path: &[String]) -> Result<()> {
        // The line is captured up front: `self.root` is borrowed mutably below,
        // so `self.err` cannot be called while walking.
        let line = self.line;
        let mut table = &mut self.root;
        for (i, key) in path.iter().enumerate() {
            let entry = table.entry(key.clone()).or_insert_with(Value::table);
            table = match entry {
                Value::Table(t) => t,
                // Descend into the last element of an array of tables.
                Value::Array(items) => match items.last_mut() {
                    Some(Value::Table(t)) => t,
                    _ => {
                        return Err(Error {
                            message: format!(
                                "`{}` is an array of values, not a table",
                                path[..=i].join(".")
                            ),
                            line,
                        })
                    }
                },
                other => {
                    return Err(Error {
                        message: format!(
                            "`{}` is already defined as {}",
                            path[..=i].join("."),
                            other.type_name()
                        ),
                        line,
                    })
                }
            };
        }
        Ok(())
    }

    /// Append a new table to the array of tables at `path`.
    fn push_array_table(&mut self, path: &[String]) -> Result<()> {
        let (last, parents) = path.split_last().expect("header is non-empty");
        if !parents.is_empty() {
            self.ensure_table(parents)?;
        }

        let line = self.line;
        let mut table = &mut self.root;
        for key in parents {
            let entry = table.get_mut(key).expect("ensure_table created it");
            table = match entry {
                Value::Table(t) => t,
                Value::Array(items) => match items.last_mut() {
                    Some(Value::Table(t)) => t,
                    _ => {
                        return Err(Error {
                            message: format!("`{key}` is not a table"),
                            line,
                        })
                    }
                },
                other => {
                    return Err(Error {
                        message: format!("`{key}` is {}, not a table", other.type_name()),
                        line,
                    })
                }
            };
        }

        let entry = table.entry(last.clone()).or_insert_with(|| Value::Array(Vec::new()));
        match entry {
            Value::Array(items) => {
                items.push(Value::table());
                Ok(())
            }
            other => Err(Error {
                message: format!("`{last}` is {}, not an array of tables", other.type_name()),
                line,
            }),
        }
    }

    /// Insert `value` at `key` inside the current table.
    fn insert(&mut self, key: &[String], value: Value) -> Result<()> {
        let mut full = self.current.clone();
        full.extend_from_slice(key);
        let (last, parents) = full.split_last().expect("key is non-empty");

        let mut table = &mut self.root;
        for (i, part) in parents.iter().enumerate() {
            let entry = table.entry(part.clone()).or_insert_with(Value::table);
            table = match entry {
                Value::Table(t) => t,
                Value::Array(items) => match items.last_mut() {
                    Some(Value::Table(t)) => t,
                    _ => {
                        return Err(Error {
                            message: format!("`{}` is not a table", parents[..=i].join(".")),
                            line: self.line,
                        })
                    }
                },
                other => {
                    return Err(Error {
                        message: format!(
                            "`{}` is {}, not a table",
                            parents[..=i].join("."),
                            other.type_name()
                        ),
                        line: self.line,
                    })
                }
            };
        }

        if table.contains_key(last) {
            return Err(Error {
                message: format!("duplicate key `{}`", full.join(".")),
                line: self.line,
            });
        }
        table.insert(last.clone(), value);
        Ok(())
    }

    /// A dotted key such as `a.b.c`, with bare or quoted segments.
    fn parse_key_path(&mut self) -> Result<Vec<String>> {
        let mut parts = Vec::new();
        loop {
            self.skip_spaces();
            let part = match self.peek() {
                Some(b'"') => self.parse_basic_string()?,
                Some(b'\'') => self.parse_literal_string()?,
                Some(c) if is_bare_key_char(c) => {
                    let start = self.pos;
                    while self.peek().is_some_and(is_bare_key_char) {
                        self.pos += 1;
                    }
                    String::from_utf8_lossy(&self.src[start..self.pos]).into_owned()
                }
                Some(c) => return self.err(format!("unexpected `{}` in key", c as char)),
                None => return self.err("unexpected end of input in key"),
            };
            parts.push(part);
            self.skip_spaces();
            if !self.eat(b'.') {
                break;
            }
        }
        Ok(parts)
    }

    fn parse_value(&mut self) -> Result<Value> {
        match self.peek() {
            Some(b'"') => Ok(Value::String(self.parse_basic_string()?)),
            Some(b'\'') => Ok(Value::String(self.parse_literal_string()?)),
            Some(b'[') => self.parse_array(),
            Some(b'{') => self.parse_inline_table(),
            Some(b't') | Some(b'f') => self.parse_bool(),
            Some(_) => self.parse_number(),
            None => self.err("expected a value"),
        }
    }

    fn parse_bool(&mut self) -> Result<Value> {
        if self.src[self.pos..].starts_with(b"true") {
            self.pos += 4;
            Ok(Value::Boolean(true))
        } else if self.src[self.pos..].starts_with(b"false") {
            self.pos += 5;
            Ok(Value::Boolean(false))
        } else {
            self.err("expected `true` or `false`")
        }
    }

    fn parse_number(&mut self) -> Result<Value> {
        let start = self.pos;
        if matches!(self.peek(), Some(b'+') | Some(b'-')) {
            self.pos += 1;
        }

        // Hex, octal and binary integers.
        if self.peek() == Some(b'0') && self.pos + 1 < self.src.len() {
            let radix = match self.src[self.pos + 1] {
                b'x' => Some(16),
                b'o' => Some(8),
                b'b' => Some(2),
                _ => None,
            };
            if let Some(radix) = radix {
                self.pos += 2;
                let digits_start = self.pos;
                while self.peek().is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_') {
                    self.pos += 1;
                }
                let text: String = String::from_utf8_lossy(&self.src[digits_start..self.pos])
                    .replace('_', "");
                return match i64::from_str_radix(&text, radix) {
                    Ok(v) => Ok(Value::Integer(v)),
                    Err(_) => self.err(format!("invalid integer `{text}`")),
                };
            }
        }

        let mut is_float = false;
        while let Some(c) = self.peek() {
            match c {
                b'0'..=b'9' | b'_' => self.pos += 1,
                b'.' | b'e' | b'E' => {
                    is_float = true;
                    self.pos += 1;
                }
                b'+' | b'-' if matches!(self.src.get(self.pos - 1), Some(b'e') | Some(b'E')) => {
                    self.pos += 1;
                }
                // A date or time reads as a string; the manifest never uses one.
                b':' | b'T' | b'Z' | b'-' => {
                    while self
                        .peek()
                        .is_some_and(|c| !matches!(c, b'\n' | b'\r' | b',' | b']' | b'}' | b'#'))
                    {
                        self.pos += 1;
                    }
                    let text = String::from_utf8_lossy(&self.src[start..self.pos]).trim().to_string();
                    return Ok(Value::String(text));
                }
                _ => break,
            }
        }

        let text: String = String::from_utf8_lossy(&self.src[start..self.pos]).replace('_', "");
        if text.is_empty() {
            return self.err("expected a value");
        }
        if is_float {
            match text.parse::<f64>() {
                Ok(v) => Ok(Value::Float(v)),
                Err(_) => self.err(format!("invalid float `{text}`")),
            }
        } else {
            match text.parse::<i64>() {
                Ok(v) => Ok(Value::Integer(v)),
                Err(_) => self.err(format!("invalid integer `{text}`")),
            }
        }
    }

    fn parse_array(&mut self) -> Result<Value> {
        self.bump(); // `[`
        let mut items = Vec::new();
        loop {
            self.skip_trivia();
            if self.peek() == Some(b']') {
                self.bump();
                break;
            }
            if self.peek().is_none() {
                return self.err("unterminated array");
            }
            items.push(self.parse_value()?);
            self.skip_trivia();
            if self.eat(b',') {
                continue;
            }
            self.skip_trivia();
            if self.eat(b']') {
                break;
            }
            return self.err("expected `,` or `]` in array");
        }
        Ok(Value::Array(items))
    }

    fn parse_inline_table(&mut self) -> Result<Value> {
        self.bump(); // `{`
        let mut table = BTreeMap::new();
        loop {
            // TOML 1.0 forbids newlines in inline tables, but the manifest
            // examples in SPEC §41 span several lines, so they are allowed.
            self.skip_trivia();
            if self.eat(b'}') {
                break;
            }
            if self.peek().is_none() {
                return self.err("unterminated inline table");
            }

            let key = self.parse_key_path()?;
            self.skip_spaces();
            if !self.eat(b'=') {
                return self.err("expected `=` in inline table");
            }
            self.skip_trivia();
            let value = self.parse_value()?;

            // Insert, creating intermediate tables for a dotted key.
            let (last, parents) = key.split_last().expect("key is non-empty");
            let mut target = &mut table;
            for part in parents {
                let entry = target.entry(part.clone()).or_insert_with(Value::table);
                target = match entry {
                    Value::Table(t) => t,
                    _ => return self.err(format!("`{part}` is not a table")),
                };
            }
            target.insert(last.clone(), value);

            self.skip_trivia();
            if self.eat(b',') {
                continue;
            }
            self.skip_trivia();
            if self.eat(b'}') {
                break;
            }
            return self.err("expected `,` or `}` in inline table");
        }
        Ok(Value::Table(table))
    }

    fn parse_basic_string(&mut self) -> Result<String> {
        self.bump(); // opening quote

        // `"""` — a multi-line string.
        if self.peek() == Some(b'"') && self.src.get(self.pos + 1) == Some(&b'"') {
            self.pos += 2;
            // A newline immediately after the opening delimiter is trimmed.
            if self.peek() == Some(b'\n') {
                self.bump();
            }
            let mut out = String::new();
            loop {
                if self.pos + 2 < self.src.len() + 1 && self.src[self.pos..].starts_with(b"\"\"\"") {
                    self.pos += 3;
                    break;
                }
                match self.bump() {
                    None => return self.err("unterminated multi-line string"),
                    Some(b'\\') => match self.parse_escape()? {
                        Some(c) => out.push(c),
                        None => {}
                    },
                    Some(c) => push_byte(&mut out, c, self.src, &mut self.pos),
                }
            }
            return Ok(out);
        }

        let mut out = String::new();
        loop {
            match self.bump() {
                None | Some(b'\n') => return self.err("unterminated string"),
                Some(b'"') => break,
                Some(b'\\') => {
                    if let Some(c) = self.parse_escape()? {
                        out.push(c);
                    }
                }
                Some(c) => push_byte(&mut out, c, self.src, &mut self.pos),
            }
        }
        Ok(out)
    }

    /// Returns `None` for a line-continuation escape, which contributes nothing.
    fn parse_escape(&mut self) -> Result<Option<char>> {
        let c = match self.bump() {
            Some(c) => c,
            None => return self.err("unterminated escape sequence"),
        };
        Ok(Some(match c {
            b'n' => '\n',
            b't' => '\t',
            b'r' => '\r',
            b'"' => '"',
            b'\\' => '\\',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'u' | b'U' => {
                let len = if c == b'u' { 4 } else { 8 };
                if self.pos + len > self.src.len() {
                    return self.err("truncated unicode escape");
                }
                let hex = String::from_utf8_lossy(&self.src[self.pos..self.pos + len]).into_owned();
                self.pos += len;
                match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    Some(ch) => ch,
                    None => return self.err(format!("invalid unicode escape `\\u{hex}`")),
                }
            }
            // A backslash before a newline swallows the following whitespace.
            b'\n' | b' ' | b'\t' | b'\r' => {
                while matches!(self.peek(), Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r')) {
                    self.bump();
                }
                return Ok(None);
            }
            other => return self.err(format!("invalid escape `\\{}`", other as char)),
        }))
    }

    fn parse_literal_string(&mut self) -> Result<String> {
        self.bump(); // `'`

        // `'''` — a multi-line literal string.
        if self.peek() == Some(b'\'') && self.src.get(self.pos + 1) == Some(&b'\'') {
            self.pos += 2;
            if self.peek() == Some(b'\n') {
                self.bump();
            }
            let start = self.pos;
            loop {
                if self.src[self.pos..].starts_with(b"'''") {
                    let text = String::from_utf8_lossy(&self.src[start..self.pos]).into_owned();
                    self.pos += 3;
                    return Ok(text);
                }
                if self.bump().is_none() {
                    return self.err("unterminated multi-line literal string");
                }
            }
        }

        let start = self.pos;
        loop {
            match self.bump() {
                None | Some(b'\n') => return self.err("unterminated literal string"),
                Some(b'\'') => break,
                _ => {}
            }
        }
        Ok(String::from_utf8_lossy(&self.src[start..self.pos - 1]).into_owned())
    }
}

/// Append one UTF-8 character starting at the byte just consumed.
fn push_byte(out: &mut String, first: u8, src: &[u8], pos: &mut usize) {
    if first < 0x80 {
        out.push(first as char);
        return;
    }
    // Multi-byte: figure out how many continuation bytes follow.
    let extra = if first >= 0xF0 {
        3
    } else if first >= 0xE0 {
        2
    } else {
        1
    };
    let start = *pos - 1;
    let end = (start + 1 + extra).min(src.len());
    out.push_str(&String::from_utf8_lossy(&src[start..end]));
    *pos = end;
}

fn is_bare_key_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'-'
}

// ===========================================================================
// Serialisation
// ===========================================================================

/// Render a table as a TOML document.
///
/// Output is deterministic: keys are sorted, scalars come before sub-tables,
/// and formatting is fixed. Lockfiles therefore do not churn (SPEC §63).
pub fn to_string(value: &Value) -> String {
    let mut out = String::new();
    let table = match value.as_table() {
        Some(t) => t,
        None => return String::new(),
    };
    write_table(&mut out, table, &[]);
    out
}

fn write_table(out: &mut String, table: &BTreeMap<String, Value>, path: &[String]) {
    // Scalars and inline values first.
    for (key, value) in table {
        if is_sub_table(value) {
            continue;
        }
        out.push_str(&format_key(key));
        out.push_str(" = ");
        out.push_str(&format_value(value));
        out.push('\n');
    }

    // Then sub-tables and arrays of tables.
    for (key, value) in table {
        match value {
            Value::Table(sub) => {
                let mut sub_path = path.to_vec();
                sub_path.push(key.clone());
                out.push('\n');
                out.push_str(&format!("[{}]\n", format_path(&sub_path)));
                write_table(out, sub, &sub_path);
            }
            Value::Array(items) if all_tables(items) => {
                let mut sub_path = path.to_vec();
                sub_path.push(key.clone());
                for item in items {
                    let Some(sub) = item.as_table() else { continue };
                    out.push('\n');
                    out.push_str(&format!("[[{}]]\n", format_path(&sub_path)));
                    write_table(out, sub, &sub_path);
                }
            }
            _ => {}
        }
    }
}

/// Whether a value is written as its own `[table]` section.
fn is_sub_table(value: &Value) -> bool {
    match value {
        Value::Table(_) => true,
        Value::Array(items) => all_tables(items),
        _ => false,
    }
}

fn all_tables(items: &[Value]) -> bool {
    !items.is_empty() && items.iter().all(|v| matches!(v, Value::Table(_)))
}

fn format_path(path: &[String]) -> String {
    path.iter().map(|s| format_key(s)).collect::<Vec<_>>().join(".")
}

fn format_key(key: &str) -> String {
    if !key.is_empty() && key.bytes().all(is_bare_key_char) {
        key.to_string()
    } else {
        format_string(key)
    }
}

fn format_value(value: &Value) -> String {
    match value {
        Value::String(s) => format_string(s),
        Value::Integer(i) => i.to_string(),
        Value::Float(f) => {
            if f.fract() == 0.0 && f.is_finite() {
                format!("{f:.1}")
            } else {
                f.to_string()
            }
        }
        Value::Boolean(b) => b.to_string(),
        Value::Array(items) => {
            let parts: Vec<_> = items.iter().map(format_value).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Table(table) => {
            let parts: Vec<_> = table
                .iter()
                .map(|(k, v)| format!("{} = {}", format_key(k), format_value(v)))
                .collect();
            format!("{{ {} }}", parts.join(", "))
        }
    }
}

fn format_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_package_manifest() {
        // SPEC §37
        let doc = parse(
            r#"
[package]
name = "myapp"
version = "1.0.0"
description = "My L application"
license = "MIT"
authors = ["Sasha"]

[dependencies]
http = "^1.2.0"
json = "^2.0.0"

[dev-dependencies]
test = "^1.0.0"
"#,
        )
        .expect("valid manifest");

        assert_eq!(doc.get_str("package.name"), Some("myapp"));
        assert_eq!(doc.get_str("package.version"), Some("1.0.0"));
        assert_eq!(
            doc.get("package.authors").unwrap().as_array().unwrap()[0].as_str(),
            Some("Sasha")
        );
        assert_eq!(doc.get_str("dependencies.http"), Some("^1.2.0"));
        assert_eq!(doc.get_str("dev-dependencies.test"), Some("^1.0.0"));
    }

    #[test]
    fn parses_arrays_of_tables() {
        // SPEC §40 — the lockfile format
        let doc = parse(
            r#"
[[package]]
name = "http"
version = "1.2.3"
source = "registry"
checksum = "abc"

[[package]]
name = "json"
version = "2.1.0"
source = "registry"
checksum = "def"
"#,
        )
        .expect("valid lockfile");

        let packages = doc.get("package").unwrap().as_array().unwrap();
        assert_eq!(packages.len(), 2);
        assert_eq!(packages[0].get_str("name"), Some("http"));
        assert_eq!(packages[1].get_str("version"), Some("2.1.0"));
    }

    #[test]
    fn parses_multi_line_inline_tables() {
        // SPEC §41 writes git dependencies across several lines.
        let doc = parse(
            r#"
[dependencies]
http = {
    git = "https://github.com/example/http",
    branch = "development"
}
"#,
        )
        .expect("valid dependency table");

        assert_eq!(
            doc.get_str("dependencies.http.git"),
            Some("https://github.com/example/http")
        );
        assert_eq!(doc.get_str("dependencies.http.branch"), Some("development"));
    }

    #[test]
    fn parses_workspace_members() {
        // SPEC §64
        let doc = parse(
            r#"
[workspace]
members = [
    "packages/server",
    "packages/client",
    "packages/common"
]
"#,
        )
        .expect("valid workspace");
        let members = doc.get("workspace.members").unwrap().as_array().unwrap();
        assert_eq!(members.len(), 3);
        assert_eq!(members[2].as_str(), Some("packages/common"));
    }

    #[test]
    fn parses_scalar_types() {
        let doc = parse(
            "i = 42\nneg = -7\nf = 2.5\nexp = 1e3\nb = true\nhex = 0xFF\nsep = 1_000\n",
        )
        .unwrap();
        assert_eq!(doc.get("i").unwrap().as_integer(), Some(42));
        assert_eq!(doc.get("neg").unwrap().as_integer(), Some(-7));
        assert_eq!(doc.get("f").unwrap().as_float(), Some(2.5));
        assert_eq!(doc.get("exp").unwrap().as_float(), Some(1000.0));
        assert_eq!(doc.get("b").unwrap().as_bool(), Some(true));
        assert_eq!(doc.get("hex").unwrap().as_integer(), Some(255));
        assert_eq!(doc.get("sep").unwrap().as_integer(), Some(1000));
    }

    #[test]
    fn parses_strings_with_escapes_and_comments() {
        let doc = parse(
            "# a comment\na = \"line\\nbreak\"  # trailing\nb = 'literal\\n'\nc = \"\"\"\nmulti\nline\"\"\"\n",
        )
        .unwrap();
        assert_eq!(doc.get_str("a"), Some("line\nbreak"));
        assert_eq!(doc.get_str("b"), Some("literal\\n"));
        assert_eq!(doc.get_str("c"), Some("multi\nline"));
    }

    #[test]
    fn parses_dotted_keys() {
        let doc = parse("a.b.c = 1\n").unwrap();
        assert_eq!(doc.get("a.b.c").unwrap().as_integer(), Some(1));
    }

    #[test]
    fn parses_utf8_strings() {
        let doc = parse("a = \"héllo wörld 🎉\"\n").unwrap();
        assert_eq!(doc.get_str("a"), Some("héllo wörld 🎉"));
    }

    #[test]
    fn rejects_duplicate_keys() {
        let err = parse("a = 1\na = 2\n").unwrap_err();
        assert!(err.message.contains("duplicate key"), "{err}");
    }

    #[test]
    fn reports_the_failing_line() {
        let err = parse("a = 1\nb = 2\nc = \n").unwrap_err();
        assert_eq!(err.line, 3);
    }

    #[test]
    fn round_trips_a_manifest() {
        let src = r#"
[package]
name = "myapp"
version = "1.0.0"
authors = ["Sasha", "Alex"]

[dependencies]
http = "^2.0.0"

[[bin]]
name = "one"

[[bin]]
name = "two"
"#;
        let doc = parse(src).unwrap();
        let rendered = to_string(&doc);
        let reparsed = parse(&rendered).expect("re-parses");
        assert_eq!(doc, reparsed, "round trip changed the document:\n{rendered}");
    }

    #[test]
    fn serialisation_is_deterministic() {
        let doc = parse("z = 1\na = 2\n[t]\nq = 3\n").unwrap();
        assert_eq!(to_string(&doc), to_string(&doc));
        // Keys are sorted, so `a` precedes `z`.
        let out = to_string(&doc);
        assert!(out.find("a = 2").unwrap() < out.find("z = 1").unwrap(), "{out}");
    }

    #[test]
    fn escapes_on_write() {
        let mut table = BTreeMap::new();
        table.insert("k".to_string(), Value::String("a\"b\\c\nd".into()));
        let out = to_string(&Value::Table(table));
        assert_eq!(out.trim(), r#"k = "a\"b\\c\nd""#);
        assert_eq!(parse(&out).unwrap().get_str("k"), Some("a\"b\\c\nd"));
    }

    #[test]
    fn empty_input_is_an_empty_table() {
        let doc = parse("").unwrap();
        assert!(doc.as_table().unwrap().is_empty());
        assert_eq!(to_string(&doc), "");
    }
}
