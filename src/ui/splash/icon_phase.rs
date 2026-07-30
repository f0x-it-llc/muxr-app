//! The splash sequence's first beat: the real app icon springs in, holds long
//! enough to register as "the app icon", then scales up and dissolves.
//!
//! ```text
//!   0ms            350ms                  1000ms            1380ms
//!   |-- spring-in --|------- hold ---------|--- dissolve ----|-- done
//!   scale .75 -> 1   scale 1, opacity 1     scale 1 -> 1.2
//!   opacity 0 -> 1                          opacity 1 -> 0
//! ```
//!
//! [`icon_visual`] is the whole state machine as one pure function of elapsed
//! milliseconds — the widget only accumulates a clock, calls it, and paints
//! what it returns.
//!
//! # Cueing the rest of the sequence
//!
//! The phase is published through an optional [`RwSignal`]
//! ([`IconPhaseView::on_phase`]), written **only when it changes**, so the
//! orchestrator can start the burst and the rain the instant the icon starts
//! dissolving — the source's `setTimeout(..., ICON_DELAY)` cue, expressed as
//! a reactive edge instead of a timer. `IconPhase::Dissolve` is published at
//! exactly [`spec::ICON_DELAY_MS`].
//!
//! # How the icon is actually drawn
//!
//! The framework has no app-reachable way to hand a decoded image's pixels to
//! `PaintScene::draw_image` (`ImageSource`'s inner `ImageData` accessor is
//! private), so this widget composes rather than blits: it retains `frust`'s
//! own `Image` widget as a child pod and paints it under a scale transform
//! plus a translucent layer. Both `PaintScene::push_transform` and
//! `PaintScene::push_layer` compose onto the child's draws, which is what
//! makes an app-authored scale/fade wrapper around a framework widget
//! possible at all. [`IconArt::Vector`] is the decode-failure fallback: a
//! rounded amber tile with the `▣` brand glyph centered.
//!
//! # Reduced motion
//!
//! `MotionScheme::reduce_motion` collapses the whole beat: [`icon_visual`]
//! reports [`IconPhase::Done`] on the first frame with nothing painted and no
//! continuation frames requested, so an orchestrator that mounts this under
//! reduced motion is cued straight to the assembled screen. The source's
//! reduced path keeps a 200ms static icon before jumping ahead; that beat is
//! deliberately dropped here in favour of the flat "terminal state
//! immediately" rule, and the orchestrator is expected to skip the splash
//! entirely rather than flash a frame of nothing.

use frust::authoring::text::TextStyle;
use frust::authoring::{
    Affine, BoxConstraints, BuildCtx, ChangeFlags, ChildPod, LayoutCtx, PaintCtx, PaintScene,
    Point, SemanticsCtx, Size, View, Widget, any, build_child, rebuild_child, teardown_child,
};
use frust::{AnyView, FrameTime, Image, ImageFit, ImageSource, RwSignal, Set, Theme};

use super::spec;
use super::{ShapedGlyph, delta_ms, fill_size, mono_style, reduce_motion};

/// The app icon, embedded so the splash never touches the filesystem on a
/// cold start (the scaffold's `assets/logo.png` idiom — see
/// `examples/huddle`'s `settings_about`). This is the 1024px iOS
/// `AppIcon.appiconset` master, copied to `assets/splash_icon.png`.
const SPLASH_ICON_PNG: &[u8] = include_bytes!("../../../assets/splash_icon.png");

/// The brand glyph the [`IconArt::Vector`] fallback centers on its tile.
const FALLBACK_GLYPH: &str = "▣";
/// The fallback glyph's size as a fraction of the tile — a hand-tuned
/// **community-approximate** value (the design reference has no vector
/// stand-in for the icon at all, so there is nothing to match): half the tile
/// reads as an icon rather than as text.
const FALLBACK_GLYPH_FRACTION: f64 = 0.5;

/// Where the icon beat currently is. Published through
/// [`IconPhaseView::on_phase`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IconPhase {
    /// Springing in (0 → [`spec::ICON_IN_MS`]).
    #[default]
    SpringIn,
    /// Held at rest ([`spec::ICON_IN_MS`] → [`spec::ICON_DELAY_MS`]).
    Hold,
    /// Scaling up and fading out ([`spec::ICON_DELAY_MS`] → +
    /// [`spec::ICON_DISSOLVE_MS`]). **The burst/rain cue.**
    Dissolve,
    /// Gone. Nothing is painted and no more frames are requested.
    Done,
}

/// The icon's resolved appearance at an instant — the entire animation state,
/// as plain data.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IconVisual {
    /// Which beat this instant falls in.
    pub phase: IconPhase,
    /// Uniform scale about the icon's own center. May exceed 1 during the
    /// spring's overshoot.
    pub scale: f64,
    /// Layer opacity, always within `0..=1`.
    pub opacity: f64,
}

/// The icon beat's state machine: scale/opacity/phase at `elapsed_ms`.
///
/// `reduce_motion` short-circuits to [`IconPhase::Done`] (see the module
/// docs). Negative elapsed values clamp to zero; values past the end stay at
/// the terminal state forever, so a late frame can never resurrect the icon.
pub fn icon_visual(elapsed_ms: f64, reduce_motion: bool) -> IconVisual {
    if reduce_motion {
        return IconVisual {
            phase: IconPhase::Done,
            scale: 1.0,
            opacity: 0.0,
        };
    }

    let t = elapsed_ms.max(0.0);
    if t < spec::ICON_IN_MS {
        // One spring drives both properties, exactly as the source's single
        // `transition: opacity .35s var(--ease-spring), transform .35s
        // var(--ease-spring)` does. The curve overshoots past 1, which is the
        // point for scale and must be clamped for opacity.
        let p = spec::SPRING.transform(t / spec::ICON_IN_MS);
        IconVisual {
            phase: IconPhase::SpringIn,
            scale: spec::ICON_ENTER_SCALE + (1.0 - spec::ICON_ENTER_SCALE) * p,
            opacity: p.clamp(0.0, 1.0),
        }
    } else if t < spec::ICON_DELAY_MS {
        IconVisual {
            phase: IconPhase::Hold,
            scale: 1.0,
            opacity: 1.0,
        }
    } else if t < spec::ICON_DELAY_MS + spec::ICON_DISSOLVE_MS {
        let p = spec::DISSOLVE_EASE.transform((t - spec::ICON_DELAY_MS) / spec::ICON_DISSOLVE_MS);
        IconVisual {
            phase: IconPhase::Dissolve,
            scale: 1.0 + (spec::ICON_DISSOLVE_SCALE - 1.0) * p,
            opacity: (1.0 - p).clamp(0.0, 1.0),
        }
    } else {
        IconVisual {
            phase: IconPhase::Done,
            scale: spec::ICON_DISSOLVE_SCALE,
            opacity: 0.0,
        }
    }
}

/// What the icon beat draws: the decoded app icon, or a vector stand-in.
#[derive(Clone)]
pub enum IconArt {
    /// The decoded app icon bitmap.
    Image(ImageSource),
    /// A rounded amber tile with the `▣` brand glyph centered — the
    /// decode-failure fallback (see [`splash_icon_art`]).
    Vector,
}

/// Decode the embedded app icon, falling back to [`IconArt::Vector`] if the
/// bytes don't decode.
///
/// **Call once and retain the result** (from the composition root's first
/// `build`, via `AppState::splash_plan()` — not `Component::init`, since at
/// `init` time the shell has not yet applied the theme; see that function's
/// doc for why), mirroring how `examples/huddle` stashes its decoded logo in
/// state: decoding is a real PNG decode plus a full-size RGBA allocation, and
/// `IconArt` clones for free afterwards (`ImageSource` is `Arc`-backed).
pub fn splash_icon_art() -> IconArt {
    match ImageSource::decode(SPLASH_ICON_PNG) {
        Ok(source) => IconArt::Image(source),
        Err(error) => {
            log::warn!("splash icon failed to decode ({error}); using the vector fallback");
            IconArt::Vector
        }
    }
}

/// The icon beat as a full-surface layer that centers a
/// [`spec::ICON_SIZE`]-square icon.
///
/// Chain [`IconPhaseView::on_phase`] to receive the phase cue.
pub fn icon_phase<State: 'static>(art: IconArt) -> IconPhaseView<State> {
    let child = match &art {
        // `Contain` on a square source in a square box is an exact fit; it
        // also degrades gracefully if the asset is ever replaced with a
        // non-square one.
        IconArt::Image(source) => Some(any(Image(source.clone()).fit(ImageFit::Contain))),
        IconArt::Vector => None,
    };
    IconPhaseView {
        child,
        on_phase: None,
    }
}

/// The declarative half of the icon beat. See [`icon_phase`].
pub struct IconPhaseView<State: 'static> {
    /// The `Image` child for [`IconArt::Image`]; `None` selects the vector
    /// fallback, which the widget paints itself.
    child: Option<AnyView<State>>,
    /// Where to publish phase changes, if anyone is listening.
    on_phase: Option<RwSignal<IconPhase>>,
}

impl<State: 'static> IconPhaseView<State> {
    /// Publish every [`IconPhase`] change into `signal` (written only on a
    /// change, so a tracked reader rebuilds four times across the whole beat,
    /// not once per frame).
    pub fn on_phase(mut self, signal: RwSignal<IconPhase>) -> Self {
        self.on_phase = Some(signal);
        self
    }
}

/// The retained half of the icon beat. See [`icon_phase`].
pub struct IconPhaseWidget {
    /// The `Image` child pod, or `None` for the vector fallback.
    child: Option<ChildPod>,
    /// The vector fallback's shaped brand glyph (only ever `Some` when
    /// `child` is `None`), plus the style it was shaped at so a theme swap
    /// re-shapes it.
    fallback_glyph: Option<ShapedGlyph>,
    fallback_style: Option<TextStyle>,
    /// The icon box's origin within the widget's own (full-surface) box.
    icon_origin: Point,
    /// Milliseconds since the first paint.
    elapsed_ms: f64,
    /// The previous frame's clock, for differencing.
    last_frame: Option<FrameTime>,
    /// Where to publish phase changes.
    on_phase: Option<RwSignal<IconPhase>>,
    /// The last phase published, so a change is published exactly once.
    published: Option<IconPhase>,
}

impl IconPhaseWidget {
    /// Publish `phase` if it differs from the last published one.
    fn publish(&mut self, phase: IconPhase) {
        if self.published == Some(phase) {
            return;
        }
        self.published = Some(phase);
        if let Some(signal) = self.on_phase {
            signal.set(phase);
        }
    }

    /// Paint the vector fallback tile at `origin`.
    fn paint_fallback(&self, ctx: &PaintCtx, scene: &mut dyn PaintScene, origin: Point) {
        let theme = Theme::from_paint_ctx(ctx);
        // Glyph's accent-role split: `primary_container`/`on_primary_container`
        // is the *bright fill* pair (`primary` is accent ink) — see
        // `docs/CODE_STANDARDS.md`'s Glyph token precedents.
        let (fill, ink) = theme.map_or((spec::AMBER_FALLBACK, spec::ON_AMBER_FALLBACK), |theme| {
            let scheme = theme.scheme();
            (scheme.primary_container, scheme.on_primary_container)
        });
        let box_size = Size::new(spec::ICON_SIZE, spec::ICON_SIZE);
        scene.fill_rounded_rect(
            origin,
            box_size,
            spec::ICON_SIZE * spec::ICON_RADIUS_FRACTION,
            fill,
        );
        if let Some(glyph) = &self.fallback_glyph {
            let glyph_size = glyph.size();
            let placed = Point::new(
                origin.x + (box_size.width - glyph_size.width) / 2.0,
                origin.y + (box_size.height - glyph_size.height) / 2.0,
            );
            glyph.draw(scene, Affine::translate(placed.to_vec2()), ink);
        }
    }
}

impl<State: 'static> View<State> for IconPhaseView<State> {
    type Element = IconPhaseWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> IconPhaseWidget {
        IconPhaseWidget {
            child: self.child.as_ref().map(|child| build_child(child, ctx)),
            fallback_glyph: None,
            fallback_style: None,
            icon_origin: Point::ZERO,
            elapsed_ms: 0.0,
            last_frame: None,
            on_phase: self.on_phase,
            published: None,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut IconPhaseWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        element.on_phase = self.on_phase;
        match (&prev.child, &self.child, &mut element.child) {
            // The ordinary path: the same art re-passed every rebuild. The
            // `Image` view's own rebuild is an `Arc`-pointer check, never a
            // re-decode.
            (Some(prev_child), Some(next_child), Some(pod)) => {
                rebuild_child(prev_child, next_child, pod, ctx)
            }
            // Art swapped between bitmap and vector mid-flight (not something
            // the orchestrator does, but the lifecycle has to be closed).
            (Some(prev_child), None, Some(pod)) => {
                teardown_child(prev_child, pod, ctx);
                element.child = None;
                ChangeFlags::LAYOUT | ChangeFlags::PAINT
            }
            (None, Some(next_child), _) => {
                element.child = Some(build_child(next_child, ctx));
                ChangeFlags::LAYOUT | ChangeFlags::PAINT
            }
            _ => ChangeFlags::NONE,
        }
    }

    fn teardown(&self, element: &mut IconPhaseWidget, ctx: &mut BuildCtx<'_>) {
        if let (Some(child), Some(pod)) = (&self.child, &mut element.child) {
            teardown_child(child, pod, ctx);
        }
    }
}

impl Widget for IconPhaseWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let icon = Size::new(spec::ICON_SIZE, spec::ICON_SIZE);
        let size = fill_size(bc, icon);
        self.icon_origin = Point::new(
            (size.width - icon.width) / 2.0,
            (size.height - icon.height) / 2.0,
        );

        match &mut self.child {
            Some(pod) => {
                pod.layout_child(ctx, &BoxConstraints::tight(icon));
                pod.set_origin(self.icon_origin);
            }
            None => {
                // Vector fallback: shape the brand glyph once per style.
                let theme = Theme::from_layout_ctx(ctx).cloned();
                let style = mono_style(
                    theme.as_ref(),
                    (spec::ICON_SIZE * FALLBACK_GLYPH_FRACTION) as f32,
                );
                if self.fallback_style.as_ref() != Some(&style) {
                    self.fallback_glyph = Some(ShapedGlyph::shape(ctx, FALLBACK_GLYPH, &style));
                    self.fallback_style = Some(style);
                }
            }
        }
        size
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let reduce = reduce_motion(Theme::from_paint_ctx(ctx));
        self.elapsed_ms += delta_ms(&mut self.last_frame, ctx.frame_time());
        let visual = icon_visual(self.elapsed_ms, reduce);
        self.publish(visual.phase);

        if visual.opacity > 0.0 {
            let origin = ctx.origin() + self.icon_origin.to_vec2();
            let center = Point::new(
                origin.x + spec::ICON_SIZE / 2.0,
                origin.y + spec::ICON_SIZE / 2.0,
            );
            // Scale about the icon's own center. Spelled out rather than
            // reaching for a scale-about helper so the composition order is
            // visible: translate the center to the origin, scale, translate
            // back.
            scene.push_transform(
                Affine::translate(center.to_vec2())
                    * Affine::scale(visual.scale)
                    * Affine::translate(-center.to_vec2()),
            );
            scene.push_layer(
                origin,
                Size::new(spec::ICON_SIZE, spec::ICON_SIZE),
                visual.opacity as f32,
            );
            match &mut self.child {
                Some(pod) => pod.paint_child(ctx, scene),
                None => self.paint_fallback(ctx, scene, origin),
            }
            scene.pop_layer();
            scene.pop_transform();
        }

        // A beat with a visible endpoint, not a decorative loop: the unpaced
        // `request_frame` (`TickClass::Transition`) is the right class here
        // (`docs/CODE_STANDARDS.md`'s frame-pacing rule).
        if visual.phase != IconPhase::Done {
            ctx.request_frame();
        }
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        // Decorative: contributes no node of its own, but a container must
        // still forward or the child's subtree drops out of the a11y tree.
        if let Some(pod) = &self.child {
            pod.semantics_child(ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_frame_starts_scaled_down_and_invisible() {
        let v = icon_visual(0.0, false);
        assert_eq!(v.phase, IconPhase::SpringIn);
        assert!((v.scale - spec::ICON_ENTER_SCALE).abs() < 1e-9);
        assert!(v.opacity.abs() < 1e-9);
    }

    #[test]
    fn a_negative_clock_clamps_to_the_first_frame() {
        assert_eq!(icon_visual(-50.0, false), icon_visual(0.0, false));
    }

    #[test]
    fn the_spring_overshoots_past_full_size_before_settling() {
        // `cubic-bezier(0.34,1.6,0.5,1)` rises above 1 in its middle third.
        let overshot = (1..=9)
            .map(|i| icon_visual(spec::ICON_IN_MS * f64::from(i) / 10.0, false).scale)
            .any(|scale| scale > 1.0);
        assert!(overshot, "the spring must overshoot, not ease flat into 1");
    }

    #[test]
    fn opacity_never_leaves_the_unit_range_despite_the_overshoot() {
        for i in 0..=140 {
            let t = f64::from(i) * 10.0;
            let v = icon_visual(t, false);
            assert!(
                (0.0..=1.0).contains(&v.opacity),
                "opacity out of range at {t}ms: {}",
                v.opacity
            );
        }
    }

    #[test]
    fn the_hold_beat_is_fully_settled() {
        for t in [spec::ICON_IN_MS, 700.0, spec::ICON_DELAY_MS - 1.0] {
            let v = icon_visual(t, false);
            assert_eq!(v.phase, IconPhase::Hold, "at {t}ms");
            assert!((v.scale - 1.0).abs() < 1e-9);
            assert!((v.opacity - 1.0).abs() < 1e-9);
        }
    }

    #[test]
    fn dissolve_starts_exactly_at_the_burst_cue() {
        assert_eq!(
            icon_visual(spec::ICON_DELAY_MS - 0.001, false).phase,
            IconPhase::Hold
        );
        let cue = icon_visual(spec::ICON_DELAY_MS, false);
        assert_eq!(cue.phase, IconPhase::Dissolve);
        assert!((cue.scale - 1.0).abs() < 1e-9, "dissolve starts at rest");
        assert!((cue.opacity - 1.0).abs() < 1e-9);
    }

    #[test]
    fn dissolve_scales_up_while_fading_out() {
        let mid = icon_visual(spec::ICON_DELAY_MS + spec::ICON_DISSOLVE_MS / 2.0, false);
        assert_eq!(mid.phase, IconPhase::Dissolve);
        assert!(mid.scale > 1.0 && mid.scale < spec::ICON_DISSOLVE_SCALE);
        assert!(mid.opacity > 0.0 && mid.opacity < 1.0);
    }

    #[test]
    fn the_beat_ends_invisible_and_stays_there() {
        let end = spec::ICON_DELAY_MS + spec::ICON_DISSOLVE_MS;
        for t in [end, end + 1.0, 60_000.0] {
            let v = icon_visual(t, false);
            assert_eq!(v.phase, IconPhase::Done, "at {t}ms");
            assert!(v.opacity.abs() < 1e-9);
        }
    }

    #[test]
    fn the_phase_sequence_is_monotonic_across_the_whole_beat() {
        let mut seen = Vec::new();
        let mut t = 0.0;
        while t <= spec::ICON_DELAY_MS + spec::ICON_DISSOLVE_MS + 100.0 {
            let phase = icon_visual(t, false).phase;
            if seen.last() != Some(&phase) {
                seen.push(phase);
            }
            t += 5.0;
        }
        assert_eq!(
            seen,
            vec![
                IconPhase::SpringIn,
                IconPhase::Hold,
                IconPhase::Dissolve,
                IconPhase::Done
            ],
            "the beat must walk each phase exactly once, in order"
        );
    }

    #[test]
    fn reduced_motion_is_done_before_it_starts() {
        for t in [0.0, 100.0, 5_000.0] {
            let v = icon_visual(t, true);
            assert_eq!(v.phase, IconPhase::Done);
            assert!(v.opacity.abs() < 1e-9, "nothing is painted");
        }
    }

    #[test]
    fn the_embedded_icon_decodes_to_a_square_bitmap() {
        match splash_icon_art() {
            IconArt::Image(source) => {
                let size = source.natural_size();
                assert_eq!(size.width, size.height, "the app icon master is square");
                assert!(size.width > 0.0);
            }
            IconArt::Vector => panic!("assets/splash_icon.png must decode"),
        }
    }
}
