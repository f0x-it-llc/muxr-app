//! Endpoint and [`Channel`] construction for a `(host, port, TrustConfig)`
//! triple.
//!
//! Trust is installed via `Endpoint::tls_config_with_verifier` rather than a
//! custom connector: one route covers all three hand-built `rustls`
//! verifiers on the connector that already owns ALPN/handshake-timeout/
//! keepalive below. A custom verifier replaces the default, so the
//! `ClientTlsConfig` here carries nothing but the server name — mixing in a
//! CA/trust-anchor/root method beside it is an error.
//!
//! Server names are set explicitly rather than left to the URI host: `muxrd`
//! serves an IP-SAN self-signed cert, so rustls must match a
//! `ServerName::IpAddress`. `domain_name` gets that for a bare address and is
//! also the only way an IPv6 literal works (the URI authority needs
//! brackets; `ServerName` refuses them).
//!
//! Keepalive below mirrors `muxrd`'s own settings — parity, not tuning.

use std::net::Ipv6Addr;
use std::time::Duration;

use tonic::transport::{Channel, ClientTlsConfig, Endpoint};

use crate::failure::AppFailure;
use crate::net::error;
use crate::net::tls::TrustConfig;

/// HTTP/2 keepalive ping interval — parity with `muxrd`'s server setting.
pub const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(10);

/// How long a keepalive ping may go unanswered before the connection is
/// considered dead — parity with `muxrd`'s server setting.
pub const KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(20);

/// Build the endpoint for a target, TLS and keepalive already configured.
///
/// Fallible before any socket is opened: an empty/unparseable host, a
/// [`TrustConfig`] whose verifier cannot be built, or a URI tonic rejects all
/// surface here rather than as a connect failure.
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

/// Dial a target and return its connected channel.
pub async fn connect(host: &str, port: u16, trust: &TrustConfig) -> Result<Channel, AppFailure> {
    endpoint(host, port, trust)?
        .connect()
        .await
        .map_err(|err| error::from_transport(&format!("connect {host}:{port}"), &err))
}

/// The URI authority for a target: an IPv6 literal is bracketed, everything else
/// is passed through.
fn authority(host: &str, port: u16) -> String {
    let name = server_name(host);
    if name.parse::<Ipv6Addr>().is_ok() {
        format!("[{name}]:{port}")
    } else {
        format!("{name}:{port}")
    }
}

/// The bare name TLS verifies against — the host with any URI brackets removed,
/// since `ServerName` parses `::1` but not `[::1]`.
fn server_name(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host)
}

#[cfg(test)]
mod tests {
    use rustls::pki_types::ServerName;

    use super::*;

    /// A bare IPv4 host keeps its shape, and the name TLS matches on parses as
    /// an IP address rather than a DNS name — the form an IP-SAN certificate
    /// needs.
    #[test]
    fn ipv4_authority_and_server_name() {
        assert_eq!(authority("127.0.0.1", 7777), "127.0.0.1:7777");
        assert!(matches!(
            ServerName::try_from(server_name("127.0.0.1")).expect("parses"),
            ServerName::IpAddress(_)
        ));
    }

    /// An IPv6 literal is bracketed in the authority and unbracketed for TLS,
    /// whichever way the caller spelled it.
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

    /// A DNS host is untouched and still resolves to a DNS server name.
    #[test]
    fn dns_host_stays_a_dns_server_name() {
        assert_eq!(authority("muxr.example", 443), "muxr.example:443");
        assert!(matches!(
            ServerName::try_from(server_name("muxr.example")).expect("parses"),
            ServerName::DnsName(_)
        ));
    }

    /// Every trust mode builds an endpoint, and the URI it dials is `https`.
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

    /// An empty host is rejected here, not as a connect timeout later.
    #[test]
    fn empty_host_is_rejected_before_dialing() {
        let failure = endpoint("", 7777, &TrustConfig::InsecureAcceptAny)
            .expect_err("an empty host has no endpoint");
        assert!(matches!(failure, AppFailure::Network(_)));
    }
}
