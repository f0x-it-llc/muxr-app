//! The switcher's plain-data model: one [`ServerFace`] per remembered server,
//! and the pure [`faces_for`] that orders and classifies them.
//!
//! Deviation: reads `connect`'s own [`SavedServer`] and [`TrustMode`] rather
//! than minting a second saved-server type (the `pro` domain does the same).

use std::collections::BTreeMap;

use crate::connection::TrustMode;
use crate::features::connect::domain::entities::SavedServer;

/// One session as a non-live face remembers it. Strings only, so the
/// `sessions` slice can fill it without sharing a type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionGlimpse {
    pub name: String,
    /// The backend's short label (`zellij`, `herdr`).
    pub backend: String,
    pub running: bool,
    /// The dim second line (`2h · 3 tabs · 5 panes`).
    pub meta_line: String,
}

/// What each server last showed this process: endpoint to its glimpses and
/// the epoch-ms stamp they were seen at. Never persisted, never a secret.
pub type LastSeen = BTreeMap<String, (Vec<SessionGlimpse>, i64)>;

/// What a card shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FaceKind {
    /// The connected server; the switcher scales the real page instead of
    /// drawing a face.
    Live,
    /// Not connected, but listed earlier in this process.
    Cached {
        sessions: Vec<SessionGlimpse>,
        last_seen_ms: i64,
    },
    /// Never connected in this process, and nothing to unlock.
    Never,
    /// A stored credential the keyring has not opened yet.
    Locked,
    /// The last switch to it failed.
    Failed { message: String },
}

/// One card of the strip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerFace {
    /// `host:port`, the row's identity.
    pub endpoint: String,
    pub label: String,
    pub trust_mode: TrustMode,
    pub read_only: bool,
    pub kind: FaceKind,
}

/// The strip: `saved` most recently used first with the `active` endpoint
/// moved to the front. A non-active row is `Failed` when `failures` names it,
/// else `Locked` when it claims a credential and the keyring is shut, else
/// `Cached` when `cache` holds it, else `Never`.
pub fn faces_for(
    saved: &[SavedServer],
    active: Option<&str>,
    keyring_unlocked: bool,
    cache: &LastSeen,
    failures: &BTreeMap<String, String>,
) -> Vec<ServerFace> {
    let mut rows: Vec<&SavedServer> = saved.iter().collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row.last_used_ms));
    if let Some(at) = rows
        .iter()
        .position(|row| active == Some(row.endpoint().as_str()))
    {
        let live = rows.remove(at);
        rows.insert(0, live);
    }
    rows.into_iter()
        .map(|row| {
            let endpoint = row.endpoint();
            let kind = if active == Some(endpoint.as_str()) {
                FaceKind::Live
            } else if let Some(message) = failures.get(&endpoint) {
                FaceKind::Failed {
                    message: message.clone(),
                }
            } else if row.has_credential && !keyring_unlocked {
                FaceKind::Locked
            } else if let Some((sessions, last_seen_ms)) = cache.get(&endpoint) {
                FaceKind::Cached {
                    sessions: sessions.clone(),
                    last_seen_ms: *last_seen_ms,
                }
            } else {
                FaceKind::Never
            };
            ServerFace {
                endpoint,
                label: row.label.clone(),
                trust_mode: row.trust_mode,
                read_only: row.read_only,
                kind,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(host: &str, last_used_ms: i64, has_credential: bool) -> SavedServer {
        SavedServer {
            host: host.to_string(),
            port: 7777,
            trust_mode: TrustMode::System,
            label: host.to_string(),
            last_used_ms,
            read_only: false,
            has_credential,
            token_name: None,
            fingerprint: None,
        }
    }

    fn glimpse() -> (Vec<SessionGlimpse>, i64) {
        (
            vec![SessionGlimpse {
                name: "main".to_string(),
                backend: "zellij".to_string(),
                running: true,
                meta_line: "1h".to_string(),
            }],
            42,
        )
    }

    fn endpoints(faces: &[ServerFace]) -> Vec<&str> {
        faces.iter().map(|f| f.endpoint.as_str()).collect()
    }

    #[test]
    fn faces_are_most_recently_used_first_with_the_active_server_leading() {
        let saved = [row("a", 10, true), row("b", 30, true), row("c", 20, true)];
        let faces = faces_for(
            &saved,
            Some("a:7777"),
            true,
            &LastSeen::new(),
            &BTreeMap::new(),
        );
        assert_eq!(endpoints(&faces), ["a:7777", "b:7777", "c:7777"]);
        assert_eq!(faces[0].kind, FaceKind::Live);

        let none_active = faces_for(&saved, None, true, &LastSeen::new(), &BTreeMap::new());
        assert_eq!(endpoints(&none_active), ["b:7777", "c:7777", "a:7777"]);
        assert!(none_active.iter().all(|f| f.kind != FaceKind::Live));
    }

    #[test]
    fn a_shut_keyring_locks_only_the_rows_that_claim_a_credential() {
        let saved = [row("a", 3, true), row("b", 2, false)];
        let faces = faces_for(&saved, None, false, &LastSeen::new(), &BTreeMap::new());
        assert_eq!(faces[0].kind, FaceKind::Locked);
        assert_eq!(faces[1].kind, FaceKind::Never);

        let open = faces_for(&saved, None, true, &LastSeen::new(), &BTreeMap::new());
        assert_eq!(open[0].kind, FaceKind::Never);
    }

    #[test]
    fn a_cached_row_shows_what_it_last_listed() {
        let mut cache = LastSeen::new();
        cache.insert("a:7777".to_string(), glimpse());
        let faces = faces_for(&[row("a", 1, true)], None, true, &cache, &BTreeMap::new());
        let (sessions, last_seen_ms) = glimpse();
        assert_eq!(
            faces[0].kind,
            FaceKind::Cached {
                sessions,
                last_seen_ms
            }
        );
    }

    #[test]
    fn a_failure_outranks_lock_and_cache_but_never_the_live_face() {
        let mut cache = LastSeen::new();
        cache.insert("a:7777".to_string(), glimpse());
        let mut failures = BTreeMap::new();
        failures.insert("a:7777".to_string(), "down".to_string());
        failures.insert("b:7777".to_string(), "down".to_string());
        let saved = [row("a", 2, true), row("b", 1, true)];

        let faces = faces_for(&saved, Some("b:7777"), false, &cache, &failures);
        assert_eq!(faces[0].kind, FaceKind::Live, "the active row is live");
        assert_eq!(
            faces[1].kind,
            FaceKind::Failed {
                message: "down".to_string()
            }
        );
    }

    #[test]
    fn faces_carry_the_rows_label_posture_and_read_only_flag() {
        let mut saved = row("a", 1, false);
        saved.read_only = true;
        saved.trust_mode = TrustMode::Pinned;
        saved.label = "rig".to_string();
        let faces = faces_for(&[saved], None, true, &LastSeen::new(), &BTreeMap::new());
        assert_eq!(faces[0].label, "rig");
        assert_eq!(faces[0].trust_mode, TrustMode::Pinned);
        assert!(faces[0].read_only);
    }
}
