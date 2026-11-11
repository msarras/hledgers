//! The register command (Hledger.Cli.Commands.Register): the postings
//! report as lines of text laid out to the available width, or as csv,
//! tsv, json, html or fods, or as a chart; and the text layout it shares
//! with aregister (postingsOrTransactionsReportAsText).

use crate::account::{Realness, account_name_drop, elide_account_name};
use crate::amount::render::{
    AmountFormat, show_mixed_amount_lines_b, show_mixed_amount_lines_reusing,
};
use crate::amount::{MixedAmount, Rounding, styles_with_rounding};
use crate::cli::anchor::{date_cell, set_account_anchor};
use crate::cli::args::{Arity, Flag, Mode};
use crate::cli::chart::{
    CHART_FLAG, chart_requested, check_chart_format, postings_report_as_chart, render_chart,
};
use crate::cli::io::{unsupported_output_format_error, with_title};
use crate::cli::opts::{CliOpts, OUTPUT_FILE_FLAG, OUTPUT_VIEW_FLAG, output_format_flag};
use crate::model::Journal;
use crate::reports::opts::PeriodTitles;
use crate::reports::postings::{
    PostingsReportItem, postings_report, postings_report_start_balances,
};
use crate::text::{Wide, fit_text, is_space, show_string};
use crate::write::csv::print_delimited;
use crate::write::fods::print_fods;
use crate::write::html::titled_table_html;
use crate::write::json::to_json_text;
use crate::write::spreadsheet::{
    self as spr, CellType, add_header_borders, cell_from_mixed_amount, horizontal_span,
    integer_cell,
};
use crate::write::table::{Align, Cell};

const NONE: Arity = Arity::None;
const REQ: Arity = Arity::Req;

pub static MODE: Mode = Mode {
    name: "register",
    aliases: &["reg"],
    flags: &[
        Flag::new(&["cumulative"], NONE, "", "accumulation mode: show running total from report start date (default)"),
        Flag::new(&["historical"], NONE, "", "accumulation mode: show historical running total/balance (includes postings before report start date)").short(&['H']),
        Flag::new(&["average"], NONE, "", "show running average of posting amounts instead of total (implies --empty)").short(&['A']),
        Flag::new(&["match"], REQ, "DESC", "fuzzy search for one recent posting with description closest to DESC").short(&['m']),
        Flag::new(&["related"], NONE, "", "show postings' siblings instead").short(&['r']),
        Flag::new(&["invert"], NONE, "", "display all amounts with reversed sign"),
        Flag::new(&["drop"], REQ, "N", "omit N leading account name parts"),
        Flag::new(&["sort"], REQ, "FIELDS", "sort by: date, desc, account, amount, absamount, or a comma-separated combination of these. For a descending sort, prefix with -. (Default: date)"),
        WIDTH_FLAG,
        ALIGN_ALL_FLAG,
        Flag::new(&["base-url"], REQ, "URLPREFIX", "in html output, generate links to hledger-web, with this prefix. (Usually the base url shown by hledger-web; can also be relative.)"),
        output_format_flag("select the output format. Supported formats:\ntxt, csv, tsv, html, fods, json, svg."),
        OUTPUT_FILE_FLAG,
        OUTPUT_VIEW_FLAG,
        CHART_FLAG,
    ],
    doc: crate::command_doc!("Register"),
    ..Mode::DEFAULT
};

/// The register commands' --width flag.
pub const WIDTH_FLAG: Flag = Flag::new(
    &["width"],
    REQ,
    "N",
    "set output width (default: terminal width). -wN,M sets description width as well.",
)
.short(&['w']);

/// The register commands' --align-all flag.
pub const ALIGN_ALL_FLAG: Flag = Flag::new(
    &["align-all"],
    NONE,
    "",
    "guarantee alignment across all lines (slower)",
);

/// Run the register command (register): with --match, the one recent
/// posting whose description is most like the given one; otherwise the
/// postings report, or its chart.
pub fn register(opts: &CliOpts, j: &Journal) -> Result<String, String> {
    let spec = &opts.spec;
    let ropts = &spec.opts;
    let styles = styles_with_rounding(Rounding::Hard, &j.commodity_styles());
    if let Some(desc) = opts.raw.string("match") {
        let report = postings_report(spec, j)?;
        let Some(item) = similar_posting(&report, desc) else {
            return Err(format!(
                "no postings found with description like {}",
                show_string(desc)
            ));
        };
        let mut posting = item.posting.clone();
        posting.style_amounts(&styles);
        let item = PostingsReportItem {
            date: Some(posting.date_in(item.txn)),
            period: None,
            description: item.txn.map(|t| t.description.as_str()),
            posting,
            txn: item.txn,
            total: MixedAmount::null(),
        };
        return postings_report_as_text(opts, &[item]);
    }
    let fmt = opts.output_format();
    if chart_requested(opts) {
        check_chart_format(opts)?;
        let mut report = postings_report(spec, j)?;
        report.iter_mut().for_each(|i| i.style_amounts(&styles));
        let starts = postings_report_start_balances(spec, j)?;
        let title = ropts.effective_title("");
        let account_type = |a: &str| j.account_type(a);
        let chart = postings_report_as_chart(&account_type, &starts, ropts, title, &report);
        return render_chart(opts, chart, || {
            Ok(titled_table_html(title, &postings_report_as_html_rows(opts, &report)) + "\n")
        });
    }
    if !matches!(
        fmt.as_str(),
        "txt" | "json" | "csv" | "tsv" | "html" | "fods"
    ) {
        return Err(unsupported_output_format_error(&fmt));
    }
    let mut report = postings_report(spec, j)?;
    report.iter_mut().for_each(|i| i.style_amounts(&styles));
    Ok(match fmt.as_str() {
        "txt" => with_title(opts, postings_report_as_text(opts, &report)?),
        "json" => to_json_text(&report),
        "csv" | "tsv" => print_delimited(&fmt, postings_report_as_csv(opts, &report)),
        "html" => {
            let rows = postings_report_as_html_rows(opts, &report);
            titled_table_html(ropts.effective_title(""), &rows) + "\n"
        }
        _ => {
            let fmt = AmountFormat::ONE_LINE_NO_COST;
            let rows = postings_report_as_spreadsheet(opts, &fmt, true, &report).collect();
            print_fods(ropts.translations.tr("Register"), ((1, 0), rows))
        }
    })
}

/// The report as html table rows, with links to hledger-web if --base-url
/// is given.
fn postings_report_as_html_rows(opts: &CliOpts, report: &[PostingsReportItem]) -> spr::Rows {
    postings_report_as_spreadsheet(opts, &AmountFormat::ONE_LINE_NO_COST, true, report).collect()
}

/// The report as csv or tsv records: a heading row, after the title if
/// any (postingsReportAsCsv).
fn postings_report_as_csv<'a>(
    opts: &'a CliOpts,
    report: &'a [PostingsReportItem],
) -> impl Iterator<Item = Vec<spr::Cell<String>>> + 'a {
    let mut rows =
        postings_report_as_spreadsheet(opts, &AmountFormat::MACHINE, false, report).peekable();
    let title = opts.spec.opts.effective_title("");
    let title_row = (!title.is_empty())
        .then(|| horizontal_span(rows.peek().map_or(0, Vec::len), spr::Cell::header(title)));
    title_row.into_iter().chain(rows)
}

/// The report as spreadsheet rows: a heading row, then a row per item
/// with its transaction's number, date, code and description, the
/// account, amount and running total, the account and date linking to
/// hledger-web if `links` and --base-url say so (postingsReportAsSpreadsheet,
/// postingsReportItemAsRecord). The date is the primary one, or with
/// --period-titles=dates a summary posting's period. Each row is made as
/// it is taken, so that a writer need not hold them all.
fn postings_report_as_spreadsheet<'a>(
    opts: &'a CliOpts,
    fmt: &'a AmountFormat,
    links: bool,
    report: &'a [PostingsReportItem],
) -> impl Iterator<Item = Vec<spr::Cell<String>>> + 'a {
    let ropts = &opts.spec.opts;
    let base = if links {
        ropts.balance_base_url.as_deref()
    } else {
        None
    };
    let query: &[String] = if links { &ropts.query_string } else { &[] };
    let headings = [
        "txnidx",
        "date",
        "code",
        "description",
        "account",
        "amount",
        "total",
    ];
    let heading = add_header_borders(headings.into_iter().map(spr::Cell::header).collect());
    std::iter::once(heading).chain(report.iter().map(move |item| {
        let p = &item.posting;
        let t = item.txn;
        let date = match (item.period, ropts.period_titles) {
            (Some(per), PeriodTitles::Dates) => spr::Cell::new(per.as_date_span().show_full()),
            _ => spr::Cell {
                typ: CellType::Date,
                ..date_cell(base, query, &p.account, p.date_in(t))
            },
        };
        let acct = bracketed(p.realness, &account_name_drop(ropts.drop, &p.account));
        let amount = |m: &MixedAmount| cell_from_mixed_amount(fmt, "amount", m).into_text();
        vec![
            integer_cell(t.map_or(0, |t| t.index)),
            date,
            spr::Cell::new(t.map_or(String::new(), |t| t.code.clone())),
            spr::Cell::new(t.map_or(String::new(), |t| t.description.clone())),
            set_account_anchor(base, query, &p.account, spr::Cell::new(acct)),
            amount(&p.amount),
            amount(&item.total),
        ]
    }))
}

/// An account name in the brackets of its kind of posting, if virtual.
fn bracketed(r: Realness, a: &str) -> String {
    match r {
        Realness::BalancedVirtual => format!("[{a}]"),
        Realness::Virtual => format!("({a})"),
        Realness::Real => a.to_string(),
    }
}

/// The report as text (postingsReportAsText).
pub fn postings_report_as_text(
    opts: &CliOpts,
    report: &[PostingsReportItem],
) -> Result<String, String> {
    postings_or_transactions_report_as_text(
        opts,
        report,
        |i| &i.posting.amount,
        |i| &i.total,
        |widths, i, amt, bal| postings_report_item_as_text(opts, widths, i, amt, bal),
    )
}

/// A postings report line as text (postingsReportItemAsText): the date
/// (or with a report interval, the period), description, account, amount
/// and running total, in columns fitted to the width; the date and
/// description shown only for a transaction's first posting. Amounts on
/// several lines make several lines.
fn postings_report_item_as_text(
    opts: &CliOpts,
    widths: ItemWidths,
    item: &PostingsReportItem,
    amt: &[Wide],
    bal: &[Wide],
) -> String {
    let ropts = &opts.spec.opts;
    let titles = ropts.period_titles;
    let (date_width, date) = match item.period {
        None => (10, item.date.map(|d| d.to_string()).unwrap_or_default()),
        Some(per) => {
            let width = match titles {
                PeriodTitles::Dates => 22,
                PeriodTitles::Compact => per.text_width() as isize,
            };
            let date = match (item.date, titles) {
                (None, _) => String::new(),
                (Some(_), PeriodTitles::Dates) => per.as_date_span().show_full(),
                (Some(_), PeriodTitles::Compact) => per.to_string(),
            };
            (width, date)
        }
    };
    let cols = register_columns(widths, date_width, item.period.is_none());
    let p = &item.posting;
    let a = account_name_drop(ropts.drop, &p.account);
    let acct = match p.realness {
        Realness::Real => elide_account_name(cols.account, &a),
        r => bracketed(r, &elide_account_name(cols.account - 2, &a)),
    };
    register_line(
        &cols,
        &date,
        item.description.unwrap_or(""),
        &acct,
        amt,
        bal,
    )
}

/// What a register line's layout starts from: the total width, the
/// description's width if given, and the amount and balance columns'
/// preferred widths.
#[derive(Clone, Copy, Debug)]
pub struct ItemWidths {
    pub total: isize,
    pub description: Option<isize>,
    pub amount: isize,
    pub balance: isize,
}

/// A register line's column widths.
#[derive(Clone, Copy, Debug)]
pub struct RegisterColumns {
    pub date: isize,
    pub description: isize,
    pub account: isize,
    pub amount: isize,
    pub balance: isize,
}

/// A register line's column widths (from postingsReportItemAsText and
/// accountTransactionsReportItemAsText): the amount and balance columns
/// as wide as preferred, unless that leaves the description and account
/// less than two columns each, when they shrink in proportion; the
/// description and account sharing the rest, half each unless the
/// description's width is given; without a description (a summary
/// posting's line), all of it to the account.
pub fn register_columns(w: ItemWidths, date: isize, has_description: bool) -> RegisterColumns {
    let min = 2;
    let max_amounts = (w.total - (date + 1 + min + 2 + min + 2 + 2)).max(0);
    let (amount, balance) = if w.amount + w.balance - max_amounts <= 0 {
        (w.amount, w.balance)
    } else {
        let proportion = w.amount as f64 / (w.amount + w.balance) as f64;
        let amount = (proportion * max_amounts as f64).round_ties_even() as isize;
        (amount, max_amounts - amount)
    };
    let remaining = w.total - (date + 1 + 2 + amount + 2 + balance);
    let (description, account) = if has_description {
        let d = w.description.unwrap_or((remaining - 2).div_euclid(2));
        (d, remaining - 2 - d)
    } else {
        (0, remaining - 2)
    };
    RegisterColumns {
        date,
        description,
        account,
        amount,
        balance,
    }
}

/// A register line: the date, description and account fitted to their
/// columns, the amount's lines right-aligned at the top of theirs, the
/// balance's at the bottom, ending with a newline. This is the row the
/// table renderer makes of these cells (single spaces and pairs of spaces
/// between them, no borders), written directly: as many lines as the
/// tallest cell has, each cell padded to its column's width.
pub fn register_line(
    cols: &RegisterColumns,
    date: &str,
    description: &str,
    account: &str,
    amt: &[Wide],
    bal: &[Wide],
) -> String {
    let fitted =
        |w: isize, s: &str| Cell::text(Align::TopLeft, &fit_text(Some(w), Some(w), true, true, s));
    let (date, description, account) = (
        fitted(cols.date, date).lines,
        fitted(cols.description, description).lines,
        fitted(cols.account, account).lines,
    );
    let width = |lines: &[Wide]| lines.iter().map(|l| l.width).max().unwrap_or(0);
    // an amount line is padded to its column's width, which a wider amount
    // overflows; the column is that wide if it has lines
    let amounts_width = |w: isize, lines: &[Wide]| {
        if lines.is_empty() {
            0
        } else {
            w.max(0) as usize
        }
    };
    let widths = (
        width(&date),
        width(&description),
        width(&account),
        amounts_width(cols.amount, amt),
        amounts_width(cols.balance, bal),
    );
    let n = [
        date.len(),
        description.len(),
        account.len(),
        amt.len(),
        bal.len(),
    ]
    .into_iter()
    .fold(1, usize::max);
    const SPACES: &str = "                                                                ";
    let spaces = |out: &mut String, mut k: usize| {
        while k > 0 {
            let n = k.min(SPACES.len());
            out.push_str(&SPACES[..n]);
            k -= n;
        }
    };
    let left = |out: &mut String, lines: &[Wide], w: usize, i: usize| match lines.get(i) {
        Some(t) => {
            out.push_str(&t.s);
            spaces(out, w.saturating_sub(t.width));
        }
        None => spaces(out, w),
    };
    let right = |out: &mut String, line: Option<&Wide>, column: isize, w: usize| match line {
        Some(a) => {
            spaces(out, (column - a.width as isize).max(0) as usize);
            out.push_str(&a.s);
        }
        None => spaces(out, w),
    };
    let mut out = String::new();
    for i in 0..n {
        if i > 0 {
            out.push('\n');
        }
        left(&mut out, &date, widths.0, i);
        spaces(&mut out, 1);
        left(&mut out, &description, widths.1, i);
        spaces(&mut out, 2);
        left(&mut out, &account, widths.2, i);
        spaces(&mut out, 2);
        right(&mut out, amt.get(i), cols.amount, widths.3);
        spaces(&mut out, 2);
        right(
            &mut out,
            (i + bal.len()).checked_sub(n).and_then(|k| bal.get(k)),
            cols.balance,
            widths.4,
        );
    }
    out.push('\n');
    out
}

/// A postings or account transactions report as text, each item rendered
/// with its amount and balance shown on lines, and the preferred widths of
/// those columns: as wide as the widest of the first 1000 items' (with
/// --align-all, all items'), at least 12, widening as wider ones come
/// (postingsOrTransactionsReportAsText). The total width comes from
/// --width, which is an error if malformed and there are items.
pub fn postings_or_transactions_report_as_text<T: Sync>(
    opts: &CliOpts,
    items: &[T],
    amount: impl Fn(&T) -> &MixedAmount + Sync,
    balance: impl Fn(&T) -> &MixedAmount + Sync,
    item_as_text: impl Fn(ItemWidths, &T, &[Wide], &[Wide]) -> String + Sync,
) -> Result<String, String> {
    const MIN_WIDTH: usize = 12;
    const CHUNK_SIZE: usize = 1000;
    // the items are shown and rendered a block at a time, in runs on all cores
    const BLOCK: usize = 8192;
    const RUN: usize = 256;
    if items.is_empty() {
        return Ok(String::new());
    }
    let work = items.len();
    let (total, description) = opts.register_widths()?;
    let align_all = opts.raw.bool("align-all");
    let fmt = AmountFormat {
        colour: opts.spec.opts.color,
        ..AmountFormat::ONE_LINE_NO_COST
    };
    // the items' amounts and balances shown, a run of them at a time on
    // each core; successive balances are mostly the same amounts, whose
    // texts are reused
    type Shown = (Vec<Wide>, Vec<Wide>);
    let show = |items: &[T]| -> Vec<Vec<Shown>> {
        let runs: Vec<&[T]> = items.chunks(RUN).collect();
        crate::par::map(work, &runs, |run| {
            let mut last_balance = Vec::new();
            (run.iter())
                .map(|i| {
                    (
                        show_mixed_amount_lines_b(&fmt, amount(i)),
                        show_mixed_amount_lines_reusing(&fmt, balance(i), &mut last_balance),
                    )
                })
                .collect()
        })
    };
    // the items the starting widths come from are shown first, the others
    // as they come
    let n_start = if align_all {
        items.len()
    } else {
        items.len().min(CHUNK_SIZE)
    };
    let start = show(&items[..n_start]);
    let widest = |lines: &[Wide], w: usize| lines.iter().map(|l| l.width).fold(w, usize::max);
    let mut amt_width = start
        .iter()
        .flatten()
        .fold(MIN_WIDTH, |w, (a, _)| widest(a, w));
    let mut bal_width = start
        .iter()
        .flatten()
        .fold(MIN_WIDTH, |w, (_, b)| widest(b, w));
    let mut out = String::new();
    // each item's widths found in order, widening as wider amounts come;
    // then each run's items rendered on a core, and the runs' texts joined
    let mut render = |items: &[T], shown: Vec<Vec<Shown>>| {
        let mut items = items.iter();
        let mut runs = Vec::with_capacity(shown.len());
        for run in shown {
            let run: Vec<(ItemWidths, &T, Shown)> = (run.into_iter())
                .map(|s| {
                    if !align_all {
                        amt_width = widest(&s.0, amt_width);
                        bal_width = widest(&s.1, bal_width);
                    }
                    let widths = ItemWidths {
                        total,
                        description,
                        amount: amt_width as isize,
                        balance: bal_width as isize,
                    };
                    (widths, items.next().expect("an item for each shown"), s)
                })
                .collect();
            runs.push(run);
        }
        let texts = crate::par::map_owned(work, runs, |run| {
            let mut text = String::new();
            for (widths, item, (amt, bal)) in run {
                text.push_str(&item_as_text(widths, item, &amt, &bal));
            }
            text
        });
        for text in texts {
            out.push_str(&text);
        }
    };
    render(&items[..n_start], start);
    for block in items[n_start..].chunks(BLOCK) {
        render(block, show(block));
    }
    Ok(out)
}

/// For --match: the report's posting whose transaction's description is
/// most like this one, the most recent of equally similar ones
/// (similarPosting).
fn similar_posting<'a, 'j>(
    report: &'a [PostingsReportItem<'j>],
    desc: &str,
) -> Option<&'a PostingsReportItem<'j>> {
    let mut matches: Vec<(f64, &PostingsReportItem)> = (report.iter())
        .map(|i| {
            let score = i
                .txn
                .map_or(0.0, |t| compare_descriptions(desc, &t.description));
            (score, i)
        })
        .filter(|(score, _)| *score > 0.0)
        .collect();
    let date = |i: &PostingsReportItem| i.posting.date_in(i.txn);
    matches.sort_by(|(n1, i1), (n2, i2)| {
        n2.partial_cmp(n1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| date(i2).cmp(&date(i1)))
    });
    matches.first().map(|(_, i)| *i)
}

/// How alike two descriptions are, from 0 to 1, ignoring digits
/// (Register's compareDescriptions).
fn compare_descriptions(s: &str, t: &str) -> f64 {
    let simplify = |s: &str| -> Vec<char> { s.chars().filter(|c| !c.is_ascii_digit()).collect() };
    compare_strings(&simplify(s), &simplify(t))
}

/// The similarity of two strings, from 0 to 1, by the letter pairs of
/// their words, ignoring case (Simon White's letter pairs algorithm, with
/// a modification for short strings) (compareStrings).
fn compare_strings(s1: &[char], s2: &[char]) -> f64 {
    let upper = |c: &char| {
        let mut u = c.to_uppercase();
        match (u.next(), u.next()) {
            (Some(x), None) => x,
            _ => *c,
        }
    };
    match (s1, s2) {
        ([], []) => return 1.0,
        ([_], []) | ([], [_]) => return 0.0,
        ([a], [b]) => return if upper(a) == upper(b) { 1.0 } else { 0.0 },
        _ => {}
    }
    // wordLetterPairs . uppercase
    let pairs = |s: &[char]| -> Vec<(char, char)> {
        let s: Vec<char> = s.iter().map(upper).collect();
        (s.split(|c| is_space(*c)))
            .flat_map(|w| w.windows(2).map(|p| (p[0], p[1])).collect::<Vec<_>>())
            .collect()
    };
    let (p1, p2) = (pairs(s1), pairs(s2));
    // intersect keeps the first list's duplicates
    let common = p1.iter().filter(|p| p2.contains(p)).count();
    2.0 * common as f64 / (p1.len() + p2.len()) as f64
}

pub(crate) mod tests {
    use super::*;
    use crate::dates::Day;
    use crate::reports::opts::RawOpts;
    use crate::reports::periodic::tests::read;

    // tests_Register
    #[cfg_attr(test, test)]
    pub(crate) fn unicode_in_register_layout() {
        let j = read("2009/01/01 * медвежья шкура\n  расходы:покупки  100\n  актив:наличные\n");
        let today = Day::from_ymd(2026, 1, 1).unwrap();
        let opts = CliOpts::from_raw_opts(RawOpts::default(), today, false).unwrap();
        let report = postings_report(&opts.spec, &j).unwrap();
        assert_eq!(
            postings_report_as_text(&opts, &report).unwrap(),
            concat!(
                "2009-01-01 медвежья шкура       расходы:покупки                100           100\n",
                "                                актив:наличные                -100             0\n",
            )
        );
    }

    #[test]
    fn similarity() {
        let c = |a: &str, b: &str| compare_descriptions(a, b);
        assert_eq!(c("", ""), 1.0);
        assert_eq!(c("a", ""), 0.0);
        assert_eq!(c("a", "A"), 1.0);
        assert_eq!(c("France", "French"), 0.4);
        assert_eq!(c("groceries 2024", "Groceries"), 1.0);
        assert!(c("a b", "c").is_nan());
    }
}
