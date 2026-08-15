# Muxr - Development Guide

## Prerequisites

- A [Frust](https://github.com/f0x-it-llc/frust) checkout, with this repo as its
  `apps/muxr-app` — `Cargo.toml`'s `frust`/`clean-signals-frust` dependencies are **path**
  dependencies into that checkout (`../../crates/frust`, `../../plugins/clean-signals-frust`),
  a temporary pre-publication arrangement documented verbatim in `Cargo.toml`'s comments. Once
  Frust crates are published this app will depend on versions instead. The terminal screen and the
  `muxrd` network layer add five more OS-capability plugins the same temporary way —
  `frust-clipboard`, `frust-haptics` (needs Android's `android.permission.VIBRATE`, already checked
  into `AndroidManifest.xml`), `frust-shared-preferences`, `frust-secure-storage`, `frust-camera` —
  plus `../../crates/frust-plugin` for the Android JNI accessor, all under `../../plugins/<name>` in
  the same checkout.
- No `clean-signals-rs` sibling checkout is required: `clean-signals` itself isn't published
  yet, so `Cargo.toml` git+rev-pins it directly (`rev = "910f626"` on `master`) to its public
  repo — the identical spec `clean-signals-frust`'s own `clean-signals` dependency uses, so
  Cargo unifies both into one crate identity (see `Cargo.toml`'s comment for why that identity
  match is load-bearing). Bump this rev only in lockstep with the Frust checkout's
  `plugins/clean-signals-frust` pin — never one alone. A local `clean-signals-rs` checkout is
  still useful for iterating on `clean-signals` itself via a `[patch]` override, but is optional.
- Rust (stable, 2024 edition) plus the Frust CLI (`frust`) for device builds/flavors.
- Android: the Frust CLI's own Android prerequisites (NDK, `cargo-ndk`, JDK 17) — see the Frust
  checkout's `docs/DEVELOPMENT.md`.
- iOS: macOS host, Xcode, the `aarch64-apple-ios`/`aarch64-apple-ios-sim` Rust targets.

## Build

```bash
cargo build            # debug
cargo build --release  # release
```

## Codegen (`muxr.v1` gRPC)

`build.rs` compiles `proto/muxr.proto` into `$OUT_DIR/muxr.v1.rs`, which
`src/net/proto.rs` includes; generated code is build output and is never
committed. **No `protoc` install is required** — on any machine, in CI, or for a
cross-compile: `protox` is a protobuf compiler written in Rust, and its
`FileDescriptorSet` goes straight to `tonic-prost-build`'s `compile_fds`
(`prost-build`'s own path would shell out to a system `protoc`). Nothing to
install; `cargo build` is the whole story.

`proto/muxr.proto` is a **vendored copy** of `muxrd/proto/muxr.proto` from the
`muxr-core` repo at rev `ec6b032` (recorded in the file's own header). To
refresh it against a newer server:

```bash
cp <muxr-core>/muxrd/proto/muxr.proto proto/muxr.proto   # then re-add the header block
# update the rev recorded in that header, and rebuild
```

Re-copy the file wholesale and never hand-patch a message: wire drift then shows up as a codegen
or type error at the call sites that consumed the old shape, instead of a silent decode mismatch
against a live server. **One recorded exception:** `AttachReq`'s three `resume_*` fields (4/5/6)
are hand-patched ahead of rev `ec6b032`, sourced from muxr-core's unmerged
`fix/herdr-resumable-attach` (`cf2154e`) — no merged rev exists yet to re-copy from (the vendored
file's own header carries this as a tripwire). Retires at the next wholesale re-copy, once that
branch merges or releases.

Because the gRPC stack brings TLS with a C-built crypto provider (**aws-lc-rs**,
rustls' default and tonic's `tls-aws-lc` feature — the one provider in the tree,
since a second one is a runtime install conflict), a change to the network layer
is checked against both device targets, not just the host:

```bash
cargo ndk -t arm64-v8a check      # Android arm64 (reads ANDROID_NDK_HOME)
cargo check --target aarch64-apple-ios
```

`cargo ndk` reads `ANDROID_NDK_HOME`; nothing in this repo or its Gradle rig
pins an NDK version. aws-lc-rs' Android cross-compile needs **NDK r27.3 or
newer** (older r27 carried a CMake bug that broke it) — r28.2
(`28.2.13676358`) is what this rig is verified against. If a build machine is
stuck on an older NDK, the recorded fallback is swapping tonic's `tls-aws-lc`
feature for `tls-ring` (and rustls/rcgen to their `ring` backends) — a
whole-tree provider swap, never a per-crate one.

## Run

```bash
cargo run                              # desktop preview
frust run --flavor develop             # device/simulator: Develop flavor/scheme
frust run --flavor production          # device/simulator: Production flavor/scheme
```

The cold-start splash (`docs/ARCHITECTURE.md`'s UI Substrate section, `splash` bullet) plays on
`cargo run` too — it is gated per **process launch**, so it replays every time the desktop binary
restarts, but never on a warm in-process navigation back to `/`.

## Test Rig (Live `muxrd`)

Exercising the app against a real server (rather than the mock/headless suites) needs a `muxr-core`
checkout running `muxrd` in Docker, reachable from the device:

```bash
cd <muxr-core>
BIND_ADDR=<host-ip> ./docker/run.sh --host <host-ip>            # e.g. a tailscale IP: 100.99.99.127
BIND_ADDR=<host-ip> ./docker/run.sh --host <host-ip> --herdr    # the herdr backend — same ports,
                                                                 # run one variant at a time
```

`<host-ip>` must be reachable from the device (a LAN/tailscale IP, never `localhost`) and must match
the IP entered in `muxrctl`'s pairing setup below — that is what lands in the server cert's SAN.

Pair from the rig's own `muxrctl` TUI (`ssh -t root@<host-ip> -p 2222`): **Configure** (enter the
identical `<host-ip>`) → **Cert** → **Tokens** → **Server** → **Pair QR**, then scan it from the
app's Connect screen. No SSH access: mint a token directly —
`docker exec muxr-grpc-rig muxrd create-token --name <token-name>`.

Attaching a second `zellij` client to the same session resizes the phone's view to the smaller of
the two clients' dimensions — expected `zellij` behaviour, not an app bug.

## Test

```bash
cargo test                                   # all tests (unit + tests/*.rs integration suites)
cargo test --test routes                     # one integration suite
```

## Verify Gate

```bash
cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
```

Run this before committing.

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

Android, without the CLI:

```bash
cd android && ./gradlew :app:assembleDevelopDebug
cd android && ./gradlew :app:assembleProductionRelease
```

## Android Signing

Release signing reads `android/app/keystores/key.properties` (gitignored; see
`android/app/keystores/README.md` for the one-time keystore-generation steps), with
`develop.*`/`prod.*` keys (`storeFile`, `storePassword`, `keyAlias`, `keyPassword`), or the
equivalent `DEVELOP_*`/plain `*_KEYSTORE_*` env vars for CI. With neither present, both flavors'
release builds fall back to debug signing so a keystore-less dev machine still builds — Gradle
logs a loud warning (`[muxr] release variant <variantName> is DEBUG-SIGNED: ...`) whenever this
fallback fires, covering every release-artifact entry point Gradle's own task-naming convention
exposes for the variant: `assemble<Flavor>Release` (APK), `bundle<Flavor>Release` (the AAB —
the Play-listing artifact; the `frust` CLI's own `appbundle` artifact maps to this task), and any
`package<Flavor>Release*` task (the direct packaging tasks `assemble`/`bundle` build on top of).
Real keystores are generated once and stored externally — never committed.

**Signing-path conflict with the `frust` CLI:** the CLI's own release gate
(`frust-drive`'s `android_build::signing`) checks for `android/key.properties` — a different
path this Gradle rig never reads. Until the CLI accepts this rig's `app/keystores/key.properties`
path (a separately-filed frust issue), flavored RELEASE builds go through
`./gradlew :app:assemble<Flavor>Release` directly (see Flavors, above) rather than
`frust build --release`. Do **not** create `android/key.properties` as a workaround to satisfy
the CLI gate: the CLI would pass, but Gradle ignores that file entirely, so the build silently
stays debug-signed — the warning above is the tripwire that catches this.

## iOS Signing

No `key.properties` equivalent: Xcode resolves the signing identity from the Apple Developer
Team ID (`frust.toml`'s `[ios] team`), matched to a provisioning profile on the signing machine.
