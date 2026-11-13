//! One thin use case per repository method; controllers run them through `ControllerCore::run`, never `execute` directly.

pub mod create_token;
pub mod list_tokens;
pub mod load_preferences;
pub mod revoke_token;
pub mod update_appearance;
pub mod update_poll_prefs;

pub use create_token::CreateToken;
pub use list_tokens::ListTokens;
pub use load_preferences::LoadPreferences;
pub use revoke_token::RevokeToken;
pub use update_appearance::UpdateAppearance;
pub use update_poll_prefs::UpdatePollPrefs;
