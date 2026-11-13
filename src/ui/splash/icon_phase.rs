//! The splash's first beat: the app icon springs in, holds, then scales up and dissolves.
//! No app-reachable path blits decoded pixels, so it composes `frust`'s `Image` widget as a
//! child pod under a scale transform and a translucent layer.

use frust::authoring::text::TextStyle;
use frust::authoring::{
    Affine, BoxConstraints, BuildCtx, ChangeFlags, ChildPod, LayoutCtx, PaintCtx, PaintScene,
    Point, SemanticsCtx, Size, View, Widget, build_child, rebuild_child, teardown_child,
};
use frust::{FrameTime, Image, ImageFit, ImageSource, ImageView, RwSignal, Set, Theme};

use super::spec;
use super::{ShapedGlyph, delta_ms, fill_size, mono_style, reduce_motion};

/// Embedded so a cold start never touches the filesystem; the 1024px iOS app-icon master.
const SPLASH_ICON_PNG: &[u8] = include_bytes!("../../../assets/splash_icon.png");

const FALLBACK_GLYPH: &str = "▣";
/// Hand-tuned: the design has no vector stand-in to match.
const FALLBACK_GLYPH_FRACTION: f64 = 0.5;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IconPhase {
    #[default]
    SpringIn,
    Hold,
    /// The burst and assembly cue.
    Dissolve,
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IconVisual {
    pub phase: IconPhase,
    /// May exceed 1 during the spring's overshoot.
    pub scale: f64,
    pub opacity: f64,
}

/// Negative elapsed clamps to zero; past the end the terminal state holds, so a late frame
/// never resurrects the icon.
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
        // One spring drives both; its overshoot is wanted for scale, clamped for opacity.
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

#[derive(Clone)]
pub enum IconArt {
    Image(ImageSource),
    Vector,
}

/// Call once and retain the result: decoding is a full PNG decode plus RGBA allocation, and
/// `IconArt` clones cheaply afterwards.
pub fn splash_icon_art() -> IconArt {
    match ImageSource::decode(SPLASH_ICON_PNG) {
        Ok(source) => IconArt::Image(source),
        Err(error) => {
            log::warn!("splash icon failed to decode ({error}); using the vector fallback");
            IconArt::Vector
        }
    }
}

pub fn icon_phase<State: 'static>(art: IconArt) -> IconPhaseView<State> {
    let child = match &art {
        IconArt::Image(source) => Some(Image(source.clone()).fit(ImageFit::Contain)),
        IconArt::Vector => None,
    };
    IconPhaseView {
        child,
        on_phase: None,
        state: std::marker::PhantomData,
    }
}

pub struct IconPhaseView<State: 'static> {
    child: Option<ImageView>,
    on_phase: Option<RwSignal<IconPhase>>,
    state: std::marker::PhantomData<fn() -> State>,
}

impl<State: 'static> IconPhaseView<State> {
    /// Written only on a change, so a tracked reader rebuilds once per phase, not per frame.
    pub fn on_phase(mut self, signal: RwSignal<IconPhase>) -> Self {
        self.on_phase = Some(signal);
        self
    }
}

pub struct IconPhaseWidget {
    child: Option<ChildPod>,
    /// Shaped only for the vector fallback; the style is kept so a theme swap re-shapes it.
    fallback_glyph: Option<ShapedGlyph>,
    fallback_style: Option<TextStyle>,
    icon_origin: Point,
    elapsed_ms: f64,
    last_frame: Option<FrameTime>,
    on_phase: Option<RwSignal<IconPhase>>,
    published: Option<IconPhase>,
}

impl IconPhaseWidget {
    fn publish(&mut self, phase: IconPhase) {
        if self.published == Some(phase) {
            return;
        }
        self.published = Some(phase);
        if let Some(signal) = self.on_phase {
            signal.set(phase);
        }
    }

    fn paint_fallback(&self, ctx: &PaintCtx, scene: &mut dyn PaintScene, origin: Point) {
        let theme = Theme::from_paint_ctx(ctx);
        // Accent-role split: `primary_container` is the bright fill, `primary` is accent ink.
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
            child: self
                .child
                .as_ref()
                .map(|child| build_child::<State, _>(child, ctx)),
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
            // The `Image` view's rebuild is an `Arc`-pointer check, never a re-decode.
            (Some(prev_child), Some(next_child), Some(pod)) => {
                rebuild_child::<State, _>(prev_child, next_child, pod, ctx)
            }
            (Some(prev_child), None, Some(pod)) => {
                teardown_child::<State, _>(prev_child, pod, ctx);
                element.child = None;
                ChangeFlags::LAYOUT | ChangeFlags::PAINT
            }
            (None, Some(next_child), _) => {
                element.child = Some(build_child::<State, _>(next_child, ctx));
                ChangeFlags::LAYOUT | ChangeFlags::PAINT
            }
            _ => ChangeFlags::NONE,
        }
    }

    fn teardown(&self, element: &mut IconPhaseWidget, ctx: &mut BuildCtx<'_>) {
        if let (Some(child), Some(pod)) = (&self.child, &mut element.child) {
            teardown_child::<State, _>(child, pod, ctx);
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

        // A beat with a visible endpoint uses the unpaced `request_frame`, not a decorative loop's.
        if visual.phase != IconPhase::Done {
            ctx.request_frame();
        }
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        // Decorative, but a container must forward or the child's subtree leaves the a11y tree.
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
