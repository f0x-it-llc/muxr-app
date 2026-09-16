//! The three trust modes a `muxrd` connection can be dialled under, and the
//! `rustls` [`ServerCertVerifier`] each one installs.
//!
//! [`TrustConfig`] is separate from `crate::connection::TrustMode` (a
//! screen's plain-data posture label) — mapping between them is the connect
//! seam's job, not this module's.
//!
//! A pin is over the leaf certificate's DER bytes, not its SPKI — the `fp`
//! form a pairing payload encodes. Parsing is fail-closed: a missing, short,
//! long or non-hex pin errors before a socket opens, never downgrades to a
//! weaker mode.
//!
//! Pinning replaces chain and name validation, not proof-of-possession: the
//! pinned verifier still runs `verify_tls12/13_signature` through rustls'
//! own helpers, so presenting the pinned cert without its private key still
//! fails the handshake. Skipping those two methods — the shape most "danger"
//! verifier snippets copy — would accept a replayed certificate.

use std::fmt;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, Error as RustlsError, SignatureScheme};
use sha2::{Digest, Sha256};

use crate::failure::AppFailure;

/// Width of a certificate pin, in bytes — SHA-256's digest length.
pub const PIN_LEN: usize = 32;

/// Width of a certificate pin written as hex — the exact length
/// [`TrustConfig::pinned_from_hex`] accepts, no shorter and no longer.
pub const PIN_HEX_LEN: usize = PIN_LEN * 2;

/// Lowercase hex alphabet for [`leaf_fingerprint_hex`].
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// How a dialled server's certificate is verified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrustConfig {
    /// A publicly trusted certificate chain. On Apple targets the platform's
    /// own trust store validates chain and name (`rustls-platform-verifier`
    /// over Security.framework). On Android, rustls' WebPKI verifier over the
    /// bundled Mozilla root program (`webpki-roots`) does instead — see
    /// [`system_ca_verifier`] for why the platform verifier is not used there.
    SystemCa,
    /// Accept any certificate. Dev servers only — this is the one mode with no
    /// authentication of the peer at all.
    InsecureAcceptAny,
    /// Accept exactly the leaf certificate whose DER bytes hash to this digest.
    PinnedSha256([u8; PIN_LEN]),
}

/// Why a hex pin was rejected — always *before* a connection is attempted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PinError {
    /// The pin was not exactly [`PIN_HEX_LEN`] characters long.
    WrongLength(usize),
    /// The pin was the right length but held a non-hex character.
    NotHex,
}

impl fmt::Display for PinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PinError::WrongLength(len) => write!(
                f,
                "certificate pin must be {PIN_HEX_LEN} hex characters, got {len}"
            ),
            PinError::NotHex => f.write_str("certificate pin is not hexadecimal"),
        }
    }
}

impl std::error::Error for PinError {}

impl From<PinError> for AppFailure {
    /// Lands on [`AppFailure::Network`] — a bad address, not a rejected
    /// credential, so `Auth`'s "please reconnect" message would misdescribe it.
    fn from(error: PinError) -> Self {
        AppFailure::Network(error.to_string())
    }
}

impl TrustConfig {
    /// Parse a 64-character hex certificate pin. Case-insensitive,
    /// exact-length, hex-only — no lenient path: a pin that fails to parse
    /// must never degrade into [`TrustConfig::SystemCa`] or
    /// [`TrustConfig::InsecureAcceptAny`].
    pub fn pinned_from_hex(pin: &str) -> Result<Self, PinError> {
        let bytes = pin.as_bytes();
        if bytes.len() != PIN_HEX_LEN {
            return Err(PinError::WrongLength(bytes.len()));
        }

        let mut digest = [0u8; PIN_LEN];
        for (slot, pair) in digest.iter_mut().zip(bytes.as_chunks::<2>().0) {
            let high = hex_nibble(pair[0]).ok_or(PinError::NotHex)?;
            let low = hex_nibble(pair[1]).ok_or(PinError::NotHex)?;
            *slot = (high << 4) | low;
        }
        Ok(TrustConfig::PinnedSha256(digest))
    }

    /// The `rustls` verifier this mode installs on a client config. Fallible
    /// only for [`TrustConfig::SystemCa`], whose verifier is built rather than
    /// constructed inline — see [`system_ca_verifier`].
    pub fn verifier(&self) -> Result<Arc<dyn ServerCertVerifier>, AppFailure> {
        let provider = crypto_provider();
        match self {
            TrustConfig::PinnedSha256(pin) => Ok(Arc::new(PinnedVerifier {
                pin: *pin,
                provider,
            })),
            TrustConfig::InsecureAcceptAny => Ok(Arc::new(AcceptAnyVerifier { provider })),
            TrustConfig::SystemCa => system_ca_verifier(provider),
        }
    }
}

/// SHA-256 over a leaf certificate's DER bytes, lowercase hex — the `fp` form a
/// pairing payload carries and [`TrustConfig::pinned_from_hex`] consumes.
pub fn leaf_fingerprint_hex(leaf_der: &[u8]) -> String {
    let digest = leaf_digest(leaf_der);
    let mut out = String::with_capacity(PIN_HEX_LEN);
    for byte in digest {
        out.push(HEX_DIGITS[usize::from(byte >> 4)] as char);
        out.push(HEX_DIGITS[usize::from(byte & 0x0f)] as char);
    }
    out
}

/// One hex character's value, or `None` if it is not hex.
fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// SHA-256 over a leaf certificate's DER bytes.
fn leaf_digest(leaf_der: &[u8]) -> [u8; PIN_LEN] {
    let digest = Sha256::digest(leaf_der);
    let mut pin = [0u8; PIN_LEN];
    pin.copy_from_slice(&digest);
    pin
}

/// Compare two pins without an early exit. Hand-rolled XOR fold rather than
/// the `subtle` crate (no new deps) — a pin is public data (it ships in the
/// pairing QR), so timing leakage reveals nothing new; this is defense in
/// depth against the *pattern*, and `black_box` stops an optimizer from
/// reintroducing the branch.
fn pins_match(presented: &[u8; PIN_LEN], expected: &[u8; PIN_LEN]) -> bool {
    let mut difference = 0u8;
    for index in 0..PIN_LEN {
        difference |= presented[index] ^ expected[index];
    }
    std::hint::black_box(difference) == 0
}

/// The process-wide `rustls` crypto provider, installing aws-lc-rs if
/// nothing has installed one yet. Installing (not just constructing) matters
/// because tonic builds its own `ClientConfig` from
/// `CryptoProvider::get_default()`, so one shared provider keeps signature
/// algorithms and cipher suites from splitting across two instances. A lost
/// install race is a no-op: this tree pairs rustls' default features with
/// tonic's `tls-aws-lc`, so the winner is always the same provider. Getting
/// this wrong — installing a second, conflicting provider at runtime — is
/// exactly the failure this shared-provider contract guards against.
fn crypto_provider() -> Arc<CryptoProvider> {
    if let Some(installed) = CryptoProvider::get_default() {
        return installed.clone();
    }
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
}

/// [`TrustConfig::SystemCa`] off Android: the platform's own trust store,
/// through `rustls-platform-verifier` (Security.framework on Apple targets).
#[cfg(not(target_os = "android"))]
fn system_ca_verifier(
    provider: Arc<CryptoProvider>,
) -> Result<Arc<dyn ServerCertVerifier>, AppFailure> {
    let verifier = rustls_platform_verifier::Verifier::new(provider)
        .map_err(|err| AppFailure::Network(format!("platform verifier: {err}")))?;
    Ok(Arc::new(verifier))
}

/// [`TrustConfig::SystemCa`] on Android: rustls' own WebPKI verifier over the
/// Mozilla root program — deliberately *not* the platform verifier.
///
/// `rustls-platform-verifier`'s Android component enables revocation checking
/// with OCSP tried first and no CRL preference, and Android's checker raises
/// a hard `CertPathValidatorException: Certificate does not specify OCSP
/// responder` for any certificate carrying no OCSP URI — which the verifier
/// then reports as `Revoked` and the handshake fails. Google Trust Services
/// and Let's Encrypt have both dropped OCSP for CRLs, so that is now the
/// ordinary shape of a public certificate, and every "Public domain (CA)"
/// dial failed on it (observed against the demo's GTS WE1 chain). Upstream
/// tracks it as rustls-platform-verifier PR #34, unreleased as of 0.7.0.
///
/// The trade: this trusts the bundled Mozilla roots, not the device's own
/// store, so a user-installed or enterprise root is not honoured on Android —
/// a server under one needs a pin (a scanned `fp`) instead. Revocation is not
/// checked, which matches what browsers do for these certificates today.
#[cfg(target_os = "android")]
fn system_ca_verifier(
    provider: Arc<CryptoProvider>,
) -> Result<Arc<dyn ServerCertVerifier>, AppFailure> {
    webpki_roots_verifier(provider)
}

/// rustls' WebPKI verifier over `webpki-roots`' bundled Mozilla trust
/// anchors. Android's [`system_ca_verifier`]; compiled under `test` too so
/// the host suite exercises the exact verifier a device installs.
#[cfg(any(target_os = "android", test))]
fn webpki_roots_verifier(
    provider: Arc<CryptoProvider>,
) -> Result<Arc<dyn ServerCertVerifier>, AppFailure> {
    use rustls::RootCertStore;
    use rustls::client::WebPkiServerVerifier;

    let roots: RootCertStore = webpki_roots::TLS_SERVER_ROOTS.iter().cloned().collect();
    let verifier: Arc<dyn ServerCertVerifier> =
        WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider)
            .build()
            .map_err(|err| AppFailure::Network(format!("system-CA verifier: {err}")))?;
    Ok(verifier)
}

/// Accepts exactly the leaf certificate whose DER bytes hash to [`Self::pin`].
///
/// Chain building and name matching are deliberately absent — a pin *is* the
/// identity — but handshake-signature verification is not (see the module docs).
#[derive(Debug)]
struct PinnedVerifier {
    pin: [u8; PIN_LEN],
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        if pins_match(&leaf_digest(end_entity.as_ref()), &self.pin) {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(RustlsError::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Accepts any certificate. Reachable only through
/// [`TrustConfig::InsecureAcceptAny`], whose own doc states the exposure.
///
/// Handshake signatures are still verified, so this proves the peer holds the
/// key for whatever certificate it presented — it just does not care *which*
/// certificate that is.
#[derive(Debug)]
struct AcceptAnyVerifier {
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for AcceptAnyVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The digest/hex pair round-trips: hashing bytes and parsing the printed
    /// hex must land on the same 32 bytes a pinned verifier compares against.
    #[test]
    fn fingerprint_hex_round_trips_into_a_pin() {
        let hex = leaf_fingerprint_hex(b"a certificate's DER bytes");
        assert_eq!(hex.len(), PIN_HEX_LEN);

        let parsed = TrustConfig::pinned_from_hex(&hex).expect("its own hex parses");
        assert_eq!(
            parsed,
            TrustConfig::PinnedSha256(leaf_digest(b"a certificate's DER bytes"))
        );
    }

    /// A pairing payload may arrive upper-cased; both spellings must produce the
    /// identical pin.
    #[test]
    fn pin_parsing_is_case_insensitive() {
        let lower = leaf_fingerprint_hex(b"leaf");
        let upper = lower.to_uppercase();

        assert_eq!(
            TrustConfig::pinned_from_hex(&lower).expect("lowercase"),
            TrustConfig::pinned_from_hex(&upper).expect("uppercase")
        );
    }

    /// Fail-closed: every malformed pin is an error, never a weaker trust mode.
    #[test]
    fn malformed_pins_are_rejected_before_any_dial() {
        assert_eq!(
            TrustConfig::pinned_from_hex(""),
            Err(PinError::WrongLength(0))
        );
        assert_eq!(
            TrustConfig::pinned_from_hex(&"a".repeat(PIN_HEX_LEN - 1)),
            Err(PinError::WrongLength(PIN_HEX_LEN - 1))
        );
        assert_eq!(
            TrustConfig::pinned_from_hex(&"a".repeat(PIN_HEX_LEN + 1)),
            Err(PinError::WrongLength(PIN_HEX_LEN + 1))
        );
        // Right length, wrong alphabet — the case a length-only check misses.
        let mut not_hex = "a".repeat(PIN_HEX_LEN - 1);
        not_hex.push('z');
        assert_eq!(
            TrustConfig::pinned_from_hex(&not_hex),
            Err(PinError::NotHex)
        );
    }

    /// The constant-time compare still answers the plain question correctly.
    #[test]
    fn pins_match_only_on_equal_digests() {
        let a = leaf_digest(b"one");
        let b = leaf_digest(b"two");
        assert!(pins_match(&a, &a));
        assert!(!pins_match(&a, &b));

        // A single flipped bit anywhere must still fail.
        let mut nearly = a;
        nearly[PIN_LEN - 1] ^= 0x01;
        assert!(!pins_match(&a, &nearly));
    }

    /// Every mode builds a verifier on the host — including `SystemCa`, whose
    /// platform verifier is what a release build actually installs.
    #[test]
    fn every_trust_mode_builds_a_verifier() {
        for mode in [
            TrustConfig::SystemCa,
            TrustConfig::InsecureAcceptAny,
            TrustConfig::PinnedSha256([7u8; PIN_LEN]),
        ] {
            assert!(mode.verifier().is_ok(), "{mode:?} built no verifier");
        }
    }

    /// The verifier Android installs for `SystemCa` builds on the host too,
    /// and it is a real WebPKI verifier over a non-empty root set: a leaf
    /// nobody signed is refused, not waved through, and the refusal is not
    /// the platform verifier's `Revoked` — the failure this path exists to
    /// avoid.
    #[test]
    fn the_webpki_roots_verifier_refuses_an_unknown_issuer() {
        assert!(
            !webpki_roots::TLS_SERVER_ROOTS.is_empty(),
            "the bundled root program is empty"
        );
        let verifier = webpki_roots_verifier(crypto_provider()).expect("builds on the host");

        let leaf = CertificateDer::from(b"not a certificate anyone signed".to_vec());
        let name = ServerName::try_from("demo.example").expect("a DNS name");
        let now = UnixTime::since_unix_epoch(std::time::Duration::from_secs(1_700_000_000));

        let refused = verifier
            .verify_server_cert(&leaf, &[], &name, &[], now)
            .expect_err("an unsigned leaf must be refused");
        assert!(
            !matches!(
                refused,
                RustlsError::InvalidCertificate(CertificateError::Revoked)
            ),
            "the WebPKI path must never surface the platform verifier's Revoked: {refused:?}"
        );
    }

    /// The pinned verifier answers on the certificate it was pinned to and
    /// refuses every other one.
    #[test]
    fn pinned_verifier_accepts_only_its_own_leaf() {
        let leaf = CertificateDer::from(b"leaf-der".to_vec());
        let other = CertificateDer::from(b"other-der".to_vec());
        let verifier = PinnedVerifier {
            pin: leaf_digest(leaf.as_ref()),
            provider: crypto_provider(),
        };
        let name = ServerName::try_from("127.0.0.1").expect("an IP literal is a server name");
        let now = UnixTime::since_unix_epoch(std::time::Duration::from_secs(1_700_000_000));

        assert!(
            verifier
                .verify_server_cert(&leaf, &[], &name, &[], now)
                .is_ok()
        );
        assert!(
            verifier
                .verify_server_cert(&other, &[], &name, &[], now)
                .is_err()
        );
    }
}
