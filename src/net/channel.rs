//! Endpoint and [`Channel`] construction. A custom verifier means the `ClientTlsConfig` carries
//! only the server name, set explicitly: `muxrd`'s IP-SAN cert needs `ServerName::IpAddress`,
//! and an IPv6 literal must drop the URI's brackets.

use std::net::Ipv6Addr;
use std::time::Duration;

use tonic::transport::{Channel, ClientTlsConfig, Endpoint};

use crate::failure::AppFailure;
use crate::net::error;
use crate::net::tls::TrustConfig;

/// Parity with `muxrd`'s server setting.
pub const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(10);

/// Parity with `muxrd`'s server setting.
pub const KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(20);

/// Fails before any socket opens on an empty host or an unbuildable verifier.
pub fn endpoint(host: &str, port: u16, trust: &TrustConfig) -> Result<Endpoint, AppFailure> {
    let name = server_name(host);
    if name.is_empty() {
        return Err(AppFailure::Network("server host is empty".to_string()));
    }

    let verifier = trust.verifier()?;
    let uri = format!("https://{}", authority(host, port));

    Endpoint::from_shared(uri)
        .and_then(|endpoint| {
            endpoint.tls_config_with_verifier(
                ClientTlsConfig::new().domain_name(name.to_string()),
                verifier,
            )
        })
        .map(|endpoint| {
            endpoint
                .http2_keep_alive_interval(KEEP_ALIVE_INTERVAL)
                .keep_alive_timeout(KEEP_ALIVE_TIMEOUT)
                .keep_alive_while_idle(true)
        })
        .map_err(|err| error::from_transport(&format!("endpoint {name}:{port}"), &err))
}

pub async fn connect(host: &str, port: u16, trust: &TrustConfig) -> Result<Channel, AppFailure> {
    endpoint(host, port, trust)?
        .connect()
        .await
        .map_err(|err| error::from_transport(&format!("connect {host}:{port}"), &err))
}

fn authority(host: &str, port: u16) -> String {
    let name = server_name(host);
    if name.parse::<Ipv6Addr>().is_ok() {
        format!("[{name}]:{port}")
    } else {
        format!("{name}:{port}")
    }
}

/// `ServerName` parses `::1` but not `[::1]`.
fn server_name(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host)
}

#[cfg(test)]
mod tests {
    use rustls::pki_types::ServerName;

    use super::*;

    /// An IP-SAN certificate needs an IP-address server name, not a DNS one.
    #[test]
    fn ipv4_authority_and_server_name() {
        assert_eq!(authority("127.0.0.1", 7777), "127.0.0.1:7777");
        assert!(matches!(
            ServerName::try_from(server_name("127.0.0.1")).expect("parses"),
            ServerName::IpAddress(_)
        ));
    }

    #[test]
    fn ipv6_authority_brackets_but_server_name_does_not() {
        assert_eq!(authority("::1", 7777), "[::1]:7777");
        assert_eq!(authority("[::1]", 7777), "[::1]:7777");
        assert_eq!(server_name("[::1]"), "::1");
        assert!(matches!(
            ServerName::try_from(server_name("[::1]")).expect("parses"),
            ServerName::IpAddress(_)
        ));
    }

    #[test]
    fn dns_host_stays_a_dns_server_name() {
        assert_eq!(authority("muxr.example", 443), "muxr.example:443");
        assert!(matches!(
            ServerName::try_from(server_name("muxr.example")).expect("parses"),
            ServerName::DnsName(_)
        ));
    }

    #[test]
    fn endpoint_builds_for_every_trust_mode() {
        for trust in [
            TrustConfig::SystemCa,
            TrustConfig::InsecureAcceptAny,
            TrustConfig::PinnedSha256([3u8; 32]),
        ] {
            let endpoint = endpoint("127.0.0.1", 7777, &trust).expect("endpoint builds");
            assert_eq!(endpoint.uri().scheme_str(), Some("https"));
            assert_eq!(
                endpoint.uri().authority().map(|a| a.as_str()),
                Some("127.0.0.1:7777")
            );
        }
    }

    #[test]
    fn empty_host_is_rejected_before_dialing() {
        let failure = endpoint("", 7777, &TrustConfig::InsecureAcceptAny)
            .expect_err("an empty host has no endpoint");
        assert!(matches!(failure, AppFailure::Network(_)));
    }
}
