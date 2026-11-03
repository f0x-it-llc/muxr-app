//! The in-memory keyring and its vault codec — framework-free.
//!
//! [`Keyring`] maps a server endpoint (`host:port`) to its auth token, held in
//! [`Zeroizing`] strings so every copy scrubs on drop. It has one owner (no
//! `Clone`) and a redacting `Debug`. [`KeyringVault`] is the codec for the
//! single persisted blob: JSON `{"version":1,"entries":{"host:port":
//! {"token":"…","savedAtMs":<i64>}}}`.
//!
//! Codec placement: it lives here in `domain` because `serde_json` is a plain
//! crate dependency (no `frust::`), keeping the strict parse next to its tests.
//! Decoding is strict — a corrupted vault is a [`VaultError`], never an empty
//! keyring, so a storage failure cannot masquerade as "no credentials".

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use zeroize::Zeroizing;

/// The only vault schema version this build reads or writes.
const VAULT_VERSION: i64 = 1;

/// Endpoint -> token, in memory. See the [module docs](self).
#[derive(Default)]
pub struct Keyring {
    entries: BTreeMap<String, Zeroizing<String>>,
}

impl Keyring {
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts `token` for `endpoint`, replacing (and scrubbing) any previous one.
    pub fn insert(&mut self, endpoint: impl Into<String>, token: Zeroizing<String>) {
        self.entries.insert(endpoint.into(), token);
    }

    /// Removes `endpoint`'s token; true when one was present.
    pub fn remove(&mut self, endpoint: &str) -> bool {
        self.entries.remove(endpoint).is_some()
    }

    /// A clone of the secret for `endpoint`.
    pub fn token_for(&self, endpoint: &str) -> Option<Zeroizing<String>> {
        self.entries.get(endpoint).cloned()
    }

    pub fn endpoints(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Zeroizes every token and clears the map.
    pub fn lock(&mut self) {
        // Dropping a `Zeroizing` scrubs it.
        self.entries.clear();
    }
}

impl std::fmt::Debug for Keyring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keyring")
            .field("endpoints", &self.entries.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// A vault blob that could not be read as a version-1 keyring.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VaultError(pub String);

impl std::fmt::Display for VaultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unreadable keyring vault: {}", self.0)
    }
}

impl std::error::Error for VaultError {}

fn bad(reason: &str) -> VaultError {
    VaultError(reason.to_string())
}

/// Encoder/decoder for the persisted vault blob.
pub struct KeyringVault;

impl KeyringVault {
    /// Serializes `keyring`; `saved_at_ms` stamps each endpoint's entry.
    pub fn encode(keyring: &Keyring, saved_at_ms: impl Fn(&str) -> i64) -> String {
        let entries: Map<String, Value> = keyring
            .entries
            .iter()
            .map(|(endpoint, token)| {
                (
                    endpoint.clone(),
                    json!({ "token": token.as_str(), "savedAtMs": saved_at_ms(endpoint) }),
                )
            })
            .collect();
        json!({ "version": VAULT_VERSION, "entries": entries }).to_string()
    }

    /// Strictly parses a vault blob.
    pub fn decode(raw: &str) -> Result<Keyring, VaultError> {
        let root: Value = serde_json::from_str(raw).map_err(|_| bad("not valid JSON"))?;
        let root = root
            .as_object()
            .ok_or_else(|| bad("root is not an object"))?;
        match root.get("version").and_then(Value::as_i64) {
            Some(VAULT_VERSION) => {}
            _ => return Err(bad("unknown or missing version")),
        }
        let entries = root
            .get("entries")
            .and_then(Value::as_object)
            .ok_or_else(|| bad("entries is missing or not an object"))?;
        let mut keyring = Keyring::new();
        for (endpoint, entry) in entries {
            let token = entry
                .get("token")
                .and_then(Value::as_str)
                .ok_or_else(|| bad("an entry's token is missing or not a string"))?;
            keyring.insert(endpoint.clone(), Zeroizing::new(token.to_string()));
        }
        Ok(keyring)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Keyring {
        let mut keyring = Keyring::new();
        keyring.insert("a.example:50051", Zeroizing::new("tok-one".to_string()));
        keyring.insert(
            "b.example:50052",
            Zeroizing::new("tökén-✓-日本".to_string()),
        );
        keyring
    }

    #[test]
    fn codec_round_trips_two_entries_with_unicode() {
        let encoded = KeyringVault::encode(&sample(), |e| e.len() as i64);
        let decoded = KeyringVault::decode(&encoded).expect("decodes");
        assert_eq!(decoded.len(), 2);
        assert_eq!(
            decoded.token_for("b.example:50052").unwrap().as_str(),
            "tökén-✓-日本"
        );
        assert_eq!(
            decoded.token_for("a.example:50051").unwrap().as_str(),
            "tok-one"
        );
        let value: Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(value["version"], 1);
        assert_eq!(value["entries"]["a.example:50051"]["savedAtMs"], 15);
    }

    #[test]
    fn decode_rejects_malformed_vaults() {
        for raw in [
            "not json",
            "[]",
            r#"{"version":2,"entries":{}}"#,
            r#"{"entries":{}}"#,
            r#"{"version":"1","entries":{}}"#,
            r#"{"version":1}"#,
            r#"{"version":1,"entries":[]}"#,
            r#"{"version":1,"entries":{"h:1":{"token":5,"savedAtMs":1}}}"#,
            r#"{"version":1,"entries":{"h:1":{"savedAtMs":1}}}"#,
            r#"{"version":1,"entries":{"h:1":"tok"}}"#,
        ] {
            assert!(KeyringVault::decode(raw).is_err(), "should reject {raw}");
        }
    }

    #[test]
    fn decode_accepts_empty_entries() {
        assert!(
            KeyringVault::decode(r#"{"version":1,"entries":{}}"#)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn lock_empties_and_hides_tokens() {
        let mut keyring = sample();
        keyring.lock();
        assert!(keyring.is_empty());
        assert!(keyring.token_for("a.example:50051").is_none());
    }

    #[test]
    fn insert_remove_and_endpoints() {
        let mut keyring = sample();
        assert_eq!(
            keyring.endpoints(),
            vec!["a.example:50051", "b.example:50052"]
        );
        assert!(keyring.remove("a.example:50051"));
        assert!(!keyring.remove("a.example:50051"));
        assert_eq!(keyring.len(), 1);
    }

    #[test]
    fn debug_never_prints_tokens() {
        let keyring = sample();
        let text = format!("{keyring:?}");
        assert!(text.contains("a.example:50051"));
        assert!(!text.contains("tok-one") && !text.contains("tökén"));
        let unlocked = super::super::entities::UnlockedKeyring {
            status: super::super::entities::CredentialStatus::Success,
            keyring: Some(keyring),
            migrated: vec![],
            unreadable: vec![],
        };
        let text = format!("{unlocked:?}");
        assert!(!text.contains("tok-one") && !text.contains("tökén"));
    }
}
