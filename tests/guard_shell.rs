//! The session guard at the **shell boundary**: what happens to the scaffold
//! page when a navigation resolves out of the connected subtree.
//!
//! `tests/routes.rs` owns the guard as a pure redirect (which path resolves to
//! which) and `tests/navigation_motion.rs` owns the bar; this suite owns the
//! outer page stack across that boundary — placement, unwind, re-placement —
//! because the defect it pins shut is a *stack shape*: a bounce that pushed
//! Connect over a live shell page left the authenticated inner navigator
//! retained underneath it, reachable again by the next in-shell navigation and
//! by a platform back press, neither of which re-runs the guard.
//!
//! Depth here is the **outer** navigator's: `1` is Connect alone, `2` is the
//! shell page over it. In-shell navigation never moves it (`navigation_motion`'s
//! own note), so a depth of 2 is exactly "the scaffold is on the stack".

use std::cell::Cell;
use std::rc::Rc;
use std::sync::OnceLock;

use frust::{AnyView, Component, GetUntracked};
use frust_core::RenderRoot;
use frust_core::accesskit::Role;
use frust_text::TextContext;

use muxr::connection::{ConnectTarget, TrustMode, expect_connection_controller};
use muxr::routes::{app_nav, sessions_back_signal};
use muxr::{AppState, MuxrApp};

mod support;
use support::{node_labels, node_roles, node_values, serial, settle, setup};

const HOST: &str = "mock.example";
const PORT: u16 = 7777;
/// The bar's brand wordmark and its back affordance's accessible name —
/// transcribed rather than imported, like `tests/navigation_motion.rs` does.
const BRAND: &str = "muxr";
const BACK: &str = "Back";

fn mock_target() -> ConnectTarget {
    ConnectTarget {
        host: HOST.to_string(),
        port: PORT,
        trust_mode: TrustMode::System,
        label: "Mock rig".to_string(),
        offer_credential: false,
        token: zeroize::Zeroizing::new(String::new()),
        remember: false,
        fingerprint: None,
    }
}

/// Burn this process's one cold-start splash claim, so no test mounts under it.
fn spend_cold_start() {
    static SPENT: OnceLock<()> = OnceLock::new();
    SPENT.get_or_init(|| {
        let mut throwaway = MuxrApp.init();
        let _ = MuxrApp.build(&mut throwaway);
    });
}

type Logic = Box<dyn FnMut(&mut AppState) -> AnyView<AppState>>;

/// The whole app, warm-started under the Glyph theme.
struct Harness {
    root: RenderRoot<AppState, AnyView<AppState>>,
    logic: Logic,
    state: AppState,
    tcx: TextContext,
}

impl Harness {
    fn mount() -> Self {
        spend_cold_start();
        let mut root: RenderRoot<AppState, AnyView<AppState>> = RenderRoot::new();
        root.set_theme(Box::new(frust_glyph::baseline()));
        let mut harness = Harness {
            root,
            logic: Box::new(|state: &mut AppState| MuxrApp.build(state)),
            state: MuxrApp.init(),
            tcx: TextContext::new(),
        };
        harness.settle();
        harness
    }

    /// Mount, connect (mock), and land on `/sessions` the way Connect does.
    fn connected() -> Self {
        let mut harness = Harness::mount();
        harness.connect();
        harness.push("/sessions");
        harness
    }

    fn connect(&mut self) {
        expect_connection_controller().connect(mock_target());
        assert!(connected(), "the mock connection settles synchronously");
    }

    fn settle(&mut self) {
        settle(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
        );
    }

    fn push(&mut self, location: &str) {
        app_nav().push(location);
        self.settle();
    }

    fn go(&mut self, location: &str) {
        app_nav().go(location);
        self.settle();
    }

    fn depth(&self) -> usize {
        self.state.nav.controller().depth()
    }

    fn has_bar(&self) -> bool {
        node_roles(&self.root).contains(&Role::TitleBar)
    }

    fn labels(&self) -> Vec<String> {
        node_labels(&self.root)
    }

    fn values(&self) -> Vec<String> {
        node_values(&self.root)
    }
}

fn connected() -> bool {
    expect_connection_controller().connected.get_untracked()
}

/// One platform back press, settled.
fn press_back(h: &mut Harness) {
    frust::push_back_press();
    h.settle();
}

/// Assert the app is on Connect with nothing of the shell left on the stack.
fn assert_unwound(h: &Harness) {
    assert_eq!(
        h.depth(),
        1,
        "the shell page is off the outer stack, not merely covered"
    );
    assert!(
        !h.has_bar(),
        "…so the bar-less Connect route is what is on screen: {:?}",
        node_roles(&h.root)
    );
}

/// Assert the shell is placed and showing its landing route's face — the brand,
/// the connected host, and no back affordance (a fresh inner navigator, one
/// page deep).
fn assert_landed_fresh(h: &Harness) {
    assert_eq!(h.depth(), 2, "the shell page is on the outer stack");
    assert!(h.has_bar(), "…with its bar: {:?}", node_roles(&h.root));
    assert!(
        h.labels().iter().any(|l| l == HOST),
        "…titled with the connected host: {:?}",
        h.labels()
    );
    assert!(
        h.values().iter().any(|v| v == BRAND),
        "…wearing the brand: {:?}",
        h.values()
    );
    assert!(
        !h.labels().iter().any(|l| l == BACK),
        "…and no back affordance, so the inner navigator is one page deep: {:?}",
        h.labels()
    );
}

#[test]
fn a_navigation_after_the_session_expires_unwinds_the_shell_instead_of_covering_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    h.push("/terminal?session=s-1");
    assert_eq!(h.depth(), 2, "an in-shell push moves no outer page");

    // The server revoked the token (or restarted): the controller tears the
    // connection down where it stands, and nothing navigates — the guard fires
    // at navigation time, so the stale screen is still up.
    expect_connection_controller().on_session_expired();
    h.settle();
    assert!(!connected());
    assert_eq!(h.depth(), 2, "…with the shell page still on the stack");

    // The user taps anything that navigates. The bounce must remove the shell,
    // not stack Connect over it.
    h.push("/terminal?session=s-1");

    assert_unwound(&h);
}

#[test]
fn the_unwound_shell_is_not_reachable_by_a_back_press() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    h.push("/settings");
    expect_connection_controller().on_session_expired();
    h.settle();

    h.push("/sessions");
    assert_unwound(&h);

    // A back press is not a router navigation, so nothing re-runs the guard: if
    // the shell were merely covered, this is the press that would step back
    // into the authenticated subtree.
    press_back(&mut h);

    assert_unwound(&h);
    assert!(!connected(), "…and nothing re-authenticated on the way");
}

#[test]
fn reconnecting_after_an_unwind_places_a_fresh_shell_with_its_own_dismiss_cell() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    // Two pages deep in the inner navigator, so a retained one would show.
    h.push("/terminal?session=s-1");
    let before = sessions_back_signal();

    expect_connection_controller().on_session_expired();
    h.settle();
    h.push("/sessions");
    assert_unwound(&h);

    h.connect();
    h.push("/sessions");

    assert_landed_fresh(&h);
    let after = sessions_back_signal();
    assert!(
        !Rc::ptr_eq(&before, &after),
        "the placement minted its own dismiss cell"
    );
    assert_eq!(after.get(), 0, "…starting unbumped");

    // The bar is live, and so is the seam behind it: a back press at the
    // landing route asks rather than pops.
    press_back(&mut h);
    assert_eq!(
        after.get(),
        1,
        "the back press bumped this placement's cell"
    );
    assert_eq!(h.depth(), 2, "…and popped nothing");
    assert!(connected(), "…nor disconnected anything");
}

#[test]
fn a_go_into_the_shell_carries_the_dismiss_cell_a_push_would() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    h.connect();
    let before = sessions_back_signal();

    // The latent cold-link case: a `go` straight into the connected subtree
    // places the scaffold itself, so it has to carry the back contract a push
    // places it with.
    h.go("/sessions");

    assert_landed_fresh(&h);
    let after = sessions_back_signal();
    assert!(
        !Rc::ptr_eq(&before, &after),
        "a placement, not a bare replace"
    );

    press_back(&mut h);

    assert_eq!(after.get(), 1, "a back press asks the page");
    assert_eq!(
        h.depth(),
        2,
        "…rather than popping the shell out from under it"
    );
    assert!(connected());
}

#[test]
fn an_unauthenticated_settings_navigation_never_places_the_shell() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    assert!(!connected());

    // `/settings` lives inside the shell now, so resolving it while
    // disconnected would otherwise place the whole authenticated scaffold.
    h.push("/settings");

    assert_eq!(h.depth(), 1, "the bounce left the stack alone");
    assert!(
        !h.has_bar(),
        "…and placed no scaffold: {:?}",
        node_roles(&h.root)
    );
}

#[test]
fn a_go_out_of_the_shell_unwinds_it_rather_than_stacking_connect_on_top() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();

    // Settings' disconnect: the controller clears the connection and the
    // screen `go`es home. A replace would have swapped the shell page for a
    // second Connect and left the first one under it.
    expect_connection_controller().disconnect();
    h.go("/");

    assert_unwound(&h);
}

/// The dismiss cell each placement mints — asserted here as an `Rc` identity,
/// which is the only observable a covered-vs-torn-down shell differs on from
/// outside the navigator.
#[test]
fn each_placement_publishes_a_cell_no_earlier_one_shares() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    let first: Rc<Cell<u64>> = sessions_back_signal();

    expect_connection_controller().on_session_expired();
    h.settle();
    h.push("/sessions");
    h.connect();
    h.push("/sessions");
    let second = sessions_back_signal();

    expect_connection_controller().on_session_expired();
    h.settle();
    h.push("/sessions");
    h.connect();
    h.push("/sessions");
    let third = sessions_back_signal();

    assert!(!Rc::ptr_eq(&first, &second));
    assert!(!Rc::ptr_eq(&second, &third));
    assert_eq!(third.get(), 0);
}
