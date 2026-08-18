//! [`PrefsSavedServersRepository`] — the real
//! [`SavedServersRepository`]: the saved-server list as one JSON array in
//! `frust-shared-preferences`, under the Flutter client's own key.
//!
//! The metadata half of the two-store split: never the credential itself,
//! which lives in [`super::secure_credential_service`]'s gated store under the
//! same `host:port` key.
//!
//! # Storage shape
//!
//! One key, [`KEY_SAVED_SERVERS`], holding a JSON array of flat objects. Field
//! names are the Flutter client's — name parity, not on-disk compatibility:
//!
//! | Field | Type | Domain field |
//! |---|---|---|
//! | `host` | string | [`SavedServer::host`] |
//! | `port` | number | [`SavedServer::port`] |
//! | `trustMode` | string | [`SavedServer::trust_mode`] (see [`trust_mode_name`]) |
//! | `insecure` | bool | *derived*, written for older readers only |
//! | `label` | string | [`SavedServer::label`] |
//! | `lastUsedMs` | number | [`SavedServer::last_used_ms`] |
//! | `readOnly` | bool | [`SavedServer::read_only`] |
//! | `hasCredential` | bool | [`SavedServer::has_credential`] |
//! | `tokenName` | string/null | [`SavedServer::token_name`] |
//! | `fingerprint` | string/null | [`SavedServer::fingerprint`] |
//!
//! # Never fails the load
//!
//! A malformed payload decodes to an empty list, never a panic; inside a
//! well-formed array one unusable entry is skipped without failing its
//! neighbours, unknown fields are ignored, and absent fields default.
//!
//! Legacy records migrate: no `trustMode` derives one — `insecure: true`
//! becomes [`TrustMode::NoVerification`], a bare `fingerprint` becomes
//! [`TrustMode::Pinned`], anything else [`TrustMode::System`]; an unrecognised
//! `trustMode` falls back to the same derivation. The stored string for
//! `NoVerification` remains `"insecure"` — that variant's Rust rename touched
//! no byte on disk.
//!
//! A fingerprint is kept only for [`TrustMode::Pinned`], and only when it's 64
//! lowercase hex characters; a `Pinned` entry with a missing/malformed one
//! stays `Pinned` without one, failing closed at connect time.
//!
//! # Reads are a mirror; writes are fire-and-forget
//!
//! The list loads once at construction; every read answers from that mirror,
//! keeping [`SavedServersRepository`]'s synchronous contract honest (a save
//! followed by a list must see the save). The write is spawned on
//! [`frust::spawn_blocking`] and its failure is logged, never surfaced. The
//! payload is encoded inside that task, from the shared mirror, so two writes
//! landing out of order both persist the current list.

use std::cmp::Reverse;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use frust::spawn_blocking;
use frust_shared_preferences::{PrefsError, SharedPreferences};

use super::json::{self, Json};
use crate::failure::AppFailure;
use crate::features::connect::domain::entities::{SavedServer, TrustMode};
use crate::features::connect::domain::repositories::SavedServersRepository;

/// The one preferences key the whole list lives under — the Flutter client's
/// own (`server_store.dart`'s `_key`).
const KEY_SAVED_SERVERS: &str = "zelli_saved_servers";

/// How long a certificate fingerprint is, in hex characters.
const FINGERPRINT_LEN: usize = 64;

/// The real [`SavedServersRepository`]. See the [module docs](self).
pub struct PrefsSavedServersRepository {
    store: SharedPreferences,
    /// The authoritative list for this process (see the module docs' mirror
    /// note); shared with each in-flight write task, which re-encodes from it.
    servers: Arc<Mutex<Vec<SavedServer>>>,
}

impl PrefsSavedServersRepository {
    /// Opens the platform's standard preferences store and loads the
    /// saved-server list out of it.
    ///
    /// # Errors
    /// [`SharedPreferences::standard`]'s own contract. A malformed payload is
    /// not an error — it loads as empty.
    pub fn standard() -> Result<Self, PrefsError> {
        let store = SharedPreferences::standard()?;
        let servers = store
            .get_string(KEY_SAVED_SERVERS)
            .map(|raw| decode_list(&raw))
            .unwrap_or_default();
        Ok(Self {
            store,
            servers: Arc::new(Mutex::new(servers)),
        })
    }

    /// Recovers a poisoned lock rather than propagating it.
    fn mirror(&self) -> MutexGuard<'_, Vec<SavedServer>> {
        self.servers.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Persists the current mirror, off the UI thread. See the module docs.
    fn persist(&self) {
        let store = self.store.clone();
        let servers = Arc::clone(&self.servers);
        spawn_blocking(move || {
            let payload = {
                let servers = servers.lock().unwrap_or_else(PoisonError::into_inner);
                encode_list(&servers)
            };
            if let Err(err) = store.set_string(KEY_SAVED_SERVERS, payload) {
                log::warn!("saved servers: failed to persist the list: {err}");
            }
        });
    }
}

impl SavedServersRepository for PrefsSavedServersRepository {
    fn list(&self) -> Result<Vec<SavedServer>, AppFailure> {
        Ok(self.mirror().clone())
    }

    fn save(&self, server: SavedServer) -> Result<(), AppFailure> {
        {
            let mut servers = self.mirror();
            upsert(&mut servers, server);
        }
        self.persist();
        Ok(())
    }

    fn remove(&self, endpoint: &str) -> Result<(), AppFailure> {
        {
            let mut servers = self.mirror();
            servers.retain(|existing| existing.endpoint() != endpoint);
        }
        self.persist();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The list's shared ordering rule
// ---------------------------------------------------------------------------

/// Inserts `server`, replacing any row with the same endpoint, and
/// re-establishes the most-recently-used-first order (new row goes in first
/// under a stable sort, so it wins any tie). Shared by both implementations
/// of the contract, so they can't answer a `list()` in different orders.
pub(super) fn upsert(servers: &mut Vec<SavedServer>, server: SavedServer) {
    let endpoint = server.endpoint();
    servers.retain(|existing| existing.endpoint() != endpoint);
    servers.insert(0, server);
    servers.sort_by_key(|server| Reverse(server.last_used_ms));
}

// ---------------------------------------------------------------------------
// The schema
// ---------------------------------------------------------------------------

pub(super) fn encode_list(servers: &[SavedServer]) -> String {
    Json::Array(servers.iter().map(encode_entry).collect()).encode()
}

fn encode_entry(server: &SavedServer) -> Json {
    Json::Object(vec![
        ("host".to_string(), Json::String(server.host.clone())),
        ("port".to_string(), Json::Number(f64::from(server.port))),
        (
            "trustMode".to_string(),
            Json::String(trust_mode_name(server.trust_mode).to_string()),
        ),
        // An older build knows this field, not `trustMode`, and would
        // otherwise read an insecure rig as CA-verified.
        (
            "insecure".to_string(),
            Json::Bool(server.trust_mode == TrustMode::NoVerification),
        ),
        ("label".to_string(), Json::String(server.label.clone())),
        (
            "lastUsedMs".to_string(),
            // Well inside the range an `f64` holds exactly (see `json`'s own docs).
            Json::Number(server.last_used_ms as f64),
        ),
        ("readOnly".to_string(), Json::Bool(server.read_only)),
        (
            "hasCredential".to_string(),
            Json::Bool(server.has_credential),
        ),
        ("tokenName".to_string(), optional(server.token_name.clone())),
        (
            "fingerprint".to_string(),
            // Only a pinned row stores one.
            optional(match server.trust_mode {
                TrustMode::Pinned => server.fingerprint.clone(),
                _ => None,
            }),
        ),
    ])
}

fn optional(value: Option<String>) -> Json {
    value.map_or(Json::Null, Json::String)
}

/// Malformed → empty (see the [module docs](self)).
pub(super) fn decode_list(raw: &str) -> Vec<SavedServer> {
    let Some(Json::Array(entries)) = json::parse(raw) else {
        return Vec::new();
    };
    let mut servers: Vec<SavedServer> = entries.iter().filter_map(decode_entry).collect();
    servers.sort_by_key(|server| Reverse(server.last_used_ms));
    servers
}

/// `None` when the entry names no dialable endpoint.
fn decode_entry(entry: &Json) -> Option<SavedServer> {
    let host = entry.get("host").and_then(Json::as_str)?;
    if host.is_empty() {
        return None;
    }
    let port = u16::try_from(entry.get("port").and_then(Json::as_i64)?).ok()?;

    let fingerprint = entry
        .get("fingerprint")
        .and_then(Json::as_str)
        .filter(|value| is_fingerprint(value))
        .map(str::to_string);
    // Back-compat derivation for an entry stored before `trustMode` existed.
    let legacy_insecure = entry
        .get("insecure")
        .and_then(Json::as_bool)
        .unwrap_or(false);
    let derived = if legacy_insecure {
        TrustMode::NoVerification
    } else if fingerprint.is_some() {
        TrustMode::Pinned
    } else {
        TrustMode::System
    };
    let trust_mode = entry
        .get("trustMode")
        .and_then(Json::as_str)
        .and_then(trust_mode_from_name)
        .unwrap_or(derived);

    Some(SavedServer {
        label: entry
            .get("label")
            .and_then(Json::as_str)
            .filter(|label| !label.is_empty())
            .unwrap_or(host)
            .to_string(),
        host: host.to_string(),
        port,
        trust_mode,
        last_used_ms: entry
            .get("lastUsedMs")
            .and_then(Json::as_i64)
            .unwrap_or_default(),
        read_only: entry
            .get("readOnly")
            .and_then(Json::as_bool)
            .unwrap_or(false),
        has_credential: entry
            .get("hasCredential")
            .and_then(Json::as_bool)
            .unwrap_or(false),
        token_name: entry
            .get("tokenName")
            .and_then(Json::as_str)
            .filter(|name| !name.is_empty())
            .map(str::to_string),
        // A pin belongs only to the posture that pins.
        fingerprint: match trust_mode {
            TrustMode::Pinned => fingerprint,
            _ => None,
        },
    })
}

/// 64 lowercase hex characters — the same shape `crate::net::tls` accepts at
/// dial time.
fn is_fingerprint(value: &str) -> bool {
    value.len() == FINGERPRINT_LEN
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The Flutter client's spellings, so the two clients' records agree on what
/// a mode is called.
fn trust_mode_name(mode: TrustMode) -> &'static str {
    match mode {
        TrustMode::System => "systemCa",
        TrustMode::NoVerification => "insecure",
        TrustMode::Pinned => "selfSignedPinned",
    }
}

/// [`trust_mode_name`]'s inverse; `None` for an unknown name, sending the
/// caller to the legacy derivation rather than a guess.
fn trust_mode_from_name(name: &str) -> Option<TrustMode> {
    match name {
        "systemCa" => Some(TrustMode::System),
        "insecure" => Some(TrustMode::NoVerification),
        "selfSignedPinned" => Some(TrustMode::Pinned),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fingerprint() -> String {
        "ab".repeat(32)
    }

    fn server(host: &str, port: u16, last_used_ms: i64) -> SavedServer {
        SavedServer {
            host: host.to_string(),
            port,
            trust_mode: TrustMode::System,
            label: host.to_string(),
            last_used_ms,
            read_only: false,
            has_credential: false,
            token_name: None,
            fingerprint: None,
        }
    }

    // -----------------------------------------------------------------------
    // Round trips
    // -----------------------------------------------------------------------

    #[test]
    fn every_field_survives_a_round_trip() {
        let mut stored = server("100.71.31.57", 50051, 1_700_000_000_000);
        stored.trust_mode = TrustMode::Pinned;
        stored.label = "Home rig".to_string();
        stored.read_only = true;
        stored.has_credential = true;
        stored.token_name = Some("muxr-mobile-home-abc".to_string());
        stored.fingerprint = Some(fingerprint());

        let decoded = decode_list(&encode_list(std::slice::from_ref(&stored)));

        assert_eq!(decoded, vec![stored]);
    }

    #[test]
    fn the_encoded_payload_is_an_array_of_the_flutter_field_names() {
        let payload = encode_list(&[server("rig.example", 50051, 7)]);
        for field in [
            "\"host\"",
            "\"port\"",
            "\"trustMode\"",
            "\"insecure\"",
            "\"label\"",
            "\"lastUsedMs\"",
            "\"readOnly\"",
            "\"hasCredential\"",
            "\"tokenName\"",
            "\"fingerprint\"",
        ] {
            assert!(payload.contains(field), "{field} missing from {payload}");
        }
        assert!(payload.starts_with('['), "{payload}");
    }

    #[test]
    fn the_legacy_insecure_bool_is_still_written_beside_the_trust_mode() {
        let mut unverified = server("rig.example", 50051, 0);
        unverified.trust_mode = TrustMode::NoVerification;
        let payload = encode_list(&[unverified]);
        assert!(payload.contains(r#""trustMode":"insecure""#), "{payload}");
        assert!(payload.contains(r#""insecure":true"#), "{payload}");

        let payload = encode_list(&[server("rig.example", 50051, 0)]);
        assert!(payload.contains(r#""trustMode":"systemCa""#), "{payload}");
        assert!(payload.contains(r#""insecure":false"#), "{payload}");
    }

    #[test]
    fn a_stamp_is_written_as_a_whole_number_rather_than_a_float() {
        let payload = encode_list(&[server("rig.example", 50051, 1_700_000_000_000)]);
        assert!(
            payload.contains(r#""lastUsedMs":1700000000000"#),
            "{payload}"
        );
    }

    #[test]
    fn the_list_decodes_most_recently_used_first() {
        let payload = encode_list(&[
            server("older.example", 1, 100),
            server("newest.example", 2, 300),
            server("middle.example", 3, 200),
        ]);
        let endpoints: Vec<String> = decode_list(&payload)
            .iter()
            .map(SavedServer::endpoint)
            .collect();
        assert_eq!(
            endpoints,
            vec![
                "newest.example:2".to_string(),
                "middle.example:3".to_string(),
                "older.example:1".to_string()
            ]
        );
    }

    // -----------------------------------------------------------------------
    // Legacy migration
    // -----------------------------------------------------------------------

    #[test]
    fn a_legacy_insecure_record_migrates_to_the_no_verification_posture() {
        let decoded = decode_list(r#"[{"host":"rig","port":50051,"insecure":true}]"#);
        assert_eq!(decoded[0].trust_mode, TrustMode::NoVerification);
    }

    #[test]
    fn a_legacy_fingerprint_record_migrates_to_the_pinned_posture() {
        let raw = format!(
            r#"[{{"host":"rig","port":50051,"fingerprint":"{}"}}]"#,
            fingerprint()
        );
        let decoded = decode_list(&raw);
        assert_eq!(decoded[0].trust_mode, TrustMode::Pinned);
        assert_eq!(decoded[0].fingerprint, Some(fingerprint()));
    }

    #[test]
    fn a_legacy_record_with_neither_hint_migrates_to_system_trust() {
        let decoded = decode_list(r#"[{"host":"rig","port":50051}]"#);
        assert_eq!(decoded[0].trust_mode, TrustMode::System);
        assert_eq!(decoded[0].label, "rig", "an unlabelled row is its own host");
        assert_eq!(decoded[0].last_used_ms, 0);
        assert!(!decoded[0].has_credential);
        assert_eq!(decoded[0].token_name, None);
    }

    #[test]
    fn an_unknown_trust_mode_name_falls_back_to_the_legacy_derivation() {
        let decoded =
            decode_list(r#"[{"host":"rig","port":50051,"trustMode":"quantum","insecure":true}]"#);
        assert_eq!(
            decoded[0].trust_mode,
            TrustMode::NoVerification,
            "a mode this build never heard of must not read as the safest one by accident"
        );
    }

    #[test]
    fn an_explicit_trust_mode_outranks_the_legacy_bool() {
        let decoded =
            decode_list(r#"[{"host":"rig","port":50051,"trustMode":"systemCa","insecure":true}]"#);
        assert_eq!(decoded[0].trust_mode, TrustMode::System);
    }

    // -----------------------------------------------------------------------
    // Tolerance
    // -----------------------------------------------------------------------

    #[test]
    fn a_malformed_payload_decodes_to_an_empty_list_rather_than_crashing() {
        for raw in [
            "",
            "not json",
            "{",
            "{\"host\":\"rig\"}",
            "null",
            "42",
            "[1,]",
        ] {
            assert!(decode_list(raw).is_empty(), "{raw:?}");
        }
    }

    #[test]
    fn an_unusable_entry_is_skipped_without_taking_its_neighbours_with_it() {
        let raw = concat!(
            r#"[{"port":50051},"#,                    // no host
            r#"{"host":"rig","port":99999},"#,        // port out of range
            r#"{"host":"","port":1},"#,               // empty host
            r#"{"host":"good","port":50051,"x":1}]"#, // …and one that works
        );
        let decoded = decode_list(raw);
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].endpoint(), "good:50051");
    }

    #[test]
    fn unknown_fields_are_ignored_rather_than_rejected() {
        let decoded =
            decode_list(r#"[{"host":"rig","port":1,"futureField":{"nested":true},"label":"L"}]"#);
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].label, "L");
    }

    #[test]
    fn a_wrong_typed_field_falls_back_to_its_own_default() {
        let decoded =
            decode_list(r#"[{"host":"rig","port":1,"readOnly":"yes","lastUsedMs":"soon"}]"#);
        assert!(!decoded[0].read_only);
        assert_eq!(decoded[0].last_used_ms, 0);
    }

    // -----------------------------------------------------------------------
    // The fingerprint invariant
    // -----------------------------------------------------------------------

    #[test]
    fn a_fingerprint_is_kept_only_for_the_posture_that_pins() {
        let raw = format!(
            r#"[{{"host":"rig","port":1,"trustMode":"systemCa","fingerprint":"{}"}}]"#,
            fingerprint()
        );
        assert_eq!(
            decode_list(&raw)[0].fingerprint,
            None,
            "a system-trust row must never carry a pin"
        );

        let mut pinned = server("rig", 1, 0);
        pinned.trust_mode = TrustMode::System;
        pinned.fingerprint = Some(fingerprint());
        let payload = encode_list(&[pinned]);
        assert!(payload.contains(r#""fingerprint":null"#), "{payload}");
    }

    #[test]
    fn a_malformed_pin_decodes_as_absent_so_the_dial_fails_closed() {
        for value in ["", "not-hex", "AB", &"ab".repeat(31), &"AB".repeat(32)] {
            let raw = format!(
                r#"[{{"host":"rig","port":1,"trustMode":"selfSignedPinned","fingerprint":"{value}"}}]"#
            );
            let decoded = decode_list(&raw);
            assert_eq!(decoded[0].trust_mode, TrustMode::Pinned, "{value:?}");
            assert_eq!(
                decoded[0].fingerprint, None,
                "a pin that cannot be parsed is no pin at all: {value:?}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Ordering
    // -----------------------------------------------------------------------

    #[test]
    fn an_upsert_replaces_by_endpoint_rather_than_appending() {
        let mut servers = vec![server("a.example", 1, 10), server("b.example", 2, 20)];
        let mut renamed = server("a.example", 1, 30);
        renamed.label = "renamed".to_string();

        upsert(&mut servers, renamed);

        assert_eq!(servers.len(), 2, "no duplicate row");
        assert_eq!(servers[0].label, "renamed");
    }

    #[test]
    fn an_upsert_that_ties_on_recency_still_lands_first() {
        let mut servers = vec![server("a.example", 1, 10), server("b.example", 2, 10)];
        upsert(&mut servers, server("c.example", 3, 10));
        assert_eq!(servers[0].endpoint(), "c.example:3");
    }
}
