//! Valuation: converting amounts to cost or to market value (the types of
//! Hledger.Data.Valuation; the conversions arrive with the reports that use
//! them).

use crate::amount::Sym;
use crate::dates::Day;

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
