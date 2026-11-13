//! Decodes a scanned `muxr://pair` QR code. Every rejection is `None`. Scheme and authority match case-sensitively; query order is arbitrary.
//! v1: `muxr://pair?v=1&h=<host>&p=<port>&fp=<hex>&t=<base64url>&ro=<0|1>&n=<pct-encoded>`; v2 adds `tm=<pin|ca>` (`pin` needs `fp`; `ca` ignores it). `t` has no padding; `fp` is `^[0-9a-f]{64}$`.
//! `p` is `1..=65535`; `ro` is true only for `1`; `n` is required and percent-decoded.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use percent_encoding::percent_decode_str;
use zeroize::Zeroizing;

use crate::features::connect::domain::entities::{PairingPayload, PairingTrust};

const MIN_PORT: u16 = 1;

const SCHEME_PREFIX: &str = "muxr://";

const AUTHORITY: &str = "pair";

/// Every rejection is `None`, never a panic.
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

fn split_query(query: &str) -> Vec<(&str, &str)> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .collect()
}

fn find_param<'a>(pairs: &[(&'a str, &'a str)], key: &str) -> Option<&'a str> {
    pairs.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

fn decode_token(raw: &str) -> Option<Zeroizing<String>> {
    let bytes = URL_SAFE_NO_PAD.decode(raw).ok()?;
    let token = String::from_utf8(bytes).ok()?;
    Some(Zeroizing::new(token))
}

fn decode_label(raw: &str) -> Option<String> {
    percent_decode_str(raw)
        .decode_utf8()
        .ok()
        .map(|label| label.into_owned())
}

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

fn pinned_trust(pairs: &[(&str, &str)]) -> Option<PairingTrust> {
    let fp_hex = find_param(pairs, "fp")?;
    valid_fingerprint(fp_hex).then(|| PairingTrust::Pin {
        fp_hex: fp_hex.to_string(),
    })
}

/// Hand-rolled `^[0-9a-f]{64}$`.
fn valid_fingerprint(fp: &str) -> bool {
    fp.len() == 64
        && fp
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `czNjcjN0LXRva2Vu` is `"s3cr3t-token"`, base64url-encoded with no padding.
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
        assert!(!payload.read_only);
    }

    #[test]
    fn a_well_formed_v2_ca_uri_silently_ignores_a_present_fp() {
        let uri = format!(
            "muxr://pair?v=2&h=rig.example.com&p=50051&t={TOKEN_B64}&n=Cloud&tm=ca&fp={FP}"
        );
        let payload =
            parse_pairing_uri(&uri).expect("well-formed v2 tm=ca URI (fp present) still parses");
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
        // `"hi"` base64url-encoded with padding is `"aGk="`; without, `"aGk"`.
        let uri = "muxr://pair?v=2&h=h&p=1&t=aGk%3D&n=n&tm=ca";
        let uri_raw_pad = "muxr://pair?v=2&h=h&p=1&t=aGk=&n=n&tm=ca";
        assert!(parse_pairing_uri(uri).is_none());
        assert!(parse_pairing_uri(uri_raw_pad).is_none());

        let uri_nopad = "muxr://pair?v=2&h=h&p=1&t=aGk&n=n&tm=ca";
        let payload = parse_pairing_uri(uri_nopad).expect("no-pad token parses");
        assert_eq!(payload.token.as_str(), "hi");
    }

    #[test]
    fn every_reject_rule_rejects() {
        let good =
            |v: &str, extra: &str| format!("muxr://pair?v={v}&h=h&p=1&t={TOKEN_B64}&n=n{extra}");
        let cases: Vec<String> = vec![
            "http://pair?v=1&h=h&p=1&fp=x&t=t&n=n".to_string(),
            "muxr://nope?v=1&h=h&p=1&fp=x&t=t&n=n".to_string(),
            "muxr://pair".to_string(),
            format!("muxr://pair?h=h&p=1&fp={FP}&t={TOKEN_B64}&n=n"),
            good("3", &format!("&fp={FP}")),
            format!("muxr://pair?v=1&p=1&fp={FP}&t={TOKEN_B64}&n=n"),
            format!("muxr://pair?v=1&h=&p=1&fp={FP}&t={TOKEN_B64}&n=n"),
            format!("muxr://pair?v=1&h=h&fp={FP}&t={TOKEN_B64}&n=n"),
            format!("muxr://pair?v=1&h=h&p=notaport&fp={FP}&t={TOKEN_B64}&n=n"),
            format!("muxr://pair?v=1&h=h&p=1&fp={FP}&n=n"),
            format!("muxr://pair?v=1&h=h&p=1&fp={FP}&t=not+valid/b64!&n=n"),
            format!("muxr://pair?v=1&h=h&p=1&fp={FP}&t={TOKEN_B64}"),
            good("1", ""),
            good("1", "&fp=0123abcd"),
            good("1", &format!("&fp={}", "A".to_string() + &FP[1..])),
            good("1", &format!("&fp={}", "z".to_string() + &FP[1..])),
            format!("muxr://pair?v=2&h=h&p=1&t={TOKEN_B64}&n=n&tm=pin"),
            format!("muxr://pair?v=2&h=h&p=1&t={TOKEN_B64}&n=n&tm=pin&fp=short"),
            format!("muxr://pair?v=2&h=h&p=1&t={TOKEN_B64}&n=n&tm=bogus"),
            format!("muxr://pair?v=2&h=h&p=1&t={TOKEN_B64}&n=n"),
        ];
        for uri in cases {
            assert!(
                parse_pairing_uri(&uri).is_none(),
                "expected rejection, got Some for {uri:?}"
            );
        }
    }

    /// Copied from the camera plugin's QR conformance fixture; its `t` is not valid base64url UTF-8.
    const PAYLOAD_DENSE: &str = "muxr://pair?v=2&h=192.168.1.100&p=50051&t=abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQ&fp=\
0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef&tm=pin&n=Test%20Server";

    #[test]
    fn the_camera_plugins_dense_conformance_fixture_is_rejected_on_its_non_utf8_token() {
        assert!(parse_pairing_uri(PAYLOAD_DENSE).is_none());
    }
}
