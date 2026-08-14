//! A minimal JSON value, encoder and parser — the saved-server store's wire
//! format, and nothing else.
//!
//! Hand-rolled because this app's manifest carries no JSON dependency and adding
//! one to spell a ten-field record is not the trade
//! (`docs/CODE_STANDARDS.md`'s hand-roll-JSON-at-the-FFI-seam rule).
//!
//! [`parse`] answers `None` for anything that is not well-formed JSON, and the
//! store above treats that as an empty list rather than a failure — a corrupt
//! payload must never take a boot down. [`Json::encode`] emits the subset this
//! module can read back, so a round trip is lossless for every value the store
//! writes. Numbers are `f64`: the largest value this schema stores is an epoch
//! millisecond stamp, far inside the 2^53 range an `f64` represents exactly, and
//! [`Json::as_i64`] refuses a fractional part rather than rounding.

use std::fmt::Write as _;

/// How deep a nested array/object may go before [`parse`] gives up — the limit
/// exists so a hostile payload cannot recurse the parser into a stack overflow.
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
    /// An object, in source order. A `Vec` rather than a map: insertion order is
    /// what makes an encoded record diffable, and a duplicate key resolves
    /// last-wins through [`Json::get`] either way.
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

    /// This value as a whole number, or `None` if it is not a number or carries
    /// a fractional part (which this schema never writes, and which rounding
    /// would silently accept).
    pub(super) fn as_i64(&self) -> Option<i64> {
        match self {
            Json::Number(value) if value.fract() == 0.0 && value.is_finite() => {
                // `as` saturates at the i64 bounds in Rust, so an out-of-range
                // stamp clamps rather than wrapping into a negative one.
                Some(*value as i64)
            }
            _ => None,
        }
    }

    /// The value stored under `key`, or `None` when this is not an object or
    /// carries no such key. A duplicate key resolves **last-wins**, matching
    /// how every mainstream parser reads one.
    pub(super) fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(fields) => fields
                .iter()
                .rev()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    /// This value, encoded. Compact (no whitespace) — the payload is machine
    /// state, not something a human reads out of the store.
    pub(super) fn encode(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    /// [`Self::encode`]'s recursive half.
    fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(true) => out.push_str("true"),
            Json::Bool(false) => out.push_str("false"),
            Json::Number(value) => {
                // A non-finite float has no JSON spelling; this schema never
                // produces one, and writing `null` keeps the payload parseable
                // if some later field ever does.
                if value.is_finite() {
                    let _ = write!(out, "{value}");
                } else {
                    out.push_str("null");
                }
            }
            Json::String(value) => write_string(value, out),
            Json::Array(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Json::Object(fields) => {
                out.push('{');
                for (index, (name, value)) in fields.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    write_string(name, out);
                    out.push(':');
                    value.write(out);
                }
                out.push('}');
            }
        }
    }
}

/// `value` as a quoted, escaped JSON string.
fn write_string(value: &str, out: &mut String) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            // Every other C0 control needs the `\u` form; everything above it
            // is legal verbatim in a JSON string, including non-ASCII.
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Parse `raw` as one JSON value, or `None` if it is malformed (including
/// trailing content after the value).
pub(super) fn parse(raw: &str) -> Option<Json> {
    let mut parser = Parser {
        input: raw.as_bytes(),
        at: 0,
    };
    let value = parser.value(0)?;
    parser.skip_whitespace();
    parser.at.eq(&parser.input.len()).then_some(value)
}

/// A position in the input. Byte-indexed: every structural character JSON uses
/// is ASCII, and the one place multi-byte text appears (inside a string) is
/// copied out through `str::from_utf8`.
struct Parser<'a> {
    input: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    /// One value at `depth`, consuming leading whitespace.
    fn value(&mut self, depth: usize) -> Option<Json> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.skip_whitespace();
        match self.peek()? {
            b'n' => self.literal(b"null", Json::Null),
            b't' => self.literal(b"true", Json::Bool(true)),
            b'f' => self.literal(b"false", Json::Bool(false)),
            b'"' => self.string().map(Json::String),
            b'[' => self.array(depth),
            b'{' => self.object(depth),
            _ => self.number(),
        }
    }

    /// The byte at the cursor, without consuming it.
    fn peek(&self) -> Option<u8> {
        self.input.get(self.at).copied()
    }

    /// Consume `byte` if it is next.
    fn eat(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.at += 1;
            return true;
        }
        false
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    /// One of the three bare keywords.
    fn literal(&mut self, word: &[u8], value: Json) -> Option<Json> {
        if self.input[self.at..].starts_with(word) {
            self.at += word.len();
            return Some(value);
        }
        None
    }

    fn array(&mut self, depth: usize) -> Option<Json> {
        self.at += 1;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.eat(b']') {
            return Some(Json::Array(items));
        }
        loop {
            items.push(self.value(depth + 1)?);
            self.skip_whitespace();
            if self.eat(b',') {
                continue;
            }
            return self.eat(b']').then_some(Json::Array(items));
        }
    }

    fn object(&mut self, depth: usize) -> Option<Json> {
        self.at += 1;
        let mut fields = Vec::new();
        self.skip_whitespace();
        if self.eat(b'}') {
            return Some(Json::Object(fields));
        }
        loop {
            self.skip_whitespace();
            let name = self.string()?;
            self.skip_whitespace();
            if !self.eat(b':') {
                return None;
            }
            fields.push((name, self.value(depth + 1)?));
            self.skip_whitespace();
            if self.eat(b',') {
                continue;
            }
            return self.eat(b'}').then_some(Json::Object(fields));
        }
    }

    /// A quoted string with its escapes resolved.
    fn string(&mut self) -> Option<String> {
        if !self.eat(b'"') {
            return None;
        }
        let mut out = String::new();
        loop {
            let byte = self.peek()?;
            match byte {
                b'"' => {
                    self.at += 1;
                    return Some(out);
                }
                b'\\' => {
                    self.at += 1;
                    self.escape(&mut out)?;
                }
                // A raw control character is illegal inside a JSON string.
                0x00..=0x1f => return None,
                _ => {
                    // Copy the whole UTF-8 sequence at once: `from_utf8` on the
                    // remainder validates it, and the first character of a
                    // valid remainder is the one at the cursor.
                    let rest = std::str::from_utf8(&self.input[self.at..]).ok()?;
                    let ch = rest.chars().next()?;
                    self.at += ch.len_utf8();
                    out.push(ch);
                }
            }
        }
    }

    /// One escape sequence, already past its backslash.
    fn escape(&mut self, out: &mut String) -> Option<()> {
        let byte = self.peek()?;
        self.at += 1;
        let resolved = match byte {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{08}',
            b'f' => '\u{0c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => return self.unicode_escape(out),
            _ => return None,
        };
        out.push(resolved);
        Some(())
    }

    /// A `\uXXXX` escape, joining a surrogate pair when one follows.
    fn unicode_escape(&mut self, out: &mut String) -> Option<()> {
        let first = self.hex4()?;
        // A high surrogate is only meaningful paired with the low one that
        // follows it; JSON has no other way to spell an astral character.
        if (0xd800..0xdc00).contains(&first) {
            if !(self.eat(b'\\') && self.eat(b'u')) {
                return None;
            }
            let second = self.hex4()?;
            if !(0xdc00..0xe000).contains(&second) {
                return None;
            }
            let combined = 0x10000 + ((first - 0xd800) << 10) + (second - 0xdc00);
            out.push(char::from_u32(combined)?);
            return Some(());
        }
        out.push(char::from_u32(first)?);
        Some(())
    }

    /// Four hex digits, as a code unit.
    fn hex4(&mut self) -> Option<u32> {
        let digits = self.input.get(self.at..self.at + 4)?;
        let text = std::str::from_utf8(digits).ok()?;
        let value = u32::from_str_radix(text, 16).ok()?;
        self.at += 4;
        Some(value)
    }

    /// A number token, handed to `f64`'s own parser once its span is known.
    fn number(&mut self) -> Option<Json> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        self.integer_part()?;
        if self.eat(b'.') {
            self.digits()?;
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            self.digits()?;
        }
        let text = std::str::from_utf8(&self.input[start..self.at]).ok()?;
        text.parse::<f64>().ok().map(Json::Number)
    }

    /// A number's integer part: a bare `0`, or a non-zero digit and whatever
    /// follows it. The grammar's no-leading-zeros rule, enforced so a
    /// hand-edited `01` reads as the malformed payload it is rather than as 1.
    fn integer_part(&mut self) -> Option<()> {
        if self.eat(b'0') {
            if matches!(self.peek(), Some(b'0'..=b'9')) {
                return None;
            }
            return Some(());
        }
        self.digits()
    }

    /// At least one digit.
    fn digits(&mut self) -> Option<()> {
        let start = self.at;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.at += 1;
        }
        (self.at > start).then_some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flat_record_round_trips() {
        let value = Json::Object(vec![
            ("host".to_string(), Json::String("rig.example".to_string())),
            ("port".to_string(), Json::Number(50051.0)),
            ("readOnly".to_string(), Json::Bool(false)),
            ("tokenName".to_string(), Json::Null),
        ]);
        let encoded = value.encode();
        assert_eq!(
            encoded,
            r#"{"host":"rig.example","port":50051,"readOnly":false,"tokenName":null}"#
        );
        assert_eq!(parse(&encoded), Some(value));
    }

    #[test]
    fn an_array_of_records_round_trips() {
        let value = Json::Array(vec![
            Json::Object(vec![("a".to_string(), Json::Number(1.0))]),
            Json::Object(vec![("a".to_string(), Json::Number(2.0))]),
        ]);
        assert_eq!(parse(&value.encode()), Some(value));
    }

    #[test]
    fn every_escape_this_encoder_writes_reads_back() {
        let raw = "quote:\" backslash:\\ newline:\n tab:\t bell:\u{07} unicode:é emoji:🦀";
        let encoded = Json::String(raw.to_string()).encode();
        assert!(encoded.contains("\\u0007"), "{encoded}");
        assert_eq!(
            parse(&encoded).and_then(|v| v.as_str().map(str::to_string)),
            Some(raw.to_string())
        );
    }

    #[test]
    fn a_surrogate_pair_escape_decodes_to_one_astral_character() {
        assert_eq!(
            parse(r#""🦀""#).and_then(|v| v.as_str().map(str::to_string)),
            Some("🦀".to_string())
        );
    }

    #[test]
    fn a_lone_high_surrogate_is_malformed_rather_than_a_replacement_character() {
        assert_eq!(parse(r#""\ud83e""#), None);
    }

    #[test]
    fn accessors_refuse_the_wrong_shape() {
        let value = parse(r#"{"n":7,"f":7.5,"s":"x","b":true,"a":[1]}"#).expect("well-formed");
        assert_eq!(value.get("n").and_then(Json::as_i64), Some(7));
        assert_eq!(
            value.get("f").and_then(Json::as_i64),
            None,
            "a fractional number is not a whole one"
        );
        assert_eq!(value.get("s").and_then(Json::as_str), Some("x"));
        assert_eq!(value.get("b").and_then(Json::as_bool), Some(true));
        assert_eq!(value.get("a"), Some(&Json::Array(vec![Json::Number(1.0)])));
        assert_eq!(value.get("missing"), None);
        assert_eq!(value.get("s").and_then(Json::as_bool), None);
    }

    #[test]
    fn a_duplicate_key_resolves_last_wins() {
        let value = parse(r#"{"a":1,"a":2}"#).expect("legal JSON, if unusual");
        assert_eq!(value.get("a").and_then(Json::as_i64), Some(2));
    }

    #[test]
    fn whitespace_between_every_token_is_accepted() {
        let value = parse(" { \"a\" : [ 1 , 2 ] } ").expect("well-formed");
        assert_eq!(
            value.get("a"),
            Some(&Json::Array(vec![Json::Number(1.0), Json::Number(2.0)]))
        );
    }

    #[test]
    fn malformed_payloads_parse_to_nothing_rather_than_panicking() {
        for raw in [
            "",
            "not json at all",
            "{",
            "[1,]",
            "{\"a\":}",
            "{\"a\" 1}",
            "\"unterminated",
            "01",
            "{}{}",
            "tru",
            "[1] trailing",
        ] {
            assert_eq!(parse(raw), None, "{raw:?} must not parse");
        }
    }

    #[test]
    fn nesting_past_the_depth_limit_is_refused_rather_than_recursed() {
        let deep = format!("{}{}", "[".repeat(MAX_DEPTH + 2), "]".repeat(MAX_DEPTH + 2));
        assert_eq!(parse(&deep), None);
    }

    #[test]
    fn an_exponent_form_still_reads_as_a_whole_number() {
        // Nothing here writes one, but a hand-edited store can hold one.
        assert_eq!(parse("1e3").and_then(|v| v.as_i64()), Some(1000));
    }
}
