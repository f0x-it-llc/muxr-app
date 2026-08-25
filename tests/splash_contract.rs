//! Tripwire for the transcribe-once rule: every constant `ui::splash::spec` quotes
//! from `docs/splash.html` is re-read out of that file here and compared.

use muxr::ui::splash::contract;

const HTML: &str = include_str!("../docs/splash.html");

/// The digits at the front of `text`, parsed. `what` names them in the failure.
fn leading_number(text: &str, what: &str) -> f64 {
    let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
    assert!(
        !digits.is_empty(),
        "no number after `{what}` in docs/splash.html"
    );
    digits.parse().expect("ascii digits parse as f64")
}

/// The number following `needle`'s first occurrence in `hay`.
fn number_after(hay: &str, needle: &str) -> f64 {
    let at = hay
        .find(needle)
        .unwrap_or_else(|| panic!("`{needle}` is gone from docs/splash.html"));
    leading_number(&hay[at + needle.len()..], needle)
}

/// Asserts the number the mock writes after `needle` still equals `transcribed`.
fn same(needle: &str, transcribed: f64) {
    assert_eq!(
        number_after(HTML, needle),
        transcribed,
        "`{needle}` in docs/splash.html drifted from its ui::splash::spec transcription"
    );
}

#[test]
fn icon_beats_match_the_mock() {
    same("var ICON_IN = ", contract::ICON_IN_MS);
    same("var ICON_HOLD = ", contract::ICON_HOLD_MS);
}

#[test]
fn rain_phase_edges_match_the_mock() {
    same("var DENSE_END = ", contract::DENSE_END_MS);
    same("var THIN_END = ", contract::THIN_END_MS);
    same("THIN_END + ", contract::RAIN_TAIL_MS);
}

#[test]
fn rain_grid_matches_the_mock() {
    same("var fontSize = ", f64::from(contract::RAIN_FONT_SIZE));
    same("colW = ", contract::RAIN_COL_W);
}

#[test]
fn piece_entrance_matches_the_mock() {
    same("var PIECE_IN = ", contract::PIECE_IN_MS);
}

#[test]
fn burst_count_matches_the_mock() {
    let spawn = HTML
        .split_once("function spawnBurst()")
        .expect("`spawnBurst` is gone from docs/splash.html")
        .1;
    assert_eq!(
        number_after(spawn, "for(var i=0;i<"),
        contract::BURST_COUNT as f64,
        "spawnBurst's particle count drifted from spec::BURST_COUNT"
    );
}

#[test]
fn piece_delays_match_the_mock() {
    let delays: Vec<f64> = HTML
        .split("data-delay=\"")
        .skip(1)
        .map(|rest| leading_number(rest, "data-delay"))
        .collect();
    assert_eq!(
        delays.as_slice(),
        &contract::PIECE_DELAYS_MS[..],
        "the `.piece` data-delay ladder drifted from spec::PIECE_DELAYS_MS"
    );
}
