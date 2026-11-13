//! The QR-pairing sheet. The detection callback runs on a plugin-owned thread and never
//! writes a signal; a UI-thread pump parses and applies. Scanned text is a bearer token:
//! never formatted into a message, a label or a log line.

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use clean_signals_frust::use_interval;
use frust::{
    Align, Alignment, Axis, ButtonStyle, Component, CrossAxisAlignment, Either, FlexView, Get,
    RwSignal, Set, SizedBox, Theme, View, button, component, container, icon, icons, inflexible,
    on_cleanup, platform_view, spawn_blocking, spawn_local, text,
};
use frust_camera::barcode::{Barcode, BarcodeFormat};
use frust_camera::{BarcodeStreamOptions, CameraError, CameraSession, DetectionPolicy};
use zeroize::Zeroizing;

use super::super::controller::ConnectController;
use super::qr_affordance::FAB_LABEL;
use crate::AppState;
use crate::features::connect::domain::use_cases::parse_pairing::parse_pairing_uri;
use crate::ui::paint::HAIRLINE;
use crate::ui::palette::{active_theme, inked};
use crate::ui::spacing;

const SHEET_TITLE: &str = FAB_LABEL;
const SCAN_HINT: &str = "Point the camera at the QR code shown by muxrctl on your server.";
pub const NO_CAMERA: &str =
    "Camera pairing needs a phone or tablet — enter the server URL and token by hand instead.";
/// The plugin's own detail is never shown.
pub(super) const CAMERA_UNAVAILABLE: &str = "Camera unavailable.";
const RETRY_LABEL: &str = "try again";
const VIEWFINDER_LABEL: &str = "Camera viewfinder";

/// Leaves the caption room on a 340px-wide phone.
const VIEWFINDER: f64 = 220.0;
const VIEWFINDER_MARK: f64 = 44.0;

const SCAN_POLL_INTERVAL: Duration = Duration::from_millis(100);

const STREAM_ERROR_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// `close` fires exactly once, from the scan pump; every other way out tears this down.
pub fn qr_sheet_body(
    close: Rc<dyn Fn()>,
    controller: Arc<ConnectController>,
) -> impl View<AppState> {
    component(QrScanSheet { close, controller })
}

struct QrScanSheet {
    close: Rc<dyn Fn()>,
    controller: Arc<ConnectController>,
}

pub struct QrSheetState {
    /// `Arc<Mutex<..>>` only because [`on_cleanup`] takes a `Send + Sync` closure.
    session: Arc<Mutex<Option<Arc<CameraSession>>>>,
    detections: Arc<Detections>,
    opened: bool,
    camera_failed: RwSignal<bool>,
}

/// A mutex, never a signal: writing a signal on the plugin's thread is forbidden outright.
#[derive(Default)]
struct Detections {
    last: Mutex<Option<Zeroizing<String>>>,
}

impl Detections {
    fn publish(&self, raw: &str) {
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(Zeroizing::new(raw.to_string()));
    }

    fn take(&self) -> Option<Zeroizing<String>> {
        self.last
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

impl Component for QrScanSheet {
    type State = QrSheetState;

    fn init(&self) -> QrSheetState {
        let session: Arc<Mutex<Option<Arc<CameraSession>>>> = Arc::new(Mutex::new(None));
        {
            // This sheet holds the device's single image-stream claim; `close` blocks on
            // Apple, so it runs off the UI thread.
            let cell = Arc::clone(&session);
            on_cleanup(move || {
                let session = cell.lock().unwrap_or_else(PoisonError::into_inner).take();
                if let Some(session) = session {
                    spawn_blocking(move || session.close());
                }
            });
        }

        let detections = Arc::new(Detections::default());
        spawn_detection_pump(
            Arc::clone(&detections),
            Arc::clone(&self.controller),
            Rc::clone(&self.close),
        );

        let camera_failed = RwSignal::new(false);
        spawn_stream_error_poller(Arc::clone(&session), camera_failed);

        QrSheetState {
            session,
            detections,
            opened: false,
            camera_failed,
        }
    }

    fn build(&self, state: &mut QrSheetState) -> impl View<QrSheetState> {
        let theme = active_theme();
        open_camera_once(state);
        match sheet_stage(state) {
            Stage::Live => Either::Left(live_body(state, &theme)),
            Stage::Unavailable(message, tone, retryable) => {
                Either::Right(message_body(&theme, message, tone, retryable))
            }
        }
    }
}

enum Stage {
    Live,
    Unavailable(&'static str, MessageTone, bool),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MessageTone {
    Muted,
    Failed,
}

/// `has_backend` is a parameter: the real gate is a compile-time constant, `false` on a
/// test host.
fn stage_for(has_backend: bool, state: &QrSheetState) -> Stage {
    if !has_backend {
        return Stage::Unavailable(NO_CAMERA, MessageTone::Muted, false);
    }
    if state.camera_failed.get() {
        return Stage::Unavailable(CAMERA_UNAVAILABLE, MessageTone::Failed, true);
    }
    if has_session(state) {
        return Stage::Live;
    }
    Stage::Unavailable(CAMERA_UNAVAILABLE, MessageTone::Failed, true)
}

fn sheet_stage(state: &QrSheetState) -> Stage {
    stage_for(camera_backend_available(), state)
}

pub(super) const fn camera_backend_available() -> bool {
    cfg!(any(target_os = "android", target_os = "ios"))
}

fn has_session(state: &QrSheetState) -> bool {
    state
        .session
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .is_some()
}

fn open_camera_once(state: &mut QrSheetState) {
    if state.opened || !camera_backend_available() {
        return;
    }
    state.opened = true;
    let Ok(session) = open_camera() else {
        state.camera_failed.set(true);
        return;
    };
    let session = Arc::new(session);
    if start_scan(&session, &state.detections).is_err() {
        // A session that cannot stream is let go rather than held open behind an error state.
        spawn_blocking(move || session.close());
        state.camera_failed.set(true);
        return;
    }
    *state.session.lock().unwrap_or_else(PoisonError::into_inner) = Some(session);
}

/// `Ok(())` is accepted, not confirmed, on iOS: a late failure arrives through the poller.
/// `NoDuplicates`: a code sits in frame for as long as it takes to focus.
fn start_scan(session: &CameraSession, detections: &Arc<Detections>) -> Result<(), CameraError> {
    let detections = Arc::clone(detections);
    session.start_barcode_stream(
        BarcodeStreamOptions {
            formats: vec![BarcodeFormat::QrCode],
            detection: DetectionPolicy::NoDuplicates,
        },
        move |found: &[Barcode]| {
            if let Some(barcode) = found.first() {
                detections.publish(&barcode.raw_value);
            }
        },
    )
}

fn spawn_stream_error_poller(
    session: Arc<Mutex<Option<Arc<CameraSession>>>>,
    camera_failed: RwSignal<bool>,
) {
    if !camera_backend_available() {
        return;
    }
    use_interval(STREAM_ERROR_POLL_INTERVAL, move || {
        let current = session
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let Some(current) = current else {
            return;
        };
        if current.take_stream_error().is_some() {
            *session.lock().unwrap_or_else(PoisonError::into_inner) = None;
            spawn_blocking(move || current.close());
            camera_failed.set(true);
        }
    });
}

/// Checks its cancel flag before every read, so a torn-down sheet never touches the
/// controller again.
fn spawn_detection_pump(
    detections: Arc<Detections>,
    controller: Arc<ConnectController>,
    close: Rc<dyn Fn()>,
) {
    let cancelled = Arc::new(AtomicBool::new(false));
    {
        let cancelled = Arc::clone(&cancelled);
        on_cleanup(move || cancelled.store(true, Ordering::Relaxed));
    }
    spawn_local(async move {
        loop {
            if cancelled.load(Ordering::Relaxed) {
                return;
            }
            if let Some(raw) = detections.take()
                && let Some(payload) = parse_pairing_uri(&raw)
            {
                controller.apply_scanned_payload(payload);
                close();
                return;
            }
            clean_signals::time::sleep(SCAN_POLL_INTERVAL).await;
        }
    });
}

fn live_body(state: &QrSheetState, theme: &Theme) -> impl View<QrSheetState> + use<> {
    let scheme = theme.scheme();
    let guard = state.session.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(session) = guard.as_ref() else {
        // Unreachable: rendering the instruction beats an `expect` on a lock a rebuild could race.
        drop(guard);
        return Either::Left(message_body(
            theme,
            CAMERA_UNAVAILABLE,
            MessageTone::Failed,
            true,
        ));
    };
    let view_type = session.preview_view_type().to_string();
    let params = session.params_json();
    drop(guard);

    let viewfinder = container(
        SizedBox::<QrSheetState>(Some(VIEWFINDER), Some(VIEWFINDER)).child(
            platform_view(view_type)
                .size(VIEWFINDER, VIEWFINDER)
                .params_json(params)
                .semantics_label(VIEWFINDER_LABEL),
        ),
    )
    .fill(scheme.surface_container_lowest)
    .radius(theme.shape.medium)
    .border(scheme.outline, HAIRLINE);

    let children = vec![
        inflexible(Align(Alignment::CENTER, viewfinder)),
        inflexible(SizedBox::<QrSheetState>(None, Some(spacing::XL))),
        inflexible(
            text(SHEET_TITLE).style(inked(&theme.type_scale.title_medium, scheme.on_surface)),
        ),
        inflexible(SizedBox::<QrSheetState>(None, Some(spacing::XL / 2.0))),
        inflexible(text(SCAN_HINT).style(inked(
            &theme.type_scale.body_small,
            scheme.on_surface_variant,
        ))),
    ];

    Either::Right(FlexView::new(Axis::Vertical, children).cross_axis(CrossAxisAlignment::Stretch))
}

fn message_body(
    theme: &Theme,
    message: &str,
    tone: MessageTone,
    retryable: bool,
) -> impl View<QrSheetState> + use<> {
    let scheme = theme.scheme();
    let viewfinder = container(
        SizedBox::<QrSheetState>(Some(VIEWFINDER), Some(VIEWFINDER)).child(Align(
            Alignment::CENTER,
            icon(icons::SCAN_MARK)
                .size(VIEWFINDER_MARK)
                .color(scheme.primary),
        )),
    )
    .fill(scheme.surface_container_lowest)
    .radius(theme.shape.medium)
    .border(scheme.outline, HAIRLINE);
    let message_ink = match tone {
        MessageTone::Muted => scheme.on_surface_variant,
        MessageTone::Failed => scheme.error,
    };

    let mut children = vec![
        inflexible(Align(Alignment::CENTER, viewfinder)),
        inflexible(SizedBox::<QrSheetState>(None, Some(spacing::XL))),
        inflexible(
            text(SHEET_TITLE).style(inked(&theme.type_scale.title_medium, scheme.on_surface)),
        ),
        inflexible(SizedBox::<QrSheetState>(None, Some(spacing::XL / 2.0))),
        inflexible(text(message).style(inked(&theme.type_scale.body_small, message_ink))),
    ];
    if retryable {
        children.push(inflexible(SizedBox::<QrSheetState>(
            None,
            Some(spacing::XL),
        )));
        children.push(inflexible(Align(
            Alignment::CENTER,
            button(RETRY_LABEL, retry_camera).style(ButtonStyle::Secondary),
        )));
    }

    FlexView::new(Axis::Vertical, children).cross_axis(CrossAxisAlignment::Stretch)
}

/// Permission is not re-asked; it was confirmed before this sheet mounted.
fn retry_camera(state: &mut QrSheetState) {
    state.camera_failed.set(false);
    state.opened = false;
}

fn open_camera() -> Result<CameraSession, CameraError> {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        frust_camera::Camera::open(frust_camera::Lens::Back, frust_camera::Resolution::Auto)
    }
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        Err(CameraError::PlatformNotInitialized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A real `CameraSession` cannot be built on a host, so these pin the error-to-stage
    // mapping and the no-backend poller.

    fn bare_state(camera_failed: bool) -> QrSheetState {
        QrSheetState {
            session: Arc::new(Mutex::new(None)),
            detections: Arc::new(Detections::default()),
            opened: true,
            camera_failed: RwSignal::new(camera_failed),
        }
    }

    #[test]
    fn a_camera_failure_maps_to_the_unavailable_stage_regardless_of_cause() {
        let state = bare_state(true);
        assert!(
            matches!(
                stage_for(true, &state),
                Stage::Unavailable(CAMERA_UNAVAILABLE, MessageTone::Failed, true)
            ),
            "a camera_failed state must render the same failure copy a \
             synchronous open/start refusal already does"
        );
    }

    #[test]
    fn no_backend_wins_over_a_stale_camera_failed_flag() {
        let state = bare_state(true);
        assert!(matches!(
            stage_for(false, &state),
            Stage::Unavailable(NO_CAMERA, MessageTone::Muted, false)
        ));
    }

    #[test]
    fn the_stream_error_poller_never_touches_use_interval_without_a_camera_backend() {
        // No ambient owner on purpose: reaching `use_interval` would spawn an unstoppable task.
        assert!(!camera_backend_available());
        spawn_stream_error_poller(Arc::new(Mutex::new(None)), RwSignal::new(false));
    }
}
