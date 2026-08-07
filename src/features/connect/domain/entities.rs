//! `connect` domain entities — the feature's plain-data types (framework-free,
//! per [`crate::features::connect::domain`]'s module doc).
//!
//! [`SavedServer`] is a `muxrd` endpoint the user has connected to before,
//! rendered by the Connect screen's saved-servers list
//! (`docs/redesign.html`'s `.saved-hdr`/`.saved-row` block) and tapped either to
//! refill the form or — when it carries a stored credential — to unlock and
//! reconnect in one gesture. [`PairingPayload`]/[`PairingTrust`] are the typed
//! result of scanning a `muxr://pair` QR code (`use_cases::parse_pairing`), the
//! shape `muxrctl`'s generator and the Flutter client's `PairingPayload.tryParse`
//! agree on. [`CredentialStatus`]/[`UnlockedCredential`]/[`StoreRequest`]/
//! [`UnlockRequest`] are the vocabulary of
//! [`super::repositories::CredentialService`], the biometric-gated secret store.
//!
//! # Two stores, one key
//!
//! A saved server's *metadata* (this type) and its *secret* live in two
//! different stores that share nothing but [`SavedServer::endpoint`]'s
//! `host:port` key: the list is plain device storage, the token is behind the
//! biometric gate. [`SavedServer::has_credential`]/[`SavedServer::token_name`]
//! are the only trace the metadata keeps of the secret — whether one exists and
//! what the server calls the token it belongs to, never the token itself.
//!
//! # Trust-mode reuse (a documented layering tension)
//!
//! [`TrustMode`] is **not** re-minted here: it is
//! [`crate::connection::TrustMode`], the same enum
//! [`crate::connection::ConnectTarget`] carries, so a saved row's posture flows
//! into a connect request with no mapping layer. `crate::connection` is an
//! app-level module beside `crate::failure` (the composition root's own
//! vocabulary), *not* a sibling feature — but it is also not on
//! `docs/ARCHITECTURE.md`'s literal allow-list for `domain` (which names only
//! "this feature's own `domain` + `crate::failure`"), and the module hosting the
//! enum does import `frust::` for its controller half. The enum itself is a
//! `Copy`, three-variant, dependency-free data type; duplicating it here plus a
//! `From` mapping would buy a stricter reading of the rule at the cost of two
//! ways to spell one concept. Reuse wins; the allow-list is the thing that
//! wants widening.

use std::time::{SystemTime, UNIX_EPOCH};

use zeroize::Zeroizing;

pub use crate::connection::TrustMode;

/// A `muxrd` endpoint the user has connected to before.
///
/// Persisted as one entry of the saved-server list (see
/// [`crate::features::connect::data::repositories::prefs_saved_servers`] for the
/// on-disk schema). [`Self::last_used_ms`] is an epoch timestamp rather than a
/// pre-formatted label: the store is the same list across restarts, so "2 hours
/// ago" is a fact about *now* and is derived at the presentation edge through
/// [`Self::last_used_label`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedServer {
    /// The host to dial (no scheme, no port).
    pub host: String,
    /// The port to dial.
    pub port: u16,
    /// The TLS posture this endpoint was last trusted under.
    pub trust_mode: TrustMode,
    /// A human label for the rig ([`crate::connection::ConnectTarget::label`]'s
    /// source).
    pub label: String,
    /// When this endpoint was last connected to, in milliseconds since the Unix
    /// epoch ([`epoch_millis`]). Also the list's sort key — most recent first.
    /// `0` for a row restored from a store that predates the field, which
    /// [`Self::last_used_label`] prints as never-used rather than as 1970.
    pub last_used_ms: i64,
    /// Whether the credential this endpoint last logged in with only grants
    /// read-only access — rendered as a badge on the row, and re-applied
    /// without a fresh login when reconnecting through a stored credential.
    pub read_only: bool,
    /// Whether a biometric-gated credential is stored for this endpoint. The
    /// metadata half of the two-store split (see the [module docs](self)): the
    /// secret itself never appears here.
    pub has_credential: bool,
    /// The name of the server-side token the stored credential holds, so
    /// deleting or rotating the credential can revoke it. `None` when no
    /// credential is stored, and also for a QR-paired one — `muxrctl` mints
    /// that token itself and this device never learns its name.
    pub token_name: Option<String>,
    /// The pinned certificate's SHA-256 as 64 lowercase hex characters, for an
    /// endpoint paired by QR. Required by (and only meaningful for)
    /// [`TrustMode::Pinned`]; `None` otherwise, so a weaker posture can never
    /// carry a stale pin.
    pub fingerprint: Option<String>,
}

impl SavedServer {
    /// `host:port`, the way both the row and
    /// [`crate::connection::ServerInfo::endpoint`] spell it. Also this row's
    /// identity for [`super::repositories::SavedServersRepository::remove`] and
    /// the key its secret is stored under
    /// ([`super::repositories::CredentialService`]) — a host/port pair is
    /// unique per saved server.
    pub fn endpoint(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// [`Self::last_used_ms`] as the row's meta line prints it, relative to
    /// `now_ms` — "just now", "2 hours ago", "yesterday", "last week".
    ///
    /// Deliberately coarse: the row is a recency cue, not a log. A stamp in the
    /// future (a device clock that moved backwards) reads as "just now" rather
    /// than as a negative age.
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

/// What [`SavedServer::last_used_label`] prints for a row that has never
/// recorded a login (a legacy entry stored before the stamp existed).
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

/// Milliseconds since the Unix epoch — what [`SavedServer::last_used_ms`]
/// records, and the one clock read this feature makes.
///
/// `std::time` rather than a date crate: muxr does no date arithmetic beyond
/// [`SavedServer::last_used_label`]'s subtraction, and the app's manifest
/// carries no date dependency. A clock before the epoch (unset device time)
/// yields `0`, which reads as never-used rather than as a negative stamp.
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

/// How a biometric-gated credential operation ended.
///
/// One closed enum for both directions of
/// [`super::repositories::CredentialService`]: every plugin error the gate or
/// the store can raise maps onto exactly one of these, so nothing above this
/// layer ever matches on a `frust_secure_storage` type. The Flutter client's
/// `UnlockStatus` is the same table plus this app's own
/// [`Self::KeyInvalidated`] arm, which its `local_auth` gate had no counterpart
/// for.
///
/// [`Self::NotFound`] is **unlock-only** by construction: a store is what
/// produces "the gate passed but there is nothing there", and a save cannot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialStatus {
    /// The gate passed and the store answered.
    Success,
    /// The user (or the system) dismissed the prompt. Silent by contract — a
    /// cancel is an answer, not a failure.
    Canceled,
    /// The device has biometric hardware but nothing enrolled.
    NotEnrolled,
    /// The device has no PIN/pattern/passcode set at all, so neither the
    /// biometric nor its device-credential fallback can run.
    NoDeviceCredential,
    /// Too many failed attempts — biometrics are locked out for now.
    LockedOut,
    /// No usable biometric on this device or this build (desktop included).
    Unavailable,
    /// A biometric re-enrollment invalidated the store's key: whatever was
    /// stored can never be read again, so the caller clears the metadata that
    /// claims a credential exists.
    KeyInvalidated,
    /// The gate passed but the store held nothing for this server.
    NotFound,
    /// Authentication ran and did not succeed, or the store itself failed.
    Failed,
}

impl CredentialStatus {
    /// Whether this is the one outcome that leaves the caller nothing to say.
    ///
    /// A cancel is deliberately silent — the user dismissed a prompt they
    /// raised, and answering it with a message would be scolding them.
    pub fn is_silent(self) -> bool {
        matches!(self, CredentialStatus::Canceled)
    }

    /// The user-facing sentence this outcome is reported with, or `None` for
    /// the two that report nothing ([`Self::Success`] and [`Self::Canceled`]).
    ///
    /// Transcribed from the Flutter client's own eight-branch table
    /// (`connect_controller.dart`'s `recoverAndConnect`), plus this app's
    /// [`Self::KeyInvalidated`] arm. Every one of them ends by pointing at the
    /// manual form, because that is the only path left once the gate refuses:
    /// there is no plain-storage fallback anywhere in this feature.
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

/// What an unlock produced: its [`CredentialStatus`] and, on
/// [`CredentialStatus::Success`], the recovered auth token.
///
/// The token is [`Zeroizing`] and this type's [`std::fmt::Debug`] redacts it,
/// for the reason [`PairingPayload`]'s own does: a derived impl forwards
/// straight through `Zeroizing` and prints the secret in the clear.
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
    /// The raw auth token to store, and the only secret this feature ever
    /// writes to disk.
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
/// [`super::use_cases::parse_pairing::parse_pairing_uri`].
///
/// Not constructed directly outside that parser or tests — every field here
/// already passed the wire grammar's own validation (non-empty host, a legal
/// port, a decodable token, a well-formed trust posture), so a caller
/// downstream of the parser never re-checks them.
#[derive(Clone, PartialEq, Eq)]
pub struct PairingPayload {
    /// The `muxrd` host to dial.
    pub host: String,
    /// The port to dial.
    pub port: u16,
    /// The bearer auth token the QR encodes, base64url-decoded to its raw
    /// UTF-8 form. [`Zeroizing`]-wrapped for the same reason the Connect
    /// form's own login-token field is (`presentation::controller`'s
    /// `ConnectController::token` doc): a drop or overwrite scrubs the
    /// buffer instead of leaving stale plaintext parked in a freed
    /// allocation. This type's own [`Debug`] impl (below) is the other half
    /// of that hygiene — a derived one would forward straight through
    /// `Zeroizing`'s own `Debug` and print the token in the clear.
    pub token: Zeroizing<String>,
    /// Whether the credential the token names only grants read-only access.
    pub read_only: bool,
    /// The rig's human label — the QR's `n` field, percent-decoded.
    pub label: String,
    /// The TLS posture this payload asks the receiving client to trust the
    /// server under.
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

/// The TLS posture a [`PairingPayload`] asks the receiving client to trust
/// the server under.
///
/// A separate type from [`TrustMode`] rather than a reuse of it (contrast the
/// module docs' Trust-mode-reuse note, above): `TrustMode` is the connect
/// *form*'s three-way posture, including `System`, which the wire grammar
/// never emits, and it is `Copy` with no payload — a QR-encoded pin carries
/// its fingerprint **as data**, which `TrustMode::Pinned` has nowhere to
/// hold. Whoever maps a scanned payload onto a connect request (a later,
/// out-of-scope task) picks the matching `TrustMode` arm and threads `Pin`'s
/// `fp_hex` into the actual certificate-pinning path directly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PairingTrust {
    /// Pin the server's certificate to this SHA-256 fingerprint — 64
    /// lowercase hex characters, the DER-encoded leaf's digest (see
    /// `Cargo.toml`'s `sha2` dependency comment).
    Pin {
        /// The pinned fingerprint. Already validated against
        /// `^[0-9a-f]{64}$` by the parser that produced it.
        fp_hex: String,
    },
    /// Trust the server's certificate through the platform's system CA
    /// store — no fingerprint carried or checked.
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
            trust_mode: TrustMode::TrustOnFirstUse,
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
        // Sanity, not precision: the app's own recency labels are meaningless
        // if this returns 0 (an unset clock) on a normal machine.
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
