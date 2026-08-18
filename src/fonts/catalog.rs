//! [`FontCatalog`] — the downloadable catalog's reactive state, and the
//! download state machine the picker drives.
//!
//! **A thread-local handle, not a context value** (same idiom as
//! `crate::ui::toasts::app_toasts`): one catalog per UI thread, self-healing
//! if its signals were disposed with a torn-down owner.
//!
//! **Threading.** [`FontCatalog::load`]/[`FontCatalog::install`] are `async`,
//! awaited from `spawn_local`; each hands the fetch to `frust::spawn`
//! (reqwest needs a live tokio context) and disk/hash/parse work to
//! `frust::spawn_blocking`, answering back over a `oneshot` so every signal
//! write lands on the UI thread.
//!
//! **Lifecycle.** The manifest fetches when the picker opens, never at
//! startup. A failed fetch/parse falls back to the cached copy
//! ([`FontCatalog::is_stale`]); with no cache either, status becomes
//! [`CatalogStatus::Unavailable`] with **no toast** — a missing catalog isn't
//! a user error, and installed fonts still list.
//!
//! **Per-font states:** `Available → Downloading → Installed | Failed`,
//! single-flight per family (a second tap while downloading no-ops, a failed
//! row's tap retries); a download only makes a font selectable, never selects
//! it.

use std::sync::Arc;

use frust::{Get, GetUntracked, RwSignal, Set};

use crate::failure::AppFailure;

use super::manifest::{self, FontEntry};
use super::{source, store};

/// Where the catalog itself stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogStatus {
    /// Before a picker has ever opened.
    Idle,
    /// A fetch is in flight.
    Loading,
    /// Entries available (see [`FontCatalog::is_stale`] for the offline-cache case).
    Ready,
    /// The fetch failed and nothing was cached.
    Unavailable,
}

/// Where one catalog entry stands for this device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontState {
    /// Offered, not downloaded.
    Available,
    /// Downloading, verifying, or registering right now.
    Downloading,
    /// Cached, registered, and selectable.
    Installed,
    /// The last attempt failed. A tap retries.
    Failed,
}

/// The app's font catalog handle — `Copy`, like every other signal-bundle
/// handle in this app (`ui::toasts::FailureToasts`,
/// `settings::presentation::scramble::RevokeAnimations`), so a sheet row, a
/// spawned task and the settings screen all act through the same state.
#[derive(Clone, Copy)]
pub struct FontCatalog {
    status: RwSignal<CatalogStatus>,
    /// Entries last resolved, in manifest order.
    entries: RwSignal<Arc<[FontEntry]>>,
    /// Whether [`Self::entries`] came from the offline cache after a failed
    /// fetch.
    stale: RwSignal<bool>,
    /// Transient per-family states; a family absent here falls back to the
    /// store (see [`FontCatalog::state_of`]).
    states: RwSignal<Arc<[(String, FontState)]>>,
    /// Live download fraction (`0.0..=1.0`) per family with a known size hint
    /// (see [`FontCatalog::progress_of_entry`]) — absent means not
    /// downloading, or downloading with no hint (indeterminate).
    progress: RwSignal<Arc<[(String, f64)]>>,
}

thread_local! {
    /// The one [`FontCatalog`] per UI thread — see [`font_catalog`].
    static CATALOG: std::cell::Cell<Option<FontCatalog>> = const { std::cell::Cell::new(None) };
}

/// The app's font catalog, created once per thread and reused afterwards.
///
/// Self-healing, like `ui::toasts::app_toasts`: a handle whose signals were
/// disposed with their owner (a headless test that dropped its ambient
/// `Owner`) reads back `None` and is replaced rather than handed out dead.
pub fn font_catalog() -> FontCatalog {
    CATALOG.with(|cell| {
        if let Some(catalog) = cell.get()
            && catalog.status.try_get_untracked().is_some()
        {
            return catalog;
        }
        let catalog = FontCatalog {
            status: RwSignal::new(CatalogStatus::Idle),
            entries: RwSignal::new(Arc::from(Vec::new())),
            stale: RwSignal::new(false),
            states: RwSignal::new(Arc::from(Vec::new())),
            progress: RwSignal::new(Arc::from(Vec::new())),
        };
        cell.set(Some(catalog));
        catalog
    })
}

impl FontCatalog {
    /// Tracked — a load landing wakes the sheet rendering it.
    pub fn status(&self) -> CatalogStatus {
        self.status.get()
    }

    /// The entries the catalog last resolved (tracked).
    pub fn entries(&self) -> Arc<[FontEntry]> {
        self.entries.get()
    }

    /// Untracked — for a press handler, which runs outside any rebuild and
    /// must not subscribe one (`docs/CODE_STANDARDS.md`'s tracked-read rule).
    pub fn entries_untracked(&self) -> Arc<[FontEntry]> {
        self.entries.get_untracked()
    }

    /// Whether those entries came from the offline cache (tracked).
    pub fn is_stale(&self) -> bool {
        self.stale.get()
    }

    /// Where `entry` stands on this device (tracked), resolved through its
    /// **effective** family — what it actually registered as, if that
    /// differs from the manifest's claim (see [`super::store`]) — so a row
    /// can't claim "available" while already installed under another name.
    pub fn state_of(&self, entry: &FontEntry) -> FontState {
        let family = effective_family(entry);
        if let Some((_, state)) = self.states.get().iter().find(|(known, _)| *known == family) {
            return *state;
        }
        if store::is_downloaded(&family) {
            FontState::Installed
        } else {
            FontState::Available
        }
    }

    /// Live download fraction for `family` (tracked); `None` if nothing is
    /// downloading under that name, or its entry carries no size hint.
    fn progress_of(&self, family: &str) -> Option<f64> {
        self.progress
            .get()
            .iter()
            .find(|(known, _)| known == family)
            .map(|(_, fraction)| *fraction)
    }

    /// [`Self::progress_of`] through `entry`'s effective family — the same
    /// key [`Self::state_of`]/[`Self::install`] use. `None` renders the
    /// indeterminate dots loader, `Some` the determinate bar.
    pub fn progress_of_entry(&self, entry: &FontEntry) -> Option<f64> {
        self.progress_of(&effective_family(entry))
    }

    /// Fetch the catalog unless a fetch is in flight or one already landed —
    /// the picker calls this on open.
    ///
    /// [`CatalogStatus::Unavailable`] is never cached as a verdict:
    /// re-opening retries. Never fails outward (see the [module docs](self)).
    pub async fn load(&self) {
        match self.status.get_untracked() {
            CatalogStatus::Loading | CatalogStatus::Ready => return,
            CatalogStatus::Idle | CatalogStatus::Unavailable => {}
        }
        self.status.set(CatalogStatus::Loading);
        match run_off_thread(fetch_catalog()).await {
            Ok((entries, stale)) => {
                self.entries.set(Arc::from(entries));
                self.stale.set(stale);
                self.status.set(CatalogStatus::Ready);
            }
            Err(failure) => {
                log::warn!("fonts: font catalog unavailable: {failure}");
                self.status.set(CatalogStatus::Unavailable);
            }
        }
    }

    /// Download, verify, cache and register `entry`.
    ///
    /// Single-flight per family: a call for a family already downloading or
    /// installed answers `Ok(())` without starting a second one. The row's
    /// state carries the outcome; the returned failure is only for the
    /// caller's own toast.
    ///
    /// While in flight, `received / entry.total_bytes()` publishes for
    /// [`Self::progress_of_entry`] (see [`Self::spawn_progress_pump`]); a
    /// zero size hint never publishes one, leaving it indeterminate.
    pub async fn install(&self, entry: FontEntry) -> Result<(), AppFailure> {
        let family = effective_family(&entry);
        match self.state_of_untracked(&family) {
            FontState::Downloading | FontState::Installed => return Ok(()),
            FontState::Available | FontState::Failed => {}
        }
        self.set_state(&family, FontState::Downloading);
        // A stale fraction from a previous failed attempt must not flash
        // before this one's first real chunk lands.
        self.clear_progress(&family);

        let total = entry.total_bytes();
        let (progress_tx, progress_rx) = tokio::sync::mpsc::unbounded_channel::<u64>();
        if total > 0 {
            self.spawn_progress_pump(family.clone(), total, progress_rx);
        }

        let outcome = run_off_thread(download(entry, progress_tx)).await;
        self.clear_progress(&family);
        match outcome {
            Ok(registered) => {
                // The family it registered as may differ from the manifest's
                // claim; record the state under both so the row flips either
                // way.
                self.set_state(&family, FontState::Installed);
                if registered != family {
                    self.set_state(&registered, FontState::Installed);
                }
                Ok(())
            }
            Err(failure) => {
                self.set_state(&family, FontState::Failed);
                Err(failure)
            }
        }
    }

    /// [`Self::state_of`] by family, without tracking — for the guards above,
    /// which run outside any rebuild.
    fn state_of_untracked(&self, family: &str) -> FontState {
        if let Some((_, state)) = self
            .states
            .get_untracked()
            .iter()
            .find(|(known, _)| known == family)
        {
            return *state;
        }
        if store::is_downloaded(family) {
            FontState::Installed
        } else {
            FontState::Available
        }
    }

    /// Publish `state` for `family`, replacing any state it already had.
    fn set_state(&self, family: &str, state: FontState) {
        let mut states: Vec<(String, FontState)> = self
            .states
            .get_untracked()
            .iter()
            .filter(|(known, _)| known != family)
            .cloned()
            .collect();
        states.push((family.to_string(), state));
        self.states.set(Arc::from(states));
    }

    /// Drain `receiver` on the UI thread, publishing a throttled
    /// whole-percent fraction for `family` ([`Self::publish_progress`]
    /// itself refuses non-new values). Ends once the sender (`download`'s
    /// per-file callback) drops — its own spawned task, not a child of
    /// [`Self::install`]'s future, so it outlives a torn-down caller.
    fn spawn_progress_pump(
        self,
        family: String,
        total: u64,
        mut receiver: tokio::sync::mpsc::UnboundedReceiver<u64>,
    ) {
        frust::spawn_local(async move {
            let mut last_percent: Option<u64> = None;
            while let Some(received) = receiver.recv().await {
                let fraction = (received as f64 / total as f64).clamp(0.0, 1.0);
                let percent = (fraction * 100.0) as u64;
                if last_percent == Some(percent) {
                    continue;
                }
                last_percent = Some(percent);
                self.publish_progress(&family, fraction);
            }
        });
    }

    /// Publish `fraction` (clamped `0.0..=1.0`) for `family`, never
    /// backward — the manifest's size hint is advisory, so a fraction no
    /// higher than what's already published is dropped.
    fn publish_progress(&self, family: &str, fraction: f64) {
        let fraction = fraction.clamp(0.0, 1.0);
        let current = self.progress.get_untracked();
        if current
            .iter()
            .any(|(known, prev)| known == family && *prev >= fraction)
        {
            return;
        }
        let mut next: Vec<(String, f64)> = current
            .iter()
            .filter(|(known, _)| known != family)
            .cloned()
            .collect();
        next.push((family.to_string(), fraction));
        self.progress.set(Arc::from(next));
    }

    /// Drop `family`'s published fraction, if any.
    fn clear_progress(&self, family: &str) {
        let current = self.progress.get_untracked();
        if !current.iter().any(|(known, _)| known == family) {
            return;
        }
        let next: Vec<(String, f64)> = current
            .iter()
            .filter(|(known, _)| known != family)
            .cloned()
            .collect();
        self.progress.set(Arc::from(next));
    }
}

/// The family `entry` resolves to on this device: the one it actually
/// registered as when that differed from the manifest's claim, else the claim
/// itself.
fn effective_family(entry: &FontEntry) -> String {
    store::registered_family_for(&entry.family).unwrap_or_else(|| entry.family.clone())
}

/// Run `work` on the background runtime and await its answer here.
///
/// `frust::spawn` is required, not just sanctioned, for the fetches below:
/// reqwest's connector spawns onto the tokio runtime, so its futures must be
/// polled inside one; the `oneshot` carries the answer back to the awaiting
/// UI thread since `frust::spawn` itself returns nothing.
async fn run_off_thread<T: Send + 'static>(
    work: impl std::future::Future<Output = Result<T, AppFailure>> + Send + 'static,
) -> Result<T, AppFailure> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    frust::spawn(async move {
        let _ = tx.send(work.await);
    });
    rx.await.unwrap_or_else(|_| {
        Err(AppFailure::Internal(
            "font task ended without answering".to_string(),
        ))
    })
}

/// Fetch + parse the catalog, falling back to the cached copy — the `bool` is
/// whether that fallback is what answered.
async fn fetch_catalog() -> Result<(Vec<FontEntry>, bool), AppFailure> {
    let fetched = source::font_source().fetch_manifest().await;
    // Parse/cache-write/offline-read are blocking/CPU work: the blocking
    // pool, not the async worker.
    frust::spawn_blocking(move || match fetched {
        Ok(bytes) => match manifest::parse_manifest(&bytes) {
            Ok(entries) => {
                store::write_cached_manifest(&bytes);
                Ok((entries, false))
            }
            Err(failure) => cached_catalog_or(failure),
        },
        Err(failure) => cached_catalog_or(failure),
    })
    .await
    .map_err(|err| AppFailure::Internal(format!("font catalog worker: {err}")))?
}

/// The cached catalog if there is a usable one, else `failure` — the offline
/// grace every fetch/parse error lands in.
fn cached_catalog_or(failure: AppFailure) -> Result<(Vec<FontEntry>, bool), AppFailure> {
    match store::read_cached_manifest()
        .as_deref()
        .map(manifest::parse_manifest)
    {
        Some(Ok(entries)) => Ok((entries, true)),
        _ => Err(failure),
    }
}

/// Download every file of `entry`, then verify/cache/register the lot;
/// answers the family it registered as.
///
/// `progress` receives cumulative bytes **across every file**, in manifest
/// order — [`FontCatalog::install`]'s pump divides this by
/// `entry.total_bytes()`, so a later file picks up where the previous one
/// left off rather than resetting toward `0`.
async fn download(
    entry: FontEntry,
    progress: tokio::sync::mpsc::UnboundedSender<u64>,
) -> Result<String, AppFailure> {
    let source = source::font_source();
    let mut files = Vec::with_capacity(entry.files.len());
    let mut completed_bytes: u64 = 0;
    for file in &entry.files {
        let base = completed_bytes;
        let tx = progress.clone();
        let mut report = move |received: u64| {
            let _ = tx.send(base + received);
        };
        let bytes = source
            .fetch_font_with_progress(&file.url, &mut report)
            .await?;
        completed_bytes += bytes.len() as u64;
        files.push((file.clone(), bytes));
    }
    frust::spawn_blocking(move || store::install_downloaded(&entry, files))
        .await
        .map_err(|err| AppFailure::Internal(format!("font install worker: {err}")))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fonts::manifest::FontFile;

    fn entry(family: &str) -> FontEntry {
        FontEntry {
            id: "fixture-nf".to_string(),
            display_name: format!("{family} Nerd Font"),
            family: family.to_string(),
            license: "OFL-1.1".to_string(),
            nerd_font: true,
            files: vec![FontFile {
                name: "Fixture-Regular.ttf".to_string(),
                url: "https://example.invalid/Fixture-Regular.ttf".to_string(),
                sha256: "0".repeat(64),
                bytes: 1024,
            }],
        }
    }

    /// A catalog with fresh, empty signals — every unit test below's starting
    /// point.
    fn test_catalog() -> FontCatalog {
        FontCatalog {
            status: RwSignal::new(CatalogStatus::Idle),
            entries: RwSignal::new(Arc::from(Vec::new())),
            stale: RwSignal::new(false),
            states: RwSignal::new(Arc::from(Vec::new())),
            progress: RwSignal::new(Arc::from(Vec::new())),
        }
    }

    #[test]
    fn an_untouched_entry_reads_as_available() {
        // `state_of_untracked`'s store-backed fallback — what a fresh row
        // resolves through.
        let catalog = test_catalog();
        assert_eq!(
            catalog.state_of_untracked("Nothing Installed Mono"),
            FontState::Available
        );
    }

    #[test]
    fn a_published_state_wins_over_the_store_fallback() {
        let catalog = test_catalog();
        catalog.set_state("Fixture Mono", FontState::Downloading);
        assert_eq!(
            catalog.state_of_untracked("Fixture Mono"),
            FontState::Downloading
        );
        // …and re-publishing replaces rather than duplicates.
        catalog.set_state("Fixture Mono", FontState::Failed);
        assert_eq!(catalog.states.get_untracked().len(), 1);
        assert_eq!(
            catalog.state_of_untracked("Fixture Mono"),
            FontState::Failed
        );
    }

    #[test]
    fn an_entry_with_no_alias_resolves_to_its_own_manifest_family() {
        assert_eq!(effective_family(&entry("Unaliased Mono")), "Unaliased Mono");
    }

    // -- progress: publish/clear, independent of the async download path ---

    #[test]
    fn progress_publishes_monotonically_clamped_and_never_regresses() {
        let catalog = test_catalog();
        assert_eq!(catalog.progress_of("Fixture Mono"), None);

        catalog.publish_progress("Fixture Mono", 0.25);
        assert_eq!(catalog.progress_of("Fixture Mono"), Some(0.25));

        // Lower than published is dropped — advisory size hints must never
        // read back as a regression.
        catalog.publish_progress("Fixture Mono", 0.10);
        assert_eq!(catalog.progress_of("Fixture Mono"), Some(0.25));

        catalog.publish_progress("Fixture Mono", 0.9);
        assert_eq!(catalog.progress_of("Fixture Mono"), Some(0.9));

        // Clamped to 1.0 even given an over-counted total.
        catalog.publish_progress("Fixture Mono", 1.5);
        assert_eq!(catalog.progress_of("Fixture Mono"), Some(1.0));

        catalog.clear_progress("Fixture Mono");
        assert_eq!(catalog.progress_of("Fixture Mono"), None);
    }

    #[test]
    fn progress_of_entry_resolves_through_the_effective_family() {
        let catalog = test_catalog();
        let entry = entry("Claimed Mono");
        // Nothing published yet, under either name.
        assert_eq!(catalog.progress_of_entry(&entry), None);
        catalog.publish_progress("Claimed Mono", 0.4);
        assert_eq!(catalog.progress_of_entry(&entry), Some(0.4));
    }
}
