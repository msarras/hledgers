//! Periodic reports (Hledger.Reports.ReportTypes,
//! Hledger.Reports.MultiBalanceReport, Hledger.Reports.BalanceReport): a
//! value for each account and report period, with row totals and averages
//! and a totals row. One engine makes the multi-period balance report; the
//! single-column balance report is its one-period case.

use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::{BTreeMap, HashSet};

use crate::account::tree::{Account, AccountTree, BalanceData, PeriodData, ROOT};
use crate::account::{
    AccountName, AccountType, DepthSpec, account_leaf_name, account_name_clipped_depth,
    account_name_drop, account_name_level, clip_or_ellipsify_account_name,
};
use crate::amount::{
    CommodityStyles, MixedAmount, Precision, Rounding, num, per, styles_with_rounding,
};
use crate::dates::{
    DateSpan, Day, DayPartition, empty_date_span, maybe_day_partition_to_date_spans, null_date,
};
use crate::decimal::Decimal;
use crate::model::{Journal, Posting, PostingRef};
use crate::query::Query;
use crate::reports::opts::{
    AccountListMode, BalanceAccumulation, BalanceCalculation, Layout, NormalSign, ReportOpts,
    ReportSpec, mixed_amount_apply_valuation_after_sum,
};
use crate::valuation::{ConversionOp, PriceOracle};

/// An account's names in a report row: its full name, the name to show,
/// and how many steps to indent it (DisplayName).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DisplayName {
    pub full: AccountName,
    pub name: AccountName,
    pub indent: usize,
}

impl DisplayName {
    /// The full name, not indented (flatDisplayName).
    pub fn flat(a: &str) -> DisplayName {
        DisplayName {
            full: AccountName::from(a),
            name: AccountName::from(a),
            indent: 0,
        }
    }

    /// The leaf name, indented by its level (treeDisplayName).
    pub fn tree(a: &str) -> DisplayName {
        DisplayName {
            full: AccountName::from(a),
            name: AccountName::from(account_leaf_name(a)),
            indent: account_name_level(a),
        }
    }
}

/// A report row: a name, a value for each report period, and their total
/// and average (PeriodicReportRow).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeriodicReportRow<N, A> {
    pub name: N,
    pub amounts: Vec<A>,
    pub total: A,
    pub average: A,
}

impl<N, A> PeriodicReportRow<N, A> {
    /// With the name transformed (prrMapName).
    pub fn map_name<M>(self, f: impl FnOnce(N) -> M) -> PeriodicReportRow<M, A> {
        PeriodicReportRow {
            name: f(self.name),
            amounts: self.amounts,
            total: self.total,
            average: self.average,
        }
    }

    /// Each value transformed (fmap).
    pub fn map_values(&mut self, mut f: impl FnMut(&A) -> A) {
        for a in &mut self.amounts {
            *a = f(a);
        }
        self.total = f(&self.total);
        self.average = f(&self.average);
    }
}

impl<N> PeriodicReportRow<N, MixedAmount> {
    /// The other row's values added to these, period by period, a period
    /// only one has keeping its value; this row's name (prrAdd).
    pub fn plus(mut self, other: &PeriodicReportRow<N, MixedAmount>) -> Self {
        for (i, b) in other.amounts.iter().enumerate() {
            match self.amounts.get_mut(i) {
                Some(a) => *a = &*a + b,
                None => self.amounts.push(b.clone()),
            }
        }
        self.total = &self.total + &other.total;
        self.average = &self.average + &other.average;
        self
    }
}

/// A report of rows of values per period, with the periods and a totals
/// row (PeriodicReport).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeriodicReport<N, A> {
    pub dates: Vec<DateSpan>,
    pub rows: Vec<PeriodicReportRow<N, A>>,
    pub totals: PeriodicReportRow<(), A>,
}

impl<N, A> PeriodicReport<N, A> {
    /// The span of the report's periods, exact (periodicReportSpan).
    pub fn span(&self) -> DateSpan {
        match (self.dates.first(), self.dates.last()) {
            (Some(s), Some(e)) => DateSpan::exact(s.start(), e.end()),
            _ => DateSpan::default(),
        }
    }

    /// With each row's name transformed (prMapName).
    pub fn map_name<M>(self, mut f: impl FnMut(N) -> M) -> PeriodicReport<M, A> {
        PeriodicReport {
            dates: self.dates,
            rows: self.rows.into_iter().map(|r| r.map_name(&mut f)).collect(),
            totals: self.totals,
        }
    }

    /// With each row's name transformed, the rows it gives none for
    /// dropped (prMapMaybeName).
    pub fn map_maybe_name<M>(self, mut f: impl FnMut(N) -> Option<M>) -> PeriodicReport<M, A> {
        let rows = (self.rows.into_iter())
            .filter_map(|r| {
                let PeriodicReportRow {
                    name,
                    amounts,
                    total,
                    average,
                } = r;
                f(name).map(|name| PeriodicReportRow {
                    name,
                    amounts,
                    total,
                    average,
                })
            })
            .collect();
        PeriodicReport {
            dates: self.dates,
            rows,
            totals: self.totals,
        }
    }
}

impl<N, A> PeriodicReport<N, A> {
    /// Each value transformed, the totals' too (fmap).
    pub fn map_values(&mut self, mut f: impl FnMut(&A) -> A) {
        for r in &mut self.rows {
            r.map_values(&mut f);
        }
        self.totals.map_values(f);
    }
}

impl<N> PeriodicReport<N, MixedAmount> {
    /// With every amount styled (HasAmounts' styleAmounts).
    pub fn style_amounts(&mut self, styles: &CommodityStyles) {
        self.map_values(|m| m.clone().styled(styles));
    }
}

/// Several periodic reports for the same periods, each with a title and
/// whether it adds to the overall totals or subtracts, and the overall
/// totals (CompoundPeriodicReport).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompoundPeriodicReport<N, A> {
    pub title: String,
    pub dates: Vec<DateSpan>,
    pub subreports: Vec<(String, PeriodicReport<N, A>, bool)>,
    pub totals: PeriodicReportRow<(), A>,
}

impl<N> CompoundPeriodicReport<N, MixedAmount> {
    /// With every amount styled (HasAmounts' styleAmounts).
    pub fn style_amounts(&mut self, styles: &CommodityStyles) {
        for (_, r, _) in &mut self.subreports {
            r.style_amounts(styles);
        }
        self.totals.map_values(|m| m.clone().styled(styles));
    }
}

/// One section of a compound balance report: its title, the account type
/// selecting its accounts, whether their balances are normally positive or
/// negative, and whether it adds to the report's overall totals or
/// subtracts (CBCSubreportSpec). In every compound command, a section's
/// option transformation sets its normal sign, and its report
/// transformation negates the amounts of a normally negative one, so that
/// they show as positive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubreportSpec {
    pub title: &'static str,
    pub account_type: AccountType,
    pub normal_sign: NormalSign,
    pub increases_total: bool,
}

/// A multi-period balance report: balance changes, or accumulated or
/// historical balances, of each account in each period (MultiBalanceReport).
pub type MultiBalanceReport = PeriodicReport<DisplayName, MixedAmount>;
pub type MultiBalanceReportRow = PeriodicReportRow<DisplayName, MixedAmount>;

/// A single-column balance report: each account's names and amount, and
/// their total (BalanceReport).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BalanceReport {
    pub items: Vec<(DisplayName, MixedAmount)>,
    pub total: MixedAmount,
}

impl BalanceReport {
    /// With every amount styled (HasAmounts' styleAmounts).
    pub fn style_amounts(&mut self, styles: &CommodityStyles) {
        for (_, m) in &mut self.items {
            *m = std::mem::take(m).styled(styles);
        }
        self.total = std::mem::take(&mut self.total).styled(styles);
    }
}

/// The balance report of the matched accounts over the report period: the
/// multi-period report's one period, each row's total being its amount
/// (balanceReport).
pub fn balance_report(spec: &ReportSpec, j: &Journal) -> Result<BalanceReport, String> {
    let report = multi_balance_report(spec, j)?;
    Ok(BalanceReport {
        items: report.rows.into_iter().map(|r| (r.name, r.total)).collect(),
        total: report.totals.total,
    })
}

/// The styles to show a balance report's amounts with: the journal's
/// commodity styles, hard-rounded; none for counts, which keep their own
/// (balanceReportStyles).
pub fn balance_report_styles(ropts: &ReportOpts, j: &Journal) -> CommodityStyles {
    if is_count(ropts.balance_calc) {
        CommodityStyles::new()
    } else {
        styles_with_rounding(Rounding::Hard, &j.commodity_styles())
    }
}

/// Whether the report counts postings or transactions
/// (balanceCalcIsCount).
fn is_count(calc: BalanceCalculation) -> bool {
    matches!(
        calc,
        BalanceCalculation::PostingsCount | BalanceCalculation::TransactionsCount
    )
}

/// The multi-period balance report of the matched accounts: in each report
/// period, their balance changes, or their balances accumulated from the
/// report's start or from the journal's start, valued as the options say,
/// with --invert's and --percent's signs and proportions, without the
/// accounts the options make boring (multiBalanceReport).
pub fn multi_balance_report(spec: &ReportSpec, j: &Journal) -> Result<MultiBalanceReport, String> {
    let oracle = PriceOracle::new(spec.opts.infer_prices, spec.opts.debug, j);
    multi_balance_report_with(spec, j, &oracle)
}

/// Like [`multi_balance_report`], with this price oracle
/// (multiBalanceReportWith).
pub fn multi_balance_report_with(
    spec: &ReportSpec,
    j: &Journal,
    oracle: &PriceOracle,
) -> Result<MultiBalanceReport, String> {
    let (span, colspans) = spec.report_span(j)?;
    let spec = make_report_query(spec, span);
    let valued = get_postings(&spec, j, oracle, span)?;
    let postings: Vec<PostingRef> = valued.postings().collect();
    let acct = generate_multi_balance_account(&spec, j, oracle, colspans.as_ref(), &postings)?;
    generate_multi_balance_report(&spec.opts, colspans.as_ref(), acct)
}

/// A compound balance report: for each section, the multi-period balance
/// report of the postings its account type selects among those the
/// report's query matches, with its normal sign and its normally negative
/// amounts negated; and the overall totals, each section's totals added
/// or (unless counting, when every count adds) subtracted
/// (compoundBalanceReport, compoundBalanceReportWith).
pub fn compound_balance_report(
    spec: &ReportSpec,
    j: &Journal,
    sections: &[SubreportSpec],
) -> Result<CompoundPeriodicReport<DisplayName, MixedAmount>, String> {
    let oracle = PriceOracle::new(spec.opts.infer_prices, spec.opts.debug, j);
    let (span, colspans) = spec.report_span(j)?;
    let spec = make_report_query(spec, span);
    let valued = get_postings(&spec, j, &oracle, span)?;
    let counting = is_count(spec.opts.balance_calc);
    let types = |a: &str| j.account_type(a);
    let mut subreports = Vec::with_capacity(sections.len());
    for s in sections {
        let q = Query::Type(vec![s.account_type]);
        let negative = s.normal_sign == NormalSign::NormallyNegative;
        let subspec = ReportSpec {
            opts: ReportOpts {
                normal_balance: Some(s.normal_sign),
                ..spec.opts.clone()
            },
            query: Query::And(vec![q.clone(), spec.query.clone()]),
            ..spec.clone()
        };
        let matched: Vec<PostingRef> = (valued.postings())
            .filter(|p| q.matches_posting_extra(&types, *p))
            .collect();
        // with --count, a normally negative section counts each posting
        // as -1, so that its negation shows positive counts
        let negated: Vec<Posting>;
        let postings = if counting && negative {
            negated = (matched.iter())
                .map(|p| Posting {
                    amount: -&p.p.amount,
                    ..p.p.clone()
                })
                .collect();
            (matched.iter().zip(&negated))
                .map(|(p, n)| PostingRef { t: p.t, p: n })
                .collect()
        } else {
            matched
        };
        let acct =
            generate_multi_balance_account(&subspec, j, &oracle, colspans.as_ref(), &postings)?;
        let mut report = generate_multi_balance_report(&subspec.opts, colspans.as_ref(), acct)?;
        if negative {
            report.map_values(|m| -m);
        }
        subreports.push((s.title.to_string(), report, s.increases_total));
    }
    let totals = (subreports.iter())
        .map(|(_, r, increases_total)| {
            let mut t = r.totals.clone();
            if !(*increases_total || counting) {
                t.map_values(|m| -m);
            }
            t
        })
        .reduce(|a, b| a.plus(&b))
        .unwrap_or(PeriodicReportRow {
            name: (),
            amounts: Vec::new(),
            total: MixedAmount::null(),
            average: MixedAmount::null(),
        });
    Ok(CompoundPeriodicReport {
        title: String::new(),
        dates: maybe_day_partition_to_date_spans(colspans.as_ref()),
        subreports,
        totals,
    })
}

/// The report spec with the query's dates replaced by the report span's,
/// if there is one (makeReportQuery).
pub fn make_report_query(spec: &ReportSpec, span: DateSpan) -> ReportSpec {
    if span == DateSpan::default() {
        return spec.clone();
    }
    let dateless = spec.query.clone().filter(|q| !q.is_date_or_date2());
    let dates = if spec.opts.date2 {
        Query::Date2(span)
    } else {
        Query::Date(span)
    };
    ReportSpec {
        query: Query::And(vec![dateless, dates]).simplify(),
        ..spec.clone()
    }
}

/// The journal with just the postings, and parts of amounts, the query
/// matches (or with --related, their sibling postings) within the report
/// span, which for a report needing the postings before its start is all
/// time to the report's end; valued as the options say, unless that
/// happens after summing; without postings that have no amount; with every
/// amount 1 when counting (getPostings, journalValueAndFilterPostingsWith).
pub fn get_postings<'j>(
    spec: &ReportSpec,
    j: &'j Journal,
    oracle: &PriceOracle,
    span: DateSpan,
) -> Result<Cow<'j, Journal>, String> {
    let ropts = &spec.opts;
    let full_span = if ropts.requires_historical() {
        DateSpan::exact(None, span.end())
    } else {
        span
    };
    let full_span_query = {
        let s = if full_span == DateSpan::default() {
            empty_date_span()
        } else {
            full_span
        };
        if ropts.date2 {
            Query::Date2(s)
        } else {
            Query::Date(s)
        }
    };
    let depthless = spec.query.clone().filter(|q| !q.is_depth());
    let dateless = depthless.filter(|q| !q.is_date_or_date2());
    let full_query = Query::And(vec![dateless, full_span_query]);
    let after_sum = ropts.valuation_after_sum().is_some();
    let spec = ReportSpec {
        query: full_query,
        opts: ReportOpts {
            period: full_span.as_period(),
            value: if after_sum { None } else { ropts.value.clone() },
            ..ropts.clone()
        },
        ..spec.clone()
    };
    let count = is_count(ropts.balance_calc);
    // when nothing is converted, valued or counted, and the query matches
    // every posting, all with amounts, those are the journal's own
    let unchanged = !count
        && !spec.opts.related
        && spec.opts.value.is_none()
        && matches!(
            spec.opts.conversion_op,
            None | Some(ConversionOp::NoConversionOp)
        )
        && spec.query.clone().filter(Query::is_amt_or_cur).is_null()
        && {
            let types = |a: &str| j.account_type(a);
            let q = &spec.query;
            crate::par::map(j.txns.len(), &j.txns, |t| {
                (t.postings.iter()).all(|p| {
                    p.has_amount() && q.matches_posting_extra(&types, PostingRef::new(t, p))
                })
            })
            .into_iter()
            .all(|all| all)
        };
    if unchanged {
        return Ok(Cow::Borrowed(j));
    }
    let mut valued = spec.journal_value_and_filter_postings_with(j, oracle)?;
    for t in &mut valued.txns {
        t.postings.retain(|p| p.has_amount());
        if count {
            for p in &mut t.postings {
                p.amount = MixedAmount::from_amount(num(Decimal::from_i64(1)));
            }
        }
    }
    Ok(Cow::Owned(valued))
}

/// The account tree of a multi-period balance report, from its postings:
/// their accounts clipped to the query's depth, one posting per
/// transaction and account when counting transactions; balances
/// accumulated and valued as the options say; boring accounts marked;
/// signs inverted with --invert; the declared accounts the query matches
/// added with --declared and --empty; declaration orders set
/// (generateMultiBalanceAccount).
pub fn generate_multi_balance_account(
    spec: &ReportSpec,
    j: &Journal,
    oracle: &PriceOracle,
    colspans: Option<&DayPartition>,
    postings: &[PostingRef],
) -> Result<AccountTree<BalanceData>, String> {
    let ropts = &spec.opts;
    let depth_spec = spec.query.clone().filter(Query::is_depth).depth();
    let mut clipped: Vec<(PostingRef, &str)> = postings
        .iter()
        .map(|p| {
            (
                *p,
                clip_or_ellipsify_account_name(&depth_spec, &p.p.account),
            )
        })
        .collect();
    if ropts.balance_calc == BalanceCalculation::TransactionsCount {
        let mut seen = HashSet::new();
        clipped.retain(|(p, a)| seen.insert((p.t.map(|t| t.index), *a)));
    }
    let mut acct = calculate_report_account(spec, j, oracle, colspans, &clipped)?;
    mark_account_boring(spec, &mut acct);
    if ropts.invert {
        acct = acct.map_data(|d| d.map(|b| b.map(|m| -m)));
    }
    if ropts.declared && ropts.empty {
        acct = add_declared_accounts(spec, j, acct);
    }
    acct.set_declarations(j);
    Ok(acct)
}

/// The tree with the declared accounts the query's account, type and tag
/// terms match (clipped to its depth) added, not boring, with zero
/// balances (addDeclaredAccounts).
fn add_declared_accounts(
    spec: &ReportSpec,
    j: &Journal,
    acct: AccountTree<BalanceData>,
) -> AccountTree<BalanceData> {
    let depth_spec = spec.query.clone().filter(Query::is_depth).depth();
    let q = (spec.query.clone()).filter_or_not(|q| q.is_acct() || q.is_type() || q.is_tag());
    let types = |a: &str| j.account_type(a);
    let tags = |a: &str| {
        j.declared_account_tags
            .get(a)
            .map_or(&[][..], Vec::as_slice)
    };
    let mut names: Vec<&str> = j.declared_account_names().map(|a| &**a).collect();
    names.sort_unstable();
    names.dedup();
    let declared: Vec<&str> = names
        .into_iter()
        .filter(|a| q.matches_account_extra(&types, &tags, a))
        .map(|a| clip_or_ellipsify_account_name(&depth_spec, a))
        .collect();
    let zeros = acct.root().data.map(|_| BalanceData::default());
    let mut declared_tree =
        AccountTree::from_names("root", zeros.clone(), declared.iter().copied(), |_| {
            zeros.clone()
        });
    let set: HashSet<&str> = declared.iter().copied().collect();
    for a in &mut declared_tree.accounts {
        a.boring = !set.contains(&*a.name);
    }
    acct.merge(&declared_tree).map_data(|d| {
        d.map(|v| match v {
            (Some(a), _) => a.clone(),
            (None, Some(b)) => b.clone(),
            (None, None) => BalanceData::default(),
        })
    })
}

/// The account tree of these postings (with their clipped account names),
/// with a value for every report period: changes, or balances accumulated
/// from the report's start or from the journal's start, valued after
/// summing if the options say so. With no report periods, just a root
/// with one empty period (calculateReportAccount).
fn calculate_report_account(
    spec: &ReportSpec,
    j: &Journal,
    oracle: &PriceOracle,
    colspans: Option<&DayPartition>,
    postings: &[(PostingRef, &str)],
) -> Result<AccountTree<BalanceData>, String> {
    let Some(colspans) = colspans else {
        let data = PeriodData::from_list(
            BalanceData::default(),
            [(null_date(), BalanceData::default())],
        );
        return Ok(AccountTree::from_names("root", data, [], |_| {
            PeriodData::default()
        }));
    };
    let ropts = &spec.opts;
    let ends = colspans.to_period_data();
    let changes = AccountTree::from_postings(postings.iter().map(|(p, a)| {
        let start = colspans.find(ropts.posting_date(p.p, p.t)).0;
        (start, *a, &p.p.amount)
    }));
    let value = mixed_amount_apply_valuation_after_sum(ropts, j, oracle);
    // the valued amounts at each period's end (periodDataValuation)
    let avalue = |pd: PeriodData<BalanceData>| -> Result<PeriodData<BalanceData>, String> {
        let Some(value) = &value else {
            return Ok(pd);
        };
        let at = |d: Day, b: &BalanceData| -> Result<BalanceData, String> {
            Ok(BalanceData {
                excl: value(d, &b.excl)?,
                incl: value(d, &b.incl)?,
                postings: b.postings,
            })
        };
        let mut periods = BTreeMap::new();
        for (start, end) in &ends.periods {
            if let Some(b) = pd.periods.get(start) {
                periods.insert(*start, at(*end, b)?);
            }
        }
        Ok(PeriodData {
            pre: at(ends.pre, &pd.pre)?,
            periods,
        })
    };
    // per-period changes, zero-based cumulative totals, or historical
    // balances, from the unvalued changes (rowbals)
    let rowbals = |unvalued: PeriodData<BalanceData>| -> Result<PeriodData<BalanceData>, String> {
        let historical = |u: &PeriodData<BalanceData>| avalue(u.cumulative_sum());
        let changes = |u: PeriodData<BalanceData>| match ropts.balance_calc {
            BalanceCalculation::ValueChange | BalanceCalculation::Gain => {
                Ok(historical(&u)?.period_changes())
            }
            _ => avalue(u),
        };
        match ropts.balance_accum {
            BalanceAccumulation::PerPeriod => changes(unvalued),
            BalanceAccumulation::Cumulative => {
                let c = changes(unvalued)?;
                Ok(PeriodData {
                    pre: BalanceData::default(),
                    ..c
                }
                .cumulative_sum())
            }
            BalanceAccumulation::Historical => historical(&unvalued),
        }
    };
    let mut accounts = Vec::with_capacity(changes.accounts.len());
    for mut a in changes.accounts {
        a.data.pad(&BalanceData::default(), &ends);
        a.data = rowbals(a.data)?;
        accounts.push(a);
    }
    Ok(AccountTree { accounts })
}

/// Mark the accounts a report can leave out (markAccountBoring). With a
/// depth limit of 0, every account but the root. Otherwise the root, and
/// each account both boring itself (deeper than its depth limit, or with
/// all its period amounts looking zero, unless --empty keeps it) and a
/// boring parent (in list mode always; in tree mode, one with fewer than
/// two interesting subaccounts, or one with --no-elide or at the fold
/// depth, or dropped by --drop).
fn mark_account_boring(spec: &ReportSpec, acct: &mut AccountTree<BalanceData>) {
    let ropts = &spec.opts;
    let depth_spec = spec.query.depth();
    if depth_spec
        == (DepthSpec {
            flat: Some(0),
            regexps: vec![],
        })
    {
        for a in &mut acct.accounts {
            a.boring = true;
        }
        acct.accounts[ROOT].boring = false;
        return;
    }
    let tree = ropts.account_list_mode == AccountListMode::Tree;
    let strip_costs = ropts.conversion_op != Some(ConversionOp::NoConversionOp);
    let has_postings = |a: &Account<BalanceData>| a.data.periods.values().any(|b| b.postings > 0);
    // subaccounts first, their marks being needed
    for i in (0..acct.accounts.len()).rev() {
        let a = &acct.accounts[i];
        let level = account_name_level(&a.name);
        let qdepth = account_name_clipped_depth(&depth_spec, &a.name).unwrap_or(usize::MAX);
        let too_deep = level > qdepth;
        let keep_when_empty = if tree {
            a.subs.is_empty()
        } else {
            has_postings(a)
                || (ropts.no_elide && level > ropts.drop && acct.any(i, &|a| has_postings(a)))
        };
        let keep_empty = ropts.empty && keep_when_empty;
        let all_zeros = !keep_empty
            && a.data.periods.values().all(|b| {
                let m = if tree && level == qdepth {
                    &b.incl
                } else {
                    &b.excl
                };
                if strip_costs {
                    m.clone().strip_costs().looks_zero()
                } else {
                    m.looks_zero()
                }
            });
        let boring_parent = !tree || {
            let at_fold_depth = ropts.fold_depth == Some(level);
            let minimum = if ropts.no_elide || at_fold_depth {
                1
            } else {
                2
            };
            let interesting = (a.subs.iter())
                .filter(|&&s| acct.any(s, &|a| !a.boring))
                .count();
            interesting < minimum || level <= ropts.drop
        };
        acct.accounts[i].boring = (too_deep || all_zeros) && boring_parent;
    }
    acct.accounts[ROOT].boring = true;
}

/// The report from the account tree: its rows, sorted by amount or by
/// declaration, the totals row from the root's balances including
/// subaccounts, then percentages with --percent, and averages of counts
/// shown with one decimal place (generateMultiBalanceReport).
pub fn generate_multi_balance_report(
    ropts: &ReportOpts,
    colspans: Option<&DayPartition>,
    acct: AccountTree<BalanceData>,
) -> Result<MultiBalanceReport, String> {
    let mut report = generate_periodic_report(
        ropts,
        colspans,
        acct,
        |balance, data| make_row(ropts, balance, data),
        |b: &BalanceData| b.incl.clone(),
        MixedAmount::clone,
    );
    if ropts.percent {
        report = report_percent(ropts, report)?;
    }
    if is_count(ropts.balance_calc) && !ropts.percent {
        let one = |m: &MixedAmount| m.clone().with_precision(Precision::Fixed(1));
        for r in &mut report.rows {
            r.average = one(&r.average);
        }
        report.totals.average = one(&report.totals.average);
    }
    Ok(report)
}

/// Which of an account's balances in a period a row shows: including or
/// excluding its subaccounts' (bdincludingsubs, bdexcludingsubs).
pub type Balance = fn(&BalanceData) -> &MixedAmount;

fn incl(b: &BalanceData) -> &MixedAmount {
    &b.incl
}

fn excl(b: &BalanceData) -> &MixedAmount {
    &b.excl
}

/// A periodic report from an account tree whose accounts have data of
/// type `T` in each period: its rows, made by `make_row` from an account's
/// period data and the balance to show, sorted by amount (the amount
/// `tree_amount` reads from an account's data in tree mode, or
/// `flat_amount` from a row's total in list mode) or by declaration; the
/// totals row from the root's balances including subaccounts
/// (generatePeriodicReport).
pub fn generate_periodic_report<T, C>(
    ropts: &ReportOpts,
    colspans: Option<&DayPartition>,
    mut acct: AccountTree<T>,
    make_row: impl Fn(Balance, &PeriodData<T>) -> PeriodicReportRow<(), C>,
    tree_amount: impl Fn(&T) -> MixedAmount,
    flat_amount: impl Fn(&C) -> MixedAmount,
) -> PeriodicReport<DisplayName, C> {
    let rows = match (ropts.account_list_mode, ropts.sort_amount) {
        (AccountListMode::Tree, true) => {
            sort_tree_by_amount(ropts, &mut acct, tree_amount);
            build_report_rows(ropts, &acct, &make_row)
        }
        (AccountListMode::Flat, true) => sort_flat_by_amount(
            ropts,
            build_report_rows(ropts, &acct, &make_row),
            flat_amount,
        ),
        _ => {
            acct.sort_by_declaration();
            build_report_rows(ropts, &acct, &make_row)
        }
    };
    let totals = make_row(incl, &acct.root().data);
    PeriodicReport {
        dates: maybe_day_partition_to_date_spans(colspans),
        rows,
        totals,
    }
}

/// Each account's subaccounts sorted by amount, largest first (or for
/// normally negative accounts, smallest first), then by name: the sum of
/// the amounts `amount` reads from their period data, or for accumulated
/// balances the last (sortTreeByAmount).
fn sort_tree_by_amount<T>(
    ropts: &ReportOpts,
    acct: &mut AccountTree<T>,
    amount: impl Fn(&T) -> MixedAmount,
) {
    let amount = |a: &Account<T>| -> MixedAmount {
        let m = match ropts.balance_accum {
            BalanceAccumulation::PerPeriod => a.data.periods.values().map(&amount).sum(),
            _ => a.data.last().map_or_else(MixedAmount::null, &amount),
        };
        m.strip_costs()
    };
    match ropts.normal_balance {
        Some(NormalSign::NormallyNegative) => {
            acct.sort_subs_by_key(|a| (amount(a), a.name.clone()))
        }
        _ => acct.sort_subs_by_key(|a| (Reverse(amount(a)), a.name.clone())),
    }
}

/// The rows sorted by the amount `amount` reads from their totals, largest
/// first (or for normally negative accounts, smallest first), then by full
/// name (sortFlatByAmount).
fn sort_flat_by_amount<C>(
    ropts: &ReportOpts,
    mut rows: Vec<PeriodicReportRow<DisplayName, C>>,
    amount: impl Fn(&C) -> MixedAmount,
) -> Vec<PeriodicReportRow<DisplayName, C>> {
    let amount = |r: &PeriodicReportRow<DisplayName, C>| amount(&r.total).strip_costs();
    match ropts.normal_balance {
        Some(NormalSign::NormallyNegative) => {
            rows.sort_by_cached_key(|r| (amount(r), r.name.full.clone()))
        }
        _ => rows.sort_by_cached_key(|r| (Reverse(amount(r)), r.name.full.clone())),
    }
    rows
}

/// The report's rows in tree order: one for each account that is not
/// boring, or is boring but must be shown; boring parents left out with
/// their names (in tree mode) prefixed to their first interesting
/// subaccount's, and the accounts --drop drops left out
/// (buildReportRows).
fn build_report_rows<T, C>(
    ropts: &ReportOpts,
    acct: &AccountTree<T>,
    make_row: &impl Fn(Balance, &PeriodData<T>) -> PeriodicReportRow<(), C>,
) -> Vec<PeriodicReportRow<DisplayName, C>> {
    let mut rows = Vec::new();
    let mut add = RowAdder {
        ropts,
        acct,
        make_row,
        rows: &mut rows,
    };
    add.rows(true, -(ropts.drop as isize), 0, ROOT);
    rows
}

/// What adding the rows of an account and its subaccounts needs
/// (buildReportRows' mkRows).
struct RowAdder<'a, T, C, F> {
    ropts: &'a ReportOpts,
    acct: &'a AccountTree<T>,
    make_row: &'a F,
    rows: &'a mut Vec<PeriodicReportRow<DisplayName, C>>,
}

impl<T, C, F: Fn(Balance, &PeriodData<T>) -> PeriodicReportRow<(), C>> RowAdder<'_, T, C, F> {
    /// Add the rows of an account at this depth, below this many boring
    /// parents, and of its subaccounts (mkRows).
    fn rows(&mut self, is_root: bool, d: isize, boring_parents: usize, i: usize) {
        fn all_boring<T>(acct: &AccountTree<T>, i: usize) -> bool {
            let a = &acct.accounts[i];
            a.boring && a.subs.iter().all(|&s| all_boring(acct, s))
        }
        let (ropts, acct) = (self.ropts, self.acct);
        let a = &acct.accounts[i];
        let tree = ropts.account_list_mode == AccountListMode::Tree;
        let can_omit_parents = !tree || !ropts.no_elide;
        let (sub_d, sub_boring_parents) = if all_boring(acct, i) {
            return;
        } else if a.boring && is_root {
            (d, 0)
        } else if a.boring && d < 0 {
            (d + 1, 0)
        } else if a.boring && can_omit_parents {
            (d, boring_parents + 1)
        } else {
            let name = displayed_name(ropts, d, boring_parents, &a.name);
            let balance = if tree { incl } else { excl };
            let row = (self.make_row)(balance, &a.data).map_name(|()| name);
            self.rows.push(row);
            (d + 1, 0)
        };
        for &s in &a.subs {
            self.rows(false, sub_d, sub_boring_parents, s);
        }
    }
}

/// An account's names in its row: in tree mode its leaf name, after those
/// of the boring parents left out above it, indented by its depth; in list
/// mode its full name; either way without the components --drop drops.
/// The root's row, shown when the depth is 0, is `...` (displayedName).
fn displayed_name(
    ropts: &ReportOpts,
    d: isize,
    boring_parents: usize,
    name: &AccountName,
) -> DisplayName {
    if d == 0 && &**name == "root" {
        return DisplayName::flat("...");
    }
    let dropped = account_name_drop(ropts.drop, name);
    let (shown, indent) = match ropts.account_list_mode {
        AccountListMode::Tree => {
            let components: Vec<&str> = dropped.split(':').collect();
            let first = components.len().saturating_sub(boring_parents + 1);
            (components[first..].join(":"), d.max(0) as usize)
        }
        AccountListMode::Flat => (dropped, 0),
    };
    DisplayName {
        full: name.clone(),
        name: AccountName::from(shown),
        indent,
    }
}

/// A report row from an account's period data: its amounts, their total
/// (or for accumulated balances, the last) and their average
/// (makeMultiBalanceReportRow, makePeriodicReportRow).
fn make_row(
    ropts: &ReportOpts,
    balance: Balance,
    data: &PeriodData<BalanceData>,
) -> PeriodicReportRow<(), MixedAmount> {
    let amounts: Vec<MixedAmount> = data.periods.values().map(|b| balance(b).clone()).collect();
    let (sum, average) = MixedAmount::sum_and_average(&amounts);
    let total = match ropts.balance_accum {
        BalanceAccumulation::PerPeriod => sum,
        _ => amounts.last().cloned().unwrap_or_default(),
    };
    PeriodicReportRow {
        name: (),
        amounts,
        total,
        average,
    }
}

/// The report with each value as a percentage of its column's total
/// (reportPercent). Upstream calculates a percentage when it is shown, so
/// one that cannot be calculated is an error only then; here the values
/// a report does not show are left null instead: unless the output shows
/// every value, the period values with --summary-only (unless the
/// bare-wide layout lists their commodities), the row totals unless the
/// report has one period (the single-column report shows its totals) or a
/// row total column, and the averages without --average.
fn report_percent(
    ropts: &ReportOpts,
    report: MultiBalanceReport,
) -> Result<MultiBalanceReport, String> {
    let all = ropts.all_values_shown;
    let shown_periods = all || !ropts.summary_only || ropts.layout == Ok(Layout::BareWide);
    let shown_total = all || report.dates.len() <= 1 || ropts.has_totals_column();
    let t = &report.totals;
    let percent = |amounts: &[MixedAmount], total: &MixedAmount, average: &MixedAmount| {
        let amounts = if shown_periods {
            (amounts.iter().zip(&t.amounts))
                .map(|(a, b)| perdivide(a, b))
                .collect::<Result<Vec<_>, _>>()?
        } else {
            vec![MixedAmount::null(); amounts.len()]
        };
        let total = if shown_total {
            perdivide(total, &t.total)?
        } else {
            MixedAmount::null()
        };
        let average = if all || ropts.average {
            perdivide(average, &t.average)?
        } else {
            MixedAmount::null()
        };
        Ok::<_, String>((amounts, total, average))
    };
    let mut rows = Vec::with_capacity(report.rows.len());
    for r in &report.rows {
        let (amounts, total, average) = percent(&r.amounts, &r.total, &r.average)?;
        rows.push(PeriodicReportRow {
            name: r.name.clone(),
            amounts,
            total,
            average,
        });
    }
    let (amounts, total, average) = percent(&t.amounts, &t.total, &t.average)?;
    Ok(PeriodicReport {
        dates: report.dates.clone(),
        rows,
        totals: PeriodicReportRow {
            name: (),
            amounts,
            total,
            average,
        },
    })
}

/// The first amount as a percentage of the second's magnitude, which
/// should be in the same single commodity (perdivide).
fn perdivide(a: &MixedAmount, b: &MixedAmount) -> Result<MixedAmount, String> {
    let error = || {
        "Cannot calculate percentages if accounts have different commodities (Hint: Try --cost, -V or similar flags.)"
            .to_string()
    };
    let (a, b) = (a.unify().ok_or_else(error)?, b.unify().ok_or_else(error)?);
    if !(a.is_zero() || b.is_zero() || a.commodity == b.commodity) {
        return Err(error());
    }
    let q = if b.quantity.is_zero() {
        Decimal::ZERO
    } else {
        &(&a.quantity / &b.quantity.abs()) * &Decimal::from_i64(100)
    };
    Ok(MixedAmount::from_amount(per(q)))
}

pub(crate) mod tests {
    use super::*;
    use crate::amount::usd;
    use crate::read::InputOpts;

    /// The journal of this text, finalised.
    pub(crate) fn read(s: &str) -> Journal {
        let iopts = InputOpts {
            today: Day::from_ymd(2026, 1, 1).unwrap(),
            ..InputOpts::default()
        };
        let (j, st) = crate::read::journal::parse_journal(&iopts, "f.j", s).unwrap();
        let mut j = crate::read::finalise::journal_finalise(&iopts, "f.j", s, j, st).unwrap();
        crate::read::finalise::journal_number_and_tie_transactions(&mut j);
        j
    }

    /// samplejournal: examples/sample.journal plus a loan taken and paid off.
    pub(crate) fn sample_journal() -> Journal {
        read(concat!(
            "2008/01/01 income\n    assets:bank:checking  $1\n    income:salary\n\n",
            "2008/06/01 gift\n    assets:bank:checking  $1\n    income:gifts\n\n",
            "2008/06/02 save\n    assets:bank:saving  $1\n    assets:bank:checking\n\n",
            "2008/06/03 * eat & shop\n    expenses:food      $1\n    expenses:supplies  $1\n    assets:cash\n\n",
            "2008/10/01 take a loan\n    assets:bank:checking  $1\n    liabilities:debts  $-1\n\n",
            "2008/12/31 pay off\n    liabilities:debts  $1\n    assets:bank:checking\n",
        ))
    }

    fn day(y: i64, m: u32, d: u32) -> Day {
        Day::from_ymd(y, m, d).unwrap()
    }

    fn usds(n: i64) -> MixedAmount {
        MixedAmount::from_amount(usd(Decimal::from_i64(n)))
    }

    /// The spec of these options and this query, with the flags' query
    /// added, as the tests' `gives` does.
    fn spec(opts: ReportOpts, query: Query) -> ReportSpec {
        let base = ReportSpec::new(day(2026, 1, 1), opts).unwrap();
        ReportSpec {
            query: Query::And(vec![base.opts.query_from_flags(), query]).simplify(),
            ..base
        }
    }

    fn items(r: &BalanceReport) -> Vec<(String, String, usize, MixedAmount)> {
        (r.items.iter())
            .map(|(n, m)| (n.full.to_string(), n.name.to_string(), n.indent, m.clone()))
            .collect()
    }

    fn flat(rows: &[(&str, i64)]) -> Vec<(String, String, usize, MixedAmount)> {
        (rows.iter())
            .map(|(a, n)| (a.to_string(), a.to_string(), 0, usds(*n)))
            .collect()
    }

    // tests_BalanceReport.
    #[cfg_attr(test, test)]
    pub(crate) fn balance_reports() {
        let j = sample_journal();
        let gives = |s: ReportSpec, j: &Journal, rows: Vec<_>, total: MixedAmount| {
            let r = balance_report(&s, j).unwrap();
            assert_eq!(items(&r), rows);
            assert_eq!(r.total, total);
        };
        let any = || Query::Any;
        let opts = ReportOpts::default;
        gives(
            spec(opts(), any()),
            &Journal::default(),
            vec![],
            MixedAmount::null(),
        );
        gives(
            spec(opts(), any()),
            &j,
            flat(&[
                ("assets:bank:checking", 1),
                ("assets:bank:saving", 1),
                ("assets:cash", -2),
                ("expenses:food", 1),
                ("expenses:supplies", 1),
                ("income:gifts", -1),
                ("income:salary", -1),
            ]),
            usds(0),
        );
        let tree = ReportOpts {
            account_list_mode: AccountListMode::Tree,
            ..opts()
        };
        let t = |a: &str, n: &str, i: usize, q: i64| (a.to_string(), n.to_string(), i, usds(q));
        gives(
            spec(tree, any()),
            &j,
            vec![
                t("assets", "assets", 0, 0),
                t("assets:bank", "bank", 1, 2),
                t("assets:bank:checking", "checking", 2, 1),
                t("assets:bank:saving", "saving", 2, 1),
                t("assets:cash", "cash", 1, -2),
                t("expenses", "expenses", 0, 2),
                t("expenses:food", "food", 1, 1),
                t("expenses:supplies", "supplies", 1, 1),
                t("income", "income", 0, -2),
                t("income:gifts", "gifts", 1, -1),
                t("income:salary", "salary", 1, -1),
            ],
            usds(0),
        );
        let depth1 = ReportOpts {
            depth: DepthSpec {
                flat: Some(1),
                regexps: vec![],
            },
            ..opts()
        };
        let depth1_rows = flat(&[("expenses", 2), ("income", -2)]);
        gives(spec(depth1, any()), &j, depth1_rows.clone(), usds(0));
        gives(spec(opts(), Query::Depth(1)), &j, depth1_rows, usds(0));
        // samplejournal2: one transaction with a secondary date
        let j2 =
            read("2008/01/01=2009/01/01 income\n    assets:bank:checking  $1\n    income:salary\n");
        let y2009 = DateSpan::exact(Some(day(2009, 1, 1)), Some(day(2010, 1, 1)));
        gives(
            spec(opts(), Query::Date(y2009)),
            &j2,
            vec![],
            MixedAmount::null(),
        );
        gives(
            spec(opts(), Query::Date2(y2009)),
            &j2,
            flat(&[("assets:bank:checking", 1), ("income:salary", -1)]),
            usds(0),
        );
        let income = || Query::Desc(crate::regex::to_regex_ci("income").unwrap());
        gives(
            spec(opts(), income()),
            &j,
            flat(&[("assets:bank:checking", 1), ("income:salary", -1)]),
            usds(0),
        );
        gives(
            spec(opts(), Query::Not(Box::new(income()))),
            &j,
            flat(&[
                ("assets:bank:saving", 1),
                ("assets:cash", -2),
                ("expenses:food", 1),
                ("expenses:supplies", 1),
                ("income:gifts", -1),
            ]),
            usds(0),
        );
        let period = |b: Day, e: Day| ReportOpts {
            period: crate::dates::Period::PeriodBetween(b, e),
            ..opts()
        };
        gives(
            spec(period(day(2008, 1, 1), day(2008, 1, 2)), any()),
            &j,
            flat(&[("assets:bank:checking", 1), ("income:salary", -1)]),
            usds(0),
        );
        gives(
            spec(period(day(2008, 1, 2), day(2008, 1, 3)), any()),
            &j,
            vec![],
            MixedAmount::null(),
        );
    }

    // tests_MultiBalanceReport.
    #[cfg_attr(test, test)]
    pub(crate) fn multi_balance_reports() {
        let r = multi_balance_report(
            &spec(ReportOpts::default(), Query::Any),
            &Journal::default(),
        )
        .unwrap();
        assert!(r.rows.is_empty());
        assert_eq!(r.totals.total, MixedAmount::null());
        let opts = ReportOpts {
            period: crate::dates::Period::PeriodBetween(day(2008, 1, 1), day(2008, 1, 2)),
            balance_accum: BalanceAccumulation::Historical,
            ..ReportOpts::default()
        };
        let r = multi_balance_report(&spec(opts, Query::Any), &sample_journal()).unwrap();
        let rows: Vec<_> = (r.rows.iter())
            .map(|r| {
                (
                    r.name.clone(),
                    r.amounts.clone(),
                    r.total.clone(),
                    r.average.clone(),
                )
            })
            .collect();
        let row = |a: &str, n: i64| (DisplayName::flat(a), vec![usds(n)], usds(n), usds(n));
        assert_eq!(
            rows,
            [row("assets:bank:checking", 1), row("income:salary", -1)]
        );
        assert_eq!(r.totals.total, usds(0));
        assert_eq!(r.dates.len(), 1);
    }

    #[test]
    fn percentages() {
        let pc = |a: i64, b: i64| perdivide(&usds(a), &usds(b)).unwrap();
        assert_eq!(
            pc(1, 4),
            MixedAmount::from_amount(per(Decimal::from_i64(25)))
        );
        assert_eq!(pc(1, 0), MixedAmount::from_amount(per(Decimal::ZERO)));
        let eur = MixedAmount::from_amount(crate::amount::eur(Decimal::from_i64(1)));
        assert!(perdivide(&usds(1), &eur).is_err());
        assert!(perdivide(&(&usds(1) + &eur), &usds(1)).is_err());
    }
    // prrAdd: a period only one row has keeps its value (zipWithPadded).
    #[test]
    fn rows_add_padded() {
        let row = |ns: &[i64]| PeriodicReportRow {
            name: (),
            amounts: ns.iter().map(|n| usds(*n)).collect(),
            total: usds(ns.iter().sum()),
            average: usds(0),
        };
        assert_eq!(row(&[1, 2]).plus(&row(&[10, 20, 30])), row(&[11, 22, 30]));
    }

    // compoundBalanceReport: a normally negative section's amounts
    // negated, and the net totals subtracting a section that decreases
    // them, unless counting.
    #[test]
    fn compound_reports() {
        let j = read("2024-01-01\n    assets  10\n    liabilities  -4\n    equity  -6\n");
        let sections = [
            SubreportSpec {
                title: "Assets",
                account_type: AccountType::Asset,
                normal_sign: NormalSign::NormallyPositive,
                increases_total: true,
            },
            SubreportSpec {
                title: "Liabilities",
                account_type: AccountType::Liability,
                normal_sign: NormalSign::NormallyNegative,
                increases_total: false,
            },
        ];
        let n = |n| MixedAmount::from_amount(crate::amount::num(Decimal::from_i64(n)));
        let totals = |balance_calc| {
            let opts = ReportOpts {
                balance_calc,
                ..ReportOpts::default()
            };
            let r = compound_balance_report(&spec(opts, Query::Any), &j, &sections).unwrap();
            let sections: Vec<MixedAmount> = (r.subreports.iter())
                .map(|(_, s, _)| s.totals.total.clone())
                .collect();
            (sections, r.totals.total)
        };
        assert_eq!(
            totals(BalanceCalculation::Change),
            (vec![n(10), n(4)], n(6))
        );
        assert_eq!(
            totals(BalanceCalculation::PostingsCount),
            (vec![n(1), n(1)], n(2))
        );
    }
}
