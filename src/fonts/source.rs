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
//!
//! The streaming half is [`get_with_progress`]: it reads the body via
//! `reqwest::Response::chunk()` rather than buffering the whole thing first,
//! so an oversized response is caught (and discarded) the instant the
//! running total crosses the cap rather than after every byte has already
//! been held in memory. [`FontSource::fetch_font_with_progress`] rides the
//! same read, handing the cumulative received-byte count to an optional
//! sink after every chunk — a font's own download is the only fetch that
//! needs it (see [`super::catalog`]'s per-entry fraction).

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

/// A [`FontSource::fetch_font_with_progress`] progress sink: called after
/// every chunk with the cumulative bytes received for that one file's fetch
/// so far (never above the download cap — a chunk that would cross it aborts
/// the read before the sink runs). `Send` because the future carrying it
/// crosses onto the background runtime (`frust::spawn`, see
/// `super::catalog`'s module docs).
pub type ProgressSink<'a> = &'a mut (dyn FnMut(u64) + Send);

/// Where the catalog's bytes come from. The manifest fetch answers a whole
/// body — tens of kilobytes, no streaming API needed — and the SHA-256 check
/// downstream needs a font's whole body in hand anyway; streaming only buys
/// the font path early cap enforcement and live progress (see the
/// [module docs](self)).
#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
pub trait FontSource: Send + Sync {
    /// The catalog document ([`MANIFEST_URL`]).
    async fn fetch_manifest(&self) -> Result<Vec<u8>, AppFailure>;

    /// One font file, by its manifest URL.
    async fn fetch_font(&self, url: &str) -> Result<Vec<u8>, AppFailure>;

    /// [`Self::fetch_font`], reporting the cumulative bytes received to
    /// `progress` as the body streams in.
    ///
    /// Default: no progress at all — a single opaque [`Self::fetch_font`]
    /// call — so [`OfflineFontSource`] and every test double implementing
    /// only [`Self::fetch_font`]/[`Self::fetch_manifest`] keep compiling
    /// untouched; only [`HttpFontSource`] overrides it.
    async fn fetch_font_with_progress(
        &self,
        url: &str,
        _progress: ProgressSink<'_>,
    ) -> Result<Vec<u8>, AppFailure> {
        self.fetch_font(url).await
    }
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

/// What this client calls itself. Load-bearing, not cosmetic: GitHub's
/// release-asset chain (`github.com/…/releases/download/…` → 302 →
/// `release-assets.githubusercontent.com`) refuses User-Agent-less requests
/// outright, while `raw.githubusercontent.com` tolerates them — so without
/// this, the manifest loads and every font download fails on the spot.
/// `reqwest` sends no User-Agent unless told to; the original Flutter client
/// never hit this only because Dart's `HttpClient` always sends its own
/// (`Dart/<sdk> (dart:io)`).
const USER_AGENT: &str = concat!("muxr/", env!("CARGO_PKG_VERSION"));

/// The shared client, or the construction error that keeps recurring.
fn client() -> Result<&'static reqwest::Client, AppFailure> {
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .timeout(FETCH_TIMEOUT)
                .build()
                .map_err(|err| err.to_string())
        })
        .as_ref()
        .map_err(|err| AppFailure::Network(format!("font http client: {err}")))
}

/// `GET url`, bounded by `max_bytes`. [`get_with_progress`] with no progress
/// sink.
async fn get(url: &str, max_bytes: u64) -> Result<Vec<u8>, AppFailure> {
    get_with_progress(url, max_bytes, None).await
}

/// `GET url`, bounded by `max_bytes`, read one chunk at a time
/// (`reqwest::Response::chunk()`) so the cap is enforced **while the body
/// arrives** rather than only after it is fully buffered — the module docs'
/// "and while streaming" clause. `progress`, when given, is called after
/// every chunk that keeps the running total under the cap with that
/// cumulative total.
async fn get_with_progress(
    url: &str,
    max_bytes: u64,
    mut progress: Option<ProgressSink<'_>>,
) -> Result<Vec<u8>, AppFailure> {
    let mut response = client()?
        .get(url)
        .send()
        .await
        .map_err(|err| AppFailure::Network(format!("fetching {url}: {err}")))?;
    let status = response.status();
    if !status.is_success() {
        return Err(AppFailure::Network(format!("HTTP {status} for {url}")));
    }
    // The advertised length first (cheap refusal), then the stream itself —
    // a server's `Content-Length` is a claim, not a fact.
    if response.content_length().is_some_and(|len| len > max_bytes) {
        return Err(AppFailure::Network(format!(
            "{url} advertises more than the {max_bytes}-byte cap"
        )));
    }
    let mut accumulator = CappedAccumulator::new(url, max_bytes);
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| AppFailure::Network(format!("reading {url}: {err}")))?
    {
        let received = accumulator.push(&chunk)?;
        if let Some(sink) = progress.as_mut() {
            sink(received);
        }
    }
    Ok(accumulator.into_bytes())
}

/// Enforces a byte cap against a body arriving one chunk at a time — the
/// pure core of [`get_with_progress`]'s streaming read, network-free and so
/// exercised directly by a unit test.
struct CappedAccumulator<'a> {
    url: &'a str,
    max_bytes: u64,
    bytes: Vec<u8>,
}

impl<'a> CappedAccumulator<'a> {
    fn new(url: &'a str, max_bytes: u64) -> Self {
        Self {
            url,
            max_bytes,
            bytes: Vec::new(),
        }
    }

    /// Append one chunk. `Err` the instant the running total exceeds the
    /// cap — the caller must stop reading; nothing already pushed is ever
    /// handed back once this returns an error. `Ok` answers the cumulative
    /// byte count so far.
    fn push(&mut self, chunk: &[u8]) -> Result<u64, AppFailure> {
        self.bytes.extend_from_slice(chunk);
        let total = self.bytes.len() as u64;
        if total > self.max_bytes {
            return Err(AppFailure::Network(format!(
                "{} exceeded the {}-byte cap",
                self.url, self.max_bytes
            )));
        }
        Ok(total)
    }

    fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
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

    async fn fetch_font_with_progress(
        &self,
        url: &str,
        progress: ProgressSink<'_>,
    ) -> Result<Vec<u8>, AppFailure> {
        get_with_progress(url, MAX_FONT_BYTES, Some(progress)).await
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

    #[test]
    fn the_default_progress_method_never_touches_the_sink() {
        // OfflineFontSource implements neither override — its
        // `fetch_font_with_progress` is the trait's default, which must
        // answer through `fetch_font` alone and call the sink zero times.
        let source = OfflineFontSource;
        let calls = std::sync::atomic::AtomicU64::new(0);
        let mut sink: Box<dyn FnMut(u64) + Send> = Box::new(|_| {
            calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
        let result = futures_block_on(
            source.fetch_font_with_progress("https://example.invalid/a.ttf", &mut *sink),
        );
        assert!(matches!(result, Err(AppFailure::Network(_))));
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 0);
    }

    /// Drives a future to completion on this thread — these two futures never
    /// yield, so no runtime is involved.
    fn futures_block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a current-thread runtime")
            .block_on(future)
    }

    // -- CappedAccumulator: the streaming read's pure core, no network -----

    #[test]
    fn the_accumulator_aborts_mid_stream_once_the_cap_is_exceeded() {
        let mut acc = CappedAccumulator::new("https://example.invalid/x", 10);
        assert_eq!(acc.push(&[0u8; 6]).expect("under the cap"), 6);
        let err = acc.push(&[0u8; 6]).expect_err("6 + 6 > 10 must abort");
        assert!(
            matches!(err, AppFailure::Network(_)),
            "an oversized body is a network refusal, not an internal one"
        );
    }

    #[test]
    fn the_accumulator_reports_a_monotonically_growing_running_total() {
        let mut acc = CappedAccumulator::new("https://example.invalid/x", 100);
        let totals: Vec<u64> = [10usize, 20, 5]
            .into_iter()
            .map(|n| acc.push(&vec![0u8; n]).expect("under the cap"))
            .collect();
        assert_eq!(totals, vec![10, 30, 35]);
        assert!(
            totals.windows(2).all(|pair| pair[1] >= pair[0]),
            "the running total must never regress: {totals:?}"
        );
    }
}
