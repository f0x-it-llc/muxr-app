//! The generated `muxr.v1` service: every message type, the client stubs
//! (`muxr_client::MuxrClient`) and the server stubs (`muxr_server::MuxrServer`)
//! `build.rs` emits from `proto/muxr.proto` into `$OUT_DIR`.
//!
//! **Hand-written code never restates a wire shape.** A field number, an enum
//! value, an RPC name or a metadata key belongs in the vendored proto (or, for
//! `authorization: Bearer …`, in the channel's interceptor) — everything here
//! is derived from that one file, so a server-side wire change surfaces as a
//! compile error at the call sites instead of a silent decode mismatch.
//!
//! The server stubs exist for **in-process test fixtures**: a fake `muxrd` a
//! test can stand up on a loopback port to exercise the client without the
//! docker rig. Nothing in a shipped build serves gRPC, which is why tonic's
//! `server`/`router` features are `[dev-dependencies]`-only (see `Cargo.toml`)
//! and why the stubs cost the app nothing but compile time.

// One private module holds every generated item, re-exported below, so the
// boundary between generated and authored code in this file is a `mod` block
// rather than a convention. `include_proto!` expands to an `include!` of
// `$OUT_DIR/muxr.v1.rs`, which is why nothing here is editable: the file is
// rewritten from the proto on every build whose input changed.
//
// No lint allowance is wrapped around it. The generated client and server
// modules carry their own inner `#![allow(...)]`, so this module passes
// `clippy -D warnings` unaided — if a future codegen bump changes that, the
// allowance belongs on this `mod`, never widened to the file or the crate.
mod generated {
    tonic::include_proto!("muxr.v1");
}

pub use generated::*;

#[cfg(test)]
mod tests {
    use prost::Message;

    use super::*;

    /// The codegen chain is wired end to end: a message encodes to protobuf
    /// bytes and decodes back to the same value.
    ///
    /// `LoginRequest` specifically — the first call any session makes, and the
    /// one whose shape the whole authenticated surface depends on.
    #[test]
    fn login_request_round_trips_through_prost() {
        let request = LoginRequest {
            auth_token: "pairing-token".to_string(),
            remember_me: true,
        };

        let encoded = request.encode_to_vec();
        let decoded = LoginRequest::decode(encoded.as_slice()).expect("decodes its own encoding");

        assert_eq!(decoded, request);
        // A non-empty encoding proves the fields were actually written: proto3
        // omits default values, so an all-default message encodes to zero bytes
        // and would round-trip vacuously.
        assert!(!encoded.is_empty());
    }

    /// The client half exists and is nameable from `crate::net::proto`.
    #[test]
    fn client_stub_is_generated() {
        let name = std::any::type_name::<muxr_client::MuxrClient<tonic::transport::Channel>>();
        assert!(
            name.contains("MuxrClient"),
            "unexpected client type: {name}"
        );
    }

    /// The server half exists too, and carries the service path a test fixture
    /// is routed by — the same `muxr.v1.Muxr` prefix muxrd's auth layer keys
    /// its public-path allowlist on.
    #[test]
    fn server_stub_is_generated_with_its_service_name() {
        assert_eq!(
            <muxr_server::MuxrServer<()> as tonic::server::NamedService>::NAME,
            "muxr.v1.Muxr"
        );
    }
}
