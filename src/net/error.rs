//! The one boundary turning a gRPC outcome into [`AppFailure`]. Only `UNAUTHENTICATED` becomes
//! [`AppFailure::Auth`]; every other code collapses, lossily, into [`AppFailure::Network`].
//! Detail strings are for logs only.

use tonic::{Code, Status};

use crate::failure::AppFailure;
use crate::net::proto::ActionAck;

pub fn from_status(status: &Status) -> AppFailure {
    let detail = format!("{:?}: {}", status.code(), status.message());
    match status.code() {
        Code::Unauthenticated => AppFailure::Auth(detail),
        // Retryable transport codes; every other code collapses here too.
        Code::Unavailable | Code::DeadlineExceeded => AppFailure::Network(detail),
        _ => AppFailure::Network(detail),
    }
}

/// `context` names the attempt so a log line says which target failed.
pub fn from_transport(context: &str, error: &tonic::transport::Error) -> AppFailure {
    AppFailure::Network(format!("{context}: {error}"))
}

/// The RPC can succeed while the action is refused, so every `ActionAck` call routes here.
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

    #[test]
    fn unauthenticated_maps_to_a_non_retryable_auth_failure() {
        let failure = from_status(&Status::unauthenticated("session token expired"));
        assert!(matches!(failure, AppFailure::Auth(_)));
        assert!(!failure.is_retryable());
        assert!(failure.to_string().contains("session token expired"));
        assert_eq!(
            failure.user_message(),
            "Sign-in problem — please reconnect and try again."
        );
    }

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
