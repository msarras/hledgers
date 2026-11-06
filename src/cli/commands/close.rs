//! The close command (Hledger.Cli.Commands.Close): transactions that
//! close accounts' balances at the end of a period, open them again at
//! the start of the next, retain earnings, or assert or assign balances,
//! printed as journal entries.

use std::collections::{BTreeMap, BTreeSet};

use crate::account::{
    AccountName, AccountType, account_name_to_account_only_regex, lot_base_account,
};
use crate::amount::{
    Amount, CommodityStyles, CostBasis, MixedAmount, Rounding, Sym, styles_with_rounding,
};
use crate::cli::args::{Arity, Flag, Mode};
use crate::cli::commands::print::{
    LAYOUT_FLAG, ROUND_FLAG, layout_from_raw_opts, round_from_raw_opts,
};
use crate::cli::io::with_title;
use crate::cli::opts::CliOpts;
use crate::decimal::Decimal;
use crate::lots::{lot_subaccount_name, parse_lot_name, show_lot_name};
use crate::model::{BalanceAssertion, Journal, Posting, SourcePos, Transaction};
use crate::query::Query;
use crate::reports::opts::{AccountListMode, BalanceAccumulation, ReportOpts, ReportSpec};
use crate::reports::periodic::balance_report;
use crate::valuation::ConversionOp;
use crate::write::journal::show_transaction;

const NONE: Arity = Arity::None;
const REQ: Arity = Arity::Req;
const OPT: Arity = Arity::Opt;

pub static MODE: Mode = Mode {
    name: "close",
    aliases: &["equity"],
    flags: &[
        Flag::new(
            &["clopen"],
            OPT,
            "TAGVAL",
            "show closing and opening balances transactions, for AL accounts by default",
        ),
        Flag::new(
            &["close"],
            OPT,
            "TAGVAL",
            "show just a closing balances transaction",
        ),
        Flag::new(
            &["open"],
            OPT,
            "TAGVAL",
            "show just an opening balances transaction",
        ),
        Flag::new(
            &["assert"],
            OPT,
            "TAGVAL",
            "show a balance assertions transaction",
        ),
        Flag::new(
            &["assign"],
            OPT,
            "TAGVAL",
            "show a balance assignments transaction",
        ),
        Flag::new(
            &["retain"],
            OPT,
            "TAGVAL",
            "show a retain earnings transaction, for RX accounts by default",
        ),
        Flag::new(&["explicit"], NONE, "", "show all amounts explicitly").short(&['x']),
        Flag::new(
            &["show-costs"],
            NONE,
            "",
            "show amounts with different costs separately",
        ),
        Flag::new(
            &["interleaved"],
            NONE,
            "",
            "show source and destination postings together",
        ),
        Flag::new(&["assertion-type"], REQ, "TYPE", "=, ==, =* or ==*"),
        Flag::new(
            &["close-desc"],
            REQ,
            "DESC",
            "set closing transaction's description",
        ),
        Flag::new(
            &["close-acct"],
            REQ,
            "ACCT",
            "set closing transaction's destination account",
        ),
        Flag::new(
            &["open-desc"],
            REQ,
            "DESC",
            "set opening transaction's description",
        ),
        Flag::new(
            &["open-acct"],
            REQ,
            "ACCT",
            "set opening transaction's source account",
        ),
        ROUND_FLAG,
        LAYOUT_FLAG,
        // old spellings, still supported
        Flag::new(&["closing"], NONE, "", "old spelling of --close")
            .sets(&["close"])
            .hidden(),
        Flag::new(&["opening"], NONE, "", "old spelling of --open")
            .sets(&["open"])
            .hidden(),
        Flag::new(&["migrate"], NONE, "", "old spelling of --clopen")
            .sets(&["clopen"])
            .hidden(),
        Flag::new(&["close-to"], REQ, "ACCT", "old spelling of --close-acct")
            .sets(&["close-acct"])
            .hidden(),
        Flag::new(&["open-from"], REQ, "ACCT", "old spelling of --open-acct")
            .sets(&["open-acct"])
            .hidden(),
    ],
    args: Some("[--close|--open|--clopen|--assign|--assert|--retain] [ACCTQUERY]"),
    doc: crate::command_doc!("Close"),
    ..Mode::DEFAULT
};

/// What close shows (CloseMode).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloseMode {
    Clopen,
    Close,
    Open,
    Assign,
    Assert,
    Retain,
}

impl CloseMode {
    /// The mode's flag name, which is also its tag's name in the
    /// transactions' comments (except Clopen, Close and Open's, clopen).
    fn flag(self) -> &'static str {
        match self {
            CloseMode::Clopen => "clopen",
            CloseMode::Close => "close",
            CloseMode::Open => "open",
            CloseMode::Assign => "assign",
            CloseMode::Assert => "assert",
            CloseMode::Retain => "retain",
        }
    }
}

/// Run the close command (close): the transactions of the mode the last
/// mode flag selects (--close by default), for the accounts the query
/// arguments' account and type: terms select (asset and liability
/// accounts by default, revenue and expense accounts with --retain),
/// except the equity account they transfer to; with the historical
/// balances on the closing date: the report period's last day, or
/// yesterday or the journal's last day, whichever is later. The opening
/// date is the next day. Each transaction's comment has a tag naming the
/// mode, with the mode flag's value, or for clopen, the journal file's
/// name with its number incremented.
pub fn close(opts: &CliOpts, j: &Journal) -> Result<String, String> {
    use CloseMode::*;
    let raw = &opts.raw;
    let mode = raw
        .choice(|n| {
            [Clopen, Close, Open, Assign, Assert, Retain]
                .into_iter()
                .find(|m| m.flag() == n)
        })
        .unwrap_or(Close);
    let (default_acctsq, default_close_acct) = if mode == Retain {
        (
            Query::Type(vec![AccountType::Revenue, AccountType::Expense]),
            "equity:retained earnings",
        )
    } else {
        (
            Query::Type(vec![AccountType::Asset, AccountType::Liability]),
            "equity:opening/closing balances",
        )
    };
    let close_acct = raw.string("close-acct").unwrap_or(default_close_acct);
    let open_acct = raw.string("open-acct").unwrap_or(close_acct);
    // a recognisable tag on all the generated transactions
    let tagval = raw.string(mode.flag()).unwrap_or("");
    let comment = match mode {
        Assert => format!("assert:{tagval}"),
        Assign => format!("assign:{tagval}"),
        Retain => format!("retain:{tagval}"),
        _ if tagval.is_empty() => format!("clopen:{}", next_file_base_name(j)?),
        _ => format!("clopen:{tagval}"),
    };
    let ropts = ReportOpts {
        balance_accum: BalanceAccumulation::Historical,
        account_list_mode: AccountListMode::Flat,
        ..opts.spec.opts.clone()
    };
    let spec = ReportSpec {
        opts: ropts,
        ..opts.spec.clone()
    }
    .with_default_conversion_op(ConversionOp::NoConversionOp);
    // the closing date: the report period's last day, or yesterday or the
    // journal's last day, whichever is later; the opening date is the next
    let argsq = &spec.query;
    let yesterday = spec.today.add_days(-1);
    let journal_last_day = j.date_span(false).end().map(|e| e.add_days(-1));
    let close_date = match argsq.end_date(false) {
        Some(e) => e.add_days(-1),
        None => journal_last_day.map_or(yesterday, |d| d.max(yesterday)),
    };
    let open_date = close_date.add_days(1);
    let explicit = raw.bool("explicit");
    // the accounts to close, never the equity account
    let argsacctq = argsq.clone().filter(|q| q.is_acct() || q.is_type());
    let q2 = if argsacctq.is_null() {
        Query::And(vec![argsq.clone(), default_acctsq])
    } else {
        argsq.clone()
    };
    let q3 = Query::And(vec![
        q2,
        Query::Not(Box::new(Query::Acct(account_name_to_account_only_regex(
            close_acct,
        )))),
    ]);
    let spec3 = ReportSpec { query: q3, ..spec };
    let report = balance_report(&spec3, j)?;
    // costs stripped unless --show-costs, and accounts thereby emptied
    // dropped unless -E; lot subaccounts' costs are always stripped, as
    // their names hold them
    let show_costs = spec3.opts.show_costs;
    let avg_costs = crate::lots::journal_average_pool_costs(j, Some(open_date));
    let lot_styles = styles_with_rounding(Rounding::No, &j.commodity_styles());
    let balances: Vec<(String, MixedAmount)> = (report.items.into_iter())
        .filter_map(|(name, b)| {
            let a = with_average_cost(&avg_costs, &lot_styles, &name.full, &b);
            let keep_costs = show_costs && !is_lot_subaccount(&a);
            let b = if keep_costs { b } else { b.strip_costs() };
            (keep_costs || spec3.opts.empty || !b.looks_zero()).then_some((a, b))
        })
        .collect();
    let total: MixedAmount = balances.iter().map(|(_, b)| b).sum();
    // balance assertion amounts are exact, so the amounts are too (#941, #1137)
    let precise = Amount::with_full_precision;
    let interleaved = raw.bool("interleaved");
    let assertion = |amount: Amount| -> Box<BalanceAssertion> {
        let (total, inclusive) = match raw.string("assertion-type") {
            Some("=") => (false, false),
            Some("=*") => (false, true),
            Some("==") => (true, false),
            Some("==*") => (true, true),
            _ => (false, false),
        };
        Box::new(BalanceAssertion {
            amount,
            total,
            inclusive,
            pos: SourcePos::null(),
        })
    };
    let posting = |account: &str, amount: MixedAmount| Posting {
        account: account.into(),
        amount,
        ..Posting::default()
    };
    let one = MixedAmount::from_amount;
    let zeroed = |b: &Amount| Amount {
        quantity: Decimal::ZERO,
        cost: None,
        ..b.clone()
    };
    let mut txns = Vec::new();
    if matches!(mode, Clopen | Close | Assert | Retain) {
        let desc = raw.string("close-desc").unwrap_or(match mode {
            Retain => "retain earnings",
            Assert => "assert balances",
            _ => "closing balances",
        });
        let mut ps = Vec::new();
        for (a, mb) in &balances {
            for (b, last) in mark_last_in_commodity(mb.amounts()) {
                // after each commodity's last posting, a balance
                // assertion, unpriced (#1035, #824); not on lot
                // subaccounts, whose balances are checked before lots are
                // calculated
                let asserted = last && !is_lot_subaccount(a);
                if mode == Assert {
                    let mut p = posting(a, one(precise(zeroed(&b))));
                    p.assertion = asserted.then(|| assertion(precise(b.clone())));
                    ps.push(p);
                } else {
                    let mut p = posting(a, one(precise(-b.clone())));
                    p.assertion = asserted.then(|| assertion(precise(zeroed(&b))));
                    ps.push(p);
                    if interleaved {
                        ps.push(posting(close_acct, one(precise(b))));
                    }
                }
            }
        }
        if mode != Assert && !interleaved {
            ps.push(posting(close_acct, equity_amount(explicit, &total)));
        }
        txns.push(Transaction {
            date: close_date,
            description: desc.to_string(),
            comment: comment.clone(),
            postings: ps,
            ..Transaction::default()
        });
    }
    if matches!(mode, Clopen | Open | Assign) {
        let desc = raw.string("open-desc").unwrap_or("opening balances");
        let mut ps = Vec::new();
        for (a, mb) in &balances {
            for (b, sum) in mark_last_with_commodity_sum(mb.amounts()) {
                if mode == Assign {
                    let mut p = posting(a, MixedAmount::missing());
                    p.assertion = Some(assertion(b));
                    ps.push(p);
                } else {
                    let mut p = posting(a, one(precise(b.clone())));
                    p.assertion = sum
                        .filter(|_| !is_lot_subaccount(a))
                        .map(|s| assertion(precise(Amount { cost: None, ..s })));
                    ps.push(p);
                    if interleaved {
                        ps.push(posting(open_acct, one(precise(-b))));
                    }
                }
            }
        }
        if !interleaved {
            ps.push(posting(open_acct, equity_amount(explicit, &-&total)));
        }
        txns.push(Transaction {
            date: open_date,
            description: desc.to_string(),
            comment: comment.clone(),
            postings: ps,
            ..Transaction::default()
        });
    }
    // printed like print, with --round's rounding and --layout's layout
    let styles = match round_from_raw_opts(opts)? {
        Some(r) => styles_with_rounding(r, &j.commodity_styles()),
        None => j.commodity_styles(),
    };
    let layout = layout_from_raw_opts(opts)?;
    let mut out = String::new();
    for mut t in txns {
        t.style_amounts(&styles);
        out.push_str(&show_transaction(&t, layout, false));
    }
    Ok(with_title(opts, out))
}

/// The amount of the posting to or from the equity account: all the
/// balances, with -x, otherwise none (to be inferred).
fn equity_amount(explicit: bool, total: &MixedAmount) -> MixedAmount {
    if explicit {
        total.clone().with_full_precision()
    } else {
        MixedAmount::missing()
    }
}

/// An averaged lot's subaccount name (without a cost, which changes as
/// the pool is re-averaged) with the pool's average cost as of the opening
/// date, which all its lots share, so the entries say what the lots cost
/// (withAverageCost). Other names are unchanged.
fn with_average_cost(
    avg_costs: &BTreeMap<(AccountName, Sym), Amount>,
    styles: &CommodityStyles,
    a: &str,
    b: &MixedAmount,
) -> String {
    let averaged = || {
        let cb = parse_lot_name(|_| None, lot_subaccount_name(a)?).ok()?;
        let commodities: BTreeSet<Sym> = b.amounts().into_iter().map(|x| x.commodity).collect();
        let [c] = Vec::from_iter(commodities).try_into().ok()?;
        let base = lot_base_account(a);
        let avg = avg_costs.get(&(AccountName::from(base), c))?;
        let cb = CostBasis {
            cost: Some(Box::new(avg.clone().styled(styles))),
            ..cb
        };
        Some(format!("{base}:{}", show_lot_name(&cb)))
    };
    averaged().unwrap_or_else(|| a.to_string())
}

/// Whether the account is a lot subaccount (isLotSubaccount).
fn is_lot_subaccount(a: &str) -> bool {
    lot_base_account(a) != a
}

/// Each amount, marked if it is the last of its commodity's (they are
/// grouped by commodity).
fn mark_last_in_commodity(amounts: Vec<Amount>) -> Vec<(Amount, bool)> {
    let last: Vec<bool> = (0..amounts.len())
        .map(|i| {
            amounts
                .get(i + 1)
                .is_none_or(|n| n.commodity != amounts[i].commodity)
        })
        .collect();
    amounts.into_iter().zip(last).collect()
}

/// Each amount, the last of its commodity's with the sum of that
/// commodity's (they are grouped by commodity).
fn mark_last_with_commodity_sum(amounts: Vec<Amount>) -> Vec<(Amount, Option<Amount>)> {
    let mut out: Vec<(Amount, Option<Amount>)> = Vec::new();
    let mut sum = Amount::null();
    for (i, a) in amounts.iter().enumerate() {
        // Num's sum: from zero, each the right operand
        sum = &sum + a;
        let last = amounts
            .get(i + 1)
            .is_none_or(|n| n.commodity != a.commodity);
        out.push((a.clone(), last.then(|| sum.clone())));
        if last {
            sum = Amount::null();
        }
    }
    out
}

/// The journal file's base name with its last number incremented, for
/// the clopen tag; empty if it has no number (close's inferredval).
fn next_file_base_name(j: &Journal) -> Result<String, String> {
    let path = j.files.first().map_or("(unknown)", |(f, _)| f.as_str());
    let base = take_base_name(path);
    let chars: Vec<char> = base.chars().collect();
    let Some(end) = chars.iter().rposition(char::is_ascii_digit) else {
        return Ok(String::new());
    };
    let start = chars[..end]
        .iter()
        .rposition(|c| !c.is_ascii_digit())
        .map_or(0, |i| i + 1);
    let digits: String = chars[start..=end].iter().collect();
    let prefix: String = chars[..start].iter().collect();
    let suffix: String = chars[end + 1..].iter().collect();
    Ok(format!("{prefix}{}{suffix}", increment_digits(&digits)))
}

/// A number written in decimal digits, plus one, as Haskell shows it.
fn increment_digits(digits: &str) -> String {
    let mut ds: Vec<u8> = digits.trim_start_matches('0').bytes().collect();
    let mut i = ds.len();
    loop {
        if i == 0 {
            ds.insert(0, b'1');
            break;
        }
        i -= 1;
        if ds[i] == b'9' {
            ds[i] = b'0';
        } else {
            ds[i] += 1;
            break;
        }
    }
    String::from_utf8(ds).unwrap_or_default()
}

/// The file name without its directory and last extension (takeBaseName).
fn take_base_name(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rfind('.').map_or(name, |i| &name[..i])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incrementing() {
        assert_eq!(increment_digits("2023"), "2024");
        assert_eq!(increment_digits("0999"), "1000");
        assert_eq!(increment_digits("000"), "1");
        assert_eq!(take_base_name("/a/b/fin2023x.journal"), "fin2023x");
        assert_eq!(take_base_name("-"), "-");
    }
}
