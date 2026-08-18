//! The `.fab` QR affordance and its camera-permission gate.
//!
//! The permission prompt is gated on the tap, not the sheet's mount:
//! [`request_camera_access`] runs the blocking permission call off the UI
//! thread and pushes [`open_qr_sheet`] only on `Granted`. Every other
//! outcome raises a toast instead ([`permission_outcome_message`] is the
//! whole decision table) and never mounts the sheet; the FAB stays tappable,
//! so a re-tap is the retry. Asking before the sheet exists keeps the OS
//! dialog from showing over the sheet's entrance animation.
//!
//! That round trip is a real async window, so `request_camera_access` owns a
//! single-flight latch: a re-entrant call while it reads `true` is dropped,
//! rather than starting a second check that could resolve `Granted` twice
//! and open two sessions on one physical camera. It clears once this check
//! has settled, whichever way. The press handler additionally refuses the
//! tap while [`ConnectController::busy_untracked`] holds — load-bearing: the
//! offer dialog pops synchronously on accept, so for the rest of the held
//! save this screen is uncovered and the affordance is reachable.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use frust::authoring::Size;
use frust::{
    Align, Alignment, AnyView, EdgeInsets, IconSource, Padding, any, safe_area, spawn_local,
};
use frust_camera::{CameraError, PermissionStatus};

use super::super::controller::ConnectController;
use super::qr_sheet::{CAMERA_UNAVAILABLE, camera_backend_available, qr_sheet_body};
use crate::AppState;
use crate::ui::palette::active_theme;
use crate::ui::sheet::show_sheet;
use crate::ui::toasts::app_toasts;
use crate::ui::topbar::icon_button;

/// The QR affordance's accessible name — what a screen reader announces, and
/// what `tests/connect.rs` addresses the button by.
pub const FAB_LABEL: &str = "Pair by QR code";

/// Inset from the screen's safe area. Bespoke placement geometry, not a spacing step.
const FAB_MARGIN: f64 = 20.0;
/// The FAB's box, in logical px — clears the 44px platform minimum tap target.
const FAB_BOX: f64 = 46.0;
const FAB_MARK: f64 = 22.0;

/// A concurrent request raced this tap's own call out. Transient, so a re-tap is the retry.
const PERMISSION_PENDING: &str =
    "Camera permission request already in progress — try again in a moment.";
/// iOS shows its dialog once per install, so a second ask is a no-op and the
/// only way forward is the system settings app.
const PERMISSION_DENIED: &str = concat!(
    "Camera permission denied — enable camera access for muxr in your device ",
    "settings, then scan again.",
);
/// Android has no foreground activity cached to show the dialog on — transient.
const PERMISSION_NEEDS_UI: &str =
    "The permission dialog needs muxr in the foreground — try again from here.";

/// The QR/scan mark, as a vector rather than the `▣` glyph: `U+25A3` is in
/// none of the bundled faces, so each platform would draw a different mark
/// (`crate::ui::topbar`'s "Chrome icons are vectors").
///
/// App-supplied because `frust::icons` carries no QR/scan glyph. Authored
/// against the standard 24×24 design box as one `d` string of axis-aligned
/// rectangles: eight bracket arms ([`SCAN_BRACKET`]-long, [`SCAN_STROKE`]-thick,
/// inset [`SCAN_INSET`]) plus the centre square. The tests below re-derive
/// that geometry, so a typo cannot pass as art.
pub(super) const SCAN_MARK: IconSource = IconSource {
    d: concat!(
        // top-left corner: across, then down
        "M4 4h7v2.5H4Z M4 4h2.5v7H4Z ",
        // top-right
        "M13 4h7v2.5h-7Z M17.5 4H20v7h-2.5Z ",
        // bottom-left
        "M4 17.5h2.5V20H4Z M4 13h2.5v7H4Z ",
        // bottom-right
        "M13 17.5h7V20h-7Z M17.5 13H20v7h-2.5Z ",
        // the centre square
        "M9 9h6v6H9Z",
    ),
    design: 24.0,
};

/// [`SCAN_MARK`]'s inset, bracket-arm length, and arm thickness — the three
/// numbers the `d` string is built from.
const SCAN_INSET: f64 = 4.0;
const SCAN_BRACKET: f64 = 7.0;
const SCAN_STROKE: f64 = 2.5;

// The mark reads as a scan *frame* only while its corner brackets stay corners:
// an edge's two arms must leave a gap between them, and an arm must be thinner
// than it is long.
const _: () = {
    assert!(2.0 * SCAN_BRACKET < 24.0 - 2.0 * SCAN_INSET);
    assert!(SCAN_STROKE < SCAN_BRACKET);
};

/// The amber QR/pair affordance, pinned to the screen's bottom-trailing safe
/// area. The tap is the camera-permission gate (see the [module docs](self)).
///
/// `pending` is threaded down rather than resolved here: it has to survive
/// across rebuilds, so two taps landing on two builds' press handlers close
/// over the same cell. An [`icon_button`] rather than [`frust::button`]
/// because the affordance is a mark, not a `String` label.
pub(crate) fn qr_affordance<S: 'static>(
    overlays: &frust::NavigatorController<AppState>,
    controller: &Arc<ConnectController>,
    pending: &Rc<Cell<bool>>,
) -> AnyView<S> {
    let overlays = overlays.clone();
    let controller = Arc::clone(controller);
    let pending = Rc::clone(pending);
    let theme = active_theme();
    let scheme = theme.scheme();
    any(safe_area(Align(
        Alignment::BOTTOM_RIGHT,
        Padding(
            EdgeInsets::all(FAB_MARGIN),
            icon_button(SCAN_MARK, FAB_LABEL, move |_state: &mut S| {
                if controller.busy_untracked() {
                    return;
                }
                let overlays = overlays.clone();
                let controller = Arc::clone(&controller);
                request_camera_access(&pending, move || open_qr_sheet(&overlays, controller));
            })
            .fill(scheme.primary_container)
            .ink(scheme.on_primary_container)
            .icon_size(FAB_MARK)
            .box_size(Size::new(FAB_BOX, FAB_BOX))
            .radius(FAB_BOX / 2.0),
        ),
    )))
}

/// Pushes the QR sheet as a page of `overlays`, closing itself the moment a
/// scanned payload lands.
fn open_qr_sheet(
    overlays: &frust::NavigatorController<AppState>,
    controller: Arc<ConnectController>,
) {
    show_sheet(overlays, move |sheet| {
        // Threaded in as the one-shot closer the scan pump fires.
        let sheet = sheet.clone();
        qr_sheet_body(
            Rc::new(move || {
                sheet.close();
            }),
            Arc::clone(&controller),
        )
    });
}

/// The FAB permission gate: checks/requests off the UI thread and calls
/// `on_granted` only on `Granted` (see the [module docs](self)). With no
/// camera backend there is no dialog to race, so this calls `on_granted`
/// directly and lets the sheet render its own unavailable state.
fn request_camera_access(pending: &Rc<Cell<bool>>, on_granted: impl FnOnce() + 'static) {
    if pending.replace(true) {
        return;
    }
    if !camera_backend_available() {
        on_granted();
        pending.set(false);
        return;
    }
    let pending = Rc::clone(pending);
    spawn_local(async move {
        let outcome = match frust::spawn_blocking(request_permission_checked).await {
            Ok(result) => result,
            Err(join_error) => Err(CameraError::Platform(join_error.to_string())),
        };
        match permission_outcome_message(&outcome) {
            None => on_granted(),
            Some(message) => app_toasts().show(message),
        }
        pending.set(false);
    });
}

/// Target-gated so a build with no camera backend never reaches AVFoundation
/// (or the Android JNI surface) at all. The `Err` arm is reachable only in
/// principle, since the caller checks the gate first.
fn request_permission_checked() -> Result<PermissionStatus, CameraError> {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        frust_camera::Camera::request_permission()
    }
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        Err(CameraError::PlatformNotInitialized)
    }
}

/// `None` opens the sheet; `Some` is the toast to show instead. Split out so
/// the mapping is unit-testable independent of the target-gated platform
/// call behind it.
fn permission_outcome_message(
    outcome: &Result<PermissionStatus, CameraError>,
) -> Option<&'static str> {
    match outcome {
        Ok(PermissionStatus::Granted) => None,
        Ok(PermissionStatus::Denied) => Some(PERMISSION_DENIED),
        Ok(PermissionStatus::NeedsUi) => Some(PERMISSION_NEEDS_UI),
        Ok(PermissionStatus::Pending) => Some(PERMISSION_PENDING),
        // `#[non_exhaustive]`: an unrecognized answer reads as an unspecified refusal.
        Ok(_) | Err(_) => Some(CAMERA_UNAVAILABLE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use frust::authoring::{BezPath, Shape};

    const EPSILON: f64 = 1e-9;

    #[test]
    fn the_scan_mark_parses_into_nine_subpaths() {
        // A malformed `d` is a panic at first paint — this is the tripwire.
        let path = BezPath::from_svg(SCAN_MARK.d).expect("the scan mark parses");
        assert!(!path.elements().is_empty());
        assert_eq!(
            SCAN_MARK.d.matches('M').count(),
            9,
            "eight bracket arms plus the centre square"
        );
    }

    #[test]
    fn the_scan_mark_fills_its_design_box_symmetrically() {
        // Every bracket sits `SCAN_INSET` in from its own edge on all four sides.
        let path = BezPath::from_svg(SCAN_MARK.d).expect("the scan mark parses");
        let bbox = path.bounding_box();
        let far = SCAN_MARK.design - SCAN_INSET;
        assert!((bbox.x0 - SCAN_INSET).abs() < EPSILON, "{bbox:?}");
        assert!((bbox.y0 - SCAN_INSET).abs() < EPSILON, "{bbox:?}");
        assert!((bbox.x1 - far).abs() < EPSILON, "{bbox:?}");
        assert!((bbox.y1 - far).abs() < EPSILON, "{bbox:?}");
    }

    #[test]
    fn the_far_brackets_sit_where_the_named_numbers_put_them() {
        assert!((SCAN_MARK.design - SCAN_INSET - SCAN_BRACKET - 13.0).abs() < EPSILON);
        assert!((SCAN_MARK.design - SCAN_INSET - SCAN_STROKE - 17.5).abs() < EPSILON);
    }

    #[test]
    fn the_fab_clears_the_minimum_tap_target() {
        const MIN_TAP_TARGET: f64 = 44.0;
        const { assert!(FAB_BOX >= MIN_TAP_TARGET) };
        const { assert!(FAB_MARK < FAB_BOX) };
    }

    // The real permission arm is target-gated, so an end-to-end test cannot
    // run headless. These pin the outcome→action table, the pass-through
    // with no camera backend, and the single-flight latch (simulated by
    // re-entering from inside a call's own `on_granted`).

    #[test]
    fn only_a_granted_outcome_opens_the_sheet() {
        assert_eq!(
            permission_outcome_message(&Ok(PermissionStatus::Granted)),
            None,
            "a grant carries no feedback — the gate opens the sheet instead"
        );
    }

    #[test]
    fn every_other_outcome_carries_feedback_and_never_opens_the_sheet() {
        assert_eq!(
            permission_outcome_message(&Ok(PermissionStatus::Denied)),
            Some(PERMISSION_DENIED)
        );
        assert_eq!(
            permission_outcome_message(&Ok(PermissionStatus::NeedsUi)),
            Some(PERMISSION_NEEDS_UI)
        );
        assert_eq!(
            permission_outcome_message(&Ok(PermissionStatus::Pending)),
            Some(PERMISSION_PENDING)
        );
        assert_eq!(
            permission_outcome_message(&Err(CameraError::PlatformNotInitialized)),
            Some(CAMERA_UNAVAILABLE),
            "a platform error is an unspecified refusal, not a panic or a \
             silent open"
        );
    }

    #[test]
    fn a_build_with_no_camera_backend_has_no_dialog_to_gate_on() {
        assert!(!camera_backend_available());

        let pending = Rc::new(Cell::new(false));
        let opened = Rc::new(Cell::new(false));
        let flag = Rc::clone(&opened);
        request_camera_access(&pending, move || flag.set(true));
        assert!(
            opened.get(),
            "request_camera_access must call on_granted directly when there \
             is no camera backend to ask permission of"
        );
        assert!(
            !pending.get(),
            "…and clear its latch once that fast path has settled, so the \
             next tap is admitted"
        );
    }

    #[test]
    fn a_second_tap_while_the_first_permission_check_is_still_open_is_dropped() {
        // Re-enters from inside the first call's `on_granted`, before `pending`
        // clears — a fast double tap during the async permission window.
        let pending = Rc::new(Cell::new(false));
        let opens = Rc::new(Cell::new(0_u32));

        fn tap(pending: &Rc<Cell<bool>>, opens: &Rc<Cell<u32>>) {
            let pending_reentrant = Rc::clone(pending);
            let opens_reentrant = Rc::clone(opens);
            request_camera_access(pending, move || {
                opens_reentrant.set(opens_reentrant.get() + 1);
                tap(&pending_reentrant, &opens_reentrant);
            });
        }

        tap(&pending, &opens);

        assert_eq!(
            opens.get(),
            1,
            "a re-entrant tap while the first check is still open must not \
             open a second sheet"
        );
        assert!(
            !pending.get(),
            "the latch clears once its own check has fully settled, so a \
             later, non-reentrant tap is still admitted"
        );
    }
}
