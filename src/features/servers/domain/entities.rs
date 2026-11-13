//! The switcher's plain-data model: one [`ServerFace`] per remembered server, ordered and classified by the pure [`faces_for`].
//! Reuses `connect`'s [`SavedServer`] and [`TrustMode`] and `sessions`' [`Session`] rather than mirroring them.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::connection::{ServerInfo, TrustMode};
use crate::features::connect::domain::entities::SavedServer;
use crate::features::sessions::domain::entities::Session;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionGlimpse {
    pub name: String,
    pub backend: String,
    pub running: bool,
    pub meta_line: String,
}

/// Plain data, never a secret: `ServerInfo` carries no token.
#[derive(Clone, Debug, PartialEq)]
pub struct PageSnapshot {
    pub sessions: Vec<Session>,
    pub info: Option<ServerInfo>,
    pub read_only: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SeenServer {
    pub glimpses: Vec<SessionGlimpse>,
    pub seen_at_ms: i64,
    /// Shared, so a strip refresh copies a pointer, not the list.
    pub page: Option<Arc<PageSnapshot>>,
}

/// What each server last showed this process, by endpoint. Never persisted.
pub type LastSeen = BTreeMap<String, SeenServer>;

#[derive(Clone, Debug, PartialEq)]
pub enum FaceKind {
    /// The connected server; the switcher scales the real page instead of drawing a face.
    Live,
    Cached {
        sessions: Vec<SessionGlimpse>,
        last_seen_ms: i64,
        page: Option<Arc<PageSnapshot>>,
    },
    Never,
    Locked,
    Failed {
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ServerFace {
    pub endpoint: String,
    pub label: String,
    pub trust_mode: TrustMode,
    pub read_only: bool,
    pub kind: FaceKind,
}

/// A saved row `order` does not name goes after the named ones; nothing reorders on use. Non-active rows are `Failed`, else `Locked`, else `Cached`, else `Never`.
pub fn faces_for(
    saved: &[SavedServer],
    order: &[String],
    active: Option<&str>,
    keyring_unlocked: bool,
    cache: &LastSeen,
    failures: &BTreeMap<String, String>,
) -> Vec<ServerFace> {
    let mut rows: Vec<&SavedServer> = saved.iter().collect();
    // Stable sort: unnamed rows (key `order.len()`) keep their saved order.
    rows.sort_by_key(|row| {
        let endpoint = row.endpoint();
        order
            .iter()
            .position(|e| *e == endpoint)
            .unwrap_or(order.len())
    });
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
            } else if let Some(seen) = cache.get(&endpoint) {
                FaceKind::Cached {
                    sessions: seen.glimpses.clone(),
                    last_seen_ms: seen.seen_at_ms,
                    page: seen.page.clone(),
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

    fn glimpse() -> SeenServer {
        SeenServer {
            glimpses: vec![SessionGlimpse {
                name: "main".to_string(),
                backend: "zellij".to_string(),
                running: true,
                meta_line: "1h".to_string(),
            }],
            seen_at_ms: 42,
            page: None,
        }
    }

    fn endpoints(faces: &[ServerFace]) -> Vec<&str> {
        faces.iter().map(|f| f.endpoint.as_str()).collect()
    }

    fn order(of: &[&str]) -> Vec<String> {
        of.iter().map(|h| format!("{h}:7777")).collect()
    }

    #[test]
    fn the_active_server_keeps_its_slot_whatever_its_recency() {
        let saved = [row("a", 10, true), row("b", 30, true), row("c", 20, true)];
        let o = order(&["a", "b", "c"]);
        for active in ["a:7777", "b:7777", "c:7777"] {
            let faces = faces_for(
                &saved,
                &o,
                Some(active),
                true,
                &LastSeen::new(),
                &BTreeMap::new(),
            );
            assert_eq!(endpoints(&faces), ["a:7777", "b:7777", "c:7777"]);
            assert_eq!(
                faces.iter().position(|f| f.kind == FaceKind::Live),
                o.iter().position(|e| e == active)
            );
        }
        let none_active = faces_for(&saved, &o, None, true, &LastSeen::new(), &BTreeMap::new());
        assert_eq!(endpoints(&none_active), ["a:7777", "b:7777", "c:7777"]);
        assert!(none_active.iter().all(|f| f.kind != FaceKind::Live));
    }

    #[test]
    fn a_new_row_appends_and_a_removed_row_drops() {
        let o = order(&["a", "b"]);
        let grown = [row("c", 99, true), row("b", 1, true), row("a", 2, true)];
        let faces = faces_for(&grown, &o, None, true, &LastSeen::new(), &BTreeMap::new());
        assert_eq!(endpoints(&faces), ["a:7777", "b:7777", "c:7777"]);
        let shrunk = [row("b", 1, true)];
        let faces = faces_for(&shrunk, &o, None, true, &LastSeen::new(), &BTreeMap::new());
        assert_eq!(endpoints(&faces), ["b:7777"]);
    }

    #[test]
    fn a_shut_keyring_locks_only_the_rows_that_claim_a_credential() {
        let saved = [row("a", 3, true), row("b", 2, false)];
        let faces = faces_for(&saved, &[], None, false, &LastSeen::new(), &BTreeMap::new());
        assert_eq!(faces[0].kind, FaceKind::Locked);
        assert_eq!(faces[1].kind, FaceKind::Never);

        let open = faces_for(&saved, &[], None, true, &LastSeen::new(), &BTreeMap::new());
        assert_eq!(open[0].kind, FaceKind::Never);
    }

    #[test]
    fn a_cached_row_shows_what_it_last_listed() {
        let mut cache = LastSeen::new();
        cache.insert("a:7777".to_string(), glimpse());
        let faces = faces_for(
            &[row("a", 1, true)],
            &[],
            None,
            true,
            &cache,
            &BTreeMap::new(),
        );
        let seen = glimpse();
        assert_eq!(
            faces[0].kind,
            FaceKind::Cached {
                sessions: seen.glimpses,
                last_seen_ms: seen.seen_at_ms,
                page: None,
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

        let faces = faces_for(&saved, &[], Some("b:7777"), false, &cache, &failures);
        assert_eq!(faces[1].kind, FaceKind::Live, "the active row is live");
        assert_eq!(
            faces[0].kind,
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
        let faces = faces_for(
            &[saved],
            &[],
            None,
            true,
            &LastSeen::new(),
            &BTreeMap::new(),
        );
        assert_eq!(faces[0].label, "rig");
        assert_eq!(faces[0].trust_mode, TrustMode::Pinned);
        assert!(faces[0].read_only);
    }
}
