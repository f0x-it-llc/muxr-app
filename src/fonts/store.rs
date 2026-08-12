//! The on-disk font cache and the process-wide record of what is installed.
//!
//! Everything here is **blocking** — file reads, hashing, font parsing — and
//! every caller reaches it through `frust::spawn_blocking` (see
//! [`super::catalog`]), never from the UI thread.
//!
//! # Layout
//!
//! ```text
//! <data dir>/fonts/
//!   manifest.json                  the last catalog that parsed (offline copy)
//!   BlexMono_Nerd_Font_Mono/
//!     meta                         family / display / manifest family / file hashes
//!     BlexMonoNerdFontMono-Bold.ttf
//!     BlexMonoNerdFontMono-Regular.ttf
//! ```
//!
//! The directory name is the **registered** family, sanitized; the `meta`
//! sidecar is what makes a cold start cheap and honest (see below). Installed
//! *is* the cache's contents — nothing about a downloaded font is written to
//! the preferences store, so clearing app storage clears the fonts too, and
//! the only preference that survives is the family name the terminal resolves
//! (`super::resolve_terminal_family`, which degrades to the bundled stack for
//! a family that is no longer installed).
//!
//! # Integrity: SHA-256, mandatory, before anything is written
//!
//! [`install_downloaded`] verifies **every** downloaded file — size cap, sfnt
//! magic, then the manifest's SHA-256 — while the bytes are still in memory.
//! A mismatch fails the install with nothing on disk to clean up; the failure
//! is [`AppFailure::Internal`], whose user-facing message is the generic
//! "Something went wrong" (a hash mismatch is not a detail to put in front of
//! a user). The magic check is a cheap format probe, never an integrity
//! control — the hash is.
//!
//! [`scan_cache_and_register`] re-verifies the same hashes at startup, out of
//! the `meta` sidecar, and deletes any family whose files no longer match (or
//! whose sidecar is unreadable). A broken cache entry is dropped, never fatal.
//!
//! # The family name is fontique's, not the manifest's
//!
//! A manifest entry claims a `family`; the string a `FontFamily` stack must
//! carry is whatever fontique resolves the bytes into. [`install_downloaded`]
//! settles that by registering the entry's first file into a throwaway
//! [`TextContext`] and reading the resolved name back — `frust::register_app_fonts`
//! (the process-wide push a shell drains into its own context) reports
//! nothing, so this probe is the only way to *know*. The reported name wins
//! and is what gets recorded, cached and persisted; a disagreement is logged.
//!
//! The probe costs one extra parse of that first file (the same bytes are also
//! pushed to the shell's registry, which parses them again on its next frame
//! drain). That is the accepted price of not silently installing a family
//! nothing can resolve — it is paid once per download, on a background
//! thread, at the end of a multi-second network fetch.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use frust::authoring::text::TextContext;
use sha2::{Digest, Sha256};

use crate::failure::AppFailure;

use super::manifest::{FontEntry, FontFile};
use super::source::MAX_FONT_BYTES;

/// The cache directory's name under the app's data directory.
const FONTS_DIR: &str = "fonts";
/// The cached catalog's filename inside it.
const MANIFEST_FILE: &str = "manifest.json";
/// One family's sidecar filename.
const META_FILE: &str = "meta";

/// Valid `sfnt` magic prefixes — TrueType outlines, `OTTO` (CFF/OpenType), and
/// legacy Apple `true`. A cheap format probe, run before the hash so obvious
/// garbage (an HTML error page served with a 200) is refused early.
const SFNT_MAGICS: [&[u8; 4]; 3] = [b"\x00\x01\x00\x00", b"OTTO", b"true"];

/// One installed, downloaded family — the process-wide record
/// [`super::installed_families`] folds in beside the bundled faces.
#[derive(Clone, Debug, PartialEq, Eq)]
struct InstalledFont {
    /// The family fontique resolved — what a `FontFamily` stack names and what
    /// the preference persists.
    family: String,
    /// The catalog's label for it, for the picker and the settings row.
    display_name: String,
    /// The family the *manifest* claimed, when it differed from
    /// [`Self::family`] — the picker resolves a catalog row's true family
    /// through this alias (see [`registered_family_for`]).
    manifest_family: String,
}

/// The process-wide record of every downloaded family registered this run.
static INSTALLED: Mutex<Vec<InstalledFont>> = Mutex::new(Vec::new());

/// The cache root override [`use_font_cache_dir_for_tests`] installs.
static CACHE_DIR_OVERRIDE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Point the font cache at `dir` (used verbatim — no `fonts/` suffix is
/// appended) for the rest of this process's life. **Test-only**, the same
/// process-static shape `features::settings::di`'s repository seams use: a
/// `tests/*.rs` binary links the ordinary build of this crate, so a
/// `#[cfg(test)]` helper would be unreachable there.
pub fn use_font_cache_dir_for_tests(dir: PathBuf) {
    *CACHE_DIR_OVERRIDE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(dir);
}

/// Forget every downloaded family this process has registered. **Test-only**
/// (same contract as [`use_font_cache_dir_for_tests`]): the record is a
/// process-wide static, so a suite that installs a font would otherwise leak
/// it into every later test in the same binary. It cannot un-register the
/// bytes themselves — that is the framework's process-wide registry, and
/// nothing but a fresh process clears it.
pub fn reset_installed_fonts_for_tests() {
    INSTALLED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

/// The cache root: `<override>` when a test installed one, else the platform
/// data directory joined with [`FONTS_DIR`].
///
/// # Errors
///
/// [`AppFailure::Storage`] when no data directory resolves at all — a desktop
/// environment with neither `HOME` nor `APPDATA`, or an Android process whose
/// platform handles the shell installs are not up yet.
fn cache_root() -> Result<PathBuf, AppFailure> {
    if let Some(dir) = CACHE_DIR_OVERRIDE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
    {
        return Ok(dir);
    }
    platform_data_dir()
        .map(|dir| dir.join(FONTS_DIR))
        .ok_or_else(|| {
            AppFailure::Storage("no writable app directory for the font cache".to_string())
        })
}

/// The app-private directory the cache hangs off.
///
/// Off Android this is `frust-paths`' own resolution (`$XDG_DATA_HOME`/
/// `$HOME/.local/share`, `%APPDATA%`), namespaced per binary by
/// `frust_paths::app_stem` exactly as `frust-shared-preferences`' own file
/// backend does — so two Frust apps on one desktop never share a cache. On
/// iOS that resolves inside the app's own sandbox container, which is what
/// makes it durable there too.
#[cfg(not(target_os = "android"))]
fn platform_data_dir() -> Option<PathBuf> {
    frust_paths::data_dir().map(|dir| dir.join(frust_paths::app_stem()))
}

/// Android's own app-private files directory (`Context.getFilesDir()`).
///
/// `frust-paths` cannot answer this: its Unix branch resolves `$XDG_DATA_HOME`
/// or `$HOME`, and an Android app process has neither pointing anywhere it may
/// write — so a download would fail with a permission error on the one
/// platform this feature is most used on. The `Context` comes from
/// `frust_plugin::android::with_jni_env`, the sanctioned platform-handle
/// accessor `src/net/tls.rs` already uses for the TLS verifier's own JNI
/// initialization. No per-binary namespacing: the directory is already
/// app-private.
///
/// Falls back to `frust-paths` if the handles are not installed (a call
/// racing the shell's own `nativeInitPlatform`), so the ladder is never a hard
/// failure on a platform where the fallback might still work.
#[cfg(target_os = "android")]
fn platform_data_dir() -> Option<PathBuf> {
    android_files_dir()
        .or_else(|| frust_paths::data_dir().map(|dir| dir.join(frust_paths::app_stem())))
}

/// `context.getFilesDir().getAbsolutePath()`, or `None` if any step fails.
#[cfg(target_os = "android")]
fn android_files_dir() -> Option<PathBuf> {
    use jni::objects::JString;
    use jni::{jni_sig, jni_str};

    let path = frust_plugin::android::with_jni_env(|env, context| {
        let dir = env
            .call_method(
                context,
                jni_str!("getFilesDir"),
                jni_sig!("()Ljava/io/File;"),
                &[],
            )
            .ok()?
            .l()
            .ok()?;
        let path = env
            .call_method(
                &dir,
                jni_str!("getAbsolutePath"),
                jni_sig!("()Ljava/lang/String;"),
                &[],
            )
            .ok()?
            .l()
            .ok()?;
        let path = env.cast_local::<JString>(path).ok()?;
        Some(path.to_string())
    })
    .ok()
    .flatten()?;
    Some(PathBuf::from(path))
}

// ---------------------------------------------------------------------------
// The installed record
// ---------------------------------------------------------------------------

/// Every downloaded family registered this process, in registration order.
pub fn installed_downloads() -> Vec<String> {
    INSTALLED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .map(|font| font.family.clone())
        .collect()
}

/// Whether `family` names a downloaded family this process has registered.
pub fn is_downloaded(family: &str) -> bool {
    INSTALLED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .any(|font| font.family == family)
}

/// The catalog label a downloaded `family` was installed under, if any — what
/// the picker row and the settings row print instead of the raw family name
/// (`BlexMono Nerd Font Mono` → `IBM Plex Mono Nerd Font Mono`). Survives a
/// relaunch through the `meta` sidecar, so it needs no live catalog.
pub fn display_name_for(family: &str) -> Option<String> {
    INSTALLED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .find(|font| font.family == family)
        .map(|font| font.display_name.clone())
}

/// The family a catalog entry claiming `manifest_family` actually registered
/// as, when the two disagreed (see the [module docs](self)). `None` when this
/// entry has never been installed here, or when the manifest was right.
pub fn registered_family_for(manifest_family: &str) -> Option<String> {
    INSTALLED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .find(|font| font.manifest_family == manifest_family)
        .map(|font| font.family.clone())
}

/// Record `font` unless its family is already known.
fn remember(font: InstalledFont) {
    let mut installed = INSTALLED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if installed.iter().any(|known| known.family == font.family) {
        return;
    }
    installed.push(font);
}

// ---------------------------------------------------------------------------
// The cached catalog
// ---------------------------------------------------------------------------

/// The last catalog document that parsed, if one was cached — what the picker
/// renders when the network is gone.
pub fn read_cached_manifest() -> Option<Vec<u8>> {
    let path = cache_root().ok()?.join(MANIFEST_FILE);
    std::fs::read(path).ok()
}

/// Cache `bytes` as the catalog's last known good copy. Best-effort: a failed
/// write is logged and swallowed, since the catalog it came from is already in
/// hand.
pub fn write_cached_manifest(bytes: &[u8]) {
    let Ok(root) = cache_root() else {
        return;
    };
    if let Err(err) = frust_paths::atomic_write(&root.join(MANIFEST_FILE), bytes) {
        log::warn!("fonts: failed to cache the font manifest: {err}");
    }
}

// ---------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------

/// Verify, cache and register a downloaded entry; answer the family it
/// registered as.
///
/// `files` pairs each of `entry`'s files with its downloaded bytes, in any
/// order. Blocking — call it from `frust::spawn_blocking`.
///
/// # Errors
///
/// - [`AppFailure::Internal`] — a size/magic/SHA-256 check failed, or the
///   bytes carry no font fontique can parse. **Nothing is written**: every
///   check runs against the in-memory bytes first.
/// - [`AppFailure::Storage`] — the cache directory could not be resolved or
///   written. A partial write is removed before this returns.
pub fn install_downloaded(
    entry: &FontEntry,
    files: Vec<(FontFile, Vec<u8>)>,
) -> Result<String, AppFailure> {
    if files.is_empty() {
        return Err(AppFailure::Internal(format!(
            "font install: {} has no files",
            entry.id
        )));
    }
    for (file, bytes) in &files {
        verify(file, bytes)?;
    }

    // The name probe, before anything touches the disk (see the module docs).
    let family = probe_family(&files[0].1).ok_or_else(|| {
        AppFailure::Internal(format!(
            "font install: {} carries no parseable font faces",
            entry.id
        ))
    })?;
    if family != entry.family {
        log::warn!(
            "fonts: manifest entry {} claims family {:?} but registers as {:?} — using the \
             registered name",
            entry.id,
            entry.family,
            family
        );
    }

    let dir = cache_root()?.join(family_key(&family));
    if let Err(err) = write_family(&dir, entry, &family, &files) {
        // Never leave a half-written family behind for the next startup scan
        // to trip over.
        let _ = std::fs::remove_dir_all(&dir);
        return Err(err);
    }

    for (_, bytes) in &files {
        frust::register_app_fonts(bytes.clone());
    }
    remember(InstalledFont {
        family: family.clone(),
        display_name: entry.display_name.clone(),
        manifest_family: entry.family.clone(),
    });
    Ok(family)
}

/// Write every file plus the `meta` sidecar into `dir`.
fn write_family(
    dir: &Path,
    entry: &FontEntry,
    family: &str,
    files: &[(FontFile, Vec<u8>)],
) -> Result<(), AppFailure> {
    for (file, bytes) in files {
        frust_paths::atomic_write(&dir.join(&file.name), bytes)
            .map_err(|err| AppFailure::Storage(format!("caching {}: {err}", file.name)))?;
    }
    let meta = encode_meta(entry, family, files);
    frust_paths::atomic_write(&dir.join(META_FILE), meta.as_bytes())
        .map_err(|err| AppFailure::Storage(format!("caching the {family} sidecar: {err}")))
}

/// Check one downloaded file against its manifest record: the size cap, the
/// sfnt magic, then — the one that matters — its SHA-256.
fn verify(file: &FontFile, bytes: &[u8]) -> Result<(), AppFailure> {
    if bytes.len() as u64 > MAX_FONT_BYTES {
        return Err(AppFailure::Internal(format!(
            "font install: {} is larger than the download cap",
            file.name
        )));
    }
    if !has_sfnt_magic(bytes) {
        return Err(AppFailure::Internal(format!(
            "font install: {} is not a font file",
            file.name
        )));
    }
    let actual = sha256_hex(bytes);
    if actual != file.sha256 {
        return Err(AppFailure::Internal(format!(
            "font install: sha256 mismatch for {}",
            file.name
        )));
    }
    Ok(())
}

/// Whether `bytes` open with a recognized `sfnt` magic prefix.
fn has_sfnt_magic(bytes: &[u8]) -> bool {
    SFNT_MAGICS
        .iter()
        .any(|magic| bytes.starts_with(magic.as_slice()))
}

/// `bytes`' SHA-256, lowercase hex — the same digest spelling the manifest
/// uses.
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// The family name fontique resolves `bytes` into, or `None` if it parses no
/// faces at all. See the [module docs](self)' name section for the cost.
fn probe_family(bytes: &[u8]) -> Option<String> {
    let mut cx = TextContext::new();
    let registered = cx.register_fonts(bytes.to_vec()).ok()?;
    registered
        .into_iter()
        .map(|family| family.name)
        .find(|name| !name.is_empty())
}

/// A filesystem-safe directory name for `family`.
fn family_key(family: &str) -> String {
    family
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The `meta` sidecar
// ---------------------------------------------------------------------------

/// One family's sidecar: the two names, the manifest's own claim, and one
/// `<sha256> <filename>` line per cached file.
///
/// A hand-written line format rather than JSON: this app writes no JSON
/// anywhere (`super::json` is parse-only), and a sidecar with three keys and a
/// file list does not justify an encoder.
fn encode_meta(entry: &FontEntry, family: &str, files: &[(FontFile, Vec<u8>)]) -> String {
    let mut out = String::new();
    out.push_str(&format!("family={family}\n"));
    out.push_str(&format!("display={}\n", entry.display_name));
    out.push_str(&format!("manifest={}\n", entry.family));
    for (file, _) in files {
        out.push_str(&format!("file={} {}\n", file.sha256, file.name));
    }
    out
}

/// A parsed sidecar.
struct Meta {
    family: String,
    display_name: String,
    manifest_family: String,
    /// `(sha256, filename)` per cached file.
    files: Vec<(String, String)>,
}

/// Parse a sidecar, or `None` if it is missing a name or a file list.
fn parse_meta(text: &str) -> Option<Meta> {
    let mut family = String::new();
    let mut display_name = String::new();
    let mut manifest_family = String::new();
    let mut files = Vec::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "family" => family = value.to_string(),
            "display" => display_name = value.to_string(),
            "manifest" => manifest_family = value.to_string(),
            "file" => {
                if let Some((sha, name)) = value.split_once(' ') {
                    files.push((sha.to_string(), name.to_string()));
                }
            }
            _ => {}
        }
    }
    if family.is_empty() || files.is_empty() {
        return None;
    }
    if display_name.is_empty() {
        display_name = family.clone();
    }
    if manifest_family.is_empty() {
        manifest_family = family.clone();
    }
    Some(Meta {
        family,
        display_name,
        manifest_family,
        files,
    })
}

// ---------------------------------------------------------------------------
// Startup scan
// ---------------------------------------------------------------------------

/// Re-register every valid cached family, dropping the ones that no longer
/// verify; answer the families registered.
///
/// Blocking — `super::register_cached_fonts_at_startup` is the one caller, and
/// it runs this on a background worker. Never fails: an unreadable cache root
/// answers an empty list, and a family whose sidecar is gone, unparseable, or
/// whose bytes no longer hash to what the sidecar recorded is **deleted** and
/// skipped. A corrupt cache must never keep the app from starting.
pub fn scan_cache_and_register() -> Vec<String> {
    let Ok(root) = cache_root() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        // No cache yet is the ordinary cold-start case, not a failure.
        return Vec::new();
    };

    let mut registered = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        match load_cached_family(&dir) {
            Some((meta, payloads)) => {
                if is_downloaded(&meta.family) {
                    continue;
                }
                for bytes in payloads {
                    frust::register_app_fonts(bytes);
                }
                registered.push(meta.family.clone());
                remember(InstalledFont {
                    family: meta.family,
                    display_name: meta.display_name,
                    manifest_family: meta.manifest_family,
                });
            }
            None => {
                log::warn!(
                    "fonts: dropping unusable cached font directory {}",
                    dir.display()
                );
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
    }
    registered
}

/// One cached family's sidecar plus its verified file bytes, or `None` when
/// anything about it fails to check out.
fn load_cached_family(dir: &Path) -> Option<(Meta, Vec<Vec<u8>>)> {
    let text = std::fs::read_to_string(dir.join(META_FILE)).ok()?;
    let meta = parse_meta(&text)?;
    let mut payloads = Vec::with_capacity(meta.files.len());
    for (sha, name) in &meta.files {
        // A sidecar naming a path rather than a filename would escape the
        // cache directory; refuse it outright.
        if name.contains('/') || name.contains('\\') || name.is_empty() {
            return None;
        }
        let bytes = std::fs::read(dir.join(name)).ok()?;
        if !has_sfnt_magic(&bytes) || sha256_hex(&bytes) != *sha {
            return None;
        }
        payloads.push(bytes);
    }
    Some((meta, payloads))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::MutexGuard;

    /// The bundled regular face — a real, parseable TTF for the tests that
    /// need one. Its family (`JetBrains Mono`) is what the probe must report
    /// no matter what a manifest entry claims.
    const REAL_FONT: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf");

    /// Serializes every test here against the process-wide statics this module
    /// owns (the installed record, the cache-dir override) — the same shape
    /// `frust-shell-common`'s own registry tests use for their global slot.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn serial() -> MutexGuard<'static, ()> {
        TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A fresh, empty cache root under the OS temp directory, installed as the
    /// process's cache for the rest of the test.
    fn temp_cache(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "muxr-fonts-{tag}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temp cache dir");
        use_font_cache_dir_for_tests(dir.clone());
        reset_installed_fonts_for_tests();
        dir
    }

    /// A one-file entry over `bytes`, claiming `family`.
    fn entry_for(family: &str, bytes: &[u8]) -> (FontEntry, Vec<(FontFile, Vec<u8>)>) {
        let file = FontFile {
            name: "Fixture-Regular.ttf".to_string(),
            url: "https://example.invalid/Fixture-Regular.ttf".to_string(),
            sha256: sha256_hex(bytes),
            bytes: bytes.len() as u64,
        };
        let entry = FontEntry {
            id: "fixture-nf".to_string(),
            display_name: format!("{family} Display"),
            family: family.to_string(),
            license: "OFL-1.1".to_string(),
            nerd_font: true,
            files: vec![file.clone()],
        };
        (entry, vec![(file, bytes.to_vec())])
    }

    #[test]
    fn a_sha256_mismatch_is_refused_and_caches_nothing() {
        let _guard = serial();
        let root = temp_cache("mismatch");
        let (entry, mut files) = entry_for("Fixture Mono", REAL_FONT);
        // One flipped byte: still a font by magic, no longer the file the
        // manifest vouched for.
        files[0].1[400] ^= 0xff;

        let failure = install_downloaded(&entry, files).expect_err("a mismatch must be refused");

        assert!(
            matches!(failure, AppFailure::Internal(_)),
            "an integrity failure is internal, not a network error: {failure:?}"
        );
        assert_eq!(
            std::fs::read_dir(&root).expect("root").count(),
            0,
            "nothing is written before the hash checks out"
        );
        assert!(installed_downloads().is_empty());
    }

    #[test]
    fn bytes_that_are_not_a_font_are_refused_before_any_hashing_matters() {
        let _guard = serial();
        let root = temp_cache("magic");
        // An HTML error page served with a 200 — the shape the magic check
        // exists for. Its hash is honest, so only the magic can catch it.
        let payload = b"<!DOCTYPE html><html>404</html>".to_vec();
        let (entry, mut files) = entry_for("Fixture Mono", REAL_FONT);
        files[0].0.sha256 = sha256_hex(&payload);
        files[0].1 = payload;

        assert!(matches!(
            install_downloaded(&entry, files),
            Err(AppFailure::Internal(_))
        ));
        assert_eq!(std::fs::read_dir(&root).expect("root").count(), 0);
    }

    #[test]
    fn a_verified_download_is_cached_registered_and_recorded_under_the_reported_family() {
        let _guard = serial();
        let root = temp_cache("install");
        // The manifest lies about the family; the probe is what settles it.
        let (entry, files) = entry_for("Fixture Mono", REAL_FONT);

        let family = install_downloaded(&entry, files).expect("a verified install");

        assert_eq!(
            family, "JetBrains Mono",
            "the family fontique reports wins over the manifest's claim"
        );
        assert!(installed_downloads().contains(&family));
        assert_eq!(
            display_name_for(&family).as_deref(),
            Some("Fixture Mono Display"),
            "the catalog label is what the picker prints"
        );
        assert_eq!(
            registered_family_for("Fixture Mono").as_deref(),
            Some("JetBrains Mono"),
            "the alias is what resolves a catalog row to its real family"
        );
        let dir = root.join("JetBrains_Mono");
        assert!(dir.join("Fixture-Regular.ttf").is_file());
        let meta = std::fs::read_to_string(dir.join(META_FILE)).expect("the sidecar");
        assert!(meta.contains("family=JetBrains Mono"), "{meta}");
        assert!(meta.contains("manifest=Fixture Mono"), "{meta}");
    }

    #[test]
    fn the_startup_scan_registers_a_cached_family_and_lists_it() {
        let _guard = serial();
        let root = temp_cache("scan");
        let dir = root.join("Fixture_Mono");
        std::fs::create_dir_all(&dir).expect("a family dir");
        std::fs::write(dir.join("Fixture-Regular.ttf"), REAL_FONT).expect("the face");
        std::fs::write(
            dir.join(META_FILE),
            format!(
                "family=Fixture Mono\ndisplay=Fixture Mono Nerd Font\nmanifest=Fixture Mono\n\
                 file={} Fixture-Regular.ttf\n",
                sha256_hex(REAL_FONT)
            ),
        )
        .expect("the sidecar");

        let registered = scan_cache_and_register();

        assert_eq!(registered, vec!["Fixture Mono".to_string()]);
        assert!(is_downloaded("Fixture Mono"));
        assert_eq!(
            display_name_for("Fixture Mono").as_deref(),
            Some("Fixture Mono Nerd Font"),
            "the display name survives a relaunch through the sidecar"
        );
        assert!(dir.is_dir(), "a good cache entry is left alone");
    }

    #[test]
    fn the_startup_scan_deletes_a_family_whose_bytes_no_longer_verify() {
        let _guard = serial();
        let root = temp_cache("corrupt");
        let dir = root.join("Corrupt_Mono");
        std::fs::create_dir_all(&dir).expect("a family dir");
        std::fs::write(dir.join("Corrupt-Regular.ttf"), b"not a font at all").expect("the face");
        std::fs::write(
            dir.join(META_FILE),
            format!(
                "family=Corrupt Mono\ndisplay=Corrupt Mono\nfile={} Corrupt-Regular.ttf\n",
                sha256_hex(REAL_FONT)
            ),
        )
        .expect("the sidecar");

        let registered = scan_cache_and_register();

        assert!(registered.is_empty(), "nothing usable was cached");
        assert!(!dir.exists(), "the unusable entry is deleted, never fatal");
        assert!(!is_downloaded("Corrupt Mono"));
    }

    #[test]
    fn a_family_directory_with_no_sidecar_is_dropped() {
        let _guard = serial();
        let root = temp_cache("nometa");
        let dir = root.join("Orphan_Mono");
        std::fs::create_dir_all(&dir).expect("a family dir");
        std::fs::write(dir.join("Orphan-Regular.ttf"), REAL_FONT).expect("the face");

        assert!(scan_cache_and_register().is_empty());
        assert!(!dir.exists());
    }

    #[test]
    fn the_cached_manifest_round_trips_through_the_cache_root() {
        let _guard = serial();
        let _root = temp_cache("manifest");
        assert_eq!(read_cached_manifest(), None, "nothing cached yet");
        write_cached_manifest(b"{\"fonts\":[]}");
        assert_eq!(
            read_cached_manifest().as_deref(),
            Some(&b"{\"fonts\":[]}"[..])
        );
    }

    #[test]
    fn a_sidecar_naming_a_path_rather_than_a_filename_is_refused() {
        // Path traversal through the sidecar: `../../` would otherwise read
        // (and then register) a file from outside the cache entirely.
        let _guard = serial();
        let root = temp_cache("traversal");
        let dir = root.join("Escape_Mono");
        std::fs::create_dir_all(&dir).expect("a family dir");
        std::fs::write(
            dir.join(META_FILE),
            format!(
                "family=Escape Mono\nfile={} ../../etc/passwd\n",
                sha256_hex(REAL_FONT)
            ),
        )
        .expect("the sidecar");

        assert!(scan_cache_and_register().is_empty());
        assert!(!dir.exists());
    }

    #[test]
    fn a_family_key_is_filesystem_safe() {
        assert_eq!(
            family_key("BlexMono Nerd Font Mono"),
            "BlexMono_Nerd_Font_Mono"
        );
        assert_eq!(family_key("../escape"), "___escape");
        assert!(!family_key("a/b\\c").contains('/'));
    }

    #[test]
    fn the_digest_is_the_manifests_own_spelling() {
        // Lowercase hex, 64 chars — the empty string's SHA-256.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn only_real_sfnt_magic_passes_the_format_probe() {
        assert!(has_sfnt_magic(REAL_FONT));
        assert!(has_sfnt_magic(b"OTTO....."));
        assert!(has_sfnt_magic(b"true....."));
        assert!(!has_sfnt_magic(b"<!DOCTYPE html>"));
        assert!(!has_sfnt_magic(b""));
    }

    #[test]
    fn a_sidecar_round_trips_its_names_and_files() {
        let (entry, files) = entry_for("Fixture Mono", REAL_FONT);
        let encoded = encode_meta(&entry, "JetBrains Mono", &files);
        let meta = parse_meta(&encoded).expect("its own output parses");
        assert_eq!(meta.family, "JetBrains Mono");
        assert_eq!(meta.display_name, "Fixture Mono Display");
        assert_eq!(meta.manifest_family, "Fixture Mono");
        assert_eq!(meta.files.len(), 1);
        assert_eq!(meta.files[0].1, "Fixture-Regular.ttf");
    }

    #[test]
    fn a_sidecar_missing_its_family_or_files_does_not_parse() {
        assert!(parse_meta("display=A\nfile=aa a.ttf\n").is_none());
        assert!(parse_meta("family=A\n").is_none());
        // …and the two optional names default to the family itself.
        let meta = parse_meta("family=A\nfile=aa a.ttf\n").expect("the minimum");
        assert_eq!(meta.display_name, "A");
        assert_eq!(meta.manifest_family, "A");
    }
}
