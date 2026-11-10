//! The cell model of the table writers (Hledger.Write.Spreadsheet): a
//! report as rows of cells, each with its content and what the csv, tsv,
//! html and fods writers need to know about it: the kind of value, header
//! or body style, borders, merged cell span, link, and class.

use crate::amount::render::{
    AmountFormat, show_mixed_amount_b, show_mixed_amount_lines_parts_b,
    show_mixed_amount_one_line_parts_b,
};
use crate::amount::{Amount, MixedAmount, Sym};
use crate::text::Wide;

/// The kind of value a cell holds (Type). An amount cell's amount has no
/// commodity when the amount is shown without its symbol.
#[derive(Clone, Debug, PartialEq)]
pub enum CellType {
    String,
    Integer,
    Amount(Amount),
    MixedAmount,
    Date,
}

/// A body cell's emphasis: an ordinary item, or a total (Emphasis).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Emphasis {
    Item,
    Total,
}

/// A cell's style: in the table body, or a heading (Style).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Style {
    Body(Emphasis),
    Head,
}

/// A border line (NumLines).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Lines {
    #[default]
    NoLine,
    SingleLine,
    DoubleLine,
}

/// The lines on a cell's four sides (Border).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Border {
    pub left: Lines,
    pub right: Lines,
    pub top: Lines,
    pub bottom: Lines,
}

impl Border {
    /// With left and right swapped for top and bottom (transposeBorder).
    fn transposed(self) -> Border {
        Border {
            left: self.top,
            right: self.bottom,
            top: self.left,
            bottom: self.right,
        }
    }
}

/// How a cell takes part in a merged cell (Span): not at all; covered by
/// one (html omits it, fods keeps it, csv shows its content); or the first
/// cell of one merging this many cells across, or down.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Span {
    #[default]
    NoSpan,
    Covered,
    Horizontal(usize),
    Vertical(usize),
}

impl Span {
    /// Across for down (transposeSpan).
    fn transposed(self) -> Span {
        match self {
            Span::Horizontal(n) => Span::Vertical(n),
            Span::Vertical(n) => Span::Horizontal(n),
            s => s,
        }
    }
}

/// A table cell (Cell): its content, and its type, borders, style, span,
/// link (anchor) with the link's description (title), and class, which
/// is several words for html. `parts` are the separately styled amounts of
/// a multi-commodity amount, for writers (html) that style each; empty for
/// other cells; `content` is always complete.
#[derive(Clone, Debug, PartialEq)]
pub struct Cell<T> {
    pub typ: CellType,
    pub border: Border,
    pub style: Style,
    pub span: Span,
    pub anchor: String,
    pub title: String,
    pub class: String,
    pub parts: Vec<T>,
    pub content: T,
}

impl<T> Cell<T> {
    /// A text cell in the body, without borders, span, link or class
    /// (defaultCell).
    pub fn new(content: T) -> Cell<T> {
        Cell {
            typ: CellType::String,
            border: Border::default(),
            style: Style::Body(Emphasis::Item),
            span: Span::NoSpan,
            anchor: String::new(),
            title: String::new(),
            class: String::new(),
            parts: Vec::new(),
            content,
        }
    }

    /// The same cell with its content and parts transformed (fmap).
    pub fn map<U>(self, mut f: impl FnMut(T) -> U) -> Cell<U> {
        Cell {
            typ: self.typ,
            border: self.border,
            style: self.style,
            span: self.span,
            anchor: self.anchor,
            title: self.title,
            class: self.class,
            parts: self.parts.into_iter().map(&mut f).collect(),
            content: f(self.content),
        }
    }

    /// With this class.
    pub fn with_class(self, class: &str) -> Cell<T> {
        Cell {
            class: class.to_string(),
            ..self
        }
    }

    /// The cell of a transposed table (transposeCell).
    fn transposed(self) -> Cell<T> {
        Cell {
            border: self.border.transposed(),
            span: self.span.transposed(),
            ..self
        }
    }
}

impl<T: Default> Cell<T> {
    /// An empty cell (emptyCell).
    pub fn empty() -> Cell<T> {
        Cell::new(T::default())
    }
}

impl Cell<String> {
    /// A heading cell (headerCell).
    pub fn header(text: &str) -> Cell<String> {
        Cell {
            style: Style::Head,
            ..Cell::new(text.to_string())
        }
    }
}

impl Cell<Wide> {
    /// The cell with its text, no longer knowing its width (wbToText).
    pub fn into_text(self) -> Cell<String> {
        self.map(|w| w.s)
    }
}

/// Rows of text cells.
pub type Rows = Vec<Vec<Cell<String>>>;

/// A sheet: how many of its first rows and columns are headings (which a
/// spreadsheet keeps in view), and its rows.
pub type Sheet = ((usize, usize), Rows);

/// The table's rows as columns, the cells' borders and spans turned too
/// (transpose). A short row leaves the later columns shorter.
pub fn transpose<T>(rows: Vec<Vec<Cell<T>>>) -> Vec<Vec<Cell<T>>> {
    let n = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut columns: Vec<Vec<Cell<T>>> = (0..n).map(|_| Vec::new()).collect();
    for row in rows {
        for (i, c) in row.into_iter().enumerate() {
            columns[i].push(c.transposed());
        }
    }
    columns
}

/// These heading cells with a double line below (addHeaderBorders).
pub fn add_header_borders<T>(cells: Vec<Cell<T>>) -> Vec<Cell<T>> {
    cells
        .into_iter()
        .map(|c| Cell {
            border: Border {
                bottom: Lines::DoubleLine,
                ..Border::default()
            },
            ..c
        })
        .collect()
}

/// The cell merged across as many columns as there are sub-cells: it
/// first, then covered empty cells (horizontalSpan).
pub fn horizontal_span<T: Default>(columns: usize, cell: Cell<T>) -> Vec<Cell<T>> {
    if columns == 0 {
        return Vec::new();
    }
    let mut cells = vec![Cell {
        span: Span::Horizontal(columns),
        ..cell
    }];
    cells.extend((1..columns).map(|_| Cell {
        span: Span::Covered,
        ..Cell::empty()
    }));
    cells
}

/// The rows, each with the header cells first, merged down over all of
/// them when there are several (addRowSpanHeader; Print.hs's own version
/// takes several header cells).
pub fn add_row_span_header<T: Clone>(
    headers: &[Cell<T>],
    rows: Vec<Vec<Cell<T>>>,
) -> Vec<Vec<Cell<T>>> {
    let n = rows.len();
    rows.into_iter()
        .enumerate()
        .map(|(i, row)| {
            let mut r: Vec<Cell<T>> = (headers.iter())
                .map(|h| Cell {
                    span: match (n, i) {
                        (1, _) => h.span,
                        (_, 0) => Span::Vertical(n),
                        _ => Span::Covered,
                    },
                    ..h.clone()
                })
                .collect();
            r.extend(row);
            r
        })
        .collect()
}

/// The cells' contents (rawTableContent).
pub fn raw_table_content<T>(rows: Vec<Vec<Cell<T>>>) -> Vec<Vec<T>> {
    rows.into_iter()
        .map(|r| r.into_iter().map(|c| c.content).collect())
        .collect()
}

/// The class with "negative" added if the amount is negative, so that
/// writers with a stylesheet (html) can colour it; not if it rounds to
/// zero when shown (markNegative).
fn mark_negative(negative: bool, class: &str) -> String {
    match (negative, class.is_empty()) {
        (false, _) => class.to_string(),
        (true, true) => "negative".to_string(),
        (true, false) => format!("{class} negative"),
    }
}

/// A cell showing a mixed amount on a line, with this class
/// (cellFromMixedAmount): an amount cell if it adds up to one amount;
/// its amounts as parts when they are shown on one line, unelided.
pub fn cell_from_mixed_amount(fmt: &AmountFormat, class: &str, m: &MixedAmount) -> Cell<Wide> {
    Cell {
        class: mark_negative(m.is_negative() == Some(true) && !m.looks_zero(), class),
        typ: match m.unify() {
            Some(a) => amount_type(fmt, a),
            None => CellType::MixedAmount,
        },
        parts: if fmt.one_line && fmt.max_width.is_none() {
            (show_mixed_amount_one_line_parts_b(fmt, m).into_iter())
                .map(|(w, _)| w)
                .collect()
        } else {
            Vec::new()
        },
        ..Cell::new(show_mixed_amount_b(fmt, m))
    }
}

/// A cell for each line of a mixed amount shown on several lines, with
/// this class (cellsFromMixedAmount).
pub fn cells_from_mixed_amount(
    fmt: &AmountFormat,
    class: &str,
    m: &MixedAmount,
) -> Vec<Cell<Wide>> {
    (show_mixed_amount_lines_parts_b(fmt, m).into_iter())
        .map(|(text, a)| cell_from_amount(fmt, class, text, a))
        .collect()
}

/// A cell showing an amount as this text, with this class (cellFromAmount).
pub fn cell_from_amount<T>(fmt: &AmountFormat, class: &str, text: T, a: Amount) -> Cell<T> {
    Cell {
        class: mark_negative(a.is_negative() && !a.looks_zero(), class),
        typ: amount_type(fmt, a),
        ..Cell::new(text)
    }
}

/// An amount cell's type: the amount, without its commodity if that is
/// not shown (amountType).
fn amount_type(fmt: &AmountFormat, a: Amount) -> CellType {
    if fmt.commodity {
        CellType::Amount(a)
    } else {
        CellType::Amount(Amount {
            commodity: Sym::from(""),
            ..a
        })
    }
}

/// A cell showing a whole number (integerCell).
pub fn integer_cell(n: impl ToString) -> Cell<String> {
    Cell {
        typ: CellType::Integer,
        ..Cell::new(n.to_string())
    }
}

/// Totals rows: in the totals style, a double line above the first
/// (addTotalBorders).
pub fn add_total_borders<T>(rows: Vec<Vec<Cell<T>>>) -> Vec<Vec<Cell<T>>> {
    (rows.into_iter().enumerate())
        .map(|(i, row)| {
            let top = if i == 0 {
                Lines::DoubleLine
            } else {
                Lines::NoLine
            };
            (row.into_iter())
                .map(|c| Cell {
                    style: Style::Body(Emphasis::Total),
                    border: Border {
                        top,
                        ..Border::default()
                    },
                    ..c
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_span_header() {
        let rows = |n: usize| (0..n).map(|i| vec![Cell::new(i.to_string())]).collect();
        let spans = |rows: Vec<Vec<Cell<String>>>| -> Vec<Span> {
            rows.iter().map(|r| r[0].span).collect()
        };
        let h = [Cell::new("h".to_string())];
        assert!(add_row_span_header(&h, rows(0)).is_empty());
        assert_eq!(spans(add_row_span_header(&h, rows(1))), [Span::NoSpan]);
        assert_eq!(
            spans(add_row_span_header(&h, rows(3))),
            [Span::Vertical(3), Span::Covered, Span::Covered]
        );
    }

    #[test]
    fn transposing() {
        let mut c = Cell::new("a".to_string());
        c.span = Span::Horizontal(2);
        c.border.bottom = Lines::DoubleLine;
        let t = transpose(vec![
            vec![c, Cell::empty()],
            vec![Cell::new("b".to_string())],
        ]);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0][0].span, Span::Vertical(2));
        assert_eq!(t[0][0].border.right, Lines::DoubleLine);
        assert_eq!(raw_table_content(t), [vec!["a", "b"], vec![""]]);
    }
}
