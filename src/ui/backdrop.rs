//! The full-bleed page background. No facade widget paints an unconditional fill, so this
//! declares an oversized intrinsic size and lets the incoming constraint clamp it down.

use frust::Theme;
use frust::authoring::{
    BoxConstraints, BuildCtx, ChangeFlags, Color, EventCtx, EventResult, InputEvent, LayoutCtx,
    PaintCtx, PaintScene, Size, View, Widget,
};

/// Larger than any real viewport (logical px), so `constrain` always clamps to the maximum.
const INTRINSIC: f64 = 1.0e7;

/// The theme's `surface_container_low`. The splash overlay and the `/` route both mount this,
/// so the handoff cannot show a one-frame colour step.
pub fn page_backdrop<State: 'static>() -> BackdropView<State> {
    let color = frust::use_context::<Theme>()
        .unwrap_or_else(frust_glyph::baseline)
        .scheme()
        .surface_container_low;
    BackdropView {
        color,
        block_input: false,
        state: std::marker::PhantomData,
    }
}

pub struct BackdropView<State: 'static> {
    color: Color,
    block_input: bool,
    state: std::marker::PhantomData<fn() -> State>,
}

impl<State: 'static> BackdropView<State> {
    /// Swallows every pointer event, blocking what sits below it in the same stack; the splash
    /// overlay needs this while the live Connect screen sits underneath.
    pub fn block_input(mut self, block: bool) -> Self {
        self.block_input = block;
        self
    }
}

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
        // Key and IME events are focus-routed, never hit-tested.
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
        let view = page_backdrop::<()>();
        assert_eq!(view.color, Color::from_rgb8(0x10, 0x13, 0x1a));
        assert_ne!(
            view.color,
            frust_glyph::baseline().scheme().surface_container_lowest,
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
