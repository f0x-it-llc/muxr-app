//! [`MockAuthTokensRepository`] — the in-memory [`AuthTokensRepository`] the
//! mock era runs on.
//!
//! Seeds **exactly** the two tokens `docs/redesign.html`'s `auth tokens` group
//! draws — `zelli-mobile-100-71-31-57-mr3g6w5g` (read-write, created
//! 2026-07-02) and `token_2` (read-only, created 2026-07-10) — so the screen
//! and the mock are the same picture, and both scope chips are reachable from
//! the seed. It mutates that set in place on create/revoke, so the screen
//! proves a real round trip rather than patching its own signal.
//!
//! # Framework-free, and deliberately not `frust`-aware
//!
//! No `frust::` types here (the data-layer rule): the store is a plain
//! [`Mutex<Vec<AuthToken>>`]. The mutex — not a `RefCell` — is what makes this
//! `Send + Sync`, which
//! [`SettingsController`](crate::features::settings::presentation::controller::SettingsController)
//! needs to hold it behind an `Arc` under `use_controller`'s bounds.
//!
//! # Failure mapping
//!
//! This is one of the feature's two failure-mapping sites into [`AppFailure`]:
//! an unknown token name maps to [`AppFailure::Internal`] (a bug — the screen
//! only ever revokes a name it just rendered), a blank name to the same (the
//! dialog that collects one cannot submit blank text), and a duplicate name to
//! the same again. A real gRPC client maps transport errors to
//! [`AppFailure::Network`] and a rejected credential to [`AppFailure::Auth`] in
//! exactly this spot.
//!
//! # The minted creation date
//!
//! A token created at runtime is stamped [`MOCK_TODAY`] rather than a real
//! clock reading: there is no daemon to mint a timestamp yet, and a fixed date
//! keeps the headless suites asserting on one exact string. The real client
//! reads the date off the mint response.

use std::sync::Mutex;
use std::time::Duration;

use crate::failure::AppFailure;
use crate::features::settings::domain::entities::{AuthToken, TokenScope};
use crate::features::settings::domain::repositories::AuthTokensRepository;

/// Simulated round-trip latency on every call — the same short, deliberate
/// `sleep` the `sessions` mock uses, so the screen's async path
/// (spawn → await → signal write → rebuild) is the one the real client will
/// take rather than an accidentally-synchronous shortcut.
const MOCK_LATENCY: Duration = Duration::from_millis(20);

/// The creation date stamped on a token minted at runtime — see the
/// [module docs](self). `pub` so a test asserts on the constant rather than on
/// a copy of the literal.
pub const MOCK_TODAY: &str = "2026-07-31";

/// The scope a token minted from this screen gets. The redesign's dashed row
/// collects a name and nothing else, so there is no scope picker to read: a
/// freshly minted token is full-access, and narrowing it is a server-side
/// concern the real API will expose.
const MINTED_SCOPE: TokenScope = TokenScope::ReadWrite;

/// An in-memory [`AuthTokensRepository`]. See the [module docs](self).
pub struct MockAuthTokensRepository {
    /// The live token set. Mutated in place by create/revoke.
    tokens: Mutex<Vec<AuthToken>>,
}

impl MockAuthTokensRepository {
    /// A repository seeded with [`seed`]'s two tokens.
    pub fn seeded() -> Self {
        Self {
            tokens: Mutex::new(seed()),
        }
    }

    /// The live token set, cloned — a **synchronous** peek for this module's
    /// own unit tests, never part of the [`AuthTokensRepository`] contract
    /// (which is what production code speaks).
    pub fn snapshot(&self) -> Vec<AuthToken> {
        self.lock().clone()
    }

    /// The store, with a poisoned lock recovered rather than propagated: a
    /// panicking test must not cascade into every later call on this
    /// repository (the `sessions` mock's own recovery).
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<AuthToken>> {
        self.tokens
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Default for MockAuthTokensRepository {
    fn default() -> Self {
        Self::seeded()
    }
}

/// The seeded token set — `docs/redesign.html`'s two `.token-item`s,
/// transcribed 1:1 (names, scopes and dates), in the order it draws them.
fn seed() -> Vec<AuthToken> {
    vec![
        AuthToken {
            name: "zelli-mobile-100-71-31-57-mr3g6w5g".to_string(),
            scope: TokenScope::ReadWrite,
            created: "2026-07-02".to_string(),
        },
        AuthToken {
            name: "token_2".to_string(),
            scope: TokenScope::ReadOnly,
            created: "2026-07-10".to_string(),
        },
    ]
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl AuthTokensRepository for MockAuthTokensRepository {
    async fn list_tokens(&self) -> Result<Vec<AuthToken>, AppFailure> {
        clean_signals::time::sleep(MOCK_LATENCY).await;
        Ok(self.snapshot())
    }

    async fn create_token(&self, name: String) -> Result<AuthToken, AppFailure> {
        clean_signals::time::sleep(MOCK_LATENCY).await;
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(AppFailure::Internal(
                "create_token called with a blank name".to_string(),
            ));
        }
        let mut tokens = self.lock();
        if tokens.iter().any(|token| token.name == name) {
            return Err(AppFailure::Internal(format!(
                "create_token: {name} already exists"
            )));
        }
        let token = AuthToken {
            name,
            scope: MINTED_SCOPE,
            created: MOCK_TODAY.to_string(),
        };
        tokens.push(token.clone());
        Ok(token)
    }

    async fn revoke_token(&self, name: String) -> Result<(), AppFailure> {
        clean_signals::time::sleep(MOCK_LATENCY).await;
        let mut tokens = self.lock();
        let before = tokens.len();
        tokens.retain(|token| token.name != name);
        if tokens.len() == before {
            return Err(AppFailure::Internal(format!(
                "revoke_token: no token {name}"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How many tokens [`seed`] ships — the redesign's two rows.
    const SEEDED_COUNT: usize = 2;

    /// Drive one repository call to completion on a current-thread runtime —
    /// these are pure data-layer tests, with no frust runtime in play.
    fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a current-thread runtime")
            .block_on(fut)
    }

    #[test]
    fn the_seed_is_the_redesigns_two_tokens_verbatim() {
        let repo = MockAuthTokensRepository::seeded();
        let tokens = block_on(repo.list_tokens()).expect("the mock never fails to list");

        assert_eq!(tokens.len(), SEEDED_COUNT);
        assert_eq!(tokens[0].name, "zelli-mobile-100-71-31-57-mr3g6w5g");
        assert_eq!(tokens[0].scope, TokenScope::ReadWrite);
        assert_eq!(tokens[0].created, "2026-07-02");
        assert_eq!(tokens[1].name, "token_2");
        assert_eq!(tokens[1].scope, TokenScope::ReadOnly);
        assert_eq!(tokens[1].created, "2026-07-10");
    }

    #[test]
    fn create_appends_a_read_write_token_dated_today() {
        let repo = MockAuthTokensRepository::seeded();
        let created = block_on(repo.create_token("  laptop  ".to_string())).expect("create");

        assert_eq!(created.name, "laptop", "the stored name is trimmed");
        assert_eq!(created.scope, TokenScope::ReadWrite);
        assert_eq!(created.created, MOCK_TODAY);
        let tokens = repo.snapshot();
        assert_eq!(tokens.len(), SEEDED_COUNT + 1);
        assert_eq!(tokens.last().map(|t| t.name.as_str()), Some("laptop"));
    }

    #[test]
    fn create_rejects_a_blank_or_duplicate_name() {
        let repo = MockAuthTokensRepository::seeded();

        let blank = block_on(repo.create_token("   ".to_string())).expect_err("blank rejected");
        assert!(matches!(blank, AppFailure::Internal(_)));

        let dup = block_on(repo.create_token("token_2".to_string())).expect_err("dup rejected");
        assert!(matches!(dup, AppFailure::Internal(_)));

        assert_eq!(repo.snapshot().len(), SEEDED_COUNT, "nothing was appended");
    }

    #[test]
    fn revoke_removes_exactly_one_token() {
        let repo = MockAuthTokensRepository::seeded();
        block_on(repo.revoke_token("token_2".to_string())).expect("revoke succeeds");

        let tokens = repo.snapshot();
        assert_eq!(tokens.len(), SEEDED_COUNT - 1);
        assert!(tokens.iter().all(|t| t.name != "token_2"));
    }

    #[test]
    fn revoke_of_an_unknown_token_fails_without_mutating() {
        let repo = MockAuthTokensRepository::seeded();
        let failure = block_on(repo.revoke_token("nope".to_string()))
            .expect_err("an unknown name is a bug, and reports as one");

        assert!(matches!(failure, AppFailure::Internal(_)));
        assert_eq!(repo.snapshot().len(), SEEDED_COUNT);
    }

    #[test]
    fn revoking_every_token_empties_the_store() {
        let repo = MockAuthTokensRepository::seeded();
        for name in ["zelli-mobile-100-71-31-57-mr3g6w5g", "token_2"] {
            block_on(repo.revoke_token(name.to_string())).expect("revoke succeeds");
        }

        assert!(repo.snapshot().is_empty(), "the empty group is reachable");
    }
}
