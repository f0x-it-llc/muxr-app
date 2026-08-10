//! The one-shot Ctrl/Alt latch, and the transform every outgoing emission
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModifierLatch {
    /// `Ctrl` is armed for the next single-code-unit emission.
    pub ctrl: bool,
    /// `Alt` is armed for the next single-code-unit emission.
    pub alt: bool,
}

impl ModifierLatch {
    /// Nothing armed.
    pub const NONE: Self = Self {
        ctrl: false,
        alt: false,
    };

    /// Whether either flag is armed.
    pub fn armed(self) -> bool {
        self.ctrl || self.alt
    }

    /// Both flags of either side (used to fold a hardware key event's own
    /// modifiers into the latch for that one emission).
    pub fn union(self, other: Self) -> Self {
        Self {
            ctrl: self.ctrl || other.ctrl,
            alt: self.alt || other.alt,
        }
    }

    /// The latch a desktop hardware key event carries — `Modifiers`' ctrl/alt,
    /// applied once to that event and never retained.
    pub fn from_modifiers(mods: Modifiers) -> Self {
        Self {
            ctrl: mods.ctrl,
            alt: mods.alt,
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
pub fn apply_latch(latch: ModifierLatch, data: &[u8]) -> Vec<u8> {
    let Some(unit) = single_unit(data) else {
        return data.to_vec();
    };
    if !latch.armed() {
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
    use super::super::bytes::{ARROW_UP, BarKey};
    use super::super::harness::{hex, type_text};
    use super::super::source::KeystrokeSource;
    use super::*;

    #[test]
    fn the_latch_table_matches_the_incumbent_verbatim() {
        let ctrl = ModifierLatch {
            ctrl: true,
            alt: false,
        };
        let alt = ModifierLatch {
            ctrl: false,
            alt: true,
        };
        let both = ModifierLatch {
            ctrl: true,
            alt: true,
        };
        let cases: [(ModifierLatch, &[u8], &str, &str); 9] = [
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
                alt: true
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
}
