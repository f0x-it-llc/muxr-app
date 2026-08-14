//! `connect` domain entities — the feature's plain-data types, framework-free.
//!
//! **Two stores, one key.** A saved server's *metadata* ([`SavedServer`]) and its
//! *secret* live in two stores sharing nothing but [`SavedServer::endpoint`]'s
//! `host:port` key. [`SavedServer::has_credential`]/[`SavedServer::token_name`] are
//! the only trace the metadata keeps of the secret — whether one exists and what
//! the server calls its token, never the token itself.
//!
//! **Trust-mode reuse, a documented layering tension.** [`TrustMode`] is not
//! re-minted here: it is [`crate::connection::TrustMode`], the same enum
//! [`crate::connection::ConnectTarget`] carries, so a saved row's posture flows
//! into a connect request with no mapping layer. `crate::connection` is an
//! app-level module rather than a sibling feature, but it is not on
//! `docs/ARCHITECTURE.md`'s literal `domain` allow-list either; duplicating a
//! `Copy`, three-variant, dependency-free enum plus a `From` mapping would buy a
//! stricter reading of the rule at the cost of two ways to spell one concept.

use std::time::{SystemTime, UNIX_EPOCH};

use zeroize::Zeroizing;

pub use crate::connection::TrustMode;

/// A `muxrd` endpoint the user has connected to before, one entry of the
/// saved-server list. [`Self::last_used_ms`] is an epoch timestamp rather than a
/// label: the store outlives restarts, so "2 hours ago" is a fact about *now* and
/// is derived at the presentation edge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedServer {
    /// The host to dial (no scheme, no port).
    pub host: String,
    /// The port to dial.
    pub port: u16,
    /// The TLS posture this endpoint was last trusted under.
    pub trust_mode: TrustMode,
    /// A human label for the rig.
    pub label: String,
    /// When this endpoint was last connected to ([`epoch_millis`]), and the list's
    /// sort key — most recent first. `0` for a row from a store that predates the
    /// field, which prints as never-used rather than as 1970.
    pub last_used_ms: i64,
    /// Whether the credential this endpoint last logged in with is read-only —
    /// rendered as a badge, and re-applied when reconnecting through a stored one.
    pub read_only: bool,
    /// Whether a gated credential is stored for this endpoint — the metadata half
    /// of the two-store split; the secret itself never appears here.
    pub has_credential: bool,
    /// The server-side token's name, so deleting or rotating the credential can
    /// revoke it. `None` with no credential stored, and for a QR-paired one —
    /// `muxrctl` mints that token and this device never learns its name.
    pub token_name: Option<String>,
    /// The pinned certificate's SHA-256 as 64 lowercase hex characters. Only
    /// meaningful for [`TrustMode::Pinned`]; `None` otherwise, so a weaker posture
    /// can never carry a stale pin.
    pub fingerprint: Option<String>,
}

impl SavedServer {
    /// `host:port` — this row's identity for
    /// [`SavedServersRepository::remove`](super::repositories::SavedServersRepository::remove)
    /// and the key its secret is stored under.
    pub fn endpoint(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// [`Self::last_used_ms`] as the row's meta line prints it, relative to `now_ms`
    /// — deliberately coarse, since the row is a recency cue rather than a log. A
    /// stamp in the future reads as "just now", never as a negative age.
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

/// What [`SavedServer::last_used_label`] prints for a row that has never recorded
/// a login (a legacy entry stored before the stamp existed).
const NEVER_USED: &str = "not used yet";

/// Milliseconds in a minute — [`SavedServer::last_used_label`]'s coarsest unit.
const MS_PER_MINUTE: i64 = 60_000;

/// `"1 hour ago"` / `"3 hours ago"` — the one place the plural `s` is decided.
fn plural(count: i64, unit: &str) -> String {
    if count == 1 {
        format!("1 {unit} ago")
    } else {
        format!("{count} {unit}s ago")
    }
}

/// Milliseconds since the Unix epoch — the one clock read this feature makes. An
/// unset device clock yields `0`, which reads as never-used.
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

/// How a biometric-gated credential operation ended — one closed enum for both
/// directions of [`super::repositories::CredentialService`], so nothing above this
/// layer ever matches on a `frust_secure_storage` type. [`Self::NotFound`] is
/// **unlock-only** by construction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialStatus {
    /// The gate passed and the store answered.
    Success,
    /// The user (or the system) dismissed the prompt. Silent by contract — a
    /// cancel is an answer, not a failure.
    Canceled,
    /// The device has biometric hardware but nothing enrolled.
    NotEnrolled,
    /// No PIN/pattern/passcode is set at all, so neither the biometric nor its
    /// device-credential fallback can run.
    NoDeviceCredential,
    /// Too many failed attempts — biometrics are locked out for now.
    LockedOut,
    /// No usable biometric on this device or this build (desktop included).
    Unavailable,
    /// A re-enrollment invalidated the store's key: what was stored can never be
    /// read again, so the caller clears the metadata claiming it exists.
    KeyInvalidated,
    /// The gate passed but the store held nothing for this server.
    NotFound,
    /// Authentication ran and did not succeed, or the store itself failed.
    Failed,
}

impl CredentialStatus {
    /// Whether this outcome leaves the caller nothing to say — a cancel is silent,
    /// since the user dismissed a prompt they raised.
    pub fn is_silent(self) -> bool {
        matches!(self, CredentialStatus::Canceled)
    }

    /// The user-facing sentence this outcome is reported with, or `None` for the two
    /// that report nothing. Every message points at the manual form, the only path
    /// left once the gate refuses.
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

/// What an unlock produced: its [`CredentialStatus`] and, on success, the recovered
/// token. The token is [`Zeroizing`] and this type's [`std::fmt::Debug`] redacts it
/// — a derived impl would print the secret in the clear.
#[derive(Clone, PartialEq, Eq)]
pub struct UnlockedCredential {
    /// How the unlock ended.
    pub status: CredentialStatus,
    /// The recovered token — `Some` exactly when [`Self::status`] is
    /// [`CredentialStatus::Success`].
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
    /// The raw auth token to store, and the only secret this feature ever writes
    /// to disk.
    pub token: Zeroizing<String>,
    /// The rationale the system prompt shows. **Never** carries credential
    /// material — the platform renders this to the user.
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

/// A `muxr://pair` QR payload, decoded and validated by
/// [`super::use_cases::parse_pairing::parse_pairing_uri`]. Not constructed outside
/// that parser or tests: every field here already passed the wire grammar's own
/// validation, so a caller downstream never re-checks it.
#[derive(Clone, PartialEq, Eq)]
pub struct PairingPayload {
    /// The `muxrd` host to dial.
    pub host: String,
    /// The port to dial.
    pub port: u16,
    /// The bearer auth token the QR encodes, base64url-decoded. [`Zeroizing`]-
    /// wrapped, with this type's redacting [`Debug`] as the other half.
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

/// The TLS posture a [`PairingPayload`] asks the client to trust the server under.
/// A separate type from [`TrustMode`], which is the *form*'s three-way posture and
/// is `Copy` with no payload — a QR-encoded pin carries its fingerprint **as
/// data**, which `TrustMode::Pinned` has nowhere to hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PairingTrust {
    /// Pin the server's certificate to this SHA-256 fingerprint — the DER-encoded
    /// leaf's digest, 64 lowercase hex characters.
    Pin {
        /// The pinned fingerprint. Already validated against `^[0-9a-f]{64}$` by
        /// the parser that produced it.
        fp_hex: String,
    },
    /// Trust the platform's system CA store — no fingerprint carried or checked.
    Ca,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A row `age_ms` old relative to [`NOW_MS`].
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

    /// A fixed "now" the age cases are measured against.
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
        // What a legacy entry (stored before the stamp existed) decodes to.
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
        // Sanity, not precision: the app's own recency labels are meaningless if
        // this returns 0 (an unset clock) on a normal machine.
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
