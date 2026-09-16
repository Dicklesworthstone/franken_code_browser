#![allow(dead_code)]

//! Independent, test-only parser for the emitted schema's JSON subset.
//! Numeric tokens are intentionally rejected: this schema's integers must be
//! strings. This is NOT a general JSON parser or a production dependency.

use std::collections::BTreeMap;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Json { Null, Bool(bool), String(String), Array(Vec<Json>), Object(BTreeMap<String, Json>) }
impl Json {
    pub fn get(&self, field: &str) -> &Json {
        match self { Self::Object(fields) => fields.get(field).unwrap_or_else(|| panic!("missing {field}")),
            _ => panic!("not an object") }
    }
    pub fn text(&self) -> &str { match self { Self::String(text) => text, _ => panic!("not a string: {self:?}") } }
    pub fn flag(&self) -> bool { match self { Self::Bool(value) => *value, _ => panic!("not a bool") } }
    pub fn array(&self) -> &[Json] { match self { Self::Array(value) => value, _ => panic!("not an array") } }
    pub fn number(&self) -> u64 { fcb_app::args::decimal(self.text()).unwrap() }
}

pub fn parse(bytes: &[u8]) -> Result<Json, &'static str> {
    if bytes.len() > fcb_app::output::MAX_RESPONSE_BYTES { return Err("oversized document"); }
    let text = std::str::from_utf8(bytes).map_err(|_| "not UTF-8")?;
    let mut parser = Parser { text, cursor: 0 };
    let value = parser.value(0)?;
    parser.space();
    if parser.cursor != text.len() { return Err("trailing or second document"); }
    Ok(value)
}
struct Parser<'a> { text: &'a str, cursor: usize }
impl Parser<'_> {
    fn peek(&self) -> Option<u8> { self.text.as_bytes().get(self.cursor).copied() }
    fn space(&mut self) { while self.peek().is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n')) { self.cursor += 1; } }
    fn eat(&mut self, expected: u8) -> Result<(), &'static str> {
        self.space();
        if self.peek() != Some(expected) { return Err("unexpected token"); }
        self.cursor += 1; Ok(())
    }
    fn word(&mut self, word: &str, value: Json) -> Result<Json, &'static str> {
        if !self.text[self.cursor..].starts_with(word) { return Err("bad literal"); }
        self.cursor += word.len(); Ok(value)
    }
    fn value(&mut self, depth: usize) -> Result<Json, &'static str> {
        if depth > 32 { return Err("nesting limit"); }
        self.space();
        match self.peek() {
            Some(b'n') => self.word("null", Json::Null),
            Some(b't') => self.word("true", Json::Bool(true)),
            Some(b'f') => self.word("false", Json::Bool(false)),
            Some(b'"') => self.string().map(Json::String),
            Some(b'[') => {
                self.cursor += 1; self.space();
                let mut values = Vec::new();
                if self.peek() == Some(b']') { self.cursor += 1; return Ok(Json::Array(values)); }
                loop {
                    values.push(self.value(depth + 1)?); self.space();
                    if self.peek() == Some(b']') { self.cursor += 1; break; }
                    self.eat(b',')?;
                }
                Ok(Json::Array(values))
            }
            Some(b'{') => {
                self.cursor += 1; self.space();
                let mut values = BTreeMap::new();
                if self.peek() == Some(b'}') { self.cursor += 1; return Ok(Json::Object(values)); }
                loop {
                    let key = self.string()?; self.eat(b':')?;
                    let value = self.value(depth + 1)?;
                    if values.insert(key, value).is_some() { return Err("duplicate field"); }
                    self.space();
                    if self.peek() == Some(b'}') { self.cursor += 1; break; }
                    self.eat(b',')?;
                }
                Ok(Json::Object(values))
            }
            _ => Err("unsupported/malformed token, including numeric integer"),
        }
    }
    fn string(&mut self) -> Result<String, &'static str> {
        self.eat(b'"')?;
        let mut output = String::new();
        loop {
            let ch = self.text[self.cursor..].chars().next().ok_or("unterminated string")?;
            self.cursor += ch.len_utf8();
            match ch {
                '"' => return Ok(output),
                '\\' => {
                    let escape = self.peek().ok_or("truncated escape")?; self.cursor += 1;
                    match escape {
                        b'"' => output.push('"'), b'\\' => output.push('\\'), b'/' => output.push('/'),
                        b'b' => output.push('\u{0008}'), b'f' => output.push('\u{000c}'),
                        b'n' => output.push('\n'), b'r' => output.push('\r'), b't' => output.push('\t'),
                        b'u' => {
                            let end = self.cursor.checked_add(4).ok_or("escape overflow")?;
                            let hex = self.text.get(self.cursor..end).ok_or("truncated unicode escape")?;
                            let scalar = u32::from_str_radix(hex, 16).map_err(|_| "bad hex escape")?;
                            output.push(char::from_u32(scalar).ok_or("surrogate outside emitted subset")?);
                            self.cursor = end;
                        }
                        _ => return Err("bad escape"),
                    }
                }
                ch if ch < '\u{0020}' => return Err("unescaped control"),
                _ => output.push(ch),
            }
        }
    }
}
