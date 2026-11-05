//! Amount parsing (the amount parsers of Hledger.Read.Common): numbers with
//! their decimal and digit group marks, commodity symbols, signs, costs, lot
//! cost bases, balance assertions, and amount expressions.
//!
//! Like the other parsers here, these work on a [`Cursor`], and one that fails
//! without consuming input leaves the cursor where it started.

use std::collections::BTreeMap;
use std::sync::Arc;

use dashu_int::IBig;

use super::common::datep;
use crate::amount::{
    Amount, AmountCost, AmountStyle, CostBasis, DigitGroups, Precision, Side, Sym,
    is_nonsimple_commodity_char,
};
use crate::decimal::Decimal;
use crate::model::{BalanceAssertion, Commodity, SourcePos};
use crate::parse::{Cursor, Item, PResult, ParseError, is_non_newline_space};

/// The kinds of amount, which are parsed a little differently
/// (AmountParseKind).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AmountKind {
    /// An amount in the data: a posting's, a cost, a balance assertion's, a
    /// market price. If a decimal-mark directive is in effect, its number
    /// must use only that decimal mark.
    Ordinary,
    /// An auto posting rule's `*AMT` multiplier: like an ordinary amount, but
    /// the default commodity is not applied to it.
    Multiplier,
    /// The sample amount of a commodity or D directive, declaring a display
    /// style, which may use another decimal mark than a decimal-mark
    /// directive's.
    Style,
}

/// What the amount parsers need from the journal parse state.
#[derive(Clone, Copy, Debug, Default)]
pub struct AmountCtx<'s> {
    /// The decimal mark declared by a decimal-mark directive, which numbers
    /// must use (jparsedecimalmark).
    pub decimal_mark: Option<char>,
    /// The default commodity and its style, from the latest D directive
    /// (jparsedefaultcommodity).
    pub default_commodity: Option<&'s (Sym, Arc<AmountStyle>)>,
    /// The commodities declared so far (jdeclaredcommodities).
    pub commodities: Option<&'s BTreeMap<Sym, Commodity>>,
    /// The default year, for lot dates without one (jparsedefaultyear).
    pub year: Option<i64>,
}

impl AmountCtx<'_> {
    /// The style declared for this commodity by its commodity directive, or
    /// else by the D directive (journalAmountStyleFor).
    fn style_for(&self, commodity: &str) -> Option<&AmountStyle> {
        self.commodities
            .and_then(|cs| cs.get(commodity))
            .and_then(|c| c.format.as_ref())
            .or(self.default_commodity.map(|(_, s)| &**s))
    }

    /// How to read an ambiguous number in this commodity: by the declared
    /// decimal mark, or else by the commodity's declared style.
    fn number_hint(&self, commodity: &str) -> Option<NumberHint> {
        match self.decimal_mark {
            Some(d) => Some(NumberHint {
                decimal_mark: Some(d),
                group_mark: None,
            }),
            None => self.style_for(commodity).map(|s| NumberHint {
                decimal_mark: s.decimal_mark,
                group_mark: s.digit_groups.as_ref().map(|g| g.sep),
            }),
        }
    }
}

/// What a suggested style says about reading numbers: its decimal mark and
/// its digit group mark.
#[derive(Clone, Copy, Debug)]
pub struct NumberHint {
    pub decimal_mark: Option<char>,
    pub group_mark: Option<char>,
}

/// Whether a character can be a decimal mark (isDecimalMark).
pub fn is_decimal_mark(c: char) -> bool {
    c == '.' || c == ','
}

/// Whether a character can separate digit groups: a period, comma,
/// apostrophe, underscore, or one of several spaces
/// (isDigitSeparatorChar).
fn is_digit_separator_char(c: char) -> bool {
    matches!(
        c,
        '.' | ','
            | '\''
            | '_'
            | ' '
            | '\u{a0}'
            | '\u{2002}'
            | '\u{2003}'
            | '\u{2008}'
            | '\u{2009}'
            | '\u{202f}'
            | '\u{205f}'
    )
}

/// Whether a character can start a commodity symbol written before a number.
fn starts_commodity_symbol(c: char) -> bool {
    c == '"' || !is_nonsimple_commodity_char(c)
}

/// The next character after any spaces, and whether there were spaces
/// (peekAfterSpaces).
fn peek_after_spaces(c: &Cursor) -> (bool, Option<char>) {
    let rest = c.rest();
    let after = rest.trim_start_matches(is_non_newline_space);
    (after.len() < rest.len(), after.chars().next())
}

/// A number as written, its marks not yet interpreted (RawNumber and
/// AmbiguousNumber). Digit groups are the digits as written.
#[derive(Clone, Debug, PartialEq, Eq)]
enum RawNumber<'a> {
    /// Digits, maybe with a decimal mark and more digits: `100`, `100.`, `.5`.
    NoSeparators(&'a str, Option<(char, &'a str)>),
    /// Digit groups separated by a mark, maybe with a decimal mark and more
    /// digits: `1,000.5`, `1 000`.
    WithSeparators(char, Vec<&'a str>, Option<(char, &'a str)>),
    /// Two digit groups separated by a period or comma, which is either a
    /// decimal mark or a digit group mark: `1,000`.
    Ambiguous(&'a str, char, &'a str),
}

/// One or more digits (digitgroupp).
fn digitgroupp<'a>(c: &mut Cursor<'a>) -> PResult<&'a str> {
    let digits = c.take_while(|ch| ch.is_ascii_digit());
    if digits.is_empty() {
        return Err(c.unexpected(&[Item::label("digits")]));
    }
    Ok(digits)
}

/// A number's digits and marks, with the mistakes that can be seen without
/// knowing which mark is the decimal mark reported (rawnumberp).
fn rawnumberp<'a>(c: &mut Cursor<'a>) -> PResult<RawNumber<'a>> {
    let raw = match c.peek() {
        Some(d) if is_decimal_mark(d) => {
            c.bump();
            RawNumber::NoSeparators("", Some((d, digitgroupp(c)?)))
        }
        Some(d) if d.is_ascii_digit() => leading_digits(c),
        _ => return Err(c.unexpected(&[Item::label("number")])),
    };
    if c.peek().is_some_and(is_decimal_mark) {
        return Err(ParseError::fail_at(
            c.src,
            c.pos,
            "invalid number (invalid use of separator)",
        ));
    }
    if c.peek() == Some(' ') && c.peek2().is_some_and(|d| d.is_ascii_digit()) {
        return Err(ParseError::fail_at(
            c.src,
            c.pos + 1,
            "invalid number (excessive trailing digits)",
        ));
    }
    Ok(raw)
}

/// A number starting with a digit: digit groups and maybe a decimal mark.
fn leading_digits<'a>(c: &mut Cursor<'a>) -> RawNumber<'a> {
    let grp1 = c.take_while(|ch| ch.is_ascii_digit());
    let start = *c;
    if let Some(sep) = c.bump().filter(|&s| is_digit_separator_char(s)) {
        let grp2 = c.take_while(|ch| ch.is_ascii_digit());
        if !grp2.is_empty() {
            let mut groups = vec![grp1, grp2];
            loop {
                let before = *c;
                let next = if c.eat(sep) {
                    c.take_while(|ch| ch.is_ascii_digit())
                } else {
                    ""
                };
                if next.is_empty() {
                    *c = before;
                    break;
                }
                groups.push(next);
            }
            if let Some(d) = c.peek().filter(|&d| is_decimal_mark(d) && d != sep) {
                c.bump();
                let decimals = c.take_while(|ch| ch.is_ascii_digit());
                return RawNumber::WithSeparators(sep, groups, Some((d, decimals)));
            }
            return if groups.len() == 2 && is_decimal_mark(sep) {
                RawNumber::Ambiguous(groups[0], sep, groups[1])
            } else {
                RawNumber::WithSeparators(sep, groups, None)
            };
        }
    }
    *c = start;
    match c.peek() {
        Some(d) if is_decimal_mark(d) => {
            c.bump();
            RawNumber::NoSeparators(grp1, Some((d, "")))
        }
        _ => RawNumber::NoSeparators(grp1, None),
    }
}

/// An exponent (`e`, an optional sign, digits) if one follows the number
/// (optionalexponentp). An `e` not followed by a valid exponent is left
/// alone.
fn optional_exponentp(c: &mut Cursor) -> Option<i64> {
    let start = *c;
    if !matches!(c.peek(), Some('e' | 'E')) {
        return None;
    }
    c.bump();
    let negative = signp(c);
    let digits = c.take_while(|ch| ch.is_ascii_digit());
    if digits.is_empty() {
        *c = start;
        return None;
    }
    let n = super::common::read_digits(digits);
    Some(if negative { -n } else { n })
}

/// A minus or plus sign and any spaces after it, if there is one; whether it
/// was a minus (signp).
fn signp(c: &mut Cursor) -> bool {
    match c.peek() {
        Some(s @ ('-' | '+')) => {
            c.bump();
            c.skip_spaces();
            s == '-'
        }
        _ => false,
    }
}

/// The decimal number, its precision, its decimal mark and its digit groups
/// that a raw number means, given the decimal mark it must use, if any, and
/// a style suggesting how to read an ambiguous number (interpretRawNumber).
fn interpret_raw_number(
    required_mark: Option<char>,
    hint: Option<NumberHint>,
    raw: RawNumber,
    exponent: Option<i64>,
) -> Result<(Decimal, u8, Option<char>, Option<DigitGroups>), String> {
    let raw = match raw {
        RawNumber::Ambiguous(grp1, sep, grp2) => disambiguate_number(hint, grp1, sep, grp2),
        raw => raw,
    };
    if let Some(d) = required_mark {
        let declared = |msg: String| {
            Err(format!(
                "invalid number: the decimal mark is declared to be {}, {msg}",
                show_char(d)
            ))
        };
        let (decimal_mark, group_mark) = match &raw {
            RawNumber::NoSeparators(_, dec) => (dec.map(|(m, _)| m), None),
            RawNumber::WithSeparators(sep, _, dec) => (dec.map(|(m, _)| m), Some(*sep)),
            RawNumber::Ambiguous(..) => (None, None),
        };
        match decimal_mark {
            Some(m) if m != d => {
                return declared(format!(
                    "but this number's decimal mark is {}",
                    show_char(m)
                ));
            }
            _ if group_mark == Some(d) => {
                return declared("so it can appear only once".to_string());
            }
            _ => {}
        }
    }
    from_raw_number(raw, exponent)
}

/// A character as Haskell's `show` writes it: `'.'`.
fn show_char(c: char) -> String {
    format!("'{c}'")
}

/// An ambiguous number read as a decimal number if its mark can be a decimal
/// mark and the suggested style allows that, otherwise as digit groups
/// (disambiguateNumber).
fn disambiguate_number<'a>(
    hint: Option<NumberHint>,
    grp1: &'a str,
    sep: char,
    grp2: &'a str,
) -> RawNumber<'a> {
    let valid_decimal_mark = |h: NumberHint| match (h.decimal_mark, h.group_mark) {
        (Some(d), _) => d == sep,
        (None, Some(g)) => g != sep,
        (None, None) => true,
    };
    if is_decimal_mark(sep) && hint.is_none_or(valid_decimal_mark) {
        RawNumber::NoSeparators(grp1, Some((sep, grp2)))
    } else {
        RawNumber::WithSeparators(sep, vec![grp1, grp2], None)
    }
}

/// A raw number's value, precision, decimal mark and digit groups
/// (fromRawNumber).
fn from_raw_number(
    raw: RawNumber,
    exponent: Option<i64>,
) -> Result<(Decimal, u8, Option<char>, Option<DigitGroups>), String> {
    let (int_groups, decimals, groups) = match raw {
        RawNumber::WithSeparators(..) if exponent.is_some() => {
            return Err(
                "invalid number: digit separators and exponents may not be used together"
                    .to_string(),
            );
        }
        RawNumber::NoSeparators(digits, decimals) => (vec![digits], decimals, None),
        RawNumber::WithSeparators(sep, groups, decimals) => {
            let mut sizes: Vec<u8> = groups.iter().map(|g| g.len() as u8).collect();
            if sizes.len() >= 2 && sizes[0] < sizes[1] {
                sizes.remove(0);
            }
            sizes.reverse();
            (groups, decimals, Some(DigitGroups { sep, sizes }))
        }
        RawNumber::Ambiguous(..) => unreachable!("disambiguated before"),
    };
    let decimal_digits = decimals.map_or("", |(_, d)| d);
    let mut digits = String::with_capacity(32);
    int_groups.iter().for_each(|g| digits.push_str(g));
    digits.push_str(decimal_digits);
    let mantissa = parse_digits(&digits);
    let precision = decimal_digits.len() as i64 - exponent.unwrap_or(0);
    let quantity = if precision < 0 {
        let shift = usize::try_from(-precision).unwrap_or(usize::MAX);
        (Decimal::new(mantissa * IBig::from(10u8).pow(shift), 0), 0)
    } else if precision < 256 {
        (Decimal::new(mantissa, precision as u8), precision as u8)
    } else {
        return Err(
            "invalid number: numbers with more than 255 decimal places are currently not supported"
                .to_string(),
        );
    };
    Ok((quantity.0, quantity.1, decimals.map(|(m, _)| m), groups))
}

/// The natural number these ASCII digits make (none make 0).
fn parse_digits(digits: &str) -> IBig {
    if digits.len() <= 18 {
        IBig::from(
            digits
                .bytes()
                .fold(0u64, |n, b| n * 10 + u64::from(b - b'0')),
        )
    } else {
        digits.parse().expect("ASCII digits")
    }
}

/// A number, with an optional sign and exponent: its value, precision,
/// decimal mark and digit groups, reading an ambiguous number by the
/// suggested style (numberp).
pub fn numberp(
    c: &mut Cursor,
    hint: Option<NumberHint>,
) -> PResult<(Decimal, u8, Option<char>, Option<DigitGroups>)> {
    let negative = signp(c);
    let raw = rawnumberp(c)?;
    let exponent = optional_exponentp(c);
    let (q, p, d, g) = interpret_raw_number(None, hint, raw, exponent)
        .map_err(|msg| ParseError::fail_at(c.src, c.pos, msg))?;
    Ok((if negative { -q } else { q }, p, d, g))
}

/// A commodity symbol: in double quotes, or a run of characters that need no
/// quotes (commoditysymbolp).
pub fn commodity_symbolp<'a>(c: &mut Cursor<'a>) -> PResult<&'a str> {
    if c.peek() == Some('"') {
        quoted_commodity_symbolp(c)
    } else {
        let s = c.take_while(|ch| !is_nonsimple_commodity_char(ch));
        if s.is_empty() {
            return Err(c.unexpected(&[Item::label("commodity symbol")]));
        }
        Ok(s)
    }
}

/// A commodity symbol in double quotes, which can contain anything but a
/// semicolon, newline or double quote (quotedcommoditysymbolp).
fn quoted_commodity_symbolp<'a>(c: &mut Cursor<'a>) -> PResult<&'a str> {
    c.bump();
    let s = c.take_while(|ch| ch != ';' && ch != '\n' && ch != '"');
    if !c.eat('"') {
        return Err(c.unexpected(&[Item::tokens("\"")]));
    }
    Ok(s)
}

/// An amount: a single-commodity amount and any cost, valuation expression
/// and lot annotations after it (amountp').
pub fn amountp(c: &mut Cursor, ctx: &AmountCtx, kind: AmountKind) -> PResult<Amount> {
    let a = simple_amountp(c, ctx, kind, None)?;
    c.skip_spaces();
    annotationsp(c, ctx, a)
}

/// A posting's amount: like [`amountp`], but the amount may be an amount
/// expression (postingamountp).
pub fn posting_amountp(c: &mut Cursor, ctx: &AmountCtx, kind: AmountKind) -> PResult<Amount> {
    let a = amount_exprp(c, ctx, kind)?;
    c.skip_spaces();
    annotationsp(c, ctx, a)
}

/// An amount with an optional cost, but no lot annotations (amountnobasisp).
pub fn amount_nobasisp(c: &mut Cursor, ctx: &AmountCtx) -> PResult<Amount> {
    let mut a = simple_amountp(c, ctx, AmountKind::Ordinary, None)?;
    c.skip_spaces();
    if matches!(
        (c.peek(), c.peek2()),
        (Some('@'), _) | (Some('('), Some('@'))
    ) {
        a.cost = Some(costp(c, ctx, &a)?);
        c.skip_spaces();
    }
    Ok(a)
}

/// The cost, valuation expression and lot annotations that may follow an
/// amount and the spaces after it, each at most once, in any order
/// (amountannotationsp).
fn annotationsp(c: &mut Cursor, ctx: &AmountCtx, mut a: Amount) -> PResult<Amount> {
    let (mut cost, mut valuation, mut basis, mut date, mut note) = (None, false, None, None, None);
    loop {
        match (c.peek(), c.peek2()) {
            (Some('@'), _) | (Some('('), Some('@')) if cost.is_none() => {
                cost = Some(costp(c, ctx, &a)?);
            }
            (Some('('), Some('(')) if !valuation => {
                valuation_exprp(c)?;
                valuation = true;
            }
            (Some('{'), _) if basis.is_none() => basis = Some(lot_costp(c, ctx, &a.quantity)?),
            (Some('['), _) if date.is_none() => date = Some(lot_datep(c, ctx)?),
            (Some('('), _) if note.is_none() => note = Some(lot_notep(c)?),
            _ => break,
        }
        c.skip_spaces();
    }
    let consolidated = basis
        .as_ref()
        .is_some_and(|b: &CostBasis| b.date.is_some() || b.label.is_some());
    if consolidated && (date.is_some() || note.is_some()) {
        return Err(ParseError::fail_at(
            c.src,
            c.pos,
            "hledger lot syntax {...} cannot be combined with ledger-style [DATE] or (NOTE)",
        ));
    }
    a.cost = cost;
    if basis.is_some() || date.is_some() || note.is_some() {
        let basis = basis.unwrap_or_default();
        a.cost_basis = Some(Box::new(CostBasis {
            cost: basis.cost,
            date: basis.date.or(date),
            label: basis.label.or(note),
        }));
    }
    Ok(a)
}

/// A single-commodity amount with no cost: a commodity symbol and a number,
/// in either order, or just a number, with a sign before either (simpleamountp').
/// Without a symbol, the default commodity is applied, except to a
/// multiplier. Given a nesting depth, it is an operand of an amount
/// expression inside that many parentheses: a symbol after the number cannot
/// begin with `/`, or inside parentheses contain `)`.
fn simple_amountp(
    c: &mut Cursor,
    ctx: &AmountCtx,
    kind: AmountKind,
    depth: Option<usize>,
) -> PResult<Amount> {
    let negative = signp(c);
    let r = if c.peek().is_some_and(starts_commodity_symbol) {
        left_symbol_amountp(c, ctx, kind)
    } else {
        right_or_no_symbol_amountp(c, ctx, kind, depth)
    };
    let mut a = r?;
    if negative {
        a.quantity = -a.quantity;
    }
    Ok(a)
}

/// The part of an amount after its sign, with the symbol first.
fn left_symbol_amountp(c: &mut Cursor, ctx: &AmountCtx, kind: AmountKind) -> PResult<Amount> {
    let commodity = commodity_symbolp(c)?;
    let hint = ctx.number_hint(commodity);
    let spaced = c.skip_spaces();
    let negative = signp(c);
    let (q, precision, mark, groups) = numberp_in_amount(c, ctx, kind, hint)?;
    Ok(Amount {
        commodity: Sym::from(commodity),
        quantity: if negative { -q } else { q },
        style: Arc::new(AmountStyle {
            side: Side::L,
            spaced,
            digit_groups: groups,
            decimal_mark: mark,
            precision: Precision::Fixed(precision),
            ..AmountStyle::default()
        }),
        ..Amount::null()
    })
}

/// The part of an amount after its sign, with the symbol after the number,
/// or no symbol.
fn right_or_no_symbol_amountp(
    c: &mut Cursor,
    ctx: &AmountCtx,
    kind: AmountKind,
    depth: Option<usize>,
) -> PResult<Amount> {
    let start = c.pos;
    let raw = match rawnumberp(c) {
        Err(_) if c.pos == start => return Err(c.unexpected(&[Item::label("amount")])),
        r => r?,
    };
    let exponent = optional_exponentp(c);
    let region = (start, c.pos);
    let in_parens = depth.is_some_and(|d| d > 0);
    let (_, next) = peek_after_spaces(c);
    let symbol_follows = next.is_some_and(|n| {
        starts_commodity_symbol(n) && !(depth.is_some() && n == '/') && !(in_parens && n == ')')
    });
    let mut right_symbol = None;
    if symbol_follows {
        let before = *c;
        let spaced = c.skip_spaces();
        let symbol = if in_parens && c.peek() != Some('"') {
            let s = c.take_while(|ch| !is_nonsimple_commodity_char(ch) && ch != ')');
            (!s.is_empty()).then_some(s).ok_or(())
        } else {
            commodity_symbolp(c).map_err(|_| ())
        };
        match symbol {
            Ok(s) => right_symbol = Some((spaced, s)),
            Err(()) => *c = before,
        }
    }
    let interpret = |c: &Cursor, hint| {
        let required = if kind == AmountKind::Style {
            None
        } else {
            ctx.decimal_mark
        };
        interpret_raw_number(required, hint, raw, exponent)
            .map_err(|msg| ParseError::at_region(c.src, region.0, region.1, &msg))
    };
    match right_symbol {
        Some((spaced, commodity)) => {
            let (q, precision, mark, groups) = interpret(c, ctx.number_hint(commodity))?;
            Ok(Amount {
                commodity: Sym::from(commodity),
                quantity: q,
                style: Arc::new(AmountStyle {
                    side: Side::R,
                    spaced,
                    digit_groups: groups,
                    decimal_mark: mark,
                    precision: Precision::Fixed(precision),
                    ..AmountStyle::default()
                }),
                ..Amount::null()
            })
        }
        None => {
            let (q, precision, mark, groups) = interpret(c, ctx.number_hint(""))?;
            let style = AmountStyle {
                precision: Precision::Fixed(precision),
                decimal_mark: mark,
                digit_groups: groups,
                ..AmountStyle::default()
            };
            let a = Amount {
                quantity: q,
                style: Arc::new(style),
                ..Amount::null()
            };
            Ok(if kind == AmountKind::Multiplier {
                a
            } else {
                with_default_commodity(ctx, a)
            })
        }
    }
}

/// The number of an amount whose symbol has been read, checked against the
/// declared decimal mark unless it declares a style (interpretNumber).
fn numberp_in_amount(
    c: &mut Cursor,
    ctx: &AmountCtx,
    kind: AmountKind,
    hint: Option<NumberHint>,
) -> PResult<(Decimal, u8, Option<char>, Option<DigitGroups>)> {
    let start = c.pos;
    let raw = rawnumberp(c)?;
    let exponent = optional_exponentp(c);
    let required = if kind == AmountKind::Style {
        None
    } else {
        ctx.decimal_mark
    };
    interpret_raw_number(required, hint, raw, exponent)
        .map_err(|msg| ParseError::at_region(c.src, start, c.pos, &msg))
}

/// The amount in the default commodity, with its style, if a D directive set
/// one; its own precision, decimal mark and digit groups are kept where that
/// style is greater or has none.
fn with_default_commodity(ctx: &AmountCtx, a: Amount) -> Amount {
    match ctx.default_commodity {
        Some((commodity, style)) => Amount {
            commodity: commodity.clone(),
            style: Arc::new(AmountStyle {
                precision: style.precision.max(a.style.precision),
                decimal_mark: style.decimal_mark.or(a.style.decimal_mark),
                digit_groups: style.digit_groups.clone().or(a.style.digit_groups.clone()),
                ..(**style).clone()
            }),
            ..a
        },
        None => a,
    }
}

/// An amount expression: amounts combined with `+ - * /` and parentheses,
/// evaluated to a single amount (amountexprp). `*` and `/` are applied
/// before `+` and `-`, otherwise left to right. The results of `*` and `/`
/// are exact; they are rounded once the whole entry has been read. The
/// default commodity is applied to a result with no commodity, unless it is
/// a multiplier.
pub fn amount_exprp(c: &mut Cursor, ctx: &AmountCtx, kind: AmountKind) -> PResult<Amount> {
    let a = sump(c, ctx, 0)?;
    Ok(
        if kind != AmountKind::Multiplier && a.commodity.is_empty() {
            with_default_commodity(ctx, a)
        } else {
            a
        },
    )
}

/// A sum or difference of products.
fn sump(c: &mut Cursor, ctx: &AmountCtx, depth: usize) -> PResult<Amount> {
    let mut a = productp(c, ctx, depth)?;
    while let Some(op @ ('+' | '-')) = c.peek() {
        let off = c.pos;
        c.bump();
        c.skip_spaces();
        let b = productp(c, ctx, depth)?;
        a = add_amounts(c, (off, c.pos), op, a, b)?;
    }
    Ok(a)
}

/// A product or quotient of factors.
fn productp(c: &mut Cursor, ctx: &AmountCtx, depth: usize) -> PResult<Amount> {
    let mut a = factorp(c, ctx, depth)?;
    while let Some(op @ ('*' | '/')) = c.peek() {
        let off = c.pos;
        c.bump();
        c.skip_spaces();
        let b = factorp(c, ctx, depth)?;
        a = multiply_amounts(c, (off, c.pos), op, a, b)?;
    }
    Ok(a)
}

/// A simple amount, or a parenthesised expression with an optional sign; and
/// the spaces after it. Operands get no default commodity.
fn factorp(c: &mut Cursor, ctx: &AmountCtx, depth: usize) -> PResult<Amount> {
    match (c.peek(), c.peek2()) {
        (Some('('), _) => parensp(c, ctx, depth),
        (Some(s @ ('-' | '+')), _)
            if c.rest()[1..]
                .trim_start_matches(is_non_newline_space)
                .starts_with('(') =>
        {
            c.bump();
            c.skip_spaces();
            let mut a = parensp(c, ctx, depth)?;
            if s == '-' {
                a.quantity = -a.quantity;
            }
            Ok(a)
        }
        _ => {
            let a = simple_amountp(c, ctx, AmountKind::Multiplier, Some(depth))?;
            // In 10 EUR/4 the symbol is read as EUR/; explain that.
            if a.style.side == Side::R && a.commodity.ends_with('/') {
                let (spaced, next) = peek_after_spaces(c);
                let looks_divided = match next {
                    Some(n) if n.is_ascii_digit() => true,
                    Some(n) if !spaced => matches!(n, '(' | '.' | '-' | '+'),
                    _ => false,
                };
                if looks_divided {
                    return Err(ParseError::at_region(
                        c.src,
                        c.pos - 1,
                        c.pos,
                        &format!(
                            "this / is read as part of the commodity symbol {}; \
                             to divide, write a space before it (eg 10 EUR / 4)",
                            a.commodity
                        ),
                    ));
                }
            }
            c.skip_spaces();
            Ok(a)
        }
    }
}

/// A parenthesised amount expression and the spaces after it.
fn parensp(c: &mut Cursor, ctx: &AmountCtx, depth: usize) -> PResult<Amount> {
    c.bump();
    c.skip_spaces();
    let a = sump(c, ctx, depth + 1)?;
    if !c.eat(')') {
        return Err(c.unexpected(&[Item::label("closing parenthesis")]));
    }
    c.skip_spaces();
    Ok(a)
}

/// Two amounts added or subtracted: they must have the same commodity, or one
/// none, when the other's is used. The result has the larger precision.
fn add_amounts(
    c: &Cursor,
    region: (usize, usize),
    op: char,
    a: Amount,
    b: Amount,
) -> PResult<Amount> {
    if !a.commodity.is_empty() && !b.commodity.is_empty() && a.commodity != b.commodity {
        return Err(expr_error(
            c,
            region,
            &format!(
                "can't {} amounts with different commodities ({} and {})",
                if op == '+' { "add" } else { "subtract" },
                a.commodity,
                b.commodity
            ),
        ));
    }
    let quantity = if op == '+' {
        &a.quantity + &b.quantity
    } else {
        &a.quantity - &b.quantity
    };
    let precision = a.style.precision.max(b.style.precision);
    let base = if !a.commodity.is_empty() || b.commodity.is_empty() {
        a
    } else {
        b
    };
    Ok(Amount {
        quantity,
        style: Arc::new(AmountStyle {
            precision,
            ..(*base.style).clone()
        }),
        ..base
    })
}

/// Two amounts multiplied or divided: `*` needs a plain number on one side,
/// `/` on the right; the result has the other side's commodity and style.
fn multiply_amounts(
    c: &Cursor,
    region: (usize, usize),
    op: char,
    a: Amount,
    b: Amount,
) -> PResult<Amount> {
    let (a_sym, b_sym) = (!a.commodity.is_empty(), !b.commodity.is_empty());
    if op == '*' && a_sym && b_sym {
        return Err(expr_error(
            c,
            region,
            "can't multiply two amounts with commodity symbols; one side of * should be a plain number",
        ));
    }
    if op == '/' && b_sym {
        return Err(expr_error(
            c,
            region,
            "the right side of / should be a number without a commodity symbol",
        ));
    }
    if op == '/' && b.quantity.is_zero() {
        return Err(expr_error(c, region, "can't divide by zero"));
    }
    Ok(match op {
        '*' if b_sym => b.multiply(&a.quantity),
        '*' => a.multiply(&b.quantity),
        _ => a.divide(&b.quantity),
    })
}

fn expr_error(c: &Cursor, (start, end): (usize, usize), msg: &str) -> ParseError {
    ParseError::at_region(c.src, start, end, msg)
}

/// A cost: `@ UNITAMT`, `@@ TOTALAMT`, or the same with the `@` or `@@` in
/// parentheses, which are ignored (costp). A total cost gets the sign of the
/// amount it is the cost of.
pub fn costp(c: &mut Cursor, ctx: &AmountCtx, base: &Amount) -> PResult<AmountCost> {
    let parenthesised = c.eat('(');
    if !c.eat('@') {
        return Err(c.unexpected(&[Item::label("transaction price")]));
    }
    let total = c.eat('@');
    if parenthesised && !c.eat(')') {
        return Err(c.unexpected(&[Item::tokens(")")]));
    }
    c.skip_spaces();
    let mut cost = simple_amountp(c, ctx, AmountKind::Ordinary, None)?;
    Ok(if total {
        let sign = Decimal::from_i64(if base.quantity.is_negative() { -1 } else { 1 });
        cost.quantity = &sign * &cost.quantity;
        AmountCost::Total(Box::new(cost))
    } else {
        AmountCost::Unit(Box::new(cost))
    })
}

/// A valuation expression in double parentheses, which is ignored
/// (valuationexprp).
fn valuation_exprp(c: &mut Cursor) -> PResult<()> {
    c.eat_str("((");
    c.take_while(|ch| ch != ')' && ch != '\n');
    if !c.eat_str("))") {
        return Err(c.unexpected(&[Item::tokens("))")]));
    }
    Ok(())
}

/// A balance assertion or assignment: `=`, `==`, `=*` or `==*` and an amount,
/// which may have a cost but no lot annotations (balanceassertionp). `pos`
/// is where it starts.
pub fn balance_assertionp(
    c: &mut Cursor,
    ctx: &AmountCtx,
    pos: SourcePos,
) -> PResult<BalanceAssertion> {
    c.eat('=');
    let total = c.eat('=');
    let inclusive = c.eat('*');
    c.skip_spaces();
    let start = c.pos;
    let amount = amount_nobasisp(c, ctx).map_err(|e| {
        if c.pos == start {
            c.unexpected(&[Item::label(
                "amount (for a balance assertion or assignment)",
            )])
        } else {
            e
        }
    })?;
    Ok(BalanceAssertion {
        amount,
        total,
        inclusive,
        pos,
    })
}

/// A lot cost basis in braces (lotcostp): Ledger-style `{UNITCOST}`,
/// `{{TOTALCOST}}`, `{=UNITCOST}`, `{{=TOTALCOST}}` or `{}`, or hledger-style
/// `{DATE, "LABEL", COST}` (or in double braces) with each part optional. A
/// total cost is converted to a unit cost by dividing by the magnitude of the
/// posting's quantity.
fn lot_costp(c: &mut Cursor, ctx: &AmountCtx, quantity: &Decimal) -> PResult<CostBasis> {
    c.bump();
    let double = c.eat('{');
    // what an error at the first part would list as also possible here
    let mut hints = Vec::new();
    if !c.skip_spaces() && !double {
        hints.push(Item::tokens("{"));
    }
    let basis = match c.peek() {
        None => return Err(c.unexpected(&hints)),
        Some('}') => CostBasis::default(),
        Some('=') => ledger_lot_costp(c, ctx, Vec::new())?,
        Some('"') => {
            let label = lot_labelp(c)?;
            c.skip_spaces();
            let mut cost = None;
            if c.eat(',') {
                c.skip_spaces();
                if looks_like_date(c) {
                    return Err(wrong_order(c));
                }
                cost = Some(Box::new(simple_amountp(
                    c,
                    ctx,
                    AmountKind::Ordinary,
                    None,
                )?));
                c.skip_spaces();
            }
            CostBasis {
                date: None,
                label: Some(label),
                cost,
            }
        }
        Some(_) if looks_like_date(c) => {
            let date = datep(c, ctx.year)?;
            c.skip_spaces();
            if !matches!(c.peek(), Some(',' | '}')) {
                return Err(c.unexpected(&[Item::tokens(","), Item::tokens("}")]));
            }
            let before = *c;
            let mut label = None;
            if c.eat(',') {
                c.skip_spaces();
                if c.peek() == Some('"') {
                    label = Some(lot_labelp(c)?);
                    c.skip_spaces();
                } else {
                    *c = before;
                }
            }
            let mut cost = None;
            if c.eat(',') {
                c.skip_spaces();
                cost = Some(Box::new(simple_amountp(
                    c,
                    ctx,
                    AmountKind::Ordinary,
                    None,
                )?));
                c.skip_spaces();
            }
            CostBasis {
                date: Some(date),
                label,
                cost,
            }
        }
        Some(_) => {
            hints.push(Item::label("digit"));
            ledger_lot_costp(c, ctx, hints)?
        }
    };
    if c.peek()
        .is_some_and(|n| n == ',' || n == '"' || n.is_ascii_digit())
    {
        return Err(wrong_order(c));
    }
    if !c.eat('}') || (double && !c.eat('}')) {
        return Err(c.unexpected(&[Item::tokens("}")]));
    }
    Ok(if double {
        CostBasis {
            cost: basis
                .cost
                .map(|a| Box::new(a.divide_and_update_precision(&quantity.abs()))),
            ..basis
        }
    } else {
        basis
    })
}

/// A Ledger-style lot cost: an optional `=` (ignored) and an optional amount.
/// `hints` are what an error at the amount would also list.
fn ledger_lot_costp(c: &mut Cursor, ctx: &AmountCtx, mut hints: Vec<Item>) -> PResult<CostBasis> {
    if c.eat('=') {
        hints.clear();
    } else {
        hints.push(Item::tokens("="));
    }
    if c.skip_spaces() {
        hints.clear();
    }
    let start = c.pos;
    let cost = match simple_amountp(c, ctx, AmountKind::Ordinary, None) {
        Ok(a) => Some(Box::new(a)),
        Err(_) if c.pos == start => {
            if c.peek() != Some('}') {
                hints.extend([Item::tokens("}"), Item::label("amount")]);
                return Err(c.unexpected(&hints));
            }
            None
        }
        Err(e) => return Err(e),
    };
    c.skip_spaces();
    Ok(CostBasis {
        date: None,
        label: None,
        cost,
    })
}

/// Whether a date (four digits, a hyphen and a digit) comes next.
fn looks_like_date(c: &Cursor) -> bool {
    let b = c.rest().as_bytes();
    b.len() >= 6 && b[..4].iter().all(u8::is_ascii_digit) && b[4] == b'-' && b[5].is_ascii_digit()
}

fn wrong_order(c: &Cursor) -> ParseError {
    ParseError::fail_at(
        c.src,
        c.pos,
        "cost basis parts must be in this order (if present): {DATE, \"LABEL\", COST}",
    )
}

/// A lot label in double quotes.
fn lot_labelp(c: &mut Cursor) -> PResult<Arc<str>> {
    c.bump();
    let label = c.take_while(|ch| ch != '"' && ch != '\n');
    if !c.eat('"') {
        return Err(c.unexpected(&[Item::tokens("\"")]));
    }
    Ok(Arc::from(label))
}

/// A Ledger-style lot date in square brackets (lotdatep).
fn lot_datep(c: &mut Cursor, ctx: &AmountCtx) -> PResult<crate::dates::Day> {
    c.bump();
    c.skip_spaces();
    let d = datep(c, ctx.year)?;
    c.skip_spaces();
    if !c.eat(']') {
        return Err(c.unexpected(&[Item::tokens("]")]));
    }
    Ok(d)
}

/// A Ledger-style lot note in parentheses, without any double quotes
/// (lotnotep).
fn lot_notep(c: &mut Cursor) -> PResult<Arc<str>> {
    c.bump();
    c.skip_spaces();
    let note = c.take_while(|ch| ch != ')' && ch != '\n');
    if !c.eat(')') {
        return Err(c.unexpected(&[Item::tokens(")")]));
    }
    Ok(Arc::from(note.trim_end().replace('"', "")))
}

/// An amount read from text with no journal context: the whole text, with
/// any cost and lot annotations (parseamount).
pub fn parse_amount(s: &str) -> Result<Amount, String> {
    parse_amount_with(&AmountCtx::default(), s)
}

/// An amount read from text using this context (parseamountWithStyles).
pub fn parse_amount_with(ctx: &AmountCtx, s: &str) -> Result<Amount, String> {
    let mut c = Cursor::new(s);
    let a = amountp(&mut c, ctx, AmountKind::Ordinary).map_err(|e| e.pretty("", s))?;
    if !c.at_end() {
        return Err(c.unexpected(&[Item::EndOfInput]).pretty("", s));
    }
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amount::usd;

    fn dec(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn style(precision: u8, decimal_mark: Option<char>) -> Arc<AmountStyle> {
        Arc::new(AmountStyle {
            precision: Precision::Fixed(precision),
            decimal_mark,
            ..AmountStyle::default()
        })
    }

    fn amount(s: &str) -> Result<Amount, String> {
        parse_amount(s)
    }

    /// A posting amount (an expression), parsed with this context.
    fn expr_with(ctx: &AmountCtx, s: &str) -> Result<Amount, String> {
        let mut c = Cursor::new(s);
        let a = posting_amountp(&mut c, ctx, AmountKind::Ordinary).map_err(|e| e.pretty("", s))?;
        if !c.at_end() {
            return Err(c.unexpected(&[Item::EndOfInput]).pretty("", s));
        }
        Ok(a)
    }

    fn expr(s: &str) -> Result<Amount, String> {
        expr_with(&AmountCtx::default(), s)
    }

    /// The parts of an expression's result that the expression rules decide.
    fn eq(s: &str, commodity: &str, quantity: &str, precision: u8) {
        let a = expr(s).unwrap();
        assert_eq!(
            (&*a.commodity, a.quantity, a.style.precision),
            (commodity, dec(quantity), Precision::Fixed(precision)),
            "{s}"
        );
    }

    fn number(
        s: &str,
        hint: Option<NumberHint>,
    ) -> Result<(Decimal, u8, Option<char>, Option<DigitGroups>), String> {
        let mut c = Cursor::new(s);
        let n = numberp(&mut c, hint).map_err(|e| e.pretty("", s))?;
        if !c.at_end() {
            return Err(c.unexpected(&[Item::EndOfInput]).pretty("", s));
        }
        Ok(n)
    }

    fn groups(sep: char, sizes: &[u8]) -> Option<DigitGroups> {
        Some(DigitGroups {
            sep,
            sizes: sizes.to_vec(),
        })
    }

    // tests_Common "amountp".
    #[test]
    fn amounts() {
        assert_eq!(amount("$47.18"), Ok(usd(dec("47.18"))));
        assert_eq!(
            amount("$1."),
            Ok(usd(dec("1")).with_precision(Precision::Fixed(0)))
        );
        let unit = amount("$10 @ €0.5").unwrap();
        assert_eq!((&*unit.commodity, &unit.quantity), ("$", &dec("10")));
        assert_eq!(unit.style, style(0, None));
        let Some(AmountCost::Unit(cost)) = &unit.cost else {
            panic!("unit cost")
        };
        assert_eq!((&*cost.commodity, &cost.quantity), ("€", &dec("0.5")));
        assert_eq!(cost.style, style(1, Some('.')));
        let total = amount("$10 @@ €5").unwrap();
        let Some(AmountCost::Total(cost)) = &total.cost else {
            panic!("total cost")
        };
        assert_eq!((&*cost.commodity, &cost.quantity), ("€", &dec("5")));
        assert_eq!(cost.style, style(0, None));
        assert!(amount("$10 (@) €0.5").is_ok());
        assert!(amount("$10 (@@) €0.5").is_ok());
    }

    // tests_Common "postingamountp".
    #[test]
    fn amount_expressions() {
        assert_eq!(expr("$47.18"), Ok(usd(dec("47.18"))));
        eq("$-50.20 + $-209.00 + $-16.54 + $-438", "$", "-713.74", 2);
        eq("10 EUR-2.50 EUR", "EUR", "7.5", 2);
        eq("$10 - -$5", "$", "15", 0);
        eq("$1 + $2 * 3", "$", "7", 0);
        eq("($1 + $2) * 3", "$", "9", 0);
        eq("((1.0 + 2) * (3 - 1)) / 4", "", "1.5", 1);
        eq("-($1 + $2) - +($3)", "$", "-6", 0);
        eq("10.00 EUR / 4", "EUR", "2.5", 2);
        eq("(10.00 EUR + 5 EUR) / 4", "EUR", "3.75", 2);
        eq("$47.97 * 0.05", "$", "2.3985", 2);
        let a = expr("$100.00 / 3").unwrap();
        assert!(a.quantity > dec("33.3333333") && a.quantity < dec("33.3333334"));
        assert_eq!(a.style.precision, Precision::Fixed(2));
        eq("3 * $10.005", "$", "30.015", 3);
        eq("0.5 * $10", "$", "5", 0);
        eq("5 / 2", "", "2.5", 0);
        eq("$1 + $0.5", "$", "1.5", 1);
        eq("$10 + 5.5", "$", "15.5", 1);
        eq("5 - $10", "$", "-5", 0);
        eq("(2 + 3) * 10 EUR", "EUR", "50", 0);
        assert_eq!(expr("5 + 10 EUR").unwrap().style.side, Side::R);
        let a = expr("1 X + 2 X @ $3").unwrap();
        let Some(AmountCost::Unit(cost)) = &a.cost else {
            panic!("unit cost")
        };
        assert_eq!(
            (a.quantity, &*cost.commodity, &cost.quantity),
            (dec("3"), "$", &dec("3"))
        );
        let err = |s: &str, msg: &str| {
            let e = expr(s).unwrap_err();
            assert!(e.contains(msg), "{s}: {e}");
        };
        err(
            "$1 + 1 EUR",
            "can't add amounts with different commodities ($ and EUR)",
        );
        err(
            "$2 * $1",
            "can't multiply two amounts with commodity symbols",
        );
        err("2 / $1", "the right side of / should be a number");
        err("$1 / 0", "can't divide by zero");
        err(
            "10 EUR/4",
            "this / is read as part of the commodity symbol EUR/; to divide, write a space before it",
        );
        err(
            "10 EUR/ 4",
            "this / is read as part of the commodity symbol EUR/",
        );
        err(
            "10 EUR/-2",
            "this / is read as part of the commodity symbol EUR/",
        );
        assert_eq!(&*expr("10 EUR/ + 2 EUR/").unwrap().commodity, "EUR/");
        err("($1 + $2", "closing parenthesis");
        let d = (Sym::from("$"), Arc::new(AmountStyle::default()));
        let ctx = AmountCtx {
            default_commodity: Some(&d),
            ..AmountCtx::default()
        };
        let on = |s: &str| {
            let a = expr_with(&ctx, s).unwrap();
            (a.commodity.to_string(), a.quantity)
        };
        assert_eq!(on("10 EUR * 2"), ("EUR".to_string(), dec("20")));
        assert_eq!(on("10 EUR + 2"), ("EUR".to_string(), dec("12")));
        assert_eq!(on("10 + 2"), ("$".to_string(), dec("12")));
    }

    // tests_Common "numberp".
    #[test]
    fn numbers() {
        let ok = |s: &str, q: &str, p: u8, d: Option<char>, g: Option<DigitGroups>| {
            assert_eq!(number(s, None), Ok((dec(q), p, d, g)), "{s}");
        };
        ok("0", "0", 0, None, None);
        ok("1", "1", 0, None, None);
        ok("1.1", "1.1", 1, Some('.'), None);
        ok("1,000.1", "1000.1", 1, Some('.'), groups(',', &[3]));
        ok("1_000.1", "1000.1", 1, Some('.'), groups('_', &[3]));
        ok("1'000.1", "1000.1", 1, Some('.'), groups('\'', &[3]));
        ok("1.00.000,1", "100000.1", 1, Some(','), groups('.', &[3, 2]));
        ok("1,000,000", "1000000", 0, None, groups(',', &[3, 3]));
        ok("1.", "1", 0, Some('.'), None);
        ok("1,", "1", 0, Some(','), None);
        ok(".1", "0.1", 1, Some('.'), None);
        ok(",1", "0.1", 1, Some(','), None);
        for bad in [
            "",
            "1,000.000,1",
            "1.000,000.1",
            "1,000.000.1",
            "1,,1",
            "1..1",
            ".1,",
            ",1.",
        ] {
            assert!(number(bad, None).is_err(), "{bad}");
        }
        let fives = |n| format!("1.{}", "5".repeat(n));
        let n = number(&fives(255), None).unwrap();
        assert_eq!((n.0, n.1), (dec(&fives(255)), 255));
        assert!(number(&fives(256), None).is_err());
    }

    // tests_Common "numberp with a suggested decimal mark".
    #[test]
    fn numbers_with_a_suggested_decimal_mark() {
        let hint = |c| {
            Some(NumberHint {
                decimal_mark: Some(c),
                group_mark: None,
            })
        };
        let ok = |s: &str, c: char, q: &str, p: u8, d: Option<char>, g: Option<DigitGroups>| {
            assert_eq!(number(s, hint(c)), Ok((dec(q), p, d, g)), "{s}");
        };
        ok("1.000", '.', "1", 3, Some('.'), None);
        ok("1.000", ',', "1000", 0, None, groups('.', &[3]));
        ok("1.000.000", ',', "1000000", 0, None, groups('.', &[3, 3]));
        ok("1,000,000", '.', "1000000", 0, None, groups(',', &[3, 3]));
        ok("1 000 000", '.', "1000000", 0, None, groups(' ', &[3, 3]));
        ok("1.2.34", '.', "1234", 0, None, groups('.', &[2, 1, 1]));
        ok(
            "1,000,000.00",
            ',',
            "1000000",
            2,
            Some('.'),
            groups(',', &[3, 3]),
        );
    }

    // tests_Common "amountp with a decimal-mark directive".
    #[test]
    fn amounts_with_a_declared_decimal_mark() {
        let ctx = AmountCtx {
            decimal_mark: Some(','),
            ..AmountCtx::default()
        };
        let parse = |kind, s: &str| {
            let mut c = Cursor::new(s);
            amountp(&mut c, &ctx, kind).map_err(|e| e.pretty("", s))
        };
        assert!(parse(AmountKind::Ordinary, "1.000,5 EUR").is_ok());
        let err = |s: &str, msg: &str| {
            let e = parse(AmountKind::Ordinary, s).unwrap_err();
            assert!(e.contains(msg), "{s}: {e}");
        };
        err("1,000.5 EUR", "number's decimal mark is '.'");
        err(".5 EUR", "number's decimal mark is '.'");
        err("1 X @ 1,000.5 EUR", "number's decimal mark is '.'");
        err("1,000,000 EUR", "so it can appear only once");
        assert!(parse(AmountKind::Style, "1,000.5 EUR").is_ok());
    }

    // The error messages of some malformed amounts, as hledger shows them.
    #[test]
    fn amount_errors() {
        let err = |s: &str| expr(s).unwrap_err();
        assert!(err("1 @").ends_with("unexpected end of input\nexpecting amount\n"));
        assert!(err("1 @ x").ends_with("unexpected end of input\nexpecting number\n"));
        assert!(err("1 {").ends_with("unexpected end of input\nexpecting '{'\n"));
        assert!(
            err("1 {\n")
                .ends_with("unexpected newline\nexpecting '=', '{', '}', amount, or digit\n")
        );
        assert!(err("1,000.000,1").ends_with("invalid number (invalid use of separator)\n"));
        assert!(err("1.5 2").contains("1:5:"));
    }
}
