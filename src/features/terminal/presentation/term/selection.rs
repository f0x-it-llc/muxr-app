//! The pure selection model: which cells a long-press drag selects and what text they copy.
//! A [`GridPoint`] addresses the rendered screen with no buffer anchor, which is why the
//! runtime seam drops a held selection on any new content generation.

use frust::authoring::Point;

use super::geometry::CellGeometry;

/// `NUL` is what a never-written cell reads back as, so blank screen space is a separator.
pub const SEPARATORS: [char; 10] = ['\0', ' ', '.', ':', '-', '\\', '"', '*', '+', '/'];

pub fn is_separator(c: char) -> bool {
    SEPARATORS.contains(&c)
}

/// `row` first: the derived `Ord` is row-major reading order, which [`GridRange::merge`] needs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct GridPoint {
    pub row: u16,
    pub col: u16,
}

impl GridPoint {
    pub fn new(row: u16, col: u16) -> Self {
        Self { row, col }
    }
}

/// Inclusive, in row-major reading order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridRange {
    pub begin: GridPoint,
    pub end: GridPoint,
}

impl GridRange {
    pub fn cell(at: GridPoint) -> Self {
        Self { begin: at, end: at }
    }

    #[must_use]
    pub fn merge(self, other: Self) -> Self {
        Self {
            begin: self.begin.min(other.begin),
            end: self.end.max(other.end),
        }
    }

    pub fn contains(self, at: GridPoint) -> bool {
        at >= self.begin && at <= self.end
    }

    /// Ends clamp independently and are never re-sorted: a reversed result reads as nothing
    /// selected rather than panicking.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    pub row: u16,
    pub col: u16,
    /// Never zero.
    pub span: u16,
}

/// Clamped rather than rejected: a drag routinely leaves the widget's box.
pub fn cell_at(point: Point, geo: CellGeometry) -> GridPoint {
    GridPoint {
        row: axis(point.y, geo.height, geo.rows),
        col: axis(point.x, geo.width, geo.cols),
    }
}

fn axis(value: f64, cell: f64, count: u16) -> u16 {
    let last = count.saturating_sub(1);
    // Finiteness first: a NaN coordinate lands on cell 0.
    if !value.is_finite() || !cell.is_finite() || value <= 0.0 || cell <= 0.0 {
        return 0;
    }
    let index = (value / cell).floor();
    if !index.is_finite() {
        return 0;
    }
    index.clamp(0.0, f64::from(last)) as u16
}

/// A wide character's second half reads back its first half, so a CJK word does not split.
pub fn char_at(screen: &vt100::Screen, at: GridPoint) -> char {
    let Some(cell) = screen.cell(at.row, at.col) else {
        return '\0';
    };
    if cell.is_wide_continuation() && at.col > 0 {
        return char_at(screen, GridPoint::new(at.row, at.col - 1));
    }
    cell.contents().chars().next().unwrap_or('\0')
}

/// `None` on a separator; expansion stops at the row's own ends.
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

/// Both ends re-resolve on every call; an end on a separator contributes nothing.
pub fn resolve(screen: &vt100::Screen, anchor: GridPoint, extent: GridPoint) -> Option<GridRange> {
    match (word_at(screen, anchor), word_at(screen, extent)) {
        (Some(a), Some(b)) => Some(a.merge(b)),
        (Some(only), None) | (None, Some(only)) => Some(only),
        (None, None) => None,
    }
}

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

/// `contents_between`'s `end_col` is exclusive where a [`GridRange`] is inclusive. Both ends
/// clamp first: its multi-row branch computes `cols - start_col` in `u16`.
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

    fn screen(cols: u16, rows: u16, bytes: &[u8]) -> vt100::Parser {
        let mut parser = vt100::Parser::new(rows, cols, 0);
        parser.process(bytes);
        parser
    }

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
        assert_eq!(word_at(screen, GridPoint::new(0, 5)), None);
        assert_eq!(word_at(screen, GridPoint::new(0, 30)), None);
        assert_eq!(word_at(screen, GridPoint::new(2, 0)), None);
    }

    #[test]
    fn every_separator_actually_splits_a_word_in_a_real_screen() {
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

        let forward = resolve(screen, anchor, GridPoint::new(0, 15)).expect("resolved");
        assert_eq!(forward.begin, GridPoint::new(0, 4));
        assert_eq!(forward.end, GridPoint::new(0, 17));
        let backward = resolve(screen, anchor, GridPoint::new(0, 1)).expect("resolved");
        assert_eq!(backward.begin, GridPoint::new(0, 0));
        assert_eq!(backward.end, GridPoint::new(0, 6));
    }

    #[test]
    fn a_drag_over_whitespace_narrows_to_the_other_ends_word() {
        let parser = screen(30, 2, b"one two");
        let screen = parser.screen();
        let anchor = GridPoint::new(0, 1); // inside `one`
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
        assert_eq!(range.clamp_to(80, 10), range);
    }

    #[test]
    fn clamp_to_never_reorders_a_range_it_leaves_reversed() {
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
        assert_eq!(
            cell_at(Point::new(9999.0, 9999.0), geo),
            GridPoint::new(3, 7)
        );
        assert_eq!(cell_at(Point::new(-40.0, -40.0), geo), GridPoint::new(0, 0));
        assert_eq!(
            cell_at(Point::new(f64::NAN, 10.0), geo),
            GridPoint::new(0, 0)
        );
    }

    #[test]
    fn copied_text_joins_a_wrapped_line_without_a_newline() {
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
        // 70 columns leaves no wrap flag; begin col 60 outlives the 80 -> 40 shrink.
        let bytes = [vec![b'x'; 70], b"\r\nyz".to_vec()].concat();
        let mut parser = vt100::Parser::new(4, 80, 0);
        parser.process(&bytes);
        parser.screen_mut().set_size(4, 40);

        let range = GridRange {
            begin: GridPoint::new(0, 60), // inside the `x` run, now off-screen
            end: GridPoint::new(1, 1),    // the `z` in "yz"
        };
        let text = selected_text(parser.screen(), range);
        assert_eq!(
            text, "x\nyz",
            "begin clamps to the new width exactly like end already did"
        );
    }

    #[test]
    fn a_wide_characters_second_half_reads_back_as_word_material() {
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
