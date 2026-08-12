//! A small JSON reader and writer.
//!
//! Used for the language-server wire protocol (SPEC §78) and the registry API
//! (SPEC §89). Object keys are ordered so output is deterministic.

use std::collections::BTreeMap;
use std::fmt;

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}

impl Json {
    pub fn object() -> Json {
        Json::Object(BTreeMap::new())
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Json::Number(n) => Some(*n as i64),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&BTreeMap<String, Json>> {
        match self {
            Json::Object(o) => Some(o),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }

    /// Look up a slash-separated path, e.g. `params/textDocument/uri`.
    pub fn get(&self, path: &str) -> Option<&Json> {
        let mut current = self;
        for segment in path.split('/').filter(|s| !s.is_empty()) {
            current = match current {
                Json::Object(o) => o.get(segment)?,
                Json::Array(a) => a.get(segment.parse::<usize>().ok()?)?,
                _ => return None,
            };
        }
        Some(current)
    }

    pub fn get_str(&self, path: &str) -> Option<&str> {
        self.get(path)?.as_str()
    }

    pub fn get_i64(&self, path: &str) -> Option<i64> {
        self.get(path)?.as_i64()
    }

    /// Insert into an object, creating it if this value is not one.
    pub fn insert(&mut self, key: impl Into<String>, value: Json) -> &mut Self {
        if !matches!(self, Json::Object(_)) {
            *self = Json::object();
        }
        if let Json::Object(map) = self {
            map.insert(key.into(), value);
        }
        self
    }
}

impl From<&str> for Json {
    fn from(s: &str) -> Json {
        Json::String(s.to_string())
    }
}

impl From<String> for Json {
    fn from(s: String) -> Json {
        Json::String(s)
    }
}

impl From<bool> for Json {
    fn from(b: bool) -> Json {
        Json::Bool(b)
    }
}

impl From<i64> for Json {
    fn from(n: i64) -> Json {
        Json::Number(n as f64)
    }
}

impl From<usize> for Json {
    fn from(n: usize) -> Json {
        Json::Number(n as f64)
    }
}

impl From<f64> for Json {
    fn from(n: f64) -> Json {
        Json::Number(n)
    }
}

impl<T: Into<Json>> From<Vec<T>> for Json {
    fn from(items: Vec<T>) -> Json {
        Json::Array(items.into_iter().map(Into::into).collect())
    }
}

impl<T: Into<Json>> From<Option<T>> for Json {
    fn from(value: Option<T>) -> Json {
        match value {
            Some(v) => v.into(),
            None => Json::Null,
        }
    }
}

/// Build an object from key/value pairs.
#[macro_export]
macro_rules! json_object {
    ($($key:expr => $value:expr),* $(,)?) => {{
        let mut map = std::collections::BTreeMap::new();
        $( map.insert($key.to_string(), $crate::Json::from($value)); )*
        $crate::Json::Object(map)
    }};
}

#[derive(Clone, Debug, PartialEq)]
pub struct Error {
    pub message: String,
    pub offset: usize,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "at byte {}: {}", self.offset, self.message)
    }
}

impl std::error::Error for Error {}

type Result<T> = std::result::Result<T, Error>;

pub fn parse(src: &str) -> Result<Json> {
    let mut p = JsonParser { src: src.as_bytes(), pos: 0, depth: 0 };
    p.skip_ws();
    let value = p.parse_value()?;
    p.skip_ws();
    if p.pos != p.src.len() {
        return p.err("trailing data after JSON value");
    }
    Ok(value)
}

/// Nesting limit, so that hostile input cannot overflow the stack.
const MAX_DEPTH: usize = 128;

struct JsonParser<'a> {
    src: &'a [u8],
    pos: usize,
    depth: usize,
}

impl<'a> JsonParser<'a> {
    fn err<T>(&self, message: impl Into<String>) -> Result<T> {
        Err(Error { message: message.into(), offset: self.pos })
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, c: u8) -> Result<()> {
        if self.peek() == Some(c) {
            self.pos += 1;
            Ok(())
        } else {
            self.err(format!("expected `{}`", c as char))
        }
    }

    fn parse_value(&mut self) -> Result<Json> {
        if self.depth > MAX_DEPTH {
            return self.err("JSON nested too deeply");
        }
        match self.peek() {
            Some(b'n') => self.parse_literal("null", Json::Null),
            Some(b't') => self.parse_literal("true", Json::Bool(true)),
            Some(b'f') => self.parse_literal("false", Json::Bool(false)),
            Some(b'"') => Ok(Json::String(self.parse_string()?)),
            Some(b'[') => self.parse_array(),
            Some(b'{') => self.parse_object(),
            Some(_) => self.parse_number(),
            None => self.err("unexpected end of input"),
        }
    }

    fn parse_literal(&mut self, text: &str, value: Json) -> Result<Json> {
        if self.src[self.pos..].starts_with(text.as_bytes()) {
            self.pos += text.len();
            Ok(value)
        } else {
            self.err(format!("expected `{text}`"))
        }
    }

    fn parse_number(&mut self) -> Result<Json> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E' | b'+' | b'-'))
        {
            self.pos += 1;
        }
        let text = std::str::from_utf8(&self.src[start..self.pos]).unwrap_or("");
        match text.parse::<f64>() {
            Ok(n) => Ok(Json::Number(n)),
            Err(_) => {
                self.pos = start;
                self.err("invalid number")
            }
        }
    }

    fn parse_string(&mut self) -> Result<String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let c = match self.peek() {
                Some(c) => c,
                None => return self.err("unterminated string"),
            };
            self.pos += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let esc = match self.peek() {
                        Some(e) => e,
                        None => return self.err("unterminated escape"),
                    };
                    self.pos += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'n' => out.push('\n'),
                        b't' => out.push('\t'),
                        b'r' => out.push('\r'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'u' => out.push(self.parse_unicode_escape()?),
                        other => {
                            return self.err(format!("invalid escape `\\{}`", other as char))
                        }
                    }
                }
                _ => {
                    // Copy the character, however many bytes it takes.
                    let start = self.pos - 1;
                    let extra = if c < 0x80 {
                        0
                    } else if c >= 0xF0 {
                        3
                    } else if c >= 0xE0 {
                        2
                    } else {
                        1
                    };
                    let end = (start + 1 + extra).min(self.src.len());
                    out.push_str(&String::from_utf8_lossy(&self.src[start..end]));
                    self.pos = end;
                }
            }
        }
        Ok(out)
    }

    fn parse_unicode_escape(&mut self) -> Result<char> {
        let hi = self.parse_hex4()?;
        // A surrogate pair is encoded as two `\u` escapes.
        if (0xD800..0xDC00).contains(&hi) {
            if self.peek() == Some(b'\\') && self.src.get(self.pos + 1) == Some(&b'u') {
                self.pos += 2;
                let lo = self.parse_hex4()?;
                if (0xDC00..0xE000).contains(&lo) {
                    let combined = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                    return char::from_u32(combined)
                        .ok_or_else(|| Error {
                            message: "invalid surrogate pair".into(),
                            offset: self.pos,
                        });
                }
            }
            return Ok('\u{FFFD}');
        }
        char::from_u32(hi).ok_or_else(|| Error {
            message: "invalid unicode escape".into(),
            offset: self.pos,
        })
    }

    fn parse_hex4(&mut self) -> Result<u32> {
        if self.pos + 4 > self.src.len() {
            return self.err("truncated unicode escape");
        }
        let hex = String::from_utf8_lossy(&self.src[self.pos..self.pos + 4]).into_owned();
        self.pos += 4;
        u32::from_str_radix(&hex, 16).map_err(|_| Error {
            message: format!("invalid unicode escape `\\u{hex}`"),
            offset: self.pos,
        })
    }

    fn parse_array(&mut self) -> Result<Json> {
        self.expect(b'[')?;
        self.depth += 1;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(Json::Array(items));
        }
        loop {
            self.skip_ws();
            items.push(self.parse_value()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                _ => return self.err("expected `,` or `]`"),
            }
        }
        self.depth -= 1;
        Ok(Json::Array(items))
    }

    fn parse_object(&mut self) -> Result<Json> {
        self.expect(b'{')?;
        self.depth += 1;
        let mut map = BTreeMap::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(Json::Object(map));
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            self.expect(b':')?;
            self.skip_ws();
            let value = self.parse_value()?;
            map.insert(key, value);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                _ => return self.err("expected `,` or `}`"),
            }
        }
        self.depth -= 1;
        Ok(Json::Object(map))
    }
}

/// Render compactly, as required for JSON-RPC framing.
pub fn to_string(value: &Json) -> String {
    let mut out = String::new();
    write_json(&mut out, value);
    out
}

/// Render with two-space indentation, for files people read.
pub fn to_string_pretty(value: &Json) -> String {
    let mut out = String::new();
    write_pretty(&mut out, value, 0);
    out.push('\n');
    out
}

fn write_json(out: &mut String, value: &Json) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Number(n) => out.push_str(&format_number(*n)),
        Json::String(s) => write_string(out, s),
        Json::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(out, item);
            }
            out.push(']');
        }
        Json::Object(map) => {
            out.push('{');
            for (i, (k, v)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(out, k);
                out.push(':');
                write_json(out, v);
            }
            out.push('}');
        }
    }
}

fn write_pretty(out: &mut String, value: &Json, indent: usize) {
    let pad = "  ".repeat(indent);
    let inner_pad = "  ".repeat(indent + 1);
    match value {
        Json::Array(items) if !items.is_empty() => {
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                out.push_str(&inner_pad);
                write_pretty(out, item, indent + 1);
                if i + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&pad);
            out.push(']');
        }
        Json::Object(map) if !map.is_empty() => {
            out.push_str("{\n");
            for (i, (k, v)) in map.iter().enumerate() {
                out.push_str(&inner_pad);
                write_string(out, k);
                out.push_str(": ");
                write_pretty(out, v, indent + 1);
                if i + 1 < map.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&pad);
            out.push('}');
        }
        other => write_json(out, other),
    }
}

/// Integers print without a decimal point; other numbers use the shortest
/// representation that round-trips.
fn format_number(n: f64) -> String {
    if !n.is_finite() {
        return "null".to_string();
    }
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scalars() {
        assert_eq!(parse("null").unwrap(), Json::Null);
        assert_eq!(parse("true").unwrap(), Json::Bool(true));
        assert_eq!(parse("42").unwrap(), Json::Number(42.0));
        assert_eq!(parse("-1.5e2").unwrap(), Json::Number(-150.0));
        assert_eq!(parse(r#""hi""#).unwrap(), Json::String("hi".into()));
    }

    #[test]
    fn parses_nested_structures() {
        let v = parse(r#"{"a": [1, 2, {"b": null}], "c": true}"#).unwrap();
        assert_eq!(v.get("a/2/b"), Some(&Json::Null));
        assert_eq!(v.get("c").unwrap().as_bool(), Some(true));
    }

    #[test]
    fn parses_lsp_style_requests() {
        // SPEC §78
        let v = parse(
            r#"{"jsonrpc":"2.0","id":1,"method":"textDocument/hover",
                "params":{"textDocument":{"uri":"file:///main.l"},
                "position":{"line":3,"character":7}}}"#,
        )
        .unwrap();
        assert_eq!(v.get_str("method"), Some("textDocument/hover"));
        assert_eq!(v.get_str("params/textDocument/uri"), Some("file:///main.l"));
        assert_eq!(v.get_i64("params/position/line"), Some(3));
    }

    #[test]
    fn parses_escapes_and_unicode() {
        assert_eq!(parse(r#""a\nb\t\"c\\""#).unwrap().as_str(), Some("a\nb\t\"c\\"));
        assert_eq!(parse(r#""é""#).unwrap().as_str(), Some("é"));
        // A surrogate pair.
        assert_eq!(parse(r#""🎉""#).unwrap().as_str(), Some("🎉"));
        assert_eq!(parse(r#""héllo 🎉""#).unwrap().as_str(), Some("héllo 🎉"));
    }

    #[test]
    fn rejects_malformed_input() {
        assert!(parse("{").is_err());
        assert!(parse("[1,]").is_err());
        assert!(parse(r#"{"a" 1}"#).is_err());
        assert!(parse("1 2").is_err());
        assert!(parse("").is_err());
    }

    #[test]
    fn rejects_deeply_nested_input() {
        let deep = "[".repeat(200) + &"]".repeat(200);
        assert!(parse(&deep).is_err());
    }

    #[test]
    fn round_trips() {
        let src = r#"{"a":[1,2,3],"b":{"c":"d"},"e":null,"f":true,"g":1.5}"#;
        let parsed = parse(src).unwrap();
        assert_eq!(to_string(&parsed), src);
        assert_eq!(parse(&to_string_pretty(&parsed)).unwrap(), parsed);
    }

    #[test]
    fn writes_integers_without_a_decimal_point() {
        assert_eq!(to_string(&Json::Number(42.0)), "42");
        assert_eq!(to_string(&Json::Number(1.5)), "1.5");
    }

    #[test]
    fn builds_objects_with_the_macro() {
        let v = json_object! {
            "name" => "http",
            "version" => "2.1.0",
            "downloads" => 1_284_923i64,
            "yanked" => false,
        };
        assert_eq!(v.get_str("name"), Some("http"));
        assert_eq!(v.get_i64("downloads"), Some(1_284_923));
        assert_eq!(
            to_string(&v),
            r#"{"downloads":1284923,"name":"http","version":"2.1.0","yanked":false}"#
        );
    }

    #[test]
    fn pretty_output_is_indented() {
        let v = json_object! { "a" => 1i64 };
        assert_eq!(to_string_pretty(&v), "{\n  \"a\": 1\n}\n");
    }
}
