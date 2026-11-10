//! The compound balance commands (Hledger.Cli.CompoundBalanceCommand and
//! Hledger.Cli.Commands.Balancesheet, Balancesheetequity, Incomestatement
//! and Cashflow): a multi-period balance report in sections (assets and
//! liabilities, revenues and expenses, ...), each with its own totals, and
//! the net totals, all amounts normally positive. Text, csv, tsv, json,
//! html and fods output, and charts (with --chart, or svg).

use std::collections::{BTreeMap, BTreeSet};

use crate::account::AccountType;
use crate::amount::render::AmountFormat;
use crate::amount::{MixedAmount, Sym};
use crate::cli::anchor::{LinkOpts, header_date_span_cell};
use crate::cli::args::{Arity, Flag, Mode};
use crate::cli::chart::{
    CHART_WIDTH_FLAG, chart_requested, check_chart_arg, check_chart_format,
    compound_balance_report_as_chart, render_chart,
};
use crate::cli::commands::balance::{
    ACCOUNT_CLASS, NBSP, TIDY_COLUMN_LABELS, TOTAL_ROW_HEADING_SPREADSHEET,
    balance_sub_report_as_spreadsheet_parts, multi_balance_report_as_part_table,
    multi_balance_report_num_header_columns, multi_balance_report_table_as_text,
    multi_balance_row_as_text, multi_balance_totals_rows, report_link_opts, valuation_description,
};
use crate::cli::io::{unsupported_output_format_error, warn_if_large_multi_period_report};
use crate::cli::opts::{
    CliOpts, OUTPUT_FILE_FLAG, OUTPUT_VIEW_FLAG, flat_flag, output_format_flag, tree_flag,
};
use crate::dates::{DateSpan, Day, Interval, prevday, spans_span};
use crate::model::Journal;
use crate::reports::opts::{
    BalanceAccumulation, BalanceCalculation, Layout, NormalSign, ReportOpts, ReportSpec,
    balance_accumulation_override, report_period_name,
};
use crate::reports::periodic::{
    CompoundPeriodicReport, DisplayName, PeriodicReport, SubreportSpec, balance_report_styles,
    compound_balance_report,
};
use crate::text::Wide;
use crate::write::csv::{print_csv, print_tsv};
use crate::write::fods::{locale_encoding_name, print_fods};
use crate::write::html::{table_html, table_style};
use crate::write::json::to_json_text;
use crate::write::spreadsheet::{
    self as spr, Border, Emphasis, Style, horizontal_span, raw_table_content,
};
use crate::write::table::{Header, Line, Table, concat_tables};

/// A compound balance command: its mode, its report's name (which the
/// title qualifies by the report interval), its sections, and its default
/// accumulation, which the accumulation flags override
/// (CompoundBalanceCommandSpec).
pub struct CompoundBalanceCommandSpec {
    pub mode: Mode,
    pub name: &'static str,
    pub sections: &'static [SubreportSpec],
    pub accum: BalanceAccumulation,
}

const ASSETS: SubreportSpec = SubreportSpec {
    title: "Assets",
    account_type: AccountType::Asset,
    normal_sign: NormalSign::NormallyPositive,
    increases_total: true,
};
const LIABILITIES: SubreportSpec = SubreportSpec {
    title: "Liabilities",
    account_type: AccountType::Liability,
    normal_sign: NormalSign::NormallyNegative,
    increases_total: false,
};
const EQUITY: SubreportSpec = SubreportSpec {
    title: "Equity",
    account_type: AccountType::Equity,
    normal_sign: NormalSign::NormallyNegative,
    increases_total: false,
};
const REVENUES: SubreportSpec = SubreportSpec {
    title: "Revenues",
    account_type: AccountType::Revenue,
    normal_sign: NormalSign::NormallyNegative,
    increases_total: true,
};
const EXPENSES: SubreportSpec = SubreportSpec {
    title: "Expenses",
    account_type: AccountType::Expense,
    normal_sign: NormalSign::NormallyPositive,
    increases_total: false,
};
const CASH_FLOWS: SubreportSpec = SubreportSpec {
    title: "Cash flows",
    account_type: AccountType::Cash,
    normal_sign: NormalSign::NormallyPositive,
    increases_total: true,
};

/// The balancesheet command (balancesheetSpec): assets and liabilities,
/// historical balances.
pub static BALANCESHEET: CompoundBalanceCommandSpec = CompoundBalanceCommandSpec {
    mode: Mode {
        name: "balancesheet",
        aliases: &["bs"],
        flags: &HISTORICAL_FLAGS,
        doc: crate::command_doc!("Balancesheet"),
        ..Mode::DEFAULT
    },
    name: "Balance Sheet",
    sections: &[ASSETS, LIABILITIES],
    accum: BalanceAccumulation::Historical,
};

/// The balancesheetequity command (balancesheetequitySpec): assets,
/// liabilities and equity, historical balances.
pub static BALANCESHEETEQUITY: CompoundBalanceCommandSpec = CompoundBalanceCommandSpec {
    mode: Mode {
        name: "balancesheetequity",
        aliases: &["bse"],
        flags: &HISTORICAL_FLAGS,
        doc: crate::command_doc!("Balancesheetequity"),
        ..Mode::DEFAULT
    },
    name: "Balance Sheet With Equity",
    sections: &[ASSETS, LIABILITIES, EQUITY],
    accum: BalanceAccumulation::Historical,
};

/// The incomestatement command (incomestatementSpec): revenues and
/// expenses, balance changes.
pub static INCOMESTATEMENT: CompoundBalanceCommandSpec = CompoundBalanceCommandSpec {
    mode: Mode {
        name: "incomestatement",
        aliases: &["is"],
        flags: &PER_PERIOD_FLAGS,
        doc: crate::command_doc!("Incomestatement"),
        ..Mode::DEFAULT
    },
    name: "Income Statement",
    sections: &[REVENUES, EXPENSES],
    accum: BalanceAccumulation::PerPeriod,
};

/// The cashflow command (cashflowSpec): cash accounts, balance changes.
pub static CASHFLOW: CompoundBalanceCommandSpec = CompoundBalanceCommandSpec {
    mode: Mode {
        name: "cashflow",
        aliases: &["cf"],
        flags: &PER_PERIOD_FLAGS,
        doc: crate::command_doc!("Cashflow"),
        ..Mode::DEFAULT
    },
    name: "Cashflow Statement",
    sections: &[CASH_FLOWS],
    accum: BalanceAccumulation::PerPeriod,
};

static HISTORICAL_FLAGS: [Flag; 28] = compound_balance_command_flags(true);
static PER_PERIOD_FLAGS: [Flag; 28] = compound_balance_command_flags(false);

const NONE: Arity = Arity::None;
const REQ: Arity = Arity::Req;

/// The flags of a compound balance command, whose default accumulation is
/// historical or per period (compoundBalanceCommandMode).
const fn compound_balance_command_flags(historical_default: bool) -> [Flag; 28] {
    let (change, historical) = if historical_default {
        (
            "accumulation mode: accumulate amounts from column start to column end (in multicolumn reports)",
            "accumulation mode: accumulate amounts from journal start to column end (includes postings before report start date) (default)",
        )
    } else {
        (
            "accumulation mode: accumulate amounts from column start to column end (in multicolumn reports) (default)",
            "accumulation mode: accumulate amounts from journal start to column end (includes postings before report start date)",
        )
    };
    [
        Flag::new(&["sum"], NONE, "", "calculation mode: show sum of posting amounts (default)"),
        Flag::new(&["valuechange"], NONE, "", "calculation mode: show total change of value of period-end historical balances (caused by deposits, withdrawals, market price fluctuations)"),
        Flag::new(&["gain"], NONE, "", "calculation mode: show unrealised capital gain/loss (historical balance value minus cost basis)"),
        Flag::new(&["count"], NONE, "", "calculation mode: show the count of postings"),
        Flag::new(&["change"], NONE, "", change),
        Flag::new(&["cumulative"], NONE, "", "accumulation mode: accumulate amounts from report start (specified by e.g. -b/--begin) to column end"),
        Flag::new(&["historical"], NONE, "", historical).short(&['H']),
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
        Flag::new(&["sort-amount"], NONE, "", "sort by amount instead of account code/name").short(&['S']),
        Flag::new(&["percent"], NONE, "", "express values in percentage of each column's total").short(&['%']),
        Flag::new(&["layout"], REQ, "ARG", "how to show multi-commodity amounts:\n'wide[,WIDTH]': all commodities on one line\n'tall'        : each commodity on a new line\n'bare'        : bare numbers, symbols in a column\n"),
        Flag::new(&["base-url"], REQ, "URLPREFIX", "in html output, generate hyperlinks to hledger-web, with this prefix. (Usually the base url shown by hledger-web; can also be relative.)"),
        output_format_flag("select the output format. Supported formats:\ntxt, html, csv, tsv, json, svg."),
        OUTPUT_FILE_FLAG,
        OUTPUT_VIEW_FLAG,
        Flag::new(&["chart"], Arity::Opt, "KIND,SCALE", "show a chart: instead of the table (txt), above it (html), or as data (csv, tsv, json). KIND: grouped (sections side by side) or diverging (above and below zero). SCALE: the amount that fills the chart (eg 1000 or 1000 USD; one per commodity), for comparable charts.").default("auto").check(check_chart_arg),
        CHART_WIDTH_FLAG,
        Flag::new(&["commodity-column"], NONE, "", "show commodity symbols in a separate column, amounts as bare numbers, one row per commodity").hidden(),
    ]
}

/// Run a compound balance command: its report, with its default
/// accumulation unless overridden, titled, styled, rendered
/// (compoundBalanceCommand).
pub fn compound_balance_command(
    cbc: &CompoundBalanceCommandSpec,
    opts: &CliOpts,
    j: &Journal,
) -> Result<String, String> {
    warn_if_large_multi_period_report(opts, j)?;
    let fmt = opts.output_format();
    let chart = chart_requested(opts);
    match fmt.as_str() {
        _ if chart => check_chart_format(opts)?,
        "txt" | "csv" | "tsv" | "json" | "html" | "fods" => {}
        f => return Err(unsupported_output_format_error(f)),
    }
    let ropts = &opts.spec.opts;
    let accum_override = balance_accumulation_override(&opts.raw, ropts.balance_calc);
    let ropts1 = ReportOpts {
        balance_accum: accum_override.unwrap_or(cbc.accum),
        // json shows every value of the report
        all_values_shown: fmt == "json",
        ..ropts.clone()
    };
    // the layout is needed by the report's own output (an invalid one is an
    // error before the report is made), and by a chart's html
    let layout = ropts1.layout.clone();
    if !chart {
        layout.clone()?;
    }
    let spec = ReportSpec {
        opts: ReportOpts {
            // a chart shows the period values, which --percent must calculate
            summary_only: ropts1.summary_only && !chart,
            ..ropts1.clone()
        },
        ..opts.spec.clone()
    };
    let mut cbr = compound_balance_report(&spec, j, cbc.sections)?;
    let title = compound_balance_report_title(cbc, ropts, accum_override, &cbr.dates);
    cbr.title = ropts1.effective_title(&title).to_string();
    for (title, _, _) in &mut cbr.subreports {
        *title = ropts.translations.tr(title).to_string();
    }
    apply_subreport_titles(&ropts1, &mut cbr);
    cbr.style_amounts(&balance_report_styles(ropts, j));
    if chart {
        let section_types: Vec<Option<AccountType>> =
            cbc.sections.iter().map(|s| Some(s.account_type)).collect();
        let account_type = |a: &str| j.account_type(a);
        let c = compound_balance_report_as_chart(&account_type, &section_types, &ropts1, &cbr);
        return render_chart(opts, c, || {
            Ok(compound_balance_report_as_html(&ropts1, layout?, cbc.sections, &cbr) + "\n")
        });
    }
    let layout = layout?;
    Ok(match fmt.as_str() {
        "csv" => print_csv(&compound_balance_report_as_csv(
            &ropts1,
            layout,
            cbc.sections,
            &cbr,
        )),
        "tsv" => print_tsv(&compound_balance_report_as_csv(
            &ropts1,
            layout,
            cbc.sections,
            &cbr,
        )),
        "json" => to_json_text(&cbr),
        "html" => compound_balance_report_as_html(&ropts1, layout, cbc.sections, &cbr) + "\n",
        "fods" => {
            let (title, sheet) = compound_balance_report_as_spreadsheet(
                &AmountFormat::ONE_LINE_NO_COST,
                "Account",
                Some(""),
                &ropts1,
                layout,
                cbc.sections,
                &cbr,
            );
            print_fods(locale_encoding_name(), &BTreeMap::from([(title, sheet)]))
        }
        _ => compound_balance_report_as_text(&ropts1, layout, &cbr),
    })
}

/// The title of a compound balance report: the report's name for its
/// interval, its dates (for historical balances, its period end dates),
/// what it shows when the accumulation was overridden or the calculation
/// is not the plain sum, and its valuation. The options are the command
/// line's, the accumulation override given separately, since the title
/// says which it was (compoundBalanceReportTitle).
fn compound_balance_report_title(
    cbc: &CompoundBalanceCommandSpec,
    ropts: &ReportOpts,
    accum_override: Option<BalanceAccumulation>,
    dates: &[DateSpan],
) -> String {
    use BalanceAccumulation::*;
    use BalanceCalculation::*;
    let accum = accum_override.unwrap_or(cbc.accum);
    let dates_text = match accum {
        Historical => {
            let ends: Vec<Day> = dates
                .iter()
                .filter_map(DateSpan::end)
                .map(prevday)
                .collect();
            show_end_dates(&ends)
        }
        _ => spans_span(dates).to_string(),
    };
    let clarification = match (ropts.balance_calc, accum, accum_override) {
        (ValueChange, PerPeriod, _) => "(Period-End Value Changes)",
        (ValueChange, Cumulative, _) => "(Cumulative Period-End Value Changes)",
        (Gain, PerPeriod, _) => "(Incremental Gain)",
        (Gain, Cumulative, _) => "(Cumulative Gain)",
        (Gain, Historical, _) => "(Historical Gain)",
        (_, _, Some(PerPeriod)) => "(Balance Changes)",
        (_, _, Some(Cumulative)) => "(Cumulative Ending Balances)",
        (_, _, Some(Historical)) => "(Historical Ending Balances)",
        _ => "",
    };
    let t = &ropts.translations;
    let clarification = if clarification.is_empty() {
        String::new()
    } else {
        format!(" {}", t.tr(clarification))
    };
    let changing_valuation = ropts.balance_calc == ValueChange && ropts.balance_accum != Historical;
    t.trf(
        "{report} {dates}{clarification}{valuation}",
        &[
            ("report", t.tr(&interval_title(cbc.name, &ropts.interval))),
            ("dates", &dates_text),
            ("clarification", &clarification),
            (
                "valuation",
                &valuation_description(ropts, changing_valuation),
            ),
        ],
    )
}

/// The report's name qualified by its interval, twelve months making a
/// year and a quarter three months (cbctitle, titleInterval).
fn interval_title(name: &str, interval: &Interval) -> String {
    let qualifier = match interval {
        Interval::NoInterval => "",
        Interval::Days(1) => "Daily ",
        Interval::Weeks(1) => "Weekly ",
        Interval::Weeks(2) => "Biweekly ",
        Interval::Months(1) => "Monthly ",
        Interval::Months(2) => "Bimonthly ",
        Interval::Months(3) | Interval::Quarters(1) => "Quarterly ",
        Interval::Months(6) | Interval::Quarters(2) => "Half-yearly ",
        Interval::Years(1) | Interval::Months(12) => "Yearly ",
        Interval::Years(2) => "Biennial ",
        _ => "Periodic ",
    };
    format!("{qualifier}{name}")
}

/// End dates, inclusive, shown as discrete dates rather than a span: the
/// first and last joined by `..` (showEndDates).
fn show_end_dates(ends: &[Day]) -> String {
    match ends {
        [] => String::new(),
        [e] => e.to_string(),
        [first, .., last] => format!("{first}..{last}"),
    }
}

/// The sections' titles as --subreport-titles says: its `|`-separated
/// titles replacing the first ones in order, or if it is empty, none
/// (applySubreportTitles).
fn apply_subreport_titles(
    ropts: &ReportOpts,
    cbr: &mut CompoundPeriodicReport<DisplayName, MixedAmount>,
) {
    match ropts.subreport_titles.as_deref() {
        None => {}
        Some("") => {
            for (title, _, _) in &mut cbr.subreports {
                title.clear();
            }
        }
        Some(s) => {
            for ((title, _, _), custom) in cbr.subreports.iter_mut().zip(s.split('|')) {
                *title = custom.to_string();
            }
        }
    }
}

/// A compound balance report as text: its title, then the table of its
/// sections below double lines, each with its title row (unless that is
/// empty) and its totals, then unless --no-total or with one section, the
/// net totals (compoundBalanceReportAsText).
fn compound_balance_report_as_text(
    ropts: &ReportOpts,
    layout: Layout,
    cbr: &CompoundPeriodicReport<DisplayName, MixedAmount>,
) -> String {
    let commodities = all_commodities_from_subreports(&cbr.subreports);
    let section =
        |(title, report, _): &(String, PeriodicReport<DisplayName, MixedAmount>, bool)| {
            let t = multi_balance_report_as_part_table(ropts, layout, &commodities, report);
            if title.is_empty() {
                return t;
            }
            let mut rows = vec![Vec::new()];
            rows.extend(t.rows);
            Table {
                row_headers: Header::Group(
                    Line::SingleLine,
                    vec![Header::Leaf(title.clone()), t.row_headers],
                ),
                col_headers: t.col_headers,
                rows,
            }
        };
    let mut table = (cbr.subreports.iter().map(section))
        .reduce(|a, b| concat_tables(Line::DoubleLine, a, b))
        .unwrap_or(Table {
            row_headers: Header::Group(Line::NoLine, Vec::new()),
            col_headers: Header::Group(Line::NoLine, Vec::new()),
            rows: Vec::new(),
        });
    if !(ropts.no_total || cbr.subreports.len() == 1) {
        let lines: Vec<Vec<Wide>> =
            multi_balance_row_as_text(ropts, layout, &commodities, &cbr.totals);
        let net = ropts.translations.tr("Net:").to_string();
        let headers = std::iter::once(net).chain(std::iter::repeat_n(
            String::new(),
            lines.len().saturating_sub(1),
        ));
        let totals = Table {
            row_headers: Header::Group(Line::NoLine, headers.map(Header::Leaf).collect()),
            col_headers: Header::Leaf(()),
            rows: lines,
        };
        table = concat_tables(Line::DoubleLine, table, totals);
    }
    let mut out = if cbr.title.is_empty() {
        String::new()
    } else {
        format!("{}\n\n", cbr.title)
    };
    out.push_str(&multi_balance_report_table_as_text(ropts, layout, &table));
    out
}

/// A compound balance report as CSV records: its title spanning the
/// heading row's columns, unless it is empty, then the spreadsheet rows
/// (compoundBalanceReportAsCsv).
fn compound_balance_report_as_csv(
    ropts: &ReportOpts,
    layout: Layout,
    sections: &[SubreportSpec],
    cbr: &CompoundPeriodicReport<DisplayName, MixedAmount>,
) -> Vec<Vec<String>> {
    let (title, (_, rows)) = compound_balance_report_as_spreadsheet(
        &AmountFormat::MACHINE,
        "Account",
        None,
        ropts,
        layout,
        sections,
        cbr,
    );
    let mut records = Vec::new();
    if !title.is_empty() {
        records.push(horizontal_span(rows[0].len(), spr::Cell::header(&title)));
    }
    records.extend(rows);
    raw_table_content(records)
}

/// A compound balance report as html: the table styles with the account
/// column unwrapped and alternate rows shaded, the title, and one table
/// with a row of no-break space after each section
/// (compoundBalanceReportAsHtml).
fn compound_balance_report_as_html(
    ropts: &ReportOpts,
    layout: Layout,
    sections: &[SubreportSpec],
    cbr: &CompoundPeriodicReport<DisplayName, MixedAmount>,
) -> String {
    let (title, (_, rows)) = compound_balance_report_as_spreadsheet(
        &AmountFormat::ONE_LINE_NO_COST,
        "",
        Some(NBSP),
        ropts,
        layout,
        sections,
        cbr,
    );
    let mut rules = table_style();
    rules.push(("td:nth-child(1)".into(), "white-space:nowrap".into()));
    rules.push((
        "tr:nth-child(odd) td".into(),
        "background-color:#eee".into(),
    ));
    table_html(&rules, &title, &rows)
}

/// A compound balance report as a spreadsheet, with its title and the
/// numbers of its heading rows and columns
/// (compoundBalanceReportAsSpreadsheet, compoundBalanceReportAsSpreadsheetParts):
/// the heading row (this label over the accounts, then a heading per
/// period, spanning the commodity columns in the bare-wide layout, and the
/// total's and average's when shown), then each section's title row
/// (unless the title is empty), body rows, totals rows, and a row of the
/// blank text if one is given, then the net totals rows.
fn compound_balance_report_as_spreadsheet(
    fmt: &AmountFormat,
    account_label: &str,
    blank: Option<&str>,
    ropts: &ReportOpts,
    layout: Layout,
    sections: &[SubreportSpec],
    cbr: &CompoundPeriodicReport<DisplayName, MixedAmount>,
) -> (String, spr::Sheet) {
    let colspans = &cbr.dates;
    let base = ropts.balance_base_url.as_deref();
    let all_commodities = all_commodities_from_subreports(&cbr.subreports);
    let mut leading_headers = vec![spr::Cell::header(account_label)];
    match layout {
        Layout::Tidy => {
            leading_headers.extend(TIDY_COLUMN_LABELS.iter().map(|l| spr::Cell::header(l)));
        }
        Layout::Bare => leading_headers.push(spr::Cell::header("Commodity")),
        _ => {}
    }
    let mut data_headers: Vec<spr::Cell<String>> = Vec::new();
    if layout != Layout::Tidy {
        // a period's heading links to the register for that period; it
        // shows the period's end date in a historical report
        if !ropts.summary_only {
            // in English, as the other headings are
            let english = ReportOpts {
                translations: Default::default(),
                ..ropts.clone()
            };
            let name = report_period_name(&english, colspans);
            data_headers.extend(colspans.iter().map(|s| spr::Cell {
                border: Border::default(),
                content: name(s),
                ..header_date_span_cell(
                    ropts.date2,
                    ropts.period_titles,
                    base,
                    &ropts.query_string,
                    s,
                )
            }));
        }
        if ropts.has_totals_column() {
            data_headers.push(spr::Cell::header("Total"));
        }
        if ropts.average {
            data_headers.push(spr::Cell::header("Average"));
        }
    }
    let sub_columns = match layout {
        Layout::BareWide => all_commodities.len(),
        _ => 1,
    };
    let mut header_row = leading_headers;
    for h in &data_headers {
        header_row.extend(horizontal_span(sub_columns, h.clone()));
    }
    let blank_row = blank.map(|b| horizontal_span(header_row.len(), spr::Cell::new(b.to_string())));
    // a section's rows link with its account type term as well, so that
    // their registers exclude a subaccount of another type, as the figures
    // do; a section is rendered with its own options, so that its links say
    // when its figures are negated
    let type_term = |s: &SubreportSpec| format!("type:{}", s.account_type);
    let mut rows = vec![header_row.clone()];
    for (spec, (title, report, _)) in sections.iter().zip(&cbr.subreports) {
        let section_term = vec![type_term(spec)];
        let sropts = ReportOpts {
            normal_balance: Some(spec.normal_sign),
            ..ropts.clone()
        };
        if !title.is_empty() {
            let account_cell = spr::Cell {
                style: Style::Body(Emphasis::Total),
                ..spr::Cell::new(title.clone()).with_class(ACCOUNT_CLASS)
            };
            rows.push(match layout {
                Layout::BareWide => {
                    let mut row = vec![account_cell];
                    for _ in &data_headers {
                        row.extend(all_commodities.iter().map(|c| spr::Cell::header(c)));
                    }
                    row
                }
                _ => horizontal_span(header_row.len(), account_cell),
            });
        }
        let body_opts = ReportOpts {
            no_total: true,
            query_string: [section_term.clone(), sropts.query_string.clone()].concat(),
            ..sropts.clone()
        };
        let (_, body, _) = balance_sub_report_as_spreadsheet_parts(
            fmt,
            &body_opts,
            layout,
            &all_commodities,
            report,
        );
        rows.extend(body);
        if !ropts.no_total {
            rows.extend(multi_balance_totals_rows(
                fmt,
                &sropts,
                layout,
                &report_link_opts(&sropts, &report.dates),
                &section_term,
                &all_commodities,
                &report.dates,
                TOTAL_ROW_HEADING_SPREADSHEET,
                &report.totals,
            ));
        }
        rows.extend(blank_row.clone());
    }
    // the net total's register is that of all the sections' account
    // types; it shows the net with the opposite sign when a section's
    // negation is not undone by subtracting it, as an income statement's
    // revenues are added negated
    if !(ropts.no_total || cbr.subreports.len() == 1) {
        let net_term = format!(
            "type:{}",
            (sections.iter())
                .map(|s| s.account_type.to_string())
                .collect::<String>()
        );
        let net_negated = (sections.iter())
            .any(|s| (s.normal_sign == NormalSign::NormallyNegative) != !s.increases_total);
        let lo = LinkOpts {
            negated: net_negated,
            ..report_link_opts(ropts, colspans)
        };
        rows.extend(multi_balance_totals_rows(
            fmt,
            ropts,
            layout,
            &lo,
            &[net_term],
            &all_commodities,
            colspans,
            "Net:",
            &cbr.totals,
        ));
    }
    (
        cbr.title.clone(),
        ((1, multi_balance_report_num_header_columns(layout)), rows),
    )
}

/// The commodities of all the sections' period amounts, sorted: the
/// bare-wide layout's commodity columns (allCommoditiesFromSubreports).
fn all_commodities_from_subreports(
    subreports: &[(String, PeriodicReport<DisplayName, MixedAmount>, bool)],
) -> Vec<Sym> {
    (subreports.iter())
        .flat_map(|(_, r, _)| r.rows.iter())
        .flat_map(|row| row.amounts.iter().flat_map(MixedAmount::commodities))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_by_interval() {
        let title = |i| interval_title("Balance Sheet", &i);
        assert_eq!(title(Interval::NoInterval), "Balance Sheet");
        assert_eq!(title(Interval::Months(12)), "Yearly Balance Sheet");
        assert_eq!(title(Interval::Quarters(1)), "Quarterly Balance Sheet");
        assert_eq!(title(Interval::Quarters(2)), "Half-yearly Balance Sheet");
        assert_eq!(title(Interval::Weeks(2)), "Biweekly Balance Sheet");
        assert_eq!(title(Interval::Days(3)), "Periodic Balance Sheet");
    }

    #[test]
    fn end_dates() {
        let d = |m, d| Day::from_ymd(2024, m, d).unwrap();
        assert_eq!(show_end_dates(&[]), "");
        assert_eq!(show_end_dates(&[d(1, 31)]), "2024-01-31");
        assert_eq!(
            show_end_dates(&[d(1, 31), d(2, 29), d(3, 31)]),
            "2024-01-31..2024-03-31"
        );
    }
}
