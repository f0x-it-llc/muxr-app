//! The terminal **key bar**: the twelve keys a soft keyboard has no room
//! for, docked above the keyboard while — and only while — the IME is up.
//!
//! ```text
//! Ctrl  Alt  Shift  Esc │ Tab  ↑  ↓  ←  → │ PgUp  PgDn │ Del
//! ```
//!
//! Ported from the Flutter incumbent (order/sequences/latch semantics), plus
//! a `Shift` it never had. Every byte leaves through the same [`InputSeam`]
//! the grid emits on, so a latch armed here transforms the next OS-keyboard
//! character — the reason the latch lives at that emission choke point.
//!
//! Contracts:
//! 1. **Fixed slot, never appears/disappears**: `key_bar(false, ..)` returns
//!    a zero-height view, mounted unconditionally — an unkeyed child list
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
    Align, Alignment, Axis, CrossAxisAlignment, EdgeInsets, FlexChild, FlexView, Get, IconSource,
    Padding, SizedBox, Theme, flexible, icon, icons, inflexible, text,
};

use crate::ui::palette::{Palette, active_theme};

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

    let theme = active_theme();
    let colors = Palette::from_theme(&theme);
    let ink = CardInk::from_theme(&theme);
    let danger = theme.scheme().error;
    let latch = seam.latch();
    // The keyboard's own inset the bar docks on; zero in a headless test/desktop.
    let keyboard = frust::use_context::<WindowInsets>()
        .map(|insets| insets.view_insets.bottom)
        .unwrap_or(0.0);

    let mut children: Vec<FlexChild<State>> = Vec::with_capacity(BarKey::ORDER.len() * 2);
    for (index, key) in BarKey::ORDER.iter().copied().enumerate() {
        if key.separator_before() {
            // Replaces the gap rather than adding to it — its own margin,
            // and the row has no px to spend twice.
            children.push(inflexible(separator::<State>(&colors)));
        } else if index > 0 {
            children.push(inflexible(SizedBox::<State>(Some(CAP_GAP), None)));
        }
        children.push(flexible(
            cap_weight(key),
            key_cap(key, &theme, &colors, danger, latch, seam),
        ));
    }

    // Band outside, padding inside: full-bleed chrome must reach both screen
    // edges, keyboard inset included.
    any(BarBackdropView {
        fill: ink.head,
        hairline: ink.card_border,
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
        // Via `press_bar_key`, never hand-rolled: it's where the read-only gate lives.
        on_press: Rc::new(move |_state: &mut State| {
            pressed_seam.press_bar_key(key);
        }),
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
    label: TextStyle,
    ink: Color,
    fill: Color,
    border: Color,
    armed: bool,
    seam: InputSeam,
    on_press: Rc<dyn Fn(&mut State)>,
}

impl<State: 'static> KeyCapView<State> {
    /// The cap's content: a vector arrow, or its printed label — centred
    /// inside a [`CAP_PAD_X`] inset that stays inside the press target and
    /// fill, so a wide label can never paint over the border.
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
        // Closures and the `Rc`-backed seam aren't comparable; reinstall both every time.
        element.on_press = erase_callback(&self.on_press);
        element.seam = self.seam.clone();
        let mut flags = rebuild_child(&prev.content(), &self.content(), &mut element.content, ctx);
        if prev.fill != self.fill || prev.border != self.border || prev.armed != self.armed {
            element.fill = self.fill;
            element.border = self.border;
            element.toggled = self.key.is_latch().then_some(self.armed);
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
        let size = bc.constrain(Size::new(width, CAP_HEIGHT));
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
