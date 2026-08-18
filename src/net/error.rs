//! The one boundary that turns a gRPC outcome into [`AppFailure`].
//!
//! Everything below `src/net` speaks `tonic::Status`/`tonic::transport::Error`;
//! nothing above it does — a repository maps here and hands its caller the
//! app's closed vocabulary.
//!
//! Only `UNAUTHENTICATED` becomes [`AppFailure::Auth`] (the one code a user
//! resolves by reconnecting); `UNAVAILABLE`/`DEADLINE_EXCEEDED` and every
//! other code become [`AppFailure::Network`]. That collapse is deliberate
//! and lossy — e.g. a `PERMISSION_DENIED` from a read-only token currently
//! reads as retryable — but splitting it needs a new `AppFailure` variant,
//! which is the failure vocabulary's call, not this module's.
//!
//! Detail strings are for logs only; `AppFailure::user_message()` never
//! includes them.

use tonic::{Code, Status};

use crate::failure::AppFailure;
use crate::net::proto::ActionAck;

/// Map a gRPC status onto the app's failure vocabulary.
pub fn from_status(status: &Status) -> AppFailure {
    let detail = format!("{:?}: {}", status.code(), status.message());
    match status.code() {
        Code::Unauthenticated => AppFailure::Auth(detail),
        // Retryable transport codes; everything else collapses here too (see
        // module docs).
        Code::Unavailable | Code::DeadlineExceeded => AppFailure::Network(detail),
        _ => AppFailure::Network(detail),
    }
}

/// Map a transport-level failure (endpoint construction, TLS handshake, dial)
/// onto [`AppFailure::Network`].
///
/// `context` names what was being attempted; it is joined into the detail string
/// so a log line says which target failed, never a bare "transport error".
pub fn from_transport(context: &str, error: &tonic::transport::Error) -> AppFailure {
    AppFailure::Network(format!("{context}: {error}"))
}

/// Turn `muxrd`'s generic action acknowledgement into a `Result`.
///
/// An `ActionAck` is not an error at the gRPC layer — the RPC succeeded and the
/// *action* did not — so a caller that only checks `Status` silently treats a
/// refused pane close as a success. Every `ActionAck`-returning call routes
/// through here instead.
pub fn ack_result(ack: &ActionAck) -> Result<(), AppFailure> {
    if ack.ok {
        return Ok(());
    }
    let detail = if ack.error.is_empty() {
        "server rejected the action without a reason".to_string()
    } else {
        ack.error.clone()
    };
    Err(AppFailure::Network(detail))
}

#[cfg(test)]
mod tests {
    use clean_signals::Failure;

    use super::*;

    /// Only `UNAUTHENTICATED` becomes an auth failure, and an auth failure is
    /// never retryable — retrying a rejected token just fails again.
    #[test]
    fn unauthenticated_maps_to_a_non_retryable_auth_failure() {
        let failure = from_status(&Status::unauthenticated("session token expired"));
        assert!(matches!(failure, AppFailure::Auth(_)));
        assert!(!failure.is_retryable());
        // The raw server detail stays in Display for logs, never in the toast.
        assert!(failure.to_string().contains("session token expired"));
        assert_eq!(
            failure.user_message(),
            "Sign-in problem — please reconnect and try again."
        );
    }

    /// The two transport codes map to a retryable network failure.
    #[test]
    fn unavailable_and_deadline_exceeded_are_retryable_network_failures() {
        for status in [
            Status::unavailable("connection refused"),
            Status::deadline_exceeded("no response in time"),
        ] {
            let failure = from_status(&status);
            assert!(matches!(failure, AppFailure::Network(_)), "{status:?}");
            assert!(failure.is_retryable(), "{status:?}");
        }
    }

    /// Every other code lands on the network bucket rather than leaking a
    /// server string into `Internal`'s "something went wrong" path.
    #[test]
    fn other_codes_fall_through_to_network() {
        for status in [
            Status::permission_denied("read-only token"),
            Status::internal("relay panicked"),
            Status::not_found("no such session"),
            Status::invalid_argument("empty pane id"),
        ] {
            assert!(
                matches!(from_status(&status), AppFailure::Network(_)),
                "{status:?}"
            );
        }
    }

    /// A successful ack is `Ok`; a refused one carries the server's own reason.
    #[test]
    fn ack_result_distinguishes_ok_from_refused() {
        let ok = ActionAck {
            ok: true,
            error: String::new(),
            info: "terminal_3".to_string(),
        };
        assert!(ack_result(&ok).is_ok());

        let refused = ActionAck {
            ok: false,
            error: "pane 3 is already closed".to_string(),
            info: String::new(),
        };
        let failure = ack_result(&refused).expect_err("a refused action is a failure");
        assert!(failure.to_string().contains("pane 3 is already closed"));
    }

    /// A refused ack with no message still fails, with a stand-in reason rather
    /// than an empty detail string.
    #[test]
    fn ack_result_fails_even_without_a_reason() {
        let refused = ActionAck {
            ok: false,
            error: String::new(),
            info: String::new(),
        };
        let failure = ack_result(&refused).expect_err("ok == false is a failure");
        assert!(!failure.to_string().is_empty());
    }
}
