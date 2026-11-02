//! The pinch **intent reducer**: frust's `ScaleEvent` stream (a two-finger touch pinch and a
//! desktop ctrl/cmd+wheel alike) becomes at most one [`PinchIntent`] per gesture.
//!
//! **Decision D1 (verbatim):** fingers SPREADING = zoom in = ENTER fullscreen; fingers
//! CONVERGING = zoom out = EXIT. Swap [`ENTER_SCALE`] and [`EXIT_SCALE`] to invert.
//!
//! The reducer is pure: it multiplies the per-event `scale_delta`s into a cumulative scale and
//! answers once when that crosses a threshold, so a slow drift and a quick flick behave the same
//! and a wobbling gesture never fires twice. Mapping the intent onto a pane is the screen's job.

use frust::ScalePhase;
use frust::authoring::Point;
use std::rc::Rc;

/// Cumulative scale at or above which a gesture enters fullscreen (fingers spreading).
pub const ENTER_SCALE: f64 = 1.25;

/// Cumulative scale at or below which a gesture exits fullscreen (fingers converging).
pub const EXIT_SCALE: f64 = 0.8;

/// What a completed threshold crossing asks the screen to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinchIntent {
    /// Fingers spread past [`ENTER_SCALE`].
    EnterFullscreen,
    /// Fingers converged past [`EXIT_SCALE`].
    ExitFullscreen,
}

/// Where an intent is reported: the intent and the gesture's focal in grid-local logical px.
pub type PinchSink = Rc<dyn Fn(PinchIntent, Point)>;

/// Folds one gesture's scale events into at most one [`PinchIntent`].
#[derive(Debug)]
pub struct PinchIntentTracker {
    /// Product of every `scale_delta` since the gesture began.
    cumulative: f64,
    /// Whether this gesture already answered.
    fired: bool,
    /// Whether a `Begin` opened the current gesture (a lone `Update` has none).
    begun: bool,
}

impl Default for PinchIntentTracker {
    fn default() -> Self {
        Self {
            cumulative: 1.0,
            fired: false,
            begun: false,
        }
    }
}

impl PinchIntentTracker {
    /// A tracker between gestures.
    pub fn new() -> Self {
        Self::default()
    }

    fn reset(&mut self) {
        self.cumulative = 1.0;
        self.fired = false;
    }

    /// Feed one event; `Some` exactly once per gesture, on the crossing `Update`.
    ///
    /// An `Update` with no `Begin` (a desktop wheel notch) is a one-event gesture of its own.
    pub fn feed(&mut self, phase: ScalePhase, scale_delta: f64) -> Option<PinchIntent> {
        match phase {
            ScalePhase::Begin => {
                self.reset();
                self.begun = true;
                None
            }
            ScalePhase::Update => {
                if !self.begun {
                    self.reset();
                }
                self.cumulative *= scale_delta;
                let intent = self.decide();
                if !self.begun {
                    self.reset();
                }
                intent
            }
            ScalePhase::End => {
                self.reset();
                self.begun = false;
                None
            }
        }
    }

    fn decide(&mut self) -> Option<PinchIntent> {
        if self.fired {
            return None;
        }
        let intent = if self.cumulative >= ENTER_SCALE {
            PinchIntent::EnterFullscreen
        } else if self.cumulative <= EXIT_SCALE {
            PinchIntent::ExitFullscreen
        } else {
            return None;
        };
        self.fired = true;
        Some(intent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gesture(deltas: &[f64]) -> Vec<Option<PinchIntent>> {
        let mut t = PinchIntentTracker::new();
        t.feed(ScalePhase::Begin, 1.0);
        deltas
            .iter()
            .map(|d| t.feed(ScalePhase::Update, *d))
            .collect()
    }

    #[test]
    fn enter_fires_once_at_the_threshold() {
        let out = gesture(&[1.1, 1.1, 1.2, 1.5]);
        assert_eq!(out, [None, None, Some(PinchIntent::EnterFullscreen), None]);
    }

    #[test]
    fn exit_fires_once_at_the_threshold() {
        let out = gesture(&[0.95, 0.9, 0.9]);
        assert_eq!(out, [None, None, Some(PinchIntent::ExitFullscreen)]);
    }

    #[test]
    fn an_oscillation_inside_the_band_fires_nothing() {
        assert!(
            gesture(&[0.95, 0.95, 1.1, 1.1, 1.05])
                .iter()
                .all(Option::is_none)
        );
    }

    #[test]
    fn end_resets_so_the_next_gesture_can_fire() {
        let mut t = PinchIntentTracker::new();
        t.feed(ScalePhase::Begin, 1.0);
        assert!(t.feed(ScalePhase::Update, 1.5).is_some());
        t.feed(ScalePhase::End, 1.0);
        t.feed(ScalePhase::Begin, 1.0);
        assert_eq!(
            t.feed(ScalePhase::Update, 1.5),
            Some(PinchIntent::EnterFullscreen)
        );
    }

    #[test]
    fn a_lone_update_is_a_one_event_gesture() {
        let mut t = PinchIntentTracker::new();
        assert_eq!(t.feed(ScalePhase::Update, 1.1), None);
        assert_eq!(t.feed(ScalePhase::Update, 1.1), None);
        assert_eq!(
            t.feed(ScalePhase::Update, 1.4),
            Some(PinchIntent::EnterFullscreen)
        );
        assert_eq!(
            t.feed(ScalePhase::Update, 0.7),
            Some(PinchIntent::ExitFullscreen)
        );
    }
}
