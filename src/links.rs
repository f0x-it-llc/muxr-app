//! The app's one seam onto the device's external URL opener. A browser-out
//! link is [`open_external`], fire-and-forget; every headless suite swaps the
//! real launcher out through [`set_link_opener_for_tests`], so a tapped link
//! records rather than handing this machine's browser a URL.

use std::sync::{Arc, RwLock};

use frust_url_launcher::UrlLauncher;

type Opener = Arc<dyn Fn(&str) + Send + Sync>;

/// The process-static override; `None` routes to the real launcher. Same
/// shape as each feature's `use_mock_*_for_tests` seam in its `di.rs`.
static OPENER: RwLock<Option<Opener>> = RwLock::new(None);

/// Route every [`open_external`] call through `opener` for the rest of the
/// process — no test binary may reach a real browser by omission.
pub fn set_link_opener_for_tests(opener: impl Fn(&str) + Send + Sync + 'static) {
    *OPENER.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(opener));
}

/// Open `url` in the device's default external handler. Best-effort: a
/// failure (no browser installed, a scheme the launcher refuses) is logged,
/// never surfaced — the caller has already put the address in words.
pub fn open_external(url: &str) {
    let installed = OPENER.read().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(opener) = installed {
        opener(url);
        return;
    }
    if let Err(err) = UrlLauncher::open_external(url) {
        log::warn!("external link not opened: {err}");
    }
}
