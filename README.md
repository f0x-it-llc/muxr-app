# Muxr

Muxr puts your zellij, herdr and tmux terminal sessions on your iPhone, iPad or
Android device, including the sessions your coding agents (Claude Code, Codex
and others) run in, so you can watch and steer them from your phone. It needs
`muxrd`, a daemon you run on your own machine, from
[muxr-core](https://github.com/f0x-it-llc/muxr-core).

## Get the app

<a href="https://apps.apple.com/app/muxr-zellij-herdr-client/id6815185967"><img src="docs/badges/app-store.svg" alt="Download on the App Store" height="44"></a>
<a href="https://play.google.com/store/apps/details?id=com.it.f0x.muxr"><img src="docs/badges/google-play.png" alt="Get it on Google Play" height="44"></a>

[Website](https://muxr.app/) · [Quick start](https://muxr.app/docs.html) · [Usage guide](https://muxr.app/guide.html) · [FAQ](https://muxr.app/faq.html) · [How-to guides](https://muxr.app/how-to/) · [Comparisons](https://muxr.app/compare/)

## What it does

- Attaches to remote zellij, herdr and tmux sessions through `muxrd`, over TLS with token authentication.
- Pairs with a server by scanning a QR code.
- Saves servers and switches between them without leaving the app.
- Browses the sessions on a server and opens, creates and manages them.
- Shows a terminal with a key bar and a tree of spaces, tabs and panes, driven by touch gestures.
- Uses a tablet layout with a pane rail beside the terminal.
- Lists the coding agents running on herdr: blocked, done, working or idle.
- Supports read-only tokens, for a device that should watch without typing.
- Protects saved credentials with the device's biometrics. There is no account.
- Has settings for tokens, fonts and appearance.

## Price

The app is free for one server. Muxr Pro is a one-time $19.99 purchase, 50% off ($9.99) until January 13, 2027. Pro unlocks more than one server. There is no subscription and no account.

## Status

Muxr works with zellij, herdr and tmux today. Push notifications, voice input and desktop apps are coming soon; see the [roadmap](https://muxr.app/roadmap.html).

## Not the npm package

Muxr by F0X IT LLC (muxr.app) is a zellij, herdr and tmux client and is unrelated to the npm package @trymuxr/cli at trymuxr.com.

## Building from source: requirements

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
- `proto/muxr/v1/` is vendored from muxr-core, see `proto/VENDORED.md`.
- `docs/` holds design mocks, icon masters and the store badges.

## Contributing

Pull requests are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) and [CLA.md](CLA.md).

## Changelog

See [CHANGELOG.md](CHANGELOG.md) for what changed in each release.

## License

[PolyForm Shield License 1.0.0](LICENSE)

Required Notice: Copyright (c) 2026 F0X IT LLC

The software is free to use, modify and share for any purpose, including at work, except to provide a product that competes with Muxr. A competing use needs a license from F0X IT LLC. Source published before 15 November 2026 was under the PolyForm Noncommercial License 1.0.0 and remains available under it.

Apple and the Apple logo are trademarks of Apple Inc. Google Play and the Google Play logo are trademarks of Google LLC. The badge artwork under docs/badges/ is not covered by this repository's license.
