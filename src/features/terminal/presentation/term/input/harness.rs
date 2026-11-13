use frust::authoring::EditingState;

use super::source::KeystrokeSource;
use super::utf16::len_utf16;

pub(super) fn snapshot(text: &str) -> EditingState {
    let end = i32::try_from(len_utf16(text)).unwrap_or(i32::MAX);
    EditingState {
        text: text.to_string(),
        selection_base: end,
        selection_extent: end,
        composing_base: -1,
        composing_extent: -1,
    }
}

pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Each snapshot is the current mirror plus one character: bridges re-anchor synchronously.
pub(super) fn type_text(source: &mut KeystrokeSource, text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for ch in text.chars() {
        let mut buffer = source.mirror().to_string();
        buffer.push(ch);
        out.extend(source.on_snapshot(&snapshot(&buffer)).bytes);
    }
    out
}
