//! `connect` domain entities — the feature's plain-data types, framework-free.
//!
//! Two stores, one key: a saved server's metadata ([`SavedServer`]) and its
//! secret share nothing but [`SavedServer::endpoint`]'s `host:port`.
//! [`SavedServer::has_credential`]/[`SavedServer::token_name`] are the only
//! trace the metadata keeps of the secret.
//!
//! [`TrustMode`] is reused from [`crate::connection::TrustMode`] rather than
//! re-minted, so a saved row's posture flows into a connect request with no
//! mapping layer.

use std::time::{SystemTime, UNIX_EPOCH};

use zeroize::Zeroizing;

pub use crate::connection::TrustMode;

/// A `muxrd` endpoint the user has connected to before. [`Self::last_used_ms`]
/// is an epoch timestamp, not a label, since the store outlives restarts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedServer {
    pub host: String,
    pub port: u16,
    /// TLS posture last trusted under this endpoint.
    pub trust_mode: TrustMode,
    pub label: String,
    /// Epoch ms of last connect ([`epoch_millis`]); the list's sort key (recent
    /// first). `0` for a legacy row (predates this field) — reads as
    /// never-used, not 1970.
    pub last_used_ms: i64,
    /// Read-only posture of the last credential used here; shown as a badge and
    /// re-applied on reconnect.
    pub read_only: bool,
    /// Whether a gated credential is stored for this endpoint; the secret
    /// itself never appears here.
    pub has_credential: bool,
    /// The stored token's name (for revoke-on-delete). `None` with no
    /// credential, or for a QR-paired one — `muxrctl` never tells this device
    /// the name.
    pub token_name: Option<String>,
    /// Pinned certificate SHA-256, 64 lowercase hex. Meaningful only for
    /// [`TrustMode::Pinned`]; otherwise `None`, so a weaker posture never
    /// carries a stale pin.
    pub fingerprint: Option<String>,
}

impl SavedServer {
    /// `host:port` — this row's identity and its secret's storage key.
    pub fn endpoint(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// Coarse recency label relative to `now_ms` — a future stamp reads as
    /// "just now", never negative.
    pub fn last_used_label(&self, now_ms: i64) -> String {
        if self.last_used_ms <= 0 {
            return NEVER_USED.to_string();
        }
        let elapsed = (now_ms - self.last_used_ms).max(0);
        let minutes = elapsed / MS_PER_MINUTE;
        if minutes < 1 {
            return "just now".to_string();
        }
        let hours = minutes / 60;
        if hours < 1 {
            return plural(minutes, "minute");
        }
        let days = hours / 24;
        if days < 1 {
            return plural(hours, "hour");
        }
        if days < 2 {
            return "yesterday".to_string();
        }
        let weeks = days / 7;
        if weeks < 1 {
            return plural(days, "day");
        }
        if weeks < 2 {
            return "last week".to_string();
        }
        plural(weeks, "week")
    }
}

/// Printed for a row that has never recorded a login.
const NEVER_USED: &str = "not used yet";

const MS_PER_MINUTE: i64 = 60_000;

/// `"1 hour ago"` / `"3 hours ago"` — the one place the plural `s` is decided.
fn plural(count: i64, unit: &str) -> String {
    if count == 1 {
        format!("1 {unit} ago")
    } else {
        format!("{count} {unit}s ago")
    }
}

/// Ms since Unix epoch; an unset clock yields `0`, read as never-used.
pub fn epoch_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// The credential vocabulary
// ---------------------------------------------------------------------------

/// Outcome of a gated credential operation; shields callers from
/// `frust_secure_storage`'s own type. [`Self::NotFound`] is unlock-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialStatus {
    /// The gate passed and the store answered.
    Success,
    /// Dismissed by the user or system. Silent by contract — not a failure.
    Canceled,
    /// The device has biometric hardware but nothing enrolled.
    NotEnrolled,
    /// No PIN/pattern/passcode set, so neither biometric nor its fallback can run.
    NoDeviceCredential,
    /// Too many failed attempts — biometrics are locked out for now.
    LockedOut,
    /// No usable biometric on this device or this build (desktop included).
    Unavailable,
    /// Re-enrollment invalidated the store's key — stored data is
    /// unrecoverable, so the caller clears the metadata claiming it exists.
    KeyInvalidated,
    /// Gate passed but nothing was stored for this server.
    NotFound,
    /// Authentication ran and did not succeed, or the store itself failed.
    Failed,
}

impl CredentialStatus {
    /// True only for [`CredentialStatus::Canceled`] — dismissing your own
    /// prompt needs no message.
    pub fn is_silent(self) -> bool {
        matches!(self, CredentialStatus::Canceled)
    }

    /// User-facing message, or `None` for the two silent outcomes; every
    /// message points at the manual form.
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

/// Unlock result: status plus, on success, the recovered token. `Debug`
/// redacts the token (a derived impl would print the secret).
#[derive(Clone, PartialEq, Eq)]
pub struct UnlockedCredential {
    pub status: CredentialStatus,
    /// `Some` exactly when [`Self::status`] is [`CredentialStatus::Success`].
    pub token: Option<Zeroizing<String>>,
}

impl UnlockedCredential {
    /// A non-success outcome, carrying no token.
    pub fn refused(status: CredentialStatus) -> Self {
        Self {
            status,
            token: None,
        }
    }

    /// A recovered token.
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

/// [`super::repositories::CredentialService::store`]'s parameters.
#[derive(Clone, PartialEq, Eq)]
pub struct StoreRequest {
    /// The server this secret belongs to — [`SavedServer::endpoint`].
    pub server_key: String,
    /// The raw token to store — the only secret this feature ever writes to disk.
    pub token: Zeroizing<String>,
    /// Shown in the system prompt. Never carries credential material.
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

/// [`super::repositories::CredentialService::unlock`]'s parameters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnlockRequest {
    /// The server whose secret to recover — [`SavedServer::endpoint`].
    pub server_key: String,
    /// The rationale the system prompt shows. See [`StoreRequest::reason`].
    pub reason: String,
}

/// A decoded, validated `muxr://pair` payload (see
/// [`super::use_cases::parse_pairing::parse_pairing_uri`]) — every field
/// already passed the wire grammar, so callers never re-check it.
#[derive(Clone, PartialEq, Eq)]
pub struct PairingPayload {
    pub host: String,
    pub port: u16,
    /// Bearer token, base64url-decoded and [`Zeroizing`]-wrapped; this type's
    /// `Debug` redacts it.
    pub token: Zeroizing<String>,
    /// Whether the credential the token names only grants read-only access.
    pub read_only: bool,
    /// The rig's human label — the QR's `n` field, percent-decoded.
    pub label: String,
    /// The TLS posture this payload asks the receiving client to trust the server
    /// under.
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

/// TLS posture a [`PairingPayload`] asks the client to trust under. Separate
/// from [`TrustMode`] (which is `Copy`, no payload) because a QR pin carries
/// its fingerprint as data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PairingTrust {
    /// Pin to this SHA-256 fingerprint of the DER-encoded leaf (64 lowercase hex chars).
    Pin {
        /// Already validated against `^[0-9a-f]{64}$` by the parser.
        fp_hex: String,
    },
    /// Trust the platform's system CA store — no fingerprint carried or checked.
    Ca,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aged(age_ms: i64) -> SavedServer {
        SavedServer {
            host: "100.71.31.57".to_string(),
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
        assert_eq!(aged(0).endpoint(), "100.71.31.57:50051");
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
        // Sanity only — an unset clock yields 0, meaningless for recency labels.
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
