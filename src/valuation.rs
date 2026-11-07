//! Valuation (Hledger.Data.Valuation): converting amounts to their cost,
//! or to their market value in another commodity on some date, using the
//! market prices declared by P directives, inferred from costs, or given
//! by commodity aliases.

use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::amount::{Amount, AmountCost, CommodityStyles, MixedAmount, Sym};
use crate::dates::{Day, null_date};
use crate::decimal::Decimal;
use crate::model::{CONVERSION_POSTING_TAG, Journal, MarketPrice, PriceDirective, SourcePos};
use crate::text::quote_if_needed;

/// Which cost to convert amounts to (ConversionOp).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConversionOp {
    NoConversionOp,
    /// To the cost basis where known, otherwise the transacted cost (`-B`,
    /// `--value=cost`).
    ToCost,
    /// To the transacted cost (`--value=transacted`).
    ToTransactedCost,
}

/// When to value amounts, and optionally in which commodity
/// (ValuationType).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValuationType {
    /// At each posting's date.
    AtThen(Option<Sym>),
    /// At the end of each report period.
    AtEnd(Option<Sym>),
    /// Today.
    AtNow(Option<Sym>),
    /// On this date.
    AtDate(Day, Option<Sym>),
}

impl ValuationType {
    /// The commodity to value in, if one was given
    /// (valuationTypeValuationCommodity).
    pub fn commodity(&self) -> Option<&Sym> {
        match self {
            ValuationType::AtThen(c)
            | ValuationType::AtEnd(c)
            | ValuationType::AtNow(c)
            | ValuationType::AtDate(_, c) => c.as_ref(),
        }
    }
}

/// The market price a price directive declares (priceDirectiveToMarketPrice).
pub fn price_directive_to_market_price(pd: &PriceDirective) -> MarketPrice {
    MarketPrice {
        date: pd.date,
        from: pd.commodity.clone(),
        to: pd.amount.commodity.clone(),
        rate: pd.amount.quantity.clone(),
    }
}

/// The price directive on this date implied by an amount's cost, if it has
/// one: its unit cost, or its total cost divided by its quantity, shown
/// with all its decimals or 8 if they are infinite
/// (amountPriceDirectiveFromCost).
pub fn amount_price_directive_from_cost(date: Day, a: &Amount) -> Option<PriceDirective> {
    let price = match &a.cost {
        Some(AmountCost::Unit(u)) => (**u).clone(),
        Some(AmountCost::Total(t)) if !a.quantity.is_zero() => (**t)
            .clone()
            .divide(&a.quantity)
            .with_full_precision_up_to(None),
        _ => return None,
    };
    Some(PriceDirective {
        pos: SourcePos::null(),
        date,
        commodity: a.commodity.clone(),
        amount: price,
    })
}

/// The same price in the other direction: the inverse rate, or zero for a
/// zero price (marketPriceReverse).
pub fn market_price_reverse(p: &MarketPrice) -> MarketPrice {
    MarketPrice {
        date: p.date,
        from: p.to.clone(),
        to: p.from.clone(),
        rate: if p.rate.is_zero() {
            Decimal::ZERO
        } else {
            &Decimal::from_i64(1) / &p.rate
        },
    }
}

/// The journal's commodity aliases as fixed exchange rates: price
/// directives dated 0000-01-01 saying that one unit of a commodity is
/// worth the alias's quantity (as written) of the alias, each at the
/// commodity directive declaring the alias (journalAliasPriceDirectives).
pub fn journal_alias_price_directives(j: &Journal) -> Vec<PriceDirective> {
    j.declared_commodities
        .values()
        .flat_map(|c| {
            c.aliases.iter().map(|(a, pos)| PriceDirective {
                pos: pos.clone(),
                date: null_date(),
                commodity: c.symbol.clone(),
                amount: a.clone(),
            })
        })
        .collect()
}

/// For each commodity pair, by date, the price in effect from that date:
/// on a day with several, the last declared one, or if none was declared,
/// the last inferred one (PriceIndex, buildPriceIndex).
type PriceIndex = BTreeMap<(Sym, Sym), BTreeMap<Day, MarketPrice>>;

/// For each commodity, by date, the commodity of its last price on that
/// date (DefaultValuationIndex, buildDefaultValuationIndex).
type DefaultValuationIndex = BTreeMap<Sym, BTreeMap<Day, Sym>>;

fn build_price_index(declared: &[MarketPrice], inferred: &[MarketPrice]) -> PriceIndex {
    let mut index = PriceIndex::new();
    for p in inferred.iter().chain(declared) {
        index
            .entry((p.from.clone(), p.to.clone()))
            .or_default()
            .insert(p.date, p.clone());
    }
    index
}

fn build_default_valuation_index(prices: &[MarketPrice]) -> DefaultValuationIndex {
    let mut index = DefaultValuationIndex::new();
    for p in prices {
        index
            .entry(p.from.clone())
            .or_default()
            .insert(p.date, p.to.clone());
    }
    index
}

/// Look up market prices (exchange rates) between commodities on any date
/// (PriceOracle, journalPriceOracle). Built once from a journal's declared
/// prices, optionally its prices inferred from costs, and its commodity
/// aliases' fixed rates. An alias rate is used only between two
/// commodities with no market price between them in either direction,
/// only when market prices alone can not make the conversion, and never to
/// choose a default valuation commodity; so an alias can be the valuation
/// commodity, and amounts left in an alias can be valued.
#[derive(Clone, Debug)]
pub struct PriceOracle {
    /// The declared prices, the inferred prices used, and the usable alias
    /// rates.
    declared: Vec<MarketPrice>,
    inferred: Vec<MarketPrice>,
    alias_prices: Vec<MarketPrice>,
    /// Their indexes, built when first needed, as upstream's lazy ones are.
    indexes: OnceCell<PriceIndexes>,
    /// The aliases which different file trees declared differently, with
    /// their declarations.
    ambiguous_aliases: BTreeMap<Sym, Vec<PriceDirective>>,
    /// At --debug=2 or more, if there are aliases: the usable alias rates,
    /// to show when they are tried.
    debug_alias_rates: Option<Vec<PriceDirective>>,
    /// The lookups which have tried the alias rates, so that each is shown
    /// once (upstream's oracle is memoised).
    alias_tries: RefCell<BTreeSet<(Day, Sym, Sym)>>,
    /// The results of the lookups made so far, as upstream memoises them.
    lookups: RefCell<HashMap<(Day, Sym, Option<Sym>), Lookup>>,
}

/// A price lookup's result: the commodity and rate, if found.
type Lookup = Result<Option<(Sym, Decimal)>, String>;

/// A price oracle's indexes (PriceIndexes).
#[derive(Clone, Debug)]
struct PriceIndexes {
    /// The declared and inferred prices (piForward).
    forward: PriceIndex,
    /// The same with the usable alias rates added, if there are any.
    forward_with_aliases: Option<PriceIndex>,
    /// Destinations of declared prices (piDeclaredDefault).
    declared_default: DefaultValuationIndex,
    /// Destinations of inferred prices (piInferredDefault).
    inferred_default: DefaultValuationIndex,
}

impl PriceOracle {
    /// The price oracle of a journal, using prices inferred from its costs
    /// if `infer` is true, showing debug output at this --debug level
    /// (journalPriceOracle).
    pub fn new(infer: bool, debug: usize, j: &Journal) -> PriceOracle {
        let declared: Vec<MarketPrice> = j
            .price_directives
            .iter()
            .map(price_directive_to_market_price)
            .collect();
        let inferred = if infer {
            j.inferred_market_prices.clone()
        } else {
            Vec::new()
        };
        // the aliases declared differently, as an alias of a different
        // commodity or with a different quantity, can not be used
        // (journalAliasPriceDirectivesChecked)
        let alias_pds = journal_alias_price_directives(j);
        let mut by_alias: BTreeMap<Sym, Vec<PriceDirective>> = BTreeMap::new();
        for pd in &alias_pds {
            by_alias
                .entry(pd.amount.commodity.clone())
                .or_default()
                .push(pd.clone());
        }
        let (ambiguous_aliases, usable): (BTreeMap<_, _>, BTreeMap<_, _>) =
            by_alias.into_iter().partition(|(_, pds)| pds.len() > 1);
        let alias_prices: Vec<MarketPrice> = usable
            .values()
            .flatten()
            .map(price_directive_to_market_price)
            .collect();
        let debug_alias_rates =
            (debug >= 2 && !(alias_prices.is_empty() && ambiguous_aliases.is_empty())).then(|| {
                (alias_pds.into_iter())
                    .filter(|pd| !ambiguous_aliases.contains_key(&pd.amount.commodity))
                    .collect()
            });
        PriceOracle {
            declared,
            inferred,
            alias_prices,
            indexes: OnceCell::new(),
            ambiguous_aliases,
            debug_alias_rates,
            alias_tries: RefCell::default(),
            lookups: RefCell::default(),
        }
    }

    /// The indexes of the prices, built the first time.
    fn indexes(&self) -> &PriceIndexes {
        self.indexes.get_or_init(|| {
            let forward = build_price_index(&self.declared, &self.inferred);
            let forward_with_aliases = (!self.alias_prices.is_empty())
                .then(|| with_alias_prices(&forward, &self.alias_prices));
            PriceIndexes {
                forward,
                forward_with_aliases,
                declared_default: build_default_valuation_index(&self.declared),
                inferred_default: build_default_valuation_index(&self.inferred),
            }
        })
    }

    /// On this date, the commodity to value `from` in (`to` if given,
    /// otherwise its default valuation commodity), and how much of it one
    /// unit of `from` is worth; None if no price can be found or
    /// calculated, or if the two commodities are the same. Converting to
    /// or from an alias which file trees declared differently, when market
    /// prices can not do it, is an error.
    pub fn lookup(&self, d: Day, from: &Sym, to: Option<&Sym>) -> Lookup {
        let key = (d, from.clone(), to.cloned());
        if let Some(found) = self.lookups.borrow().get(&key) {
            return found.clone();
        }
        let found = self.lookup_once(d, from, to);
        self.lookups.borrow_mut().insert(key, found.clone());
        found
    }

    /// `lookup`, worked out.
    fn lookup_once(&self, d: Day, from: &Sym, to: Option<&Sym>) -> Lookup {
        let indexes = self.indexes();
        let Some(to) = to else {
            return Ok(self
                .default_valuation_commodity(d, from)
                .and_then(|to| price_lookup(&indexes.forward, d, from, &to)));
        };
        if let Some(found) = price_lookup(&indexes.forward, d, from, to) {
            return Ok(Some(found));
        }
        if let Some(pds) = [from, to]
            .into_iter()
            .find_map(|c| self.ambiguous_aliases.get(c))
        {
            return Err(ambiguous_alias_error(pds));
        }
        // at --debug=2 or more, show when alias rates are tried, and where
        // they were declared
        if let Some(rates) = &self.debug_alias_rates
            && self
                .alias_tries
                .borrow_mut()
                .insert((d, from.clone(), to.clone()))
        {
            let rates: Vec<String> = (rates.iter())
                .map(|pd| {
                    format!(
                        " {}>{} {}  (alias declared at {}:{})",
                        pd.commodity,
                        pd.amount.commodity,
                        pd.amount.quantity,
                        pd.pos.file,
                        pd.pos.line
                    )
                })
                .collect();
            eprintln!(
                "no market price found from {} to {} on {d}, trying commodity alias rates:\n{}",
                quote_if_needed(from),
                quote_if_needed(to),
                rates.join("\n")
            );
        }
        Ok(indexes
            .forward_with_aliases
            .as_ref()
            .and_then(|index| price_lookup(index, d, from, to)))
    }

    /// The default valuation commodity of a commodity on a date: the
    /// destination of its latest declared price on or before the date if
    /// any commodity has a declared price by then; otherwise, if there are
    /// declared prices, of its latest declared one; otherwise of its latest
    /// inferred price on or before the date (lookupDefaultValuations).
    fn default_valuation_commodity(&self, d: Day, from: &Sym) -> Option<Sym> {
        let on_or_before = |index: &DefaultValuationIndex| {
            index
                .get(from)
                .and_then(|m| m.range(..=d).next_back())
                .map(|(_, to)| to.clone())
        };
        let PriceIndexes {
            declared_default,
            inferred_default,
            ..
        } = self.indexes();
        let declared_by_then = declared_default
            .values()
            .any(|m| m.first_key_value().is_some_and(|(day, _)| *day <= d));
        if declared_by_then {
            on_or_before(declared_default)
        } else if !declared_default.is_empty() {
            declared_default
                .get(from)
                .and_then(|m| m.last_key_value())
                .map(|(_, to)| to.clone())
        } else {
            on_or_before(inferred_default)
        }
    }
}

/// A price index with alias rates added, for the commodity pairs with no
/// price in either direction (buildPriceIndexes' addAliasPrices).
fn with_alias_prices(forward: &PriceIndex, aliases: &[MarketPrice]) -> PriceIndex {
    let mut index = forward.clone();
    for p in aliases {
        let (f, t) = (p.from.clone(), p.to.clone());
        if !forward.contains_key(&(f.clone(), t.clone()))
            && !forward.contains_key(&(t.clone(), f.clone()))
        {
            index.insert((f, t), BTreeMap::from([(p.date, p.clone())]));
        }
    }
    index
}

/// The error for using an alias declared differently by different files
/// (ambiguousAliasError).
fn ambiguous_alias_error(pds: &[PriceDirective]) -> String {
    let alias = pds
        .first()
        .map(|pd| quote_if_needed(&pd.amount.commodity))
        .unwrap_or_default();
    let mut lines = vec![format!(
        "commodity alias {alias} is declared differently by different files, so it can't be used for valuation:"
    )];
    for pd in pds {
        lines.push(format!(
            "{}:{}: commodity {}  ; alias: {} {alias}",
            pd.pos.file,
            pd.pos.line,
            quote_if_needed(&pd.commodity),
            pd.amount.quantity
        ));
    }
    lines.push(
        "To use it, make these declarations agree; or use a different alias in one of them."
            .to_string(),
    );
    lines.join("\n") + "\n"
}

/// With the prices in effect on a date, the value of one unit of `from` in
/// `to`: the most direct price, preferring a chain of forward (declared or
/// inferred) prices to one using reverse prices too. Returns `to` and the
/// rate, which has at least as many decimal places as any price used; None
/// if no price is found, or the commodities are the same (priceLookup,
/// makePriceGraph).
fn price_lookup(index: &PriceIndex, d: Day, from: &Sym, to: &Sym) -> Option<(Sym, Decimal)> {
    if to == from {
        return None;
    }
    // the latest price on or before the date for each commodity pair
    let forward: Vec<MarketPrice> = index
        .values()
        .filter_map(|m| m.range(..=d).next_back().map(|(_, p)| p.clone()))
        .collect();
    let path = match prices_shortest_path(from, to, &forward) {
        Some(path) => path.into_iter().cloned().collect::<Vec<_>>(),
        None => {
            // and the reverses of those whose pair has no forward price
            let pairs: BTreeSet<(&Sym, &Sym)> = forward.iter().map(|p| (&p.from, &p.to)).collect();
            let reverse: Vec<MarketPrice> = forward
                .iter()
                .map(market_price_reverse)
                .filter(|p| !pairs.contains(&(&p.from, &p.to)))
                .collect();
            let all: Vec<MarketPrice> = forward.iter().cloned().chain(reverse).collect();
            prices_shortest_path(from, to, &all)?
                .into_iter()
                .cloned()
                .collect()
        }
    };
    let last = path.last()?.to.clone();
    let rate = path
        .iter()
        .fold(Decimal::from_i64(1), |product, p| &product * &p.rate);
    // the product drops trailing zeros, which are restored up to the
    // most decimal places of any rate (setMinDecimalPlaces)
    let places = path.iter().map(|p| p.rate.places()).max().unwrap_or(0);
    let rate = if rate.places() < places {
        rate.round_to(places)
    } else {
        rate
    };
    Some((last, rate))
}

/// The first of the shortest chains of prices leading from one commodity
/// to another, found by a breadth-first search over prices with at most
/// one per directed commodity pair, never revisiting a commodity
/// (pricesShortestPath).
fn prices_shortest_path<'p>(
    start: &Sym,
    end: &Sym,
    edges: &'p [MarketPrice],
) -> Option<Vec<&'p MarketPrice>> {
    // each path so far, with the prices it may still use
    let mut paths: Vec<(Vec<&MarketPrice>, Vec<&MarketPrice>)> =
        vec![(Vec::new(), edges.iter().collect())];
    loop {
        let mut extended = Vec::new();
        for (path, unused) in &paths {
            let path_end = path.last().map_or(start, |e| &e.to);
            let (next, remaining): (Vec<&MarketPrice>, Vec<&MarketPrice>) =
                unused.iter().copied().partition(|e| &e.from == path_end);
            for e in next {
                let visited = |c: &Sym| c == start || c == &e.to || path.iter().any(|p| &p.to == c);
                let remaining = remaining
                    .iter()
                    .copied()
                    .filter(|r| !visited(&r.to))
                    .collect();
                let mut path = path.clone();
                path.push(e);
                extended.push((path, remaining));
            }
        }
        if extended.is_empty() {
            return None;
        }
        if let Some((path, _)) = extended
            .iter()
            .find(|(path, _)| path.last().is_some_and(|e| &e.to == end))
        {
            return Some(path.clone());
        }
        paths = extended;
    }
}

/// The market value of an amount on a date in the given commodity, or its
/// default valuation commodity, with that commodity's style and all its
/// decimal digits (up to 8 if they are infinite), and no cost; or the
/// amount unchanged if the prices can not value it (amountValueAtDate).
pub fn amount_value_at_date(
    oracle: &PriceOracle,
    styles: &CommodityStyles,
    to: Option<&Sym>,
    d: Day,
    a: &Amount,
) -> Result<Amount, String> {
    Ok(match oracle.lookup(d, &a.commodity, to)? {
        None => a.clone(),
        Some((commodity, rate)) => Amount {
            commodity,
            quantity: &rate * &a.quantity,
            ..Amount::null()
        }
        .styled(styles)
        .with_full_precision_up_to(None),
    })
}

/// The market values of a mixed amount's amounts on a date, added up
/// (mixedAmountValueAtDate).
pub fn mixed_amount_value_at_date(
    oracle: &PriceOracle,
    styles: &CommodityStyles,
    to: Option<&Sym>,
    d: Day,
    m: &MixedAmount,
) -> Result<MixedAmount, String> {
    let valued = m
        .amounts_raw()
        .iter()
        .map(|a| amount_value_at_date(oracle, styles, to, d, a))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(MixedAmount::from_amounts(valued))
}

/// A mixed amount's value as a valuation type says: on the posting's
/// date, at the end of its report period, today, or on a given date
/// (mixedAmountApplyValuation, amountApplyValuation).
pub fn mixed_amount_apply_valuation(
    oracle: &PriceOracle,
    styles: &CommodityStyles,
    period_last: impl FnOnce() -> Result<Day, String>,
    today: Day,
    posting_date: Day,
    v: &ValuationType,
    m: &MixedAmount,
) -> Result<MixedAmount, String> {
    let d = match v {
        ValuationType::AtThen(_) => posting_date,
        ValuationType::AtEnd(_) => period_last()?,
        ValuationType::AtNow(_) => today,
        ValuationType::AtDate(d, _) => *d,
    };
    mixed_amount_value_at_date(oracle, styles, v.commodity(), d, m)
}

/// The gain of a mixed amount: its value less the value of its cost basis
/// (mixedAmountApplyGain).
pub fn mixed_amount_apply_gain(
    oracle: &PriceOracle,
    styles: &CommodityStyles,
    period_last: impl FnOnce() -> Result<Day, String>,
    today: Day,
    posting_date: Day,
    v: &ValuationType,
    m: &MixedAmount,
) -> Result<MixedAmount, String> {
    let gain = m - &m.clone().to_cost_basis();
    mixed_amount_apply_valuation(oracle, styles, period_last, today, posting_date, v, &gain)
}

/// The gain of a mixed amount on a date: its value less the value of its
/// cost basis (mixedAmountGainAtDate).
pub fn mixed_amount_gain_at_date(
    oracle: &PriceOracle,
    styles: &CommodityStyles,
    to: Option<&Sym>,
    d: Day,
    m: &MixedAmount,
) -> Result<MixedAmount, String> {
    let gain = m - &m.clone().to_cost_basis();
    mixed_amount_value_at_date(oracle, styles, to, d, &gain)
}

/// Convert the journal's posting amounts to cost: to the cost basis where
/// known, otherwise the transacted cost (ToCost); or to the transacted cost
/// (ToTransactedCost). Equity conversion postings with no cost of their
/// own are dropped, the costs they balance being converted already
/// (journalToCost, transactionToCost, postingToCost).
pub fn journal_to_cost(op: ConversionOp, j: &mut Journal) {
    let to_cost: fn(MixedAmount) -> MixedAmount = match op {
        ConversionOp::NoConversionOp => return,
        ConversionOp::ToCost => MixedAmount::to_cost_basis,
        ConversionOp::ToTransactedCost => MixedAmount::to_cost,
    };
    for t in &mut j.txns {
        t.postings.retain(|p| {
            !(p.has_tag(CONVERSION_POSTING_TAG)
                && p.amount.amounts_raw().iter().all(|a| a.cost.is_none()))
        });
        for p in &mut t.postings {
            p.amount = to_cost(std::mem::take(&mut p.amount));
        }
    }
}

pub(crate) mod tests {
    use super::*;

    fn day(y: i64, m: u32, d: u32) -> Day {
        Day::from_ymd(y, m, d).unwrap()
    }

    fn price(y: i64, m: u32, d: u32, from: &str, rate: &str, to: &str) -> MarketPrice {
        MarketPrice {
            date: Day::from_gregorian(y, m.into(), d.into()),
            from: from.into(),
            to: to.into(),
            rate: rate.parse().unwrap(),
        }
    }

    /// An oracle of these declared prices and alias rates.
    fn oracle(declared: &[MarketPrice], aliases: &[MarketPrice]) -> PriceOracle {
        let j = Journal {
            price_directives: declared
                .iter()
                .map(|p| PriceDirective {
                    pos: SourcePos::null(),
                    date: p.date,
                    commodity: p.from.clone(),
                    amount: Amount {
                        commodity: p.to.clone(),
                        quantity: p.rate.clone(),
                        ..Amount::null()
                    },
                })
                .collect(),
            ..Journal::default()
        };
        PriceOracle {
            alias_prices: aliases.to_vec(),
            ..PriceOracle::new(false, 0, &j)
        }
    }

    fn lookup(o: &PriceOracle, d: Day, from: &str, to: Option<&str>) -> Option<(String, Decimal)> {
        o.lookup(d, &Sym::from(from), to.map(Sym::from).as_ref())
            .unwrap()
            .map(|(c, r)| (c.to_string(), r))
    }

    fn some(c: &str, r: &str) -> Option<(String, Decimal)> {
        Some((c.to_string(), r.parse().unwrap()))
    }

    #[cfg_attr(test, test)]
    pub(crate) fn price_lookup() {
        let ps1 = [
            price(2000, 1, 1, "A", "10", "B"),
            price(2000, 1, 1, "B", "10", "C"),
            price(2000, 1, 1, "C", "10", "D"),
            price(2000, 1, 1, "E", "2", "D"),
            price(2001, 1, 1, "A", "11", "B"),
        ];
        let o = oracle(&ps1, &[]);
        assert_eq!(lookup(&o, day(1999, 1, 1), "A", None), None);
        assert_eq!(lookup(&o, day(2000, 1, 1), "A", None), some("B", "10"));
        assert_eq!(
            lookup(&o, day(2000, 1, 1), "B", Some("A")),
            some("A", "0.1")
        );
        assert_eq!(
            lookup(&o, day(2000, 1, 1), "A", Some("E")),
            some("E", "500")
        );
        // alias rates (100 d per D; and 1000 B per A, which is ignored since
        // there are A-B market prices) are used only between commodities with
        // no market prices between them, and are never default destinations
        let aliases = [
            price(0, 1, 1, "D", "100", "d"),
            price(0, 1, 1, "A", "1000", "B"),
        ];
        let o = oracle(&ps1, &aliases);
        assert_eq!(
            lookup(&o, day(2000, 1, 1), "D", Some("d")),
            some("d", "100")
        );
        assert_eq!(
            lookup(&o, day(2000, 1, 1), "d", Some("D")),
            some("D", "0.01")
        );
        assert_eq!(
            lookup(&o, day(2000, 1, 1), "A", Some("d")),
            some("d", "100000")
        );
        assert_eq!(lookup(&o, day(2000, 1, 1), "A", Some("B")), some("B", "10"));
        assert_eq!(lookup(&o, day(2000, 1, 1), "D", None), None);
    }

    #[cfg_attr(test, test)]
    pub(crate) fn market_price_reversed() {
        let p = |rate: &str| MarketPrice {
            date: null_date(),
            from: "".into(),
            to: "".into(),
            rate: rate.parse().unwrap(),
        };
        assert_eq!(market_price_reverse(&p("2")), p("0.5"));
        // the reverse of a 0 price is a 0 price
        assert_eq!(market_price_reverse(&p("0")), p("0"));
    }
}
