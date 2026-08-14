//! `settings` domain entities — the feature's plain-data types
//! (framework-free). Two aggregates, one per repository contract next door:
//! [`AuthToken`] (`auth tokens`) and [`Preferences`] (`terminal appearance` +
//! `session poll`, filled together by one load).
//!
//! Each preference carries its own closed choice set
//! ([`AppearancePrefs::FONT_SIZES_PX`], [`CursorStyle::ALL`],
//! [`PollPrefs::REFRESH_CHOICES_SECS`]) beside the field it constrains, so the
//! screen's pickers share one builder without any of them minting its own
//! list. The font family is the exception — which faces resolve is a runtime
//! fact about this device's bundled/downloaded assets a framework-free module
//! can't hold, so `data` persists it as given rather than clamping it (see
//! `data::repositories::prefs_repository`'s own docs). A cursor style prints
//! two ways — `block` in the row, `Block` in the picker
//! ([`CursorStyle::label`] vs [`CursorStyle::option_label`]) — the redesign's
//! own casing, not a rendering choice to re-derive.
//!
//! **The two-part token shape**: [`AuthToken`] is what `ListTokens` (and a
//! mint's own stored record) carry — metadata only, **never** a secret.
//! [`MintedToken`] is `CreateToken`'s response shape: everything [`AuthToken`]
//! has, plus the one-time [`MintedToken::token`] the daemon hands back
//! **only** there. Splitting them keeps a stray secret out of [`AuthToken`]'s
//! `Debug`/equality, which every row and test assertion formats freely.

/// One auth token the connected server holds, as the `auth tokens` group
/// renders it (`.token-item`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthToken {
    /// The token's name — `zelli-mobile-100-71-31-57-mr3g6w5g` in the mock.
    ///
    /// **Also its key**: unlike a session (which carries a separate `id`
    /// because a rename changes its name), a token is never renamed, and the
    /// daemon addresses one by the name it minted. So
    /// [`AuthTokensRepository::revoke_token`](super::repositories::AuthTokensRepository::revoke_token)
    /// takes this.
    pub name: String,
    /// What the token is allowed to do — the `.token-scope` chip.
    pub scope: TokenScope,
    /// The token's creation stamp, printed by [`created_label`](Self::created_label).
    /// A plain `String`, not a date type: nothing here does date arithmetic,
    /// and the daemon hands the label over already formatted.
    pub created: String,
}

impl AuthToken {
    /// The `.token-meta` line's leading span — `created 2026-07-02`.
    pub fn created_label(&self) -> String {
        format!("created {}", self.created)
    }
}

/// A token's permission scope. Two states, because the daemon mints exactly
/// two.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenScope {
    /// Full control: attach, create, kill (`.token-scope.rw`).
    ReadWrite,
    /// Observation only (`.token-scope.ro`).
    ReadOnly,
}

impl TokenScope {
    /// The chip label this scope renders as.
    pub fn label(self) -> &'static str {
        match self {
            TokenScope::ReadWrite => "read-write",
            TokenScope::ReadOnly => "read-only",
        }
    }
}

/// A request to mint a new auth token — the create dialog's name plus its
/// read-only toggle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateTokenRequest {
    /// Already trimmed by the dialog that collected it.
    pub name: String,
    /// Mints read-write when unset.
    pub read_only: bool,
}

/// A freshly minted token: everything [`AuthToken`] carries, plus the
/// one-time secret the daemon returns **only** on create — see the [module
/// docs](self)'s "two-part token shape" for why this is a separate type
/// rather than an optional field on [`AuthToken`].
#[derive(Clone, PartialEq, Eq)]
pub struct MintedToken {
    /// The minted token's name.
    pub name: String,
    /// Its scope, as stored (mirrors [`CreateTokenRequest::read_only`]).
    pub scope: TokenScope,
    /// Its creation stamp — see [`AuthToken::created`].
    pub created: String,
    /// The secret itself. Shown exactly once, in the presentation layer's
    /// one-time reveal sheet, and never logged: `Zeroizing` so the buffer is
    /// scrubbed the moment this value is dropped (the reveal's dismissal, or
    /// the next mint overwriting the pending one).
    pub token: zeroize::Zeroizing<String>,
}

impl MintedToken {
    /// The reveal sheet's `created <date>` line — the same spelling
    /// [`AuthToken::created_label`] uses.
    pub fn created_label(&self) -> String {
        format!("created {}", self.created)
    }
}

impl std::fmt::Debug for MintedToken {
    /// Hand-written so [`Self::token`] never prints: `Zeroizing`'s own
    /// `Debug` forwards straight to the wrapped `String`, so a derived impl
    /// here would put a live secret in the clear in any log line or failed
    /// assertion that formats one.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MintedToken")
            .field("name", &self.name)
            .field("scope", &self.scope)
            .field("created", &self.created)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// Every preference the settings screen edits, as one aggregate — what a
/// single
/// [`PreferencesRepository::load_preferences`](super::repositories::PreferencesRepository::load_preferences)
/// returns, so the two groups that render them never load out of step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preferences {
    /// The `terminal appearance` group.
    pub appearance: AppearancePrefs,
    /// The `session poll` group.
    pub poll: PollPrefs,
}

impl Preferences {
    /// The app's shipped defaults: `JetBrains Mono` (the app-wide bundled
    /// face), `12px`, `block`, `every 3s`, bell on. One definition, two
    /// consumers — the mock repository seeds from it, and the controller's
    /// signals hold it until the first load lands, so the screen never
    /// paints a frame of blank values.
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
/// `sessions`' own mock poll already runs at this same period.
const DEFAULT_REFRESH_SECS: u32 = 3;

/// How the remote terminal should be drawn (`terminal appearance`). The
/// terminal screen's own controller reads these back through the same
/// [`PreferencesRepository`](super::repositories::PreferencesRepository) —
/// `terminal::domain` re-exports it rather than minting a second store — and
/// maps `cursor_style`/`font_size_px`/`font_family` onto the live grid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppearancePrefs {
    /// The terminal font family, by name.
    pub font_family: String,
    /// The terminal font size, in px.
    pub font_size_px: u32,
    /// The cursor shape.
    pub cursor_style: CursorStyle,
}

impl AppearancePrefs {
    /// The font sizes the picker offers, in px (`10px`–`14px`).
    pub const FONT_SIZES_PX: [u32; 5] = [10, 11, 12, 13, 14];

    /// This preference's `.set-value` text — `12px`.
    pub fn font_size_label(&self) -> String {
        font_size_label(self.font_size_px)
    }
}

/// How a font size prints, in a row value and in a picker row alike.
pub fn font_size_label(px: u32) -> String {
    format!("{px}px")
}

/// The terminal cursor's shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorStyle {
    /// A filled block (the default).
    Block,
    /// A baseline underline.
    Underline,
    /// A thin vertical bar.
    Bar,
}

impl CursorStyle {
    /// Every style, in the order the picker sheet lists them (screen 6).
    pub const ALL: [CursorStyle; 3] =
        [CursorStyle::Block, CursorStyle::Underline, CursorStyle::Bar];

    /// The `.set-value` text — lowercase, as screen 5 prints it.
    pub fn label(self) -> &'static str {
        match self {
            CursorStyle::Block => "block",
            CursorStyle::Underline => "underline",
            CursorStyle::Bar => "bar",
        }
    }

    /// The picker row's text — capitalized, as screen 6 prints it.
    pub fn option_label(self) -> &'static str {
        match self {
            CursorStyle::Block => "Block",
            CursorStyle::Underline => "Underline",
            CursorStyle::Bar => "Bar",
        }
    }
}

/// How often the app re-reads the server's layout, and whether a terminal bell
/// reaches the device (`session poll`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PollPrefs {
    /// The layout-refresh period, in seconds.
    pub layout_refresh_secs: u32,
    /// Whether a bell raises the haptic + badge notification.
    pub bell_notifications: bool,
}

impl PollPrefs {
    /// The refresh periods the picker offers, in seconds.
    pub const REFRESH_CHOICES_SECS: [u32; 4] = [1, DEFAULT_REFRESH_SECS, 5, 10];

    /// This preference's `.set-value` text — `every 3s`.
    pub fn layout_refresh_label(&self) -> String {
        refresh_label(self.layout_refresh_secs)
    }
}

/// How a refresh period prints, in a row value and in a picker row alike.
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
        // …and the rest of the shape is still there for a log line to be
        // useful at all.
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
        // The tripwire that keeps a picker from opening with nothing selected:
        // a default outside its own option list would render every row
        // unselected and no check mark at all. (The font family has no list
        // here — see the module docs; its own "the default is installed"
        // tripwire is `crate::fonts`' `resolve_terminal_family` test.)
        let prefs = Preferences::defaults();
        assert!(AppearancePrefs::FONT_SIZES_PX.contains(&prefs.appearance.font_size_px));
        assert!(CursorStyle::ALL.contains(&prefs.appearance.cursor_style));
        assert!(PollPrefs::REFRESH_CHOICES_SECS.contains(&prefs.poll.layout_refresh_secs));
    }

    #[test]
    fn the_pickers_option_lists_are_the_redesigns() {
        // The font family has no list here at all: the picker sources it from
        // `crate::fonts` (installed faces, bundled or downloaded).
        assert_eq!(AppearancePrefs::FONT_SIZES_PX, [10, 11, 12, 13, 14]);
        assert_eq!(PollPrefs::REFRESH_CHOICES_SECS, [1, 3, 5, 10]);
        assert_eq!(
            CursorStyle::ALL.map(CursorStyle::option_label),
            ["Block", "Underline", "Bar"]
        );
    }

    #[test]
    fn a_cursor_style_prints_differently_in_a_row_and_in_a_picker() {
        // Screen 5 says `block`; screen 6 says `Block`. Two labels, one value.
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
