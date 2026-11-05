//! A renderer-independent description of a chart, made from a report
//! (Hledger.Write.Chart), with its colours (Hledger.Write.Chart.Palette)
//! and its data as CSV records. Each chart-capable report has an adapter
//! (cli::chart) making its default chart, and the text and svg renderers
//! (write::chart_text, write::chart_svg) draw one.
//!
//! The model carries the report's display information (period headings,
//! account display names, commodity display styles, the title), so that a
//! chart is labelled as the report's table is. Values are kept exact
//! (decimals); renderers convert them to doubles only for drawing.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::account::AccountType;
use crate::amount::Amount;
use crate::amount::render::{AmountFormat, show_amount_b};
use crate::dates::{DateSpan, Day};
use crate::decimal::Decimal;
use crate::write::json::{ToJson, tagged};

/// A chart: a title, an x axis, and the panels drawn against it,
/// typically one per commodity (Chart). Without panels, or series, it has
/// no data, which renderers say.
#[derive(Clone, Debug, PartialEq)]
pub struct Chart {
    pub title: String,
    pub x_axis: XAxis,
    pub panels: Vec<Panel>,
}

/// What a chart's x positions are (XAxis): categories, such as a single
/// period report's accounts; consecutive report periods with their
/// headings; or dates, eg a register's.
#[derive(Clone, Debug, PartialEq)]
pub enum XAxis {
    Categories(Vec<ChartLabel>),
    Periods(Vec<(DateSpan, String)>),
    Dates(Vec<Day>),
}

/// A name for a category or a series (ChartLabel): the full name (used in
/// csv, json and tooltips), the name to show and its indent, its account
/// type if known (charts colour accounts by their main type), and whether
/// it names an account (rather than eg a total).
#[derive(Clone, Debug, PartialEq)]
pub struct ChartLabel {
    pub full: String,
    pub short: String,
    pub indent: usize,
    pub typ: Option<AccountType>,
    pub account: bool,
}

impl ChartLabel {
    /// A label for something other than an account, eg a total
    /// (plainLabel).
    pub fn plain(t: &str) -> ChartLabel {
        ChartLabel {
            full: t.to_string(),
            short: t.to_string(),
            indent: 0,
            typ: None,
            account: false,
        }
    }
}

/// A chart area with its own y axis, holding series in one unit (Panel):
/// the unit, an amount whose commodity and style show the values; the
/// series in display order; the groups' names (eg a compound report's
/// sections); a fixed scale, the amount the chart spans, if one was asked
/// for; the bar layout; whether the values are counts of whole things
/// (text charts then draw at most a cell per unit); summary values shown
/// below (eg -T's and -A's); and text to show at each x position instead
/// of its value, if any.
#[derive(Clone, Debug, PartialEq)]
pub struct Panel {
    pub unit: Amount,
    pub series: Vec<Series>,
    pub group_names: Vec<String>,
    pub scale: Option<Decimal>,
    pub layout: BarLayout,
    pub counts: bool,
    pub summary: Vec<Summary>,
    pub value_labels: Vec<Option<String>>,
}

/// A value summarising a panel's values over the x axis, with its name as
/// the report's table shows it and its own display style (Summary).
#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub kind: SummaryKind,
    pub label: String,
    pub value: Amount,
}

/// SummaryKind: the total of all x positions' values, or their average
/// (which svg charts also draw as a line).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SummaryKind {
    Total,
    Average,
}

/// How a panel's groups of bars are drawn (BarLayout): all stacked in one
/// diverging bar at each x position, positive values above zero and
/// negative ones below; or each group stacked in its own bar, side by side,
/// its values shown with their usual sign as positive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarLayout {
    Diverging,
    Grouped,
}

/// A named sequence of values, one per x position, none being a gap
/// (Series); with its role, mark, group (eg its section), and usual sign
/// (1, -1, or 0 for none), the other sign's values being contra amounts.
#[derive(Clone, Debug, PartialEq)]
pub struct Series {
    pub label: ChartLabel,
    pub role: SeriesRole,
    pub mark: Mark,
    pub values: Vec<Option<Decimal>>,
    pub group: usize,
    pub sign: i32,
}

impl Series {
    /// The value at this x position, if there is one (valueAt).
    pub fn value_at(&self, j: usize) -> Option<&Decimal> {
        self.values.get(j).and_then(Option::as_ref)
    }
}

/// What a series represents (SeriesRole): an item, eg an account; several
/// combined ("other"); a total of other series; or an amount missing from
/// what a bar stands for, drawn hatched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeriesRole {
    Item,
    Other,
    Total,
    Shortfall,
}

/// How a series is drawn (Mark): bars from zero, a line joining the values
/// (drawn as bars in text), or a marker at each value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Bar,
    Line,
    Marker,
}

/// Whether this value of the series is a contra amount, of the opposite
/// sign to the series' usual sign (isContra).
pub fn is_contra(s: &Series, q: &Decimal) -> bool {
    s.sign != 0 && !q.is_zero() && q.is_negative() != (s.sign < 0)
}

/// Whether this value of the series is drawn hatched: contra amounts and
/// shortfalls are (isHatched).
pub fn is_hatched(s: &Series, q: &Decimal) -> bool {
    s.role == SeriesRole::Shortfall || is_contra(s, q)
}

/// Whether this total is, at every x position, just the end of these
/// series' stacked bars: the sum of their positive values or of their
/// negative ones, so that marking it adds nothing (totalIsBarEnd).
pub fn total_is_bar_end(bars: &[&Series], total: &Series) -> bool {
    (total.values.iter().enumerate()).all(|(j, t)| {
        let Some(t) = t else { return true };
        let qs: Vec<&Decimal> = bars.iter().filter_map(|s| s.value_at(j)).collect();
        let sum = |pos: bool| {
            (qs.iter())
                .filter(|q| {
                    if pos {
                        **q > &Decimal::ZERO
                    } else {
                        q.is_negative()
                    }
                })
                .fold(Decimal::ZERO, |a, q| &a + *q)
        };
        *t == sum(true) || *t == sum(false)
    })
}

/// The main account type (asset, liability, equity, revenue or expense)
/// of an account type, which is one of those or a subtype of one
/// (mainAccountType).
pub fn main_account_type(t: AccountType) -> AccountType {
    use AccountType::*;
    [Asset, Liability, Equity, Revenue, Expense]
        .into_iter()
        .find(|m| t.is_subtype_of(*m))
        .unwrap_or(t)
}

/// How a series or bar is coloured, independently of the renderer
/// (Colour): its main account type's hue in its Nth tint; the Nth
/// categorical colour; a hue no account type uses, for a lone series that
/// is not an account (or of unknown type); a neutral colour; a muted one
/// for "other", tinged with its type's hue; or the ink colour of totals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Colour {
    Type(AccountType, usize),
    Index(usize),
    Lone,
    Neutral,
    Other(Option<AccountType>),
    Total,
}

/// How many tints of a type's colour renderers provide (maxTints).
pub const MAX_TINTS: usize = 4;

/// How many categorical colours renderers provide (categoricalColours).
const CATEGORICAL_COLOURS: usize = 8;

/// How a chart's accounts are coloured, the same in all its panels
/// (Colouring): by name, in distinct colours, when they are all of one
/// type or of unknown type; by main account type when they are of several;
/// or singly, when each panel shows one account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Colouring {
    ByName,
    ByType,
    Single,
}

/// How this chart's accounts are coloured, considering all its panels
/// (chartColouring).
pub fn chart_colouring(chart: &Chart) -> Colouring {
    let singles = (chart.panels.iter()).all(|p| {
        (p.series.iter())
            .filter(|s| s.role != SeriesRole::Total)
            .count()
            <= 1
    });
    if singles {
        return Colouring::Single;
    }
    let types: BTreeSet<AccountType> = (chart.panels.iter())
        .flat_map(|p| &p.series)
        .filter(|s| matches!(s.role, SeriesRole::Item | SeriesRole::Shortfall))
        .filter_map(|s| s.label.typ.map(main_account_type))
        .collect();
    if types.len() > 1 {
        Colouring::ByType
    } else {
        Colouring::ByName
    }
}

/// The colours of a panel's series, in order, under the chart's colouring
/// (seriesColours): by type, each account of known type gets its main
/// type's hue in one of several tints; otherwise, and for accounts of
/// unknown type, categorical colours; a lone series that is not an account,
/// or a lone account of unknown type in a chart of single accounts, gets
/// the lone colour. Tints and categorical colours are chosen by name (see
/// [`stable_slots`]), so that an account keeps its colour in other charts;
/// a typed series that is not an account gets its type's first tint.
pub fn series_colours(colouring: Colouring, series: &[&Series]) -> Vec<Colour> {
    let type_of = |s: &Series| -> Option<AccountType> {
        if colouring == Colouring::ByName {
            None
        } else {
            s.label.typ.map(main_account_type)
        }
    };
    let only_one = series
        .iter()
        .filter(|s| s.role != SeriesRole::Total)
        .count()
        == 1;
    // the ordinary series choosing tints or categorical colours by name:
    // accounts, and series of unknown type
    let named: Vec<(usize, &Series)> = (series.iter().enumerate())
        .filter(|(_, s)| s.role == SeriesRole::Item && (s.label.account || type_of(s).is_none()))
        .map(|(i, s)| (i, *s))
        .collect();
    let mut types: Vec<Option<AccountType>> = Vec::new();
    for (_, s) in &named {
        if !types.contains(&type_of(s)) {
            types.push(type_of(s));
        }
    }
    let mut slots: BTreeMap<usize, usize> = BTreeMap::new();
    for t in types {
        let (is, items): (Vec<usize>, Vec<(String, Decimal)>) = (named.iter())
            .filter(|(_, s)| type_of(s) == t)
            .map(|(i, s)| (*i, (s.label.full.clone(), series_size(s))))
            .unzip();
        // a type's tints favour the first, most saturated ones
        let ks = if t.is_some() {
            stable_slots_favouring_first(0.5, MAX_TINTS, &items)
        } else {
            stable_slots(CATEGORICAL_COLOURS, &items)
        };
        slots.extend(is.into_iter().zip(ks));
    }
    (series.iter().enumerate())
        .map(|(i, s)| {
            let k = slots.get(&i).copied().unwrap_or(0);
            match (s.role, type_of(s)) {
                (SeriesRole::Total, _) => Colour::Total,
                (SeriesRole::Other, t) => Colour::Other(t),
                (_, Some(t)) if s.label.account => Colour::Type(t, k),
                (_, Some(t)) => Colour::Type(t, 0),
                (_, None) if only_one && (colouring == Colouring::Single || !s.label.account) => {
                    Colour::Lone
                }
                (_, None) => Colour::Index(k),
            }
        })
        .collect()
}

/// A series' size, for ranking: the sum of its absolute values
/// (seriesSize).
pub fn series_size(s: &Series) -> Decimal {
    (s.values.iter().flatten()).fold(Decimal::ZERO, |a, q| &a + &q.abs())
}

/// Slots 0 to n-1 for these names, chosen by name rather than position,
/// so that a name tends to get the same slot in other lists (stableSlots):
/// each name has its own order of preference (from a hash of it) and gets
/// the first slot still free, the names choosing largest first (then by
/// name). With more names than slots, slots are shared.
pub fn stable_slots(n: usize, items: &[(String, Decimal)]) -> Vec<usize> {
    stable_slots_favouring_first(0.0, n, items)
}

/// Like [`stable_slots`], each name's preferences biased towards the
/// first slots by this much, from 0 (not at all) to 1 (fully)
/// (stableSlotsFavouringFirst).
pub fn stable_slots_favouring_first(
    bias: f64,
    n: usize,
    items: &[(String, Decimal)],
) -> Vec<usize> {
    if n == 0 {
        return vec![0; items.len()];
    }
    // FNV-1a over the characters' code points, with murmur3's finaliser
    let hash = |s: &str| -> u32 {
        let h = (s.chars()).fold(2166136261u32, |h, c| (h ^ c as u32).wrapping_mul(16777619));
        let h = (h ^ (h >> 16)).wrapping_mul(0x85ebca6b);
        let h = (h ^ (h >> 13)).wrapping_mul(0xc2b2ae35);
        h ^ (h >> 16)
    };
    let preferences = |name: &str| -> Vec<usize> {
        let key =
            |k: usize| bias * k as f64 + f64::from(hash(&format!("{name}/{k}"))) / 4294967296.0;
        let mut ks: Vec<usize> = (0..n).collect();
        ks.sort_by(|a, b| key(*a).total_cmp(&key(*b)));
        ks
    };
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by(|a, b| {
        let (na, sa) = &items[*a];
        let (nb, sb) = &items[*b];
        sb.cmp(sa).then_with(|| na.cmp(nb)).then_with(|| a.cmp(b))
    });
    let mut used = BTreeSet::new();
    let mut assigned = vec![0; items.len()];
    for i in order {
        let prefs = preferences(&items[i].0);
        let slot = (prefs.iter())
            .find(|k| !used.contains(*k))
            .copied()
            .unwrap_or(prefs[0]);
        used.insert(slot);
        assigned[i] = slot;
    }
    assigned
}

/// Each panel with a fixed scale: the absolute quantity of the scale
/// amount in its commodity, or else of the one without a commodity
/// symbol, the last of several in a commodity (setChartScales).
pub fn set_chart_scales(scales: &[Amount], chart: &mut Chart) {
    for p in &mut chart.panels {
        let find = |c: &str| (scales.iter().rev()).find(|a| &*a.commodity == c);
        if let Some(a) = find(&p.unit.commodity).or_else(|| find("")) {
            p.scale = Some(a.quantity.abs());
        }
    }
}

/// Every panel with this bar layout, if given, prepared for it: the
/// grouped layout shows series with their usual sign as positive, their
/// contra amounts as negative (setChartLayout).
pub fn set_chart_layout(layout: Option<BarLayout>, chart: &mut Chart) {
    for p in &mut chart.panels {
        p.layout = layout.unwrap_or(p.layout);
        if p.layout == BarLayout::Grouped {
            for s in &mut p.series {
                if s.sign != 0 {
                    let sign = Decimal::from_i64(i64::from(s.sign));
                    for v in s.values.iter_mut().flatten() {
                        *v = &*v * &sign;
                    }
                    s.sign = 1;
                }
            }
        }
    }
}

/// With a fixed scale and the largest positive and (absolute) negative
/// extents of the values, how much of the scale each direction gets
/// (scaleSpans): negative values the room they need and positive ones the
/// rest; both shrunk in proportion if they don't fit.
pub fn scale_spans(scale: &Decimal, maxpos: &Decimal, maxneg: &Decimal) -> (Decimal, Decimal) {
    let zero = Decimal::ZERO;
    if *maxpos <= zero && *maxneg > zero {
        (zero, scale.clone())
    } else if &(maxpos + maxneg) <= scale {
        (scale - maxneg, maxneg.clone())
    } else {
        let neg = &(maxneg * scale) / &(maxpos + maxneg);
        (scale - &neg, neg)
    }
}

/// [`scale_spans`] for doubles.
pub fn scale_spans_f64(scale: f64, maxpos: f64, maxneg: f64) -> (f64, f64) {
    if maxpos <= 0.0 && maxneg > 0.0 {
        (0.0, scale)
    } else if maxpos + maxneg <= scale {
        (scale - maxneg, maxneg)
    } else {
        let neg = maxneg * scale / (maxpos + maxneg);
        (scale - neg, neg)
    }
}

/// A chart's data as CSV records in long form, one per value, after a
/// heading record (chartAsCsv): the commodity, series, role, mark, x and
/// value, in the unit's precision without symbol or digit groups, a gap
/// as an empty field; each panel's summary values after its series.
pub fn chart_as_csv(chart: &Chart) -> Vec<Vec<String>> {
    let xlabels: Vec<String> = match &chart.x_axis {
        XAxis::Categories(ls) => ls.iter().map(|l| l.full.clone()).collect(),
        XAxis::Periods(ps) => ps.iter().map(|(_, l)| l.clone()).collect(),
        XAxis::Dates(ds) => ds.iter().map(Day::to_string).collect(),
    };
    let show = |unit: &Amount, q: &Decimal| {
        let fmt = AmountFormat {
            commodity: false,
            ..AmountFormat::MACHINE
        };
        let a = Amount {
            quantity: q.clone(),
            ..unit.clone()
        };
        show_amount_b(&fmt, &a).s
    };
    let mut records = vec![
        ["commodity", "series", "role", "mark", "x", "value"]
            .map(String::from)
            .to_vec(),
    ];
    for p in &chart.panels {
        let commodity = p.unit.commodity.to_string();
        for s in &p.series {
            for (x, q) in xlabels.iter().zip(&s.values) {
                records.push(vec![
                    commodity.clone(),
                    s.label.full.clone(),
                    role_name(s.role).to_string(),
                    mark_name(s.mark).to_string(),
                    x.clone(),
                    q.as_ref().map_or(String::new(), |q| show(&p.unit, q)),
                ]);
            }
        }
        for su in &p.summary {
            let kind = match su.kind {
                SummaryKind::Total => "rowtotal",
                SummaryKind::Average => "average",
            };
            records.push(vec![
                commodity.clone(),
                su.label.clone(),
                kind.to_string(),
                String::new(),
                String::new(),
                show(&su.value, &su.value.quantity),
            ]);
        }
    }
    records
}

fn role_name(r: SeriesRole) -> &'static str {
    match r {
        SeriesRole::Item => "item",
        SeriesRole::Other => "other",
        SeriesRole::Total => "total",
        SeriesRole::Shortfall => "shortfall",
    }
}

fn mark_name(m: Mark) -> &'static str {
    match m {
        Mark::Bar => "bar",
        Mark::Line => "line",
        Mark::Marker => "marker",
    }
}

// Colours (Hledger.Write.Chart.Palette)

/// A chart colour as #rrggbb, in its variant for a light or a dark
/// background (colourHex).
pub fn colour_hex(dark: bool, c: Colour) -> String {
    let pick = |(light, darkc): (&str, &str)| if dark { darkc } else { light }.to_string();
    const GREY: &str = "#898781";
    match c {
        Colour::Type(t, k) => tint(
            tint_fraction(dark, k),
            &pick(type_colour(main_account_type(t))),
        ),
        Colour::Index(k) => pick(CATEGORICAL[k % CATEGORICAL.len()]),
        Colour::Other(None) => GREY.to_string(),
        Colour::Other(Some(t)) => mix_with(0.6, GREY, &pick(type_colour(main_account_type(t)))),
        Colour::Lone => pick(("#1597b0", "#22a6be")),
        Colour::Neutral => pick(("#52514e", "#c3c2b7")),
        Colour::Total => pick(("#0b0b0b", "#ffffff")),
    }
}

/// The conventional colours of the main account types, light and dark
/// (typeColour).
fn type_colour(t: AccountType) -> (&'static str, &'static str) {
    match t {
        AccountType::Asset => ("#2a78d6", "#3987e5"),
        AccountType::Liability => ("#eda100", "#c98500"),
        AccountType::Equity => ("#4a3aa7", "#9085e9"),
        AccountType::Revenue => ("#008300", "#008300"),
        AccountType::Expense => ("#e34948", "#e66767"),
        _ => ("#4a3aa7", "#9085e9"),
    }
}

/// The categorical palette, light and dark (categorical).
const CATEGORICAL: [(&str, &str); 8] = [
    ("#2a78d6", "#3987e5"),
    ("#eb6834", "#d95926"),
    ("#1baf7a", "#199e70"),
    ("#eda100", "#c98500"),
    ("#e87ba4", "#d55181"),
    ("#008300", "#008300"),
    ("#4a3aa7", "#9085e9"),
    ("#e34948", "#e66767"),
];

/// How the Nth tint of a type's colour is mixed with white (positive) or
/// black (negative), the most visible first (tintFraction).
fn tint_fraction(dark: bool, k: usize) -> f64 {
    let fractions = if dark {
        [0.0, 0.4, -0.3, 0.65]
    } else {
        [0.0, -0.3, 0.4, 0.65]
    };
    fractions[k % MAX_TINTS]
}

/// A #rrggbb colour mixed with white (by a positive fraction) or black (by
/// a negative one) (tint).
pub fn tint(f: f64, hex: &str) -> String {
    if f >= 0.0 {
        mix_with(f, "#ffffff", hex)
    } else {
        mix_with(-f, "#000000", hex)
    }
}

/// A #rrggbb colour mixed with another, by this fraction of the other
/// (mixWith); rounding half to even, as Haskell's round does.
fn mix_with(f: f64, other: &str, hex: &str) -> String {
    let rgb = |h: &str| -> Option<Vec<i64>> {
        let digits: String = h.chars().skip(1).collect();
        if digits.chars().count() != 6 || !digits.is_ascii() {
            return None;
        }
        (0..3)
            .map(|i| i64::from_str_radix(&digits[2 * i..2 * i + 2], 16).ok())
            .collect()
    };
    match (rgb(hex), rgb(other)) {
        (Some(cs), Some(os)) => {
            let mut out = String::from("#");
            for (c, o) in cs.iter().zip(os) {
                let mixed = (*c as f64 + (o as f64 - *c as f64) * f).round_ties_even() as i64;
                out.push_str(&format!("{mixed:02x}"));
            }
            out
        }
        _ => hex.to_string(),
    }
}

// JSON (the charts' derived ToJSON instances)

impl ToJson for AccountType {
    fn to_json(&self) -> Value {
        json!(self.name())
    }
}

impl ToJson for ChartLabel {
    fn to_json(&self) -> Value {
        json!({
            "lbFull": self.full,
            "lbShort": self.short,
            "lbIndent": self.indent,
            "lbType": self.typ.to_json(),
            "lbAccount": self.account,
        })
    }
}

impl ToJson for Series {
    fn to_json(&self) -> Value {
        json!({
            "seLabel": self.label.to_json(),
            "seRole": match self.role {
                SeriesRole::Item => "SeriesItem",
                SeriesRole::Other => "SeriesOther",
                SeriesRole::Total => "SeriesTotal",
                SeriesRole::Shortfall => "SeriesShortfall",
            },
            "seMark": match self.mark {
                Mark::Bar => "MarkBar",
                Mark::Line => "MarkLine",
                Mark::Marker => "MarkMarker",
            },
            "seValues": self.values.to_json(),
            "seGroup": self.group,
            "seSign": self.sign,
        })
    }
}

impl ToJson for Summary {
    fn to_json(&self) -> Value {
        json!({
            "suKind": match self.kind {
                SummaryKind::Total => "SummaryTotal",
                SummaryKind::Average => "SummaryAverage",
            },
            "suLabel": self.label,
            "suValue": self.value.to_json(),
        })
    }
}

impl ToJson for Panel {
    fn to_json(&self) -> Value {
        json!({
            "pnUnit": self.unit.to_json(),
            "pnSeries": self.series.to_json(),
            "pnGroupNames": self.group_names,
            "pnScale": self.scale.to_json(),
            "pnLayout": match self.layout {
                BarLayout::Diverging => "Diverging",
                BarLayout::Grouped => "Grouped",
            },
            "pnCounts": self.counts,
            "pnSummary": self.summary.to_json(),
            "pnValueLabels": self.value_labels,
        })
    }
}

impl ToJson for Chart {
    fn to_json(&self) -> Value {
        let x_axis = match &self.x_axis {
            XAxis::Categories(ls) => tagged("XCategories", ls.to_json()),
            XAxis::Periods(ps) => tagged(
                "XPeriods",
                Value::Array((ps.iter()).map(|(s, l)| json!([s.to_json(), l])).collect()),
            ),
            XAxis::Dates(ds) => tagged(
                "XDates",
                Value::Array(ds.iter().map(|d| json!(d.to_string())).collect()),
            ),
        };
        json!({
            "chTitle": self.title,
            "chXAxis": x_axis,
            "chPanels": self.panels.to_json(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(n: i64) -> Decimal {
        Decimal::from_i64(n)
    }

    fn ser(name: &str, typ: Option<AccountType>, values: Vec<Option<Decimal>>) -> Series {
        Series {
            label: ChartLabel {
                full: name.to_string(),
                short: name.to_string(),
                indent: 0,
                typ,
                account: true,
            },
            role: SeriesRole::Item,
            mark: Mark::Bar,
            values,
            group: 0,
            sign: 0,
        }
    }

    // tests_Hledger_Write_Chart.
    #[test]
    fn scale_spans_cases() {
        let spans = |s, p, n| scale_spans(&d(s), &d(p), &d(n));
        assert_eq!(spans(100, 80, 0), (d(100), d(0)));
        assert_eq!(spans(100, 0, 30), (d(0), d(100)));
        assert_eq!(spans(100, 50, 30), (d(70), d(30)));
        assert_eq!(spans(100, 150, 50), (d(75), d(25)));
        assert_eq!(spans(100, 0, 0), (d(100), d(0)));
    }

    #[test]
    fn total_is_bar_end_cases() {
        let s = |vs: Vec<Option<i64>>| ser("s", None, vs.into_iter().map(|v| v.map(d)).collect());
        let ends = |bars: Vec<Series>, total: Series| {
            total_is_bar_end(&bars.iter().collect::<Vec<_>>(), &total)
        };
        assert!(ends(
            vec![s(vec![Some(1), Some(2)]), s(vec![Some(3), None])],
            s(vec![Some(4), Some(2)])
        ));
        assert!(ends(
            vec![s(vec![Some(-1)]), s(vec![Some(-3)])],
            s(vec![Some(-4)])
        ));
        assert!(!ends(
            vec![s(vec![Some(5)]), s(vec![Some(-3)])],
            s(vec![Some(2)])
        ));
        assert!(!ends(
            vec![s(vec![Some(450)]), s(vec![Some(300)])],
            s(vec![Some(450)])
        ));
    }

    #[test]
    fn series_colour_choices() {
        use AccountType::*;
        let one = |n: &str, t| ser(n, t, vec![Some(d(1))]);
        let colours = |c, ss: &[Series]| series_colours(c, &ss.iter().collect::<Vec<_>>());
        let tints = |cs: &[Colour], ty| {
            let mut ks: Vec<usize> = (cs.iter())
                .filter_map(|c| match c {
                    Colour::Type(t, k) if *t == ty => Some(*k),
                    _ => None,
                })
                .collect();
            ks.dedup();
            ks
        };
        let cs = colours(
            Colouring::ByType,
            &[
                one("e1", Some(Expense)),
                one("a", Some(Cash)),
                one("u1", None),
                one("e2", Some(Expense)),
                one("u2", None),
            ],
        );
        assert_eq!(
            cs.iter()
                .filter(|c| matches!(c, Colour::Type(Asset, _)))
                .count(),
            1
        );
        let mut e = tints(&cs, Expense);
        e.sort_unstable();
        e.dedup();
        assert_eq!(e.len(), 2);
        let idx: BTreeSet<usize> = (cs.iter())
            .filter_map(|c| match c {
                Colour::Index(k) => Some(*k),
                _ => None,
            })
            .collect();
        assert_eq!(idx.len(), 2);
        // colours follow the accounts, not their positions
        let fwd = colours(
            Colouring::ByType,
            &[one("e1", Some(Expense)), one("e2", Some(Expense))],
        );
        let mut rev = colours(
            Colouring::ByType,
            &[one("e2", Some(Expense)), one("e1", Some(Expense))],
        );
        rev.reverse();
        assert_eq!(fwd, rev);
        // all of one type: categorical colours, all different, kept when fewer are shown
        let rs: Vec<Series> = (0..=MAX_TINTS)
            .map(|i| one(&i.to_string(), Some(Revenue)))
            .collect();
        let rcs = colours(Colouring::ByName, &rs);
        let distinct: BTreeSet<String> = rcs.iter().map(|c| format!("{c:?}")).collect();
        assert_eq!(distinct.len(), MAX_TINTS + 1);
        assert_eq!(colours(Colouring::ByName, &rs[..1]), rcs[..1]);
        // typed series which are not accounts get their types' first tints
        let nonacct = |n: &str, t| ChartLabel {
            account: false,
            ..one(n, Some(t)).label
        };
        let na: Vec<Series> = [("Kosten", Asset), ("Gewinn", Revenue), ("Verlust", Expense)]
            .into_iter()
            .map(|(n, t)| Series {
                label: nonacct(n, t),
                ..one(n, Some(t))
            })
            .collect();
        assert_eq!(
            colours(Colouring::ByType, &na),
            [
                Colour::Type(Asset, 0),
                Colour::Type(Revenue, 0),
                Colour::Type(Expense, 0)
            ]
        );
        // a lone series which is not an account gets the lone colour
        let lone = Series {
            label: ChartLabel::plain("u"),
            ..one("u", None)
        };
        assert_eq!(colours(Colouring::ByName, &[lone]), [Colour::Lone]);
        let other = Series {
            role: SeriesRole::Other,
            ..one("o", None)
        };
        let total = Series {
            role: SeriesRole::Total,
            ..one("t", None)
        };
        assert_eq!(
            colours(Colouring::ByName, &[other, total]),
            [Colour::Other(None), Colour::Total]
        );
        // in a chart of single accounts, a lone account has its type's hue,
        // or if unknown the lone colour
        let k = stable_slots_favouring_first(0.5, MAX_TINTS, &[("e".to_string(), d(1))])[0];
        assert_eq!(
            colours(Colouring::Single, &[one("e", Some(Expense))]),
            [Colour::Type(Expense, k)]
        );
        assert_eq!(
            colours(Colouring::Single, &[one("u", None)]),
            [Colour::Lone]
        );
    }

    #[test]
    fn stable_slot_choices() {
        let items: Vec<(String, Decimal)> = [("a", 3), ("b", 2), ("c", 1)]
            .map(|(n, s)| (n.to_string(), d(s)))
            .to_vec();
        let slots = stable_slots(8, &items);
        assert_eq!(slots.iter().collect::<BTreeSet<_>>().len(), 3);
        let mut reversed = items.clone();
        reversed.reverse();
        let mut rslots = stable_slots(8, &reversed);
        rslots.reverse();
        assert_eq!(rslots, slots);
        assert!(slots.iter().all(|k| *k < 8));
        assert_eq!(
            stable_slots(2, &items)
                .iter()
                .collect::<BTreeSet<_>>()
                .len(),
            2
        );
        assert_eq!(stable_slots(0, &items), [0, 0, 0]);
    }

    // tests_Hledger_Write_Chart_Palette.
    #[test]
    fn palette() {
        assert_eq!(
            colour_hex(false, Colour::Type(AccountType::Revenue, 0)),
            "#008300"
        );
        assert_eq!(
            colour_hex(true, Colour::Type(AccountType::Expense, 0)),
            "#e66767"
        );
        assert_eq!(
            colour_hex(false, Colour::Type(AccountType::Cash, 0)),
            "#2a78d6"
        );
        assert_eq!(
            colour_hex(false, Colour::Type(AccountType::Expense, MAX_TINTS)),
            colour_hex(false, Colour::Type(AccountType::Expense, 0))
        );
        assert_eq!(tint(0.5, "#000000"), "#808080");
        assert_eq!(tint(-0.5, "#ffffff"), "#808080");
        assert_eq!(tint(0.5, "not a colour"), "not a colour");
    }
}
