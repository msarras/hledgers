//! The prices command (Hledger.Cli.Commands.Prices): market prices as P
//! directives: declared, inferred from costs, reversed, or given by
//! commodity aliases; or a summary of the declared ones per commodity.

use std::collections::BTreeSet;

use crate::amount::currency::to_currency_code;
use crate::amount::{Amount, DEFAULT_MAX_DISPLAY_PRECISION, Sym};
use crate::cli::args::{Arity, Flag, Mode};
use crate::cli::io::with_title;
use crate::cli::opts::CliOpts;
use crate::dates::Day;
use crate::model::render::show_price_directive;
use crate::model::{Journal, PriceDirective};
use crate::query::Query;
use crate::valuation::{amount_price_directive_from_cost, journal_alias_price_directives};

const NONE: Arity = Arity::None;

pub static MODE: Mode = Mode {
    name: "prices",
    aliases: &[],
    flags: &[
        Flag::new(
            &["show-reverse"],
            NONE,
            "",
            "also show the prices inferred by reversing known prices",
        ),
        Flag::new(
            &["show-aliases"],
            NONE,
            "",
            "also show the exchange rates provided by commodity aliases",
        ),
        Flag::new(
            &["summary"],
            NONE,
            "",
            "summarise declared prices per commodity instead of listing them",
        ),
        Flag::new(
            &["locations"],
            NONE,
            "",
            "also show where prices where declared",
        ),
        Flag::new(
            &["costs"],
            NONE,
            "",
            "deprecated, use --infer-market-prices instead",
        )
        .sets(&["infer-market-prices"])
        .hidden(),
        Flag::new(
            &["inverted-costs"],
            NONE,
            "",
            "deprecated, use --show-reverse instead",
        )
        .sets(&["show-reverse"])
        .hidden(),
        Flag::new(
            &["infer-reverse-prices"],
            NONE,
            "",
            "deprecated, use --show-reverse instead",
        )
        .sets(&["show-reverse"])
        .hidden(),
    ],
    doc: crate::command_doc!("Prices"),
    ..Mode::DEFAULT
};

/// Run the prices command (prices): the declared prices, with
/// --infer-market-prices also those inferred from costs, with
/// --show-aliases the alias rates usable between commodities with no
/// market price between them, and with --show-reverse the reverses of all
/// these; those the query selects, by date, styled with their commodity's
/// style but their own precision.
pub fn prices(opts: &CliOpts, j: &Journal) -> Result<String, String> {
    let raw = &opts.raw;
    let q = &opts.spec.query;
    let show_loc = raw.bool("locations");
    if raw.bool("summary") {
        let declared = j
            .price_directives
            .iter()
            .filter(|pd| q.matches_price_directive(pd))
            .collect::<Vec<_>>();
        return Ok(with_title(opts, prices_summary(j, q, &declared)));
    }
    let market = if raw.bool("infer-market-prices") {
        // with --locations, at their transactions' first lines
        // (postingPriceDirectivesFromCost, postingPriceDirectivesWithLoc)
        let from_costs = j.txns.iter().flat_map(|t| {
            t.postings.iter().flat_map(move |p| {
                let date = p.date_in(Some(t));
                p.amount.amounts_raw().iter().filter_map(move |a| {
                    let pd = amount_price_directive_from_cost(date, a)?;
                    Some(if show_loc {
                        PriceDirective {
                            pos: t.pos.0.clone(),
                            ..pd
                        }
                    } else {
                        pd
                    })
                })
            })
        });
        merge_price_directives(j.price_directives.clone(), from_costs)
    } else {
        j.price_directives.clone()
    };
    let pair = |pd: &PriceDirective| (pd.commodity, pd.amount.commodity);
    let alias_prices: Vec<PriceDirective> = if raw.bool("show-aliases") {
        let market_pairs: BTreeSet<(Sym, Sym)> = market.iter().map(pair).collect();
        journal_alias_price_directives(j)
            .into_iter()
            .filter(|pd| {
                let (f, t) = pair(pd);
                !market_pairs.contains(&(f, t)) && !market_pairs.contains(&(t, f))
            })
            .collect()
    } else {
        Vec::new()
    };
    let alias_pairs: BTreeSet<(Sym, Sym)> = alias_prices.iter().map(pair).collect();
    // an alias rate, or its reverse
    let is_alias_rate = |pd: &PriceDirective| {
        let (f, t) = pair(pd);
        alias_pairs.contains(&(f, t)) || alias_pairs.contains(&(t, f))
    };
    let mut forward = market;
    forward.extend(alias_prices);
    let all = if raw.bool("show-reverse") {
        let reverse: Vec<PriceDirective> =
            forward.iter().filter_map(reverse_price_directive).collect();
        merge_price_directives(forward, reverse)
    } else {
        forward
    };
    let mut selected: Vec<&PriceDirective> = all
        .iter()
        .filter(|pd| q.matches_price_directive(pd))
        .collect();
    selected.sort_by_key(|pd| pd.date);
    let styles = j.commodity_styles();
    let mut out = String::new();
    for pd in selected {
        let styled = PriceDirective {
            amount: pd.amount.clone().styled(&styles),
            ..pd.clone()
        };
        out.push_str(&show_price_directive(&styled));
        let mut notes = Vec::new();
        if is_alias_rate(pd) {
            notes.push("commodity alias".to_string());
        }
        if show_loc && !pd.pos.file.is_empty() {
            notes.push(format!("location: {}:{}", pd.pos.file, pd.pos.line));
        }
        if !notes.is_empty() {
            out.push_str("  ; ");
            out.push_str(&notes.join(", "));
        }
        out.push('\n');
    }
    Ok(with_title(opts, out))
}

/// The first price directives, and those of the second whose date and
/// commodity pair none of the first has (mergePriceDirectives).
fn merge_price_directives(
    mut pds1: Vec<PriceDirective>,
    pds2: impl IntoIterator<Item = PriceDirective>,
) -> Vec<PriceDirective> {
    let id = |pd: &PriceDirective| (pd.date, pd.commodity, pd.amount.commodity);
    let ids: BTreeSet<(Day, Sym, Sym)> = pds1.iter().map(id).collect();
    pds1.extend(pds2.into_iter().filter(|pd| !ids.contains(&id(pd))));
    pds1
}

/// The price directive in the opposite direction, keeping its position,
/// shown with all its decimal digits up to 8; none for a zero price
/// (reversePriceDirective).
fn reverse_price_directive(pd: &PriceDirective) -> Option<PriceDirective> {
    if pd.amount.is_zero() {
        return None;
    }
    let amount = Amount {
        commodity: pd.commodity,
        ..pd.amount.clone()
    }
    .invert()
    .with_full_precision_up_to(Some(DEFAULT_MAX_DISPLAY_PRECISION));
    Some(PriceDirective {
        commodity: pd.amount.commodity,
        amount,
        ..pd.clone()
    })
}

/// A table of the commodities used, by ISO currency code, except the base
/// currency and those the query's cur: terms exclude, with their number of
/// declared prices, first and last price dates, the period between them,
/// and the share of its weekdays having a price (pricesSummary).
fn prices_summary(j: &Journal, q: &Query, pds: &[&PriceDirective]) -> String {
    let base = j.base_currency_code();
    let symq = q.clone().filter(Query::is_cur);
    let used = j.commodities_used();
    let codes: BTreeSet<&str> = used
        .iter()
        .map(|c| to_currency_code(c))
        .filter(|c| symq.matches_commodity(c) && *c != base)
        .collect();
    let mut rows: Vec<Vec<String>> = vec![
        [
            "Commodity",
            "Prices",
            "Earliest",
            "Latest",
            "Period",
            "Coverage",
        ]
        .map(String::from)
        .to_vec(),
    ];
    for code in codes {
        let dates: Vec<Day> = pds
            .iter()
            .filter(|pd| to_currency_code(&pd.commodity) == code)
            .map(|pd| pd.date)
            .collect();
        let days: BTreeSet<Day> = dates.iter().copied().collect();
        let (first, last) = (days.first().copied(), days.last().copied());
        let (period, coverage) = match (first, last) {
            (Some(s), Some(e)) if dates.len() >= 2 => {
                let n = e.diff_days(s) + 1;
                let weekdays: Vec<Day> = (0..n)
                    .map(|i| s.add_days(i))
                    .filter(|d| d.weekday() <= 5)
                    .collect();
                let covered = weekdays.iter().filter(|d| days.contains(d)).count();
                let pct = if weekdays.is_empty() {
                    0.0
                } else {
                    covered as f64 / weekdays.len() as f64 * 100.0
                };
                (compact_duration(n), format!("{pct:.0}%"))
            }
            _ => (String::new(), String::new()),
        };
        let show = |d: Option<Day>| d.map_or("-".to_string(), |d| d.to_string());
        rows.push(vec![
            code.to_string(),
            dates.len().to_string(),
            show(first),
            show(last),
            period,
            coverage,
        ]);
    }
    let left = [true, false, true, true, false, false];
    let widths: Vec<usize> = (0..left.len())
        .map(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0))
        .collect();
    let mut out = String::new();
    for r in &rows {
        let cells: Vec<String> = r
            .iter()
            .zip(&widths)
            .zip(left)
            .map(|((c, w), l)| {
                if l {
                    format!("{c:<w$}")
                } else {
                    format!("{c:>w$}")
                }
            })
            .collect();
        out.push_str(cells.join("  ").trim_end());
        out.push('\n');
    }
    out
}

/// A number of days as a short duration: "45d", "11mo", "1y 3mo", with
/// 30-day months and 365-day years (compactDuration).
fn compact_duration(n: i64) -> String {
    if n < 60 {
        format!("{n}d")
    } else if n < 365 {
        format!("{}mo", n / 30)
    } else {
        let (y, mo) = (n / 365, n % 365 / 30);
        if mo == 0 {
            format!("{y}y")
        } else {
            format!("{y}y {mo}mo")
        }
    }
}
