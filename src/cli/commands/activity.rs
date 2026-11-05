//! The activity command (Hledger.Cli.Commands.Activity): a bar chart of
//! the number of transactions in each report period (weekly by default),
//! or with --postings, of postings by account; the balance command's
//! counting chart (`balance --count --chart -W`).

use crate::account::DepthSpec;
use crate::cli::args::{Arity, Flag, Mode};
use crate::cli::commands::balance::{CHART_WIDTH_FLAG, balance};
use crate::cli::opts::{CliOpts, OUTPUT_FILE_FLAG, OUTPUT_VIEW_FLAG, output_format_flag};
use crate::dates::Interval;
use crate::model::Journal;
use crate::reports::opts::{BalanceCalculation, ReportOpts, ReportSpec};

const NONE: Arity = Arity::None;

pub static MODE: Mode = Mode {
    name: "activity",
    aliases: &[],
    flags: &[
        output_format_flag(
            "select the output format. Supported formats:\ntxt, svg, html, csv, tsv, json.",
        ),
        OUTPUT_FILE_FLAG,
        OUTPUT_VIEW_FLAG,
        CHART_WIDTH_FLAG,
        Flag::new(
            &["txns"],
            NONE,
            "",
            "count transactions, showing just the total (default)",
        ),
        Flag::new(
            &["postings"],
            NONE,
            "",
            "count postings, showing each account's count",
        ),
        Flag::new(
            &["sort-amount"],
            NONE,
            "",
            "with --postings, sort accounts by posting count, largest first",
        )
        .short(&['S']),
        Flag::new(
            &["total"],
            NONE,
            "",
            "show the total count, below the chart",
        )
        .short(&['T'])
        .sets(&["row-total"]),
        Flag::new(
            &["average"],
            NONE,
            "",
            "show the average count per period, below the chart (and as a line in SVG charts)",
        )
        .short(&['A']),
    ],
};

/// Run the activity command (activity): the balance command's chart of
/// the number of transactions per report period (weekly by default), as a
/// single total, since a transaction's postings may be in several
/// accounts (like depth 0); or with --postings, of postings, by account.
/// The last of --txns and --postings wins.
pub fn activity(opts: &CliOpts, j: &Journal) -> Result<String, String> {
    let txns = (opts.raw)
        .choice(|n| match n {
            "txns" => Some(true),
            "postings" => Some(false),
            _ => None,
        })
        .unwrap_or(true);
    let ropts = &opts.spec.opts;
    let ropts = ReportOpts {
        balance_calc: if txns {
            BalanceCalculation::TransactionsCount
        } else {
            BalanceCalculation::PostingsCount
        },
        interval: match ropts.interval {
            Interval::NoInterval => Interval::Weeks(1),
            ref i => i.clone(),
        },
        depth: if txns {
            DepthSpec {
                flat: Some(0),
                regexps: Vec::new(),
            }
        } else {
            ropts.depth.clone()
        },
        ..ropts.clone()
    };
    let mut raw = opts.raw.clone();
    raw.0.push(("chart".to_string(), "auto".to_string()));
    let opts = CliOpts {
        raw,
        spec: ReportSpec::new(opts.spec.today, ropts)?,
        ..opts.clone()
    };
    balance(&opts, j)
}
