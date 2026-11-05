//! The account transactions report (Hledger.Reports.AccountTransactionsReport):
//! the transactions affecting an account, each as seen from it, with the
//! amount it posts there and the running total or balance after it. The
//! aregister command shows it.

use crate::account::AccountName;
use crate::amount::{CommodityStyles, MixedAmount};
use crate::dates::{DateSpan, Day};
use crate::decimal::Decimal;
use crate::model::{Journal, Posting, PostingRef, Transaction};
use crate::query::Query;
use crate::reports::opts::{BalanceAccumulation, ReportSpec};
use crate::reports::postings::{register_running_calculation, sum_postings};

/// One line of an account transactions report
/// (AccountTransactionsReportItem): the transaction (valued, its costs
/// stripped unless shown, negated with --invert); the transaction as seen
/// from the account, with just the postings the report query matches,
/// dated at its register date; whether it posts to more than one other
/// account; the other accounts (or if none, the account's, or with no
/// account, all); the amount it posts to the account; and the running
/// total, balance or average after it.
#[derive(Clone, Debug)]
pub struct AccountTransactionsReportItem {
    pub txn: Transaction,
    pub acct_txn: Transaction,
    pub split: bool,
    pub other_accounts: Vec<AccountName>,
    pub amount: MixedAmount,
    pub balance: MixedAmount,
}

impl AccountTransactionsReportItem {
    /// The transactions' and amounts' amounts styled (styleAmounts).
    pub fn style_amounts(&mut self, styles: &CommodityStyles) {
        self.txn.style_amounts(styles);
        self.acct_txn.style_amounts(styles);
        self.amount = std::mem::take(&mut self.amount).styled(styles);
        self.balance = std::mem::take(&mut self.balance).styled(styles);
    }
}

/// The account transactions report of the account (or accounts) the
/// account query matches, most recent first, and the balance its running
/// total starts from: zero, or with --historical and a start date in the
/// query, the sum of the account's postings before that date (with
/// --average, their average per transaction). A depth limit is ignored;
/// cur: and amt: terms select parts of amounts before valuation
/// (accountTransactionsReportWithStart). The account query may instead be
/// the error of finding no account, which is returned only if a posting
/// needs it to be matched against, as upstream's lazy one would be.
pub fn account_transactions_report(
    spec: &ReportSpec,
    j: &Journal,
    thisacctq: Result<&Query, &str>,
) -> Result<(MixedAmount, Vec<AccountTransactionsReportItem>), String> {
    let ropts = &spec.opts;
    let types = |a: &str| j.account_type(a);
    let reportq = Query::And(vec![
        (spec.query.clone())
            .filter(|q| !q.is_depth())
            .filter(|q| !q.is_amt_or_cur()),
        Query::Date(ropts.period.as_date_span()),
    ])
    .simplify();
    let amtq = spec.query.clone().filter(Query::is_amt_or_cur);
    // the transactions the account query matches, by their real and status
    // matched postings if the report query has such terms; their amounts
    // selected by cur: and amt:, valued, costs stripped unless shown
    let realq = reportq.clone().filter(Query::is_real);
    let statusq = reportq.clone().filter(Query::is_status);
    let relevantq = (!realq.is_null() || !statusq.is_null())
        .then(|| Query::And(vec![realq, statusq]).simplify());
    let mut aj = j.clone();
    let mut needed = false;
    aj.txns.retain(|t| {
        let relevant = match &relevantq {
            None => t.clone(),
            Some(q) => filter_transaction_postings(q, t),
        };
        match thisacctq {
            Ok(q) => q.matches_transaction(&relevant),
            Err(_) => {
                needed |= !relevant.postings.is_empty();
                false
            }
        }
    });
    let thisacctq = match thisacctq {
        Ok(q) => q,
        Err(e) if needed => return Err(e.to_string()),
        // no transactions are left to match it
        Err(_) => &Query::None,
    };
    if !amtq.is_null() {
        for t in &mut aj.txns {
            t.postings.retain_mut(|p| {
                p.amount = std::mem::take(&mut p.amount).filter(|a| amtq.matches_amount(a));
                !p.amount.amounts_raw().is_empty()
            });
        }
    }
    let mut aj = spec.journal_apply_valuation(aj)?;
    if !ropts.show_costs {
        for p in aj.txns.iter_mut().flat_map(|t| t.postings.iter_mut()) {
            p.amount = std::mem::take(&mut p.amount).strip_costs();
        }
    }
    // the number of the first item, and the running total or average
    // before it, which with --historical include the transactions before
    // the report's start
    let (start_num, start_bal) = if ropts.balance_accum != BalanceAccumulation::Historical {
        (1, MixedAmount::null())
    } else {
        // the postings before the report's start, by whichever kind of
        // date its start date is: the report's kind if it has one, else
        // the other
        let cutoff = |secondary: bool| {
            reportq.start_date(secondary).map(|d| {
                let span = DateSpan::exact(None, Some(d));
                if secondary {
                    Query::Date2(span)
                } else {
                    Query::Date(span)
                }
            })
        };
        let to_start = cutoff(ropts.date2)
            .or_else(|| cutoff(!ropts.date2))
            .unwrap_or(Query::None);
        let dateless = reportq.clone().filter(|q| !q.is_date_or_date2());
        let priorq = Query::And(vec![thisacctq.clone(), to_start, dateless]);
        let prior: Vec<Vec<&Posting>> = (aj.txns.iter())
            .map(|t| {
                (t.postings.iter())
                    .filter(|p| priorq.matches_posting(PostingRef::new(t, p)) && p.has_amount())
                    .collect::<Vec<_>>()
            })
            .filter(|ps| !ps.is_empty())
            .collect();
        let n = prior.len();
        let sum = sum_postings(prior.into_iter().flatten());
        if ropts.average {
            (n + 1, sum.divide(&Decimal::from_i64(n as i64)))
        } else {
            (n + 1, sum)
        }
    };
    let mut dated: Vec<(Day, Transaction)> = (aj.txns.into_iter())
        .map(|mut t| {
            if ropts.invert {
                for p in &mut t.postings {
                    p.amount = -std::mem::take(&mut p.amount);
                }
            }
            let d = transaction_register_date(&types, ropts.date2, &reportq, thisacctq, &t);
            (d, t)
        })
        .collect();
    // the most recent first, the later of a day's first, for an accurate
    // starting balance
    dated.sort_by(|(d1, t1), (d2, t2)| d2.cmp(d1).then(t2.index.cmp(&t1.index)));
    // the running calculation goes from the oldest (mapAccumR)
    let running = register_running_calculation(ropts);
    let mut items = Vec::new();
    let (mut i, mut bal) = (start_num, start_bal.clone());
    for (d, t) in dated.into_iter().rev() {
        let acct_txn = filter_transaction_postings_extra(&types, &reportq, &t);
        if acct_txn.postings.is_empty() {
            continue;
        }
        let (this, other): (Vec<&Posting>, Vec<&Posting>) = (acct_txn.postings.iter())
            .partition(|p| thisacctq.matches_posting(PostingRef::new(&acct_txn, p)));
        let mut other_names: Vec<&str> = Vec::new();
        for p in &other {
            if !other_names.contains(&&*p.account) {
                other_names.push(&p.account);
            }
        }
        let other_accounts = if *thisacctq == Query::None {
            summarise_posting_accounts(&acct_txn.postings.iter().collect::<Vec<_>>())
        } else if other_names.is_empty() {
            summarise_posting_accounts(&this)
        } else {
            summarise_posting_accounts(&other)
        };
        // the change to the account: the sum of its postings (negated
        // twice upstream, by maNegate and the sign function, also maNegate),
        // or if none match, of the others negated
        let amount = if this.is_empty() {
            -sum_postings(other.iter().copied())
        } else {
            sum_postings(this.iter().copied())
        };
        bal = running(i, &bal, &amount);
        i += 1;
        let split = other_names.len() > 1;
        items.push(AccountTransactionsReportItem {
            acct_txn: Transaction {
                date: d,
                ..acct_txn
            },
            txn: t,
            split,
            other_accounts,
            amount,
            balance: bal.clone(),
        });
    }
    items.reverse();
    Ok((start_bal, items))
}

/// The transaction with just the postings the query matches
/// (filterTransactionPostings).
fn filter_transaction_postings(q: &Query, t: &Transaction) -> Transaction {
    Transaction {
        postings: (t.postings.iter())
            .filter(|p| q.matches_posting(PostingRef::new(t, p)))
            .cloned()
            .collect(),
        ..t.clone()
    }
}

/// The transaction with just the postings the query matches, `type:`
/// terms matching the accounts' types (filterTransactionPostingsExtra).
fn filter_transaction_postings_extra(
    types: &dyn Fn(&str) -> Option<crate::account::AccountType>,
    q: &Query,
    t: &Transaction,
) -> Transaction {
    Transaction {
        postings: (t.postings.iter())
            .filter(|p| q.matches_posting_extra(types, PostingRef::new(t, p)))
            .cloned()
            .collect(),
        ..t.clone()
    }
}

/// The date to show for a transaction in an account's register: the
/// earliest date (or with --date2 secondary date) of its postings that
/// both the report query and the account query match, if any, otherwise
/// the transaction's; `type:` terms matching the accounts' types as
/// `types` gives them (transactionRegisterDateExtra; with no types,
/// transactionRegisterDate).
pub fn transaction_register_date(
    types: &dyn Fn(&str) -> Option<crate::account::AccountType>,
    date2: bool,
    reportq: &Query,
    thisacctq: &Query,
    t: &Transaction,
) -> Day {
    let reported = filter_transaction_postings_extra(types, reportq, t);
    (reported.postings.iter())
        .filter(|p| thisacctq.matches_posting(PostingRef::new(t, p)))
        .map(|p| {
            if date2 {
                p.date2_in(Some(t))
            } else {
                p.date_in(Some(t))
            }
        })
        .min()
        .unwrap_or_else(|| t.date_of(date2))
}

/// The accounts of some postings, just the real ones' if there are any
/// (summarisePostingAccounts).
fn summarise_posting_accounts(ps: &[&Posting]) -> Vec<AccountName> {
    let real: Vec<&&Posting> = ps.iter().filter(|p| p.is_real()).collect();
    if real.is_empty() {
        ps.iter().map(|p| p.account.clone()).collect()
    } else {
        real.iter().map(|p| p.account.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::AccountType;
    use crate::amount::render::show_mixed_amount;
    use crate::amount::usd;
    use crate::reports::opts::ReportOpts;
    use crate::reports::periodic::tests::sample_journal;

    fn day(y: i64, m: u32, d: u32) -> Day {
        Day::from_ymd(y, m, d).unwrap()
    }

    // tests_AccountTransactionsReport. The sample journal's amounts are
    // written without decimals here, so show as $1 rather than $1.00.
    #[test]
    fn account_transactions_report_with_start() {
        let j = sample_journal();
        let checking = Query::Acct(crate::regex::to_regex("assets:bank:checking").unwrap());
        let from_june = DateSpan::exact(Some(day(2008, 6, 1)), None);
        let spec = |accum: BalanceAccumulation, q: Query| ReportSpec {
            query: q,
            ..ReportSpec::new(
                day(2026, 1, 1),
                ReportOpts {
                    balance_accum: accum,
                    ..ReportOpts::default()
                },
            )
            .unwrap()
        };
        let shown = |items: &[AccountTransactionsReportItem]| -> Vec<String> {
            items
                .iter()
                .map(|i| show_mixed_amount(&i.balance))
                .collect()
        };
        // with historical balances, the running balance starts from the postings before the start date
        let historical = spec(BalanceAccumulation::Historical, Query::Date(from_june));
        let (start, items) = account_transactions_report(&historical, &j, Ok(&checking)).unwrap();
        assert_eq!(show_mixed_amount(&start), "$1");
        assert_eq!(shown(&items), ["$1", "$2", "$1", "$2"]);
        // otherwise from zero
        let per_period = spec(BalanceAccumulation::PerPeriod, Query::Date(from_june));
        let (start, items) = account_transactions_report(&per_period, &j, Ok(&checking)).unwrap();
        assert_eq!(show_mixed_amount(&start), "0");
        assert_eq!(shown(&items), ["0", "$1", "0", "$1"]);
        // a date2: start date counts too, by secondary date (here the same days)
        let historical2 = spec(BalanceAccumulation::Historical, Query::Date2(from_june));
        let (start, _) = account_transactions_report(&historical2, &j, Ok(&checking)).unwrap();
        assert_eq!(show_mixed_amount(&start), "$1");
    }

    #[test]
    fn transaction_register_date_with_types() {
        let one = |n: i64| usd(Decimal::from_i64(n));
        let t = Transaction {
            date: day(2008, 1, 1),
            postings: vec![
                Posting {
                    date: Some(day(2008, 1, 5)),
                    ..Posting::new("assets:bank:checking", one(1))
                },
                Posting::new("income:salary", one(-1)),
            ],
            ..Transaction::default()
        };
        let checking = Query::Acct(crate::regex::to_regex("assets:bank:checking").unwrap());
        let assets = Query::Type(vec![AccountType::Asset]);
        let asset_types = |a: &str| a.starts_with("assets").then_some(AccountType::Asset);
        // a type: term in the report query matches the posting only when the account types are known
        assert_eq!(
            transaction_register_date(&|_| None, false, &assets, &checking, &t),
            day(2008, 1, 1)
        );
        assert_eq!(
            transaction_register_date(&asset_types, false, &assets, &checking, &t),
            day(2008, 1, 5)
        );
    }
}
