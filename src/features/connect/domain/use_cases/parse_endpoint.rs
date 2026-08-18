//! [`parse_endpoint`] — the `host:port` validation behind the connect CTA.
//!
//! A plain function rather than a `clean_signals::UseCase`: this is a pure
//! synchronous parse with no I/O, nothing to cancel — wrapping it would buy an
//! `await` and a pump-poll in every test, and no behavior.
//!
//! Mock-grade: no DNS, no IDN, no IPv6 bracket syntax, no scheme stripping —
//! just a non-empty host, a `:`, and a port in `1..=65535`.

use crate::failure::AppFailure;

/// The lowest legal TCP port — `0` is "any port" to the OS, never a dial target.
const MIN_PORT: u16 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
}

/// Parse `raw` as `host:port`.
///
/// Documented gap: every rejection maps to [`AppFailure::Network`], since
/// `AppFailure` has no validation variant and `Internal` means a bug, not a
/// typo. The internal detail (never shown to the user) names the offending
/// input so a log tells the whole story.
pub fn parse_endpoint(raw: &str) -> Result<Endpoint, AppFailure> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(AppFailure::Network("server url is empty".to_string()));
    }
    // `rsplit_once`, not `split_once`: a future IPv6 literal has colons of its
    // own, and the port is always the last one.
    let Some((host, port)) = raw.rsplit_once(':') else {
        return Err(AppFailure::Network(format!(
            "server url {raw:?} has no ':port'"
        )));
    };
    let host = host.trim();
    if host.is_empty() {
        return Err(AppFailure::Network(format!(
            "server url {raw:?} has an empty host"
        )));
    }
    let port: u16 = port
        .trim()
        .parse()
        .map_err(|_| AppFailure::Network(format!("server url {raw:?} has an unparseable port")))?;
    if port < MIN_PORT {
        return Err(AppFailure::Network(format!(
            "server url {raw:?} dials port 0"
        )));
    }
    Ok(Endpoint {
        host: host.to_string(),
        port,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_well_formed_host_and_port_parses() {
        assert_eq!(
            parse_endpoint("zelli.example.com:50051"),
            Ok(Endpoint {
                host: "zelli.example.com".to_string(),
                port: 50051,
            })
        );
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        assert_eq!(
            parse_endpoint("  100.71.31.57 : 50051  "),
            Ok(Endpoint {
                host: "100.71.31.57".to_string(),
                port: 50051,
            })
        );
    }

    #[test]
    fn every_malformed_input_is_rejected() {
        for raw in [
            "",
            "   ",
            "zelli.example.com",
            ":50051",
            "zelli.example.com:",
            "zelli.example.com:hello",
            "zelli.example.com:0",
            "zelli.example.com:99999",
        ] {
            assert!(
                parse_endpoint(raw).is_err(),
                "{raw:?} must not parse as a dial target"
            );
        }
    }

    #[test]
    fn a_rejection_never_leaks_its_detail_to_the_user() {
        use clean_signals::Failure;

        let failure = parse_endpoint("nope").expect_err("a host with no port is rejected");
        assert!(!failure.user_message().contains("nope"));
        // The log-facing Display does carry it.
        assert!(failure.to_string().contains("nope"));
    }
}
