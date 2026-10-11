//! `.conn-row` — the server-info strip under the top bar: the heartbeat's
//! glowing success dot and ping ring, the dim `zellij 0.44.3 · herdr 0.3.1`
//! version meta, the live `36ms` latency, and — only for the posture that
//! actually skips certificate validation — the `insecure` warning chip, then
//! the cyan `read-only` chip for a read-only credential. Closed by a hairline
//! rule.
//!
//! **No endpoint here.** The shell's floating bar already titles the
//! connected host, so the meta text takes the flexible slot instead and
//! nothing in the row can overlap at any width. Latency lives here only:
//! Settings renders the endpoint/TLS/versions again, but a live round-trip
//! time belongs next to the session list it gates (see
//! [`super::super::heartbeat`] for what keeps it live).
//!
//! `None` renders a muted "not connected" line rather than nothing: the
//! session guard makes it unreachable in practice, but a silent gap would be
//! the worse failure if it ever were.

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

/// `.conn-row{padding:13px 16px}` — the Y value has no on-scale
/// `ui::spacing` match and stays a local one-off.
const STRIP_PAD_X: f64 = spacing::XL;
const STRIP_PAD_Y: f64 = 13.0;
/// `.conn-row{gap:10px}`.
const STRIP_GAP: f64 = spacing::MD;
/// `.dot-ok{width:7px;height:7px}` — and, through
/// [`super::super::heartbeat`], the unscaled diameter of the ping ring that
/// expands around it, so the two cannot drift apart.
pub(in crate::features::sessions::presentation) const DOT: f64 = 7.0;
/// The label the strip prints for the one TLS posture that skips
/// certificate validation — the source's `.warn-badge`.
const INSECURE_CHIP: &str = "insecure";
/// The chip a read-only credential adds after it.
const READ_ONLY_CHIP: &str = "read-only";
/// The separator between the strip's per-backend versions
/// (`zellij 0.44.3 · herdr 0.3.1`).
const META_SEPARATOR: &str = " \u{b7} ";

/// See the [module docs](self).
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
                // Amber while the reading rolls, dim once it settles — the one
                // place this screen inks with the brand hue for something
                // other than a call to action, and a *transition* cue rather
                // than a health one (see the module docs' colour rule).
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
    // `.conn-row`'s bottom hairline is a plain `divider` stacked under the
    // padded row rather than a knob on the container — `Container` carries no
    // bottom-rule option by design (see `frust-widgets::container`'s module
    // docs), so the rule composes as an ordinary second child instead of a
    // paint-time overlay.
    column()
        .child(Padding(
            EdgeInsets::symmetric(STRIP_PAD_X, STRIP_PAD_Y),
            content.cross_axis(CrossAxisAlignment::Center),
        ))
        .child(divider(colors.rule))
}

/// The strip's dim meta text: every backend the handshake detected, as
/// `"zellij 0.44.3 · herdr 0.3.1"`. A server that reported no backends at
/// all (an older daemon) falls back to [`ServerInfo::version`], already
/// spelled in exactly this `<label> <version>` shape.
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

/// The `.warn-badge` the strip prints for a TLS posture — `Some` only for
/// the one posture that actually skips certificate validation, since the
/// redesign flags a risk rather than narrating every posture (Settings
/// prints the full [`ServerInfo::tls_label`] instead). Branches on the
/// [`TrustMode`] variant, never the label: the Connect screen's own trust
/// selector marks the same posture with the same `BadgeVariant::Warning`
/// chip, from the same enum.
fn insecure_chip(mode: TrustMode) -> Option<&'static str> {
    match mode {
        TrustMode::NoVerification => Some(INSECURE_CHIP),
        TrustMode::System | TrustMode::Pinned => None,
    }
}

/// The host half of a `host:port` endpoint — what the top bar's centered
/// title shows (`100.71.31.57`). `pub(crate)` because `crate::routes`'
/// `BarTitle::ConnectedHost` resolves `/sessions`' bar title through this
/// one function rather than minting a second copy of the split.
pub(crate) fn host_of(endpoint: &str) -> &str {
    endpoint.rsplit_once(':').map_or(endpoint, |(host, _)| host)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::{BackendVersion, ServerBackend};

    fn info(mode: TrustMode, backends: Vec<BackendVersion>) -> ServerInfo {
        ServerInfo {
            endpoint: "100.71.31.57:50051".to_string(),
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
        assert_eq!(host_of("100.71.31.57:50051"), "100.71.31.57");
        assert_eq!(host_of("mock.example:7777"), "mock.example");
        assert_eq!(host_of("mock.example"), "mock.example", "no port at all");
    }
}
