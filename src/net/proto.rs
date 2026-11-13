//! The `muxr.v1` service generated from `proto/muxr.proto`. Wire shapes live only in the
//! proto; the server stubs exist for in-process test fixtures.

// The generated file carries its own `#![allow(...)]`; never widen it to the crate.
mod generated {
    tonic::include_proto!("muxr.v1");
}

pub use generated::*;

#[cfg(test)]
mod tests {
    use prost::Message;

    use super::*;

    #[test]
    fn login_request_round_trips_through_prost() {
        let request = LoginRequest {
            auth_token: "pairing-token".to_string(),
            remember_me: true,
        };

        let encoded = request.encode_to_vec();
        let decoded = LoginRequest::decode(encoded.as_slice()).expect("decodes its own encoding");

        assert_eq!(decoded, request);
        // proto3 omits defaults, so a non-empty encoding proves the fields were written.
        assert!(!encoded.is_empty());
    }

    #[test]
    fn client_stub_is_generated() {
        let name = std::any::type_name::<muxr_client::MuxrClient<tonic::transport::Channel>>();
        assert!(
            name.contains("MuxrClient"),
            "unexpected client type: {name}"
        );
    }

    /// `muxr.v1.Muxr` is the service-path prefix muxrd's auth layer allowlists.
    #[test]
    fn server_stub_is_generated_with_its_service_name() {
        assert_eq!(
            <muxr_server::MuxrServer<()> as tonic::server::NamedService>::NAME,
            "muxr.v1.Muxr"
        );
    }
}
