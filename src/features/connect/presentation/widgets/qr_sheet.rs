//! The QR-pairing sheet: a live scanning viewfinder, or the copy that
//! explains why there isn't one.
//!
//! A `Component` rather than a plain builder because it owns a camera
//! session, and therefore a lifecycle: opened on mount, torn down on every
//! way out through `Component::init`'s `on_cleanup` — never a `Drop` impl.
//! Permission is already granted by the time this mounts (the FAB gate,
//! [`super::qr_affordance`]).
//!
//! No signal write from inside the detection callback, which runs on a
//! plugin-owned thread: it copies the raw value into [`Detections`]' plain
//! mutex cell, and the UI-thread [`spawn_detection_pump`] loop parses it,
//! applies it, and closes the sheet. First parseable payload wins; an
//! unparseable one is dropped silently. No scanned text is ever formatted
//! into a message, a label or a log line — the raw value is a bearer token
//! in the clear, and it lives in a [`Zeroizing`] holder until the parse
//! consumes it.
//!
//! A stream start is accepted, not confirmed, on iOS: the platform can
//! still reject the attach after [`start_scan`] returned `Ok`, so
//! [`spawn_stream_error_poller`] routes a late failure into the same
//! `camera_failed` signal a synchronous one sets. Every
//! [`CameraSession::close`] call runs off the UI thread, since it blocks on Apple.
//!
//! The camera is a phone/tablet affordance ([`camera_backend_available`]):
//! every other target renders the unavailable state, keeping a desktop
//! preview (and every headless test) from asking AVFoundation for a capture
//! device it has no usage-description string to justify.

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use clean_signals_frust::use_interval;
use frust::{
    Align, Alignment, AnyView, Axis, ButtonStyle, Component, CrossAxisAlignment, FlexView, Get,
    RwSignal, Set, SizedBox, Theme, any, button, component, container, icon, icons, inflexible,
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
/// Pairing by hand is the whole path with no camera backend, so this is a
/// plain instruction rather than an error.
pub const NO_CAMERA: &str =
    "Camera pairing needs a phone or tablet — enter the server URL and token by hand instead.";
/// The camera exists but refused, or the permission check failed outright.
/// The plugin's own detail is never shown (`crate::failure`'s rule).
pub(super) const CAMERA_UNAVAILABLE: &str = "Camera unavailable.";
const RETRY_LABEL: &str = "try again";
/// A platform view carries no frust-visible semantics of its own.
const VIEWFINDER_LABEL: &str = "Camera viewfinder";

/// The largest viewfinder square that still leaves the sheet's caption room
/// on a 340px-wide phone.
const VIEWFINDER: f64 = 220.0;
const VIEWFINDER_MARK: f64 = 44.0;

/// Well under the reaction time of pointing a phone at a code, and far
/// above an uncontended lock's cost. The scan itself runs at the camera's
/// rate, not this one.
const SCAN_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Modest — only has to notice a deferred attach failure before the user
/// gives up on a dead viewfinder.
const STREAM_ERROR_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// `close` is the one-shot dismissal, fired exactly once by the scan pump
/// when a payload has been applied; every other way out tears this
/// component down, which is what closes the camera. `pub` so
/// `tests/connect.rs` can mount it directly.
pub fn qr_sheet_body(close: Rc<dyn Fn()>, controller: Arc<ConnectController>) -> AnyView<AppState> {
    any(component(QrScanSheet { close, controller }))
}

/// Stateless; everything retained lives in [`QrSheetState`].
struct QrScanSheet {
    /// See [`qr_sheet_body`].
    close: Rc<dyn Fn()>,
    /// The form a scanned payload is applied to.
    controller: Arc<ConnectController>,
}

pub struct QrSheetState {
    /// `Arc<Mutex<..>>` only because [`on_cleanup`] takes a `Send + Sync` closure.
    session: Arc<Mutex<Option<Arc<CameraSession>>>>,
    /// Where the plugin thread leaves what it decoded.
    detections: Arc<Detections>,
    /// Whether the camera has been opened for this mount, so a refusal is
    /// not retried on every rebuild.
    opened: bool,
    /// An [`RwSignal`] because the poller writes it from outside `build`,
    /// and a tracked read wakes the rebuild that switches stage.
    camera_failed: RwSignal<bool>,
}

/// The one-slot hand-off between the camera's decode thread and the UI thread.
///
/// A plain mutex, never a signal: `on_detect` runs on a plugin-owned thread
/// with the platform's buffer borrowed, where writing a signal is forbidden
/// outright. The held value is [`Zeroizing`] because a pairing QR's text is
/// a token.
#[derive(Default)]
struct Detections {
    last: Mutex<Option<Zeroizing<String>>>,
}

impl Detections {
    /// Records `raw` as the newest detection, replacing (and scrubbing) any
    /// the pump has not taken yet. Runs on the plugin's thread.
    fn publish(&self, raw: &str) {
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(Zeroizing::new(raw.to_string()));
    }

    /// Runs on the UI thread.
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
            // The mandatory teardown: this sheet holds the device's single
            // image-stream claim, so `close` is the whole teardown — off the
            // UI thread, since it blocks on Apple.
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

    fn build(&self, state: &mut QrSheetState) -> AnyView<QrSheetState> {
        let theme = active_theme();
        open_camera_once(state);
        match sheet_stage(state) {
            Stage::Live => live_body(state, &theme),
            Stage::Unavailable(message, tone, retryable) => {
                message_body(&theme, message, tone, retryable)
            }
        }
    }
}

/// What the sheet is showing this frame.
enum Stage {
    Live,
    /// No viewfinder — the copy that says why, its ink, and whether a retry
    /// could plausibly change the answer.
    Unavailable(&'static str, MessageTone, bool),
}

/// Whether a message reads as an ordinary instruction or as a failure.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MessageTone {
    /// Nothing went wrong (the no-backend path).
    Muted,
    /// The camera is there and refused.
    Failed,
}

/// The error→stage decision [`sheet_stage`] wraps. `has_backend` is a
/// parameter rather than a direct [`camera_backend_available`] read since
/// that gate is a compile-time constant, always `false` on a test host.
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
    // Unreachable in practice: `open_camera_once` either opens a session or
    // sets `camera_failed`. A safe fallback rather than an `unreachable!`.
    Stage::Unavailable(CAMERA_UNAVAILABLE, MessageTone::Failed, true)
}

/// [`stage_for`] with this build's real [`camera_backend_available`] answer
/// plugged in.
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

/// Once per mount, from `build`. A no-op with no camera backend:
/// [`sheet_stage`] renders [`NO_CAMERA`] instead.
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
        // A session that cannot stream is a viewfinder with nothing behind
        // it — let it go rather than hold the device open behind an error state.
        spawn_blocking(move || session.close());
        state.camera_failed.set(true);
        return;
    }
    *state.session.lock().unwrap_or_else(PoisonError::into_inner) = Some(session);
}

/// Wires `on_detect` to `detections` and nothing else. `Ok(())` is accepted,
/// not confirmed, on iOS (see the [module docs](self)).
/// [`DetectionPolicy::NoDuplicates`] because a code sits in frame for as
/// long as it takes to focus; QR only, since `muxrctl` emits nothing else.
fn start_scan(session: &CameraSession, detections: &Arc<Detections>) -> Result<(), CameraError> {
    let detections = Arc::clone(detections);
    session.start_barcode_stream(
        BarcodeStreamOptions {
            formats: vec![BarcodeFormat::QrCode],
            detection: DetectionPolicy::NoDuplicates,
        },
        move |found: &[Barcode]| {
            // Never empty (the plugin's contract) — `first` is the
            // one-code-per-frame convenience, not a defensive check.
            if let Some(barcode) = found.first() {
                detections.publish(&barcode.raw_value);
            }
        },
    )
}

/// Gated like every other camera path, so a build with no backend never
/// calls [`use_interval`] at all. A `None` session on a tick is a no-op.
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
            // Same treatment `open_camera_once` gives a synchronous failure.
            *session.lock().unwrap_or_else(PoisonError::into_inner) = None;
            spawn_blocking(move || current.close());
            camera_failed.set(true);
        }
    });
}

/// Polls the detection cell on the UI thread, applies the first parseable
/// payload, and closes the sheet. Checks its cancel flag before every read,
/// so a sheet torn down between two wakes never touches the controller again.
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

/// The platform preview inside the same framed viewfinder the placeholder
/// draws, and the scan hint.
fn live_body(state: &QrSheetState, theme: &Theme) -> AnyView<QrSheetState> {
    let scheme = theme.scheme();
    let guard = state.session.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(session) = guard.as_ref() else {
        // Unreachable: `Stage::Live` is what a held session means. Rendering
        // the instruction beats an `expect` on a lock a rebuild could race.
        drop(guard);
        return message_body(theme, CAMERA_UNAVAILABLE, MessageTone::Failed, true);
    };
    // Target-gated by the session itself — never a shared literal.
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
        inflexible(any(Align(Alignment::CENTER, viewfinder))),
        inflexible(any(SizedBox::<QrSheetState>(None, Some(spacing::XL)))),
        inflexible(any(
            text(SHEET_TITLE).style(inked(&theme.type_scale.title_medium, scheme.on_surface))
        )),
        inflexible(any(SizedBox::<QrSheetState>(None, Some(spacing::XL / 2.0)))),
        inflexible(any(text(SCAN_HINT).style(inked(
            &theme.type_scale.body_small,
            scheme.on_surface_variant,
        )))),
    ];

    any(FlexView::new(Axis::Vertical, children).cross_axis(CrossAxisAlignment::Stretch))
}

/// The placeholder viewfinder over `message`, and a retry where one could help.
fn message_body(
    theme: &Theme,
    message: &str,
    tone: MessageTone,
    retryable: bool,
) -> AnyView<QrSheetState> {
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
        inflexible(any(Align(Alignment::CENTER, viewfinder))),
        inflexible(any(SizedBox::<QrSheetState>(None, Some(spacing::XL)))),
        inflexible(any(
            text(SHEET_TITLE).style(inked(&theme.type_scale.title_medium, scheme.on_surface))
        )),
        inflexible(any(SizedBox::<QrSheetState>(None, Some(spacing::XL / 2.0)))),
        inflexible(any(
            text(message).style(inked(&theme.type_scale.body_small, message_ink))
        )),
    ];
    if retryable {
        children.push(inflexible(any(SizedBox::<QrSheetState>(
            None,
            Some(spacing::XL),
        ))));
        children.push(inflexible(any(Align(
            Alignment::CENTER,
            button(RETRY_LABEL, retry_camera).style(ButtonStyle::Secondary),
        ))));
    }

    any(FlexView::new(Axis::Vertical, children).cross_axis(CrossAxisAlignment::Stretch))
}

/// Clears the failure and lets `build`'s own open branch reopen on the next
/// rebuild. Permission is not re-asked; it was confirmed before this sheet mounted.
fn retry_camera(state: &mut QrSheetState) {
    state.camera_failed.set(false);
    state.opened = false;
}

/// Opens the back camera, or refuses with no camera backend — this app's
/// product line, not the plugin's fail-soft.
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

    // The real deferred-failure path needs a real `CameraSession`, which no
    // host test target can construct. What headless testing can pin: the
    // error→stage mapping both failure paths share, and that the poller
    // never touches `use_interval` without a camera backend.

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
        // No ambient owner here on purpose: reaching `use_interval` would
        // spawn a task this bare process cannot stop, or panic with no runtime installed.
        assert!(!camera_backend_available());
        spawn_stream_error_poller(Arc::new(Mutex::new(None)), RwSignal::new(false));
    }
}
