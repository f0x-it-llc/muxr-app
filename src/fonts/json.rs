//! A minimal, read-only JSON value and parser — the font manifest's wire
//! format, and nothing else.
//!
//! # Why hand-rolled (again)
//!
//! The app already hand-rolls JSON once, for the saved-server store
//! (`features::connect::data::repositories::json`, whose module docs carry the
//! full rationale: this manifest declares no JSON dependency, and the
//! framework's own shells hand-roll their fixed payloads the same way —
//! `docs/CODE_STANDARDS.md`'s hand-roll-JSON rule). That copy is `pub(super)`
//! inside a *feature's* `data` layer, and an app-level module reaching across
//! into a feature's data layer is exactly the layering violation
//! `docs/ARCHITECTURE.md` forbids — so this is a second, deliberately
//! narrower copy rather than a shared import. It is **parse-only**: nothing
//! here re-encodes a manifest (the cached copy is the fetched bytes,
//! verbatim), so the encoder half of the saved-server copy has no counterpart.
//!
//! Promoting one shared `crate::json` beside `failure.rs` is the obvious
//! follow-up once a third consumer appears; it would touch `connect`'s data
//! layer, which is out of this module's remit.
//!
//! # Contract
//!
//! [`parse`] answers `None` for anything that is not well-formed JSON — the
//! catalog above treats that as "no catalog", never a panic. Numbers are
//! `f64`: the largest value this schema carries is a font file's byte count
//! (~3e6), far inside the range `f64` represents exactly.

/// How deep a nested array/object may go before [`parse`] gives up. The
/// manifest is three levels (an object holding an array of objects holding an
/// array of objects); the limit exists so a hostile or hand-edited payload
/// cannot recurse this parser into a stack overflow.
const MAX_DEPTH: usize = 32;

/// One JSON value.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Json {
    /// `null`.
    Null,
    /// `true`/`false`.
    Bool(bool),
    /// Any JSON number (see the [module docs](self)' note on `f64`).
    Number(f64),
    /// A string, with every escape already resolved.
    String(String),
    /// An array, in source order.
    Array(Vec<Json>),
    /// An object, in source order. A `Vec` rather than a map: the manifest has
    /// a handful of known keys per level, and a duplicate key (legal JSON)
    /// resolves first-wins through [`Json::get`] rather than needing a map's
    /// own policy.
    Object(Vec<(String, Json)>),
}

impl Json {
    /// This value as a string, or `None` if it is not one.
    pub(super) fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(value) => Some(value),
            _ => None,
        }
    }

    /// This value as a bool, or `None` if it is not one.
    pub(super) fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(value) => Some(*value),
            _ => None,
        }
    }

    /// This value as a non-negative whole number, or `None` if it is not a
    /// number, is negative, or carries a fractional part (which this schema
    /// never writes, and which rounding would silently accept).
    pub(super) fn as_u64(&self) -> Option<u64> {
        match self {
            Json::Number(value) if value.fract() == 0.0 && value.is_finite() && *value >= 0.0 => {
                Some(*value as u64)
            }
            _ => None,
        }
    }

    /// This value's elements, or `None` if it is not an array.
    pub(super) fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(items) => Some(items),
            _ => None,
        }
    }

    /// The value stored under `key`, or `None` for a non-object (or an object
    /// without it). First match wins on a duplicate key.
    pub(super) fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(fields) => fields
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }
}

/// Parse `input` as one JSON document, or `None` if it is malformed (or
/// carries trailing content past the top-level value).
pub(super) fn parse(input: &str) -> Option<Json> {
    let bytes = input.as_bytes();
    let mut cursor = Cursor { bytes, at: 0 };
    cursor.skip_whitespace();
    let value = cursor.value(0)?;
    cursor.skip_whitespace();
    if cursor.at == bytes.len() {
        Some(value)
    } else {
        None
    }
}

/// A byte cursor over the document being parsed.
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Cursor<'_> {
    /// The byte at the cursor, without consuming it.
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    /// Consume `byte` if it is next; report whether it was.
    fn eat(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    /// Skip JSON's four whitespace bytes.
    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    /// One value at `depth`, or `None` for malformed input.
    fn value(&mut self, depth: usize) -> Option<Json> {
        if depth > MAX_DEPTH {
            return None;
        }
        match self.peek()? {
            b'{' => self.object(depth),
            b'[' => self.array(depth),
            b'"' => self.string().map(Json::String),
            b't' => self.literal(b"true").then_some(Json::Bool(true)),
            b'f' => self.literal(b"false").then_some(Json::Bool(false)),
            b'n' => self.literal(b"null").then_some(Json::Null),
            _ => self.number(),
        }
    }

    /// Consume `word` exactly, or report `false` (leaving the cursor put).
    fn literal(&mut self, word: &[u8]) -> bool {
        if self.bytes[self.at..].starts_with(word) {
            self.at += word.len();
            true
        } else {
            false
        }
    }

    /// `{ "key": value, .. }`.
    fn object(&mut self, depth: usize) -> Option<Json> {
        self.at += 1; // '{'
        let mut fields = Vec::new();
        self.skip_whitespace();
        if self.eat(b'}') {
            return Some(Json::Object(fields));
        }
        loop {
            self.skip_whitespace();
            let key = self.string()?;
            self.skip_whitespace();
            if !self.eat(b':') {
                return None;
            }
            self.skip_whitespace();
            let value = self.value(depth + 1)?;
            fields.push((key, value));
            self.skip_whitespace();
            if self.eat(b',') {
                continue;
            }
            return self.eat(b'}').then_some(Json::Object(fields));
        }
    }

    /// `[ value, .. ]`.
    fn array(&mut self, depth: usize) -> Option<Json> {
        self.at += 1; // '['
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.eat(b']') {
            return Some(Json::Array(items));
        }
        loop {
            self.skip_whitespace();
            items.push(self.value(depth + 1)?);
            self.skip_whitespace();
            if self.eat(b',') {
                continue;
            }
            return self.eat(b']').then_some(Json::Array(items));
        }
    }

    /// A `"…"` string with every escape resolved (including surrogate pairs).
    fn string(&mut self) -> Option<String> {
        if !self.eat(b'"') {
            return None;
        }
        let mut out = String::new();
        loop {
            let byte = self.peek()?;
            self.at += 1;
            match byte {
                b'"' => return Some(out),
                b'\\' => {
                    let escape = self.peek()?;
                    self.at += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.unicode_escape()?),
                        _ => return None,
                    }
                }
                // A raw control byte is illegal in a JSON string.
                0x00..=0x1f => return None,
                _ => {
                    // Any other byte is part of a UTF-8 sequence: copy the
                    // whole sequence over verbatim (the input is already a
                    // `&str`, so it is valid UTF-8 by construction).
                    let start = self.at - 1;
                    while self.peek().is_some_and(|next| (0x80..0xc0).contains(&next)) {
                        self.at += 1;
                    }
                    out.push_str(std::str::from_utf8(&self.bytes[start..self.at]).ok()?);
                }
            }
        }
    }

    /// A `\uXXXX` escape's character, joining a surrogate pair when one
    /// follows.
    fn unicode_escape(&mut self) -> Option<char> {
        let first = self.hex4()?;
        // A high surrogate is only meaningful paired with its low half.
        if (0xd800..0xdc00).contains(&first) {
            if !self.eat(b'\\') || !self.eat(b'u') {
                return None;
            }
            let second = self.hex4()?;
            if !(0xdc00..0xe000).contains(&second) {
                return None;
            }
            let combined =
                0x1_0000 + ((u32::from(first) - 0xd800) << 10) + (u32::from(second) - 0xdc00);
            return char::from_u32(combined);
        }
        char::from_u32(u32::from(first))
    }

    /// Four hex digits, as a `u16`.
    fn hex4(&mut self) -> Option<u16> {
        let end = self.at.checked_add(4)?;
        let digits = self.bytes.get(self.at..end)?;
        let text = std::str::from_utf8(digits).ok()?;
        let value = u16::from_str_radix(text, 16).ok()?;
        self.at = end;
        Some(value)
    }

    /// A JSON number, parsed by `f64`'s own parser over the span this scans.
    fn number(&mut self) -> Option<Json> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        while self
            .peek()
            .is_some_and(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-'))
        {
            self.at += 1;
        }
        if start == self.at {
            return None;
        }
        let text = std::str::from_utf8(&self.bytes[start..self.at]).ok()?;
        text.parse::<f64>().ok().map(Json::Number)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_manifest_shaped_document_round_trips_into_values() {
        let parsed = parse(
            r#"{ "version": 1, "fonts": [ { "id": "a", "isNerdFont": true,
                 "files": [ { "bytes": 2411088, "path": "A.ttf" } ] } ] }"#,
        )
        .expect("well-formed");
        assert_eq!(parsed.get("version").and_then(Json::as_u64), Some(1));
        let fonts = parsed
            .get("fonts")
            .and_then(Json::as_array)
            .expect("an array");
        assert_eq!(fonts.len(), 1);
        assert_eq!(fonts[0].get("id").and_then(Json::as_str), Some("a"));
        assert_eq!(
            fonts[0].get("isNerdFont").and_then(Json::as_bool),
            Some(true)
        );
        let files = fonts[0]
            .get("files")
            .and_then(Json::as_array)
            .expect("an array");
        assert_eq!(files[0].get("bytes").and_then(Json::as_u64), Some(2411088));
    }

    #[test]
    fn escapes_and_unicode_survive_a_parse() {
        let parsed = parse(r#"{ "a": "q\"\\\né😀", "b": "café" }"#).expect("valid");
        assert_eq!(
            parsed.get("a").and_then(Json::as_str),
            Some("q\"\\\n\u{e9}\u{1f600}")
        );
        assert_eq!(parsed.get("b").and_then(Json::as_str), Some("café"));
    }

    #[test]
    fn malformed_input_is_none_rather_than_a_panic() {
        for bad in [
            "",
            "{",
            "{\"a\":}",
            "[1,]",
            "{\"a\":1}trailing",
            "\"unterminated",
            "{\"a\": \"\u{1}\"}",
        ] {
            assert_eq!(parse(bad), None, "{bad:?} must not parse");
        }
    }

    #[test]
    fn nesting_past_the_depth_cap_is_refused() {
        let deep = format!("{}1{}", "[".repeat(64), "]".repeat(64));
        assert_eq!(parse(&deep), None);
        let shallow = format!("{}1{}", "[".repeat(8), "]".repeat(8));
        assert!(parse(&shallow).is_some());
    }

    #[test]
    fn a_number_with_a_fraction_is_not_a_whole_number() {
        let parsed = parse(r#"{ "a": 1.5, "b": -3, "c": 12 }"#).expect("valid");
        assert_eq!(parsed.get("a").and_then(Json::as_u64), None);
        assert_eq!(parsed.get("b").and_then(Json::as_u64), None, "negative");
        assert_eq!(parsed.get("c").and_then(Json::as_u64), Some(12));
    }
}
