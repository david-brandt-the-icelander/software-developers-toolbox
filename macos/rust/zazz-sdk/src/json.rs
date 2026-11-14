// A small JSON value with a strict parser, compact and pretty printers, accessors and the json! macro.
use crate::error::{Error, Result};
use std::fmt::Write as _;

const MAX_DEPTH: usize = 128;

// A JSON value; objects keep insertion order so output is deterministic.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

// Builds a Json value from JSON-like syntax; values that are more than one token go in parentheses: json!({"n": (a + 1)}).
#[macro_export]
macro_rules! json {
    (null) => { $crate::Json::Null };
    ([ $($e:tt),* $(,)? ]) => { $crate::Json::Arr(vec![ $( $crate::json!($e) ),* ]) };
    ({ $($k:literal : $v:tt),* $(,)? }) => { $crate::Json::Obj(vec![ $( ($k.to_string(), $crate::json!($v)) ),* ]) };
    ($e:expr) => { $crate::Json::from($e) };
}

impl From<bool> for Json {
    // Wraps a boolean.
    fn from(v: bool) -> Json {
        Json::Bool(v)
    }
}

impl From<i64> for Json {
    // Wraps an integer.
    fn from(v: i64) -> Json {
        Json::Int(v)
    }
}

impl From<i32> for Json {
    // Wraps an integer.
    fn from(v: i32) -> Json {
        Json::Int(v as i64)
    }
}

impl From<u32> for Json {
    // Wraps an integer.
    fn from(v: u32) -> Json {
        Json::Int(v as i64)
    }
}

impl From<usize> for Json {
    // Wraps a size, saturating at i64::MAX.
    fn from(v: usize) -> Json {
        Json::Int(i64::try_from(v).unwrap_or(i64::MAX))
    }
}

impl From<u64> for Json {
    // Wraps an integer, saturating at i64::MAX.
    fn from(v: u64) -> Json {
        Json::Int(i64::try_from(v).unwrap_or(i64::MAX))
    }
}

impl From<f64> for Json {
    // Wraps a float; non-finite values become null.
    fn from(v: f64) -> Json {
        if v.is_finite() {
            Json::Float(v)
        } else {
            Json::Null
        }
    }
}

impl From<&str> for Json {
    // Wraps a string slice.
    fn from(v: &str) -> Json {
        Json::Str(v.to_string())
    }
}

impl From<String> for Json {
    // Wraps a string.
    fn from(v: String) -> Json {
        Json::Str(v)
    }
}

impl From<&String> for Json {
    // Wraps a borrowed string.
    fn from(v: &String) -> Json {
        Json::Str(v.clone())
    }
}

impl<T: Into<Json>> From<Vec<T>> for Json {
    // Wraps a list.
    fn from(v: Vec<T>) -> Json {
        Json::Arr(v.into_iter().map(Into::into).collect())
    }
}

impl<T: Into<Json>> From<Option<T>> for Json {
    // Wraps an optional value; None becomes null.
    fn from(v: Option<T>) -> Json {
        v.map_or(Json::Null, Into::into)
    }
}

impl std::fmt::Display for Json {
    // Prints compact JSON.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = String::new();
        write_value(&mut s, self, None, 0);
        f.write_str(&s)
    }
}

impl std::str::FromStr for Json {
    type Err = Error;

    // Parses JSON text.
    fn from_str(s: &str) -> Result<Json> {
        Json::parse(s)
    }
}

impl std::ops::Index<&str> for Json {
    type Output = Json;

    // Returns a field of an object, or null.
    fn index(&self, key: &str) -> &Json {
        self.get(key).unwrap_or(&Json::Null)
    }
}

impl std::ops::Index<usize> for Json {
    type Output = Json;

    // Returns an item of an array, or null.
    fn index(&self, i: usize) -> &Json {
        self.arr().and_then(|a| a.get(i)).unwrap_or(&Json::Null)
    }
}

impl Json {
    // Parses JSON text strictly (RFC 8259), with a nesting limit.
    pub fn parse(text: &str) -> Result<Json> {
        let mut p = Parser { s: text.as_bytes(), i: 0, depth: 0 };
        let v = p.value()?;
        p.ws();
        if p.i != p.s.len() {
            return Err(p.err("trailing characters"));
        }
        Ok(v)
    }

    // Parses JSON bytes, which must be UTF-8.
    pub fn parse_bytes(bytes: &[u8]) -> Result<Json> {
        Json::parse(std::str::from_utf8(bytes).map_err(|_| Error::bad_input("JSON is not UTF-8"))?)
    }

    // Prints indented JSON.
    pub fn pretty(&self) -> String {
        let mut s = String::new();
        write_value(&mut s, self, Some(2), 0);
        s
    }

    // An empty object.
    pub fn object() -> Json {
        Json::Obj(Vec::new())
    }

    // Sets a field on an object, replacing an existing one; does nothing on other values.
    pub fn set(&mut self, key: &str, v: impl Into<Json>) {
        let Json::Obj(fields) = self else { return };
        let v = v.into();
        match fields.iter_mut().find(|(k, _)| k == key) {
            Some(slot) => slot.1 = v,
            None => fields.push((key.to_string(), v)),
        }
    }

    // Returns a field of an object.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    // Returns the string, if this is one.
    pub fn str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    // Returns the integer, if this is one (floats with no fraction count).
    pub fn i64(&self) -> Option<i64> {
        match self {
            Json::Int(n) => Some(*n),
            Json::Float(f) if f.fract() == 0.0 && f.abs() < 9.0e15 => Some(*f as i64),
            _ => None,
        }
    }

    // Returns the number as a float, if this is a number.
    pub fn f64(&self) -> Option<f64> {
        match self {
            Json::Int(n) => Some(*n as f64),
            Json::Float(f) => Some(*f),
            _ => None,
        }
    }

    // Returns the boolean, if this is one.
    pub fn bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    // Returns the items, if this is an array.
    pub fn arr(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(a) => Some(a),
            _ => None,
        }
    }

    // Returns the fields, if this is an object.
    pub fn obj(&self) -> Option<&[(String, Json)]> {
        match self {
            Json::Obj(o) => Some(o),
            _ => None,
        }
    }

    // True for null.
    pub fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }

    // Returns a string field or a default.
    pub fn str_or<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.get(key).and_then(Json::str).unwrap_or(default)
    }

    // Returns a required string field, or a bad_input error naming it.
    pub fn need_str(&self, key: &str) -> Result<&str> {
        self.get(key)
            .and_then(Json::str)
            .ok_or_else(|| Error::bad_input(&format!("'{key}' must be a string")))
    }

    // Returns a required integer field, or a bad_input error naming it.
    pub fn need_i64(&self, key: &str) -> Result<i64> {
        self.get(key)
            .and_then(Json::i64)
            .ok_or_else(|| Error::bad_input(&format!("'{key}' must be an integer")))
    }

    // Returns a list of strings from a field that holds one string or a list of them.
    pub fn strs(&self, key: &str) -> Result<Vec<String>> {
        let bad = || Error::bad_input(&format!("'{key}' must be a string or a list of strings"));
        match self.get(key) {
            Some(Json::Str(s)) => Ok(vec![s.clone()]),
            Some(Json::Arr(a)) => a.iter().map(|v| v.str().map(str::to_string).ok_or_else(bad)).collect(),
            _ => Err(bad()),
        }
    }
}

// Writes a string with JSON escapes.
fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

// Writes a newline and indentation when pretty-printing.
fn newline(out: &mut String, indent: Option<usize>, level: usize) {
    if let Some(n) = indent {
        out.push('\n');
        out.extend(std::iter::repeat_n(' ', n * level));
    }
}

// Writes a float so that it reads back as a float.
fn write_float(out: &mut String, f: f64) {
    if f.fract() == 0.0 && f.abs() < 1e16 {
        let _ = write!(out, "{f:.1}");
    } else {
        let _ = write!(out, "{f}");
    }
}

// Writes any value, compact or indented.
fn write_value(out: &mut String, v: &Json, indent: Option<usize>, level: usize) {
    match v {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Int(n) => {
            let _ = write!(out, "{n}");
        }
        Json::Float(f) => write_float(out, *f),
        Json::Str(s) => write_str(out, s),
        Json::Arr(a) if a.is_empty() => out.push_str("[]"),
        Json::Obj(o) if o.is_empty() => out.push_str("{}"),
        Json::Arr(a) => {
            out.push('[');
            for (i, item) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                newline(out, indent, level + 1);
                write_value(out, item, indent, level + 1);
            }
            newline(out, indent, level);
            out.push(']');
        }
        Json::Obj(o) => {
            out.push('{');
            for (i, (k, item)) in o.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                newline(out, indent, level + 1);
                write_str(out, k);
                out.push_str(if indent.is_some() { ": " } else { ":" });
                write_value(out, item, indent, level + 1);
            }
            newline(out, indent, level);
            out.push('}');
        }
    }
}

// A recursive-descent parser over bytes.
struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    depth: usize,
}

impl Parser<'_> {
    // Builds a parse error at the current offset.
    fn err(&self, what: &str) -> Error {
        Error::bad_input(&format!("invalid JSON at byte {}: {what}", self.i))
    }

    // Skips whitespace.
    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    // Consumes an exact literal.
    fn lit(&mut self, word: &str, v: Json) -> Result<Json> {
        if self.s[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            return Ok(v);
        }
        Err(self.err("unexpected token"))
    }

    // Parses any value.
    fn value(&mut self) -> Result<Json> {
        self.ws();
        match self.s.get(self.i) {
            None => Err(self.err("unexpected end")),
            Some(b'n') => self.lit("null", Json::Null),
            Some(b't') => self.lit("true", Json::Bool(true)),
            Some(b'f') => self.lit("false", Json::Bool(false)),
            Some(b'"') => self.string().map(Json::Str),
            Some(b'[') => self.nested(Parser::array),
            Some(b'{') => self.nested(Parser::object),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.err("unexpected character")),
        }
    }

    // Runs a container parser with the nesting limit applied.
    fn nested(&mut self, f: fn(&mut Self) -> Result<Json>) -> Result<Json> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.err("nested too deeply"));
        }
        let v = f(self);
        self.depth -= 1;
        v
    }

    // Consumes a byte if it is next.
    fn eat(&mut self, b: u8) -> bool {
        self.ws();
        let hit = self.s.get(self.i) == Some(&b);
        self.i += hit as usize;
        hit
    }

    // Parses an array after its opening bracket.
    fn array(&mut self) -> Result<Json> {
        self.i += 1;
        let mut items = Vec::new();
        if self.eat(b']') {
            return Ok(Json::Arr(items));
        }
        loop {
            items.push(self.value()?);
            if self.eat(b']') {
                return Ok(Json::Arr(items));
            }
            if !self.eat(b',') {
                return Err(self.err("expected ',' or ']'"));
            }
        }
    }

    // Parses an object after its opening brace; duplicate keys keep the last value.
    fn object(&mut self) -> Result<Json> {
        self.i += 1;
        let mut out = Json::object();
        if self.eat(b'}') {
            return Ok(out);
        }
        loop {
            self.ws();
            if self.s.get(self.i) != Some(&b'"') {
                return Err(self.err("expected a key"));
            }
            let key = self.string()?;
            if !self.eat(b':') {
                return Err(self.err("expected ':'"));
            }
            let v = self.value()?;
            out.set(&key, v);
            if self.eat(b'}') {
                return Ok(out);
            }
            if !self.eat(b',') {
                return Err(self.err("expected ',' or '}'"));
            }
        }
    }

    // Reads four hex digits.
    fn hex4(&mut self) -> Result<u32> {
        let digits = self.s.get(self.i..self.i + 4).ok_or_else(|| self.err("short \\u escape"))?;
        let text = std::str::from_utf8(digits).map_err(|_| self.err("bad \\u escape"))?;
        let n = u32::from_str_radix(text, 16).map_err(|_| self.err("bad \\u escape"))?;
        self.i += 4;
        Ok(n)
    }

    // Decodes a \u escape, joining surrogate pairs.
    fn unicode(&mut self) -> Result<char> {
        let hi = self.hex4()?;
        if !(0xd800..0xdc00).contains(&hi) {
            return char::from_u32(hi).ok_or_else(|| self.err("lone surrogate"));
        }
        if !self.s[self.i..].starts_with(b"\\u") {
            return Err(self.err("lone surrogate"));
        }
        self.i += 2;
        let lo = self.hex4()?;
        if !(0xdc00..0xe000).contains(&lo) {
            return Err(self.err("bad surrogate pair"));
        }
        char::from_u32(0x10000 + ((hi - 0xd800) << 10) + (lo - 0xdc00))
            .ok_or_else(|| self.err("bad surrogate pair"))
    }

    // Parses a string starting at its opening quote.
    fn string(&mut self) -> Result<String> {
        self.i += 1;
        let mut out = String::new();
        loop {
            let start = self.i;
            while self.i < self.s.len() && !matches!(self.s[self.i], b'"' | b'\\' | 0..=0x1f) {
                self.i += 1;
            }
            out.push_str(std::str::from_utf8(&self.s[start..self.i]).map_err(|_| self.err("bad UTF-8"))?);
            match self.s.get(self.i) {
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    let esc = *self.s.get(self.i + 1).ok_or_else(|| self.err("unterminated string"))?;
                    self.i += 2;
                    out.push(match esc {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => self.unicode()?,
                        _ => return Err(self.err("bad escape")),
                    });
                }
                Some(_) => return Err(self.err("control character in string")),
                None => return Err(self.err("unterminated string")),
            }
        }
    }

    // Parses a number; integers that fit stay integers.
    fn number(&mut self) -> Result<Json> {
        let start = self.i;
        let digits = |p: &mut Self| {
            let s = p.i;
            while p.i < p.s.len() && p.s[p.i].is_ascii_digit() {
                p.i += 1;
            }
            p.i > s
        };
        self.i += (self.s[self.i] == b'-') as usize;
        let int_start = self.i;
        if !digits(self) || (self.s[int_start] == b'0' && self.i - int_start > 1) {
            return Err(self.err("bad number"));
        }
        let mut float = false;
        if self.s.get(self.i) == Some(&b'.') {
            self.i += 1;
            float = true;
            if !digits(self) {
                return Err(self.err("bad number"));
            }
        }
        if matches!(self.s.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            float = true;
            self.i += matches!(self.s.get(self.i), Some(b'+' | b'-')) as usize;
            if !digits(self) {
                return Err(self.err("bad number"));
            }
        }
        let text = std::str::from_utf8(&self.s[start..self.i]).map_err(|_| self.err("bad number"))?;
        if !float {
            if let Ok(n) = text.parse::<i64>() {
                return Ok(Json::Int(n));
            }
        }
        text.parse::<f64>().map(Json::from).map_err(|_| self.err("bad number"))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Checks parse then print round-trips a document with every kind of value.
    #[test]
    fn test_parse_print_roundtrip() {
        let text = r#"{"a":[1,-2,3.5,1e3,true,false,null],"b":{"c":"x\"y\\z\n\t\u0001"},"e":[],"f":{}}"#;
        let v = Json::parse(text).expect("parse");
        assert_eq!(v["a"][0], Json::Int(1));
        assert_eq!(v["a"][3], Json::Float(1000.0));
        assert_eq!(Json::parse(&v.to_string()).expect("reparse"), v);
        assert_eq!(
            v.to_string(),
            r#"{"a":[1,-2,3.5,1000.0,true,false,null],"b":{"c":"x\"y\\z\n\t\u0001"},"e":[],"f":{}}"#
        );
        assert_eq!("[1]".parse::<Json>().expect("fromstr"), Json::Arr(vec![Json::Int(1)]));
    }

    // Checks pretty printing.
    #[test]
    fn test_pretty() {
        let v = json!({"a": [1, 2], "b": {}});
        assert_eq!(v.pretty(), "{\n  \"a\": [\n    1,\n    2\n  ],\n  \"b\": {}\n}");
    }

    // Checks escapes, surrogate pairs and the slash escape decode.
    #[test]
    fn test_parse_escapes() {
        let v = Json::parse(r#""\ud83d\ude00 \/ \b\f\r é""#).expect("parse");
        assert_eq!(v.str(), Some("😀 / \u{8}\u{c}\r é"));
        for bad in
            [r#""\ud83d""#, r#""\ud83dx""#, r#""\ud83d\u0041""#, r#""\x""#, r#""\u12""#, r#""\uzzzz""#, "\"a"]
        {
            assert!(Json::parse(bad).is_err(), "{bad}");
        }
        assert!(Json::parse("\"\\").is_err());
        assert!(Json::parse("\"a\u{1}\"").is_err());
        assert!(Json::parse(r#""\udc00""#).is_err());
        assert!(Json::parse(r#""\ud83d\uZZZZ""#).is_err());
    }

    // Checks malformed documents are refused with an offset.
    #[test]
    fn test_parse_errors() {
        for bad in [
            "",
            "nul",
            "[1,]",
            "[1 2]",
            "{\"a\" 1}",
            "{\"a\":1,}",
            "{1:2}",
            "01",
            "-",
            "1.",
            "1e",
            "1}",
            "x",
            "{\"a\":1 \"b\"}",
            "tru",
        ] {
            let e = Json::parse(bad).unwrap_err();
            assert!(e.message.starts_with("invalid JSON at byte"), "{bad}: {}", e.message);
        }
        assert!(Json::parse(&"[".repeat(200)).unwrap_err().message.contains("nested too deeply"));
        assert!(Json::parse_bytes(&[0xff]).is_err());
        assert_eq!(Json::parse_bytes(b"2").expect("bytes"), Json::Int(2));
    }

    // Checks number edge cases: big integers become floats, exponents and negatives parse.
    #[test]
    fn test_numbers() {
        assert_eq!(Json::parse("99999999999999999999").expect("big"), Json::Float(1e20));
        assert_eq!(Json::parse("-0.5E-1").expect("exp").f64(), Some(-0.05));
        assert_eq!(Json::parse("2.0").expect("float").i64(), Some(2));
        assert_eq!(Json::Float(2.5).i64(), None);
        assert_eq!(Json::Float(1e17).to_string(), "100000000000000000");
        assert_eq!(Json::from(f64::NAN), Json::Null);
        assert_eq!(Json::Str("1".into()).f64(), None);
    }

    // Checks the From conversions and the json! macro.
    #[test]
    fn test_from_and_macro() {
        let s = String::from("s");
        let v = json!({"n": null, "b": true, "i": 1, "f": 1.5, "s": "x", "a": [1, "two"], "o": {"k": (2 + 3)}, "r": (&s)});
        assert_eq!(v["o"]["k"], Json::Int(5));
        assert_eq!(v["r"].str(), Some("s"));
        assert_eq!(Json::from(3u32), Json::Int(3));
        assert_eq!(Json::from(3usize), Json::Int(3));
        assert_eq!(Json::from(u64::MAX), Json::Int(i64::MAX));
        assert_eq!(Json::from(3i32), Json::Int(3));
        assert_eq!(Json::from(String::from("x")), Json::Str("x".into()));
        assert_eq!(Json::from(vec![1i64]), json!([1]));
        assert_eq!(Json::from(None::<i64>), Json::Null);
        assert_eq!(Json::from(Some(true)), Json::Bool(true));
    }

    // Checks accessors and setters.
    #[test]
    fn test_accessors() {
        let mut v = json!({"s": "x", "n": 3, "l": ["a", "b"], "b": false});
        v.set("s", "y");
        v.set("new", 1i64);
        assert_eq!(v.str_or("s", "d"), "y");
        assert_eq!(v.str_or("zz", "d"), "d");
        assert_eq!(v.need_str("s").expect("s"), "y");
        assert!(v.need_str("n").is_err());
        assert_eq!(v.need_i64("n").expect("n"), 3);
        assert!(v.need_i64("s").is_err());
        assert_eq!(v.strs("l").expect("l"), vec!["a", "b"]);
        assert_eq!(v.strs("s").expect("s"), vec!["y"]);
        assert!(v.strs("n").is_err());
        assert!(json!({"l": [1]}).strs("l").is_err());
        assert_eq!(v["b"].bool(), Some(false));
        assert_eq!(v["n"].bool(), None);
        assert!(v["missing"].is_null());
        assert_eq!(v["l"][5], Json::Null);
        assert_eq!(v["n"].arr(), None);
        assert_eq!(v["n"].obj(), None);
        assert_eq!(v.obj().map(<[_]>::len), Some(5));
        assert_eq!(v["n"].str(), None);
        assert_eq!(v["n"]["x"], Json::Null);
        let mut not_obj = Json::Int(1);
        not_obj.set("a", 1i64);
        assert_eq!(not_obj, Json::Int(1));
        assert_eq!(Json::parse(r#"{"a":1,"a":2}"#).expect("dup")["a"], Json::Int(2));
    }
}
