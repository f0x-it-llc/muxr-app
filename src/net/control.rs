//! `ControlEvent.kind` strings this client understands; the proto types `kind` as a bare
//! `string`. Unknown kinds are ignored, never an error.

/// Server-minted connection id, echoed back on a later pane/tab/space request.
pub const CONTROL_CONNECTION_ID: &str = "connection_id";

pub const CONTROL_EXIT: &str = "exit";
