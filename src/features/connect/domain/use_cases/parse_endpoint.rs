//! The `host:port` validation behind the connect CTA. No DNS, IDN, IPv6 brackets or scheme stripping.

use crate::failure::AppFailure;

const MIN_PORT: u16 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
}

/// A trimmed `raw` with no `:` is a bare host dialing `default_port`; otherwise the port must be in `1..=65535`.
/// Every rejection maps to [`AppFailure::Network`]; the internal detail names the input.
pub fn parse_endpoint(raw: &str, default_port: u16) -> Result<Endpoint, AppFailure> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(AppFailure::Network("server url is empty".to_string()));
    }
    if !raw.contains(':') {
        return Ok(Endpoint {
            host: raw.to_string(),
            port: default_port,
        });
    }
    // `rsplit_once`: an IPv6 literal has colons of its own; the port is the last one.
    let Some((host, port)) = raw.rsplit_once(':') else {
        unreachable!("the `contains(':')` check above guarantees a match");
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

    const UNUSED_DEFAULT: u16 = 50051;

    #[test]
    fn a_well_formed_host_and_port_parses() {
        assert_eq!(
            parse_endpoint("zelli.example.com:50051", UNUSED_DEFAULT),
            Ok(Endpoint {
                host: "zelli.example.com".to_string(),
                port: 50051,
            })
        );
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        assert_eq!(
            parse_endpoint("  100.64.0.7 : 50051  ", UNUSED_DEFAULT),
            Ok(Endpoint {
                host: "100.64.0.7".to_string(),
                port: 50051,
            })
        );
    }

    #[test]
    fn a_bare_host_parses_to_the_callers_default_port() {
        assert_eq!(
            parse_endpoint("demo.muxr.app", 443),
            Ok(Endpoint {
                host: "demo.muxr.app".to_string(),
                port: 443,
            })
        );
    }

    #[test]
    fn a_bare_hosts_surrounding_whitespace_is_trimmed() {
        assert_eq!(
            parse_endpoint("  demo.muxr.app  ", 443),
            Ok(Endpoint {
                host: "demo.muxr.app".to_string(),
                port: 443,
            })
        );
    }

    #[test]
    fn every_malformed_input_is_rejected() {
        for raw in [
            "",
            "   ",
            ":50051",
            "zelli.example.com:",
            "zelli.example.com:hello",
            "zelli.example.com:0",
            "zelli.example.com:99999",
        ] {
            assert!(
                parse_endpoint(raw, UNUSED_DEFAULT).is_err(),
                "{raw:?} must not parse as a dial target"
            );
        }
    }

    #[test]
    fn a_rejection_never_leaks_its_detail_to_the_user() {
        use clean_signals::Failure;

        let failure = parse_endpoint("nope:oops", UNUSED_DEFAULT)
            .expect_err("an unparseable port is rejected");
        assert!(!failure.user_message().contains("nope"));
        assert!(failure.to_string().contains("nope"));
    }
}
