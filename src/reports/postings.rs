//! The postings report (Hledger.Reports.PostingsReport): the postings a
//! query matches, each with the running total (or average) after it; or
//! with a report interval, one summary posting per account and period.
//! The register command shows it.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use crate::account::{AccountName, DepthSpec, clip_or_ellipsify_account_name};
use crate::amount::{CommodityStyles, MixedAmount};
use crate::dates::{
    DateSpan, Day, Interval, Period, group_by_date_span, maybe_day_partition_to_date_spans,
    null_date,
};
use crate::decimal::Decimal;
use crate::model::{Journal, Posting, PostingRef, Transaction};
use crate::query::Query;
use crate::reports::opts::{BalanceAccumulation, ReportOpts, ReportSpec, SortField};

/// One line of a postings report (PostingsReportItem): the date, if this
/// is its transaction's first posting or its date differs from the
/// previous posting's (for a summary posting, the first of its period);
/// the report period, for a summary posting; the transaction's
/// description, for its first posting; the posting, its account clipped
/// to the depth limit; its transaction in the journal, if it has one; and
/// the running total after it, or with --average the running average
/// posting amount (with --historical, including the postings before the
/// report period).
#[derive(Clone, Debug)]
pub struct PostingsReportItem<'j> {
    pub date: Option<Day>,
    pub period: Option<Period>,
    pub description: Option<&'j str>,
    pub posting: Posting,
    pub txn: Option<&'j Transaction>,
    pub total: MixedAmount,
}

impl PostingsReportItem<'_> {
    /// The posting's and running total's amounts styled (styleAmounts).
    pub fn style_amounts(&mut self, styles: &CommodityStyles) {
        self.posting.style_amounts(styles);
        self.total = std::mem::take(&mut self.total).styled(styles);
    }
}

/// A posting with its transaction in the journal, if it has one (a
/// Posting with its ptransaction).
type TxnPosting<'j> = (Posting, Option<&'j Transaction>);

/// The postings report of the journal: the postings the query matches
/// (with --related, their sibling postings), valued as the options say,
/// costs stripped unless shown, negated with --invert, in date order
/// unless sorted; without zero amounts unless --empty or --average; with a
/// report interval, summarised by account in each period; each with the
/// running total or average (postingsReport).
pub fn postings_report<'j>(
    spec: &ReportSpec,
    j: &'j Journal,
) -> Result<Vec<PostingsReportItem<'j>>, String> {
    let ropts = &spec.opts;
    let (reportspan, periods) = spec.report_span_both_dates(j)?;
    let colspans = maybe_day_partition_to_date_spans(periods.as_ref());
    let date2 = ropts.date2;
    let depth = spec.query.depth();
    let (preceding, report) = matched_postings_before_and_during(spec, j, reportspan)?;
    let show_empty = ropts.empty || ropts.average;
    let display: Vec<(TxnPosting, Option<Period>)> = if ropts.interval != Interval::NoInterval {
        summarise_postings_by_interval(date2, &depth, show_empty, &colspans, report)
            .into_iter()
            .map(|(p, period)| ((p, None), Some(period)))
            .collect()
    } else {
        (report.into_iter())
            .filter(|(p, _)| show_empty || !p.amount.looks_zero())
            .map(|tp| (tp, None))
            .collect()
    };
    let sorted = ropts.sort_spec != [SortField::Date(false)];
    let historical = ropts.balance_accum == BalanceAccumulation::Historical;
    if historical && sorted {
        return Err("--historical and --sort should not be used together".to_string());
    }
    let display = if sorted {
        sort_postings(ropts, display)
    } else {
        display
    };
    let start_bal = if historical {
        let sum = sum_postings(preceding.iter().map(|(p, _)| p));
        if ropts.average {
            sum.divide(&Decimal::from_i64(preceding.len() as i64))
        } else {
            sum
        }
    } else {
        MixedAmount::null()
    };
    let start_num = if historical { preceding.len() + 1 } else { 1 };
    Ok(postings_report_items(
        ropts, &depth, start_bal, start_num, display,
    ))
}

/// The account balances a postings report's running total starts from:
/// with --historical (and not --average), the sums of the matched
/// postings before the report period, by account, clipped to the depth
/// limit as the report's are; otherwise none
/// (postingsReportStartBalances).
pub fn postings_report_start_balances(
    spec: &ReportSpec,
    j: &Journal,
) -> Result<BTreeMap<AccountName, MixedAmount>, String> {
    let ropts = &spec.opts;
    let mut balances: BTreeMap<AccountName, MixedAmount> = BTreeMap::new();
    if ropts.balance_accum != BalanceAccumulation::Historical || ropts.average {
        return Ok(balances);
    }
    let (reportspan, _) = spec.report_span_both_dates(j)?;
    let (preceding, _) = matched_postings_before_and_during(spec, j, reportspan)?;
    let depth = spec.query.depth();
    for (p, _) in preceding {
        let a = clip_or_ellipsify_account_name(&depth, &p.account);
        // M.fromListWith maPlus: the later amount on the left
        match balances.get_mut(a) {
            Some(prev) => *prev = p.amount + std::mem::take(prev),
            None => {
                balances.insert(AccountName::from(a), p.amount);
            }
        }
    }
    Ok(balances)
}

/// The function calculating the running total, or with --average the
/// running average, from the item's number, the previous total or
/// average, and the posting's amount (registerRunningCalculationFn).
pub fn register_running_calculation(
    ropts: &ReportOpts,
) -> impl Fn(usize, MixedAmount, &MixedAmount) -> MixedAmount {
    let average = ropts.average;
    move |i, prev, amt| {
        if average {
            let change = (amt - &prev).divide(&Decimal::from_i64(i as i64));
            prev + &change
        } else {
            prev + amt
        }
    }
}

/// The amounts of these postings added up, from the first
/// (sumPostings).
pub fn sum_postings<'a>(ps: impl IntoIterator<Item = &'a Posting>) -> MixedAmount {
    ps.into_iter()
        .fold(MixedAmount::null(), |total, p| total + &p.amount)
}

/// The postings ordered by the sort keys, the earlier ones deciding,
/// keeping their order where all are equal (sortPostings).
fn sort_postings<'j>(
    ropts: &ReportOpts,
    mut ps: Vec<(TxnPosting<'j>, Option<Period>)>,
) -> Vec<(TxnPosting<'j>, Option<Period>)> {
    let date = |(p, t): &TxnPosting| ropts.posting_date(p, *t);
    let description = |(_, t): &TxnPosting<'j>| t.map_or("", |t| t.description.as_str());
    ps.sort_by(|(a, _), (b, _)| {
        for key in &ropts.sort_spec {
            let (o, reversed) = match *key {
                SortField::AbsAmount(r) => (a.0.amount.abs().cmp(&b.0.amount.abs()), r),
                SortField::Amount(r) => (a.0.amount.cmp(&b.0.amount), r),
                SortField::Account(r) => (a.0.account.cmp(&b.0.account), r),
                SortField::Date(r) => (date(a).cmp(&date(b)), r),
                SortField::Description(r) => (description(a).cmp(description(b)), r),
            };
            let o = if reversed { o.reverse() } else { o };
            if o != Ordering::Equal {
                return o;
            }
        }
        Ordering::Equal
    });
    ps
}

/// The postings the query matches before the report span's end, valued,
/// with costs stripped unless --show-costs, negated with --invert, in
/// date order (by secondary date with --date2), split into those before
/// the report span's start and those in it. Date and depth terms in the
/// query are ignored, besides deciding which kind of date the span is in
/// (matchedPostingsBeforeAndDuring).
fn matched_postings_before_and_during<'j>(
    spec: &ReportSpec,
    j: &'j Journal,
    reportspan: DateSpan,
) -> Result<(Vec<TxnPosting<'j>>, Vec<TxnPosting<'j>>), String> {
    let ropts = &spec.opts;
    let q = &spec.query;
    let dateq = q.clone().filter(Query::is_date_or_date2);
    let date2q = dateq.is_date2() || (dateq.is_date() && ropts.date2);
    let date_query = |s: DateSpan| {
        if date2q {
            Query::Date2(s)
        } else {
            Query::Date(s)
        }
    };
    let before_start = date_query(DateSpan::exact(None, reportspan.start()));
    let before_end = date_query(DateSpan::exact(None, reportspan.end()));
    let undated = (q.clone())
        .filter(|q| !q.is_depth())
        .filter(|q| !q.is_date_or_date2());
    let before_and_during = ReportSpec {
        query: Query::And(vec![undated, before_end]),
        ..spec.clone()
    };
    // the postings the query matches, copied from the journal when nothing
    // else would change them, or else from the filtered and valued journal,
    // whose transactions are the journal's, in the same order
    let matched: Vec<TxnPosting<'j>> = if before_and_during.filters_postings_only() {
        let types = |a: &str| j.account_type(a);
        let q = &before_and_during.query;
        let runs: Vec<&[Transaction]> = j.txns.chunks(1024).collect();
        let runs = crate::par::map(j.txns.len(), &runs, |ts| {
            let mut run: Vec<TxnPosting> = (ts.iter())
                .flat_map(|t| t.postings.iter().map(move |p| (p, t)))
                .filter(|&(p, t)| q.matches_posting_extra(&types, PostingRef::new(t, p)))
                .map(|(p, t)| (p.clone(), Some(t)))
                .collect();
            run.shrink_to_fit();
            run
        });
        // in a vector of their size, as a vector grown by doubling would
        // hold room for up to as many again
        let mut matched = Vec::with_capacity(runs.iter().map(Vec::len).sum());
        runs.into_iter().for_each(|run| matched.extend(run));
        matched
    } else {
        let valued = before_and_during.journal_value_and_filter_postings(j)?;
        (valued.txns.into_iter().zip(&j.txns))
            .flat_map(|(vt, t)| vt.postings.into_iter().map(move |p| (p, Some(t))))
            .collect()
    };
    let mut ps: Vec<TxnPosting<'j>> = (matched.into_iter())
        .filter(|(p, _)| p.has_amount())
        .map(|(mut p, t)| {
            if !ropts.show_costs {
                p.amount = std::mem::take(&mut p.amount).strip_costs();
            }
            if ropts.invert {
                p.amount = -std::mem::take(&mut p.amount);
            }
            (p, t)
        })
        .collect();
    ps.sort_by_key(|(p, t)| ropts.posting_date(p, *t));
    let n = ps
        .iter()
        .take_while(|(p, t)| before_start.matches_posting(PostingRef { t: *t, p }))
        .count();
    let during = ps.split_off(n);
    Ok((ps, during))
}

/// The postings summarised in each of these periods, by account clipped
/// to the depth limit (summarisePostingsByInterval).
fn summarise_postings_by_interval(
    date2: bool,
    depth: &DepthSpec,
    show_empty: bool,
    colspans: &[DateSpan],
    ps: Vec<TxnPosting>,
) -> Vec<(Posting, Period)> {
    let date = |(p, t): &TxnPosting| {
        if date2 { p.date2_in(*t) } else { p.date_in(*t) }
    };
    group_by_date_span(show_empty, date, colspans, ps)
        .into_iter()
        .flat_map(|(span, ps)| summarise_postings_in_date_span(span, date2, depth, show_empty, ps))
        .collect()
}

/// The postings of a report period as one summary posting per account,
/// clipped to the depth limit (`...` at depth 0), in account name order,
/// dated at the period's start, without zero amounts unless `show_empty`;
/// with `show_empty`, an empty period has one summary posting of nothing
/// (summarisePostingsInDateSpan).
fn summarise_postings_in_date_span(
    span: DateSpan,
    date2: bool,
    depth: &DepthSpec,
    show_empty: bool,
    ps: Vec<TxnPosting>,
) -> Vec<(Posting, Period)> {
    let period = span.as_period();
    if ps.is_empty() && (span.start().is_none() || span.end().is_none()) {
        return Vec::new();
    }
    let clip = |a: &str| AccountName::from(clip_or_ellipsify_account_name(depth, a));
    let start = span.start().unwrap_or_else(|| {
        ps.first().map_or_else(null_date, |(p, t)| {
            if date2 { p.date2_in(*t) } else { p.date_in(*t) }
        })
    });
    let summary = |account: AccountName, amount: MixedAmount| Posting {
        date: Some(start),
        account,
        amount,
        ..Posting::default()
    };
    if ps.is_empty() && show_empty {
        return vec![(summary(clip(""), MixedAmount::null()), period)];
    }
    // M.fromListWith (flip maPlus): the earlier amount on the left
    let mut balances: BTreeMap<AccountName, MixedAmount> = BTreeMap::new();
    for (p, _) in ps {
        let a = clip_or_ellipsify_account_name(depth, &p.account);
        match balances.get_mut(a) {
            Some(prev) => *prev = std::mem::take(prev) + &p.amount,
            None => {
                balances.insert(AccountName::from(a), p.amount);
            }
        }
    }
    (balances.into_iter())
        .filter(|(_, amt)| show_empty || !amt.looks_zero())
        .map(|(a, amt)| (summary(a, amt), period))
        .collect()
}

/// The report's items from its postings or summary postings: each with
/// its date and description if shown, its account clipped to the depth
/// limit (summary postings' are already), and the running calculation
/// after it, starting from this balance and item number
/// (postingsReportItems, mkpostingsReportItem).
fn postings_report_items<'j>(
    ropts: &ReportOpts,
    depth: &DepthSpec,
    start_bal: MixedAmount,
    start_num: usize,
    ps: Vec<(TxnPosting<'j>, Option<Period>)>,
) -> Vec<PostingsReportItem<'j>> {
    let running = register_running_calculation(ropts);
    let date = |p: &Posting, t: Option<&Transaction>| ropts.posting_date(p, t);
    let mut items = Vec::with_capacity(ps.len());
    let mut bal = start_bal;
    // the previous posting's transaction, date and period (initially nullposting's)
    let mut prev: (Option<*const Transaction>, Day, Option<Period>) = (None, null_date(), None);
    for (i, ((mut p, t), period)) in ps.into_iter().enumerate() {
        let d = date(&p, t);
        let txn = t.map(std::ptr::from_ref);
        let (show_date, show_desc) = if period.is_some() {
            (period != prev.2, false)
        } else {
            let first_in_txn = txn != prev.0;
            (first_in_txn || d != prev.1, first_in_txn)
        };
        prev = (txn, d, period);
        if period.is_none() {
            let clipped = clip_or_ellipsify_account_name(depth, &p.account);
            if clipped != &*p.account {
                p.account = AccountName::from(clipped);
            }
        }
        bal = running(start_num + i, bal, &p.amount);
        items.push(PostingsReportItem {
            date: show_date.then_some(d),
            period,
            description: if show_desc {
                t.map(|t| t.description.as_str())
            } else {
                None
            },
            posting: p,
            txn: t,
            total: bal.clone(),
        });
    }
    items
}

pub(crate) mod tests {
    use super::*;
    use crate::regex::to_regex;
    use crate::reports::periodic::tests::sample_journal;

    fn spec(opts: ReportOpts, query: Query) -> ReportSpec {
        let today = Day::from_ymd(2026, 1, 1).unwrap();
        ReportSpec {
            query,
            ..ReportSpec::new(today, opts).unwrap()
        }
    }

    fn acct(r: &str) -> Query {
        Query::Acct(to_regex(r).unwrap())
    }

    // tests_PostingsReport
    #[cfg_attr(test, test)]
    pub(crate) fn postings_report_counts() {
        let j = sample_journal();
        let count = |opts: ReportOpts, q: Query, j: &Journal| {
            postings_report(&spec(opts, q), j).unwrap().len()
        };
        let def = ReportOpts::default;
        assert_eq!(count(def(), Query::Any, &Journal::default()), 0);
        assert_eq!(count(def(), Query::Any, &j), 13);
        // register --depth just clips account names
        assert_eq!(count(def(), Query::Depth(2), &j), 13);
        let cleared = Query::StatusQ(crate::model::Status::Cleared);
        let q = Query::And(vec![Query::Depth(1), cleared.clone(), acct("expenses")]);
        assert_eq!(count(def(), q, &j), 2);
        let q = Query::And(vec![
            Query::And(vec![Query::Depth(1), cleared]),
            acct("expenses"),
        ]);
        assert_eq!(count(def(), q, &j), 2);
        let monthly = || ReportOpts {
            interval: Interval::Months(1),
            ..def()
        };
        assert_eq!(count(monthly(), Query::Any, &j), 11);
        let monthly_empty = ReportOpts {
            empty: true,
            ..monthly()
        };
        assert_eq!(count(monthly_empty, Query::Any, &j), 20);
        assert_eq!(count(def(), acct("assets:bank:checking"), &j), 5);
    }

    #[cfg_attr(test, test)]
    pub(crate) fn summarise_nothing() {
        assert!(
            summarise_postings_by_interval(false, &DepthSpec::default(), false, &[], vec![])
                .is_empty()
        );
    }

    #[test]
    fn sums_keep_the_order_of_their_amounts() {
        // a sum takes its right operand's style: in a summary posting the
        // later posting's, in a starting balance the earlier one's
        use crate::amount::{Amount, AmountStyle, Side, usd};
        let day = |d| Day::from_ymd(2025, 1, d).unwrap();
        let posting = |d, side| Posting {
            date: Some(day(d)),
            account: "a".into(),
            amount: MixedAmount::from_amount(Amount {
                style: AmountStyle {
                    side,
                    ..AmountStyle::default()
                },
                ..usd(Decimal::from_i64(1))
            }),
            ..Posting::default()
        };
        let side = |m: &MixedAmount| m.amounts_raw()[0].style.side;
        let summary = summarise_postings_in_date_span(
            DateSpan::exact(Some(day(1)), Some(day(31))),
            false,
            &DepthSpec::default(),
            false,
            vec![(posting(1, Side::L), None), (posting(2, Side::R), None)],
        );
        assert_eq!(side(&summary[0].0.amount), Side::R);
        let txn = |d, side| Transaction {
            date: day(d),
            postings: vec![posting(d, side)],
            ..Transaction::default()
        };
        let j = Journal {
            txns: vec![txn(1, Side::L), txn(2, Side::R)],
            ..Journal::default()
        };
        let historical = ReportOpts {
            balance_accum: BalanceAccumulation::Historical,
            ..ReportOpts::default()
        };
        let after = Query::Date(DateSpan::exact(Some(day(10)), None));
        let start = postings_report_start_balances(&spec(historical, after), &j).unwrap();
        assert_eq!(side(&start["a"]), Side::L);
    }
}
