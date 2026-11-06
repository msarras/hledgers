//! Extra checks of a finalised journal (Hledger.Data.JournalChecks and its
//! Ordereddates and Uniqueleafnames modules): some done with `--strict`,
//! the others when named to the check command. Each returns the first
//! problem's message, with an excerpt.

use std::collections::BTreeSet;

use super::errors::{
    account_tag_error_excerpt, commodity_tag_error_excerpt, posting_account_error_excerpt,
    posting_error_excerpt, price_directive_error_excerpt, transaction_error_excerpt,
};
use crate::account::{account_leaf_name, lot_base_account};
use crate::amount::Sym;
use crate::amount::render::AmountFormat;
use crate::model::{
    CONVERSION_POSTING_TAG, COST_POSTING_TAG, FEESPLIT_POSTING_TAG, GENERATED_POSTING_TAG,
    GENERATED_TRANSACTION_TAG, IMBALANCE_POSTING_TAG, Journal, LOT_PARENT_ASSERTION_TAG,
    LOTSPLIT_POSTING_TAG, MODIFIED_TRANSACTION_TAG, Posting, Status, Transaction,
};
use crate::text::{chomp, is_space, show_string};
use crate::write::journal::{
    DEFAULT_DECIMAL_MARK_COLUMN, posting_as_lines, show_transaction_line_first_part,
};

/// The checks `--strict` adds, most important first: commodities, then
/// accounts; balancing without inferred costs happens when reading
/// (journalStrictChecks).
pub fn journal_strict_checks(j: &Journal) -> Result<(), String> {
    journal_check_commodities(j)?;
    journal_check_accounts(j)
}

/// Every posting's account has been declared, except postings hledger
/// generated to an account it chose (journalCheckAccounts).
pub fn journal_check_accounts(j: &Journal) -> Result<(), String> {
    let declared: BTreeSet<&str> = j.declared_account_names().map(|a| &**a).collect();
    for t in &j.txns {
        for p in &t.postings {
            let generated_account = p.has_tag(IMBALANCE_POSTING_TAG)
                || (p.has_tag(GENERATED_POSTING_TAG)
                    && (p.has_tag(CONVERSION_POSTING_TAG) || is_gain_posting(p)));
            let acct = lot_base_account(&p.account);
            if generated_account || declared.contains(acct) {
                continue;
            }
            let (f, l, ex) = posting_account_error_excerpt(t, p);
            return Err(format!(
                "{f}:{l}:\n{ex}\nStrict account checking is enabled, and\n\
                 account \"{acct}\" has not been declared.\n\
                 Consider adding an account directive. Examples:\n\naccount {acct}\n"
            ));
        }
    }
    Ok(())
}

/// Whether a posting is a lot disposal's gain posting (isGainPosting).
fn is_gain_posting(p: &Posting) -> bool {
    p.tags.iter().any(|(n, v)| n == "_ptype" && v == "gain")
}

/// The commodities a posting's amounts and assertion use, ignoring the
/// missing amount and zeros with no commodity (postingCommoditiesUsed).
fn posting_commodities_used(p: &Posting) -> impl Iterator<Item = &Sym> {
    p.amount
        .amounts_raw()
        .iter()
        .filter(|a| !(a.is_missing() || (a.is_zero() && a.commodity.is_empty())))
        .map(|a| &a.commodity)
        .chain(p.assertion.as_ref().map(|ba| &ba.amount.commodity))
}

/// The declared commodities and their aliases (commoditiesAndAliases).
fn commodities_and_aliases(j: &Journal) -> BTreeSet<&Sym> {
    let mut declared: BTreeSet<&Sym> = j.declared_commodities.keys().collect();
    for c in j.declared_commodities.values() {
        declared.extend(c.aliases.iter().map(|(a, _)| &a.commodity));
    }
    declared
}

/// The commodities used in P directives and postings which are neither
/// declared nor an alias of a declared one, sorted
/// (journalUndeclaredCommodities).
pub fn journal_undeclared_commodities(j: &Journal) -> Vec<Sym> {
    let declared = commodities_and_aliases(j);
    let used = (j.price_directives.iter())
        .flat_map(|pd| [&pd.commodity, &pd.amount.commodity])
        .chain(
            j.txns
                .iter()
                .flat_map(|t| &t.postings)
                .flat_map(posting_commodities_used),
        );
    let undeclared: BTreeSet<&Sym> = used.filter(|c| !declared.contains(c)).collect();
    undeclared.into_iter().cloned().collect()
}

/// Every commodity used in P directives and postings has been declared,
/// or is an alias of a declared one (journalCheckCommodities).
pub fn journal_check_commodities(j: &Journal) -> Result<(), String> {
    let declared = commodities_and_aliases(j);
    let error = |(f, l, ex): (String, usize, String), comm: &str| {
        format!(
            "{f}:{l}:\n{ex}\nStrict commodity checking is enabled, and\n\
             commodity {} has not been declared.\n\
             Consider adding a commodity directive. Examples:\n\n\
             commodity {comm}1000.00\ncommodity 1.000,00 {comm}\n",
            show_string(comm)
        )
    };
    for pd in &j.price_directives {
        if let Some(c) = [&pd.commodity, &pd.amount.commodity]
            .into_iter()
            .find(|c| !declared.contains(c))
        {
            return Err(error(price_directive_error_excerpt(pd), c));
        }
    }
    for t in &j.txns {
        for p in &t.postings {
            if let Some(c) = posting_commodities_used(p).find(|c| !declared.contains(c)) {
                return Err(error(posting_error_excerpt(t, p, amount_columns), c));
            }
        }
    }
    Ok(())
}

/// The columns of a posting's amount and assertion in the rendered
/// transaction, if it is found there.
fn amount_columns(p: &Posting, t: &Transaction, rendered: &str) -> super::errors::Columns {
    let index = t.postings.iter().position(|q| q == p)? + 1;
    let comment_lines = t.comment.split_terminator('\n').count().saturating_sub(1);
    let line = rendered
        .split_terminator('\n')
        .nth(comment_lines + index)
        .unwrap_or("");
    let chars: Vec<char> = line.chars().collect();
    let acct_end = 4 + p.account.chars().count() + if p.is_virtual() { 2 } else { 0 };
    let rest = chars.get(acct_end..).unwrap_or(&[]);
    let start = acct_end + rest.iter().take_while(|c| is_space(**c)).count() + 1;
    let amount: String = chars
        .get(start..)
        .unwrap_or(&[])
        .iter()
        .take_while(|c| **c != ';')
        .collect();
    let end = start + amount.trim_end_matches(is_space).chars().count();
    Some((start, Some(end)))
}

/// Every transaction's payee has been declared (journalCheckPayees).
pub fn journal_check_payees(j: &Journal) -> Result<(), String> {
    let declared: BTreeSet<&str> = j.declared_payees.iter().map(|(p, _)| p.as_str()).collect();
    for t in &j.txns {
        let payee = t.payee();
        if declared.contains(payee) {
            continue;
        }
        let col = show_transaction_line_first_part(t).chars().count() + 2;
        let cols = Some((col, Some(col + payee.chars().count() - 1)));
        let (f, l, ex) = transaction_error_excerpt(t, cols);
        return Err(format!(
            "{f}:{l}:\n{ex}\nStrict payee checking is enabled, and\n\
             payee {} has not been declared.\n\
             Consider adding a payee directive. Examples:\n\npayee {payee}\n",
            show_string(payee)
        ));
    }
    Ok(())
}

/// The tags that need no declaration: those hledger gives a meaning, and
/// the hidden and visible forms of those it adds (builtinTags).
fn builtin_tags() -> Vec<String> {
    let hidden = [
        GENERATED_TRANSACTION_TAG,
        MODIFIED_TRANSACTION_TAG,
        GENERATED_POSTING_TAG,
        COST_POSTING_TAG,
        CONVERSION_POSTING_TAG,
        "_ptype",
        FEESPLIT_POSTING_TAG,
        LOTSPLIT_POSTING_TAG,
        LOT_PARENT_ASSERTION_TAG,
    ];
    [
        "date", "date2", "type", "alias", "lots", "t", "assert", "retain", "start",
    ]
    .iter()
    .map(|t| t.to_string())
    .chain(hidden.iter().map(|t| t.to_string()))
    .chain(hidden.iter().map(|t| t[1..].to_string()))
    .collect()
}

/// Every tag on account and commodity declarations, transactions and
/// postings has been declared (journalCheckTags).
pub fn journal_check_tags(j: &Journal) -> Result<(), String> {
    let builtin = builtin_tags();
    let declared: BTreeSet<&str> = j
        .declared_tags
        .iter()
        .map(|(t, _)| t.as_str())
        .chain(builtin.iter().map(String::as_str))
        .collect();
    let error = |(f, l, ex): (String, usize, String), tag: &str| {
        format!(
            "{f}:{l}:\n{ex}\nStrict tag checking is enabled, and\n\
             tag {} has not been declared.\n\
             Consider adding a tag directive. Examples:\n\ntag {tag}\n",
            show_string(tag)
        )
    };
    for (a, info) in &j.declared_accounts {
        if let Some((tag, _)) = info
            .tags
            .iter()
            .find(|(n, _)| !declared.contains(n.as_str()))
        {
            return Err(error(account_tag_error_excerpt(a, info), tag));
        }
    }
    for c in j.declared_commodities.values() {
        if let Some((tag, _)) = c.tags.iter().find(|(n, _)| !declared.contains(n.as_str())) {
            return Err(error(commodity_tag_error_excerpt(c), tag));
        }
    }
    for t in &j.txns {
        if let Some((tag, _)) = t.all_tags().find(|(n, _)| !declared.contains(n.as_str())) {
            return Err(error(transaction_error_excerpt(t, None), tag));
        }
    }
    Ok(())
}

/// The most days an account's latest posting may be after its latest
/// balance assertion (maxlag).
const MAX_LAG: i64 = 7;

/// Accounts with balance assertions have none of their postings more than
/// 7 days after their latest assertion (journalCheckRecentAssertions).
pub fn journal_check_recent_assertions(j: &Journal) -> Result<(), String> {
    let mut ps: Vec<(&Transaction, &Posting)> = j
        .txns
        .iter()
        .flat_map(|t| t.postings.iter().map(move |p| (t, p)))
        .collect();
    ps.sort_by(|a, b| a.1.account.cmp(&b.1.account));
    for group in ps.chunk_by(|a, b| a.1.account == b.1.account) {
        if let Some(e) = find_recent_assertion_error(group) {
            return Err(e);
        }
    }
    Ok(())
}

/// The recentassertions problem in one account's postings, if any: the
/// earliest posting more than 7 days after the latest assertion
/// (findRecentAssertionError).
fn find_recent_assertion_error(ps: &[(&Transaction, &Posting)]) -> Option<String> {
    let date = |(t, p): &(&Transaction, &Posting)| p.date_in(Some(t));
    let mut rps = ps.to_vec();
    rps.sort_by_key(|tp| std::cmp::Reverse(date(tp)));
    let split = rps.iter().position(|(_, p)| p.assertion.is_some())?;
    let (after, until) = rps.split_at(split);
    let latest = date(&until[0]);
    let within = |tp: &(&Transaction, &Posting)| date(tp).diff_days(latest) <= MAX_LAG;
    let kept = after.len() - after.iter().rev().take_while(|tp| within(tp)).count();
    let (t, p) = *after[..kept].last()?;
    let lag = p.date_in(Some(t)).diff_days(latest);
    let (f, l, ex) = posting_account_error_excerpt(t, p);
    let status_width = if p.status == Status::Unmarked { 0 } else { 2 };
    let shown = Posting {
        comment: String::new(),
        ..p.clone()
    };
    let posting = posting_as_lines(
        &AmountFormat::DEFAULT,
        true,
        status_width,
        p.account.chars().count(),
        DEFAULT_DECIMAL_MARK_COLUMN,
        &shown,
    )
    .into_iter()
    .next()
    .unwrap_or_default();
    // M8: the account name is bold when stdout gets colour (bold').
    let msg = format!(
        "{f}:{l}:\n{}\n\n\
         The recentassertions check is enabled, so accounts with balance assertions\n\
         must have a recent one, not more than {MAX_LAG} days older than their latest posting.\n\
         In account: {}\n\
         the last assertion was on {latest}, {lag} days before this latest posting.\n\
         Consider adding a new balance assertion to the above posting. Eg:\n\n\
         {posting} = BALANCE\n",
        chomp(&ex),
        p.account
    );
    Some(chomp(&msg).to_string())
}

/// Each file's transactions are in date order (journalCheckOrdereddates).
pub fn journal_check_ordereddates(j: &Journal) -> Result<(), String> {
    for ts in j.txns.chunk_by(|a, b| a.file() == b.file()) {
        let Some(i) = ts.windows(2).position(|w| w[0].date > w[1].date) else {
            continue;
        };
        let (prev, t) = (&ts[i], &ts[i + 1]);
        let (_, _, ex1) = transaction_error_excerpt(prev, None);
        let (f, l, ex2) = transaction_error_excerpt(t, Some((1, Some(10))));
        return Err(format!(
            "{f}:{l}:\n{}\n\n{}\n\nOrdered dates checking is enabled, and this transaction's\n\
             date ({}) is out of order with the previous transaction.\n\
             Consider moving this entry into date order, or adjusting its date.",
            chomp(&ex1),
            chomp(&ex2),
            t.date
        ));
    }
    Ok(())
}

/// No two accounts used have the same last name part
/// (journalCheckUniqueleafnames).
pub fn journal_check_uniqueleafnames(j: &Journal) -> Result<(), String> {
    let used: BTreeSet<&str> = j
        .txns
        .iter()
        .flat_map(|t| t.postings.iter().map(|p| &*p.account))
        .collect();
    let mut by_leaf: Vec<(&str, &str)> = used.iter().map(|a| (account_leaf_name(a), *a)).collect();
    by_leaf.sort_by_key(|(leaf, _)| *leaf);
    let Some(dupes) = by_leaf.chunk_by(|a, b| a.0 == b.0).find(|g| g.len() > 1) else {
        return Ok(());
    };
    let leaf = dupes[0].0;
    let fulls: Vec<&str> = dupes.iter().map(|(_, a)| *a).collect();
    let ps: Vec<(&Transaction, &Posting)> = j
        .txns
        .iter()
        .flat_map(|t| t.postings.iter().map(move |p| (t, p)))
        .filter(|(_, p)| fulls.contains(&&*p.account))
        .collect();
    let [(t1, p1), (t2, p2), ..] = ps.as_slice() else {
        return Ok(());
    };
    let (_, _, ex1) = posting_error_excerpt(t1, p1, |_, _, _| None);
    let (f, l, ex2) = posting_error_excerpt(t2, p2, |p, _, _| {
        let alen = p.account.chars().count();
        let llen = account_leaf_name(&p.account).chars().count();
        let col = 5 + usize::from(p.is_virtual()) + alen - llen;
        Some((col, Some(col + llen - 1)))
    });
    let accts: String = fulls.iter().map(|a| format!("{a}\n")).collect();
    let msg = format!(
        "{f}:{l}:\n{}\n\n{}\n\nChecking for unique account leaf names is enabled, and\n\
         account leaf name {} is not unique.\n\
         It appears in these account names, which are used in {} places:\n{accts}\n\
         Consider changing these account names so their last parts are different.",
        chomp(&ex1),
        chomp(&ex2),
        show_string(leaf),
        ps.len()
    );
    Err(chomp(&msg).to_string())
}
