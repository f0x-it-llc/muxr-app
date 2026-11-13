//! A minimal, parse-only JSON value and parser for the font manifest.
//! Hand-rolled because the connect feature's copy is private to its data layer; numbers are `f64`.
//! [`parse`] answers `None` for anything that is not well-formed JSON.

/// Nesting limit so a hostile payload cannot overflow the stack (the manifest is three levels deep).
const MAX_DEPTH: usize = 32;

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    /// In source order; a duplicate key resolves first-wins through [`Json::get`].
    Object(Vec<(String, Json)>),
}

impl Json {
    pub(super) fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(value) => Some(value),
            _ => None,
        }
    }

    pub(super) fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(value) => Some(*value),
            _ => None,
        }
    }

    /// Rejects negative and fractional numbers rather than rounding them.
    pub(super) fn as_u64(&self) -> Option<u64> {
        match self {
            Json::Number(value) if value.fract() == 0.0 && value.is_finite() && *value >= 0.0 => {
                Some(*value as u64)
            }
            _ => None,
        }
    }

    pub(super) fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(items) => Some(items),
            _ => None,
        }
    }

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

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Cursor<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn eat(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

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

    fn literal(&mut self, word: &[u8]) -> bool {
        if self.bytes[self.at..].starts_with(word) {
            self.at += word.len();
            true
        } else {
            false
        }
    }

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
                0x00..=0x1f => return None,
                _ => {
                    let start = self.at - 1;
                    while self.peek().is_some_and(|next| (0x80..0xc0).contains(&next)) {
                        self.at += 1;
                    }
                    out.push_str(std::str::from_utf8(&self.bytes[start..self.at]).ok()?);
                }
            }
        }
    }

    fn unicode_escape(&mut self) -> Option<char> {
        let first = self.hex4()?;
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

    fn hex4(&mut self) -> Option<u16> {
        let end = self.at.checked_add(4)?;
        let digits = self.bytes.get(self.at..end)?;
        let text = std::str::from_utf8(digits).ok()?;
        let value = u16::from_str_radix(text, 16).ok()?;
        self.at = end;
        Some(value)
    }

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
