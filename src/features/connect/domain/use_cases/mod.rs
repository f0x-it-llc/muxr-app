//! `connect` use cases — one file per `<verb><noun>` action (see
//! `examples/huddle`'s `domain/use_cases/` convention).
//!
//! | Use case | What it decides |
//! |---|---|
//! | [`parse_endpoint`] | Whether the form's server URL is a dialable `host:port` |
//! | [`parse_pairing`] | Whether a scanned QR code is a well-formed `muxr://pair` payload |
//!
//! [`parse_endpoint`](parse_endpoint::parse_endpoint) and
//! [`parse_pairing`](parse_pairing::parse_pairing_uri) are plain functions
//! rather than `clean_signals::UseCase`s — see `parse_endpoint`'s own module
//! docs for why (that trait is async; both are pure synchronous parses).

pub mod parse_endpoint;
pub mod parse_pairing;
