//! Presentation of the agent vocabulary (`crate::agents`): the status tone,
//! glyph and dot, the kind monogram tiles, the count pills and the spoken
//! summary line. Shared by sessions and terminal, so it imports neither.
//!
//! Design reference: the Penpot page "06 · Agents" components frame
//! (`design/previews/06-agents/`). Glyph first: every status reads by shape
//! (`◉ ● ✓ ○ ·`) and word as well as by colour.
//!
//! Kind tiles are monograms on a brand-adjacent accent — never vendor
//! artwork. The accents are design constants (the one place literal hex is
//! allowed here); every other colour comes from [`Palette`].

use frust::authoring::Color;
use frust::{
    Align, Alignment, AnyView, EdgeInsets, Padding, SizedBox, View, any, colored_box, container,
    stack,
};
use frust_glyph::{BadgeVariant, avatar, badge};

use crate::agents::{AgentStatus, AgentSummary};
use crate::ui::paint::{GLOW_SPREAD, GLOW_STD_DEV, HAIRLINE, RADIUS_FULL};
use crate::ui::palette::{Palette, active_theme, washed};

/// The status dot's diameter, per the design.
pub const DOT: f64 = 8.0;
/// A blocked dot's halo slot: the dot plus a 25 % ring around it.
const HALO: f64 = 14.0;
const HALO_ALPHA: f32 = 0.25;

/// A kind's monogram and accent (design constants, brand-adjacent).
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

/// The edge of a tile in [`kind_tiles`], per the design's tile strip.
pub const TILE_SIZE: f64 = 22.0;
/// Fraction of a tile's edge the next tile overlaps in [`kind_tiles`].
const TILE_OVERLAP: f64 = 0.3;
/// The avatar's accent-wash alpha (Glyph's `AVATAR_WASH_ALPHA`), folded over
/// the surface so a stacked tile is opaque and hides the one beneath it.
const TILE_WASH_ALPHA: f32 = 0.12;
/// The monogram for a kind no mark is drawn for.
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

/// The colour a status reads in: blocked warns, working is alive, done is
/// informational, idle and unknown recede.
pub fn status_tone(colors: &Palette, status: AgentStatus) -> Color {
    match status {
        AgentStatus::Blocked => colors.warning,
        AgentStatus::Working => colors.success,
        AgentStatus::Done => colors.cyan,
        AgentStatus::Idle | AgentStatus::Unknown => colors.fg_dim,
    }
}

/// The status as a shape: `◉` needs you, `●` working, `✓` done, `○` idle,
/// `·` unknown.
pub fn status_glyph(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Blocked => "◉",
        AgentStatus::Working => "●",
        AgentStatus::Done => "✓",
        AgentStatus::Idle => "○",
        AgentStatus::Unknown => "·",
    }
}

/// The 8 px status dot. Blocked sits in a 25 % halo; idle and unknown are a
/// hollow ring; working and done are solid. Constant slot size per status
/// class, so a status change never reflows a row.
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

/// One kind's monogram tile at `size` px. Its accessible label is the
/// monogram.
pub fn kind_tile<S: 'static>(kind: &str, size: f64) -> impl View<S> {
    let colors = Palette::from_theme(&active_theme());
    let mark = kind_mark(kind, &colors);
    avatar(mark.monogram).size(size).accent(mark.accent)
}

/// Up to `max` overlapping kind tiles, then a `+N` tile for the rest.
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

/// A tile whose wash is pre-composited over the surface: the stack overlaps
/// tiles, and a translucent wash would let the tile beneath show through.
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

/// A pill per non-zero status, in order blocked, done, working, idle: the
/// glyph and the count, on Warning / Info / Success / Neutral. Unknown has no
/// pill (the design never advertises it).
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

/// The roll-up as one sentence for accessibility: "4 agents: 1 needs you, 1
/// done, 1 working, 1 idle" (unknown last, when present).
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
