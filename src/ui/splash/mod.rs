//! The splash's paint-driven building blocks — four beats off one master
//! clock (see [`sequence`] for the cold-start timeline), each independently
//! mountable/testable, plus a second orchestrator for a loading band:
//!
//! | Building block | What it paints |
//! |---|---|
//! | [`icon_phase`] | The real app icon springing in, holding, then dissolving outward |
//! | [`burst`] | 14 one-shot glyph particles flying out of the icon's center |
//! | [`rain`] | The full-surface amber matrix rain, thinning column by column — cued by [`loading`] alone |
//! | [`piece_entry`](sequence::piece_entry)/`piece_motion` | The assembly primitive: one piece's drop and land flash, defined in [`sequence`], replayed by [`loading`] |
//! | [`sequence`] | The cold-start orchestrator: icon then burst over one clock, ending in the Connect screen's piece-by-piece assembly |
//! | [`loading`] | The rain and that same assembly, held over a still-loading band and released when its content settles |
//!
//! No widget here knows about the others — its orchestrator owns the sequencing,
//! [`sequence`] cueing [`burst`] and the assembly together at [`spec::ICON_DELAY_MS`].
//!
//! `docs/splash.html` is the design reference; its `<script>` constants are the
//! timing contract, transcribed exactly once, here, into [`spec`] — never re-typed
//! at a use site. Colors come from the active [`frust::Theme`] where a token exists;
//! the two with none (the rain's near-white head, the reference amber for an
//! unthemed paint pass) are named constants in [`spec`] carrying the CSS custom
//! property they transcribe.
//!
//! Every random choice comes from [`SplashRng`], a seeded LCG, never a system RNG:
//! each widget takes a `seed: u64` so a test can replay an exact sequence. Each
//! widget also independently collapses under `MotionScheme::reduce_motion`.

pub mod burst;
pub mod icon_phase;
pub mod loading;
pub mod rain;
pub mod sequence;

use frust::authoring::scene::GlyphRun;
use frust::authoring::text::{FontFamily, GenericSlot, TextContext, TextStyle};
use frust::authoring::{Affine, BoxConstraints, Brush, Color, LayoutCtx, PaintScene, Point, Size};
use frust::{FrameTime, Theme};

/// The `docs/splash.html` timing/geometry contract, transcribed once.
///
/// Every value below is quoted from that checked-in file (the `<script>`
/// constants for timing, the CSS rules for geometry/color), with the source
/// expression in the doc comment — so it cannot drift from a remote source.
pub(crate) mod spec {
    use frust::Curve;
    use frust::authoring::Color;

    // ---- Icon phase (`.app-icon` + the `ICON_*` script constants) ---------

    /// Spring-in duration (`ICON_IN = 350`).
    pub(crate) const ICON_IN_MS: f64 = 350.0;
    /// Hold-at-rest duration (`ICON_HOLD = 650`).
    pub(crate) const ICON_HOLD_MS: f64 = 650.0;
    /// When the icon dissolves (`ICON_DELAY = ICON_IN + ICON_HOLD`); also
    /// the cold-start cue for the burst and the assembly.
    pub(crate) const ICON_DELAY_MS: f64 = ICON_IN_MS + ICON_HOLD_MS;
    /// Dissolve duration (`.app-icon.dissolve{transition:… .38s …}`).
    pub(crate) const ICON_DISSOLVE_MS: f64 = 380.0;
    /// Icon edge length in logical px (`.app-icon{width:96px;height:96px}`).
    pub(crate) const ICON_SIZE: f64 = 96.0;
    /// Corner rounding as a fraction of the edge (`border-radius:22%`).
    pub(crate) const ICON_RADIUS_FRACTION: f64 = 0.22;
    /// Scale the icon springs in *from* (`.app-icon{transform:scale(0.75)}`).
    pub(crate) const ICON_ENTER_SCALE: f64 = 0.75;
    /// Scale the icon dissolves *to* (`.app-icon.dissolve{transform:scale(1.2)}`).
    pub(crate) const ICON_DISSOLVE_SCALE: f64 = 1.2;
    /// The overshooting spring easing (`--ease-spring: cubic-bezier(0.34,1.6,0.5,1)`).
    pub(crate) const SPRING: Curve = Curve::Cubic(0.34, 1.6, 0.5, 1.0);
    /// The accelerating dissolve easing
    /// (`.app-icon.dissolve{transition:… cubic-bezier(0.4,0,1,1)}`).
    pub(crate) const DISSOLVE_EASE: Curve = Curve::Cubic(0.4, 0.0, 1.0, 1.0);

    // ---- Glyph burst (`spawnBurst`) --------------------------------------

    /// Particle count (`for(var i=0;i<14;i++)`).
    pub(crate) const BURST_COUNT: usize = 14;
    /// Minimum fly-out distance in logical px (`dist = 55 + Math.random()*95`).
    pub(crate) const BURST_DIST_BASE: f64 = 55.0;
    /// Random span added to [`BURST_DIST_BASE`] (same expression).
    pub(crate) const BURST_DIST_SPAN: f64 = 95.0;
    /// Fly-out duration (`transition:transform .6s …, opacity .6s ease-out`).
    pub(crate) const BURST_MS: f64 = 600.0;
    /// Fly-out (translate + scale) easing (`cubic-bezier(0.16,1,0.3,1)`).
    pub(crate) const BURST_MOVE_EASE: Curve = Curve::Cubic(0.16, 1.0, 0.3, 1.0);
    /// Fade-out easing (`opacity .6s ease-out`).
    pub(crate) const BURST_FADE_EASE: Curve = Curve::EaseOut;
    /// Particle glyph size in logical px (`.burst-particle{font-size:13px}`).
    pub(crate) const BURST_FONT_SIZE: f32 = 13.0;
    /// Scale a particle shrinks to as it flies out (`… scale(0.6)`).
    pub(crate) const BURST_END_SCALE: f64 = 0.6;
    /// The particle alphabet (`BURST_CHARS`).
    pub(crate) const BURST_CHARS: &[&str] = &["▣", "◧", "◎", "▤", "0", "1", "#"];

    // ---- Matrix rain (the loading-rain reference block's `rainFrame`, plus
    //      the `fontSize`/`colW` its `initCols` sets above the block) -------

    /// Rain glyph size in logical px (`var fontSize = 14`).
    pub(crate) const RAIN_FONT_SIZE: f32 = 14.0;
    /// Row pitch in logical px — the same `fontSize`, which the source uses
    /// as the vertical step (`ty = py - tI*fontSize`).
    pub(crate) const RAIN_ROW_H: f64 = 14.0;
    /// Column pitch in logical px (`var colW = 14`).
    pub(crate) const RAIN_COL_W: f64 = 14.0;
    /// Glyphs drawn per column, head included (`var trailLen = 7`).
    pub(crate) const RAIN_TRAIL: usize = 7;
    /// Slowest per-reference-frame fall, in rows (`rand(0.35,0.75)`).
    pub(crate) const RAIN_ADVANCE_MIN: f64 = 0.35;
    /// Fastest per-reference-frame fall, in rows (same expression).
    pub(crate) const RAIN_ADVANCE_MAX: f64 = 0.75;
    /// Respawn band, top of range, in rows above the surface (`rand(-6,-1)`).
    pub(crate) const RAIN_RESPAWN_MIN_ROW: f64 = -6.0;
    /// Respawn band, bottom of range, in rows (same expression).
    pub(crate) const RAIN_RESPAWN_MAX_ROW: f64 = -1.0;
    /// How far below the surface a head must fall to count as exited, in rows
    /// (`if(py > H + fontSize*6)`).
    pub(crate) const RAIN_EXIT_MARGIN_ROWS: f64 = 6.0;
    /// End of the full-density phase, on the rain-local clock (`DENSE_END`).
    pub(crate) const DENSE_END_MS: f64 = 1400.0;
    /// End of the thinning ramp, on the rain-local clock (`THIN_END`).
    pub(crate) const THIN_END_MS: f64 = 2800.0;
    /// Extra tail the source keeps painting past [`THIN_END_MS`] before it
    /// stops the loop (`if(t < THIN_END + 400)`).
    pub(crate) const RAIN_TAIL_MS: f64 = 400.0;
    /// Alpha floor the thinning ramp never fades a live column below
    /// (`Math.max(0.15, globalDensity)`).
    pub(crate) const RAIN_MIN_FADE: f64 = 0.15;
    /// Head-glyph alpha (`rgba(255,238,200,0.95)`).
    pub(crate) const RAIN_HEAD_ALPHA: f64 = 0.95;
    /// Trail-glyph alpha multiplier (`alpha*0.75*overallFade`).
    pub(crate) const RAIN_TRAIL_ALPHA: f64 = 0.75;
    /// The near-white head ink (`rgba(255,238,200,…)`). No `Theme` token
    /// fits (neither `on_surface` nor the accent), so it stays a named
    /// constant per `docs/CODE_STANDARDS.md`'s token-not-hardcode rule.
    pub(crate) const RAIN_HEAD_COLOR: Color = Color::from_rgb8(0xff, 0xee, 0xc8);
    /// Glyph's amber accent (`--amber:#ffb627`), used only as the unthemed
    /// fallback — a themed paint pass resolves `Theme::scheme().primary`.
    pub(crate) const AMBER_FALLBACK: Color = Color::from_rgb8(0xff, 0xb6, 0x27);
    /// Ink on an amber fill (`.btn-primary{color:#241a04}`), the unthemed
    /// fallback for `ColorScheme::on_primary_container`.
    pub(crate) const ON_AMBER_FALLBACK: Color = Color::from_rgb8(0x24, 0x1a, 0x04);
    /// The matrix-style half of the glyph pool (`GLYPHS`).
    pub(crate) const RAIN_GLYPHS: &[&str] = &[
        "0", "1", "ア", "イ", "ウ", "エ", "オ", "カ", "キ", "ク", "ケ", "コ", "サ", "シ", "ス",
        "<", ">", "{", "}", "[", "]", "/", "\\", "|", "_", "+", "-", "=", ".", ":", ";", "#", "$",
        "%",
    ];
    /// The muxr/Glyph brand half of the pool (`BRAND_GLYPHS`). Each entry is
    /// **double-weighted** in the pool (`GLYPHS.concat(BRAND_GLYPHS, BRAND_GLYPHS)`).
    pub(crate) const BRAND_GLYPHS: &[&str] = &["▣", "◧", "◎", "▤", "⛶", "◆", "▮", "›", "//", "λ"];
    /// Distinct glyphs to shape once: the two lists concatenated, before
    /// weighting.
    pub(crate) const DISTINCT_LEN: usize = RAIN_GLYPHS.len() + BRAND_GLYPHS.len();
    /// Weighted-pool size: brand glyphs appear twice (see [`BRAND_GLYPHS`]).
    pub(crate) const POOL_LEN: usize = RAIN_GLYPHS.len() + 2 * BRAND_GLYPHS.len();

    // ---- Assembly (`.piece` + `landPieces`) ------------------------------

    /// Per-piece stagger in source order (each `.piece`'s `data-delay`), from
    /// the assembly start. Eight entries, one per Connect-screen piece.
    pub(crate) const PIECE_DELAYS_MS: [f64; 8] =
        [0.0, 130.0, 260.0, 390.0, 480.0, 580.0, 720.0, 860.0];
    /// A piece's entrance duration (`.piece{transition:opacity .5s
    /// var(--ease-spring)…}`), riding [`SPRING`]'s overshoot curve.
    pub(crate) const PIECE_IN_MS: f64 = 500.0;
    /// Piece 7's CTA-to-QR-square gap (`.cta-row{display:flex;gap:8px}`).
    pub(crate) const CTA_ROW_GAP: f64 = 8.0;
    /// The QR square beside the CTA (`.qr-square{width:44px`).
    pub(crate) const QR_SQUARE: f64 = 44.0;
    /// The unlock dock closing piece 8 (`.unlock-dock{height:48px`).
    pub(crate) const DOCK_HEIGHT: f64 = 48.0;
    /// The dock's gap under the last saved row (`.unlock-dock{…margin-top:14px`).
    pub(crate) const DOCK_GAP: f64 = 14.0;
    /// Vertical offset a piece drops in from (`transform:translateY(-70px)`).
    pub(crate) const PIECE_ENTER_DY: f64 = -70.0;
    /// Rotation a piece unwinds from, in radians (`rotate(-5deg)`).
    pub(crate) const PIECE_ENTER_ROTATION: f64 = -5.0 * std::f64::consts::PI / 180.0;
    /// Scale a piece grows in from (`scale(0.88)`).
    pub(crate) const PIECE_ENTER_SCALE: f64 = 0.88;
    /// Land-flash duration (`.piece.land.flash{animation:landFlash .5s ease-out}`).
    pub(crate) const LAND_FLASH_MS: f64 = 500.0;
    /// The flash's peak keyframe, as a fraction of [`LAND_FLASH_MS`]
    /// (`@keyframes landFlash{ 40%{ … } }`).
    pub(crate) const LAND_FLASH_PEAK: f64 = 0.4;
    /// The flash's easing (`animation:landFlash .5s ease-out`).
    pub(crate) const LAND_FLASH_EASE: Curve = Curve::EaseOut;
    /// Peak glow blur radius in logical px (`box-shadow:0 0 24px 6px`), used
    /// directly as `PaintScene::draw_shadow`'s `std_dev` (same CSS-blur-px
    /// treatment `frust_theme`'s Glyph elevation table applies).
    pub(crate) const LAND_FLASH_BLUR: f64 = 24.0;
    /// Peak glow spread in logical px (the `6px` in the same rule): how far
    /// the glow's rounded rect grows past the piece's own box.
    pub(crate) const LAND_FLASH_SPREAD: f64 = 6.0;
    /// The glow ink's alpha (`--amber-glow: rgba(255,182,39,0.35)`) — the
    /// accent at 35%, so it resolves from `ColorScheme::primary` rather than
    /// being a second amber constant.
    pub(crate) const AMBER_GLOW_ALPHA: f64 = 0.35;

    /// Floor on the dense beat before a release takes effect. A tuning value
    /// for this app, not a `docs/splash.html` transcription.
    pub(crate) const LOADING_MIN_DENSE_MS: f64 = 600.0;
    /// Hold ceiling: a load that never settles releases here anyway.
    pub(crate) const LOADING_MAX_HOLD_MS: f64 = 8_000.0;
    /// `loading`'s own tuning — the beat between the release and the first
    /// landing. A literal on purpose: a cold-start change must not move it.
    pub(crate) const LOADING_ASSEMBLY_OFFSET_MS: f64 = 100.0;

    // ---- Reduced motion (the source's `if(reduced)` branch) ---------------

    /// How long the reduced-motion path holds the static icon before
    /// revealing the finished screen (`setTimeout(…, 200)`).
    pub(crate) const REDUCED_HOLD_MS: f64 = 200.0;

    /// The reference frame the source's per-frame constants were authored
    /// against (60 Hz). The rain scales its row advance by
    /// `delta / REFERENCE_FRAME_MS` so a 120 Hz device falls at the same
    /// speed rather than twice as fast.
    pub(crate) const REFERENCE_FRAME_MS: f64 = 1000.0 / 60.0;
    /// Ceiling on that scale factor, so a stalled frame (app resumed, GC
    /// pause) teleports the rain at most four reference frames instead of
    /// jumping the whole gap.
    pub(crate) const MAX_FRAME_FACTOR: f64 = 4.0;

    /// The `i`th distinct glyph — [`RAIN_GLYPHS`] then [`BRAND_GLYPHS`].
    ///
    /// Panics on an out-of-range index; every caller derives `i` from
    /// [`pool_to_distinct`] or a `0..DISTINCT_LEN` loop.
    pub(crate) fn distinct_glyph(i: usize) -> &'static str {
        let plain = RAIN_GLYPHS.len();
        if i < plain {
            RAIN_GLYPHS[i]
        } else {
            BRAND_GLYPHS[i - plain]
        }
    }

    /// The `index`th piece's stagger, from [`PIECE_DELAYS_MS`]. An index
    /// past the table's end reuses the last entry rather than panicking — a
    /// ninth piece lands with the eighth rather than taking the sequence down.
    pub(crate) fn piece_delay_ms(index: usize) -> f64 {
        PIECE_DELAYS_MS[index.min(PIECE_DELAYS_MS.len() - 1)]
    }

    /// Map a weighted-pool index (`0..POOL_LEN`) onto its distinct-glyph
    /// index (`0..DISTINCT_LEN`) — the two brand-glyph copies fold onto the
    /// same shaped glyph, which is what makes the pool *weighted* rather than
    /// merely longer.
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

// ---------------------------------------------------------------------------
// Shared paint helpers
// ---------------------------------------------------------------------------

/// One glyph, shaped once at layout and re-drawn many times per paint.
///
/// The rain draws ~200 glyphs a frame and the burst 14. Re-shaping per draw
/// (or per alpha — a [`TextStyle`] bakes color into the shaped run) would be
/// wasteful, so each distinct glyph is shaped once and every draw clones the
/// run, overriding only its transform and brush — both plain public fields
/// on `frust`'s scene-level `GlyphRun`.
pub(crate) struct ShapedGlyph {
    /// The origin-relative runs (`Point::ORIGIN`-anchored, so a draw's
    /// transform is the whole placement).
    runs: Vec<GlyphRun>,
    /// The shaped advance/height, for centering a particle on a point.
    size: Size,
}

impl ShapedGlyph {
    /// Shape `text` at `style` through the layout pass's text context.
    /// Panics if no text context was threaded into this layout pass — a
    /// shell-wiring bug, not a state a splash widget should paper over.
    pub(crate) fn shape(ctx: &mut LayoutCtx, text: &str, style: &TextStyle) -> Self {
        let text_ctx = ctx.text_context::<TextContext>();
        let layout = text_ctx.layout(text, style, None);
        let size = layout.size();
        Self {
            runs: layout.to_scene_runs(Point::ORIGIN),
            size,
        }
    }

    /// The shaped glyph's box (advance width × line height).
    pub(crate) fn size(&self) -> Size {
        self.size
    }

    /// Draw this glyph under `transform`, tinted `color`. `transform` maps
    /// the glyph's top-left-anchored box into absolute surface coordinates,
    /// so callers compose translation/scale themselves rather than going
    /// through `PaintScene::push_transform` per glyph.
    pub(crate) fn draw(&self, scene: &mut dyn PaintScene, transform: Affine, color: Color) {
        for base in &self.runs {
            let mut run = base.clone();
            run.transform = transform * run.transform;
            run.brush = Brush::Solid(color);
            scene.draw_glyph_run(run);
        }
    }
}

/// The size a full-surface splash layer takes: the incoming maximum, falling
/// back to `intrinsic` on an axis the parent left unbounded (a `Column`
/// child, say) so an unconstrained mount degrades to the icon-sized box
/// instead of an infinite one.
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

/// The Glyph monospace stack at `size` px, resolved from the active theme's
/// type scale when one is threaded (Glyph's UI face is IBM Plex Mono) and
/// falling back to that stack literally when none is — the
/// theme-then-fallback precedence `docs/CODE_STANDARDS.md` requires.
pub(crate) fn mono_style(theme: Option<&Theme>, size: f32) -> TextStyle {
    let family = theme.map_or_else(
        || FontFamily::stack_with_generic(["IBM Plex Mono"], GenericSlot::Monospace),
        |theme| theme.type_scale.body_medium.family.clone(),
    );
    TextStyle {
        family,
        size,
        ..TextStyle::default()
    }
}

/// The amber accent ink: `ColorScheme::primary` (Glyph's `--amber`, and the
/// accent *ink* half of Glyph's accent-role split), falling back to
/// [`spec::AMBER_FALLBACK`] with no theme threaded.
pub(crate) fn accent_ink(theme: Option<&Theme>) -> Color {
    theme.map_or(spec::AMBER_FALLBACK, |theme| theme.scheme().primary)
}

/// `color` with its alpha replaced by `alpha` (clamped) — mirrors the
/// per-module helper of the same shape used across `frust`'s own widgets.
pub(crate) fn with_alpha(color: Color, alpha: f64) -> Color {
    let c = color.components;
    Color::new([c[0], c[1], c[2], alpha.clamp(0.0, 1.0) as f32])
}

/// Whether the active theme asks for reduced motion (`false` with no theme
/// threaded — the conservative default, matching every `frust` widget).
pub(crate) fn reduce_motion(theme: Option<&Theme>) -> bool {
    theme.is_some_and(|theme| theme.motion.reduce_motion)
}

/// Milliseconds elapsed since the previous paint, advancing `previous`. The
/// first call is the clock reference and returns `0.0`, so a widget's first
/// paint never jumps its animation by an arbitrary shell epoch. `FrameTime`s
/// are only ever differenced, never read absolutely (`docs/CODE_STANDARDS.md`:
/// no wall clock inside widget code).
pub(crate) fn delta_ms(previous: &mut Option<FrameTime>, now: FrameTime) -> f64 {
    match previous.replace(now) {
        Some(earlier) => now.saturating_sub(earlier).as_secs_f64() * 1000.0,
        None => 0.0,
    }
}

/// How many reference frames (see [`spec::REFERENCE_FRAME_MS`]) `delta`
/// spans, capped at [`spec::MAX_FRAME_FACTOR`].
pub(crate) fn frame_factor(delta_ms: f64) -> f64 {
    (delta_ms / spec::REFERENCE_FRAME_MS).clamp(0.0, spec::MAX_FRAME_FACTOR)
}

/// A tiny seeded linear-congruential generator — the splash's only
/// randomness source. Deliberately not a `rand` dependency: the sequence
/// must replay from a `u64` seed for tests, and nothing here needs
/// statistical quality. Constants are Knuth's MMIX LCG multiplier/increment;
/// the high 32 bits are taken because an LCG's low bits have short periods.
#[derive(Clone, Debug)]
pub(crate) struct SplashRng {
    state: u64,
}

impl SplashRng {
    /// A generator seeded from `seed`. Any seed is valid (including `0` — the
    /// golden-ratio scramble keeps a zero seed from starting in a degenerate
    /// state).
    pub(crate) fn new(seed: u64) -> Self {
        Self {
            state: seed ^ 0x9E37_79B9_7F4A_7C15,
        }
    }

    /// The next raw 32 bits.
    fn next_u32(&mut self) -> u32 {
        self.state = self
            .state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.state >> 32) as u32
    }

    /// The next value in `[0, 1)` — the `Math.random()` analog.
    pub(crate) fn unit(&mut self) -> f64 {
        f64::from(self.next_u32()) / (f64::from(u32::MAX) + 1.0)
    }

    /// The next value in `[lo, hi)` — the source's `rand(a,b)` helper.
    pub(crate) fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + self.unit() * (hi - lo)
    }

    /// The next index below `n` (`0` when `n == 0`) — the source's
    /// `Math.floor(Math.random()*arr.length)`.
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
        // Every pool index maps into the distinct table...
        for i in 0..spec::POOL_LEN {
            assert!(spec::pool_to_distinct(i) < spec::DISTINCT_LEN);
        }
        // ...and each brand glyph is reachable from exactly two pool slots.
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
        // A 120Hz frame advances half as far per frame (same speed on screen).
        assert!((frame_factor(spec::REFERENCE_FRAME_MS / 2.0) - 0.5).abs() < 1e-9);
        // A one-second stall is capped, not teleported.
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
        // Out-of-range alphas clamp rather than producing an invalid color.
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
