//! The app's one seam onto the device's external URL opener; tests swap it through
//! [`set_link_opener_for_tests`] so no suite hands a real browser a URL.

use std::sync::{Arc, RwLock};

use frust_url_launcher::UrlLauncher;

type Opener = Arc<dyn Fn(&str) + Send + Sync>;

static OPENER: RwLock<Option<Opener>> = RwLock::new(None);

pub fn set_link_opener_for_tests(opener: impl Fn(&str) + Send + Sync + 'static) {
    *OPENER.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(opener));
}

/// Best-effort: a failure is logged, never surfaced.
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
