//! The downloadable-font catalog's wire format: `fonts/manifest.json`, parsed into [`FontEntry`].
//! `family` (the font's own name-table family) is the persisted preference and the stack name; `display_name` is the picker label only.
//! A malformed entry is skipped; a document with no usable entry is an [`AppFailure::Internal`].

use crate::failure::AppFailure;

use super::json::{self, Json};

/// Served by muxr.app itself (`no-cache`), so font discovery has no GitHub dependency.
pub const MANIFEST_URL: &str = "https://muxr.app/fonts/manifest.json";

const RAW_BASE: &str = "https://muxr.app/fonts";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontFile {
    pub name: String,
    pub url: String,
    /// Lowercase hex SHA-256; mandatory, an entry whose file omits it is skipped.
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontEntry {
    pub id: String,
    pub display_name: String,
    pub family: String,
    pub license: String,
    pub nerd_font: bool,
    pub files: Vec<FontFile>,
}

impl FontEntry {
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|file| file.bytes).sum()
    }

    /// Binary units: `2.4 MB`, or `812 KB` under a megabyte.
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

/// # Errors
///
/// [`AppFailure::Internal`] when the bytes are not UTF-8, not JSON, or carry no usable entry.
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
    // Fail closed on a missing or short hash; the sfnt magic check is a format probe, not integrity.
    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let url = value
        .get("url")
        .and_then(Json::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("{RAW_BASE}/{path}"));
    // Fail closed on a non-HTTPS url so a plaintext origin is never fetched.
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

fn leaf(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Four entries from the real manifest: two whose `family` differs from `displayName`, one single-file.
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
            { "weight": 700, "style": "normal", "path": "0xProtoNerdFontMono-Bold.ttf", "bytes": 2411088, "sha256": "aa8bbe93f5dfa90381a80ad90ee3118b36d34c1140575ee1fefca20de00c6689", "url": "https://muxr.app/fonts/0xProtoNerdFontMono-Bold.ttf" },
            { "weight": 400, "style": "normal", "path": "0xProtoNerdFontMono-Regular.ttf", "bytes": 2412316, "sha256": "e98bf52027a5895c1cad6101b8ef5905e8acc5309bb264eaff9063bf94d99848", "url": "https://muxr.app/fonts/0xProtoNerdFontMono-Regular.ttf" }
          ]
        },
        {
          "id": "bigblue-terminal-437-nf",
          "displayName": "BigBlue Terminal 437 Nerd Font Mono",
          "family": "BigBlueTerm437 Nerd Font Mono",
          "license": "CC-BY-SA-4.0",
          "isNerdFont": true,
          "files": [
            { "weight": 400, "style": "normal", "path": "BigBlueTerm437NerdFontMono-Regular.ttf", "bytes": 2278376, "sha256": "f89f27225795feba3ee5184052da69532966d950512106a5e859d85765835442", "url": "https://muxr.app/fonts/BigBlueTerm437NerdFontMono-Regular.ttf" }
          ]
        },
        {
          "id": "ibm-plex-mono-nf",
          "displayName": "IBM Plex Mono Nerd Font Mono",
          "family": "BlexMono Nerd Font Mono",
          "license": "OFL-1.1",
          "isNerdFont": true,
          "files": [
            { "weight": 700, "style": "normal", "path": "BlexMonoNerdFontMono-Bold.ttf", "bytes": 2355496, "sha256": "565b42d837b5121627d57cfd57338be32254c58a1b770468c3380b1f78de2aa4", "url": "https://muxr.app/fonts/BlexMonoNerdFontMono-Bold.ttf" },
            { "weight": 400, "style": "normal", "path": "BlexMonoNerdFontMono-Regular.ttf", "bytes": 2353588, "sha256": "f9892505b7cd6e9d29f9ba0924da73df023714adf6dbe6d8f920d47b8930fcef", "url": "https://muxr.app/fonts/BlexMonoNerdFontMono-Regular.ttf" }
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
        // A picker row showing `BlexMono` would be unrecognizable; a stack naming `IBM Plex Mono Nerd Font Mono` resolves to nothing.
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
        assert!(file.url.starts_with("https://muxr.app/fonts/"));
        assert_eq!(file.sha256.len(), 64);
    }

    #[test]
    fn a_plaintext_http_url_is_refused() {
        // The sha256 guards integrity, but a manifest must not point a download at a plaintext origin.
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
