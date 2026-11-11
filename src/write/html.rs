//! Writing spreadsheet tables as HTML (Hledger.Write.Html and
//! Hledger.Write.Html.Attribute), as blaze-html renders them, with newlines
//! between elements for readability.

use crate::write::spreadsheet::{Cell, CellType, Emphasis, Lines, Span, Style};

/// CSS rules, one per line, after a newline (stylesheet).
pub fn stylesheet(rules: &[(String, String)]) -> String {
    let mut out = String::from("\n");
    for (el, styles) in rules {
        out.push_str(&format!("{el} {{{styles}}}\n"));
    }
    out
}

/// The tables' style rules: collapsed borders, padded cells except
/// account names, no wrapping within dates and amounts, and a rule for
/// each border class (tableStyle, borderStyle).
pub fn table_style() -> Vec<(String, String)> {
    let mut rules: Vec<(String, String)> = [
        ("table", "border-collapse:collapse"),
        ("th, td", "padding-left:1em"),
        ("th.account, td.account", "padding-left:0;"),
        ("td.date, span.amount", "white-space:nowrap"),
    ]
    .iter()
    .map(|(a, b)| (a.to_string(), b.to_string()))
    .collect();
    for side in ["left", "right", "top", "bottom"] {
        for (line, cls) in [("1px solid", "single"), ("3px double", "double")] {
            rules.push((
                format!(".border-{side}-{cls}"),
                format!("border-{side}:{line}"),
            ));
        }
    }
    rules
}

/// Text escaped for html content or an attribute value, as blaze escapes
/// it.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// A table with the built-in styles and a link to an optional user
/// stylesheet, after this report title as a heading unless it is empty
/// (titledTableHtml; styledTableHtml with no title).
pub fn titled_table_html(title: &str, rows: &[Vec<Cell<String>>]) -> String {
    table_html(&table_style(), title, rows)
}

/// A table with these style rules, which the optional user stylesheet
/// linked after them can override, after this title unless it is empty.
pub fn table_html(rules: &[(String, String)], title: &str, rows: &[Vec<Cell<String>>]) -> String {
    let mut out = format!("<style>{}</style>\n", stylesheet(rules));
    out.push_str("<link rel=\"stylesheet\" href=\"hledger.css\">\n");
    if !title.is_empty() {
        out.push_str(&format_title(title));
    }
    out.push_str("<table>\n");
    for row in rows {
        out.push_str(&format_row(row));
    }
    out.push_str("</table>");
    out
}

/// A table row, and a newline (formatRow).
pub fn format_row(row: &[Cell<String>]) -> String {
    let mut out = String::from("<tr>");
    for c in row {
        out.push_str(&format_cell(c));
    }
    out.push_str("</tr>\n");
    out
}

/// A report title as a heading, and a newline (formatTitle).
pub fn format_title(title: &str) -> String {
    format!("<h3 class=\"report-title\">{}</h3>\n", escape(title))
}

/// A cell as a th or td element (formatCell): each amount in a
/// span.amount, the content in a link if the cell has one, a total in
/// bold; its class, "date" for a date, and its borders as classes;
/// numbers right-aligned; a merged cell's span (top-aligned if it spans
/// rows); nothing for a cell covered by a merged one.
pub fn format_cell(cell: &Cell<String>) -> String {
    let amount_span = |s: &str| format!("<span class=\"amount\">{}</span>", escape(s));
    let text = if cell.parts.is_empty() {
        match cell.typ {
            CellType::Amount(_) => amount_span(&cell.content),
            _ => escape(&cell.content),
        }
    } else {
        (cell.parts.iter())
            .map(|p| amount_span(p))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let content = if cell.anchor.is_empty() {
        text
    } else if cell.title.is_empty() {
        format!("<a href=\"{}\">{text}</a>", escape(&cell.anchor))
    } else {
        format!(
            "<a href=\"{}\" title=\"{}\">{text}</a>",
            escape(&cell.anchor),
            escape(&cell.title)
        )
    };
    let mut classes: Vec<String> = Vec::new();
    if !cell.class.is_empty() {
        classes.push(cell.class.clone());
    }
    if cell.typ == CellType::Date {
        classes.push("date".to_string());
    }
    classes.extend(border_classes(cell));
    let mut attrs: Vec<String> = Vec::new();
    let (element, content) = match cell.style {
        Style::Head => ("th", content),
        Style::Body(emphasis) => {
            if !matches!(cell.typ, CellType::String | CellType::Date) {
                attrs.push("align=\"right\"".to_string());
            }
            if matches!(cell.span, Span::Vertical(n) if n > 1) {
                attrs.push("valign=\"top\"".to_string());
            }
            match emphasis {
                Emphasis::Item => ("td", content),
                Emphasis::Total => ("td", format!("<b>{content}</b>")),
            }
        }
    };
    if !classes.is_empty() {
        attrs.push(format!("class=\"{}\"", escape(&classes.join(" "))));
    }
    match cell.span {
        Span::Covered => return String::new(),
        Span::Horizontal(n) if n != 1 => attrs.insert(0, format!("colspan=\"{n}\"")),
        Span::Vertical(n) if n != 1 => attrs.insert(0, format!("rowspan=\"{n}\"")),
        _ => {}
    }
    let attrs: String = attrs.iter().map(|a| format!(" {a}")).collect();
    format!("<{element}{attrs}>{content}</{element}>")
}

/// The classes marking a cell's borders, eg `border-top-double`
/// (borderClasses).
pub fn border_classes<T>(cell: &Cell<T>) -> Vec<String> {
    let b = &cell.border;
    [
        ("left", b.left),
        ("right", b.right),
        ("top", b.top),
        ("bottom", b.bottom),
    ]
    .into_iter()
    .filter_map(|(side, line)| {
        let cls = match line {
            Lines::NoLine => return None,
            Lines::SingleLine => "single",
            Lines::DoubleLine => "double",
        };
        Some(format!("border-{side}-{cls}"))
    })
    .collect()
}

pub(crate) mod tests {
    use super::*;
    use crate::amount::Amount;
    use crate::write::spreadsheet::Border;

    // tests_Hledger_Write_Html.
    #[cfg_attr(test, test)]
    pub(crate) fn format_cells() {
        let str_ = |s: &str| Cell::new(s.to_string());
        assert_eq!(format_cell(&str_("a")), "<td>a</td>");
        assert_eq!(
            format_cell(&str_("a<b>&\"c\"")),
            "<td>a&lt;b&gt;&amp;&quot;c&quot;</td>"
        );
        assert_eq!(format_cell(&Cell::header("h")), "<th>h</th>");
        assert_eq!(
            format_cell(&Cell {
                typ: CellType::Date,
                ..str_("2026-01-01")
            }),
            "<td class=\"date\">2026-01-01</td>"
        );
        assert_eq!(
            format_cell(&Cell {
                typ: CellType::Amount(Amount::null()),
                class: "amount".to_string(),
                ..str_("$1")
            }),
            "<td align=\"right\" class=\"amount\"><span class=\"amount\">$1</span></td>"
        );
        assert_eq!(
            format_cell(&Cell {
                typ: CellType::MixedAmount,
                parts: vec!["$1".to_string(), "2 €".to_string()],
                ..str_("$1, 2 €")
            }),
            "<td align=\"right\"><span class=\"amount\">$1</span>, <span class=\"amount\">2 €</span></td>"
        );
        assert_eq!(
            format_cell(&Cell {
                anchor: "register?q=a&b".to_string(),
                ..str_("a")
            }),
            "<td><a href=\"register?q=a&amp;b\">a</a></td>"
        );
        assert_eq!(
            format_cell(&Cell {
                anchor: "register?q=a".to_string(),
                title: "Show \"a\"".to_string(),
                ..str_("a")
            }),
            "<td><a href=\"register?q=a\" title=\"Show &quot;a&quot;\">a</a></td>"
        );
        assert_eq!(
            format_cell(&Cell {
                title: "t".to_string(),
                ..str_("a")
            }),
            "<td>a</td>"
        );
        assert_eq!(
            format_cell(&Cell {
                style: Style::Body(Emphasis::Total),
                border: Border {
                    top: Lines::DoubleLine,
                    ..Border::default()
                },
                ..str_("Total:")
            }),
            "<td class=\"border-top-double\"><b>Total:</b></td>"
        );
        assert_eq!(
            format_cell(&Cell {
                class: "account".to_string(),
                border: Border {
                    bottom: Lines::SingleLine,
                    ..Border::default()
                },
                ..Cell::header("h")
            }),
            "<th class=\"account border-bottom-single\">h</th>"
        );
        let spanned = |span| Cell { span, ..str_("a") };
        assert_eq!(
            format_cell(&spanned(Span::Horizontal(2))),
            "<td colspan=\"2\">a</td>"
        );
        assert_eq!(format_cell(&spanned(Span::Horizontal(1))), "<td>a</td>");
        assert_eq!(
            format_cell(&spanned(Span::Vertical(2))),
            "<td rowspan=\"2\" valign=\"top\">a</td>"
        );
        assert_eq!(format_cell(&spanned(Span::Covered)), "");
    }

    #[cfg_attr(test, test)]
    pub(crate) fn rows_and_tables() {
        assert_eq!(
            format_row(&[Cell::new("a".to_string()), Cell::new("b".to_string())]),
            "<tr><td>a</td><td>b</td></tr>\n"
        );
        let html = titled_table_html("T & U", &[vec![Cell::header("h")]]);
        assert!(html.starts_with("<style>\ntable {"));
        assert!(html.ends_with(
            "</style>\n<link rel=\"stylesheet\" href=\"hledger.css\">\n<h3 class=\"report-title\">T &amp; U</h3>\n<table>\n<tr><th>h</th></tr>\n</table>"
        ));
    }
}
