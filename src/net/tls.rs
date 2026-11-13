//! The trust modes a `muxrd` dial runs under and the `rustls` verifier each installs. A pin is
//! SHA-256 over the leaf DER (not SPKI) and parses fail-closed. Pinning replaces chain and name
//! validation, never handshake-signature checks, so a replayed certificate still fails.

use std::fmt;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, Error as RustlsError, SignatureScheme};
use sha2::{Digest, Sha256};

use crate::failure::AppFailure;

/// SHA-256's digest length.
pub const PIN_LEN: usize = 32;

pub const PIN_HEX_LEN: usize = PIN_LEN * 2;

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrustConfig {
    /// Apple uses the platform trust store; Android uses WebPKI over the bundled Mozilla roots
    /// (see [`system_ca_verifier`]).
    SystemCa,
    /// Dev servers only: no peer authentication at all.
    InsecureAcceptAny,
    /// The leaf certificate whose DER bytes hash to this digest.
    PinnedSha256([u8; PIN_LEN]),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PinError {
    WrongLength(usize),
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
    /// `Network`, not `Auth`: a bad address, not a rejected credential.
    fn from(error: PinError) -> Self {
        AppFailure::Network(error.to_string())
    }
}

impl TrustConfig {
    /// Case-insensitive, exact-length, hex-only; a bad pin never degrades into a weaker mode.
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

    /// Fallible only for [`TrustConfig::SystemCa`].
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

/// The `fp` form a pairing payload carries and [`TrustConfig::pinned_from_hex`] consumes.
pub fn leaf_fingerprint_hex(leaf_der: &[u8]) -> String {
    let digest = leaf_digest(leaf_der);
    let mut out = String::with_capacity(PIN_HEX_LEN);
    for byte in digest {
        out.push(HEX_DIGITS[usize::from(byte >> 4)] as char);
        out.push(HEX_DIGITS[usize::from(byte & 0x0f)] as char);
    }
    out
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn leaf_digest(leaf_der: &[u8]) -> [u8; PIN_LEN] {
    let digest = Sha256::digest(leaf_der);
    let mut pin = [0u8; PIN_LEN];
    pin.copy_from_slice(&digest);
    pin
}

/// Constant-time compare; `black_box` stops the optimizer reintroducing an early exit.
fn pins_match(presented: &[u8; PIN_LEN], expected: &[u8; PIN_LEN]) -> bool {
    let mut difference = 0u8;
    for index in 0..PIN_LEN {
        difference |= presented[index] ^ expected[index];
    }
    std::hint::black_box(difference) == 0
}

/// Installs aws-lc-rs as the process default when none is set: tonic builds its `ClientConfig`
/// from `CryptoProvider::get_default()`, so one provider must be shared. A lost race is a no-op.
fn crypto_provider() -> Arc<CryptoProvider> {
    if let Some(installed) = CryptoProvider::get_default() {
        return installed.clone();
    }
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
}

#[cfg(not(target_os = "android"))]
fn system_ca_verifier(
    provider: Arc<CryptoProvider>,
) -> Result<Arc<dyn ServerCertVerifier>, AppFailure> {
    let verifier = rustls_platform_verifier::Verifier::new(provider)
        .map_err(|err| AppFailure::Network(format!("platform verifier: {err}")))?;
    Ok(Arc::new(verifier))
}

/// Not the platform verifier: its Android revocation check reports a certificate without an OCSP
/// URI, the ordinary shape of a public certificate, as `Revoked`. Only the bundled Mozilla roots
/// are trusted, so a user or enterprise root needs a pin; revocation is not checked.
#[cfg(target_os = "android")]
fn system_ca_verifier(
    provider: Arc<CryptoProvider>,
) -> Result<Arc<dyn ServerCertVerifier>, AppFailure> {
    webpki_roots_verifier(provider)
}

/// Compiled under `test` too so the host suite exercises the verifier a device installs.
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

/// No chain or name checks, since a pin is the identity; handshake signatures are still verified.
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

/// Still verifies handshake signatures, so the peer holds the key for the certificate it presents.
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

    /// A pairing payload may arrive upper-cased.
    #[test]
    fn pin_parsing_is_case_insensitive() {
        let lower = leaf_fingerprint_hex(b"leaf");
        let upper = lower.to_uppercase();

        assert_eq!(
            TrustConfig::pinned_from_hex(&lower).expect("lowercase"),
            TrustConfig::pinned_from_hex(&upper).expect("uppercase")
        );
    }

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
        // Right length, wrong alphabet.
        let mut not_hex = "a".repeat(PIN_HEX_LEN - 1);
        not_hex.push('z');
        assert_eq!(
            TrustConfig::pinned_from_hex(&not_hex),
            Err(PinError::NotHex)
        );
    }

    #[test]
    fn pins_match_only_on_equal_digests() {
        let a = leaf_digest(b"one");
        let b = leaf_digest(b"two");
        assert!(pins_match(&a, &a));
        assert!(!pins_match(&a, &b));

        let mut nearly = a;
        nearly[PIN_LEN - 1] ^= 0x01;
        assert!(!pins_match(&a, &nearly));
    }

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

    /// The refusal must not be the platform verifier's `Revoked`.
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
