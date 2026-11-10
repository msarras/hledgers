//! The roi command (Hledger.Cli.Commands.Roi): an investment's internal
//! rate of return (IRR) and time-weighted rate of return (TWR) in each
//! report period, from its value at the start and end of the period and
//! the money that went in and out of it.

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};

use crate::amount::render::{AmountFormat, show_amount, show_mixed_amount_b};
use crate::amount::{Amount, CommodityStyles, MixedAmount, Rounding, Sym, styles_with_rounding};
use crate::cli::args::{Arity, Flag, Mode};
use crate::cli::io::{warn_if_large_multi_period_report, with_title, write_output};
use crate::cli::opts::CliOpts;
use crate::console::usage_error;
use crate::dates::{Day, Period};
use crate::decimal::Decimal;
use crate::model::{Journal, Posting, PostingRef, Transaction};
use crate::query::{Query, parse_query};
use crate::reports::opts::ReportOpts;
use crate::reports::postings::sum_postings;
use crate::text::{quote_if_needed, show_ffloat, show_string};
use crate::valuation::{
    ConversionOp, PriceOracle, amount_price_directive_from_cost, mixed_amount_apply_valuation,
};
use crate::write::table::{Align, Cell, Header, Line, Table, TableOpts, render_table};

const NONE: Arity = Arity::None;
const REQ: Arity = Arity::Req;

pub static MODE: Mode = Mode {
    name: "roi",
    aliases: &[],
    flags: &[
        Flag::new(
            &["cashflow"],
            NONE,
            "",
            "show all amounts that were used to compute returns",
        ),
        Flag::new(
            &["investment"],
            REQ,
            "QUERY",
            "query to select your investment transactions",
        ),
        Flag::new(
            &["profit-loss", "pnl"],
            REQ,
            "QUERY",
            "query to select profit-and-loss or appreciation/valuation transactions (optional)",
        )
        .sets(&["pnl"]),
    ],
    doc: crate::command_doc!("Roi"),
    ..Mode::DEFAULT
};

/// One report period's figures (OneSpan): its start and exclusive end,
/// the investment's value at the start of its first day and the end of
/// its last, its deposits and withdrawals, and its profit and loss.
struct OneSpan {
    begin: Day,
    end: Day,
    value_before: MixedAmount,
    value_after: MixedAmount,
    cash_flow: Vec<(Day, MixedAmount)>,
}

/// One cash flow of the investment, with what it is computed from, which
/// --cashflow shows (InvestmentFlow): the cost of the transaction's
/// investment postings on that date, the transaction's profit and loss
/// (gains positive), and the flow (deposits negative, withdrawals
/// positive).
struct InvestmentFlow {
    date: Day,
    description: String,
    cost: MixedAmount,
    pnl: MixedAmount,
    flow: MixedAmount,
}

/// What valuing amounts of the investment in a period needs: the period's
/// last day, and the commodity its value should end up in, if known.
struct SpanValuation {
    last_day: Day,
    expected: Option<Sym>,
}

/// A calculation's details, which --cashflow shows, and its result, or why
/// there is none.
type Calculated<T> = (String, Result<T, String>);

/// The investment transactions and what is calculated from them once.
struct Roi<'a> {
    ropts: &'a ReportOpts,
    today: Day,
    oracle: PriceOracle,
    styles: CommodityStyles,
    investment: Query,
    pnl: Query,
    trans: Vec<&'a Transaction>,
    /// Each day's changes to the investment's holdings.
    changes: BTreeMap<Day, MixedAmount>,
    /// The investment's holdings at the end of each day that changes them,
    /// each commodity's added up without costs or cost bases.
    holdings: BTreeMap<Day, MixedAmount>,
    /// The same with their costs and cost bases, made if needed.
    holdings_with_costs: OnceCell<BTreeMap<Day, MixedAmount>>,
    /// The commodities that have costs or cost bases in the holdings.
    costed: BTreeSet<Sym>,
    /// The unit prices of the investment's purchases and sales, by
    /// commodity and date.
    trade_prices: BTreeMap<Sym, BTreeMap<Day, Amount>>,
}

/// Run the roi command (roi): for each report period, and for all of them
/// together if there are several, the investment's value at the start and
/// end, the net deposits, the profit and loss, the IRR and the TWR (for
/// the period, and annualised). The --investment query selects the
/// investment's transactions and postings; --pnl, the postings of its
/// profits and losses (such as revaluations and fees) among them. With
/// --cashflow, also the cash flows and value changes they are computed
/// from.
pub fn roi(opts: &CliOpts, j: &Journal) -> Result<String, String> {
    warn_if_large_multi_period_report(opts, j)?;
    let spec = &opts.spec;
    let ropts = &spec.opts;
    let today = spec.today;
    let flags_query = ReportOpts {
        period: Period::PeriodAll,
        ..ropts.clone()
    }
    .query_from_flags();
    let make_query = |s: &str| -> Result<Query, String> {
        let (q, _) = parse_query(today, s).map_err(|e| usage_error(&e))?;
        Ok(Query::And(vec![flags_query.clone(), q]).simplify())
    };
    let investment = make_query(opts.raw.string("investment").unwrap_or(""))?;
    let pnl = match opts.raw.string("pnl") {
        None => Query::None,
        Some(s) => make_query(s)?,
    };
    if pnl == Query::Any {
        return Err(cant_compute(
            "Need some transactions classed as investment and not pnl, but the pnl query matches any transaction",
        ));
    }
    let trans: Vec<&Transaction> = (j.txns.iter())
        .filter(|t| investment.matches_transaction(t))
        .collect();
    let mut r = Roi {
        ropts,
        today,
        oracle: PriceOracle::new(ropts.infer_prices, ropts.debug, j),
        styles: styles_with_rounding(Rounding::Hard, &j.commodity_styles()),
        investment,
        pnl,
        trans,
        changes: BTreeMap::new(),
        holdings: BTreeMap::new(),
        holdings_with_costs: OnceCell::new(),
        costed: BTreeSet::new(),
        trade_prices: BTreeMap::new(),
    };
    // each day's changes (M.fromListWith maPlus: the later posting on the
    // left), accumulated; and the prices of the investment's trades, the
    // last one of a day winning
    let mut changes: BTreeMap<Day, MixedAmount> = BTreeMap::new();
    let mut trade_prices: BTreeMap<Sym, BTreeMap<Day, Amount>> = BTreeMap::new();
    for &t in &r.trans {
        for p in t.real_postings().filter(|p| r.is_investment(t, p)) {
            let d = r.posting_day(t, p);
            let total = match changes.remove(&d) {
                Some(prev) => &p.amount + &prev,
                None => p.amount.clone(),
            };
            changes.insert(d, total);
            for a in p.amount.amounts() {
                if let Some(pd) = amount_price_directive_from_cost(d, &a) {
                    trade_prices
                        .entry(pd.commodity.clone())
                        .or_default()
                        .insert(d, pd.amount);
                }
            }
        }
    }
    let mut acc = MixedAmount::null();
    for (d, change) in &changes {
        acc = &acc + &change.clone().strip_costs();
        r.holdings.insert(*d, acc.clone());
    }
    r.costed = (changes.values().flat_map(|m| m.amounts_raw()))
        .filter(|a| a.cost.is_some() || a.cost_basis.is_some())
        .map(|a| a.commodity.clone())
        .collect();
    r.changes = changes;
    r.trade_prices = trade_prices;
    if r.trans.is_empty() {
        return Err("No relevant transactions found. Check your investments query".to_string());
    }
    let filtered = j.with_txns(r.trans.iter().map(|t| (*t).clone()).collect());
    let (full_span, periods) = spec.report_span(&filtered)?;
    let undefined = || cant_compute("Undefined start or end of the period");
    let spans: Vec<(Day, Day)> = periods
        .ok_or_else(undefined)?
        .periods()
        .map(|(b, e)| (b, e.add_days(1)))
        .collect();
    let mut details = String::new();
    let mut rows = Vec::new();
    for &(b, e) in &spans {
        let (d, row) = r.process_span(opts, b, e)?;
        details.push_str(&d);
        rows.push(row);
    }
    let single = spans.len() == 1;
    if !single {
        let (Some(b), Some(e)) = (full_span.start(), full_span.end()) else {
            return Err(undefined());
        };
        let (d, row) = r.process_span(opts, b, e)?;
        details.push_str(&d);
        rows.push(row);
    }
    let rows = match rows
        .into_iter()
        .collect::<Result<Vec<Vec<String>>, String>>()
    {
        Ok(rows) => rows,
        Err(e) => {
            // show any --cashflow details before the error, to help troubleshoot it
            if !details.is_empty() {
                write_output(opts, &details)?;
            }
            return Err(e);
        }
    };
    let leaves = |names: &[&str]| -> Vec<Header<String>> {
        names.iter().map(|n| Header::Leaf(n.to_string())).collect()
    };
    let numbers: Vec<Header<String>> = (1..=spans.len())
        .map(|n| Header::Leaf(n.to_string()))
        .collect();
    let row_headers = if single {
        Header::Group(Line::NoLine, numbers)
    } else {
        Header::Group(
            Line::SingleLine,
            vec![
                Header::Group(Line::NoLine, numbers),
                Header::Group(Line::NoLine, leaves(&["Total"])),
            ],
        )
    };
    let col_headers = Header::Group(
        Line::DoubleLine,
        vec![
            Header::Group(Line::SingleLine, leaves(&["Begin", "End"])),
            Header::Group(
                Line::SingleLine,
                leaves(&["Value (begin)", "Cashflow", "Value (end)", "PnL"]),
            ),
            Header::Group(Line::SingleLine, leaves(&["IRR"])),
            Header::Group(Line::SingleLine, leaves(&["TWR/period", "TWR/year"])),
        ],
    );
    let table = Table {
        row_headers,
        col_headers,
        rows,
    };
    Ok(with_title(
        opts,
        details + &render(ropts.pretty, &table) + "\n",
    ))
}

/// A table as Text.Tabular.AsciiWide's render shows it: every cell's text
/// right-aligned, with borders.
fn render(pretty: bool, t: &Table<String, String, String>) -> String {
    let cell = |s: &String| Cell::text(Align::TopRight, s);
    let opts = TableOpts {
        pretty,
        ..TableOpts::default()
    };
    render_table(&opts, cell, cell, cell, t)
}

/// An error about something that prevents calculating the returns.
fn cant_compute(msg: &str) -> String {
    format!("{msg} - will be unable to compute the rates of return")
}

/// An error about amounts in several commodities, with advice
/// (multiCommodityError).
fn multi_commodity_error(msg: &str) -> String {
    format!(
        "{msg}\nConsider using --value to force all costs to be in a single commodity.\n\
         For example, \"--value=end,<commodity> --infer-market-prices\", where commodity is the one that you want to value the investment in."
    )
}

/// The quantity of an amount in one commodity, or an error if it has
/// several (unMix).
fn un_mix(m: &MixedAmount) -> Result<Decimal, String> {
    match m.unify() {
        Some(a) => Ok(a.quantity),
        None => {
            let shown: Vec<String> = (m.amounts().iter())
                .map(|a| show_string(&show_amount(a)))
                .collect();
            Err(multi_commodity_error(&format!(
                "Amounts could not be converted to a single commodity: [{}]",
                shown.join(",")
            )))
        }
    }
}

/// The amount without the commodities whose quantities add up to zero,
/// positions fully closed, which summing can leave as amounts with
/// different costs (dropClosedPositions).
fn drop_closed_positions(m: &MixedAmount) -> MixedAmount {
    let mut sums: BTreeMap<Sym, Decimal> = BTreeMap::new();
    for a in m.amounts() {
        let s = sums.entry(a.commodity.clone()).or_insert(Decimal::ZERO);
        *s = &a.quantity + s;
    }
    let closed: BTreeSet<Sym> = (sums.into_iter())
        .filter(|(_, q)| q.is_zero())
        .map(|(c, _)| c)
        .collect();
    m.clone().filter(|a| !closed.contains(&a.commodity))
}

/// The amount, which should now be in the expected commodity if one is
/// known, or an error saying which commodities had no market price
/// (checkValued).
fn check_valued(
    requested: Option<&Sym>,
    expected: Option<&Sym>,
    date: Day,
    m: MixedAmount,
) -> Result<MixedAmount, String> {
    let Some(c) = expected else {
        return Ok(m);
    };
    let mut unpriced: Vec<Sym> = Vec::new();
    for a in m.amounts() {
        if &a.commodity != c && !a.is_zero() && !unpriced.contains(&a.commodity) {
            unpriced.push(a.commodity);
        }
    }
    if unpriced.is_empty() {
        return Ok(m);
    }
    let showc = |s: &str| quote_if_needed(s);
    let names: Vec<String> = unpriced.iter().map(|u| showc(u)).collect();
    let requested_note = match requested {
        Some(r) if r != c => format!(
            "\n{} was requested with --value, but the cash flows and PnL are in {}, as no market price converts them to {}.",
            showc(r),
            showc(c),
            showc(r)
        ),
        _ => String::new(),
    };
    Err(format!(
        "Could not value the investment on {date}: no market price in {} is known for {} on or before that date.{requested_note}\n\
         Add a P directive, or use --infer-market-prices to also use the prices recorded in transactions.",
        showc(c),
        names.join(", ")
    ))
}

impl Roi<'_> {
    /// The posting's date, or with --date2 its secondary date.
    fn posting_day(&self, t: &Transaction, p: &Posting) -> Day {
        self.ropts.posting_date(p, Some(t))
    }

    fn is_investment(&self, t: &Transaction, p: &Posting) -> bool {
        self.investment.matches_posting(PostingRef::new(t, p))
    }

    fn is_pnl(&self, t: &Transaction, p: &Posting) -> bool {
        !self.is_investment(t, p) && self.pnl.matches_posting(PostingRef::new(t, p))
    }

    /// The investment's holdings at the end of this day, with their costs
    /// or each commodity's added up.
    fn holdings_at(&self, with_costs: bool, d: Day) -> MixedAmount {
        let holdings = if with_costs {
            self.holdings_with_costs.get_or_init(|| {
                let mut acc = MixedAmount::null();
                (self.changes.iter())
                    .map(|(d, change)| {
                        acc = &acc + change;
                        (*d, acc.clone())
                    })
                    .collect()
            })
        } else {
            &self.holdings
        };
        (holdings.range(..=d).next_back()).map_or_else(MixedAmount::null, |(_, m)| m.clone())
    }

    /// Whether valuing the holdings needs their costs: when amounts are
    /// converted to cost first. Otherwise each commodity's holdings value
    /// at the same rate, or, unpriced, at its latest trade price, so their
    /// sum values the same, though an unpriced commodity's amounts with
    /// different costs show differently.
    fn valuing_needs_costs(&self) -> bool {
        matches!(
            self.ropts.conversion_op,
            Some(ConversionOp::ToCost | ConversionOp::ToTransactedCost)
        )
    }

    /// The holdings at the end of a day, valued on that day (see
    /// valuing_needs_costs): from each commodity's sum, unless the costs
    /// could show, in a value still in a commodity with costs, or in an
    /// error.
    fn holdings_value(&self, sv: &SpanValuation, d: Day) -> Result<MixedAmount, String> {
        if !self.valuing_needs_costs()
            && let Ok(v) = self.value_on(sv, d, &self.holdings_at(false, d))
            && v.commodities().iter().all(|c| !self.costed.contains(c))
        {
            return Ok(v);
        }
        self.value_on(sv, d, &self.holdings_at(true, d))
    }

    /// The price of the investment's latest purchase or sale of this
    /// commodity on or before this date.
    fn latest_trade_price(&self, c: &Sym, d: Day) -> Option<&Amount> {
        self.trade_prices
            .get(c)?
            .range(..=d)
            .next_back()
            .map(|(_, a)| a)
    }

    /// The amounts converted to cost (each styled), as -B or --value=cost
    /// say (mixedAmountToCost).
    fn to_cost(&self, op: ConversionOp, m: &MixedAmount) -> MixedAmount {
        match op {
            ConversionOp::NoConversionOp => m.clone(),
            ConversionOp::ToCost => m.map(|a| a.to_cost_basis().styled(&self.styles)),
            ConversionOp::ToTransactedCost => m.map(|a| a.to_cost().styled(&self.styles)),
        }
    }

    /// An amount converted to cost and valued as the options say, at the
    /// period's last day for an end value, with at most 8 decimal places
    /// (roi's mixedAmountValue).
    fn value(&self, period_last: Day, date: Day, m: &MixedAmount) -> Result<MixedAmount, String> {
        let m = match self.ropts.conversion_op {
            Some(op) => self.to_cost(op, m),
            None => m.clone(),
        };
        let m = match &self.ropts.value {
            Some(v) => mixed_amount_apply_valuation(
                &self.oracle,
                &self.styles,
                || Ok(period_last),
                self.today,
                date,
                v,
                &m,
            )?,
            None => m,
        };
        Ok(m.with_precision_max(8))
    }

    /// An amount of the investment valued on a date: positions fully closed
    /// by then dropped first; a commodity with no market price on or
    /// before then valued at the price of the investment's latest purchase
    /// or sale of it; the value in the expected commodity, or an error
    /// (valueOn).
    fn value_on(
        &self,
        sv: &SpanValuation,
        date: Day,
        m0: &MixedAmount,
    ) -> Result<MixedAmount, String> {
        let m = drop_closed_positions(m0);
        let valued = self.value(sv.last_day, date, &m)?;
        let unpriced: BTreeSet<Sym> = match &sv.expected {
            None => BTreeSet::new(),
            Some(c) => (valued.amounts().into_iter())
                .filter(|a| &a.commodity != c && !a.is_zero())
                .map(|a| a.commodity)
                .collect(),
        };
        let valued = if unpriced.is_empty() {
            valued
        } else {
            let repriced = (m.amounts().into_iter()).map(|a| {
                if !unpriced.contains(&a.commodity) {
                    return a;
                }
                match self.latest_trade_price(&a.commodity, date) {
                    Some(pr) => Amount {
                        quantity: &pr.quantity * &a.quantity,
                        ..pr.clone()
                    },
                    None => a,
                }
            });
            self.value(sv.last_day, date, &MixedAmount::from_amounts(repriced))?
        };
        let requested = self.ropts.value.as_ref().and_then(|v| v.commodity());
        check_valued(requested, sv.expected.as_ref(), date, valued)
    }

    /// The amounts of the real postings the query matches in the
    /// investment transactions, by date, without zeros (calculateCashFlow).
    fn cash_flow(&self, q: &Query) -> Vec<(Day, MixedAmount)> {
        let mut flows = Vec::new();
        for &t in &self.trans {
            for p in t.real_postings() {
                if q.matches_posting(PostingRef::new(t, p)) && !p.amount.is_zero() {
                    flows.push((self.posting_day(t, p), p.amount.clone()));
                }
            }
        }
        flows
    }

    /// The investment's cash flows from `b` to `last_day`, measured from
    /// its side: for each transaction and each date of its investment
    /// postings, the cost of those postings, less the transaction's profit
    /// and loss (counted on the first of those dates), valued; so that
    /// other postings in the transaction (fees, gains on other holdings)
    /// do not count, and flows are dated like the changes they make
    /// (investmentCashFlows).
    fn investment_cash_flows(
        &self,
        sv: &SpanValuation,
        b: Day,
    ) -> Result<Vec<InvestmentFlow>, String> {
        let mut flows = Vec::new();
        for &t in &self.trans {
            let ps: Vec<&Posting> = t.real_postings().collect();
            let mut by_day: BTreeMap<Day, Vec<&Posting>> = BTreeMap::new();
            for p in ps.iter().filter(|p| self.is_investment(t, p)) {
                by_day.entry(self.posting_day(t, p)).or_default().push(p);
            }
            let pnl = sum_postings(ps.iter().copied().filter(|p| self.is_pnl(t, p)));
            for (i, (d, invps)) in by_day.into_iter().enumerate() {
                if d < b || d > sv.last_day {
                    continue;
                }
                let cost = self.to_cost(ConversionOp::ToCost, &sum_postings(invps));
                let cost = self.value_on(sv, d, &cost)?;
                let gain = if i == 0 {
                    -self.value_on(sv, d, &pnl)?
                } else {
                    MixedAmount::null()
                };
                let net = &cost - &gain;
                if !net.is_zero() {
                    flows.push(InvestmentFlow {
                        date: d,
                        description: t.description.clone(),
                        cost,
                        pnl: gain,
                        flow: -net,
                    });
                }
            }
        }
        Ok(flows)
    }

    /// An amount on one line without costs, styled.
    fn show(&self, m: &MixedAmount) -> String {
        show_mixed_amount_b(
            &AmountFormat::ONE_LINE_NO_COST,
            &m.clone().styled(&self.styles),
        )
        .s
    }

    /// The report row of the period from `b` to the day before `e`, or the
    /// error that prevents calculating it, and with --cashflow the details
    /// of the calculation; an error if the period has several commodities
    /// (processSpan).
    fn process_span(
        &self,
        opts: &CliOpts,
        b: Day,
        e: Day,
    ) -> Result<Calculated<Vec<String>>, String> {
        let last_day = e.add_days(-1);
        let in_span = |flows: Vec<(Day, MixedAmount)>| -> Result<Vec<(Day, MixedAmount)>, String> {
            (flows.into_iter())
                .filter(|(d, _)| *d >= b && *d <= last_day)
                .map(|(d, m)| Ok((d, self.value(last_day, d, &m)?)))
                .collect()
        };
        let not = |q: &Query| Query::Not(Box::new(q.clone()));
        // the other postings of the investment transactions, where the money
        // comes from or goes to: their commodity, after valuation, is the
        // one the investment should be valued in
        let other_flows =
            in_span(self.cash_flow(&Query::And(vec![not(&self.investment), not(&self.pnl)])))?;
        let pnl =
            in_span(self.cash_flow(&Query::And(vec![not(&self.investment), self.pnl.clone()])))?;
        let expected = match &self.ropts.value {
            None => None,
            Some(v) => {
                let cs: BTreeSet<Sym> = (other_flows.iter().chain(&pnl))
                    .flat_map(|(_, m)| m.commodities())
                    .collect();
                match cs.len() {
                    1 => cs.into_iter().next(),
                    0 => v.commodity().cloned(),
                    _ => None,
                }
            }
        };
        let sv = SpanValuation { last_day, expected };
        // the value at the start is the value at the end of the previous day
        let before = b.add_days(-1);
        let value_before = self.holdings_value(&sv, before)?;
        let value_after = self.holdings_value(&sv, last_day)?;
        let flow_details = self.investment_cash_flows(&sv, b)?;
        let cash_flow: Vec<(Day, MixedAmount)> = (flow_details.iter())
            .map(|f| (f.date, f.flow.clone()))
            .collect();
        let commodities: BTreeSet<Sym> = [&value_before, &value_after]
            .into_iter()
            .chain(cash_flow.iter().chain(&pnl).map(|(_, m)| m))
            .flat_map(MixedAmount::commodities)
            .collect();
        if commodities.len() > 1 {
            let names: Vec<String> = commodities.iter().map(|c| quote_if_needed(c)).collect();
            return Err(multi_commodity_error(&format!(
                "Period ({b},{e}) has multiple commodities: {}",
                names.join(", ")
            )));
        }
        let commodity = commodities
            .first()
            .cloned()
            .unwrap_or_else(|| Sym::from(""));
        // a quantity of the period's commodity, styled
        let show_quantity = |q: &Decimal| {
            self.show(&MixedAmount::from_amount(Amount {
                commodity: commodity.clone(),
                quantity: q.clone(),
                ..Amount::null()
            }))
        };
        let span = OneSpan {
            begin: b,
            end: e,
            value_before,
            value_after,
            cash_flow,
        };
        let show_cash_flow = opts.raw.bool("cashflow");
        let (irr_details, irr) =
            self.internal_rate_of_return(show_cash_flow, &flow_details, &span)?;
        // upstream's TWR is lazy: its errors (eg amounts in several
        // commodities) surface only when its details are shown, or when the
        // IRR was found and the row needs it
        // from each commodity's holdings, which value the same, unless that
        // fails, when the error may show amounts with their costs
        let twr_with = |with_costs| {
            self.time_weighted_return(show_cash_flow, &show_quantity, &sv, &span, with_costs)
        };
        let (twr_details, twr) = if show_cash_flow || irr.is_ok() {
            match twr_with(self.valuing_needs_costs()) {
                Ok(twr) => twr,
                Err(_) => twr_with(true)?,
            }
        } else {
            (String::new(), Err(String::new()))
        };
        let cash_flow_amount = -span.cash_flow.iter().map(|(_, m)| m).sum::<MixedAmount>();
        let small_is_zero = |x: f64| if x.abs() < 0.01 { 0.0 } else { x };
        let percent = |x: f64| format!("{}%", show_ffloat(2, small_is_zero(x)));
        let row = irr.and_then(|irr| {
            twr.map(|(period, annualized)| {
                vec![
                    b.to_string(),
                    last_day.to_string(),
                    self.show(&span.value_before),
                    self.show(&cash_flow_amount),
                    self.show(&span.value_after),
                    self.show(&(&span.value_after - &(&span.value_before + &cash_flow_amount))),
                    percent(irr),
                    percent(period),
                    percent(annualized),
                ]
            })
        });
        Ok((irr_details + &twr_details, row))
    }

    /// The time-weighted rate of return for the period, and its annualised
    /// rate, in percent, or why it can not be calculated; and with
    /// --cashflow, how it was (timeWeightedReturn). It uses the
    /// investment's value at the end of each day: between days with cash
    /// flows the return is the ratio of values; a day with cash flows is
    /// split into its price changes, applied to the holdings carried over,
    /// and its flows, as if made at the day's closing prices, deposits at
    /// the start and withdrawals at the end. A stretch with nothing
    /// invested earns nothing. The returns are chained.
    fn time_weighted_return(
        &self,
        show_cash_flow: bool,
        show_quantity: &dyn Fn(&Decimal) -> String,
        sv: &SpanValuation,
        span: &OneSpan,
        with_costs: bool,
    ) -> Result<Calculated<(f64, f64)>, String> {
        let (begin, end) = (span.begin, span.end);
        let last_day = end.add_days(-1);
        let holdings_at = |d: Day| self.holdings_at(with_costs, d);
        let value_at = |d: Day| un_mix(&self.value_on(sv, d, &holdings_at(d))?);
        // each day's deposits and withdrawals, deposits positive
        // (M.fromListWith: the later on the left)
        let mut flows_by_day: BTreeMap<Day, (Decimal, Decimal)> = BTreeMap::new();
        for (d, m) in &span.cash_flow {
            let f = -un_mix(m)?;
            let zero = Decimal::ZERO;
            let (dep, wdr) = if zero <= f { (f, zero) } else { (zero, f) };
            let both = match flows_by_day.remove(d) {
                Some((i, o)) => (&dep + &i, &wdr + &o),
                None => (dep, wdr),
            };
            flows_by_day.insert(*d, both);
        }
        // the parts of the period: start, end, what it is, the value at the
        // start, deposits, withdrawals, the value at the end
        type Part = (Day, Day, &'static str, Decimal, Decimal, Decimal, Decimal);
        let mut parts: Vec<Part> = Vec::new();
        let (mut prev_day, mut prev_value) = (begin.add_days(-1), value_at(begin.add_days(-1))?);
        for (d, (deposits, withdrawals)) in flows_by_day {
            let before = d.add_days(-1);
            let start_value = value_at(before)?;
            let moved_value = un_mix(&self.value_on(sv, d, &holdings_at(before))?)?;
            let end_value = value_at(d)?;
            let zero = || Decimal::ZERO;
            if prev_day.add_days(1) <= before {
                parts.push((
                    prev_day.add_days(1),
                    before,
                    "no cash flows",
                    prev_value,
                    zero(),
                    zero(),
                    start_value.clone(),
                ));
            }
            if moved_value != start_value {
                parts.push((
                    d,
                    d,
                    "prices",
                    start_value,
                    zero(),
                    zero(),
                    moved_value.clone(),
                ));
            }
            parts.push((
                d,
                d,
                "cash flows",
                moved_value,
                deposits,
                withdrawals,
                end_value.clone(),
            ));
            (prev_day, prev_value) = (d, end_value);
        }
        if prev_day < last_day {
            let v = value_at(last_day)?;
            parts.push((
                prev_day.add_days(1),
                last_day,
                "no cash flows",
                prev_value,
                Decimal::ZERO,
                Decimal::ZERO,
                v,
            ));
        }
        let part_return = |(b, e, part, v0, dep, wdr, v1): &Part| -> Result<f64, String> {
            let base = v0 + dep;
            if base.is_zero() {
                // nothing invested at the start
                Ok(0.0)
            } else if base.is_negative() {
                let when = if b == e {
                    b.to_string()
                } else {
                    format!("{b} to {e}")
                };
                Err(format!(
                    "Cannot compute the time-weighted return for {when} ({part}): the investment's value at the start ({}) plus deposits ({}) is negative.",
                    show_quantity(v0),
                    show_quantity(dep)
                ))
            } else {
                Ok((v1 - wdr).to_f64() / base.to_f64() - 1.0)
            }
        };
        let returns: Vec<Result<f64, String>> = parts.iter().map(part_return).collect();
        // once a part's return can not be computed, neither can the
        // cumulative returns from there on
        let mut cumulative: Vec<Result<f64, String>> = Vec::new();
        let mut acc: Result<f64, String> = Ok(0.0);
        for r in &returns {
            acc = match (&acc, r) {
                (Err(e), _) | (Ok(_), Err(e)) => Err(e.clone()),
                (Ok(a), Ok(x)) => Ok((1.0 + a) * (1.0 + x) - 1.0),
            };
            cumulative.push(acc.clone());
        }
        let twr = cumulative.last().cloned().unwrap_or(Ok(0.0));
        let years = end.diff_days(begin) as f64 / 365.25;
        let result = twr.map(|twr| {
            let period = (&Decimal::from_i64(100) * &Decimal::from_f64(twr)).round_to(2);
            let annualized = 100.0 * ((1.0 + twr).powf(1.0 / years) - 1.0);
            (period.to_f64(), annualized)
        });
        if !show_cash_flow {
            return Ok((String::new(), result));
        }
        let show_percent = |r: &Result<f64, String>| match r {
            Err(_) => "error".to_string(),
            Ok(r) => (&Decimal::from_f64(*r) * &Decimal::from_i64(100))
                .round_to(2)
                .to_string(),
        };
        let leaves = |names: &[&str]| -> Vec<Header<String>> {
            names.iter().map(|n| Header::Leaf(n.to_string())).collect()
        };
        let table = Table {
            row_headers: Header::Group(
                Line::NoLine,
                (1..=parts.len())
                    .map(|n| Header::Leaf(n.to_string()))
                    .collect(),
            ),
            col_headers: Header::Group(
                Line::DoubleLine,
                vec![
                    Header::Group(Line::SingleLine, leaves(&["Begin", "End", "Part"])),
                    Header::Group(
                        Line::SingleLine,
                        leaves(&["Value (begin)", "Deposits", "Withdrawals", "Value (end)"]),
                    ),
                    Header::Group(Line::SingleLine, leaves(&["Return, %", "Cumulative, %"])),
                ],
            ),
            rows: (parts.iter().zip(&returns).zip(&cumulative))
                .map(|(((b, e, part, v0, dep, wdr, v1), r), c)| {
                    vec![
                        b.to_string(),
                        e.to_string(),
                        part.to_string(),
                        show_quantity(v0),
                        show_quantity(dep),
                        show_quantity(wdr),
                        show_quantity(v1),
                        show_percent(r),
                        show_percent(c),
                    ]
                })
                .collect(),
        };
        let mut details = format!("\nTWR returns for period {begin} - {last_day}\n");
        details += &render(self.ropts.pretty, &table);
        details += "Each row's return is (Value (end) - Withdrawals) / (Value (begin) + Deposits) - 1; they are chained.\n";
        details += &match &result {
            Ok((period, annualized)) => format!(
                "Total period TWR: {}%.\nPeriod: {} years.\nAnnualized TWR: {}%\n\n",
                show_ffloat(2, *period),
                show_ffloat(2, years),
                show_ffloat(2, *annualized)
            ),
            Err(_) => "\n".to_string(),
        };
        Ok((details, result))
    }

    /// The internal rate of return for the period, in percent, or why it
    /// can not be calculated; and with --cashflow, the cash flows it is
    /// calculated from: the value at the start as a deposit, the flows, and
    /// the value at the end as a withdrawal the day after the period
    /// (internalRateOfReturn).
    fn internal_rate_of_return(
        &self,
        show_cash_flow: bool,
        flow_details: &[InvestmentFlow],
        span: &OneSpan,
    ) -> Result<Calculated<f64>, String> {
        let (begin, end) = (span.begin, span.end);
        let mut flows = span.cash_flow.clone();
        flows.sort_by_key(|(d, _)| *d);
        let total: Vec<(Day, MixedAmount)> = std::iter::once((begin, -&span.value_before))
            .chain(flows)
            .chain(std::iter::once((end, span.value_after.clone())))
            .filter(|(_, m)| !m.is_zero())
            .collect();
        let details = if !show_cash_flow {
            String::new()
        } else {
            let mut rows: Vec<(Day, String, String, String, MixedAmount)> = Vec::new();
            if !span.value_before.is_zero() {
                let start = "value at the start".to_string();
                rows.push((
                    begin,
                    start,
                    String::new(),
                    String::new(),
                    -&span.value_before,
                ));
            }
            let mut sorted: Vec<&InvestmentFlow> = flow_details.iter().collect();
            sorted.sort_by_key(|f| f.date);
            for f in sorted {
                rows.push((
                    f.date,
                    f.description.clone(),
                    self.show(&f.cost),
                    self.show(&f.pnl),
                    f.flow.clone(),
                ));
            }
            if !span.value_after.is_zero() {
                let last = format!("value at the end of {}", end.add_days(-1));
                rows.push((
                    end,
                    last,
                    String::new(),
                    String::new(),
                    span.value_after.clone(),
                ));
            }
            let table = Table {
                row_headers: Header::Group(
                    Line::NoLine,
                    rows.iter()
                        .map(|(d, ..)| Header::Leaf(d.to_string()))
                        .collect(),
                ),
                col_headers: Header::Group(
                    Line::SingleLine,
                    [
                        "Description",
                        "Investment, at cost",
                        "Profit/loss",
                        "Amount",
                    ]
                    .iter()
                    .map(|h| Header::Leaf(h.to_string()))
                    .collect(),
                ),
                rows: (rows.iter())
                    .map(|(_, desc, cost, pnl, amt)| {
                        vec![desc.clone(), cost.clone(), pnl.clone(), self.show(amt)]
                    })
                    .collect(),
            };
            format!(
                "\nIRR cash flows for {begin} - {}\n{}Amounts are from your point of view: what you put in is negative, what you take out (or still have) positive.\n\n",
                end.add_days(-1),
                render(self.ropts.pretty, &table)
            )
        };
        if total.is_empty() {
            // 0% is always a solution, so require at least something here
            return Ok((details, Ok(0.0)));
        }
        let flows: Vec<(f64, f64)> = (total.iter())
            .map(|(d, m)| Ok((un_mix(m)?.to_f64(), end.diff_days(*d) as f64 / 365.25)))
            .collect::<Result<_, String>>()?;
        // interestSum
        let npv = |rate: f64| {
            flows
                .iter()
                .fold(0.0, |s, (q, years)| s + q * rate.powf(*years))
        };
        let irr = solve_irr(npv).map(|r| (r - 1.0) * 100.0);
        Ok((details, irr))
    }
}

/// The rate solving the net present value equation, by Ridders' method in
/// (1e-12, 10000), or why there is none (solveIRR).
pub fn solve_irr(npv: impl Fn(f64) -> f64) -> Result<f64, String> {
    match ridders(100, 0.00001, (0.000000000001, 10000.0), npv) {
        RootSearch::Root(s) => Ok(s),
        RootSearch::NotBracketed => Err(
            "Equation for Internal Rate of Return (IRR) can not be solved.\n  Possible causes: IRR is huge (>1000000%), balance of investment becomes negative at some point in time."
                .to_string(),
        ),
        RootSearch::SearchFailed => Err(
            "Equation for Internal Rate of Return (IRR) can not be solved.\n  Either search does not converge to a solution, or converges too slowly."
                .to_string(),
        ),
    }
}

/// The outcome of a root search (math-functions' Root).
#[derive(Debug, PartialEq)]
enum RootSearch {
    NotBracketed,
    SearchFailed,
    Root(f64),
}

/// A root of the function in the bracket, by Ridders' method, as
/// math-functions' Numeric.RootFinding.ridders finds it: within this
/// absolute tolerance (or one unit in the last place) in at most this many
/// steps; the function must have opposite signs at the bracket's ends.
fn ridders(max_iter: usize, tol: f64, (lo, hi): (f64, f64), f: impl Fn(f64) -> f64) -> RootSearch {
    // a step: a bracket, or a root found
    enum Step {
        Bracket(f64, f64),
        Root(f64),
    }
    // Haskell's signum, which keeps a zero (or NaN) as it is
    let signum = |x: f64| {
        if x > 0.0 {
            1.0
        } else if x < 0.0 {
            -1.0
        } else {
            x
        }
    };
    let within = |a: f64, b: f64| ulp_distance(a, b) <= 1 || (a - b).abs() < tol;
    let (flo, fhi) = (f(lo), f(hi));
    if flo == 0.0 {
        return RootSearch::Root(lo);
    }
    if fhi == 0.0 {
        return RootSearch::Root(hi);
    }
    if flo * fhi > 0.0 {
        return RootSearch::NotBracketed;
    }
    let (mut a, mut fa, mut b, mut fb) = if lo < hi {
        (lo, flo, hi, fhi)
    } else {
        (hi, fhi, lo, flo)
    };
    // the steps are the given bracket, then each Ridders (or bisection)
    // update of it (riddersIterations); at most max_iter are examined
    // (findRoot)
    let mut step = Step::Bracket(lo, hi);
    for _ in 0..max_iter {
        match step {
            Step::Root(x) => return RootSearch::Root(x),
            Step::Bracket(x, y) if within(x, y) => return RootSearch::Root((x + y) / 2.0),
            Step::Bracket(..) => {}
        }
        let dm = (b - a) * 0.5;
        let m = (a + b) / 2.0;
        let fm = f(m);
        step = if fm == 0.0 {
            Step::Root(m)
        } else {
            let n = m - signum(fb - fa) * dm * fm / (fm * fm - fa * fb).sqrt();
            let fnn = f(n);
            if fnn == 0.0 {
                Step::Root(n)
            } else {
                (a, fa, b, fb) = if n <= a || n >= b {
                    // the update left the bracket: bisect instead
                    if fm * fa < 0.0 {
                        (a, fa, m, fm)
                    } else {
                        (m, fm, b, fb)
                    }
                } else if fnn * fm < 0.0 {
                    (n, fnn, m, fm)
                } else if fnn * fa < 0.0 {
                    (a, fa, n, fnn)
                } else {
                    (n, fnn, b, fb)
                };
                Step::Bracket(a, b)
            }
        };
    }
    RootSearch::SearchFailed
}

/// The number of representable doubles between two (ulpDistance).
fn ulp_distance(a: f64, b: f64) -> u64 {
    // ordered so that they compare as unsigned integers
    let order = |x: f64| {
        let i = x.to_bits();
        if i < 0x8000_0000_0000_0000 {
            i + 0x8000_0000_0000_0000
        } else {
            u64::MAX - i
        }
    };
    order(a).abs_diff(order(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    // math-functions' tests of ridders, with absolute tolerances
    #[test]
    fn ridders_finds_roots() {
        let exact = 0.5527151130967832;
        for tol in [0.0, 1e-12, 1e-6] {
            let RootSearch::Root(r) = ridders(100, tol, (0.0, std::f64::consts::FRAC_PI_2), |x| {
                x.sin() - 0.525
            }) else {
                panic!("no root");
            };
            assert!(
                ulp_distance(r, exact) <= 1 || (r - exact).abs() < tol,
                "{r} with {tol}"
            );
        }
        assert_eq!(
            ridders(100, 1e-6, (0.0, 1.0), |x| x + 1.0),
            RootSearch::NotBracketed
        );
        assert_eq!(
            ridders(100, 1e-6, (1.0, 3.0), |x| x - 1.0),
            RootSearch::Root(1.0)
        );
    }

    #[test]
    fn ulps() {
        assert_eq!(ulp_distance(1.0, 1.0), 0);
        assert_eq!(ulp_distance(1.0, f64::from_bits(1.0f64.to_bits() + 1)), 1);
        assert_eq!(ulp_distance(0.0, -0.0), 1);
    }

    #[test]
    fn irr_of_a_year_at_twelve_percent() {
        // $100 in, $112 out a year (365.25 days) later
        let npv = |rate: f64| -100.0 * rate.powf(1.0) + 112.0 * rate.powf(0.0);
        let r = solve_irr(npv).unwrap();
        assert!(((r - 1.0) * 100.0 - 12.0).abs() < 1e-3);
    }
}
