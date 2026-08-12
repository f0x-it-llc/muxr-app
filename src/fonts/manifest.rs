//! The downloadable-font catalog's wire format: `muxr-core`'s
//! `fonts/manifest.json`, parsed into [`FontEntry`] records.
//!
//! # The two names, and why they differ
//!
//! Every entry carries **both** a [`FontEntry::display_name`] (what the picker
//! prints — "IBM Plex Mono Nerd Font Mono") and a [`FontEntry::family`] (the
//! family the font's own `name` table declares, and so the string a
//! `FontFamily` stack must name — "BlexMono Nerd Font Mono"). They differ for
//! 27 of the catalog's 34 entries, because the Nerd Font project renames the
//! patched face. **The persisted preference is the `family`**; the display
//! name never reaches storage. A stack naming a display name would resolve to
//! nothing and shape with the fallback face instead — silently, with no error
//! anywhere.
//!
//! The manifest's claim is not taken on faith: [`super::store`] registers the
//! downloaded bytes and records the family fontique *actually* resolved,
//! preferring that over this field when the two disagree (see
//! `store::install_downloaded`).
//!
//! # Lenient by entry, strict by document
//!
//! A malformed *entry* (or file) is skipped rather than failing the catalog —
//! a manifest gaining a field this app doesn't model, or shipping one broken
//! row, must not take the whole picker offline. A document that isn't JSON at
//! all, or that carries no usable entry, is an [`AppFailure::Internal`]: it is
//! the catalog's own contract that broke, not the user's network.

use crate::failure::AppFailure;

use super::json::{self, Json};

/// Where the catalog lives — the `muxr-core` repo's raw `main` copy, the same
/// URL the Flutter client fetches.
pub const MANIFEST_URL: &str =
    "https://raw.githubusercontent.com/f0x-it-llc/muxr-core/main/fonts/manifest.json";

/// The base a file's relative `path` is resolved against when its entry
/// carries no absolute `url` of its own (every current entry does carry one;
/// this is the schema's documented fallback, not dead weight).
const RAW_BASE: &str = "https://raw.githubusercontent.com/f0x-it-llc/muxr-core/main/fonts";

/// One downloadable file inside a [`FontEntry`] — one weight/style of the
/// family.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontFile {
    /// The file's own name, with any directory part of the manifest `path`
    /// stripped — what the cache stores it as.
    pub name: String,
    /// Where to fetch it from.
    pub url: String,
    /// Its SHA-256, lowercase hex. **Mandatory**: an entry whose file omits it
    /// is skipped at parse time, so nothing downstream can install an
    /// unverifiable font (see [`super::store`]'s integrity contract).
    pub sha256: String,
    /// The size the manifest advertises, in bytes — the picker's size hint,
    /// and a sanity bound on the download. Never trusted as an integrity
    /// control.
    pub bytes: u64,
}

/// One font family the catalog offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontEntry {
    /// The manifest's own slug (`ibm-plex-mono-nf`) — stable across renames,
    /// and this entry's identity in the picker.
    pub id: String,
    /// The picker's label. See the [module docs](self)' two-names section.
    pub display_name: String,
    /// The family name a `FontFamily` stack must carry. See the same section.
    pub family: String,
    /// The SPDX license id the row prints beside the size.
    pub license: String,
    /// Whether the face ships Nerd Font icon glyphs (every current entry
    /// does; the schema allows otherwise).
    pub nerd_font: bool,
    /// Its files, in manifest order — all of them are downloaded, so a
    /// family's bold and regular weights both resolve.
    pub files: Vec<FontFile>,
}

impl FontEntry {
    /// The total download size of every file in this entry.
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|file| file.bytes).sum()
    }

    /// The picker's size hint — `2.4 MB`, or `812 KB` under a megabyte.
    /// Binary units, matching the Flutter client's own rounding.
    pub fn size_label(&self) -> String {
        let bytes = self.total_bytes() as f64;
        let mb = bytes / (1024.0 * 1024.0);
        if mb >= 1.0 {
            format!("{mb:.1} MB")
        } else {
            format!("{:.0} KB", bytes / 1024.0)
        }
    }
}

/// Parse a fetched manifest document into its entries.
///
/// # Errors
///
/// [`AppFailure::Internal`] when the bytes are not UTF-8, not JSON, or carry
/// no usable entry — see the [module docs](self)' leniency contract.
pub fn parse_manifest(bytes: &[u8]) -> Result<Vec<FontEntry>, AppFailure> {
    let text = std::str::from_utf8(bytes)
        .map_err(|err| AppFailure::Internal(format!("font manifest is not UTF-8: {err}")))?;
    let document = json::parse(text)
        .ok_or_else(|| AppFailure::Internal("font manifest is not valid JSON".to_string()))?;
    let fonts = document
        .get("fonts")
        .and_then(Json::as_array)
        .ok_or_else(|| {
            AppFailure::Internal("font manifest carries no `fonts` array".to_string())
        })?;

    let entries: Vec<FontEntry> = fonts.iter().filter_map(parse_entry).collect();
    if entries.is_empty() {
        return Err(AppFailure::Internal(
            "font manifest carries no usable entries".to_string(),
        ));
    }
    Ok(entries)
}

/// One entry, or `None` when a required field is missing/mistyped or every one
/// of its files is unusable.
fn parse_entry(value: &Json) -> Option<FontEntry> {
    let id = value.get("id").and_then(Json::as_str)?.to_string();
    let display_name = value.get("displayName").and_then(Json::as_str)?.to_string();
    let family = value.get("family").and_then(Json::as_str)?.to_string();
    if display_name.is_empty() || family.is_empty() {
        return None;
    }
    let license = value
        .get("license")
        .and_then(Json::as_str)
        .unwrap_or("")
        .to_string();
    let nerd_font = value
        .get("isNerdFont")
        .and_then(Json::as_bool)
        .unwrap_or(false);
    let files: Vec<FontFile> = value
        .get("files")
        .and_then(Json::as_array)
        .unwrap_or(&[])
        .iter()
        .filter_map(parse_file)
        .collect();
    if files.is_empty() {
        return None;
    }
    Some(FontEntry {
        id,
        display_name,
        family,
        license,
        nerd_font,
        files,
    })
}

/// One file, or `None` when it has no path, no usable name, or — the strict
/// one — no SHA-256 to verify it against.
fn parse_file(value: &Json) -> Option<FontFile> {
    let path = value.get("path").and_then(Json::as_str)?;
    let name = leaf(path);
    if name.is_empty() {
        return None;
    }
    let sha256 = value
        .get("sha256")
        .and_then(Json::as_str)?
        .trim()
        .to_lowercase();
    // Fail closed on a missing/short hash: the 4-byte sfnt magic the store
    // also checks is a format probe, never an integrity control.
    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let url = value
        .get("url")
        .and_then(Json::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("{RAW_BASE}/{path}"));
    // Fail closed on a non-HTTPS url, same as the hash above: integrity is
    // carried by the sha256 either way, but a plaintext fetch would leak the
    // request and invite junk bytes we'd only reject after downloading them.
    if !url.starts_with("https://") {
        return None;
    }
    let bytes = value.get("bytes").and_then(Json::as_u64).unwrap_or(0);
    Some(FontFile {
        name: name.to_string(),
        url,
        sha256,
        bytes,
    })
}

/// The trailing filename of a manifest `path` (which may carry directory
/// segments).
fn leaf(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Four real entries, transcribed from `muxr-core`'s own
    /// `fonts/manifest.json` — including two whose `family` differs from their
    /// `displayName` (the fact this whole module exists for) and one
    /// single-file entry.
    pub(super) const FIXTURE: &str = r#"{
      "version": 1,
      "fonts": [
        {
          "id": "0xproto-nf",
          "displayName": "0xProto Nerd Font Mono",
          "family": "0xProto Nerd Font Mono",
          "license": "OFL-1.1",
          "isNerdFont": true,
          "files": [
            { "weight": 700, "style": "normal", "path": "0xProtoNerdFontMono-Bold.ttf", "bytes": 2411088, "sha256": "aa8bbe93f5dfa90381a80ad90ee3118b36d34c1140575ee1fefca20de00c6689", "url": "https://github.com/f0x-it-llc/muxr-core/releases/download/fonts-v1/0xProtoNerdFontMono-Bold.ttf" },
            { "weight": 400, "style": "normal", "path": "0xProtoNerdFontMono-Regular.ttf", "bytes": 2412316, "sha256": "e98bf52027a5895c1cad6101b8ef5905e8acc5309bb264eaff9063bf94d99848", "url": "https://github.com/f0x-it-llc/muxr-core/releases/download/fonts-v1/0xProtoNerdFontMono-Regular.ttf" }
          ]
        },
        {
          "id": "bigblue-terminal-437-nf",
          "displayName": "BigBlue Terminal 437 Nerd Font Mono",
          "family": "BigBlueTerm437 Nerd Font Mono",
          "license": "CC-BY-SA-4.0",
          "isNerdFont": true,
          "files": [
            { "weight": 400, "style": "normal", "path": "BigBlueTerm437NerdFontMono-Regular.ttf", "bytes": 2278376, "sha256": "f89f27225795feba3ee5184052da69532966d950512106a5e859d85765835442", "url": "https://github.com/f0x-it-llc/muxr-core/releases/download/fonts-v1/BigBlueTerm437NerdFontMono-Regular.ttf" }
          ]
        },
        {
          "id": "ibm-plex-mono-nf",
          "displayName": "IBM Plex Mono Nerd Font Mono",
          "family": "BlexMono Nerd Font Mono",
          "license": "OFL-1.1",
          "isNerdFont": true,
          "files": [
            { "weight": 700, "style": "normal", "path": "BlexMonoNerdFontMono-Bold.ttf", "bytes": 2355496, "sha256": "565b42d837b5121627d57cfd57338be32254c58a1b770468c3380b1f78de2aa4", "url": "https://github.com/f0x-it-llc/muxr-core/releases/download/fonts-v1/BlexMonoNerdFontMono-Bold.ttf" },
            { "weight": 400, "style": "normal", "path": "BlexMonoNerdFontMono-Regular.ttf", "bytes": 2353588, "sha256": "f9892505b7cd6e9d29f9ba0924da73df023714adf6dbe6d8f920d47b8930fcef", "url": "https://github.com/f0x-it-llc/muxr-core/releases/download/fonts-v1/BlexMonoNerdFontMono-Regular.ttf" }
          ]
        },
        {
          "id": "broken-no-hash",
          "displayName": "Unverifiable Mono",
          "family": "Unverifiable Mono",
          "license": "OFL-1.1",
          "isNerdFont": false,
          "files": [
            { "weight": 400, "style": "normal", "path": "Unverifiable.ttf", "bytes": 10 }
          ]
        }
      ]
    }"#;

    #[test]
    fn the_catalog_parses_every_well_formed_entry() {
        let entries = parse_manifest(FIXTURE.as_bytes()).expect("the fixture parses");
        assert_eq!(
            entries.len(),
            3,
            "the hash-less entry is dropped, the other three survive"
        );
        assert_eq!(entries[0].id, "0xproto-nf");
        assert_eq!(entries[0].files.len(), 2);
        assert_eq!(entries[1].files.len(), 1);
        assert!(entries[0].nerd_font);
    }

    #[test]
    fn a_family_that_differs_from_its_display_name_keeps_both() {
        // The whole reason this module models two names — a picker row that
        // said `BlexMono` would be unrecognizable, a stack that said
        // `IBM Plex Mono Nerd Font Mono` would resolve to nothing.
        let entries = parse_manifest(FIXTURE.as_bytes()).expect("parses");
        let plex = entries
            .iter()
            .find(|entry| entry.id == "ibm-plex-mono-nf")
            .expect("the fixture carries it");
        assert_eq!(plex.display_name, "IBM Plex Mono Nerd Font Mono");
        assert_eq!(plex.family, "BlexMono Nerd Font Mono");
        assert_ne!(plex.display_name, plex.family);
    }

    #[test]
    fn a_file_without_a_usable_sha256_is_refused() {
        // Fail-closed: the hash is the only integrity control there is.
        let entries = parse_manifest(FIXTURE.as_bytes()).expect("parses");
        assert!(
            !entries.iter().any(|entry| entry.id == "broken-no-hash"),
            "an entry whose only file has no hash has no usable file left"
        );
    }

    #[test]
    fn the_download_url_and_cached_name_come_off_the_manifest() {
        let entries = parse_manifest(FIXTURE.as_bytes()).expect("parses");
        let file = &entries[1].files[0];
        assert_eq!(file.name, "BigBlueTerm437NerdFontMono-Regular.ttf");
        assert!(
            file.url
                .starts_with("https://github.com/f0x-it-llc/muxr-core/releases/")
        );
        assert_eq!(file.sha256.len(), 64);
    }

    #[test]
    fn a_plaintext_http_url_is_refused() {
        // Fail-closed like the hash: the sha256 still guards integrity, but a
        // manifest must not be able to point a download at a plaintext origin.
        let doc = r#"{"fonts":[{"id":"a","displayName":"A","family":"A","license":"OFL-1.1",
            "files":[{"path":"A.ttf","bytes":1,"url":"http://example.com/A.ttf",
            "sha256":"aa8bbe93f5dfa90381a80ad90ee3118b36d34c1140575ee1fefca20de00c6689"}]}]}"#;
        assert!(
            matches!(parse_manifest(doc.as_bytes()), Err(AppFailure::Internal(_))),
            "an entry whose only file is plaintext-http has no usable file left"
        );
    }

    #[test]
    fn a_relative_path_resolves_against_the_repos_raw_base() {
        let doc = r#"{"fonts":[{"id":"a","displayName":"A","family":"A","license":"OFL-1.1",
            "files":[{"path":"sub/A.ttf","bytes":1,
            "sha256":"aa8bbe93f5dfa90381a80ad90ee3118b36d34c1140575ee1fefca20de00c6689"}]}]}"#;
        let entries = parse_manifest(doc.as_bytes()).expect("parses");
        assert_eq!(entries[0].files[0].url, format!("{RAW_BASE}/sub/A.ttf"));
        assert_eq!(entries[0].files[0].name, "A.ttf", "the leaf, not the path");
    }

    #[test]
    fn a_document_that_is_not_a_catalog_is_an_internal_failure() {
        for bad in ["", "not json", "{}", r#"{"fonts":[]}"#, r#"{"fonts":[{}]}"#] {
            assert!(
                matches!(parse_manifest(bad.as_bytes()), Err(AppFailure::Internal(_))),
                "{bad:?} must not parse into a catalog"
            );
        }
    }

    #[test]
    fn the_size_hint_reads_in_binary_units() {
        let entries = parse_manifest(FIXTURE.as_bytes()).expect("parses");
        assert_eq!(entries[0].total_bytes(), 2411088 + 2412316);
        assert_eq!(entries[0].size_label(), "4.6 MB");
        let small = FontEntry {
            files: vec![FontFile {
                name: "a.ttf".to_string(),
                url: String::new(),
                sha256: String::new(),
                bytes: 8 * 1024,
            }],
            ..entries[0].clone()
        };
        assert_eq!(small.size_label(), "8 KB");
    }
}
