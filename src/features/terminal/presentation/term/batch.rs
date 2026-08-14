//! Same-style run batching: one emulator screen in, a reusable list of [`GridRun`]s
//! out.
//!
//! **Batching is the whole performance story.** Each `GlyphRun` the scene layer
//! emits becomes exactly one vello `draw_glyphs` call, and the text engine's shape
//! cache is a bounded LRU keyed on `(text, style)`. [`batch_screen`] coalesces
//! consecutive cells sharing a whole [`CellStyle`] into **one** shaped run and
//! **one** background rectangle — never a draw per cell. That is the property S9
//! measured, and the reason a 54x60 grid costs ~130 shapes a frame on a build log
//! rather than ~3200.
//!
//! Four things the naive loop gets wrong:
//!
//! 1. **A never-written cell has no contents.** `vt100` returns an empty string for
//!    a cell nothing printed to; pushing that would shift every glyph after it one
//!    column left, so a space is substituted.
//! 2. **A wide character owns two columns.** Its continuation cell contributes no
//!    glyph of its own, so it is skipped and the run's cell span advances by 2.
//! 3. **Trailing blanks over the default background paint nothing.** They are
//!    trimmed, and a run left empty by that is discarded — most of a terminal screen
//!    most of the time. A run with a **non-default** background always commits: its
//!    rect paints where its text does not.
//! 4. **Interior blanks are not a run boundary.** Splitting on them would turn every
//!    word of every line into its own draw call.
//!
//! **Allocation reuse**: [`RunBatch`] retains its `GridRun` slots (and their
//! `String`s) across generations — `runs[..len]` are live, `runs[len..]` are
//! retained empties the next batch re-fills. At 30 repaints a second, re-allocating
//! a few hundred `String`s per generation is the difference between a terminal and a
//! garbage collector, so a test pins it.

use frust::authoring::text::TextLayout;

use super::style::CellStyle;

/// One batched run: consecutive same-style cells on one row, shaped once.
pub struct GridRun {
    row: u16,
    col: u16,
    /// Columns covered — the background rect's width in cells (a wide cell
    /// counts 2).
    cells: u16,
    style: CellStyle,
    text: String,
    /// Filled by the shaping pass in the widget's `layout`.
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

    /// The run's row.
    pub fn row(&self) -> u16 {
        self.row
    }

    /// The run's starting column.
    pub fn col(&self) -> u16 {
        self.col
    }

    /// Columns the run covers.
    pub fn cells(&self) -> u16 {
        self.cells
    }

    /// The run's style.
    pub fn style(&self) -> CellStyle {
        self.style
    }

    /// The run's text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The shaped layout, once the widget's `layout` pass has filled it in.
    pub fn layout(&self) -> Option<&TextLayout> {
        self.layout.as_ref()
    }

    /// Attach a shaped layout (the shaping pass's only mutation).
    pub fn set_layout(&mut self, layout: TextLayout) {
        self.layout = Some(layout);
    }
}

/// A reusable batch of [`GridRun`]s — see the [module docs](self)' allocation
/// note.
#[derive(Default)]
pub struct RunBatch {
    runs: Vec<GridRun>,
    len: usize,
}

impl RunBatch {
    /// The live runs.
    pub fn active(&self) -> &[GridRun] {
        &self.runs[..self.len]
    }

    /// The live runs, mutably — the shaping pass fills each run's layout.
    pub fn active_mut(&mut self) -> &mut [GridRun] {
        &mut self.runs[..self.len]
    }

    /// How many runs the current batch holds.
    pub fn count(&self) -> usize {
        self.len
    }

    /// Retained slots, live or not — the allocation-reuse assertion's window
    /// into this structure.
    pub fn capacity(&self) -> usize {
        self.runs.len()
    }

    /// Drop every live run (keeping its allocation) and start a fresh batch.
    fn begin(&mut self) {
        self.len = 0;
    }

    /// Start a run at `(row, col)` in the slot after the committed ones.
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

    /// Append one cell's glyph(s) to the open run, advancing its span by
    /// `span` (2 for a wide character — see the [module docs](self)).
    fn push_cell(&mut self, glyph: &str, span: u16) {
        let slot = &mut self.runs[self.len];
        slot.text.push_str(glyph);
        slot.cells += span;
    }

    /// Commit the open run, or discard it if nothing would paint (see the
    /// [module docs](self)' point 3).
    fn commit(&mut self, default_bg: bool) {
        let slot = &mut self.runs[self.len];
        if slot.cells == 0 {
            return;
        }
        if default_bg {
            let trimmed = slot.text.trim_end_matches(' ').len();
            // Each trimmed character is an ASCII space, i.e. exactly one byte and one
            // column — so the span shrinks with the text, and a decoration spanning
            // the run (an SGR underline) cannot trail off across blanks.
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

/// Batch one emulator screen into same-style runs (see the [module docs](self)).
/// Walks the screen's **own** size rather than a caller-supplied grid, so the batch
/// can never disagree with what the emulator actually holds. `is_default_bg` decides
/// which runs are blank-elidable; it is the palette's question, threaded in as a
/// closure so this module stays free of colour resolution.
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
                // A never-written cell: a space, or every glyph after it shifts
                // a column left.
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

    /// A parser fed `bytes`, batched into runs against the app palette.
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
        // Two SGR colours, three cells each: two runs, not six.
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
        // Print at column 0, then jump to column 10 in the same style: the eight
        // untouched cells between must contribute eight spaces, or `b` paints at
        // column 2.
        let batch = batch_of(b"a\x1b[1;11Hb");
        let runs = batch.active();
        assert_eq!(runs.len(), 1, "one style spans the gap");
        assert_eq!(runs[0].text(), "a         b");
        assert_eq!(runs[0].col(), 0);
        assert_eq!(runs[0].cells(), 11);
    }

    #[test]
    fn a_non_default_background_run_survives_even_when_blank() {
        // A coloured background paints even with no glyphs, so a blank run over
        // one must not be discarded.
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
        // A CJK ideograph is two columns wide; the run must carry one glyph and
        // a span of 2, and the ASCII after it must land on column 2.
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
        // The slot vec also holds the uncommitted scratch slot each row's blank
        // tail opened and discarded, so capacity >= committed count.
        let capacity = batch.capacity();
        assert!(capacity >= batch.count());

        // A screen that batches to fewer runs must keep every slot for reuse.
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
