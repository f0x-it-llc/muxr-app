//! The `.fab` QR affordance and its three press-order gates: busy, the
//! free-tier verdict, then the camera permission.
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
//!
//! Ahead of the permission gate sits [`pro_gate::scan_blocked`](super::pro_gate::scan_blocked):
//! a free install that already remembers a server is turned back at the tap,
//! before the camera is ever asked about, and its unlock re-enters
//! [`start_scan`] — the same permission-then-sheet flow the ungated press
//! takes.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use frust::authoring::Size;
use frust::{
    Align, Alignment, ButtonStyle, EdgeInsets, Padding, View, button, icons, safe_area, spawn_local,
};
use frust_camera::{CameraError, PermissionStatus};

use super::super::controller::ConnectController;
use super::pro_gate::scan_blocked;
use super::qr_sheet::{CAMERA_UNAVAILABLE, camera_backend_available, qr_sheet_body};
use crate::AppState;
use crate::features::pro::presentation::controller::ProController;
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
    pro: &Arc<ProController>,
) -> impl View<S> {
    let theme = active_theme();
    let scheme = theme.scheme();
    safe_area(Align(
        Alignment::BOTTOM_RIGHT,
        Padding(
            EdgeInsets::all(FAB_MARGIN),
            icon_button(
                icons::SCAN_MARK,
                FAB_LABEL,
                qr_press(overlays, controller, pending, pro),
            )
            .fill(scheme.primary_container)
            .ink(scheme.on_primary_container)
            .icon_size(FAB_MARK)
            .box_size(Size::new(FAB_BOX, FAB_BOX))
            .radius(FAB_BOX / 2.0),
        ),
    ))
}

/// The Tablet form's in-flow QR action: a secondary button under the FAB's
/// own name, firing the FAB's press path gate for gate.
///
/// Known gap: `frust::button` takes a text label only, so the mock's leading
/// scan mark is not painted here.
pub(crate) fn qr_button<S: 'static>(
    overlays: &frust::NavigatorController<AppState>,
    controller: &Arc<ConnectController>,
    pending: &Rc<Cell<bool>>,
    pro: &Arc<ProController>,
) -> impl View<S> {
    styled_qr_button(qr_press(overlays, controller, pending, pro))
}

/// [`qr_button`] with nothing behind the press: the splash's copy of the
/// Tablet action, the same pixels with no handles to fire (the parity
/// contract — the overlay forwards no input anyway).
pub(crate) fn splash_qr_button<S: 'static>() -> impl View<S> {
    styled_qr_button(|_state: &mut S| {})
}

/// The Tablet QR action's one styling, shared by its live and splash copies.
fn styled_qr_button<S: 'static>(on_press: impl Fn(&mut S) + 'static) -> impl View<S> {
    button(FAB_LABEL, on_press)
        .style(ButtonStyle::Secondary)
        .label_alignment(Alignment::CENTER)
}

/// The press both QR affordances fire: busy, the free-tier gate, then
/// [`start_scan`] (see the [module docs](self)).
fn qr_press<S: 'static>(
    overlays: &frust::NavigatorController<AppState>,
    controller: &Arc<ConnectController>,
    pending: &Rc<Cell<bool>>,
    pro: &Arc<ProController>,
) -> impl Fn(&mut S) + 'static {
    let overlays = overlays.clone();
    let controller = Arc::clone(controller);
    let pending = Rc::clone(pending);
    let pro = Arc::clone(pro);
    move |_state: &mut S| {
        if controller.busy_untracked() {
            return;
        }
        let overlays = overlays.clone();
        let controller = Arc::clone(&controller);
        let pending = Rc::clone(&pending);
        let on_unlocked: Rc<dyn Fn()> = {
            let overlays = overlays.clone();
            let controller = Arc::clone(&controller);
            let pending = Rc::clone(&pending);
            Rc::new(move || start_scan(&overlays, &controller, &pending))
        };
        if scan_blocked(&controller, &overlays, &pro, on_unlocked) {
            return;
        }
        start_scan(&overlays, &controller, &pending);
    }
}

/// The scan flow proper — the camera-permission gate, then the sheet it
/// opens on a grant — shared by the press and by [`scan_blocked`]'s unlock
/// callback, so an unlock from the paywall lands the user in the scanner
/// they asked for.
fn start_scan(
    overlays: &frust::NavigatorController<AppState>,
    controller: &Arc<ConnectController>,
    pending: &Rc<Cell<bool>>,
) {
    let overlays = overlays.clone();
    let controller = Arc::clone(controller);
    request_camera_access(pending, move || open_qr_sheet(&overlays, controller));
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
