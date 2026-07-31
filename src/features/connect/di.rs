//! `connect`'s composition-root wiring seam.
//!
//! [`provide`] is called exactly once, from `crate::MuxrApp::init`, alongside
//! every other feature's own `di::provide` — feature-owned wiring, not the
//! root's own concern (mirrors `examples/huddle`'s `HuddleApp::init`, which
//! constructs and `provide_context`s each feature's repository the same way).
//! Empty today: `connect` has no `data`-layer repository yet to construct and
//! publish. The seam itself is the deliverable — Wave 2's connect flow grows
//! a real `provide_context`-ed repository here.

/// Wire this feature's own dependencies into context. A no-op today — see
/// the [module docs](self).
pub fn provide() {}
