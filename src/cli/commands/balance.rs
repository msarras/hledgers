//! The balance command (Hledger.Cli.Commands.Balance and
//! Hledger.Cli.Commands.Balance.Internal): accounts with their balance
//! changes or balances, in one column (a Ledger-style list or tree, lines
//! laid out by a --format line format), or with a report interval, in a
//! table with a column per period. Text output; the other output formats
//! and charts come with hledgers milestone 5c, budgets with 5b.

use std::collections::BTreeSet;

use crate::account::account_name_drop;
use crate::amount::render::{
    AmountFormat, show_mixed_amount_b, show_mixed_amount_lines_b, show_mixed_amount_lines_parts_b,
};
use crate::amount::{MixedAmount, Sym};
use crate::cli::args::{Arity, Flag, Mode};
use crate::cli::commands::not_yet;
use crate::cli::commands::print::unsupported_output_format_error;
use crate::cli::io::{warn_if_large_multi_period_report, with_title};
use crate::cli::opts::{
    CliOpts, OUTPUT_FILE_FLAG, OUTPUT_VIEW_FLAG, flat_flag, output_format_flag, tree_flag,
};
use crate::dates::{DateSpan, Interval, prevday};
use crate::model::Journal;
use crate::reports::opts::{
    AccountListMode, BalanceAccumulation, BalanceCalculation, Layout, PeriodTitles, ReportOpts,
};
use crate::reports::periodic::{
    BalanceReport, DisplayName, MultiBalanceReport, PeriodicReportRow, balance_report,
    balance_report_styles, multi_balance_report,
};
use crate::text::{Wide, format_text};
use crate::valuation::{ConversionOp, ValuationType};
use crate::write::string_format::{Component, Field, StringFormat};
use crate::write::table::{
    Align, Cell, Header, Line, Table, TableOpts, concat_tables, render_columns, render_row,
    render_table, render_table_by_rows,
};

const NONE: Arity = Arity::None;
const REQ: Arity = Arity::Req;
const OPT: Arity = Arity::Opt;

pub static MODE: Mode = Mode {
    name: "balance",
    aliases: &["bal"],
    flags: &[
        Flag::new(&["sum"], NONE, "", "calculation mode: show sum of posting amounts (default)"),
        Flag::new(&["valuechange"], NONE, "", "calculation mode: show total change of value of period-end historical balances (caused by deposits, withdrawals, market price fluctuations)"),
        Flag::new(&["gain"], NONE, "", "calculation mode: show capital gain/loss (historical balance value minus cost basis, or transacted cost where there is none)"),
        Flag::new(&["budget"], OPT, "DESCPAT", "calculation mode: show sum of posting amounts together with budget goals defined by periodic\ntransactions. With a DESCPAT argument (must be separated by = not space),\nuse only periodic transactions with matching description\n(case insensitive substring match).\n"),
        Flag::new(&["count"], NONE, "", "calculation mode: show the count of postings"),
        Flag::new(&["change"], NONE, "", "accumulation mode: accumulate amounts from column start to column end (in multicolumn reports, default)"),
        Flag::new(&["cumulative"], NONE, "", "accumulation mode: accumulate amounts from report start (specified by e.g. -b/--begin) to column end"),
        Flag::new(&["historical"], NONE, "", "accumulation mode: accumulate amounts from journal start to column end (includes postings before report start date)").short(&['H']),
        flat_flag(true),
        tree_flag(true),
        Flag::new(&["drop"], REQ, "N", "in list mode, omit N leading account name parts"),
        Flag::new(&["declared"], NONE, "", "include non-parent declared accounts (best used with -E)"),
        Flag::new(&["average"], NONE, "", "show a row average column (in multicolumn reports)").short(&['A']),
        Flag::new(&["row-total"], NONE, "", "show a row total column (in multicolumn reports)").short(&['T']),
        Flag::new(&["summary-only"], NONE, "", "display only row summaries (e.g. row total, average) (in multicolumn reports)"),
        Flag::new(&["no-total"], NONE, "", "omit the final total row").short(&['N']),
        Flag::new(&["no-elide"], NONE, "", "in tree mode, don't squash boring parent accounts; in list mode, also show parent accounts (usually zero, hidden without -E)"),
        Flag::new(&["full-names"], NONE, "", "in tree mode, show full account names instead of indented leaf names"),
        Flag::new(&["format"], REQ, "FORMATSTR", "use this custom line format (in simple reports)"),
        Flag::new(&["sort-amount"], NONE, "", "sort by amount instead of account code/name (in flat mode). With multiple columns, sorts by the row total, or by row average if that is displayed.").short(&['S']),
        Flag::new(&["percent"], NONE, "", "express values in percentage of each column's total").short(&['%']),
        Flag::new(&["related"], NONE, "", "show the other accounts transacted with, instead").short(&['r']),
        Flag::new(&["invert"], NONE, "", "display all amounts with reversed sign"),
        Flag::new(&["transpose"], NONE, "", "switch rows and columns (use vertical time axis); repeat to cancel"),
        Flag::new(&["layout"], REQ, "ARG", "how to lay out multi-commodity amounts and the overall table:\n'wide[,W]': commodities on same line, up to W wide\n'tall'    : commodities on separate lines\n'bare'    : commodity symbols in a separate column\n'tidy'    : each data field in its own column\n"),
        Flag::new(&["base-url"], REQ, "URLPREFIX", "in html output, generate links to hledger-web, with this prefix. (Usually the base url shown by hledger-web; can also be relative.)"),
        output_format_flag("select the output format. Supported formats:\ntxt, html, csv, tsv, json, fods, svg."),
        OUTPUT_FILE_FLAG,
        OUTPUT_VIEW_FLAG,
        CHART_FLAG,
        CHART_WIDTH_FLAG,
        Flag::new(&["commodity-column"], NONE, "", "show commodity symbols in a separate column, amounts as bare numbers, one row per commodity").hidden(),
    ],
};

/// The --chart flag (chartFlag).
pub const CHART_FLAG: Flag = Flag::new(
    &["chart"],
    OPT,
    "SCALE",
    "show a chart: instead of the table (txt), above it (html), or as data (csv, tsv, json). SCALE: the amount that fills the chart (eg 1000 or 1000 USD; one per commodity), for comparable charts.",
)
.default("auto");

/// The --width flag of commands that can show a chart (chartWidthFlag).
pub const CHART_WIDTH_FLAG: Flag = Flag::new(
    &["width"],
    REQ,
    "N",
    "with --chart: draw the chart N characters wide (default: the terminal width)",
)
.short(&['w']);

/// Run the balance command (balance): a multi-period report with a report
/// interval, otherwise a single-column one.
pub fn balance(opts: &CliOpts, j: &Journal) -> Result<String, String> {
    let fmt = opts.output_format();
    if opts.raw.string("chart").is_some() || fmt == "svg" {
        return Err(not_yet("balance --chart", "5c"));
    }
    let ropts0 = &opts.spec.opts;
    if ropts0.balance_calc == BalanceCalculation::Budget {
        return Err(not_yet("balance --budget", "5b"));
    }
    let layout = ropts0.layout.clone()?;
    // a tidy report has no totals
    let ropts = ReportOpts {
        no_total: ropts0.no_total || layout == Layout::Tidy,
        ..ropts0.clone()
    };
    let delimited = fmt == "csv" || fmt == "tsv";
    let multiperiod =
        ropts.interval != Interval::NoInterval || (layout == Layout::Tidy && delimited);
    let styles = balance_report_styles(&ropts, j);
    let other_format = |fmt: &str| match fmt {
        "csv" | "tsv" | "html" | "json" | "fods" => not_yet(&format!("balance -O {fmt}"), "5c"),
        _ => unsupported_output_format_error(fmt),
    };
    if multiperiod {
        warn_if_large_multi_period_report(opts, j);
        let mut report = multi_balance_report(&opts.spec, j)?;
        report.style_amounts(&styles);
        match fmt.as_str() {
            "txt" => Ok(multi_balance_report_as_text(&ropts, layout, &report)),
            f => Err(other_format(f)),
        }
    } else {
        let mut report = balance_report(&opts.spec, j)?;
        report.style_amounts(&styles);
        match fmt.as_str() {
            "txt" => Ok(with_title(
                opts,
                balance_report_as_text(&ropts, layout, &report)?,
            )),
            f => Err(other_format(f)),
        }
    }
}

// Single-column balance reports

/// A single-column balance report as text: each item's line(s) in the
/// --format line format, then unless --no-total a line of dashes and the
/// total; or laid out with commodity columns (balanceReportAsText). The
/// default format's amount column widens to fit the widest amount.
pub fn balance_report_as_text(
    ropts: &ReportOpts,
    layout: Layout,
    report: &BalanceReport,
) -> Result<String, String> {
    let custom = !matches!(
        ropts.format.components().first(),
        Some(Component::Field {
            field: Field::Total,
            ..
        })
    );
    match layout {
        Layout::Bare if custom => {
            return Err("Custom format not supported with commodity columns".to_string());
        }
        Layout::Bare => return Ok(bare_layout_balance_report_as_text(ropts, report)),
        Layout::BareWide => return Ok(bare_wide_layout_balance_report_as_text(ropts, report)),
        _ => {}
    }
    let is_default = ropts.format == StringFormat::default();
    let amount_width = default_balance_amount_column_width(ropts, layout, report);
    let format = if is_default {
        widened_default_format(amount_width)
    } else {
        ropts.format.clone()
    };
    let mut out = String::new();
    let mut sizes: Vec<Vec<usize>> = Vec::new();
    for (name, amount) in &report.items {
        let (text, widths) = if ropts.account_list_mode == AccountListMode::Tree && ropts.full_names
        {
            let full = account_name_drop(ropts.drop, &name.full);
            render_balance_report_item(ropts, &format, layout, &full, 0, amount)
        } else {
            render_balance_report_item(ropts, &format, layout, &name.name, name.indent, amount)
        };
        out.push_str(&text);
        out.push('\n');
        sizes.push(widths);
    }
    if !ropts.no_total {
        let overline_width = if custom {
            let columns = sizes.iter().map(Vec::len).max().unwrap_or(0);
            (0..columns)
                .map(|i| {
                    sizes
                        .iter()
                        .filter_map(|s| s.get(i))
                        .max()
                        .copied()
                        .unwrap_or(0)
                })
                .sum()
        } else if is_default {
            amount_width
        } else {
            20
        };
        let (total, _) = render_balance_report_item(ropts, &format, layout, "", 0, &report.total);
        out.push_str(&"-".repeat(overline_width));
        out.push('\n');
        out.push_str(&total);
        out.push('\n');
    }
    Ok(out)
}

/// The default line format with its amount column this wide
/// (widenDefaultBalanceLineFormat's setTotalMinWidth).
fn widened_default_format(width: usize) -> StringFormat {
    let mut components = StringFormat::default().components().to_vec();
    if let Some(Component::Field { min, .. }) = components.first_mut() {
        *min = Some(width);
    }
    StringFormat::BottomAligned(components)
}

/// The width of the default format's amount column: 20, or the width of
/// the widest amount shown, items' or total's
/// (defaultBalanceAmountColumnWidth).
fn default_balance_amount_column_width(
    ropts: &ReportOpts,
    layout: Layout,
    report: &BalanceReport,
) -> usize {
    let fmt = AmountFormat {
        commodity: layout != Layout::Bare,
        min_width: Some(0),
        colour: false,
        ..AmountFormat::NO_COST
    };
    let total = (!ropts.no_total).then_some(&report.total);
    (report.items.iter().map(|(_, m)| m))
        .chain(total)
        .map(|m| show_mixed_amount_b(&fmt, m).width)
        .fold(20, usize::max)
}

/// A balance report item (its shown name, indent and amount) as text in
/// the format, with each component's width (renderBalanceReportItem): the
/// components side by side, an amount of several lines (or several
/// commodities) next to the last line of the others when bottom-aligned.
fn render_balance_report_item(
    ropts: &ReportOpts,
    format: &StringFormat,
    layout: Layout,
    name: &str,
    indent: usize,
    amount: &MixedAmount,
) -> (String, Vec<usize>) {
    let (top, one_line) = match format {
        StringFormat::OneLine(_) => (true, true),
        StringFormat::TopAligned(_) => (true, false),
        StringFormat::BottomAligned(_) => (false, false),
    };
    let cells: Vec<Cell> = (format.components().iter())
        .map(|c| render_component(top, one_line, ropts, layout, (name, indent, amount), c))
        .collect();
    let widths = cells.iter().map(Cell::width).collect();
    let opts = TableOpts {
        borders: false,
        border_spaces: false,
        ..TableOpts::default()
    };
    let row = Header::Group(Line::NoLine, cells.into_iter().map(Header::Leaf).collect());
    (render_row(&opts, &row), widths)
}

/// One format component of a balance report item as a cell
/// (renderComponent).
fn render_component(
    top: bool,
    one_line: bool,
    ropts: &ReportOpts,
    layout: Layout,
    (name, indent, amount): (&str, usize, &MixedAmount),
    c: &Component,
) -> Cell {
    let Component::Field {
        left,
        min,
        max,
        field,
    } = c
    else {
        let Component::Literal(s) = c else {
            unreachable!()
        };
        return Cell::text(Align::TopLeft, s);
    };
    let align = match (top, *left) {
        (true, true) => Align::TopLeft,
        (true, false) => Align::TopRight,
        (false, true) => Align::BottomLeft,
        (false, false) => Align::BottomRight,
    };
    let one = |w: Wide| Cell {
        align,
        lines: vec![w],
    };
    match field {
        Field::DepthSpacer => {
            let d = indent * min.unwrap_or(1);
            let d = max.map_or(d, |m| d.min(m));
            one(Wide {
                s: " ".repeat(d),
                width: d,
            })
        }
        Field::Account => Cell::text(align, &format_text(*left, *min, *max, name)),
        Field::Total => {
            let fmt = AmountFormat {
                commodity: layout != Layout::Bare,
                one_line,
                min_width: min.map(|w| w as isize),
                max_width: max.map(|w| w as isize),
                colour: ropts.color,
                ..AmountFormat::NO_COST
            };
            one(show_mixed_amount_b(&fmt, amount))
        }
        _ => one(Wide::default()),
    }
}

/// A single-column balance report with the commodity symbols in a column
/// of their own, the amounts as bare numbers one commodity per line
/// (bareLayoutBalanceReportAsText).
fn bare_layout_balance_report_as_text(ropts: &ReportOpts, report: &BalanceReport) -> String {
    let render = |name: &str, indent: usize, amount: &MixedAmount| -> Vec<Cell> {
        let cs: Vec<Sym> = if amount.looks_zero() {
            vec![Sym::from("")]
        } else {
            amount.commodities().into_iter().collect()
        };
        let fmt = AmountFormat {
            commodity: false,
            commodity_order: Some(cs.clone()),
            colour: ropts.color,
            ..AmountFormat::ONE_LINE_NO_COST
        };
        let amounts = show_mixed_amount_lines_b(&fmt, amount);
        let shown_name = format!("{}{name}", " ".repeat(indent.saturating_sub(1) * 2));
        let mut names = vec![Wide::default(); amounts.len().saturating_sub(1)];
        names.push(Wide::new(&shown_name));
        vec![
            Cell {
                align: Align::TopRight,
                lines: amounts,
            },
            Cell {
                align: Align::TopLeft,
                lines: cs.iter().map(|c| Wide::new(c)).collect(),
            },
            Cell {
                align: Align::TopLeft,
                lines: names,
            },
        ]
    };
    let rows: Vec<Vec<Cell>> = (report.items.iter())
        .map(|(n, m)| render(&n.name, n.indent, m))
        .collect();
    let total = (!ropts.no_total).then(|| render("", 0, &report.total));
    let mut sizes: Vec<usize> = Vec::new();
    for row in total.iter().chain(&rows) {
        for (i, c) in row.iter().enumerate() {
            match sizes.get_mut(i) {
                Some(s) => *s = (*s).max(c.width()),
                None => sizes.push(c.width()),
            }
        }
    }
    let overline = Cell::text(
        Align::TopLeft,
        &"-".repeat(sizes.first().copied().unwrap_or(0)),
    );
    let opts = TableOpts {
        borders: ropts.pretty,
        ..TableOpts::default()
    };
    let sep = if ropts.pretty {
        Line::SingleLine
    } else {
        Line::NoLine
    };
    let mut out = String::new();
    let mut emit = |cells: Vec<Cell>| {
        let row = Header::Group(sep, cells.into_iter().map(Header::Leaf).collect());
        out.push_str(&render_columns(&opts, &sizes, &row));
        out.push('\n');
    };
    for row in rows {
        emit(row);
    }
    if let Some(total) = total {
        emit(vec![overline]);
        emit(total);
    }
    out
}

/// A single-column balance report with a column for each commodity
/// (bareWideLayoutBalanceReportAsText, from
/// balanceReportAsSpreadsheetParts' cells).
fn bare_wide_layout_balance_report_as_text(ropts: &ReportOpts, report: &BalanceReport) -> String {
    let commodities: Vec<Sym> = (report.items.iter())
        .flat_map(|(_, m)| m.commodities())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let fmt = AmountFormat {
        commodity: false,
        commodity_order: Some(commodities.clone()),
        ..AmountFormat::MACHINE
    };
    let amounts = |m: &MixedAmount| -> Vec<String> {
        (show_mixed_amount_lines_parts_b(&fmt, m).into_iter())
            .map(|(w, _)| w.s)
            .collect()
    };
    let mut row_headers = Vec::new();
    let mut rows = Vec::new();
    for (name, m) in &report.items {
        row_headers.push(Header::Leaf(render_balance_acct(ropts, "\u{a0}", name)));
        rows.push(amounts(m));
    }
    let mut row_header = Header::Group(Line::NoLine, row_headers);
    if !ropts.no_total {
        row_header = Header::Group(
            Line::SingleLine,
            vec![
                row_header,
                Header::Group(Line::NoLine, vec![Header::Leaf("Total:".to_string())]),
            ],
        );
        rows.push(amounts(&report.total));
    }
    let table = Table {
        row_headers: row_header,
        col_headers: Header::Group(
            Line::NoLine,
            commodities
                .iter()
                .map(|c| Header::Leaf(c.to_string()))
                .collect(),
        ),
        rows,
    };
    let opts = TableOpts {
        borders: ropts.pretty,
        pretty: ropts.pretty,
        ..TableOpts::default()
    };
    render_table(
        &opts,
        |h: &String| Cell::text(Align::TopLeft, h),
        |h: &String| Cell::text(Align::TopLeft, h),
        |a: &String| Cell::text(Align::TopRight, a),
        &table,
    )
}

/// An account's name in a report: in tree mode (without --full-names) its
/// shown name indented by two of these spaces per step, otherwise its full
/// name without the components --drop drops (renderBalanceAcct).
fn render_balance_acct(ropts: &ReportOpts, space: &str, name: &DisplayName) -> String {
    if ropts.account_list_mode == AccountListMode::Tree && !ropts.full_names {
        format!("{}{}", space.repeat(name.indent * 2), name.name)
    } else {
        account_name_drop(ropts.drop, &name.full)
    }
}

// Multi-column balance reports

/// A multi-period balance report as text: its title, then the table
/// (multiBalanceReportAsText).
pub fn multi_balance_report_as_text(
    ropts: &ReportOpts,
    layout: Layout,
    report: &MultiBalanceReport,
) -> String {
    let title = multi_balance_report_title(ropts, report);
    let mut out = if title.is_empty() {
        String::new()
    } else {
        format!("{title}\n\n")
    };
    let table = multi_balance_report_as_table(ropts, layout, report);
    out.push_str(&multi_balance_report_table_as_text(ropts, layout, &table));
    out
}

/// The heading of a multi-period balance report: what it shows, over which
/// dates, valued how; or --title's (multiBalanceReportTitle).
pub fn multi_balance_report_title(ropts: &ReportOpts, report: &MultiBalanceReport) -> String {
    use BalanceAccumulation::*;
    use BalanceCalculation::*;
    let what = match (ropts.balance_calc, ropts.balance_accum) {
        (ValueChange, PerPeriod) => "Period-end value changes",
        (ValueChange, Cumulative) => "Cumulative period-end value changes",
        (Gain, PerPeriod) => "Incremental gain",
        (Gain, Cumulative) => "Cumulative gain",
        (Gain, Historical) => "Historical gain",
        (PostingsCount, PerPeriod) => "Posting counts",
        (PostingsCount, Cumulative) => "Cumulative posting counts",
        (PostingsCount, Historical) => "Historical posting counts",
        (TransactionsCount, _) => "Transaction counts",
        (_, PerPeriod) => "Balance changes",
        (_, Cumulative) => "Ending balances (cumulative)",
        (_, Historical) => "Ending balances (historical)",
    };
    let changing_valuation = ropts.balance_calc == ValueChange && ropts.balance_accum != Historical;
    let cost = match ropts.conversion_op {
        Some(ConversionOp::ToCost) => ", converted to cost",
        _ => "",
    };
    let value = match &ropts.value {
        Some(ValuationType::AtThen(_)) => ", valued at posting date".to_string(),
        Some(ValuationType::AtEnd(_)) if changing_valuation => String::new(),
        Some(ValuationType::AtEnd(_)) => ", valued at period ends".to_string(),
        Some(ValuationType::AtNow(_)) => ", current value".to_string(),
        Some(ValuationType::AtDate(d, _)) => format!(", valued at {d}"),
        None => String::new(),
    };
    let default = format!("{what} in {}{cost}{value}:", report.span());
    ropts.effective_title(&default).to_string()
}

/// A multi-period balance report as a table of text: an account name per
/// row (repeated for each line of its amounts), the period names (and
/// Total and Average) as column headings, the totals row below a line
/// unless --no-total; transposed with --transpose, keeping just the leaf
/// accounts in tree mode (multiBalanceReportAsTable,
/// multiBalanceReportAsPartTable).
pub fn multi_balance_report_as_table(
    ropts: &ReportOpts,
    layout: Layout,
    report: &MultiBalanceReport,
) -> Table<String, String, Wide> {
    let commodities: Vec<Sym> = (report.rows.iter())
        .flat_map(|r| r.amounts.iter().flat_map(MixedAmount::commodities))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut span_names: Vec<String> = Vec::new();
    if !ropts.summary_only {
        span_names
            .extend((report.dates.iter()).map(|s| report_period_name(ropts, &report.dates, s)));
    }
    if ropts.has_totals_column() {
        span_names.push("  Total".to_string());
    }
    if ropts.average {
        span_names.push("Average".to_string());
    }
    let mut col_headings: Vec<String> = Vec::new();
    if layout == Layout::Bare {
        col_headings.push("Commodity".to_string());
    }
    match layout {
        Layout::BareWide => {
            for s in &span_names {
                col_headings.extend(commodities.iter().map(|c| format!("{s} ({c})")));
            }
        }
        _ => col_headings.extend(span_names),
    }
    let tree = ropts.account_list_mode == AccountListMode::Tree;
    let is_leaf = |row: &PeriodicReportRow<DisplayName, MixedAmount>| {
        let prefix = format!("{}:", row.name.full);
        !(report.rows.iter()).any(|r| r.name.full.starts_with(&prefix))
    };
    let mut accounts = Vec::new();
    let mut rows = Vec::new();
    for row in &report.rows {
        if ropts.transpose && tree && !is_leaf(row) {
            continue;
        }
        let lines = multi_balance_row_as_text(ropts, layout, &commodities, row);
        let name = render_balance_acct(ropts, " ", &row.name);
        accounts.extend(std::iter::repeat_n(Header::Leaf(name), lines.len()));
        rows.extend(lines);
    }
    let pretty_line = if ropts.pretty {
        Line::SingleLine
    } else {
        Line::NoLine
    };
    let mut table = Table {
        row_headers: Header::Group(Line::NoLine, accounts),
        col_headers: Header::Group(
            pretty_line,
            col_headings.into_iter().map(Header::Leaf).collect(),
        ),
        rows,
    };
    if !ropts.no_total {
        let totals = multi_balance_row_as_text(ropts, layout, &commodities, &report.totals);
        let headers = std::iter::repeat_n(Header::Leaf(String::new()), totals.len().max(1));
        let total_table = Table {
            row_headers: Header::Group(Line::NoLine, headers.collect()),
            col_headers: Header::Leaf(()),
            rows: totals,
        };
        table = concat_tables(Line::SingleLine, table, total_table);
    }
    if ropts.transpose {
        table = Table {
            row_headers: table.col_headers,
            col_headers: table.row_headers,
            rows: transpose(&table.rows),
        };
    }
    table
}

/// A multi-period report row's cells as lines of text, laid out as the
/// layout says: the amounts on one line each (wide), one commodity per
/// line (tall), as bare numbers after a commodity column (bare), or a
/// column per commodity (barewide); a tidy layout has none in text
/// (multiBalanceRowAsText, multiBalanceRowAsCellBuilders).
fn multi_balance_row_as_text<N>(
    ropts: &ReportOpts,
    layout: Layout,
    all_commodities: &[Sym],
    row: &PeriodicReportRow<N, MixedAmount>,
) -> Vec<Vec<Wide>> {
    let fmt = AmountFormat {
        colour: ropts.color,
        ..AmountFormat::ONE_LINE_NO_COST
    };
    let mut amounts: Vec<&MixedAmount> = Vec::new();
    if !ropts.summary_only {
        amounts.extend(&row.amounts);
    }
    if ropts.has_totals_column() && !row.amounts.is_empty() {
        amounts.push(&row.total);
    }
    if ropts.average && !row.amounts.is_empty() {
        amounts.push(&row.average);
    }
    let bare = |cs: &[Sym]| AmountFormat {
        commodity: false,
        commodity_order: Some(cs.to_vec()),
        min_width: None,
        ..fmt.clone()
    };
    match layout {
        Layout::Wide(width) => {
            let fmt = AmountFormat {
                max_width: width.map(|w| w as isize),
                ..fmt.clone()
            };
            vec![
                amounts
                    .iter()
                    .map(|m| show_mixed_amount_b(&fmt, m))
                    .collect(),
            ]
        }
        Layout::Tall => {
            let fmt = AmountFormat {
                max_width: None,
                ..fmt.clone()
            };
            let columns: Vec<Vec<Wide>> = (amounts.iter())
                .map(|m| show_mixed_amount_lines_b(&fmt, m))
                .collect();
            if columns.is_empty() {
                return vec![vec![]];
            }
            let n = columns.iter().map(Vec::len).max().unwrap_or(0);
            (0..n)
                .map(|i| {
                    (columns.iter())
                        .map(|c| c.get(i).cloned().unwrap_or_default())
                        .collect()
                })
                .collect()
        }
        Layout::Bare => {
            let cs: Vec<Sym> = if amounts.iter().all(|m| m.looks_zero()) {
                vec![Sym::from("")]
            } else {
                (amounts.iter())
                    .flat_map(|m| m.commodities())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect()
            };
            let fmt = bare(&cs);
            let columns: Vec<Vec<Wide>> = (amounts.iter())
                .map(|m| show_mixed_amount_lines_b(&fmt, m))
                .collect();
            (cs.iter().zip(transpose(&columns)))
                .map(|(c, mut line)| {
                    line.insert(0, Wide::new(c));
                    line
                })
                .collect()
        }
        Layout::BareWide => {
            let fmt = bare(all_commodities);
            vec![
                (amounts.iter())
                    .flat_map(|m| show_mixed_amount_lines_b(&fmt, m))
                    .collect(),
            ]
        }
        Layout::Tidy => Vec::new(),
    }
}

/// The rows of a table as columns, a short row leaving the later columns
/// shorter (Data.List.transpose).
fn transpose<T: Clone>(rows: &[Vec<T>]) -> Vec<Vec<T>> {
    let n = rows.iter().map(Vec::len).max().unwrap_or(0);
    (0..n)
        .map(|i| rows.iter().filter_map(|r| r.get(i).cloned()).collect())
        .collect()
}

/// A multi-period report's table as text: plain, or with box drawing and
/// borders with --pretty; headings and amounts right-aligned, account
/// names left-aligned, as is the commodity column of the bare layout
/// (multiBalanceReportTableAsText).
pub fn multi_balance_report_table_as_text(
    ropts: &ReportOpts,
    layout: Layout,
    table: &Table<String, String, Wide>,
) -> String {
    let opts = TableOpts {
        borders: ropts.pretty,
        pretty: ropts.pretty,
        ..TableOpts::default()
    };
    let bare = layout == Layout::Bare && !ropts.transpose;
    let first_left = |i: usize| {
        if bare && i == 0 {
            Align::TopLeft
        } else {
            Align::TopRight
        }
    };
    render_table_by_rows(
        &opts,
        |chs| {
            (chs.iter().enumerate())
                .map(|(i, h)| Cell::text(first_left(i), h))
                .collect()
        },
        |rh, row| {
            let cells = (row.iter().enumerate())
                .map(|(i, w)| Cell {
                    align: first_left(i),
                    lines: vec![w.clone()],
                })
                .collect();
            (Cell::text(Align::TopLeft, rh), cells)
        },
        table,
    )
}

/// A report period's column heading: for changes, its compact name (with
/// the month abbreviated unless the report spans several years) or with
/// --period-titles=dates its dates; for accumulated balances, its last
/// date (reportPeriodName).
pub fn report_period_name(ropts: &ReportOpts, spans: &[DateSpan], span: &DateSpan) -> String {
    match ropts.balance_accum {
        BalanceAccumulation::PerPeriod => match ropts.period_titles {
            PeriodTitles::Dates => span.show_full(),
            PeriodTitles::Compact => {
                let years: BTreeSet<Option<i64>> = spans.iter().map(DateSpan::start_year).collect();
                if years.len() > 1 {
                    span.to_string()
                } else {
                    span.show_abbrev()
                }
            }
        },
        _ => span
            .end()
            .map_or_else(String::new, |e| prevday(e).to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dates::Day;
    use crate::reports::opts::ReportSpec;

    // tests_Balance "unicode in balance layout".
    #[test]
    fn unicode_in_balance_layout() {
        let s = "2009/01/01 * медвежья шкура\n  расходы:покупки  100\n  актив:наличные\n";
        let iopts = crate::read::InputOpts::default();
        let (j, st) = crate::read::journal::parse_journal(&iopts, "f.j", s).unwrap();
        let j = crate::read::finalise::journal_finalise(&iopts, "f.j", s, j, st).unwrap();
        let ropts = ReportOpts {
            no_total: true,
            ..ReportOpts::default()
        };
        let spec = ReportSpec::new(Day::from_ymd(2008, 11, 26).unwrap(), ropts.clone()).unwrap();
        let report = balance_report(&spec, &j).unwrap();
        assert_eq!(
            balance_report_as_text(&ropts, Layout::Wide(None), &report).unwrap(),
            "                -100  актив:наличные\n                 100  расходы:покупки\n"
        );
    }
}
