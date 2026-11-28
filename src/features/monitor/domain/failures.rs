//! The monitor's failure texts and the pure reading of them. Every refusal is an
//! `AppFailure::Internal` carrying one of these, so none reads as a sign-in problem and only a
//! transport failure stays retryable.

use crate::failure::AppFailure;

pub const MONITORING_OFF: &str = "host monitoring is switched off on this server";

pub const READ_ONLY: &str = "this server token is read-only and cannot signal processes";

pub const INVALID_REQUEST: &str = "the server rejected the request as invalid";

pub const SIGNAL_REFUSED: &str = "the server refused the signal";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MonitorFailureKind {
    MonitoringOff,
    ReadOnly,
    /// The server's own reason for refusing the signal.
    Refused(String),
    InvalidRequest,
    Other,
}

pub fn monitoring_off() -> AppFailure {
    AppFailure::Internal(MONITORING_OFF.to_string())
}

pub fn read_only(detail: &str) -> AppFailure {
    AppFailure::Internal(format!("{READ_ONLY}: {detail}"))
}

pub fn invalid_request(detail: &str) -> AppFailure {
    AppFailure::Internal(format!("{INVALID_REQUEST}: {detail}"))
}

pub fn signal_refused(reason: &str) -> AppFailure {
    AppFailure::Internal(format!("{SIGNAL_REFUSED}: {reason}"))
}

pub fn monitor_failure_kind(failure: &AppFailure) -> MonitorFailureKind {
    let AppFailure::Internal(text) = failure else {
        return MonitorFailureKind::Other;
    };
    if text == MONITORING_OFF {
        MonitorFailureKind::MonitoringOff
    } else if text.starts_with(READ_ONLY) {
        MonitorFailureKind::ReadOnly
    } else if text.starts_with(INVALID_REQUEST) {
        MonitorFailureKind::InvalidRequest
    } else if let Some(reason) = text.strip_prefix(SIGNAL_REFUSED) {
        MonitorFailureKind::Refused(reason.trim_start_matches(": ").to_string())
    } else {
        MonitorFailureKind::Other
    }
}

#[cfg(test)]
mod tests {
    use clean_signals::Failure;

    use super::*;

    #[test]
    fn each_constructor_reads_back_as_its_kind() {
        assert_eq!(
            monitor_failure_kind(&monitoring_off()),
            MonitorFailureKind::MonitoringOff
        );
        assert_eq!(
            monitor_failure_kind(&read_only("token")),
            MonitorFailureKind::ReadOnly
        );
        assert_eq!(
            monitor_failure_kind(&invalid_request("pid 0")),
            MonitorFailureKind::InvalidRequest
        );
        assert_eq!(
            monitor_failure_kind(&signal_refused("no such process")),
            MonitorFailureKind::Refused("no such process".to_string())
        );
    }

    #[test]
    fn anything_else_is_other() {
        for failure in [
            AppFailure::Network("down".to_string()),
            AppFailure::Auth("expired".to_string()),
            AppFailure::Internal("boom".to_string()),
        ] {
            assert_eq!(monitor_failure_kind(&failure), MonitorFailureKind::Other);
        }
    }

    #[test]
    fn refusals_are_never_retryable_or_auth() {
        for failure in [
            monitoring_off(),
            read_only("token"),
            invalid_request("pid 0"),
            signal_refused("denied"),
        ] {
            assert!(!failure.is_retryable());
            assert!(!matches!(failure, AppFailure::Auth(_)));
        }
    }
}
