//! [`FontSource`]: where the catalog's bytes come from, and the seam a headless suite swaps out.
//! Fetches are bounded by a timeout and a size cap (checked against `Content-Length` and while streaming); transport failures retry, refusals never do.
//! Font bytes are SHA-256-verified downstream; the manifest relies on TLS plus the cap.

use std::sync::{Arc, Mutex, OnceLock};

use crate::failure::AppFailure;

use super::manifest::MANIFEST_URL;

/// A stalled connection must not leave a picker row spinning forever.
const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Total attempts per fetch (1 + retries); only transport failures retry, never HTTP errors or cap violations.
const FETCH_ATTEMPTS: u32 = 3;

/// The last entry repeats if attempts exceed the table.
const RETRY_BACKOFF: [std::time::Duration; 2] = [
    std::time::Duration::from_millis(500),
    std::time::Duration::from_millis(1500),
];

/// 1 MiB; the real manifest is about 40 KB.
pub const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

/// 50 MiB; the catalog's biggest entry is about 3 MB.
pub const MAX_FONT_BYTES: u64 = 50 * 1024 * 1024;

/// Called after every chunk with the cumulative bytes received; `Send` because the future crosses onto the background runtime.
pub type ProgressSink<'a> = &'a mut (dyn FnMut(u64) + Send);

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
pub trait FontSource: Send + Sync {
    async fn fetch_manifest(&self) -> Result<Vec<u8>, AppFailure>;

    async fn fetch_font(&self, url: &str) -> Result<Vec<u8>, AppFailure>;

    /// Default: one opaque [`Self::fetch_font`] call with no progress; only [`HttpFontSource`] overrides it.
    async fn fetch_font_with_progress(
        &self,
        url: &str,
        _progress: ProgressSink<'_>,
    ) -> Result<Vec<u8>, AppFailure> {
        self.fetch_font(url).await
    }
}

pub type SharedFontSource = Arc<dyn FontSource>;

static OVERRIDE: Mutex<Option<SharedFontSource>> = Mutex::new(None);

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

/// Test-only: callable from a `tests/*.rs` binary, which cannot reach a `#[cfg(test)]` item.
pub fn use_font_source_for_tests(source: SharedFontSource) {
    *OVERRIDE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(source);
}

pub fn use_offline_font_source_for_tests() {
    use_font_source_for_tests(Arc::new(OfflineFontSource));
}

pub struct HttpFontSource;

/// Built lazily: it needs a live tokio runtime, which every fetch has under `frust::spawn`.
static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();

/// Load-bearing: GitHub's release-asset redirect chain refuses requests with no User-Agent, so every download would fail.
const USER_AGENT: &str = concat!("muxr/", env!("CARGO_PKG_VERSION"));

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

async fn get(url: &str, max_bytes: u64) -> Result<Vec<u8>, AppFailure> {
    get_with_progress(url, max_bytes, None).await
}

/// `Transient` (transport) failures are retried; `Fatal` ones (HTTP error, cap violation) are answered as-is.
enum FetchError {
    Transient(AppFailure),
    Fatal(AppFailure),
}

fn retry_delay(failed_attempt: u32) -> Option<std::time::Duration> {
    if failed_attempt >= FETCH_ATTEMPTS {
        return None;
    }
    let index = (failed_attempt as usize - 1).min(RETRY_BACKOFF.len() - 1);
    Some(RETRY_BACKOFF[index])
}

/// Enforces the cap while the body arrives; each retry reports `0` to the sink first so the bar drops back to the file's base.
async fn get_with_progress(
    url: &str,
    max_bytes: u64,
    mut progress: Option<ProgressSink<'_>>,
) -> Result<Vec<u8>, AppFailure> {
    let mut attempt = 1u32;
    loop {
        match get_once(url, max_bytes, &mut progress).await {
            Ok(bytes) => return Ok(bytes),
            Err(FetchError::Fatal(failure)) => return Err(failure),
            Err(FetchError::Transient(failure)) => match retry_delay(attempt) {
                None => return Err(failure),
                Some(delay) => {
                    log::warn!("fonts: retrying {url} after attempt {attempt}: {failure}");
                    tokio::time::sleep(delay).await;
                    if let Some(sink) = progress.as_mut() {
                        sink(0);
                    }
                    attempt += 1;
                }
            },
        }
    }
}

async fn get_once(
    url: &str,
    max_bytes: u64,
    progress: &mut Option<ProgressSink<'_>>,
) -> Result<Vec<u8>, FetchError> {
    let mut response = client()
        .map_err(FetchError::Fatal)?
        .get(url)
        .send()
        .await
        .map_err(|err| {
            FetchError::Transient(AppFailure::Network(format!("fetching {url}: {err}")))
        })?;
    let status = response.status();
    if !status.is_success() {
        return Err(FetchError::Fatal(AppFailure::Network(format!(
            "HTTP {status} for {url}"
        ))));
    }
    // `Content-Length` is a claim, not a fact: refuse early, then enforce while streaming.
    if response.content_length().is_some_and(|len| len > max_bytes) {
        return Err(FetchError::Fatal(AppFailure::Network(format!(
            "{url} advertises more than the {max_bytes}-byte cap"
        ))));
    }
    let mut accumulator = CappedAccumulator::new(url, max_bytes);
    loop {
        let chunk = response.chunk().await.map_err(|err| {
            FetchError::Transient(AppFailure::Network(format!("reading {url}: {err}")))
        })?;
        let Some(chunk) = chunk else {
            return Ok(accumulator.into_bytes());
        };
        let received = accumulator.push(&chunk).map_err(FetchError::Fatal)?;
        if let Some(sink) = progress.as_mut() {
            sink(received);
        }
    }
}

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

    /// `Err` the instant the running total exceeds the cap; the caller must stop reading.
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
        let source = OfflineFontSource;
        let manifest = futures_block_on(source.fetch_manifest());
        assert!(matches!(manifest, Err(AppFailure::Network(_))));
        let font = futures_block_on(source.fetch_font("https://example.invalid/a.ttf"));
        assert!(matches!(font, Err(AppFailure::Network(_))));
    }

    #[test]
    fn the_default_progress_method_never_touches_the_sink() {
        // OfflineFontSource relies on the trait's default `fetch_font_with_progress`.
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

    /// These futures never yield, so no runtime is involved.
    fn futures_block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a current-thread runtime")
            .block_on(future)
    }

    #[test]
    fn the_retry_schedule_backs_off_then_gives_up() {
        assert_eq!(retry_delay(1), Some(RETRY_BACKOFF[0]));
        assert_eq!(retry_delay(2), Some(RETRY_BACKOFF[1]));
        assert_eq!(
            retry_delay(FETCH_ATTEMPTS),
            None,
            "the final attempt's failure is the answer — no further sleep"
        );
    }

    #[test]
    fn every_scheduled_retry_has_a_positive_delay() {
        for failed in 1..FETCH_ATTEMPTS {
            let delay = retry_delay(failed).expect("attempts under the cap retry");
            assert!(delay > std::time::Duration::ZERO);
        }
    }

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
