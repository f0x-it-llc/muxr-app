# Contributing to Muxr

Pull requests are welcome.

## Build and test

You need Rust (stable). Device builds also need the `frust` CLI
(https://frust.dev/), the Android SDK and NDK, and Xcode. Before opening a
pull request, run the verify gate from the repository root:

```
cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
```

All four commands must pass.

## License

Muxr is licensed under the PolyForm Noncommercial License 1.0.0. See
[LICENSE](LICENSE). Your contributions are accepted under the same license.

## Contributor License Agreement

Every contributor must agree to [CLA.md](CLA.md) before a pull request is
merged. To agree, write this in the pull request:

> I have read and agree to the CLA
