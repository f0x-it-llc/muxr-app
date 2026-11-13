//! The QR affordance and its press-order gates: busy, the free-tier verdict, then the
//! camera permission. The permission is asked on the tap, before the sheet exists, so the
//! OS dialog never shows over the sheet's entrance animation.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use frust::authoring::Size;
use frust::{
    Align, Alignment, ButtonStyle, Either, SizedBox, Theme, View, button, container, icon, icons,
    spawn_local,
};
use frust_camera::{CameraError, PermissionStatus};

use super::super::controller::ConnectController;
use super::pro_gate::scan_blocked;
use super::qr_sheet::{CAMERA_UNAVAILABLE, camera_backend_available, qr_sheet_body};
use crate::AppState;
use crate::features::pro::presentation::controller::ProController;
use crate::ui::paint::HAIRLINE;
use crate::ui::palette::Palette;
use crate::ui::sheet::show_sheet;
use crate::ui::splash::spec::QR_SQUARE;
use crate::ui::toasts::app_toasts;
use crate::ui::topbar::icon_button;

/// Also what `tests/connect.rs` addresses the button by, and the QR sheet's title.
pub const FAB_LABEL: &str = "Pair by QR code";

const QR_MARK: f64 = 20.0;

/// Transient, so a re-tap is the retry.
const PERMISSION_PENDING: &str =
    "Camera permission request already in progress — try again in a moment.";
/// iOS shows its dialog once per install; the only way forward is the system settings app.
const PERMISSION_DENIED: &str = concat!(
    "Camera permission denied — enable camera access for muxr in your device ",
    "settings, then scan again.",
);
const PERMISSION_NEEDS_UI: &str =
    "The permission dialog needs muxr in the foreground — try again from here.";

pub(crate) fn qr_square<S: 'static>(
    colors: &Palette,
    theme: &Theme,
    on_scan: Option<Rc<dyn Fn()>>,
) -> impl View<S> {
    let mark = match on_scan {
        None => Either::Left(Align(
            Alignment::CENTER,
            icon(icons::SCAN_MARK).size(QR_MARK).color(colors.amber),
        )),
        Some(on_scan) => Either::Right(
            icon_button(icons::SCAN_MARK, FAB_LABEL, move |_state: &mut S| on_scan())
                .ink(colors.amber)
                .icon_size(QR_MARK)
                .box_size(Size::new(QR_SQUARE, QR_SQUARE))
                .radius(theme.shape.small),
        ),
    };
    SizedBox::<S>(Some(QR_SQUARE), Some(QR_SQUARE)).child(
        container(mark)
            .fill(colors.raised)
            .radius(theme.shape.small)
            .border(colors.outline, HAIRLINE),
    )
}

/// `frust::button` takes a text label only, so the mock's leading scan mark is not painted.
pub(crate) fn qr_button<S: 'static>(
    overlays: &frust::NavigatorController<AppState>,
    controller: &Arc<ConnectController>,
    pending: &Rc<Cell<bool>>,
    pro: &Arc<ProController>,
) -> impl View<S> {
    let press = qr_scan_action(overlays, controller, pending, pro);
    styled_qr_button(move |_state: &mut S| press())
}

pub(crate) fn splash_qr_button<S: 'static>() -> impl View<S> {
    styled_qr_button(|_state: &mut S| {})
}

fn styled_qr_button<S: 'static>(on_press: impl Fn(&mut S) + 'static) -> impl View<S> {
    button(FAB_LABEL, on_press)
        .style(ButtonStyle::Secondary)
        .label_alignment(Alignment::CENTER)
}

/// Busy blocks the tap too: the offer dialog pops synchronously on accept, leaving the
/// affordance reachable for the rest of the held save. `pending` survives rebuilds, so taps
/// on two builds' handlers share one cell.
pub(crate) fn qr_scan_action(
    overlays: &frust::NavigatorController<AppState>,
    controller: &Arc<ConnectController>,
    pending: &Rc<Cell<bool>>,
    pro: &Arc<ProController>,
) -> Rc<dyn Fn()> {
    let overlays = overlays.clone();
    let controller = Arc::clone(controller);
    let pending = Rc::clone(pending);
    let pro = Arc::clone(pro);
    Rc::new(move || {
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
    })
}

fn start_scan(
    overlays: &frust::NavigatorController<AppState>,
    controller: &Arc<ConnectController>,
    pending: &Rc<Cell<bool>>,
) {
    let overlays = overlays.clone();
    let controller = Arc::clone(controller);
    request_camera_access(pending, move || open_qr_sheet(&overlays, controller));
}

fn open_qr_sheet(
    overlays: &frust::NavigatorController<AppState>,
    controller: Arc<ConnectController>,
) {
    show_sheet(overlays, move |sheet| {
        let sheet = sheet.clone();
        qr_sheet_body(
            Rc::new(move || {
                sheet.close();
            }),
            Arc::clone(&controller),
        )
    });
}

/// Single-flight on `pending`: a second `Granted` would open two sessions on one camera.
/// With no camera backend this calls `on_granted` directly.
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

/// Target-gated so a build with no camera backend never reaches AVFoundation or JNI.
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
    fn the_square_clears_the_minimum_tap_target() {
        const MIN_TAP_TARGET: f64 = 44.0;
        const { assert!(QR_SQUARE >= MIN_TAP_TARGET) };
        const { assert!(QR_MARK < QR_SQUARE) };
    }

    // The permission arm is target-gated, so these pin the outcome table and the latch headless.

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
        // A fast double tap during the async permission window.
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
