//! [`TerminalFeed`](crate::features::terminal::domain::feed::TerminalFeed)
//! implementations — the byte-stream half of this feature's data layer.
//!
//! Two, over the identical port:
//!
//! - [`demo_feed::DemoFeed`], the checked-in build-log replay every mock-era
//!   terminal screen (and every headless test, and the on-device visual gate)
//!   drives the grid from.
//! - [`grpc_feed::GrpcTerminalFeed`], the real `muxrd` session: one
//!   `AttachTerminal` bidirectional stream with a bounded reconnect schedule.
//!   Built and tested, not yet mounted — the screen's `LiveSession` still
//!   constructs the demo replay, and the cutover is its own step.

pub mod demo_feed;
pub mod grpc_feed;
