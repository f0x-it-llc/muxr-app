//! [`GreetingController`] — the root screen's view model (huddle
//! clean-architecture refactor's `--arch clean-signals` scaffold).
//!
//! Mirrors `examples/huddle`'s controller shape: it embeds a
//! [`ControllerCore`] by composition and owns the one signal the page
//! renders — an [`AsyncState`] over the loaded greeting.
//! [`LoadGreeting`] is an async [`UseCase`](clean_signals::UseCase) that
//! loads it through the injected [`GreetingRepository`]; it **loads
//! cleanly** (one attempt, never fails), so no retry vocabulary is
//! exercised here.

use std::sync::Arc;

use clean_signals::{AsyncState, ControllerCore, NoParams, RunOptions, async_state_signal};
use frust::RwSignal;

use crate::failure::AppFailure;
use crate::features::greeting::domain::repositories::GreetingRepository;
use crate::features::greeting::domain::use_cases::LoadGreeting;

/// View model for the root screen.
pub struct GreetingController {
    core: ControllerCore<AppFailure>,
    load_greeting: LoadGreeting,
    /// The greeting's async load state — `Loading` drives a placeholder,
    /// `Data(greeting)` the resolved text, `Error` a failure message (see
    /// `presentation::pages::greeting_page`).
    pub greeting: RwSignal<AsyncState<String, AppFailure>>,
}

impl GreetingController {
    /// A fresh controller with an empty (`Loading`) signal, wired to the
    /// injected [`GreetingRepository`] (the composition root builds it once
    /// and passes it in as a prop — see `crate`'s root `Component::init`).
    pub fn new(repo: Arc<dyn GreetingRepository + Send + Sync>) -> Self {
        Self {
            core: ControllerCore::new(),
            load_greeting: LoadGreeting::new(repo),
            greeting: async_state_signal(),
        }
    }

    /// Runs [`LoadGreeting`] into [`Self::greeting`]. Called once from
    /// `presentation::pages::init_controller` (and again on a "Reload"
    /// tap).
    pub async fn load(&self) {
        let _ = self
            .core
            .run_into(
                &self.load_greeting,
                NoParams,
                self.greeting,
                RunOptions::default(),
            )
            .await;
    }
}

// The `use_controller`/`ControllerCore::run_into` seam needs this: a
// controller shares its `ControllerCore` by `AsRef` rather than by field
// access, so the helpers stay generic over any controller shape.
impl AsRef<ControllerCore<AppFailure>> for GreetingController {
    fn as_ref(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }
}