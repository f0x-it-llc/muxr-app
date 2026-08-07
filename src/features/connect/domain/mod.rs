//! `connect` domain — entities, repository contract(s), and use cases.
//! Framework-free: a domain file imports only this feature's own `domain`
//! items plus `crate::failure` — never `frust::`, and never a `data`/
//! `presentation` path segment (see `examples/huddle`'s `tests/architecture.rs`
//! for the enforced version of this rule).
//!
//! | Module | What it owns |
//! |---|---|
//! | [`entities`] | `SavedServer` (a remembered `muxrd` endpoint), `PairingPayload`/`PairingTrust` (a scanned QR's typed payload), and the `CredentialService` vocabulary (`CredentialStatus`/`UnlockedCredential`/`StoreRequest`/`UnlockRequest`) |
//! | [`repositories`] | `SavedServersRepository` (the saved-server metadata store) and `CredentialService` (the biometric-gated secret store) — the two-store split; see that module's own docs |
//! | [`use_cases`] | `parse_endpoint` — the form's `host:port` validation; `parse_pairing::parse_pairing_uri` — decodes a scanned `muxr://pair` QR code into a `PairingPayload` |
//!
//! One documented deviation from the import rule above: [`entities`] re-exports
//! [`crate::connection::TrustMode`] rather than minting a second trust-mode
//! enum. See that module's own docs for the reasoning and the tension.

pub mod entities;
pub mod repositories;
pub mod use_cases;
