//! `ControlEvent.kind` values this client understands on an `AttachTerminal`
//! stream. Hand-written because `proto/muxr.proto` types `kind` as a bare
//! `string`; named once here rather than in the terminal feature so the live
//! feed and the `net::testing` fixture can't drift into mismatched literals.
//! Unknown kinds are ignored, never an error.

/// Server-minted connection id, echoed back on a later pane/tab/space request.
pub const CONTROL_CONNECTION_ID: &str = "connection_id";

/// Clean server-side stream close — nothing more will arrive.
pub const CONTROL_EXIT: &str = "exit";
