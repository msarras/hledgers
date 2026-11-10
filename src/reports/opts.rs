//! Report options (Hledger.Reports.ReportOptions): what the standard
//! command-line options and query arguments ask a report to show, and the
//! query they make.

use std::collections::BTreeSet;

use crate::account::DepthSpec;
use crate::amount::{MixedAmount, Sym};
use crate::console::usage_error;
use crate::dates::{
    DateSpan, Day, DayPartition, EfDay, Interval, Period, fix_smart_date_str_ef, parse_period_expr,
    parsedate, prevday, split_span,
};
use crate::model::{Journal, Posting, PostingRef, Status, Transaction};
use crate::query::{Query, QueryOpt, parse_depth_spec, parse_query_list};
use crate::reports::string_format::{StringFormat, parse_string_format};
use crate::text::strip_quotes;
use crate::valuation::{
    ConversionOp, PriceOracle, ValuationType, journal_to_cost, mixed_amount_apply_gain,
    mixed_amount_apply_valuation, mixed_amount_gain_at_date, mixed_amount_value_at_date,
};

/// What to calculate in each balance report cell (BalanceCalculation).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BalanceCalculation {
    #[default]
    Change,
    Budget,
    ValueChange,
    Gain,
    PostingsCount,
    TransactionsCount,
}

/// How balance report values accumulate across periods
/// (BalanceAccumulation).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BalanceAccumulation {
    #[default]
    PerPeriod,
    Cumulative,
    Historical,
}

/// Show accounts as a flat list or a tree (AccountListMode).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AccountListMode {
    #[default]
    Flat,
    Tree,
}

/// How multi-commodity balance reports are laid out (Layout).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Wide(Option<usize>),
    Tall,
    Bare,
    BareWide,
    Tidy,
}

/// How period column headings are shown (PeriodTitles).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PeriodTitles {
    #[default]
    Compact,
    Dates,
}

/// Whether accounts normally have positive or negative balances
/// (NormalSign).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NormalSign {
    NormallyPositive,
    NormallyNegative,
}

/// A report sort key, and whether it is reversed (SortField).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortField {
    AbsAmount(bool),
    Account(bool),
    Amount(bool),
    Date(bool),
    Description(bool),
}

/// Parsed options as (name, value) pairs in command-line order; positional
/// arguments have the name `args`. Where a flag repeats, the rightmost wins.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RawOpts(pub Vec<(String, String)>);

impl RawOpts {
    pub fn bool(&self, name: &str) -> bool {
        self.0.iter().any(|(n, _)| n == name)
    }

    pub fn string(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn args(&self) -> impl Iterator<Item = &str> {
        self.list("args")
    }

    /// The values of every option with this name, in order
    /// (listofstringopt).
    pub fn list<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> {
        self.0
            .iter()
            .filter(move |(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// Whether the option appears an odd number of times (toggleopt).
    pub fn toggle(&self, name: &str) -> bool {
        self.list(name).count() % 2 == 1
    }

    /// What `f` makes of the last option name it accepts (choiceopt).
    pub fn choice<T>(&self, f: impl Fn(&str) -> Option<T>) -> Option<T> {
        self.0.iter().filter_map(|(n, _)| f(n)).next_back()
    }

    /// The option's value as an integer from 0 to the largest Int
    /// (maybeposintopt).
    pub fn posint(&self, name: &str) -> Result<Option<usize>, String> {
        let Some(v) = self.string(name) else {
            return Ok(None);
        };
        let usage = crate::console::usage_error;
        let n: i128 = v
            .trim()
            .parse()
            .map_err(|_| usage(&format!("could not parse {name} number: {v}")))?;
        if !(0..=i128::from(i64::MAX)).contains(&n) {
            return Err(usage(&format!(
                "argument to {name} must lie in the range 0 to {}, but is {n}",
                i64::MAX
            )));
        }
        Ok(Some(n as usize))
    }
}

/// Options for customising reports (ReportOpts). Some options' errors
/// surface only when a report uses them, as upstream's lazy fields do:
/// those fields hold a Result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportOpts {
    pub period: Period,
    pub interval: Interval,
    /// Zero, one or two statuses to match.
    pub statuses: Vec<Status>,
    pub conversion_op: Option<ConversionOp>,
    pub value: Option<ValuationType>,
    /// Infer market prices from transactions (`--infer-market-prices`).
    pub infer_prices: bool,
    pub depth: DepthSpec,
    /// A folding UI's default fold depth (hledger-web).
    pub fold_depth: Option<usize>,
    pub no_elide: bool,
    pub date2: bool,
    pub empty: bool,
    pub full_names: bool,
    pub real: bool,
    /// The line format of `--format`, or the balance report's default.
    pub format: StringFormat,
    pub balance_base_url: Option<String>,
    pub pretty: bool,
    /// The query arguments.
    pub query_string: Vec<String>,
    pub average: bool,
    pub related: bool,
    pub sort_spec: Vec<SortField>,
    pub txn_dates: bool,
    pub balance_calc: BalanceCalculation,
    pub balance_accum: BalanceAccumulation,
    /// A description substring selecting periodic transactions for budgets.
    pub budget_pattern: Option<String>,
    pub account_list_mode: AccountListMode,
    pub drop: usize,
    pub declared: bool,
    pub row_total: bool,
    pub no_total: bool,
    pub summary_only: bool,
    pub show_costs: bool,
    pub sort_amount: bool,
    pub percent: bool,
    pub invert: bool,
    pub normal_balance: Option<NormalSign>,
    pub color: bool,
    pub transpose: bool,
    pub layout: Result<Layout, String>,
    pub period_titles: PeriodTitles,
    /// An explicit `--title`, perhaps empty to suppress the report's own.
    pub title: Option<String>,
    pub subreport_titles: Option<String>,
    /// Whether the output shows every value of the report, as json does,
    /// so that --percent calculates every percentage, not just those the
    /// other formats show (upstream calculates each when it is shown).
    pub all_values_shown: bool,
    /// The language of the report's text (--lang).
    pub translations: std::sync::Arc<crate::i18n::Translations>,
    /// How much debug output to show (`--debug`; debugLevel, a global
    /// upstream).
    pub debug: usize,
}

impl Default for ReportOpts {
    /// defreportopts.
    fn default() -> ReportOpts {
        ReportOpts {
            period: Period::PeriodAll,
            interval: Interval::NoInterval,
            statuses: Vec::new(),
            conversion_op: None,
            value: None,
            infer_prices: false,
            depth: DepthSpec::default(),
            fold_depth: None,
            no_elide: false,
            date2: false,
            empty: false,
            full_names: false,
            real: false,
            format: StringFormat::default(),
            balance_base_url: None,
            pretty: false,
            query_string: Vec::new(),
            average: false,
            related: false,
            sort_spec: vec![SortField::Date(false)],
            txn_dates: false,
            balance_calc: BalanceCalculation::Change,
            balance_accum: BalanceAccumulation::PerPeriod,
            budget_pattern: None,
            account_list_mode: AccountListMode::Flat,
            drop: 0,
            declared: false,
            row_total: false,
            no_total: false,
            summary_only: false,
            show_costs: false,
            sort_amount: false,
            percent: false,
            invert: false,
            normal_balance: None,
            color: false,
            transpose: false,
            layout: Ok(Layout::Wide(None)),
            period_titles: PeriodTitles::Compact,
            title: None,
            subreport_titles: None,
            all_values_shown: false,
            translations: std::sync::Arc::new(crate::i18n::Translations::none()),
            debug: 0,
        }
    }
}

impl ReportOpts {
    /// Report options from the parsed command line, today's date, and
    /// whether stdout gets colour (rawOptsToReportOpts).
    pub fn from_raw_opts(today: Day, color: bool, raw: &RawOpts) -> Result<ReportOpts, String> {
        let format = match raw.string("format") {
            None => StringFormat::default(),
            Some(f) => parse_string_format(f)
                .map_err(|e| usage_error(&format!("could not parse format option: {e}")))?,
        };
        let pretty = match raw.string("pretty") {
            None => false,
            Some("always" | "yes" | "y") => true,
            Some("never" | "no" | "n") => false,
            Some(_) => return Err(usage_error("this argument should be one of y, yes, n, no")),
        };
        let balance_calc = balance_calc_opt(raw);
        Ok(ReportOpts {
            period: period_from_raw_opts(today, raw)?,
            interval: interval_from_raw_opts(raw)?,
            statuses: statuses_from_raw_opts(raw),
            conversion_op: conversion_op_from_raw_opts(raw, balance_calc)?,
            value: valuation_type_from_raw_opts(raw, balance_calc)?,
            infer_prices: raw.bool("infer-market-prices"),
            depth: depth_from_raw_opts(raw)?,
            no_elide: raw.bool("no-elide"),
            date2: raw.bool("date2"),
            empty: raw.bool("empty"),
            full_names: raw.bool("full-names"),
            real: raw.bool("real"),
            format,
            balance_base_url: raw.string("base-url").map(str::to_string),
            query_string: raw.args().map(str::to_string).collect(),
            average: raw.bool("average"),
            related: raw.bool("related"),
            sort_spec: sort_spec_from_raw_opts(raw)?,
            txn_dates: raw.bool("txn-dates"),
            balance_calc,
            balance_accum: balance_accumulation_override(raw, balance_calc)
                .unwrap_or(BalanceAccumulation::PerPeriod),
            budget_pattern: raw.string("budget").map(str::to_string),
            account_list_mode: raw
                .choice(|n| match n {
                    "tree" => Some(AccountListMode::Tree),
                    "flat" => Some(AccountListMode::Flat),
                    _ => None,
                })
                .unwrap_or(AccountListMode::Flat),
            drop: raw.posint("drop")?.unwrap_or(0),
            declared: raw.bool("declared"),
            row_total: raw.bool("row-total"),
            no_total: raw.bool("no-total"),
            summary_only: raw.bool("summary-only"),
            show_costs: raw.bool("show-costs"),
            sort_amount: raw.bool("sort-amount"),
            percent: raw.bool("percent"),
            invert: raw.bool("invert"),
            pretty,
            color,
            transpose: raw.toggle("transpose"),
            layout: layout_opt(raw),
            period_titles: match raw.string("period-titles") {
                None | Some("compact") => PeriodTitles::Compact,
                Some("dates") => PeriodTitles::Dates,
                Some(s) => {
                    return Err(usage_error(&format!(
                        "--period-titles's argument should be \"compact\" or \"dates\", not {}",
                        crate::text::show_string(s)
                    )));
                }
            },
            title: raw.string("title").map(|t| t.replace("\\n", "\n")),
            subreport_titles: raw
                .string("subreport-titles")
                .map(|t| t.replace("\\n", "\n")),
            debug: raw.posint("debug")?.unwrap_or(0),
            ..ReportOpts::default()
        })
    }

    /// Transactions' dates for this report: secondary with `--date2`
    /// (transactionDateFn).
    pub fn transaction_date(&self, t: &Transaction) -> Day {
        t.date_of(self.date2)
    }

    /// Postings' dates for this report (postingDateFn).
    pub fn posting_date(&self, p: &Posting, t: Option<&Transaction>) -> Day {
        if self.date2 {
            p.date2_in(t)
        } else {
            p.date_in(t)
        }
    }

    /// The query the date, status, depth and real flags make
    /// (queryFromFlags).
    pub fn query_from_flags(&self) -> Query {
        let mut qs = Vec::new();
        if self.real {
            qs.push(Query::Real(true));
        }
        if let Some(d) = self.depth.flat {
            qs.push(Query::Depth(d));
        }
        for (r, d) in &self.depth.regexps {
            qs.push(Query::DepthAcct(r.clone(), *d));
        }
        let span = self.period.as_date_span();
        qs.push(if self.date2 {
            Query::Date2(span)
        } else {
            Query::Date(span)
        });
        qs.push(Query::Or(
            self.statuses.iter().map(|s| Query::StatusQ(*s)).collect(),
        ));
        Query::And(qs).simplify()
    }

    /// The report's title: `--title`'s, or the given default
    /// (effectiveTitle).
    pub fn effective_title<'a>(&'a self, default: &'a str) -> &'a str {
        self.title.as_deref().unwrap_or(default)
    }

    /// Whether a multi-period report has a row total column: with
    /// --row-total, unless it shows accumulated balances
    /// (multiBalanceHasTotalsColumn).
    pub fn has_totals_column(&self) -> bool {
        self.row_total && self.balance_accum == BalanceAccumulation::PerPeriod
    }

    /// Whether the report needs the postings before its start: it shows
    /// historical balances, or calculates value changes or gains
    /// (requiresHistorical).
    pub fn requires_historical(&self) -> bool {
        self.balance_accum == BalanceAccumulation::Historical
            || matches!(
                self.balance_calc,
                BalanceCalculation::ValueChange | BalanceCalculation::Gain
            )
    }

    /// For an end value report that needs the postings before its start,
    /// the commodity to value sums in after summing, None meaning each
    /// one's default valuation commodity; otherwise None, valuation
    /// happening to postings before summing (valuationAfterSum).
    pub fn valuation_after_sum(&self) -> Option<Option<&Sym>> {
        match &self.value {
            Some(ValuationType::AtEnd(c)) if self.requires_historical() => Some(c.as_ref()),
            _ => None,
        }
    }
}

/// How sums are valued on a date in a report that values after summing:
/// their value, or with --gain their gain, in the commodity
/// [`valuation_after_sum`](ReportOpts::valuation_after_sum) gives; None if
/// the report does not value after summing
/// (mixedAmountApplyValuationAfterSumFromOptsWith, which is then `id`).
pub fn mixed_amount_apply_valuation_after_sum<'a>(
    ropts: &'a ReportOpts,
    j: &Journal,
    oracle: &'a PriceOracle,
) -> Option<impl Fn(Day, &MixedAmount) -> Result<MixedAmount, String> + 'a> {
    let to = ropts.valuation_after_sum()?;
    let styles = j.commodity_styles();
    let gain = ropts.balance_calc == BalanceCalculation::Gain;
    Some(move |d: Day, m: &MixedAmount| {
        if gain {
            mixed_amount_gain_at_date(oracle, &styles, to, d, m)
        } else {
            mixed_amount_value_at_date(oracle, &styles, to, d, m)
        }
    })
}

/// Report options with today's date, their query, and the query's options
/// (ReportSpec).
#[derive(Clone, Debug)]
pub struct ReportSpec {
    pub opts: ReportOpts,
    pub today: Day,
    pub query: Query,
    pub query_opts: Vec<QueryOpt>,
}

impl ReportSpec {
    /// The report spec of these options: the query arguments parsed and
    /// combined with the flags' query, an interval given by a query
    /// argument replacing the flags' (reportOptsToSpec).
    pub fn new(today: Day, mut opts: ReportOpts) -> Result<ReportSpec, String> {
        let (args_query, query_opts) = parse_query_list(today, &opts.query_string)?;
        if let Some(i) = query_opts.iter().rev().find_map(|o| match o {
            QueryOpt::Interval(i) => Some(i.clone()),
            _ => None,
        }) {
            opts.interval = i;
        }
        let query = Query::And(vec![opts.query_from_flags(), args_query]).simplify();
        Ok(ReportSpec {
            opts,
            today,
            query,
            query_opts,
        })
    }

    /// With the conversion operation set, unless it is already
    /// (setDefaultConversionOp).
    pub fn with_default_conversion_op(mut self, op: ConversionOp) -> ReportSpec {
        self.opts.conversion_op.get_or_insert(op);
        self
    }

    /// The report's span: the requested one, its open ends filled with the
    /// journal's first and last dates (for an end value report, the last
    /// being at least the day after the latest P directive not after
    /// today), enlarged to whole report intervals; and the report periods,
    /// if any (reportSpan).
    pub fn report_span(&self, j: &Journal) -> Result<(DateSpan, Option<DayPartition>), String> {
        self.report_span_helper(false, j)
    }

    /// Like [`report_span`](ReportSpec::report_span), with both primary
    /// and secondary dates (reportSpanBothDates).
    pub fn report_span_both_dates(
        &self,
        j: &Journal,
    ) -> Result<(DateSpan, Option<DayPartition>), String> {
        self.report_span_helper(true, j)
    }

    fn report_span_helper(
        &self,
        both_dates: bool,
        j: &Journal,
    ) -> Result<(DateSpan, Option<DayPartition>), String> {
        let (span, adjust) = self.report_span_and_adjust(both_dates, j);
        let periods = split_span(adjust, &self.opts.interval, span)?;
        let enlarged = periods.as_ref().map_or(DateSpan::default(), |p| {
            let (s, e) = p.start_end();
            DateSpan::exact(Some(s), Some(e.add_days(1)))
        });
        Ok((enlarged, periods))
    }

    /// The requested span with its open ends filled from the journal, and
    /// whether report periods should start at an interval boundary (when no
    /// start date was requested) (reportSpanAndAdjust).
    fn report_span_and_adjust(&self, both_dates: bool, j: &Journal) -> (DateSpan, bool) {
        let ropts = &self.opts;
        let requested = if both_dates {
            self.query.date_span_either()
        } else {
            self.query.date_span(ropts.date2)
        };
        let DateSpan(first, last) = if both_dates {
            j.date_span_both_dates()
        } else {
            j.date_span(ropts.date2)
        };
        let last = match ropts.value {
            Some(ValuationType::AtEnd(_)) => {
                let latest_price = j
                    .price_directives
                    .iter()
                    .map(|p| p.date)
                    .filter(|d| *d <= self.today)
                    .max()
                    .map(|d| EfDay::exact(d.add_days(1)));
                std::cmp::max_by_key(last, latest_price, |e| e.map(|e| e.day))
            }
            _ => last,
        };
        (
            requested.valid_defaults_from(DateSpan(first, last)),
            requested.start().is_none(),
        )
    }

    /// Whether filtering and valuing the journal as this spec says only
    /// keeps the postings the query matches, as they are: without
    /// --related, nothing converted or valued, and no parts of amounts
    /// selected by `cur:` or `amt:` terms.
    pub fn filters_postings_only(&self) -> bool {
        !self.opts.related
            && self.opts.value.is_none()
            && matches!(
                self.opts.conversion_op,
                None | Some(ConversionOp::NoConversionOp)
            )
            && self.query.clone().filter(Query::is_amt_or_cur).is_null()
    }

    /// The journal with just the postings, and parts of amounts, the query
    /// matches (or with --related, their sibling postings), converted to
    /// cost and/or to value as the options say, using its own market
    /// prices (journalValueAndFilterPostings).
    pub fn journal_value_and_filter_postings(&self, j: &Journal) -> Result<Journal, String> {
        let oracle = PriceOracle::new(self.opts.infer_prices, self.opts.debug, j);
        self.journal_value_and_filter_postings_with(j, &oracle)
    }

    /// Like
    /// [`journal_value_and_filter_postings`](ReportSpec::journal_value_and_filter_postings),
    /// with this price oracle. All filtering happens before valuation
    /// (journalValueAndFilterPostingsWith).
    pub fn journal_value_and_filter_postings_with(
        &self,
        j: &Journal,
        oracle: &PriceOracle,
    ) -> Result<Journal, String> {
        let (q, related) = (&self.query, self.opts.related);
        let types = |a: &str| j.account_type(a);
        // each transaction copied with the postings the query matches
        let txns = crate::par::map(j.txns.len(), &j.txns, |t| {
            if related {
                let mut t = t.clone();
                filter_transaction_related_postings(q, &mut t);
                t
            } else {
                t.with_postings_where(|p| q.matches_posting_extra(&types, PostingRef::new(t, p)))
            }
        });
        let mut filtered = j.with_txns(txns);
        let amtq = q.clone().filter(Query::is_amt_or_cur);
        if !amtq.is_null() {
            for t in &mut filtered.txns {
                t.postings.retain_mut(|p| {
                    p.amount = std::mem::take(&mut p.amount).filter(|a| amtq.matches_amount(a));
                    !p.amount.amounts_raw().is_empty()
                });
            }
        }
        self.journal_apply_valuation_with(filtered, oracle)
    }

    /// The journal with its posting amounts converted to cost and/or to
    /// value as the options say (-B, -V, -X, --value), using its own market
    /// prices (journalApplyValuationFromOpts).
    pub fn journal_apply_valuation(&self, j: Journal) -> Result<Journal, String> {
        let oracle = PriceOracle::new(self.opts.infer_prices, self.opts.debug, &j);
        self.journal_apply_valuation_with(j, &oracle)
    }

    /// Like [`journal_apply_valuation`](ReportSpec::journal_apply_valuation),
    /// with this price oracle. Amounts are converted to cost first, then
    /// valued; with --gain, not converted, and their gain is calculated
    /// instead of their value. An end value is the value on the last day of
    /// the report period, or with a report interval, of the subperiod
    /// containing the posting (journalApplyValuationFromOptsWith).
    pub fn journal_apply_valuation_with(
        &self,
        mut j: Journal,
        oracle: &PriceOracle,
    ) -> Result<Journal, String> {
        let ropts = &self.opts;
        let gain = ropts.balance_calc == BalanceCalculation::Gain;
        let periods = match (&ropts.value, &ropts.interval) {
            (Some(ValuationType::AtEnd(_)), Interval::NoInterval) => self.report_span(&j),
            (Some(ValuationType::AtEnd(_)), _) => self.report_span_both_dates(&j),
            _ => Ok((DateSpan::default(), None)),
        };
        // an error in them is raised when a posting first needs its
        // period, as upstream's lazily computed spans raise it
        let periods = periods.map(|(_, ps)| ps);
        let styles = j.commodity_styles();
        if !gain {
            let op = ropts.conversion_op.unwrap_or(ConversionOp::NoConversionOp);
            journal_to_cost(op, &mut j);
        }
        let Some(v) = &ropts.value else {
            return Ok(j);
        };
        for t in &mut j.txns {
            for i in 0..t.postings.len() {
                let p = &t.postings[i];
                // the period's last day (postingperiodend)
                let period_last = || {
                    let periods = periods.as_ref().map_err(String::clone)?;
                    let d = ropts.posting_date(p, Some(t));
                    let last = match ropts.interval {
                        Interval::NoInterval => periods.as_ref().map(|ps| ps.start_end().1),
                        _ => periods.as_ref().map(|ps| ps.find(d).1),
                    };
                    last.ok_or_else(|| {
                        "journalApplyValuationFromOpts: expected all spans to have an end date"
                            .to_string()
                    })
                };
                let (today, date) = (self.today, p.date_in(Some(t)));
                let amount = if gain {
                    mixed_amount_apply_gain(
                        oracle,
                        &styles,
                        period_last,
                        today,
                        date,
                        v,
                        &p.amount,
                    )?
                } else {
                    mixed_amount_apply_valuation(
                        oracle,
                        &styles,
                        period_last,
                        today,
                        date,
                        v,
                        &p.amount,
                    )?
                };
                t.postings[i].amount = amount;
            }
        }
        Ok(j)
    }
}

/// The transaction with just the postings that the query does not match,
/// if it matches any, otherwise none (filterTransactionRelatedPostings,
/// whose list difference removes one equal posting per match).
fn filter_transaction_related_postings(q: &Query, t: &mut Transaction) {
    let matched: Vec<usize> = (0..t.postings.len())
        .filter(|&i| q.matches_posting(PostingRef::new(t, &t.postings[i])))
        .collect();
    if matched.is_empty() {
        t.postings.clear();
        return;
    }
    let mut removed = vec![false; t.postings.len()];
    for m in matched {
        if let Some(i) =
            (0..t.postings.len()).find(|&i| !removed[i] && t.postings[i] == t.postings[m])
        {
            removed[i] = true;
        }
    }
    let mut removed = removed.into_iter();
    t.postings.retain(|_| !removed.next().unwrap_or(false));
}

/// The report period from the begin, end and period options, the last of
/// each kind winning (periodFromRawOpts).
fn period_from_raw_opts(today: Day, raw: &RawOpts) -> Result<Period, String> {
    let begin = begin_dates_from_raw_opts(today, raw)?.last().map(|d| d.day);
    let end = end_dates_from_raw_opts(today, raw)?.last().map(|d| d.day);
    Ok(match (begin, end) {
        (None, None) => Period::PeriodAll,
        (Some(b), None) => Period::PeriodFrom(b),
        (None, Some(e)) => Period::PeriodTo(e),
        (Some(b), Some(e)) => Period::PeriodBetween(b, e).simplify(),
    })
}

/// The span of a period option, or a usage error.
fn period_option_span(today: Day, v: &str) -> Result<(Interval, DateSpan), String> {
    parse_period_expr(today, strip_quotes(v))
        .map_err(|e| usage_error(&format!("could not parse period option: {e}")))
}

/// The start dates from the begin and period options, in order
/// (beginDatesFromRawOpts).
fn begin_dates_from_raw_opts(today: Day, raw: &RawOpts) -> Result<Vec<EfDay>, String> {
    let mut out = Vec::new();
    for (n, v) in &raw.0 {
        match n.as_str() {
            "begin" => out.push(
                fix_smart_date_str_ef(today, v)
                    .map_err(|e| usage_error(&format!("could not parse begin date: {e}")))?,
            ),
            "period" => out.extend(period_option_span(today, v)?.1.0),
            _ => {}
        }
    }
    Ok(out)
}

/// The end dates from the end and period options, in order
/// (endDatesFromRawOpts).
fn end_dates_from_raw_opts(today: Day, raw: &RawOpts) -> Result<Vec<EfDay>, String> {
    let mut out = Vec::new();
    for (n, v) in &raw.0 {
        match n.as_str() {
            "end" => out.push(
                fix_smart_date_str_ef(today, v)
                    .map_err(|e| usage_error(&format!("could not parse end date: {e}")))?,
            ),
            "period" => out.extend(period_option_span(today, v)?.1.1),
            _ => {}
        }
    }
    Ok(out)
}

/// The report interval from the period and interval flags, the last
/// winning (intervalFromRawOpts).
fn interval_from_raw_opts(raw: &RawOpts) -> Result<Interval, String> {
    let mut interval = Interval::NoInterval;
    for (n, v) in &raw.0 {
        let i = match n.as_str() {
            // the interval does not depend on the reference date
            "period" => match period_option_span(crate::dates::null_date(), v)?.0 {
                Interval::NoInterval => None,
                i => Some(i),
            },
            "daily" => Some(Interval::Days(1)),
            "weekly" => Some(Interval::Weeks(1)),
            "monthly" => Some(Interval::Months(1)),
            "quarterly" => Some(Interval::Quarters(1)),
            "yearly" => Some(Interval::Years(1)),
            _ => None,
        };
        if let Some(i) = i {
            interval = i;
        }
    }
    Ok(interval)
}

/// The statuses selected by the status flags; all three select none
/// (statusesFromRawOpts).
fn statuses_from_raw_opts(raw: &RawOpts) -> Vec<Status> {
    let mut ss: Vec<Status> = raw
        .0
        .iter()
        .filter_map(|(n, _)| match n.as_str() {
            "unmarked" => Some(Status::Unmarked),
            "pending" => Some(Status::Pending),
            "cleared" => Some(Status::Cleared),
            _ => None,
        })
        .collect();
    ss.sort();
    ss.dedup();
    if ss.len() >= 3 { Vec::new() } else { ss }
}

/// The balance calculation selected (balancecalcopt).
fn balance_calc_opt(raw: &RawOpts) -> BalanceCalculation {
    raw.choice(|n| match n {
        "sum" => Some(BalanceCalculation::Change),
        "valuechange" => Some(BalanceCalculation::ValueChange),
        "gain" => Some(BalanceCalculation::Gain),
        "budget" => Some(BalanceCalculation::Budget),
        "count" => Some(BalanceCalculation::PostingsCount),
        _ => None,
    })
    .unwrap_or_default()
}

/// The balance accumulation selected, or implied by --valuechange
/// (balanceAccumulationOverride).
pub fn balance_accumulation_override(
    raw: &RawOpts,
    calc: BalanceCalculation,
) -> Option<BalanceAccumulation> {
    raw.choice(|n| match n {
        "historical" => Some(BalanceAccumulation::Historical),
        "cumulative" => Some(BalanceAccumulation::Cumulative),
        "change" => Some(BalanceAccumulation::PerPeriod),
        _ => None,
    })
    .or((calc == BalanceCalculation::ValueChange).then_some(BalanceAccumulation::PerPeriod))
}

/// The valuation selected by -V, -X and --value, or implied by --valuechange
/// or --gain (valuationTypeFromRawOpts).
fn valuation_type_from_raw_opts(
    raw: &RawOpts,
    calc: BalanceCalculation,
) -> Result<Option<ValuationType>, String> {
    let commodity = |s: &str| -> Sym {
        Sym::from(
            s.strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .unwrap_or(s),
        )
    };
    let mut direct = None;
    for (n, v) in &raw.0 {
        let vt = match n.as_str() {
            "V" => Some(ValuationType::AtEnd(None)),
            "X" => Some(ValuationType::AtEnd(Some(commodity(v)))),
            "value" => {
                let (t, c) = match v.split_once(',') {
                    Some((t, c)) => (t, Some(commodity(c))),
                    None => (v.as_str(), None),
                };
                Some(match t {
                    "cost" | "c" | "transacted" => match c {
                        Some(c) => ValuationType::AtEnd(Some(c)),
                        None => continue,
                    },
                    "then" | "t" => ValuationType::AtThen(c),
                    "end" | "e" => ValuationType::AtEnd(c),
                    "now" | "n" => ValuationType::AtNow(c),
                    _ => match parsedate(t) {
                        Some(d) => ValuationType::AtDate(d, c),
                        None => {
                            return Err(usage_error(&format!(
                                "could not parse \"{t}\" as valuation type, should be: cost|transacted|then|end|now|c|t|e|n|YYYY-MM-DD"
                            )));
                        }
                    },
                })
            }
            _ => None,
        };
        if vt.is_some() {
            direct = vt;
        }
    }
    Ok(match (calc, direct) {
        (BalanceCalculation::ValueChange, None) => Some(ValuationType::AtEnd(None)),
        (BalanceCalculation::ValueChange, d @ Some(ValuationType::AtEnd(_))) => d,
        (BalanceCalculation::ValueChange, _) => {
            return Err(usage_error(
                "--valuechange only produces sensible results with --value=end",
            ));
        }
        (BalanceCalculation::Gain, None) => Some(ValuationType::AtEnd(None)),
        (_, d) => d,
    })
}

/// The conversion selected by -B or --value=cost/transacted
/// (conversionOpFromRawOpts).
fn conversion_op_from_raw_opts(
    raw: &RawOpts,
    calc: BalanceCalculation,
) -> Result<Option<ConversionOp>, String> {
    let op = raw
        .0
        .iter()
        .filter_map(|(n, v)| {
            let kind = v.split(',').next().unwrap_or("");
            match n.as_str() {
                "B" => Some(ConversionOp::ToCost),
                "value" if kind == "cost" || kind == "c" => Some(ConversionOp::ToCost),
                "value" if kind == "transacted" => Some(ConversionOp::ToTransactedCost),
                _ => None,
            }
        })
        .next_back();
    if op.is_some() && calc == BalanceCalculation::Gain {
        return Err(usage_error("--gain cannot be combined with --cost"));
    }
    Ok(op)
}

/// The depth limits of the --depth options: the last overall one, and all
/// the per-account ones (depthFromRawOpts).
fn depth_from_raw_opts(raw: &RawOpts) -> Result<DepthSpec, String> {
    let mut flat = None;
    let mut regexps = DepthSpec::default();
    for d in raw.list("depth") {
        let spec = parse_depth_spec(d).map_err(|e| usage_error(&e))?;
        if spec.flat.is_some() && spec.regexps.is_empty() {
            flat = Some(spec);
        } else {
            regexps = regexps.combine(spec);
        }
    }
    Ok(flat.unwrap_or_default().combine(regexps))
}

/// The sort keys of --sort, or by date (getSortSpec).
fn sort_spec_from_raw_opts(raw: &RawOpts) -> Result<Vec<SortField>, String> {
    let Some(s) = raw.string("sort") else {
        return Ok(vec![SortField::Date(false)]);
    };
    s.split(',')
        .map(|t| {
            let t = crate::text::strip(t);
            let (negated, key) = match t.strip_prefix('-') {
                Some(k) => (true, k),
                None => (false, t),
            };
            Ok(match key {
                "date" => SortField::Date(negated),
                "desc" | "description" => SortField::Description(negated),
                "account" => SortField::Account(negated),
                "amount" => SortField::Amount(negated),
                "absamount" => SortField::AbsAmount(negated),
                _ => {
                    return Err(format!(
                        "unknown --sort key {t}. Supported keys are: date, desc, account, amount, absamount."
                    ));
                }
            })
        })
        .collect()
}

/// The balance report layout of --layout or --commodity-column (layoutopt).
fn layout_opt(raw: &RawOpts) -> Result<Layout, String> {
    if let Some(opt) = raw.string("layout") {
        let lower = opt.to_lowercase();
        let (s, n) = lower.split_at(lower.find(',').unwrap_or(lower.len()));
        let width = match n.get(1..).unwrap_or("") {
            "" => None,
            w => Some(
                w.parse::<usize>()
                    .map_err(|_| usage_error("width in --layout=wide,WIDTH must be an integer"))?,
            ),
        };
        let names = [
            ("wide", Layout::Wide(width)),
            ("tall", Layout::Tall),
            ("bare", Layout::Bare),
            ("tidy", Layout::Tidy),
            ("barewide", Layout::BareWide),
        ];
        return names
            .into_iter()
            .find(|(name, _)| !s.is_empty() && name.starts_with(s))
            .map(|(_, l)| l)
            .ok_or_else(|| {
                usage_error(
                    "--layout's argument should be \"wide[,WIDTH]\", \"tall\", \"bare\", \"barewide\", or \"tidy\"",
                )
            });
    }
    Ok(if raw.bool("commodity-column") {
        Layout::Bare
    } else {
        Layout::Wide(None)
    })
}

/// A report period's column heading: for changes, its compact name (with
/// the month abbreviated unless the report spans several years) or with
/// --period-titles=dates its dates; for accumulated balances, its last
/// date (reportPeriodName).
pub fn report_period_name(ropts: &ReportOpts, spans: &[DateSpan]) -> impl Fn(&DateSpan) -> String {
    // whether the spans start in more than one year, found once for them all
    let multiyear = spans
        .iter()
        .map(DateSpan::start_year)
        .collect::<BTreeSet<_>>()
        .len()
        > 1;
    let months = ropts.translations.month_names();
    let (accum, titles) = (ropts.balance_accum, ropts.period_titles);
    move |span: &DateSpan| match accum {
        BalanceAccumulation::PerPeriod => match titles {
            PeriodTitles::Dates => span.show_full(),
            PeriodTitles::Compact if multiyear => span.to_string(),
            PeriodTitles::Compact => {
                let abbrevs: Vec<&str> = months.iter().map(|(_, a)| a.as_str()).collect();
                span.show_abbrev_with(&abbrevs)
            }
        },
        _ => span
            .end()
            .map_or_else(String::new, |e| prevday(e).to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(pairs: &[(&str, &str)]) -> RawOpts {
        RawOpts(
            pairs
                .iter()
                .map(|(n, v)| (n.to_string(), v.to_string()))
                .collect(),
        )
    }

    fn day(y: i64, m: u32, d: u32) -> Day {
        Day::from_ymd(y, m, d).unwrap()
    }

    #[test]
    fn options_from_the_command_line() {
        let today = day(2024, 6, 15);
        let opts = |pairs: &[(&str, &str)]| ReportOpts::from_raw_opts(today, false, &raw(pairs));
        let o = opts(&[("begin", "2024"), ("end", "2024-03"), ("monthly", "")]).unwrap();
        assert_eq!(
            o.period,
            Period::PeriodBetween(day(2024, 1, 1), day(2024, 3, 1)).simplify()
        );
        assert_eq!(o.interval, Interval::Months(1));
        let o = opts(&[
            ("period", "weekly from 2024-01-01"),
            ("begin", "2024-02-01"),
        ])
        .unwrap();
        assert_eq!(
            (o.period.start(), o.interval),
            (Some(day(2024, 2, 1)), Interval::Weeks(1))
        );
        let o = opts(&[("cleared", ""), ("pending", ""), ("cleared", "")]).unwrap();
        assert_eq!(o.statuses, [Status::Pending, Status::Cleared]);
        let o = opts(&[("cleared", ""), ("pending", ""), ("unmarked", "")]).unwrap();
        assert!(o.statuses.is_empty());
        let o = opts(&[("B", ""), ("V", "")]).unwrap();
        assert_eq!(
            (o.conversion_op, o.value),
            (Some(ConversionOp::ToCost), Some(ValuationType::AtEnd(None)))
        );
        let o = opts(&[("depth", "3"), ("depth", "1")]).unwrap();
        assert_eq!(o.depth.flat, Some(1));
        assert!(
            opts(&[("value", "x")])
                .unwrap_err()
                .ends_with("(use -h to see usage)")
        );
        // a bad --layout is reported only by the reports that use it
        assert!(opts(&[("layout", "20")]).unwrap().layout.is_err());
        let spec = ReportSpec::new(
            today,
            opts(&[("args", "a"), ("args", "date:2024"), ("real", "")]).unwrap(),
        )
        .unwrap();
        assert!(spec.query.is_acct() || matches!(spec.query, Query::And(_)));
    }
}
