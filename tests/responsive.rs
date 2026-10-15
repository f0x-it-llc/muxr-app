//! Proves a re-provided `WindowMetrics` flips a build-time structure without a probe: a bare
//! `Component` reads `Responsive::of(use_context::<WindowMetrics>())` in `build` and renders
//! "tablet" or "compact". The seam is `support::{metrics_at, mount_with_metrics,
//! reprovide_metrics}`; the frust facts it rests on are in `support`'s module doc.
//!
//! Each frame the harness runs rebuilds the component exactly once, so a resize is visible on
//! the very next frame (the shell's warmup window allows three) and a same-size re-provide adds
//! only the builds of the frames pumped after it.

use std::cell::Cell;
use std::rc::Rc;

use frust::{Component, ComponentView, View, any, component, text, use_context};
use frust_core::{RenderRoot, WindowMetrics};
use frust_text::TextContext;
use muxr::responsive::Responsive;

mod support;
use support::{
    frame, metrics_at, mount_with_metrics, node_values, reprovide_metrics, serial, setup,
};

/// Frames a resize may take to show, the frame gate's `WARMUP_FRAMES`.
const WARMUP_FRAMES: usize = 3;

/// Renders which structure the current metrics select and counts its own builds.
struct Probe {
    builds: Rc<Cell<usize>>,
}

impl Component for Probe {
    type State = ();

    fn init(&self) {}

    fn build(&self, _state: &mut ()) -> impl View<()> {
        self.builds.set(self.builds.get() + 1);
        let rule = Responsive::of(use_context::<WindowMetrics>());
        any(text(if rule.is_tablet() {
            "tablet"
        } else {
            "compact"
        }))
    }
}

/// The boxed app-logic closure a frame drives.
type Logic = Box<dyn FnMut(&mut ()) -> ComponentView<Probe>>;

/// The mounted probe, its frame plumbing and its build counter.
struct Fixture {
    root: RenderRoot<(), ComponentView<Probe>>,
    logic: Logic,
    tcx: TextContext,
    builds: Rc<Cell<usize>>,
}

impl Fixture {
    fn mount(width: f64, height: f64) -> Self {
        let builds = Rc::new(Cell::new(0));
        let counter = Rc::clone(&builds);
        let logic: Logic = Box::new(move |_: &mut ()| {
            component(Probe {
                builds: Rc::clone(&counter),
            })
        });
        mount_with_metrics(Some(metrics_at(width, height)), || Fixture {
            root: RenderRoot::new(),
            logic,
            tcx: TextContext::new(),
            builds,
        })
    }

    fn frame(&mut self) {
        frame(&mut self.root, &mut self.logic, &mut (), &mut self.tcx);
    }

    fn shows(&self, word: &str) -> bool {
        node_values(&self.root).iter().any(|v| v == word)
    }

    /// Re-provide `width`×`height`, then pump frames until `word` shows, at most
    /// [`WARMUP_FRAMES`]; returns how many it took.
    fn resize_to(&mut self, width: f64, height: f64, word: &str) -> usize {
        reprovide_metrics(metrics_at(width, height));
        for taken in 1..=WARMUP_FRAMES {
            self.frame();
            if self.shows(word) {
                return taken;
            }
        }
        panic!("{word:?} not shown within {WARMUP_FRAMES} frames");
    }
}

#[test]
fn a_reprovided_size_flips_the_structure_within_three_frames() {
    let _g = serial();
    let _ambient = setup();
    let mut fx = Fixture::mount(390.0, 844.0);
    fx.frame();
    assert!(fx.shows("compact"));
    assert!(!fx.shows("tablet"));

    fx.resize_to(1024.0, 720.0, "tablet");
    assert!(!fx.shows("compact"));

    fx.resize_to(390.0, 844.0, "compact");
    assert!(!fx.shows("tablet"));
}

#[test]
fn a_same_size_reprovide_adds_only_the_pumped_frames_builds() {
    let _g = serial();
    let _ambient = setup();
    let mut fx = Fixture::mount(390.0, 844.0);
    fx.frame();
    let before = fx.builds.get();

    reprovide_metrics(metrics_at(390.0, 844.0));
    assert_eq!(fx.builds.get(), before, "providing context builds nothing");
    for _ in 0..WARMUP_FRAMES {
        fx.frame();
    }
    assert_eq!(
        fx.builds.get(),
        before + WARMUP_FRAMES,
        "one build per pumped frame, none from the re-provide"
    );
    assert!(fx.shows("compact"));
}
