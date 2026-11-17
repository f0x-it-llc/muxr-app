//! Server settings' identity card: who the connected server is, in one block. An avatar tile,
//! the saved label over the endpoint, a `connected` chip, a rule and one stat cell per fact the
//! five connection rows used to carry (TLS posture, latency, a version per backend).

use frust::authoring::text::TextOverflow;
use frust::{
    Axis, EdgeInsets, FlexView, Padding, SizedBox, View, divider, flexible, inflexible, text,
};
use frust_glyph::{BadgeVariant, avatar, badge, glyph_card};

use crate::connection::ServerInfo;
use crate::ui::palette::{Palette, active_theme, inked};
use crate::ui::spacing;

use super::groups::{DISCONNECTED_VALUE, tls_posture, version_number};
use super::rows::{ROW_GAP, ROW_LINE_GAP, ValueTone, centered_row, stretch_column};
use super::saved_servers::CONNECTED_META;

const AVATAR_SIZE: f64 = 40.0;
const NO_INITIAL: &str = "?";
const STAT_GAP: f64 = spacing::BASE;
const SECTION_GAP: f64 = spacing::BASE;

/// One labelled fact in the card's stat row.
pub(in super::super) struct Stat {
    pub label: &'static str,
    pub value: String,
    pub tone: ValueTone,
}

/// The stats, in card order: TLS, latency, then one cell per backend the server reports. A server
/// reporting no backend list keeps the single zellij cell the version row used to print.
pub(in super::super) fn identity_stats(info: Option<&ServerInfo>) -> Vec<Stat> {
    let plain = |label, value: String| Stat {
        label,
        value,
        tone: ValueTone::Plain,
    };
    let Some(info) = info else {
        return vec![
            plain("tls", DISCONNECTED_VALUE.to_string()),
            plain("latency", DISCONNECTED_VALUE.to_string()),
            plain("zellij", DISCONNECTED_VALUE.to_string()),
        ];
    };
    let (posture, tone) = tls_posture(info.trust_mode);
    let mut stats = vec![
        Stat {
            label: "tls",
            value: posture.to_string(),
            tone,
        },
        plain("latency", format!("{}ms", info.latency_ms)),
    ];
    if info.backend_versions.is_empty() {
        stats.push(plain("zellij", version_number(&info.version).to_string()));
    } else {
        stats.extend(info.backend_versions.iter().map(|backend| {
            plain(
                super::groups::backend_stat_label(backend.backend),
                backend.version.clone(),
            )
        }));
    }
    stats
}

/// The label's first letter, uppercased; empty falls back to a question mark.
fn initial(label: &str) -> String {
    label
        .chars()
        .find(|c| !c.is_whitespace())
        .map_or_else(|| NO_INITIAL.to_string(), |c| c.to_uppercase().to_string())
}

/// `label` is the saved label (or the host); `None` while nothing is connected.
pub(in super::super) fn identity_card<S: 'static>(
    info: Option<&ServerInfo>,
    label: Option<&str>,
    colors: &Palette,
) -> impl View<S> + use<S> {
    let theme = active_theme();
    let shown = label.unwrap_or(DISCONNECTED_VALUE);

    let tile = avatar(initial(shown))
        .size(AVATAR_SIZE)
        .background(colors.raised)
        .foreground(colors.success)
        .border_color(colors.raised);
    let mut names = vec![inflexible(
        text(shown.to_string())
            .style(inked(&theme.type_scale.title_small_emphasized, colors.fg))
            .max_lines(1)
            .overflow(TextOverflow::Ellipsis),
    )];
    if let Some(info) = info {
        names.push(inflexible(
            text(info.endpoint.clone())
                .style(inked(&theme.type_scale.label_small, colors.fg_dim))
                .max_lines(1)
                .overflow(TextOverflow::Ellipsis),
        ));
    }
    let mut header = vec![
        inflexible(tile),
        flexible(
            1,
            FlexView::new(Axis::Vertical, names)
                .cross_axis(frust::CrossAxisAlignment::Stretch)
                .gap(ROW_LINE_GAP),
        ),
    ];
    if info.is_some() {
        header.push(inflexible(badge(CONNECTED_META, BadgeVariant::Success)));
    }

    let cells = identity_stats(info)
        .into_iter()
        .map(|stat| {
            let ink = match stat.tone {
                ValueTone::Plain => colors.fg,
                ValueTone::Ok => colors.success,
                ValueTone::Warn => colors.warning,
            };
            flexible(
                1,
                stretch_column((
                    text(stat.label.to_uppercase())
                        .style(inked(&theme.type_scale.label_small, colors.fg_dim)),
                    text(stat.value).style(inked(&theme.type_scale.label_large, ink)),
                ))
                .gap(ROW_LINE_GAP),
            )
        })
        .collect();

    let body = stretch_column((
        centered_row(header).gap(ROW_GAP),
        SizedBox::<S>(None, Some(SECTION_GAP)),
        divider(colors.rule),
        Padding(
            EdgeInsets {
                left: 0.0,
                top: SECTION_GAP,
                right: 0.0,
                bottom: 0.0,
            },
            centered_row(cells).gap(STAT_GAP),
        ),
    ));
    glyph_card::<S>().desc(body)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::connection::{BackendVersion, ServerBackend, TrustMode};

    fn info(trust_mode: TrustMode, backends: &[(ServerBackend, &str)]) -> ServerInfo {
        ServerInfo {
            endpoint: "devbox.lan:7443".to_string(),
            trust_mode,
            tls_label: String::new(),
            version: "zellij 0.45.1".to_string(),
            latency_ms: 41,
            backend_versions: backends
                .iter()
                .map(|(backend, version)| BackendVersion {
                    backend: *backend,
                    version: version.to_string(),
                })
                .collect(),
            available_backends: Vec::new(),
            capabilities: BTreeSet::new(),
            is_read_only: false,
        }
    }

    fn cells(stats: &[Stat]) -> Vec<(&'static str, &str)> {
        stats
            .iter()
            .map(|stat| (stat.label, stat.value.as_str()))
            .collect()
    }

    #[test]
    fn a_two_backend_server_gets_a_cell_per_backend_after_tls_and_latency() {
        let both = info(
            TrustMode::System,
            &[
                (ServerBackend::Zellij, "0.45.1"),
                (ServerBackend::Herdr, "0.9.5"),
            ],
        );
        assert_eq!(
            cells(&identity_stats(Some(&both))),
            [
                ("tls", "public CA"),
                ("latency", "41ms"),
                ("zellij", "0.45.1"),
                ("herdr", "0.9.5"),
            ]
        );
    }

    #[test]
    fn a_single_backend_server_gets_only_its_own_cell() {
        let herdr = info(TrustMode::Pinned, &[(ServerBackend::Herdr, "0.9.5")]);
        assert_eq!(
            cells(&identity_stats(Some(&herdr))),
            [
                ("tls", "pinned certificate"),
                ("latency", "41ms"),
                ("herdr", "0.9.5")
            ]
        );
    }

    #[test]
    fn a_server_reporting_no_backend_list_keeps_the_zellij_cell_from_its_version() {
        let legacy = info(TrustMode::System, &[]);
        let stats = identity_stats(Some(&legacy));
        assert_eq!(
            stats.last().map(|stat| (stat.label, stat.value.as_str())),
            Some(("zellij", "0.45.1"))
        );
    }

    #[test]
    fn only_the_validation_skipping_posture_is_toned_as_a_warning() {
        let insecure = identity_stats(Some(&info(TrustMode::NoVerification, &[])));
        assert_eq!(insecure[0].value, "insecure (plaintext)");
        assert!(insecure[0].tone == ValueTone::Warn);
        let validated = identity_stats(Some(&info(TrustMode::System, &[])));
        assert!(validated[0].tone == ValueTone::Ok);
    }

    #[test]
    fn nothing_connected_says_so_in_every_cell() {
        let stats = identity_stats(None);
        assert_eq!(stats.len(), 3);
        assert!(stats.iter().all(|stat| stat.value == DISCONNECTED_VALUE));
    }

    #[test]
    fn the_avatar_takes_the_labels_first_letter_uppercased() {
        assert_eq!(initial("devbox.lan"), "D");
        assert_eq!(initial("  \u{e9}cole"), "\u{c9}");
        assert_eq!(initial(""), NO_INITIAL);
    }
}
