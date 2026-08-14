//! `connect` domain — entities, repository contracts, and use cases.
//! Framework-free: a domain file imports only this feature's own `domain` items
//! plus `crate::failure` — never `frust::`, and never a `data`/`presentation`
//! path segment.
//!
//! | Module | What it owns |
//! |---|---|
//! | [`entities`] | `SavedServer`, `PairingPayload`/`PairingTrust`, and the `CredentialService` vocabulary (`CredentialStatus`/`UnlockedCredential`/`StoreRequest`/`UnlockRequest`) |
//! | [`repositories`] | `SavedServersRepository` and `CredentialService` — the two-store split |
//! | [`use_cases`] | `parse_endpoint`, `parse_pairing::parse_pairing_uri`, and the two credential use cases |
//!
//! One documented deviation from the import rule: [`entities`] re-exports
//! [`crate::connection::TrustMode`] rather than minting a second trust-mode enum.

pub mod entities;
pub mod repositories;
pub mod use_cases;
