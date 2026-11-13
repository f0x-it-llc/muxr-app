//! Plain-data types for the settings screen: auth tokens and the terminal and poll preferences.
//!
//! [`MintedToken`] is the only type that carries a secret, kept apart so `AuthToken`'s `Debug` and equality stay safe to format.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthToken {
    /// Also the token's key: tokens are never renamed and the daemon addresses one by name.
    pub name: String,
    pub scope: TokenScope,
    pub created: String,
}

impl AuthToken {
    pub fn created_label(&self) -> String {
        format!("created {}", self.created)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenScope {
    ReadWrite,
    ReadOnly,
}

impl TokenScope {
    pub fn label(self) -> &'static str {
        match self {
            TokenScope::ReadWrite => "read-write",
            TokenScope::ReadOnly => "read-only",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateTokenRequest {
    pub name: String,
    pub read_only: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub struct MintedToken {
    pub name: String,
    pub scope: TokenScope,
    pub created: String,
    /// Returned by the daemon only on create; never logged, scrubbed on drop.
    pub token: zeroize::Zeroizing<String>,
}

impl MintedToken {
    pub fn created_label(&self) -> String {
        format!("created {}", self.created)
    }
}

impl std::fmt::Debug for MintedToken {
    /// `Zeroizing`'s own `Debug` forwards to the `String`, so a derive would print the secret.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MintedToken")
            .field("name", &self.name)
            .field("scope", &self.scope)
            .field("created", &self.created)
            .field("token", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preferences {
    pub appearance: AppearancePrefs,
    pub poll: PollPrefs,
}

impl Preferences {
    pub fn defaults() -> Self {
        Preferences {
            appearance: AppearancePrefs {
                font_family: DEFAULT_FONT_FAMILY.to_string(),
                font_size_px: DEFAULT_FONT_SIZE_PX,
                cursor_style: CursorStyle::Block,
            },
            poll: PollPrefs {
                layout_refresh_secs: DEFAULT_REFRESH_SECS,
                bell_notifications: true,
            },
        }
    }
}

const DEFAULT_FONT_FAMILY: &str = "JetBrains Mono";
const DEFAULT_FONT_SIZE_PX: u32 = 12;
const DEFAULT_REFRESH_SECS: u32 = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppearancePrefs {
    pub font_family: String,
    pub font_size_px: u32,
    pub cursor_style: CursorStyle,
}

pub const FONT_SIZE_MIN_PX: u32 = 10;
pub const FONT_SIZE_MAX_PX: u32 = 24;

impl AppearancePrefs {
    pub const FONT_SIZES_PX: [u32; (FONT_SIZE_MAX_PX - FONT_SIZE_MIN_PX + 1) as usize] = {
        let mut sizes = [0; (FONT_SIZE_MAX_PX - FONT_SIZE_MIN_PX + 1) as usize];
        let mut index = 0;
        while index < sizes.len() {
            sizes[index] = FONT_SIZE_MIN_PX + index as u32;
            index += 1;
        }
        sizes
    };

    pub fn font_size_label(&self) -> String {
        font_size_label(self.font_size_px)
    }
}

pub fn font_size_label(px: u32) -> String {
    format!("{px}px")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorStyle {
    Block,
    Underline,
    Bar,
}

impl CursorStyle {
    pub const ALL: [CursorStyle; 3] =
        [CursorStyle::Block, CursorStyle::Underline, CursorStyle::Bar];

    pub fn label(self) -> &'static str {
        match self {
            CursorStyle::Block => "block",
            CursorStyle::Underline => "underline",
            CursorStyle::Bar => "bar",
        }
    }

    /// Capitalized for the picker; [`label`](Self::label) is lowercase in the row.
    pub fn option_label(self) -> &'static str {
        match self {
            CursorStyle::Block => "Block",
            CursorStyle::Underline => "Underline",
            CursorStyle::Bar => "Bar",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PollPrefs {
    pub layout_refresh_secs: u32,
    pub bell_notifications: bool,
}

impl PollPrefs {
    pub const REFRESH_CHOICES_SECS: [u32; 4] = [1, DEFAULT_REFRESH_SECS, 5, 10];

    pub fn layout_refresh_label(&self) -> String {
        refresh_label(self.layout_refresh_secs)
    }
}

pub fn refresh_label(secs: u32) -> String {
    format!("every {secs}s")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tokens_meta_line_is_the_redesigns() {
        let token = AuthToken {
            name: "token_2".to_string(),
            scope: TokenScope::ReadOnly,
            created: "2026-07-10".to_string(),
        };
        assert_eq!(token.created_label(), "created 2026-07-10");
        assert_eq!(token.scope.label(), "read-only");
        assert_eq!(TokenScope::ReadWrite.label(), "read-write");
    }

    #[test]
    fn a_minted_tokens_meta_line_matches_auth_tokens_own_spelling() {
        let minted = MintedToken {
            name: "laptop".to_string(),
            scope: TokenScope::ReadWrite,
            created: "2026-08-05".to_string(),
            token: zeroize::Zeroizing::new("s3cr3t".to_string()),
        };
        assert_eq!(minted.created_label(), "created 2026-08-05");
    }

    #[test]
    fn a_minted_tokens_debug_never_prints_its_secret() {
        let minted = MintedToken {
            name: "laptop".to_string(),
            scope: TokenScope::ReadOnly,
            created: "2026-08-05".to_string(),
            token: zeroize::Zeroizing::new("s3cr3t-once".to_string()),
        };
        let formatted = format!("{minted:?}");
        assert!(
            !formatted.contains("s3cr3t-once"),
            "the secret leaked into Debug: {formatted}"
        );
        assert!(formatted.contains("<redacted>"), "{formatted}");
        assert!(formatted.contains("laptop"));
    }

    #[test]
    fn the_defaults_are_the_redesigns_values_except_the_bundled_font() {
        let prefs = Preferences::defaults();
        assert_eq!(prefs.appearance.font_family, "JetBrains Mono");
        assert_eq!(prefs.appearance.font_size_label(), "12px");
        assert_eq!(prefs.appearance.cursor_style.label(), "block");
        assert_eq!(prefs.poll.layout_refresh_label(), "every 3s");
        assert!(prefs.poll.bell_notifications);
    }

    #[test]
    fn every_default_is_one_of_its_own_offered_choices() {
        // A default outside its option list would render a picker with nothing selected.
        let prefs = Preferences::defaults();
        assert!(AppearancePrefs::FONT_SIZES_PX.contains(&prefs.appearance.font_size_px));
        assert!(CursorStyle::ALL.contains(&prefs.appearance.cursor_style));
        assert!(PollPrefs::REFRESH_CHOICES_SECS.contains(&prefs.poll.layout_refresh_secs));
    }

    #[test]
    fn the_pickers_option_lists_are_the_redesigns() {
        assert!(
            AppearancePrefs::FONT_SIZES_PX
                .iter()
                .copied()
                .eq(FONT_SIZE_MIN_PX..=FONT_SIZE_MAX_PX)
        );
        assert_eq!(PollPrefs::REFRESH_CHOICES_SECS, [1, 3, 5, 10]);
        assert_eq!(
            CursorStyle::ALL.map(CursorStyle::option_label),
            ["Block", "Underline", "Bar"]
        );
    }

    #[test]
    fn a_cursor_style_prints_differently_in_a_row_and_in_a_picker() {
        for style in CursorStyle::ALL {
            assert_ne!(style.label(), style.option_label(), "{style:?}");
            assert_eq!(style.label(), style.option_label().to_lowercase());
        }
    }

    #[test]
    fn the_value_labels_are_the_ones_the_rows_print() {
        assert_eq!(font_size_label(14), "14px");
        assert_eq!(refresh_label(10), "every 10s");
    }
}
