//! The **pure** selection model: which cells a long-press-and-drag selects, and
//! what text those cells copy to. No `Rc`, no signal, no paint, no side effect —
//! [`super::SelectionSeam`] is the runtime half, the same split
//! [`input`](super::input) and [`InputSeam`](super::InputSeam) use.
//!
//! **Everything here speaks GRID coordinates.** A [`GridPoint`] is a `(row, col)`
//! cell address on the **rendered** screen. That is the emulator's *visible*
//! screen today; when a scrollback view lands it arrives as a **row offset**
//! applied where pixels become cells ([`cell_at`]) and where cells become text
//! ([`selected_text`]), and nothing else here changes.
//!
//! **The word rule is ported verbatim**: boundaries are **per line** and the
//! separator set is [`SEPARATORS`], so expansion never crosses a row (though the
//! *text* those cells copy to still joins without a newline) and landing on a
//! separator selects nothing while the long-press haptic still fires — the
//! incumbent's own ordering. A drag **re-resolves BOTH ends** ([`resolve`]), so
//! reversing direction is symmetric by construction rather than by a special case.
//!
//! **Streaming output clears a held selection.** Because a [`GridPoint`] addresses
//! the rendered screen and nothing here buffer-anchors it, new output scrolling
//! the screen underneath a held selection would silently retarget it — Copy would
//! hand back whatever now sits at those rows. Rather than try to tell "this
//! repaint scrolled" apart from "this repaint didn't",
//! [`SelectionSeam::observe_generation`] drops a held selection outright the moment
//! *any* new content generation arrives: a real behaviour change from the
//! buffer-anchored Flutter incumbent, and the honest default until a scrollback
//! view gives a selection something stable to hold onto.

use frust::authoring::Point;

use super::geometry::CellGeometry;

/// The characters that end a word — the incumbent's set, verbatim. `NUL` is in it
/// because that is what a never-written cell reads back as ([`char_at`]): blank
/// screen space is a separator, not a very wide word.
pub const SEPARATORS: [char; 10] = ['\0', ' ', '.', ':', '-', '\\', '"', '*', '+', '/'];

/// Whether `c` ends a word (see [`SEPARATORS`]).
pub fn is_separator(c: char) -> bool {
    SEPARATORS.contains(&c)
}

/// One cell on the rendered grid. `Ord` is derived with `row` **first**, so the
/// natural ordering is row-major reading order — what [`GridRange::merge`] needs,
/// and the reason the field order is not an implementation detail.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct GridPoint {
    /// Row on the rendered screen, 0 = top.
    pub row: u16,
    /// Column, 0 = left.
    pub col: u16,
}

impl GridPoint {
    /// A cell address.
    pub fn new(row: u16, col: u16) -> Self {
        Self { row, col }
    }
}

/// An **inclusive** row-major span of cells: everything from `begin` to `end`
/// in reading order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridRange {
    /// The earlier end, in reading order.
    pub begin: GridPoint,
    /// The later end, inclusive.
    pub end: GridPoint,
}

impl GridRange {
    /// The range covering the single cell `at`.
    pub fn cell(at: GridPoint) -> Self {
        Self { begin: at, end: at }
    }

    /// The smallest range covering both — earlier `begin`, later `end`, in
    /// row-major reading order. This is what makes a drag whose finger is
    /// **above** its anchor select the same cells as the mirror-image drag.
    #[must_use]
    pub fn merge(self, other: Self) -> Self {
        Self {
            begin: self.begin.min(other.begin),
            end: self.end.max(other.end),
        }
    }

    /// Whether `at` falls inside this range (row-major, inclusive).
    pub fn contains(self, at: GridPoint) -> bool {
        at >= self.begin && at <= self.end
    }

    /// Clamp both ends onto a `cols` x `rows` grid — what lets
    /// [`SelectionSeam::set_metrics`](super::SelectionSeam::set_metrics) keep a
    /// held selection valid the instant a grid shrinks under it, rather than
    /// carry a stale, off-screen `begin` into [`selected_text`].
    ///
    /// Each point clamps **independently** and neither is re-sorted: a row-only
    /// shrink can collapse both ends onto the same surviving row in an order
    /// that reads reversed, and [`selected_text`]/[`segments`] both already
    /// treat a reversed range as "nothing selected" — so that degrades to no
    /// text/no paint rather than a panic.
    #[must_use]
    pub fn clamp_to(self, cols: u16, rows: u16) -> Self {
        let last_col = cols.saturating_sub(1);
        let last_row = rows.saturating_sub(1);
        let clamp = |p: GridPoint| GridPoint::new(p.row.min(last_row), p.col.min(last_col));
        Self {
            begin: clamp(self.begin),
            end: clamp(self.end),
        }
    }
}

/// One painted stripe of a selection: `span` cells starting at `(row, col)`. A
/// selection is **line-wise**, not a rectangle: the first row runs from the begin
/// column to the end of the line, middle rows are full width, and the last runs to
/// the end column — which is what makes it look like selected *text*.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    /// The row this stripe paints on.
    pub row: u16,
    /// Its first column.
    pub col: u16,
    /// How many cells it covers (never zero).
    pub span: u16,
}

/// The cell a widget-local pixel `point` lands on, **clamped** into the grid `geo`
/// describes. Clamping rather than rejecting is deliberate: a drag routinely
/// leaves the widget's box, and the incumbent keeps extending to the nearest cell
/// instead of freezing. A degenerate geometry collapses to `(0, 0)`.
pub fn cell_at(point: Point, geo: CellGeometry) -> GridPoint {
    GridPoint {
        row: axis(point.y, geo.height, geo.rows),
        col: axis(point.x, geo.width, geo.cols),
    }
}

/// One axis of [`cell_at`]: `floor(value / cell)`, clamped to `0..count`.
fn axis(value: f64, cell: f64, count: u16) -> u16 {
    let last = count.saturating_sub(1);
    // Finiteness first, exactly as `geometry`'s own `axis_cells` orders it: a NaN
    // coordinate fails `is_finite` and lands on cell 0 rather than reaching a
    // comparison that would silently answer `false` either way.
    if !value.is_finite() || !cell.is_finite() || value <= 0.0 || cell <= 0.0 {
        return 0;
    }
    let index = (value / cell).floor();
    if !index.is_finite() {
        return 0;
    }
    index.clamp(0.0, f64::from(last)) as u16
}

/// The character in cell `at`, or `NUL` when the cell is blank or off-screen. A
/// **wide** character's second half carries no glyph of its own, so it reads back
/// the first half's character rather than `NUL` — otherwise every CJK word would
/// split at every other column.
pub fn char_at(screen: &vt100::Screen, at: GridPoint) -> char {
    let Some(cell) = screen.cell(at.row, at.col) else {
        return '\0';
    };
    if cell.is_wide_continuation() && at.col > 0 {
        return char_at(screen, GridPoint::new(at.row, at.col - 1));
    }
    cell.contents().chars().next().unwrap_or('\0')
}

/// The word under `at`, or `None` when `at` is a separator (see the [module
/// docs](self)). Expansion stops at the row's own ends.
pub fn word_at(screen: &vt100::Screen, at: GridPoint) -> Option<GridRange> {
    let (_, cols) = screen.size();
    if cols == 0 || at.col >= cols {
        return None;
    }
    if is_separator(char_at(screen, at)) {
        return None;
    }
    let mut begin = at.col;
    while begin > 0 && !is_separator(char_at(screen, GridPoint::new(at.row, begin - 1))) {
        begin -= 1;
    }
    let mut end = at.col;
    while end + 1 < cols && !is_separator(char_at(screen, GridPoint::new(at.row, end + 1))) {
        end += 1;
    }
    Some(GridRange {
        begin: GridPoint::new(at.row, begin),
        end: GridPoint::new(at.row, end),
    })
}

/// The selection a `(anchor, extent)` cell pair resolves to: the word at each,
/// merged in reading order. Both ends are re-resolved on every call. An end that
/// lands on a separator contributes nothing, so a drag over whitespace narrows
/// back to the other end's word; both ends on separators is no selection at all.
pub fn resolve(screen: &vt100::Screen, anchor: GridPoint, extent: GridPoint) -> Option<GridRange> {
    match (word_at(screen, anchor), word_at(screen, extent)) {
        (Some(a), Some(b)) => Some(a.merge(b)),
        (Some(only), None) | (None, Some(only)) => Some(only),
        (None, None) => None,
    }
}

/// The line-wise stripes `range` paints as, on a `cols`-wide grid (see
/// [`Segment`]). Columns past `cols - 1` are clamped, so a range resolved before a
/// resize still paints inside the grid it is painted on.
pub fn segments(range: GridRange, cols: u16) -> Vec<Segment> {
    let mut stripes = Vec::new();
    if cols == 0 || range.begin.row > range.end.row {
        return stripes;
    }
    let last_col = cols - 1;
    for row in range.begin.row..=range.end.row {
        let from = if row == range.begin.row {
            range.begin.col.min(last_col)
        } else {
            0
        };
        let to = if row == range.end.row {
            range.end.col.min(last_col)
        } else {
            last_col
        };
        if to >= from {
            stripes.push(Segment {
                row,
                col: from,
                span: to - from + 1,
            });
        }
    }
    stripes
}

/// The text `range` selects, as the clipboard should receive it.
///
/// Delegates to `vt100`'s own `Screen::contents_between`, which already implements
/// **exactly** the incumbent's join rule: trailing blanks on a row contribute
/// nothing; a row the emulator marked **wrapped** joins to the next with no
/// newline, so a wrapped command line or path pastes back as one line; a row that
/// ended for real joins with `\n`.
///
/// The one adaptation is the end bound: `contents_between`'s `end_col` is
/// **exclusive** while a [`GridRange`] is inclusive, so this passes `end.col + 1`.
/// Both ends are clamped onto the *current* screen before either reaches it — an
/// unclamped `begin` underflowed `contents_between`'s multi-row branch
/// (`cols - start_col`, in `u16`) the moment a held selection outlived a grid
/// shrink. `SelectionSeam::set_metrics` clamps the stored range too, so this is a
/// backstop. A reversed/empty range yields an empty string rather than a panic.
pub fn selected_text(screen: &vt100::Screen, range: GridRange) -> String {
    let (rows, cols) = screen.size();
    if rows == 0 || cols == 0 {
        return String::new();
    }
    let range = range.clamp_to(cols, rows);
    if range.begin.row > range.end.row
        || (range.begin.row == range.end.row && range.begin.col > range.end.col)
    {
        return String::new();
    }
    screen.contents_between(
        range.begin.row,
        range.begin.col,
        range.end.row,
        range.end.col + 1,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A screen `cols` x `rows` fed `bytes` — the same fixture shape
    /// [`super::super::batch`]'s own tests use.
    fn screen(cols: u16, rows: u16, bytes: &[u8]) -> vt100::Parser {
        let mut parser = vt100::Parser::new(rows, cols, 0);
        parser.process(bytes);
        parser
    }

    /// A geometry with 10x20px cells over a `cols` x `rows` grid — round
    /// numbers, so every pixel assertion below is exact.
    fn geometry(cols: u16, rows: u16) -> CellGeometry {
        CellGeometry {
            font_size: 12.0,
            width: 10.0,
            height: 20.0,
            cols,
            rows,
        }
    }

    #[test]
    fn the_separator_set_is_the_incumbents_and_everything_else_is_word_material() {
        for c in ['\0', ' ', '.', ':', '-', '\\', '"', '*', '+', '/'] {
            assert!(is_separator(c), "{c:?} ends a word");
        }
        for c in ['a', 'Z', '0', '_', '~', '@', '#', '$', '%', '=', ',', '\''] {
            assert!(!is_separator(c), "{c:?} is word material");
        }
    }

    #[test]
    fn a_long_press_selects_the_whole_word_under_the_finger() {
        let parser = screen(40, 4, b"cargo build --release");
        let screen = parser.screen();
        // Anywhere inside `build` (cols 6..=10) resolves to the same word.
        for col in 6..=10 {
            assert_eq!(
                word_at(screen, GridPoint::new(0, col)),
                Some(GridRange {
                    begin: GridPoint::new(0, 6),
                    end: GridPoint::new(0, 10),
                }),
                "col {col}"
            );
        }
    }

    #[test]
    fn landing_on_a_separator_selects_nothing() {
        let parser = screen(40, 4, b"cargo build");
        let screen = parser.screen();
        // Column 5 is the space between the two words…
        assert_eq!(word_at(screen, GridPoint::new(0, 5)), None);
        // …and so is every never-written cell past the end of the line.
        assert_eq!(word_at(screen, GridPoint::new(0, 30)), None);
        assert_eq!(word_at(screen, GridPoint::new(2, 0)), None);
    }

    #[test]
    fn every_separator_actually_splits_a_word_in_a_real_screen() {
        // The set is only load-bearing if the emulator agrees: a path, a URL and
        // a flag must each break at the marks the incumbent breaks at.
        let parser = screen(60, 2, b"a.b:c-d\\e\"f*g+h/i");
        let screen = parser.screen();
        for col in 0..17u16 {
            let expected = if col % 2 == 0 {
                Some(GridRange {
                    begin: GridPoint::new(0, col),
                    end: GridPoint::new(0, col),
                })
            } else {
                None
            };
            assert_eq!(
                word_at(screen, GridPoint::new(0, col)),
                expected,
                "col {col}"
            );
        }
    }

    #[test]
    fn word_boundaries_never_cross_a_line() {
        // Two rows of solid word material: the word on row 0 stops at the row's
        // own edges, never continuing onto row 1.
        let parser = screen(5, 3, b"aaaaa\r\nbbbbb");
        let screen = parser.screen();
        assert_eq!(
            word_at(screen, GridPoint::new(0, 2)),
            Some(GridRange {
                begin: GridPoint::new(0, 0),
                end: GridPoint::new(0, 4),
            })
        );
        assert_eq!(
            word_at(screen, GridPoint::new(1, 2)),
            Some(GridRange {
                begin: GridPoint::new(1, 0),
                end: GridPoint::new(1, 4),
            })
        );
    }

    #[test]
    fn a_drag_merges_both_ends_row_major_whichever_way_it_went() {
        let parser = screen(20, 4, b"alpha beta\r\ngamma delta");
        let screen = parser.screen();
        let anchor = GridPoint::new(0, 7); // inside `beta`
        let below = GridPoint::new(1, 1); // inside `gamma`

        let down = resolve(screen, anchor, below).expect("both ends are words");
        let up = resolve(screen, below, anchor).expect("the mirror drag");
        assert_eq!(down, up, "the merge is direction-independent");
        assert_eq!(
            down,
            GridRange {
                begin: GridPoint::new(0, 6), // `beta` starts at col 6
                end: GridPoint::new(1, 4),   // `gamma` ends at col 4
            }
        );
    }

    #[test]
    fn the_anchor_stays_put_when_the_drag_reverses_direction() {
        let parser = screen(30, 2, b"one two three four");
        let screen = parser.screen();
        let anchor = GridPoint::new(0, 5); // inside `two` (cols 4..=6)

        // Forward, onto `four` (cols 14..=17).
        let forward = resolve(screen, anchor, GridPoint::new(0, 15)).expect("resolved");
        assert_eq!(forward.begin, GridPoint::new(0, 4));
        assert_eq!(forward.end, GridPoint::new(0, 17));
        // Back past the anchor, onto `one` (cols 0..=2): the anchor word is
        // still in, and the range now runs the other way.
        let backward = resolve(screen, anchor, GridPoint::new(0, 1)).expect("resolved");
        assert_eq!(backward.begin, GridPoint::new(0, 0));
        assert_eq!(backward.end, GridPoint::new(0, 6));
    }

    #[test]
    fn a_drag_over_whitespace_narrows_to_the_other_ends_word() {
        let parser = screen(30, 2, b"one two");
        let screen = parser.screen();
        let anchor = GridPoint::new(0, 1); // inside `one`
        // Column 3 is the space; column 20 is never-written.
        for extent in [GridPoint::new(0, 3), GridPoint::new(0, 20)] {
            assert_eq!(
                resolve(screen, anchor, extent),
                Some(GridRange {
                    begin: GridPoint::new(0, 0),
                    end: GridPoint::new(0, 2),
                }),
                "{extent:?}"
            );
        }
        // Both ends on separators is no selection at all.
        assert_eq!(
            resolve(screen, GridPoint::new(0, 3), GridPoint::new(0, 20)),
            None
        );
    }

    #[test]
    fn a_multi_row_range_paints_line_wise_segments_not_a_rectangle() {
        let range = GridRange {
            begin: GridPoint::new(1, 6),
            end: GridPoint::new(3, 2),
        };
        assert_eq!(
            segments(range, 10),
            vec![
                Segment {
                    row: 1,
                    col: 6,
                    span: 4
                }, // begin col -> end of line
                Segment {
                    row: 2,
                    col: 0,
                    span: 10
                }, // a full middle row
                Segment {
                    row: 3,
                    col: 0,
                    span: 3
                }, // start of line -> end col
            ]
        );
    }

    #[test]
    fn a_single_row_range_is_one_segment_and_columns_clamp_to_the_grid() {
        assert_eq!(
            segments(
                GridRange {
                    begin: GridPoint::new(0, 2),
                    end: GridPoint::new(0, 5)
                },
                10
            ),
            vec![Segment {
                row: 0,
                col: 2,
                span: 4
            }]
        );
        // A range resolved before a shrink still paints inside the new grid.
        assert_eq!(
            segments(
                GridRange {
                    begin: GridPoint::new(0, 0),
                    end: GridPoint::new(0, 99)
                },
                8
            ),
            vec![Segment {
                row: 0,
                col: 0,
                span: 8
            }]
        );
        assert!(segments(GridRange::cell(GridPoint::new(0, 0)), 0).is_empty());
    }

    #[test]
    fn clamp_to_pulls_both_ends_inside_a_shrunk_grid() {
        let range = GridRange {
            begin: GridPoint::new(0, 60),
            end: GridPoint::new(2, 5),
        };
        assert_eq!(
            range.clamp_to(40, 10),
            GridRange {
                begin: GridPoint::new(0, 39),
                end: GridPoint::new(2, 5),
            },
            "begin clamps exactly like end already did"
        );
        // A grid that grew, or an already-in-bounds range, is untouched.
        assert_eq!(range.clamp_to(80, 10), range);
    }

    #[test]
    fn clamp_to_never_reorders_a_range_it_leaves_reversed() {
        // A row-only shrink can collapse both ends onto the last surviving row
        // in an order that now reads reversed by column — `clamp_to` leaves it
        // that way, and `selected_text`/`segments` both read a reversed range
        // as nothing selected, so it degrades rather than panicking.
        let range = GridRange {
            begin: GridPoint::new(1, 50),
            end: GridPoint::new(3, 2),
        };
        assert_eq!(
            range.clamp_to(60, 2),
            GridRange {
                begin: GridPoint::new(1, 50),
                end: GridPoint::new(1, 2),
            }
        );
    }

    #[test]
    fn pixels_map_to_cells_and_clamp_into_the_grid_rather_than_escaping_it() {
        let geo = geometry(8, 4);
        assert_eq!(cell_at(Point::new(0.0, 0.0), geo), GridPoint::new(0, 0));
        assert_eq!(cell_at(Point::new(25.0, 45.0), geo), GridPoint::new(2, 2));
        // Off the right/bottom edge — a drag routinely leaves the box.
        assert_eq!(
            cell_at(Point::new(9999.0, 9999.0), geo),
            GridPoint::new(3, 7)
        );
        // Off the top/left, and a degenerate coordinate.
        assert_eq!(cell_at(Point::new(-40.0, -40.0), geo), GridPoint::new(0, 0));
        assert_eq!(
            cell_at(Point::new(f64::NAN, 10.0), geo),
            GridPoint::new(0, 0)
        );
    }

    #[test]
    fn copied_text_joins_a_wrapped_line_without_a_newline() {
        // 10 columns, a 14-character path: the emulator wraps it onto row 1 and
        // marks row 0 wrapped, so the two rows are ONE logical line.
        let parser = screen(10, 4, b"/usr/local/bin");
        let screen = parser.screen();
        assert!(screen.row_wrapped(0), "sanity: row 0 really did wrap");
        let text = selected_text(
            screen,
            GridRange {
                begin: GridPoint::new(0, 0),
                end: GridPoint::new(1, 3),
            },
        );
        assert_eq!(text, "/usr/local/bin", "a wrapped path pastes back as one");
    }

    #[test]
    fn copied_text_joins_distinct_lines_with_a_newline_and_trims_trailing_blanks() {
        let parser = screen(20, 4, b"first\r\nsecond");
        let screen = parser.screen();
        let text = selected_text(
            screen,
            GridRange {
                begin: GridPoint::new(0, 0),
                end: GridPoint::new(1, 5),
            },
        );
        assert_eq!(
            text, "first\nsecond",
            "two real lines join with a newline, and neither carries its \
             trailing blanks"
        );
    }

    #[test]
    fn copied_text_covers_the_inclusive_end_cell() {
        let parser = screen(20, 2, b"abcdef");
        let screen = parser.screen();
        // cols 1..=3 inclusive is "bcd" — the off-by-one this conversion exists
        // to get right.
        assert_eq!(
            selected_text(
                screen,
                GridRange {
                    begin: GridPoint::new(0, 1),
                    end: GridPoint::new(0, 3)
                }
            ),
            "bcd"
        );
        // A single cell is one character, never empty.
        assert_eq!(
            selected_text(screen, GridRange::cell(GridPoint::new(0, 0))),
            "a"
        );
    }

    #[test]
    fn an_out_of_range_selection_yields_empty_text_rather_than_panicking() {
        let parser = screen(10, 3, b"hi");
        let screen = parser.screen();
        assert_eq!(
            selected_text(
                screen,
                GridRange {
                    begin: GridPoint::new(9, 0),
                    end: GridPoint::new(9, 5)
                }
            ),
            ""
        );
        // A reversed range is empty, not a wrap-around.
        assert_eq!(
            selected_text(
                screen,
                GridRange {
                    begin: GridPoint::new(0, 5),
                    end: GridPoint::new(0, 1)
                }
            ),
            ""
        );
    }

    #[test]
    fn selected_text_clamps_a_stale_begin_after_a_shrink_instead_of_panicking() {
        // 70 'x's — not a full 80-col row, so there is no wrap flag to reason
        // about — a real newline, then a short second row: a multi-row selection
        // whose BEGIN column (60) is about to outlive an 80 -> 40 shrink.
        let bytes = [vec![b'x'; 70], b"\r\nyz".to_vec()].concat();
        let mut parser = vt100::Parser::new(4, 80, 0);
        parser.process(&bytes);
        // The grid shrinks under the held selection — a rotation, a font-size
        // bump, a fullscreen collapse.
        parser.screen_mut().set_size(4, 40);

        let range = GridRange {
            begin: GridPoint::new(0, 60), // inside the `x` run, now off-screen
            end: GridPoint::new(1, 1),    // the `z` in "yz"
        };
        // Before this fix, `contents_between`'s multi-row branch computed
        // `cols - start_col` (`40 - 60`) in `u16` and underflowed — a real panic
        // under `overflow-checks`, reachable whenever a multi-row selection
        // outlived a grid shrink.
        let text = selected_text(parser.screen(), range);
        assert_eq!(
            text, "x\nyz",
            "begin clamps to the new width exactly like end already did"
        );
    }

    #[test]
    fn a_wide_characters_second_half_reads_back_as_word_material() {
        // Two double-width glyphs: four columns, one word.
        let parser = screen(20, 2, "日本".as_bytes());
        let screen = parser.screen();
        assert_eq!(char_at(screen, GridPoint::new(0, 1)), '日');
        assert_eq!(
            word_at(screen, GridPoint::new(0, 1)),
            Some(GridRange {
                begin: GridPoint::new(0, 0),
                end: GridPoint::new(0, 3),
            }),
            "a CJK word does not split at every other column"
        );
    }
}
