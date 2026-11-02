//! Tripwire for the transcribe-once rule: every constant `ui::splash::spec` quotes
//! from `docs/splash.html` is re-read out of that file here and compared.

use muxr::ui::splash::contract;

const HTML: &str = include_str!("../docs/splash.html");

/// The number at the front of `text`, parsed whole — digits and at most one
/// decimal point, so a fractional literal is never read as its integer part.
/// `what` names it in the failure.
fn leading_number(text: &str, what: &str) -> f64 {
    let mut point_seen = false;
    let literal: String = text
        .chars()
        .take_while(|c| {
            c.is_ascii_digit() || (*c == '.' && !std::mem::replace(&mut point_seen, true))
        })
        .collect();
    assert!(
        !literal.is_empty(),
        "no number after `{what}` in docs/splash.html"
    );
    literal
        .parse()
        .unwrap_or_else(|e| panic!("`{literal}` after `{what}` is not an f64: {e}"))
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
fn the_parse_reads_a_fractional_literal_whole() {
    assert_eq!(leading_number("900;", "a whole literal"), 900.0);
    assert_eq!(leading_number("14, colW = 14;", "a list"), 14.0);
    assert_eq!(leading_number("14.9px", "a fraction"), 14.9);
    // The drift a truncating parse would wave through: a mock retuned to 14.9
    // must not still read as the 14.0 it was transcribed at.
    assert_ne!(leading_number("14.9px", "a fraction"), 14.0);
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

#[test]
fn cta_row_and_unlock_dock_geometry_match_the_mock() {
    same(".cta-row{display:flex;gap:", contract::CTA_ROW_GAP);
    same(".qr-square{width:", contract::QR_SQUARE);
    same(".unlock-dock{height:", contract::DOCK_HEIGHT);
    same(".unlock-dock{height:48px;margin-top:", contract::DOCK_GAP);
}

#[test]
fn piece_seven_is_the_cta_row_and_piece_eight_closes_on_the_dock() {
    let pieces: Vec<&str> = HTML.split("piece\" data-delay=\"").skip(1).collect();
    assert_eq!(pieces.len(), contract::PIECE_DELAYS_MS.len());
    let before = |i: usize| {
        HTML.split("piece\" data-delay=\"")
            .nth(i)
            .unwrap_or_default()
    };
    assert!(before(6).ends_with("cta-row "), "piece 7 is the CTA row");
    assert!(pieces[6].contains("qr-square"), "…carrying the QR square");
    assert!(before(7).ends_with("saved "), "piece 8 is the saved list");
    assert!(pieces[7].contains("unlock-dock"), "…closed by the dock");
}
