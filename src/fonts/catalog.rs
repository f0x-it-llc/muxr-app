//! [`FontCatalog`]: the downloadable catalog's reactive state and the picker's download state machine.
//! A failed fetch/parse falls back to the cached copy; with none, status is [`CatalogStatus::Unavailable`], with no toast.
//! Downloads are single-flight per family (`Available -> Downloading -> Installed | Failed`) and never select the font.

use std::sync::Arc;

use frust::{Get, GetUntracked, RwSignal, Set};

use crate::failure::AppFailure;

use super::manifest::{self, FontEntry};
use super::{source, store};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogStatus {
    Idle,
    Loading,
    Ready,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontState {
    Available,
    Downloading,
    Installed,
    Failed,
}

#[derive(Clone, Copy)]
pub struct FontCatalog {
    status: RwSignal<CatalogStatus>,
    entries: RwSignal<Arc<[FontEntry]>>,
    stale: RwSignal<bool>,
    states: RwSignal<Arc<[(String, FontState)]>>,
    progress: RwSignal<Arc<[(String, f64)]>>,
}

thread_local! {
    static CATALOG: std::cell::Cell<Option<FontCatalog>> = const { std::cell::Cell::new(None) };
}

/// Created once per thread; replaced if its signals were disposed with a torn-down owner.
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
    pub fn status(&self) -> CatalogStatus {
        self.status.get()
    }

    pub fn entries(&self) -> Arc<[FontEntry]> {
        self.entries.get()
    }

    /// Untracked, for press handlers that run outside any rebuild.
    pub fn entries_untracked(&self) -> Arc<[FontEntry]> {
        self.entries.get_untracked()
    }

    pub fn is_stale(&self) -> bool {
        self.stale.get()
    }

    /// Resolved through the effective family, so a row cannot read "available" while installed under another name.
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

    fn progress_of(&self, family: &str) -> Option<f64> {
        self.progress
            .get()
            .iter()
            .find(|(known, _)| known == family)
            .map(|(_, fraction)| *fraction)
    }

    /// `None` renders the indeterminate loader, `Some` the determinate bar.
    pub fn progress_of_entry(&self, entry: &FontEntry) -> Option<f64> {
        self.progress_of(&effective_family(entry))
    }

    /// [`CatalogStatus::Unavailable`] is never cached as a verdict: re-opening retries.
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

    /// Single-flight per family: a family already downloading or installed answers `Ok(())`.
    /// A zero size hint never publishes progress, leaving it indeterminate.
    pub async fn install(&self, entry: FontEntry) -> Result<(), AppFailure> {
        let family = effective_family(&entry);
        match self.state_of_untracked(&family) {
            FontState::Downloading | FontState::Installed => return Ok(()),
            FontState::Available | FontState::Failed => {}
        }
        self.set_state(&family, FontState::Downloading);
        // Clears a stale fraction left by a failed attempt.
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
                // The registered family may differ from the manifest's claim; record both so the row flips either way.
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

    /// Drains `receiver` on the UI thread into throttled whole-percent fractions; its own task, so it outlives a torn-down caller.
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

    /// Never moves backward: the manifest's size hint is advisory.
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

fn effective_family(entry: &FontEntry) -> String {
    store::registered_family_for(&entry.family).unwrap_or_else(|| entry.family.clone())
}

/// Runs `work` on the background runtime (reqwest needs a live tokio context) and awaits its answer over a `oneshot`.
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

async fn fetch_catalog() -> Result<(Vec<FontEntry>, bool), AppFailure> {
    let fetched = source::font_source().fetch_manifest().await;
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

fn cached_catalog_or(failure: AppFailure) -> Result<(Vec<FontEntry>, bool), AppFailure> {
    match store::read_cached_manifest()
        .as_deref()
        .map(manifest::parse_manifest)
    {
        Some(Ok(entries)) => Ok((entries, true)),
        _ => Err(failure),
    }
}

/// `progress` receives cumulative bytes across every file, so a later file continues where the previous one left off.
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

    #[test]
    fn progress_publishes_monotonically_clamped_and_never_regresses() {
        let catalog = test_catalog();
        assert_eq!(catalog.progress_of("Fixture Mono"), None);

        catalog.publish_progress("Fixture Mono", 0.25);
        assert_eq!(catalog.progress_of("Fixture Mono"), Some(0.25));

        // Advisory size hints must never read back as a regression.
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
        assert_eq!(catalog.progress_of_entry(&entry), None);
        catalog.publish_progress("Claimed Mono", 0.4);
        assert_eq!(catalog.progress_of_entry(&entry), Some(0.4));
    }
}
