//! Coalesces one emulator screen into same-style [`GridRun`]s: each run is one `draw_glyphs`
//! call, and [`RunBatch`] keeps its run slots (and their `String`s) across generations.
//! Trailing blanks over the default background paint nothing; interior blanks never split a run.

use frust::authoring::text::TextLayout;

use super::style::CellStyle;

pub struct GridRun {
    row: u16,
    col: u16,
    /// Columns covered, in cells (a wide cell counts 2).
    cells: u16,
    style: CellStyle,
    text: String,
    layout: Option<TextLayout>,
}

impl GridRun {
    fn blank() -> Self {
        Self {
            row: 0,
            col: 0,
            cells: 0,
            style: CellStyle::default(),
            text: String::new(),
            layout: None,
        }
    }

    pub fn row(&self) -> u16 {
        self.row
    }

    pub fn col(&self) -> u16 {
        self.col
    }

    pub fn cells(&self) -> u16 {
        self.cells
    }

    pub fn style(&self) -> CellStyle {
        self.style
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn layout(&self) -> Option<&TextLayout> {
        self.layout.as_ref()
    }

    pub fn set_layout(&mut self, layout: TextLayout) {
        self.layout = Some(layout);
    }
}

#[derive(Default)]
pub struct RunBatch {
    runs: Vec<GridRun>,
    len: usize,
}

impl RunBatch {
    pub fn active(&self) -> &[GridRun] {
        &self.runs[..self.len]
    }

    pub fn active_mut(&mut self) -> &mut [GridRun] {
        &mut self.runs[..self.len]
    }

    pub fn count(&self) -> usize {
        self.len
    }

    pub fn capacity(&self) -> usize {
        self.runs.len()
    }

    fn begin(&mut self) {
        self.len = 0;
    }

    fn open(&mut self, row: u16, col: u16, style: CellStyle) {
        if self.runs.len() == self.len {
            self.runs.push(GridRun::blank());
        }
        let slot = &mut self.runs[self.len];
        slot.row = row;
        slot.col = col;
        slot.cells = 0;
        slot.style = style;
        slot.text.clear();
        slot.layout = None;
    }

    fn push_cell(&mut self, glyph: &str, span: u16) {
        let slot = &mut self.runs[self.len];
        slot.text.push_str(glyph);
        slot.cells += span;
    }

    fn commit(&mut self, default_bg: bool) {
        let slot = &mut self.runs[self.len];
        if slot.cells == 0 {
            return;
        }
        if default_bg {
            let trimmed = slot.text.trim_end_matches(' ').len();
            // One column per trimmed space, so an SGR underline does not trail across blanks.
            let dropped = slot.text.len() - trimmed;
            slot.text.truncate(trimmed);
            slot.cells = slot.cells.saturating_sub(dropped as u16);
            if slot.text.is_empty() {
                return;
            }
        }
        self.len += 1;
    }
}

/// Walks the screen's own size so the batch cannot disagree with the emulator;
/// `is_default_bg` decides which runs are blank-elidable.
pub fn batch_screen(
    screen: &vt100::Screen,
    out: &mut RunBatch,
    mut is_default_bg: impl FnMut(CellStyle) -> bool,
) {
    let (rows, cols) = screen.size();
    out.begin();
    for row in 0..rows {
        let mut open: Option<CellStyle> = None;
        for col in 0..cols {
            let cell = screen.cell(row, col);
            if cell.is_some_and(vt100::Cell::is_wide_continuation) {
                continue;
            }
            let style = cell.map(CellStyle::from_cell).unwrap_or_default();
            let glyph = match cell {
                Some(c) if c.has_contents() => c.contents(),
                // A never-written cell is empty; a space keeps later glyphs in their columns.
                _ => " ",
            };
            let span = if cell.is_some_and(vt100::Cell::is_wide) {
                2
            } else {
                1
            };
            if open != Some(style) {
                if let Some(previous) = open {
                    out.commit(is_default_bg(previous));
                }
                out.open(row, col, style);
                open = Some(style);
            }
            out.push_cell(glyph, span);
        }
        if let Some(previous) = open {
            out.commit(is_default_bg(previous));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::terminal::presentation::term::style::TerminalPalette;

    const COLS: u16 = 80;
    const ROWS: u16 = 24;

    fn batch_of(bytes: &[u8]) -> RunBatch {
        let mut parser = vt100::Parser::new(ROWS, COLS, 0);
        parser.process(bytes);
        let mut batch = RunBatch::default();
        let palette = TerminalPalette::fallback();
        batch_screen(parser.screen(), &mut batch, |style| {
            style.resolve(&palette).1.is_none()
        });
        batch
    }

    #[test]
    fn a_blank_screen_batches_to_zero_runs() {
        assert_eq!(
            batch_of(b"").count(),
            0,
            "a blank default-background screen paints no runs"
        );
    }

    #[test]
    fn same_style_cells_coalesce_and_a_style_change_splits() {
        let batch = batch_of(b"\x1b[31mAAA\x1b[32mBBB");
        let runs = batch.active();
        assert_eq!(runs.len(), 2, "one run per style, not one per cell");
        assert_eq!(
            (runs[0].row(), runs[0].col(), runs[0].text()),
            (0, 0, "AAA")
        );
        assert_eq!(
            (runs[1].row(), runs[1].col(), runs[1].text()),
            (0, 3, "BBB")
        );
    }

    #[test]
    fn bold_splits_a_run_even_at_an_unchanged_colour() {
        let batch = batch_of(b"ab\x1b[1mcd\x1b[0mef");
        let runs = batch.active();
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].text(), "ab");
        assert_eq!(runs[1].text(), "cd");
        assert_eq!(runs[2].text(), "ef");
        assert!(runs[1].style().bold);
        assert!(!runs[0].style().bold);
    }

    #[test]
    fn interior_blanks_stay_inside_a_run_and_trailing_blanks_are_trimmed() {
        let batch = batch_of(b"ab   cd");
        let runs = batch.active();
        assert_eq!(runs.len(), 1, "interior spaces must not split a run");
        assert_eq!(
            runs[0].text(),
            "ab   cd",
            "and the blank tail is not shaped"
        );
    }

    #[test]
    fn a_never_written_cell_substitutes_a_space_so_columns_do_not_shift() {
        // Column 10 in the same style: the untouched cells between must become spaces.
        let batch = batch_of(b"a\x1b[1;11Hb");
        let runs = batch.active();
        assert_eq!(runs.len(), 1, "one style spans the gap");
        assert_eq!(runs[0].text(), "a         b");
        assert_eq!(runs[0].col(), 0);
        assert_eq!(runs[0].cells(), 11);
    }

    #[test]
    fn a_non_default_background_run_survives_even_when_blank() {
        let batch = batch_of(b"\x1b[41m   \x1b[0m");
        let runs = batch.active();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text(), "   ");
        let palette = TerminalPalette::fallback();
        assert!(
            runs[0].style().resolve(&palette).1.is_some(),
            "it has a background rect to paint"
        );
    }

    #[test]
    fn a_wide_cell_spans_two_columns_and_its_continuation_contributes_nothing() {
        let batch = batch_of("\u{4f60}a".as_bytes());
        let runs = batch.active();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text(), "\u{4f60}a");
        assert_eq!(
            runs[0].cells(),
            3,
            "two columns for the wide cell, one for the ASCII"
        );
    }

    #[test]
    fn per_row_batching_keeps_runs_on_their_own_rows() {
        let batch = batch_of(b"aa\r\nbb");
        let runs = batch.active();
        assert_eq!(runs.len(), 2);
        assert_eq!((runs[0].row(), runs[0].text()), (0, "aa"));
        assert_eq!((runs[1].row(), runs[1].text()), (1, "bb"));
    }

    #[test]
    fn the_batch_reuses_its_run_allocations_across_generations() {
        let mut parser = vt100::Parser::new(ROWS, COLS, 0);
        let mut batch = RunBatch::default();
        let palette = TerminalPalette::fallback();
        let run = |parser: &vt100::Parser, batch: &mut RunBatch| {
            batch_screen(parser.screen(), batch, |style| {
                style.resolve(&palette).1.is_none()
            });
        };

        parser.process(b"\x1b[31mAAA\x1b[32mBBB");
        run(&parser, &mut batch);
        assert_eq!(batch.count(), 2);
        // Capacity also counts the scratch slot a row's blank tail opened and discarded.
        let capacity = batch.capacity();
        assert!(capacity >= batch.count());

        parser.process(b"\x1b[H\x1b[0maaaaaa");
        run(&parser, &mut batch);
        assert_eq!(batch.count(), 1);
        assert_eq!(
            batch.capacity(),
            capacity,
            "slots are retained, not dropped"
        );
    }
}
