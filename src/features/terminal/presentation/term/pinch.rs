//! Reduces frust's `ScaleEvent` stream to at most one [`PinchIntent`] per gesture: the
//! `scale_delta`s multiply into a cumulative scale that answers once on crossing a threshold.
//! A lone wheel notch (scale 1.105, ctrl+wheel arrives as exp(0.1×lines)) crosses none.

use frust::ScalePhase;
use frust::authoring::Point;
use std::rc::Rc;

pub const ENTER_SCALE: f64 = 1.25;

pub const EXIT_SCALE: f64 = 0.8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinchIntent {
    EnterFullscreen,
    ExitFullscreen,
}

/// The intent and the gesture's focal point in grid-local logical px.
pub type PinchSink = Rc<dyn Fn(PinchIntent, Point)>;

#[derive(Debug)]
pub struct PinchIntentTracker {
    cumulative: f64,
    fired: bool,
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
    pub fn new() -> Self {
        Self::default()
    }

    fn reset(&mut self) {
        self.cumulative = 1.0;
        self.fired = false;
    }

    /// `Some` at most once per gesture; an `Update` with no `Begin` is a one-event gesture.
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
