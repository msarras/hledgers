//! The print command (Hledger.Cli.Commands.Print): transactions shown as
//! journal entries, or with --export, the whole journal.

use std::collections::BTreeSet;

use crate::amount::{CommodityStyles, Rounding, styles_with_rounding};
use crate::cli::args::{Arity, Flag, Mode};
use crate::cli::opts::{CliOpts, OUTPUT_FILE_FLAG, OUTPUT_VIEW_FLAG, output_format_flag};
use crate::model::{
    FEESPLIT_POSTING_TAG, GENERATED_POSTING_TAG, Journal, LOTSPLIT_POSTING_TAG, Posting,
    Transaction, comment_add_tag_next_line, source_pos_pair_pretty,
};
use crate::text::is_space;
use crate::write::journal::{ItemRenderer, PostingLayout, journal_items_as_text, show_transaction};

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
        Flag::new(
            &["round"],
            REQ,
            "TYPE",
            "how much rounding or padding should be done when displaying amounts ?\nnone - show original decimal digits,\n       as in journal (default)\nsoft - just add or remove decimal zeros\n       to match precision\nhard - round posting amounts to precision\n       (can unbalance transactions)\nall  - also round cost amounts to precision\n       (can unbalance transactions)",
        ),
        Flag::new(
            &["layout"],
            REQ,
            "hledger1|COL",
            "how should posting amounts be aligned ?\nhledger1 - right-align amounts, as in hledger 1\nCOL      - align decimal marks at column COL (default: 53)",
        ),
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

/// What a milestone not yet done would provide.
fn not_yet(what: &str, milestone: &str) -> String {
    format!("{what} is not supported yet (hledgers milestone {milestone})")
}

/// Run the print command (print').
pub fn print(opts: &CliOpts, mut j: Journal) -> Result<String, String> {
    let raw = &opts.raw;
    if raw.bool("new") {
        return Err(not_yet("print --new", "8"));
    }
    let ropts = &opts.spec.opts;
    if ropts.value.is_some() || ropts.conversion_op.is_some() {
        return Err(not_yet(
            "valuation and cost conversion (-B, -V, -X, --value)",
            "5",
        ));
    }
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
    if let Some(desc) = raw.string("match") {
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
    print_entries(opts, &j)
}

/// The --round option's rounding, if given (roundFromRawOpts).
fn round_from_raw_opts(opts: &CliOpts) -> Result<Option<Rounding>, String> {
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
fn layout_from_raw_opts(opts: &CliOpts) -> Result<PostingLayout, String> {
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
fn print_entries(opts: &CliOpts, j: &Journal) -> Result<String, String> {
    let raw = &opts.raw;
    let ropts = &opts.spec.opts;
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
        let txns = entries_report_unsorted(opts, j);
        return match fmt.as_str() {
            "journal" => {
                let txns: Vec<Transaction> = txns
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
    let mut txns = entries_report_unsorted(opts, j);
    txns.sort_by_key(|t| ropts.transaction_date(t));
    match fmt.as_str() {
        "journal" => {
            let txns: Vec<Transaction> = txns
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
        "ledger" | "beancount" | "sql" => Err(not_yet(&format!("print -O {fmt}"), "8")),
        "csv" | "tsv" | "json" | "html" | "fods" => Err(not_yet(&format!("print -O {fmt}"), "5")),
        _ => Err(unsupported_output_format_error(&fmt)),
    }
}

/// The report's title, if --title gives one, and a blank line, before the
/// output (withTitle).
fn with_title(opts: &CliOpts, body: String) -> String {
    let title = opts.spec.opts.effective_title("");
    if title.is_empty() {
        body
    } else {
        format!("{title}\n\n{body}")
    }
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

/// The transactions the query selects (but not by depth), in journal
/// order, with --invert's signs (entriesReportUnsorted).
fn entries_report_unsorted(opts: &CliOpts, j: &Journal) -> Vec<Transaction> {
    let q = opts.spec.query.clone().filter(|q| !q.is_depth());
    j.txns
        .iter()
        .filter(|t| q.matches_transaction(t))
        .cloned()
        .map(|mut t| {
            if opts.spec.opts.invert {
                t.negate();
            }
            t
        })
        .collect()
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
