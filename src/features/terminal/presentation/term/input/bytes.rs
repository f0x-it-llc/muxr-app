//! The byte vocabulary: what each key sends, and the key bar's own keys. Constants
//! and total tables, no state. The one rewrite anything here performs is
//! [`text_bytes`]' `\n` → CR, which is the Return contract rather than a correction
//! (see the [module docs](super)).

use frust::authoring::NamedKey;

use super::latch::ModifierLatch;

/// The byte a derived Backspace sends: `0x7f` (DEL, `^?`). **Not** `0x08` (BS):
/// `0x7f` is what xterm and every `stty erase` default actually send against a real
/// PTY, and what the Flutter incumbent sends.
pub const BACKSPACE_BYTE: u8 = 0x7f;

/// The byte a derived Return sends: `0x0d` (CR). Both platforms' Return paths
/// converge here: a terminal expects CR — the line discipline turns it into the
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
/// Insert (`CSI 2 ~`). Its clipboard siblings (`Copy`/`Cut`/`Paste`) send
/// nothing — see [`named_key_bytes`].
pub const INSERT: &[u8] = b"\x1b[2~";

/// `CBT` — back-tab, what Shift+`Tab` sends (the bar's `Shift` armed over
/// [`TAB`], and a desktop Shift+Tab).
pub const BACK_TAB: &[u8] = b"\x1b[Z";
/// Shift+`↑`: [`ARROW_UP`]'s modified form. The `1;2` is xterm's own parameter
/// pair — a default row count of 1 and a modifier code of 2, which is Shift.
pub const SHIFT_ARROW_UP: &[u8] = b"\x1b[1;2A";
/// Shift+`↓` — see [`SHIFT_ARROW_UP`].
pub const SHIFT_ARROW_DOWN: &[u8] = b"\x1b[1;2B";
/// Shift+`→` — see [`SHIFT_ARROW_UP`].
pub const SHIFT_ARROW_RIGHT: &[u8] = b"\x1b[1;2C";
/// Shift+`←` — see [`SHIFT_ARROW_UP`].
pub const SHIFT_ARROW_LEFT: &[u8] = b"\x1b[1;2D";
/// Shift+`PgUp` — the same modifier code, as the tilde form's second parameter.
pub const SHIFT_PAGE_UP: &[u8] = b"\x1b[5;2~";
/// Shift+`PgDn` — see [`SHIFT_PAGE_UP`].
pub const SHIFT_PAGE_DOWN: &[u8] = b"\x1b[6;2~";
/// Shift+`Del` — see [`SHIFT_PAGE_UP`].
pub const SHIFT_FORWARD_DELETE: &[u8] = b"\x1b[3;2~";

/// Every sequence that has a Shift variant, as `(plain, shifted)` — the whole
/// of the modifier arithmetic this module performs on a sequence key. Keyed
/// on the **bytes**, not a key enum, so the bar's `↑` and a hardware `↑` send
/// the same thing shifted as they do plain, through one table. Deliberately
/// not exhaustive: [`ESC`], [`HOME`] and [`END`] have no row and are sent
/// unmodified with Shift armed, and **no other modifier is parameterized at
/// all** (see [`ModifierLatch`]).
const SHIFTED_SEQUENCES: [(&[u8], &[u8]); 8] = [
    (TAB, BACK_TAB),
    (ARROW_UP, SHIFT_ARROW_UP),
    (ARROW_DOWN, SHIFT_ARROW_DOWN),
    (ARROW_RIGHT, SHIFT_ARROW_RIGHT),
    (ARROW_LEFT, SHIFT_ARROW_LEFT),
    (PAGE_UP, SHIFT_PAGE_UP),
    (PAGE_DOWN, SHIFT_PAGE_DOWN),
    (FORWARD_DELETE, SHIFT_FORWARD_DELETE),
];

/// `sequence`'s Shift variant, or `None` for a sequence that has none. The selector
/// [`KeystrokeSource`](super::source::KeystrokeSource) runs before it emits a
/// sequence key: the latch's own transform is structurally blind to anything longer
/// than one code unit, so a shifted arrow has to be *chosen* rather than derived.
pub fn shifted_sequence(sequence: &[u8]) -> Option<&'static [u8]> {
    SHIFTED_SEQUENCES
        .iter()
        .find(|(plain, _)| *plain == sequence)
        .map(|(_, shifted)| *shifted)
}

/// The bytes a committed run of text sends: its UTF-8, with every `\n` translated
/// to [`ENTER_BYTE`] — the **only** rewrite this module performs. A pasted `"\r\n"`
/// therefore sends two CRs, which is what every naive paste into a terminal does.
pub fn text_bytes(text: &str) -> Vec<u8> {
    text.bytes()
        .map(|b| if b == b'\n' { ENTER_BYTE } else { b })
        .collect()
}

/// One key of the terminal key bar — the twelve keys a soft keyboard has no room
/// for, in the order the incumbent draws them.
/// [`Ctrl`](Self::Ctrl)/[`Alt`](Self::Alt)/[`Shift`](Self::Shift) send nothing:
/// they toggle the [`ModifierLatch`] the *next* emission passes through, wherever
/// that emission comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarKey {
    /// Arms/disarms the one-shot `Ctrl`.
    Ctrl,
    /// Arms/disarms the one-shot `Alt`.
    Alt,
    /// Arms/disarms the one-shot `Shift` — which, unlike the other two, selects a
    /// different *sequence* rather than transforming a character
    /// ([`SHIFTED_SEQUENCES`]).
    Shift,
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
    /// Every key, in bar order: the three latches, `Esc`, │ `Tab` and the four
    /// arrows, │ the two page keys, │ `Del`.
    pub const ORDER: [BarKey; 12] = [
        BarKey::Ctrl,
        BarKey::Alt,
        BarKey::Shift,
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
            BarKey::Shift => "Shift",
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
            BarKey::Shift => "Shift",
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

    /// The bytes this key sends **unmodified**, or `None` for the three latch keys.
    /// An armed `Shift` picks a different sequence for some — see
    /// [`shifted_sequence`], which the source applies on the way out.
    pub fn sequence(self) -> Option<&'static [u8]> {
        match self {
            BarKey::Ctrl | BarKey::Alt | BarKey::Shift => None,
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

    /// Whether a group separator is drawn immediately **before** this key — the
    /// three rules of the incumbent's bar.
    pub fn separator_before(self) -> bool {
        matches!(self, BarKey::Tab | BarKey::PageUp | BarKey::Del)
    }

    /// Whether this key is currently armed, given `latch`. Only the three latch
    /// keys can be.
    pub fn armed(self, latch: ModifierLatch) -> bool {
        match self {
            BarKey::Ctrl => latch.ctrl,
            BarKey::Alt => latch.alt,
            BarKey::Shift => latch.shift,
            _ => false,
        }
    }
}

/// The bytes a focus-routed [`NamedKey`] sends — the desktop preview's real
/// keyboard, and Android's `IME_ACTION_DONE`-as-`Enter`. An exhaustive match,
/// so a new [`NamedKey`] variant is a compile error, not a silently dropped
/// key. Shares its sequences with [`BarKey`]: the bar's `↑` and a physical
/// `↑` send the same three bytes, and the same six shifted ones.
/// `Copy`/`Cut`/`Paste` deliberately send **nothing** — not a silent drop:
/// the shell already decodes that hardware key onto `InputEvent::EditCommand`
/// before a widget sees `InputEvent::Key`, so a byte here would double-handle
/// the keypress (handled instead by `InputSeam::absorb`/`edit_command`).
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
        NamedKey::Insert => INSERT,
        NamedKey::Copy | NamedKey::Cut | NamedKey::Paste => &[],
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
        assert!(BarKey::Shift.sequence().is_none());
    }

    #[test]
    fn every_shifted_bar_key_sends_the_documented_variant() {
        // The other half of the table above: the same tap with `Shift` armed
        // from the bar first.
        let expected: [(BarKey, &str); 8] = [
            (BarKey::Tab, "1b5b5a"),
            (BarKey::Up, "1b5b313b3241"),
            (BarKey::Down, "1b5b313b3242"),
            (BarKey::Left, "1b5b313b3244"),
            (BarKey::Right, "1b5b313b3243"),
            (BarKey::PageUp, "1b5b353b327e"),
            (BarKey::PageDown, "1b5b363b327e"),
            (BarKey::Del, "1b5b333b327e"),
        ];
        for (key, hex_bytes) in expected {
            let mut source = KeystrokeSource::new();
            source.on_bar_key(BarKey::Shift);
            assert_eq!(hex(&source.on_bar_key(key).bytes), hex_bytes, "{key:?}");
        }

        // `Esc` has no variant, so an armed `Shift` leaves it exactly as it is
        // (and — being one code unit — spends the latch the ordinary way).
        let mut source = KeystrokeSource::new();
        source.on_bar_key(BarKey::Shift);
        assert_eq!(hex(&source.on_bar_key(BarKey::Esc).bytes), "1b");
        assert!(shifted_sequence(ESC).is_none());
        assert!(shifted_sequence(HOME).is_none(), "nor Home/End");
        assert!(shifted_sequence(END).is_none());
    }

    #[test]
    fn the_bar_order_and_grouping_match_the_incumbent() {
        assert_eq!(
            BarKey::ORDER.map(BarKey::label),
            [
                "Ctrl", "Alt", "Shift", "Esc", "Tab", "\u{2191}", "\u{2193}", "\u{2190}",
                "\u{2192}", "PgUp", "PgDn", "Del"
            ]
        );
        let separators: Vec<&str> = BarKey::ORDER
            .iter()
            .filter(|key| key.separator_before())
            .map(|key| key.label())
            .collect();
        assert_eq!(separators, ["Tab", "PgUp", "Del"], "three group rules");
        let latches: Vec<&str> = BarKey::ORDER
            .iter()
            .filter(|key| key.is_latch())
            .map(|key| key.label())
            .collect();
        assert_eq!(latches, ["Ctrl", "Alt", "Shift"], "three latch keys, first");
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

        // Shifted too, because the selector keys on the bytes rather than on
        // either key enum: one table serves both paths.
        assert_eq!(
            shifted_sequence(named_key_bytes(NamedKey::Tab)),
            Some(BACK_TAB)
        );
        assert_eq!(
            shifted_sequence(named_key_bytes(NamedKey::ArrowUp)),
            shifted_sequence(BarKey::Up.sequence().expect("an arrow has a sequence"))
        );
        assert!(
            shifted_sequence(named_key_bytes(NamedKey::Enter)).is_none(),
            "Return has no shifted form here"
        );
    }

    #[test]
    fn insert_sends_the_conventional_csi_sequence() {
        assert_eq!(named_key_bytes(NamedKey::Insert), INSERT);
        assert_eq!(hex(INSERT), "1b5b327e");
    }

    #[test]
    fn the_clipboard_named_keys_send_nothing_here_deliberately() {
        // Handled on the `EditCommand` path instead (see `named_key_bytes`).
        for key in [NamedKey::Copy, NamedKey::Cut, NamedKey::Paste] {
            assert!(
                named_key_bytes(key).is_empty(),
                "{key:?} is handled via EditCommand, not re-sent as bytes"
            );
        }
    }
}
