//! `muxr_topbar` — the app's branded top bar (`docs/redesign.html`'s
//! `.topbar`), mounted **once** by [`crate::shell`] as persistent chrome
//! **floating over** the navigator rather than by each screen.
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
//! # Chrome icons are vectors. Font glyphs are for content text only.
//!
//! The bar's two affordances used to be **text runs** — a `‹` (`U+2039`) and a
//! `⚙` (`U+2699`) set in the button label's own face. Neither is in the
//! bundled Glyph faces (IBM Plex Mono / Space Mono), so each platform resolved
//! them through its own fallback chain and each answered differently: the `⚙`
//! was simply **absent on iPhone**, and the same string rendered as visibly
//! different marks on Android and iOS. That is not a tuning problem — a font
//! fallback chain is not a thing an app can specify — so every icon this bar
//! paints is now a **vector**: a [`frust::icons`] `IconSource` (or an
//! app-supplied one, [`CHEVRON_LEFT`]) filled as a path by [`icon_button`],
//! identical on every platform by construction.
//!
//! **The rule this leaves behind, for the whole app:** *chrome* icons — a bar
//! affordance, a FAB, a row's trailing chevron — are vectors. A **font glyph
//! is only ever content text**, where the run is the content (the Sessions
//! meta line's `▣ 9 panes`, the splash's brand rain) and a missing face
//! degrades to a tofu box in a sentence rather than to an invisible button.
//!
//! # Modal-aware: the bar dims and goes inert under an overlay
//!
//! The bar floats **over** the navigator (see [`crate::shell::root_view`]), so
//! a sheet or dialog pushed *inside* the navigator cannot cover it — it stayed
//! fully lit and, worse, fully **tappable** under an open modal: the `⚙`
//! navigated straight out from under a sheet. That is a modality hole, not a
//! cosmetic one.
//!
//! [`TopBarView::overlay`] closes it, from the depth fact
//! `crate::routes::BarState::overlay_active` publishes (live page-stack depth
//! deeper than the top *route*'s). While it is set the bar:
//!
//! - paints the **same scrim** the modal itself paints
//!   ([`crate::ui::sheet::SCRIM_ALPHA`] over `colors.scrim`), staged on the
//!   **same** [`ModalStage`] timings the sheet and the Glyph dialog use — two
//!   clocks started in the same rebuild rather than one shared driver, because
//!   a `NavigatorController` exposes no transition progress to read (FINDINGS
//!   #40), exactly as the bar's own reveal is matched to the page transition
//!   under it;
//! - takes **no input** and publishes **no semantics** — the same
//!   *inert-node-absent* rule the hidden state follows, and for the same
//!   reason: an accessibility node whose affordances route nowhere is worse
//!   than no node, and here it would additionally sit *outside* the modal's
//!   own `Role::Dialog` subtree, offering a screen reader a way around the
//!   modality the visual scrim is announcing.
//!
//! Inertness is driven by the **flag**, not by the tint's animation: modality
//! is binary, so the bar goes dead on the frame the overlay opens and comes
//! back on the frame it closes, while the tint fades either way. A gesture
//! pop or a scrim dismissal clears the flag through the mirror's own
//! depth-fall reconcile — nothing here polls.
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
//! absent child on purpose: the bar is a fixed-index child of the shell's root
//! `Stack`, and a `Stack` whose child count changed would shift every later
//! child's positional index and make reconciliation tear the live navigator
//! down (the bug `crate::shell::root_view`'s stable-`Stack` invariant exists
//! for). A **fully** hidden bar lays out to [`Size::ZERO`], paints nothing,
//! routes nothing, and contributes no semantics node.
//!
//! # Entrance and exit (why the bar animates itself)
//!
//! The bar does **not** displace the page under it — it floats over a
//! full-height navigator and each bar-bearing page reserves its own top
//! clearance ([`topbar_clearance`]). So a route change that gains or loses the
//! bar must not be a discrete 46px reflow of the page: the bar instead slides
//! in from fully above the top edge (its own inset-extended height) while
//! fading up, and reverses on the way out.
//!
//! Its timings and curves are **GlyphSlide's** — the very pattern the page
//! transition under it runs ([`GLYPH_SLIDE_ENTER`]/[`GLYPH_SLIDE_EXIT`],
//! imported rather than transcribed; the curves are transcribed because
//! `frust-widgets`' own are private) — so bar and page read as one motion.
//! They are matched, **not** frame-coupled: a `NavigatorController` exposes no
//! transition progress to read (FINDINGS #40), so nothing app-side can drive
//! the bar off the page's own driver. Same numbers, two clocks started in the
//! same rebuild.
//!
//! **Reduced motion** collapses the reveal exactly the way every framework
//! pattern collapses — the theme's `MotionScheme::reduce_motion`, the
//! framework's own [`REDUCE_MOTION_DURATION`], linear, and no geometric motion
//! at all (a pure fade). The bar re-derives that collapse rather than calling
//! it because neither `nav::transition::resolve_spec` nor `PatternSwitcher`'s
//! internal branch is reachable through the `frust::`/`frust::authoring`
//! facade an app widget is limited to.
//!
//! **A mid-reveal bar takes no input and publishes no semantics.** It paints
//! (that is the whole point) but a half-shown bar sitting over a page must not
//! swallow a tap meant for the page under it, and an accessibility node whose
//! affordances route nowhere is worse than no node. Both come back the frame
//! the reveal settles.
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
//! `AppBar` does. Since the bar floats *over* the page rather than beside it,
//! that same sum is what a bar-bearing page reserves at its own top:
//! [`topbar_clearance`] is the one helper the three such screens share, so the
//! two numbers cannot drift apart.

use std::rc::Rc;

use frust::authoring::text::LineHeight;
use frust::authoring::{
    Action, AnyView, BoxConstraints, BuildCtx, ChangeFlags, ChildPod, Color, ErasedCallback,
    EventCtx, EventResult, InputEvent, LayoutCtx, PRESSED_OPACITY, PaintCtx, PaintScene, Point,
    PointerPhase, Role, SemanticsCtx, Size, View, Widget, any, build_child, erase_callback,
    rebuild_child, route_event, teardown_child,
};
use frust::motion::patterns::{
    GLYPH_SLIDE_ENTER, GLYPH_SLIDE_EXIT, PatternLayer, TransitionPattern,
};
use frust::motion::switcher::{REDUCE_MOTION_DURATION, pattern_switcher};
use frust::{
    AnimationController, Axis, ChildKey, CrossAxisAlignment, Curve, EdgeInsets, FlexView, IconData,
    IconSource, Padding, SizedBox, Theme, Timing, icon, inflexible, safe_area, text,
};

use crate::ui::sheet::{ModalStage, SCRIM_ALPHA};

/// `.topbar{height:46px}` — the content band, below the status-bar inset.
///
/// `pub` because a page under the bar has to reserve exactly this much (plus
/// the status-bar inset the bar also consumes) at its own top — see
/// [`topbar_clearance`], the wrapper every such page uses instead of spelling
/// the sum itself.
pub const BAR_HEIGHT: f64 = 46.0;
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
/// The drawn size of the vector inside that box, in logical px. An original
/// value: `.topbar-icon` sets its glyph at 15px, and a Material Symbols
/// outline at 15px reads thinner than a 15px glyph does, so the mark is drawn
/// a step up — still comfortably inside the 30px box, with the box itself
/// staying the tap target.
const ICON_SIZE: f64 = 20.0;
/// Minimum clear space between the centered title and either side zone, in
/// logical px. An original value (the source's absolutely-positioned title can
/// overlap; a real title has to be clamped instead).
const TITLE_GUTTER: f64 = 8.0;
/// `.topbar{border-bottom:1px solid var(--border)}`.
const HAIRLINE: f64 = 1.0;

/// The brand wordmark, and the accessibility name of the bar itself.
const BRAND: &str = "muxr";

/// The back affordance's accessible name — what a screen reader announces, and
/// what a test addresses the button by.
///
/// A **word**, not the mark it draws: the slot used to carry the literal `‹`
/// as its label (it *was* the label, since the affordance was a text run), and
/// "single left-pointing angle quotation mark" is not a thing to read out.
pub const BACK_LABEL: &str = "Back";

/// The back affordance's mark: a chevron pointing at the leading edge, mirrored
/// from `frust::icons::CHEVRON_RIGHT` about the centre of its own 24×24 design
/// box (`x' = 24 - x`; the relative segments negate their `dx`).
///
/// **Why an app-supplied source rather than `icons::ARROW_BACK`.** The design
/// (`docs/redesign.html`'s settings topbar) draws a *chevron*, and the vendored
/// Material Symbols starter set carries `chevron_right` but no `chevron_left`,
/// so the collection cannot express the mark the mock asks for. Mirroring the
/// one it does carry keeps the stroke weight, the corner cuts and the optical
/// size identical to the framework's own chevron — a hand-drawn substitute
/// would match none of the three. `IconData::from_path` is the documented
/// app-supply seam; an `IconSource` is the cheaper half of it (a `d` string
/// needs no `kurbo` in app code), and the same seam the Connect FAB uses.
pub const CHEVRON_LEFT: IconSource = IconSource {
    d: "M 11.4 12 16 7.4 l -1.4 -1.4 -6 6 6 6 1.4 -1.4 -4.6 -4.6 Z",
    design: ICON_DESIGN_BOX,
};

/// The side length of the square design box every icon here is authored
/// against — Material Symbols' own 24, which [`frust::icons`]' generated
/// entries all carry.
const ICON_DESIGN_BOX: f64 = 24.0;

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

// ---------------------------------------------------------------------------
// The bar's own entrance/exit (see the module docs)
// ---------------------------------------------------------------------------

/// The reveal's slide-in easing — `motion::patterns`' `GLYPH_SLIDE_SPATIAL`
/// (the Glyph `spatial` curve, `Cubic(0.05,0.7,0.1,1)`), transcribed: the
/// durations beside it are `pub` in `frust-widgets`, the curves are not.
const REVEAL_SPATIAL_CURVE: Curve = Curve::Cubic(0.05, 0.7, 0.1, 1.0);
/// The reveal's fade-in easing — `GLYPH_SLIDE_EFFECTS` (the Glyph `effects`
/// curve, `Cubic(0.2,0,0,1)`), transcribed for the same reason.
const REVEAL_EFFECTS_CURVE: Curve = Curve::Cubic(0.2, 0.0, 0.0, 1.0);
/// The reveal's exit easing — `GLYPH_SLIDE_EXIT_CURVE` (the Glyph `exit`
/// curve, `Cubic(0.3,0,1,1)`), driving both the slide-out and the exit fade.
const REVEAL_EXIT_CURVE: Curve = Curve::Cubic(0.3, 0.0, 1.0, 1.0);

/// Where the bar paints at reveal progress `p` — `0.0` fully above the top
/// edge and transparent, `1.0` at rest — travelling `extent` (its own
/// inset-extended height, so a hidden bar is *entirely* off the top of the
/// screen rather than merely displaced).
///
/// `showing` picks the direction's easing family, exactly as `GlyphSlide`
/// splits its slower spatial entrance from its faster exit: an entrance eases
/// `p` on the spatial curve for the offset and the effects curve for the fade;
/// an exit eases the *remaining* progress on the exit curve for both.
///
/// `reduce_motion` collapses it to a pure linear fade with **no** geometric
/// motion — the framework's own rule (see the [module docs](self)).
fn reveal_layer(p: f64, showing: bool, extent: f64, reduce_motion: bool) -> PatternLayer {
    let p = p.clamp(0.0, 1.0);
    if reduce_motion {
        return PatternLayer {
            dx: 0.0,
            dy: 0.0,
            alpha: p as f32,
            scale: 1.0,
        };
    }
    let (travelled, alpha) = if showing {
        (
            REVEAL_SPATIAL_CURVE.transform(p),
            REVEAL_EFFECTS_CURVE.transform(p),
        )
    } else {
        // The exit runs on one curve for both channels, measured from the
        // still-shown state: at `p == 1.0` nothing has moved yet.
        let gone = REVEAL_EXIT_CURVE.transform(1.0 - p);
        (1.0 - gone, 1.0 - gone)
    };
    PatternLayer {
        dx: 0.0,
        dy: -(1.0 - travelled) * extent,
        alpha: alpha as f32,
        scale: 1.0,
    }
}

/// One in-flight reveal: a `0→1` driver plus the visual range it maps onto.
///
/// The range is what makes an *interrupted* reveal value-continuous — a flip
/// mid-flight starts a fresh driver from wherever the bar was actually
/// painting, the same "value continuity, not velocity continuity" contract
/// `frust::motion`'s own implicit animations document.
struct Reveal {
    /// The `0→1` progress driver (linear: the easing lives in
    /// [`reveal_layer`], which needs the raw progress to split its two
    /// channels the way `GlyphSlide` does).
    driver: AnimationController,
    /// The painted reveal this transition started from.
    from: f64,
    /// The reveal it is heading to (`1.0` showing, `0.0` hiding).
    to: f64,
    /// Whether it collapsed to the reduced-motion fade — resolved once at the
    /// start, stable for the whole transition (`PatternSwitcher`'s rule).
    reduced: bool,
}

impl Reveal {
    /// Stage a reveal from `from` to `to`, at the theme's motion policy.
    fn start(from: f64, to: f64, reduce_motion: bool) -> Self {
        let showing = to > from;
        let duration = if reduce_motion {
            REDUCE_MOTION_DURATION
        } else if showing {
            GLYPH_SLIDE_ENTER
        } else {
            GLYPH_SLIDE_EXIT
        };
        let mut driver = AnimationController::new(duration);
        driver.forward();
        Reveal {
            driver,
            from,
            to,
            reduced: reduce_motion,
        }
    }

    /// Whether this reveal is bringing the bar on screen.
    fn showing(&self) -> bool {
        self.to > self.from
    }
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
///     .action(icons::SETTINGS, "Settings", |state: &mut AppState| {
///         state.push_settings()
///     })
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
        overlay: false,
        surface: scheme.surface,
        rule: scheme.outline_variant,
        scrim: scheme.scrim,
        on_surface: scheme.on_surface,
        slot_radius: theme.shape.small,
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

/// `body` with the top clearance a bar-bearing page owes reserved above it:
/// the status-bar inset **plus** [`BAR_HEIGHT`], which is exactly the box the
/// floating bar occupies.
///
/// The one place that sum is spelled. The bar no longer displaces the page
/// (see [`crate::shell::root_view`]), so each of the three bar-bearing screens
/// pads its own top instead — and three screens computing "46 plus the inset"
/// independently is three chances to drift, most of them invisible until a
/// device with a different status bar shows up. `/` (Connect) is bar-less and
/// wraps nothing.
///
/// Composed out of framework widgets rather than hand-rolled: an outer
/// [`safe_area`] restricted to its top edge contributes the inset, an inner
/// [`Padding`] the band. `body` may keep its own `safe_area(..).top(false)`
/// for the other three edges — nesting is harmless, since a `SafeArea` deflates
/// constraints without consuming the window insets its descendants read.
pub fn topbar_clearance<State: 'static, V: View<State>>(body: V) -> AnyView<State> {
    any(safe_area(Padding(
        EdgeInsets {
            left: 0.0,
            top: BAR_HEIGHT,
            right: 0.0,
            bottom: 0.0,
        },
        body,
    ))
    .left(false)
    .right(false)
    .bottom(false))
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
    /// Whether a modal covers the app: the bar dims itself to match and goes
    /// inert — see the [module docs](self).
    overlay: bool,
    /// `--bg-surface`: the bar's own background.
    surface: Color,
    /// `--border`: the hairline under it.
    rule: Color,
    /// `colors.scrim`: the tone the overlay tint is mixed from — the modal's
    /// own, so the bar dims in the same ink the page under it does.
    scrim: Color,
    /// The ink every slot icon inherits (`--fg`).
    on_surface: Color,
    /// The corner radius a slot's pressed wash is drawn at.
    slot_radius: f64,
}

impl<State: 'static> TopBarView<State> {
    /// Replace the brand with a back chevron firing `on_press` — every route
    /// pushed over the stack's first titled one (see
    /// `crate::routes::BarLeading::Back`).
    pub fn back<F: Fn(&mut State) + 'static>(mut self, on_press: F) -> Self {
        self.leading = Rc::new(any(self.slot_icon(CHEVRON_LEFT, BACK_LABEL, on_press)));
        self.leading_key = ChildKey::new(BACK_LABEL);
        self.back = true;
        self
    }

    /// Put a vector `mark` (`icons::SETTINGS`, and Phase 3's
    /// `icons::MORE_VERT`) in the trailing icon slot under the accessible name
    /// `label`, firing `on_press` when it is tapped.
    ///
    /// The slot is an [`icon_button`] filling the source's 30×30
    /// `.topbar-icon` box: transparent at rest (the source's `.topbar-icon`
    /// only fills on hover, which a touch screen has no notion of), a pressed
    /// wash on tap, and one real `Role::Button` node in the accessibility tree
    /// — which is also how a test addresses it.
    pub fn action<F: Fn(&mut State) + 'static>(
        mut self,
        mark: impl Into<IconData>,
        label: impl Into<String>,
        on_press: F,
    ) -> Self {
        self.action = Some(any(self.slot_icon(mark, label, on_press)));
        self
    }

    /// Paint the bar over an animated scrim and take no input while `overlay`
    /// is set — the modality mirror. See the [module docs](self).
    pub fn overlay(mut self, overlay: bool) -> Self {
        self.overlay = overlay;
        self
    }

    /// One `.topbar-icon` slot: `mark` inked in the bar's own on-surface tone,
    /// in the 30×30 box the layout hands it.
    fn slot_icon<F: Fn(&mut State) + 'static>(
        &self,
        mark: impl Into<IconData>,
        label: impl Into<String>,
        on_press: F,
    ) -> IconButtonView<State> {
        icon_button(mark, label, on_press)
            .ink(self.on_surface)
            .icon_size(ICON_SIZE)
            .radius(self.slot_radius)
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
// Icon button — the app's one vector affordance
// ---------------------------------------------------------------------------

/// A pressable **vector** icon: `mark` centred in a box, labelled `label` for
/// accessibility, firing `on_press` on a release inside its bounds.
///
/// This is the app's answer to "chrome icons are vectors" (see the
/// [module docs](self)): `frust::button` takes a `String` label and nothing
/// else, so the only way to press a `frust::icon` is to compose the press
/// around it — and a bare `GestureDetector` contributes no accessibility node
/// of its own, which would leave every chrome affordance in the app
/// unreachable to a screen reader. So this is a leaf press target over one
/// icon pod, the same shape (and for the same reason) as
/// `features::connect::presentation::chrome`'s `press_target`, differing only
/// in that it *owns* the mark it paints rather than wrapping arbitrary art.
///
/// **Lives here, beside the bar that needed it first**, and is `pub` because
/// the Connect FAB is its second caller — `examples/huddle`'s own convention
/// (promote a hand-rolled primitive only once a second consumer appears).
/// A third caller is the moment to give it its own `crate::ui` module.
///
/// The press treatment is the framework's own [`PRESSED_OPACITY`] wash rather
/// than `Button`'s scale pivot: scaling needs an `AnimationController` this
/// leaf has no reason to own, and a 30px chrome mark dipping in tone reads the
/// same.
pub fn icon_button<State: 'static, F: Fn(&mut State) + 'static>(
    mark: impl Into<IconData>,
    label: impl Into<String>,
    on_press: F,
) -> IconButtonView<State> {
    IconButtonView {
        mark: mark.into(),
        label: label.into(),
        ink: None,
        fill: Color::TRANSPARENT,
        icon_size: ICON_DESIGN_BOX,
        box_size: None,
        radius: 0.0,
        on_press: Rc::new(on_press),
    }
}

/// The declarative half of [`icon_button`].
pub struct IconButtonView<State: 'static> {
    mark: IconData,
    label: String,
    /// The mark's ink; `None` lets [`frust::icon`] resolve `on_surface` from
    /// the threaded theme (its own documented default).
    ink: Option<Color>,
    /// A resting background fill — [`Color::TRANSPARENT`] for bar chrome, an
    /// accent for a FAB.
    fill: Color,
    icon_size: f64,
    /// The tap target; `None` shrink-wraps to the mark.
    box_size: Option<Size>,
    radius: f64,
    on_press: Rc<dyn Fn(&mut State)>,
}

impl<State: 'static> IconButtonView<State> {
    /// Ink the mark explicitly, overriding the theme's `on_surface` default.
    pub fn ink(mut self, ink: Color) -> Self {
        self.ink = Some(ink);
        self
    }

    /// Fill the button's box behind the mark (a FAB's accent disc).
    pub fn fill(mut self, fill: Color) -> Self {
        self.fill = fill;
        self
    }

    /// Draw the mark at `size` logical px (default: its own 24px design box).
    pub fn icon_size(mut self, size: f64) -> Self {
        self.icon_size = size;
        self
    }

    /// Give the button a fixed tap target rather than shrink-wrapping the mark
    /// — the platform 44/48px minimum, or a design's own box.
    pub fn box_size(mut self, size: Size) -> Self {
        self.box_size = Some(size);
        self
    }

    /// Round the fill and the pressed wash by `radius`.
    pub fn radius(mut self, radius: f64) -> Self {
        self.radius = radius;
        self
    }

    /// The mark, as the pod this button paints and lays out.
    fn mark_view(&self) -> AnyView<State> {
        let mut view = icon(self.mark.clone()).size(self.icon_size);
        if let Some(ink) = self.ink {
            view = view.color(ink);
        }
        any(view)
    }
}

/// The retained half of [`icon_button`].
pub struct IconButtonWidget {
    mark: ChildPod,
    label: String,
    fill: Color,
    ink: Option<Color>,
    icon_size: f64,
    box_size: Option<Size>,
    radius: f64,
    /// The pressed *visual*; follows the pointer in and out while captured.
    pressed: bool,
    /// Armed by a `Down` (alongside `capture_pointer`), cleared on
    /// `Up`/`Cancel` — gates `Move`/`Up` so a hover never latches or fires.
    captured: bool,
    on_press: ErasedCallback,
}

impl<State: 'static> View<State> for IconButtonView<State> {
    type Element = IconButtonWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> IconButtonWidget {
        IconButtonWidget {
            mark: build_child(&self.mark_view(), ctx),
            label: self.label.clone(),
            fill: self.fill,
            ink: self.ink,
            icon_size: self.icon_size,
            box_size: self.box_size,
            radius: self.radius,
            pressed: false,
            captured: false,
            on_press: erase_callback(&self.on_press),
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut IconButtonWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        // Closures aren't comparable; reinstall the adapter every time.
        element.on_press = erase_callback(&self.on_press);
        // The icon pod diffs itself (`IconView::rebuild` skips re-parsing an
        // unchanged `d`), so this just re-supplies the current mark.
        let mut flags = rebuild_child(&prev.mark_view(), &self.mark_view(), &mut element.mark, ctx);
        if prev.box_size != self.box_size || prev.icon_size != self.icon_size {
            element.box_size = self.box_size;
            element.icon_size = self.icon_size;
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if prev.fill != self.fill || prev.ink != self.ink || prev.radius != self.radius {
            element.fill = self.fill;
            element.ink = self.ink;
            element.radius = self.radius;
            flags |= ChangeFlags::PAINT;
        }
        if prev.label != self.label {
            element.label = self.label.clone();
            // Semantics are recomputed off the paint pass, so a label change
            // republishes through PAINT.
            flags |= ChangeFlags::PAINT;
        }
        flags
    }

    fn teardown(&self, element: &mut IconButtonWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.mark_view(), &mut element.mark, ctx);
    }
}

/// Whether `pos` (widget-local) falls inside a `size`-sized box.
fn inside(pos: Point, size: Size) -> bool {
    pos.x >= 0.0 && pos.y >= 0.0 && pos.x < size.width && pos.y < size.height
}

impl Widget for IconButtonWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let mark_bc = BoxConstraints::tight(Size::new(self.icon_size, self.icon_size));
        let mark = self.mark.layout_child(ctx, &mark_bc);
        // A declared box is the tap target; without one the mark *is* the
        // button. Either way a tight incoming constraint (a `.topbar-icon`
        // slot) wins outright, and the mark centres in whatever results.
        let size = bc.constrain(self.box_size.unwrap_or(mark));
        self.mark.set_origin(Point::new(
            (size.width - mark.width) / 2.0,
            (size.height - mark.height) / 2.0,
        ));
        size
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let origin = ctx.origin();
        let size = ctx.size();
        if self.fill != Color::TRANSPARENT {
            scene.fill_rounded_rect(origin, size, self.radius, self.fill);
        }
        self.mark.paint_child(ctx, scene);
        if self.pressed {
            // Washed in the mark's own ink, so a ghost slot on a dark bar and
            // an accent disc both darken rather than one of them glowing.
            let wash = self.ink.unwrap_or(Color::BLACK);
            scene.fill_rounded_rect(origin, size, self.radius, wash.with_alpha(PRESSED_OPACITY));
        }
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        let InputEvent::Pointer(pointer) = event else {
            return EventResult::Ignored;
        };
        match pointer.phase {
            PointerPhase::Down => {
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
            // (`docs/CODE_STANDARDS.md`'s Interaction Semantics).
            PointerPhase::Cancel => {
                if !self.captured {
                    return EventResult::Ignored;
                }
                self.pressed = false;
                self.captured = false;
                ctx.request_redraw();
                EventResult::Handled
            }
        }
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        // One labelled Button leaf, like `frust`'s own `Button` over its label
        // pod: an icon is decorative on its own (`IconView` publishes nothing
        // unlabelled), and the *name* of this control is what a screen reader
        // needs — never the shape of its mark.
        ctx.push_node(Role::Button, |node| {
            node.set_label(self.label.as_str());
            node.add_action(Action::Click);
        });
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
    /// Each pod's at-rest origin, recorded by `layout` so `paint` can slide the
    /// whole bar by the reveal offset without a relayout per frame. Parallel to
    /// [`Self::children`]; empty while the bar is fully hidden (nothing is laid
    /// out then).
    rest_origins: Vec<Point>,
    /// Whether the leading slot is the 30×30 chevron box rather than the brand.
    back: bool,
    hidden: bool,
    /// Whether a modal is up: the bar is inert **from this frame**, and its
    /// tint animates toward [`Self::tint`]'s target.
    overlay: bool,
    surface: Color,
    rule: Color,
    scrim: Color,
    /// The painted overlay tint: `0.0` untinted, `1.0` at full
    /// [`SCRIM_ALPHA`].
    tint: f64,
    /// The in-flight tint staging, `None` at rest — resolved on the first
    /// **paint** after a flip, like [`Self::reveal_anim`], and on the modal's
    /// own [`ModalStage`] timings so bar and sheet dim together.
    tint_anim: Option<ModalStage>,
    /// An overlay flip the last rebuild staged, awaiting that first paint.
    tint_pending: bool,
    /// The painted reveal: `0.0` fully above the top edge, `1.0` at rest.
    reveal: f64,
    /// The in-flight reveal, `None` at rest. Created on the first **paint**
    /// after a visibility flip, not on the rebuild that staged it: a
    /// `BuildCtx` carries no theme to resolve `reduce_motion` from (the
    /// deferred-driver idiom `frust::motion`'s switcher and the navigator both
    /// use).
    reveal_anim: Option<Reveal>,
    /// A visibility flip the last rebuild staged, awaiting that first paint.
    reveal_pending: bool,
    /// The bar's own inset-extended height, recorded by `layout` — the
    /// distance a reveal travels.
    extent: f64,
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
            rest_origins: Vec::new(),
            back: self.back,
            hidden: self.hidden,
            overlay: self.overlay,
            surface: self.surface,
            rule: self.rule,
            scrim: self.scrim,
            // A first mount never animates either channel: the bar is simply
            // already where (and as dim as) the route says it belongs — this
            // is also the frame a cold start's splash covers.
            tint: if self.overlay { 1.0 } else { 0.0 },
            tint_anim: None,
            tint_pending: false,
            reveal: if self.hidden { 0.0 } else { 1.0 },
            reveal_anim: None,
            reveal_pending: false,
            extent: 0.0,
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
        if prev.back != self.back {
            element.back = self.back;
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if prev.hidden != self.hidden {
            // A visibility flip is a *reveal*, never a jump: the next paint
            // resolves the theme's motion policy and starts the animation from
            // wherever the bar is painting right now (see `Reveal`).
            element.hidden = self.hidden;
            element.reveal_pending = true;
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if prev.overlay != self.overlay {
            // Inertness follows the flag immediately (modality is binary); only
            // the tint animates, and only from the next paint — where a theme
            // is in scope to resolve the modal timings from.
            element.overlay = self.overlay;
            element.tint_pending = true;
            flags |= ChangeFlags::PAINT;
        }
        if prev.surface != self.surface || prev.rule != self.rule || prev.scrim != self.scrim {
            element.surface = self.surface;
            element.rule = self.rule;
            element.scrim = self.scrim;
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

impl TopBarWidget {
    /// Whether a reveal is staged or running — the bar is on screen and moving.
    fn revealing(&self) -> bool {
        self.reveal_pending || self.reveal_anim.is_some()
    }

    /// Whether the bar occupies a box at all: shown, or mid-reveal in either
    /// direction. A **fully** hidden bar is `false` and lays out to nothing.
    fn occupies_a_box(&self) -> bool {
        !self.hidden || self.revealing()
    }

    /// Whether the bar is settled on screen **and** nothing modal covers the
    /// app — the one state in which it takes input and publishes semantics
    /// (see the [module docs](self)).
    fn settled_open(&self) -> bool {
        !self.hidden && !self.revealing() && !self.overlay
    }

    /// Advance the overlay tint one frame and report the alpha fraction to
    /// paint it at (`0.0` untinted, `1.0` full [`SCRIM_ALPHA`]).
    fn advance_tint(&mut self, ctx: &mut PaintCtx) -> f32 {
        let Some(stage) = self.tint_anim.as_mut() else {
            return self.tint as f32;
        };
        let animating = stage.advance(ctx.frame_time());
        self.tint = stage.value();
        let (_, alpha) = stage.shape(self.tint);
        if animating {
            ctx.request_frame();
        } else {
            self.tint = stage.target();
            self.tint_anim = None;
            // One more frame so the settled tint is the one on screen — the
            // same courtesy the reveal pays itself.
            ctx.request_frame();
        }
        alpha
    }
}

impl Widget for TopBarWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        if !self.occupies_a_box() {
            // Zero-height, children unlaid — `paint`/`event`/`semantics` all
            // short-circuit on the same predicate, so nothing reads their
            // geometry. (A reveal that *ends* hidden leaves the last full box
            // behind until the next relayout: harmless, since a bar that is
            // neither painting nor routing has nothing to give away, and any
            // later rebuild corrects it.)
            self.rest_origins.clear();
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

        // The rest pose `paint` slides away from, and the distance it slides.
        self.rest_origins = self.children.iter().map(ChildPod::origin).collect();
        self.extent = content_top + BAR_HEIGHT;
        bc.constrain(Size::new(width, self.extent))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        if !self.occupies_a_box() {
            return;
        }
        let theme = Theme::from_paint_ctx(ctx);
        // Start a staged reveal now a theme is in scope: `reduce_motion` and
        // the entrance/exit duration are paint-time reads (module docs).
        if self.reveal_pending {
            let reduce = theme
                .map(|theme| theme.motion.reduce_motion)
                .unwrap_or(false);
            let to = if self.hidden { 0.0 } else { 1.0 };
            self.reveal_anim = Some(Reveal::start(self.reveal, to, reduce));
            self.reveal_pending = false;
        }
        // …and the overlay tint, on the modal's own timings (module docs).
        if self.tint_pending {
            let to = if self.overlay { 1.0 } else { 0.0 };
            self.tint_anim = Some(ModalStage::start(self.tint, to, theme));
            self.tint_pending = false;
        }

        // Advance it, and let the frame clock — not the rebuild — decide when
        // the bar has arrived.
        let mut layer = PatternLayer {
            dx: 0.0,
            dy: 0.0,
            alpha: 1.0,
            scale: 1.0,
        };
        if let Some(reveal) = self.reveal_anim.as_mut() {
            let animating = reveal.driver.advance(ctx.frame_time());
            self.reveal = reveal.from + (reveal.to - reveal.from) * reveal.driver.value_clamped();
            layer = reveal_layer(self.reveal, reveal.showing(), self.extent, reveal.reduced);
            if animating {
                ctx.request_frame();
            } else {
                self.reveal = reveal.to;
                self.reveal_anim = None;
                // One more frame so the settled pose (and the semantics/input
                // that come back with it) is the one on screen.
                ctx.request_frame();
            }
        }

        let origin = Point::new(ctx.origin().x, ctx.origin().y + layer.dy);
        let size = ctx.size();
        let faded = layer.alpha < 1.0;
        if faded {
            scene.push_layer(ctx.origin(), size, layer.alpha);
        }
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

        // Slide the slots with the surface. Pod origins (rather than a
        // `push_transform`) for the same reason `frust::motion`'s switcher
        // stages its children that way: paint and hit-testing move together.
        let aligned = self.rest_origins.len() == self.children.len();
        for (index, child) in self.children.iter_mut().enumerate() {
            if aligned {
                let rest = self.rest_origins[index];
                child.set_origin(Point::new(rest.x, rest.y + layer.dy));
            }
            child.paint_child(ctx, scene);
        }

        // The modality tint, over everything the bar just painted and inside
        // its reveal layer, so a bar arriving under an open sheet fades in
        // already dimmed rather than flashing lit.
        let tint = self.advance_tint(ctx);
        if tint > 0.0 {
            scene.fill_rect(origin, size, self.scrim.with_alpha(SCRIM_ALPHA * tint));
        }

        if faded {
            scene.pop_layer();
        }
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        if !self.settled_open() {
            // Hidden, mid-reveal, or **under a modal**: a bar that is only
            // half-arrived must not eat a tap meant for the page it floats
            // over, and a bar behind a sheet's scrim must not act at all
            // (module docs). Ignoring rather than swallowing is what lets the
            // modal underneath treat the press as the outside-tap it is.
            return EventResult::Ignored;
        }
        // Only the leading/trailing slots are interactive, but routing to every
        // child is what keeps that true as slots grow — `route_event` owns the
        // capture/focus fast paths.
        route_event(&mut self.children, ctx, event)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        if !self.settled_open() {
            // `/` has no bar at all, in the accessibility tree as on screen —
            // a mid-reveal bar routes nothing, so it publishes nothing, and a
            // bar under a modal publishes nothing for the stronger reason that
            // a node here would sit *outside* the modal's own `Role::Dialog`
            // subtree and offer a way round the modality (module docs).
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
    fn the_clearance_reserves_exactly_the_bars_own_box() {
        // The seam three screens share with the bar: what a bar-bearing page
        // pads at its top must be exactly what the floating bar occupies. At a
        // zero window inset that is the 46px band and nothing else — the inset
        // half of the sum is the *same* `WindowInsets::padding().top` both
        // sides resolve (`TopBarWidget::layout` adds it, the wrapper's own
        // top-only `safe_area` contributes it), which `LayoutCtx::new` cannot
        // fake here (the setter is `pub(crate)`).
        const CHILD: f64 = 100.0;
        let view: AnyView<()> = topbar_clearance(SizedBox::<()>(Some(CHILD), Some(CHILD)));
        let mut next_id = 0u64;
        let mut widget = view.build(&mut BuildCtx::new(&mut next_id));
        let mut ctx = LayoutCtx::new();
        let size = widget.layout(&mut ctx, &BoxConstraints::loose(Size::new(390.0, 844.0)));

        assert_eq!(size, Size::new(CHILD, CHILD + BAR_HEIGHT));
    }

    #[test]
    fn the_reveal_starts_fully_above_the_bar_and_settles_flush() {
        // The entrance's endpoints, as geometry: at rest the bar is exactly
        // where layout put it and fully opaque; at progress zero it is its own
        // whole height above the top edge (never merely nudged) and invisible.
        const EXTENT: f64 = 46.0 + 47.0;
        let settled = reveal_layer(1.0, true, EXTENT, false);
        assert_eq!((settled.dy, settled.alpha), (0.0, 1.0));
        assert_eq!(settled.dx, 0.0, "the reveal is vertical only");

        let start = reveal_layer(0.0, true, EXTENT, false);
        assert_eq!(start.dy, -EXTENT);
        assert_eq!(start.alpha, 0.0);

        // …and the exit runs the same range backwards: still flush at 1.0
        // (nothing has left yet), fully gone at 0.0.
        let leaving = reveal_layer(1.0, false, EXTENT, false);
        assert_eq!((leaving.dy, leaving.alpha), (0.0, 1.0));
        let gone = reveal_layer(0.0, false, EXTENT, false);
        assert_eq!((gone.dy, gone.alpha), (-EXTENT, 0.0));
    }

    #[test]
    fn reduced_motion_fades_the_bar_without_moving_it() {
        // Glyph's hard accessibility rule, at this seam: a reduced reveal is a
        // pure linear fade — the bar never travels, in either direction.
        for showing in [true, false] {
            for p in [0.0, 0.25, 0.5, 0.75, 1.0] {
                let layer = reveal_layer(p, showing, 93.0, true);
                assert_eq!(layer.dy, 0.0, "reduced motion never moves the bar");
                assert_eq!(layer.dx, 0.0);
                assert_eq!(layer.alpha, p as f32, "…it only fades it, linearly");
            }
        }
    }

    #[test]
    fn the_exit_clears_faster_than_the_entrance_arrives() {
        // GlyphSlide's own asymmetry, inherited: exits run on the faster exit
        // curve over a shorter duration, so at the same fraction of its own
        // timeline a leaving bar has travelled further than an arriving one.
        assert!(GLYPH_SLIDE_EXIT < GLYPH_SLIDE_ENTER);
        let arriving = reveal_layer(0.5, true, 100.0, false);
        let leaving = reveal_layer(0.5, false, 100.0, false);
        assert!(
            leaving.dy < arriving.dy,
            "half-way through, the exit is further gone ({}) than the entrance \
             is arrived ({})",
            leaving.dy,
            arriving.dy
        );
    }

    // -----------------------------------------------------------------------
    // Vector chrome
    // -----------------------------------------------------------------------

    #[test]
    fn the_back_chevron_is_the_frameworks_own_chevron_mirrored() {
        use frust::authoring::{BezPath, Shape};

        let mine = BezPath::from_svg(CHEVRON_LEFT.d).expect("the back chevron parses");
        let theirs =
            BezPath::from_svg(frust::icons::CHEVRON_RIGHT.d).expect("the vendored one parses");
        let (mine, theirs) = (mine.bounding_box(), theirs.bounding_box());

        // A mirror about the design box's centre maps `x` to `design - x`, so
        // the two bounding boxes swap across it and share their y band exactly.
        // (The framework's own icon test already pins `theirs` inside the box;
        // this pins that the transcription is a *reflection* and not a redraw.)
        let design = CHEVRON_LEFT.design;
        assert_eq!(CHEVRON_LEFT.design, frust::icons::CHEVRON_RIGHT.design);
        assert!((mine.x0 - (design - theirs.x1)).abs() < 1e-9, "{mine:?}");
        assert!((mine.x1 - (design - theirs.x0)).abs() < 1e-9, "{mine:?}");
        assert!((mine.y0 - theirs.y0).abs() < 1e-9, "{mine:?}");
        assert!((mine.y1 - theirs.y1).abs() < 1e-9, "{mine:?}");
    }

    #[test]
    fn the_slot_mark_fits_the_slot_that_holds_it() {
        // The tap target is the source's 30×30 `.topbar-icon` box; the vector
        // is drawn inside it, so a mark bigger than its box would be clipped
        // by the bar's own band before anyone noticed.
        const { assert!(ICON_SIZE < ICON_SLOT) };
        const { assert!(ICON_SLOT <= BAR_HEIGHT) };
    }

    #[test]
    fn the_back_affordance_announces_a_word_and_not_a_mark() {
        // The a11y half of the vector switch: the slot used to be labelled by
        // the glyph it drew, which a screen reader would read out as the name
        // of the character. A label is a *name*.
        assert!(
            BACK_LABEL.chars().all(|c| c.is_ascii_alphabetic()),
            "{BACK_LABEL:?} is a name, not a mark"
        );
    }

    // -----------------------------------------------------------------------
    // The overlay tint
    // -----------------------------------------------------------------------

    #[test]
    fn the_bars_tint_is_the_very_scrim_the_modal_paints() {
        // The one number this whole mirror rests on: the bar dims by exactly
        // as much as the page under the sheet does, because it is the same
        // constant and the same `colors.scrim` role. Anything else reads as a
        // bar that is "sort of" disabled.
        assert_eq!(SCRIM_ALPHA, crate::ui::sheet::SCRIM_ALPHA);
        assert!((0.0..=1.0).contains(&SCRIM_ALPHA));
    }

    #[test]
    fn an_overlay_makes_the_bar_inert_the_moment_it_opens() {
        // Modality is binary: the flag alone gates input and semantics, so a
        // bar whose tint has not started fading yet is *already* dead. (The
        // tint's own animation is the sheet's `ModalStage`, tested there.)
        let mut widget = TopBarWidget {
            children: Vec::new(),
            rest_origins: Vec::new(),
            back: false,
            hidden: false,
            overlay: false,
            surface: Color::BLACK,
            rule: Color::BLACK,
            scrim: Color::BLACK,
            tint: 0.0,
            tint_anim: None,
            tint_pending: false,
            reveal: 1.0,
            reveal_anim: None,
            reveal_pending: false,
            extent: BAR_HEIGHT,
        };
        assert!(widget.settled_open(), "a settled bar takes input");

        widget.overlay = true;
        widget.tint_pending = true;
        assert!(
            !widget.settled_open(),
            "…and stops the frame a modal opens, before the tint has moved"
        );
        assert!(
            widget.occupies_a_box(),
            "it still paints — dimmed, not gone"
        );

        widget.overlay = false;
        widget.tint_pending = true;
        assert!(
            widget.settled_open(),
            "…and comes back the frame the modal closes"
        );
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
