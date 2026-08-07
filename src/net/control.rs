//! The `ControlEvent.kind` values this client understands on an
//! `AttachTerminal` stream.
//!
//! `proto/muxr.proto` types `ControlEvent.kind` as a bare `string` and
//! enumerates nothing, so codegen cannot hand these over the way it hands over
//! a field number, an RPC name or a message shape. They are facts about what
//! `muxrd` actually sends, and hand-written code has to name them somewhere —
//! the same carve-out [`auth`](super::auth)'s
//! [`AUTHORIZATION_KEY`](super::auth::AUTHORIZATION_KEY) takes for a metadata
//! key the proto likewise cannot carry.
//!
//! **Why they live in `net` rather than in the terminal feature.** Two places
//! must say the identical thing: `terminal`'s live feed
//! (`features::terminal::data::feeds::grpc_feed`), which parses these off the
//! stream, and this layer's own in-process `muxrd` fixture (`net::testing`),
//! which sends them. The app's dependency direction is features → `net` and
//! never back, so a constant owned by a feature would make the transport layer
//! import one; two independent string literals would be worse still, since the
//! drift a green fixture test then hides is exactly the bug worth catching.
//! Naming a wire value once, beside [`proto`](super::proto), is what keeps both
//! properties.
//!
//! Every other kind is *ignored* by the feed, never an error, so a newer
//! `muxrd` can add one without breaking this client.

/// The control event carrying a stream's server-minted connection id — the
/// per-connection routing token `muxrd` expects echoed back on a later
/// pane/tab/space request (`proto/muxr.proto`'s own "echoed from
/// AttachTerminal's minted id" fields).
pub const CONTROL_CONNECTION_ID: &str = "connection_id";

/// The control event announcing a clean server-side close: the session is over
/// for good, and nothing more will arrive on the stream.
pub const CONTROL_EXIT: &str = "exit";
