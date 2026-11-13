//! Presentation of the agent vocabulary, shared by sessions and terminal so it imports neither.
//! Kind accents are design constants, the one place literal hex is allowed here.

use frust::authoring::Color;
use frust::{
    Align, Alignment, AnyView, EdgeInsets, Padding, SizedBox, View, any, colored_box, container,
    stack,
};
use frust_glyph::{BadgeVariant, avatar, badge};

use crate::agents::{AgentStatus, AgentSummary};
use crate::ui::paint::{GLOW_SPREAD, GLOW_STD_DEV, HAIRLINE, RADIUS_FULL};
use crate::ui::palette::{Palette, active_theme, washed};

pub const DOT: f64 = 8.0;
/// A blocked dot's halo slot: the dot plus a 25 % ring around it.
const HALO: f64 = 14.0;
const HALO_ALPHA: f32 = 0.25;

struct KindMark {
    monogram: &'static str,
    accent: Color,
}

const CLAUDE: Color = Color::from_rgb8(0xD9, 0x77, 0x57);
const CODEX: Color = Color::from_rgb8(0x74, 0xAA, 0x9C);
const GEMINI: Color = Color::from_rgb8(0x4A, 0x8C, 0xF7);
const CURSOR: Color = Color::from_rgb8(0xE5, 0xE5, 0xE5);
const OPENCODE: Color = Color::from_rgb8(0xF0, 0xC8, 0x46);
const COPILOT: Color = Color::from_rgb8(0xA7, 0x8B, 0xFA);
const PI: Color = Color::from_rgb8(0x5E, 0xC8, 0xD8);

pub const TILE_SIZE: f64 = 22.0;
const TILE_OVERLAP: f64 = 0.3;
/// Glyph's avatar wash alpha, folded over the surface so a stacked tile is opaque.
const TILE_WASH_ALPHA: f32 = 0.12;
const UNKNOWN_MONOGRAM: &str = "?";

fn kind_mark(kind: &str, colors: &Palette) -> KindMark {
    let (monogram, accent) = match kind.to_ascii_lowercase().as_str() {
        "claude" => ("C", CLAUDE),
        "codex" => ("X", CODEX),
        "gemini" => ("G", GEMINI),
        "cursor" => ("↗", CURSOR),
        "opencode" => ("O", OPENCODE),
        "copilot" => ("P", COPILOT),
        "pi" => ("π", PI),
        _ => (UNKNOWN_MONOGRAM, colors.fg_muted),
    };
    KindMark { monogram, accent }
}

pub fn status_tone(colors: &Palette, status: AgentStatus) -> Color {
    match status {
        AgentStatus::Blocked => colors.warning,
        AgentStatus::Working => colors.success,
        AgentStatus::Done => colors.cyan,
        AgentStatus::Idle | AgentStatus::Unknown => colors.fg_dim,
    }
}

pub fn status_glyph(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Blocked => "◉",
        AgentStatus::Working => "●",
        AgentStatus::Done => "✓",
        AgentStatus::Idle => "○",
        AgentStatus::Unknown => "·",
    }
}

/// Constant slot size per status class, so a status change never reflows a row.
pub fn status_dot<S: 'static>(status: AgentStatus) -> impl View<S> {
    let colors = Palette::from_theme(&active_theme());
    let tone = status_tone(&colors, status);
    let dot = match status {
        AgentStatus::Blocked | AgentStatus::Working | AgentStatus::Done => any(colored_box::<S>()
            .size(DOT, DOT)
            .radius(RADIUS_FULL)
            .fill(tone)
            .glow(tone, GLOW_STD_DEV, GLOW_SPREAD)),
        AgentStatus::Idle | AgentStatus::Unknown => {
            any(container(SizedBox::<S>(Some(DOT), Some(DOT)))
                .border(tone, HAIRLINE)
                .radius(RADIUS_FULL))
        }
    };
    let halo = if status == AgentStatus::Blocked {
        any(colored_box::<S>()
            .size(HALO, HALO)
            .radius(RADIUS_FULL)
            .fill(tone.with_alpha(HALO_ALPHA)))
    } else {
        any(SizedBox::<S>(Some(HALO), Some(HALO)))
    };
    SizedBox::<S>(Some(HALO), Some(HALO))
        .child(stack().child(halo).child(Align(Alignment::CENTER, dot)))
}

pub fn kind_tile<S: 'static>(kind: &str, size: f64) -> impl View<S> {
    let colors = Palette::from_theme(&active_theme());
    let mark = kind_mark(kind, &colors);
    avatar(mark.monogram).size(size).accent(mark.accent)
}

pub fn kind_tiles<S: 'static>(kinds: &[String], max: usize) -> impl View<S> {
    let size = TILE_SIZE;
    let shown = kinds.len().min(max);
    let hidden = kinds.len() - shown;
    let step = size * (1.0 - TILE_OVERLAP);
    let colors = Palette::from_theme(&active_theme());
    let mut layers = stack::<S>();
    for (i, kind) in kinds[..shown].iter().enumerate() {
        let mark = kind_mark(kind, &colors);
        let tile = opaque_tile::<S>(mark.monogram, mark.accent, size, &colors);
        layers = layers.child(offset(i, step, any(tile)));
    }
    if hidden > 0 {
        let tail = opaque_tile::<S>(format!("+{hidden}"), colors.fg_muted, size, &colors);
        layers = layers.child(offset(shown, step, any(tail)));
    }
    layers
}

/// The wash is pre-composited over the surface so an overlapped tile does not show through.
fn opaque_tile<S: 'static>(
    label: impl Into<String>,
    accent: Color,
    size: f64,
    colors: &Palette,
) -> impl View<S> {
    avatar(label).size(size).accent(accent).background(washed(
        colors.surface,
        accent,
        TILE_WASH_ALPHA,
    ))
}

fn offset<S: 'static>(index: usize, step: f64, tile: AnyView<S>) -> impl View<S> {
    Padding(
        EdgeInsets {
            left: index as f64 * step,
            top: 0.0,
            right: 0.0,
            bottom: 0.0,
        },
        tile,
    )
}

/// Unknown has no pill: the design never advertises it.
pub fn count_pills<S: 'static>(summary: &AgentSummary) -> Vec<AnyView<S>> {
    [
        (AgentStatus::Blocked, summary.blocked, BadgeVariant::Warning),
        (AgentStatus::Done, summary.done, BadgeVariant::Info),
        (AgentStatus::Working, summary.working, BadgeVariant::Success),
        (AgentStatus::Idle, summary.idle, BadgeVariant::Neutral),
    ]
    .into_iter()
    .filter(|&(_, count, _)| count > 0)
    .map(|(status, count, variant)| {
        any(badge(format!("{} {count}", status_glyph(status)), variant))
    })
    .collect()
}

pub fn summary_line(summary: &AgentSummary) -> String {
    let noun = if summary.total == 1 {
        "agent"
    } else {
        "agents"
    };
    let parts: Vec<String> = [
        (summary.blocked, AgentStatus::Blocked),
        (summary.done, AgentStatus::Done),
        (summary.working, AgentStatus::Working),
        (summary.idle, AgentStatus::Idle),
        (summary.unknown, AgentStatus::Unknown),
    ]
    .into_iter()
    .filter(|&(count, _)| count > 0)
    .map(|(count, status)| format!("{count} {}", status.label()))
    .collect();
    if parts.is_empty() {
        return format!("{} {noun}", summary.total);
    }
    format!("{} {noun}: {}", summary.total, parts.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyphs_are_the_design_shapes() {
        let glyphs: Vec<&str> = [
            AgentStatus::Blocked,
            AgentStatus::Working,
            AgentStatus::Done,
            AgentStatus::Idle,
            AgentStatus::Unknown,
        ]
        .into_iter()
        .map(status_glyph)
        .collect();
        assert_eq!(glyphs, ["◉", "●", "✓", "○", "·"]);
    }

    #[test]
    fn summary_line_reads_counts_in_priority_order() {
        let summary = AgentSummary {
            total: 4,
            blocked: 1,
            done: 1,
            working: 1,
            idle: 1,
            ..AgentSummary::default()
        };
        assert_eq!(
            summary_line(&summary),
            "4 agents: 1 needs you, 1 done, 1 working, 1 idle"
        );
        let one = AgentSummary {
            total: 1,
            idle: 1,
            ..AgentSummary::default()
        };
        assert_eq!(summary_line(&one), "1 agent: 1 idle");
    }
}
