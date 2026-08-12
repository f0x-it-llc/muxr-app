//! [`FontSource`] — where the catalog's bytes come from, and the one seam a
//! headless suite swaps out.
//!
//! # Two implementations, one process-static seam
//!
//! [`HttpFontSource`] is production: plain HTTPS `GET`s over `reqwest`
//! (rustls, the tree's one aws-lc-rs provider — see `Cargo.toml`'s dependency
//! note). [`OfflineFontSource`] answers every fetch with a network failure and
//! is what [`use_offline_font_source_for_tests`] installs.
//!
//! The swap is a **process-static override**, the same shape every feature's
//! `di.rs` uses for its own repositories (`features::settings::di`'s
//! `use_mock_preferences_for_tests`): production never branches, a test opts
//! the whole process out once, and no call site learns that the seam exists.
//! `tests/support::setup` installs the offline source for every headless
//! suite, so no test can reach the network by omission — a suite that wants a
//! catalog installs its own fixture source over the top.
//!
//! # Fetch policy
//!
//! Every fetch is bounded three ways: a request timeout ([`FETCH_TIMEOUT`]), a
//! response size cap (checked against `Content-Length` *and* while streaming,
//! since the header is the server's claim rather than a fact), and — for a
//! font — the caller's own mandatory SHA-256 check downstream
//! ([`super::store`]). Redirects follow `reqwest`'s default policy, which the
//! release-asset URLs need (`github.com/…/releases/download/…` answers a 302
//! into `objects.githubusercontent.com`).

use std::sync::{Arc, Mutex, OnceLock};

use crate::failure::AppFailure;

use super::manifest::MANIFEST_URL;

/// How long one request may take end to end before it is abandoned. A stalled
/// connection must not leave a picker row spinning forever.
const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// The largest manifest this app will read (1 MiB). The real one is ~40 KB;
/// this is the "a compromised host serves a gigabyte" bound.
pub const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

/// The largest font file this app will download (50 MiB) — the Flutter
/// client's own cap. The catalog's biggest entry is ~3 MB.
pub const MAX_FONT_BYTES: u64 = 50 * 1024 * 1024;

/// Where the catalog's bytes come from. Both methods answer whole bodies:
/// a manifest is tens of kilobytes and a font a few megabytes, so nothing here
/// needs a streaming API — and the SHA-256 check downstream needs the whole
/// body in hand anyway.
#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
pub trait FontSource: Send + Sync {
    /// The catalog document ([`MANIFEST_URL`]).
    async fn fetch_manifest(&self) -> Result<Vec<u8>, AppFailure>;

    /// One font file, by its manifest URL.
    async fn fetch_font(&self, url: &str) -> Result<Vec<u8>, AppFailure>;
}

/// The shared handle every caller holds — a type alias so the install/resolve
/// pair can never drift (mirrors `di.rs`'s `Shared*Repository` aliases).
pub type SharedFontSource = Arc<dyn FontSource>;

/// The process-wide override [`use_font_source_for_tests`] installs, read by
/// [`font_source`]. `None` — every real app run — resolves
/// [`HttpFontSource`].
static OVERRIDE: Mutex<Option<SharedFontSource>> = Mutex::new(None);

/// The source every fetch goes through: the test override when one is
/// installed, else the real HTTPS client.
pub fn font_source() -> SharedFontSource {
    if let Some(source) = OVERRIDE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
    {
        return source;
    }
    Arc::new(HttpFontSource)
}

/// Point every later [`font_source`] call at `source` for the rest of this
/// process's life. **Test-only** (the `use_mock_*_for_tests` idiom): callable
/// from a `tests/*.rs` integration binary, which links the ordinary build of
/// this crate and so cannot reach a `#[cfg(test)]` item.
pub fn use_font_source_for_tests(source: SharedFontSource) {
    *OVERRIDE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(source);
}

/// Install [`OfflineFontSource`] — the safe default every headless suite runs
/// on (see the [module docs](self)). Test-only, same contract as
/// [`use_font_source_for_tests`].
pub fn use_offline_font_source_for_tests() {
    use_font_source_for_tests(Arc::new(OfflineFontSource));
}

/// The production source: HTTPS over `reqwest`.
pub struct HttpFontSource;

/// The one client, built lazily on first use.
///
/// Lazy on purpose: building it needs a live tokio runtime context (hyper's
/// pooled connector spawns onto it), and app startup is not one — every fetch
/// runs inside `frust::spawn`, which is.
static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();

/// The shared client, or the construction error that keeps recurring.
fn client() -> Result<&'static reqwest::Client, AppFailure> {
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(FETCH_TIMEOUT)
                .build()
                .map_err(|err| err.to_string())
        })
        .as_ref()
        .map_err(|err| AppFailure::Network(format!("font http client: {err}")))
}

/// `GET url`, bounded by `max_bytes`.
async fn get(url: &str, max_bytes: u64) -> Result<Vec<u8>, AppFailure> {
    let response = client()?
        .get(url)
        .send()
        .await
        .map_err(|err| AppFailure::Network(format!("fetching {url}: {err}")))?;
    let status = response.status();
    if !status.is_success() {
        return Err(AppFailure::Network(format!("HTTP {status} for {url}")));
    }
    // The advertised length first (cheap refusal), then the real one — a
    // server's `Content-Length` is a claim, not a fact.
    if response.content_length().is_some_and(|len| len > max_bytes) {
        return Err(AppFailure::Network(format!(
            "{url} advertises more than the {max_bytes}-byte cap"
        )));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|err| AppFailure::Network(format!("reading {url}: {err}")))?;
    if bytes.len() as u64 > max_bytes {
        return Err(AppFailure::Network(format!(
            "{url} exceeded the {max_bytes}-byte cap"
        )));
    }
    Ok(bytes.to_vec())
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl FontSource for HttpFontSource {
    async fn fetch_manifest(&self) -> Result<Vec<u8>, AppFailure> {
        get(MANIFEST_URL, MAX_MANIFEST_BYTES).await
    }

    async fn fetch_font(&self, url: &str) -> Result<Vec<u8>, AppFailure> {
        get(url, MAX_FONT_BYTES).await
    }
}

/// A source with nothing behind it: every fetch is a network failure. The
/// headless default (see the [module docs](self)), and the shape the offline
/// path is exercised through.
pub struct OfflineFontSource;

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl FontSource for OfflineFontSource {
    async fn fetch_manifest(&self) -> Result<Vec<u8>, AppFailure> {
        Err(AppFailure::Network("no font source configured".to_string()))
    }

    async fn fetch_font(&self, _url: &str) -> Result<Vec<u8>, AppFailure> {
        Err(AppFailure::Network("no font source configured".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_offline_source_fails_every_fetch_as_a_network_error() {
        // Retryable, and its user message says "connection problem" — which is
        // exactly what an unreachable catalog is.
        let source = OfflineFontSource;
        let manifest = futures_block_on(source.fetch_manifest());
        assert!(matches!(manifest, Err(AppFailure::Network(_))));
        let font = futures_block_on(source.fetch_font("https://example.invalid/a.ttf"));
        assert!(matches!(font, Err(AppFailure::Network(_))));
    }

    /// Drives a future to completion on this thread — these two futures never
    /// yield, so no runtime is involved.
    fn futures_block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a current-thread runtime")
            .block_on(future)
    }
}
