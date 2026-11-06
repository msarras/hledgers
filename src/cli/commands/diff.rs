//! The diff command (Hledger.Cli.Commands.Diff): an account's
//! transactions which are in one of two journal files but not the other.

use crate::cli::args::{General, Hidden, Mode};
use crate::cli::io::with_title;
use crate::cli::opts::CliOpts;
use crate::decimal::Decimal;
use crate::model::{Journal, Posting, Transaction};
use crate::query::Query;
use crate::read::{InputOpts, read_journal_file};
use crate::write::journal::{PostingLayout, show_transaction};

pub static MODE: Mode = Mode {
    name: "diff",
    aliases: &[],
    flags: &[],
    general: General::Group2,
    hidden: Hidden::Conf,
    args: Some("-f FILE1 -f FILE2 FULLACCOUNTTNAME"),
    doc: crate::command_doc!("Diff"),
};

/// A posting with its position: its transaction's in the journal, and
/// its own in the transaction (PostingWithPath, equal when the positions
/// are).
#[derive(Clone, Copy)]
struct PostingWithPath<'j> {
    posting: &'j Posting,
    txn: &'j Transaction,
    txnidx: usize,
    pidx: usize,
}

impl PartialEq for PostingWithPath<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.txnidx == other.txnidx && self.pidx == other.pidx
    }
}

/// Run the diff command (diff): with two -f files and a full account
/// name, the transactions posting to that account in each file that have
/// no match in the other, postings matching by their first amount's
/// quantity, the closest in date preferred. The files are read with
/// default input options, ignoring balance assertions.
pub fn diff(opts: &CliOpts, _j: &Journal) -> Result<String, String> {
    let usage =
        "Please specify two input files. Usage: hledger diff -f FILE1 -f FILE2 FULLACCOUNTNAME";
    let ([f1, f2], Query::Acct(acct_re)) = (opts.files.as_slice(), &opts.spec.query) else {
        return Err(usage.to_string());
    };
    let iopts = InputOpts {
        balancing: crate::read::BalancingOpts {
            ignore_assertions: true,
            ..Default::default()
        },
        ..InputOpts::default()
    };
    let j1 = read_journal_file(&iopts, f1)?;
    let j2 = read_journal_file(&iopts, f2)?;
    let acct = acct_re.src();
    let pp1 = matching_postings(acct, &j1);
    let pp2 = matching_postings(acct, &j2);
    let m = matching(&pp1, &pp2);
    let mut out = String::from("These transactions are in the first file only:\n\n");
    for t in unmatched_txns(&pp1, m.iter().map(|(l, _)| l)) {
        out.push_str(&show_transaction(t, PostingLayout::default(), false));
    }
    out.push_str("These transactions are in the second file only:\n\n");
    for t in unmatched_txns(&pp2, m.iter().map(|(_, r)| r)) {
        out.push_str(&show_transaction(t, PostingLayout::default(), false));
    }
    Ok(with_title(opts, out))
}

/// The postings to exactly this account, with their positions
/// (matchingPostings).
fn matching_postings<'j>(acct: &str, j: &'j Journal) -> Vec<PostingWithPath<'j>> {
    let mut pps = Vec::new();
    for (txnidx, txn) in j.txns.iter().enumerate() {
        for (pidx, posting) in txn.postings.iter().enumerate() {
            if &*posting.account == acct {
                pps.push(PostingWithPath {
                    posting,
                    txn,
                    txnidx,
                    pidx,
                });
            }
        }
    }
    pps
}

/// The quantity of a posting's first amount (ppamountqty).
fn amount_quantity(pp: &PostingWithPath) -> Decimal {
    (pp.posting.amount.amounts().into_iter().next()).map_or(Decimal::ZERO, |a| a.quantity)
}

/// Pairs of postings, one from each file, with the same first quantity:
/// in each group of those, in order of the second's date less the first's,
/// a pair whenever neither posting is in an earlier one (matching,
/// combinedBinBy, greedyMaxMatching).
fn matching<'j>(
    ppl: &[PostingWithPath<'j>],
    ppr: &[PostingWithPath<'j>],
) -> Vec<(PostingWithPath<'j>, PostingWithPath<'j>)> {
    // the postings, left ones first, grouped by quantity (groupSortOn)
    let mut tagged: Vec<(Decimal, bool, PostingWithPath)> = (ppl.iter().map(|p| (false, *p)))
        .chain(ppr.iter().map(|p| (true, *p)))
        .map(|(right, p)| (amount_quantity(&p), right, p))
        .collect();
    tagged.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = Vec::new();
    for bin in tagged.chunk_by(|a, b| a.0 == b.0) {
        let lefts = bin
            .iter()
            .filter(|(_, right, _)| !right)
            .map(|(_, _, p)| *p);
        let rights: Vec<PostingWithPath> = bin
            .iter()
            .filter(|(_, right, _)| *right)
            .map(|(_, _, p)| *p)
            .collect();
        let mut pairs: Vec<(PostingWithPath, PostingWithPath)> = lefts
            .flat_map(|l| rights.iter().map(move |r| (l, *r)))
            .collect();
        // dateCloseness: the second's date less the first's
        pairs.sort_by_key(|(l, r)| r.txn.date.diff_days(l.txn.date));
        let (mut used_l, mut used_r): (Vec<PostingWithPath>, Vec<PostingWithPath>) =
            (Vec::new(), Vec::new());
        for (l, r) in pairs {
            if !used_l.contains(&l) && !used_r.contains(&r) {
                used_l.push(l);
                used_r.push(r);
                out.push((l, r));
            }
        }
    }
    out
}

/// The transactions of the postings that are not matched, once each, in
/// order (unmatchedtxns: list difference, then nubBy).
fn unmatched_txns<'a, 'j: 'a>(
    pps: &[PostingWithPath<'j>],
    matched: impl Iterator<Item = &'a PostingWithPath<'j>>,
) -> Vec<&'j Transaction> {
    let mut rest: Vec<PostingWithPath> = pps.to_vec();
    for m in matched {
        if let Some(i) = rest.iter().position(|p| p == m) {
            rest.remove(i);
        }
    }
    let mut txns: Vec<(usize, &Transaction)> = Vec::new();
    for p in rest {
        if !txns.iter().any(|(i, _)| *i == p.txnidx) {
            txns.push((p.txnidx, p.txn));
        }
    }
    txns.into_iter().map(|(_, t)| t).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::RawOpts;
    use crate::dates::Day;

    // Expectations from the reference hledger 1.99: with these files as
    // d1.j and d2.j, `hledger -f d1.j -f d2.j diff assets:bank`.
    #[test]
    fn unmatched_transactions() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, text: &str| {
            let p = dir.path().join(name);
            std::fs::write(&p, text).unwrap();
            p.display().to_string()
        };
        let f1 = write(
            "d1.j",
            "2024-01-01 opening\n    assets:bank      $100.00\n    equity\n\n\
             2024-01-05 groceries\n    expenses:food     $30.00\n    assets:bank\n\n\
             2024-01-10 rent\n    expenses:rent     $50.00\n    assets:bank\n\n\
             2024-01-20 coffee\n    expenses:food      $5.00\n    assets:bank\n",
        );
        let f2 = write(
            "d2.j",
            "2024-01-01 opening balance\n    assets:bank      $100.00\n    equity\n\n\
             2024-01-06 supermarket\n    expenses:food     $30.00\n    assets:bank\n\n\
             2024-01-25 bookshop\n    expenses:books    $12.00\n    assets:bank\n\n\
             2024-01-11 rent\n    expenses:rent     $50.00\n    assets:bank\n",
        );
        let opts = |args: &[&str]| {
            let mut raw = vec![
                ("file".to_string(), f1.clone()),
                ("file".to_string(), f2.clone()),
            ];
            raw.extend(args.iter().map(|a| ("args".to_string(), a.to_string())));
            let today = Day::from_ymd(2026, 1, 1).unwrap();
            CliOpts::from_raw_opts(RawOpts(raw), today, false).unwrap()
        };
        assert_eq!(
            diff(&opts(&["assets:bank"]), &Journal::default()).unwrap(),
            concat!(
                "These transactions are in the first file only:\n\n",
                "2024-01-20 coffee\n",
                "    expenses:food                                 $5.00\n",
                "    assets:bank                                  $-5.00\n\n",
                "These transactions are in the second file only:\n\n",
                "2024-01-25 bookshop\n",
                "    expenses:books                               $12.00\n",
                "    assets:bank                                 $-12.00\n\n",
            )
        );
        // `hledger -f d1.j -f d2.j diff`
        assert_eq!(
            diff(&opts(&[]), &Journal::default()).unwrap_err(),
            "Please specify two input files. Usage: hledger diff -f FILE1 -f FILE2 FULLACCOUNTNAME"
        );
    }
}
