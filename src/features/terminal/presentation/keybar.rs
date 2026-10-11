//! The terminal **key bar**: the keys a soft keyboard has no room for, in two
//! tiers. [`key_bar`] is the Compact tier — twelve keys docked above the
//! keyboard while, and only while, the IME is up:
//!
//! ```text
//! Ctrl  Alt  Shift  Esc │ Tab  ↑  ↓  ←  → │ PgUp  PgDn │ Del
//! ```
//!
//! [`tablet_key_bar`] is the Tablet tier (`docs/tablet.html` frame 03),
//! persistent behind the menu's "Show key bar" since a hardware keyboard
//! raises no inset, with `Fn` revealing an F1–F12 row:
//!
//! ```text
//! Ctrl  Alt  Shift  Esc │ Tab  Home  End  PgUp  PgDn  Del │ Fn        ↑
//!                                                                  ←  ↓  →
//! ```
//!
//! Ported from the Flutter incumbent (order/sequences/latch semantics), plus
//! a `Shift` it never had. Every byte leaves through the same [`InputSeam`]
//! the grid emits on, so a latch armed here transforms the next OS-keyboard
//! character — the reason the latch lives at that emission choke point.
//!
//! Contracts:
//! 1. **Fixed slot, never appears/disappears**: a hidden tier returns a
//!    zero-height view (the Tablet's keeps only the keyboard's inset), mounted
//!    unconditionally — an unkeyed child list
//!    reconciles positionally, so an appearing bar would shift the grid's
//!    index and drop focus, dismissing the keyboard it exists to serve.
//! 2. **Every cap claims focus on every tap**: an unclaimed pointer `Down` is
//!    a blur, which drops the IME surface and closes the keyboard. So each
//!    cap requests focus, re-publishes the surface, and folds routed
//!    `Key`/`Ime` events into the shared source via [`InputSeam::absorb`].
//! 3. **Arrow caps are vectors** (`↑↓←→`), not font glyphs: no per-platform
//!    fallback chain. `frust::icons::{ARROW_UPWARD, ARROW_DOWNWARD, ARROW_BACK,
//!    ARROW_FORWARD}` now carry the pair this module used to author locally —
//!    the catalog's own outlined-arrowhead-on-a-shaft art, not a
//!    transcription of the solid triangular marks this bar drew before (Ed
//!    device-gates the swap).
//!
//! Row budget ([`cap_weight`], [`CAP_GAP`]) and colour rules (armed latch =
//! selected-pill amber; `Del` = scheme `error`) are stated where enforced.

use std::rc::Rc;

use frust::authoring::text::TextStyle;
use frust::authoring::{
    Action, Affine, AnyView, BoxConstraints, BuildCtx, ChangeFlags, ChildPod, Color,
    ErasedCallback, EventCtx, EventResult, InputEvent, LayoutCtx, PaintCtx, PaintScene, Point,
    PointerPhase, Role, SemanticsCtx, Size, Toggled, Vec2, View, Widget, WindowInsets, any,
    build_child, erase_callback, rebuild_child, route_event_single, teardown_child,
};
use frust::{
    Align, Alignment, Axis, CrossAxisAlignment, EdgeInsets, Either, FlexChild, FlexView, Get,
    GetUntracked, IconSource, Padding, RwSignal, Set, SizedBox, Theme, column, flexible, icon,
    icons, inflexible, row, text,
};

use crate::responsive::WidthBand;
use crate::ui::palette::{Palette, active_theme};
use crate::ui::spacing;

use super::term::input::BarKey;
use super::term::{InputSeam, ModifierLatch};
use super::widgets::card::CardInk;

// Geometry: the bar has no mock of its own, so these are original values
// chosen against `docs/terminal.html`'s card chrome.

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
/// at the three group boundaries.
const CAP_GAP: f64 = 2.0;
/// A cap's corner radius — Glyph's `small` shape step, which is what every
/// other small tile on this screen rounds to.
const CAP_RADIUS: f64 = 8.0;
/// The inset between a cap's edge and its label or mark, per side — the reason
/// `Ctrl` reads as a key cap rather than as text with a box around it. Not free:
/// twelve caps spend `24 ×` this of a ~360 lp row, which [`LABEL_SIZE`] pays for.
pub const CAP_PAD_X: f64 = 4.0;
/// A cap's label size — cut from the incumbent's 10.5 to pay for the twelfth cap
/// and for [`CAP_PAD_X`]: the largest half-step that still leaves every cap its
/// full padding at 360 lp, with `PgUp` the binding label.
pub const LABEL_SIZE: f32 = 8.5;
/// One label character's advance at [`LABEL_SIZE`], px: IBM Plex Mono's 0.6em
/// fixed pitch plus `label_small`'s 0.475px tracking. Only relative widths
/// derive from it, so a differently-measured fallback shifts padding by a fraction.
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
/// `Del`'s edge alpha over the scheme's `error` — the danger-tone counterpart of
/// the shared palette's own `--amber-border` derivation, at the same strength.
const DANGER_BORDER_ALPHA: f32 = 0.35;

// Tablet tier geometry, from `docs/tablet.html`'s `.key`, `.ksep`, `.arrows`
// and `.frow .key`.

/// A Tablet cap's height.
pub const TABLET_CAP_HEIGHT: f64 = 44.0;
/// A Tablet cap's minimum width — also one arrow column's width.
const TABLET_CAP_MIN_W: f64 = 36.0;
/// The inset between a Tablet cap's edge and its label, per side.
const TABLET_CAP_PAD_X: f64 = 7.0;
/// A Tablet cap's label size.
pub const TABLET_LABEL_SIZE: f32 = 10.5;
/// One label character's advance at [`TABLET_LABEL_SIZE`] (see [`LABEL_ADVANCE`]).
const TABLET_LABEL_ADVANCE: f64 = 0.6 * TABLET_LABEL_SIZE as f64 + 0.475;
/// A Tablet arrow mark's drawn size.
const TABLET_ICON_SIZE: f64 = 14.0;
/// A Tablet group rule's height.
const TABLET_SEPARATOR_H: f64 = 24.0;
/// The gap between the arrow cluster's cells, both axes.
const ARROW_GAP: f64 = spacing::MICRO;
/// One arrow cell's height: two rows and their gap span one Tablet cap.
pub const ARROW_CAP_HEIGHT: f64 = (TABLET_CAP_HEIGHT - ARROW_GAP) / 2.0;
/// An F-key cap's height.
pub const FN_CAP_HEIGHT: f64 = 36.0;

/// One tier's cap metrics: height, label inset, arrow size and label size.
#[derive(Clone, Copy, PartialEq)]
struct CapShape {
    height: f64,
    pad_x: f64,
    icon: f64,
    label: f32,
}

const COMPACT_CAP: CapShape = CapShape {
    height: CAP_HEIGHT,
    pad_x: CAP_PAD_X,
    icon: ICON_SIZE,
    label: LABEL_SIZE,
};
const TABLET_CAP: CapShape = CapShape {
    height: TABLET_CAP_HEIGHT,
    pad_x: TABLET_CAP_PAD_X,
    icon: TABLET_ICON_SIZE,
    label: TABLET_LABEL_SIZE,
};
const ARROW_CAP: CapShape = CapShape {
    height: ARROW_CAP_HEIGHT,
    pad_x: 0.0,
    ..TABLET_CAP
};
const FN_CAP: CapShape = CapShape {
    height: FN_CAP_HEIGHT,
    ..TABLET_CAP
};

/// The arrow mark a key draws, or `None` for a text-labelled cap. The catalog's
/// own [`frust::icons`] arrow pair — see the [module docs](self)'s contract 3.
fn arrow_mark(key: BarKey) -> Option<IconSource> {
    match key {
        BarKey::Up => Some(icons::ARROW_UPWARD),
        BarKey::Down => Some(icons::ARROW_DOWNWARD),
        BarKey::Left => Some(icons::ARROW_BACK),
        BarKey::Right => Some(icons::ARROW_FORWARD),
        _ => None,
    }
}

/// The main-axis share `key`'s cap asks for: what it has to draw, plus
/// [`CAP_PAD_X`] a side — in whole logical px, used **as the flex weight**. A
/// weight rather than a size because the row must fill the width it is given and
/// never overflow it: `FlexView` hands each cap `weight / Σweights`, so a narrower
/// phone shrinks every cap by the same percentage.
fn cap_weight(key: BarKey) -> u32 {
    let content = match arrow_mark(key) {
        Some(_) => ICON_SIZE,
        None => key.label().chars().count() as f64 * LABEL_ADVANCE,
    };
    (content + 2.0 * CAP_PAD_X).round() as u32
}

/// The key bar, or a zero-height slot when the IME is down. `active` is the
/// focused grid's published `ImeState.active` as the screen knows it.
///
/// **Reads `seam.revision` tracked**, so a latch change wakes the rebuild
/// that repaints the armed pips. Mount from a `Component::build`.
pub fn key_bar<State: 'static>(active: bool, seam: &InputSeam) -> AnyView<State> {
    // Read unconditionally, before the inactive early-out, so the
    // subscription exists on the frame the bar is still hidden.
    let _revision = seam.revision.get();
    if !active {
        return any(SizedBox::<State>(None, Some(0.0)));
    }

    let kit = Kit::new(seam);
    let mut children: Vec<FlexChild<State>> = Vec::with_capacity(BarKey::ORDER.len() * 2);
    for (index, key) in BarKey::ORDER.iter().copied().enumerate() {
        if key.separator_before() {
            // Replaces the gap rather than adding to it — its own margin,
            // and the row has no px to spend twice.
            children.push(inflexible(separator::<State>(
                &kit.colors,
                SEPARATOR_H,
                SEPARATOR_MARGIN,
            )));
        } else if index > 0 {
            children.push(inflexible(SizedBox::<State>(Some(CAP_GAP), None)));
        }
        children.push(flexible(cap_weight(key), kit.cap(key, COMPACT_CAP)));
    }

    let row = FlexView::new(Axis::Horizontal, children).cross_axis(CrossAxisAlignment::Center);
    any(kit.band(BAR_PAD_X, BAR_PAD_Y, row))
}

/// The keyboard's own inset a bar docks on; zero in a headless test/desktop.
fn keyboard_inset() -> f64 {
    frust::use_context::<WindowInsets>()
        .map(|insets| insets.view_insets.bottom)
        .unwrap_or(0.0)
}

/// The Tablet tier ([module docs](self)): mounted while `shown` — the screen's
/// "Show key bar" with the session not read-only — docked above the keyboard
/// when one is up and at the screen's foot otherwise. The `Collapsed` band drops
/// `Home`/`End` so the row fits; `fn_row` is the `Fn` cap's own toggle.
///
/// Hidden, it keeps only the keyboard's inset, so the grid still clears a soft
/// keyboard. Read-only is re-checked on the seam: this tier never waits for an
/// IME, whose absence is how the Compact tier stays off a read-only session.
/// Tracks `seam.revision` and `fn_row`, like [`key_bar`].
pub fn tablet_key_bar<State: 'static>(
    shown: bool,
    band: WidthBand,
    fn_row: RwSignal<bool>,
    seam: &InputSeam,
) -> AnyView<State> {
    let _revision = seam.revision.get();
    let fn_open = fn_row.get();
    if !shown || seam.read_only() {
        return any(SizedBox::<State>(None, Some(keyboard_inset())));
    }

    let kit = Kit::new(seam);
    let mut row: Vec<FlexChild<State>> = Vec::with_capacity(BarKey::TABLET_ORDER.len() * 2 + 2);
    let keys = BarKey::TABLET_ORDER
        .iter()
        .copied()
        .filter(|key| band == WidthBand::Full || !matches!(key, BarKey::Home | BarKey::End));
    for key in keys {
        if key.tablet_separator_before() {
            row.push(inflexible(separator::<State>(
                &kit.colors,
                TABLET_SEPARATOR_H,
                spacing::SM,
            )));
        } else if !row.is_empty() {
            row.push(inflexible(SizedBox::<State>(Some(spacing::TIGHT), None)));
        }
        let cap = if key == BarKey::Fn {
            let toggle = Rc::new(move |_state: &mut State| fn_row.set(!fn_row.get_untracked()));
            kit.toggle_cap(key, TABLET_CAP, fn_open, toggle)
        } else {
            kit.cap(key, TABLET_CAP)
        };
        let width = tablet_cap_width(key);
        row.push(inflexible(SizedBox::<State>(Some(width), None).child(cap)));
    }
    row.push(flexible(1, SizedBox::<State>(None, None)));
    row.push(inflexible(arrow_cluster::<State>(&kit)));

    let mut rows = vec![inflexible(
        FlexView::new(Axis::Horizontal, row).cross_axis(CrossAxisAlignment::Center),
    )];
    if fn_open {
        let mut keys: Vec<FlexChild<State>> = Vec::with_capacity(BarKey::FN_ROW.len() * 2);
        for key in BarKey::FN_ROW {
            if !keys.is_empty() {
                keys.push(inflexible(SizedBox::<State>(Some(spacing::TIGHT), None)));
            }
            keys.push(flexible(1, kit.cap(key, FN_CAP)));
        }
        rows.push(inflexible(SizedBox::<State>(None, Some(spacing::XS))));
        rows.push(inflexible(FlexView::new(Axis::Horizontal, keys)));
    }
    let column = FlexView::new(Axis::Vertical, rows).cross_axis(CrossAxisAlignment::Stretch);
    any(kit.band(spacing::BASE, spacing::SM, column))
}

/// A Tablet cap's width: its label plus [`TABLET_CAP_PAD_X`] a side, never
/// under [`TABLET_CAP_MIN_W`].
fn tablet_cap_width(key: BarKey) -> f64 {
    let label = key.label().chars().count() as f64 * TABLET_LABEL_ADVANCE;
    (label + 2.0 * TABLET_CAP_PAD_X).max(TABLET_CAP_MIN_W)
}

/// The inverted-T: `↑` alone over `← ↓ →`, three [`TABLET_CAP_MIN_W`]
/// columns whose two rows together span one Tablet cap.
fn arrow_cluster<State: 'static>(kit: &Kit<'_>) -> impl View<State> {
    let cell = |key: Option<BarKey>| {
        let slot = SizedBox::<State>(Some(TABLET_CAP_MIN_W), Some(ARROW_CAP_HEIGHT));
        inflexible(match key {
            Some(key) => slot.child(kit.cap(key, ARROW_CAP)),
            None => slot,
        })
    };
    let gap = || inflexible(SizedBox::<State>(Some(ARROW_GAP), Some(ARROW_GAP)));
    let line = |keys: [Option<BarKey>; 3]| {
        let [a, b, c] = keys.map(cell);
        inflexible(row().push(a).push(gap()).push(b).push(gap()).push(c))
    };
    column()
        .push(line([None, Some(BarKey::Up), None]))
        .push(gap())
        .push(line([
            Some(BarKey::Left),
            Some(BarKey::Down),
            Some(BarKey::Right),
        ]))
}

/// One group rule — a `height`-tall hairline with `margin` either side.
fn separator<State: 'static>(colors: &Palette, height: f64, margin: f64) -> impl View<State> {
    Padding(
        EdgeInsets::symmetric(margin, 0.0),
        BarBackdropView {
            fill: colors.outline,
            hairline: Color::TRANSPARENT,
            child: any(SizedBox::<State>(Some(SEPARATOR_W), Some(height))), // erasure: keep ChildPod-fed
        },
    )
}

/// What every cap of one bar build shares: the theme's tones, the latch the
/// pips are drawn from, and the seam presses go through.
struct Kit<'a> {
    theme: Theme,
    colors: Palette,
    ink: CardInk,
    danger: Color,
    latch: ModifierLatch,
    seam: &'a InputSeam,
}

impl<'a> Kit<'a> {
    fn new(seam: &'a InputSeam) -> Self {
        let theme = active_theme();
        Self {
            colors: Palette::from_theme(&theme),
            ink: CardInk::from_theme(&theme),
            danger: theme.scheme().error,
            latch: seam.latch(),
            theme,
            seam,
        }
    }

    /// The bar's band around `content`: fill and top hairline outside, padding
    /// inside, so full-bleed chrome reaches both screen edges, keyboard inset
    /// included.
    fn band<State: 'static>(
        &self,
        pad_x: f64,
        pad_y: f64,
        content: impl View<State>,
    ) -> impl View<State> {
        BarBackdropView {
            fill: self.ink.head,
            hairline: self.ink.card_border,
            child: any(Padding(
                EdgeInsets {
                    left: pad_x,
                    top: pad_y,
                    right: pad_x,
                    bottom: pad_y + keyboard_inset(),
                },
                content,
            )),
        }
    }

    /// One cap, wired to the seam: a latch key toggles, every other key sends its
    /// sequence — both through [`InputSeam::dispatch`], the app's single byte exit.
    fn cap<State: 'static>(&self, key: BarKey, shape: CapShape) -> KeyCapView<State> {
        let pressed_seam = self.seam.clone();
        // Via `press_bar_key`, never hand-rolled: it's where the read-only gate lives.
        let on_press = Rc::new(move |_state: &mut State| {
            pressed_seam.press_bar_key(key);
        });
        self.view(key, shape, key.armed(self.latch), key.is_latch(), on_press)
    }

    /// A cap that toggles screen state rather than the latch — `Fn` — drawn and
    /// announced as armed while `on`.
    fn toggle_cap<State: 'static>(
        &self,
        key: BarKey,
        shape: CapShape,
        on: bool,
        on_press: Rc<dyn Fn(&mut State)>,
    ) -> KeyCapView<State> {
        self.view(key, shape, on, true, on_press)
    }

    fn view<State: 'static>(
        &self,
        key: BarKey,
        shape: CapShape,
        armed: bool,
        toggle: bool,
        on_press: Rc<dyn Fn(&mut State)>,
    ) -> KeyCapView<State> {
        let colors = &self.colors;
        let (fill, border, ink) = if armed {
            (colors.amber_faint, colors.amber_border, colors.amber)
        } else if key.is_danger() {
            (
                colors.raised,
                self.danger.with_alpha(DANGER_BORDER_ALPHA),
                self.danger,
            )
        } else {
            (colors.raised, colors.outline, colors.fg_muted)
        };
        let mut label = self.theme.type_scale.label_small.clone();
        label.size = shape.label;
        label.color = ink;
        KeyCapView {
            key,
            shape,
            label,
            ink,
            fill,
            border,
            armed,
            toggle,
            seam: self.seam.clone(),
            on_press,
        }
    }
}

/// A filled rect behind one child, with an optional top hairline — the
/// bar's band and (fill only) the group separators. Hand-rolled rather than
/// `frust::container`, which strokes a full rounded border rather than one
/// square top edge.
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
        // silently unreachable.
        route_event_single(&mut self.child, ctx, event)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        self.child.semantics_child(ctx);
    }
}

/// One key cap: a filled, bordered, labelled press target that claims focus and
/// co-drives the grid's keystroke source (see the [module docs](self)).
struct KeyCapView<State: 'static> {
    key: BarKey,
    shape: CapShape,
    label: TextStyle,
    ink: Color,
    fill: Color,
    border: Color,
    armed: bool,
    /// Whether the cap announces itself as a toggle (a latch key, or `Fn`).
    toggle: bool,
    seam: InputSeam,
    on_press: Rc<dyn Fn(&mut State)>,
}

impl<State: 'static> KeyCapView<State> {
    /// The cap's content: a vector arrow, or its printed label — centred
    /// inside its shape's inset ([`CAP_PAD_X`] on Compact) that stays inside
    /// the press target and fill, so a wide label can never paint over the border.
    fn content(&self) -> AnyView<State> // erasure: keep ChildPod-fed
    {
        let inner = match arrow_mark(self.key) {
            Some(mark) => Either::Left(icon(mark).size(self.shape.icon).color(self.ink)),
            None => Either::Right(text(self.key.label()).style(self.label.clone())),
        };
        any(Padding(
            EdgeInsets::symmetric(self.shape.pad_x, 0.0),
            Align(Alignment::CENTER, inner),
        ))
    }
}

/// The retained half of a [`KeyCapView`].
struct KeyCapWidget {
    /// The accessible name — a word, never the arrow glyph the cap draws.
    name: &'static str,
    /// Whether this cap is a toggle (a latch key, or `Fn`), and whether it is
    /// on. Announced as a toggle; `None` for every other key.
    toggled: Option<bool>,
    /// The cap's own height — its tier's [`CapShape`].
    height: f64,
    fill: Color,
    border: Color,
    content: ChildPod,
    /// The shared keystroke source + feed: what a tap emits into, and what the
    /// `Key`/`Ime` events routed here while this cap holds focus fold into.
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
            toggled: self.toggle.then_some(self.armed),
            height: self.shape.height,
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
        // Closures and the `Rc`-backed seam aren't comparable; reinstall both every time.
        element.on_press = erase_callback(&self.on_press);
        element.seam = self.seam.clone();
        let mut flags = rebuild_child(&prev.content(), &self.content(), &mut element.content, ctx);
        if prev.shape != self.shape {
            element.height = self.shape.height;
            flags |= ChangeFlags::LAYOUT;
        }
        // A band change shifts the Tablet row, so a slot can take another key.
        if prev.key != self.key
            || prev.fill != self.fill
            || prev.border != self.border
            || prev.armed != self.armed
        {
            element.name = self.key.name();
            element.fill = self.fill;
            element.border = self.border;
            element.toggled = self.toggle.then_some(self.armed);
            // Semantics are re-collected off paint, so this republishes through it.
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
        let size = bc.constrain(Size::new(width, self.height));
        self.content.layout_child(ctx, &BoxConstraints::tight(size));
        self.content.set_origin(Point::ZERO);
        size
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let origin = ctx.origin();
        let size = ctx.size();
        // Scale about the cap's centre while pressed; hit testing stays full size.
        if self.pressed {
            let centre = Vec2::new(origin.x + size.width / 2.0, origin.y + size.height / 2.0);
            scene.push_transform(
                Affine::translate(centre) * Affine::scale(PRESS_SCALE) * Affine::translate(-centre),
            );
        }
        let radius = CAP_RADIUS.min(size.width / 2.0).min(size.height / 2.0);
        scene.fill_rounded_rect(origin, size, radius, self.fill);
        if self.border != Color::TRANSPARENT {
            // Border tone full-size, fill inset over it — `PaintScene` has no
            // stroked-rounded-rect primitive.
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
            // Focus-routed `Key`/`Ime`: fold into the shared source, exactly
            // as the grid would — the platform never notices which widget it
            // is talking to.
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
                // Claim focus first (see contract 2, module docs) — this is a
                // hand-off, not a session blur, so the grid must not scrub.
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
            // Flags only — a `Cancel` arm never touches state. No key repeat
            // and no long-press: a bar key fires once per tap.
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
