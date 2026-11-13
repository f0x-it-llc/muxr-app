//! What each key sends, and the key bar's keys. `text_bytes`' `\n` to CR is the only rewrite.

use frust::authoring::NamedKey;

use super::latch::ModifierLatch;

/// DEL (`^?`), not BS: what xterm and the `stty erase` default send.
pub const BACKSPACE_BYTE: u8 = 0x7f;

/// CR, not LF: the line discipline turns it into the newline the program reads.
pub const ENTER_BYTE: u8 = 0x0d;

pub const ESC: &[u8] = b"\x1b";
pub const TAB: &[u8] = b"\t";
pub const ARROW_UP: &[u8] = b"\x1b[A";
pub const ARROW_DOWN: &[u8] = b"\x1b[B";
pub const ARROW_RIGHT: &[u8] = b"\x1b[C";
pub const ARROW_LEFT: &[u8] = b"\x1b[D";
pub const PAGE_UP: &[u8] = b"\x1b[5~";
pub const PAGE_DOWN: &[u8] = b"\x1b[6~";
pub const FORWARD_DELETE: &[u8] = b"\x1b[3~";
/// Normal (non-application) cursor mode, which a fresh session is in.
pub const HOME: &[u8] = b"\x1b[H";
pub const END: &[u8] = b"\x1b[F";
pub const INSERT: &[u8] = b"\x1b[2~";

/// `CBT`, what Shift+Tab sends.
pub const BACK_TAB: &[u8] = b"\x1b[Z";
/// xterm's `1;2` parameters: default count 1, modifier code 2 (Shift).
pub const SHIFT_ARROW_UP: &[u8] = b"\x1b[1;2A";
pub const SHIFT_ARROW_DOWN: &[u8] = b"\x1b[1;2B";
pub const SHIFT_ARROW_RIGHT: &[u8] = b"\x1b[1;2C";
pub const SHIFT_ARROW_LEFT: &[u8] = b"\x1b[1;2D";
/// The modifier code rides as the tilde form's second parameter.
pub const SHIFT_PAGE_UP: &[u8] = b"\x1b[5;2~";
pub const SHIFT_PAGE_DOWN: &[u8] = b"\x1b[6;2~";
pub const SHIFT_FORWARD_DELETE: &[u8] = b"\x1b[3;2~";

/// Columns plain, Shift, Alt, Ctrl. F1-F4 are SS3 plain and xterm `CSI 1 ; m` modified; F5 up use
/// the tilde form with modifier code 2 Shift, 3 Alt, 5 Ctrl. xterm's numbering skips 16 and 22.
const F_KEYS: [[&[u8]; 4]; 12] = [
    [b"\x1bOP", b"\x1b[1;2P", b"\x1b[1;3P", b"\x1b[1;5P"],
    [b"\x1bOQ", b"\x1b[1;2Q", b"\x1b[1;3Q", b"\x1b[1;5Q"],
    [b"\x1bOR", b"\x1b[1;2R", b"\x1b[1;3R", b"\x1b[1;5R"],
    [b"\x1bOS", b"\x1b[1;2S", b"\x1b[1;3S", b"\x1b[1;5S"],
    [b"\x1b[15~", b"\x1b[15;2~", b"\x1b[15;3~", b"\x1b[15;5~"],
    [b"\x1b[17~", b"\x1b[17;2~", b"\x1b[17;3~", b"\x1b[17;5~"],
    [b"\x1b[18~", b"\x1b[18;2~", b"\x1b[18;3~", b"\x1b[18;5~"],
    [b"\x1b[19~", b"\x1b[19;2~", b"\x1b[19;3~", b"\x1b[19;5~"],
    [b"\x1b[20~", b"\x1b[20;2~", b"\x1b[20;3~", b"\x1b[20;5~"],
    [b"\x1b[21~", b"\x1b[21;2~", b"\x1b[21;3~", b"\x1b[21;5~"],
    [b"\x1b[23~", b"\x1b[23;2~", b"\x1b[23;3~", b"\x1b[23;5~"],
    [b"\x1b[24~", b"\x1b[24;2~", b"\x1b[24;3~", b"\x1b[24;5~"],
];
const PLAIN: usize = 0;
const SHIFT: usize = 1;
const ALT: usize = 2;
const CTRL: usize = 3;

const F_LABELS: [&str; 12] = [
    "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
];

fn f_index(n: u8) -> Option<usize> {
    (1..=12).contains(&n).then(|| usize::from(n) - 1)
}

fn f_key_form(sequence: &[u8], column: usize) -> Option<&'static [u8]> {
    F_KEYS
        .iter()
        .find(|row| row[PLAIN] == sequence)
        .map(|row| row[column])
}

/// Keyed on bytes so bar and hardware keys share it; ESC, HOME and END have no row.
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

/// Chosen rather than derived: the latch transform is blind to anything longer than one unit.
pub fn shifted_sequence(sequence: &[u8]) -> Option<&'static [u8]> {
    SHIFTED_SEQUENCES
        .iter()
        .find(|(plain, _)| *plain == sequence)
        .map(|(_, shifted)| *shifted)
        .or_else(|| f_key_form(sequence, SHIFT))
}

/// At most one flag per emission, tried Shift, Ctrl, then Alt; an unchosen flag stays armed.
pub fn modified_sequence(
    sequence: &[u8],
    latch: ModifierLatch,
) -> Option<(&'static [u8], ModifierLatch)> {
    const NONE: ModifierLatch = ModifierLatch::NONE;
    if latch.shift
        && let Some(variant) = shifted_sequence(sequence)
    {
        return Some((
            variant,
            ModifierLatch {
                shift: true,
                ..NONE
            },
        ));
    }
    if latch.ctrl
        && let Some(variant) = f_key_form(sequence, CTRL)
    {
        return Some((variant, ModifierLatch { ctrl: true, ..NONE }));
    }
    if latch.alt
        && let Some(variant) = f_key_form(sequence, ALT)
    {
        return Some((variant, ModifierLatch { alt: true, ..NONE }));
    }
    None
}

/// A pasted `"\r\n"` therefore sends two CRs.
pub fn text_bytes(text: &str) -> Vec<u8> {
    text.bytes()
        .map(|b| if b == b'\n' { ENTER_BYTE } else { b })
        .collect()
}

/// `Ctrl`/`Alt`/`Shift` and `Fn` send nothing: they toggle the latch or the Tablet F-key row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarKey {
    Ctrl,
    Alt,
    Shift,
    Esc,
    Tab,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Del,
    Home,
    End,
    Fn,
    /// 1..=12; any other `n` sends nothing.
    F(u8),
}

impl BarKey {
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

    /// Its arrows are an inverted-T cluster laid out apart.
    pub const TABLET_ORDER: [BarKey; 11] = [
        BarKey::Ctrl,
        BarKey::Alt,
        BarKey::Shift,
        BarKey::Esc,
        BarKey::Tab,
        BarKey::Home,
        BarKey::End,
        BarKey::PageUp,
        BarKey::PageDown,
        BarKey::Del,
        BarKey::Fn,
    ];

    pub const FN_ROW: [BarKey; 12] = [
        BarKey::F(1),
        BarKey::F(2),
        BarKey::F(3),
        BarKey::F(4),
        BarKey::F(5),
        BarKey::F(6),
        BarKey::F(7),
        BarKey::F(8),
        BarKey::F(9),
        BarKey::F(10),
        BarKey::F(11),
        BarKey::F(12),
    ];

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
            BarKey::Home => "Home",
            BarKey::End => "End",
            BarKey::Fn => "Fn",
            BarKey::F(n) => f_index(n).map_or("", |i| F_LABELS[i]),
        }
    }

    /// A word, never the arrow glyph the cap draws.
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
            BarKey::Home => "Home",
            BarKey::End => "End",
            BarKey::Fn => "Function keys",
            BarKey::F(_) => self.label(),
        }
    }

    pub fn sequence(self) -> Option<&'static [u8]> {
        match self {
            BarKey::Ctrl | BarKey::Alt | BarKey::Shift | BarKey::Fn => None,
            BarKey::Home => Some(HOME),
            BarKey::End => Some(END),
            BarKey::F(n) => f_index(n).map(|i| F_KEYS[i][PLAIN]),
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

    pub fn is_latch(self) -> bool {
        matches!(self, BarKey::Ctrl | BarKey::Alt | BarKey::Shift)
    }

    pub fn is_danger(self) -> bool {
        matches!(self, BarKey::Del)
    }

    pub fn separator_before(self) -> bool {
        matches!(self, BarKey::Tab | BarKey::PageUp | BarKey::Del)
    }

    pub fn tablet_separator_before(self) -> bool {
        matches!(self, BarKey::Tab | BarKey::Fn)
    }

    pub fn armed(self, latch: ModifierLatch) -> bool {
        match self {
            BarKey::Ctrl => latch.ctrl,
            BarKey::Alt => latch.alt,
            BarKey::Shift => latch.shift,
            _ => false,
        }
    }
}

/// `NamedKey` is non-exhaustive, so an unmapped key sends nothing. `Copy`/`Cut`/`Paste` send
/// nothing because the shell already routes them to `InputEvent::EditCommand`.
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
        NamedKey::F1 => F_KEYS[0][PLAIN],
        NamedKey::F2 => F_KEYS[1][PLAIN],
        NamedKey::F3 => F_KEYS[2][PLAIN],
        NamedKey::F4 => F_KEYS[3][PLAIN],
        NamedKey::F5 => F_KEYS[4][PLAIN],
        NamedKey::F6 => F_KEYS[5][PLAIN],
        NamedKey::F7 => F_KEYS[6][PLAIN],
        NamedKey::F8 => F_KEYS[7][PLAIN],
        NamedKey::F9 => F_KEYS[8][PLAIN],
        NamedKey::F10 => F_KEYS[9][PLAIN],
        NamedKey::F11 => F_KEYS[10][PLAIN],
        NamedKey::F12 => F_KEYS[11][PLAIN],
        _ => &[],
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

        let mut source = KeystrokeSource::new();
        source.on_bar_key(BarKey::Shift);
        assert_eq!(hex(&source.on_bar_key(BarKey::Esc).bytes), "1b");
        assert!(shifted_sequence(ESC).is_none());
        assert!(shifted_sequence(HOME).is_none(), "nor Home/End");
        assert!(shifted_sequence(END).is_none());
    }

    #[test]
    fn hardware_f_keys_send_the_bars_plain_forms() {
        let keys = [
            NamedKey::F1,
            NamedKey::F2,
            NamedKey::F3,
            NamedKey::F4,
            NamedKey::F5,
            NamedKey::F6,
            NamedKey::F7,
            NamedKey::F8,
            NamedKey::F9,
            NamedKey::F10,
            NamedKey::F11,
            NamedKey::F12,
        ];
        for (row, key) in F_KEYS.iter().zip(keys) {
            assert_eq!(named_key_bytes(key), row[PLAIN], "{key:?}");
        }
        assert!(named_key_bytes(NamedKey::F13).is_empty());
        assert!(named_key_bytes(NamedKey::F24).is_empty());
    }

    /// `(key, plain, Shift, Alt, Ctrl)`.
    type FRow = (u8, Seq, Seq, Seq, Seq);
    type Seq = &'static [u8];

    /// Spelled out rather than read back from [`F_KEYS`].
    const F_EXPECTED: [FRow; 12] = [
        (1, b"\x1bOP", b"\x1b[1;2P", b"\x1b[1;3P", b"\x1b[1;5P"),
        (2, b"\x1bOQ", b"\x1b[1;2Q", b"\x1b[1;3Q", b"\x1b[1;5Q"),
        (3, b"\x1bOR", b"\x1b[1;2R", b"\x1b[1;3R", b"\x1b[1;5R"),
        (4, b"\x1bOS", b"\x1b[1;2S", b"\x1b[1;3S", b"\x1b[1;5S"),
        (5, b"\x1b[15~", b"\x1b[15;2~", b"\x1b[15;3~", b"\x1b[15;5~"),
        (6, b"\x1b[17~", b"\x1b[17;2~", b"\x1b[17;3~", b"\x1b[17;5~"),
        (7, b"\x1b[18~", b"\x1b[18;2~", b"\x1b[18;3~", b"\x1b[18;5~"),
        (8, b"\x1b[19~", b"\x1b[19;2~", b"\x1b[19;3~", b"\x1b[19;5~"),
        (9, b"\x1b[20~", b"\x1b[20;2~", b"\x1b[20;3~", b"\x1b[20;5~"),
        (10, b"\x1b[21~", b"\x1b[21;2~", b"\x1b[21;3~", b"\x1b[21;5~"),
        (11, b"\x1b[23~", b"\x1b[23;2~", b"\x1b[23;3~", b"\x1b[23;5~"),
        (12, b"\x1b[24~", b"\x1b[24;2~", b"\x1b[24;3~", b"\x1b[24;5~"),
    ];

    fn tap_with(latches: &[BarKey], key: BarKey) -> (Vec<u8>, ModifierLatch) {
        let mut source = KeystrokeSource::new();
        for latch in latches {
            source.on_bar_key(*latch);
        }
        let bytes = source.on_bar_key(key).bytes;
        (bytes, source.latch())
    }

    #[test]
    fn every_f_key_sends_its_xterm_form_plain_and_under_each_modifier() {
        for (n, plain, shift, alt, ctrl) in F_EXPECTED {
            let key = BarKey::F(n);
            assert_eq!(key.sequence(), Some(plain), "{key:?}");
            assert_eq!(tap_with(&[], key).0, plain, "{key:?}");
            for (latch, form) in [
                (BarKey::Shift, shift),
                (BarKey::Alt, alt),
                (BarKey::Ctrl, ctrl),
            ] {
                let (bytes, left) = tap_with(&[latch], key);
                assert_eq!(bytes, form, "{latch:?}+{key:?}");
                assert!(!left.armed(), "{latch:?}+{key:?} spends its one shot");
            }
            assert_eq!(key.label(), format!("F{n}"));
            assert_eq!(key.name(), key.label());
        }
    }

    #[test]
    fn home_and_end_send_the_hardware_keys_bytes_and_have_no_shifted_form() {
        assert_eq!(tap_with(&[], BarKey::Home).0, b"\x1b[H");
        assert_eq!(tap_with(&[], BarKey::End).0, b"\x1b[F");
        assert_eq!(
            BarKey::Home.sequence(),
            Some(named_key_bytes(NamedKey::Home))
        );
        assert_eq!(BarKey::End.sequence(), Some(named_key_bytes(NamedKey::End)));
        let (bytes, left) = tap_with(&[BarKey::Shift], BarKey::Home);
        assert_eq!(bytes, b"\x1b[H");
        assert!(left.shift);
    }

    #[test]
    fn one_parameter_per_f_key_shift_first_and_the_rest_stays_armed() {
        let (bytes, left) = tap_with(&[BarKey::Ctrl, BarKey::Shift], BarKey::F(5));
        assert_eq!(bytes, b"\x1b[15;2~", "Shift's form, no Ctrl folded in");
        assert!(left.ctrl && !left.shift);
        let (bytes, left) = tap_with(&[BarKey::Alt, BarKey::Ctrl], BarKey::F(5));
        assert_eq!(bytes, b"\x1b[15;5~", "Ctrl before Alt");
        assert!(left.alt && !left.ctrl);
        // Ctrl/Alt forms exist on the F-keys alone: an arrow keeps both armed.
        let (bytes, left) = tap_with(&[BarKey::Ctrl, BarKey::Alt], BarKey::Up);
        assert_eq!(bytes, ARROW_UP);
        assert!(left.ctrl && left.alt);
    }

    #[test]
    fn fn_and_an_out_of_range_f_key_send_nothing_and_arm_nothing() {
        for key in [BarKey::Fn, BarKey::F(0), BarKey::F(13)] {
            let mut source = KeystrokeSource::new();
            let emission = source.on_bar_key(key);
            assert!(emission.bytes.is_empty(), "{key:?}");
            assert_eq!(emission.class, super::super::source::InputClass::None);
            assert!(!source.latch().armed());
            assert!(!key.is_latch());
        }
        assert_eq!(BarKey::F(13).label(), "");
    }

    #[test]
    fn the_tablet_row_and_the_fn_row_keep_their_own_order() {
        assert_eq!(
            BarKey::TABLET_ORDER.map(BarKey::label),
            [
                "Ctrl", "Alt", "Shift", "Esc", "Tab", "Home", "End", "PgUp", "PgDn", "Del", "Fn"
            ]
        );
        let rules: Vec<&str> = BarKey::TABLET_ORDER
            .iter()
            .filter(|key| key.tablet_separator_before())
            .map(|key| key.label())
            .collect();
        assert_eq!(rules, ["Tab", "Fn"]);
        assert_eq!(BarKey::FN_ROW.map(BarKey::label), F_LABELS);
        assert_eq!(BarKey::ORDER.len(), 12, "the Compact row is untouched");
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
        // Handled on the `EditCommand` path instead.
        for key in [NamedKey::Copy, NamedKey::Cut, NamedKey::Paste] {
            assert!(
                named_key_bytes(key).is_empty(),
                "{key:?} is handled via EditCommand, not re-sent as bytes"
            );
        }
    }
}
