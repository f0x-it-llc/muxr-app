//! The one-shot glyph burst thrown outward from the icon's center as it dissolves. Past
//! [`spec::BURST_MS`] it paints nothing and stops requesting frames; reduced motion skips it.

use frust::authoring::text::TextStyle;
use frust::authoring::{
    Affine, BoxConstraints, BuildCtx, ChangeFlags, LayoutCtx, PaintCtx, PaintScene, Point, Size,
    View, Widget,
};
use frust::{FrameTime, Theme};

use super::spec;
use super::{
    ShapedGlyph, SplashRng, accent_ink, delta_ms, fill_size, mono_style, reduce_motion, with_alpha,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BurstParticle {
    /// Radians.
    pub angle: f64,
    pub distance: f64,
    pub glyph: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParticleMotion {
    pub dx: f64,
    pub dy: f64,
    pub scale: f64,
    pub opacity: f64,
}

/// Draw order per particle (glyph, angle, distance) matches the source's `spawnBurst` loop.
pub fn spawn_particles(seed: u64) -> Vec<BurstParticle> {
    let mut rng = SplashRng::new(seed);
    (0..spec::BURST_COUNT)
        .map(|_| BurstParticle {
            glyph: rng.below(spec::BURST_CHARS.len()),
            angle: rng.unit() * std::f64::consts::TAU,
            distance: rng.range(
                spec::BURST_DIST_BASE,
                spec::BURST_DIST_BASE + spec::BURST_DIST_SPAN,
            ),
        })
        .collect()
}

/// Past [`spec::BURST_MS`] the terminal state (fully flown, invisible) is pinned.
pub fn particle_motion(particle: &BurstParticle, elapsed_ms: f64) -> ParticleMotion {
    let t = (elapsed_ms.max(0.0) / spec::BURST_MS).clamp(0.0, 1.0);
    let travel = spec::BURST_MOVE_EASE.transform(t);
    let fade = spec::BURST_FADE_EASE.transform(t);
    ParticleMotion {
        dx: particle.angle.cos() * particle.distance * travel,
        dy: particle.angle.sin() * particle.distance * travel,
        scale: 1.0 + (spec::BURST_END_SCALE - 1.0) * travel,
        opacity: (1.0 - fade).clamp(0.0, 1.0),
    }
}

pub fn burst_finished(elapsed_ms: f64) -> bool {
    elapsed_ms >= spec::BURST_MS
}

pub fn burst<State: 'static>(seed: u64) -> BurstView<State> {
    BurstView {
        seed,
        state: std::marker::PhantomData,
    }
}

pub struct BurstView<State: 'static> {
    seed: u64,
    state: std::marker::PhantomData<fn() -> State>,
}

pub struct BurstWidget {
    particles: Vec<BurstParticle>,
    /// One per [`spec::BURST_CHARS`] entry; `style` is kept so a theme swap re-shapes them.
    glyphs: Vec<ShapedGlyph>,
    style: Option<TextStyle>,
    center: Point,
    elapsed_ms: f64,
    last_frame: Option<FrameTime>,
}

impl<State: 'static> View<State> for BurstView<State> {
    type Element = BurstWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> BurstWidget {
        BurstWidget {
            particles: spawn_particles(self.seed),
            glyphs: Vec::new(),
            style: None,
            center: Point::ZERO,
            elapsed_ms: 0.0,
            last_frame: None,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut BurstWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        // A re-seeded burst redraws its particles but keeps its clock.
        if prev.seed != self.seed {
            element.particles = spawn_particles(self.seed);
            return ChangeFlags::PAINT;
        }
        ChangeFlags::NONE
    }
}

impl Widget for BurstWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let size = fill_size(bc, Size::new(spec::ICON_SIZE, spec::ICON_SIZE));
        self.center = Point::new(size.width / 2.0, size.height / 2.0);

        let theme = Theme::from_layout_ctx(ctx).cloned();
        let style = mono_style(theme.as_ref(), spec::BURST_FONT_SIZE);
        if self.style.as_ref() != Some(&style) {
            self.glyphs = spec::BURST_CHARS
                .iter()
                .map(|glyph| ShapedGlyph::shape(ctx, glyph, &style))
                .collect();
            self.style = Some(style);
        }
        size
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let theme = Theme::from_paint_ctx(ctx);
        if reduce_motion(theme) {
            return;
        }
        self.elapsed_ms += delta_ms(&mut self.last_frame, ctx.frame_time());
        if burst_finished(self.elapsed_ms) {
            return;
        }

        let ink = accent_ink(theme);
        let center = ctx.origin() + self.center.to_vec2();
        for particle in &self.particles {
            let Some(glyph) = self.glyphs.get(particle.glyph) else {
                continue;
            };
            let motion = particle_motion(particle, self.elapsed_ms);
            let box_size = glyph.size();
            // translate(target) · scale(s) · translate(-box/2): scales about the glyph's center.
            let transform = Affine::translate((center.x + motion.dx, center.y + motion.dy))
                * Affine::scale(motion.scale)
                * Affine::translate((-box_size.width / 2.0, -box_size.height / 2.0));
            glyph.draw(scene, transform, with_alpha(ink, motion.opacity));
        }

        // A one-shot with a visible endpoint: the unpaced class, not the paced cosmetic loop.
        ctx.request_frame();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seed_fixes_the_whole_particle_set() {
        assert_eq!(spawn_particles(11), spawn_particles(11));
        assert_ne!(spawn_particles(11), spawn_particles(12));
    }

    #[test]
    fn every_particle_lands_inside_the_spec_ranges() {
        let particles = spawn_particles(3);
        assert_eq!(particles.len(), spec::BURST_COUNT);
        for p in &particles {
            assert!((0.0..std::f64::consts::TAU).contains(&p.angle));
            assert!(
                p.distance >= spec::BURST_DIST_BASE
                    && p.distance < spec::BURST_DIST_BASE + spec::BURST_DIST_SPAN
            );
            assert!(p.glyph < spec::BURST_CHARS.len());
        }
    }

    #[test]
    fn a_particle_starts_at_the_center_full_size_and_opaque() {
        let p = BurstParticle {
            angle: 0.7,
            distance: 90.0,
            glyph: 0,
        };
        let m = particle_motion(&p, 0.0);
        assert!(m.dx.abs() < 1e-9 && m.dy.abs() < 1e-9);
        assert!((m.scale - 1.0).abs() < 1e-9);
        assert!((m.opacity - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_particle_flies_the_full_distance_along_its_angle() {
        let p = BurstParticle {
            angle: std::f64::consts::FRAC_PI_2, // straight down
            distance: 120.0,
            glyph: 0,
        };
        let end = particle_motion(&p, spec::BURST_MS);
        assert!(end.dx.abs() < 1e-6, "no horizontal drift straight down");
        assert!((end.dy - 120.0).abs() < 1e-6, "dy {} != 120", end.dy);
        assert!((end.scale - spec::BURST_END_SCALE).abs() < 1e-9);
        assert!(end.opacity.abs() < 1e-9);
    }

    #[test]
    fn travel_is_monotonic_and_front_loaded() {
        let p = BurstParticle {
            angle: 0.0,
            distance: 100.0,
            glyph: 0,
        };
        let mut previous = -1.0;
        for i in 0..=20 {
            let dx = particle_motion(&p, spec::BURST_MS * f64::from(i) / 20.0).dx;
            assert!(dx >= previous - 1e-9, "travel must never go backwards");
            previous = dx;
        }
        let third = particle_motion(&p, spec::BURST_MS / 3.0).dx;
        assert!(third > 50.0, "front-loaded easing, got {third} of 100");
    }

    #[test]
    fn scale_and_opacity_shrink_monotonically() {
        let p = BurstParticle {
            angle: 2.0,
            distance: 70.0,
            glyph: 3,
        };
        let mut last_scale = f64::INFINITY;
        let mut last_opacity = f64::INFINITY;
        for i in 0..=20 {
            let m = particle_motion(&p, spec::BURST_MS * f64::from(i) / 20.0);
            assert!(m.scale <= last_scale + 1e-9);
            assert!(m.opacity <= last_opacity + 1e-9);
            assert!((0.0..=1.0).contains(&m.opacity));
            last_scale = m.scale;
            last_opacity = m.opacity;
        }
    }

    #[test]
    fn motion_is_pinned_past_the_end_and_clamped_before_the_start() {
        let p = BurstParticle {
            angle: 1.0,
            distance: 80.0,
            glyph: 1,
        };
        assert_eq!(
            particle_motion(&p, spec::BURST_MS * 10.0),
            particle_motion(&p, spec::BURST_MS)
        );
        assert_eq!(particle_motion(&p, -100.0), particle_motion(&p, 0.0));
    }

    #[test]
    fn the_burst_self_finishes_at_its_duration() {
        assert!(!burst_finished(0.0));
        assert!(!burst_finished(spec::BURST_MS - 1.0));
        assert!(burst_finished(spec::BURST_MS));
        assert!(burst_finished(spec::BURST_MS + 1_000.0));
    }
}
