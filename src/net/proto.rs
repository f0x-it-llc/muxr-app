//! The generated `muxr.v1` service: every message type, the client stubs
//! (`muxr_client::MuxrClient`) and the server stubs (`muxr_server::MuxrServer`)
//! `build.rs` emits from `proto/muxr.proto` into `$OUT_DIR`.
//!
//! Hand-written code never restates a wire shape — a field number, enum
//! value, RPC name or metadata key belongs in the vendored proto, so a
//! server-side change surfaces as a compile error, not a silent decode
//! mismatch.
//!
//! The server stubs exist only for in-process test fixtures (tonic's
//! `server`/`router` features are `[dev-dependencies]`-only); nothing
//! shipped serves gRPC.

// Generated code lives in one private module, re-exported below, so the
// generated/authored boundary is a `mod` block, not a convention.
// `$OUT_DIR/muxr.v1.rs` is rewritten on every build and is not editable. Its
// own inner `#![allow(...)]` covers lints — never widen an allowance to the
// file or crate if a codegen bump changes that.
mod generated {
    tonic::include_proto!("muxr.v1");
}

pub use generated::*;

#[cfg(test)]
mod tests {
    use prost::Message;

    use super::*;

    /// The codegen chain round-trips end to end, checked via `LoginRequest` —
    /// the first call any session makes.
    #[test]
    fn login_request_round_trips_through_prost() {
        let request = LoginRequest {
            auth_token: "pairing-token".to_string(),
            remember_me: true,
        };

        let encoded = request.encode_to_vec();
        let decoded = LoginRequest::decode(encoded.as_slice()).expect("decodes its own encoding");

        assert_eq!(decoded, request);
        // proto3 omits default values, so a non-empty encoding proves the
        // fields were actually written rather than round-tripping vacuously.
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

    /// The server half exists too, with the same `muxr.v1.Muxr` service-path
    /// prefix muxrd's auth layer allowlists.
    #[test]
    fn server_stub_is_generated_with_its_service_name() {
        assert_eq!(
            <muxr_server::MuxrServer<()> as tonic::server::NamedService>::NAME,
            "muxr.v1.Muxr"
        );
    }
}
