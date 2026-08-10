//! The terminal **key bar**: the twelve keys a soft keyboard has no room for,
//! docked above the keyboard while — and only while — the IME is up.
//!
//! ```text
//! Ctrl  Alt  Shift  Esc │ Tab  ↑  ↓  ←  → │ PgUp  PgDn │ Del
//! ```
//!
//! Ported key for key from the Flutter incumbent (order, grouping, sequences
//! and the one-shot latch semantics all verbatim), plus a `Shift` the incumbent
//! never had; the *placement* is this app's v2 decision: the bar is
//! **IME-active-only chrome**, so a resting terminal screen stays bar-free.
//! Every byte it sends leaves through the same [`InputSeam`] the grid widget
//! emits on, so the `Ctrl` armed here transforms the next character typed on the
//! **OS keyboard** — the incumbent's semantic, and the reason the latch lives at
//! the emission choke point ([`term::input`](super::term::input)) rather than in
//! this module.
//!
//! # Three things this module is careful about
//!
//! 1. **The bar is a fixed slot that collapses, never a child that appears.**
//!    `key_bar(false, ..)` returns a zero-height view rather than nothing, and
//!    the screen mounts it unconditionally. An unkeyed child list reconciles
//!    **positionally** and preserves a focused pod only inside its stable
//!    prefix, so a bar that *appeared* as a new child would shift the grid's
//!    index, tear the grid's pod down, drop focus — and dismiss the very
//!    keyboard whose presence put the bar on screen (the S10 probe's own
//!    finding, at `benchmarks/frust_bench`'s `s10_keys`).
//! 2. **A cap claims focus — because not claiming it is what dismisses the
//!    keyboard.** Frust treats *any* pointer `Down` that requests no focus as a
//!    blur, and a blur drops `focus_active` **and** the published IME surface
//!    (`RenderRoot::event`). A cap that "politely" left focus with the grid
//!    would therefore close the very keyboard the bar is docked to on its first
//!    tap. So each cap requests focus and re-publishes the surface, and — while
//!    it holds the focus path — folds the `Key`/`Ime` events routed to it into
//!    the *same* [`KeystrokeSource`](super::term::KeystrokeSource) through
//!    [`InputSeam::absorb`]. The platform sees one unbroken input surface
//!    whichever widget was tapped last; only the caret's *lit* state follows the
//!    focus path, which is what the grid's own `focused` builder input is for.
//! 3. **Arrow caps are vectors, not font glyphs.** `↑↓←→` (U+2190–2193) are
//!    Arrows-block characters whose fallback chain differs per platform —
//!    exactly the trap `docs/ARCHITECTURE.md`'s "chrome icons are vectors" rule
//!    exists to close. `frust::icons` carries no directional arrow pair, so the
//!    four marks below are app-supplied [`IconSource`]s, the same seam
//!    [`chrome::PANE_MARK`](super::chrome::PANE_MARK) uses.
//!
//! # The row budget
//!
//! Twelve caps, their padding and three group rules share **one** row on the
//! narrowest phone this app supports (~360 lp): frust has no horizontal-scroll
//! primitive, and a bar that wrapped would eat rows of the terminal it is
//! docked to. Two things buy that fit:
//!
//! 1. **A cap's flex weight is the width it asks for** ([`cap_weight`]) — its
//!    content plus [`CAP_PAD_X`] a side, in logical px. Equal shares cannot
//!    work: every cap would then have to be as wide as `Shift`, which at any
//!    legible label size overruns the row by a third. A proportional share also
//!    degrades the right way — below the design width every cap loses the same
//!    *percentage*, so the padding thins long before a label is reached.
//! 2. **The chrome is thin**: [`CAP_GAP`] between two caps, and a group rule
//!    that *replaces* that gap rather than adding to it.
//!
//! The arithmetic at 360 lp: 8 px (the bar's two ends) plus 16 (eight inter-cap
//! gaps) plus 21 (three rules) is 45 px of chrome, leaving 315 for twelve caps
//! that ask for ~310. The few px of slack, and everything a wider phone has over
//! 360, go to the caps in proportion — i.e. straight into their padding.
//!
//! # Colours
//!
//! Resolved from [`super::pieces`]' own `Palette` — the bar is part of the
//! terminal card's chrome, so it inks from the same `GlyphInk` band tone and
//! the same Glyph scheme roles the card and its sheet use. An **armed** latch
//! cap takes the amber trio a selected pill takes; `Del` — the one destructive
//! key — takes the scheme's `error` ink and a matching edge.

use std::rc::Rc;

use frust::authoring::text::TextStyle;
use frust::authoring::{
    Action, Affine, AnyView, BoxConstraints, BuildCtx, ChangeFlags, ChildPod, Color,
    ErasedCallback, EventCtx, EventResult, InputEvent, LayoutCtx, PaintCtx, PaintScene, Point,
    PointerPhase, Role, SemanticsCtx, Size, Toggled, Vec2, View, Widget, WindowInsets, any,
    build_child, erase_callback, rebuild_child, route_event_single, teardown_child,
};
use frust::{
    Align, Alignment, Axis, CrossAxisAlignment, EdgeInsets, FlexChild, FlexView, Get, IconSource,
    Padding, SizedBox, Theme, flexible, icon, inflexible, text,
};

use super::pieces::{Palette, active_theme, palette};
use super::term::input::BarKey;
use super::term::{InputSeam, ModifierLatch};

// ---------------------------------------------------------------------------
// Geometry (transcribed once; the bar has no mock of its own, so these are
// original values chosen against `docs/terminal.html`'s card chrome)
// ---------------------------------------------------------------------------

/// One key cap's height, in logical px.
pub const CAP_HEIGHT: f64 = 34.0;
/// Vertical padding above and below the cap row.
const BAR_PAD_Y: f64 = 6.0;
/// Horizontal padding at the bar's two ends.
const BAR_PAD_X: f64 = 4.0;

/// The bar's own height, keyboard inset excluded — what a screen reserves above
/// the keyboard for it.
pub const KEY_BAR_HEIGHT: f64 = CAP_HEIGHT + 2.0 * BAR_PAD_Y;

/// Gap between two caps — thin, and *replaced* (not added to) by a group rule
/// at the three group boundaries. See the row budget in the [module docs](self).
const CAP_GAP: f64 = 2.0;
/// A cap's corner radius — Glyph's `small` shape step, which is what every
/// other small tile on this screen rounds to.
const CAP_RADIUS: f64 = 8.0;
/// The inset between a cap's edge and its label or mark, per side — the reason
/// `Ctrl` reads as a key cap rather than as text with a box around it.
///
/// Not free: twelve caps spend `24 × this` of a ~360 lp row on it, which is what
/// [`LABEL_SIZE`] was cut to pay for (the row budget in the [module docs](self)).
pub const CAP_PAD_X: f64 = 4.0;
/// A cap's label size. Small deliberately, and cut from the incumbent's 10.5 to
/// pay for the twelfth cap and for [`CAP_PAD_X`]: it is the largest half-step
/// that still leaves every cap its full padding at 360 lp with margin to spare —
/// a 4-character label (`PgUp`) is the binding one, `Shift` being wider only in
/// proportion to the share its cap asks for.
pub const LABEL_SIZE: f32 = 8.5;
/// One label character's advance at [`LABEL_SIZE`], in logical px: the Glyph
/// label face is IBM Plex Mono, a 0.6 em fixed pitch, and `label_small` adds
/// 0.475 px of tracking on top — so a label's width is exactly its character
/// count times this.
///
/// Only *relative* cap widths are derived from it ([`cap_weight`]), so a font
/// fallback that measured differently would shift a cap's padding by a fraction
/// of a px rather than break the row.
const LABEL_ADVANCE: f64 = 0.6 * LABEL_SIZE as f64 + 0.475;
/// The drawn size of an arrow mark inside its cap.
pub const ICON_SIZE: f64 = 13.0;
/// Group-separator width, height and the margin either side of it.
const SEPARATOR_W: f64 = 1.0;
const SEPARATOR_H: f64 = CAP_HEIGHT - 14.0;
const SEPARATOR_MARGIN: f64 = 3.0;
/// `:active{transform:scale(0.92)}` — the card's own press treatment, one step
/// gentler for a target this small.
const PRESS_SCALE: f64 = 0.92;
/// Width a cap falls back to when a parent offers an unbounded one (a unit
/// test, a degenerate parent). Every real frame is width-bounded.
const FALLBACK_CAP_WIDTH: f64 = 32.0;
/// `Del`'s edge alpha over the scheme's `error` — the danger-tone counterpart
/// of `pieces`' own `--amber-border` derivation, at the same strength.
const DANGER_BORDER_ALPHA: f32 = 0.35;

/// The side length of the square design box the arrow marks are authored
/// against — Material Symbols' own 24, matching every other vector in the app.
const ICON_DESIGN_BOX: f64 = 24.0;

/// The `↑` cap's mark: a solid triangular head over a shaft, authored here
/// because the vendored `frust::icons` set carries no directional arrow pair
/// (see the [module docs](self)). The head and the shaft **touch** rather than
/// overlap, so a non-zero fill cannot punch a hole between them.
pub const ARROW_UP_MARK: IconSource = IconSource {
    d: "M 12 3 L 19 11 H 5 Z M 10.5 11 h 3 v 10 h -3 Z",
    design: ICON_DESIGN_BOX,
};
/// The `↓` cap's mark — see [`ARROW_UP_MARK`].
pub const ARROW_DOWN_MARK: IconSource = IconSource {
    d: "M 12 21 L 5 13 H 19 Z M 10.5 3 h 3 v 10 h -3 Z",
    design: ICON_DESIGN_BOX,
};
/// The `←` cap's mark — see [`ARROW_UP_MARK`].
pub const ARROW_LEFT_MARK: IconSource = IconSource {
    d: "M 3 12 L 11 5 V 19 Z M 11 10.5 h 10 v 3 h -10 Z",
    design: ICON_DESIGN_BOX,
};
/// The `→` cap's mark — see [`ARROW_UP_MARK`].
pub const ARROW_RIGHT_MARK: IconSource = IconSource {
    d: "M 21 12 L 13 5 V 19 Z M 3 10.5 h 10 v 3 h -10 Z",
    design: ICON_DESIGN_BOX,
};

/// The arrow mark a key draws, or `None` for a text-labelled cap.
fn arrow_mark(key: BarKey) -> Option<IconSource> {
    match key {
        BarKey::Up => Some(ARROW_UP_MARK),
        BarKey::Down => Some(ARROW_DOWN_MARK),
        BarKey::Left => Some(ARROW_LEFT_MARK),
        BarKey::Right => Some(ARROW_RIGHT_MARK),
        _ => None,
    }
}

/// The main-axis share `key`'s cap asks for: what it has to draw, plus
/// [`CAP_PAD_X`] a side — in whole logical px, used **as the flex weight**.
///
/// A weight rather than a size because the row must fill the width it is given
/// and never overflow it (see the row budget in the [module docs](self)):
/// `FlexView` hands each cap `weight / Σweights` of the space, so on the design
/// width every cap lands on the number below, a narrower phone shrinks them all
/// by the same percentage, and a wider one pads them all by the same percentage.
fn cap_weight(key: BarKey) -> u32 {
    let content = match arrow_mark(key) {
        Some(_) => ICON_SIZE,
        None => key.label().chars().count() as f64 * LABEL_ADVANCE,
    };
    (content + 2.0 * CAP_PAD_X).round() as u32
}

// ---------------------------------------------------------------------------
// The bar
// ---------------------------------------------------------------------------

/// The key bar, or a zero-height slot when the IME is down.
///
/// `active` is the focused grid's published `ImeState.active` as the screen
/// knows it: the bar exists only while the keyboard does (see the
/// [module docs](self) for why it collapses rather than unmounting).
///
/// **Reads `seam.revision` tracked**, so a latch change — whether a tap on
/// `Ctrl` here or a soft-keyboard character *consuming* that `Ctrl` over on the
/// grid — wakes the rebuild that repaints the armed pips. Mount it from a
/// `Component::build` (the app's one rebuild scope) and nowhere else.
pub fn key_bar<State: 'static>(active: bool, seam: &InputSeam) -> AnyView<State> {
    // Tracked: see the doc note above. Read unconditionally, before the
    // inactive early-out, so the subscription exists on the frame the bar is
    // still hidden.
    let _revision = seam.revision.get();
    if !active {
        return any(SizedBox::<State>(None, Some(0.0)));
    }

    let theme = active_theme();
    let colors = palette(&theme);
    let danger = theme.scheme().error;
    let latch = seam.latch();
    // The keyboard's own inset: the bar docks directly on top of it. Absent in
    // a headless test and on desktop, where it is simply zero.
    let keyboard = frust::use_context::<WindowInsets>()
        .map(|insets| insets.view_insets.bottom)
        .unwrap_or(0.0);

    let mut children: Vec<FlexChild<State>> = Vec::with_capacity(BarKey::ORDER.len() * 2);
    for (index, key) in BarKey::ORDER.iter().copied().enumerate() {
        if key.separator_before() {
            // The rule stands **in place of** the gap rather than beside it: it
            // already carries its own margin, and twelve caps leave the row no
            // px to spend twice (the row budget in the module docs).
            children.push(inflexible(separator::<State>(&colors)));
        } else if index > 0 {
            children.push(inflexible(SizedBox::<State>(Some(CAP_GAP), None)));
        }
        children.push(flexible(
            cap_weight(key),
            key_cap(key, &theme, &colors, danger, latch, seam),
        ));
    }

    // Band outside, padding inside: the bar is full-bleed chrome docked on the
    // keyboard, so its fill must reach both screen edges (and the keyboard
    // inset below the caps must be filled too, not left as a gap).
    any(BarBackdropView {
        fill: colors.head,
        hairline: colors.card_border,
        child: any(Padding(
            EdgeInsets {
                left: BAR_PAD_X,
                top: BAR_PAD_Y,
                right: BAR_PAD_X,
                bottom: BAR_PAD_Y + keyboard,
            },
            FlexView::new(Axis::Horizontal, children).cross_axis(CrossAxisAlignment::Center),
        )),
    })
}

/// One group rule — a hairline the height of a cap's middle.
fn separator<State: 'static>(colors: &Palette) -> AnyView<State> {
    any(Padding(
        EdgeInsets::symmetric(SEPARATOR_MARGIN, 0.0),
        BarBackdropView {
            fill: colors.outline,
            hairline: Color::TRANSPARENT,
            child: any(SizedBox::<State>(Some(SEPARATOR_W), Some(SEPARATOR_H))),
        },
    ))
}

/// One cap, wired to `seam`: a latch key toggles, every other key sends its
/// sequence — both through [`InputSeam::dispatch`], the app's single byte exit.
fn key_cap<State: 'static>(
    key: BarKey,
    theme: &Theme,
    colors: &Palette,
    danger: Color,
    latch: ModifierLatch,
    seam: &InputSeam,
) -> KeyCapView<State> {
    let armed = key.armed(latch);
    let (fill, border, ink) = if armed {
        (colors.amber_faint, colors.amber_border, colors.amber)
    } else if key.is_danger() {
        (
            colors.raised,
            danger.with_alpha(DANGER_BORDER_ALPHA),
            danger,
        )
    } else {
        (colors.raised, colors.outline, colors.fg_muted)
    };
    let mut label = theme.type_scale.label_small.clone();
    label.size = LABEL_SIZE;
    label.color = ink;

    let pressed_seam = seam.clone();
    KeyCapView {
        key,
        label,
        ink,
        fill,
        border,
        armed,
        seam: seam.clone(),
        // Through the seam, never `source.on_bar_key` + `dispatch` by hand:
        // `press_bar_key` is where the session's read-only gate lives, so a
        // cap mounted over a read-only session derives nothing rather than
        // relying on the screen having remembered not to mount it (review's
        // read-only minor).
        on_press: Rc::new(move |_state: &mut State| {
            pressed_seam.press_bar_key(key);
        }),
    }
}

// ---------------------------------------------------------------------------
// `BarBackdropView` — a filled box with an optional top hairline
// ---------------------------------------------------------------------------

/// A filled rect behind one child, with an optional hairline along its **top**
/// edge — the bar's own band, and (fill only) the group separators.
///
/// Hand-rolled rather than reusing [`super::chrome`]'s `term_box`: that one
/// strokes a *full* border and rounds its corners, while the bar is a
/// full-bleed band whose only edge is the rule between it and the terminal
/// above it.
struct BarBackdropView<State: 'static> {
    fill: Color,
    hairline: Color,
    child: AnyView<State>,
}

/// The retained half of a [`BarBackdropView`].
struct BarBackdropWidget {
    fill: Color,
    hairline: Color,
    child: ChildPod,
}

impl<State: 'static> View<State> for BarBackdropView<State> {
    type Element = BarBackdropWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> BarBackdropWidget {
        BarBackdropWidget {
            fill: self.fill,
            hairline: self.hairline,
            child: build_child(&self.child, ctx),
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut BarBackdropWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = rebuild_child(&prev.child, &self.child, &mut element.child, ctx);
        if prev.fill != self.fill || prev.hairline != self.hairline {
            element.fill = self.fill;
            element.hairline = self.hairline;
            flags |= ChangeFlags::PAINT;
        }
        flags
    }

    fn teardown(&self, element: &mut BarBackdropWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.child, &mut element.child, ctx);
    }
}

impl Widget for BarBackdropWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let size = self.child.layout_child(ctx, bc);
        self.child.set_origin(Point::ZERO);
        bc.constrain(size)
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let origin = ctx.origin();
        let size = ctx.size();
        if self.fill != Color::TRANSPARENT {
            scene.fill_rect(origin, size, self.fill);
        }
        if self.hairline != Color::TRANSPARENT {
            scene.fill_rect(origin, Size::new(size.width, SEPARATOR_W), self.hairline);
        }
        self.child.paint_child(ctx, scene);
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        // A decorative container still MUST route, or every cap under it is
        // silently unreachable (the `term_box` precedent).
        route_event_single(&mut self.child, ctx, event)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        self.child.semantics_child(ctx);
    }
}

// ---------------------------------------------------------------------------
// `KeyCapView` — one key
// ---------------------------------------------------------------------------

/// One key cap: a filled, bordered, labelled press target that claims focus and
/// co-drives the grid's keystroke source (see the [module docs](self)).
struct KeyCapView<State: 'static> {
    key: BarKey,
    label: TextStyle,
    ink: Color,
    fill: Color,
    border: Color,
    armed: bool,
    seam: InputSeam,
    on_press: Rc<dyn Fn(&mut State)>,
}

impl<State: 'static> KeyCapView<State> {
    /// The cap's content: a vector arrow, or its printed label — centred inside
    /// a [`CAP_PAD_X`] inset, so nothing a cap draws reaches its own edge.
    ///
    /// The inset is **inside** the press target and inside the fill, not around
    /// them: a cap's whole box stays tappable and painted, and only what it
    /// draws is held off the rounding. It is the *content box* the inset
    /// defines — a centred label sits in the same place either way, and what
    /// makes the gap visible is the cap being [`cap_weight`] wide. What this
    /// adds is the guarantee: a label is laid out in a box that already has the
    /// padding taken out of it, so one too wide for its cap can never paint over
    /// the border.
    fn content(&self) -> AnyView<State> {
        let inner: AnyView<State> = match arrow_mark(self.key) {
            Some(mark) => any(icon(mark).size(ICON_SIZE).color(self.ink)),
            None => any(text(self.key.label()).style(self.label.clone())),
        };
        any(Padding(
            EdgeInsets::symmetric(CAP_PAD_X, 0.0),
            Align(Alignment::CENTER, inner),
        ))
    }
}

/// The retained half of a [`KeyCapView`].
struct KeyCapWidget {
    /// The accessible name — a word, never the arrow glyph the cap draws.
    name: &'static str,
    /// Whether this cap is a latch key, and whether it is armed. Announced as
    /// a toggle; `None` for every non-latch key.
    toggled: Option<bool>,
    fill: Color,
    border: Color,
    content: ChildPod,
    /// The shared keystroke source + feed: what a tap emits into, and what the
    /// `Key`/`Ime` events routed here while this cap holds focus are folded
    /// into (see the module docs' point 2).
    seam: InputSeam,
    /// The pressed *visual*; follows the pointer in and out while captured.
    pressed: bool,
    /// Armed by a `Down` (alongside `capture_pointer`), cleared on `Up`/
    /// `Cancel` — gates `Move`/`Up` so a hover never latches or fires.
    captured: bool,
    on_press: ErasedCallback,
}

impl<State: 'static> View<State> for KeyCapView<State> {
    type Element = KeyCapWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> KeyCapWidget {
        KeyCapWidget {
            name: self.key.name(),
            toggled: self.key.is_latch().then_some(self.armed),
            fill: self.fill,
            border: self.border,
            content: build_child(&self.content(), ctx),
            seam: self.seam.clone(),
            pressed: false,
            captured: false,
            on_press: erase_callback(&self.on_press),
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut KeyCapWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        // Closures aren't comparable; reinstall the adapter every time. The
        // seam is `Rc`-backed and just as un-comparable, so it follows.
        element.on_press = erase_callback(&self.on_press);
        element.seam = self.seam.clone();
        let mut flags = rebuild_child(&prev.content(), &self.content(), &mut element.content, ctx);
        if prev.fill != self.fill || prev.border != self.border || prev.armed != self.armed {
            element.fill = self.fill;
            element.border = self.border;
            element.toggled = self.key.is_latch().then_some(self.armed);
            // Semantics are re-collected off the paint pass, so an armed-state
            // change republishes through PAINT.
            flags |= ChangeFlags::PAINT;
        }
        flags
    }

    fn teardown(&self, element: &mut KeyCapWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.content(), &mut element.content, ctx);
    }
}

impl Widget for KeyCapWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let width = if bc.max().width.is_finite() {
            bc.max().width
        } else {
            FALLBACK_CAP_WIDTH
        };
        let size = bc.constrain(Size::new(width, CAP_HEIGHT));
        self.content.layout_child(ctx, &BoxConstraints::tight(size));
        self.content.set_origin(Point::ZERO);
        size
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let origin = ctx.origin();
        let size = ctx.size();
        // Scale about the cap's own centre while pressed. Hit testing stays at
        // full size, exactly as the card's own `:active` treatment does.
        if self.pressed {
            let centre = Vec2::new(origin.x + size.width / 2.0, origin.y + size.height / 2.0);
            scene.push_transform(
                Affine::translate(centre) * Affine::scale(PRESS_SCALE) * Affine::translate(-centre),
            );
        }
        let radius = CAP_RADIUS.min(size.width / 2.0).min(size.height / 2.0);
        scene.fill_rounded_rect(origin, size, radius, self.fill);
        if self.border != Color::TRANSPARENT {
            // A hairline just inside the bounds: the outer rounded rect in the
            // border tone, then the fill inset over it (`PaintScene` has no
            // stroked-rounded-rect primitive).
            scene.fill_rounded_rect(origin, size, radius, self.border);
            let inset = Point::new(origin.x + SEPARATOR_W, origin.y + SEPARATOR_W);
            let inner = Size::new(
                (size.width - 2.0 * SEPARATOR_W).max(0.0),
                (size.height - 2.0 * SEPARATOR_W).max(0.0),
            );
            scene.fill_rounded_rect(inset, inner, (radius - SEPARATOR_W).max(0.0), self.fill);
        }
        self.content.paint_child(ctx, scene);
        if self.pressed {
            scene.pop_transform();
        }
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        let InputEvent::Pointer(pointer) = event else {
            // Focus-routed `Key`/`Ime` while this cap holds the focus path: fold
            // it into the shared source, exactly as the grid would have (see the
            // module docs' point 2). Nothing about the published surface changes
            // — same source, same content type — so the platform never notices
            // which widget it is talking to.
            if !ctx.has_focus() {
                return EventResult::Ignored;
            }
            if self.seam.absorb(event).is_none() {
                return EventResult::Ignored;
            }
            ctx.publish_ime_state(self.seam.ime_state(None));
            ctx.request_redraw();
            return EventResult::Handled;
        };
        match pointer.phase {
            PointerPhase::Down => {
                // Claim focus. NOT politeness — a `Down` that claims nothing is
                // a blur, and a blur drops the whole IME surface and closes the
                // keyboard this bar is docked to.
                //
                // Recorded on the seam first: this is a **hand-off**, not a
                // blur of the session, and the claim is what stops the grid
                // from scrubbing its buffers when it notices it lost the focus
                // path on the next frame (`TerminalGridWidget::note_focus_edge`).
                self.seam.claim_focus();
                ctx.request_focus();
                ctx.publish_ime_state(self.seam.ime_state(None));
                self.pressed = true;
                self.captured = true;
                ctx.capture_pointer();
                ctx.request_redraw();
                EventResult::Handled
            }
            PointerPhase::Move => {
                if !self.captured {
                    return EventResult::Ignored;
                }
                self.pressed = inside(pointer.position, ctx.size());
                ctx.request_redraw();
                EventResult::Handled
            }
            PointerPhase::Up => {
                if !self.captured {
                    return EventResult::Ignored;
                }
                if inside(pointer.position, ctx.size()) {
                    (self.on_press)(ctx);
                }
                self.pressed = false;
                self.captured = false;
                ctx.request_redraw();
                EventResult::Handled
            }
            // Flags only — a `Cancel` arm never touches state
            // (`docs/CODE_STANDARDS.md`'s Interaction Semantics). No key
            // repeat and no long-press: a bar key fires once per tap.
            PointerPhase::Cancel => {
                self.pressed = false;
                self.captured = false;
                ctx.request_redraw();
                EventResult::Handled
            }
        }
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        ctx.push_node(Role::Button, |node| {
            node.set_label(self.name);
            node.add_action(Action::Click);
            if let Some(armed) = self.toggled {
                node.set_toggled(if armed { Toggled::True } else { Toggled::False });
            }
        });
    }
}

/// Whether a widget-local `position` is inside a `size`-sized box.
fn inside(position: Point, size: Size) -> bool {
    position.x >= 0.0 && position.y >= 0.0 && position.x < size.width && position.y < size.height
}
