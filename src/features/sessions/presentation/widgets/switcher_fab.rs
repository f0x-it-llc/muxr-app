//! The Sessions screen's switcher FAB: a Glyph floating action button in the
//! shell scaffold's `fab` slot (bottom-right, the scaffold owns placement)
//! that raises the server switcher stage.
//!
//! The 0.6.5 FAB is pointer-only (frust act_000001a197874048qAwbVt2Y): it is
//! named for assistive technology through its tooltip but has no keyboard or
//! shortcut activation. That is the framework's limit, not worked around here.

use std::sync::Arc;

use frust::IconSource;
use frust_glyph::{FabView, fab};

use crate::features::servers::presentation::controller::ServersSwitcherController;

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
