//! Writing spreadsheet tables as Flat OpenDocument Spreadsheet XML
//! (Hledger.Write.Ods): number formats for amounts, styles for the cells'
//! kinds, borders and emphasis, merged cells, links, and the heading rows
//! and columns kept in view.

use std::collections::{BTreeMap, BTreeSet};

use crate::amount::{Amount, Precision};
use crate::write::spreadsheet::{Border, Cell, CellType, Emphasis, Lines, Sheet, Span, Style};

/// How an amount cell's number is formatted (CommodityStyle): its
/// commodity, precision, and whether it has digit groups.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct CommodityStyle {
    commodity: String,
    precision: PrecisionKey,
    grouping: bool,
}

/// A precision, ordered as upstream's AmountPrecision is: fixed before
/// natural.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum PrecisionKey {
    Fixed(u8),
    Natural,
}

/// The kind of a cell's data, for its style (DataStyle).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum DataStyle {
    String,
    Integer,
    Date,
    Amount(CommodityStyle),
    MixedAmount,
}

/// A cell's style key: its borders, its style (body before head, items
/// before totals) and its kind of data, ordered as upstream's are.
type StyleKey = ((BorderKey, StyleOrd), DataStyle);
type BorderKey = (Lines, Lines, Lines, Lines);
type StyleOrd = (u8, u8);

fn border_key(b: &Border) -> BorderKey {
    (b.left, b.right, b.top, b.bottom)
}

fn style_ord(s: Style) -> StyleOrd {
    match s {
        Style::Body(Emphasis::Item) => (0, 0),
        Style::Body(Emphasis::Total) => (0, 1),
        Style::Head => (1, 0),
    }
}

fn style_from_ord((s, e): StyleOrd) -> Style {
    match (s, e) {
        (0, 0) => Style::Body(Emphasis::Item),
        (0, _) => Style::Body(Emphasis::Total),
        _ => Style::Head,
    }
}

fn border_from_key((left, right, top, bottom): BorderKey) -> Border {
    Border {
        left,
        right,
        top,
        bottom,
    }
}

/// (dataStyleFromType, commodityStyleFromAmount)
fn data_style(typ: &CellType) -> DataStyle {
    match typ {
        CellType::String => DataStyle::String,
        CellType::Integer => DataStyle::Integer,
        CellType::Date => DataStyle::Date,
        CellType::Amount(a) => DataStyle::Amount(commodity_style(a)),
        CellType::MixedAmount => DataStyle::MixedAmount,
    }
}

fn commodity_style(a: &Amount) -> CommodityStyle {
    CommodityStyle {
        commodity: a.commodity.to_string(),
        precision: match a.style.precision {
            Precision::Fixed(n) => PrecisionKey::Fixed(n),
            Precision::Natural => PrecisionKey::Natural,
        },
        grouping: a.style.digit_groups.is_some_and(|g| !g.sizes.is_empty()),
    }
}

/// (numberStyleName)
fn number_style_name(s: &CommodityStyle) -> String {
    let prec = match s.precision {
        PrecisionKey::Natural => "natural".to_string(),
        PrecisionKey::Fixed(k) => k.to_string(),
    };
    let grouping = if s.grouping { "-grouping" } else { "" };
    format!("{}-{prec}{grouping}", s.commodity)
}

/// The number style of a commodity style (numberConfig).
fn number_config(s: &CommodityStyle) -> Vec<String> {
    let prec = match s.precision {
        PrecisionKey::Natural => String::new(),
        PrecisionKey::Fixed(k) => format!(" number:decimal-places='{k}'"),
    };
    let grouping = if s.grouping {
        " number:grouping='true'"
    } else {
        ""
    };
    let space = if s.commodity.is_empty() { "" } else { " " };
    vec![
        format!(
            "  <number:number-style style:name='number-{}'>",
            number_style_name(s)
        ),
        format!("    <number:number number:min-integer-digits='1'{prec}{grouping}/>"),
        format!("    <number:text>{space}{}</number:text>", s.commodity),
        "  </number:number-style>".to_string(),
    ]
}

const SIDES: [&str; 4] = ["left", "right", "top", "bottom"];

fn sides(b: &Border) -> [Lines; 4] {
    [b.left, b.right, b.top, b.bottom]
}

/// (borderName)
fn border_name(b: &Border) -> String {
    let named: Vec<String> = (SIDES.iter().zip(sides(b)))
        .filter_map(|(side, line)| match line {
            Lines::NoLine => None,
            Lines::SingleLine => Some(format!("{side}:single")),
            Lines::DoubleLine => Some(format!("{side}:double")),
        })
        .collect();
    if named.is_empty() {
        "noborder".to_string()
    } else {
        format!("border={}", named.join(","))
    }
}

/// (borderStyle)
fn border_style(b: &Border) -> Vec<String> {
    if *b == Border::default() {
        return Vec::new();
    }
    let props: String = (SIDES.iter().zip(sides(b)))
        .map(|(side, line)| {
            let style = match line {
                Lines::NoLine => "none",
                Lines::SingleLine => "1.5pt solid #000000",
                Lines::DoubleLine => "1.5pt double-thin #000000",
            };
            format!(" fo:border-{side}='{style}'")
        })
        .collect();
    vec![format!("    <style:table-cell-properties{props}/>")]
}

/// A cell style's name and its data style's name (styleNames).
fn style_names(style: Style, border: &Border, data: &DataStyle) -> (String, Option<String>) {
    let cstyle = match style {
        Style::Head => "head",
        Style::Body(Emphasis::Item) => "item",
        Style::Body(Emphasis::Total) => "total",
    };
    let bord = border_name(border);
    match data {
        DataStyle::Date => (
            format!("{cstyle}-{bord}-date"),
            Some("iso-date".to_string()),
        ),
        DataStyle::Integer => (
            format!("{cstyle}-{bord}-integer"),
            Some("integer".to_string()),
        ),
        DataStyle::Amount(c) => {
            let name = number_style_name(c);
            (
                format!("{cstyle}-{bord}-{name}"),
                Some(format!("number-{name}")),
            )
        }
        DataStyle::MixedAmount => (format!("{cstyle}-{bord}-mixedamount"), None),
        DataStyle::String => (format!("{cstyle}-{bord}"), None),
    }
}

/// A cell style (cellConfig).
fn cell_config(((border, style), data): &StyleKey) -> Vec<String> {
    let border = border_from_key(*border);
    let style = style_from_ord(*style);
    let bold = "    <style:text-properties fo:font-weight='bold'/>".to_string();
    let align_top = "    <style:table-cell-properties style:vertical-align='top'/>".to_string();
    let align = |a: &str| format!("    <style:paragraph-properties fo:text-align='{a}'/>");
    let mut more = border_style(&border);
    match style {
        Style::Body(Emphasis::Item) => more.push(align_top),
        Style::Body(Emphasis::Total) => more.extend([align_top, bold]),
        Style::Head => more.extend([align("center"), bold]),
    }
    if *data == DataStyle::MixedAmount {
        more.push(align("end"));
    }
    let (name, data_name) = style_names(style, &border, data);
    let attrs = format!(
        "style:name='{name}'{}",
        data_name.map_or(String::new(), |d| format!(" style:data-style-name='{d}'"))
    );
    if more.is_empty() {
        vec![format!(
            "  <style:style style:family='table-cell' {attrs}/>"
        )]
    } else {
        let mut lines = vec![format!("  <style:style style:family='table-cell' {attrs}>")];
        lines.extend(more);
        lines.push("  </style:style>".to_string());
        lines
    }
}

/// Text escaped for XML, a newline as a character reference (escape).
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\n' => out.push_str("&#10;"),
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// A cell's lines (formatCell): a table cell, covered if a merged cell
/// covers it, with its style, its span, its value type (numbers and dates
/// with their values), and its text, in a link if it has one.
fn format_cell(cell: &Cell<String>) -> Vec<String> {
    let (style, _) = style_names(cell.style, &cell.border, &data_style(&cell.typ));
    let value_type = match &cell.typ {
        CellType::Integer => format!("office:value-type='float' office:value='{}'", cell.content),
        CellType::Amount(a) => format!("office:value-type='float' office:value='{}'", a.quantity),
        CellType::Date => format!(
            "office:value-type='date' office:date-value='{}'",
            cell.content
        ),
        _ => "office:value-type='string'".to_string(),
    };
    let covered = if cell.span == Span::Covered {
        "covered-"
    } else {
        ""
    };
    let span = match cell.span {
        Span::Horizontal(n) if n > 1 => format!(" table:number-columns-spanned='{n}'"),
        Span::Vertical(n) if n > 1 => format!(" table:number-rows-spanned='{n}'"),
        _ => String::new(),
    };
    let text = escape(&cell.content);
    let text = if cell.anchor.is_empty() {
        text
    } else {
        format!(
            "<text:a xlink:href='{}'>{text}</text:a>",
            escape(&cell.anchor)
        )
    };
    vec![
        format!("<table:{covered}table-cell table:style-name='{style}'{span} {value_type}>"),
        format!("<text:p>{text}</text:p>"),
        format!("</table:{covered}table-cell>"),
    ]
}

/// The name of the encoding text is written in, as upstream shows its
/// locale's: UTF-8 if the locale (LC_ALL, LC_CTYPE or LANG, the first
/// set) names that codeset, otherwise ASCII, as the C locale's is.
/// hledgers always writes UTF-8; a UTF-8 locale that is not installed
/// gives ASCII upstream.
pub fn locale_encoding_name() -> &'static str {
    let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|v| std::env::var(v).ok().filter(|s| !s.is_empty()))
        .unwrap_or_default()
        .to_lowercase()
        .replace('-', "");
    if locale.contains("utf8") {
        "UTF-8"
    } else {
        "ASCII"
    }
}

/// Tables as a flat ODS document (printFods): the styles of all their
/// cells' kinds, the heading rows and columns each keeps in view, then
/// the tables, in name order.
pub fn print_fods(encoding: &str, tables: &BTreeMap<String, Sheet>) -> String {
    let cells = tables.values().flat_map(|(_, rows)| rows.iter().flatten());
    let styles: BTreeSet<StyleKey> = cells
        .map(|c| {
            (
                (border_key(&c.border), style_ord(c.style)),
                data_style(&c.typ),
            )
        })
        .collect();
    let number_styles: BTreeSet<&CommodityStyle> = (styles.iter())
        .filter_map(|(_, d)| match d {
            DataStyle::Amount(c) => Some(c),
            _ => None,
        })
        .collect();
    let mut custom: Vec<String> = number_styles.into_iter().flat_map(number_config).collect();
    custom.extend(styles.iter().flat_map(cell_config));
    let mut file_open: Vec<String> = [
        format!("<?xml version='1.0' encoding='{encoding}'?>"),
        "<office:document".into(),
        "  office:mimetype='application/vnd.oasis.opendocument.spreadsheet'".into(),
        "  office:version='1.3'".into(),
        "  xmlns:xsi='http://www.w3.org/2001/XMLSchema-instance'".into(),
        "  xmlns:xsd='http://www.w3.org/2001/XMLSchema'".into(),
        "  xmlns:text='urn:oasis:names:tc:opendocument:xmlns:text:1.0'".into(),
        "  xmlns:style='urn:oasis:names:tc:opendocument:xmlns:style:1.0'".into(),
        "  xmlns:meta='urn:oasis:names:tc:opendocument:xmlns:meta:1.0'".into(),
        "  xmlns:config='urn:oasis:names:tc:opendocument:xmlns:config:1.0'".into(),
        "  xmlns:xlink='http://www.w3.org/1999/xlink'".into(),
        "  xmlns:fo='urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0'".into(),
        "  xmlns:ooo='http://openoffice.org/2004/office'".into(),
        "  xmlns:office='urn:oasis:names:tc:opendocument:xmlns:office:1.0'".into(),
        "  xmlns:table='urn:oasis:names:tc:opendocument:xmlns:table:1.0'".into(),
        "  xmlns:number='urn:oasis:names:tc:opendocument:xmlns:datastyle:1.0'".into(),
        "  xmlns:of='urn:oasis:names:tc:opendocument:xmlns:of:1.2'".into(),
        "  xmlns:field='urn:openoffice:names:experimental:ooo-ms-interop:xmlns:field:1.0'".into(),
        "  xmlns:form='urn:oasis:names:tc:opendocument:xmlns:form:1.0'>".into(),
        "<office:styles>".into(),
        "  <number:date-style style:name='iso-date'>".into(),
        "    <number:year number:style='long'/>".into(),
        "    <number:text>-</number:text>".into(),
        "    <number:month number:style='long'/>".into(),
        "    <number:text>-</number:text>".into(),
        "    <number:day number:style='long'/>".into(),
        "  </number:date-style>".into(),
        "  <number:number-style style:name='integer'>".into(),
        "    <number:number number:min-integer-digits='1'/>".into(),
        "  </number:number-style>".into(),
    ]
    .into_iter()
    .collect();
    file_open.extend(custom);
    file_open.push("</office:styles>".into());
    // the opening part's single quotes are written as double quotes
    let mut lines: Vec<String> = file_open
        .into_iter()
        .map(|l| l.replace('\'', "\""))
        .collect();
    lines.extend(
        [
            " <office:settings>",
            "  <config:config-item-set config:name='ooo:view-settings'>",
            "   <config:config-item-map-indexed config:name='Views'>",
            "    <config:config-item-map-entry>",
            "     <config:config-item-map-named config:name='Tables'>",
        ]
        .map(String::from),
    );
    for (name, ((top, left), _)) in tables {
        lines.push(format!(
            "      <config:config-item-map-entry config:name='{name}'>"
        ));
        if *left > 0 {
            lines.push("       <config:config-item config:name='HorizontalSplitMode' config:type='short'>2</config:config-item>".into());
            lines.push(format!("       <config:config-item config:name='HorizontalSplitPosition' config:type='int'>{left}</config:config-item>"));
            lines.push(format!("       <config:config-item config:name='PositionRight' config:type='int'>{left}</config:config-item>"));
        }
        if *top > 0 {
            lines.push("       <config:config-item config:name='VerticalSplitMode' config:type='short'>2</config:config-item>".into());
            lines.push(format!("       <config:config-item config:name='VerticalSplitPosition' config:type='int'>{top}</config:config-item>"));
            lines.push(format!("       <config:config-item config:name='PositionBottom' config:type='int'>{top}</config:config-item>"));
        }
        lines.push("      </config:config-item-map-entry>".into());
    }
    lines.extend(
        [
            "     </config:config-item-map-named>",
            "    </config:config-item-map-entry>",
            "   </config:config-item-map-indexed>",
            "  </config:config-item-set>",
            " </office:settings>",
        ]
        .map(String::from),
    );
    for (name, (_, rows)) in tables {
        lines.push("<office:body>".into());
        lines.push("<office:spreadsheet>".into());
        lines.push(format!("<table:table table:name='{name}'>"));
        for row in rows {
            lines.push("<table:table-row>".into());
            lines.extend(row.iter().flat_map(format_cell));
            lines.push("</table:table-row>".into());
        }
        lines.push("</table:table>".into());
        lines.push("</office:spreadsheet>".into());
        lines.push("</office:body>".into());
    }
    lines.push("</office:document>".into());
    let mut out = lines.join("\n");
    out.push('\n');
    out
}
