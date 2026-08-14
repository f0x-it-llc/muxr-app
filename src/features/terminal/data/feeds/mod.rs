//! [`TerminalFeed`](crate::features::terminal::domain::feed::TerminalFeed)
//! implementations — the byte-stream half of this feature's data layer:
//! [`demo_feed::DemoFeed`], a checked-in replay, and
//! [`grpc_feed::GrpcTerminalFeed`], one `AttachTerminal` bidirectional stream
//! with a bounded reconnect schedule.

pub mod demo_feed;
pub mod grpc_feed;
