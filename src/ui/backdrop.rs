//! `page_backdrop` — the app's full-bleed background fill.
//!
//! A hand-rolled [`View`]/[`Widget`] pair built against [`frust::authoring`]
//! (the same shape `examples/huddle`'s `ui::fill_box` and
//! `examples/glyph-catalog`'s `AppBackground` take), needed for two reasons:
//!
//! 1. **No facade widget paints an unconditional "fill the available space"
//!    rect.** `SizedBox`/`Image` only tighten when the *incoming* constraint is
//!    already tight, and [`Stack`](frust::Stack) hands every child a LOOSE
//!    (min-zero) constraint — a naive background child collapses to zero. The
//!    trick is to declare a larger-than-any-viewport intrinsic size and let
//!    [`BoxConstraints::constrain`] clamp it down (the catalog's precedent).
//! 2. **The shell clears to `ColorScheme::surface`**, which is Glyph's
//!    `--bg-surface` (`#161a23`) — the card/input tone, not the page tone.
//!    Both the Connect screen and the splash overlay are authored against
//!    `--bg-base` (`#10131a`), so the app paints its own page background.
//!
//! # Which page tone, and why it moved
//!
//! `docs/redesign.html` paints the phone's whole screen `--bg-base`
//! (`.screen{background:var(--bg-base)}`) and gives the `.connect` column no
//! backing of its own; the older `docs/splash.html` mock used `--bg-void` for
//! both. The redesign is the visual ground truth for the screen, so this
//! backdrop resolves `--bg-base` and `features::connect`'s pieces back
//! themselves with the *same* role (`Palette::page`) — over the page that reads
//! as no backing at all, exactly as the redesign draws it, while still giving
//! the splash's rain something opaque to land on.
//!
//! **One function serves both renderings on purpose.** The splash overlay and
//! the `/` route paint the same tone by construction, so the handoff cannot
//! develop a one-frame colour step; a second, splash-only backdrop would be
//! free to drift. See `features::connect::presentation::pieces`' module docs.
//!
//! The splash additionally needs an **opaque, input-blocking** layer: the
//! navigator (already rendering the finished Connect screen) stays mounted
//! underneath the overlay for the whole sequence, so any transparent pixel
//! would leak the assembled screen through the rain, and any unblocked tap
//! would reach a field under it. [`BackdropView::block_input`] is that half.

use frust::Theme;
use frust::authoring::{
    BoxConstraints, BuildCtx, ChangeFlags, Color, EventCtx, EventResult, InputEvent, LayoutCtx,
    PaintCtx, PaintScene, Size, View, Widget,
};

/// Declared larger than any real viewport (logical px) so
/// [`BoxConstraints::constrain`] always clamps it back down to the incoming
/// maximum — see the module docs.
const INTRINSIC: f64 = 1.0e7;

/// The app's page background: a full-bleed fill of the active theme's
/// `surface_container_low` (Glyph's `--bg-base`, the page tone — see the
/// [module docs](self)).
///
/// Resolves the theme at **build** time through `use_context::<Theme>()` (the
/// app-code delivery path, mirroring `examples/glyph-catalog`'s own root
/// background), falling back to [`Theme::glyph_baseline`] — never
/// `Theme::neutral`/`m3_baseline` — per `lib.rs`'s design-system note.
pub fn page_backdrop<State: 'static>() -> BackdropView<State> {
    let color = frust::use_context::<Theme>()
        .unwrap_or_else(Theme::glyph_baseline)
        .scheme()
        .surface_container_low;
    BackdropView {
        color,
        block_input: false,
        state: std::marker::PhantomData,
    }
}

/// The declarative half of the page background. See [`page_backdrop`].
pub struct BackdropView<State: 'static> {
    color: Color,
    block_input: bool,
    /// `View<State>` is generic over the app state even though a decorative
    /// leaf never touches it.
    state: std::marker::PhantomData<fn() -> State>,
}

impl<State: 'static> BackdropView<State> {
    /// Swallow every pointer event that reaches this layer (default `false`).
    ///
    /// A [`Stack`](frust::Stack) hit-tests its children top-down and stops at
    /// the first `Handled`, so a full-bleed backdrop with this set blocks
    /// everything painted *below* it in the same stack — what the splash
    /// overlay needs while the live Connect screen sits underneath it.
    pub fn block_input(mut self, block: bool) -> Self {
        self.block_input = block;
        self
    }
}

/// The retained half of the page background. See [`page_backdrop`].
pub struct BackdropWidget {
    color: Color,
    block_input: bool,
}

impl<State: 'static> View<State> for BackdropView<State> {
    type Element = BackdropWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> BackdropWidget {
        BackdropWidget {
            color: self.color,
            block_input: self.block_input,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut BackdropWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        element.block_input = self.block_input;
        if prev.color == self.color {
            return ChangeFlags::NONE;
        }
        element.color = self.color;
        ChangeFlags::PAINT
    }
}

impl Widget for BackdropWidget {
    fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        bc.constrain(Size::new(INTRINSIC, INTRINSIC))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        scene.fill_rect(ctx.origin(), ctx.size(), self.color);
    }

    fn event(&mut self, _ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        // Pointer events only: a key/IME event is focus-routed, never
        // hit-tested, so it cannot reach a backdrop that never takes focus.
        if self.block_input && matches!(event, InputEvent::Pointer(_)) {
            return EventResult::Handled;
        }
        EventResult::Ignored
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use frust::authoring::{Point, PointerButton, PointerEvent, PointerPhase};

    fn down() -> InputEvent {
        InputEvent::Pointer(PointerEvent {
            phase: PointerPhase::Down,
            position: Point::new(10.0, 10.0),
            button: PointerButton::Primary,
        })
    }

    #[test]
    fn the_backdrop_clamps_its_oversized_intrinsic_to_the_incoming_maximum() {
        let mut widget = BackdropWidget {
            color: Color::BLACK,
            block_input: false,
        };
        let mut ctx = LayoutCtx::new();
        // The loose (min-zero) constraint a `Stack` hands every child.
        let bc = BoxConstraints::loose(Size::new(390.0, 844.0));
        assert_eq!(widget.layout(&mut ctx, &bc), Size::new(390.0, 844.0));
    }

    #[test]
    fn the_page_tone_is_glyphs_bg_base_not_the_void() {
        // `--bg-base` #10131a, reached through the role rather than the hex.
        // The splash overlay and the `/` route both mount this one function,
        // so this pins the tone on *both* sides of the handoff at once.
        let view = page_backdrop::<()>();
        assert_eq!(view.color, Color::from_rgb8(0x10, 0x13, 0x1a));
        assert_ne!(
            view.color,
            Theme::glyph_baseline().scheme().surface_container_lowest,
            "the page tone is `--bg-base`, not the darker `--bg-void`"
        );
    }

    #[test]
    fn input_blocking_is_opt_in() {
        let mut open = BackdropWidget {
            color: Color::BLACK,
            block_input: false,
        };
        let mut blocked = BackdropWidget {
            color: Color::BLACK,
            block_input: true,
        };
        let mut state = ();
        let mut ctx = EventCtx::new(&mut state, Point::ZERO, Size::new(390.0, 844.0));
        assert_eq!(open.event(&mut ctx, &down()), EventResult::Ignored);
        assert_eq!(blocked.event(&mut ctx, &down()), EventResult::Handled);
    }
}
