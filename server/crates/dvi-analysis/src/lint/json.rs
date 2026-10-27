//! A strict JSON reader that keeps where each value is written, so a configuration problem can
//! point at its key or value (`serde_json` values carry no positions).

/// A JSON value and the byte range of its text.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub value: Value,
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Node>),
    /// Members in written order: key, the key's range, value.
    Object(Vec<Member>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Member {
    pub key: String,
    pub key_start: u32,
    pub key_end: u32,
    pub value: Node,
}

/// What is wrong with a text and where (a byte offset).
#[derive(Clone, Debug, PartialEq)]
pub struct Error {
    pub at: u32,
    pub message: String,
}

/// Values nested at most: deeper text is refused rather than walked by recursion.
const MAX_DEPTH: usize = 64;

pub fn parse(text: &str) -> Result<Node, Error> {
    let mut p = Parser { s: text.as_bytes(), text, i: 0 };
    // A byte-order mark is not JSON, but editors write one: skip it.
    if text.starts_with('\u{feff}') {
        p.i = 3;
    }
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i < p.s.len() {
        return Err(p.err("unexpected text after the value"));
    }
    Ok(v)
}

impl Node {
    pub fn as_str(&self) -> Option<&str> {
        match &self.value {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&[Member]> {
        match &self.value {
            Value::Object(m) => Some(m),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Node]> {
        match &self.value {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self.value {
            Value::Null => "null",
            Value::Bool(_) => "a boolean",
            Value::Number(_) => "a number",
            Value::String(_) => "a string",
            Value::Array(_) => "an array",
            Value::Object(_) => "an object",
        }
    }
}

struct Parser<'a> {
    s: &'a [u8],
    text: &'a str,
    i: usize,
}

impl Parser<'_> {
    fn err(&self, m: &str) -> Error {
        // On a character boundary, so that the place can be shown.
        let mut at = self.i.min(self.s.len());
        while !self.text.is_char_boundary(at) {
            at -= 1;
        }
        Error { at: at as u32, message: m.to_string() }
    }

    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn value(&mut self, depth: usize) -> Result<Node, Error> {
        if depth > MAX_DEPTH {
            return Err(self.err("values nested too deeply"));
        }
        let start = self.i;
        let value = match self.s.get(self.i) {
            None => return Err(self.err("a value is missing")),
            Some(b'{') => self.object(depth)?,
            Some(b'[') => self.array(depth)?,
            Some(b'"') => Value::String(self.string()?),
            Some(b't') => self.word("true", Value::Bool(true))?,
            Some(b'f') => self.word("false", Value::Bool(false))?,
            Some(b'n') => self.word("null", Value::Null)?,
            Some(b'-' | b'0'..=b'9') => self.number()?,
            Some(b'/') => return Err(self.err("comments are not allowed in JSON (use a \"comment\" field)")),
            Some(_) => return Err(self.err("not a JSON value")),
        };
        Ok(Node { value, start: start as u32, end: self.i as u32 })
    }

    fn word(&mut self, w: &str, v: Value) -> Result<Value, Error> {
        if self.s[self.i..].starts_with(w.as_bytes()) {
            self.i += w.len();
            Ok(v)
        } else {
            Err(self.err("not a JSON value"))
        }
    }

    fn number(&mut self) -> Result<Value, Error> {
        let start = self.i;
        if self.s[self.i] == b'-' {
            self.i += 1;
        }
        let digits = |p: &mut Self| {
            let d = p.i;
            while p.i < p.s.len() && p.s[p.i].is_ascii_digit() {
                p.i += 1;
            }
            p.i > d
        };
        if self.s.get(self.i) == Some(&b'0') {
            self.i += 1;
        } else if !digits(self) {
            return Err(self.err("not a number"));
        }
        if self.s.get(self.i) == Some(&b'.') {
            self.i += 1;
            if !digits(self) {
                return Err(self.err("not a number"));
            }
        }
        if matches!(self.s.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.s.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if !digits(self) {
                return Err(self.err("not a number"));
            }
        }
        self.text[start..self.i].parse::<f64>().map(Value::Number).map_err(|_| self.err("not a number"))
    }

    fn string(&mut self) -> Result<String, Error> {
        self.i += 1;
        let mut out = String::new();
        loop {
            let Some(&c) = self.s.get(self.i) else { return Err(self.err("a string is not closed")) };
            match c {
                b'"' => {
                    self.i += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.i += 1;
                    let Some(&e) = self.s.get(self.i) else { return Err(self.err("a string is not closed")) };
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            let ch = if (0xD800..0xDC00).contains(&hi) {
                                // A surrogate pair.
                                if self.s.get(self.i) != Some(&b'\\') || self.s.get(self.i + 1) != Some(&b'u') {
                                    return Err(self.err("an unpaired surrogate in a string"));
                                }
                                self.i += 2;
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return Err(self.err("an unpaired surrogate in a string"));
                                }
                                char::from_u32(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00))
                            } else {
                                char::from_u32(hi)
                            };
                            out.push(ch.ok_or_else(|| self.err("an unpaired surrogate in a string"))?);
                        }
                        _ => return Err(self.err("an unknown escape in a string")),
                    }
                }
                0..=0x1f => return Err(self.err("a control character in a string (write it as an escape)")),
                _ => {
                    // A whole UTF-8 sequence (the text is a `str`).
                    let ch = self.text[self.i..].chars().next().unwrap();
                    out.push(ch);
                    self.i += ch.len_utf8();
                }
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, Error> {
        let h = self.text.get(self.i..self.i + 4).filter(|h| h.bytes().all(|b| b.is_ascii_hexdigit())).ok_or_else(|| self.err("a bad \\u escape"))?;
        let v = u32::from_str_radix(h, 16).map_err(|_| self.err("a bad \\u escape"))?;
        self.i += 4;
        Ok(v)
    }

    fn array(&mut self, depth: usize) -> Result<Value, Error> {
        self.i += 1;
        let mut out = Vec::new();
        self.ws();
        if self.s.get(self.i) == Some(&b']') {
            self.i += 1;
            return Ok(Value::Array(out));
        }
        loop {
            self.ws();
            out.push(self.value(depth + 1)?);
            self.ws();
            match self.s.get(self.i) {
                Some(b',') => {
                    self.i += 1;
                    self.ws();
                    if self.s.get(self.i) == Some(&b']') {
                        return Err(self.err("a trailing comma is not allowed in JSON"));
                    }
                }
                Some(b']') => {
                    self.i += 1;
                    return Ok(Value::Array(out));
                }
                _ => return Err(self.err("expected , or ]")),
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, Error> {
        self.i += 1;
        let mut out: Vec<Member> = Vec::new();
        let mut keys: rustc_hash::FxHashSet<String> = rustc_hash::FxHashSet::default();
        self.ws();
        if self.s.get(self.i) == Some(&b'}') {
            self.i += 1;
            return Ok(Value::Object(out));
        }
        loop {
            self.ws();
            if self.s.get(self.i) != Some(&b'"') {
                return Err(self.err(if self.s.get(self.i) == Some(&b'/') { "comments are not allowed in JSON (use a \"comment\" field)" } else { "expected a key in double quotes" }));
            }
            let key_start = self.i as u32;
            let key = self.string()?;
            let key_end = self.i as u32;
            if !keys.insert(key.clone()) {
                return Err(Error { at: key_start, message: format!("\"{key}\" is given twice") });
            }
            self.ws();
            if self.s.get(self.i) != Some(&b':') {
                return Err(self.err("expected :"));
            }
            self.i += 1;
            self.ws();
            let value = self.value(depth + 1)?;
            out.push(Member { key, key_start, key_end, value });
            self.ws();
            match self.s.get(self.i) {
                Some(b',') => {
                    self.i += 1;
                    self.ws();
                    if self.s.get(self.i) == Some(&b'}') {
                        return Err(self.err("a trailing comma is not allowed in JSON"));
                    }
                }
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Value::Object(out));
                }
                _ => return Err(self.err("expected , or }")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wide object is read in linear time, and a key given twice is still an error.
    #[test]
    fn wide_objects() {
        let mut t = String::from("{");
        for i in 0..200_000 {
            t.push_str(&format!("\"k{i}\": {i},"));
        }
        t.push_str("\"k7\": 1}");
        let at = std::time::Instant::now();
        let e = parse(&t).err().expect("a key given twice");
        assert!(e.message.contains("\"k7\" is given twice"), "{}", e.message);
        assert!(at.elapsed().as_secs() < 5);
    }

    #[test]
    fn values_and_positions() {
        let t = r#"{ "a": [1, -2.5e3, true, null], "b": { "c": "x\"\u00e9\ud83d\ude00" } }"#;
        let v = parse(t).unwrap();
        let m = v.as_object().unwrap();
        assert_eq!(m[0].key, "a");
        assert_eq!(&t[m[0].key_start as usize..m[0].key_end as usize], "\"a\"");
        let a = m[0].value.as_array().unwrap();
        assert_eq!(a[1].value, Value::Number(-2500.0));
        assert_eq!(&t[a[2].start as usize..a[2].end as usize], "true");
        let c = &m[1].value.as_object().unwrap()[0].value;
        assert_eq!(c.as_str(), Some("x\"é😀"));
    }

    #[test]
    fn strictness() {
        for bad in ["{\"a\": 1,}", "[1,]", "{// c\n}", "{\"a\": 01}", "{'a': 1}", "{\"a\": 1} x", "{\"a\": 1, \"a\": 2}", "\"\\x\"", "\"a\nb\""] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        let e = parse("{\"comment\":\"\\é\"}").unwrap_err();
        assert!("{\"comment\":\"\\é\"}".is_char_boundary(e.at as usize));
        let e = parse("{\n  // note\n}").unwrap_err();
        assert!(e.message.contains("comment"));
        assert_eq!(e.at, 4);
    }

    #[test]
    fn deep_nesting_is_refused() {
        let deep = format!("{}{}", "[".repeat(10_000), "]".repeat(10_000));
        assert!(parse(&deep).is_err());
    }
}
