//! The print command (Hledger.Cli.Commands.Print): transactions shown as
//! journal entries, or with --export, the whole journal.

use std::collections::{BTreeMap, BTreeSet};

use crate::amount::render::{AmountFormat, show_amount_b};
use crate::amount::{Amount, CommodityStyles, Rounding, Sym, styles_with_rounding};
use crate::cli::anchor::set_account_anchor;
use crate::cli::args::{Arity, Flag, Mode};
use crate::cli::commands::not_yet;
use crate::cli::io::with_title;
use crate::cli::opts::{CliOpts, OUTPUT_FILE_FLAG, OUTPUT_VIEW_FLAG, output_format_flag};
use crate::dates::Day;
use crate::model::{
    FEESPLIT_POSTING_TAG, GENERATED_POSTING_TAG, Journal, LOTSPLIT_POSTING_TAG, Posting,
    Transaction, comment_add_tag_next_line, source_pos_pair_pretty,
};
use crate::reports::entries::{entries_report, entries_report_unsorted};
use crate::text::{is_space, strip};
use crate::write::csv::{print_csv, print_tsv};
use crate::write::fods::{locale_encoding_name, print_fods};
use crate::write::html::titled_table_html;
use crate::write::journal::{
    ItemRenderer, PostingLayout, journal_items_as_text, show_account_name, show_transaction,
};
use crate::write::json::to_json_text;
use crate::write::spreadsheet::{
    Cell, CellType, add_header_borders, add_row_span_header, cell_from_amount, integer_cell,
    raw_table_content,
};

const NONE: Arity = Arity::None;
const REQ: Arity = Arity::Req;

pub static MODE: Mode = Mode {
    name: "print",
    aliases: &[],
    flags: &[
        Flag::new(
            &["all"],
            NONE,
            "",
            "show all details (--explicit --lots --verbose-tags)",
        )
        .short(&['a'])
        .sets(&["explicit", "lots", "verbose-tags"]),
        Flag::new(&["explicit"], NONE, "", "show all inferred info explicitly").short(&['x']),
        Flag::new(
            &["verbose-tags"],
            NONE,
            "",
            "add tags indicating generated/modified data",
        ),
        Flag::new(
            &["invert"],
            NONE,
            "",
            "display all amounts with reversed sign",
        ),
        Flag::new(
            &["locations"],
            NONE,
            "",
            "add tags showing file paths and line numbers",
        ),
        Flag::new(
            &["export"],
            NONE,
            "",
            "reproduce the whole journal, with directives and comments preserved and included files inlined",
        ),
        Flag::new(
            &["match"],
            REQ,
            "DESC",
            "fuzzy search for one recent transaction with description closest to DESC",
        )
        .short(&['m']),
        Flag::new(
            &["new"],
            NONE,
            "",
            "show only newer-dated transactions added in each file since last run",
        ),
        ROUND_FLAG,
        LAYOUT_FLAG,
        Flag::new(
            &["base-url"],
            REQ,
            "URLPREFIX",
            "in html output, generate links to hledger-web, with this prefix. (Usually the base url shown by hledger-web; can also be relative.)",
        ),
        output_format_flag(
            "select the output format. Supported formats:\njournal, ledger, beancount, csv, tsv, html, fods, json, sql.",
        ),
        OUTPUT_FILE_FLAG,
        OUTPUT_VIEW_FLAG,
    ],
};

/// The --round flag (roundFlag).
pub const ROUND_FLAG: Flag = Flag::new(
    &["round"],
    REQ,
    "TYPE",
    "how much rounding or padding should be done when displaying amounts ?\nnone - show original decimal digits,\n       as in journal (default)\nsoft - just add or remove decimal zeros\n       to match precision\nhard - round posting amounts to precision\n       (can unbalance transactions)\nall  - also round cost amounts to precision\n       (can unbalance transactions)",
);

/// The --layout flag (layoutFlag).
pub const LAYOUT_FLAG: Flag = Flag::new(
    &["layout"],
    REQ,
    "hledger1|COL",
    "how should posting amounts be aligned ?\nhledger1 - right-align amounts, as in hledger 1\nCOL      - align decimal marks at column COL (default: 53)",
);

/// Run the print command (print').
pub fn print(opts: &CliOpts, mut j: Journal) -> Result<String, String> {
    let raw = &opts.raw;
    // show amounts with all their decimal digits, except generated gain postings
    for t in &mut j.txns {
        for p in &mut t.postings {
            if !p.tags.iter().any(|(n, v)| n == "_ptype" && v == "gain") {
                p.amount = std::mem::take(&mut p.amount).with_full_precision_capped();
            }
        }
    }
    if raw.bool("locations") {
        for t in &mut j.txns {
            let tag = ("location".to_string(), source_pos_pair_pretty(&t.pos));
            t.comment = comment_add_tag_next_line(&t.comment, &tag);
        }
    }
    let j = journal_apply_match_opt(opts, j)?;
    print_entries(opts, j)
}

/// With --match, the journal with just the recent transaction whose
/// description is most like its value, or an error if there is none
/// (journalApplyMatchOpt).
pub fn journal_apply_match_opt(opts: &CliOpts, mut j: Journal) -> Result<Journal, String> {
    if let Some(desc) = opts.raw.string("match") {
        match journal_similar_transaction(opts, &j, desc) {
            Some(t) => j.txns = vec![t],
            None => {
                return Err(format!(
                    "no transactions found with descriptions like {}",
                    crate::text::show_string(desc)
                ));
            }
        }
    }
    Ok(j)
}

/// The --round option's rounding, if given (roundFromRawOpts).
pub fn round_from_raw_opts(opts: &CliOpts) -> Result<Option<Rounding>, String> {
    let mut r = None;
    for v in opts.raw.list("round") {
        r = Some(match v {
            "none" => Rounding::No,
            "soft" => Rounding::Soft,
            "hard" => Rounding::Hard,
            "all" => Rounding::All,
            _ => {
                return Err(format!(
                    "--round's value should be none, soft, hard or all; got: {v}"
                ));
            }
        });
    }
    Ok(r)
}

/// The posting layout of --layout (layoutFromRawOpts).
pub fn layout_from_raw_opts(opts: &CliOpts) -> Result<PostingLayout, String> {
    match opts.raw.string("layout") {
        None => Ok(PostingLayout::default()),
        Some("hledger1") => Ok(PostingLayout::Hledger1),
        Some(s) => s.trim().parse::<usize>().map(PostingLayout::Decimal).map_err(|_| {
            format!("--layout's value should be 'hledger1' or a positive integer column number; got: {s}")
        }),
    }
}

/// Render the selected transactions, or with --export, the journal
/// (printEntries).
fn print_entries(opts: &CliOpts, j: Journal) -> Result<String, String> {
    let raw = &opts.raw;
    let styles = match round_from_raw_opts(opts)? {
        Some(r) => styles_with_rounding(r, &j.commodity_styles()),
        None => j.commodity_styles(),
    };
    let fmt = match opts.output_format().as_str() {
        "txt" => "journal".to_string(),
        f => f.to_string(),
    };
    let layout = layout_from_raw_opts(opts)?;
    let style_all = |ts: Vec<Transaction>, styles: &CommodityStyles| -> Vec<Transaction> {
        ts.into_iter()
            .map(|mut t| {
                t.style_amounts(styles);
                t
            })
            .collect()
    };
    if raw.bool("export") {
        return match fmt.as_str() {
            "journal" => {
                let j = entries_report_unsorted(&opts.spec, j)?;
                let txns: Vec<Transaction> = j
                    .txns
                    .into_iter()
                    .map(|t| maybe_original_amounts(opts, false, t))
                    .collect();
                let txns = style_all(txns, &styles);
                let forecasting = opts.input.forecast.is_some();
                let auto = opts.input.auto;
                let show = |t: &Transaction| {
                    let s = show_transaction(t, layout, false);
                    s.strip_suffix('\n').map(str::to_string).unwrap_or(s)
                };
                let directive = |d: &str| {
                    let applied =
                        (forecasting && d.starts_with('~')) || (auto && d.starts_with('='));
                    (!applied).then(|| d.to_string())
                };
                let renderer = ItemRenderer {
                    transaction: &show,
                    directive: &directive,
                };
                Ok(journal_items_as_text(&renderer, &j.items, &txns))
            }
            "ledger" | "beancount" => Err(not_yet(&format!("print -O {fmt}"), "8")),
            _ => Err(
                "print --export supports only the journal, ledger and beancount output formats"
                    .to_string(),
            ),
        };
    }
    match fmt.as_str() {
        "journal" => {
            let txns: Vec<Transaction> = entries_report(&opts.spec, j)?
                .into_iter()
                .map(|t| maybe_original_amounts(opts, true, t))
                .collect();
            let txns = style_all(txns, &styles);
            let mut out = String::new();
            for t in &txns {
                out.push_str(&show_transaction(t, layout, false));
            }
            Ok(with_title(opts, out))
        }
        "csv" | "tsv" => {
            let txns = style_all(entries_report(&opts.spec, j)?, &styles);
            let rows = raw_table_content(entries_report_as_spreadsheet(
                &AmountFormat::MACHINE,
                None,
                &[],
                &txns,
            ));
            Ok(if fmt == "csv" {
                print_csv(&rows)
            } else {
                print_tsv(&rows)
            })
        }
        "html" | "fods" => {
            let txns = style_all(entries_report(&opts.spec, j)?, &styles);
            let ropts = &opts.spec.opts;
            let rows = entries_report_as_spreadsheet(
                &AmountFormat::ONE_LINE_NO_COST,
                ropts.balance_base_url.as_deref(),
                &ropts.query_string,
                &txns,
            );
            Ok(if fmt == "html" {
                titled_table_html("", &rows) + "\n"
            } else {
                let tables = BTreeMap::from([("Print".to_string(), ((1, 0), rows))]);
                print_fods(locale_encoding_name(), &tables)
            })
        }
        "json" => Ok(to_json_text(&style_all(
            entries_report(&opts.spec, j)?,
            &styles,
        ))),
        "ledger" | "beancount" | "sql" => Err(not_yet(&format!("print -O {fmt}"), "8")),
        _ => Err(unsupported_output_format_error(&fmt)),
    }
}

/// Transactions as a table with a row for each posting amount, its
/// transaction's fields repeated (merged down) on each, and a heading row
/// (entriesReportAsSpreadsheet). The transaction index groups the rows
/// back into transactions. With a base url, account names link to their
/// registers, restricted to the query.
pub fn entries_report_as_spreadsheet(
    fmt: &AmountFormat,
    base_url: Option<&str>,
    query: &[String],
    txns: &[Transaction],
) -> Vec<Vec<Cell<String>>> {
    const HEADINGS: [&str; 14] = [
        "txnidx",
        "date",
        "date2",
        "status",
        "code",
        "description",
        "comment",
        "account",
        "amount",
        "commodity",
        "debit",
        "credit",
        "posting-status",
        "posting-comment",
    ];
    let mut rows = vec![add_header_borders(
        HEADINGS.iter().map(|h| Cell::header(h)).collect(),
    )];
    for t in txns {
        rows.extend(transaction_to_spreadsheet(fmt, base_url, query, t));
    }
    rows
}

/// A transaction's rows: its postings', each after the transaction's
/// fields (transactionToSpreadsheet).
fn transaction_to_spreadsheet(
    fmt: &AmountFormat,
    base_url: Option<&str>,
    query: &[String],
    t: &Transaction,
) -> Vec<Vec<Cell<String>>> {
    let date_cell = |d: Day| Cell {
        typ: CellType::Date,
        ..Cell::new(d.to_string())
    };
    let common = [
        integer_cell(t.index),
        date_cell(t.date),
        t.date2.map_or_else(Cell::empty, date_cell),
        Cell::new(t.status.to_string()),
        Cell::new(t.code.clone()),
        Cell::new(t.description.clone()),
        Cell::new(strip(&t.comment).to_string()),
    ];
    let postings = (t.postings.iter())
        .flat_map(|p| posting_to_spreadsheet(fmt, base_url, query, p))
        .collect();
    add_row_span_header(&common, postings)
}

/// A posting's rows, one per amount: the account, the amount without its
/// commodity, digit group marks or cost, the commodity, the amount again
/// as a debit or (positive) credit, the status and the comment
/// (postingToSpreadsheet).
fn posting_to_spreadsheet(
    fmt: &AmountFormat,
    base_url: Option<&str>,
    query: &[String],
    p: &Posting,
) -> Vec<Vec<Cell<String>>> {
    let amount_cell = |a: Amount| {
        let text = show_amount_b(&AmountFormat::MACHINE, &a).s;
        cell_from_amount(fmt, "amount", text, a)
    };
    let account = show_account_name(p);
    let status = p.status.to_string();
    let comment = strip(&p.comment);
    (p.amount.amounts().into_iter())
        .map(|a| {
            let commodity = a.commodity.to_string();
            let negative = a.is_negative();
            let bare = Amount {
                commodity: Sym::from(""),
                ..a
            }
            .strip_cost();
            let (debit, credit) = if negative {
                (Cell::empty(), amount_cell(-&bare))
            } else {
                (amount_cell(bare.clone()), Cell::empty())
            };
            vec![
                set_account_anchor(base_url, query, &p.account, Cell::new(account.clone())),
                amount_cell(bare),
                Cell::new(commodity),
                debit,
                credit,
                Cell::new(status.clone()),
                Cell::new(comment.to_string()),
            ]
        })
        .collect()
}

/// The error for an output format a command doesn't support
/// (unsupportedOutputFormatError).
pub fn unsupported_output_format_error(fmt: &str) -> String {
    let mut msg = format!(
        "Sorry, output format \"{fmt}\" is unrecognised or not yet supported for this kind of report."
    );
    let ext = fmt.rsplit('.').next().unwrap_or("");
    if fmt.contains('.')
        && [
            "txt",
            "beancount",
            "csv",
            "json",
            "html",
            "ledger",
            "sql",
            "tsv",
            "fods",
            "svg",
        ]
        .contains(&ext)
    {
        msg.push_str(&format!(
            "\nTo write to a file named {fmt}, use -o (--output-file), not -O (--output-format)."
        ));
    }
    msg
}

/// The transaction with its postings shown as written, unless explicit
/// amounts are wanted (maybeoriginalamounts).
fn maybe_original_amounts(
    opts: &CliOpts,
    show_inferred_basis: bool,
    t: Transaction,
) -> Transaction {
    let raw = &opts.raw;
    if raw.bool("explicit")
        || opts.input.infer_costs
        || opts.spec.opts.value.is_some()
        || opts.spec.opts.conversion_op.is_some()
    {
        return t;
    }
    let kept_feesplit = |p: &Posting| {
        p.has_tag(FEESPLIT_POSTING_TAG)
            && (raw.bool("lots") || p.amount.amounts_raw().iter().any(|a| a.cost.is_some()))
    };
    if t.postings.iter().any(kept_feesplit) {
        return t;
    }
    transaction_with_mostly_original_postings(show_inferred_basis, t)
}

/// The transaction with its postings as they were written, but with their
/// current accounts, comments and tags, and the amounts of generated
/// postings (transactionWithMostlyOriginalPostings).
pub fn transaction_with_mostly_original_postings(
    show_inferred_basis: bool,
    mut t: Transaction,
) -> Transaction {
    t.postings.retain(|p| !p.has_tag(FEESPLIT_POSTING_TAG));
    for p in &mut t.postings {
        let orig = p.original().clone();
        let with_inferred_basis = |ma: crate::amount::MixedAmount| {
            if !show_inferred_basis || crate::account::split_lot_subaccount(&p.account).is_some() {
                return ma;
            }
            match (ma.amounts_raw(), p.amount.amounts_raw()) {
                ([oa], [ca]) if oa.cost_basis.is_none() && ca.cost_basis.is_some() => {
                    let mut a = oa.clone();
                    a.cost_basis = ca.cost_basis.clone();
                    crate::amount::MixedAmount::from_amount(a)
                }
                _ => ma,
            }
        };
        let amount = if p.has_tag(GENERATED_POSTING_TAG) {
            p.amount.clone()
        } else if p.has_tag(LOTSPLIT_POSTING_TAG) {
            with_inferred_basis(if orig.has_amount() {
                match (orig.amount.amounts_raw(), p.amount.amounts_raw()) {
                    ([oa], [ca]) => {
                        let mut a = oa.clone();
                        a.quantity = ca.quantity.clone();
                        crate::amount::MixedAmount::from_amount(a)
                    }
                    _ => p.amount.clone(),
                }
            } else {
                p.amount.clone()
            })
        } else {
            with_inferred_basis(orig.amount.clone())
        };
        let assertion = if orig.account == p.account {
            orig.assertion.clone()
        } else {
            p.assertion.clone()
        };
        *p = Posting {
            account: p.account.clone(),
            amount,
            comment: std::mem::take(&mut p.comment),
            tags: std::mem::take(&mut p.tags),
            assertion,
            ..orig
        };
    }
    t
}

/// The transaction whose description is most like this one, nearer dates
/// preferred, among those the flags' query selects
/// (journalSimilarTransaction).
fn journal_similar_transaction(opts: &CliOpts, j: &Journal, desc: &str) -> Option<Transaction> {
    let today = opts.spec.today;
    let q = opts.spec.opts.query_from_flags();
    let mut scored: Vec<(f64, &Transaction)> = j
        .txns
        .iter()
        .filter(|t| q.matches_transaction(t))
        .map(|t| (compare_descriptions(desc, &t.description), t))
        .filter(|(s, _)| *s > 0.0)
        .map(|(s, t)| {
            let distance = (today.diff_days(t.date)).abs() as f64;
            (100.0 * s - distance / 4.0, t)
        })
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.first().map(|(_, t)| (*t).clone())
}

/// How alike two descriptions are, from 0 to 1.5: their letter pairs'
/// similarity ignoring digits, plus 0.5 if the first is part of the second
/// (compareDescriptions).
pub fn compare_descriptions(a: &str, b: &str) -> f64 {
    let bonus = if b.contains(a) { 0.5 } else { 0.0 };
    let simplify = |s: &str| {
        s.chars()
            .filter(|c| !c.is_ascii_digit())
            .collect::<String>()
    };
    bonus + compare_strings(&simplify(a), &simplify(b))
}

/// The similarity of two strings by their words' letter pairs, ignoring
/// case (compareStrings).
fn compare_strings(s1: &str, s2: &str) -> f64 {
    let (c1, c2): (Vec<char>, Vec<char>) = (s1.chars().collect(), s2.chars().collect());
    match (c1.as_slice(), c2.as_slice()) {
        ([], []) => return 1.0,
        ([_], []) | ([], [_]) => return 0.0,
        ([a], [b]) => {
            return if a.to_uppercase().eq(b.to_uppercase()) {
                1.0
            } else {
                0.0
            };
        }
        _ => {}
    }
    let pairs = |s: &str| -> BTreeSet<(char, char)> {
        let upper = s.to_uppercase();
        upper
            .split(is_space)
            .filter(|w| !w.is_empty())
            .flat_map(|w| {
                let cs: Vec<char> = w.chars().collect();
                cs.windows(2).map(|p| (p[0], p[1])).collect::<Vec<_>>()
            })
            .collect()
    };
    let (p1, p2) = (pairs(s1), pairs(s2));
    let common = p1.intersection(&p2).count() as f64;
    let total = (p1.len() + p2.len()) as f64;
    2.0 * common / total
}
