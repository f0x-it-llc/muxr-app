//! The on-disk font cache (`<data dir>/fonts/<sanitized family>/`: files plus a `meta` sidecar) and the installed record.
//! Blocking; callers use `frust::spawn_blocking`. SHA-256 is mandatory and verified in memory before anything is written; the sfnt magic is only a format probe.
//! The registered family is whatever fontique resolves the bytes into, not the manifest's claim.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use frust::authoring::text::TextContext;
use sha2::{Digest, Sha256};

use crate::failure::AppFailure;

use super::manifest::{FontEntry, FontFile};
use super::source::MAX_FONT_BYTES;

const FONTS_DIR: &str = "fonts";
const MANIFEST_FILE: &str = "manifest.json";
const META_FILE: &str = "meta";

/// Valid `sfnt` magic prefixes: TrueType, `OTTO` (CFF), legacy Apple `true`.
const SFNT_MAGICS: [&[u8; 4]; 3] = [b"\x00\x01\x00\x00", b"OTTO", b"true"];

#[derive(Clone, Debug, PartialEq, Eq)]
struct InstalledFont {
    /// The family fontique resolved: what a stack names and the preference persists.
    family: String,
    display_name: String,
    /// The manifest's claim, when it differed from [`Self::family`].
    manifest_family: String,
}

static INSTALLED: Mutex<Vec<InstalledFont>> = Mutex::new(Vec::new());

static CACHE_DIR_OVERRIDE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Used verbatim (no `fonts/` suffix). Test-only: a `tests/*.rs` binary cannot reach a `#[cfg(test)]` helper.
pub fn use_font_cache_dir_for_tests(dir: PathBuf) {
    *CACHE_DIR_OVERRIDE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(dir);
}

/// Test-only. Cannot un-register font bytes; only a fresh process clears the framework registry.
pub fn reset_installed_fonts_for_tests() {
    INSTALLED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

/// The cache root: the test override, else the platform data directory joined with [`FONTS_DIR`].
///
/// # Errors
///
/// [`AppFailure::Storage`] when no data directory resolves.
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

/// Off Android this is `frust-paths`' resolution, namespaced per binary.
#[cfg(not(target_os = "android"))]
fn platform_data_dir() -> Option<PathBuf> {
    frust_paths::data_dir().map(|dir| dir.join(frust_paths::app_stem()))
}

/// Android's app-private files directory; `frust-paths` cannot answer it (no writable `$HOME`).
/// Falls back to `frust-paths` when the platform handles are not installed yet.
#[cfg(target_os = "android")]
fn platform_data_dir() -> Option<PathBuf> {
    android_files_dir()
        .or_else(|| frust_paths::data_dir().map(|dir| dir.join(frust_paths::app_stem())))
}

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

pub fn installed_downloads() -> Vec<String> {
    INSTALLED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .map(|font| font.family.clone())
        .collect()
}

pub fn is_downloaded(family: &str) -> bool {
    INSTALLED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .any(|font| font.family == family)
}

/// Survives a relaunch through the `meta` sidecar, so it needs no live catalog.
pub fn display_name_for(family: &str) -> Option<String> {
    INSTALLED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .find(|font| font.family == family)
        .map(|font| font.display_name.clone())
}

/// `None` when the entry was never installed here or the manifest was right.
pub fn registered_family_for(manifest_family: &str) -> Option<String> {
    INSTALLED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .find(|font| font.manifest_family == manifest_family)
        .map(|font| font.family.clone())
}

fn remember(font: InstalledFont) {
    let mut installed = INSTALLED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if installed.iter().any(|known| known.family == font.family) {
        return;
    }
    installed.push(font);
}

pub fn read_cached_manifest() -> Option<Vec<u8>> {
    let path = cache_root().ok()?.join(MANIFEST_FILE);
    std::fs::read(path).ok()
}

/// Best-effort: a failed write is logged and swallowed.
pub fn write_cached_manifest(bytes: &[u8]) {
    let Ok(root) = cache_root() else {
        return;
    };
    if let Err(err) = frust_paths::atomic_write(&root.join(MANIFEST_FILE), bytes) {
        log::warn!("fonts: failed to cache the font manifest: {err}");
    }
}

/// Verify, cache and register a downloaded entry; answers the family it registered as.
///
/// `files` pairs each of `entry`'s files with its bytes, in any order. Blocking.
///
/// # Errors
///
/// - [`AppFailure::Internal`]: a size/magic/SHA-256 check failed or no font parses. Nothing is written.
/// - [`AppFailure::Storage`]: the cache directory could not be written; a partial write is removed.
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
        // Never leave a half-written family for the next startup scan.
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

fn has_sfnt_magic(bytes: &[u8]) -> bool {
    SFNT_MAGICS
        .iter()
        .any(|magic| bytes.starts_with(magic.as_slice()))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn probe_family(bytes: &[u8]) -> Option<String> {
    let mut cx = TextContext::new();
    let registered = cx.register_fonts(bytes.to_vec()).ok()?;
    registered
        .into_iter()
        .map(|family| family.name)
        .find(|name| !name.is_empty())
}

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

/// Line format: the two names, the manifest's claim, then one `<sha256> <filename>` line per cached file.
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

struct Meta {
    family: String,
    display_name: String,
    manifest_family: String,
    files: Vec<(String, String)>,
}

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

/// Re-registers every valid cached family and answers them. Never fails: an unreadable root answers an empty
/// list, and a family with a missing, unparseable or non-matching sidecar is deleted and skipped.
pub fn scan_cache_and_register() -> Vec<String> {
    let Ok(root) = cache_root() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
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

fn load_cached_family(dir: &Path) -> Option<(Meta, Vec<Vec<u8>>)> {
    let text = std::fs::read_to_string(dir.join(META_FILE)).ok()?;
    let meta = parse_meta(&text)?;
    let mut payloads = Vec::with_capacity(meta.files.len());
    for (sha, name) in &meta.files {
        // A sidecar naming a path would escape the cache directory.
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

    /// A real TTF; its family (`JetBrains Mono`) is what the probe must report whatever a manifest claims.
    const REAL_FONT: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf");

    /// Serializes tests against the process-wide statics (installed record, cache-dir override).
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn serial() -> MutexGuard<'static, ()> {
        TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

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
        // One flipped byte: a font by magic, but not the file the manifest vouched for.
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
        // An honest hash on an HTML page: only the magic check can catch it.
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
        // `../../` in a sidecar would read a file from outside the cache.
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
        // The empty string's SHA-256.
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
        let meta = parse_meta("family=A\nfile=aa a.ttf\n").expect("the minimum");
        assert_eq!(meta.display_name, "A");
        assert_eq!(meta.manifest_family, "A");
    }
}
