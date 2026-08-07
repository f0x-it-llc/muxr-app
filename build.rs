//! `muxr.v1` codegen: `proto/muxr.proto` -> `$OUT_DIR/muxr.v1.rs`, the file
//! `src/net/proto.rs` includes.
//!
//! **Pure Rust, no `protoc`.** `protox` is a protobuf compiler written in Rust:
//! it parses the vendored `.proto` and yields the `FileDescriptorSet` that
//! `prost-build` would otherwise shell out to a system `protoc` binary to
//! produce (prost dropped its vendored-protoc feature). Nothing in this build —
//! CI, a fresh clone, a cross-compile for Android or iOS — needs `protoc`
//! installed, which is the whole reason for this route rather than
//! `compile_protos`.
//!
//! **Crate split (tonic 0.14).** `tonic-prost-build` owns codegen
//! (`configure()`/`compile_fds`); `tonic-prost` is the runtime `ProstCodec` the
//! generated stubs name; plain `tonic-build` is prost-agnostic and is not used
//! here directly. Client *and* server stubs are generated: the client is what
//! the app speaks, the server is what an in-process test fixture implements to
//! exercise it without a live `muxrd`.

use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto = Path::new("proto/muxr.proto");
    let include = Path::new("proto");

    // The only input; without this the vendored proto could change and Cargo
    // would still hand the compiler a stale `$OUT_DIR/muxr.v1.rs`.
    println!("cargo::rerun-if-changed={}", proto.display());

    let descriptors = protox::compile([proto], [include])?;

    tonic_prost_build::configure()
        .build_client(true)
        .build_server(true)
        .compile_fds(descriptors)?;

    Ok(())
}
