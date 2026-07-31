//! [`parse_endpoint`] — the `host:port` validation behind the connect CTA.
//!
//! # Why a free function rather than a `clean_signals::UseCase`
//!
//! `examples/huddle`'s use cases implement `clean_signals::UseCase`, whose
//! `execute` is `async` (it exists so `ControllerCore::run` can own the in-flight
//! /retry/failure lifecycle). This is a **pure, synchronous, infallible-to-run**
//! parse with no I/O, no latency and nothing to cancel: wrapping it in an async
//! trait would buy an `await`, a `spawn_local` and a pump-poll in every test, and
//! no behavior. It stays a plain function in `use_cases/` — same layer, same
//! `<verb><noun>` file naming, none of the machinery it doesn't need. The first
//! `connect` operation that actually does I/O (the real `muxrd` dial) is what
//! earns the `UseCase` shape.
//!
//! # Mock-grade
//!
//! No DNS, no IDN, no IPv6 bracket syntax, no scheme stripping — the rules are
//! exactly what the Connect form's placeholder promises
//! (`zelli.example.com:50051`): a non-empty host, a `:`, and a port in
//! `1..=65535`.

use crate::failure::AppFailure;

/// The lowest legal TCP port — `0` is "any port" to the OS, never a dial target.
const MIN_PORT: u16 = 1;

/// A parsed `host:port` pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    /// The host half, trimmed.
    pub host: String,
    /// The port half.
    pub port: u16,
}

/// Parse `raw` as `host:port`.
///
/// # Failure vocabulary (a documented gap)
///
/// Every rejection maps to [`AppFailure::Network`], whose user message is
/// "Connection problem — please try again." `AppFailure` (`crate::failure`, one
/// closed enum for the whole app) has **no validation variant**, and `Internal`
/// — the only other plausible arm — is documented as "a bug, an invariant
/// violation", which a typo in a text field is not. `Network` is the closest
/// honest fit: the user typed something muxr cannot reach. A `Validation` arm on
/// `AppFailure` is the real fix; it belongs to whoever owns `crate::failure`.
///
/// The internal detail (never shown — see `crate::failure`) names the offending
/// input so a log tells the whole story.
pub fn parse_endpoint(raw: &str) -> Result<Endpoint, AppFailure> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(AppFailure::Network("server url is empty".to_string()));
    }
    // `rsplit_once` rather than `split_once`: a future IPv6 literal has colons
    // of its own, and the port is always the last one.
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
        // …but the log-facing Display does carry it.
        assert!(failure.to_string().contains("nope"));
    }
}
