//! `settings`'s composition-root wiring seam.
//!
//! [`provide`] is called exactly once, from `crate::MuxrApp::init`, alongside
//! every other feature's own `di::provide` — feature-owned wiring, not the
//! root's own concern (mirrors `examples/huddle`'s `HuddleApp::init`, which
//! constructs and `provide_context`s each feature's repository the same way).
//! Empty today: `settings` has no `data`-layer repository yet to construct
//! and publish. The seam itself is the deliverable — a later task grows a
//! real `provide_context`-ed repository here, once app preferences have real
//! domain/data logic behind them.

/// Wire this feature's own dependencies into context. A no-op today — see
/// the [module docs](self).
pub fn provide() {}
