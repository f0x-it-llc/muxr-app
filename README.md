# Muxr

Muxr is a mobile client for remote terminal-multiplexer sessions, zellij and
herdr. It talks to `muxrd`, the companion daemon in
[muxr-core](https://github.com/f0x-it-llc/muxr-core), over TLS and
bearer-token gRPC. It runs on Android and iOS, on phones and tablets.

## Get the app

<a href="https://apps.apple.com/app/muxr-zellij-herdr-client/id6815185967"><img src="docs/badges/app-store.svg" alt="Download on the App Store" height="44"></a>
<a href="https://play.google.com/store/apps/details?id=com.it.f0x.muxr"><img src="docs/badges/google-play.png" alt="Get it on Google Play" height="44"></a>

New here? Follow the [quick start](https://muxr.app/docs.html).

## Features

- Connect to a `muxrd` server by scanning a QR pairing code, or from a list of saved servers.
- Switch between servers without leaving the app.
- Browse the sessions on a server and open, create or manage them.
- A terminal with a key bar and a tree of spaces, tabs and panes.
- An agents list across sessions.
- Settings for tokens, fonts and appearance.
- Muxr Pro unlocks more than one server.

## Requirements

- Rust (stable)
- The [`frust`](https://frust.dev/) CLI
- Android SDK and NDK, for Android builds
- Xcode, for iOS builds

## Build and run

The verify gate:

```
cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
```

Desktop preview:

```
cargo run
```

Device builds:

```
frust run --flavor develop
frust run --flavor production
```

## Project layout

- `src/features/<slice>/` holds one feature per slice (connect, servers, sessions, settings, terminal, pro), split into domain, data and presentation.
- `src/net/` is the gRPC client, TLS and authentication.
- `src/ui/` holds the shared widgets and painting helpers.
- `src/fonts/` is the font catalog and downloader.
- `tests/` holds the headless test suites.
- `proto/muxr.proto` is vendored from muxr-core.
- `docs/` holds design mocks, icon masters and the store badges.

## Contributing

Pull requests are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) and [CLA.md](CLA.md).

## License

[PolyForm Noncommercial License 1.0.0](LICENSE)

Required Notice: Copyright (c) 2026 F0X IT LLC

The software is free to use, modify and share for noncommercial purposes, including building it and installing it on your own devices. Commercial use needs a license from F0X IT LLC.

Apple and the Apple logo are trademarks of Apple Inc. Google Play and the Google Play logo are trademarks of Google LLC. The badge artwork under docs/badges/ is not covered by this repository's license.
