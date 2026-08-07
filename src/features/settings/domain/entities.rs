//! `settings` domain entities — the feature's plain-data types
//! (framework-free, per `crate::features::settings::domain`'s module doc).
//!
//! Two aggregates, one per repository contract next door: [`AuthToken`] (the
//! `auth tokens` group) and [`Preferences`] (the `terminal appearance` and
//! `session poll` groups, which one load fills together). Every field exists
//! because `docs/redesign.html`'s **settings** block paints it — the token's
//! name, its scope chip and its `created <date>` line; the font family/size,
//! the cursor style, the layout-refresh interval and the bell switch. Nothing
//! is modelled speculatively.
//!
//! # Where the *choices* live
//!
//! Each preference carries the closed set of values a picker sheet may offer
//! ([`AppearancePrefs::FONT_FAMILIES`], [`AppearancePrefs::FONT_SIZES_PX`],
//! [`CursorStyle::ALL`], [`PollPrefs::REFRESH_CHOICES_SECS`]) beside the field
//! it constrains, rather than in the sheet that renders them: which font sizes
//! a terminal supports is a fact about the domain, not about the picker, and
//! keeping the two together is what lets the screen's four pickers share one
//! builder without any of them minting its own list.
//!
//! # Two labels for one value
//!
//! A cursor style prints as `block` in the row's `.set-value` and as `Block`
//! in the picker sheet ([`CursorStyle::label`] vs
//! [`CursorStyle::option_label`]) — that is the redesign's own casing, taken
//! from screens 5 and 6 respectively, not a rendering choice a screen should
//! re-derive.
//!
//! # Real and mock alike
//!
//! These are deliberately plain owned data, so both
//! [`data::GrpcAuthTokensRepository`](crate::features::settings::data::repositories::GrpcAuthTokensRepository)
//! (production) and the **mock** repositories
//! (`crate::features::settings::data`, every headless suite) map their own
//! source — a `muxrd` proto message, an in-memory seed — onto the same shapes
//! field by field, with the presentation layer none the wiser which one is
//! behind it.
//!
//! # The two-part token shape
//!
//! [`AuthToken`] is what `ListTokens` (and a mint's own stored record) carry —
//! metadata only, **never** a secret. [`MintedToken`] is `CreateToken`'s own
//! response shape: everything [`AuthToken`] has, plus the one-time
//! [`MintedToken::token`] the daemon hands back **only** there (`ListTokens`
//! reports every token's secret as empty, permanently — the wire's own
//! contract). Splitting them is what keeps a stray secret out of
//! [`AuthToken`]'s `Debug`/equality, which every screen row and every test
//! assertion formats freely.

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
    /// A plain `String` rather than a date type: nothing in this app does date
    /// *arithmetic*, and the daemon hands the label over already formatted — a
    /// SQLite `DATETIME` string over the wire (empty if the daemon itself
    /// doesn't know it), the mock's own `2026-07-02` ISO date locally.
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
/// read-only toggle
/// ([`AuthTokensRepository::create_token`](super::repositories::AuthTokensRepository::create_token)'s
/// params). Mirrors the Flutter app's own `CreateTokenRequest` shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateTokenRequest {
    /// The token's name, already trimmed by the dialog that collected it.
    pub name: String,
    /// Whether the minted token should be read-only (mints read-write when
    /// unset — the mock era's own only behaviour before this toggle existed).
    pub read_only: bool,
}

/// A freshly minted token: everything [`AuthToken`] carries, plus the
/// one-time secret the daemon returns **only** on create. Mirrors the Flutter
/// app's own `MintedToken` shape — see the [module docs](self)'s "two-part
/// token shape" section for why this is a separate type from [`AuthToken`]
/// rather than an optional field on it.
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
    /// Hand-written so [`Self::token`] never prints: `Zeroizing`'s own `Debug`
    /// forwards straight to the wrapped `String` (`crate::connection`'s
    /// `ConnectTarget` documents the same hazard for its own secret field), so
    /// a derived impl here would put a live token secret in the clear in any
    /// log line — or failed test assertion — that formats one.
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
    /// The app's shipped defaults — **and** what `docs/redesign.html`'s
    /// settings screen draws: `Hack Nerd Font Mono`, `12px`, `block`,
    /// `every 3s`, bell on.
    ///
    /// One definition, two consumers: the mock repository seeds from it (so
    /// the screen and the mock are the same picture) and the controller's
    /// signals hold it until the first load lands (so the screen never paints
    /// a frame of blank preference values). A real client replaces the *load*,
    /// not this.
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

/// The default terminal font family — the redesign's `Hack Nerd Font Mono`.
const DEFAULT_FONT_FAMILY: &str = "Hack Nerd Font Mono";
/// The default terminal font size, in px — the redesign's `12px`.
const DEFAULT_FONT_SIZE_PX: u32 = 12;
/// The default layout-refresh interval, in seconds — the redesign's
/// `every 3s`, and the period `sessions`' own mock poll already runs at.
const DEFAULT_REFRESH_SECS: u32 = 3;

/// How the remote terminal should be drawn (`terminal appearance`).
///
/// **Mock-persisted only.** Nothing reads these back yet: the Phase-3 terminal
/// screen is itself a mock and renders a fixed sample, so changing a value here
/// round-trips through the repository and updates this screen's own rows and
/// nothing else. Phase 5 (the real terminal grid) is where the renderer starts
/// reading them.
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
    /// The font families the picker offers — the redesign's own four.
    pub const FONT_FAMILIES: [&'static str; 4] = [
        DEFAULT_FONT_FAMILY,
        "IBM Plex Mono",
        "Space Mono",
        "JetBrains Mono",
    ];

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
    fn the_defaults_are_the_values_the_redesign_draws() {
        let prefs = Preferences::defaults();
        assert_eq!(prefs.appearance.font_family, "Hack Nerd Font Mono");
        assert_eq!(prefs.appearance.font_size_label(), "12px");
        assert_eq!(prefs.appearance.cursor_style.label(), "block");
        assert_eq!(prefs.poll.layout_refresh_label(), "every 3s");
        assert!(prefs.poll.bell_notifications);
    }

    #[test]
    fn every_default_is_one_of_its_own_offered_choices() {
        // The tripwire that keeps a picker from opening with nothing selected:
        // a default outside its own option list would render every row
        // unselected and no check mark at all.
        let prefs = Preferences::defaults();
        assert!(
            AppearancePrefs::FONT_FAMILIES.contains(&prefs.appearance.font_family.as_str()),
            "{:?}",
            AppearancePrefs::FONT_FAMILIES
        );
        assert!(AppearancePrefs::FONT_SIZES_PX.contains(&prefs.appearance.font_size_px));
        assert!(CursorStyle::ALL.contains(&prefs.appearance.cursor_style));
        assert!(PollPrefs::REFRESH_CHOICES_SECS.contains(&prefs.poll.layout_refresh_secs));
    }

    #[test]
    fn the_pickers_option_lists_are_the_redesigns() {
        assert_eq!(
            AppearancePrefs::FONT_FAMILIES,
            [
                "Hack Nerd Font Mono",
                "IBM Plex Mono",
                "Space Mono",
                "JetBrains Mono"
            ]
        );
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
