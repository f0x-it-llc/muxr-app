//! The one-shot Ctrl/Alt/Shift latch, and the transform every outgoing emission
//! passes through.
//!
//! The latch transforms **soft-keyboard characters too** — the incumbent's
//! semantic, and the reason it is applied at the emission choke point
//! (`KeystrokeSource::emit`) rather than in the key bar: arming `Ctrl` on the
//! bar and then typing `c` on the OS keyboard must send `0x03`.

use frust::authoring::Modifiers;

use super::bytes::ESC;

/// The armed one-shot modifiers, as toggled from the key bar (and as carried by
/// a desktop hardware key event).
///
/// # The three flags are not symmetric, and the asymmetry is the design
///
/// | Flag | On a **character** | On a **sequence key** (arrow, `PgUp`, `Tab`, …) |
/// |---|---|---|
/// | `ctrl`/`alt` | transforms it ([`apply_latch`]) and is consumed | **nothing at all**, and stays armed |
/// | `shift` | never touches it, but is consumed | selects the shifted sequence ([`shifted_sequence`](super::bytes::shifted_sequence)) and is consumed by that |
///
/// `shift` leaves characters alone because the IME's own Shift has **already**
/// delivered the case: the app is handed `"A"`, and rewriting a byte it was
/// handed is precisely what this module does not do (the [module docs](super)).
/// What a terminal is missing without it is the *sequences* — back-tab, a
/// shifted arrow — so that is the whole of what it selects.
///
/// `ctrl`/`alt` staying armed across a sequence key is the incumbent's pinned
/// behaviour (`Ctrl` then `↑` then `c` still sends `0x03`), kept verbatim. The
/// consequence, recorded rather than papered over: **there is no combination
/// arithmetic**. xterm encodes a chord as one parameter (`1` + Shift `1` + Alt
/// `2` + Ctrl `4`), so a real Ctrl+Shift+`↑` is `\x1b[1;6A` — this module sends
/// `\x1b[1;2A` and leaves the `Ctrl` armed for the next character instead.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModifierLatch {
    /// `Ctrl` is armed for the next single-code-unit emission.
    pub ctrl: bool,
    /// `Alt` is armed for the next single-code-unit emission.
    pub alt: bool,
    /// `Shift` is armed for the next emission — a sequence key it has a variant
    /// for, or (spending it, changing nothing) one code unit.
    pub shift: bool,
}

impl ModifierLatch {
    /// Nothing armed.
    pub const NONE: Self = Self {
        ctrl: false,
        alt: false,
        shift: false,
    };

    /// Whether any flag is armed.
    pub fn armed(self) -> bool {
        self.ctrl || self.alt || self.shift
    }

    /// Every flag of either side (used to fold a hardware key event's own
    /// modifiers into the latch for that one emission).
    pub fn union(self, other: Self) -> Self {
        Self {
            ctrl: self.ctrl || other.ctrl,
            alt: self.alt || other.alt,
            shift: self.shift || other.shift,
        }
    }

    /// The latch a desktop hardware key event carries — `Modifiers`'
    /// ctrl/alt/shift, applied once to that event and never retained.
    pub fn from_modifiers(mods: Modifiers) -> Self {
        Self {
            ctrl: mods.ctrl,
            alt: mods.alt,
            shift: mods.shift,
        }
    }
}

/// Whether `data` is exactly **one UTF-16 code unit**, and if so its value.
///
/// The latch's eligibility test: `'a'` (1 byte) and `'é'` (2 bytes) are both one
/// unit and both transformable; an emoji (2 units) and `\x1b[A` (3 units) are
/// not. Invalid UTF-8 is never eligible.
pub fn single_unit(data: &[u8]) -> Option<u32> {
    let text = std::str::from_utf8(data).ok()?;
    let mut chars = text.chars();
    let ch = chars.next()?;
    if chars.next().is_some() || ch.len_utf16() != 1 {
        return None;
    }
    Some(ch as u32)
}

/// Apply `latch` to one outgoing emission.
///
/// The rules, verbatim from the incumbent:
///
/// 1. **Only a single-code-unit emission is transformed.** Anything longer (an
///    arrow sequence, a swipe-typed word, a backspace run) passes through
///    **unmodified** — and, at the call site, leaves the latch armed.
/// 2. **Ctrl** uppercases a lowercase ASCII letter (`0x61..=0x7a` → `−0x20`)
///    and then masks `& 0x1f` **only if** the (uppercased) code lands in
///    `0x40..=0x5f`. Outside that window there is no Ctrl transform at all —
///    `Ctrl+2` sends `'2'` — though Alt may still apply.
/// 3. **Alt** prefixes [`ESC`], the `ESC`-prefix convention every mainstream
///    terminal uses for Meta.
/// 4. **Shift is not in this transform at all** — see [`ModifierLatch`]. It
///    selects a different *sequence* upstream, so an emission with only `shift`
///    armed passes through byte for byte, exactly as an unarmed one does.
pub fn apply_latch(latch: ModifierLatch, data: &[u8]) -> Vec<u8> {
    let Some(unit) = single_unit(data) else {
        return data.to_vec();
    };
    if !latch.ctrl && !latch.alt {
        return data.to_vec();
    }
    let mut out = Vec::with_capacity(data.len() + 1);
    if latch.alt {
        out.extend_from_slice(ESC);
    }
    if latch.ctrl {
        let upper = if (0x61..=0x7a).contains(&unit) {
            unit - 0x20
        } else {
            unit
        };
        if (0x40..=0x5f).contains(&upper) {
            out.push((upper & 0x1f) as u8);
            return out;
        }
    }
    out.extend_from_slice(data);
    out
}

#[cfg(test)]
mod tests {
    use frust::authoring::{Key, NamedKey};

    use super::super::bytes::{ARROW_UP, BarKey};
    use super::super::harness::{hex, type_text};
    use super::super::source::KeystrokeSource;
    use super::*;

    #[test]
    fn the_latch_table_matches_the_incumbent_verbatim() {
        let ctrl = ModifierLatch {
            ctrl: true,
            ..ModifierLatch::NONE
        };
        let alt = ModifierLatch {
            alt: true,
            ..ModifierLatch::NONE
        };
        let both = ModifierLatch {
            ctrl: true,
            alt: true,
            ..ModifierLatch::NONE
        };
        let shift = ModifierLatch {
            shift: true,
            ..ModifierLatch::NONE
        };
        let ctrl_shift = ModifierLatch {
            ctrl: true,
            shift: true,
            ..ModifierLatch::NONE
        };
        let cases: [(ModifierLatch, &[u8], &str, &str); 12] = [
            (ctrl, b"c", "03", "Ctrl+c is ETX"),
            (
                ctrl,
                b"C",
                "03",
                "an already-uppercase letter masks the same",
            ),
            (ctrl, b"[", "1b", "0x5b is inside the window"),
            (ctrl, b"2", "32", "0x32 is outside: no Ctrl transform"),
            (alt, b"c", "1b63", "Alt prefixes ESC"),
            (both, b"a", "1b01", "Alt's ESC, then Ctrl's masked byte"),
            (both, b"2", "1b32", "outside the window, Alt still applies"),
            (
                ctrl,
                ARROW_UP,
                "1b5b41",
                "a multi-unit sequence passes through",
            ),
            (
                ctrl,
                "\u{1f600}".as_bytes(),
                "f09f9880",
                "a surrogate pair is two units: untouched",
            ),
            (shift, b"a", "61", "Shift never rewrites a character"),
            (
                shift,
                b"A",
                "41",
                "…including the one the IME already cased",
            ),
            (
                ctrl_shift,
                b"c",
                "03",
                "…and adds nothing to Ctrl's own transform",
            ),
        ];
        for (latch, data, expected, why) in cases {
            assert_eq!(hex(&apply_latch(latch, data)), expected, "{why}");
        }
    }

    #[test]
    fn a_single_unit_emission_disarms_both_flags_and_a_longer_one_does_not() {
        let mut source = KeystrokeSource::new();
        source.on_bar_key(BarKey::Ctrl);
        source.on_bar_key(BarKey::Alt);
        assert_eq!(
            source.latch(),
            ModifierLatch {
                ctrl: true,
                alt: true,
                shift: false
            }
        );

        // An arrow is three units: passes through AND leaves the latch armed.
        let arrow = source.on_bar_key(BarKey::Up);
        assert_eq!(hex(&arrow.bytes), "1b5b41");
        assert!(source.latch().armed(), "arrows leave the latch armed");

        // …so the next single-unit emission still sees both flags.
        let typed = type_text(&mut source, "a");
        assert_eq!(hex(&typed), "1b01", "Ctrl+Alt+a");
        assert_eq!(
            source.latch(),
            ModifierLatch::NONE,
            "both flags clear together"
        );
    }

    #[test]
    fn ctrl_outside_the_window_still_consumes_the_latch() {
        let mut source = KeystrokeSource::new();
        source.on_bar_key(BarKey::Ctrl);
        assert_eq!(hex(&type_text(&mut source, "2")), "32", "'2' is unchanged");
        assert!(!source.latch().armed(), "…but the latch is consumed");
    }

    #[test]
    fn the_latch_transforms_a_soft_keyboard_character_too() {
        // The whole reason the latch lives on the emission path: `Ctrl` armed
        // from the bar, `c` typed on the OS keyboard.
        let mut source = KeystrokeSource::new();
        source.on_bar_key(BarKey::Ctrl);
        assert_eq!(hex(&type_text(&mut source, "c")), "03");
    }

    #[test]
    fn a_sequence_key_consumes_the_shift_it_used() {
        // Shift's one shot is spent by the emission that actually selected a
        // variant with it — the arrow, not the character after it.
        let mut source = KeystrokeSource::new();
        source.on_bar_key(BarKey::Shift);
        assert!(source.latch().shift, "armed by its own tap");

        let arrow = source.on_bar_key(BarKey::Up);
        assert_eq!(hex(&arrow.bytes), "1b5b313b3241", "Shift+↑");
        assert_eq!(source.latch(), ModifierLatch::NONE, "…and it is spent");

        // …so the next arrow is the plain one again.
        assert_eq!(hex(&source.on_bar_key(BarKey::Up).bytes), "1b5b41");
    }

    #[test]
    fn a_shift_that_selected_nothing_survives_a_multi_unit_emission() {
        // `Esc` is one code unit, so it spends the latch the ordinary way; a
        // key with no shifted variant AND more than one unit spends nothing.
        let mut source = KeystrokeSource::new();
        source.on_bar_key(BarKey::Shift);
        source.on_bar_key(BarKey::Esc);
        assert_eq!(source.latch(), ModifierLatch::NONE, "Esc is one unit");

        let mut source = KeystrokeSource::new();
        source.on_bar_key(BarKey::Shift);
        let home = source.on_key(&Key::Named(NamedKey::Home), Modifiers::default());
        assert_eq!(hex(&home.bytes), "1b5b48", "Home has no shifted variant");
        assert!(source.latch().shift, "…so nothing consumed the Shift");
    }

    #[test]
    fn a_shift_only_latch_is_spent_by_a_character_it_did_not_touch() {
        let mut source = KeystrokeSource::new();
        source.on_bar_key(BarKey::Shift);
        assert_eq!(
            hex(&type_text(&mut source, "a")),
            "61",
            "the byte is the one the IME handed over"
        );
        assert!(!source.latch().armed(), "…and the one shot is still spent");
    }

    #[test]
    fn an_arrow_consumes_shift_and_leaves_ctrl_armed_for_the_next_character() {
        // The documented asymmetry, end to end: Shift belongs to the sequence
        // key, Ctrl belongs to the character after it (see `ModifierLatch`).
        let mut source = KeystrokeSource::new();
        source.on_bar_key(BarKey::Ctrl);
        source.on_bar_key(BarKey::Shift);

        let arrow = source.on_bar_key(BarKey::Right);
        assert_eq!(
            hex(&arrow.bytes),
            "1b5b313b3243",
            "Shift's variant, with NO Ctrl arithmetic folded in"
        );
        assert_eq!(
            source.latch(),
            ModifierLatch {
                ctrl: true,
                alt: false,
                shift: false
            },
            "Shift spent, Ctrl still armed"
        );

        assert_eq!(hex(&type_text(&mut source, "c")), "03", "Ctrl+c");
        assert_eq!(source.latch(), ModifierLatch::NONE);
    }
}
