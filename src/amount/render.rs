//! Rendering amounts as text (the rendering half of Hledger.Data.Amount):
//! with their commodity's display style, under an [`AmountFormat`]'s
//! options, as [`Wide`] text that knows its width.

use super::{Amount, AmountCost, CostBasis, DigitGroups, MixedAmount, Precision, Side, Sym};
use std::borrow::Cow;

use crate::text::{Wide, display_width};

/// Options for rendering amounts, which can override their styles. The
/// presets are upstream's defaultFmt, fullZeroFmt, noCostFmt, oneLineFmt,
/// oneLineNoCostFmt and machineFmt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AmountFormat {
    /// Show commodity symbols.
    pub commodity: bool,
    /// Show the commodity, and the style, of zero amounts (otherwise `0`).
    pub zero_commodity: bool,
    /// For a mixed amount, show these commodities in this order, with zeros
    /// for the missing ones.
    pub commodity_order: Option<Vec<Sym>>,
    /// Show digit group marks.
    pub digit_groups: bool,
    /// Show a decimal mark after the digits of a number with digit group
    /// marks and no decimals, so that it reads back unambiguously.
    pub force_decimal_mark: bool,
    /// Show a mixed amount on one line, separated by commas.
    pub one_line: bool,
    /// Pad to at least this width, on the left.
    pub min_width: Option<isize>,
    /// Elide to at most this width.
    pub max_width: Option<isize>,
    /// Show costs.
    pub cost: bool,
    /// Show lot cost bases.
    pub cost_basis: bool,
    /// Show negative amounts in red.
    pub colour: bool,
    /// Quote commodity symbols that need it.
    pub quotes: bool,
    /// Show cost bases in Ledger's lot syntax, `{COST} [DATE] (LABEL)`.
    pub ledger_lot_syntax: bool,
}

impl AmountFormat {
    pub const DEFAULT: AmountFormat = AmountFormat {
        commodity: true,
        zero_commodity: false,
        commodity_order: None,
        digit_groups: true,
        force_decimal_mark: false,
        one_line: false,
        min_width: Some(0),
        max_width: None,
        cost: true,
        cost_basis: true,
        colour: false,
        quotes: true,
        ledger_lot_syntax: false,
    };
    /// Zeros shown with their commodity and style, like other amounts.
    pub const FULL_ZERO: AmountFormat = AmountFormat {
        zero_commodity: true,
        ..AmountFormat::DEFAULT
    };
    /// No costs or cost bases.
    pub const NO_COST: AmountFormat = AmountFormat {
        cost: false,
        cost_basis: false,
        ..AmountFormat::DEFAULT
    };
    pub const ONE_LINE: AmountFormat = AmountFormat {
        one_line: true,
        ..AmountFormat::DEFAULT
    };
    pub const ONE_LINE_NO_COST: AmountFormat = AmountFormat {
        one_line: true,
        ..AmountFormat::NO_COST
    };
    /// One line, no costs, no digit group marks.
    pub const MACHINE: AmountFormat = AmountFormat {
        digit_groups: false,
        ..AmountFormat::ONE_LINE_NO_COST
    };
}

impl Default for AmountFormat {
    fn default() -> AmountFormat {
        AmountFormat::DEFAULT
    }
}

/// A commodity symbol in double quotes if it contains a character that
/// would end an unquoted symbol, or begins with `(` or `/`, which would read
/// as an amount expression (quoteCommoditySymbolIfNeeded).
pub fn quote_commodity_symbol_if_needed(s: &str) -> Cow<'_, str> {
    if s.chars().any(super::is_nonsimple_commodity_char) || s.starts_with(['(', '/']) {
        Cow::Owned(format!("\"{s}\""))
    } else {
        Cow::Borrowed(s)
    }
}

/// An amount as text with its display style and the default format: a zero
/// as `0`, the missing amount as nothing (showAmount).
pub fn show_amount(a: &Amount) -> String {
    show_amount_b(&AmountFormat::DEFAULT, a).s
}

/// An amount as text with its display style and this format (showAmountB).
/// The missing amount shows as nothing.
pub fn show_amount_b(fmt: &AmountFormat, a: &Amount) -> Wide {
    if a.is_missing() {
        return Wide::default();
    }
    let zero = a.looks_zero() && !fmt.zero_commodity;
    let commodity = if zero {
        Cow::Borrowed("")
    } else if fmt.quotes {
        quote_commodity_symbol_if_needed(&a.commodity)
    } else {
        Cow::Borrowed(&*a.commodity)
    };
    let space = !commodity.is_empty() && a.style.spaced;
    let quantity = |out: &mut Wide| {
        if zero {
            out.push_str("0", 1);
        } else {
            let groups = a.style.digit_groups.filter(|_| fmt.digit_groups);
            push_quantity(out, fmt.force_decimal_mark, a, groups);
        }
    };
    // room for the commodity and the quantity of most amounts, so that the
    // text is allocated once
    let mut out = Wide {
        s: String::with_capacity(commodity.len() + 24),
        width: 0,
    };
    match a.style.side {
        Side::L => {
            if fmt.commodity {
                out.push_str(&commodity, display_width(&commodity));
                if space {
                    out.push_str(" ", 1);
                }
            }
            quantity(&mut out);
        }
        Side::R => {
            quantity(&mut out);
            if fmt.commodity {
                if space {
                    out.push_str(" ", 1);
                }
                out.push_str(&commodity, display_width(&commodity));
            }
        }
    }
    if fmt.cost_basis {
        out.push(&if fmt.ledger_lot_syntax {
            show_cost_basis_ledger(fmt, a)
        } else {
            show_cost_basis(fmt, a)
        });
    }
    if fmt.cost {
        out.push(&show_amount_cost_b(fmt, a));
    }
    if fmt.colour && a.is_negative() {
        // ansi-terminal's colours: dull red, then reset.
        out.s = format!("\x1b[31m{}\x1b[m", out.s);
    }
    out
}

/// The number part of an amount, rounded to its display precision, with
/// its digit group and decimal marks. With `disambiguate`, a number with
/// digit group marks and no decimals gets a trailing decimal mark
/// (showAmountQuantity).
pub fn show_amount_quantity(disambiguate: bool, a: &Amount) -> Wide {
    let mut out = Wide::default();
    push_quantity(&mut out, disambiguate, a, a.style.digit_groups);
    out
}

/// [`show_amount_quantity`], with these digit groups, added to `out`.
fn push_quantity(out: &mut Wide, disambiguate: bool, a: &Amount, groups: Option<&DigitGroups>) {
    let q = a.rounded_quantity();
    let places = usize::from(q.places());
    let digits = q.abs().mantissa().to_string();
    // the digits after as many zeros as make one digit before the places
    let zeros = (places + 1).saturating_sub(digits.len());
    let int_len = digits.len() + zeros - places;
    let (int_part, frac_digits) = if zeros > 0 {
        ("0", digits.as_str())
    } else {
        digits.split_at(int_len)
    };
    if q.is_negative() {
        out.push_str("-", 1);
    }
    push_digit_groups(out, groups, int_part);
    let shows_group_mark = match groups {
        Some(DigitGroups { sizes, .. }) => sizes.first().is_some_and(|&g| int_len > usize::from(g)),
        None => false,
    };
    if places > 0 || (shows_group_mark && disambiguate) {
        out.s.push(a.style.decimal_mark.unwrap_or('.'));
        out.s
            .extend(std::iter::repeat_n('0', zeros.saturating_sub(1)));
        out.push_str(frac_digits, 1 + places);
    }
}

/// Integer digits added to `out` with digit group marks between the groups,
/// sized from the right, the last size repeating; each mark counts one
/// column (applyDigitGroupStyle).
fn push_digit_groups(out: &mut Wide, groups: Option<&DigitGroups>, digits: &str) {
    let Some(DigitGroups { sep, sizes }) = groups.filter(|g| !g.sizes.is_empty()) else {
        out.push_str(digits, digits.len());
        return;
    };
    let last = sizes[sizes.len() - 1];
    let size = |k: usize| usize::from(sizes.get(k).copied().unwrap_or(last));
    // the number of groups after the first, and their digits
    let (mut n, mut grouped) = (0, 0);
    while digits.len() - grouped > size(n) {
        grouped += size(n);
        n += 1;
    }
    let (first, mut rest) = digits.split_at(digits.len() - grouped);
    out.push_str(first, first.len());
    for k in (0..n).rev() {
        let (group, tail) = rest.split_at(size(k));
        out.s.push(*sep);
        out.push_str(group, group.len() + 1);
        rest = tail;
    }
}

/// An amount's cost as ` @ UNITCOST` or ` @@ TOTALCOST`, the total cost
/// negative when the amount is; nothing if it has no cost
/// (showAmountCostB).
pub fn show_amount_cost_b(fmt: &AmountFormat, a: &Amount) -> Wide {
    match &a.cost {
        None => Wide::default(),
        Some(AmountCost::Unit(c)) => {
            let mut out = Wide::new(" @ ");
            out.push(&show_amount_b(fmt, c));
            out
        }
        Some(AmountCost::Total(c)) => {
            let mut out = Wide::new(" @@ ");
            let c = if a.is_negative() {
                -&**c
            } else {
                (**c).clone()
            };
            out.push(&show_amount_b(fmt, &c));
            out
        }
    }
}

/// An amount's cost basis as ` {DATE, "LABEL", COST}`, with the parts it
/// has; ` {}` if it has none; nothing if there is no cost basis
/// (showAmountCostBasisB).
fn show_cost_basis(fmt: &AmountFormat, a: &Amount) -> Wide {
    let Some(basis) = a.cost_basis.as_deref() else {
        return Wide::default();
    };
    let mut parts = Vec::new();
    if let Some(date) = basis.date {
        parts.push(Wide::new(&date.to_string()));
    }
    if let Some(label) = &basis.label {
        parts.push(Wide::new(&format!("\"{label}\"")));
    }
    if let Some(cost) = &basis.cost {
        parts.push(show_amount_b(fmt, cost));
    }
    let mut out = Wide::new(" {");
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            out.push_str(", ", 2);
        }
        out.push(part);
    }
    out.push_str("}", 1);
    out
}

/// An amount's cost basis in Ledger's lot syntax, ` [DATE] (LABEL) {COST}`
/// (showAmountCostBasisLedgerB).
fn show_cost_basis_ledger(fmt: &AmountFormat, a: &Amount) -> Wide {
    let Some(basis) = a.cost_basis.as_deref() else {
        return Wide::default();
    };
    if *basis == CostBasis::default() {
        return Wide::new(" {}");
    }
    let mut out = Wide::default();
    let mut enclose = |open: &str, inner: &Wide, close: &str| {
        out.push_str(open, 2);
        out.push(inner);
        out.push_str(close, 1);
    };
    if let Some(date) = basis.date {
        enclose(" [", &Wide::new(&date.to_string()), "]");
    }
    if let Some(label) = &basis.label {
        enclose(" (", &Wide::new(label), ")");
    }
    if let Some(cost) = &basis.cost {
        enclose(" {", &show_amount_b(fmt, cost), "}");
    }
    out
}

/// Two similar amounts as text, with enough decimal places that they differ
/// if they can: each at its full precision (up to 8 for an infinite
/// decimal), or failing that both at more decimal places, up to the larger
/// internal precision (showAmountsDistinctly).
pub fn show_amounts_distinctly(fmt: &AmountFormat, a: &Amount, b: &Amount) -> (String, String) {
    let full = |x: &Amount| {
        x.clone()
            .with_full_precision()
            .with_full_precision_up_to(None)
    };
    let (a0, b0) = (full(a), full(b));
    let (sa, sb) = (show_amount_b(fmt, &a0).s, show_amount_b(fmt, &b0).s);
    if sa != sb {
        return (sa, sb);
    }
    let start = a0.display_precision().max(b0.display_precision());
    let cap = a.internal_precision().max(b.internal_precision());
    let show = |x: &Amount, p| show_amount_b(fmt, &x.clone().with_precision(Precision::Fixed(p))).s;
    let mut p = start;
    loop {
        let (sa, sb) = (show(a, p), show(b, p));
        if sa != sb || p >= cap {
            return (sa, sb);
        }
        p += 1;
    }
}

/// A mixed amount as text with the default format (showMixedAmount).
pub fn show_mixed_amount(m: &MixedAmount) -> String {
    show_mixed_amount_b(&AmountFormat::DEFAULT, m).s
}

/// A mixed amount on one line, with costs but not cost bases
/// (showMixedAmountOneLine).
pub fn show_mixed_amount_one_line(m: &MixedAmount) -> String {
    let fmt = AmountFormat {
        cost: true,
        ..AmountFormat::ONE_LINE_NO_COST
    };
    show_mixed_amount_b(&fmt, m).s
}

/// A mixed amount as text: its amounts on one line separated by commas, or
/// one per line, padded on the left to the minimum width, and with amounts
/// beyond the maximum width replaced by a count of them (on one line, at
/// least one amount is shown); a zero as `0`, the missing amount as nothing
/// (showMixedAmountB). On several lines, the width is that of the first.
pub fn show_mixed_amount_b(fmt: &AmountFormat, m: &MixedAmount) -> Wide {
    if fmt.one_line {
        return show_one_line(fmt, m);
    }
    let lines = show_mixed_amount_lines_b(fmt, m);
    Wide {
        width: lines.first().map_or(0, |l| l.width),
        s: lines
            .into_iter()
            .map(|l| l.s)
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// The lines of a mixed amount shown on several lines (showMixedAmountLinesB).
pub fn show_mixed_amount_lines_b(fmt: &AmountFormat, m: &MixedAmount) -> Vec<Wide> {
    show_mixed_amount_lines_parts_b(fmt, m)
        .into_iter()
        .map(|(w, _)| w)
        .collect()
}

/// The lines of a mixed amount shown on several lines, each paired with the
/// amount in the same position, as upstream pairs them; that is not the
/// line's own amount after an elided one (showMixedAmountLinesPartsB).
pub fn show_mixed_amount_lines_parts_b(fmt: &AmountFormat, m: &MixedAmount) -> Vec<(Wide, Amount)> {
    lines_parts_with(fmt, m, |_, a| show_amount_b(fmt, a))
}

/// Like [`show_mixed_amount_lines_b`], reusing the text of each amount
/// that the last mixed amount shown this way had in the same position (as
/// the previous running total of a register usually does), and keeping
/// this one's amounts and texts in `last` for the next.
pub fn show_mixed_amount_lines_reusing(
    fmt: &AmountFormat,
    m: &MixedAmount,
    last: &mut Vec<(Amount, Wide)>,
) -> Vec<Wide> {
    let mut texts = Vec::with_capacity(last.len());
    let lines = lines_parts_with(fmt, m, |i, a| {
        let text = match last.get(i) {
            // equal amounts with the same decimal places show alike
            Some((b, text)) if b == a && b.quantity.places() == a.quantity.places() => text.clone(),
            _ => show_amount_b(fmt, a),
        };
        texts.push((a.clone(), text.clone()));
        text
    });
    *last = texts;
    lines.into_iter().map(|(w, _)| w).collect()
}

/// The lines of showMixedAmountLinesPartsB, each amount shown by `show`
/// with its position.
fn lines_parts_with(
    fmt: &AmountFormat,
    m: &MixedAmount,
    show: impl FnMut(usize, &Amount) -> Wide,
) -> Vec<(Wide, Amount)> {
    let amounts = ordered_amounts(fmt, m);
    let mut shown = displays_with(0, &amounts, show);
    if let Some(max) = fmt.max_width {
        let (short, long): (Vec<Display>, Vec<Display>) = shown
            .into_iter()
            .partition(|d| d.text.width as isize <= max);
        let last = short.last().map_or(0, |d| d.total);
        let elision = elision(Some(max), 0, long.len(), last);
        shown = short.into_iter().chain(elision).collect();
    }
    let widest = shown.iter().map(|d| d.text.width).max().unwrap_or(0);
    shown
        .into_iter()
        .map(|d| match fmt.min_width {
            Some(min) => {
                let pad = (widest as isize).max(min) as usize - d.text.width;
                let mut line = Wide {
                    s: " ".repeat(pad),
                    width: pad,
                };
                line.push(&d.text);
                line
            }
            None => d.text,
        })
        .zip(amounts)
        .collect()
}

/// The amounts of a mixed amount shown on one line, each with its amount,
/// without padding or elision (showMixedAmountOneLinePartsB).
pub fn show_mixed_amount_one_line_parts_b(
    fmt: &AmountFormat,
    m: &MixedAmount,
) -> Vec<(Wide, Amount)> {
    ordered_amounts(fmt, m)
        .into_iter()
        .map(|a| (show_amount_b(fmt, &a), a))
        .collect()
}

/// showMixedAmountOneLineB.
fn show_one_line(fmt: &AmountFormat, m: &MixedAmount) -> Wide {
    const SEP: &str = ", ";
    let amounts = ordered_amounts(fmt, m);
    let shown = displays(SEP.len(), fmt, &amounts);
    let n = shown.len();
    let shown: Vec<Display> = match fmt.max_width {
        None => shown,
        Some(max) => {
            // Each amount with the "N more.." that would follow it.
            let mut with_elisions: Vec<(Display, Option<Display>)> = shown
                .into_iter()
                .enumerate()
                .map(|(i, d)| {
                    let e = elision(None, SEP.len(), n - 1 - i, d.total);
                    (d, e)
                })
                .collect();
            // Keep the first, then drop from the end those whose total
            // (with their elision) exceeds the maximum.
            while with_elisions.len() > 1 {
                let (d, e) = with_elisions.last().expect("not empty");
                if max < e.as_ref().unwrap_or(d).total {
                    with_elisions.pop();
                } else {
                    break;
                }
            }
            let last_elision = with_elisions.last().and_then(|(_, e)| e.clone());
            with_elisions
                .into_iter()
                .map(|(d, _)| d)
                .chain(last_elision)
                .collect()
        }
    };
    let width = shown.last().map_or(0, |d| d.total);
    let min = fmt.min_width.unwrap_or(0);
    let pad = (min - width).max(0) as usize;
    let mut s = " ".repeat(pad);
    s += &shown
        .iter()
        .map(|d| d.text.s.as_str())
        .collect::<Vec<_>>()
        .join(SEP);
    Wide {
        s,
        width: width.max(min) as usize,
    }
}

/// The amounts to show: the non-zero ones (or a zero), or with
/// `zero_commodity`, all the zeros too when all are zero; then, unless
/// `zero_commodity` is set, in the commodity order if one is given, with
/// zeros for commodities not present (orderedAmounts, whose `if` takes the
/// ordering into its `else` branch).
fn ordered_amounts(fmt: &AmountFormat, m: &MixedAmount) -> Vec<Amount> {
    // stripping costs changes nothing when there are none, the usual case
    let costless = |a: &Amount| a.cost.is_none() && a.cost_basis.is_none();
    let m = if fmt.cost || m.amounts_raw().iter().all(costless) {
        Cow::Borrowed(m)
    } else {
        Cow::Owned(m.clone().strip_costs())
    };
    if fmt.zero_commodity {
        return m.amounts_preserving_zeros();
    }
    let amounts = m.amounts();
    match &fmt.commodity_order {
        None => amounts,
        Some(order) => order
            .iter()
            .map(|c| {
                let found = amounts.iter().find(|a| &a.commodity == c);
                found
                    .cloned()
                    .unwrap_or_else(|| Amount::null().with_commodity(*c))
            })
            .collect(),
    }
}

/// An amount's text, and the width of the whole display up to and including
/// it, separators included (AmountDisplay).
#[derive(Clone)]
struct Display {
    text: Wide,
    total: isize,
}

/// The amounts' texts with their running totals (amtDisplayList).
fn displays(sep: usize, fmt: &AmountFormat, amounts: &[Amount]) -> Vec<Display> {
    displays_with(sep, amounts, |_, a| show_amount_b(fmt, a))
}

/// Like [`displays`], each amount shown by `show` with its position.
fn displays_with(
    sep: usize,
    amounts: &[Amount],
    mut show: impl FnMut(usize, &Amount) -> Wide,
) -> Vec<Display> {
    let mut total = -(sep as isize);
    amounts
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let text = show(i, a);
            total += (text.width + sep) as isize;
            Display { text, total }
        })
        .collect()
}

/// `N more..` for N elided amounts, after a display whose total is
/// `last_total`, fitted to a maximum width if there is one; its width counts
/// the separator before it (elisionDisplay).
fn elision(max: Option<isize>, sep: usize, n: usize, last_total: isize) -> Option<Display> {
    if n == 0 {
        return None;
    }
    let full = format!("{n} more..");
    let full_len = (sep + 7 + n.to_string().len()) as isize;
    let (s, len) = match max {
        Some(m) if full_len > m => {
            // a width from --format can be anything; Haskell's Int wraps
            let kept: String = full
                .chars()
                .take(m.wrapping_sub(2).max(0) as usize)
                .collect();
            (kept + "..", m.min(full_len).max(2))
        }
        Some(m) => (full, m.min(full_len).max(2)),
        None => (full, full_len),
    };
    Some(Display {
        text: Wide {
            s,
            width: len as usize,
        },
        total: last_total + len,
    })
}

/// Two similar mixed amounts as text, with enough decimal places that they
/// differ if they can, as [`show_amounts_distinctly`] does
/// (showMixedAmountsDistinctly).
pub fn show_mixed_amounts_distinctly(
    fmt: &AmountFormat,
    a: &MixedAmount,
    b: &MixedAmount,
) -> (String, String) {
    let full = |x: &MixedAmount| {
        x.clone()
            .with_full_precision()
            .with_full_precision_up_to(None)
    };
    let (a0, b0) = (full(a), full(b));
    let (sa, sb) = (
        show_mixed_amount_b(fmt, &a0).s,
        show_mixed_amount_b(fmt, &b0).s,
    );
    if sa != sb {
        return (sa, sb);
    }
    let most =
        |x: &MixedAmount, f: fn(&Amount) -> u8| x.amounts_raw().iter().map(f).max().unwrap_or(0);
    let start = most(&a0, Amount::display_precision).max(most(&b0, Amount::display_precision));
    let cap = most(a, Amount::internal_precision).max(most(b, Amount::internal_precision));
    let show = |x: &MixedAmount, p| {
        show_mixed_amount_b(fmt, &x.clone().with_precision(Precision::Fixed(p))).s
    };
    let mut p = start;
    loop {
        let (sa, sb) = (show(a, p), show(b, p));
        if sa != sb || p >= cap {
            return (sa, sb);
        }
        p += 1;
    }
}

pub(crate) mod tests {
    use super::super::{eur, gbp, usd};
    use super::*;
    use crate::decimal::Decimal;
    #[cfg(test)]
    use {
        super::super::{AmountStyle, hrs, num, per},
        crate::dates::Day,
        std::sync::Arc,
    };

    fn d(s: &str) -> Decimal {
        s.parse::<Decimal>().unwrap().normalize()
    }

    fn mixed<const N: usize>(amounts: [Amount; N]) -> MixedAmount {
        MixedAmount::from_amounts(amounts)
    }

    #[cfg(test)]
    fn styled(q: &str, style: AmountStyle) -> Amount {
        Amount {
            commodity: Sym::from("X"),
            style,
            ..num(q.parse().unwrap())
        }
    }

    #[cfg(test)]
    fn groups(sep: char, sizes: &[u8]) -> Option<&'static DigitGroups> {
        Some(
            DigitGroups {
                sep,
                sizes: sizes.to_vec(),
            }
            .intern(),
        )
    }

    #[cfg(test)]
    fn style(precision: u8) -> AmountStyle {
        AmountStyle {
            precision: Precision::Fixed(precision),
            ..AmountStyle::default()
        }
    }

    // tests_Amount "showAmount".
    #[cfg_attr(test, test)]
    pub(crate) fn show_amount_of_zero() {
        assert_eq!(show_amount(&(&usd(d("0")) + &gbp(d("0")))), "0");
    }

    // tests_Amount "showMixedAmount".
    #[cfg_attr(test, test)]
    pub(crate) fn show_mixed_amounts() {
        assert_eq!(
            show_mixed_amount(&MixedAmount::from_amount(usd(d("1")))),
            "$1.00"
        );
        assert_eq!(
            show_mixed_amount(&MixedAmount::from_amount(usd(d("1")).at(eur(d("2"))))),
            "$1.00 @ €2.00"
        );
        assert_eq!(
            show_mixed_amount(&MixedAmount::from_amount(usd(d("0")))),
            "0"
        );
        assert_eq!(show_mixed_amount(&MixedAmount::null()), "0");
        assert_eq!(show_mixed_amount(&MixedAmount::missing()), "");
    }

    // tests_Amount "showMixedAmountWithoutCost".
    #[cfg_attr(test, test)]
    pub(crate) fn show_mixed_amount_without_cost() {
        let a = usd(d("1")).at(eur(d("2")));
        let fmt = AmountFormat::NO_COST;
        assert_eq!(
            show_mixed_amount_b(&fmt, &MixedAmount::from_amount(a.clone())).s,
            "$1.00"
        );
        assert_eq!(show_mixed_amount_b(&fmt, &mixed([a.clone(), -a])).s, "0");
    }

    #[cfg(test)]
    /// All thirteen fields, in upstream's order.
    #[allow(clippy::too_many_arguments)]
    fn format(
        commodity: bool,
        zero_commodity: bool,
        digit_groups: bool,
        force_decimal_mark: bool,
        one_line: bool,
        min_width: Option<isize>,
        cost: bool,
        cost_basis: bool,
        colour: bool,
        quotes: bool,
        ledger_lot_syntax: bool,
    ) -> AmountFormat {
        AmountFormat {
            commodity,
            zero_commodity,
            commodity_order: None,
            digit_groups,
            force_decimal_mark,
            one_line,
            min_width,
            max_width: None,
            cost,
            cost_basis,
            colour,
            quotes,
            ledger_lot_syntax,
        }
    }

    // The field values of defaultFmt, fullZeroFmt, noCostFmt, oneLineFmt,
    // oneLineNoCostFmt and machineFmt.
    #[test]
    fn presets() {
        let (t, f) = (true, false);
        let z = Some(0);
        assert_eq!(
            AmountFormat::default(),
            format(t, f, t, f, f, z, t, t, f, t, f)
        );
        assert_eq!(
            AmountFormat::DEFAULT,
            format(t, f, t, f, f, z, t, t, f, t, f)
        );
        assert_eq!(
            AmountFormat::FULL_ZERO,
            format(t, t, t, f, f, z, t, t, f, t, f)
        );
        assert_eq!(
            AmountFormat::NO_COST,
            format(t, f, t, f, f, z, f, f, f, t, f)
        );
        assert_eq!(
            AmountFormat::ONE_LINE,
            format(t, f, t, f, t, z, t, t, f, t, f)
        );
        assert_eq!(
            AmountFormat::ONE_LINE_NO_COST,
            format(t, f, t, f, t, z, f, f, f, t, f)
        );
        assert_eq!(
            AmountFormat::MACHINE,
            format(t, f, f, f, t, z, f, f, f, t, f)
        );
    }

    #[test]
    fn quantities() {
        let show =
            |q: &str, s: AmountStyle| show_amount_b(&AmountFormat::FULL_ZERO, &styled(q, s)).s;
        assert_eq!(show("0.05", style(2)), "X0.05");
        assert_eq!(show("-0.05", style(2)), "X-0.05");
        assert_eq!(show("-0.004", style(2)), "X0.00");
        assert_eq!(show("0.005", style(2)), "X0.00");
        assert_eq!(show("0.015", style(2)), "X0.02");
        assert_eq!(show("12.5", style(0)), "X12");
        assert_eq!(show("1.5", style(3)), "X1.500");
        let natural = AmountStyle {
            precision: Precision::Natural,
            ..style(0)
        };
        assert_eq!(show("1.50", natural), "X1.50");
        let comma = AmountStyle {
            decimal_mark: Some(','),
            ..style(2)
        };
        assert_eq!(show("1.5", comma), "X1,50");
        let none = AmountStyle {
            decimal_mark: None,
            ..style(1)
        };
        assert_eq!(show("1.5", none), "X1.5");
        // A zero hides its commodity and style unless asked not to.
        assert_eq!(show_amount(&styled("0.004", style(2))), "0");
        assert_eq!(show_amount(&styled("-0.005", style(2))), "0");
    }

    #[test]
    fn digit_groups() {
        let grouped = |sep, sizes: &[u8], p| AmountStyle {
            digit_groups: groups(sep, sizes),
            decimal_mark: Some(if sep == '.' { ',' } else { '.' }),
            ..style(p)
        };
        let show = |q: &str, s: AmountStyle| show_amount_b(&AmountFormat::DEFAULT, &styled(q, s));
        assert_eq!(
            show("1234567.891", grouped(',', &[3], 3)).s,
            "X1,234,567.891"
        );
        assert_eq!(show("1234567", grouped('.', &[3], 2)).s, "X1.234.567,00");
        assert_eq!(show("1234567", grouped(',', &[3, 2], 0)).s, "X12,34,567");
        assert_eq!(show("-1234", grouped(',', &[3], 0)).s, "X-1,234");
        assert_eq!(show("999", grouped(',', &[3], 0)).s, "X999");
        assert_eq!(
            show("1234", grouped('\u{202f}', &[3], 0)),
            Wide {
                s: "X1\u{202f}234".into(),
                width: 6
            }
        );
        let no_sizes = AmountStyle {
            digit_groups: groups(',', &[]),
            ..style(0)
        };
        assert_eq!(show("1234", no_sizes).s, "X1234");
        // Without digit groups in the format.
        let fmt = AmountFormat {
            digit_groups: false,
            ..AmountFormat::DEFAULT
        };
        assert_eq!(
            show_amount_b(&fmt, &styled("1234", grouped(',', &[3], 0))).s,
            "X1234"
        );
        // A trailing decimal mark disambiguates a grouped integer.
        let force = AmountFormat {
            force_decimal_mark: true,
            ..AmountFormat::DEFAULT
        };
        assert_eq!(
            show_amount_b(&force, &styled("1000", grouped(',', &[3], 0))),
            Wide {
                s: "X1,000.".into(),
                width: 7
            }
        );
        assert_eq!(
            show_amount_b(&force, &styled("100", grouped(',', &[3], 0))).s,
            "X100"
        );
        assert_eq!(show_amount_b(&force, &styled("1000", style(0))).s, "X1000");
        assert_eq!(
            show_amount_quantity(true, &styled("-1000", grouped('.', &[3], 0))).s,
            "-1.000,"
        );
    }

    #[test]
    fn commodity_placement_and_quoting() {
        assert_eq!(show_amount(&per(d("5"))), "5.0 %");
        assert_eq!(show_amount(&hrs(d("1.5"))), "1.50h");
        let spaced = |c: &str| Amount {
            commodity: Sym::from(c),
            style: AmountStyle {
                spaced: true,
                ..style(0)
            },
            ..num(d("3"))
        };
        assert_eq!(show_amount(&spaced("AAPL")), "AAPL 3");
        assert_eq!(show_amount(&spaced("AAPL 2")), "\"AAPL 2\" 3");
        assert_eq!(show_amount(&spaced("x1")), "\"x1\" 3");
        assert_eq!(show_amount(&spaced("(X")), "\"(X\" 3");
        assert_eq!(show_amount(&spaced("/X")), "\"/X\" 3");
        assert_eq!(show_amount(&spaced("")), "3");
        let unquoted = AmountFormat {
            quotes: false,
            ..AmountFormat::DEFAULT
        };
        assert_eq!(show_amount_b(&unquoted, &spaced("AAPL 2")).s, "AAPL 2 3");
        let bare = AmountFormat {
            commodity: false,
            ..AmountFormat::DEFAULT
        };
        assert_eq!(show_amount_b(&bare, &spaced("AAPL")).s, "3");
        assert_eq!(show_amount_b(&AmountFormat::DEFAULT, &usd(d("1"))).width, 5);
        assert_eq!(
            show_amount_b(&AmountFormat::DEFAULT, &Amount::missing()),
            Wide::default()
        );
        assert_eq!(quote_commodity_symbol_if_needed("€"), "€");
        assert_eq!(quote_commodity_symbol_if_needed("a\"b"), "\"a\"b\"");
    }

    #[test]
    fn costs() {
        assert_eq!(
            show_amount(&usd(d("1")).at_total(eur(d("2")))),
            "$1.00 @@ €2.00"
        );
        assert_eq!(
            show_amount(&usd(d("-1")).at_total(eur(d("2")))),
            "$-1.00 @@ €-2.00"
        );
        assert_eq!(show_amount(&usd(d("-1")).at(eur(d("2")))), "$-1.00 @ €2.00");
        assert_eq!(show_amount(&usd(d("0")).at(eur(d("2")))), "0 @ €2.00");
        let fmt = AmountFormat::NO_COST;
        assert_eq!(show_amount_b(&fmt, &usd(d("1")).at(eur(d("2")))).s, "$1.00");
        assert_eq!(
            show_amount_cost_b(&AmountFormat::DEFAULT, &usd(d("1")).at(eur(d("2")))),
            Wide {
                s: " @ €2.00".into(),
                width: 8
            }
        );
    }

    #[test]
    fn cost_bases() {
        let basis = |date, label: Option<&str>, cost: Option<Amount>| CostBasis {
            date,
            label: label.map(Arc::from),
            cost: cost.map(Box::new),
        };
        let lot = |b: CostBasis| Amount {
            cost_basis: Some(Box::new(b)),
            ..usd(d("1"))
        };
        let day = Day::from_ymd(2024, 1, 2);
        let full = lot(basis(day, Some("lot 1"), Some(eur(d("3")))));
        assert_eq!(show_amount(&full), "$1.00 {2024-01-02, \"lot 1\", €3.00}");
        assert_eq!(
            show_amount(&lot(basis(None, None, Some(eur(d("3")))))),
            "$1.00 {€3.00}"
        );
        assert_eq!(show_amount(&lot(CostBasis::default())), "$1.00 {}");
        let ledger = AmountFormat {
            ledger_lot_syntax: true,
            ..AmountFormat::DEFAULT
        };
        assert_eq!(
            show_amount_b(&ledger, &full).s,
            "$1.00 [2024-01-02] (lot 1) {€3.00}"
        );
        assert_eq!(
            show_amount_b(&ledger, &lot(CostBasis::default())).s,
            "$1.00 {}"
        );
        assert_eq!(show_amount_b(&AmountFormat::NO_COST, &full).s, "$1.00");
        let with_cost = full.at(gbp(d("4")));
        assert_eq!(
            show_amount(&with_cost),
            "$1.00 {2024-01-02, \"lot 1\", €3.00} @ £4.00"
        );
    }

    #[test]
    fn colour() {
        let fmt = AmountFormat {
            colour: true,
            ..AmountFormat::DEFAULT
        };
        assert_eq!(
            show_amount_b(&fmt, &usd(d("-1"))),
            Wide {
                s: "\x1b[31m$-1.00\x1b[m".into(),
                width: 6
            }
        );
        assert_eq!(show_amount_b(&fmt, &usd(d("1"))).s, "$1.00");
        // A negative amount that shows as zero is still coloured.
        assert_eq!(
            show_amount_b(&fmt, &styled("-0.001", style(2))).s,
            "\x1b[31m0\x1b[m"
        );
    }

    #[cfg(test)]
    fn three() -> MixedAmount {
        mixed([usd(d("1")), eur(d("2")), gbp(d("3"))])
    }

    #[test]
    fn one_line() {
        let fmt = |min, max| AmountFormat {
            min_width: min,
            max_width: max,
            ..AmountFormat::ONE_LINE
        };
        let show = |min, max| show_mixed_amount_b(&fmt(min, max), &three());
        assert_eq!(
            show(None, None),
            Wide {
                s: "$1.00, £3.00, €2.00".into(),
                width: 19
            }
        );
        assert_eq!(
            show(Some(22), None),
            Wide {
                s: "   $1.00, £3.00, €2.00".into(),
                width: 22
            }
        );
        assert_eq!(show(None, Some(19)).s, "$1.00, £3.00, €2.00");
        assert_eq!(
            show(None, Some(18)),
            Wide {
                s: "$1.00, 2 more..".into(),
                width: 15
            }
        );
        assert_eq!(show(None, Some(15)).s, "$1.00, 2 more..");
        // At least one amount is shown.
        assert_eq!(
            show(None, Some(3)),
            Wide {
                s: "$1.00, 2 more..".into(),
                width: 15
            }
        );
        assert_eq!(
            show(Some(17), Some(3)),
            Wide {
                s: "  $1.00, 2 more..".into(),
                width: 17
            }
        );
        let two = mixed([usd(d("1")), eur(d("2"))]);
        assert_eq!(
            show_mixed_amount_b(&fmt(None, Some(12)), &two).s,
            "$1.00, €2.00"
        );
        assert_eq!(
            show_mixed_amount_b(&fmt(None, Some(11)), &two).s,
            "$1.00, 1 more.."
        );
        assert_eq!(
            show_mixed_amount_b(&fmt(None, None), &MixedAmount::null()),
            Wide {
                s: "0".into(),
                width: 1
            }
        );
        assert_eq!(
            show_mixed_amount_one_line(&mixed([usd(d("1")).at(eur(d("2")))])),
            "$1.00 @ €2.00"
        );
    }

    #[test]
    fn several_lines() {
        let fmt = |min, max| AmountFormat {
            min_width: min,
            max_width: max,
            ..AmountFormat::DEFAULT
        };
        let m = mixed([usd(d("1")), eur(d("2")), gbp(d("300"))]);
        assert_eq!(
            show_mixed_amount_b(&fmt(Some(0), None), &m),
            Wide {
                s: "  $1.00\n£300.00\n  €2.00".into(),
                width: 7
            }
        );
        assert_eq!(
            show_mixed_amount_b(&fmt(Some(9), None), &m).s,
            "    $1.00\n  £300.00\n    €2.00"
        );
        assert_eq!(
            show_mixed_amount_b(&fmt(None, None), &m),
            Wide {
                s: "$1.00\n£300.00\n€2.00".into(),
                width: 5
            }
        );
        assert_eq!(
            show_mixed_amount_b(&fmt(Some(0), Some(5)), &m).s,
            "$1.00\n€2.00\n1 m.."
        );
        assert_eq!(
            show_mixed_amount_b(&fmt(Some(0), Some(4)), &m),
            Wide {
                s: "3 ..".into(),
                width: 4
            }
        );
        assert_eq!(
            show_mixed_amount_b(&fmt(Some(0), Some(1)), &m),
            Wide {
                s: "..".into(),
                width: 2
            }
        );
        let lines = show_mixed_amount_lines_parts_b(&fmt(Some(0), Some(5)), &m);
        let paired: Vec<_> = lines
            .iter()
            .map(|(w, a)| (w.s.as_str(), a.commodity.to_string()))
            .collect();
        assert_eq!(
            paired,
            [
                ("$1.00", "$".to_string()),
                ("€2.00", "£".into()),
                ("1 m..", "€".into())
            ]
        );
    }

    #[test]
    fn commodity_order_and_zeros() {
        let order = |cs: &[&str]| Some(cs.iter().map(|c| Sym::from(*c)).collect());
        let fmt = AmountFormat {
            commodity_order: order(&["€", "$", "¥"]),
            ..AmountFormat::ONE_LINE
        };
        assert_eq!(show_mixed_amount_b(&fmt, &three()).s, "€2.00, $1.00, 0");
        // With zero_commodity, upstream's orderedAmounts ignores the order.
        let zeros = AmountFormat {
            zero_commodity: true,
            ..fmt.clone()
        };
        assert_eq!(
            show_mixed_amount_b(&zeros, &three()).s,
            "$1.00, £3.00, €2.00"
        );
        let all_zero = mixed([usd(d("0")), eur(d("0"))]);
        assert_eq!(show_mixed_amount_b(&zeros, &all_zero).s, "$0.00, €0.00");
        assert_eq!(
            show_mixed_amount_b(&AmountFormat::ONE_LINE, &all_zero).s,
            "0"
        );
        let parts = show_mixed_amount_one_line_parts_b(&fmt, &three());
        assert_eq!(
            parts.iter().map(|(w, _)| w.s.as_str()).collect::<Vec<_>>(),
            ["€2.00", "$1.00", "0"]
        );
        assert_eq!(&*parts[2].1.commodity, "¥");
    }

    #[test]
    fn distinct_texts() {
        let fmt = AmountFormat::DEFAULT;
        let a = usd(d("1"))
            .with_quantity(d("1.0001"))
            .with_precision(Precision::Fixed(2));
        let b = usd(d("1"))
            .with_quantity(d("1.0002"))
            .with_precision(Precision::Fixed(2));
        assert_eq!(
            show_amounts_distinctly(&fmt, &a, &b),
            ("$1.0001".into(), "$1.0002".into())
        );
        let same = show_amounts_distinctly(&fmt, &a, &a);
        assert_eq!(same, ("$1.0001".into(), "$1.0001".into()));
        let third = num(&d("1") / &d("3"));
        let near = num(&third.quantity + &Decimal::new(1.into(), 20));
        let (s1, s2) = show_amounts_distinctly(&fmt, &third, &near);
        assert_eq!((s1.len(), s2.len()), (22, 22));
        assert_ne!(s1, s2);
        let (m1, m2) = show_mixed_amounts_distinctly(
            &fmt,
            &MixedAmount::from_amount(a),
            &MixedAmount::from_amount(b),
        );
        assert_eq!((m1.as_str(), m2.as_str()), ("$1.0001", "$1.0002"));
    }
}
