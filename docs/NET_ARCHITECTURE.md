# Muxr - NET Architecture

## Overview

NET is the app's gRPC transport and connection-state layer: `src/net/` (the generated client, TLS
trust, auth, channel) and `src/connection.rs` (the app-scoped state machine every feature reads).
Neither is a feature's own `data` layer — more than one feature's repository shares the same
channel and the same connection handle.

See [ARCHITECTURE.md](ARCHITECTURE.md) for how NET relates to the rest of the app, including the
feature slices that consume it (see [FEATURES_ARCHITECTURE.md](FEATURES_ARCHITECTURE.md)).

## Net — `src/net/`

App-level infrastructure beside `connection.rs`/`failure.rs`, not a feature's `data` layer — more
than one feature's repository shares the same channel. `proto` is the generated `muxr.v1` service
`build.rs` compiles from a vendored `muxrd` proto (`docs/DEVELOPMENT.md`'s Codegen section covers the
toolchain). `tls` holds `TrustConfig` and the three `rustls` verifiers it installs —
deliberately separate from `connection::TrustMode` (a posture label a screen renders), since the
mapping between them is the connect seam's job. `channel` builds the endpoint/`Channel` for a
`(host, port, TrustConfig)` triple; `auth` is the swappable session token and its bearer interceptor;
`client` is `MuxrClient`, the connected handle every repository holds; `error` is the one place a
`tonic::Status` becomes an `AppFailure`. `testing` (`#[cfg(test)]`) stands an in-process `muxrd`
fixture on a loopback port, so the client stack is exercised without the docker rig — and is where the
client's handshake/credential coverage lives, by necessity: `dev-dependencies` (`rcgen`, tonic's
`server`/`router`) can't link into a `tests/*.rs` integration binary.

## Connection — `src/connection.rs`

`ConnectionController` is the app's one app-scoped connection handle — published once from
`MuxrApp::init`, resolved via `connection::expect_connection_controller()`; it lives beside
`failure.rs`/`responsive.rs` since more than one feature reads it (Sessions renders `server_info`'s
latency, Settings the rest). It owns the state machine; a `ConnectionBackend` seam owns the work:
`NetConnectionBackend` (dial, `Login`, a timed `GetVersion` over `src/net`) and `MockConnectionBackend`
(the facts every headless suite runs on), unified by `ConnectRun`'s `Ready`/`Pending` sum type since a
mock settles synchronously and a real dial cannot be polled outside its own runtime.
`use_mock_connection_for_tests` opts a test process onto the mock before `MuxrApp::init` runs, the same
seam shape `settings::di`'s preferences opt-out uses. Beyond `connected` the controller publishes
`connecting` (single-flight guard) and `is_read_only` (mirrored off `ServerInfo`, the primary defence
against a read-only credential's refused mutations); `ServerInfo` carries
`backend_versions`/`available_backends`/`capabilities` and the measured `latency_ms`. `CredentialSaver`
is a one-method seam `connect` installs so an accepted save offer can mint a token and write the
secure store without this module naming a feature type. `MuxrApp::init` subscribes the controller's
failure sink to the app's toast bridge once.

## Tests

`src/net`'s own `#[cfg(test)]` suite carries the client's handshake/credential coverage against the
in-process `testing` fixture — it needs `rcgen` and tonic's server features, `[dev-dependencies]`
unreachable from a `tests/*.rs` integration binary. `tests/net_channel.rs` exercises the transport
surface from outside the crate: fail-closed pin parsing, endpoint/trust-mode construction
(system-CA included, construction only), the keepalive parity constants, the `Status`→`AppFailure`
mapping, and one genuine dial against a closed port — everything that holds with no server on the
other end. See [ARCHITECTURE.md](ARCHITECTURE.md#tests) for the shared harness.
