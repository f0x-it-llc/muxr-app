//! [`TerminalFeed`](crate::features::terminal::domain::feed::TerminalFeed)
//! implementations — the byte-stream half of this feature's data layer.
//!
//! One today: [`demo_feed::DemoFeed`], the checked-in build-log replay every
//! mock-era terminal screen (and every headless test, and the on-device visual
//! gate) drives the grid from. The `muxrd` gRPC session lands beside it and
//! implements the identical port.

pub mod demo_feed;
