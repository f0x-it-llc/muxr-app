//! The `src/net` transport surface, exercised from outside the crate.
//!
//! This suite covers the contracts that hold with no server on the other end: fail-closed pin
//! parsing, endpoint/trust-mode construction (system-CA included — construction only, since a
//! real CA chain needs a real CA), the keepalive parity constants, the `Status`/`ActionAck`
//! failure mapping, and one genuine dial against a closed port.
//!
//! The handshake and credential round trips — right pin connects, wrong pin does not, `Login`
//! mints a token an authenticated RPC accepts — run against the in-process fixture in
//! `src/net/testing.rs`, and live in `src/net`'s own unit tests: that fixture needs `rcgen` and
//! tonic's server features, `[dev-dependencies]` unreachable from the library build an
//! integration test links.

use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use clean_signals::Failure;
use muxr::failure::AppFailure;
use muxr::net::auth::{AUTHORIZATION_KEY, BearerInterceptor, SessionToken};
use muxr::net::channel::{KEEP_ALIVE_INTERVAL, KEEP_ALIVE_TIMEOUT, endpoint};
use muxr::net::error::{ack_result, from_status};
use muxr::net::proto::ActionAck;
use muxr::net::tls::{PIN_HEX_LEN, PIN_LEN, leaf_fingerprint_hex};
use muxr::net::{MuxrClient, PinError, TrustConfig};
use tonic::service::Interceptor;
use tonic::{Request, Status};

/// A fingerprint printed by this crate is a pin this crate accepts — the whole
/// pairing path in one assertion.
#[test]
fn a_printed_fingerprint_parses_back_into_a_pin() {
    let der = b"a leaf certificate's DER bytes";
    let hex = leaf_fingerprint_hex(der);

    assert_eq!(hex.len(), PIN_HEX_LEN);
    assert!(
        hex.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    );

    let pin = TrustConfig::pinned_from_hex(&hex).expect("its own fingerprint parses");
    assert!(matches!(pin, TrustConfig::PinnedSha256(_)));
    // A pairing payload may arrive upper-cased; both spellings are one pin.
    assert_eq!(
        TrustConfig::pinned_from_hex(&hex.to_uppercase()).expect("uppercase parses"),
        pin
    );
}

/// Fail-closed: every malformed pin is an error, and it is raised before any
/// socket exists — there is no lenient path that quietly weakens trust.
#[test]
fn a_malformed_pin_is_rejected_before_dialing() {
    assert_eq!(
        TrustConfig::pinned_from_hex(""),
        Err(PinError::WrongLength(0))
    );
    assert_eq!(
        TrustConfig::pinned_from_hex(&"0".repeat(PIN_HEX_LEN - 2)),
        Err(PinError::WrongLength(PIN_HEX_LEN - 2))
    );
    assert_eq!(
        TrustConfig::pinned_from_hex(&"0".repeat(PIN_HEX_LEN + 2)),
        Err(PinError::WrongLength(PIN_HEX_LEN + 2))
    );

    let mut not_hex = "0".repeat(PIN_HEX_LEN - 1);
    not_hex.push('g');
    assert_eq!(
        TrustConfig::pinned_from_hex(&not_hex),
        Err(PinError::NotHex)
    );

    // The rejection reaches a caller as an app failure with a readable detail,
    // not as a silent fallback to a weaker mode.
    let failure = AppFailure::from(PinError::NotHex);
    assert!(matches!(failure, AppFailure::Network(_)));
    assert!(failure.to_string().contains("hexadecimal"));
}

/// All three trust modes build an endpoint on this host, system-CA included.
#[test]
fn every_trust_mode_builds_an_endpoint() {
    for trust in [
        TrustConfig::SystemCa,
        TrustConfig::InsecureAcceptAny,
        TrustConfig::PinnedSha256([0x5A; PIN_LEN]),
    ] {
        let built = endpoint("127.0.0.1", 7777, &trust)
            .unwrap_or_else(|err| panic!("{trust:?} built no endpoint: {err}"));
        assert_eq!(built.uri().scheme_str(), Some("https"));
    }
}

/// A host that cannot become an authority fails at construction, not as a
/// connect timeout minutes later.
#[test]
fn an_unusable_host_fails_at_construction() {
    for host in ["", "[]", "not a host"] {
        assert!(
            endpoint(host, 7777, &TrustConfig::InsecureAcceptAny).is_err(),
            "{host:?} should not build an endpoint"
        );
    }
}

/// Keepalive parity with `muxrd`: these constants are a contract, not tuning.
#[test]
fn keepalive_constants_match_the_server() {
    assert_eq!(KEEP_ALIVE_INTERVAL, Duration::from_secs(10));
    assert_eq!(KEEP_ALIVE_TIMEOUT, Duration::from_secs(20));
}

/// `UNAUTHENTICATED` is the one status that becomes a non-retryable auth
/// failure; the transport codes stay retryable; everything else falls through
/// to the network bucket.
#[test]
fn status_codes_map_into_the_app_failure_vocabulary() {
    let auth = from_status(&Status::unauthenticated("session token expired"));
    assert!(matches!(auth, AppFailure::Auth(_)));
    assert!(!auth.is_retryable());
    assert_eq!(
        auth.user_message(),
        "Sign-in problem — please reconnect and try again."
    );

    for status in [
        Status::unavailable("connection refused"),
        Status::deadline_exceeded("no response in time"),
    ] {
        let failure = from_status(&status);
        assert!(matches!(failure, AppFailure::Network(_)), "{status:?}");
        assert!(failure.is_retryable(), "{status:?}");
    }

    for status in [
        Status::permission_denied("read-only token"),
        Status::internal("relay panicked"),
    ] {
        assert!(
            matches!(from_status(&status), AppFailure::Network(_)),
            "{status:?}"
        );
    }
}

/// A server-side action refusal is a failure even though the RPC succeeded.
#[test]
fn a_refused_action_ack_is_a_failure() {
    assert!(
        ack_result(&ActionAck {
            ok: true,
            error: String::new(),
            info: "terminal_2".to_string(),
        })
        .is_ok()
    );

    let failure = ack_result(&ActionAck {
        ok: false,
        error: "tab 4 does not exist".to_string(),
        info: String::new(),
    })
    .expect_err("ok == false is a failure");
    assert!(failure.to_string().contains("tab 4 does not exist"));
    // Detail is for logs; the user sees the generic message.
    assert_eq!(
        failure.user_message(),
        "Connection problem — please try again."
    );
}

/// The bearer credential is visible on the wire exactly as `muxrd` parses it,
/// and a rotation reaches an interceptor built before it.
#[test]
fn the_bearer_header_is_stamped_and_rotatable() {
    let token = Arc::new(SessionToken::new());
    let mut interceptor = BearerInterceptor::new(Arc::clone(&token));

    // No credential: the call never leaves the process.
    assert_eq!(
        interceptor
            .call(Request::new(()))
            .expect_err("no token, no request")
            .code(),
        tonic::Code::Unauthenticated
    );

    token.set("session-one");
    token.set("session-two");
    let request = interceptor.call(Request::new(())).expect("a token stamps");
    assert_eq!(
        request
            .metadata()
            .get(AUTHORIZATION_KEY)
            .expect("authorization is present")
            .to_str()
            .expect("ascii"),
        "Bearer session-two"
    );

    token.clear();
    assert!(!token.is_set());
    assert!(interceptor.call(Request::new(())).is_err());
}

/// A real dial against a port nothing is listening on surfaces as a retryable
/// network failure — the transport error path, end to end.
#[tokio::test]
async fn a_dial_to_a_closed_port_is_a_retryable_network_failure() {
    // Bind then drop, so the port is known-free rather than guessed.
    let port = TcpListener::bind("127.0.0.1:0")
        .expect("binds a loopback port")
        .local_addr()
        .expect("a bound listener has an address")
        .port();

    let failure = MuxrClient::connect("127.0.0.1", port, TrustConfig::InsecureAcceptAny)
        .await
        .expect_err("nothing is listening");
    assert!(matches!(failure, AppFailure::Network(_)));
    assert!(failure.is_retryable());
}
