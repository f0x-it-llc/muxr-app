//! `muxr_topbar` — the app's branded top bar (`docs/redesign.html`'s
//! `.topbar`), mounted **once** by [`crate::shell`] as persistent chrome above
//! the navigator rather than by each screen.
//!
//! ```text
//! ┌──────────────────────────────────────────────┐
//! │ ▪▪  muxr        100.71.31.57              ⚙  │  46px, bg-surface
//! │ ▪▪                                           │  + hairline bottom border
//! └──────────────────────────────────────────────┘
//! ```
//!
//! Three zones: a **leading** slot (the brand — a 16px 2×2 amber grid plus the
//! `muxr` wordmark in the display face — or a `‹` back button), an **absolutely
//! centered title** slot, and one optional **trailing icon** slot. The Connect
//! screen (`/`) deliberately has **no** bar: there the whole widget is
//! [hidden](TopBarView::hidden) and lays out to zero height (see below).
//!
//! # Why a hand-rolled widget rather than `glyph::app_bar`
//!
//! `frust::glyph::app_bar` is a 52px bar whose title is *left-aligned* next to
//! its leading slot, with no brand-mark affordance. The redesign's bar is 46px
//! with a title centered on the **bar**, not on the space left over between the
//! slots — a difference no `AppBarView` builder expresses. So this is a
//! [`View`]/[`Widget`] pair over [`frust::authoring`] (the
//! `crate::ui::backdrop`/`crate::ui::sheet` precedent).
//!
//! # Hidden, never unmounted
//!
//! [`TopBarView::hidden`] is the `/` state, and it is a *flag* rather than an
//! absent child on purpose: the shell's band is `[bar, navigator]`, and a band
//! whose child count changed would shift the navigator's positional index and
//! make `Stack`/`Flex` reconciliation tear the whole live navigator down (the
//! same class of bug `crate::shell::root_view`'s stable-`Stack` invariant
//! exists for). A hidden bar lays out to [`Size::ZERO`], paints nothing, routes
//! nothing, and contributes no semantics node.
//!
//! # Crossfades (the `glyph::app_bar` behavior, reused not reimplemented)
//!
//! A route change swaps two things: the title run and the leading slot. Both
//! are staged through the framework's own
//! [`PatternSwitcher`](frust::motion::switcher) — a keyed single-slot switcher
//! that freezes the outgoing child, builds the incoming one, and stages the
//! pair under a [`TransitionPattern`]. Only the two staging *recipes* are
//! app-side, transcribed from `frust-widgets`' `glyph/appbar.rs` module docs
//! ("Title crossfade"/"Selection mode"), because the framework has no pattern
//! carrying those numbers:
//!
//! - [`TitleShift`] — the new title enters with a `±14px` x-shift + fade over
//!   `~220ms` on the `--ease-out` cubic; direction flips on a back navigation.
//! - [`LeadingSwap`] — brand ↔ `‹` crosses over vertically (`±6px` y + fade).
//!
//! `reduce_motion` is **not** handled here: `PatternSwitcher` collapses every
//! pattern to a fast (≤120ms) linear cross-fade with no geometric motion when
//! the active theme sets it, which is exactly the collapse `glyph::app_bar`
//! documents.
//!
//! **A crossfade staged on the frame the bar hides never plays.** Leaving a
//! titled route for `/` swaps both slots *and* hides the bar in one rebuild,
//! and a switcher only advances its driver during `paint` — which a hidden bar
//! skips. So the frozen outgoing child is retained, unadvanced, until the next
//! swap (arriving back at a titled route) supersedes it, which
//! `PatternSwitcher::rebuild` handles by dropping the stale pair outright. The
//! cost is one retained text pod while sitting on Connect; the alternative
//! (painting a bar with zero height) buys nothing visible. Hidden ⇒ empty title
//! ⇒ the very next visible frame is always a real swap, which is what keeps
//! that supersede guaranteed rather than merely likely.
//!
//! # Top-inset consumption (the Flutter-parity point)
//!
//! Layout adds [`WindowInsets::padding().top`](frust::authoring::WindowInsets)
//! to its own height and offsets its content **below** the inset, while the
//! background paints the full extended height — exactly what `glyph::app_bar`
//! does (see its module docs' "Top-inset consumption"), and what Flutter's
//! `AppBar` does. Content beneath this bar therefore never needs a top
//! `SafeArea` edge; a screen under it composes its body with
//! `safe_area(..).top(false)`.

use std::rc::Rc;

use frust::authoring::text::LineHeight;
use frust::authoring::{
    AnyView, BoxConstraints, BuildCtx, ChangeFlags, ChildPod, Color, EventCtx, EventResult,
    InputEvent, LayoutCtx, PaintCtx, PaintScene, Point, Role, SemanticsCtx, Size, View, Widget,
    any, build_child, rebuild_child, route_event, teardown_child,
};
use frust::motion::patterns::{PatternLayer, TransitionPattern};
use frust::motion::switcher::pattern_switcher;
use frust::{
    Align, Alignment, Axis, ButtonStyle, ChildKey, CrossAxisAlignment, Curve, FlexView, SizedBox,
    Theme, Timing, button, inflexible, text,
};

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
/// `.topbar-icon{width:30px;height:30px}` — the leading/trailing icon box.
const ICON_SLOT: f64 = 30.0;
/// Minimum clear space between the centered title and either side zone, in
/// logical px. An original value (the source's absolutely-positioned title can
/// overlap; a real title has to be clamped instead).
const TITLE_GUTTER: f64 = 8.0;
/// `.topbar{border-bottom:1px solid var(--border)}`.
const HAIRLINE: f64 = 1.0;

/// The brand wordmark, and the accessibility name of the bar itself.
const BRAND: &str = "muxr";
/// The back affordance's glyph (`docs/redesign.html`'s settings topbar, the
/// `.topbar-icon` holding a `‹`).
pub const BACK_GLYPH: &str = "\u{2039}";

/// Title-crossfade x-shift, logical px — `glyph/appbar.rs`'s `TITLE_SHIFT`
/// (`.bn-title-a{translateX(-14px)}`), transcribed.
const TITLE_SHIFT: f64 = 14.0;
/// Leading-swap y-shift, logical px — `glyph/appbar.rs`'s `SELECTION_SHIFT`
/// (`.sel-normal{translateY(-6px)}`), the vertical cross-over it uses for the
/// leading↔close swap.
const LEADING_SHIFT: f64 = 6.0;
/// Crossfade duration — `glyph/appbar.rs`'s `TITLE_DURATION` (the design
/// system's `durations.base`).
const CROSSFADE_MS: u64 = 220;
/// Crossfade easing — `glyph/appbar.rs`'s `TITLE_CURVE` (the HTML's
/// `--ease-out` cubic).
const CROSSFADE_CURVE: Curve = Curve::Cubic(0.16, 1.0, 0.3, 1.0);

/// The crossfade [`Timing`] both slots stage under. `PatternSwitcher`
/// substitutes its own ≤120ms linear cross-fade under `reduce_motion`, so this
/// is only ever the full-motion timing.
fn crossfade_timing() -> Timing {
    Timing::Duration(
        std::time::Duration::from_millis(CROSSFADE_MS),
        CROSSFADE_CURVE,
    )
}

/// The title crossfade's staging: the incoming run slides in from `±14px`
/// while fading up, the outgoing run slides the opposite way while fading out
/// — `glyph/appbar.rs`'s `title_shift`, as a [`TransitionPattern`] so
/// `PatternSwitcher` can drive it (and collapse it under `reduce_motion`).
///
/// `reverse` is the back-navigation flip: forward, the new title enters from
/// the trailing edge.
#[derive(Clone, Copy)]
struct TitleShift;

impl TransitionPattern for TitleShift {
    fn resolve(&self, p: f64, reverse: bool, _size: Size) -> (PatternLayer, PatternLayer) {
        let pc = p.clamp(0.0, 1.0);
        let sign = if reverse { -1.0 } else { 1.0 };
        let incoming = PatternLayer {
            dx: sign * (1.0 - pc) * TITLE_SHIFT,
            dy: 0.0,
            alpha: pc as f32,
            scale: 1.0,
        };
        let exiting = PatternLayer {
            dx: -sign * pc * TITLE_SHIFT,
            dy: 0.0,
            alpha: (1.0 - pc) as f32,
            scale: 1.0,
        };
        (incoming, exiting)
    }
}

/// The leading slot's brand ↔ `‹` swap: a vertical `±6px` cross-over + fade,
/// `glyph/appbar.rs`'s leading↔close treatment.
#[derive(Clone, Copy)]
struct LeadingSwap;

impl TransitionPattern for LeadingSwap {
    fn resolve(&self, p: f64, reverse: bool, _size: Size) -> (PatternLayer, PatternLayer) {
        let pc = p.clamp(0.0, 1.0);
        let sign = if reverse { -1.0 } else { 1.0 };
        let incoming = PatternLayer {
            dx: 0.0,
            dy: sign * (1.0 - pc) * LEADING_SHIFT,
            alpha: pc as f32,
            scale: 1.0,
        };
        let exiting = PatternLayer {
            dx: 0.0,
            dy: -sign * pc * LEADING_SHIFT,
            alpha: (1.0 - pc) as f32,
            scale: 1.0,
        };
        (incoming, exiting)
    }
}

/// The app's branded top bar carrying `title` in its centered slot, with the
/// brand mark leading. Swap the leading slot with [`TopBarView::back`], attach
/// the trailing icon with [`TopBarView::action`], hide it with
/// [`TopBarView::hidden`].
///
/// ```ignore
/// muxr_topbar::<AppState>("100.71.31.57")
///     .action("\u{2699}", |state: &mut AppState| state.push_settings())
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
    let title = title.into();
    TopBarView {
        leading: Rc::new(any(FlexView::new(
            Axis::Horizontal,
            vec![
                inflexible(BrandMark {
                    color: scheme.primary_container,
                }),
                inflexible(SizedBox::<State>(Some(BRAND_GAP), None)),
                inflexible(text(BRAND).style(brand_style)),
            ],
        )
        .cross_axis(CrossAxisAlignment::Center))),
        leading_key: ChildKey::new(BRAND),
        back: false,
        title: Rc::new(any(text(title.clone()).style(title_style))),
        title_key: ChildKey::new(&title),
        action: None,
        direction_reverse: false,
        hidden: false,
        surface: scheme.surface,
        rule: scheme.outline_variant,
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
    /// The leading slot's current face, staged through a [`LeadingSwap`].
    leading: Rc<AnyView<State>>,
    /// …and its identity, which is what a swap is detected by.
    leading_key: ChildKey,
    /// Whether the leading face is the `‹` (a 30×30 icon box) rather than the
    /// brand (natural width) — a layout input, not a paint one.
    back: bool,
    title: Rc<AnyView<State>>,
    title_key: ChildKey,
    action: Option<AnyView<State>>,
    /// Whether both crossfades run backwards (a pop).
    direction_reverse: bool,
    /// Whether this bar renders at all — see the [module docs](self).
    hidden: bool,
    /// `--bg-surface`: the bar's own background.
    surface: Color,
    /// `--border`: the hairline under it.
    rule: Color,
}

impl<State: 'static> TopBarView<State> {
    /// Replace the brand with a `‹` firing `on_press` — every route pushed over
    /// the stack's first titled one (see `crate::routes::BarLeading::Back`).
    pub fn back<F: Fn(&mut State) + 'static>(mut self, on_press: F) -> Self {
        let on_press = Rc::new(on_press);
        self.leading = Rc::new(any(Align(
            Alignment::CENTER,
            button(BACK_GLYPH, move |state: &mut State| on_press(state))
                .style(ButtonStyle::Ghost)
                .small(),
        )));
        self.leading_key = ChildKey::new(BACK_GLYPH);
        self.back = true;
        self
    }

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

    /// Run both crossfades backwards (the pop direction).
    pub fn reverse(mut self, reverse: bool) -> Self {
        self.direction_reverse = reverse;
        self
    }

    /// Collapse the bar to nothing — `/`'s state. See the [module docs](self)
    /// for why this is a flag rather than an unmounted child.
    pub fn hidden(mut self, hidden: bool) -> Self {
        self.hidden = hidden;
        self
    }

    /// The leading slot, wrapped in its crossfade switcher.
    fn staged_leading(&self) -> AnyView<State> {
        any(pattern_switcher(
            self.leading_key,
            LeadingSwap,
            Slot(Rc::clone(&self.leading)),
        )
        .timing(crossfade_timing())
        .reverse(self.direction_reverse))
    }

    /// The title slot, wrapped in its crossfade switcher.
    fn staged_title(&self) -> AnyView<State> {
        any(
            pattern_switcher(self.title_key, TitleShift, Slot(Rc::clone(&self.title)))
                .timing(crossfade_timing())
                .reverse(self.direction_reverse),
        )
    }
}

/// A `'static`, cheaply clonable handle to one slot's [`AnyView`].
///
/// [`pattern_switcher`] takes its child `V: View<State>` **by value** while
/// `View::build`/`rebuild` only ever hold `&self`, so the staged slots are
/// re-wrapped out of the stored view on every pass rather than moved into the
/// switcher once. A borrow can't be: `any()` needs `'static`. An `Rc` bump can.
/// Delegation is the whole implementation — `AnyView`'s own dyn-dispatch does
/// the work, so this adds a pointer hop and nothing else.
struct Slot<State: 'static>(Rc<AnyView<State>>);

impl<State: 'static> View<State> for Slot<State> {
    type Element = Box<dyn Widget>;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> Box<dyn Widget> {
        self.0.build(ctx)
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut Box<dyn Widget>,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        self.0.rebuild(&prev.0, element, ctx)
    }

    fn teardown(&self, element: &mut Box<dyn Widget>, ctx: &mut BuildCtx<'_>) {
        self.0.teardown(element, ctx);
    }
}

// ---------------------------------------------------------------------------
// The brand mark
// ---------------------------------------------------------------------------

/// `.brand-mark`: four amber cells on a 2×2 grid in a 16×16 box.
///
/// A leaf widget rather than four nested swatches *and* rather than something
/// the bar paints itself: the leading slot crossfades as one unit, and the
/// switcher can only stage a child it owns — a mark painted by
/// [`TopBarWidget::paint`] would sit outside the fade and pop.
struct BrandMark {
    color: Color,
}

/// The retained half of [`BrandMark`].
struct BrandMarkWidget {
    color: Color,
}

impl<State: 'static> View<State> for BrandMark {
    type Element = BrandMarkWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> BrandMarkWidget {
        BrandMarkWidget { color: self.color }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut BrandMarkWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        if prev.color != self.color {
            element.color = self.color;
            return ChangeFlags::PAINT;
        }
        ChangeFlags::NONE
    }
}

impl Widget for BrandMarkWidget {
    fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        bc.constrain(Size::new(MARK_SIZE, MARK_SIZE))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let origin = ctx.origin();
        for (col, row) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
            scene.fill_rounded_rect(
                Point::new(
                    origin.x + col * (MARK_CELL + MARK_GAP),
                    origin.y + row * (MARK_CELL + MARK_GAP),
                ),
                Size::new(MARK_CELL, MARK_CELL),
                MARK_RADIUS,
                self.color,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The bar itself
// ---------------------------------------------------------------------------

/// The retained half of the top bar. See [`muxr_topbar`].
pub struct TopBarWidget {
    /// `[leading, title]`, plus the action when one is attached — one flat pod
    /// list so [`route_event`]/semantics can walk it.
    children: Vec<ChildPod>,
    /// Whether the leading slot is the 30×30 `‹` box rather than the brand.
    back: bool,
    hidden: bool,
    surface: Color,
    rule: Color,
}

/// The index of each fixed pod in [`TopBarWidget::children`].
const LEADING_POD: usize = 0;
const TITLE_POD: usize = 1;
const ACTION_POD: usize = 2;

impl<State: 'static> View<State> for TopBarView<State> {
    type Element = TopBarWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> TopBarWidget {
        let mut children = vec![
            build_child(&self.staged_leading(), ctx),
            build_child(&self.staged_title(), ctx),
        ];
        if let Some(action) = &self.action {
            children.push(build_child(action, ctx));
        }
        TopBarWidget {
            children,
            back: self.back,
            hidden: self.hidden,
            surface: self.surface,
            rule: self.rule,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut TopBarWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = rebuild_child(
            &prev.staged_leading(),
            &self.staged_leading(),
            &mut element.children[LEADING_POD],
            ctx,
        );
        flags |= rebuild_child(
            &prev.staged_title(),
            &self.staged_title(),
            &mut element.children[TITLE_POD],
            ctx,
        );
        // The action slot's presence is route-driven, so unlike the two staged
        // slots it really can appear and disappear; a shape change rebuilds the
        // pod list outright rather than pretending to diff it.
        match (&prev.action, &self.action) {
            (Some(prev_action), Some(action)) => {
                flags |= rebuild_child(prev_action, action, &mut element.children[ACTION_POD], ctx);
            }
            (None, Some(action)) => {
                element.children.push(build_child(action, ctx));
                flags |= ChangeFlags::LAYOUT;
            }
            (Some(prev_action), None) => {
                let mut pod = element.children.remove(ACTION_POD);
                teardown_child(prev_action, &mut pod, ctx);
                flags |= ChangeFlags::LAYOUT;
            }
            (None, None) => {}
        }
        if prev.back != self.back || prev.hidden != self.hidden {
            element.back = self.back;
            element.hidden = self.hidden;
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if prev.surface != self.surface || prev.rule != self.rule {
            element.surface = self.surface;
            element.rule = self.rule;
            flags |= ChangeFlags::PAINT;
        }
        flags
    }

    fn teardown(&self, element: &mut TopBarWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(
            &self.staged_leading(),
            &mut element.children[LEADING_POD],
            ctx,
        );
        teardown_child(&self.staged_title(), &mut element.children[TITLE_POD], ctx);
        if let Some(action) = &self.action {
            teardown_child(action, &mut element.children[ACTION_POD], ctx);
        }
    }
}

impl Widget for TopBarWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        if self.hidden {
            // Zero-height, children unlaid — `paint`/`event`/`semantics` all
            // short-circuit on the same flag, so nothing reads their geometry.
            return bc.constrain(Size::ZERO);
        }
        let width = if bc.max().width.is_finite() {
            bc.max().width
        } else {
            0.0
        };
        let content_top = ctx.window_insets().padding().top;
        // Every slot lays out inside the 46px content band, never against the
        // inset-extended height.
        let slot_bc = BoxConstraints::loose(Size::new(width, BAR_HEIGHT));
        let icon_bc = BoxConstraints::tight(Size::new(ICON_SLOT, ICON_SLOT));
        let centered = |height: f64| content_y(content_top, height);

        // Leading: the brand row at its natural width, or the `‹`'s 30×30 box
        // flush against the leading edge (`docs/redesign.html`'s settings bar).
        let leading_bc = if self.back { icon_bc } else { slot_bc };
        let leading_size = self.children[LEADING_POD].layout_child(ctx, &leading_bc);
        self.children[LEADING_POD].set_origin(Point::new(PAD_X, centered(leading_size.height)));
        let leading_end = PAD_X + leading_size.width;

        // Trailing: the icon slot, flush against the trailing edge. Its box is
        // the source's 30×30 (tight, so the `Align` inside centers the button
        // in it) rather than whatever the button happens to measure. A back-led
        // bar with no action still reserves the box — the mock's
        // `<div style="width:30px">` spacer — so the title's clamp stays
        // symmetric and a Phase 3 `⋮` slots in without the title jumping.
        let trailing_start = match self.children.get_mut(ACTION_POD) {
            Some(action) => {
                let size = action.layout_child(ctx, &icon_bc);
                let x = (width - PAD_X - size.width).max(leading_end);
                action.set_origin(Point::new(x, centered(size.height)));
                x
            }
            None if self.back => (width - PAD_X - ICON_SLOT).max(leading_end),
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
        if self.hidden {
            return;
        }
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

        for child in &mut self.children {
            child.paint_child(ctx, scene);
        }
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        if self.hidden {
            return EventResult::Ignored;
        }
        // Only the leading/trailing slots are interactive, but routing to every
        // child is what keeps that true as slots grow — `route_event` owns the
        // capture/focus fast paths.
        route_event(&mut self.children, ctx, event)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        if self.hidden {
            // `/` has no bar at all, in the accessibility tree as on screen.
            return;
        }
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

    #[test]
    fn the_title_crossfade_matches_the_appbars_documented_staging() {
        // `glyph/appbar.rs`: the new run enters from +14px at 0 opacity and
        // rests at 0 fully opaque, while the old run leaves the opposite way.
        let (incoming, exiting) = TitleShift.resolve(0.0, false, Size::ZERO);
        assert_eq!(incoming.dx, TITLE_SHIFT);
        assert_eq!(incoming.alpha, 0.0);
        assert_eq!(exiting.dx, 0.0);
        assert_eq!(exiting.alpha, 1.0);

        let (incoming, exiting) = TitleShift.resolve(1.0, false, Size::ZERO);
        assert_eq!(incoming.dx, 0.0);
        assert_eq!(incoming.alpha, 1.0);
        assert_eq!(exiting.dx, -TITLE_SHIFT);
        assert_eq!(exiting.alpha, 0.0);

        // …and neither run ever moves vertically or scales.
        for p in [0.0, 0.5, 1.0] {
            let (incoming, exiting) = TitleShift.resolve(p, false, Size::ZERO);
            assert_eq!((incoming.dy, incoming.scale), (0.0, 1.0));
            assert_eq!((exiting.dy, exiting.scale), (0.0, 1.0));
        }
    }

    #[test]
    fn a_back_navigation_mirrors_the_title_shift() {
        let (forward, _) = TitleShift.resolve(0.0, false, Size::ZERO);
        let (back, _) = TitleShift.resolve(0.0, true, Size::ZERO);
        assert_eq!(forward.dx, TITLE_SHIFT);
        assert_eq!(back.dx, -TITLE_SHIFT, "a pop enters from the leading edge");
    }

    #[test]
    fn the_leading_swap_crosses_over_vertically() {
        // The brand ↔ `‹` swap is the appbar's leading↔close treatment: ±6px on
        // y, never on x.
        let (incoming, exiting) = LeadingSwap.resolve(0.0, false, Size::ZERO);
        assert_eq!(incoming.dy, LEADING_SHIFT);
        assert_eq!(incoming.dx, 0.0);
        assert_eq!(exiting.dy, 0.0);
        assert_eq!(exiting.alpha, 1.0);

        let (incoming, exiting) = LeadingSwap.resolve(1.0, false, Size::ZERO);
        assert_eq!(incoming.dy, 0.0);
        assert_eq!(incoming.alpha, 1.0);
        assert_eq!(exiting.dy, -LEADING_SHIFT);
        assert_eq!(exiting.alpha, 0.0);
    }
}
