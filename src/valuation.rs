//! Valuation: converting amounts to cost or to market value (the types of
//! Hledger.Data.Valuation; the conversions arrive with the reports that use
//! them).

use crate::amount::{Amount, AmountCost, Sym};
use crate::dates::Day;
use crate::model::MarketPrice;

/// Which cost to convert amounts to (ConversionOp).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConversionOp {
    NoConversionOp,
    /// To cost (`-B`, `--value=cost`).
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

/// The market price on this date implied by an amount's cost, if it has
/// one: its unit cost, or its total cost divided by its quantity, shown
/// with all its decimals or 8 if they are infinite
/// (amountPriceDirectiveFromCost, priceDirectiveToMarketPrice).
pub fn amount_price_from_cost(date: Day, a: &Amount) -> Option<MarketPrice> {
    let price = match &a.cost {
        Some(AmountCost::Unit(u)) => (**u).clone(),
        Some(AmountCost::Total(t)) if !a.quantity.is_zero() => (**t)
            .clone()
            .divide(&a.quantity)
            .with_full_precision_up_to(None),
        _ => return None,
    };
    Some(MarketPrice {
        date,
        from: a.commodity.clone(),
        to: price.commodity,
        rate: price.quantity,
    })
}
