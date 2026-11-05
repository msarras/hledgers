//! Text tables (the tabular package's Text.Tabular, and
//! Text.Tabular.AsciiWide): rows and columns of cells, with headers whose
//! groups have lines between them, rendered as text with wide characters
//! counted as two columns, plain or with box drawing characters.

use std::borrow::Borrow;

use crate::text::Wide;

/// The line between two groups of rows or columns (Properties).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Line {
    NoLine,
    SingleLine,
    DoubleLine,
}

/// Row or column headers: one header, or a group of them with a line
/// between each two (Header).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Header<H> {
    Leaf(H),
    Group(Line, Vec<Header<H>>),
}

/// A header's leaves and lines, left to right (flattenHeader's items).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part<H> {
    Line(Line),
    Leaf(H),
}

impl<H> Header<H> {
    /// The headers, left to right (headerContents).
    pub fn contents(&self) -> Vec<&H> {
        let mut out = Vec::new();
        self.flatten_into(&mut out);
        out.into_iter()
            .filter_map(|p| match p {
                Part::Leaf(h) => Some(h),
                Part::Line(_) => None,
            })
            .collect()
    }

    /// The headers and the lines between them, left to right
    /// (flattenHeader).
    pub fn flatten(&self) -> Vec<Part<&H>> {
        let mut out = Vec::new();
        self.flatten_into(&mut out);
        out
    }

    fn flatten_into<'a>(&'a self, out: &mut Vec<Part<&'a H>>) {
        match self {
            Header::Leaf(h) => out.push(Part::Leaf(h)),
            Header::Group(line, hs) => {
                for (i, h) in hs.iter().enumerate() {
                    if i > 0 {
                        out.push(Part::Line(*line));
                    }
                    h.flatten_into(out);
                }
            }
        }
    }

    /// The same structure with these headers instead, left to right, the
    /// missing ones `default` (zipHeader, keeping just the new headers).
    pub fn replaced<T: Clone>(
        &self,
        items: &mut impl Iterator<Item = T>,
        default: &T,
    ) -> Header<T> {
        match self {
            Header::Leaf(_) => Header::Leaf(items.next().unwrap_or_else(|| default.clone())),
            Header::Group(line, hs) => Header::Group(
                *line,
                hs.iter().map(|h| h.replaced(items, default)).collect(),
            ),
        }
    }
}

/// A table: row headers, column headers, and rows of cells (Table).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table<RH, CH, A> {
    pub row_headers: Header<RH>,
    pub col_headers: Header<CH>,
    pub rows: Vec<Vec<A>>,
}

/// The second table added below the first, after a line of this kind; its
/// column headers are dropped (concatTables).
pub fn concat_tables<RH, CH, CH2, A>(
    line: Line,
    t1: Table<RH, CH, A>,
    t2: Table<RH, CH2, A>,
) -> Table<RH, CH, A> {
    let mut rows = t1.rows;
    rows.extend(t2.rows);
    Table {
        row_headers: Header::Group(line, vec![t1.row_headers, t2.row_headers]),
        col_headers: t1.col_headers,
        rows,
    }
}

/// How to render a table: with box drawing characters, with outer
/// borders, with spaces around the bars (TableOpts; the default has
/// borders and spaces).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TableOpts {
    pub pretty: bool,
    pub borders: bool,
    pub border_spaces: bool,
}

impl Default for TableOpts {
    fn default() -> TableOpts {
        TableOpts {
            pretty: false,
            borders: true,
            border_spaces: true,
        }
    }
}

/// How a cell's lines are placed in its column and among the row's lines
/// (Align).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    TopRight,
    BottomRight,
    BottomLeft,
    TopLeft,
}

/// A cell: lines of text, aligned (Cell).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub align: Align,
    pub lines: Vec<Wide>,
}

impl Cell {
    /// A cell of no lines (emptyCell).
    pub fn empty() -> Cell {
        Cell {
            align: Align::TopRight,
            lines: Vec::new(),
        }
    }

    /// A cell of the text's lines, at least one (textCell).
    pub fn text(align: Align, s: &str) -> Cell {
        let lines = if s.is_empty() {
            vec![Wide::new("")]
        } else {
            s.split_terminator('\n').map(Wide::new).collect()
        };
        Cell { align, lines }
    }

    /// A cell of these lines (textsCell).
    pub fn texts<S: AsRef<str>>(align: Align, lines: &[S]) -> Cell {
        Cell {
            align,
            lines: lines.iter().map(|l| Wide::new(l.as_ref())).collect(),
        }
    }

    /// The width of its widest line (cellWidth).
    pub fn width(&self) -> usize {
        self.lines.iter().map(|l| l.width).max().unwrap_or(0)
    }
}

/// A table rendered with each row header, column header and cell made a
/// cell by the given functions (renderTable, renderTableB).
pub fn render_table<RH, CH, A>(
    opts: &TableOpts,
    row_header: impl Fn(&RH) -> Cell,
    col_header: impl Fn(&CH) -> Cell,
    cell: impl Fn(&A) -> Cell,
    t: &Table<RH, CH, A>,
) -> String {
    render_table_by_rows(
        opts,
        |chs| chs.iter().map(|c| col_header(c)).collect(),
        |rh, row| (row_header(rh), row.iter().map(&cell).collect()),
        t,
    )
}

/// A table rendered with the column headers made cells together, and each
/// row with its header, by the given functions: the column header row, a
/// double line, then the rows with the lines between their groups, the
/// row headers separated from the rest by a double line; each column as
/// wide as its widest cell (renderTableByRowsB).
pub fn render_table_by_rows<RH, CH, A>(
    opts: &TableOpts,
    col_headers: impl Fn(&[&CH]) -> Vec<Cell>,
    row: impl Fn(&RH, &[A]) -> (Cell, Vec<Cell>),
    t: &Table<RH, CH, A>,
) -> String {
    let rows: Vec<(Cell, Vec<Cell>)> = (t.row_headers.contents().into_iter())
        .zip(&t.rows)
        .map(|(rh, cells)| row(rh, cells))
        .collect();
    let empty = Cell::empty();
    let row_headers: Header<&Cell> = t
        .row_headers
        .replaced(&mut rows.iter().map(|(h, _)| h), &&empty);
    let col_cells = col_headers(&t.col_headers.contents());
    let col_headers: Header<&Cell> = t.col_headers.replaced(&mut col_cells.iter(), &&empty);
    // the column headers, after the row headers' column
    let ch2 = Header::Group(
        Line::DoubleLine,
        vec![Header::Leaf(&empty), col_headers.clone()],
    );
    let mut sizes: Vec<usize> = Vec::new();
    let mut widen = |cells: &mut dyn Iterator<Item = &Cell>| {
        for (i, c) in cells.enumerate() {
            match sizes.get_mut(i) {
                Some(s) => *s = (*s).max(c.width()),
                None => sizes.push(c.width()),
            }
        }
    };
    widen(&mut ch2.contents().into_iter().copied());
    for (rh, (_, cells)) in row_headers.contents().into_iter().zip(&rows) {
        widen(&mut std::iter::once(*rh).chain(cells));
    }
    // row headers beyond the rows have no cells
    for rh in row_headers.contents().into_iter().skip(rows.len()) {
        widen(&mut std::iter::once(*rh));
    }
    let hline =
        |vpos: VPos, line: Line| render_hline(vpos, opts.borders, opts.pretty, &sizes, &ch2, line);
    let mut lines: Vec<String> = Vec::new();
    if opts.borders {
        lines.extend(hline(VPos::Top, Line::SingleLine));
    }
    lines.push(render_columns(opts, &sizes, &ch2));
    lines.extend(hline(VPos::Middle, Line::DoubleLine));
    let mut row_cells = rows.iter().map(|(_, cs)| cs.as_slice());
    for part in row_headers.flatten() {
        match part {
            Part::Line(line) => lines.extend(hline(VPos::Middle, line)),
            Part::Leaf(h) => {
                let cells = row_cells.next().unwrap_or(&[]);
                let shown = col_headers.replaced(&mut cells.iter(), &&empty);
                let r = Header::Group(Line::DoubleLine, vec![Header::Leaf(*h), shown]);
                lines.push(render_columns(opts, &sizes, &r));
            }
        }
    }
    if opts.borders {
        lines.extend(hline(VPos::Bottom, Line::SingleLine));
    }
    let mut out = String::new();
    for l in lines {
        out.push_str(&l);
        out.push('\n');
    }
    out
}

/// One row of cells rendered, each column as wide as its cell
/// (renderRowB).
pub fn render_row<C: Borrow<Cell>>(opts: &TableOpts, h: &Header<C>) -> String {
    let widths: Vec<usize> = (h.contents().iter())
        .map(|c| Borrow::<Cell>::borrow(*c).width())
        .collect();
    render_columns(opts, &widths, h)
}

/// A row of cells rendered in columns of these widths (a missing one being
/// 0), with the lines between their groups: as many lines of text as its
/// tallest cell has, each cell's lines placed as it is aligned, without a
/// final newline (renderColumns).
pub fn render_columns<C: Borrow<Cell>>(
    opts: &TableOpts,
    widths: &[usize],
    h: &Header<C>,
) -> String {
    let parts = h.flatten();
    let n_lines = (h.contents().iter())
        .map(|c| Borrow::<Cell>::borrow(*c).lines.len())
        .max()
        .unwrap_or(0);
    let mut out: Vec<String> = Vec::with_capacity(n_lines);
    for line in 0..n_lines {
        let mut s = String::new();
        let mut column = 0;
        for part in &parts {
            match part {
                Part::Line(Line::NoLine) => {
                    if opts.border_spaces {
                        s.push_str("  ");
                    }
                }
                Part::Line(Line::SingleLine) => {
                    s.push_str(mid_bar(opts.pretty, opts.border_spaces))
                }
                Part::Line(Line::DoubleLine) => {
                    s.push_str(double_mid_bar(opts.pretty, opts.border_spaces))
                }
                Part::Leaf(c) => {
                    let c: &Cell = Borrow::<Cell>::borrow(*c);
                    let w = widths.get(column).copied().unwrap_or(0);
                    column += 1;
                    let blank = n_lines - c.lines.len();
                    let text = match c.align {
                        Align::TopLeft | Align::TopRight => c.lines.get(line),
                        Align::BottomLeft | Align::BottomRight => {
                            line.checked_sub(blank).and_then(|i| c.lines.get(i))
                        }
                    };
                    match text {
                        None => s.push_str(&" ".repeat(w)),
                        Some(t) => {
                            let pad = " ".repeat(w.saturating_sub(t.width));
                            match c.align {
                                Align::TopLeft | Align::BottomLeft => {
                                    s.push_str(&t.s);
                                    s.push_str(&pad);
                                }
                                Align::TopRight | Align::BottomRight => {
                                    s.push_str(&pad);
                                    s.push_str(&t.s);
                                }
                            }
                        }
                    }
                }
            }
        }
        out.push(if opts.borders {
            format!(
                "{}{s}{}",
                left_bar(opts.pretty, opts.border_spaces),
                right_bar(opts.pretty, opts.border_spaces)
            )
        } else if opts.border_spaces {
            format!(" {s} ")
        } else {
            s
        });
    }
    out.join("\n")
}

fn vertical_bar(pretty: bool) -> &'static str {
    if pretty { "│" } else { "|" }
}

fn left_bar(pretty: bool, spaces: bool) -> String {
    let bar = vertical_bar(pretty);
    if spaces {
        format!("{bar} ")
    } else {
        bar.to_string()
    }
}

fn right_bar(pretty: bool, spaces: bool) -> String {
    let bar = vertical_bar(pretty);
    if spaces {
        format!(" {bar}")
    } else {
        bar.to_string()
    }
}

fn mid_bar(pretty: bool, spaces: bool) -> &'static str {
    match (pretty, spaces) {
        (true, true) => " │ ",
        (true, false) => "│",
        (false, true) => " | ",
        (false, false) => "|",
    }
}

fn double_mid_bar(pretty: bool, spaces: bool) -> &'static str {
    match (pretty, spaces) {
        (true, true) => " ║ ",
        (true, false) => "║",
        (false, true) => " || ",
        (false, false) => "||",
    }
}

/// Where a horizontal line is: the top border, between rows, or the bottom
/// border (VPos).
#[derive(Clone, Copy, PartialEq, Eq)]
enum VPos {
    Top,
    Middle,
    Bottom,
}

/// Where a box drawing character is in a horizontal line (HPos).
#[derive(Clone, Copy, PartialEq, Eq)]
enum HPos {
    Left,
    Middle,
    Right,
}

/// A horizontal line of this kind under columns of these widths, crossing
/// the vertical lines between the header's groups; none for no line
/// (renderHLine).
fn render_hline<H>(
    vpos: VPos,
    borders: bool,
    pretty: bool,
    widths: &[usize],
    h: &Header<H>,
    line: Line,
) -> Option<String> {
    if line == Line::NoLine {
        return None;
    }
    let sep = boxchar(vpos, HPos::Middle, Line::NoLine, line, pretty);
    let mut core = String::new();
    let mut column = 0;
    for part in h.flatten() {
        match part {
            Part::Leaf(_) => {
                let w = widths.get(column).copied().unwrap_or(0);
                column += 1;
                core.push_str(&sep.repeat(w));
            }
            Part::Line(Line::NoLine) => {
                core.push_str(sep);
                core.push_str(sep);
            }
            Part::Line(v) => {
                core.push_str(sep);
                core.push_str(boxchar(vpos, HPos::Middle, v, line, pretty));
                core.push_str(sep);
            }
        }
    }
    let middle = format!("{sep}{core}{sep}");
    Some(if borders {
        let edge = |hpos| boxchar(vpos, hpos, Line::SingleLine, line, pretty);
        format!("{}{middle}{}", edge(HPos::Left), edge(HPos::Right))
    } else {
        middle
    })
}

/// The box drawing character joining a vertical line of one kind and a
/// horizontal line of another at this position (boxchar).
fn boxchar(vpos: VPos, hpos: HPos, vert: Line, horiz: Line, pretty: bool) -> &'static str {
    let up = if vpos == VPos::Top {
        Line::NoLine
    } else {
        vert
    };
    let down = if vpos == VPos::Bottom {
        Line::NoLine
    } else {
        vert
    };
    let left = if hpos == HPos::Left {
        Line::NoLine
    } else {
        horiz
    };
    let right = if hpos == HPos::Right {
        Line::NoLine
    } else {
        horiz
    };
    lineart(up, down, left, right, pretty)
}

/// The box drawing character with these lines up, down, left and right,
/// or its ASCII stand-in (lineart).
fn lineart(up: Line, down: Line, left: Line, right: Line, pretty: bool) -> &'static str {
    use Line::{DoubleLine as D, NoLine as N, SingleLine as S};
    let (fancy, plain) = match (up, down, left, right) {
        (S, S, S, S) => ("┼", "+"),
        (S, S, S, N) => ("┤", "+"),
        (S, S, N, S) => ("├", "+"),
        (S, N, S, S) => ("┴", "+"),
        (N, S, S, S) => ("┬", "+"),
        (S, N, N, S) => ("└", "+"),
        (S, N, S, N) => ("┘", "+"),
        (N, S, S, N) => ("┐", "+"),
        (N, S, N, S) => ("┌", "+"),
        (S, S, N, N) => ("│", "|"),
        (N, N, S, S) => ("─", "-"),
        (D, D, D, D) => ("╬", "++"),
        (D, D, D, N) => ("╣", "++"),
        (D, D, N, D) => ("╠", "++"),
        (D, N, D, D) => ("╩", "++"),
        (N, D, D, D) => ("╦", "++"),
        (D, N, N, D) => ("╚", "++"),
        (D, N, D, N) => ("╝", "++"),
        (N, D, D, N) => ("╗", "++"),
        (N, D, N, D) => ("╔", "++"),
        (D, D, N, N) => ("║", "||"),
        (N, N, D, D) => ("═", "="),
        (D, N, N, S) => ("╙", "++"),
        (D, N, S, N) => ("╜", "++"),
        (N, D, S, N) => ("╖", "++"),
        (N, D, N, S) => ("╓", "++"),
        (S, N, N, D) => ("╘", "+"),
        (S, N, D, N) => ("╛", "+"),
        (N, S, D, N) => ("╕", "+"),
        (N, S, N, D) => ("╒", "+"),
        (D, D, S, N) => ("╢", "++"),
        (D, D, N, S) => ("╟", "++"),
        (D, N, S, S) => ("╨", "++"),
        (N, D, S, S) => ("╥", "++"),
        (S, S, D, N) => ("╡", "+"),
        (S, S, N, D) => ("╞", "+"),
        (S, N, D, D) => ("╧", "+"),
        (N, S, D, D) => ("╤", "+"),
        (S, S, D, D) => ("╪", "+"),
        (D, D, S, S) => ("╫", "++"),
        _ => ("", ""),
    };
    if pretty { fancy } else { plain }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(pretty: bool) -> String {
        let t = Table {
            row_headers: Header::Group(
                Line::SingleLine,
                vec![
                    Header::Group(
                        Line::NoLine,
                        vec![
                            Header::Leaf("a".to_string()),
                            Header::Leaf("費用".to_string()),
                        ],
                    ),
                    Header::Leaf(String::new()),
                ],
            ),
            col_headers: Header::Group(
                Line::SingleLine,
                vec![
                    Header::Leaf("Jan".to_string()),
                    Header::Leaf("Total".to_string()),
                ],
            ),
            rows: vec![
                vec!["1 USD\n2 EUR".to_string(), "3".to_string()],
                vec!["-1000 円".to_string(), "4".to_string()],
                vec!["0".to_string(), "0".to_string()],
            ],
        };
        let opts = TableOpts {
            pretty,
            borders: pretty,
            ..TableOpts::default()
        };
        render_table(
            &opts,
            |h: &String| Cell::text(Align::TopLeft, h),
            |h: &String| Cell::text(Align::TopRight, h),
            |c: &String| Cell::text(Align::BottomRight, c),
            &t,
        )
    }

    #[test]
    fn rendering_tables() {
        assert_eq!(
            table(false),
            concat!(
                "      ||      Jan | Total \n",
                "======++==========+=======\n",
                " a    ||    1 USD |       \n",
                "      ||    2 EUR |     3 \n",
                " 費用 || -1000 円 |     4 \n",
                "------++----------+-------\n",
                "      ||        0 |     0 \n",
            )
        );
        assert_eq!(
            table(true),
            concat!(
                "┌──────╥──────────┬───────┐\n",
                "│      ║      Jan │ Total │\n",
                "╞══════╬══════════╪═══════╡\n",
                "│ a    ║    1 USD │       │\n",
                "│      ║    2 EUR │     3 │\n",
                "│ 費用 ║ -1000 円 │     4 │\n",
                "├──────╫──────────┼───────┤\n",
                "│      ║        0 │     0 │\n",
                "└──────╨──────────┴───────┘\n",
            )
        );
    }

    #[test]
    fn rendering_rows() {
        let opts = TableOpts {
            borders: false,
            border_spaces: false,
            ..TableOpts::default()
        };
        let row = Header::Group(
            Line::NoLine,
            vec![
                Header::Leaf(Cell::texts(Align::BottomRight, &["1 A", "-20 B"])),
                Header::Leaf(Cell::text(Align::TopLeft, "  ")),
                Header::Leaf(Cell::text(Align::BottomLeft, "a")),
            ],
        );
        assert_eq!(render_row(&opts, &row), "  1 A   \n-20 B  a");
        let t1 = Table {
            row_headers: Header::Leaf(1),
            col_headers: Header::Leaf('x'),
            rows: vec![vec![10]],
        };
        let t2 = Table {
            row_headers: Header::Leaf(2),
            col_headers: Header::Leaf(()),
            rows: vec![vec![20]],
        };
        let t = concat_tables(Line::SingleLine, t1, t2);
        assert_eq!(t.rows, [[10], [20]]);
        assert_eq!(t.row_headers.contents(), [&1, &2]);
    }
}
