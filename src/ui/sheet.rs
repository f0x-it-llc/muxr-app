//! A modal bottom sheet: [`show_sheet`] pushes one, [`sheet_scaffold`] is the
//! chrome it wraps a caller's body in.
//!
//! # This is a workaround, and it is meant to be deleted
//!
//! **Frust has no bottom-sheet primitive in the Glyph catalog** (FINDINGS #22).
//! `frust::glyph` ships a dialog, a command palette, a menu and a toast host —
//! no sheet — and the M3 `frust::bottom_sheet` is not compiled into this app
//! (`Cargo.toml` enables the `glyph` catalog only). So this module
//! hand-composes the sheet the same way `examples/glyph-catalog`'s
//! `pages/overlays.rs` does: a **transparent navigator page** pushed with
//! [`PageTransition::SlideUp`], carrying a bottom-anchored panel clamped to
//! [`MAX_WIDTH`] on wide layouts. **When frust grows a Glyph sheet widget,
//! delete this file and re-point [`show_sheet`]'s two call sites at it** — the
//! API is deliberately one function taking a body builder so that swap is
//! cheap.
//!
//! # What the scaffold adds over the catalog's hand-rolled version
//!
//! `overlays.rs` stops at "a card pinned to the bottom" — enough for a demo
//! page, not for a modal. [`SheetScaffoldWidget`] is one retained widget (the
//! `frust::authoring` seam, like `crate::ui::backdrop`) so it can own the three
//! things a demo doesn't need:
//!
//! - a **scrim** (`colors.scrim` at [`SCRIM_ALPHA`]) it paints itself, and the
//!   modal barrier that goes with it — every pointer event that misses the
//!   panel is swallowed, and a press that both starts and ends outside the
//!   panel dismisses (the fire-on-up-inside convention, inverted; exactly
//!   `frust::glyph::dialog`'s scrim contract);
//! - the **panel rect**, which is what makes that hit test possible at all: a
//!   plain `Align`-inside-`Stack` composition has no widget that knows where
//!   the panel ended up, so a tap on the panel itself would fall through to
//!   the scrim and dismiss the sheet;
//! - the **bottom safe area**, read from `LayoutCtx::window_insets` and added
//!   to the panel's own bottom padding so a gesture-nav bar never overlaps the
//!   body.
//!
//! Dismissal pops the page, so the navigator's own reversed `SlideUp` plays
//! the exit; an Android back press pops it too, through the default
//! `BackPolicy::Pop`, with no extra wiring.

use std::rc::Rc;

use frust::authoring::{
    BoxConstraints, BuildCtx, ChangeFlags, ChildPod, Color, EventCtx, EventResult, InputEvent, Key,
    LayoutCtx, NamedKey, PaintCtx, PaintScene, Point, PointerPhase, Rect, Role, SemanticsCtx, Size,
    View, Widget, any, build_child, rebuild_child, route_event_single, teardown_child,
};
use frust::{AnyView, NavigatorController, PageTransition, PopResult, Theme, TransitionSpec};

/// M3/Flutter bottom-sheet width convention: edge-to-edge on narrow screens,
/// capped and horizontally centered on wide ones (Material 3 gives modal
/// bottom sheets a 640dp max width; Flutter's `BottomSheet` constrains
/// likewise) — transcribed from `examples/glyph-catalog`'s
/// `pages/overlays.rs::SHEET_MAX_WIDTH`, the precedent this module follows.
pub const MAX_WIDTH: f64 = 640.0;

/// Scrim opacity behind the panel — the same value `frust::glyph::dialog`
/// picked for its modal barrier (heavier than M3's 32% so a Glyph panel reads
/// against the void surface); matched deliberately, so a sheet and a dialog
/// dim the app identically.
const SCRIM_ALPHA: f32 = 0.6;
/// Panel padding on the left/right/bottom edges, in logical px — Glyph's roomy
/// `--space-5` gutter (`glyph::dialog`'s `DIALOG_PADDING`). The top gutter is
/// the drag-handle zone instead (see [`handle_zone`]).
const PANEL_PAD: f64 = 20.0;
/// Drag-handle pill width/height, in logical px. An original, hand-picked
/// pair: the platform conventions agree closely (M3's drag handle is 32×4,
/// UIKit's sheet grabber ~36×5) and this sits between them.
const HANDLE_WIDTH: f64 = 36.0;
const HANDLE_HEIGHT: f64 = 4.0;
/// Gap above the handle (from the panel's top edge) and below it (to the
/// body), in logical px — Glyph's `--space-3`/`--space-4` steps.
const HANDLE_TOP: f64 = 12.0;
const HANDLE_GAP: f64 = 16.0;
/// The handle pill's ink: `on_surface_variant` faded, since no `ColorScheme`
/// role carries a "drag affordance" tone (the same alpha-derivation
/// `features::connect`'s `fg_dim` documents).
const HANDLE_ALPHA: f32 = 0.5;

/// Unthemed-fallback panel surface (Glyph dark `bg-raised` `#1e2430`) — a
/// theme resolves `colors.surface_container_high`, matching the dialog panel.
const FALLBACK_SURFACE: Color = Color::from_rgb8(0x1e, 0x24, 0x30);
/// Unthemed-fallback panel corner radius — Glyph's `--radius-lg` (14px); a
/// theme resolves `shape.large`.
const FALLBACK_RADIUS: f64 = 14.0;
/// Unthemed-fallback scrim base color; a theme resolves `colors.scrim`.
const FALLBACK_SCRIM: Color = Color::from_rgb8(0x00, 0x00, 0x00);
/// Unthemed-fallback handle ink (Glyph dark `fg-muted`); a theme resolves
/// `colors.on_surface_variant`.
const FALLBACK_HANDLE: Color = Color::from_rgb8(0xa3, 0x9c, 0x88);

/// The vertical band above the body: the panel's top gutter, the drag handle,
/// and the gap under it.
fn handle_zone() -> f64 {
    HANDLE_TOP + HANDLE_HEIGHT + HANDLE_GAP
}

/// Coerce a possibly-infinite constraint dimension to a finite value (mirrors
/// `glyph::dialog`'s helper of the same name).
fn finite_or_zero(v: f64) -> f64 {
    if v.is_finite() { v } else { 0.0 }
}

/// Push `build_body`'s content as a modal bottom sheet over the current page.
///
/// One call is the whole interaction: the sheet slides up, dims the screen,
/// and dismisses on a scrim tap, `Escape`, or a back press. `build_body` is
/// re-invoked on every rebuild while the sheet is up (the navigator's
/// page-builder contract), so it may read signals freely.
///
/// ```ignore
/// show_sheet(&state.nav.controller().clone(), move || {
///     any(text("Pair a new host").size(13.5))
/// });
/// ```
pub fn show_sheet<State: 'static>(
    nav: &NavigatorController<State>,
    build_body: impl Fn() -> AnyView<State> + 'static,
) {
    let dismiss_nav = nav.clone();
    nav.push_transparent_for_result(
        move || {
            let pop_nav = dismiss_nav.clone();
            any(sheet_scaffold(build_body()).on_dismiss(move || pop_nav.pop()))
        },
        TransitionSpec::duration(PageTransition::SlideUp),
        // The sheet answers through its body's own callbacks (which own
        // `&mut State` already); the pop itself carries nothing.
        |_state: &mut State, _result: PopResult| {},
    );
}

/// The sheet chrome around `body`: scrim, bottom-anchored panel, drag handle,
/// safe-area padding. Wire [`on_dismiss`](SheetScaffoldView::on_dismiss) to
/// close it — [`show_sheet`] does that for you, and is what app code should
/// call.
pub fn sheet_scaffold<State: 'static>(body: AnyView<State>) -> SheetScaffoldView<State> {
    SheetScaffoldView {
        body,
        on_dismiss: None,
    }
}

/// A state-free "close this sheet" callback — the same shape (and for the same
/// reason) as `frust::glyph::dialog`'s: closing is a `NavigatorController::pop`,
/// which needs no app state.
type OnDismiss = Rc<dyn Fn()>;

/// The declarative half of the sheet chrome. See [`sheet_scaffold`].
pub struct SheetScaffoldView<State: 'static> {
    body: AnyView<State>,
    on_dismiss: Option<OnDismiss>,
}

impl<State: 'static> SheetScaffoldView<State> {
    /// Set the dismissal callback fired by a scrim tap, `Escape`, or an
    /// outside-press release. Without one the sheet is a barrier that only its
    /// own body can close.
    pub fn on_dismiss<F: Fn() + 'static>(mut self, on_dismiss: F) -> Self {
        self.on_dismiss = Some(Rc::new(on_dismiss));
        self
    }
}

/// The retained half of the sheet chrome. See [`sheet_scaffold`].
pub struct SheetScaffoldWidget {
    body: ChildPod,
    on_dismiss: Option<OnDismiss>,
    /// The panel rect in this widget's own local coordinates — resolved in
    /// `layout`, read by `paint` and by the barrier hit test.
    panel: Rect,
    /// A press is in flight (the barrier captured the pointer).
    captured: bool,
    /// Whether that press started outside the panel — only an outside press
    /// released outside dismisses.
    down_outside: bool,
}

impl<State: 'static> View<State> for SheetScaffoldView<State> {
    type Element = SheetScaffoldWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> SheetScaffoldWidget {
        SheetScaffoldWidget {
            body: build_child(&self.body, ctx),
            on_dismiss: self.on_dismiss.clone(),
            panel: Rect::ZERO,
            captured: false,
            down_outside: false,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut SheetScaffoldWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        // Closures aren't comparable; reinstall the dismiss adapter every time.
        element.on_dismiss = self.on_dismiss.clone();
        rebuild_child(&prev.body, &self.body, &mut element.body, ctx)
    }

    fn teardown(&self, element: &mut SheetScaffoldWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.body, &mut element.body, ctx);
    }
}

impl SheetScaffoldWidget {
    /// Fire the dismissal callback, if one is wired.
    fn dismiss(&self) {
        if let Some(on_dismiss) = &self.on_dismiss {
            on_dismiss();
        }
    }
}

impl Widget for SheetScaffoldWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let area_w = finite_or_zero(bc.max().width);
        let area_h = finite_or_zero(bc.max().height);

        // Edge-to-edge on a phone, clamped and centered past MAX_WIDTH.
        let panel_w = area_w.min(MAX_WIDTH);
        let content_w = (panel_w - 2.0 * PANEL_PAD).max(0.0);
        // Tight width (the body spans the panel), loose height (it decides how
        // tall the sheet is).
        let body_bc = BoxConstraints::new(
            Size::new(content_w, 0.0),
            Size::new(content_w, f64::INFINITY),
        );
        let body_size = self.body.layout_child(ctx, &body_bc);

        // The gesture-nav/home-indicator inset widens the bottom gutter rather
        // than replacing it, so the body never sits flush against the bar.
        let safe_bottom = ctx.window_insets().padding().bottom;
        // Never taller than the viewport, however tall the body asked to be.
        let panel_h = (handle_zone() + body_size.height + PANEL_PAD + safe_bottom).min(area_h);

        let panel_x = ((area_w - panel_w) / 2.0).max(0.0);
        let panel_y = (area_h - panel_h).max(0.0);
        self.panel = Rect::new(panel_x, panel_y, panel_x + panel_w, panel_y + panel_h);
        self.body
            .set_origin(Point::new(panel_x + PANEL_PAD, panel_y + handle_zone()));

        bc.constrain(Size::new(area_w, area_h))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let theme = Theme::from_paint_ctx(ctx);
        let radius = theme.map(|t| t.shape.large).unwrap_or(FALLBACK_RADIUS);
        let scrim = theme
            .map(|t| t.scheme().scrim)
            .unwrap_or(FALLBACK_SCRIM)
            .with_alpha(SCRIM_ALPHA);
        let surface = theme
            .map(|t| t.scheme().surface_container_high)
            .unwrap_or(FALLBACK_SURFACE);
        let handle_ink = theme
            .map(|t| t.scheme().on_surface_variant)
            .unwrap_or(FALLBACK_HANDLE)
            .with_alpha(HANDLE_ALPHA);

        // The modal barrier, full-bleed and under everything else.
        scene.fill_rect(ctx.origin(), ctx.size(), scrim);

        let panel_origin = Point::new(
            ctx.origin().x + self.panel.x0,
            ctx.origin().y + self.panel.y0,
        );
        // Only the TOP corners should look rounded: the panel is flush with
        // the bottom edge, so it is drawn `radius` taller than it is and the
        // bottom rounding falls off-screen. (`PaintScene` has no per-corner
        // radius; this is the cheap, standard way to get a top-rounded sheet.)
        scene.fill_rounded_rect(
            panel_origin,
            Size::new(self.panel.width(), self.panel.height() + radius),
            radius,
            surface,
        );

        // The drag-handle affordance: centered pill near the panel's top edge.
        scene.fill_rounded_rect(
            Point::new(
                panel_origin.x + (self.panel.width() - HANDLE_WIDTH) / 2.0,
                panel_origin.y + HANDLE_TOP,
            ),
            Size::new(HANDLE_WIDTH, HANDLE_HEIGHT),
            HANDLE_HEIGHT / 2.0,
            handle_ink,
        );

        self.body.paint_child(ctx, scene);
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        // A press anywhere claims focus so a later Escape has a chain to
        // travel (`glyph::dialog`'s opt-in).
        if let InputEvent::Pointer(p) = event
            && p.phase == PointerPhase::Down
        {
            ctx.request_focus();
        }
        // The body (or a captured/focused descendant of it) sees the event
        // first; only what it ignores reaches the barrier below.
        if route_event_single(&mut self.body, ctx, event) == EventResult::Handled {
            return EventResult::Handled;
        }
        if let InputEvent::Key(key_event) = event {
            if key_event.key == Key::Named(NamedKey::Escape) {
                self.dismiss();
                return EventResult::Handled;
            }
            return EventResult::Ignored;
        }
        let InputEvent::Pointer(p) = event else {
            return EventResult::Ignored;
        };
        match p.phase {
            PointerPhase::Down => {
                self.captured = true;
                self.down_outside = !self.panel.contains(p.position);
                ctx.capture_pointer();
                EventResult::Handled
            }
            PointerPhase::Move => {
                if self.captured {
                    EventResult::Handled
                } else {
                    EventResult::Ignored
                }
            }
            PointerPhase::Up => {
                if !self.captured {
                    return EventResult::Ignored;
                }
                if self.down_outside && !self.panel.contains(p.position) {
                    self.dismiss();
                }
                self.captured = false;
                EventResult::Handled
            }
            // Cancel never touches state — just clear the barrier flag
            // (`docs/CODE_STANDARDS.md`'s Cancel rule).
            PointerPhase::Cancel => {
                self.captured = false;
                EventResult::Handled
            }
        }
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        ctx.push_container(
            Role::Dialog,
            |node| node.set_modal(),
            |ctx| {
                self.body.semantics_child(ctx);
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_handle_zone_is_the_sum_of_its_three_bands() {
        assert!((handle_zone() - (HANDLE_TOP + HANDLE_HEIGHT + HANDLE_GAP)).abs() < 1e-9);
    }

    #[test]
    fn derived_alphas_stay_legal() {
        for alpha in [SCRIM_ALPHA, HANDLE_ALPHA] {
            assert!((0.0..=1.0).contains(&alpha));
        }
    }

    #[test]
    fn the_panel_never_narrows_past_the_material_cap() {
        // A tripwire on the clamp's direction: the panel is `min(screen, cap)`,
        // so a phone-width screen is edge-to-edge and a tablet stops at the cap.
        assert_eq!(390.0f64.min(MAX_WIDTH), 390.0, "phone: edge to edge");
        assert_eq!(1024.0f64.min(MAX_WIDTH), MAX_WIDTH, "tablet: clamped");
    }
}
