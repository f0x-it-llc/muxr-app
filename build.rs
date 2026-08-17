//! `muxr.v1` codegen: `proto/muxr.proto` -> `$OUT_DIR/muxr.v1.rs`, the file
//! `src/net/proto.rs` includes.

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
