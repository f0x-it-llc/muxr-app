//! Muxr's closed failure vocabulary: one [`AppFailure`] enum for the whole
//! app, small enough not to need per-feature error types. Every use case
//! maps its errors into this vocabulary at the data-layer boundary;
//! presentation code matches it exhaustively, never string-matches it — no
//! catch-all variant.

use std::fmt;

use clean_signals::Failure;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppFailure {
    Network(String),
    Auth(String),
    Storage(String),
    Internal(String),
}

impl fmt::Display for AppFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppFailure::Network(msg) => write!(f, "network error: {msg}"),
            AppFailure::Auth(msg) => write!(f, "auth error: {msg}"),
            AppFailure::Storage(msg) => write!(f, "storage error: {msg}"),
            AppFailure::Internal(msg) => write!(f, "internal error: {msg}"),
        }
    }
}

impl Failure for AppFailure {
    fn user_message(&self) -> String {
        match self {
            AppFailure::Network(_) => "Connection problem — please try again.".to_string(),
            AppFailure::Auth(_) => "Sign-in problem — please reconnect and try again.".to_string(),
            AppFailure::Storage(_) => "Couldn't save your data — please try again.".to_string(),
            AppFailure::Internal(_) => "Something went wrong — please try again.".to_string(),
        }
    }

    fn is_retryable(&self) -> bool {
        matches!(self, AppFailure::Network(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_is_retryable_with_generic_user_message() {
        let f = AppFailure::Network("upstream unreachable".to_string());
        assert!(f.is_retryable());
        assert_eq!(f.user_message(), "Connection problem — please try again.");
    }

    #[test]
    fn auth_is_not_retryable_and_asks_to_reconnect() {
        let f = AppFailure::Auth("token expired".to_string());
        assert!(!f.is_retryable());
        assert_eq!(
            f.user_message(),
            "Sign-in problem — please reconnect and try again."
        );
    }

    #[test]
    fn storage_is_not_retryable() {
        let f = AppFailure::Storage("disk full".to_string());
        assert!(!f.is_retryable());
        assert_eq!(
            f.user_message(),
            "Couldn't save your data — please try again."
        );
    }

    #[test]
    fn internal_never_leaks_its_raw_message_to_the_user() {
        let f = AppFailure::Internal("unreachable match arm hit".to_string());
        assert!(!f.is_retryable());
        assert_eq!(f.user_message(), "Something went wrong — please try again.");
        // The raw detail still shows up in Display (for logs), just not user_message.
        assert!(f.to_string().contains("unreachable match arm hit"));
    }
}
