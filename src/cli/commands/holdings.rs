//! The holdings command (Hledger.Cli.Commands.Holdings): the investment
//! lots held in lot-tracked accounts at the report end date, one line per
//! account (or lot subaccount, with --lots) and commodity, with their
//! date, age, units, unit or average cost, market price and value, weight
//! in the portfolio, unrealised and realised gains, and annualised rate of
//! return (XIRR); in tree mode grouped into account sections; with a
//! report interval, the totals at each period's end; in every output
//! format, or as a chart.

use crate::i18n::Translations;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde_json::{Value, json};

use crate::account::{
    AccountName, AccountType, account_leaf_name, account_name_drop, account_name_level,
    expand_account_name, lot_base_account, parent_account_name, parent_account_names,
};
use crate::amount::render::{AmountFormat, show_amount_b, show_mixed_amount_b};
use crate::amount::{
    Amount, AmountStyle, CommodityStyles, CostBasis, MixedAmount, Precision, Rounding, Side, Sym,
    styles_with_rounding,
};
use crate::cli::args::{Arity, Flag, Mode};
use crate::cli::chart::{chart_requested, check_chart_format, render_chart};
use crate::cli::commands::balance::{CHART_FLAG, CHART_WIDTH_FLAG};
use crate::cli::commands::print::{round_from_raw_opts, unsupported_output_format_error};
use crate::cli::io::warn_if_large_multi_period_report;
use crate::cli::opts::{
    CliOpts, OUTPUT_FILE_FLAG, OUTPUT_VIEW_FLAG, flat_flag, output_format_flag, tree_flag,
};
use crate::dates::{DateSpan, Day, Interval, maybe_day_partition_to_date_spans};
use crate::decimal::Decimal;
use crate::lots::{
    ReductionMethod, journal_average_pool_totals, journal_collapse_lot_detail, lot_subaccount_name,
    parse_lot_name, resolve_reduction_method,
};
use crate::model::Journal;
use crate::query::Query;
use crate::reports::opts::{AccountListMode, BalanceAccumulation, ReportSpec};
use crate::reports::periodic::multi_balance_report;
use crate::text::show_ffloat;
use crate::valuation::{ConversionOp, PriceOracle, ValuationType, amount_value_at_date};
use crate::write::chart::{BarLayout, Chart, ChartLabel, Mark, Panel, Series, SeriesRole, XAxis};
use crate::write::csv::{print_csv, print_tsv};
use crate::write::json::double;
use crate::write::spreadsheet::{self as spr, CellType, add_header_borders};
use crate::write::table::{
    Align, Cell, Header, Line, Table, TableOpts, concat_tables, render_table,
};

const NONE: Arity = Arity::None;
const REQ: Arity = Arity::Req;

pub static MODE: Mode = Mode {
    name: "holdings",
    aliases: &[],
    flags: &[
        flat_flag(true),
        tree_flag(true),
        Flag::new(
            &["no-elide"],
            NONE,
            "",
            "in tree mode, don't squash boring parent accounts; in list mode, also show parent accounts (usually zero, hidden without -E)",
        ),
        Flag::new(
            &["full-names"],
            NONE,
            "",
            "in tree mode, show full account names instead of indented leaf names",
        ),
        Flag::new(
            &["drop"],
            REQ,
            "N",
            "omit N leading account name parts; in tree mode, remove the first N levels",
        ),
        Flag::new(
            &["sort-amount"],
            NONE,
            "",
            "sort by value (or cost) instead of account name, largest first",
        )
        .short(&['S']),
        Flag::new(&["no-total"], NONE, "", "omit the final total row").short(&['N']),
        Flag::new(
            &["round"],
            REQ,
            "TYPE",
            "how much rounding or padding should be done when displaying amounts ?\nnone - show original decimal digits\nsoft - just add or remove decimal zeros\n       to match precision\nhard - round amounts to precision (default)\nall  - also round cost amounts to precision",
        ),
        CHART_FLAG,
        CHART_WIDTH_FLAG,
        output_format_flag(
            "select the output format. Supported formats:\ntxt, csv, tsv, html, fods, json, svg.",
        ),
        OUTPUT_FILE_FLAG,
        OUTPUT_VIEW_FLAG,
    ],
    doc: crate::command_doc!("Holdings"),
    ..Mode::DEFAULT
};

/// One commodity held in one displayed account or lot subaccount, with
/// its figures: a row of the table, a record of the csv and json output
/// (Holding). A commodity no longer held but with realised gains has one
/// with -E, with zero units.
#[derive(Clone, Debug)]
struct Holding {
    account: AccountName,
    commodity: Sym,
    /// The acquisition date, when the lots share one.
    date: Option<Day>,
    age: Option<i64>,
    units: Amount,
    unit_cost: Option<Amount>,
    /// The market prices at the valuation date (several if the lots' value
    /// commodities differ).
    price: Vec<Amount>,
    cost: Vec<Amount>,
    /// The market value, when all lots are priced.
    value: Vec<Amount>,
    weight: Option<Decimal>,
    ugain: Vec<Amount>,
    ugain_pct: Option<Decimal>,
    rgain: Vec<Amount>,
    xirr: Option<f64>,
}

/// The totals of some holdings: the report's, or an account heading's in
/// tree mode (HoldingsTotals). Units and average cost only for a single
/// commodity; value and gains only when all are priced.
#[derive(Clone, Debug, Default)]
struct Totals {
    units: Option<Amount>,
    avg_cost: Option<Amount>,
    cost: Vec<Amount>,
    value: Option<Vec<Amount>>,
    weight: Option<Decimal>,
    ugain: Option<Vec<Amount>>,
    ugain_pct: Option<Decimal>,
    rgain: Vec<Amount>,
    xirr: Option<f64>,
}

/// A display line (HoldingsLine): an account heading showing its
/// subtree's totals (tree mode), or a holding.
#[derive(Clone, Debug)]
enum HLine {
    Heading {
        indent: usize,
        account: AccountName,
        display: String,
        totals: Totals,
    },
    Position {
        indent: usize,
        account: AccountName,
        display: String,
        is_lot: bool,
        holding: Holding,
    },
}

/// A holdings report (HoldingsReport).
struct Report {
    date: Day,
    lines: Vec<HLine>,
    /// The holdings as flat records, for the machine-readable outputs.
    rows: Vec<Holding>,
    totals: Totals,
    unit_cost_heading: &'static str,
    /// The display styles, with one for the % commodity.
    styles: CommodityStyles,
}

/// A lot: its units and its cost basis, if known.
type Lot = (Amount, Option<CostBasis>);

/// Values per account, each with its commodity, in ascending (lot
/// subaccount, commodity) order.
type Ix<V> = BTreeMap<AccountName, Vec<(Sym, V)>>;

/// Amounts summed per commodity, none staying none (sumAmounts).
fn sum_amounts(amts: Vec<Amount>) -> Vec<Amount> {
    if amts.is_empty() {
        Vec::new()
    } else {
        MixedAmount::from_amounts(amts).amounts()
    }
}

/// The sum of the amounts' quantities.
fn sum_quantities<'a>(amts: impl IntoIterator<Item = &'a Amount>) -> Decimal {
    amts.into_iter()
        .fold(Decimal::ZERO, |s, a| &s + &a.quantity)
}

fn hundred() -> Decimal {
    Decimal::from_i64(100)
}

/// An average cost: total cost over units, shown in the cost's precision,
/// but with at least 2 decimals when the division is inexact and the
/// precision is less (avgcost).
fn avg_cost(units: &Decimal, cost: &Amount) -> Amount {
    let avg = cost.clone().divide_and_update_precision(units);
    let pdiv = match avg.style.precision {
        Precision::Fixed(n) => n,
        Precision::Natural => crate::amount::DEFAULT_MAX_DISPLAY_PRECISION,
    };
    let pstyle = match cost.style.precision {
        Precision::Fixed(n) => n,
        Precision::Natural => 2,
    };
    let p = pstyle.max(pdiv.min(pstyle.max(2)));
    avg.with_precision(Precision::Fixed(p))
}

/// The flat (list mode) report's data: the lots, their cash flows and
/// realised gains, indexed by account, and the valuation settings
/// (flatHoldingsReport's where clause).
struct Flat {
    report_date: Day,
    styles: CommodityStyles,
    oracle: PriceOracle,
    valuing: bool,
    valuation_date: Day,
    target: Option<Sym>,
    lot_keys: Vec<(AccountName, Sym)>,
    flows: BTreeMap<(AccountName, Sym), Vec<(Day, Amount)>>,
    rgains: BTreeMap<(AccountName, Sym), Amount>,
    under_lots: Ix<Lot>,
    under_flows: Ix<Vec<(Day, Amount)>>,
    under_rgains: Ix<Amount>,
    own_lots: Ix<Lot>,
    own_flows: Ix<Vec<(Day, Amount)>>,
    own_rgains: Ix<Amount>,
    /// The first error from price lookups, raised after the report.
    error: RefCell<Option<String>>,
    portfolio_value: Option<Vec<Amount>>,
}

/// Index values under their lot subaccount and each of its parents
/// (underIx).
fn under_ix<V: Clone>(m: &BTreeMap<(AccountName, Sym), V>) -> Ix<V> {
    let mut ix: Ix<V> = BTreeMap::new();
    for ((sub, c), v) in m {
        for a in std::iter::once(&**sub).chain(parent_account_names(sub)) {
            ix.entry(AccountName::from(a))
                .or_default()
                .push((c.clone(), v.clone()));
        }
    }
    ix
}

/// Index values under their owning row account: the deepest of these at
/// or above their lot subaccount (ownIx).
fn own_ix<V: Clone>(m: &BTreeMap<(AccountName, Sym), V>, rows: &BTreeSet<AccountName>) -> Ix<V> {
    let mut ix: Ix<V> = BTreeMap::new();
    for ((sub, c), v) in m {
        let owner = std::iter::once(&**sub)
            .chain(parent_account_names(sub))
            .find(|a| rows.contains(*a));
        if let Some(owner) = owner {
            ix.entry(AccountName::from(owner))
                .or_default()
                .push((c.clone(), v.clone()));
        }
    }
    ix
}

/// The values indexed under an account, of one commodity if given.
fn in_commodity<'v, V>(ix: &'v Ix<V>, acct: &str, c: Option<&Sym>) -> Vec<&'v V> {
    ix.get(acct)
        .into_iter()
        .flatten()
        .filter(|(c2, _)| c.is_none_or(|c| c == c2))
        .map(|(_, v)| v)
        .collect()
}

impl Flat {
    fn lots_under(&self, acct: &str) -> Vec<&Lot> {
        in_commodity(&self.under_lots, acct, None)
    }

    fn lots_of(&self, acct: &str) -> Vec<&Lot> {
        in_commodity(&self.own_lots, acct, None)
    }

    /// A market price lookup; an error is kept, to be raised after the
    /// report, and the lot treated as unpriced.
    fn price(&self, from: &Sym, to: Option<&Sym>) -> Option<(Sym, Decimal)> {
        match self.oracle.lookup(self.valuation_date, from, to) {
            Ok(found) => found,
            Err(e) => {
                self.error.borrow_mut().get_or_insert(e);
                None
            }
        }
    }

    /// An amount with all its digits up to 8, then styled.
    fn mkamt(&self, c: Sym, q: Decimal) -> Amount {
        Amount {
            commodity: c,
            quantity: q,
            ..Amount::null()
        }
        .with_full_precision_up_to(None)
        .styled(&self.styles)
    }

    /// A lot's unit price and value at the valuation date, in the
    /// requested commodity or else its cost's (lotValuation).
    fn lot_valuation(&self, (a, cb): &Lot) -> Option<(Amount, Amount)> {
        let to = if self.valuing {
            self.target.clone()
        } else {
            cb.as_ref()
                .and_then(|cb| cb.cost.as_ref())
                .map(|c| c.commodity.clone())
        };
        let (pc, rate) = self.price(&a.commodity, to.as_ref())?;
        let value = &rate * &a.quantity;
        Some((self.mkamt(pc.clone(), rate), self.mkamt(pc, value)))
    }

    /// The lots' distinct unit prices and their total value per value
    /// commodity, if every lot is priced (lotsValuation).
    fn lots_valuation(&self, lots: &[&Lot]) -> Option<(Vec<Amount>, Vec<Amount>)> {
        let pvs: Vec<(Amount, Amount)> = lots
            .iter()
            .map(|l| self.lot_valuation(l))
            .collect::<Option<_>>()?;
        let mut prices: Vec<Amount> = pvs.iter().map(|(p, _)| p.clone()).collect();
        prices.sort_by(|x, y| (&x.commodity, &x.quantity).cmp(&(&y.commodity, &y.quantity)));
        prices.dedup_by(|x, y| x.commodity == y.commodity && x.quantity == y.quantity);
        Some((
            prices,
            sum_amounts(pvs.into_iter().map(|(_, v)| v).collect()),
        ))
    }

    fn value_under(&self, acct: &str) -> Option<Vec<Amount>> {
        self.lots_valuation(&self.lots_under(acct)).map(|(_, v)| v)
    }

    fn value_of(&self, acct: &str) -> Option<Vec<Amount>> {
        self.lots_valuation(&self.lots_of(acct)).map(|(_, v)| v)
    }

    /// With -V/-X/--value, a cost converted to the requested commodity, or
    /// this fallback (costValuerTo).
    fn cost_valued(&self, fallback: Option<&Sym>, a: Amount) -> Amount {
        if !self.valuing {
            return a;
        }
        let Some(tc) = self.target.as_ref().or(fallback) else {
            return a;
        };
        if &a.commodity == tc {
            return a;
        }
        match amount_value_at_date(
            &self.oracle,
            &self.styles,
            Some(tc),
            self.valuation_date,
            &a,
        ) {
            Ok(v) => v.styled(&self.styles),
            Err(e) => {
                self.error.borrow_mut().get_or_insert(e);
                a
            }
        }
    }

    /// The value commodity of an account's lots, when single.
    fn value_commodity(value: &Option<Vec<Amount>>) -> Option<Sym> {
        match value.as_deref() {
            Some([v]) => Some(v.commodity.clone()),
            _ => None,
        }
    }

    /// A row's costs converted as its value is (rowCostValuer).
    fn row_cost_valued(&self, acct: &str, a: Amount) -> Amount {
        if !self.valuing {
            return a;
        }
        let fallback = Self::value_commodity(&self.value_under(acct));
        self.cost_valued(fallback.as_ref(), a)
    }

    /// The lots' cost bases: units times unit cost, converted as a row's
    /// costs.
    fn lot_costs(&self, lots: &[&Lot], valued: impl Fn(Amount) -> Amount) -> Vec<Amount> {
        lots.iter()
            .filter_map(|(a, cb)| {
                let c = cb.as_ref()?.cost.as_deref()?;
                Some(valued(c.clone().multiply(&a.quantity)))
            })
            .collect()
    }

    /// The annualised rate of return of dated cash flows (negative money
    /// invested) up to the report date, as a percentage (xirrPct).
    fn xirr_pct(&self, cf: &[(Day, Decimal)]) -> Option<f64> {
        let npv = |rate: f64| {
            cf.iter().fold(0.0, |s, (t, n)| {
                s + n.to_f64() * rate.powf(self.report_date.diff_days(*t) as f64 / 365.25)
            })
        };
        super::roi::solve_irr(npv).ok().map(|r| (r - 1.0) * 100.0)
    }

    /// The XIRR of cash flows and a final value, all in one commodity; not
    /// for a short position (xirrOf).
    fn xirr_of(&self, flows: &[(Day, Amount)], finalv: &Amount) -> Option<f64> {
        if flows.is_empty() || flows.iter().any(|(_, a)| a.commodity != finalv.commodity) {
            return None;
        }
        let earliest = flows
            .iter()
            .fold(None::<&(Day, Amount)>, |m, f| match m {
                Some(m) if m.0 <= f.0 => Some(m),
                _ => Some(f),
            })
            .map(|(_, a)| a.quantity.clone())?;
        let short = finalv.is_negative() || (finalv.quantity.is_zero() && earliest > Decimal::ZERO);
        if short {
            return None;
        }
        let mut cf = vec![(self.report_date, finalv.quantity.clone())];
        cf.extend(flows.iter().map(|(d, a)| (*d, a.quantity.clone())));
        self.xirr_pct(&cf)
    }

    /// A value's percent of the portfolio's value (weightPct).
    fn weight_pct(&self, val: &[Amount]) -> Option<Decimal> {
        let [t] = self.portfolio_value.as_deref()? else {
            return None;
        };
        let [v] = val else { return None };
        (v.commodity == t.commodity && !t.quantity.is_zero())
            .then(|| &(&hundred() * &v.quantity) / &t.quantity)
    }

    /// Value less cost per commodity, when they cover the same commodities
    /// (gainAmounts).
    fn gain_amounts(&self, vals: Vec<Amount>, costs: Vec<Amount>) -> Option<Vec<Amount>> {
        let (vs, cs) = (sum_amounts(vals), sum_amounts(costs));
        let same =
            vs.len() == cs.len() && vs.iter().zip(&cs).all(|(v, c)| v.commodity == c.commodity);
        (!vs.is_empty() && same).then(|| {
            vs.iter()
                .zip(&cs)
                .map(|(v, c)| self.mkamt(v.commodity.clone(), &v.quantity - &c.quantity))
                .collect()
        })
    }

    /// The gain's percent of the cost's magnitude (gainPct).
    fn gain_pct(gains: &[Amount], costs: Vec<Amount>) -> Option<Decimal> {
        let [g] = gains else { return None };
        let [c] = sum_amounts(costs).try_into().ok()?;
        (g.commodity == c.commodity && !c.quantity.is_zero())
            .then(|| &(&hundred() * &g.quantity) / &c.quantity.abs())
    }

    /// The totals of the lots at or under an account, like the grand
    /// totals; a heading's figures in tree mode (accountTotals).
    fn account_totals(&self, acct: &str) -> Totals {
        let lots = self.lots_under(acct);
        let value = self.value_under(acct);
        let vc = Self::value_commodity(&value);
        let units: Vec<Amount> = sum_amounts(lots.iter().map(|(a, _)| a.clone()).collect())
            .into_iter()
            .filter(|a| !a.looks_zero())
            .collect();
        let costs = self.lot_costs(&lots, |a| self.cost_valued(vc.as_ref(), a));
        let gains = value
            .as_ref()
            .and_then(|v| self.gain_amounts(v.clone(), costs.clone()));
        let rgains: Vec<Amount> = in_commodity(&self.under_rgains, acct, None)
            .into_iter()
            .map(|g| self.cost_valued(vc.as_ref(), g.clone()))
            .collect();
        let flows: Vec<(Day, Amount)> = in_commodity(&self.under_flows, acct, None)
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        Totals {
            avg_cost: match (units.as_slice(), sum_amounts(costs.clone()).as_slice()) {
                ([u], [c]) => Some(avg_cost(&u.quantity, c)),
                _ => None,
            },
            units: match units.as_slice() {
                [u] => Some(u.clone()),
                _ => None,
            },
            cost: sum_amounts(costs.clone()),
            weight: value.as_ref().and_then(|v| self.weight_pct(v)),
            ugain_pct: Self::gain_pct(gains.as_deref().unwrap_or(&[]), costs),
            ugain: gains,
            rgain: sum_amounts(rgains),
            xirr: match value.as_deref() {
                Some([v]) => self.xirr_of(&flows, v),
                _ => None,
            },
            value,
        }
    }

    /// A row's holdings, one per commodity its lots hold, then with -E one
    /// with zero units per commodity with only realised gains left
    /// (rowHoldings).
    fn row_holdings(&self, acct: &AccountName, empty: bool) -> Vec<Holding> {
        let lots = self.lots_of(acct);
        let held: Vec<Amount> = sum_amounts(lots.iter().map(|(a, _)| a.clone()).collect())
            .into_iter()
            .filter(|a| !a.looks_zero())
            .collect();
        let mut amts = held.clone();
        if empty {
            let rgain_comms: BTreeSet<&Sym> = self
                .own_rgains
                .get(acct)
                .into_iter()
                .flatten()
                .filter(|(_, g)| !g.looks_zero())
                .map(|(c, _)| c)
                .collect();
            for c in rgain_comms {
                if !held.iter().any(|a| &a.commodity == c) {
                    amts.push(Amount {
                        commodity: c.clone(),
                        ..Amount::null()
                    });
                }
            }
        }
        amts.into_iter()
            .map(|qa| self.holding(acct, &lots, qa))
            .collect()
    }

    fn holding(&self, acct: &AccountName, lots: &[&Lot], qa: Amount) -> Holding {
        let c = qa.commodity.clone();
        let clots: Vec<&Lot> = lots
            .iter()
            .filter(|(a, _)| a.commodity == c)
            .copied()
            .collect();
        let dates: BTreeSet<Option<Day>> = clots
            .iter()
            .map(|(_, cb)| cb.as_ref().and_then(|cb| cb.date))
            .collect();
        let date = match Vec::from_iter(dates).as_slice() {
            [Some(d)] => Some(*d),
            _ => None,
        };
        let valued = |a: Amount| self.row_cost_valued(acct, a);
        let ccosts = self.lot_costs(&clots, valued);
        let unit_cost = match clots.as_slice() {
            [(_, cb)] => cb
                .as_ref()
                .and_then(|cb| cb.cost.as_deref())
                .map(|uc| valued(uc.clone())),
            _ if !ccosts.is_empty() => match MixedAmount::from_amounts(ccosts.clone())
                .amounts()
                .as_slice()
            {
                [tot] if !qa.looks_zero() => Some(avg_cost(&qa.quantity, tot)),
                _ => None,
            },
            _ => None,
        };
        let zero_lot = (qa.clone(), None);
        let vlots: Vec<&Lot> = if clots.is_empty() {
            vec![&zero_lot]
        } else {
            clots.clone()
        };
        let (price, value) = self.lots_valuation(&vlots).unwrap_or_default();
        let ugain = self
            .gain_amounts(value.clone(), ccosts.clone())
            .unwrap_or_default();
        let flows: Vec<(Day, Amount)> = in_commodity(&self.own_flows, acct, Some(&c))
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        let rgains: Vec<Amount> = in_commodity(&self.own_rgains, acct, Some(&c))
            .into_iter()
            .map(|g| valued(g.clone()))
            .collect();
        Holding {
            account: acct.clone(),
            commodity: c,
            date,
            age: date.map(|d| self.report_date.diff_days(d)),
            units: qa.styled(&self.styles),
            unit_cost,
            weight: self.weight_pct(&value),
            ugain_pct: Self::gain_pct(&ugain, ccosts.clone()),
            xirr: match value.as_slice() {
                [v] => self.xirr_of(&flows, v),
                _ => None,
            },
            cost: sum_amounts(ccosts),
            price,
            value,
            ugain,
            rgain: sum_amounts(rgains),
        }
    }
}

/// The flat holdings report, and its data (flatHoldingsReport): the lots
/// at the report end date, aggregated by the rows of a historical balance
/// report (of lot subaccounts with `show_lots`, of the accounts above
/// them otherwise), one line per row and commodity.
fn flat_report(
    show_lots: bool,
    rounding: Rounding,
    spec: &ReportSpec,
    j: &Journal,
) -> Result<(Report, Flat), String> {
    let ropts = &spec.opts;
    let q = &spec.query;
    let end = q.end_date(false);
    let report_date = end.map_or(spec.today, |e| e.add_days(-1));
    let endq = Query::And(vec![
        q.clone()
            .filter(|x| !(x.is_date_or_date2() || x.is_depth())),
        Query::Date(DateSpan::exact(None, end)),
    ]);
    let styles = styles_with_rounding(rounding, &j.commodity_styles());
    let mut lotmap: BTreeMap<(AccountName, Sym), Amount> = BTreeMap::new();
    let mut flows: BTreeMap<(AccountName, Sym), Vec<(Day, Amount)>> = BTreeMap::new();
    let mut rgains: BTreeMap<(AccountName, Sym), Amount> = BTreeMap::new();
    for r in j.postings() {
        if crate::lots::lot_subaccount_name(&r.p.account).is_none() || !endq.matches_posting(r) {
            continue;
        }
        let disposal = crate::lots::is_dispose_posting(r.p);
        for a in r.p.amount.amounts_raw() {
            let k = (r.p.account.clone(), a.commodity.clone());
            let units = a.clone().strip_cost();
            match lotmap.get_mut(&k) {
                Some(old) => *old = &units + old,
                None => {
                    lotmap.insert(k.clone(), units);
                }
            }
            let basis_cost = a.cost_basis.as_ref().and_then(|cb| cb.cost.as_deref());
            let flow = match (&a.cost, basis_cost) {
                (Some(_), _) => Some(a.to_cost()),
                (None, Some(c)) => Some(c.clone().multiply(&a.quantity)),
                (None, None) => None,
            };
            if let Some(f) = flow {
                flows
                    .entry(k.clone())
                    .or_default()
                    .insert(0, (r.date(), -f));
            }
            if disposal && a.cost.is_some() && basis_cost.is_some() {
                let (basis, transacted) = (a.to_cost_basis(), a.to_cost());
                if basis.commodity == transacted.commodity {
                    let g = &basis - &transacted;
                    match rgains.get_mut(&k) {
                        Some(old) => *old = &g + old,
                        None => {
                            rgains.insert(k, g);
                        }
                    }
                }
            }
        }
    }
    for g in rgains.values_mut() {
        *g = g.clone().styled(&styles);
    }
    let pool_avg: BTreeMap<(AccountName, Sym), Amount> = journal_average_pool_totals(j, end)
        .into_iter()
        .map(|(k, (units, cost))| (k, avg_cost(&units, &cost).styled(&styles)))
        .collect();
    let parse = |s: &str| crate::read::amount::parse_amount(s).ok();
    let lot_bases: BTreeMap<(AccountName, Sym), Lot> = lotmap
        .iter()
        .filter(|(_, a)| !a.looks_zero())
        .map(|((sub, c), a)| {
            let cb = lot_subaccount_name(sub)
                .and_then(|name| parse_lot_name(parse, name).ok())
                .map(|cb| {
                    let cost = match cb.cost {
                        Some(c) => Some(Box::new(c.styled(&styles))),
                        None => pool_avg
                            .get(&(
                                AccountName::from(lot_base_account(sub)),
                                a.commodity.clone(),
                            ))
                            .map(|c| Box::new(c.clone())),
                    };
                    CostBasis { cost, ..cb }
                });
            ((sub.clone(), c.clone()), (a.clone(), cb))
        })
        .collect();
    let (valuation_date, target) = match &ropts.value {
        None => (report_date, None),
        Some(ValuationType::AtEnd(mc) | ValuationType::AtThen(mc)) => (report_date, mc.clone()),
        Some(ValuationType::AtNow(mc)) => (spec.today, mc.clone()),
        Some(ValuationType::AtDate(d, mc)) => (*d, mc.clone()),
    };
    let mut flat = Flat {
        report_date,
        styles,
        oracle: PriceOracle::new(ropts.infer_prices, j),
        valuing: ropts.value.is_some(),
        valuation_date,
        target,
        lot_keys: lotmap.keys().cloned().collect(),
        under_lots: under_ix(&lot_bases),
        under_flows: under_ix(&flows),
        under_rgains: under_ix(&rgains),
        own_lots: BTreeMap::new(),
        own_flows: BTreeMap::new(),
        own_rgains: BTreeMap::new(),
        flows,
        rgains,
        error: RefCell::new(None),
        portfolio_value: None,
    };
    // the rows of a historical, single-period balance report without
    // valuation, on the journal with lot detail with --lots
    let mut bspec = spec.clone();
    bspec.opts.balance_accum = BalanceAccumulation::Historical;
    bspec.opts.interval = Interval::NoInterval;
    bspec.opts.conversion_op = Some(ConversionOp::NoConversionOp);
    bspec.opts.value = None;
    bspec.opts.sort_amount = false;
    let mbr = if show_lots {
        multi_balance_report(&bspec, j)?
    } else {
        let mut collapsed = j.clone();
        journal_collapse_lot_detail(&mut collapsed);
        multi_balance_report(&bspec, &collapsed)?
    };
    let empty = ropts.empty;
    let has_rgains =
        |ix: &Ix<Amount>, a: &str| in_commodity(ix, a, None).iter().any(|g| !g.looks_zero());
    let candidates: Vec<crate::reports::periodic::DisplayName> = mbr
        .rows
        .into_iter()
        .map(|r| r.name)
        .filter(|n| {
            !flat.lots_under(&n.full).is_empty()
                || (empty && has_rgains(&flat.under_rgains, &n.full))
        })
        .collect();
    let cands: BTreeSet<AccountName> = candidates.iter().map(|n| n.full.clone()).collect();
    flat.own_lots = own_ix(&lot_bases, &cands);
    flat.own_flows = own_ix(&flat.flows, &cands);
    flat.own_rgains = own_ix(&flat.rgains, &cands);
    let rows: Vec<crate::reports::periodic::DisplayName> = candidates
        .into_iter()
        .filter(|n| {
            !flat.lots_of(&n.full).is_empty() || (empty && has_rgains(&flat.own_rgains, &n.full))
        })
        .collect();
    let row_set: BTreeSet<&str> = rows.iter().map(|n| &*n.full).collect();
    let top_rows: Vec<AccountName> = rows
        .iter()
        .filter(|n| !parent_account_names(&n.full).any(|p| row_set.contains(p)))
        .map(|n| n.full.clone())
        .collect();
    flat.portfolio_value = top_rows
        .iter()
        .map(|a| flat.value_under(a))
        .collect::<Option<Vec<Vec<Amount>>>>()
        .map(|vs| sum_amounts(vs.concat()));
    // -S sorts by each row's value (or cost), along its displayed parents
    let rows = if ropts.sort_amount {
        let key: BTreeMap<&str, std::cmp::Reverse<Decimal>> = rows
            .iter()
            .map(|n| {
                let k = match flat.value_of(&n.full) {
                    Some(val) => sum_quantities(&val),
                    None => {
                        let lots = flat.lots_of(&n.full);
                        sum_quantities(&flat.lot_costs(&lots, |a| flat.row_cost_valued(&n.full, a)))
                    }
                };
                (&*n.full, std::cmp::Reverse(k))
            })
            .collect();
        let path = |a: &str| -> Vec<std::cmp::Reverse<Decimal>> {
            let mut chain: Vec<&str> = parent_account_names(a).collect();
            chain.reverse();
            chain.push(a);
            chain
                .into_iter()
                .filter_map(|x| key.get(x).cloned())
                .collect()
        };
        let mut sorted = rows.clone();
        sorted.sort_by_key(|n| path(&n.full));
        sorted
    } else {
        rows
    };
    let mut lines = Vec::new();
    let mut holdings = Vec::new();
    for n in &rows {
        for h in flat.row_holdings(&n.full, empty) {
            lines.push(HLine::Position {
                indent: n.indent,
                account: n.full.clone(),
                display: n.name.to_string(),
                is_lot: lot_subaccount_name(&n.full).is_some(),
                holding: h.clone(),
            });
            holdings.push(h);
        }
    }
    let averaged: Vec<bool> = flat
        .lot_keys
        .iter()
        .map(|(sub, c)| {
            let m = resolve_reduction_method(j, lot_base_account(sub), c).0;
            matches!(m, ReductionMethod::Average | ReductionMethod::AverageAll)
        })
        .collect();
    let unit_cost_heading = if !show_lots || (!averaged.is_empty() && averaged.iter().all(|x| *x)) {
        "Avg cost"
    } else if averaged.iter().any(|x| *x) {
        "Unit/Avg cost"
    } else {
        "Unit cost"
    };
    let totals = grand_totals(&flat, &top_rows);
    let mut styles = flat.styles.clone();
    styles.entry(Sym::from("%")).or_insert_with(|| {
        Arc::new(AmountStyle {
            side: Side::R,
            precision: Precision::Fixed(1),
            rounding: Rounding::Hard,
            ..AmountStyle::default()
        })
    });
    if let Some(e) = flat.error.borrow_mut().take() {
        return Err(e);
    }
    let report = Report {
        date: report_date,
        lines,
        rows: holdings,
        totals,
        unit_cost_heading,
        styles,
    };
    Ok((report, flat))
}

/// The grand totals, summed over the topmost displayed rows; the realised
/// gains and XIRR over all lots in the query (totals).
fn grand_totals(flat: &Flat, top_rows: &[AccountName]) -> Totals {
    let pvc = Flat::value_commodity(&flat.portfolio_value);
    let lots: Vec<(&AccountName, &Lot)> = top_rows
        .iter()
        .flat_map(|a| flat.lots_under(a).into_iter().map(move |l| (a, l)))
        .collect();
    let units: Vec<Amount> = sum_amounts(lots.iter().map(|(_, (a, _))| a.clone()).collect())
        .into_iter()
        .filter(|a| !a.looks_zero())
        .collect();
    let costs: Vec<Amount> = lots
        .iter()
        .filter_map(|(r, (a, cb))| {
            let c = cb.as_ref()?.cost.as_deref()?;
            Some(flat.row_cost_valued(r, c.clone().multiply(&a.quantity)))
        })
        .collect();
    let gains = flat
        .portfolio_value
        .as_ref()
        .and_then(|v| flat.gain_amounts(v.clone(), costs.clone()));
    let rgains: Vec<Amount> = flat
        .rgains
        .values()
        .map(|g| flat.cost_valued(pvc.as_ref(), g.clone()))
        .collect();
    let all_flows: Vec<(Day, Amount)> = flat.flows.values().flatten().cloned().collect();
    Totals {
        units: match units.as_slice() {
            [u] => Some(u.clone()),
            _ => None,
        },
        avg_cost: match (units.as_slice(), sum_amounts(costs.clone()).as_slice()) {
            ([u], [c]) => Some(avg_cost(&u.quantity, c)),
            _ => None,
        },
        cost: MixedAmount::from_amounts(costs.clone()).amounts(),
        value: flat.portfolio_value.clone(),
        weight: flat
            .portfolio_value
            .as_ref()
            .and_then(|v| flat.weight_pct(v)),
        ugain_pct: Flat::gain_pct(gains.as_deref().unwrap_or(&[]), costs),
        ugain: gains,
        rgain: if rgains.is_empty() {
            Vec::new()
        } else {
            MixedAmount::from_amounts(rgains).amounts()
        },
        xirr: match flat.portfolio_value.as_deref() {
            Some([tv]) => flat.xirr_of(&all_flows, tv),
            _ => None,
        },
    }
}

/// The holdings report (holdingsReport): flat, or in tree mode the flat
/// report's lines grouped into account sections, with --lots each
/// position's lots beneath it.
fn holdings_report(
    show_lots: bool,
    rounding: Rounding,
    spec: &ReportSpec,
    j: &Journal,
) -> Result<Report, String> {
    if spec.opts.account_list_mode != AccountListMode::Tree {
        return Ok(flat_report(show_lots, rounding, spec, j)?.0);
    }
    let mut flat_spec = spec.clone();
    flat_spec.opts.account_list_mode = AccountListMode::Flat;
    let (prpt, pflat) = flat_report(false, rounding, &flat_spec, j)?;
    let lots_report = if show_lots {
        Some(flat_report(true, rounding, &flat_spec, j)?.0)
    } else {
        None
    };
    let lines = tree_lines(
        spec.opts.drop,
        spec.opts.no_elide,
        &prpt,
        &pflat,
        lots_report.as_ref(),
    );
    let (rows, unit_cost_heading) = match lots_report {
        Some(l) => (l.rows, l.unit_cost_heading),
        None => (prpt.rows.clone(), prpt.unit_cost_heading),
    };
    Ok(Report {
        lines,
        rows,
        unit_cost_heading,
        ..prpt
    })
}

/// The flat report's lines grouped into account sections (holdingsTreeLines):
/// an account with subaccounts or several commodities is a heading with its
/// subtree's totals, its own positions beneath it named by commodity, then
/// its subaccounts; one holding one commodity is a single line. Boring
/// parents are squashed into their child unless `no_elide`; siblings keep
/// the flat order; --drop removes the first levels.
fn tree_lines(
    dropn: usize,
    no_elide: bool,
    prpt: &Report,
    pflat: &Flat,
    lots: Option<&Report>,
) -> Vec<HLine> {
    let mut by_acct: BTreeMap<&AccountName, Vec<&Holding>> = BTreeMap::new();
    for h in &prpt.rows {
        by_acct.entry(&h.account).or_default().push(h);
    }
    let mut lots_by_pos: BTreeMap<(AccountName, Sym), Vec<&Holding>> = BTreeMap::new();
    for h in lots.iter().flat_map(|l| &l.rows) {
        if lot_subaccount_name(&h.account).is_some() {
            let k = (
                AccountName::from(lot_base_account(&h.account)),
                h.commodity.clone(),
            );
            lots_by_pos.entry(k).or_default().push(h);
        }
    }
    let mut first_ix: BTreeMap<&str, usize> = BTreeMap::new();
    for (ix, h) in prpt.rows.iter().enumerate() {
        for a in expand_account_name(&h.account) {
            first_ix
                .entry(a)
                .and_modify(|i| *i = (*i).min(ix))
                .or_insert(ix);
        }
    }
    let mut kids: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for a in first_ix.keys().filter(|a| account_name_level(a) > 1) {
        kids.entry(parent_account_name(a)).or_default().insert(0, a);
    }
    for ks in kids.values_mut() {
        ks.sort_by_key(|k| first_ix.get(k));
    }
    let kids_of = |a: &str| kids.get(a).cloned().unwrap_or_default();
    let lot_lines = |d: usize, acct: &str, h: &Holding| -> Vec<HLine> {
        lots_by_pos
            .get(&(AccountName::from(acct), h.commodity.clone()))
            .into_iter()
            .flatten()
            .map(|lh| HLine::Position {
                indent: d,
                account: lh.account.clone(),
                display: lot_subaccount_name(&lh.account)
                    .unwrap_or(&lh.account)
                    .to_string(),
                is_lot: true,
                holding: (*lh).clone(),
            })
            .collect()
    };
    fn node<'r>(
        d: usize,
        acct0: &'r str,
        no_elide: bool,
        by_acct: &BTreeMap<&AccountName, Vec<&Holding>>,
        kids_of: &dyn Fn(&str) -> Vec<&'r str>,
        lot_lines: &dyn Fn(usize, &str, &Holding) -> Vec<HLine>,
        totals: &dyn Fn(&str) -> Totals,
    ) -> Vec<HLine> {
        let (mut display, mut acct) = (account_leaf_name(acct0).to_string(), acct0);
        loop {
            match kids_of(acct).as_slice() {
                [c] if !no_elide && !by_acct.contains_key(&AccountName::from(acct)) => {
                    display = format!("{display}:{}", account_leaf_name(c));
                    acct = c;
                }
                _ => break,
            }
        }
        let own: Vec<&Holding> = by_acct
            .get(&AccountName::from(acct))
            .cloned()
            .unwrap_or_default();
        let kids = kids_of(acct);
        if let ([h], []) = (own.as_slice(), kids.as_slice()) {
            let mut out = vec![HLine::Position {
                indent: d,
                account: AccountName::from(acct),
                display,
                is_lot: false,
                holding: (*h).clone(),
            }];
            out.extend(lot_lines(d + 1, acct, h));
            return out;
        }
        let mut out = vec![HLine::Heading {
            indent: d,
            account: AccountName::from(acct),
            display,
            totals: totals(acct),
        }];
        for h in own {
            out.push(HLine::Position {
                indent: d + 1,
                account: AccountName::from(acct),
                display: h.commodity.to_string(),
                is_lot: false,
                holding: h.clone(),
            });
            out.extend(lot_lines(d + 2, acct, h));
        }
        for k in kids {
            out.extend(node(
                d + 1,
                k,
                no_elide,
                by_acct,
                kids_of,
                lot_lines,
                totals,
            ));
        }
        out
    }
    let totals = |a: &str| pflat.account_totals(a);
    let mut tops: Vec<(&str, bool)> = first_ix
        .keys()
        .filter(|a| account_name_level(a) == dropn + 1)
        .map(|a| (*a, false))
        .collect();
    if dropn > 0 {
        tops.extend(
            by_acct
                .keys()
                .filter(|a| account_name_level(a) <= dropn)
                .map(|a| (&***a, true)),
        );
    }
    tops.sort_by_key(|(a, _)| first_ix.get(a));
    let mut out = Vec::new();
    for (a, shallow) in tops {
        if shallow {
            for h in by_acct.get(&AccountName::from(a)).into_iter().flatten() {
                out.push(HLine::Position {
                    indent: 0,
                    account: AccountName::from(a),
                    display: account_name_drop(dropn, a),
                    is_lot: false,
                    holding: (*h).clone(),
                });
                out.extend(lot_lines(1, a, h));
            }
        } else {
            out.extend(node(
                0, a, no_elide, &by_acct, &kids_of, &lot_lines, &totals,
            ));
        }
    }
    out
}

/// Show an age compactly: in days, or from a year, in years with one
/// decimal (showage).
fn show_age(d: i64) -> String {
    if d >= 365 {
        format!(
            "{}y",
            (&Decimal::from_i64(d) / &Decimal::from_i64(365)).round_to(1)
        )
    } else {
        format!("{d}d")
    }
}

/// A percentage as a % amount with these styles' style for % (showPercentWith).
fn show_percent(styles: &CommodityStyles, p: &Decimal) -> String {
    let a = Amount {
        commodity: Sym::from("%"),
        quantity: p.clone(),
        ..Amount::null()
    }
    .styled(styles);
    show_amount_b(&ZFMT, &a).s
}

/// No costs (noCostFmt).
const FMT: AmountFormat = AmountFormat::NO_COST;
/// No costs, zeros with their commodity.
const ZFMT: AmountFormat = AmountFormat {
    zero_commodity: true,
    ..AmountFormat::NO_COST
};
/// Machine-readable: one line, no costs or digit group marks (machineFmt).
const MFMT: AmountFormat = AmountFormat::MACHINE;

fn show(fmt: &AmountFormat, a: &Amount) -> String {
    show_amount_b(fmt, a).s
}

/// Amounts summed, machine-readable.
fn show_machine(amts: &[Amount]) -> String {
    show_mixed_amount_b(&MFMT, &MixedAmount::from_amounts(amts.iter().cloned())).s
}

/// A holding's cells, each a list of parts (holdingCellParts).
fn holding_cell_parts(styles: &CommodityStyles, h: &Holding) -> Vec<Vec<String>> {
    let pct = |p: &Option<Decimal>| {
        p.as_ref()
            .map_or(String::new(), |p| show_percent(styles, p))
    };
    vec![
        vec![h.date.map_or(String::new(), |d| d.to_string())],
        vec![h.age.map_or(String::new(), show_age)],
        vec![show(&ZFMT, &h.units)],
        vec![
            h.unit_cost
                .as_ref()
                .map_or(String::new(), |a| show(&FMT, a)),
        ],
        h.price.iter().map(|a| show(&FMT, a)).collect(),
        h.cost.iter().map(|a| show(&FMT, a)).collect(),
        h.value.iter().map(|a| show(&ZFMT, a)).collect(),
        vec![pct(&h.weight)],
        h.ugain.iter().map(|a| show(&ZFMT, a)).collect(),
        vec![pct(&h.ugain_pct)],
        h.rgain.iter().map(|a| show(&FMT, a)).collect(),
        vec![pct(&h.xirr.map(Decimal::from_f64))],
    ]
}

/// A totals row's cells, each a list of parts (totalsCellParts).
fn totals_cell_parts(styles: &CommodityStyles, t: &Totals) -> Vec<Vec<String>> {
    let pct = |p: &Option<Decimal>| {
        p.as_ref()
            .map_or(String::new(), |p| show_percent(styles, p))
    };
    vec![
        vec![],
        vec![],
        t.units.iter().map(|a| show(&FMT, a)).collect(),
        t.avg_cost.iter().map(|a| show(&FMT, a)).collect(),
        vec![],
        t.cost.iter().map(|a| show(&FMT, a)).collect(),
        t.value.iter().flatten().map(|a| show(&FMT, a)).collect(),
        vec![pct(&t.weight)],
        t.ugain.iter().flatten().map(|a| show(&ZFMT, a)).collect(),
        vec![pct(&t.ugain_pct)],
        t.rgain.iter().map(|a| show(&FMT, a)).collect(),
        vec![pct(&t.xirr.map(Decimal::from_f64))],
    ]
}

/// The machine-readable fields of a holding (holdingCsv).
fn holding_csv(h: &Holding) -> Vec<String> {
    let opt_machine = |amts: &[Amount]| {
        if amts.is_empty() {
            String::new()
        } else {
            show_machine(amts)
        }
    };
    let units_fmt = AmountFormat {
        commodity: false,
        ..MFMT
    };
    vec![
        h.account.to_string(),
        h.commodity.to_string(),
        h.date.map_or(String::new(), |d| d.to_string()),
        h.age.map_or(String::new(), |a| a.to_string()),
        show(&units_fmt, &h.units),
        h.unit_cost
            .as_ref()
            .map_or(String::new(), |a| show(&MFMT, a)),
        opt_machine(&h.price),
        show_machine(&h.cost),
        opt_machine(&h.value),
        h.weight
            .as_ref()
            .map_or(String::new(), |w| w.round_to(1).to_string()),
        opt_machine(&h.ugain),
        h.ugain_pct
            .as_ref()
            .map_or(String::new(), |p| p.round_to(1).to_string()),
        opt_machine(&h.rgain),
        h.xirr.map_or(String::new(), |x| show_ffloat(1, x)),
    ]
}

/// A holding as JSON (holdingJson).
fn holding_json(h: &Holding) -> Value {
    use crate::write::json::ToJson;
    let opt = |amts: &[Amount]| {
        if amts.is_empty() {
            Value::Null
        } else {
            Value::String(show_machine(amts))
        }
    };
    let dec = |d: &Option<Decimal>| d.as_ref().map_or(Value::Null, |d| d.round_to(1).to_json());
    json!({
        "account": &*h.account,
        "commodity": &*h.commodity,
        "date": h.date.map(|d| d.to_string()),
        "age": h.age,
        "units": h.units.quantity.to_json(),
        "unitcost": h.unit_cost.as_ref().map(|a| show(&MFMT, a)),
        "unitprice": opt(&h.price),
        "totalcost": show_machine(&h.cost),
        "totalvalue": opt(&h.value),
        "weight": dec(&h.weight),
        "ugain": opt(&h.ugain),
        "ugainpct": dec(&h.ugain_pct),
        "rgain": opt(&h.rgain),
        "xirr": h.xirr.map_or(Value::Null, double),
    })
}

struct JsonValue(Value);

impl crate::write::json::ToJson for JsonValue {
    fn to_json(&self) -> Value {
        self.0.clone()
    }
}

/// The column headings (the cost one as the report says).
fn col_headings(t: &Translations, rpt: &Report) -> Vec<String> {
    [
        "Date",
        "Age",
        "Units",
        rpt.unit_cost_heading,
        "Unit price",
        "Total cost",
        "Total value",
        "Weight",
        "UGain",
        "UGain%",
        "RGain",
        "XIRR",
    ]
    .iter()
    .map(|s| t.tr(s).to_string())
    .collect()
}

/// The report's per-column classes (spreadsheetWith's colclasses).
const COL_CLASSES: [&str; 13] = [
    "account",
    "date",
    "age",
    "units",
    "unitcost",
    "unitprice",
    "totalcost",
    "totalvalue",
    "weight",
    "ugain",
    "ugainpct",
    "rgain",
    "xirr",
];

/// Which of the data columns' parts are amounts.
const AMOUNT_COLS: [bool; 12] = [
    false, false, true, true, true, true, true, false, true, false, true, false,
];

/// A body cell of the html or fods table: text in the first two columns,
/// the others right-aligned, an amount column's parts each an amount.
fn body_cell(i: usize, class: &str, parts: &[String], amount: bool) -> spr::Cell<String> {
    let nonempty: Vec<String> = parts.iter().filter(|p| !p.is_empty()).cloned().collect();
    let mut c = spr::Cell::new(nonempty.join(", ")).with_class(class);
    if i >= 2 {
        c.typ = CellType::MixedAmount;
        if amount {
            c.parts = nonempty;
        }
    }
    c
}

/// A heading cell with its class.
fn head_cell(t: &str, class: &str) -> spr::Cell<String> {
    spr::Cell::header(t).with_class(class)
}

/// The text of a table rendered without outer borders, with column
/// headers, a row header column, and an optional totals row below a line.
fn render_text(
    row_heads: Vec<String>,
    col_heads: Vec<String>,
    rows: Vec<Vec<String>>,
    total: Option<Vec<String>>,
) -> String {
    let table = Table {
        row_headers: Header::Group(
            Line::NoLine,
            row_heads.into_iter().map(Header::Leaf).collect(),
        ),
        col_headers: Header::Group(
            Line::NoLine,
            col_heads.into_iter().map(Header::Leaf).collect(),
        ),
        rows,
    };
    let table = match total {
        Some(t) => concat_tables(
            Line::SingleLine,
            table,
            Table {
                row_headers: Header::Group(Line::NoLine, vec![Header::Leaf(String::new())]),
                col_headers: Header::<String>::Group(Line::NoLine, vec![]),
                rows: vec![t],
            },
        ),
        None => table,
    };
    let opts = TableOpts {
        borders: false,
        ..TableOpts::default()
    };
    render_table(
        &opts,
        |h: &String| Cell::text(Align::TopLeft, h),
        |h: &String| Cell::text(Align::TopRight, h),
        |c: &String| Cell::text(Align::TopRight, c),
        &table,
    )
}

/// A chart's bar parts for a holding: its value and cost if priced (its
/// value twice if not comparable with its cost), else its cost
/// (holdingBarParts).
fn holding_bar_parts(h: &Holding) -> (Vec<Amount>, Vec<Amount>, Vec<Amount>) {
    if h.value.is_empty() {
        (Vec::new(), Vec::new(), h.cost.clone())
    } else if !h.ugain.is_empty() {
        (h.value.clone(), h.cost.clone(), Vec::new())
    } else {
        (h.value.clone(), h.value.clone(), Vec::new())
    }
}

/// A chart of holdings: at each x position, in each commodity's panel, a
/// bar of the cost basis with the unrealised gain or loss reaching the
/// value, or the cost basis without a market price; the values marked,
/// and the gain percents as value labels (holdingsBarsChart).
fn holdings_bars_chart(
    t: &Translations,
    styles: &CommodityStyles,
    title: &str,
    x_axis: XAxis,
    parts: &[(Vec<Amount>, Vec<Amount>, Vec<Amount>)],
) -> Chart {
    let amts: Vec<&Amount> = parts
        .iter()
        .flat_map(|(vs, ks, us)| vs.iter().chain(ks).chain(us))
        .collect();
    let mut counts: BTreeMap<&Sym, usize> = BTreeMap::new();
    for a in &amts {
        *counts.entry(&a.commodity).or_default() += 1;
    }
    let mut commodities: Vec<(&Sym, usize)> = counts.into_iter().collect();
    commodities.sort_by(|(c1, n1), (c2, n2)| n2.cmp(n1).then(c1.cmp(c2)));
    let specs = [
        (
            "Cost",
            Some(AccountType::Asset),
            SeriesRole::Item,
            Mark::Bar,
        ),
        (
            "Unrealised gain",
            Some(AccountType::Revenue),
            SeriesRole::Item,
            Mark::Bar,
        ),
        (
            "Unrealised loss",
            Some(AccountType::Expense),
            SeriesRole::Shortfall,
            Mark::Bar,
        ),
        ("No market price", None, SeriesRole::Other, Mark::Bar),
        ("Value", None, SeriesRole::Total, Mark::Marker),
    ];
    let panel = |c: &Sym| {
        let quantity_of = |as_: &[Amount]| {
            let qs: Vec<&Amount> = as_.iter().filter(|a| &a.commodity == c).collect();
            (!qs.is_empty()).then(|| sum_quantities(qs))
        };
        let nonzero = |ok: bool, q: Decimal| (ok && !q.is_zero()).then_some(q);
        let bar_values =
            |(vs, ks, us): &(Vec<Amount>, Vec<Amount>, Vec<Amount>)| -> Vec<Option<Decimal>> {
                match (quantity_of(vs), quantity_of(ks)) {
                    (Some(v), Some(k)) => {
                        let (base, end) = if v.abs() <= k.abs() {
                            (v.clone(), k.clone())
                        } else {
                            (k.clone(), v.clone())
                        };
                        let ugain = &v - &k;
                        vec![
                            Some(base.clone()),
                            nonzero(ugain > Decimal::ZERO, &end - &base),
                            nonzero(ugain < Decimal::ZERO, &end - &base),
                            quantity_of(us),
                            Some(v),
                        ]
                    }
                    _ => vec![None, None, None, quantity_of(us), None],
                }
            };
        let rows: Vec<Vec<Option<Decimal>>> = parts.iter().map(bar_values).collect();
        let series = if rows.is_empty() {
            Vec::new()
        } else {
            specs
                .iter()
                .enumerate()
                .map(|(i, spec)| (spec, rows.iter().map(|r| r[i].clone()).collect::<Vec<_>>()))
                .filter(|((_, _, role, _), vals)| {
                    vals.iter().any(Option::is_some) || *role == SeriesRole::Total
                })
                .map(|((name, typ, role, mark), values)| Series {
                    label: ChartLabel {
                        full: t.tr(name).to_string(),
                        short: t.tr(name).to_string(),
                        indent: 0,
                        typ: *typ,
                        account: false,
                    },
                    role: *role,
                    mark: *mark,
                    values,
                    group: 0,
                    sign: 0,
                })
                .collect()
        };
        let gain_percent = |(vs, ks, _): &(Vec<Amount>, Vec<Amount>, Vec<Amount>)| match (
            quantity_of(vs),
            quantity_of(ks),
        ) {
            (Some(v), Some(k)) if !k.is_zero() => {
                let p = &(&(&v - &k) / &k.abs()) * &hundred();
                let sign = if p > Decimal::ZERO { "+" } else { "" };
                Some(format!("{sign}{}", show_percent(styles, &p)))
            }
            _ => None,
        };
        let unit = amts
            .iter()
            .find(|a| &a.commodity == c)
            .map(|a| Amount {
                quantity: Decimal::ZERO,
                cost: None,
                cost_basis: None,
                ..(*a).clone()
            })
            .unwrap_or_else(|| Amount {
                commodity: c.clone(),
                ..Amount::null()
            });
        Panel {
            unit,
            series,
            group_names: Vec::new(),
            scale: None,
            layout: BarLayout::Diverging,
            counts: false,
            summary: Vec::new(),
            value_labels: parts.iter().map(gain_percent).collect(),
        }
    };
    Chart {
        title: title.to_string(),
        x_axis,
        panels: commodities.iter().map(|(c, _)| panel(c)).collect(),
    }
}

/// Run the holdings command (holdings).
pub fn holdings(opts: &CliOpts, j: &Journal) -> Result<String, String> {
    warn_if_large_multi_period_report(opts, j);
    let ropts = &opts.spec.opts;
    if matches!(ropts.value, Some(ValuationType::AtThen(_))) {
        return Err("holdings: --value=then is not supported".to_string());
    }
    let rounding = round_from_raw_opts(opts)?.unwrap_or(Rounding::Hard);
    let fmt = opts.output_format();
    if ropts.interval != Interval::NoInterval {
        return periodic_holdings(opts, j, rounding);
    }
    let show_lots = opts.raw.bool("lots");
    let rpt = holdings_report(show_lots, rounding, &opts.spec, j)?;
    let title = ropts
        .effective_title(
            &ropts
                .translations
                .trf("Holdings on {date}", &[("date", &rpt.date.to_string())]),
        )
        .to_string();
    let t = &ropts.translations;
    let col_heads = col_headings(t, &rpt);
    let line_cells = |l: &HLine| match l {
        HLine::Heading { totals, .. } => totals_cell_parts(&rpt.styles, totals),
        HLine::Position { holding, .. } => holding_cell_parts(&rpt.styles, holding),
    };
    let total_parts = (!ropts.no_total).then(|| totals_cell_parts(&rpt.styles, &rpt.totals));
    let html = || {
        let mut rows = vec![add_header_borders(
            std::iter::once(t.tr("Account"))
                .chain(col_heads.iter().map(String::as_str))
                .zip(COL_CLASSES)
                .map(|(t, cls)| head_cell(t, cls))
                .collect(),
        )];
        for l in &rpt.lines {
            let (indent, display, heading) = match l {
                HLine::Heading {
                    indent, display, ..
                } => (*indent, display, true),
                HLine::Position {
                    indent, display, ..
                } => (*indent, display, false),
            };
            let account_class = if heading {
                "account acctheading"
            } else {
                "account"
            };
            let mut row = vec![body_cell(
                0,
                account_class,
                &["\u{a0}".repeat(indent * 2) + display],
                false,
            )];
            for (i, parts) in line_cells(l).iter().enumerate() {
                row.push(body_cell(i + 1, COL_CLASSES[i + 1], parts, AMOUNT_COLS[i]));
            }
            rows.push(row);
        }
        if let Some(tot) = &total_parts {
            let mut row = vec![body_cell(
                0,
                "account coltotal",
                &[t.tr("Total:").to_string()],
                false,
            )];
            for (i, parts) in tot.iter().enumerate() {
                let class = format!("{} coltotal", COL_CLASSES[i + 1]);
                row.push(body_cell(i + 1, &class, parts, AMOUNT_COLS[i]));
            }
            rows.extend(crate::cli::commands::balance::add_total_borders(vec![row]));
        }
        rows
    };
    if chart_requested(opts) {
        check_chart_format(opts)?;
        let positions: Vec<(&AccountName, bool, &Holding)> = rpt
            .lines
            .iter()
            .filter_map(|l| match l {
                HLine::Position {
                    account,
                    is_lot,
                    holding,
                    ..
                } => Some((account, *is_lot, holding)),
                HLine::Heading { .. } => None,
            })
            .collect();
        let name = |a: &str, is_lot: bool, h: &Holding| {
            if is_lot {
                format!(
                    "{} {} {}",
                    parent_account_name(a),
                    h.commodity,
                    account_leaf_name(a)
                )
            } else {
                format!("{a} {}", h.commodity)
            }
        };
        let labels: Vec<ChartLabel> = positions
            .iter()
            .map(|(a, lot, h)| ChartLabel {
                full: name(a, *lot, h),
                short: name(&account_name_drop(ropts.drop, a), *lot, h),
                indent: 0,
                typ: None,
                account: true,
            })
            .collect();
        let parts: Vec<_> = positions
            .iter()
            .map(|(_, _, h)| holding_bar_parts(h))
            .collect();
        let chart = holdings_bars_chart(t, &rpt.styles, &title, XAxis::Categories(labels), &parts);
        return render_chart(opts, chart, || {
            Ok(crate::write::html::titled_table_html(&title, &html()) + "\n")
        });
    }
    let records: Vec<&Holding> = rpt.rows.iter().collect();
    Ok(match fmt.as_str() {
        "txt" => {
            let mut out = if title.is_empty() {
                String::new()
            } else {
                format!("{title}\n\n")
            };
            if rpt.rows.is_empty() {
                out.push_str(&format!("{}\n", ropts.translations.tr("(no holdings)")));
                return Ok(out);
            }
            let tree = ropts.account_list_mode == AccountListMode::Tree && !ropts.full_names;
            let row_heads = rpt
                .lines
                .iter()
                .map(|l| match l {
                    HLine::Heading {
                        indent, display, ..
                    } if tree => " ".repeat(indent * 2) + display,
                    HLine::Position {
                        indent, display, ..
                    } if tree => " ".repeat(indent * 2) + display,
                    HLine::Heading { account, .. } | HLine::Position { account, .. } => {
                        account_name_drop(ropts.drop, account)
                    }
                })
                .collect();
            let rows = rpt
                .lines
                .iter()
                .map(|l| line_cells(l).iter().map(|ps| ps.join(", ")).collect())
                .collect();
            let total = total_parts
                .as_ref()
                .map(|t| t.iter().map(|ps| ps.join(", ")).collect());
            out.push_str(&render_text(row_heads, col_heads.clone(), rows, total));
            out
        }
        "csv" | "tsv" => {
            let mut recs = vec![
                [
                    "account",
                    "commodity",
                    "date",
                    "age",
                    "units",
                    "unitcost",
                    "unitprice",
                    "totalcost",
                    "totalvalue",
                    "weight",
                    "ugain",
                    "ugainpct",
                    "rgain",
                    "xirr",
                ]
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<String>>(),
            ];
            recs.extend(records.iter().map(|h| holding_csv(h)));
            if fmt == "csv" {
                print_csv(&recs)
            } else {
                print_tsv(&recs)
            }
        }
        "html" => crate::write::html::titled_table_html(&title, &html()) + "\n",
        "fods" => {
            let mut sheets = BTreeMap::new();
            sheets.insert(
                ropts.translations.tr("Holdings").to_string(),
                ((1, 0), html()),
            );
            crate::write::fods::print_fods(crate::write::fods::locale_encoding_name(), &sheets)
        }
        // upstream adds a newline to toJsonText's
        "json" => {
            crate::write::json::to_json_text(&JsonValue(Value::Array(
                records.iter().map(|h| holding_json(h)).collect(),
            ))) + "\n"
        }
        f => return Err(unsupported_output_format_error(f)),
    })
}

/// With a report interval, the holdings totals at each period's end, a
/// row per period, as a table, csv/tsv/json records, or a chart.
fn periodic_holdings(opts: &CliOpts, j: &Journal, rounding: Rounding) -> Result<String, String> {
    let spec = &opts.spec;
    let ropts = &spec.opts;
    let periods = maybe_day_partition_to_date_spans(spec.report_span_both_dates(j).1.as_ref());
    let mut reports: Vec<(DateSpan, Report)> = Vec::new();
    for spn in periods {
        let Some(e) = spn.end() else { continue };
        let mut pspec = spec.clone();
        pspec.query = Query::And(vec![
            spec.query.clone().filter(|q| !q.is_date_or_date2()),
            Query::Date(DateSpan::exact(None, Some(e))),
        ]);
        pspec.opts.account_list_mode = AccountListMode::Flat;
        reports.push((spn, holdings_report(false, rounding, &pspec, j)?));
    }
    let dates: Vec<Day> = reports.iter().map(|(_, r)| r.date).collect();
    let default_title = match (dates.iter().min(), dates.iter().max()) {
        (Some(lo), Some(hi)) => ropts.translations.trf(
            "Holdings at period ends, {dates}",
            &[("dates", &format!("{lo}..{hi}"))],
        ),
        _ => ropts.translations.tr("Holdings at period ends").to_string(),
    };
    let title = ropts.effective_title(&default_title).to_string();
    let col_heads: Vec<String> = [
        "Total cost",
        "Total value",
        "UGain",
        "UGain%",
        "RGain",
        "XIRR",
    ]
    .iter()
    .map(|s| ropts.translations.tr(s).to_string())
    .collect();
    let period_cells = |r: &Report| -> Vec<Vec<String>> {
        totals_cell_parts(&r.styles, &r.totals)
            .into_iter()
            .enumerate()
            .filter(|(i, _)| [5, 6, 8, 9, 10, 11].contains(i))
            .map(|(_, ps)| ps)
            .collect()
    };
    let classes = [
        "date",
        "totalcost",
        "totalvalue",
        "ugain",
        "ugainpct",
        "rgain",
        "xirr",
    ];
    let amount_cols = [true, true, true, false, true, false];
    let html = || {
        let mut rows = vec![add_header_borders(
            std::iter::once(ropts.translations.tr("Date"))
                .chain(col_heads.iter().map(String::as_str))
                .zip(classes)
                .map(|(t, cls)| head_cell(t, cls))
                .collect(),
        )];
        for (_, r) in &reports {
            let mut date = spr::Cell::new(r.date.to_string()).with_class("date");
            date.typ = CellType::String;
            let mut row = vec![date];
            for (i, ps) in period_cells(r).iter().enumerate() {
                row.push(body_cell(2, classes[i + 1], ps, amount_cols[i]));
            }
            rows.push(row);
        }
        rows
    };
    if chart_requested(opts) {
        check_chart_format(opts)?;
        let styles = reports
            .first()
            .map(|(_, r)| r.styles.clone())
            .unwrap_or_default();
        let x = XAxis::Periods(
            reports
                .iter()
                .map(|(s, r)| (*s, r.date.to_string()))
                .collect(),
        );
        let parts: Vec<_> = reports
            .iter()
            .map(|(_, r)| {
                let mut acc = (Vec::new(), Vec::new(), Vec::new());
                for h in &r.rows {
                    let (v, k, u) = holding_bar_parts(h);
                    acc.0.extend(v);
                    acc.1.extend(k);
                    acc.2.extend(u);
                }
                acc
            })
            .collect();
        let chart = holdings_bars_chart(&ropts.translations, &styles, &title, x, &parts);
        return render_chart(opts, chart, || {
            Ok(crate::write::html::titled_table_html(&title, &html()) + "\n")
        });
    }
    let fmt = opts.output_format();
    let machine = |amts: &[Amount]| show_machine(amts);
    Ok(match fmt.as_str() {
        "txt" => {
            let mut out = if title.is_empty() {
                String::new()
            } else {
                format!("{title}\n\n")
            };
            if reports.is_empty() {
                out.push_str(&format!("{}\n", ropts.translations.tr("(no holdings)")));
                return Ok(out);
            }
            let row_heads = dates.iter().map(|d| d.to_string()).collect();
            let rows = reports
                .iter()
                .map(|(_, r)| period_cells(r).iter().map(|ps| ps.join(", ")).collect())
                .collect();
            out.push_str(&render_text(row_heads, col_heads.clone(), rows, None));
            out
        }
        "csv" | "tsv" => {
            let mut recs: Vec<Vec<String>> = vec![
                [
                    "date",
                    "totalcost",
                    "totalvalue",
                    "ugain",
                    "ugainpct",
                    "rgain",
                    "xirr",
                ]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            ];
            for (_, r) in &reports {
                let t = &r.totals;
                let opt = |a: &[Amount]| {
                    if a.is_empty() {
                        String::new()
                    } else {
                        machine(a)
                    }
                };
                recs.push(vec![
                    r.date.to_string(),
                    opt(&t.cost),
                    t.value.as_deref().map_or(String::new(), opt),
                    t.ugain.as_deref().map_or(String::new(), opt),
                    t.ugain_pct
                        .as_ref()
                        .map_or(String::new(), |p| p.round_to(1).to_string()),
                    opt(&t.rgain),
                    t.xirr.map_or(String::new(), |x| show_ffloat(1, x)),
                ]);
            }
            if fmt == "csv" {
                print_csv(&recs)
            } else {
                print_tsv(&recs)
            }
        }
        "html" => crate::write::html::titled_table_html(&title, &html()) + "\n",
        "fods" => {
            let mut sheets = BTreeMap::new();
            sheets.insert(
                ropts.translations.tr("Holdings").to_string(),
                ((1, 0), html()),
            );
            crate::write::fods::print_fods(crate::write::fods::locale_encoding_name(), &sheets)
        }
        "json" => {
            use crate::write::json::ToJson;
            let items = reports
                .iter()
                .map(|(_, r)| {
                    let t = &r.totals;
                    json!({
                        "date": r.date.to_string(),
                        "totalcost": machine(&t.cost),
                        "totalvalue": t.value.as_deref().map(machine),
                        "ugain": t.ugain.as_deref().map(machine),
                        "ugainpct": t.ugain_pct.as_ref().map_or(Value::Null, |p| p.round_to(1).to_json()),
                        "rgain": if t.rgain.is_empty() { Value::Null } else { Value::String(machine(&t.rgain)) },
                        "xirr": t.xirr.map_or(Value::Null, double),
                    })
                })
                .collect();
            crate::write::json::to_json_text(&JsonValue(Value::Array(items))) + "\n"
        }
        f => return Err(unsupported_output_format_error(f)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages_in_days_then_years() {
        assert_eq!(show_age(44), "44d");
        assert_eq!(show_age(365), "1.0y");
        assert_eq!(show_age(400), "1.1y");
    }

    #[test]
    fn average_costs_show_cents_when_inexact() {
        let cost = crate::read::amount::parse_amount("$850").unwrap();
        let avg = avg_cost(&Decimal::from_i64(15), &cost);
        assert_eq!(show_amount_b(&FMT, &avg).s, "$56.67");
        let avg = avg_cost(&Decimal::from_i64(17), &cost);
        assert_eq!(show_amount_b(&FMT, &avg).s, "$50");
    }
}
