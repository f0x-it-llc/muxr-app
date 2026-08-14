//! [`parse_pairing_uri`] — decodes a scanned `muxr://pair` QR code into a
//! typed [`PairingPayload`].
//!
//! A pure parse, verified against both `muxrctl`'s generator and the Flutter
//! client's own `PairingPayload.tryParse`. Every rejection is `None` rather than a
//! failure: a malformed code has no single-field detail worth showing, and the
//! camera screen's retry loop — not a toast — is its failure UX.
//!
//! # Grammar
//!
//! ```text
//! v1: muxr://pair?v=1&h=<host>&p=<port>&fp=<hex>&t=<base64url>&ro=<0|1>&n=<pct-encoded>
//! v2: muxr://pair?v=2&h=<host>&p=<port>&t=<base64url>&ro=<0|1>&n=<pct-encoded>&tm=<pin|ca>[&fp=<hex>]
//! ```
//!
//! - Scheme `muxr`, authority `pair`, both matched case-sensitively — the
//!   generator only ever emits this exact casing.
//! - `v` selects the grammar: `"1"` or `"2"`; anything else rejects.
//! - `h` must be non-empty; `p` must parse as `1..=65535`.
//! - `t` is base64url with **no** padding, decoded to raw bytes and then to a UTF-8
//!   string. [`URL_SAFE_NO_PAD`]'s `RequireNone` config enforces that half: a `t`
//!   carrying a trailing `=` is rejected by the decode itself, so this only ever
//!   rejects malformed/foreign input.
//! - `ro` is `true` only for the literal `"1"`; anything else, an absent key
//!   included, is `false` — never a reject case.
//! - `n` is the QR's human label, percent-decoded per RFC 3986 §2.3. It is
//!   required, and decoded bytes that aren't valid UTF-8 reject.
//! - v1 always requires `fp`, matching `^[0-9a-f]{64}$`, and trusts via
//!   [`PairingTrust::Pin`]. v2's `tm` selects the posture: `"pin"` requires `fp`
//!   under the same rule, `"ca"` ignores a present `fp` entirely; any other value,
//!   or a missing `tm`, rejects.
//! - Query-param order is arbitrary — [`split_query`] indexes by key. It is
//!   hand-rolled (no URL crate is a dependency): split on `&`, then each pair on
//!   its first `=`; a key with no `=` is dropped, and the grammar has no bare keys.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use percent_encoding::percent_decode_str;
use zeroize::Zeroizing;

use crate::features::connect::domain::entities::{PairingPayload, PairingTrust};

/// The lowest legal TCP port — mirrors `parse_endpoint::MIN_PORT`.
const MIN_PORT: u16 = 1;

/// Every pairing code's fixed scheme, including the `://` separator.
const SCHEME_PREFIX: &str = "muxr://";

/// The URI's one legal authority segment.
const AUTHORITY: &str = "pair";

/// Parse `raw` as a `muxr://pair` QR payload. See the [module docs](self) for
/// the full grammar and every rejection rule; every rejection is `None`,
/// never a panic.
pub fn parse_pairing_uri(raw: &str) -> Option<PairingPayload> {
    let rest = raw.strip_prefix(SCHEME_PREFIX)?;
    let (authority, query) = rest.split_once('?')?;
    if authority != AUTHORITY {
        return None;
    }

    let pairs = split_query(query);

    let host = find_param(&pairs, "h")?;
    if host.is_empty() {
        return None;
    }
    let port: u16 = find_param(&pairs, "p")?.parse().ok()?;
    if port < MIN_PORT {
        return None;
    }
    let token = decode_token(find_param(&pairs, "t")?)?;
    let read_only = find_param(&pairs, "ro") == Some("1");
    let label = decode_label(find_param(&pairs, "n")?)?;
    let trust = decode_trust(find_param(&pairs, "v")?, &pairs)?;

    Some(PairingPayload {
        host: host.to_string(),
        port,
        token,
        read_only,
        label,
        trust,
    })
}

/// Split `query` into `(key, value)` pairs — see the [module docs](self)' last
/// grammar bullet for the exact rule.
fn split_query(query: &str) -> Vec<(&str, &str)> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .collect()
}

/// The first value for `key` in `pairs`. Query-param order is arbitrary per the
/// grammar, so this never depends on it; a repeated key (which the grammar never
/// has) resolves to its first occurrence.
fn find_param<'a>(pairs: &[(&'a str, &'a str)], key: &str) -> Option<&'a str> {
    pairs.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// Decode `raw` as base64url with no padding, then as UTF-8 — the auth token. A
/// padded `raw` rejects rather than decodes (the grammar's `t` rule).
fn decode_token(raw: &str) -> Option<Zeroizing<String>> {
    let bytes = URL_SAFE_NO_PAD.decode(raw).ok()?;
    let token = String::from_utf8(bytes).ok()?;
    Some(Zeroizing::new(token))
}

/// Percent-decode `raw` into the QR's human label. Malformed UTF-8 rejects rather
/// than lossily replacing — no rejection here is a best-effort guess.
fn decode_label(raw: &str) -> Option<String> {
    percent_decode_str(raw)
        .decode_utf8()
        .ok()
        .map(|label| label.into_owned())
}

/// Resolve `version` (`v`'s value) plus whatever else `pairs` carries into a
/// [`PairingTrust`], per the [module docs](self)' grammar section.
fn decode_trust(version: &str, pairs: &[(&str, &str)]) -> Option<PairingTrust> {
    match version {
        "1" => pinned_trust(pairs),
        "2" => match find_param(pairs, "tm")? {
            "pin" => pinned_trust(pairs),
            "ca" => Some(PairingTrust::Ca),
            _ => None,
        },
        _ => None,
    }
}

/// `fp` required, validated against `^[0-9a-f]{64}$`, wrapped as
/// [`PairingTrust::Pin`] — the shared tail of v1 and v2's `tm=pin`.
fn pinned_trust(pairs: &[(&str, &str)]) -> Option<PairingTrust> {
    let fp_hex = find_param(pairs, "fp")?;
    valid_fingerprint(fp_hex).then(|| PairingTrust::Pin {
        fp_hex: fp_hex.to_string(),
    })
}

/// `^[0-9a-f]{64}$`, hand-rolled — no `regex` dependency for one fixed-shape
/// check.
fn valid_fingerprint(fp: &str) -> bool {
    fp.len() == 64
        && fp
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `czNjcjN0LXRva2Vu` is `"s3cr3t-token"`, base64url-encoded with no
    /// padding — the token every happy-path fixture below carries.
    const TOKEN_B64: &str = "czNjcjN0LXRva2Vu";
    const TOKEN_PLAIN: &str = "s3cr3t-token";
    const FP: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn a_well_formed_v1_uri_parses() {
        let uri = format!(
            "muxr://pair?v=1&h=192.168.1.100&p=50051&fp={FP}&t={TOKEN_B64}&ro=1&n=Home%20Rig"
        );
        let payload = parse_pairing_uri(&uri).expect("well-formed v1 URI parses");
        assert_eq!(payload.host, "192.168.1.100");
        assert_eq!(payload.port, 50051);
        assert_eq!(payload.token.as_str(), TOKEN_PLAIN);
        assert!(payload.read_only);
        assert_eq!(payload.label, "Home Rig");
        assert_eq!(
            payload.trust,
            PairingTrust::Pin {
                fp_hex: FP.to_string()
            }
        );
    }

    #[test]
    fn a_well_formed_v2_pin_uri_parses() {
        let uri =
            format!("muxr://pair?v=2&h=10.0.0.5&p=443&t={TOKEN_B64}&ro=0&n=Office&tm=pin&fp={FP}");
        let payload = parse_pairing_uri(&uri).expect("well-formed v2 tm=pin URI parses");
        assert_eq!(payload.host, "10.0.0.5");
        assert_eq!(payload.port, 443);
        assert_eq!(payload.token.as_str(), TOKEN_PLAIN);
        assert!(!payload.read_only);
        assert_eq!(payload.label, "Office");
        assert_eq!(
            payload.trust,
            PairingTrust::Pin {
                fp_hex: FP.to_string()
            }
        );
    }

    #[test]
    fn a_well_formed_v2_ca_uri_parses_with_fp_absent() {
        let uri = format!("muxr://pair?v=2&h=rig.example.com&p=50051&t={TOKEN_B64}&n=Cloud&tm=ca");
        let payload = parse_pairing_uri(&uri).expect("well-formed v2 tm=ca URI (no fp) parses");
        assert_eq!(payload.trust, PairingTrust::Ca);
        // `ro` absent — defaults false.
        assert!(!payload.read_only);
    }

    #[test]
    fn a_well_formed_v2_ca_uri_silently_ignores_a_present_fp() {
        let uri = format!(
            "muxr://pair?v=2&h=rig.example.com&p=50051&t={TOKEN_B64}&n=Cloud&tm=ca&fp={FP}"
        );
        let payload =
            parse_pairing_uri(&uri).expect("well-formed v2 tm=ca URI (fp present) still parses");
        // The fingerprint is not carried on `Ca` even though the URI has one.
        assert_eq!(payload.trust, PairingTrust::Ca);
    }

    #[test]
    fn query_param_order_never_matters() {
        let in_order = format!(
            "muxr://pair?v=1&h=192.168.1.100&p=50051&fp={FP}&t={TOKEN_B64}&ro=1&n=Home%20Rig"
        );
        let shuffled = format!(
            "muxr://pair?n=Home%20Rig&ro=1&t={TOKEN_B64}&fp={FP}&p=50051&h=192.168.1.100&v=1"
        );
        assert_eq!(parse_pairing_uri(&in_order), parse_pairing_uri(&shuffled));
    }

    #[test]
    fn port_bounds_are_inclusive_one_to_65535() {
        let at = |port: &str| format!("muxr://pair?v=2&h=h&p={port}&t={TOKEN_B64}&n=n&tm=ca");
        assert!(
            parse_pairing_uri(&at("1")).is_some(),
            "port 1 is the lowest legal port"
        );
        assert!(
            parse_pairing_uri(&at("65535")).is_some(),
            "port 65535 is the highest legal port"
        );
        for bad in ["0", "65536", "-1", "abc", ""] {
            assert!(
                parse_pairing_uri(&at(bad)).is_none(),
                "port {bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn the_label_is_percent_decoded() {
        let uri = format!("muxr://pair?v=2&h=h&p=1&t={TOKEN_B64}&n=A%20B%2Fc%26d&tm=ca");
        let payload = parse_pairing_uri(&uri).expect("percent-encoded label parses");
        assert_eq!(payload.label, "A B/c&d");
    }

    #[test]
    fn a_padded_base64_token_is_rejected_not_tolerated() {
        // `"hi"` base64url-encoded WITH padding is `"aGk="`; the generator's
        // own no-pad form is `"aGk"`. See the module docs' padding-
        // tolerance note: `URL_SAFE_NO_PAD` rejects the padded form outright.
        let uri = "muxr://pair?v=2&h=h&p=1&t=aGk%3D&n=n&tm=ca";
        // `%3D` decodes the literal `=` through percent-encoding too, so
        // this exercises the raw, unencoded `=` form directly instead.
        let uri_raw_pad = "muxr://pair?v=2&h=h&p=1&t=aGk=&n=n&tm=ca";
        assert!(parse_pairing_uri(uri).is_none());
        assert!(parse_pairing_uri(uri_raw_pad).is_none());

        // The no-pad form of the same token parses cleanly, confirming the
        // rejection above is padding-specific, not a fixture typo.
        let uri_nopad = "muxr://pair?v=2&h=h&p=1&t=aGk&n=n&tm=ca";
        let payload = parse_pairing_uri(uri_nopad).expect("no-pad token parses");
        assert_eq!(payload.token.as_str(), "hi");
    }

    #[test]
    fn every_reject_rule_rejects() {
        let good =
            |v: &str, extra: &str| format!("muxr://pair?v={v}&h=h&p=1&t={TOKEN_B64}&n=n{extra}");
        let cases: Vec<String> = vec![
            // Wrong scheme.
            "http://pair?v=1&h=h&p=1&fp=x&t=t&n=n".to_string(),
            // Wrong authority.
            "muxr://nope?v=1&h=h&p=1&fp=x&t=t&n=n".to_string(),
            // No query at all.
            "muxr://pair".to_string(),
            // `v` missing.
            format!("muxr://pair?h=h&p=1&fp={FP}&t={TOKEN_B64}&n=n"),
            // `v` neither "1" nor "2".
            good("3", &format!("&fp={FP}")),
            // `h` missing.
            format!("muxr://pair?v=1&p=1&fp={FP}&t={TOKEN_B64}&n=n"),
            // `h` empty.
            format!("muxr://pair?v=1&h=&p=1&fp={FP}&t={TOKEN_B64}&n=n"),
            // `p` missing.
            format!("muxr://pair?v=1&h=h&fp={FP}&t={TOKEN_B64}&n=n"),
            // `p` unparseable.
            format!("muxr://pair?v=1&h=h&p=notaport&fp={FP}&t={TOKEN_B64}&n=n"),
            // `t` missing.
            format!("muxr://pair?v=1&h=h&p=1&fp={FP}&n=n"),
            // `t` not valid base64url (contains `+`/`/`, outside the
            // URL-safe alphabet).
            format!("muxr://pair?v=1&h=h&p=1&fp={FP}&t=not+valid/b64!&n=n"),
            // `n` missing entirely.
            format!("muxr://pair?v=1&h=h&p=1&fp={FP}&t={TOKEN_B64}"),
            // v1: `fp` missing.
            good("1", ""),
            // v1: `fp` too short.
            good("1", "&fp=0123abcd"),
            // v1: `fp` has an uppercase hex digit (regex is lowercase-only).
            good("1", &format!("&fp={}", "A".to_string() + &FP[1..])),
            // v1: `fp` has a non-hex character.
            good("1", &format!("&fp={}", "z".to_string() + &FP[1..])),
            // v2 tm=pin: `fp` missing.
            format!("muxr://pair?v=2&h=h&p=1&t={TOKEN_B64}&n=n&tm=pin"),
            // v2 tm=pin: `fp` malformed.
            format!("muxr://pair?v=2&h=h&p=1&t={TOKEN_B64}&n=n&tm=pin&fp=short"),
            // v2: `tm` unknown value.
            format!("muxr://pair?v=2&h=h&p=1&t={TOKEN_B64}&n=n&tm=bogus"),
            // v2: `tm` missing entirely.
            format!("muxr://pair?v=2&h=h&p=1&t={TOKEN_B64}&n=n"),
        ];
        for uri in cases {
            assert!(
                parse_pairing_uri(&uri).is_none(),
                "expected rejection, got Some for {uri:?}"
            );
        }
    }

    /// `muxr`'s pairing-QR payload shape, copied verbatim from the frust camera
    /// plugin's own QR decode-conformance fixture. Its `t` value
    /// is a 43-char string of plain letters chosen only to stress that plugin's
    /// pixel-decode path at the protocol's maximum field width — not itself
    /// produced by base64url-encoding real UTF-8 text.
    const PAYLOAD_DENSE: &str = "muxr://pair?v=2&h=192.168.1.100&p=50051&t=abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQ&fp=\
0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef&tm=pin&n=Test%20Server";

    #[test]
    fn the_camera_plugins_dense_conformance_fixture_is_rejected_on_its_non_utf8_token() {
        // Every other field is well-formed; `t`'s 43-char span decodes to bytes
        // that are not valid UTF-8, so this module's `t` rule rejects it — a fact
        // about the shared fixture, not a defect in either module.
        assert!(parse_pairing_uri(PAYLOAD_DENSE).is_none());
    }
}
