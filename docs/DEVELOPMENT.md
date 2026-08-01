# Muxr - Development Guide

## Prerequisites

- A [Frust](https://github.com/f0x-it-llc/frust) checkout, with this repo as its
  `apps/muxr-app` — `Cargo.toml`'s `frust`/`clean-signals-frust` dependencies are **path**
  dependencies into that checkout (`../../crates/frust`, `../../plugins/clean-signals-frust`),
  a temporary pre-publication arrangement documented verbatim in `Cargo.toml`'s comments. Once
  Frust crates are published this app will depend on versions instead.
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

## Run

```bash
cargo run                              # desktop preview
frust run --flavor develop             # device/simulator: Develop flavor/scheme
frust run --flavor production          # device/simulator: Production flavor/scheme
```

The cold-start splash (`docs/ARCHITECTURE.md`'s Cold-Start Splash section) plays on `cargo run`
too — it is gated per **process launch**, so it replays every time the desktop binary restarts,
but never on a warm in-process navigation back to `/`.

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
