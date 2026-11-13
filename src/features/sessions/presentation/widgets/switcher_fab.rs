//! The Sessions screen's switcher FAB: a Glyph floating action button in the
//! shell scaffold's `fab` slot (bottom-right, the scaffold owns placement)
//! that raises the server switcher stage.
//!
//! frust 0.6.5's FAB is pointer-only: it is
//! named for assistive technology through its tooltip but has no keyboard or
//! shortcut activation. That is the framework's limit, not worked around here.

use std::sync::Arc;

use frust::{IconSource, WindowMetrics, use_context};
use frust_glyph::{FabView, fab};

use crate::features::servers::presentation::controller::ServersSwitcherController;
use crate::responsive::Responsive;

/// The FAB's accessible name (a Button node with this name).
pub const SWITCHER_FAB_LABEL: &str = "Servers";

/// Two stacked layers, on the 24 px design box.
const STACK_MARK: IconSource = IconSource {
    d: "M 12 3 L 2 8.5 L 12 14 L 22 8.5 Z M 4.6 12.2 L 2 13.5 L 12 19 L 22 13.5 L 19.4 12.2 L 12 16.3 Z M 4.6 17.2 L 2 18.5 L 12 24 L 22 18.5 L 19.4 17.2 L 12 21.3 Z",
    design: 24.0,
};

/// The FAB that opens the stage over a fresh strip.
pub fn switcher_fab<S: 'static>(switcher: &Arc<ServersSwitcherController>) -> FabView<S> {
    let switcher = Arc::clone(switcher);
    fab(STACK_MARK, move |_state: &mut S| switcher.open()).tooltip(SWITCHER_FAB_LABEL)
}

/// The window's responsive rule for the shell page, read once per
/// `shell_page` build (which rebuilds on resize). The lookup lives here, in a
/// presentation file, because the layering tripwire sanctions context reads
/// only there; the shell passes the value down and never reads per widget.
pub fn shell_responsive() -> Responsive {
    Responsive::of(use_context::<WindowMetrics>())
}
