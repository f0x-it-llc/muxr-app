//! The splash's paint-driven building blocks: independently mountable widgets cued by the
//! [`sequence`] and [`loading`] orchestrators. Randomness comes only from the seeded
//! [`SplashRng`], so a test replays a sequence; each widget collapses under reduced motion.

pub mod burst;
pub mod icon_phase;
pub mod loading;
pub mod rain;
pub mod sequence;

use frust::authoring::scene::GlyphRun;
use frust::authoring::text::{FontFamily, GenericSlot, TextContext, TextStyle};
use frust::authoring::{Affine, BoxConstraints, Brush, Color, LayoutCtx, PaintScene, Point, Size};
use frust::{FrameTime, Theme};

/// The `docs/splash.html` contract, transcribed once: `<script>` constants for timing, CSS rules
/// for geometry. Times in ms, lengths in logical px.
pub(crate) mod spec {
    use frust::Curve;
    use frust::authoring::Color;

    // Icon phase: `.app-icon` and the `ICON_*` script constants.

    pub(crate) const ICON_IN_MS: f64 = 350.0;
    pub(crate) const ICON_HOLD_MS: f64 = 650.0;
    /// Also the cold-start cue for the burst and the assembly.
    pub(crate) const ICON_DELAY_MS: f64 = ICON_IN_MS + ICON_HOLD_MS;
    pub(crate) const ICON_DISSOLVE_MS: f64 = 380.0;
    pub(crate) const ICON_SIZE: f64 = 96.0;
    pub(crate) const ICON_RADIUS_FRACTION: f64 = 0.22;
    pub(crate) const ICON_ENTER_SCALE: f64 = 0.75;
    pub(crate) const ICON_DISSOLVE_SCALE: f64 = 1.2;
    pub(crate) const SPRING: Curve = Curve::Cubic(0.34, 1.6, 0.5, 1.0);
    pub(crate) const DISSOLVE_EASE: Curve = Curve::Cubic(0.4, 0.0, 1.0, 1.0);

    // Glyph burst: `spawnBurst`.

    pub(crate) const BURST_COUNT: usize = 14;
    /// `dist = 55 + Math.random()*95`.
    pub(crate) const BURST_DIST_BASE: f64 = 55.0;
    pub(crate) const BURST_DIST_SPAN: f64 = 95.0;
    pub(crate) const BURST_MS: f64 = 600.0;
    pub(crate) const BURST_MOVE_EASE: Curve = Curve::Cubic(0.16, 1.0, 0.3, 1.0);
    pub(crate) const BURST_FADE_EASE: Curve = Curve::EaseOut;
    pub(crate) const BURST_FONT_SIZE: f32 = 13.0;
    pub(crate) const BURST_END_SCALE: f64 = 0.6;
    pub(crate) const BURST_CHARS: &[&str] = &["▣", "◧", "◎", "▤", "0", "1", "#"];

    // Matrix rain: the loading-rain block's `rainFrame` and `initCols`.

    pub(crate) const RAIN_FONT_SIZE: f32 = 14.0;
    pub(crate) const RAIN_ROW_H: f64 = 14.0;
    pub(crate) const RAIN_COL_W: f64 = 14.0;
    pub(crate) const RAIN_TRAIL: usize = 7;
    /// Rows fallen per reference frame (`rand(0.35,0.75)`).
    pub(crate) const RAIN_ADVANCE_MIN: f64 = 0.35;
    pub(crate) const RAIN_ADVANCE_MAX: f64 = 0.75;
    /// Respawn band in rows above the surface (`rand(-6,-1)`).
    pub(crate) const RAIN_RESPAWN_MIN_ROW: f64 = -6.0;
    pub(crate) const RAIN_RESPAWN_MAX_ROW: f64 = -1.0;
    pub(crate) const RAIN_EXIT_MARGIN_ROWS: f64 = 6.0;
    pub(crate) const DENSE_END_MS: f64 = 1400.0;
    pub(crate) const THIN_END_MS: f64 = 2800.0;
    pub(crate) const RAIN_TAIL_MS: f64 = 400.0;
    pub(crate) const RAIN_MIN_FADE: f64 = 0.15;
    pub(crate) const RAIN_HEAD_ALPHA: f64 = 0.95;
    pub(crate) const RAIN_TRAIL_ALPHA: f64 = 0.75;
    /// `rgba(255,238,200,…)`: no theme token fits the near-white head.
    pub(crate) const RAIN_HEAD_COLOR: Color = Color::from_rgb8(0xff, 0xee, 0xc8);
    /// `--amber`, used only when no theme is threaded.
    pub(crate) const AMBER_FALLBACK: Color = Color::from_rgb8(0xff, 0xb6, 0x27);
    pub(crate) const ON_AMBER_FALLBACK: Color = Color::from_rgb8(0x24, 0x1a, 0x04);
    pub(crate) const RAIN_GLYPHS: &[&str] = &[
        "0", "1", "ア", "イ", "ウ", "エ", "オ", "カ", "キ", "ク", "ケ", "コ", "サ", "シ", "ス",
        "<", ">", "{", "}", "[", "]", "/", "\\", "|", "_", "+", "-", "=", ".", ":", ";", "#", "$",
        "%",
    ];
    /// Double-weighted in the pool (`GLYPHS.concat(BRAND_GLYPHS, BRAND_GLYPHS)`).
    pub(crate) const BRAND_GLYPHS: &[&str] = &["▣", "◧", "◎", "▤", "⛶", "◆", "▮", "›", "//", "λ"];
    pub(crate) const DISTINCT_LEN: usize = RAIN_GLYPHS.len() + BRAND_GLYPHS.len();
    pub(crate) const POOL_LEN: usize = RAIN_GLYPHS.len() + 2 * BRAND_GLYPHS.len();

    // Assembly: `.piece` and `landPieces`.

    /// Each `.piece`'s `data-delay` from the assembly start, one per Connect-screen piece.
    pub(crate) const PIECE_DELAYS_MS: [f64; 8] =
        [0.0, 130.0, 260.0, 390.0, 480.0, 580.0, 720.0, 860.0];
    pub(crate) const PIECE_IN_MS: f64 = 500.0;
    pub(crate) const CTA_ROW_GAP: f64 = 8.0;
    pub(crate) const QR_SQUARE: f64 = 44.0;
    pub(crate) const DOCK_HEIGHT: f64 = 48.0;
    pub(crate) const DOCK_GAP: f64 = 14.0;
    pub(crate) const PIECE_ENTER_DY: f64 = -70.0;
    /// Radians (`rotate(-5deg)`).
    pub(crate) const PIECE_ENTER_ROTATION: f64 = -5.0 * std::f64::consts::PI / 180.0;
    pub(crate) const PIECE_ENTER_SCALE: f64 = 0.88;
    pub(crate) const LAND_FLASH_MS: f64 = 500.0;
    /// Peak keyframe as a fraction of [`LAND_FLASH_MS`].
    pub(crate) const LAND_FLASH_PEAK: f64 = 0.4;
    pub(crate) const LAND_FLASH_EASE: Curve = Curve::EaseOut;
    /// Used directly as `draw_shadow`'s `std_dev`.
    pub(crate) const LAND_FLASH_BLUR: f64 = 24.0;
    pub(crate) const LAND_FLASH_SPREAD: f64 = 6.0;
    /// `--amber-glow` is the accent at 35%, so it resolves from `primary`.
    pub(crate) const AMBER_GLOW_ALPHA: f64 = 0.35;

    /// Loading-band tuning, not transcribed: the dense beat's floor before a release takes effect.
    pub(crate) const LOADING_MIN_DENSE_MS: f64 = 600.0;
    pub(crate) const LOADING_MAX_HOLD_MS: f64 = 8_000.0;
    /// A literal on purpose: a cold-start change must not move it.
    pub(crate) const LOADING_ASSEMBLY_OFFSET_MS: f64 = 100.0;

    // Reduced motion: the source's `if(reduced)` branch.

    pub(crate) const REDUCED_HOLD_MS: f64 = 200.0;

    /// 60 Hz; the rain scales its advance by `delta / REFERENCE_FRAME_MS`.
    pub(crate) const REFERENCE_FRAME_MS: f64 = 1000.0 / 60.0;
    /// A stalled frame advances the rain at most this many reference frames.
    pub(crate) const MAX_FRAME_FACTOR: f64 = 4.0;

    /// Panics on an out-of-range index.
    pub(crate) fn distinct_glyph(i: usize) -> &'static str {
        let plain = RAIN_GLYPHS.len();
        if i < plain {
            RAIN_GLYPHS[i]
        } else {
            BRAND_GLYPHS[i - plain]
        }
    }

    /// An index past the table reuses the last entry rather than panicking.
    pub(crate) fn piece_delay_ms(index: usize) -> f64 {
        PIECE_DELAYS_MS[index.min(PIECE_DELAYS_MS.len() - 1)]
    }

    /// Both brand-glyph copies fold onto the same shaped glyph.
    pub(crate) fn pool_to_distinct(pool_index: usize) -> usize {
        let plain = RAIN_GLYPHS.len();
        if pool_index < plain {
            pool_index
        } else {
            plain + (pool_index - plain) % BRAND_GLYPHS.len()
        }
    }
}

/// The [`spec`] values `tests/splash_contract.rs` re-reads out of `docs/splash.html`.
#[doc(hidden)]
pub mod contract {
    pub const ICON_IN_MS: f64 = super::spec::ICON_IN_MS;
    pub const ICON_HOLD_MS: f64 = super::spec::ICON_HOLD_MS;
    pub const DENSE_END_MS: f64 = super::spec::DENSE_END_MS;
    pub const THIN_END_MS: f64 = super::spec::THIN_END_MS;
    pub const RAIN_TAIL_MS: f64 = super::spec::RAIN_TAIL_MS;
    pub const PIECE_IN_MS: f64 = super::spec::PIECE_IN_MS;
    pub const RAIN_FONT_SIZE: f32 = super::spec::RAIN_FONT_SIZE;
    pub const RAIN_COL_W: f64 = super::spec::RAIN_COL_W;
    pub const BURST_COUNT: usize = super::spec::BURST_COUNT;
    pub const PIECE_DELAYS_MS: [f64; 8] = super::spec::PIECE_DELAYS_MS;
    pub const CTA_ROW_GAP: f64 = super::spec::CTA_ROW_GAP;
    pub const QR_SQUARE: f64 = super::spec::QR_SQUARE;
    pub const DOCK_HEIGHT: f64 = super::spec::DOCK_HEIGHT;
    pub const DOCK_GAP: f64 = super::spec::DOCK_GAP;
}

/// Shaped once at layout and redrawn by cloning the run; a `TextStyle` bakes colour in.
pub(crate) struct ShapedGlyph {
    runs: Vec<GlyphRun>,
    size: Size,
}

impl ShapedGlyph {
    /// Panics if no text context was threaded into this layout pass.
    pub(crate) fn shape(ctx: &mut LayoutCtx, text: &str, style: &TextStyle) -> Self {
        let text_ctx = ctx.text_context::<TextContext>();
        let layout = text_ctx.layout(text, style, None);
        let size = layout.size();
        Self {
            runs: layout.to_scene_runs(Point::ORIGIN),
            size,
        }
    }

    pub(crate) fn size(&self) -> Size {
        self.size
    }

    /// `transform` maps the glyph's top-left-anchored box into absolute surface coordinates.
    pub(crate) fn draw(&self, scene: &mut dyn PaintScene, transform: Affine, color: Color) {
        for base in &self.runs {
            let mut run = base.clone();
            run.transform = transform * run.transform;
            run.brush = Brush::Solid(color);
            scene.draw_glyph_run(run);
        }
    }
}

/// An axis the parent left unbounded falls back to `intrinsic` instead of infinity.
pub(crate) fn fill_size(bc: &BoxConstraints, intrinsic: Size) -> Size {
    let max = bc.max();
    let width = if max.width.is_finite() {
        max.width
    } else {
        intrinsic.width
    };
    let height = if max.height.is_finite() {
        max.height
    } else {
        intrinsic.height
    };
    bc.constrain(Size::new(width, height))
}

pub(crate) fn mono_style(theme: Option<&Theme>, size: f32) -> TextStyle {
    let family = theme.map_or_else(
        || FontFamily::stack_with_generic(["IBM Plex Mono"], GenericSlot::Monospace),
        |theme| theme.type_scale.body_medium.family.clone(),
    );
    TextStyle::default().with_family(family).with_size(size)
}

pub(crate) fn accent_ink(theme: Option<&Theme>) -> Color {
    theme.map_or(spec::AMBER_FALLBACK, |theme| theme.scheme().primary)
}

pub(crate) fn with_alpha(color: Color, alpha: f64) -> Color {
    let c = color.components;
    Color::new([c[0], c[1], c[2], alpha.clamp(0.0, 1.0) as f32])
}

pub(crate) fn reduce_motion(theme: Option<&Theme>) -> bool {
    theme.is_some_and(|theme| theme.motion.reduce_motion)
}

/// The first call seeds the clock and returns `0.0`; `FrameTime`s are only ever differenced.
pub(crate) fn delta_ms(previous: &mut Option<FrameTime>, now: FrameTime) -> f64 {
    match previous.replace(now) {
        Some(earlier) => now.saturating_sub(earlier).as_secs_f64() * 1000.0,
        None => 0.0,
    }
}

pub(crate) fn frame_factor(delta_ms: f64) -> f64 {
    (delta_ms / spec::REFERENCE_FRAME_MS).clamp(0.0, spec::MAX_FRAME_FACTOR)
}

/// Seeded LCG (Knuth's MMIX constants) so a sequence replays from a `u64`; the high 32 bits
/// are taken because an LCG's low bits have short periods.
#[derive(Clone, Debug)]
pub(crate) struct SplashRng {
    state: u64,
}

impl SplashRng {
    /// The golden-ratio scramble keeps a zero seed out of a degenerate state.
    pub(crate) fn new(seed: u64) -> Self {
        Self {
            state: seed ^ 0x9E37_79B9_7F4A_7C15,
        }
    }

    fn next_u32(&mut self) -> u32 {
        self.state = self
            .state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.state >> 32) as u32
    }

    /// In `[0, 1)`.
    pub(crate) fn unit(&mut self) -> f64 {
        f64::from(self.next_u32()) / (f64::from(u32::MAX) + 1.0)
    }

    pub(crate) fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + self.unit() * (hi - lo)
    }

    pub(crate) fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        let i = (self.unit() * n as f64) as usize;
        i.min(n - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_replays_the_same_sequence() {
        let mut a = SplashRng::new(7);
        let mut b = SplashRng::new(7);
        let left: Vec<f64> = (0..16).map(|_| a.unit()).collect();
        let right: Vec<f64> = (0..16).map(|_| b.unit()).collect();
        assert_eq!(left, right, "a seeded LCG must be replayable");
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = SplashRng::new(1);
        let mut b = SplashRng::new(2);
        let left: Vec<f64> = (0..8).map(|_| a.unit()).collect();
        let right: Vec<f64> = (0..8).map(|_| b.unit()).collect();
        assert_ne!(left, right);
    }

    #[test]
    fn unit_stays_in_the_half_open_zero_one_range() {
        let mut rng = SplashRng::new(0);
        for _ in 0..10_000 {
            let v = rng.unit();
            assert!((0.0..1.0).contains(&v), "unit() out of range: {v}");
        }
    }

    #[test]
    fn range_stays_within_its_bounds() {
        let mut rng = SplashRng::new(42);
        for _ in 0..10_000 {
            let v = rng.range(spec::RAIN_ADVANCE_MIN, spec::RAIN_ADVANCE_MAX);
            assert!((spec::RAIN_ADVANCE_MIN..spec::RAIN_ADVANCE_MAX).contains(&v));
        }
    }

    #[test]
    fn below_never_reaches_its_bound_and_tolerates_zero() {
        let mut rng = SplashRng::new(99);
        for _ in 0..10_000 {
            assert!(rng.below(spec::POOL_LEN) < spec::POOL_LEN);
        }
        assert_eq!(
            rng.below(0),
            0,
            "an empty range degrades to 0, never panics"
        );
    }

    #[test]
    fn the_weighted_pool_double_counts_every_brand_glyph() {
        assert_eq!(
            spec::POOL_LEN,
            spec::RAIN_GLYPHS.len() + 2 * spec::BRAND_GLYPHS.len()
        );
        for i in 0..spec::POOL_LEN {
            assert!(spec::pool_to_distinct(i) < spec::DISTINCT_LEN);
        }
        for brand in 0..spec::BRAND_GLYPHS.len() {
            let distinct = spec::RAIN_GLYPHS.len() + brand;
            let hits = (0..spec::POOL_LEN)
                .filter(|&i| spec::pool_to_distinct(i) == distinct)
                .count();
            assert_eq!(hits, 2, "brand glyphs are double-weighted");
        }
    }

    #[test]
    fn delta_ms_treats_the_first_paint_as_the_clock_reference() {
        let mut previous = None;
        assert_eq!(
            delta_ms(&mut previous, FrameTime::from_nanos(9_000_000_000)),
            0.0,
            "the first paint seeds the clock; it must not jump the animation"
        );
        let delta = delta_ms(&mut previous, FrameTime::from_nanos(9_016_000_000));
        assert!(
            (delta - 16.0).abs() < 1e-6,
            "16ms later, delta is 16ms: {delta}"
        );
    }

    #[test]
    fn frame_factor_normalizes_to_sixty_hz_and_caps_a_stall() {
        assert!((frame_factor(spec::REFERENCE_FRAME_MS) - 1.0).abs() < 1e-9);
        assert!((frame_factor(spec::REFERENCE_FRAME_MS / 2.0) - 0.5).abs() < 1e-9);
        assert_eq!(frame_factor(1000.0), spec::MAX_FRAME_FACTOR);
        assert_eq!(frame_factor(0.0), 0.0);
    }

    #[test]
    fn with_alpha_replaces_only_the_alpha_channel() {
        let faded = with_alpha(spec::AMBER_FALLBACK, 0.25);
        let base = spec::AMBER_FALLBACK.components;
        assert_eq!(faded.components[0], base[0]);
        assert_eq!(faded.components[1], base[1]);
        assert_eq!(faded.components[2], base[2]);
        assert!((faded.components[3] - 0.25).abs() < 1e-6);
        assert!((with_alpha(spec::AMBER_FALLBACK, 4.0).components[3] - 1.0).abs() < 1e-6);
        assert!((with_alpha(spec::AMBER_FALLBACK, -1.0).components[3]).abs() < 1e-6);
    }

    #[test]
    fn distinct_glyph_walks_both_halves_of_the_pool() {
        assert_eq!(spec::distinct_glyph(0), "0");
        assert_eq!(
            spec::distinct_glyph(spec::RAIN_GLYPHS.len()),
            spec::BRAND_GLYPHS[0]
        );
        assert_eq!(
            spec::distinct_glyph(spec::DISTINCT_LEN - 1),
            spec::BRAND_GLYPHS[spec::BRAND_GLYPHS.len() - 1]
        );
    }
}
