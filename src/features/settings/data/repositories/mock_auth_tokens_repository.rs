//! In-memory [`AuthTokensRepository`] for headless tests, seeded with the two tokens the design mock draws.

use std::sync::Mutex;
use std::time::Duration;

use zeroize::Zeroizing;

use crate::failure::AppFailure;
use crate::features::settings::domain::entities::{
    AuthToken, CreateTokenRequest, MintedToken, TokenScope,
};
use crate::features::settings::domain::repositories::AuthTokensRepository;

/// Keeps the async path real instead of accidentally synchronous.
const MOCK_LATENCY: Duration = Duration::from_millis(20);

/// A fixed stamp and secret prefix let suites assert exact strings.
pub const MOCK_TODAY: &str = "2026-07-31";

pub const MOCK_MINTED_SECRET_PREFIX: &str = "mock-secret-";

pub struct MockAuthTokensRepository {
    tokens: Mutex<Vec<AuthToken>>,
}

impl MockAuthTokensRepository {
    pub fn seeded() -> Self {
        Self {
            tokens: Mutex::new(seed()),
        }
    }

    pub fn snapshot(&self) -> Vec<AuthToken> {
        self.lock().clone()
    }

    /// Recovers a poisoned lock so one panicking test does not cascade.
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

    async fn create_token(&self, request: CreateTokenRequest) -> Result<MintedToken, AppFailure> {
        clean_signals::time::sleep(MOCK_LATENCY).await;
        // A blank or duplicate name is a bug: the screen never submits one.
        let name = request.name.trim().to_string();
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
        let scope = if request.read_only {
            TokenScope::ReadOnly
        } else {
            TokenScope::ReadWrite
        };
        let token = AuthToken {
            name: name.clone(),
            scope,
            created: MOCK_TODAY.to_string(),
        };
        tokens.push(token);
        Ok(MintedToken {
            name: name.clone(),
            scope,
            created: MOCK_TODAY.to_string(),
            token: Zeroizing::new(format!("{MOCK_MINTED_SECRET_PREFIX}{name}")),
        })
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

    const SEEDED_COUNT: usize = 2;

    fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a current-thread runtime")
            .block_on(fut)
    }

    fn mint(name: &str) -> CreateTokenRequest {
        CreateTokenRequest {
            name: name.to_string(),
            read_only: false,
        }
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
    fn create_appends_a_read_write_token_dated_today_with_its_one_time_secret() {
        let repo = MockAuthTokensRepository::seeded();
        let minted = block_on(repo.create_token(mint("  laptop  "))).expect("create");

        assert_eq!(minted.name, "laptop", "the stored name is trimmed");
        assert_eq!(minted.scope, TokenScope::ReadWrite);
        assert_eq!(minted.created, MOCK_TODAY);
        assert_eq!(minted.token.as_str(), "mock-secret-laptop");
        let tokens = repo.snapshot();
        assert_eq!(tokens.len(), SEEDED_COUNT + 1);
        assert_eq!(tokens.last().map(|t| t.name.as_str()), Some("laptop"));
    }

    #[test]
    fn the_read_only_toggle_mints_a_read_only_scope() {
        let repo = MockAuthTokensRepository::seeded();
        let minted = block_on(repo.create_token(CreateTokenRequest {
            name: "phone".to_string(),
            read_only: true,
        }))
        .expect("create");

        assert_eq!(minted.scope, TokenScope::ReadOnly);
        assert_eq!(
            repo.snapshot().last().map(|t| t.scope),
            Some(TokenScope::ReadOnly),
            "the stored record carries the same scope as the minted response"
        );
    }

    #[test]
    fn create_rejects_a_blank_or_duplicate_name() {
        let repo = MockAuthTokensRepository::seeded();

        let blank = block_on(repo.create_token(mint("   "))).expect_err("blank rejected");
        assert!(matches!(blank, AppFailure::Internal(_)));

        let dup = block_on(repo.create_token(mint("token_2"))).expect_err("dup rejected");
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
