# Changelog

What changed in each release of the Muxr app for iPhone, iPad and Android.
Versions are the store versions, and dates are release dates.

## 1.0.3 - Unreleased

### Added

- Settings is now three screens: app settings (terminal appearance, layout refresh, bell, Muxr Pro, saved servers and about), server settings for the connected server only (endpoint, TLS, latency, backend versions, saved credential, forget, lock and disconnect), and an auth tokens page (filter, read-write or read-only, copy name, revoke and create).
- App settings opens without a server, from a gear on the Connect screen.
- Server settings shows the muxrd version the server runs, with a chip when the server reports a newer release.
- After connecting to, or switching to, a server with a newer release, the app bar shows "muxrd <version> is available" for a few seconds, once per connection. The notice needs muxrd 0.5.5 or later; older servers show their version only.
- Paste: a long press on a blank cell offers Paste, and a selection offers Copy and Paste. Pasted text has control characters removed and is sent as bracketed paste.

### Changed

- On herdr, new panes, tabs and spaces open in the space the phone is viewing, and no longer move the focus of a desktop client. This needs muxrd 0.5.4 or later.
- Typing `exit` in the attached pane no longer closes the screen. It shows "No pane open" so you can pick another pane.
- On tablets, a pane rail card is a single press target.
- Switching to another saved server opens that server's sessions.
- Android 10 or later is required.

### Fixed

- A connection that finishes while a settings screen is open always lands on Sessions, where Back asks before disconnecting.
- Deleting a saved credential or forgetting a server revokes its token only when that server is the one connected.
- The token list reloads after switching servers, and never shows another server's tokens.
- Forgetting a server while the app is retrying to reconnect to it stops the retries.
- When a pane was created but the screen could not move onto it, the app says so and refreshes, instead of reporting a failure.

Earlier releases predate this file.
