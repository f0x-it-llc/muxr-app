//! `connect` domain entities — the feature's plain-data types (framework-free,
//! per [`crate::features::connect::domain`]'s module doc).
//!
//! [`SavedServer`] is a `muxrd` endpoint the user has connected to before,
//! rendered by the Connect screen's saved-servers list
//! (`docs/redesign.html`'s `.saved-hdr`/`.saved-row` block) and tapped to refill
//! the form. [`PairingPayload`]/[`PairingTrust`] are the typed result of
//! scanning a `muxr://pair` QR code (`use_cases::parse_pairing`), the shape
//! `muxrctl`'s generator and the Flutter client's `PairingPayload.tryParse`
//! agree on.
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

use zeroize::Zeroizing;

pub use crate::connection::TrustMode;

/// A `muxrd` endpoint the user has connected to before.
///
/// Mock-era: nothing persists these yet (see
/// [`crate::features::connect::data::repositories`]), and `last_used` is a
/// pre-formatted human label rather than a timestamp — muxr has no clock
/// abstraction, and a `String` keeps this layer free of a date crate. A real
/// persistence layer swaps it for an instant and formats at the presentation
/// edge.
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
    /// When this endpoint was last connected to, pre-formatted (see the type
    /// docs' mock-era note).
    pub last_used: String,
    /// Whether the stored credential only grants read-only access — rendered as
    /// a badge on the row, and a real constraint once sessions can be mutated.
    pub read_only: bool,
}

impl SavedServer {
    /// `host:port`, the way both the row and
    /// [`crate::connection::ServerInfo::endpoint`] spell it. Also this row's
    /// identity for [`super::repositories::SavedServersRepository::remove`] —
    /// a host/port pair is unique per saved server.
    pub fn endpoint(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
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

    #[test]
    fn an_endpoint_is_the_host_and_port_joined() {
        let server = SavedServer {
            host: "100.71.31.57".to_string(),
            port: 50051,
            trust_mode: TrustMode::TrustOnFirstUse,
            label: "Home rig".to_string(),
            last_used: "2 hours ago".to_string(),
            read_only: false,
        };
        assert_eq!(server.endpoint(), "100.71.31.57:50051");
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
