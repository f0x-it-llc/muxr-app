//! The monitor screens' widget clusters: the host strip under the bar, the full-page states and the overflow menu.

mod host_strip;
mod menu;
mod states;

pub use host_strip::{HostStrip, host_strip, uptime_label};
pub use menu::{MENU_TITLE, PAUSE_LABEL, RESUME_LABEL, menu_body};
pub use states::{
    DISABLED_BODY, DISABLED_TITLE, OFFLINE_TITLE, UNSUPPORTED_TITLE, dimmed, disabled_state,
    offline_state, placeholder, unsupported_body, unsupported_state,
};
