//! Amounts (Hledger.Data.Amount, and the amount types of Hledger.Data.Types):
//! an [`Amount`] is a quantity of one commodity, with a display style and
//! optionally a cost in another commodity and a lot cost basis; a
//! [`MixedAmount`] is a sum of amounts in several commodities.

pub mod currency;
pub mod render;

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::iter::Sum;
use std::ops::{Add, Mul, Neg, Sub};
use std::sync::{Arc, LazyLock};

use dashu_int::IBig;

use crate::dates::Day;
use crate::decimal::Decimal;

/// A commodity symbol, shared by the amounts that use it.
pub type Sym = Arc<str>;

/// The display style to use for each commodity.
pub type CommodityStyles = BTreeMap<Sym, Arc<AmountStyle>>;

/// Which side of the quantity a commodity symbol goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Side {
    L,
    R,
}

/// How many decimal digits to show: a fixed number, or all those stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Precision {
    Fixed(u8),
    Natural,
}

/// How hard a commodity style's precision is applied to amounts (print's
/// `--round`): not at all, without hiding significant digits, exactly for
/// amounts but softly for costs, or exactly for both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rounding {
    No,
    Soft,
    Hard,
    All,
}

/// Digit group marks for the integer part of a number: the separator and the
/// group sizes from the decimal mark leftward, the last size repeating (at
/// least one digit each). Thousands separated by commas are `,` and `[3]`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DigitGroups {
    pub sep: char,
    pub sizes: Vec<u8>,
}

/// How to display amounts of a commodity, as learned from the journal.
/// `rounding` is not used for display, but says how this style's precision is
/// applied when it is given to other amounts.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct AmountStyle {
    pub side: Side,
    pub spaced: bool,
    pub digit_groups: Option<DigitGroups>,
    pub decimal_mark: Option<char>,
    pub precision: Precision,
    pub rounding: Rounding,
}

/// Symbol on the left, unspaced, no digit groups, a period as decimal mark,
/// no decimal digits, no rounding (amountstyle).
impl Default for AmountStyle {
    fn default() -> AmountStyle {
        AmountStyle {
            side: Side::L,
            spaced: false,
            digit_groups: None,
            decimal_mark: Some('.'),
            precision: Precision::Fixed(0),
            rounding: Rounding::No,
        }
    }
}

/// An amount's cost, per unit (`@`) or in total (`@@`), in another commodity.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AmountCost {
    Unit(Box<Amount>),
    Total(Box<Amount>),
}

/// The cost basis of a lot: its acquisition date, a label, and its cost per
/// unit; or, with some parts left out, a pattern selecting lots.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct CostBasis {
    pub date: Option<Day>,
    pub label: Option<Arc<str>>,
    pub cost: Option<Box<Amount>>,
}

/// A quantity of one commodity (`AUTO` marks an amount left to be inferred),
/// how to display it, and optionally its cost and lot cost basis. Equality
/// and order compare the fields in this order, quantities by value, as
/// Haskell's derived instances do.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Amount {
    pub commodity: Sym,
    pub quantity: Decimal,
    pub style: Arc<AmountStyle>,
    pub cost: Option<AmountCost>,
    pub cost_basis: Option<Box<CostBasis>>,
}

/// The display precision given to infinite decimals, such as the quotients
/// of divisions that do not terminate.
pub const DEFAULT_MAX_DISPLAY_PRECISION: u8 = 8;

static DEFAULT_STYLE: LazyLock<Arc<AmountStyle>> = LazyLock::new(Default::default);
static NO_SYMBOL: LazyLock<Sym> = LazyLock::new(|| Sym::from(""));

impl Amount {
    /// Zero with no commodity, no cost, and the default style (nullamt).
    pub fn null() -> Amount {
        Amount {
            commodity: NO_SYMBOL.clone(),
            quantity: Decimal::ZERO,
            style: DEFAULT_STYLE.clone(),
            cost: None,
            cost_basis: None,
        }
    }

    /// The marker for an amount not written, to be inferred (missingamt).
    pub fn missing() -> Amount {
        Amount {
            commodity: Sym::from("AUTO"),
            ..Amount::null()
        }
    }

    pub fn is_missing(&self) -> bool {
        &*self.commodity == "AUTO"
    }

    /// This amount at a cost per unit (`at`).
    pub fn at(self, cost: Amount) -> Amount {
        Amount {
            cost: Some(AmountCost::Unit(Box::new(cost))),
            ..self
        }
    }

    /// This amount at a total cost (`@@`).
    pub fn at_total(self, cost: Amount) -> Amount {
        Amount {
            cost: Some(AmountCost::Total(Box::new(cost))),
            ..self
        }
    }

    /// In another commodity, without its cost, as if exchanged one for one
    /// (amountWithCommodity).
    pub fn with_commodity(self, commodity: Sym) -> Amount {
        Amount {
            commodity,
            cost: None,
            ..self
        }
    }

    /// What this amount cost, in its cost's commodity; itself if it has no
    /// cost (amountCost).
    pub fn to_cost(&self) -> Amount {
        match &self.cost {
            None => self.clone(),
            Some(AmountCost::Unit(c)) => Amount {
                quantity: &c.quantity * &self.quantity,
                ..(**c).clone()
            },
            Some(AmountCost::Total(c)) => (**c).clone(),
        }
    }

    /// What this amount cost by its lot cost basis, if that has a cost,
    /// otherwise by its cost (amountCostBasis).
    pub fn to_cost_basis(&self) -> Amount {
        match self.cost_basis.as_ref().and_then(|b| b.cost.as_ref()) {
            Some(c) => Amount {
                quantity: &c.quantity * &self.quantity,
                ..(**c).clone()
            },
            None => self.to_cost(),
        }
    }

    /// With its lot's cost per unit, if it has one, as its cost
    /// (amountSetCostToBasis).
    pub fn set_cost_to_basis(self) -> Amount {
        match self.cost_basis.as_ref().and_then(|b| b.cost.clone()) {
            Some(c) => Amount {
                cost: Some(AmountCost::Unit(c)),
                ..self
            },
            None => self,
        }
    }

    pub fn strip_cost(self) -> Amount {
        Amount { cost: None, ..self }
    }

    /// The amount and its cost amount, if any (getAmounts).
    pub fn and_cost(&self) -> impl Iterator<Item = &Amount> {
        let cost = match &self.cost {
            Some(AmountCost::Unit(c) | AmountCost::Total(c)) => Some(&**c),
            None => None,
        };
        std::iter::once(self).chain(cost)
    }

    /// Whether the quantity, and any total cost, is exactly zero (amountIsZero).
    pub fn is_zero(&self) -> bool {
        self.and_total_cost(|a| a.quantity.is_zero())
    }

    /// Whether the quantity, and any total cost, shows as zero at its display
    /// precision (amountLooksZero).
    pub fn looks_zero(&self) -> bool {
        self.and_total_cost(|a| {
            let q = &a.quantity;
            match a.style.precision {
                // Half-even rounding takes half a unit of the last shown digit to zero.
                Precision::Fixed(d) if q.places() > d => {
                    q.abs() <= Decimal::new(IBig::from(5), d + 1)
                }
                _ => q.is_zero(),
            }
        })
    }

    fn and_total_cost(&self, test: impl Fn(&Amount) -> bool) -> bool {
        match &self.cost {
            Some(AmountCost::Total(c)) => test(self) && test(c),
            _ => test(self),
        }
    }

    /// Whether the quantity is negative (the cost is not considered).
    pub fn is_negative(&self) -> bool {
        self.quantity.is_negative()
    }

    /// With a new quantity, and natural precision so that it shows exactly
    /// (amountSetQuantity).
    pub fn with_quantity(self, quantity: Decimal) -> Amount {
        Amount {
            quantity,
            ..self.with_precision(Precision::Natural)
        }
    }

    /// Apply a function to the quantity, and to any total cost's quantity
    /// (transformAmount).
    fn transform(self, f: impl Fn(&Decimal) -> Decimal) -> Amount {
        let quantity = f(&self.quantity);
        let cost = match self.cost {
            Some(AmountCost::Total(c)) => Some(AmountCost::Total(Box::new(Amount {
                quantity: f(&c.quantity),
                ..*c
            }))),
            cost => cost,
        };
        Amount {
            quantity,
            cost,
            ..self
        }
    }

    /// The quantity and any total cost divided by this number; unchanged if
    /// it is zero (divideAmount).
    pub fn divide(self, n: &Decimal) -> Amount {
        if n.is_zero() {
            self
        } else {
            self.transform(|q| q / n)
        }
    }

    /// Like [`Amount::divide`], with the display precision set to show the
    /// quotient's digits, up to 8 (divideAmountAndUpdatePrecision).
    pub fn divide_and_update_precision(self, n: &Decimal) -> Amount {
        if n.is_zero() {
            return self;
        }
        let a = self.divide(n);
        let p = DEFAULT_MAX_DISPLAY_PRECISION.min(a.internal_precision());
        a.with_precision(Precision::Fixed(p))
    }

    /// The quantity and any total cost multiplied by this number
    /// (multiplyAmount).
    pub fn multiply(self, n: &Decimal) -> Amount {
        self.transform(|q| n * q)
    }

    /// One over the quantity; a zero amount, and any cost, unchanged
    /// (invertAmount).
    pub fn invert(self) -> Amount {
        if self.quantity.is_zero() {
            return self;
        }
        let quantity = &Decimal::from_i64(1) / &self.quantity;
        Amount { quantity, ..self }
    }

    /// With the quantity's absolute value.
    pub fn abs(&self) -> Amount {
        Amount {
            quantity: self.quantity.abs(),
            ..self.clone()
        }
    }

    /// The quantity rounded to the display precision (amountRoundedQuantity).
    pub fn rounded_quantity(&self) -> Decimal {
        match self.style.precision {
            Precision::Natural => self.quantity.clone(),
            Precision::Fixed(p) => self.quantity.round_to(p),
        }
    }

    /// The number of decimal places needed to show the quantity exactly
    /// (amountInternalPrecision).
    pub fn internal_precision(&self) -> u8 {
        self.quantity.normalize().places()
    }

    /// The number of decimal places shown (amountDisplayPrecision).
    pub fn display_precision(&self) -> u8 {
        match self.style.precision {
            Precision::Fixed(n) => n,
            Precision::Natural => self.internal_precision(),
        }
    }

    /// With this display precision (amountSetPrecision).
    pub fn with_precision(self, precision: Precision) -> Amount {
        Amount {
            style: style_with_precision(&self.style, precision),
            ..self
        }
    }

    /// With at least this many decimal places shown (amountSetPrecisionMin).
    pub fn with_precision_min(self, min: u8) -> Amount {
        let p = min.max(self.display_precision());
        self.with_precision(Precision::Fixed(p))
    }

    /// With at most this many decimal places shown (amountSetPrecisionMax).
    pub fn with_precision_max(self, max: u8) -> Amount {
        let p = max.min(self.display_precision());
        self.with_precision(Precision::Fixed(p))
    }

    /// With enough decimal places shown to show the quantity exactly; natural
    /// precision stays natural (amountSetFullPrecision).
    pub fn with_full_precision(self) -> Amount {
        let p = (self.style.precision).max(Precision::Fixed(self.internal_precision()));
        self.with_precision(p)
    }

    /// With enough decimal places shown to show the quantity exactly, but at
    /// most `max`, or, with no maximum, 8 for an infinite decimal
    /// (amountSetFullPrecisionUpTo).
    pub fn with_full_precision_up_to(self, max: Option<u8>) -> Amount {
        let full = self.display_precision().max(self.internal_precision());
        let p = match max {
            Some(max) => max.min(full),
            None if self.quantity.has_max_digits() => DEFAULT_MAX_DISPLAY_PRECISION,
            None => full,
        };
        self.with_precision(Precision::Fixed(p))
    }

    /// With the quantity rounded (half-even) or padded to this many decimal
    /// places, and that display precision (setAmountInternalPrecision).
    pub fn with_internal_precision(self, places: u8) -> Amount {
        Amount {
            quantity: self.quantity.round_to(places),
            ..self.with_precision(Precision::Fixed(places))
        }
    }

    /// With its commodity's style, and its cost amount with the cost
    /// commodity's style, applying each style's precision as its rounding
    /// strategy says; amounts whose commodity has no style are unchanged
    /// (styleAmounts).
    pub fn styled(self, styles: &CommodityStyles) -> Amount {
        let style = restyle(styles, false, &self);
        let cost = self.cost.map(|cost| match cost {
            AmountCost::Unit(c) => AmountCost::Unit(Box::new(c.styled_as_cost(styles))),
            AmountCost::Total(c) => AmountCost::Total(Box::new(c.styled_as_cost(styles))),
        });
        Amount {
            style,
            cost,
            ..self
        }
    }

    fn styled_as_cost(self, styles: &CommodityStyles) -> Amount {
        Amount {
            style: restyle(styles, true, &self),
            ..self
        }
    }

    /// With the default style (amountUnstyled).
    pub fn unstyled(self) -> Amount {
        Amount {
            style: DEFAULT_STYLE.clone(),
            ..self
        }
    }
}

fn style_with_precision(style: &Arc<AmountStyle>, precision: Precision) -> Arc<AmountStyle> {
    if style.precision == precision {
        style.clone()
    } else {
        Arc::new(AmountStyle {
            precision,
            ..(**style).clone()
        })
    }
}

/// An amount's new style: its commodity's, if it has one, with the precision
/// chosen by that style's rounding strategy (amountStyleApplyWithRounding):
/// - No: keep the amount's precision;
/// - Soft: for a cost, keep it; otherwise if either is natural, natural; if
///   the new is greater, the new; else as near the new as possible without
///   hiding non-zero digits;
/// - Hard: for a cost, keep it; otherwise the new;
/// - All: the new.
fn restyle(styles: &CommodityStyles, is_cost: bool, a: &Amount) -> Arc<AmountStyle> {
    let Some(new) = styles.get(&a.commodity) else {
        return a.style.clone();
    };
    let old = a.style.precision;
    let precision = match new.rounding {
        Rounding::No => old,
        Rounding::Soft | Rounding::Hard if is_cost => old,
        Rounding::Soft => match (new.precision, old) {
            (Precision::Fixed(n), Precision::Fixed(o)) if n >= o => new.precision,
            (Precision::Fixed(n), Precision::Fixed(o)) => {
                Precision::Fixed(o.min(a.internal_precision()).max(n))
            }
            _ => Precision::Natural,
        },
        Rounding::Hard | Rounding::All => new.precision,
    };
    style_with_precision(new, precision)
}

/// These styles with this rounding strategy (amountStylesSetRounding).
pub fn styles_with_rounding(rounding: Rounding, styles: &CommodityStyles) -> CommodityStyles {
    styles
        .iter()
        .map(|(c, s)| {
            (
                c.clone(),
                Arc::new(AmountStyle {
                    rounding,
                    ..(**s).clone()
                }),
            )
        })
        .collect()
}

/// The style of `a`, with the most decimal places of `a` and `b`, the first
/// of their digit group styles, and a decimal mark that is not the digit
/// group mark (canonicalStyle).
fn canonical_style(a: &AmountStyle, b: &AmountStyle) -> AmountStyle {
    let digit_groups = a.digit_groups.clone().or_else(|| b.digit_groups.clone());
    let default_mark = match &digit_groups {
        Some(DigitGroups { sep: '.', .. }) => ',',
        _ => '.',
    };
    let decimal_mark = match digit_groups {
        Some(_) => Some(default_mark),
        None => a.decimal_mark.or(b.decimal_mark).or(Some(default_mark)),
    };
    AmountStyle {
        precision: a.precision.max(b.precision),
        decimal_mark,
        digit_groups,
        ..a.clone()
    }
}

/// Merge an amount's style into its commodity's style so far (addAmountStyle).
/// Amounts are added in reverse order of appearance, so the first one's
/// general style wins.
pub fn add_amount_style(a: &Amount, styles: &mut CommodityStyles) {
    add_amount_style_as(a, &a.commodity, &a.style, styles);
}

/// [`add_amount_style`] given the amount's commodity symbol and style,
/// read already; the amount itself is read only to keep its style.
pub fn add_amount_style_as(
    a: &Amount,
    commodity: &str,
    style: &AmountStyle,
    styles: &mut CommodityStyles,
) {
    match styles.get(commodity) {
        None => {
            styles.insert(a.commodity.clone(), a.style.clone());
        }
        Some(old) => {
            let new = canonical_style(style, old);
            if new != **old {
                styles.insert(a.commodity.clone(), Arc::new(new));
            }
        }
    }
}

/// The commodity styles inferred from some amounts, in the order they were
/// read (commodityStylesFromAmounts).
pub fn commodity_styles_from_amounts(amounts: &[Amount]) -> CommodityStyles {
    let mut styles = CommodityStyles::new();
    for a in amounts.iter().rev() {
        add_amount_style(a, &mut styles);
    }
    styles
}

/// Whether a character needs a commodity symbol to be quoted
/// (isNonsimpleCommodityChar).
pub fn is_nonsimple_commodity_char(c: char) -> bool {
    matches!(
        c,
        '-' | '+' | '.' | '@' | '*' | ';' | '\t' | '\n' | ' ' | '"' | '{' | '}' | '='
    ) || c.is_ascii_digit()
}

/// The result of `+`, `-` or `*` on two amounts, which should be in the same
/// commodity: the second's commodity and style, with the larger precision,
/// and no cost (similarAmountsOp).
fn similar_amounts_op(
    a: &Amount,
    b: &Amount,
    op: impl Fn(&Decimal, &Decimal) -> Decimal,
) -> Amount {
    Amount {
        commodity: b.commodity.clone(),
        quantity: op(&a.quantity, &b.quantity),
        style: style_with_precision(&b.style, a.style.precision.max(b.style.precision)),
        cost: None,
        cost_basis: None,
    }
}

impl Add for &Amount {
    type Output = Amount;

    fn add(self, rhs: &Amount) -> Amount {
        similar_amounts_op(self, rhs, |a, b| a + b)
    }
}

impl Sub for &Amount {
    type Output = Amount;

    fn sub(self, rhs: &Amount) -> Amount {
        similar_amounts_op(self, rhs, |a, b| a - b)
    }
}

impl Mul for &Amount {
    type Output = Amount;

    fn mul(self, rhs: &Amount) -> Amount {
        similar_amounts_op(self, rhs, |a, b| a * b)
    }
}

/// Negates the quantity and any total cost.
impl Neg for Amount {
    type Output = Amount;

    fn neg(self) -> Amount {
        self.transform(|q| -q)
    }
}

impl Neg for &Amount {
    type Output = Amount;

    fn neg(self) -> Amount {
        -self.clone()
    }
}

/// An amount with no commodity (`num`, like the other constructors here
/// mostly used in tests).
pub fn num(quantity: Decimal) -> Amount {
    Amount {
        quantity,
        ..Amount::null()
    }
}

fn with_style(commodity: &str, quantity: Decimal, style: AmountStyle) -> Amount {
    Amount {
        commodity: Sym::from(commodity),
        quantity,
        style: Arc::new(style),
        ..Amount::null()
    }
}

/// Hours: `h` on the right, two decimal places.
pub fn hrs(quantity: Decimal) -> Amount {
    let style = AmountStyle {
        side: Side::R,
        precision: Precision::Fixed(2),
        ..AmountStyle::default()
    };
    with_style("h", quantity, style)
}

fn money(symbol: &str, quantity: Decimal) -> Amount {
    let style = AmountStyle {
        precision: Precision::Fixed(2),
        ..AmountStyle::default()
    };
    with_style(symbol, quantity.round_to(2), style)
}

/// Dollars, rounded to whole cents.
pub fn usd(quantity: Decimal) -> Amount {
    money("$", quantity)
}

/// Euros, rounded to whole cents.
pub fn eur(quantity: Decimal) -> Amount {
    money("€", quantity)
}

/// Pounds, rounded to whole pence.
pub fn gbp(quantity: Decimal) -> Amount {
    money("£", quantity)
}

/// A percentage: `%` on the right after a space, one decimal place.
pub fn per(quantity: Decimal) -> Amount {
    let style = AmountStyle {
        side: Side::R,
        spaced: true,
        precision: Precision::Fixed(1),
        ..AmountStyle::default()
    };
    with_style("%", quantity, style)
}

/// A sum of amounts, kept with at most one amount for each commodity and
/// cost: amounts with the same commodity and no cost, the same unit cost, or
/// a total cost in the same commodity, are added together. They are kept in
/// order of commodity, then cost commodity (none first), then total costs
/// before unit costs, which are in order of unit cost (upstream's
/// MixedAmountKey).
///
/// Equality and order compare the quantities and total costs commodity by
/// commodity, a commodity missing from one side counting as zero (maCompare).
#[derive(Clone, Debug, Default)]
pub struct MixedAmount(Vec<Amount>);

/// The order of MixedAmountKey: commodity, cost commodity, then cost kind
/// and unit cost.
fn key_cmp(a: &Amount, b: &Amount) -> Ordering {
    fn cost_commodity(a: &Amount) -> Option<&Sym> {
        match &a.cost {
            Some(AmountCost::Unit(c) | AmountCost::Total(c)) => Some(&c.commodity),
            None => None,
        }
    }
    fn cost_detail(a: &Amount) -> Option<(u8, Option<&Decimal>)> {
        match &a.cost {
            None => None,
            Some(AmountCost::Total(_)) => Some((0, None)),
            Some(AmountCost::Unit(c)) => Some((1, Some(&c.quantity))),
        }
    }
    a.commodity
        .cmp(&b.commodity)
        .then_with(|| cost_commodity(a).cmp(&cost_commodity(b)))
        .then_with(|| cost_detail(a).cmp(&cost_detail(b)))
}

/// Two amounts with the same commodity and cost added together: the second's
/// commodity and style with the larger precision, the first's unit cost, or
/// the sum of their total costs, and no cost basis (sumSimilarAmounts).
fn sum_similar(a: &Amount, b: &Amount) -> Amount {
    let cost = match (&a.cost, &b.cost) {
        (Some(AmountCost::Total(ac)), Some(AmountCost::Total(bc))) => {
            Some(AmountCost::Total(Box::new(Amount {
                quantity: &ac.quantity + &bc.quantity,
                ..(**ac).clone()
            })))
        }
        _ => a.cost.clone(),
    };
    Amount { cost, ..a + b }
}

impl MixedAmount {
    /// No amounts: zero (nullmixedamt).
    pub fn null() -> MixedAmount {
        MixedAmount(Vec::new())
    }

    /// The marker for an amount not written, to be inferred (missingmixedamt).
    pub fn missing() -> MixedAmount {
        MixedAmount::from_amount(Amount::missing())
    }

    /// Just this amount (mixedAmount).
    pub fn from_amount(a: Amount) -> MixedAmount {
        MixedAmount(vec![a])
    }

    /// These amounts added up (mixed).
    pub fn from_amounts(amounts: impl IntoIterator<Item = Amount>) -> MixedAmount {
        let mut m = MixedAmount::null();
        for a in amounts {
            m.add_amount(a);
        }
        m
    }

    /// Add an amount; one with the same commodity and cost as one already
    /// here is added to that (maAddAmount).
    pub fn add_amount(&mut self, a: Amount) {
        match self.0.binary_search_by(|x| key_cmp(x, &a)) {
            Ok(i) => self.0[i] = sum_similar(&a, &self.0[i]),
            Err(i) => self.0.insert(i, a),
        }
    }

    /// Whether this contains the missing amount marker (isMissingMixedAmount).
    pub fn is_missing(&self) -> bool {
        self.0.iter().any(|a| a.cost.is_none() && a.is_missing())
    }

    /// The amounts to show: just the missing amount marker if that is here;
    /// otherwise the non-zero amounts; or if there are none, one zero, in the
    /// first commodity that has a zero here, or with no commodity (amounts).
    pub fn amounts(&self) -> Vec<Amount> {
        if self.is_missing() {
            return vec![Amount::missing()];
        }
        let (zeros, nonzeros): (Vec<&Amount>, Vec<&Amount>) =
            self.0.iter().partition(|a| a.is_zero());
        if nonzeros.is_empty() {
            let zero = zeros.into_iter().find(|a| !a.commodity.is_empty());
            vec![zero.map_or_else(Amount::null, Amount::clone)]
        } else {
            nonzeros.into_iter().cloned().collect()
        }
    }

    /// Like [`MixedAmount::amounts`], but when all are zero, all of the zeros
    /// (amountsPreservingZeros).
    pub fn amounts_preserving_zeros(&self) -> Vec<Amount> {
        if self.is_missing() {
            return vec![Amount::missing()];
        }
        let (zeros, nonzeros): (Vec<&Amount>, Vec<&Amount>) =
            self.0.iter().partition(|a| a.is_zero());
        match (nonzeros.is_empty(), zeros.is_empty()) {
            (false, _) => nonzeros.into_iter().cloned().collect(),
            (true, false) => zeros.into_iter().cloned().collect(),
            (true, true) => vec![Amount::null()],
        }
    }

    /// All the amounts, as they are stored (amountsRaw).
    pub fn amounts_raw(&self) -> &[Amount] {
        &self.0
    }

    /// The commodities of the amounts to show; none if there are no amounts
    /// (maCommodities).
    pub fn commodities(&self) -> BTreeSet<Sym> {
        if self.0.is_empty() {
            return BTreeSet::new();
        }
        self.amounts().into_iter().map(|a| a.commodity).collect()
    }

    /// Just the amounts satisfying a predicate (filterMixedAmount).
    pub fn filter(self, keep: impl FnMut(&Amount) -> bool) -> MixedAmount {
        let mut amounts = self.0;
        amounts.retain(keep);
        MixedAmount(amounts)
    }

    /// Just the amounts in this commodity, or a zero in it if there are none
    /// (filterMixedAmountByCommodity).
    pub fn filter_by_commodity(&self, commodity: &Sym) -> MixedAmount {
        let amounts: Vec<Amount> = self
            .0
            .iter()
            .filter(|a| &a.commodity == commodity)
            .cloned()
            .collect();
        if amounts.is_empty() {
            MixedAmount::from_amount(Amount {
                commodity: commodity.clone(),
                ..Amount::null()
            })
        } else {
            MixedAmount(amounts)
        }
    }

    /// The amounts transformed, then added up again (mapMixedAmount).
    pub fn map(&self, f: impl FnMut(&Amount) -> Amount) -> MixedAmount {
        MixedAmount::from_amounts(self.0.iter().map(f))
    }

    /// The amounts transformed in a way that keeps their commodities and
    /// costs, and so their order (mapMixedAmountUnsafe).
    fn map_in_place(self, f: impl FnMut(Amount) -> Amount) -> MixedAmount {
        MixedAmount(self.0.into_iter().map(f).collect())
    }

    /// The single amount this adds up to, ignoring costs and zeros, if it has
    /// at most one commodity (unifyMixedAmount).
    pub fn unify(&self) -> Option<Amount> {
        let mut total = Amount::null();
        for a in self.amounts() {
            total = if total.is_zero() {
                a
            } else if a.is_zero() {
                total
            } else if total.commodity == a.commodity {
                &total + &a
            } else {
                return None;
            };
        }
        Some(total)
    }

    /// Without costs or cost bases, amounts that then have the same
    /// commodity added together (mixedAmountStripCosts).
    pub fn strip_costs(self) -> MixedAmount {
        let costless = |a: &Amount| a.cost.is_none() && a.cost_basis.is_none();
        if self.0.iter().all(costless) {
            return self;
        }
        let (plain, costly): (Vec<Amount>, Vec<Amount>) = self.0.into_iter().partition(costless);
        let mut m = MixedAmount(plain);
        for a in costly {
            m.add_amount(Amount {
                cost: None,
                cost_basis: None,
                ..a
            });
        }
        m
    }

    /// The amounts with costs converted to their cost (mixedAmountCost).
    pub fn to_cost(self) -> MixedAmount {
        if self.0.iter().all(|a| a.cost.is_none()) {
            return self;
        }
        let (plain, costly): (Vec<Amount>, Vec<Amount>) =
            self.0.into_iter().partition(|a| a.cost.is_none());
        let mut m = MixedAmount(plain);
        for a in costly {
            m.add_amount(a.to_cost());
        }
        m
    }

    /// The amounts with a cost basis or cost converted to that
    /// (mixedAmountCostBasis).
    pub fn to_cost_basis(self) -> MixedAmount {
        let (plain, costly): (Vec<Amount>, Vec<Amount>) = self.0.into_iter().partition(|a| {
            a.cost.is_none()
                && a.cost_basis
                    .as_ref()
                    .and_then(|b| b.cost.as_ref())
                    .is_none()
        });
        let mut m = MixedAmount(plain);
        for a in costly {
            m.add_amount(a.to_cost_basis());
        }
        m
    }

    /// Quantities and total costs divided by this number; unchanged if it is
    /// zero (divideMixedAmount).
    pub fn divide(self, n: &Decimal) -> MixedAmount {
        if n.is_zero() {
            self
        } else {
            self.map_in_place(|a| a.divide(n))
        }
    }

    /// Quantities and total costs multiplied by this number
    /// (multiplyMixedAmount).
    pub fn multiply(self, n: &Decimal) -> MixedAmount {
        self.map_in_place(|a| a.multiply(n))
    }

    /// The sum of some mixed amounts, and their average
    /// (sumAndAverageMixedAmounts).
    pub fn sum_and_average<'a>(
        amounts: impl IntoIterator<Item = &'a MixedAmount>,
    ) -> (MixedAmount, MixedAmount) {
        let mut count = 0;
        let mut total = MixedAmount::null();
        for m in amounts {
            count += 1;
            total = &total + m;
        }
        let average = total.clone().divide(&Decimal::from_i64(count));
        (total, average)
    }

    /// The average of some mixed amounts (averageMixedAmounts).
    pub fn average<'a>(amounts: impl IntoIterator<Item = &'a MixedAmount>) -> MixedAmount {
        MixedAmount::sum_and_average(amounts).1
    }

    /// Whether the amounts, without costs, are all negative (Some(true)),
    /// all not negative (Some(false)), or mixed (None) (isNegativeMixedAmount).
    pub fn is_negative(&self) -> Option<bool> {
        let amounts = self.clone().strip_costs().amounts();
        match amounts.as_slice() {
            [a] => Some(a.is_negative()),
            amounts if amounts.iter().all(Amount::is_negative) => Some(true),
            amounts if !amounts.iter().any(Amount::is_negative) => Some(false),
            _ => None,
        }
    }

    /// Whether every amount is exactly zero (mixedAmountIsZero).
    pub fn is_zero(&self) -> bool {
        self.0.iter().all(Amount::is_zero)
    }

    /// Whether every amount shows as zero (mixedAmountLooksZero).
    pub fn looks_zero(&self) -> bool {
        self.0.iter().all(Amount::looks_zero)
    }

    /// With the quantities' absolute values.
    pub fn abs(&self) -> MixedAmount {
        self.map(Amount::abs)
    }

    /// Each amount with this display precision (mixedAmountSetPrecision).
    pub fn with_precision(self, precision: Precision) -> MixedAmount {
        self.map_in_place(|a| a.with_precision(precision))
    }

    /// Each amount with at most this many decimal places shown
    /// (mixedAmountSetPrecisionMax).
    pub fn with_precision_max(self, max: u8) -> MixedAmount {
        self.map_in_place(|a| a.with_precision_max(max))
    }

    /// Each amount with enough decimal places to show it exactly
    /// (mixedAmountSetFullPrecision).
    pub fn with_full_precision(self) -> MixedAmount {
        self.map_in_place(Amount::with_full_precision)
    }

    /// Like [`MixedAmount::with_full_precision`], but an infinite decimal gets
    /// 8 decimal places (mixedAmountSetFullPrecisionCapped).
    pub fn with_full_precision_capped(self) -> MixedAmount {
        self.map_in_place(|a| {
            if a.quantity.has_max_digits() {
                a.with_full_precision_up_to(None)
            } else {
                a.with_full_precision()
            }
        })
    }

    /// Each amount with enough decimal places to show it exactly, up to a
    /// maximum (mixedAmountSetFullPrecisionUpTo).
    pub fn with_full_precision_up_to(self, max: Option<u8>) -> MixedAmount {
        self.map_in_place(|a| a.with_full_precision_up_to(max))
    }

    /// Each amount styled with its commodity's style (styleAmounts).
    pub fn styled(self, styles: &CommodityStyles) -> MixedAmount {
        self.map_in_place(|a| a.styled(styles))
    }

    /// Each amount with the default style (mixedAmountUnstyled).
    pub fn unstyled(self) -> MixedAmount {
        self.map_in_place(Amount::unstyled)
    }
}

/// maCompare.
fn ma_compare(a: &[Amount], b: &[Amount]) -> Ordering {
    fn quantity(a: Option<&Amount>) -> Decimal {
        a.map_or(Decimal::ZERO, |a| a.quantity.clone())
    }
    fn total_cost(a: Option<&Amount>) -> Decimal {
        match a.and_then(|a| a.cost.as_ref()) {
            Some(AmountCost::Total(c)) => c.quantity.clone(),
            _ => Decimal::ZERO,
        }
    }
    let compare = |x: Option<&Amount>, y: Option<&Amount>| {
        quantity(x)
            .cmp(&quantity(y))
            .then_with(|| total_cost(x).cmp(&total_cost(y)))
    };
    let (mut i, mut j) = (0, 0);
    loop {
        let (x, y) = (a.get(i), b.get(j));
        let ord = match (x, y) {
            (None, None) => return Ordering::Equal,
            (Some(xa), Some(ya)) => match key_cmp(xa, ya) {
                Ordering::Equal => {
                    (i, j) = (i + 1, j + 1);
                    compare(x, y)
                }
                Ordering::Less => {
                    i += 1;
                    compare(x, None)
                }
                Ordering::Greater => {
                    j += 1;
                    compare(None, y)
                }
            },
            (Some(_), None) => {
                i += 1;
                compare(x, None)
            }
            (None, Some(_)) => {
                j += 1;
                compare(None, y)
            }
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
}

impl PartialEq for MixedAmount {
    fn eq(&self, other: &MixedAmount) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for MixedAmount {}

impl Ord for MixedAmount {
    fn cmp(&self, other: &MixedAmount) -> Ordering {
        ma_compare(&self.0, &other.0)
    }
}

impl PartialOrd for MixedAmount {
    fn partial_cmp(&self, other: &MixedAmount) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Two sums' amounts, each in key order, merged; amounts with the same
/// commodity and cost are added, the right one's commodity and style
/// winning (maPlus).
fn merged(
    a: impl ExactSizeIterator<Item = Amount>,
    b: impl ExactSizeIterator<Item = Amount>,
) -> MixedAmount {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut a, mut b) = (a.peekable(), b.peekable());
    while let (Some(x), Some(y)) = (a.peek(), b.peek()) {
        match key_cmp(x, y) {
            Ordering::Less => out.extend(a.next()),
            Ordering::Greater => out.extend(b.next()),
            Ordering::Equal => {
                let (x, y) = (a.next().expect("peeked"), b.next().expect("peeked"));
                out.push(sum_similar(&x, &y));
            }
        }
    }
    out.extend(a);
    out.extend(b);
    MixedAmount(out)
}

/// The two sums merged (maPlus).
impl Add for &MixedAmount {
    type Output = MixedAmount;

    fn add(self, rhs: &MixedAmount) -> MixedAmount {
        merged(self.0.iter().cloned(), rhs.0.iter().cloned())
    }
}

/// The two sums merged, reusing the left one's amounts (maPlus).
impl Add<&MixedAmount> for MixedAmount {
    type Output = MixedAmount;

    fn add(self, rhs: &MixedAmount) -> MixedAmount {
        match rhs.0.as_slice() {
            [b] if self.0.len() >= LONG => self.with_amount_on_right(b.clone()),
            _ => merged(self.0.into_iter(), rhs.0.iter().cloned()),
        }
    }
}

/// The two sums merged, reusing the right one's amounts (maPlus).
impl Add<MixedAmount> for &MixedAmount {
    type Output = MixedAmount;

    fn add(self, rhs: MixedAmount) -> MixedAmount {
        match self.0.as_slice() {
            [a] if rhs.0.len() >= LONG => rhs.with_amount_on_left(a.clone()),
            _ => merged(self.0.iter().cloned(), rhs.0.into_iter()),
        }
    }
}

/// The two sums merged, reusing their amounts (maPlus).
impl Add for MixedAmount {
    type Output = MixedAmount;

    fn add(mut self, mut rhs: MixedAmount) -> MixedAmount {
        if rhs.0.len() == 1 && self.0.len() >= LONG {
            let b = rhs.0.pop().expect("one amount");
            self.with_amount_on_right(b)
        } else if self.0.len() == 1 && rhs.0.len() >= LONG {
            let a = self.0.pop().expect("one amount");
            rhs.with_amount_on_left(a)
        } else {
            merged(self.0.into_iter(), rhs.0.into_iter())
        }
    }
}

/// The length from which one amount is added to a sum in place, rather
/// than by merging the two into a new one; shorter sums are merged, which
/// allocates no more than they need.
const LONG: usize = 8;

impl MixedAmount {
    /// This sum plus one amount on its right, as `merged` would add them,
    /// in place: a long sum plus a posting's amount costs no more than
    /// finding its place.
    fn with_amount_on_right(mut self, b: Amount) -> MixedAmount {
        match self.0.binary_search_by(|x| key_cmp(x, &b)) {
            Ok(i) => self.0[i] = sum_similar(&self.0[i], &b),
            Err(i) => self.0.insert(i, b),
        }
        self
    }

    /// One amount plus this sum on its right, likewise.
    fn with_amount_on_left(mut self, a: Amount) -> MixedAmount {
        match self.0.binary_search_by(|x| key_cmp(x, &a)) {
            Ok(i) => self.0[i] = sum_similar(&a, &self.0[i]),
            Err(i) => self.0.insert(i, a),
        }
        self
    }
}

/// maMinus.
impl Sub for &MixedAmount {
    type Output = MixedAmount;

    fn sub(self, rhs: &MixedAmount) -> MixedAmount {
        self + &-rhs
    }
}

/// Negates the quantities and total costs (maNegate).
impl Neg for MixedAmount {
    type Output = MixedAmount;

    fn neg(self) -> MixedAmount {
        self.map_in_place(Amount::neg)
    }
}

impl Neg for &MixedAmount {
    type Output = MixedAmount;

    fn neg(self) -> MixedAmount {
        -self.clone()
    }
}

/// maSum.
impl<'a> Sum<&'a MixedAmount> for MixedAmount {
    fn sum<I: Iterator<Item = &'a MixedAmount>>(iter: I) -> MixedAmount {
        iter.fold(MixedAmount::null(), |total, m| total + m)
    }
}

impl Sum<MixedAmount> for MixedAmount {
    fn sum<I: Iterator<Item = MixedAmount>>(iter: I) -> MixedAmount {
        iter.fold(MixedAmount::null(), |total, m| total + m)
    }
}

pub(crate) mod tests {
    use super::*;
    use Precision::Fixed;

    /// A decimal as a Haskell literal makes it: with the fewest places.
    fn d(s: &str) -> Decimal {
        s.parse::<Decimal>().unwrap().normalize()
    }

    fn mixed<const N: usize>(amounts: [Amount; N]) -> MixedAmount {
        MixedAmount::from_amounts(amounts)
    }

    #[cfg(test)]
    fn sym(s: &str) -> Sym {
        Sym::from(s)
    }

    // tests_Amount "amountCost".
    #[cfg_attr(test, test)]
    pub(crate) fn amount_cost() {
        assert_eq!(eur(d("1")).to_cost(), eur(d("1")));
        assert_eq!(eur(d("2")).at(usd(d("2"))).to_cost(), usd(d("4")));
        assert_eq!(eur(d("1")).at_total(usd(d("2"))).to_cost(), usd(d("2")));
        assert_eq!(eur(d("-1")).at_total(usd(d("-2"))).to_cost(), usd(d("-2")));
    }

    // tests_Amount "amountLooksZero".
    #[cfg_attr(test, test)]
    pub(crate) fn amount_looks_zero() {
        assert!(Amount::null().looks_zero());
        assert!(usd(d("0")).looks_zero());
    }

    #[test]
    fn looks_zero_at_display_precision() {
        let at2 = |q: &str| num(q.parse().unwrap()).with_precision(Fixed(2));
        assert!(at2("0.005").looks_zero());
        assert!(at2("-0.005").looks_zero());
        assert!(!at2("0.0051").looks_zero());
        assert!(!at2("0.015").looks_zero());
        assert!(!at2("0.01").looks_zero());
        assert!(at2("0.00").looks_zero());
        assert!(
            !num(d("0.001"))
                .with_precision(Precision::Natural)
                .looks_zero()
        );
        assert!(!usd(d("0")).at_total(eur(d("1"))).looks_zero());
        assert!(!usd(d("0")).at_total(eur(d("1"))).is_zero());
        assert!(usd(d("0")).at(eur(d("1"))).is_zero());
        assert!(!at2("0.001").is_zero());
    }

    // tests_Amount "negating amounts".
    #[cfg_attr(test, test)]
    pub(crate) fn negating_amounts() {
        assert_eq!(
            -usd(d("1")),
            Amount {
                quantity: d("-1"),
                ..usd(d("1"))
            }
        );
        let b = usd(d("1")).at(eur(d("2")));
        assert_eq!(
            -&b,
            Amount {
                quantity: d("-1"),
                ..b.clone()
            }
        );
        // A total cost is negated too.
        let t = -usd(d("1")).at_total(eur(d("2")));
        assert_eq!(t.to_cost(), eur(d("-2")));
    }

    // tests_Amount "adding amounts without costs".
    #[cfg_attr(test, test)]
    pub(crate) fn adding_amounts_without_costs() {
        assert_eq!(&usd(d("1.23")) + &usd(d("-1.23")), usd(d("0")));
        assert_eq!(&usd(d("-1.23")) + &usd(d("-1.23")), usd(d("-2.46")));
        let sum = |amounts: &[Amount]| amounts.iter().fold(num(Decimal::ZERO), |t, a| &t + a);
        let a = [
            usd(d("1.23")),
            usd(d("-1.23")),
            usd(d("-1.23")),
            -usd(d("-1.23")),
        ];
        assert_eq!(sum(&a), usd(d("0")));
        // The highest precision is preserved.
        let p1 = usd(d("1")).with_precision(Fixed(1));
        let p3 = usd(d("1")).with_precision(Fixed(3));
        assert_eq!(sum(&[p1.clone(), p3.clone()]).style.precision, Fixed(3));
        assert_eq!(sum(&[p3, p1]).style.precision, Fixed(3));
        // Adding different commodities assumes a conversion rate of 1.
        assert!((&usd(d("1.23")) - &eur(d("1.23"))).looks_zero());
    }

    #[test]
    fn arithmetic_takes_the_second_operands_commodity_and_style() {
        let a = usd(d("2")).at(eur(d("1")));
        let b = hrs(d("3"));
        let product = &a * &b;
        assert_eq!(&*product.commodity, "h");
        assert_eq!(product.quantity, d("6"));
        assert_eq!(product.style.side, Side::R);
        assert_eq!(product.cost, None);
    }

    #[test]
    fn division_multiplication_and_inversion() {
        let a = usd(d("10")).at_total(eur(d("9")));
        let half = a.clone().divide(&d("2"));
        assert_eq!(half.quantity, d("5"));
        assert_eq!(half.to_cost().quantity, d("4.5"));
        assert_eq!(a.clone().divide(&Decimal::ZERO), a);
        let unit = usd(d("10")).at(eur(d("9"))).multiply(&d("2"));
        assert_eq!(
            (unit.quantity.clone(), unit.to_cost().quantity),
            (d("20"), d("180"))
        );
        let third = usd(d("10")).divide_and_update_precision(&d("3"));
        assert_eq!(third.style.precision, Fixed(8));
        let quarter = usd(d("1")).divide_and_update_precision(&d("4"));
        assert_eq!(
            (quarter.quantity.clone(), quarter.style.precision),
            (d("0.25"), Fixed(2))
        );
        assert_eq!(num(d("4")).invert().quantity, d("0.25"));
        assert_eq!(num(d("0")).invert(), num(d("0")));
        assert_eq!(usd(d("-2")).abs(), usd(d("2")));
    }

    #[test]
    fn precisions() {
        let a = num(d("1.2345"));
        assert_eq!(a.internal_precision(), 4);
        assert_eq!(a.display_precision(), 0);
        assert_eq!(a.clone().with_full_precision().style.precision, Fixed(4));
        assert_eq!(usd(d("1")).with_full_precision().style.precision, Fixed(2));
        let natural = a.clone().with_precision(Precision::Natural);
        assert_eq!(natural.display_precision(), 4);
        assert_eq!(
            natural.clone().with_full_precision().style.precision,
            Precision::Natural
        );
        assert_eq!(
            a.clone().with_full_precision_up_to(Some(3)).style.precision,
            Fixed(3)
        );
        assert_eq!(
            a.clone().with_full_precision_up_to(None).style.precision,
            Fixed(4)
        );
        let third = num(&d("1") / &d("3"));
        assert_eq!(
            third.with_full_precision_up_to(None).style.precision,
            Fixed(8)
        );
        assert_eq!(a.clone().with_precision_min(2).style.precision, Fixed(2));
        assert_eq!(usd(d("1")).with_precision_min(1).style.precision, Fixed(2));
        assert_eq!(usd(d("1")).with_precision_max(1).style.precision, Fixed(1));
        assert_eq!(natural.with_precision_max(9).style.precision, Fixed(4));
        let rounded = num(d("1.25")).with_internal_precision(1);
        assert_eq!(
            (rounded.quantity.to_string(), rounded.style.precision),
            ("1.2".into(), Fixed(1))
        );
        assert_eq!(
            num(d("1.5"))
                .with_internal_precision(3)
                .quantity
                .to_string(),
            "1.500"
        );
        assert_eq!(
            usd(d("1")).with_quantity(d("2.5")).style.precision,
            Precision::Natural
        );
        assert_eq!(
            num(d("1.005"))
                .with_precision(Fixed(2))
                .rounded_quantity()
                .to_string(),
            "1.00"
        );
        assert_eq!(
            num(d("1.015"))
                .with_precision(Fixed(2))
                .rounded_quantity()
                .to_string(),
            "1.02"
        );
        assert_eq!(
            num(d("1.015"))
                .with_precision(Precision::Natural)
                .rounded_quantity()
                .to_string(),
            "1.015"
        );
    }

    #[test]
    fn costs_and_bases() {
        let basis = CostBasis {
            date: Day::from_ymd(2024, 1, 2),
            label: None,
            cost: Some(Box::new(usd(d("3")))),
        };
        let lot = num(d("5")).at(usd(d("4")));
        let lot = Amount {
            commodity: sym("X"),
            cost_basis: Some(Box::new(basis)),
            ..lot
        };
        assert_eq!(lot.to_cost().quantity, d("20"));
        assert_eq!(lot.to_cost_basis().quantity, d("15"));
        assert_eq!(lot.clone().set_cost_to_basis().to_cost().quantity, d("15"));
        assert_eq!(usd(d("1")).to_cost_basis(), usd(d("1")));
        assert_eq!(lot.clone().strip_cost().cost, None);
        assert_eq!(lot.and_cost().count(), 2);
        assert_eq!(usd(d("1")).and_cost().count(), 1);
        assert_eq!(
            &*usd(d("1"))
                .at(eur(d("1")))
                .with_commodity(sym("X"))
                .commodity,
            "X"
        );
        assert!(Amount::missing().is_missing() && !Amount::null().is_missing());
    }

    #[cfg(test)]
    fn style(precision: Precision, rounding: Rounding) -> Arc<AmountStyle> {
        Arc::new(AmountStyle {
            spaced: true,
            digit_groups: Some(DigitGroups {
                sep: ',',
                sizes: vec![3],
            }),
            precision,
            rounding,
            ..AmountStyle::default()
        })
    }

    #[cfg(test)]
    /// The precisions of an amount (with old precision `old` and quantity
    /// `q`) and of its unit cost (precision `old` too), after styling both
    /// commodities with `new` and `rounding`.
    fn restyled(
        q: &str,
        old: Precision,
        new: Precision,
        rounding: Rounding,
    ) -> (Precision, Precision) {
        let styles = CommodityStyles::from([
            (sym("A"), style(new, rounding)),
            (sym("B"), style(new, rounding)),
        ]);
        let cost = Amount {
            commodity: sym("B"),
            ..num(d("1"))
        }
        .with_precision(old);
        let a = Amount {
            commodity: sym("A"),
            ..num(q.parse().unwrap())
        }
        .with_precision(old)
        .at(cost);
        let a = a.styled(&styles);
        let Some(AmountCost::Unit(c)) = &a.cost else {
            unreachable!()
        };
        (a.style.precision, c.style.precision)
    }

    #[test]
    fn style_application_follows_the_rounding_strategy() {
        use Rounding::*;
        let n = Precision::Natural;
        assert_eq!(
            restyled("1.5", Fixed(1), Fixed(2), No),
            (Fixed(1), Fixed(1))
        );
        assert_eq!(
            restyled("1.5", Fixed(1), Fixed(2), Soft),
            (Fixed(2), Fixed(1))
        );
        assert_eq!(
            restyled("1.230", Fixed(3), Fixed(1), Soft),
            (Fixed(2), Fixed(3))
        );
        assert_eq!(
            restyled("1.500", Fixed(3), Fixed(0), Soft),
            (Fixed(1), Fixed(3))
        );
        assert_eq!(restyled("1.5", n, Fixed(0), Soft), (n, n));
        assert_eq!(restyled("1.5", Fixed(1), n, Soft), (n, Fixed(1)));
        assert_eq!(
            restyled("1.2345", Fixed(4), Fixed(2), Hard),
            (Fixed(2), Fixed(4))
        );
        assert_eq!(
            restyled("1.2345", Fixed(4), Fixed(2), All),
            (Fixed(2), Fixed(2))
        );
        // The rest of the style is the commodity's.
        let styles = CommodityStyles::from([(sym("A"), style(Fixed(2), No))]);
        let a = Amount {
            commodity: sym("A"),
            ..num(d("1"))
        }
        .styled(&styles);
        assert!(a.style.spaced && a.style.digit_groups.is_some());
        // An amount whose commodity has no style is unchanged.
        assert_eq!(usd(d("1")).styled(&styles), usd(d("1")));
        assert_eq!(a.unstyled().style, Arc::new(AmountStyle::default()));
        let soft = styles_with_rounding(Soft, &styles);
        assert_eq!(soft[&sym("A")].rounding, Soft);
    }

    #[test]
    fn commodity_styles_are_merged_first_style_first() {
        let plain = |side, mark: Option<char>, p: u8| AmountStyle {
            side,
            decimal_mark: mark,
            precision: Fixed(p),
            ..AmountStyle::default()
        };
        let amount = |s: AmountStyle| Amount {
            commodity: sym("A"),
            style: Arc::new(s),
            ..num(d("1"))
        };
        let grouped = |sep| AmountStyle {
            digit_groups: Some(DigitGroups {
                sep,
                sizes: vec![3],
            }),
            ..plain(Side::R, None, 0)
        };
        // The first amount's side, the most decimal places.
        let styles = commodity_styles_from_amounts(&[
            amount(plain(Side::L, Some(','), 1)),
            amount(plain(Side::R, Some('.'), 3)),
        ]);
        assert_eq!(*styles[&sym("A")], plain(Side::L, Some(','), 3));
        // The first digit group style seen, and a decimal mark that differs.
        let styles = commodity_styles_from_amounts(&[
            amount(plain(Side::L, Some('.'), 2)),
            amount(grouped('.')),
        ]);
        assert_eq!(styles[&sym("A")].decimal_mark, Some(','));
        assert_eq!(styles[&sym("A")].digit_groups, grouped('.').digit_groups);
        let styles = commodity_styles_from_amounts(&[
            amount(grouped(',')),
            amount(plain(Side::L, Some(','), 2)),
        ]);
        assert_eq!(styles[&sym("A")].decimal_mark, Some('.'));
        // No decimal mark anywhere: a period.
        let styles = commodity_styles_from_amounts(&[
            amount(plain(Side::L, None, 0)),
            amount(plain(Side::L, None, 1)),
        ]);
        assert_eq!(styles[&sym("A")].decimal_mark, Some('.'));
        let mut styles = CommodityStyles::new();
        add_amount_style(&usd(d("1")), &mut styles);
        let first = styles[&sym("$")].clone();
        add_amount_style(&usd(d("2")), &mut styles);
        assert!(Arc::ptr_eq(&first, &styles[&sym("$")]));
    }

    #[test]
    fn constructors() {
        assert_eq!(usd(d("1.005")).quantity.to_string(), "1.00");
        assert_eq!(usd(d("1")).quantity.places(), 2);
        assert_eq!(
            (per(d("5")).style.side, per(d("5")).style.spaced),
            (Side::R, true)
        );
        assert_eq!(hrs(d("1")).style.precision, Fixed(2));
        assert_eq!(&*gbp(d("1")).commodity, "£");
        assert!(is_nonsimple_commodity_char('1') && is_nonsimple_commodity_char('{'));
        assert!(!is_nonsimple_commodity_char('$') && !is_nonsimple_commodity_char('('));
    }

    // tests_Amount "comparing mixed amounts compares based on quantities".
    #[cfg_attr(test, test)]
    pub(crate) fn comparing_mixed_amounts() {
        let usdpos = mixed([usd(d("1"))]);
        let usdneg = mixed([usd(d("-1"))]);
        let eurneg = mixed([eur(d("-12"))]);
        assert_eq!(usdneg.cmp(&usdpos), Ordering::Less);
        assert_eq!(eurneg.cmp(&usdpos), Ordering::Less);
        // A missing commodity counts as zero.
        assert_eq!(mixed([usd(d("0"))]), MixedAmount::null());
        assert_eq!(mixed([usd(d("1")), eur(d("0"))]), mixed([usd(d("1"))]));
        // Commodities are compared in key order: at $, the first has 0, the second 1.
        assert!(mixed([eur(d("1"))]) < mixed([usd(d("1"))]));
    }

    // tests_Amount "adding mixed amounts to zero, the commodity and amount
    // style are preserved".
    #[cfg_attr(test, test)]
    pub(crate) fn adding_mixed_amounts_to_zero() {
        let total: MixedAmount = [
            usd(d("1.25")),
            usd(d("-1")).with_precision(Fixed(3)),
            usd(d("-0.25")),
        ]
        .map(MixedAmount::from_amount)
        .iter()
        .sum();
        assert_eq!(
            total,
            MixedAmount::from_amount(usd(d("0")).with_precision(Fixed(3)))
        );
        assert_eq!(total.amounts(), [usd(d("0")).with_precision(Fixed(3))]);
    }

    // tests_Amount "adding mixed amounts with total costs".
    #[cfg_attr(test, test)]
    pub(crate) fn adding_mixed_amounts_with_total_costs() {
        let total: MixedAmount = [
            usd(d("1")).at_total(eur(d("1"))),
            usd(d("-2")).at_total(eur(d("1"))),
        ]
        .map(MixedAmount::from_amount)
        .iter()
        .sum();
        assert_eq!(
            total,
            MixedAmount::from_amount(usd(d("-1")).at_total(eur(d("2"))))
        );
        assert_eq!(total.amounts(), [usd(d("-1")).at_total(eur(d("2")))]);
    }

    // tests_Amount "amounts".
    #[cfg_attr(test, test)]
    pub(crate) fn amounts() {
        // A missing amount overrides any other amounts.
        assert_eq!(
            mixed([usd(d("1")), Amount::missing()]).amounts(),
            [Amount::missing()]
        );
        // Costless same-commodity amounts are combined.
        assert_eq!(mixed([usd(d("0")), usd(d("2"))]).amounts(), [usd(d("2"))]);
        // Amounts with the same unit cost are combined.
        assert_eq!(
            mixed([usd(d("1")).at(eur(d("1"))), usd(d("1")).at(eur(d("1")))]).amounts(),
            [usd(d("2")).at(eur(d("1")))]
        );
        // Amounts with different unit costs are not combined.
        assert_eq!(
            mixed([usd(d("1")).at(eur(d("1"))), usd(d("1")).at(eur(d("2")))]).amounts(),
            [usd(d("1")).at(eur(d("1"))), usd(d("1")).at(eur(d("2")))]
        );
        // Amounts with total costs are combined.
        assert_eq!(
            mixed([
                usd(d("1")).at_total(eur(d("1"))),
                usd(d("1")).at_total(eur(d("1")))
            ])
            .amounts(),
            [usd(d("2")).at_total(eur(d("2")))]
        );
    }

    #[test]
    fn zeros_in_amounts() {
        assert_eq!(MixedAmount::null().amounts(), [Amount::null()]);
        assert_eq!(
            mixed([num(d("0")), usd(d("0")), eur(d("0"))]).amounts(),
            [usd(d("0"))]
        );
        assert_eq!(
            mixed([usd(d("0")), eur(d("0"))]).amounts_preserving_zeros(),
            [usd(d("0")), eur(d("0"))]
        );
        assert_eq!(
            mixed([usd(d("0")), eur(d("1"))]).amounts_preserving_zeros(),
            [eur(d("1"))]
        );
        assert_eq!(
            MixedAmount::null().amounts_preserving_zeros(),
            [Amount::null()]
        );
        assert_eq!(
            MixedAmount::missing().amounts_preserving_zeros(),
            [Amount::missing()]
        );
        assert!(MixedAmount::missing().is_missing());
    }

    // tests_Amount "mixedAmountStripCosts".
    #[cfg_attr(test, test)]
    pub(crate) fn mixed_amount_strip_costs() {
        assert_eq!(
            MixedAmount::null().strip_costs().amounts(),
            [Amount::null()]
        );
        assert!(
            mixed([
                usd(d("10")),
                usd(d("10")).at_total(eur(d("7"))),
                usd(d("-10")),
                usd(d("-10")).at_total(eur(d("-7"))),
            ])
            .strip_costs()
            .looks_zero()
        );
    }

    #[test]
    fn mixed_amounts_are_kept_in_key_order() {
        let m = mixed([
            eur(d("3")),
            usd(d("1")).at(eur(d("2"))),
            usd(d("1")),
            usd(d("1")).at_total(eur(d("5"))),
            usd(d("1")).at(eur(d("1"))),
            usd(d("1")).at_total(gbp(d("5"))),
        ]);
        let keys: Vec<String> = m
            .amounts_raw()
            .iter()
            .map(|a| match &a.cost {
                None => a.commodity.to_string(),
                Some(AmountCost::Unit(c)) => {
                    format!("{}@{}{}", a.commodity, c.commodity, c.quantity)
                }
                Some(AmountCost::Total(c)) => format!("{}@@{}", a.commodity, c.commodity),
            })
            .collect();
        assert_eq!(keys, ["$", "$@@£", "$@@€", "$@€1.00", "$@€2.00", "€"]);
    }

    #[test]
    fn merging_keeps_styles_as_upstream_does() {
        let narrow = usd(d("1"));
        let wide = usd(d("2")).with_precision(Fixed(4));
        // maPlus: the right operand's style, with the larger precision.
        let sum = &mixed([wide.clone()])
            + &mixed([Amount {
                style: hrs(d("0")).style,
                ..narrow.clone()
            }]);
        assert_eq!(sum.amounts_raw()[0].style.side, Side::R);
        assert_eq!(sum.amounts_raw()[0].style.precision, Fixed(4));
        // maAddAmount: the style of the amount already there.
        let mut m = mixed([narrow.clone()]);
        m.add_amount(Amount {
            style: hrs(d("0")).style,
            ..wide
        });
        assert_eq!(m.amounts_raw()[0].style.side, Side::L);
        assert_eq!(m.amounts_raw()[0].quantity, d("3"));
        // A cost basis is dropped when amounts are added, kept otherwise.
        let based = Amount {
            cost_basis: Some(Box::default()),
            ..narrow.clone()
        };
        assert!(mixed([based.clone()]).amounts_raw()[0].cost_basis.is_some());
        assert!(
            mixed([based.clone(), based]).amounts_raw()[0]
                .cost_basis
                .is_none()
        );
    }

    #[test]
    fn mixed_amount_operations() {
        let m = mixed([usd(d("1")), eur(d("2"))]);
        assert_eq!((-&m).amounts(), [usd(d("-1")), eur(d("-2"))]);
        assert_eq!((&m - &m).amounts(), [usd(d("0"))]);
        assert!((&m - &m).is_zero());
        assert_eq!(m.commodities(), BTreeSet::from([sym("$"), sym("€")]));
        assert!(MixedAmount::null().commodities().is_empty());
        assert_eq!(m.filter_by_commodity(&sym("€")).amounts(), [eur(d("2"))]);
        assert_eq!(
            m.filter_by_commodity(&sym("£")).amounts_raw()[0].commodity,
            sym("£")
        );
        assert_eq!(
            m.clone().filter(|a| &*a.commodity == "$").amounts(),
            [usd(d("1"))]
        );
        let merged = m.map(|a| a.clone().with_commodity(sym("X")));
        assert_eq!(
            (
                merged.amounts_raw().len(),
                merged.amounts_raw()[0].quantity.clone()
            ),
            (1, d("3"))
        );
        assert_eq!(
            mixed([usd(d("-1")), eur(d("2"))]).abs().amounts(),
            [usd(d("1")), eur(d("2"))]
        );
        let converted = mixed([usd(d("1")), eur(d("2")).at(usd(d("1.5")))]).to_cost();
        assert_eq!(converted.amounts(), [usd(d("4"))]);
        let doubled = m.clone().multiply(&d("2")).divide(&d("4"));
        assert_eq!(doubled.amounts(), [usd(d("0.5")), eur(d("1"))]);
    }

    #[test]
    fn unify_and_signs() {
        assert_eq!(
            mixed([usd(d("1")), usd(d("2")).at(eur(d("1")))]).unify(),
            Some(usd(d("3")))
        );
        assert_eq!(mixed([usd(d("1")), eur(d("2"))]).unify(), None);
        assert_eq!(mixed([usd(d("0")), eur(d("2"))]).unify(), Some(eur(d("2"))));
        assert_eq!(MixedAmount::null().unify(), Some(Amount::null()));
        assert_eq!(
            mixed([usd(d("-1")), eur(d("-2"))]).is_negative(),
            Some(true)
        );
        assert_eq!(mixed([usd(d("-1")), eur(d("2"))]).is_negative(), None);
        assert_eq!(mixed([usd(d("1"))]).is_negative(), Some(false));
        assert_eq!(MixedAmount::null().is_negative(), Some(false));
    }

    #[test]
    fn sums_and_averages() {
        let ms = [
            mixed([usd(d("1"))]),
            mixed([usd(d("2"))]),
            mixed([usd(d("6"))]),
        ];
        let (total, average) = MixedAmount::sum_and_average(&ms);
        assert_eq!(
            (total.amounts(), average.amounts()),
            (vec![usd(d("9"))], vec![usd(d("3"))])
        );
        assert_eq!(MixedAmount::average(&ms), average);
        assert_eq!(MixedAmount::average(&[]), MixedAmount::null());
        assert_eq!(ms.iter().sum::<MixedAmount>(), total);
    }

    #[test]
    fn mixed_amount_precisions_and_styles() {
        let m = mixed([
            num(d("1.2345")),
            num(&d("1") / &d("3")).with_commodity(sym("X")),
        ]);
        let p = |m: MixedAmount| {
            m.amounts_raw()
                .iter()
                .map(|a| a.style.precision)
                .collect::<Vec<_>>()
        };
        assert_eq!(p(m.clone().with_precision(Fixed(1))), [Fixed(1), Fixed(1)]);
        assert_eq!(p(m.clone().with_full_precision()), [Fixed(4), Fixed(255)]);
        assert_eq!(
            p(m.clone().with_full_precision_capped()),
            [Fixed(4), Fixed(8)]
        );
        assert_eq!(
            p(m.clone().with_full_precision_up_to(Some(2))),
            [Fixed(2), Fixed(2)]
        );
        assert_eq!(p(m.clone().with_precision_max(0)), [Fixed(0), Fixed(0)]);
        let styles = CommodityStyles::from([(sym("X"), style(Fixed(2), Rounding::Hard))]);
        assert_eq!(p(m.clone().styled(&styles)), [Fixed(0), Fixed(2)]);
        assert_eq!(p(m.styled(&styles).unstyled()), [Fixed(0), Fixed(0)]);
    }
}
