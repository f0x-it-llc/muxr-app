//! The monitor screens' widget clusters: the host strip under the bar and the full-page states.

mod host_strip;
mod states;

pub use host_strip::{HostStrip, host_strip, uptime_label};
pub use states::{
    DISABLED_BODY, DISABLED_TITLE, OFFLINE_TITLE, UNSUPPORTED_TITLE, dimmed, disabled_state,
    offline_state, placeholder, unsupported_body, unsupported_state,
};
