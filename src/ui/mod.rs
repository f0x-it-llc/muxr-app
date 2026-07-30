//! Shared, cross-feature widget helpers. `examples/huddle`'s `ui/` (toast,
//! sheet, scaffold, fill_box, …) is the shape this module grows into as muxr
//! needs its own app-wide chrome shared by more than one feature.
//!
//! [`splash`] is the first addition: the cold-start splash sequence's
//! paint-driven widgets (see `docs/splash.html`/`docs/muxr-app-icons` for the
//! design reference). Nothing there is mounted yet — a later task composes
//! the pieces into an orchestrator and wires it into the route table.

pub mod splash;
