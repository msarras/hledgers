//! The stats command (Hledger.Cli.Commands.Stats): statistics about the
//! journal, for the whole report period or each report interval, and
//! how long they took.

use std::collections::{BTreeSet, HashSet};

use crate::account::account_name_level;
use crate::cli::args::{Arity, Flag, Mode};
use crate::cli::io::{warn_if_large_multi_period_report, with_title};
use crate::cli::opts::CliOpts;
use crate::dates::{DateSpan, Day, maybe_day_partition_to_date_spans};
use crate::model::{Journal, PostingRef, Transaction};
use crate::query::Query;
use crate::text::{display_width, fit_text, show_ffloat};

const NONE: Arity = Arity::None;
const REQ: Arity = Arity::Req;

pub static MODE: Mode = Mode {
    name: "stats",
    aliases: &[],
    flags: &[
        Flag::new(&["oneline"], NONE, "", "show a single line of output").short(&['q']),
        Flag::new(&["verbose"], NONE, "", "show more detailed output").short(&['v']),
        Flag::new(&["output-file"], REQ, "FILE", "write output to FILE.").short(&['o']),
    ],
    doc: crate::command_doc!("Stats"),
    ..Mode::DEFAULT
};

/// The width the labels are padded to (labelwidth).
const LABEL_WIDTH: usize = 20;

/// Run the stats command (stats): the statistics of the postings the query
/// matches in each report period (one, without a report interval), unless
/// --oneline; then a line of run time statistics, with --oneline also the
/// version and the main file's name, tab-separated.
pub fn stats(opts: &CliOpts, j: Journal) -> Result<String, String> {
    warn_if_large_multi_period_report(opts, &j);
    let raw = &opts.raw;
    let oneline = raw.bool("oneline");
    let verbose = raw.bool("verbose");
    let (_, periods) = opts.spec.report_span_both_dates(&j);
    let j = ledger_journal(&opts.spec.query, j);
    let (texts, counts): (Vec<String>, Vec<usize>) =
        maybe_day_partition_to_date_spans(periods.as_ref())
            .iter()
            .map(|span| ledger_stats(verbose, &j, opts.spec.today, span))
            .unzip();
    let mut out = if oneline {
        String::new()
    } else {
        texts.join("\n")
    };
    if oneline && opts.debug > 0 {
        out.push_str(&vec!["."; 21].join("       "));
        out.push('\n');
    }
    // the run time, from the program start to now, all but the output
    let elapsed = opts.start_time.elapsed().as_secs_f32();
    let txns: usize = counts.iter().sum();
    let mut items = Vec::new();
    if oneline {
        let main_file = j.files.first().map_or("(unknown)", |(f, _)| f.as_str());
        items.push(take_file_name(main_file).to_string());
    }
    items.push(format!("{} s elapsed", show_ffloat(2, elapsed)));
    items.push(format!("{} txns/s", show_ffloat(0, txns as f32 / elapsed)));
    if oneline {
        let version = crate::cli::version::progname_and_version("");
        let version = version.trim_start();
        out.push_str(&format!("{version}\t{}\n", items.join("\t")));
    } else {
        let label = format!("{:<LABEL_WIDTH$}: ", "Runtime stats");
        out.push_str(&format!("{label}{}\n", items.join(", ")));
    }
    Ok(with_title(opts, out))
}

/// The journal with just the postings, and the parts of their amounts,
/// the query matches, all its transactions kept (ledgerFromJournal's
/// journal: filterJournalPostings, filterJournalAmounts).
fn ledger_journal(q: &Query, mut j: Journal) -> Journal {
    let nodepthq = q.clone().filter(|q| !q.is_depth());
    let curq = q.clone().filter(Query::is_cur);
    let depthq = q.clone().filter(Query::is_depth);
    let mut txns = std::mem::take(&mut j.txns);
    let types = |a: &str| j.account_type(a);
    let filter_postings = |q: &Query, t: &mut Transaction| {
        let keep: Vec<bool> = (t.postings.iter())
            .map(|p| q.matches_posting_extra(&types, PostingRef::new(t, p)))
            .collect();
        let mut keep = keep.into_iter();
        t.postings.retain(|_| keep.next().unwrap_or(false));
    };
    for t in &mut txns {
        filter_postings(&nodepthq, t);
        if !curq.is_null() {
            t.postings.retain_mut(|p| {
                p.amount = std::mem::take(&mut p.amount).filter(|a| curq.matches_amount(a));
                !p.amount.amounts_raw().is_empty()
            });
        }
        filter_postings(&depthq, t);
    }
    j.txns = txns;
    j
}

/// The statistics of the journal's transactions in this period, as lines
/// of labelled values, and their number (showLedgerStats).
fn ledger_stats(verbose: bool, j: &Journal, today: Day, span: &DateSpan) -> (String, usize) {
    let path = j.files.first().map_or("(unknown)", |(f, _)| f.as_str());
    let included: Vec<&str> = j.files.iter().skip(1).map(|(f, _)| f.as_str()).collect();
    let txns: Vec<_> = j.txns.iter().filter(|t| span.contains(t.date)).collect();
    let postings = || txns.iter().flat_map(|t| &t.postings);
    let accounts: HashSet<&str> = postings().map(|p| &*p.account).collect();
    let commodities: BTreeSet<&str> = postings()
        .flat_map(|p| p.amount.amounts_raw())
        .map(|a| &*a.commodity)
        .collect();
    let last = txns.iter().map(|t| t.date).max();
    let n = txns.len();
    let days = span.days().unwrap_or(0);
    let rate = |count: usize, days: i64| {
        let r = if days == 0 {
            0.0
        } else {
            count as f64 / days as f64
        };
        format!("{count} ({} per day)", show_ffloat(1, r))
    };
    let within = |d: i64| {
        txns.iter()
            .filter(|t| t.date >= today.add_days(-d) && t.date <= today)
            .count()
    };
    let show_end = |e: Option<Day>| e.map_or(String::new(), |d| d.to_string());
    let elapsed = last.map_or(String::new(), |d| {
        let ago = today.diff_days(d);
        let unit = if ago.abs() == 1 { "day" } else { "days" };
        let direction = if ago >= 0 { "ago" } else { "from now" };
        format!(" ({} {unit} {direction})", ago.abs())
    });
    let base_currency = match j.base_currency() {
        None => "none".to_string(),
        Some((sym, code)) if sym == code => code.to_string(),
        Some((sym, code)) => format!("{sym} ({code})"),
    };
    let verbose_list = |items: Vec<&str>| {
        if verbose {
            format!(" ({})", items.join(", "))
        } else {
            String::new()
        }
    };
    let price_commodities: BTreeSet<&str> =
        j.price_directives.iter().map(|pd| &*pd.commodity).collect();
    let rows: Vec<(&str, String)> = vec![
        (
            "Main file",
            if verbose {
                path.to_string()
            } else {
                format!(".../{}", take_file_name(path))
            },
        ),
        (
            "Included files",
            if verbose {
                included.iter().map(|f| format!("{f}\n")).collect()
            } else {
                included.len().to_string()
            },
        ),
        (
            "Txns span",
            format!(
                "{} to {} ({days} days)",
                show_end(span.start()),
                show_end(span.end())
            ),
        ),
        (
            "Last txn",
            format!(
                "{}{elapsed}",
                last.map_or("none".to_string(), |d| d.to_string())
            ),
        ),
        ("Txns", rate(n, days)),
        ("Txns last 30 days", rate(within(30), 30)),
        ("Txns last 7 days", rate(within(7), 7)),
        (
            "Payees/descriptions",
            txns.iter()
                .map(|t| t.description.as_str())
                .collect::<HashSet<_>>()
                .len()
                .to_string(),
        ),
        (
            "Accounts",
            format!(
                "{} (depth {})",
                accounts.len(),
                accounts
                    .iter()
                    .map(|a| account_name_level(a))
                    .max()
                    .unwrap_or(0)
            ),
        ),
        (
            "Commodities",
            format!(
                "{}{}",
                commodities.len(),
                verbose_list(commodities.iter().copied().collect())
            ),
        ),
        ("Base currency", base_currency),
        (
            "Market prices",
            format!(
                "{}{}",
                j.price_directives.len(),
                verbose_list(price_commodities.into_iter().collect())
            ),
        ),
    ];
    let mut out = String::new();
    for (label, value) in rows {
        out.push_str(&show_row(label, &value));
        out.push('\n');
    }
    (out, n)
}

/// A label, padded or cut to the label width, and a value, whose lines
/// after the first are indented below it and all padded to the widest
/// (renderRow of two top-left aligned cells without borders).
fn show_row(label: &str, value: &str) -> String {
    let label = format!(
        "{}: ",
        fit_text(
            Some(LABEL_WIDTH as isize),
            Some(LABEL_WIDTH as isize),
            false,
            true,
            label
        )
    );
    let lines: Vec<&str> = if value.is_empty() {
        vec![""]
    } else {
        value.lines().collect()
    };
    let width = lines.iter().map(|l| display_width(l)).max().unwrap_or(0);
    let indent = " ".repeat(display_width(&label));
    lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let pad = " ".repeat(width - display_width(l));
            format!("{}{l}{pad}", if i == 0 { &label } else { &indent })
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The last component of a file path (takeFileName).
fn take_file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}
