# Muxr - Development Guide

## Prerequisites

- A [Frust](https://github.com/f0x-it-llc/frust) checkout, with this repo as its `apps/muxr-app`:
  `Cargo.toml`'s `frust` (`../../crates/frust`) and `clean-signals-frust`
  (`../../plugins/clean-signals-frust`) are **path** dependencies into it, as are six plugin crates
  under `../../plugins/<dir>`, each directory dropping the crate's `frust-` prefix:
  `frust-clipboard`, `frust-haptics` (needs Android's `android.permission.VIBRATE`, already checked
  into `AndroidManifest.xml`), `frust-shared-preferences`, `frust-secure-storage`, `frust-camera`
  and `frust-iap` (also included as the Gradle module `:frust-iap` by `android/settings.gradle.kts`)
  — plus `../../crates/frust-plugin` for the Android JNI accessor. A temporary pre-publication
  arrangement, documented verbatim in `Cargo.toml`'s comments.
- No `clean-signals-rs` sibling checkout is required: `clean-signals` itself isn't published yet, so
  `Cargo.toml` git+rev-pins it (`rev = "910f626"` on `master`) with the identical spec
  `clean-signals-frust`'s own dependency uses, so Cargo unifies both into one crate identity (see
  `Cargo.toml`'s comment for why that match is load-bearing). Bump the rev only in lockstep with the
  Frust checkout's `plugins/clean-signals-frust` pin, never one alone; a local checkout for
  `[patch]`-iterating on `clean-signals` itself stays optional.
- Rust (stable, 2024 edition) plus the Frust CLI (`frust`) for device builds/flavors.
- Android: NDK, `cargo-ndk`, JDK 17 — the Frust CLI's prerequisites (frust `docs/DEVELOPMENT.md`).
- iOS: macOS host, Xcode, the `aarch64-apple-ios`/`aarch64-apple-ios-sim` Rust targets.

## Build

```bash
cargo build            # debug
cargo build --release  # release
```

## Codegen (`muxr.v1` gRPC)

`build.rs` compiles `proto/muxr.proto` into `$OUT_DIR/muxr.v1.rs`, which `src/net/proto.rs` includes;
generated code is never committed. **No `protoc` install is required** anywhere, CI and
cross-compiles included: `protox` is a Rust protobuf compiler feeding its `FileDescriptorSet` straight
to `tonic-prost-build`'s `compile_fds` (`prost-build`'s path would shell out to a system `protoc`).

`proto/muxr.proto` **vendors** `muxrd/proto/muxr.proto` from the `muxr-core` repo at rev `41c39e1`
(branch `feat/space-scoped-layout`, recorded in its own header). To refresh against a newer server:

```bash
cp <muxr-core>/muxrd/proto/muxr.proto proto/muxr.proto   # then re-add the header block
# update the rev recorded in that header, and rebuild
```

Re-copy wholesale and never hand-patch a message: wire drift then shows up as a codegen or type error
at the call sites that consumed the old shape, not a silent decode mismatch against a live server.

Because the gRPC stack brings TLS with a C-built crypto provider (**aws-lc-rs**, rustls' default and
tonic's `tls-aws-lc` feature — the one provider in the tree, since a second is a runtime install
conflict), a change to the network layer is checked against both device targets, not just the host:

```bash
cargo ndk -t arm64-v8a check      # Android arm64 (reads ANDROID_NDK_HOME)
cargo check --target aarch64-apple-ios
```

`cargo ndk` reads `ANDROID_NDK_HOME`; nothing here or in the Gradle rig pins an NDK version.
aws-lc-rs' Android cross-compile needs **NDK r27.3 or newer** (older r27 had a CMake bug that broke
it) — r28.2 (`28.2.13676358`) is what this rig is verified against. On an older machine the recorded
fallback swaps tonic's `tls-aws-lc` for `tls-ring` (and rustls/rcgen to their `ring` backends), a
whole-tree swap, never a per-crate one.

## Run

```bash
cargo run                              # desktop preview
cargo run --features mock-iap          # desktop preview with the purchase flow scripted
frust run --flavor develop             # device/simulator: Develop flavor/scheme
frust run --flavor production          # device/simulator: Production flavor/scheme
```

The cold-start splash (`docs/ARCHITECTURE.md`'s UI Substrate, `splash` bullet) plays on `cargo run`
too — gated per **process launch**, so it replays on every restart, not on a warm return to `/`.

`mock-iap` swaps `pro`'s entitlement store for the scripted in-memory one, since no desktop platform
can sell; **no device build ever passes it** (the CLI only ever selects `lean` or `frust/perf-trace`).
Without it, a desktop run logs one `NotAvailable(UnsupportedPlatform)` at launch and stays Free.

## Test Rig (Live `muxrd`)

Exercising the app against a real server (rather than the mock/headless suites) needs a `muxr-core`
checkout running `muxrd` in Docker, reachable from the device — one variant at a time:

```bash
cd <muxr-core>
BIND_ADDR=<host-ip> ./docker/run.sh --host <host-ip>            # e.g. a tailscale IP: 100.99.99.127
BIND_ADDR=<host-ip> ./docker/run.sh --host <host-ip> --herdr    # the herdr backend, same ports
```

`<host-ip>` must be reachable from the device (a LAN/tailscale IP, never `localhost`) and must match
`muxrctl`'s pairing setup — that IP lands in the cert's SAN. Pair from the rig's own `muxrctl` TUI
(`ssh -t root@<host-ip> -p 2222`): **Configure** (same `<host-ip>`) → **Cert** → **Tokens** →
**Server** → **Pair QR**, then scan it in Connect. Without SSH:
`docker exec muxr-grpc-rig muxrd create-token --name <token-name>`.

A second `zellij` client on the same session resizes the phone's view to the smaller of the two
clients' dimensions — expected `zellij` behaviour, not an app bug.

## Test

```bash
cargo test                                   # all tests (unit + tests/*.rs integration suites)
cargo test --test routes                     # one integration suite
```

The verify gate, run before committing:

```bash
cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
```

## Flavors

Two flavors, both wired on Android (Gradle product flavors) and iOS (Xcode schemes):

| Flavor | Android application ID | Android app name | iOS bundle ID | iOS scheme |
|--------|------------------------|-------------------|----------------|------------|
| `develop` | `com.it.f0x.muxr.dev` | Muxr Dev | `com.it.f0x.muxr.dev` | `Develop` |
| `production` | `com.it.f0x.muxr` | Muxr | `com.it.f0x.muxr` | `Production` |

`frust run --flavor <f>`/`frust build ... --flavor <f>` map `<f>` to Android's matching product
flavor and to the iOS scheme `<Capitalized f>`; the iOS build configuration is
`<Debug|Profile|Release>-<Scheme>` (e.g. `Release-Production`). The Apple Developer Team ID
signing a device build comes from `frust.toml`'s `[ios] team`.

Both flavors bill the **real** store: `com.it.f0x.muxr.dev` has its own Play Console entry ("Muxr
Dev", internal testing only), its own `muxr_pro` product and license testers, so a sideloaded develop
release completes a real purchase; mocks live only in tests and Run's `--features mock-iap`.

Android, without the CLI:

```bash
cd android && ./gradlew :app:assembleDevelopDebug
cd android && ./gradlew :app:assembleDevelopRelease      # the develop RELEASE build
cd android && ./gradlew :app:assembleProductionRelease
```

## Android Signing

Release signing reads `android/app/keystores/key.properties` (gitignored;
`android/app/keystores/README.md` has the generation steps), with `develop.*`/`prod.*` keys
(`storeFile`, `storePassword`, `keyAlias`, `keyPassword`), or the `DEVELOP_*`/plain `*_KEYSTORE_*`
env vars for CI — the names `frust.toml`'s `[signing] env` points the CLI gate at too. With neither,
both flavors' release builds fall back to debug signing so a keystore-less machine still builds; Gradle
logs a loud warning (`[muxr] release variant <variantName> is DEBUG-SIGNED: [FRUST-SIGNING-FALLBACK] ...`,
the token the CLI greps to refuse the artifact) from every release task for the variant —
`assemble<Flavor>Release` (APK), `bundle<Flavor>Release` (the AAB the Play listing takes, and what the
CLI's `appbundle` maps to) and any `package<Flavor>Release*`. Real keystores live outside the repo.

`frust.toml`'s `[signing] key-properties = "app/keystores/key.properties"` with `prefix = "prod"`
points the CLI's release gate (`frust-drive`'s `android_build::signing`) at that same file, so
`frust build --release`/`appbundle` resolves the identical `prod.*` keys Gradle already reads instead
of the scaffold default `android/key.properties`. Gradle itself is untouched, and the CLI's generated
`android/.frust-signing.properties` is transient (owner-only, deleted on exit, and gitignored
as a backstop). `prefix` is single-valued, so it wires up the `production` flavor only: a
**develop** release build keeps the direct `./gradlew :app:assembleDevelopRelease` recipe above.

## Release (Play)

```bash
frust build appbundle --release --flavor production \
  --build-name <semver> --build-number <N>          # run from apps/muxr-app
```

`--build-name` is the versionName (semver), `--build-number` the versionCode — monotonic per upload,
never hand-edited; with no keystore the gate fails naming `prod.storeFile`. The signed bundle lands
at `android/app/build/outputs/bundle/productionRelease/app-production-release.aab`. Check 16 KB page
alignment before uploading:

```bash
llvm-readelf -lW android/app/src/main/jniLibs/arm64-v8a/libmuxr.so | grep LOAD   # every Align 0x4000
```

Everything Play-Console-side is [RELEASE_ANDROID.md](RELEASE_ANDROID.md)'s.

## iOS Signing

No `key.properties` equivalent: Xcode resolves the signing identity from the Apple Developer Team ID
(`frust.toml`'s `[ios] team`), matched to a provisioning profile on the signing machine.
