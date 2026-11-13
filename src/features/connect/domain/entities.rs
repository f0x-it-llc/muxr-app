//! Plain-data types. A saved server's metadata and its secret share only the `host:port` endpoint.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use zeroize::Zeroizing;

use crate::ui::recency;

use super::keyring::Keyring;

pub use crate::connection::TrustMode;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedServer {
    pub host: String,
    pub port: u16,
    pub trust_mode: TrustMode,
    pub label: String,
    /// Epoch ms of last connect; the list's sort key. `0` for a legacy row reads as never-used.
    pub last_used_ms: i64,
    pub read_only: bool,
    pub has_credential: bool,
    /// `None` with no credential, or for a QR-paired one (`muxrctl` never sends the name).
    pub token_name: Option<String>,
    /// Pinned certificate SHA-256, 64 lowercase hex; `None` unless [`TrustMode::Pinned`].
    pub fingerprint: Option<String>,
}

impl SavedServer {
    pub fn endpoint(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    pub fn last_used_label(&self, now_ms: i64) -> String {
        if self.last_used_ms <= 0 {
            return NEVER_USED.to_string();
        }
        let elapsed = (now_ms - self.last_used_ms).max(0);
        recency::age_label(Duration::from_millis(elapsed.unsigned_abs()))
    }
}

const NEVER_USED: &str = "not used yet";

pub fn epoch_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

/// Shields callers from `frust_secure_storage`'s own type. [`Self::NotFound`] is unlock-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialStatus {
    Success,
    /// Dismissed by the user or system; silent by contract.
    Canceled,
    NotEnrolled,
    NoDeviceCredential,
    LockedOut,
    Unavailable,
    /// Re-enrollment invalidated the store's key; the data is unrecoverable.
    KeyInvalidated,
    NotFound,
    Failed,
}

impl CredentialStatus {
    pub fn is_silent(self) -> bool {
        matches!(self, CredentialStatus::Canceled)
    }

    pub fn message(self) -> Option<&'static str> {
        match self {
            CredentialStatus::Success | CredentialStatus::Canceled => None,
            CredentialStatus::NotEnrolled => {
                Some("No biometrics enrolled. Set up biometrics in device settings.")
            }
            CredentialStatus::NoDeviceCredential => {
                Some("No device PIN/pattern/password set. Please log in manually.")
            }
            CredentialStatus::LockedOut => {
                Some("Too many failed attempts. Try again later or log in manually.")
            }
            CredentialStatus::Unavailable => {
                Some("Biometrics are not available on this device. Please log in manually.")
            }
            CredentialStatus::KeyInvalidated => Some(
                "Biometric enrollment changed, so the saved credential is gone. \
                 Please log in manually.",
            ),
            CredentialStatus::NotFound => {
                Some("No saved credential found. Please log in manually.")
            }
            CredentialStatus::Failed => {
                Some("Biometric authentication failed. Please log in manually.")
            }
        }
    }
}

/// `Debug` redacts the token.
#[derive(Clone, PartialEq, Eq)]
pub struct UnlockedCredential {
    pub status: CredentialStatus,
    pub token: Option<Zeroizing<String>>,
}

impl UnlockedCredential {
    pub fn refused(status: CredentialStatus) -> Self {
        Self {
            status,
            token: None,
        }
    }

    pub fn recovered(token: Zeroizing<String>) -> Self {
        Self {
            status: CredentialStatus::Success,
            token: Some(token),
        }
    }
}

impl std::fmt::Debug for UnlockedCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnlockedCredential")
            .field("status", &self.status)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// `Debug` prints endpoints only.
pub struct UnlockedKeyring {
    pub status: CredentialStatus,
    pub keyring: Option<Keyring>,
    pub migrated: Vec<String>,
    pub unreadable: Vec<String>,
}

impl UnlockedKeyring {
    pub fn refused(status: CredentialStatus) -> Self {
        Self {
            status,
            keyring: None,
            migrated: Vec::new(),
            unreadable: Vec::new(),
        }
    }
}

impl std::fmt::Debug for UnlockedKeyring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnlockedKeyring")
            .field("status", &self.status)
            .field("keyring", &self.keyring)
            .field("migrated", &self.migrated)
            .field("unreadable", &self.unreadable)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct StoreRequest {
    pub server_key: String,
    pub token: Zeroizing<String>,
    pub reason: String,
}

impl std::fmt::Debug for StoreRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreRequest")
            .field("server_key", &self.server_key)
            .field("token", &"<redacted>")
            .field("reason", &self.reason)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnlockRequest {
    pub server_key: String,
    pub reason: String,
}

#[derive(Clone, PartialEq, Eq)]
pub struct PairingPayload {
    pub host: String,
    pub port: u16,
    /// Base64url-decoded and [`Zeroizing`]-wrapped; `Debug` redacts it.
    pub token: Zeroizing<String>,
    pub read_only: bool,
    pub label: String,
    pub trust: PairingTrust,
}

impl std::fmt::Debug for PairingPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingPayload")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("token", &"<redacted>")
            .field("read_only", &self.read_only)
            .field("label", &self.label)
            .field("trust", &self.trust)
            .finish()
    }
}

/// Separate from the `Copy` [`TrustMode`] because a QR pin carries its fingerprint as data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PairingTrust {
    Pin { fp_hex: String },
    Ca,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aged(age_ms: i64) -> SavedServer {
        SavedServer {
            host: "100.64.0.7".to_string(),
            port: 50051,
            trust_mode: TrustMode::NoVerification,
            label: "Home rig".to_string(),
            last_used_ms: NOW_MS - age_ms,
            read_only: false,
            has_credential: false,
            token_name: None,
            fingerprint: None,
        }
    }

    const NOW_MS: i64 = 1_800_000_000_000;
    const MINUTE: i64 = 60_000;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;

    #[test]
    fn an_endpoint_is_the_host_and_port_joined() {
        assert_eq!(aged(0).endpoint(), "100.64.0.7:50051");
    }

    #[test]
    fn the_recency_label_reads_the_mocks_own_three_spellings() {
        assert_eq!(aged(2 * HOUR).last_used_label(NOW_MS), "2 hours ago");
        assert_eq!(aged(30 * HOUR).last_used_label(NOW_MS), "yesterday");
        assert_eq!(aged(9 * DAY).last_used_label(NOW_MS), "last week");
    }

    #[test]
    fn the_recency_label_singularizes_and_floors() {
        assert_eq!(aged(0).last_used_label(NOW_MS), "just now");
        assert_eq!(aged(90 * 1_000).last_used_label(NOW_MS), "1 minute ago");
        assert_eq!(aged(HOUR + MINUTE).last_used_label(NOW_MS), "1 hour ago");
        assert_eq!(aged(3 * DAY).last_used_label(NOW_MS), "3 days ago");
        assert_eq!(aged(30 * DAY).last_used_label(NOW_MS), "4 weeks ago");
    }

    #[test]
    fn a_stampless_row_reads_as_never_used_rather_than_as_1970() {
        let mut legacy = aged(0);
        legacy.last_used_ms = 0;
        assert_eq!(legacy.last_used_label(NOW_MS), NEVER_USED);
    }

    #[test]
    fn a_stamp_from_the_future_never_reads_as_a_negative_age() {
        assert_eq!(aged(-HOUR).last_used_label(NOW_MS), "just now");
    }

    #[test]
    fn every_reportable_outcome_carries_a_message_and_the_two_silent_ones_do_not() {
        for status in [CredentialStatus::Success, CredentialStatus::Canceled] {
            assert_eq!(status.message(), None, "{status:?}");
        }
        for status in [
            CredentialStatus::NotEnrolled,
            CredentialStatus::NoDeviceCredential,
            CredentialStatus::LockedOut,
            CredentialStatus::Unavailable,
            CredentialStatus::KeyInvalidated,
            CredentialStatus::NotFound,
            CredentialStatus::Failed,
        ] {
            assert!(status.message().is_some(), "{status:?}");
            assert!(!status.is_silent(), "{status:?}");
        }
    }

    #[test]
    fn an_unlocked_credentials_debug_impl_never_prints_the_token() {
        let unlocked = UnlockedCredential::recovered(Zeroizing::new("super-secret".to_string()));
        let debug = format!("{unlocked:?}");
        assert!(!debug.contains("super-secret"), "{debug}");
        assert!(debug.contains("<redacted>"), "{debug}");
    }

    #[test]
    fn a_store_requests_debug_impl_never_prints_the_token() {
        let request = StoreRequest {
            server_key: "rig:50051".to_string(),
            token: Zeroizing::new("super-secret".to_string()),
            reason: "Save credentials for rig:50051".to_string(),
        };
        let debug = format!("{request:?}");
        assert!(!debug.contains("super-secret"), "{debug}");
        assert!(debug.contains("<redacted>"), "{debug}");
    }

    #[test]
    fn the_epoch_clock_reads_a_plausible_present() {
        assert!(epoch_millis() > 1_700_000_000_000, "{}", epoch_millis());
    }

    #[test]
    fn a_pairing_payloads_debug_impl_never_prints_the_token() {
        let payload = PairingPayload {
            host: "192.168.1.100".to_string(),
            port: 50051,
            token: Zeroizing::new("super-secret-bearer-token".to_string()),
            read_only: false,
            label: "Test Server".to_string(),
            trust: PairingTrust::Ca,
        };
        let debug = format!("{payload:?}");
        assert!(!debug.contains("super-secret-bearer-token"));
        assert!(debug.contains("<redacted>"));
    }
}
