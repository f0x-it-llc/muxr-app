//! `connect` use cases — one file per `<verb><noun>` action (see
//! `examples/huddle`'s `domain/use_cases/` convention).
//!
//! | Use case | What it decides |
//! |---|---|
//! | [`parse_endpoint`] | Whether the form's server URL is a dialable `host:port` |
//!
//! [`parse_endpoint`](parse_endpoint::parse_endpoint) is a plain function rather
//! than a `clean_signals::UseCase` — see its own module docs for why (that trait
//! is async; this is a pure synchronous parse).

pub mod parse_endpoint;
