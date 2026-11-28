//! The strip under the bar: a status dot, the host's OS, arch and uptime, and the stream's state.

use std::time::Duration;

use frust::{
    Axis, CrossAxisAlignment, EdgeInsets, FlexView, Padding, SizedBox, View, colored_box, divider,
    flexible, inflexible, text,
};

use crate::features::monitor::domain::entities::HostInfo;
use crate::features::monitor::presentation::controllers::Link;
use crate::ui::paint::{GLOW_SPREAD, GLOW_STD_DEV, RADIUS_FULL};
use crate::ui::palette::{Palette, active_theme, inked};
use crate::ui::recency::age_label;
use crate::ui::spacing;

const STRIP_PAD_X: f64 = spacing::XL;
const STRIP_PAD_Y: f64 = 13.0;
const STRIP_GAP: f64 = spacing::MD;
const DOT: f64 = 7.0;
const SEPARATOR: &str = " \u{b7} ";
const RECONNECTING: &str = "reconnecting";
const HOST_PENDING: &str = "reading host\u{2026}";
const SECS_PER_MINUTE: u64 = 60;
const SECS_PER_HOUR: u64 = 3_600;
const SECS_PER_DAY: u64 = 86_400;

pub struct HostStrip<'a> {
    pub link: Link,
    pub host: Option<&'a HostInfo>,
    pub interval_ms: u32,
    pub paused: bool,
    pub now_ms: i64,
}

pub fn uptime_label(uptime: Duration) -> String {
    let secs = uptime.as_secs();
    let (days, hours, minutes) = (
        secs / SECS_PER_DAY,
        secs % SECS_PER_DAY / SECS_PER_HOUR,
        secs % SECS_PER_HOUR / SECS_PER_MINUTE,
    );
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        "<1m".to_string()
    }
}

fn identity(host: &HostInfo, now_ms: i64) -> String {
    let mut parts = vec![format!("{} {}", host.os_name, host.os_version)];
    if !host.arch.is_empty() {
        parts.push(host.arch.clone());
    }
    if let Some(boot) = host.boot_unix_s {
        let uptime = u64::try_from(now_ms / 1_000 - boot).unwrap_or(0);
        parts.push(format!("up {}", uptime_label(Duration::from_secs(uptime))));
    }
    parts.join(SEPARATOR)
}

fn since(since_unix_ms: i64, now_ms: i64) -> Duration {
    Duration::from_millis(u64::try_from(now_ms - since_unix_ms).unwrap_or(0))
}

pub fn host_strip<S: 'static>(strip: &HostStrip<'_>, colors: &Palette) -> impl View<S> + use<S> {
    let theme = active_theme();
    let (tone, left, right) = match strip.link {
        Link::Stale { since_unix_ms } => (
            colors.warning,
            format!(
                "{RECONNECTING}{SEPARATOR}last sample {}",
                age_label(since(since_unix_ms, strip.now_ms))
            ),
            "stale".to_string(),
        ),
        _ => {
            let left = strip
                .host
                .map_or_else(|| HOST_PENDING.to_string(), |h| identity(h, strip.now_ms));
            let seconds = strip.interval_ms / 1_000;
            if strip.paused {
                (colors.fg_faintest, left, "paused".to_string())
            } else {
                (colors.success, left, format!("live{SEPARATOR}{seconds}s"))
            }
        }
    };
    let ink = if matches!(strip.link, Link::Stale { .. }) {
        colors.warning
    } else {
        colors.fg_dim
    };
    let dot = colored_box::<S>()
        .size(DOT, DOT)
        .radius(RADIUS_FULL)
        .fill(tone)
        .glow(tone, GLOW_STD_DEV, GLOW_SPREAD);
    let row = FlexView::new(
        Axis::Horizontal,
        vec![
            inflexible(dot),
            inflexible(SizedBox::<S>(Some(STRIP_GAP), None)),
            flexible(
                1,
                text(left).style(inked(&theme.type_scale.body_small, ink)),
            ),
            inflexible(SizedBox::<S>(Some(STRIP_GAP), None)),
            inflexible(text(right).style(inked(&theme.type_scale.body_small, ink))),
        ],
    )
    .cross_axis(CrossAxisAlignment::Center);
    frust::column()
        .child(Padding(
            EdgeInsets::symmetric(STRIP_PAD_X, STRIP_PAD_Y),
            row,
        ))
        .child(divider(colors.rule))
        .cross_axis(CrossAxisAlignment::Stretch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uptime_reads_in_its_two_largest_units() {
        let label = |secs| uptime_label(Duration::from_secs(secs));
        assert_eq!(label(30), "<1m");
        assert_eq!(label(5 * 60), "5m");
        assert_eq!(label(3 * 3_600 + 12 * 60), "3h 12m");
        assert_eq!(label(4 * 86_400 + 3 * 3_600), "4d 3h");
    }

    #[test]
    fn identity_names_os_arch_and_uptime() {
        let host = HostInfo {
            os_name: "Arch Linux".to_string(),
            os_version: "rolling".to_string(),
            arch: "x86_64".to_string(),
            boot_unix_s: Some(1_000),
            ..HostInfo::default()
        };
        assert_eq!(
            identity(&host, (1_000 + 2 * 3_600 + 16 * 60) * 1_000),
            "Arch Linux rolling \u{b7} x86_64 \u{b7} up 2h 16m"
        );
        let no_boot = HostInfo {
            boot_unix_s: None,
            ..host
        };
        assert_eq!(identity(&no_boot, 0), "Arch Linux rolling \u{b7} x86_64");
    }
}
