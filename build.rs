//! `muxr.v1` codegen: every `.proto` under `proto/` (include root `proto`, files named
//! `muxr/v1/<file>.proto`) -> `$OUT_DIR/muxr.v1.rs`, the file `src/net/proto.rs` includes.

use std::path::{Path, PathBuf};

/// Collects `.proto` files under `dir` recursively, skipping names that start with `.`.
fn collect(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with('.'))
        {
            continue;
        }
        if path.is_dir() {
            collect(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "proto") {
            out.push(path);
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let include = Path::new("proto");

    // Without this the vendored protos could change and Cargo would still hand the
    // compiler a stale `$OUT_DIR/muxr.v1.rs`.
    println!("cargo::rerun-if-changed={}", include.display());

    let mut files = Vec::new();
    collect(include, &mut files)?;
    files.sort();

    let mut names = Vec::new();
    for file in &files {
        println!("cargo::rerun-if-changed={}", file.display());
        names.push(file.strip_prefix(include)?.to_path_buf());
    }

    let descriptors = protox::compile(&names, [include])?;

    tonic_prost_build::configure()
        .build_client(true)
        .build_server(true)
        .compile_fds(descriptors)?;

    Ok(())
}
