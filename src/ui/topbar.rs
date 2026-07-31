//! `muxr_topbar` — the app's branded top bar (`docs/redesign.html`'s
//! `.topbar`), shared by every screen that carries one.
//!
//! ```text
//! ┌──────────────────────────────────────────────┐
//! │ ▪▪  muxr        100.71.31.57              ⚙  │  46px, bg-surface
//! │ ▪▪                                           │  + hairline bottom border
//! └──────────────────────────────────────────────┘
//! ```
//!
//! Three zones: the **brand** (a 16px 2×2 amber grid plus the `muxr` wordmark
//! in the display face), an **absolutely centered title** slot, and one
//! optional **trailing icon** slot. Sessions passes the connected host and a
//! `⚙` that pushes `/settings`; the terminal screen (Phase 3) passes `terminal`
//! and a `⋮` — which is why the trailing slot is a generic glyph + `on_press`
//! closure rather than anything settings-specific, and why the whole thing is
//! `<State>`-generic like [`crate::ui::dialogs`] rather than pinned to
//! `AppState`. The Connect screen deliberately has **no** top bar.
//!
//! # Why a hand-rolled widget rather than `glyph::app_bar`
//!
//! `frust::glyph::app_bar` is a 52px bar whose title is *left-aligned* next to
//! its leading slot, with no brand-mark affordance. The redesign's bar is 46px
//! with a title centered on the **bar**, not on the space left over between the
//! slots — a difference no `AppBarView` builder expresses. So this is a
//! [`View`]/[`Widget`] pair over [`frust::authoring`] (the
//! `crate::ui::backdrop`/`crate::ui::sheet` precedent), which also lets it paint
//! the brand mark's four cells directly instead of nesting four decorative
//! swatch widgets.
//!
//! # Top-inset consumption (the Flutter-parity point)
//!
//! Layout adds [`WindowInsets::padding().top`](frust::authoring::WindowInsets)
//! to its own height and offsets its content **below** the inset, while the
//! background paints the full extended height — exactly what `glyph::app_bar`
//! does (see its module docs' "Top-inset consumption"), and what Flutter's
//! `AppBar` does. Content beneath this bar therefore never needs a top
//! `SafeArea` edge; a screen mounting it composes its body with
//! `safe_area(..).top(false)`.

use std::rc::Rc;

use frust::authoring::text::LineHeight;
use frust::authoring::{
    AnyView, BoxConstraints, BuildCtx, ChangeFlags, ChildPod, Color, EventCtx, EventResult,
    InputEvent, LayoutCtx, PaintCtx, PaintScene, Point, Role, SemanticsCtx, Size, View, Widget,
    any, build_child, rebuild_child, route_event, teardown_child,
};
use frust::{Align, Alignment, ButtonStyle, Theme, button, text};

/// `.topbar{height:46px}` — the content band, below the status-bar inset.
const BAR_HEIGHT: f64 = 46.0;
/// `.topbar{padding:0 16px}`.
const PAD_X: f64 = 16.0;
/// `.brand-mark{width:16px;height:16px}`.
const MARK_SIZE: f64 = 16.0;
/// `.brand-mark{gap:2px}` — so each of the four cells is 7×7.
const MARK_GAP: f64 = 2.0;
/// One `.brand-mark span`.
const MARK_CELL: f64 = (MARK_SIZE - MARK_GAP) / 2.0;
/// `.brand-mark span{border-radius:1px}`.
const MARK_RADIUS: f64 = 1.0;
/// `.brand{gap:7px}` — mark to wordmark.
const BRAND_GAP: f64 = 7.0;
/// `.brand{font-size:13px}` — the wordmark is a fixed-size logotype, not a
/// type-scale role, so it overrides the display role's size (and only that;
/// family/weight stay Space Mono 700, which *is* what the role carries).
const BRAND_SIZE: f32 = 13.0;
/// …and its line box, tightened to match. The display role carries a 36px
/// line height for a 32px face; left alone it would give the wordmark a
/// 36px-tall box inside a 46px bar, so what "centered" means would depend on
/// where the shaper puts a baseline inside that box rather than on the bar.
const BRAND_LINE_HEIGHT: f32 = 18.0;
/// `.topbar-icon{width:30px;height:30px}` — the trailing slot's box.
const ICON_SLOT: f64 = 30.0;
/// Minimum clear space between the centered title and either side zone, in
/// logical px. An original value (the source's absolutely-positioned title can
/// overlap; a real title has to be clamped instead).
const TITLE_GUTTER: f64 = 8.0;
/// `.topbar{border-bottom:1px solid var(--border)}`.
const HAIRLINE: f64 = 1.0;

/// The brand wordmark, and the accessibility name of the bar itself.
const BRAND: &str = "muxr";

/// The app's branded top bar carrying `title` in its centered slot. Attach the
/// trailing icon with [`TopBarView::action`].
///
/// ```ignore
/// muxr_topbar::<SessionsState>(host).action("\u{2699}", |state: &mut SessionsState| {
///     state.actions.open_settings();
/// })
/// ```
pub fn muxr_topbar<State: 'static>(title: impl Into<String>) -> TopBarView<State> {
    let theme = active_theme();
    let scheme = theme.scheme();
    let mut brand_style = theme.type_scale.display_small.clone();
    brand_style.size = BRAND_SIZE;
    brand_style.line_height = LineHeight::Absolute(BRAND_LINE_HEIGHT);
    brand_style.color = scheme.primary;
    let mut title_style = theme.type_scale.label_large.clone();
    title_style.color = scheme.on_surface;
    TopBarView {
        brand: any(text(BRAND).style(brand_style)),
        title: any(text(title.into()).style(title_style)),
        action: None,
        surface: scheme.surface,
        rule: scheme.outline_variant,
        mark: scheme.primary_container,
    }
}

/// The active theme, or the Glyph baseline when none is threaded (headless
/// tests, a shell with no design system installed). **Never**
/// `Theme::neutral`/`m3_baseline` — see `lib.rs`'s design-system note.
fn active_theme() -> Theme {
    frust::use_context::<Theme>().unwrap_or_else(Theme::glyph_baseline)
}

/// The y a slot `height` tall takes to sit centered in the content band —
/// **below** the status-bar inset, which the bar consumes on the content's
/// behalf (see the [module docs](self)) rather than letting it squash the
/// band.
fn content_y(top_inset: f64, height: f64) -> f64 {
    top_inset + (BAR_HEIGHT - height) / 2.0
}

/// The declarative half of the top bar. See [`muxr_topbar`].
pub struct TopBarView<State: 'static> {
    brand: AnyView<State>,
    title: AnyView<State>,
    action: Option<AnyView<State>>,
    /// `--bg-surface`: the bar's own background.
    surface: Color,
    /// `--border`: the hairline under it.
    rule: Color,
    /// `--amber`: the brand mark's four cells.
    mark: Color,
}

impl<State: 'static> TopBarView<State> {
    /// Put `glyph` (`⚙`, `⋮`, …) in the trailing icon slot, firing `on_press`
    /// when it is tapped.
    ///
    /// The slot is a small [`ButtonStyle::Ghost`] button centered in the
    /// source's 30×30 box: transparent at rest (the source's `.topbar-icon`
    /// only fills on hover, which a touch screen has no notion of), a pressed
    /// wash on tap, and — unlike a hand-composed gesture wrapper — one real
    /// `Role::Button` node in the accessibility tree, which is also how a test
    /// addresses it. The one deviation: the source sets the glyph at 15px,
    /// while `frust::button` inks its label from the theme's button-label role
    /// (12.5px) with no per-button override, so the glyph renders a shade
    /// smaller than the mock's.
    pub fn action<F: Fn(&mut State) + 'static>(
        mut self,
        glyph: impl Into<String>,
        on_press: F,
    ) -> Self {
        let on_press = Rc::new(on_press);
        self.action = Some(any(Align(
            Alignment::CENTER,
            button(glyph.into(), move |state: &mut State| on_press(state))
                .style(ButtonStyle::Ghost)
                .small(),
        )));
        self
    }
}

/// The retained half of the top bar. See [`muxr_topbar`].
pub struct TopBarWidget {
    /// `[brand, title]`, plus the action when one is attached — one flat pod
    /// list so [`route_event`]/semantics can walk it.
    children: Vec<ChildPod>,
    surface: Color,
    rule: Color,
    mark: Color,
    /// The status-bar inset resolved in `layout`, so `paint` can place the
    /// brand mark under the same offset the child pods got.
    top_inset: f64,
}

/// The index of each fixed pod in [`TopBarWidget::children`].
const BRAND_POD: usize = 0;
const TITLE_POD: usize = 1;

impl<State: 'static> View<State> for TopBarView<State> {
    type Element = TopBarWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> TopBarWidget {
        let mut children = vec![build_child(&self.brand, ctx), build_child(&self.title, ctx)];
        if let Some(action) = &self.action {
            children.push(build_child(action, ctx));
        }
        TopBarWidget {
            children,
            surface: self.surface,
            rule: self.rule,
            mark: self.mark,
            top_inset: 0.0,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut TopBarWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = rebuild_child(
            &prev.brand,
            &self.brand,
            &mut element.children[BRAND_POD],
            ctx,
        );
        flags |= rebuild_child(
            &prev.title,
            &self.title,
            &mut element.children[TITLE_POD],
            ctx,
        );
        // The action slot is fixed per screen (Sessions' ⚙ never becomes a ⋮),
        // so only the same-shape case reconciles in place; a shape change
        // rebuilds the pod list outright rather than pretending to diff it.
        match (&prev.action, &self.action) {
            (Some(prev_action), Some(action)) => {
                flags |= rebuild_child(prev_action, action, &mut element.children[2], ctx);
            }
            (None, Some(action)) => {
                element.children.push(build_child(action, ctx));
                flags |= ChangeFlags::LAYOUT;
            }
            (Some(prev_action), None) => {
                let mut pod = element.children.remove(2);
                teardown_child(prev_action, &mut pod, ctx);
                flags |= ChangeFlags::LAYOUT;
            }
            (None, None) => {}
        }
        if prev.surface != self.surface || prev.rule != self.rule || prev.mark != self.mark {
            element.surface = self.surface;
            element.rule = self.rule;
            element.mark = self.mark;
            flags |= ChangeFlags::PAINT;
        }
        flags
    }

    fn teardown(&self, element: &mut TopBarWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.brand, &mut element.children[BRAND_POD], ctx);
        teardown_child(&self.title, &mut element.children[TITLE_POD], ctx);
        if let Some(action) = &self.action {
            teardown_child(action, &mut element.children[2], ctx);
        }
    }
}

impl Widget for TopBarWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let width = if bc.max().width.is_finite() {
            bc.max().width
        } else {
            0.0
        };
        self.top_inset = ctx.window_insets().padding().top;
        let content_top = self.top_inset;
        // Every slot lays out inside the 46px content band, never against the
        // inset-extended height.
        let slot_bc = BoxConstraints::loose(Size::new(width, BAR_HEIGHT));
        let centered = |height: f64| content_y(content_top, height);

        // Leading: the painted mark, then the wordmark beside it.
        let brand_x = PAD_X + MARK_SIZE + BRAND_GAP;
        let brand_size = self.children[BRAND_POD].layout_child(ctx, &slot_bc);
        self.children[BRAND_POD].set_origin(Point::new(brand_x, centered(brand_size.height)));
        let leading_end = brand_x + brand_size.width;

        // Trailing: the icon slot, flush against the trailing edge. Its box is
        // the source's 30×30 (tight, so the `Align` inside centers the button
        // in it) rather than whatever the button happens to measure.
        let trailing_start = match self.children.get_mut(2) {
            Some(action) => {
                let size = action
                    .layout_child(ctx, &BoxConstraints::tight(Size::new(ICON_SLOT, ICON_SLOT)));
                let x = (width - PAD_X - size.width).max(leading_end);
                action.set_origin(Point::new(x, centered(size.height)));
                x
            }
            None => width - PAD_X,
        };

        // The title is centered on the **bar**, not on the gap between the
        // slots (the source's `left:50%;translateX(-50%)`), so its width is
        // clamped by whichever side crowds it first.
        let center = width / 2.0;
        let half = (center - leading_end).min(trailing_start - center).max(0.0);
        let title_max = (2.0 * half - TITLE_GUTTER).max(0.0);
        let title_size = self.children[TITLE_POD].layout_child(
            ctx,
            &BoxConstraints::loose(Size::new(title_max, BAR_HEIGHT)),
        );
        self.children[TITLE_POD].set_origin(Point::new(
            (width - title_size.width) / 2.0,
            centered(title_size.height),
        ));

        bc.constrain(Size::new(width, content_top + BAR_HEIGHT))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let origin = ctx.origin();
        let size = ctx.size();
        // The surface runs the full extended height, so it sits under the
        // status bar (see the module docs' inset note).
        scene.fill_rect(origin, size, self.surface);
        // `border-bottom`: a hairline centered on the bar's last row.
        let y = origin.y + size.height - HAIRLINE / 2.0;
        scene.stroke_line(
            Point::new(origin.x, y),
            Point::new(origin.x + size.width, y),
            HAIRLINE,
            self.rule,
        );

        // `.brand-mark`: four cells on a 2×2 grid, painted rather than nested.
        let mark_x = origin.x + PAD_X;
        let mark_y = origin.y + self.top_inset + (BAR_HEIGHT - MARK_SIZE) / 2.0;
        for (col, row) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
            scene.fill_rounded_rect(
                Point::new(
                    mark_x + col * (MARK_CELL + MARK_GAP),
                    mark_y + row * (MARK_CELL + MARK_GAP),
                ),
                Size::new(MARK_CELL, MARK_CELL),
                MARK_RADIUS,
                self.mark,
            );
        }

        for child in &mut self.children {
            child.paint_child(ctx, scene);
        }
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        // Only the action slot is interactive, but routing to every child is
        // what keeps that true as slots grow — `route_event` owns the
        // capture/focus fast paths.
        route_event(&mut self.children, ctx, event)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        ctx.push_container(
            Role::TitleBar,
            |node| node.set_label(BRAND),
            |ctx| {
                for child in &self.children {
                    child.semantics_child(ctx);
                }
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_brand_mark_cells_tile_its_box_exactly() {
        // A tripwire on the derived cell size: two cells plus the gap must be
        // the 16px mark, or the grid drifts off `.brand-mark`'s box.
        assert!((2.0 * MARK_CELL + MARK_GAP - MARK_SIZE).abs() < 1e-9);
    }

    #[test]
    fn a_slot_centers_in_the_band_below_the_status_bar_inset() {
        // The inset-consumption contract, as arithmetic: content sits *below*
        // the inset, centered in the 46px band — never centered in the
        // inset-extended height (which would push the bar's content down by
        // half the status bar).
        assert_eq!(content_y(0.0, ICON_SLOT), (BAR_HEIGHT - ICON_SLOT) / 2.0);
        assert_eq!(
            content_y(47.0, MARK_SIZE),
            47.0 + (BAR_HEIGHT - MARK_SIZE) / 2.0
        );
    }
}
