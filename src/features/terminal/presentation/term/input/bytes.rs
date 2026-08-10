//! The byte vocabulary: what each key sends, and the key bar's own keys.
//!
//! Constants and total tables, no state. The one rewrite anything here performs
//! is [`text_bytes`]' `\n` → CR, which is the Return contract rather than a
//! correction (see the [module docs](super)).

use frust::authoring::NamedKey;

use super::latch::ModifierLatch;

/// The byte a derived Backspace sends: `0x7f` (DEL, `^?`).
///
/// **Not** `0x08` (BS): `0x7f` is what xterm and every `stty erase` default
/// actually send against a real PTY, and it is what the Flutter incumbent this
/// app replaces sends.
pub const BACKSPACE_BYTE: u8 = 0x7f;

/// The byte a derived Return sends: `0x0d` (CR).
///
/// Both platforms' Return paths converge here (see the [module docs](super)): a
/// terminal expects CR for Return — the line discipline turns it into the
/// newline the program under it reads, and sending LF instead skips that.
pub const ENTER_BYTE: u8 = 0x0d;

/// `ESC` — the key bar's `Esc`, and the prefix of every sequence below.
pub const ESC: &[u8] = b"\x1b";
/// `HT` — the key bar's `Tab`.
pub const TAB: &[u8] = b"\t";
/// `CUU` — cursor up (the bar's `↑`).
pub const ARROW_UP: &[u8] = b"\x1b[A";
/// `CUD` — cursor down (the bar's `↓`).
pub const ARROW_DOWN: &[u8] = b"\x1b[B";
/// `CUF` — cursor forward (the bar's `→`).
pub const ARROW_RIGHT: &[u8] = b"\x1b[C";
/// `CUB` — cursor back (the bar's `←`).
pub const ARROW_LEFT: &[u8] = b"\x1b[D";
/// Page up (the bar's `PgUp`).
pub const PAGE_UP: &[u8] = b"\x1b[5~";
/// Page down (the bar's `PgDn`).
pub const PAGE_DOWN: &[u8] = b"\x1b[6~";
/// Forward delete (the bar's `Del`) — **not** Backspace.
pub const FORWARD_DELETE: &[u8] = b"\x1b[3~";
/// Home, in the normal (non-application) cursor mode a fresh session is in.
pub const HOME: &[u8] = b"\x1b[H";
/// End, same mode as [`HOME`].
pub const END: &[u8] = b"\x1b[F";

/// The bytes a committed run of text sends: its UTF-8, with every `\n`
/// translated to [`ENTER_BYTE`].
///
/// The **only** rewrite this module performs. A pasted `"\r\n"` therefore sends
/// two CRs — two Returns — which is what every naive paste into a terminal does.
pub fn text_bytes(text: &str) -> Vec<u8> {
    text.bytes()
        .map(|b| if b == b'\n' { ENTER_BYTE } else { b })
        .collect()
}

/// One key of the terminal key bar — the eleven keys a soft keyboard has no
/// room for, in the order the incumbent draws them.
///
/// [`Ctrl`](Self::Ctrl)/[`Alt`](Self::Alt) send nothing: they toggle the
/// [`ModifierLatch`] the *next* emission passes through, wherever that emission
/// comes from (see the [module docs](super)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarKey {
    /// Arms/disarms the one-shot `Ctrl`.
    Ctrl,
    /// Arms/disarms the one-shot `Alt`.
    Alt,
    /// `ESC`.
    Esc,
    /// `HT`.
    Tab,
    /// Cursor up.
    Up,
    /// Cursor down.
    Down,
    /// Cursor left.
    Left,
    /// Cursor right.
    Right,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Forward delete — the one destructive key, and the one drawn in the
    /// danger tone.
    Del,
}

impl BarKey {
    /// Every key, in bar order: the two latches, `Esc`, │ `Tab` and the four
    /// arrows, │ the two page keys, │ `Del`.
    pub const ORDER: [BarKey; 11] = [
        BarKey::Ctrl,
        BarKey::Alt,
        BarKey::Esc,
        BarKey::Tab,
        BarKey::Up,
        BarKey::Down,
        BarKey::Left,
        BarKey::Right,
        BarKey::PageUp,
        BarKey::PageDown,
        BarKey::Del,
    ];

    /// The cap's printed label.
    pub fn label(self) -> &'static str {
        match self {
            BarKey::Ctrl => "Ctrl",
            BarKey::Alt => "Alt",
            BarKey::Esc => "Esc",
            BarKey::Tab => "Tab",
            BarKey::Up => "\u{2191}",
            BarKey::Down => "\u{2193}",
            BarKey::Left => "\u{2190}",
            BarKey::Right => "\u{2192}",
            BarKey::PageUp => "PgUp",
            BarKey::PageDown => "PgDn",
            BarKey::Del => "Del",
        }
    }

    /// The cap's accessible name — a **word**, never the arrow glyph it draws
    /// (`crate::ui::topbar`'s `BACK_LABEL` rule).
    pub fn name(self) -> &'static str {
        match self {
            BarKey::Ctrl => "Control",
            BarKey::Alt => "Alt",
            BarKey::Esc => "Escape",
            BarKey::Tab => "Tab",
            BarKey::Up => "Up arrow",
            BarKey::Down => "Down arrow",
            BarKey::Left => "Left arrow",
            BarKey::Right => "Right arrow",
            BarKey::PageUp => "Page up",
            BarKey::PageDown => "Page down",
            BarKey::Del => "Delete",
        }
    }

    /// The bytes this key sends, or `None` for the two latch keys.
    pub fn sequence(self) -> Option<&'static [u8]> {
        match self {
            BarKey::Ctrl | BarKey::Alt => None,
            BarKey::Esc => Some(ESC),
            BarKey::Tab => Some(TAB),
            BarKey::Up => Some(ARROW_UP),
            BarKey::Down => Some(ARROW_DOWN),
            BarKey::Left => Some(ARROW_LEFT),
            BarKey::Right => Some(ARROW_RIGHT),
            BarKey::PageUp => Some(PAGE_UP),
            BarKey::PageDown => Some(PAGE_DOWN),
            BarKey::Del => Some(FORWARD_DELETE),
        }
    }

    /// Whether this key toggles the latch rather than sending anything.
    pub fn is_latch(self) -> bool {
        self.sequence().is_none()
    }

    /// Whether this key is drawn in the danger tone (`Del` alone).
    pub fn is_danger(self) -> bool {
        matches!(self, BarKey::Del)
    }

    /// Whether a group separator is drawn immediately **before** this key —
    /// the three rules of the incumbent's bar (`Ctrl Alt Esc │ Tab ← ↑ ↓ → │
    /// PgUp PgDn │ Del`).
    pub fn separator_before(self) -> bool {
        matches!(self, BarKey::Tab | BarKey::PageUp | BarKey::Del)
    }

    /// Whether this key is currently armed, given `latch`. Only the two latch
    /// keys can be.
    pub fn armed(self, latch: ModifierLatch) -> bool {
        match self {
            BarKey::Ctrl => latch.ctrl,
            BarKey::Alt => latch.alt,
            _ => false,
        }
    }
}

/// The bytes a focus-routed [`NamedKey`] sends — the desktop preview's real
/// keyboard, and Android's `IME_ACTION_DONE`-as-`Enter`.
///
/// An exhaustive match, so a new [`NamedKey`] variant is a compile error here
/// rather than a silently dropped key. Shares its sequences with [`BarKey`]:
/// the bar's `↑` and a physical `↑` send the same three bytes.
pub fn named_key_bytes(key: NamedKey) -> &'static [u8] {
    match key {
        NamedKey::Enter => &[ENTER_BYTE],
        NamedKey::Backspace => &[BACKSPACE_BYTE],
        NamedKey::Delete => FORWARD_DELETE,
        NamedKey::ArrowUp => ARROW_UP,
        NamedKey::ArrowDown => ARROW_DOWN,
        NamedKey::ArrowLeft => ARROW_LEFT,
        NamedKey::ArrowRight => ARROW_RIGHT,
        NamedKey::Home => HOME,
        NamedKey::End => END,
        NamedKey::Escape => ESC,
        NamedKey::Tab => TAB,
    }
}

#[cfg(test)]
mod tests {
    use super::super::harness::hex;
    use super::super::source::KeystrokeSource;
    use super::*;

    #[test]
    fn every_bar_key_sends_the_documented_sequence() {
        let expected: [(BarKey, &str); 9] = [
            (BarKey::Esc, "1b"),
            (BarKey::Tab, "09"),
            (BarKey::Up, "1b5b41"),
            (BarKey::Down, "1b5b42"),
            (BarKey::Left, "1b5b44"),
            (BarKey::Right, "1b5b43"),
            (BarKey::PageUp, "1b5b357e"),
            (BarKey::PageDown, "1b5b367e"),
            (BarKey::Del, "1b5b337e"),
        ];
        for (key, hex_bytes) in expected {
            let mut source = KeystrokeSource::new();
            assert_eq!(hex(&source.on_bar_key(key).bytes), hex_bytes, "{key:?}");
        }
        assert!(BarKey::Ctrl.sequence().is_none());
        assert!(BarKey::Alt.sequence().is_none());
    }

    #[test]
    fn the_bar_order_and_grouping_match_the_incumbent() {
        assert_eq!(
            BarKey::ORDER.map(BarKey::label),
            [
                "Ctrl", "Alt", "Esc", "Tab", "\u{2191}", "\u{2193}", "\u{2190}", "\u{2192}",
                "PgUp", "PgDn", "Del"
            ]
        );
        let separators: Vec<&str> = BarKey::ORDER
            .iter()
            .filter(|key| key.separator_before())
            .map(|key| key.label())
            .collect();
        assert_eq!(separators, ["Tab", "PgUp", "Del"], "three group rules");
        assert!(BarKey::Del.is_danger());
        assert!(!BarKey::Esc.is_danger());
    }

    #[test]
    fn a_named_key_and_its_bar_twin_send_the_same_bytes() {
        assert_eq!(named_key_bytes(NamedKey::ArrowUp), ARROW_UP);
        assert_eq!(named_key_bytes(NamedKey::Escape), ESC);
        assert_eq!(named_key_bytes(NamedKey::Tab), TAB);
        assert_eq!(named_key_bytes(NamedKey::Delete), FORWARD_DELETE);
        assert_eq!(named_key_bytes(NamedKey::Enter), &[ENTER_BYTE]);
        assert_eq!(named_key_bytes(NamedKey::Backspace), &[BACKSPACE_BYTE]);
    }
}
