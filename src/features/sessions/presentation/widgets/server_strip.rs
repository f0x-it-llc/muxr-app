//! `.conn-row`: the heartbeat dot and ring, the version meta, the live latency, the `insecure` chip
//! for the one posture that skips certificate validation, and the `read-only` chip.
//! The endpoint is left to the shell's bar; `None` renders a muted "not connected" line.

use frust::{
    CrossAxisAlignment, EdgeInsets, Padding, View, column, divider, flexible, inflexible, text,
};
use frust_glyph::{BadgeVariant, badge};

use crate::connection::{ServerInfo, TrustMode};
use crate::ui::palette::{Palette, inked};
use crate::ui::spacing;

use super::super::heartbeat::{self, HeartbeatVisual};
use super::super::pages::SessionsState;
use super::{active_theme, centered_row, h_gap};

const STRIP_PAD_X: f64 = spacing::XL;
const STRIP_PAD_Y: f64 = 13.0;
const STRIP_GAP: f64 = spacing::MD;
/// The unscaled diameter of the ping ring around the dot, so the two cannot drift apart.
pub(in crate::features::sessions::presentation) const DOT: f64 = 7.0;
const INSECURE_CHIP: &str = "insecure";
const READ_ONLY_CHIP: &str = "read-only";
const META_SEPARATOR: &str = " \u{b7} ";

pub(in crate::features::sessions::presentation) fn server_strip(
    info: Option<&ServerInfo>,
    beat: HeartbeatVisual,
    read_only: bool,
    colors: &Palette,
) -> impl View<SessionsState> {
    let theme = active_theme();
    let content = match info {
        None => centered_row(vec![inflexible(
            text("not connected").style(inked(&theme.type_scale.body_small, colors.fg_dim)),
        )]),
        Some(info) => {
            let mut children = vec![
                inflexible(heartbeat::pinging_dot(beat.ring, colors.success)),
                h_gap(STRIP_GAP),
                flexible(
                    1,
                    text(meta_line(info)).style(inked(&theme.type_scale.body_small, colors.fg_dim)),
                ),
                h_gap(STRIP_GAP),
                // Amber while the reading rolls, dim once it settles: a transition cue, not a health one.
                inflexible(text(format!("{}ms", beat.latency_ms)).style(inked(
                    &theme.type_scale.body_small,
                    if beat.rolling {
                        colors.amber
                    } else {
                        colors.fg_dim
                    },
                ))),
            ];
            if let Some(chip) = insecure_chip(info.trust_mode) {
                children.push(h_gap(STRIP_GAP));
                children.push(inflexible(badge(chip, BadgeVariant::Warning)));
            }
            if read_only {
                children.push(h_gap(STRIP_GAP));
                children.push(inflexible(badge(READ_ONLY_CHIP, BadgeVariant::Info)));
            }
            centered_row(children)
        }
    };
    // The bottom hairline is a `divider` stacked under the padded row: `Container` has no bottom-rule option.
    column()
        .child(Padding(
            EdgeInsets::symmetric(STRIP_PAD_X, STRIP_PAD_Y),
            content.cross_axis(CrossAxisAlignment::Center),
        ))
        .child(divider(colors.rule))
}

/// Every backend the handshake detected, as `"zellij 0.44.3 · herdr 0.3.1"`; a server that reported
/// none falls back to [`ServerInfo::version`].
fn meta_line(info: &ServerInfo) -> String {
    if info.backend_versions.is_empty() {
        return info.version.clone();
    }
    info.backend_versions
        .iter()
        .map(|entry| format!("{} {}", entry.backend.label(), entry.version))
        .collect::<Vec<String>>()
        .join(META_SEPARATOR)
}

/// Some only for the posture that skips certificate validation. Branches on the [`TrustMode`]
/// variant, never the label.
fn insecure_chip(mode: TrustMode) -> Option<&'static str> {
    match mode {
        TrustMode::NoVerification => Some(INSECURE_CHIP),
        TrustMode::System | TrustMode::Pinned => None,
    }
}

/// The host half of a `host:port` endpoint; `pub(crate)` because the route table's bar title resolves through it.
pub(crate) fn host_of(endpoint: &str) -> &str {
    endpoint.rsplit_once(':').map_or(endpoint, |(host, _)| host)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::{BackendVersion, ServerBackend};

    fn info(mode: TrustMode, backends: Vec<BackendVersion>) -> ServerInfo {
        ServerInfo {
            endpoint: "100.64.0.7:50051".to_string(),
            trust_mode: mode,
            tls_label: "No verification".to_string(),
            version: "zellij 0.44.3".to_string(),
            latency_ms: 36,
            backend_versions: backends,
            available_backends: Vec::new(),
            capabilities: std::collections::BTreeSet::new(),
            is_read_only: false,
        }
    }

    fn backend(backend: ServerBackend, version: &str) -> BackendVersion {
        BackendVersion {
            backend,
            version: version.to_string(),
        }
    }

    #[test]
    fn the_strips_meta_line_names_every_detected_backend() {
        assert_eq!(
            meta_line(&info(
                TrustMode::NoVerification,
                vec![
                    backend(ServerBackend::Zellij, "0.44.3"),
                    backend(ServerBackend::Herdr, "0.3.1"),
                ]
            )),
            "zellij 0.44.3 \u{b7} herdr 0.3.1"
        );
        assert_eq!(
            meta_line(&info(
                TrustMode::NoVerification,
                vec![backend(ServerBackend::Zellij, "0.44.3")]
            )),
            "zellij 0.44.3",
            "one backend prints on its own, with no dangling separator"
        );
    }

    #[test]
    fn a_server_that_reported_no_backends_falls_back_to_the_handshakes_version() {
        assert_eq!(
            meta_line(&info(TrustMode::NoVerification, Vec::new())),
            "zellij 0.44.3"
        );
    }

    #[test]
    fn only_the_validation_skipping_posture_earns_the_insecure_chip() {
        assert_eq!(insecure_chip(TrustMode::NoVerification), Some("insecure"));
        assert_eq!(insecure_chip(TrustMode::System), None);
        assert_eq!(insecure_chip(TrustMode::Pinned), None);
    }

    #[test]
    fn the_top_bars_title_is_the_host_without_its_port() {
        assert_eq!(host_of("100.64.0.7:50051"), "100.64.0.7");
        assert_eq!(host_of("mock.example:7777"), "mock.example");
        assert_eq!(host_of("mock.example"), "mock.example", "no port at all");
    }
}
