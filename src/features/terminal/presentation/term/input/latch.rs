//! The one-shot Ctrl/Alt/Shift latch, applied at the emission choke point rather than the key bar,
//! so armed `Ctrl` plus an OS-keyboard `c` sends `0x03`.

use frust::authoring::Modifiers;

use super::bytes::ESC;

/// `ctrl`/`alt` transform a single-unit character and stay armed across a sequence key.
/// `shift` never touches a character (the IME already cased it); it selects a shifted sequence.
/// No chord arithmetic: armed Ctrl then Shift+Up sends `\x1b[1;2A` and leaves Ctrl armed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModifierLatch {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl ModifierLatch {
    pub const NONE: Self = Self {
        ctrl: false,
        alt: false,
        shift: false,
    };

    pub fn armed(self) -> bool {
        self.ctrl || self.alt || self.shift
    }

    pub fn union(self, other: Self) -> Self {
        Self {
            ctrl: self.ctrl || other.ctrl,
            alt: self.alt || other.alt,
            shift: self.shift || other.shift,
        }
    }

    /// Applied once to that event and never retained.
    pub fn from_modifiers(mods: Modifiers) -> Self {
        Self {
            ctrl: mods.ctrl,
            alt: mods.alt,
            shift: mods.shift,
        }
    }
}

/// The latch's eligibility test: exactly one UTF-16 unit. Invalid UTF-8 is never eligible.
pub fn single_unit(data: &[u8]) -> Option<u32> {
    let text = std::str::from_utf8(data).ok()?;
    let mut chars = text.chars();
    let ch = chars.next()?;
    if chars.next().is_some() || ch.len_utf16() != 1 {
        return None;
    }
    Some(ch as u32)
}

/// Only a single-unit emission is transformed; a longer one passes and leaves the latch armed.
/// Ctrl masks `& 0x1f` only when the uppercased code lands in `0x40..=0x5f`; Alt prefixes [`ESC`].
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

        // An arrow is three units: it passes through and leaves the latch armed.
        let arrow = source.on_bar_key(BarKey::Up);
        assert_eq!(hex(&arrow.bytes), "1b5b41");
        assert!(source.latch().armed(), "arrows leave the latch armed");

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
        let mut source = KeystrokeSource::new();
        source.on_bar_key(BarKey::Ctrl);
        assert_eq!(hex(&type_text(&mut source, "c")), "03");
    }

    #[test]
    fn a_sequence_key_consumes_the_shift_it_used() {
        // Shift is spent by the arrow that selected a variant, not the character after it.
        let mut source = KeystrokeSource::new();
        source.on_bar_key(BarKey::Shift);
        assert!(source.latch().shift, "armed by its own tap");

        let arrow = source.on_bar_key(BarKey::Up);
        assert_eq!(hex(&arrow.bytes), "1b5b313b3241", "Shift+↑");
        assert_eq!(source.latch(), ModifierLatch::NONE, "…and it is spent");

        assert_eq!(hex(&source.on_bar_key(BarKey::Up).bytes), "1b5b41");
    }

    #[test]
    fn a_shift_that_selected_nothing_survives_a_multi_unit_emission() {
        // `Esc` is one unit and spends the latch; a longer key with no shifted variant does not.
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
