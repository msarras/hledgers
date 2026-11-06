//! The balance command (Hledger.Cli.Commands.Balance and
//! Hledger.Cli.Commands.Balance.Internal): accounts with their balance
//! changes or balances, in one column (a Ledger-style list or tree, lines
//! laid out by a --format line format), or with a report interval, in a
//! table with a column per period; or a budget report. Text, csv, tsv,
//! json, html and fods output, and charts (with --chart, or svg).

use std::collections::{BTreeMap, BTreeSet};

use crate::account::{
    UNBUDGETED_ACCOUNT_NAME, account_name_drop, account_name_level, parent_account_names,
};
use crate::amount::render::{
    AmountFormat, show_mixed_amount_b, show_mixed_amount_lines_b, show_mixed_amount_lines_parts_b,
};
use crate::amount::{MixedAmount, Sym};
use crate::cli::anchor::{
    LinkOpts, amount_phrase, compose_anchor_with, date_span_cell_with, date_term,
    header_date_span_cell, link_params, remove_dates, remove_inacct, render_period_heading,
    set_account_anchor, set_account_anchor_with, total_date_span_cell, value_param, with_link,
};
use crate::cli::args::{Arity, Flag, Mode};
use crate::cli::chart::{
    balance_report_as_chart, chart_requested, check_chart_arg, check_chart_format,
    check_chart_width, multi_balance_report_as_chart, render_chart,
};
use crate::cli::commands::print::unsupported_output_format_error;
use crate::cli::io::{warn_if_large_multi_period_report, with_title};
use crate::cli::opts::{
    CliOpts, OUTPUT_FILE_FLAG, OUTPUT_VIEW_FLAG, flat_flag, output_format_flag, tree_flag,
};
use crate::dates::{DateSpan, Day, Interval, Period, prevday, spans_span};
use crate::decimal::Decimal;
use crate::model::Journal;
use crate::reports::budget::{BudgetCell, BudgetReport, budget_report};
use crate::reports::opts::{
    AccountListMode, BalanceAccumulation, BalanceCalculation, Layout, NormalSign, PeriodTitles,
    ReportOpts, ReportSpec,
};
use crate::reports::periodic::{
    BalanceReport, DisplayName, MultiBalanceReport, PeriodicReportRow, balance_report,
    balance_report_styles, multi_balance_report,
};
use crate::text::{Wide, format_text};
use crate::valuation::{ConversionOp, ValuationType};
use crate::write::csv::{print_csv, print_tsv};
use crate::write::fods::{locale_encoding_name, print_fods};
use crate::write::html::titled_table_html;
use crate::write::json::to_json_text;
use crate::write::spreadsheet::{
    self as spr, Border, CellType, Emphasis, Lines, Style, add_header_borders, add_row_span_header,
    cell_from_amount, cell_from_mixed_amount, cells_from_mixed_amount, horizontal_span,
    raw_table_content,
};
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
    doc: crate::command_doc!("Balance"),
    ..Mode::DEFAULT
};

/// The --chart flag (chartFlag).
pub const CHART_FLAG: Flag = Flag::new(
    &["chart"],
    OPT,
    "SCALE",
    "show a chart: instead of the table (txt), above it (html), or as data (csv, tsv, json). SCALE: the amount that fills the chart (eg 1000 or 1000 USD; one per commodity), for comparable charts.",
)
.default("auto")
.check(check_chart_arg);

/// The --width flag of commands that can show a chart (chartWidthFlag).
pub const CHART_WIDTH_FLAG: Flag = Flag::new(
    &["width"],
    REQ,
    "N",
    "with --chart: draw the chart N characters wide (default: the terminal width)",
)
.short(&['w'])
.check(check_chart_width);

/// Run the balance command (balance): a budget report with --budget, a
/// multi-period report with a report interval, otherwise a single-column
/// one.
pub fn balance(opts: &CliOpts, j: &Journal) -> Result<String, String> {
    let fmt = opts.output_format();
    let ropts0 = &opts.spec.opts;
    let budget = ropts0.balance_calc == BalanceCalculation::Budget;
    if chart_requested(opts) {
        if budget {
            return Err("--chart does not yet support --budget".to_string());
        }
        return balance_chart(opts, j);
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
    // json shows every value of the report
    let spec = ReportSpec {
        opts: ReportOpts {
            all_values_shown: fmt == "json",
            ..ropts0.clone()
        },
        ..opts.spec.clone()
    };
    let known = matches!(
        fmt.as_str(),
        "txt" | "csv" | "tsv" | "json" | "html" | "fods"
    );
    let delimited_text = |records: Vec<Vec<String>>| {
        if fmt == "csv" {
            print_csv(&records)
        } else {
            print_tsv(&records)
        }
    };
    let fods = |name: &str, sheet: spr::Sheet| {
        print_fods(
            locale_encoding_name(),
            &BTreeMap::from([(ropts.translations.tr(name).to_string(), sheet)]),
        )
    };
    let one_line = AmountFormat::ONE_LINE_NO_COST;
    if budget {
        warn_if_large_multi_period_report(opts, j);
        if !known {
            return Err(unsupported_output_format_error(&fmt));
        }
        let (span, _) = spec.report_span(j);
        let mut report = budget_report(&spec, &opts.input.balancing, span, j)?;
        report.style_amounts(&styles);
        match fmt.as_str() {
            "txt" => budget_report_as_text(&ropts, layout, &report),
            "json" => Ok(to_json_text(&report) + "\n"),
            "html" => {
                let (_, rows) = budget_report_as_spreadsheet(&one_line, &ropts, layout, &report)?;
                let title = budget_report_title(&ropts, &report);
                Ok(titled_table_html(&title, &rows) + "\n")
            }
            "fods" => {
                let sheet = budget_report_as_spreadsheet(&one_line, &ropts, layout, &report)?;
                Ok(fods("Budget Report", sheet))
            }
            _ => {
                let fmt = AmountFormat::MACHINE;
                let (_, rows) = budget_report_as_spreadsheet(&fmt, &ropts, layout, &report)?;
                Ok(delimited_text(raw_table_content(rows)))
            }
        }
    } else if multiperiod {
        warn_if_large_multi_period_report(opts, j);
        if !known {
            return Err(unsupported_output_format_error(&fmt));
        }
        let mut report = multi_balance_report(&spec, j)?;
        report.style_amounts(&styles);
        Ok(match fmt.as_str() {
            "txt" => multi_balance_report_as_text(&ropts, layout, &report),
            "json" => to_json_text(&report) + "\n",
            "csv" | "tsv" => delimited_text(multi_balance_report_as_csv(&ropts, layout, &report)),
            "html" => {
                let (_, rows) = multi_balance_report_as_spreadsheet(&ropts, layout, &report);
                let title = multi_balance_report_title(&ropts, &report);
                titled_table_html(&title, &rows) + "\n"
            }
            _ => fods(
                "Multi-period Balance Report",
                multi_balance_report_as_spreadsheet(&ropts, layout, &report),
            ),
        })
    } else {
        if !known {
            return Err(unsupported_output_format_error(&fmt));
        }
        let mut report = balance_report(&spec, j)?;
        report.style_amounts(&styles);
        Ok(match fmt.as_str() {
            "txt" => with_title(opts, balance_report_as_text(&ropts, layout, &report)?),
            "json" => to_json_text(&report) + "\n",
            "csv" | "tsv" => {
                let fmt = AmountFormat::MACHINE;
                delimited_text(raw_table_content(balance_report_as_spreadsheet(
                    &fmt, &ropts, layout, &report,
                )))
            }
            "html" => {
                // this report has no default title, so one is shown only with --title
                let rows = balance_report_as_spreadsheet(&one_line, &ropts, layout, &report);
                titled_table_html(ropts.effective_title(""), &rows) + "\n"
            }
            _ => fods(
                "Balance Report",
                (
                    (1, 0),
                    balance_report_as_spreadsheet(&one_line, &ropts, layout, &report),
                ),
            ),
        })
    }
}

/// The balance command's chart (balanceChart): of the multi-period report
/// with a report interval, otherwise of the single-column one; with html,
/// above the report's html.
fn balance_chart(opts: &CliOpts, j: &Journal) -> Result<String, String> {
    let ropts = &opts.spec.opts;
    let multiperiod = ropts.interval != Interval::NoInterval;
    if multiperiod {
        warn_if_large_multi_period_report(opts, j);
    }
    check_chart_format(opts)?;
    // the layout is needed only for the html
    let layout = || ropts.layout.clone();
    let styles = balance_report_styles(ropts, j);
    let account_type = |a: &str| j.account_type(a);
    // a chart shows the period values, which --percent must calculate
    let spec = ReportSpec {
        opts: ReportOpts {
            summary_only: false,
            ..ropts.clone()
        },
        ..opts.spec.clone()
    };
    let one_line = AmountFormat::ONE_LINE_NO_COST;
    if multiperiod {
        let mut report = multi_balance_report(&spec, j)?;
        report.style_amounts(&styles);
        let title = multi_balance_report_title(ropts, &report);
        let chart = multi_balance_report_as_chart(&account_type, ropts, &title, &report);
        render_chart(opts, chart, || {
            let (_, rows) = multi_balance_report_as_spreadsheet(ropts, layout()?, &report);
            Ok(titled_table_html(&title, &rows) + "\n")
        })
    } else {
        let mut report = balance_report(&spec, j)?;
        report.style_amounts(&styles);
        let title = ropts.effective_title("");
        let chart = balance_report_as_chart(&account_type, ropts, title, &report);
        render_chart(opts, chart, || {
            let rows = balance_report_as_spreadsheet(&one_line, ropts, layout()?, &report);
            Ok(titled_table_html(title, &rows) + "\n")
        })
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
    let t = &ropts.translations;
    let default = t.trf(
        "{report} in {dates}{valuation}:",
        &[
            ("report", t.tr(what)),
            ("dates", &report.span().to_string()),
            (
                "valuation",
                &valuation_description(ropts, changing_valuation),
            ),
        ],
    );
    ropts.effective_title(&default).to_string()
}

/// How a report title says amounts are converted or valued: nothing for
/// end values when the report calculates changing values, which are
/// period-end values anyway (multiBalanceReportTitle's valuationdesc).
pub fn valuation_description(ropts: &ReportOpts, changing_valuation: bool) -> String {
    let t = &ropts.translations;
    let cost = match ropts.conversion_op {
        Some(ConversionOp::ToCost) => t.tr(", converted to cost"),
        _ => "",
    };
    let value = match &ropts.value {
        Some(ValuationType::AtThen(_)) => t.tr(", valued at posting date").to_string(),
        Some(ValuationType::AtEnd(_)) if changing_valuation => String::new(),
        Some(ValuationType::AtEnd(_)) => t.tr(", valued at period ends").to_string(),
        Some(ValuationType::AtNow(_)) => t.tr(", current value").to_string(),
        Some(ValuationType::AtDate(d, _)) => {
            t.trf(", valued at {date}", &[("date", &d.to_string())])
        }
        None => String::new(),
    };
    format!("{cost}{value}")
}

/// A multi-period balance report as a table of text, the bare-wide
/// layout's commodity columns being its own (multiBalanceReportAsTable).
pub fn multi_balance_report_as_table(
    ropts: &ReportOpts,
    layout: Layout,
    report: &MultiBalanceReport,
) -> Table<String, String, Wide> {
    let commodities = all_commodities_from_periodic_report(&report.rows);
    multi_balance_report_as_part_table(ropts, layout, &commodities, report)
}

/// The commodities of these rows' period amounts, sorted: the bare-wide
/// layout's commodity columns (allCommoditiesFromPeriodicReport).
pub fn all_commodities_from_periodic_report<N>(
    rows: &[PeriodicReportRow<N, MixedAmount>],
) -> Vec<Sym> {
    (rows.iter())
        .flat_map(|r| r.amounts.iter().flat_map(MixedAmount::commodities))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// A multi-period balance report as a table of text, with these commodity
/// columns in the bare-wide layout: an account name per row (repeated for
/// each line of its amounts), the period names (and Total and Average) as
/// column headings, the totals row below a line unless --no-total;
/// transposed with --transpose, keeping just the leaf accounts in tree
/// mode (multiBalanceReportAsPartTable).
pub fn multi_balance_report_as_part_table(
    ropts: &ReportOpts,
    layout: Layout,
    commodities: &[Sym],
    report: &MultiBalanceReport,
) -> Table<String, String, Wide> {
    let mut span_names: Vec<String> = Vec::new();
    if !ropts.summary_only {
        span_names
            .extend((report.dates.iter()).map(|s| report_period_name(ropts, &report.dates, s)));
    }
    if ropts.has_totals_column() {
        span_names.push(format!(
            "  {}",
            ropts.translations.trc("column heading", "Total")
        ));
    }
    if ropts.average {
        span_names.push(
            ropts
                .translations
                .trc("column heading", "Average")
                .to_string(),
        );
    }
    let mut col_headings: Vec<String> = Vec::new();
    if layout == Layout::Bare {
        col_headings.push(
            ropts
                .translations
                .trc("column heading", "Commodity")
                .to_string(),
        );
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
        let lines = multi_balance_row_as_text(ropts, layout, commodities, row);
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
        let totals = multi_balance_row_as_text(ropts, layout, commodities, &report.totals);
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
/// layout says; a tidy layout has none in text (multiBalanceRowAsText).
pub fn multi_balance_row_as_text<N>(
    ropts: &ReportOpts,
    layout: Layout,
    all_commodities: &[Sym],
    row: &PeriodicReportRow<N, MixedAmount>,
) -> Vec<Vec<Wide>> {
    let fmt = AmountFormat {
        colour: ropts.color,
        ..AmountFormat::ONE_LINE_NO_COST
    };
    let date_span_cell =
        |s: &DateSpan| spr::Cell::new(render_period_heading(ropts.period_titles, s));
    let rows = multi_balance_row_as_cell_builders(
        &fmt,
        ropts,
        layout,
        &[],
        all_commodities,
        RowClass::Value,
        &date_span_cell,
        row,
    );
    raw_table_content(rows)
}

/// A multi-period report row's cells as lines, laid out as the layout
/// says (multiBalanceRowAsCellBuilders): the amounts on one line each
/// (wide), one commodity per line (tall), as bare numbers after a
/// commodity column (bare), a column per commodity of the report
/// (barewide), or a line per period and commodity, with the period's
/// name, first and last dates and the commodity (tidy, which has no total
/// or average). The periods' amounts come first unless --summary-only,
/// then the row's total and average if wanted. `colspans` are the
/// report's periods (none for text); `date_span_cell` shows one.
#[allow(clippy::too_many_arguments)]
pub fn multi_balance_row_as_cell_builders<N>(
    fmt: &AmountFormat,
    ropts: &ReportOpts,
    layout: Layout,
    colspans: &[DateSpan],
    all_commodities: &[Sym],
    rc: RowClass,
    date_span_cell: &dyn Fn(&DateSpan) -> spr::Cell<String>,
    row: &PeriodicReportRow<N, MixedAmount>,
) -> Vec<Vec<spr::Cell<Wide>>> {
    let classified: Vec<(&str, &MixedAmount)> =
        row.amounts.iter().map(|m| (amount_class(rc), m)).collect();
    // each figure with the span it covers: a column's, the whole report's
    // for a row total, none for an average (or for text, which has none)
    let mut spanclsamts: Vec<(Option<DateSpan>, (&str, &MixedAmount))> = Vec::new();
    if !ropts.summary_only {
        let spans = colspans
            .iter()
            .map(|s| Some(*s))
            .chain(std::iter::repeat(None));
        spanclsamts.extend(spans.zip(classified.iter().copied()));
    }
    if ropts.has_totals_column() && !row.amounts.is_empty() {
        let span = (!colspans.is_empty()).then(|| spans_span(colspans));
        spanclsamts.push((span, (row_total_class(rc), &row.total)));
    }
    if ropts.average && !row.amounts.is_empty() {
        spanclsamts.push((None, (row_average_class(rc), &row.average)));
    }
    let clsamts: Vec<(&str, &MixedAmount)> = spanclsamts.iter().map(|(_, c)| *c).collect();
    // in the wide layout a figure links to the register it is derived from,
    // through its row's cell for the span it covers, unless it looks zero
    // or is not a sum of postings; a totals row's span cell has its title
    let link_figure = |span: &Option<DateSpan>, m: &MixedAmount, c: spr::Cell<Wide>| match span {
        Some(span) if figures_link(ropts) && !m.looks_zero() => {
            let ds = date_span_cell(span);
            let title = match rc {
                RowClass::Total => ds.title,
                RowClass::Value => amount_phrase(&report_link_opts(ropts, colspans), false),
            };
            with_link(ds.anchor, &title, c)
        }
        _ => c,
    };
    let wb_cell = |s: &str| spr::Cell::new(Wide::new(s));
    let cs: Vec<Sym> = if clsamts.iter().all(|(_, m)| m.looks_zero()) {
        vec![Sym::from("")]
    } else {
        (clsamts.iter())
            .flat_map(|(_, m)| m.commodities())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    };
    match layout {
        Layout::Wide(width) => {
            let fmt = AmountFormat {
                max_width: width.map(|w| w as isize),
                ..fmt.clone()
            };
            vec![
                (spanclsamts.iter())
                    .map(|(span, (cls, m))| {
                        link_figure(span, m, cell_from_mixed_amount(&fmt, cls, m))
                    })
                    .collect(),
            ]
        }
        Layout::Tall => {
            let fmt = AmountFormat {
                max_width: None,
                ..fmt.clone()
            };
            let columns: Vec<Vec<spr::Cell<Wide>>> = (clsamts.iter())
                .map(|(cls, m)| cells_from_mixed_amount(&fmt, cls, m))
                .collect();
            if columns.is_empty() {
                return vec![vec![]];
            }
            let n = columns.iter().map(Vec::len).max().unwrap_or(0);
            (0..n)
                .map(|i| {
                    (columns.iter())
                        .map(|c| c.get(i).cloned().unwrap_or_else(spr::Cell::empty))
                        .collect()
                })
                .collect()
        }
        Layout::Bare => {
            let fmt = set_display_commodity_bare(&cs, fmt);
            let columns: Vec<Vec<spr::Cell<Wide>>> = (clsamts.iter())
                .map(|(cls, m)| cells_from_mixed_amount(&fmt, cls, m))
                .collect();
            (cs.iter().zip(transpose(&columns)))
                .map(|(c, mut line)| {
                    line.insert(0, wb_cell(c));
                    line
                })
                .collect()
        }
        Layout::BareWide => {
            let fmt = set_display_commodity_bare(all_commodities, fmt);
            vec![
                (clsamts.iter())
                    .flat_map(|(cls, m)| cells_from_mixed_amount(&fmt, cls, m))
                    .collect(),
            ]
        }
        Layout::Tidy => {
            let fmt = set_display_commodity_bare(&cs, fmt);
            let date_cell = |d: Option<Day>| spr::Cell {
                typ: CellType::Date,
                ..wb_cell(&d.map_or_else(String::new, |d| d.to_string()))
            };
            (colspans.iter().zip(&classified))
                .flat_map(|(span, (cls, m))| {
                    (cs.iter().zip(cells_from_mixed_amount(&fmt, cls, m)))
                        .map(|(c, a)| {
                            vec![
                                date_span_cell(span).map(|s| Wide::new(&s)),
                                date_cell(span.start()),
                                date_cell(span.end().map(prevday)),
                                wb_cell(c),
                                a,
                            ]
                        })
                        .collect::<Vec<_>>()
                })
                .collect()
        }
    }
}

/// An amount format for the bare layouts, which show commodity symbols in
/// their own column(s): no symbols, the amounts in this commodity order,
/// unpadded (setDisplayCommodityBare). The order must include every
/// commodity to be shown: an amount in another is dropped.
fn set_display_commodity_bare(cs: &[Sym], fmt: &AmountFormat) -> AmountFormat {
    AmountFormat {
        commodity: false,
        commodity_order: Some(cs.to_vec()),
        min_width: None,
        ..fmt.clone()
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
                    let months = ropts.translations.month_names();
                    let abbrevs: Vec<&str> = months.iter().map(|(_, a)| a.as_str()).collect();
                    span.show_abbrev_with(&abbrevs)
                }
            }
        },
        _ => span
            .end()
            .map_or_else(String::new, |e| prevday(e).to_string()),
    }
}

// Spreadsheet output (csv, tsv, html, fods)

/// Whether a row has an account's figures or the totals (RowClass).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowClass {
    Value,
    Total,
}

/// The class of an account cell (accountClass).
pub const ACCOUNT_CLASS: &str = "account";

/// The heading of the totals row in spreadsheets
/// (totalRowHeadingSpreadsheet, totalRowHeadingBudgetCsv).
pub const TOTAL_ROW_HEADING_SPREADSHEET: &str = "Total:";

/// A no-break space, which indents account names in spreadsheets (nbsp).
pub const NBSP: &str = "\u{a0}";

/// The class of an account cell in a tree-mode report: "account", its
/// row's indent, and the account's depth (its name's level, deeper than
/// the indent when boring parents are squashed into its row), so that a
/// UI can fold the tree; in list mode just "account" (treeAccountClass).
pub fn tree_account_class(ropts: &ReportOpts, indent: usize, account: &str) -> String {
    if ropts.account_list_mode == AccountListMode::Tree {
        format!(
            "account indent-{indent} depth-{}",
            account_name_level(account)
        )
    } else {
        ACCOUNT_CLASS.to_string()
    }
}

/// Whether a report's figures link to registers: only sums of postings,
/// which is what a register shows (figuresLink).
fn figures_link(ropts: &ReportOpts) -> bool {
    ropts.balance_calc == BalanceCalculation::Change && !ropts.percent
}

/// The register link options of a report's rows: its accumulation, the
/// span its columns cover, whether it shows figures with the sign opposite
/// to the register's (as the compound reports' negated sections do), and
/// how it converts amounts (reportLinkOpts).
pub fn report_link_opts(ropts: &ReportOpts, colspans: &[DateSpan]) -> LinkOpts {
    LinkOpts {
        accum: ropts.balance_accum,
        span: spans_span(colspans),
        date2: ropts.date2,
        includes_subs: true,
        negated: ropts.normal_balance == Some(NormalSign::NormallyNegative),
        value: value_param(ropts.conversion_op, ropts.value.as_ref()),
    }
}

/// The link options of an account's row: whether its figures include the
/// subaccounts, which in tree mode they always do, and in list mode unless
/// some subaccount has a row of its own (rowLinkOpts).
fn row_link_opts(
    ropts: &ReportOpts,
    colspans: &[DateSpan],
    shown_parents: &BTreeSet<String>,
    account: &str,
) -> LinkOpts {
    LinkOpts {
        includes_subs: ropts.account_list_mode == AccountListMode::Tree
            || !shown_parents.contains(account),
        ..report_link_opts(ropts, colspans)
    }
}

/// The accounts that have a subaccount among these (shownParentsOf).
fn shown_parents_of<'a>(names: impl Iterator<Item = &'a str>) -> BTreeSet<String> {
    names
        .flat_map(|n| {
            parent_account_names(n)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The class of a figure (amountClass).
fn amount_class(rc: RowClass) -> &'static str {
    match rc {
        RowClass::Value => "amount",
        RowClass::Total => "amount coltotal",
    }
}

/// The class of a row's total (rowTotalClass).
fn row_total_class(rc: RowClass) -> &'static str {
    match rc {
        RowClass::Value => "amount rowtotal",
        RowClass::Total => "amount coltotal",
    }
}

/// The class of a row's average (rowAverageClass).
fn row_average_class(rc: RowClass) -> &'static str {
    match rc {
        RowClass::Value => "amount rowaverage",
        RowClass::Total => "amount colaverage",
    }
}

/// The class of a budget goal (budgetClass).
fn budget_class(rc: RowClass) -> &'static str {
    match rc {
        RowClass::Value => "budget",
        RowClass::Total => "budget coltotal",
    }
}

/// The class of a row's total goal (budgetTotalClass).
fn budget_total_class(rc: RowClass) -> &'static str {
    match rc {
        RowClass::Value => "budget rowtotal",
        RowClass::Total => "budget coltotal",
    }
}

/// The class of a row's average goal (budgetAverageClass).
fn budget_average_class(rc: RowClass) -> &'static str {
    match rc {
        RowClass::Value => "budget rowaverage",
        RowClass::Total => "budget colaverage",
    }
}

/// Totals rows: in the totals style, a double line above the first
/// (addTotalBorders).
pub fn add_total_borders<T>(rows: Vec<Vec<spr::Cell<T>>>) -> Vec<Vec<spr::Cell<T>>> {
    (rows.into_iter().enumerate())
        .map(|(i, row)| {
            let top = if i == 0 {
                Lines::DoubleLine
            } else {
                Lines::NoLine
            };
            (row.into_iter())
                .map(|c| spr::Cell {
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

/// A single-column balance report as a spreadsheet: the heading row, a
/// row per account (several in the bare layout, one per commodity), and
/// unless --no-total the totals row; transposed with --transpose
/// (balanceReportAsSpreadsheet, balanceReportAsSpreadsheetParts).
pub fn balance_report_as_spreadsheet(
    fmt: &AmountFormat,
    ropts: &ReportOpts,
    layout: Layout,
    report: &BalanceReport,
) -> Vec<Vec<spr::Cell<String>>> {
    let all_commodities: Vec<Sym> = (report.items.iter())
        .flat_map(|(_, m)| m.commodities())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let h_cell = |cls: &str, label: &str| spr::Cell::header(label).with_class(cls);
    let mut headers = vec![h_cell(ACCOUNT_CLASS, "account")];
    match layout {
        Layout::BareWide => headers.extend(all_commodities.iter().map(|c| h_cell("amount", c))),
        Layout::Bare => {
            headers.push(spr::Cell::header("commodity"));
            headers.push(h_cell("amount", "balance"));
        }
        _ => headers.push(h_cell("amount", "balance")),
    }
    let render_amount = |rc: RowClass, m: &MixedAmount| {
        let fmt = if layout == Layout::Bare {
            AmountFormat {
                commodity: false,
                commodity_order: Some(m.commodities().into_iter().collect()),
                ..fmt.clone()
            }
        } else {
            fmt.clone()
        };
        cell_from_mixed_amount(&fmt, amount_class(rc), m).into_text()
    };
    let base = ropts.balance_base_url.as_deref();
    let shown_parents = shown_parents_of(report.items.iter().map(|(n, _)| &*n.full));
    // the report's period, from -b/-e/-p (a date: query term is in the
    // query string already, and the report intersected the two)
    let period_terms = if ropts.period == Period::PeriodAll {
        Vec::new()
    } else {
        date_term(ropts.date2, &ropts.period.as_date_span())
    };
    let link_query = [period_terms.clone(), ropts.query_string.clone()].concat();
    let rows = |rc: RowClass, name: &DisplayName, m: &MixedAmount| -> Vec<Vec<spr::Cell<String>>> {
        let lo = row_link_opts(ropts, &[], &shown_parents, &name.full);
        let account_cell = match rc {
            // the total row's heading as it is: --drop etc. don't apply
            RowClass::Total => spr::Cell::new(name.name.to_string()).with_class(ACCOUNT_CLASS),
            RowClass::Value => spr::Cell::new(render_balance_acct(ropts, NBSP, name))
                .with_class(&tree_account_class(ropts, name.indent, &name.full)),
        };
        let account_base = if rc == RowClass::Value { base } else { None };
        let account_cell =
            set_account_anchor_with(&lo, account_base, &link_query, &name.full, account_cell);
        // the figure links where the account name does, or in the total row
        // to the register of everything in the report; not if it looks
        // zero, which has an empty register
        let link_figure = |c: spr::Cell<String>| {
            if !figures_link(ropts) || m.looks_zero() {
                c
            } else if rc == RowClass::Value {
                with_link(account_cell.anchor.clone(), &amount_phrase(&lo, false), c)
            } else {
                let q = [period_terms.clone(), remove_inacct(&ropts.query_string)].concat();
                let anchor = compose_anchor_with(&link_params(&lo), base, &q);
                with_link(anchor, &amount_phrase(&lo, true), c)
            }
        };
        let lines = match layout {
            Layout::BareWide => {
                let fmt = AmountFormat {
                    commodity: false,
                    commodity_order: Some(all_commodities.clone()),
                    ..AmountFormat::MACHINE
                };
                vec![
                    (show_mixed_amount_lines_parts_b(&fmt, m).into_iter())
                        .map(|(text, a)| cell_from_amount(&fmt, amount_class(rc), text.s, a))
                        .collect(),
                ]
            }
            Layout::Bare => (m.clone().strip_costs().amounts().into_iter())
                .map(|a| {
                    vec![
                        spr::Cell::new(a.commodity.to_string()),
                        render_amount(rc, &MixedAmount::from_amount(a)),
                    ]
                })
                .collect(),
            _ => vec![vec![link_figure(render_amount(rc, m))]],
        };
        add_row_span_header(&[account_cell], lines)
    };
    let mut table = vec![add_header_borders(headers)];
    for (name, m) in &report.items {
        table.extend(rows(RowClass::Value, name, m));
    }
    if !ropts.no_total {
        let total_name = DisplayName::flat(TOTAL_ROW_HEADING_SPREADSHEET);
        table.extend(add_total_borders(rows(
            RowClass::Total,
            &total_name,
            &report.total,
        )));
    }
    if ropts.transpose {
        table = spr::transpose(table);
    }
    table
}

/// A multi-period balance report's spreadsheet rows: the heading rows
/// (two in the bare-wide layout, the periods above the commodities), the
/// account rows, and unless --no-total the totals rows; with these
/// commodity columns in the bare-wide layout
/// (balanceSubReportAsSpreadsheetParts).
pub fn balance_sub_report_as_spreadsheet_parts(
    fmt: &AmountFormat,
    ropts: &ReportOpts,
    layout: Layout,
    all_commodities: &[Sym],
    report: &MultiBalanceReport,
) -> SpreadsheetParts {
    let colspans = &report.dates;
    let base = ropts.balance_base_url.as_deref();
    let query = &ropts.query_string;
    let h_cell = |cls: &str, label: &str| spr::Cell::header(label).with_class(cls);
    // the headings over figures are marked as such, so that a stylesheet
    // can align them with the figures
    let mut date_headers: Vec<spr::Cell<String>> = Vec::new();
    if !ropts.summary_only {
        date_headers.extend(colspans.iter().map(|s| {
            header_date_span_cell(ropts.date2, ropts.period_titles, base, query, s)
                .with_class(amount_class(RowClass::Value))
        }));
    }
    if ropts.has_totals_column() {
        date_headers.push(h_cell(row_total_class(RowClass::Value), "total"));
    }
    if ropts.average {
        date_headers.push(h_cell(row_average_class(RowClass::Value), "average"));
    }
    let mut headers = vec![h_cell(ACCOUNT_CLASS, "account")];
    match layout {
        Layout::Tidy => headers.extend(TIDY_COLUMN_LABELS.iter().map(|l| spr::Cell::header(l))),
        Layout::BareWide => {
            for _ in &date_headers {
                headers.extend(all_commodities.iter().map(|c| spr::Cell::header(c)));
            }
        }
        Layout::Bare => {
            headers.push(spr::Cell::header("commodity"));
            headers.extend(date_headers.iter().cloned());
        }
        _ => headers.extend(date_headers.iter().cloned()),
    }
    let headers = add_header_borders(headers);
    let all_headers = match layout {
        Layout::BareWide => {
            let mut top = vec![spr::Cell::empty()];
            for h in date_headers {
                top.extend(horizontal_span(all_commodities.len(), h));
            }
            let top = (top.into_iter())
                .map(|c| spr::Cell {
                    border: Border::default(),
                    ..c
                })
                .collect();
            vec![top, headers]
        }
        _ => vec![headers],
    };
    let shown_parents = shown_parents_of(report.rows.iter().map(|r| &*r.name.full));
    // an account's link covers the span its row sums, the columns', which
    // can be wider than the span asked for; when none was asked for, the
    // register is unrestricted, as the report is
    let requested_span = ropts.period != Period::PeriodAll
        || (query.iter()).any(|t| t.starts_with("date:") || t.starts_with("date2:"));
    let mut row_query = Vec::new();
    if requested_span {
        row_query.extend(date_term(ropts.date2, &spans_span(colspans)));
    }
    row_query.extend(remove_dates(query));
    let mut body = Vec::new();
    for row in &report.rows {
        let account = &*row.name.full;
        let lo = row_link_opts(ropts, colspans, &shown_parents, account);
        let account_cell = spr::Cell::new(render_balance_acct(ropts, NBSP, &row.name))
            .with_class(&tree_account_class(ropts, row.name.indent, account));
        let account_cell = set_account_anchor_with(&lo, base, &row_query, account, account_cell);
        let date_span_cell =
            |s: &DateSpan| date_span_cell_with(&lo, ropts.period_titles, base, query, account, s);
        let lines = multi_balance_row_as_cell_builders(
            fmt,
            ropts,
            layout,
            colspans,
            all_commodities,
            RowClass::Value,
            &date_span_cell,
            row,
        );
        let lines = (lines.into_iter())
            .map(|l| l.into_iter().map(spr::Cell::into_text).collect())
            .collect();
        body.extend(add_row_span_header(&[account_cell], lines));
    }
    let totals = if ropts.no_total {
        Vec::new()
    } else {
        multi_balance_totals_rows(
            fmt,
            ropts,
            layout,
            &report_link_opts(ropts, colspans),
            &[],
            all_commodities,
            colspans,
            TOTAL_ROW_HEADING_SPREADSHEET,
            &report.totals,
        )
    };
    (all_headers, body, totals)
}

/// A spreadsheet's heading rows, body rows and totals rows.
pub type SpreadsheetParts = (spr::Rows, spr::Rows, spr::Rows);

/// The totals row of a multi-period report as spreadsheet rows: the label,
/// then a total per column, each linking to the register of everything in
/// the report's query plus these terms, for that column
/// (multiBalanceTotalsRows).
#[allow(clippy::too_many_arguments)]
pub fn multi_balance_totals_rows<N>(
    fmt: &AmountFormat,
    ropts: &ReportOpts,
    layout: Layout,
    lo: &LinkOpts,
    extra_query: &[String],
    all_commodities: &[Sym],
    colspans: &[DateSpan],
    label: &str,
    row: &PeriodicReportRow<N, MixedAmount>,
) -> Vec<Vec<spr::Cell<String>>> {
    let base = ropts.balance_base_url.as_deref();
    let date_span_cell = |s: &DateSpan| {
        total_date_span_cell(
            lo,
            ropts.period_titles,
            base,
            extra_query,
            &ropts.query_string,
            s,
        )
    };
    let lines = multi_balance_row_as_cell_builders(
        fmt,
        ropts,
        layout,
        colspans,
        all_commodities,
        RowClass::Total,
        &date_span_cell,
        row,
    );
    let lines = (lines.into_iter())
        .map(|l| l.into_iter().map(spr::Cell::into_text).collect())
        .collect();
    let label = spr::Cell::new(label.to_string()).with_class(ACCOUNT_CLASS);
    add_total_borders(add_row_span_header(&[label], lines))
}

/// The tidy layout's column headings after the account's
/// (tidyColumnLabels).
pub const TIDY_COLUMN_LABELS: [&str; 5] =
    ["period", "start_date", "end_date", "commodity", "value"];

/// The number of heading columns of a multi-period report's spreadsheet
/// (multiBalanceReportNumHeaderColumns).
pub fn multi_balance_report_num_header_columns(layout: Layout) -> usize {
    match layout {
        Layout::Bare => 2,
        Layout::Tidy => 0,
        _ => 1,
    }
}

/// A multi-period balance report as CSV records: the heading rows, the
/// account rows and unless --no-total the totals rows, transposed with
/// --transpose (multiBalanceReportAsCsv).
pub fn multi_balance_report_as_csv(
    ropts: &ReportOpts,
    layout: Layout,
    report: &MultiBalanceReport,
) -> Vec<Vec<String>> {
    let commodities = all_commodities_from_periodic_report(&report.rows);
    let (mut rows, body, totals) = balance_sub_report_as_spreadsheet_parts(
        &AmountFormat::MACHINE,
        ropts,
        layout,
        &commodities,
        report,
    );
    rows.extend(body);
    rows.extend(totals);
    let records = raw_table_content(rows);
    if ropts.transpose {
        transpose(&records)
    } else {
        records
    }
}

/// A multi-period balance report as a spreadsheet, with the numbers of its
/// heading rows and columns, transposed with --transpose
/// (multiBalanceReportAsSpreadsheet).
pub fn multi_balance_report_as_spreadsheet(
    ropts: &ReportOpts,
    layout: Layout,
    report: &MultiBalanceReport,
) -> spr::Sheet {
    let commodities = all_commodities_from_periodic_report(&report.rows);
    let (mut rows, body, totals) = balance_sub_report_as_spreadsheet_parts(
        &AmountFormat::ONE_LINE_NO_COST,
        ropts,
        layout,
        &commodities,
        report,
    );
    let fixed = (rows.len(), multi_balance_report_num_header_columns(layout));
    rows.extend(body);
    rows.extend(totals);
    if ropts.transpose {
        ((fixed.1, fixed.0), spr::transpose(rows))
    } else {
        (fixed, rows)
    }
}

/// A budget report as a spreadsheet, with the numbers of its heading rows
/// and heading columns (budgetReportAsSpreadsheet): each period's actual
/// amounts and goals in alternating columns, then the row totals' and
/// averages' if wanted; the wide, bare and bare-wide layouts only;
/// transposed with --transpose.
pub fn budget_report_as_spreadsheet(
    fmt: &AmountFormat,
    ropts: &ReportOpts,
    layout: Layout,
    report: &BudgetReport,
) -> Result<spr::Sheet, String> {
    match layout {
        Layout::Wide(_) | Layout::Bare | Layout::BareWide => {}
        Layout::Tall => return Err("LayoutTall not supported for the chosen output format.".into()),
        Layout::Tidy => return Err("LayoutTidy not supported for the chosen output format.".into()),
    }
    let mut leading_headers = vec!["Account".to_string()];
    if layout == Layout::Bare {
        leading_headers.push("Commodity".to_string());
    }
    let mut date_headers: Vec<String> = Vec::new();
    if !ropts.summary_only {
        for s in &report.dates {
            date_headers.push(render_period_heading(ropts.period_titles, s));
            date_headers.push("budget".to_string());
        }
    }
    if ropts.row_total {
        date_headers.extend(["Total".to_string(), "budget".to_string()]);
    }
    if ropts.average {
        date_headers.extend(["Average".to_string(), "budget".to_string()]);
    }
    let all_commodities: Vec<Sym> = (report.rows.iter())
        .flat_map(|r| r.amounts.iter())
        .flat_map(|(actual, goal)| actual.iter().chain(goal))
        .flat_map(MixedAmount::commodities)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let headers = |hs: Vec<String>| hs.iter().map(|h| spr::Cell::header(h)).collect::<Vec<_>>();
    let all_headers = match layout {
        Layout::BareWide => {
            let mut top = vec![spr::Cell::empty()];
            for h in &date_headers {
                top.extend(horizontal_span(all_commodities.len(), spr::Cell::header(h)));
            }
            let mut second = leading_headers.clone();
            for _ in &date_headers {
                second.extend(all_commodities.iter().map(|c| c.to_string()));
            }
            vec![top, add_header_borders(headers(second))]
        }
        _ => {
            let mut hs = leading_headers.clone();
            hs.extend(date_headers);
            vec![add_header_borders(headers(hs))]
        }
    };
    let mut rows = all_headers;
    let fixed = (rows.len(), leading_headers.len());
    for row in &report.rows {
        let account_cell = set_account_anchor(
            ropts.balance_base_url.as_deref(),
            &ropts.query_string,
            &row.name.full,
            spr::Cell::new(render_balance_acct(ropts, NBSP, &row.name)),
        );
        rows.extend(budget_row_as_texts(
            fmt,
            ropts,
            layout,
            &all_commodities,
            RowClass::Value,
            account_cell,
            row,
        ));
    }
    if !ropts.no_total {
        let label = spr::Cell::new(TOTAL_ROW_HEADING_SPREADSHEET.to_string());
        rows.extend(add_total_borders(budget_row_as_texts(
            fmt,
            ropts,
            layout,
            &all_commodities,
            RowClass::Total,
            label,
            &report.totals,
        )));
    }
    Ok(if ropts.transpose {
        ((fixed.1, fixed.0), spr::transpose(rows))
    } else {
        (fixed, rows)
    })
}

/// A budget report row's spreadsheet rows: the account cell, then the
/// actual amounts and goals, as the layout says (budgetReportAsSpreadsheet's
/// rowAsTexts).
fn budget_row_as_texts<N>(
    fmt: &AmountFormat,
    ropts: &ReportOpts,
    layout: Layout,
    all_commodities: &[Sym],
    rc: RowClass,
    account_cell: spr::Cell<String>,
    row: &PeriodicReportRow<N, BudgetCell>,
) -> Vec<Vec<spr::Cell<String>>> {
    let mut vals: Vec<(&str, Option<&MixedAmount>)> = Vec::new();
    if !ropts.summary_only {
        for (actual, goal) in &row.amounts {
            vals.push((amount_class(rc), actual.as_ref()));
            vals.push((budget_class(rc), goal.as_ref()));
        }
    }
    if ropts.row_total {
        vals.push((row_total_class(rc), row.total.0.as_ref()));
        vals.push((budget_total_class(rc), row.total.1.as_ref()));
    }
    if ropts.average {
        vals.push((row_average_class(rc), row.average.0.as_ref()));
        vals.push((budget_average_class(rc), row.average.1.as_ref()));
    }
    let null = MixedAmount::null();
    let bare_cells = |cs: &[Sym]| -> Vec<Vec<spr::Cell<String>>> {
        let fmt = set_display_commodity_bare(cs, fmt);
        (vals.iter())
            .map(|(cls, m)| {
                (cells_from_mixed_amount(&fmt, cls, m.unwrap_or(&null)).into_iter())
                    .map(spr::Cell::into_text)
                    .collect()
            })
            .collect()
    };
    let lines: Vec<Vec<spr::Cell<String>>> = match layout {
        Layout::Bare => {
            let cs: Vec<Sym> = (vals.iter())
                .filter_map(|(_, m)| *m)
                .flat_map(MixedAmount::commodities)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            (cs.iter().zip(transpose(&bare_cells(&cs))))
                .map(|(c, mut line)| {
                    line.insert(0, spr::Cell::new(c.to_string()));
                    line
                })
                .collect()
        }
        Layout::BareWide => vec![bare_cells(all_commodities).concat()],
        _ => vec![
            (vals.iter())
                .map(|(cls, m)| match m {
                    None => spr::Cell::empty(),
                    Some(m) => cell_from_mixed_amount(fmt, cls, m).into_text(),
                })
                .collect(),
        ],
    };
    add_row_span_header(&[account_cell], lines)
}

// Budget reports

/// A budget cell's values shown on one line: the actual amount, and if
/// there is a goal, it and the percentage of it reached, if there is one
/// (BudgetDisplayCell).
type BudgetDisplayCell = (Wide, Option<(Wide, Option<Wide>)>);

/// A budget report as text: its title, then the table
/// (budgetReportAsText).
pub fn budget_report_as_text(
    ropts: &ReportOpts,
    layout: Layout,
    report: &BudgetReport,
) -> Result<String, String> {
    let title = budget_report_title(ropts, report);
    let mut out = if title.is_empty() {
        String::new()
    } else {
        format!("{title}\n\n")
    };
    let table = budget_report_as_table(ropts, layout, report)?;
    out.push_str(&multi_balance_report_table_as_text(ropts, layout, &table));
    Ok(out)
}

/// A budget report's heading: what it shows, over which dates, valued
/// how; or --title's (budgetReportTitle).
fn budget_report_title(ropts: &ReportOpts, report: &BudgetReport) -> String {
    let default = ropts.translations.trf(
        "Budget performance in {dates}{valuation}:",
        &[
            ("dates", &report.span().to_string()),
            ("valuation", &valuation_description(ropts, false)),
        ],
    );
    ropts.effective_title(&default).to_string()
}

/// A budget report as a table of text (budgetReportAsTable): a line for
/// each row, or with the tall and bare layouts, for each commodity of the
/// row; each cell the actual amount and, if there is a goal, `[PERCENT% of
/// GOAL]`, each part in a column as wide as the widest of its kind; the
/// totals row below a line unless --no-total; the bare layout's commodity
/// column first; transposed with --transpose. The wide, tall and bare
/// layouts only.
fn budget_report_as_table(
    ropts: &ReportOpts,
    layout: Layout,
    report: &BudgetReport,
) -> Result<Table<String, String, Wide>, String> {
    let layout_name = match layout {
        Layout::Wide(_) | Layout::Tall | Layout::Bare => None,
        Layout::BareWide => Some("LayoutBareWide"),
        Layout::Tidy => Some("LayoutTidy"),
    };
    if let Some(name) = layout_name {
        return Err(format!(
            "{name} not supported for the chosen output format."
        ));
    }
    let cells = |row| row_to_budget_cells(ropts, row);
    let total_cells = row_to_budget_cells(ropts, &report.totals);
    let show_cell = |cs: &[Sym], cell: &BudgetCell| show_budget_cell(ropts, layout, cs, cell);
    // the column widths of the actual amounts, goals and percentages, over
    // every line of every row, the totals' included; with --transpose,
    // each line's
    let widths: Vec<(usize, usize, usize)> = {
        let cell_widths = |cells: &[BudgetCell]| -> Vec<Vec<(usize, usize, usize)>> {
            let cs = budget_cells_commodities(cells);
            (cells.iter())
                .map(|c| {
                    (show_cell(&cs, c).iter())
                        .map(|(actual, goal)| {
                            let (gw, pw) = goal.as_ref().map_or((0, 0), |(g, p)| {
                                (g.width, p.as_ref().map_or(0, |p| p.width))
                            });
                            (actual.width, gw, pw)
                        })
                        .collect()
                })
                .collect()
        };
        let rows: Vec<Vec<Vec<(usize, usize, usize)>>> = (report.rows.iter())
            .map(|r| cell_widths(&cells(r)))
            .chain(std::iter::once(cell_widths(&total_cells)))
            .collect();
        let ncols = rows.iter().map(Vec::len).min().unwrap_or(0);
        let columns: Vec<Vec<(usize, usize, usize)>> = (0..ncols)
            .map(|i| rows.iter().flat_map(|r| r[i].iter().copied()).collect())
            .collect();
        let columns = if ropts.transpose {
            transpose(&columns)
        } else {
            columns
        };
        (columns.iter())
            .map(|c| {
                let max =
                    |f: fn(&(usize, usize, usize)) -> usize| c.iter().map(f).max().unwrap_or(0);
                (max(|w| w.0), max(|w| w.1), max(|w| w.2))
            })
            .collect()
    };
    // a row's lines: the commodity and the cells' values; for a row with
    // no amounts or goals, which may be needed to show the tree, one line
    // of zeros, except for the unbudgeted row (showrow)
    let show_row =
        |unbudgeted: bool, cells: &[BudgetCell]| -> Vec<(Wide, Vec<BudgetDisplayCell>)> {
            let mut cs = budget_cells_commodities(cells);
            if cs.is_empty() && !unbudgeted {
                cs.push(Sym::from(""));
            }
            let columns: Vec<Vec<BudgetDisplayCell>> =
                cells.iter().map(|c| show_cell(&cs, c)).collect();
            (cs.iter().map(|c| Wide::new(c)))
                .zip(transpose(&columns))
                .collect()
        };
    let mut accounts = Vec::new();
    let mut commodities = Vec::new();
    let mut lines = Vec::new();
    for row in &report.rows {
        let unbudgeted = &*row.name.full == UNBUDGETED_ACCOUNT_NAME;
        let name = render_balance_acct(ropts, " ", &row.name);
        for (c, line) in show_row(unbudgeted, &cells(row)) {
            accounts.push(name.clone());
            commodities.push(c);
            lines.push(line);
        }
    }
    let (total_commodities, total_lines): (Vec<Wide>, Vec<Vec<BudgetDisplayCell>>) =
        show_row(false, &total_cells).into_iter().unzip();
    let total_widths = if ropts.transpose {
        widths.get(lines.len()..).unwrap_or_default()
    } else {
        &widths[..]
    };
    // the lines' cells shown with the widths of their columns (showcells)
    let show_cells = |widths: &[(usize, usize, usize)], lines: &[Vec<BudgetDisplayCell>]| {
        let show = |cells: &[BudgetDisplayCell]| -> Vec<Wide> {
            (cells.iter().zip(widths))
                .map(|(c, w)| show_budget_display_cell(*w, c))
                .collect()
        };
        if ropts.transpose {
            let shown: Vec<Vec<Wide>> = transpose(lines).iter().map(|c| show(c)).collect();
            transpose(&shown)
        } else {
            lines.iter().map(|l| show(l)).collect()
        }
    };
    // the bare layout's commodity column
    let with_commodities = |cs: Vec<Wide>, rows: Vec<Vec<Wide>>| -> Vec<Vec<Wide>> {
        if layout == Layout::Bare {
            (cs.into_iter().zip(rows))
                .map(|(c, mut row)| {
                    row.insert(0, c);
                    row
                })
                .collect()
        } else {
            rows
        }
    };
    let rows = with_commodities(commodities, show_cells(&widths, &lines));
    let total_rows = with_commodities(total_commodities, show_cells(total_widths, &total_lines));
    let mut col_headings: Vec<String> = Vec::new();
    if layout == Layout::Bare {
        col_headings.push(
            ropts
                .translations
                .trc("column heading", "Commodity")
                .to_string(),
        );
    }
    if !ropts.summary_only {
        col_headings
            .extend((report.dates.iter()).map(|s| report_period_name(ropts, &report.dates, s)));
    }
    if ropts.row_total {
        col_headings.push(format!(
            "  {}",
            ropts.translations.trc("column heading", "Total")
        ));
    }
    if ropts.average {
        col_headings.push(
            ropts
                .translations
                .trc("column heading", "Average")
                .to_string(),
        );
    }
    let pretty_line = if ropts.pretty {
        Line::SingleLine
    } else {
        Line::NoLine
    };
    let mut table = Table {
        row_headers: Header::Group(
            Line::NoLine,
            accounts.into_iter().map(Header::Leaf).collect(),
        ),
        col_headers: Header::Group(
            pretty_line,
            col_headings.into_iter().map(Header::Leaf).collect(),
        ),
        rows,
    };
    if !ropts.no_total {
        let headers = std::iter::repeat_n(String::new(), total_rows.len().max(1));
        let totals = Table {
            row_headers: Header::Group(Line::NoLine, headers.map(Header::Leaf).collect()),
            col_headers: Header::Leaf(()),
            rows: total_rows,
        };
        table = concat_tables(Line::SingleLine, table, totals);
    }
    if ropts.transpose {
        table = Table {
            row_headers: table.col_headers,
            col_headers: table.row_headers,
            rows: transpose(&table.rows),
        };
    }
    Ok(table)
}

/// A budget report row's cells: the periods' unless --summary-only, then
/// its total and average if wanted (rowToBudgetCells).
fn row_to_budget_cells<N>(
    ropts: &ReportOpts,
    row: &PeriodicReportRow<N, BudgetCell>,
) -> Vec<BudgetCell> {
    let mut cells = Vec::new();
    if !ropts.summary_only {
        cells.extend(row.amounts.iter().cloned());
    }
    if ropts.row_total && !row.amounts.is_empty() {
        cells.push(row.total.clone());
    }
    if ropts.average && !row.amounts.is_empty() {
        cells.push(row.average.clone());
    }
    cells
}

/// The commodities of these cells' actual amounts and goals, sorted
/// (budgetCellsCommodities).
fn budget_cells_commodities(cells: &[BudgetCell]) -> Vec<Sym> {
    (cells.iter())
        .flat_map(|(actual, goal)| actual.iter().chain(goal))
        .flat_map(MixedAmount::commodities)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// A budget cell's values shown as the layout says: on one line (wide), or
/// a line for each of the row's commodities (tall, and bare without the
/// symbols), with the percentage of the goal reached on each line where
/// it can be calculated (showcell, mkBudgetDisplayFns).
fn show_budget_cell(
    ropts: &ReportOpts,
    layout: Layout,
    cs: &[Sym],
    (actual, goal): &BudgetCell,
) -> Vec<BudgetDisplayCell> {
    let show = |m: &MixedAmount| -> Vec<Wide> {
        match layout {
            Layout::Wide(width) => vec![show_mixed_amount_b(
                &AmountFormat {
                    max_width: width.map(|w| w as isize),
                    colour: ropts.color,
                    ..AmountFormat::ONE_LINE_NO_COST
                },
                m,
            )],
            _ => show_mixed_amount_lines_b(
                &AmountFormat {
                    commodity: layout != Layout::Bare,
                    commodity_order: Some(cs.to_vec()),
                    min_width: None,
                    colour: ropts.color,
                    ..AmountFormat::NO_COST
                },
                m,
            ),
        }
    };
    let null = MixedAmount::null();
    let actual = actual.as_ref().unwrap_or(&null);
    let actuals = show(actual);
    let Some(goal) = goal else {
        return actuals.into_iter().map(|a| (a, None)).collect();
    };
    let percentages: Vec<Option<Decimal>> = match layout {
        Layout::Wide(_) => vec![budget_percentage(ropts, actual, goal)],
        _ => (cs.iter())
            .map(|c| {
                let find = |m: &MixedAmount| m.amounts().into_iter().find(|a| &a.commodity == c);
                match (find(actual), find(goal)) {
                    (Some(a), Some(g)) => budget_percentage(
                        ropts,
                        &MixedAmount::from_amount(a),
                        &MixedAmount::from_amount(g),
                    ),
                    _ => None,
                }
            })
            .collect(),
    };
    let goals = (show(goal).into_iter()).zip(percentages).map(|(g, p)| {
        let p = p.map(|p| Wide::new(&p.round_to(0).to_string()));
        Some((g, p))
    });
    actuals.into_iter().zip(goals).collect()
}

/// The percentage of the goal the actual amount reaches, if they are in
/// one commodity (or the actual amount is zero) and the goal is not zero;
/// with -B or --value=transacted, of their costs (percentage).
fn budget_percentage(
    ropts: &ReportOpts,
    actual: &MixedAmount,
    goal: &MixedAmount,
) -> Option<Decimal> {
    let costed = |m: &MixedAmount| match ropts.conversion_op {
        Some(ConversionOp::ToCost) => m.clone().to_cost_basis().amounts(),
        Some(ConversionOp::ToTransactedCost) => m.clone().to_cost().amounts(),
        _ => m.clone().strip_costs().amounts(),
    };
    match (costed(actual).as_slice(), costed(goal).as_slice()) {
        ([a], [g]) if (a.commodity == g.commodity || a.looks_zero()) && !g.looks_zero() => {
            Some(&(&Decimal::from_i64(100) * &a.quantity) / &g.quantity)
        }
        _ => None,
    }
}

/// A budget cell's values in columns of these widths: the actual amount
/// right-aligned, then `[PERCENT% of GOAL]` with the percentage and goal
/// right-aligned, or spaces where there is no goal or percentage
/// (showBudgetDisplayCell).
fn show_budget_display_cell(
    (actual_width, goal_width, percent_width): (usize, usize, usize),
    (actual, goal): &BudgetDisplayCell,
) -> Wide {
    let pad = |w: usize, text: &Wide| " ".repeat(w.saturating_sub(text.width));
    let percent_total = if percent_width == 0 {
        0
    } else {
        percent_width + 5
    };
    let goal_total = if goal_width == 0 {
        0
    } else {
        goal_width + percent_total + 3
    };
    let mut s = pad(actual_width, actual);
    s.push_str(&actual.s);
    match goal {
        None => s.push_str(&" ".repeat(goal_total)),
        Some((goal, percent)) => {
            s.push_str(" [");
            match percent {
                None => s.push_str(&" ".repeat(percent_total)),
                Some(p) => {
                    s.push_str(&pad(percent_width, p));
                    s.push_str(&p.s);
                    s.push_str("% of ");
                }
            }
            s.push_str(&pad(goal_width, goal));
            s.push_str(&goal.s);
            s.push(']');
        }
    }
    Wide {
        s,
        width: actual_width + goal_total,
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
    // showBudgetDisplayCell: the actual amount, then the percentage and
    // goal, each right-aligned in its column, or spaces in their place.
    #[test]
    fn budget_display_cells() {
        let w = |s: &str| Wide::new(s);
        let show = |cell: BudgetDisplayCell| show_budget_display_cell((4, 4, 3), &cell).s;
        assert_eq!(
            show((w("$10"), Some((w("$25"), Some(w("40")))))),
            " $10 [ 40% of  $25]"
        );
        assert_eq!(show((w("0"), Some((w("0"), None)))), "   0 [           0]");
        assert_eq!(show((w("$5"), None)), format!("  $5{}", " ".repeat(15)));
        assert_eq!(
            show_budget_display_cell((2, 0, 0), &(w("$5"), None)).width,
            2
        );
    }
}
