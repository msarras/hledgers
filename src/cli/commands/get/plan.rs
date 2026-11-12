//! What `get` fetches when the journal has no `prices/getprices` helper, as
//! a pure function of the journal, its price files and today: the
//! commodities held and those they are priced in, the days whose prices are
//! missing, the tickers to try, and the checks a fetched series must pass to
//! be used.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use super::sources::{Series, Source};
use crate::account::AccountType;
use crate::amount::currency::{is_currency_code, to_currency_code};
use crate::amount::{Amount, AmountStyle, Precision, Side, Sym};
use crate::dates::Day;
use crate::decimal::Decimal;
use crate::model::render::show_price_directive;
use crate::model::{Journal, PriceDirective, SourcePos};

/// Days: how long before its first posting a commodity's prices are wanted,
/// the longest run of missing days taken for a weekend or holiday, and how
/// far apart a journal price and a fetched one may be to be compared.
const NEAR: i64 = 7;

/// A commodity's `price:` tag: one word, its market code, or three, the
/// source and ticker to fetch its prices from and the commodity they are in.
#[derive(Debug, PartialEq)]
enum PriceTag {
    Code(String),
    Declared(Source, String, Sym),
}

/// A `price:` tag's value read; an error if it is neither one word nor
/// three, or names an unknown source.
fn parse_price_tag(commodity: &str, value: &str) -> Result<PriceTag, String> {
    match value.split_whitespace().collect::<Vec<_>>()[..] {
        [code] => Ok(PriceTag::Code(code.to_string())),
        [source, ticker, quote] => match Source::parse(source) {
            Some(s) => Ok(PriceTag::Declared(s, ticker.to_string(), Sym::from(quote))),
            None => {
                let names: Vec<&str> = Source::ALL.iter().map(|s| s.name()).collect();
                Err(format!(
                    "commodity {commodity} has a price: tag with an unknown source, \"{value}\": the sources are: {}",
                    names.join(", ")
                ))
            }
        },
        _ => Err(format!(
            "commodity {commodity} has a malformed price: tag, \"{value}\": it should be a market code (one word) or SOURCE TICKER QUOTE (three words)"
        )),
    }
}

/// A commodity's `price:` tags: at most one of one word, and the pairs of
/// three-word ones, each in a different quote commodity.
#[derive(Debug, Default)]
struct PriceTags {
    code: Option<String>,
    pairs: Vec<(Source, String, Sym)>,
}

/// Each commodity's `price:` tags; an error for a malformed one, a second
/// one-word one, or a second three-word one in the same quote.
fn price_tags(j: &Journal) -> Result<BTreeMap<Sym, PriceTags>, String> {
    let mut tags: BTreeMap<Sym, PriceTags> = BTreeMap::new();
    for (c, ts) in &j.declared_commodity_tags {
        for (_, v) in ts.iter().filter(|(n, _)| n.eq_ignore_ascii_case("price")) {
            let t = tags.entry(*c).or_default();
            match parse_price_tag(c, v)? {
                PriceTag::Code(code) => match &t.code {
                    Some(old) => {
                        return Err(format!(
                            "commodity {c} has two one-word price: tags, {old} and {code}: it can have one market code"
                        ));
                    }
                    None => t.code = Some(code),
                },
                PriceTag::Declared(s, ticker, q) => {
                    if t.pairs.iter().any(|p| p.2 == q) {
                        return Err(format!(
                            "commodity {c} has two price: tags with prices in {q}: it can have one for each commodity"
                        ));
                    }
                    t.pairs.push((s, ticker, q));
                }
            }
        }
    }
    Ok(tags)
}

/// A commodity's market code: its one-word `price:` tag, else its ISO 4217
/// code or symbol, uppercased (`$` is USD, `btc` BTC).
fn market_code(tags: &BTreeMap<Sym, PriceTags>, c: Sym) -> String {
    let code = tags.get(&c).and_then(|t| t.code.clone());
    code.unwrap_or_else(|| to_currency_code(&c).to_uppercase())
}

/// The commodities held: those with a non-zero balance in some asset or
/// liability account, other than the commodity with no symbol.
fn held(j: &Journal) -> BTreeSet<Sym> {
    let mut sums: BTreeMap<(&str, Sym), Decimal> = BTreeMap::new();
    for p in j.postings() {
        for a in p.p.amount.amounts_raw() {
            let sum = sums
                .entry((&*p.p.account, a.commodity))
                .or_insert(Decimal::ZERO);
            *sum = &*sum + &a.quantity;
        }
    }
    let kept = |a: &str| {
        j.account_type(a)
            .is_some_and(|t| t.is_subtype_of(AccountType::Asset) || t == AccountType::Liability)
    };
    (sums.into_iter())
        .filter(|((a, c), q)| !q.is_zero() && !c.is_empty() && kept(a))
        .map(|((_, c), _)| c)
        .collect()
}

/// The commodity of a commodity's latest price in the journal, declared or
/// implied by a cost, a declared one winning on the same day.
fn latest_quote(j: &Journal, c: Sym) -> Option<Sym> {
    let costs = (j.inferred_market_prices.iter())
        .filter(|p| p.from == c)
        .map(|p| (p.date, p.to));
    let declared = (j.price_directives.iter())
        .filter(|p| p.commodity == c)
        .map(|p| (p.date, p.amount.commodity));
    costs
        .chain(declared)
        .max_by_key(|(d, _)| *d)
        .map(|(_, q)| q)
}

/// The Yahoo tickers to try for a commodity, in order: its FX pair with its
/// quote when both are currencies (`pair`), its market code, and the pair a
/// cryptocurrency is quoted as when the quote is a currency.
#[derive(Debug, PartialEq)]
pub struct Inferred {
    pub tickers: Vec<String>,
    pub pair: bool,
    /// The quote's market code, the currency Yahoo must quote a ticker in.
    pub quote_code: String,
    /// Whether that code is the quote's one-word `price:` tag.
    pub quote_tagged: bool,
}

fn inferred(code: &str, quote_code: &str, quote_tagged: bool) -> Inferred {
    let pair = is_currency_code(code) && is_currency_code(quote_code);
    let mut tickers = Vec::new();
    if pair {
        tickers.push(format!("{code}{quote_code}=X"));
    }
    tickers.push(code.to_string());
    if is_currency_code(quote_code) {
        tickers.push(format!("{code}-{quote_code}"));
    }
    Inferred {
        tickers,
        pair,
        quote_code: quote_code.to_string(),
        quote_tagged,
    }
}

/// Where a commodity's prices come from.
#[derive(Debug, PartialEq)]
pub enum Lookup {
    /// The source and ticker its three-word tag declares, used unchecked.
    Declared(Source, String),
    /// Yahoo, by the first of these tickers to pass the checks.
    Inferred(Inferred),
}

impl Lookup {
    pub fn source(&self) -> Source {
        match self {
            Lookup::Declared(s, _) => *s,
            Lookup::Inferred(_) => Source::Yahoo,
        }
    }
}

/// The source and the tickers, as `get` shows its requests.
impl fmt::Display for Lookup {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Lookup::Declared(s, ticker) => write!(f, "{} {ticker}", s.name()),
            Lookup::Inferred(inf) => write!(f, "yahoo {}", inf.tickers.join(" or ")),
        }
    }
}

/// A commodity to fetch prices for, in its quote commodity, with the first
/// and last days wanted.
#[derive(Debug, PartialEq)]
pub struct Target {
    pub commodity: Sym,
    pub quote: Sym,
    pub lookup: Lookup,
    pub start: Day,
    pub end: Day,
}

/// The commodities to fetch prices for: those held, other than the base
/// currency, and then the commodities they are priced in, other than the
/// base currency, and so on. Each is priced in the quote of each of its
/// three-word tags, as a target of its own, else in the commodity of its
/// latest price in the journal, else in the base currency, and is wanted
/// from NEAR days before its first posting, or from the first day wanted of
/// a commodity priced in it if earlier, to its source's last final day. A
/// commodity is the base currency if its market code is the base
/// currency's. An error for a malformed `price:` tag, or a repeated one.
pub fn targets(j: &Journal, today: Day) -> Result<Vec<Target>, String> {
    let tags = price_tags(j)?;
    let code = |c: Sym| market_code(&tags, c);
    let base = j.base_currency().map(|(s, _)| Sym::from(s));
    let base_code = base.map(code);
    let is_base = |c: Sym| Some(code(c)) == base_code;
    let mut first: BTreeMap<Sym, Day> = BTreeMap::new();
    for p in j.postings() {
        for a in p.p.amount.amounts_raw() {
            let d = first.entry(a.commodity).or_insert(p.date());
            *d = (*d).min(p.date());
        }
    }
    // the held commodities, then the quotes they lead to, each once, with
    // the index of each one's commodity in cs, its quote and its lookup
    let mut cs: Vec<Sym> = held(j).into_iter().filter(|c| !is_base(*c)).collect();
    let mut pairs: Vec<(usize, Sym, Lookup)> = Vec::new();
    let mut k = 0;
    while k < cs.len() {
        let c = cs[k];
        let declared = tags.get(&c).map_or(&[][..], |t| &t.pairs[..]);
        let quotes: Vec<(Sym, Lookup)> = if declared.is_empty() {
            let q = latest_quote(j, c).or(base);
            let tagged = |q: Sym| tags.get(&q).is_some_and(|t| t.code.is_some());
            let pair = |q: Sym| (q, Lookup::Inferred(inferred(&code(c), &code(q), tagged(q))));
            q.map(pair).into_iter().collect()
        } else {
            (declared.iter())
                .map(|(s, ticker, q)| (*q, Lookup::Declared(*s, ticker.clone())))
                .collect()
        };
        for (q, lookup) in quotes {
            if !q.is_empty() && !is_base(q) && !cs.contains(&q) {
                cs.push(q);
            }
            pairs.push((k, q, lookup));
        }
        k += 1;
    }
    // each one's first day, moved back to those of the commodities priced in it
    let mut starts: Vec<Option<Day>> = (cs.iter())
        .map(|c| first.get(c).map(|d| d.add_days(-NEAR)))
        .collect();
    loop {
        let mut changed = false;
        for (k, q, _) in &pairs {
            let Some(i) = cs.iter().position(|c| c == q) else {
                continue;
            };
            if let Some(s) = starts[*k].filter(|s| starts[i].is_none_or(|t| *s < t)) {
                starts[i] = Some(s);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    Ok(pairs
        .into_iter()
        .filter_map(|(k, quote, lookup)| {
            Some(Target {
                commodity: cs[k],
                quote,
                start: starts[k]?,
                end: lookup.source().last_day(today),
                lookup,
            })
        })
        .collect())
}

/// The days with a declared price of a target's commodity in its quote, in
/// the journal or in its price file.
pub fn have(j: &Journal, file: &[PriceDirective], t: &Target) -> BTreeSet<Day> {
    (j.price_directives.iter().chain(file))
        .filter(|p| p.commodity == t.commodity && p.amount.commodity == t.quote)
        .map(|p| p.date)
        .collect()
}

/// The runs of days from `start` to `end` without a price that are worth
/// asking for: longer than NEAR days, as no weekend or holiday is, or
/// reaching `end`.
pub fn holes(start: Day, end: Day, have: &BTreeSet<Day>) -> Vec<(Day, Day)> {
    let mut runs = Vec::new();
    let mut d = start;
    while d <= end {
        if have.contains(&d) {
            d = d.add_days(1);
            continue;
        }
        let from = d;
        while d <= end && !have.contains(&d) {
            d = d.add_days(1);
        }
        let to = d.add_days(-1);
        if to.diff_days(from) + 1 > NEAR || to == end {
            runs.push((from, to));
        }
    }
    runs
}

/// The prices of a target's commodity in its quote that fetched ones are
/// checked against, by day: implied by costs, declared in the journal, and
/// declared in its price file, the last of a day winning.
pub fn known(j: &Journal, file: &[PriceDirective], t: &Target) -> BTreeMap<Day, Decimal> {
    let costs = (j.inferred_market_prices.iter())
        .filter(|p| p.from == t.commodity && p.to == t.quote)
        .map(|p| (p.date, p.rate.abs()));
    let declared = (j.price_directives.iter().chain(file))
        .filter(|p| p.commodity == t.commodity && p.amount.commodity == t.quote)
        .map(|p| (p.date, p.amount.quantity.abs()));
    costs.chain(declared).collect()
}

/// What a ticker tried for a commodity gave.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// Yahoo does not know it.
    NotFound,
    /// Yahoo quotes it in this currency, not the quote's.
    Currency(String),
    /// Its close nearest a journal price, within NEAR days of it, is not
    /// between half and double that price: the close and the price, with
    /// their days.
    Price {
        close: (Day, Decimal),
        known: (Day, Decimal),
    },
    /// In the quote's currency, with no journal price within NEAR days of a
    /// close to check against; Yahoo's name for it.
    Unchecked(String),
    /// In the quote's currency, and its close nearest a journal price, within
    /// NEAR days of it, is between half and double that price.
    Passed,
}

/// The two checks of a series fetched for a ticker: the currency it is
/// quoted in must be the quote's code, and of the journal prices and the
/// closes, the price and close nearest each other in time (the latest price
/// among equals), if within NEAR days, must be within half and double of
/// each other.
pub fn check(s: &Series, quote_code: &str, known: &BTreeMap<Day, Decimal>) -> Outcome {
    if s.currency != quote_code {
        return Outcome::Currency(s.currency.clone());
    }
    // each known price with the closes just before and after it, the latest
    // price first, so that it wins among equally near pairs
    let pairs = known.iter().rev().flat_map(|(kd, kp)| {
        let before = s.closes.range(..=*kd).next_back();
        let after = s.closes.range(*kd..).next();
        (before.into_iter().chain(after))
            .map(move |(cd, cp)| (cd.diff_days(*kd).abs(), (cd, cp), (kd, kp)))
    });
    match pairs.min_by_key(|(gap, ..)| *gap) {
        Some((gap, (cd, cp), (kd, kp))) if gap <= NEAR => {
            let two = Decimal::from_i64(2);
            if &(&two * cp) >= kp && cp <= &(&two * kp) {
                Outcome::Passed
            } else {
                Outcome::Price {
                    close: (*cd, cp.clone()),
                    known: (*kd, kp.clone()),
                }
            }
        }
        _ => Outcome::Unchecked(s.name.clone()),
    }
}

/// What to do after trying an inferred commodity's tickers.
#[derive(Debug, PartialEq)]
pub enum Verdict {
    /// Use the ticker with this index.
    Use(usize),
    /// Try the next ticker.
    More,
    /// Use none, with this warning.
    Skip(String),
}

/// What to do after trying a commodity's tickers in order, these being the
/// outcomes so far: use the first that passes both checks; with no journal
/// price to check against, use two currencies' FX pair, or else, once all
/// are tried, the only ticker in the quote's currency that the price check
/// did not reject; otherwise skip the commodity, with a warning saying what
/// Yahoo has and the tag that settles it.
pub fn verdict(c: Sym, q: Sym, inf: &Inferred, outcomes: &[Outcome]) -> Verdict {
    let Some(last) = outcomes.len().checked_sub(1) else {
        return Verdict::More;
    };
    match outcomes[last] {
        Outcome::Passed => return Verdict::Use(last),
        Outcome::Unchecked(_) if inf.pair && last == 0 => return Verdict::Use(0),
        _ if outcomes.len() < inf.tickers.len() => return Verdict::More,
        _ => {}
    }
    let unchecked: Vec<usize> = (0..outcomes.len())
        .filter(|&i| matches!(outcomes[i], Outcome::Unchecked(_)))
        .collect();
    match unchecked[..] {
        [i] if !inf.pair => Verdict::Use(i),
        _ => Verdict::Skip(skip_message(c, q, inf, outcomes)),
    }
}

/// Why no ticker is used for a commodity: what Yahoo has for each tried,
/// then the tags that settle it. If Yahoo quotes a ticker in another
/// currency, they are the quote's market code, unless its own tag gives it,
/// and that ticker priced in a commodity for the other currency; otherwise
/// a ticker.
fn skip_message(c: Sym, q: Sym, inf: &Inferred, outcomes: &[Outcome]) -> String {
    let code = &inf.quote_code;
    let (mut said, mut unchecked, mut missing) = (Vec::new(), Vec::new(), Vec::new());
    let mut currency = None;
    for (t, o) in inf.tickers.iter().zip(outcomes) {
        match o {
            Outcome::NotFound => missing.push(t.as_str()),
            Outcome::Currency(cur) => {
                currency.get_or_insert((t, cur));
                let is = if inf.quote_tagged {
                    format!("is {code} by its price: tag")
                } else {
                    format!("is taken as {code}")
                };
                said.push(format!("Yahoo quotes {t} in {cur} but {q} {is}"));
            }
            Outcome::Price {
                close: (cd, cp),
                known: (kd, kp),
            } => said.push(format!(
                "Yahoo's {t} closed at {cp} on {cd} but the journal has {} {q} on {kd}",
                kp.round_to(kp.places().min(8))
            )),
            Outcome::Unchecked(name) if name.is_empty() => unchecked.push(t.clone()),
            Outcome::Unchecked(name) => unchecked.push(format!("{t} ({name})")),
            Outcome::Passed => {}
        }
    }
    if !unchecked.is_empty() {
        said.push(format!(
            "Yahoo has {} in {code}, with no journal price within {NEAR} days to check against",
            unchecked.join(" and ")
        ));
    }
    if !missing.is_empty() {
        said.push(format!("Yahoo has no {}", missing.join(" or ")));
    }
    let settle = match currency {
        Some((t, cur)) => {
            let fetch = format!(
                "to fetch it in {cur}, declare: commodity {c} ; price: yahoo {t} QUOTE, QUOTE being your commodity for {cur}"
            );
            if inf.quote_tagged {
                fetch
            } else {
                format!("if {q} is {cur}, declare: commodity {q} ; price: {cur}; or {fetch}")
            }
        }
        None => format!("to choose a ticker, declare: commodity {c} ; price: yahoo TICKER {q}"),
    };
    format!("{c}: {}; {settle}", said.join("; "))
}

/// A fetched price as a P directive, its amount in the quote commodity's
/// style, if it has one, otherwise after the number with a space, but with
/// no digit groups and with the price's own decimal places.
pub fn price_line(date: Day, t: &Target, price: &Decimal, style: Option<&AmountStyle>) -> String {
    let style = style.copied().unwrap_or(AmountStyle {
        side: Side::R,
        spaced: true,
        ..AmountStyle::default()
    });
    let amount = Amount {
        commodity: t.quote,
        quantity: price.clone(),
        style: AmountStyle {
            digit_groups: None,
            precision: Precision::Fixed(price.places()),
            ..style
        },
        cost: None,
        cost_basis: None,
    };
    show_price_directive(&PriceDirective {
        pos: SourcePos::null(),
        date,
        commodity: t.commodity,
        amount,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amount::DigitGroups;

    fn journal(text: &str) -> Journal {
        let iopts = crate::read::InputOpts::default();
        let (j, st) = crate::read::journal::parse_journal(&iopts, "f.j", text).unwrap();
        crate::read::finalise::journal_finalise(&iopts, "f.j", text, j, st).unwrap()
    }

    fn day(m: u32, d: u32) -> Day {
        Day::from_ymd(2026, m, d).unwrap()
    }

    fn num(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn show(t: &Target) -> String {
        let (c, q, l) = (t.commodity, t.quote, &t.lookup);
        format!("{c} in {q}: {l} {}..{}", t.start, t.end)
    }

    fn shown(j: &Journal) -> Vec<String> {
        targets(j, day(11, 12)).unwrap().iter().map(show).collect()
    }

    #[test]
    fn held_quotes_chain_and_windows() {
        let j = journal(
            "commodity $ ; price: CAD

2026-01-10 buy
    assets:broker      10 VEQT.TO @ $45
    assets:cash      $-450

2026-02-01 usd
    assets:usd         100 usd @ $1.40
    assets:cash       $-140

2026-03-01 btc
    assets:crypto      0.001 btc @ 70000 usd
    assets:usd         -70 usd

2026-04-01 sold out
    assets:broker      5 XIU.TO @ $40
    assets:cash       $-200

2026-05-01 sold out
    assets:broker     -5 XIU.TO @ $41
    assets:cash        $205

2026-06-01 not in an asset or liability account
    expenses:x         5 PTS
    revenues:y        -5 PTS

2026-06-02 zero in total, but held per account
    assets:a           1 AAPL
    liabilities:b     -1 AAPL
",
        );
        // $ is the base currency, CAD by its tag; usd is priced in $ and is
        // wanted from btc's start, or its own if earlier
        assert_eq!(
            shown(&j),
            [
                "AAPL in $: yahoo AAPL or AAPL-CAD 2026-05-26..2026-11-11",
                "VEQT.TO in $: yahoo VEQT.TO or VEQT.TO-CAD 2026-01-03..2026-11-11",
                "btc in usd: yahoo BTC or BTC-USD 2026-02-22..2026-11-11",
                "usd in $: yahoo USDCAD=X or USD or USD-CAD 2026-01-25..2026-11-11",
            ]
        );
        // $'s market code is its tag, usd's its symbol
        let tagged: Vec<bool> = (targets(&j, day(11, 12)).unwrap().iter())
            .map(|t| matches!(&t.lookup, Lookup::Inferred(inf) if inf.quote_tagged))
            .collect();
        assert_eq!(tagged, [true, true, false, true]);
    }

    #[test]
    fn a_quote_not_held_is_wanted_for_the_commodities_priced_in_it() {
        let text = "commodity $ ; price: CAD

P 2026-01-01 usd $1.40

2026-03-01 btc
    assets:crypto      0.001 btc @ 70000 usd
    equity:conversion  -70 usd

2026-04-01 eth
    assets:crypto      0.01 eth @ 3000 usd
    equity:conversion  -30 usd

2026-05-01 a later price of eth, in $
    assets:crypto      0.01 eth @ $4200
    assets:cash       $-42
";
        // usd, not held, is wanted from btc's start; eth's latest price is in
        // $, the base currency
        assert_eq!(
            shown(&journal(text)),
            [
                "btc in usd: yahoo BTC or BTC-USD 2026-02-22..2026-11-11",
                "eth in $: yahoo ETH or ETH-CAD 2026-03-25..2026-11-11",
                "usd in $: yahoo USDCAD=X or USD or USD-CAD 2026-02-22..2026-11-11",
            ]
        );
        // were $ USD, usd would be the base currency too
        let j = journal(&text.replace("commodity $ ; price: CAD\n", ""));
        assert_eq!(
            shown(&j),
            [
                "btc in usd: yahoo BTC or BTC-USD 2026-02-22..2026-11-11",
                "eth in $: yahoo ETH or ETH-USD 2026-03-25..2026-11-11",
            ]
        );
    }

    #[test]
    fn price_tags_override_and_malformed_ones_are_errors() {
        let j = journal(
            "commodity VEQT ; price: yahoo VEQT.TO CAD
commodity btc ; price: yahoo BTC-USD usd, price: yahoo BTC-CAD $
commodity $ ; Price: CAD

2026-01-10 buy
    assets:broker      10 VEQT
    assets:cash       $-450

2026-01-11 buy
    assets:crypto      0.001 btc
    assets:cash       $-100
",
        );
        // whatever the case of its name, a tag of three words is a target
        // of its own, and its quote leads to others; one word names the
        // market code, here making CAD and $ both the base currency
        assert_eq!(
            shown(&j),
            [
                "VEQT in CAD: yahoo VEQT.TO 2026-01-03..2026-11-11",
                "btc in usd: yahoo BTC-USD 2026-01-04..2026-11-11",
                "btc in $: yahoo BTC-CAD 2026-01-04..2026-11-11",
                "usd in $: yahoo USDCAD=X or USD or USD-CAD 2026-01-04..2026-11-11",
            ]
        );
        let err = |tag: &str| {
            targets(
                &journal(&format!("commodity X ; price: {tag}\n")),
                day(11, 12),
            )
            .unwrap_err()
        };
        assert_eq!(
            err("yahoo X.TO"),
            "commodity X has a malformed price: tag, \"yahoo X.TO\": it should be a market code (one word) or SOURCE TICKER QUOTE (three words)"
        );
        assert!(err("yahoo X.TO $ now").contains("malformed"));
        assert_eq!(
            err("bloomberg X.TO $"),
            "commodity X has a price: tag with an unknown source, \"bloomberg X.TO $\": the sources are: yahoo"
        );
        assert_eq!(
            err("CAD, price: USD"),
            "commodity X has two one-word price: tags, CAD and USD: it can have one market code"
        );
        assert_eq!(
            err("yahoo X.TO $, price: yahoo X.V $"),
            "commodity X has two price: tags with prices in $: it can have one for each commodity"
        );
        assert_eq!(
            parse_price_tag("$", "CAD"),
            Ok(PriceTag::Code("CAD".to_string()))
        );
    }

    #[test]
    fn holes_worth_asking_for() {
        let have: BTreeSet<Day> = [5, 6, 7, 8, 9, 12, 13, 14, 15, 16, 26, 27, 28]
            .into_iter()
            .map(|d| day(1, d))
            .collect();
        // the four days before the first price and the weekend are not
        // asked for; nine days missing and the days up to the end are
        assert_eq!(
            holes(day(1, 1), day(1, 31), &have),
            [(day(1, 17), day(1, 25)), (day(1, 29), day(1, 31))]
        );
        assert_eq!(
            holes(day(1, 1), day(1, 31), &BTreeSet::new()),
            [(day(1, 1), day(1, 31))]
        );
        assert_eq!(holes(day(1, 9), day(1, 9), &have), []);
        assert_eq!(holes(day(2, 1), day(1, 31), &have), []);
    }

    #[test]
    fn have_and_known_prices_come_from_the_journal_and_the_price_file() {
        let j = journal(
            "P 2026-01-04 VEQT.TO 33 usd
P 2026-01-05 VEQT.TO $45.10

2026-01-02 buy
    assets:broker      10 VEQT.TO @ $45
    assets:cash      $-450
",
        );
        let file = super::super::parse_prices("P 2026-01-06 VEQT.TO $45.20\n").unwrap();
        let t = &targets(&j, day(11, 12)).unwrap()[0];
        assert_eq!(
            show(t),
            "VEQT.TO in $: yahoo VEQT.TO or VEQT.TO-USD 2025-12-26..2026-11-11"
        );
        assert_eq!(
            have(&j, &file, t).into_iter().collect::<Vec<_>>(),
            [day(1, 5), day(1, 6)]
        );
        let known: Vec<String> = (known(&j, &file, t).iter())
            .map(|(d, p)| format!("{d} {p}"))
            .collect();
        assert_eq!(
            known,
            ["2026-01-02 45", "2026-01-05 45.10", "2026-01-06 45.20"]
        );
    }

    fn series(currency: &str, name: &str, closes: &[(Day, &str)]) -> Series {
        Series {
            currency: currency.to_string(),
            name: name.to_string(),
            closes: closes.iter().map(|(d, p)| (*d, num(p))).collect(),
        }
    }

    #[test]
    fn checks() {
        let known: BTreeMap<Day, Decimal> =
            [(day(11, 2), num("72000")), (day(3, 1), num("70000"))].into();
        // bare BTC is an ETF in USD, worth about 32
        let etf = series(
            "USD",
            "Grayscale Bitcoin Mini Trust ETF",
            &[(day(11, 3), "32.1")],
        );
        assert_eq!(
            check(&etf, "USD", &known),
            Outcome::Price {
                close: (day(11, 3), num("32.1")),
                known: (day(11, 2), num("72000"))
            }
        );
        let btc = series("USD", "Bitcoin USD", &[(day(11, 3), "71000.5")]);
        assert_eq!(check(&btc, "USD", &known), Outcome::Passed);
        assert_eq!(check(&btc, "CAD", &known), Outcome::Currency("USD".into()));
        // no journal price within 7 days
        let later = series("USD", "Bitcoin USD", &[(day(11, 10), "71000.5")]);
        assert_eq!(
            check(&later, "USD", &known),
            Outcome::Unchecked("Bitcoin USD".into())
        );
        // half and double are the limits
        let half = series("USD", "", &[(day(11, 2), "36000")]);
        assert_eq!(check(&half, "USD", &known), Outcome::Passed);
        let under = series("USD", "", &[(day(11, 2), "35999.99")]);
        assert!(matches!(
            check(&under, "USD", &known),
            Outcome::Price { .. }
        ));
    }

    #[test]
    fn verdicts() {
        let btc = inferred("BTC", "USD", false);
        assert_eq!(btc.tickers, ["BTC", "BTC-USD"]);
        let price = || Outcome::Price {
            close: (day(11, 3), num("32.1")),
            known: (day(11, 2), num("72000")),
        };
        let unchecked = |s: &str| Outcome::Unchecked(s.to_string());
        let v = |inf: &Inferred, os: &[Outcome]| verdict("btc".into(), "usd".into(), inf, os);
        // the ETF fails the price check, the coin passes
        assert_eq!(v(&btc, &[price()]), Verdict::More);
        assert_eq!(v(&btc, &[price(), Outcome::Passed]), Verdict::Use(1));
        // with no journal price nearby, the only ticker in the currency
        assert_eq!(v(&btc, &[unchecked("A")]), Verdict::More);
        assert_eq!(
            v(&btc, &[unchecked("A"), Outcome::NotFound]),
            Verdict::Use(0)
        );
        assert_eq!(
            v(&btc, &[unchecked("Grayscale Bitcoin Mini Trust ETF"), unchecked("Bitcoin USD")]),
            Verdict::Skip(
                "btc: Yahoo has BTC (Grayscale Bitcoin Mini Trust ETF) and BTC-USD (Bitcoin USD) in USD, with no journal price within 7 days to check against; to choose a ticker, declare: commodity btc ; price: yahoo TICKER usd".into()
            )
        );
        assert_eq!(
            v(&btc, &[price(), Outcome::NotFound]),
            Verdict::Skip(
                "btc: Yahoo's BTC closed at 32.1 on 2026-11-03 but the journal has 72000 usd on 2026-11-02; Yahoo has no BTC-USD; to choose a ticker, declare: commodity btc ; price: yahoo TICKER usd".into()
            )
        );
        // two currencies with no journal price nearby: their FX pair
        let usd = inferred("USD", "CAD", false);
        assert_eq!(usd.tickers, ["USDCAD=X", "USD", "USD-CAD"]);
        assert_eq!(v(&usd, &[unchecked("USD/CAD")]), Verdict::Use(0));
        // but never another ticker unchecked
        let v = |inf: &Inferred, os: &[Outcome]| verdict("usd".into(), "$".into(), inf, os);
        assert!(matches!(
            v(
                &usd,
                &[Outcome::NotFound, unchecked("ProShares"), Outcome::NotFound]
            ),
            Verdict::Skip(_)
        ));
        // the currency check, with the tags that settle it: the quote's
        // market code, or the ticker in a commodity for the other currency
        let veqt = inferred("VEQT.TO", "USD", false);
        let v = |os: &[Outcome]| verdict("VEQT.TO".into(), "$".into(), &veqt, os);
        assert_eq!(
            v(&[Outcome::Currency("CAD".into()), Outcome::NotFound]),
            Verdict::Skip(
                "VEQT.TO: Yahoo quotes VEQT.TO in CAD but $ is taken as USD; Yahoo has no VEQT.TO-USD; if $ is CAD, declare: commodity $ ; price: CAD; or to fetch it in CAD, declare: commodity VEQT.TO ; price: yahoo VEQT.TO QUOTE, QUOTE being your commodity for CAD".into()
            )
        );
        // the quote's market code given by its tag is not questioned
        let aapl = inferred("AAPL", "CAD", true);
        assert_eq!(
            verdict(
                "AAPL".into(),
                "$".into(),
                &aapl,
                &[Outcome::Currency("USD".into()), Outcome::NotFound]
            ),
            Verdict::Skip(
                "AAPL: Yahoo quotes AAPL in USD but $ is CAD by its price: tag; Yahoo has no AAPL-CAD; to fetch it in USD, declare: commodity AAPL ; price: yahoo AAPL QUOTE, QUOTE being your commodity for USD".into()
            )
        );
    }

    #[test]
    fn price_lines_in_the_quote_style() {
        let t = |q: &str| Target {
            commodity: "VEQT.TO".into(),
            quote: q.into(),
            lookup: Lookup::Inferred(inferred("VEQT.TO", "CAD", false)),
            start: day(7, 1),
            end: day(7, 6),
        };
        let dollars = AmountStyle {
            digit_groups: Some(
                DigitGroups {
                    sep: ',',
                    sizes: vec![3],
                }
                .intern(),
            ),
            precision: Precision::Fixed(2),
            ..AmountStyle::default()
        };
        let line = price_line(day(7, 6), &t("$"), &num("1234.5"), Some(&dollars));
        assert_eq!(line, "P 2026-07-06 VEQT.TO $1234.5");
        let euros = AmountStyle {
            side: Side::R,
            spaced: true,
            decimal_mark: Some(','),
            ..AmountStyle::default()
        };
        let line = price_line(day(7, 6), &t("EUR"), &num("1.42042"), Some(&euros));
        assert_eq!(line, "P 2026-07-06 VEQT.TO 1,42042 EUR");
        // which merging reads back as the same price
        let read = super::super::parse_prices(&line).unwrap();
        assert_eq!(read[0].amount.quantity, num("1.42042"));
        assert_eq!(show_price_directive(&read[0]), line);
        let line = price_line(day(7, 6), &t("CAD"), &num("52.28"), None);
        assert_eq!(line, "P 2026-07-06 VEQT.TO 52.28 CAD");
    }
}
