//! JSON output (Hledger.Data.Json): transactions, amounts and reports as
//! hledger's aeson instances encode them, printed as aeson-pretty prints
//! them with hledger's settings: two-space indents, object keys sorted,
//! numbers and strings in aeson's notation.

use dashu_int::IBig;
use dashu_int::ops::UnsignedAbs;
use serde_json::{Number, Value, json};

use crate::account::Realness;
use crate::amount::{
    Amount, AmountCost, AmountStyle, CostBasis, MixedAmount, Precision, Rounding, Side,
};
use crate::dates::{DateSpan, EfDay, Period};
use crate::decimal::Decimal;
use crate::model::{BalanceAssertion, Posting, SourcePos, Status, Transaction};
use crate::reports::account_txns::AccountTransactionsReportItem;
use crate::reports::periodic::{
    BalanceReport, CompoundPeriodicReport, DisplayName, PeriodicReport, PeriodicReportRow,
};
use crate::reports::postings::PostingsReportItem;

/// A value as hledger's JSON output shows it (ToJSON).
pub trait ToJson {
    fn to_json(&self) -> Value;
}

/// The value as pretty-printed JSON, with a final newline (toJsonText).
pub fn to_json_text(v: &impl ToJson) -> String {
    let mut out = String::new();
    write_value(&mut out, 0, &v.to_json());
    out.push('\n');
    out
}

/// aeson-pretty's layout: an array's or object's items one per line,
/// indented two spaces deeper than the brackets, which are empty with no
/// items; `": "` after a key.
fn write_value(out: &mut String, level: usize, v: &Value) {
    let item = |out: &mut String, i: usize| {
        out.push_str(if i == 0 { "\n" } else { ",\n" });
        out.push_str(&"  ".repeat(level + 1));
    };
    let close = |out: &mut String, empty: bool, bracket: char| {
        if !empty {
            out.push('\n');
            out.push_str(&"  ".repeat(level));
        }
        out.push(bracket);
    };
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        // serde_json writes a positive exponent as e+N, aeson as eN
        Value::Number(n) => out.push_str(&n.to_string().replacen("e+", "e", 1)),
        Value::String(s) => write_string(out, s),
        Value::Array(vs) => {
            out.push('[');
            for (i, v) in vs.iter().enumerate() {
                item(out, i);
                write_value(out, level + 1, v);
            }
            close(out, vs.is_empty(), ']');
        }
        Value::Object(m) => {
            out.push('{');
            for (i, (k, v)) in m.iter().enumerate() {
                item(out, i);
                write_string(out, k);
                out.push_str(": ");
                write_value(out, level + 1, v);
            }
            close(out, m.is_empty(), '}');
        }
    }
}

/// A JSON string as aeson writes it: `"` and `\` escaped, newline,
/// carriage return and tab as `\n`, `\r`, `\t`, the other control
/// characters as `\u00xx`, everything else as it is.
fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < '\u{20}' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A number written as this text, which must be valid JSON.
fn number(text: &str) -> Value {
    Value::Number(text.parse::<Number>().expect("a valid JSON number"))
}

/// A number in scientific notation, `D.DDDeN` (scientific's formatScientific
/// Exponent): its significant digits, without trailing zeros, the decimal
/// point after the first (followed by 0 if there is only one), and the
/// power of ten of the first.
fn exponent_notation(negative: bool, digits: &str, exponent: i64) -> String {
    let (first, rest) = digits.split_at(1);
    let rest = if rest.is_empty() { "0" } else { rest };
    let sign = if negative { "-" } else { "" };
    format!("{sign}{first}.{rest}e{exponent}")
}

/// An integer as aeson-pretty writes it: in exponent notation if its
/// magnitude exceeds 10^19, otherwise in full (fromNumber's Generic).
fn integer(n: &IBig) -> Value {
    let negative = *n < IBig::ZERO;
    let digits = n.unsigned_abs().to_string();
    if digits.len() > 20 || (digits.len() == 20 && digits != "10000000000000000000") {
        let significant = digits.trim_end_matches('0');
        number(&exponent_notation(
            negative,
            significant,
            digits.len() as i64 - 1,
        ))
    } else {
        number(&n.to_string())
    }
}

/// A double as aeson-pretty writes it (aeson's realFloatToJSON, then
/// fromNumber's Generic): its shortest digits that read back the same; in
/// exponent notation if its magnitude exceeds 10^19; in full if it is a
/// whole number; otherwise in decimal notation from 0.1 to 10^7 and
/// exponent notation outside that range. An infinite one (a decimal too
/// large for a double) is the string `+inf` or `-inf`.
pub fn double(x: f64) -> Value {
    if x == 0.0 {
        return number("0");
    }
    if x.is_infinite() {
        return Value::String(if x > 0.0 { "+inf" } else { "-inf" }.to_string());
    }
    // Rust shows a float's shortest round-tripping digits: d.ddde-N or dde N
    let shown = format!("{:e}", x.abs());
    let (mantissa, exp) = shown.split_once('e').expect("exponent notation");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let first_exp: i64 = exp.parse().expect("an exponent");
    let last_exp = first_exp - (digits.len() as i64 - 1);
    let negative = x < 0.0;
    let sign = if negative { "-" } else { "" };
    if x.abs() > 1e19 {
        number(&exponent_notation(negative, &digits, first_exp))
    } else if last_exp >= 0 {
        number(&format!("{sign}{digits}{}", "0".repeat(last_exp as usize)))
    } else if (0.1..1e7).contains(&x.abs()) {
        // the digits with the decimal point after the first first_exp+1
        let point = first_exp + 1;
        let text = if point <= 0 {
            format!("0.{}{digits}", "0".repeat((-point) as usize))
        } else {
            let (int, frac) = digits.split_at(point as usize);
            format!("{int}.{frac}")
        };
        number(&format!("{sign}{text}"))
    } else {
        number(&exponent_notation(negative, &digits, first_exp))
    }
}

/// The encoding of a constructor with one field, under aeson's default
/// options: its name as the tag, the field as the contents.
pub(crate) fn tagged(tag: &str, contents: Value) -> Value {
    json!({"tag": tag, "contents": contents})
}

impl<T: ToJson> ToJson for Option<T> {
    fn to_json(&self) -> Value {
        self.as_ref().map_or(Value::Null, T::to_json)
    }
}

impl<T: ToJson> ToJson for [T] {
    fn to_json(&self) -> Value {
        Value::Array(self.iter().map(T::to_json).collect())
    }
}

impl<T: ToJson> ToJson for Vec<T> {
    fn to_json(&self) -> Value {
        self.as_slice().to_json()
    }
}

impl<A: ToJson, B: ToJson> ToJson for (A, B) {
    fn to_json(&self) -> Value {
        json!([self.0.to_json(), self.1.to_json()])
    }
}

/// The three-key object (decimalKV): the decimal places and mantissa,
/// after rounding to 10 places if there are more, and the nearest double.
impl ToJson for Decimal {
    fn to_json(&self) -> Value {
        let d = if self.places() <= 10 {
            self.clone()
        } else {
            self.round_to(10)
        };
        json!({
            "decimalPlaces": d.places(),
            "decimalMantissa": integer(d.mantissa()),
            "floatingPoint": double(d.to_f64()),
        })
    }
}

impl ToJson for AmountStyle {
    fn to_json(&self) -> Value {
        json!({
            "ascommodityside": match self.side {
                Side::L => "L",
                Side::R => "R",
            },
            "ascommodityspaced": self.spaced,
            "asdigitgroups": self.digit_groups.as_ref().map(|g| json!([g.sep.to_string(), g.sizes])),
            "asdecimalmark": self.decimal_mark.map(|c| c.to_string()),
            "asprecision": match self.precision {
                Precision::Fixed(n) => Some(n),
                Precision::Natural => None,
            },
            "asrounding": match self.rounding {
                Rounding::No => "NoRounding",
                Rounding::Soft => "SoftRounding",
                Rounding::Hard => "HardRounding",
                Rounding::All => "AllRounding",
            },
        })
    }
}

impl ToJson for AmountCost {
    fn to_json(&self) -> Value {
        match self {
            AmountCost::Unit(a) => tagged("UnitCost", a.to_json()),
            AmountCost::Total(a) => tagged("TotalCost", a.to_json()),
        }
    }
}

impl ToJson for CostBasis {
    fn to_json(&self) -> Value {
        json!({
            "cbDate": self.date.map(|d| d.to_string()),
            "cbLabel": self.label.as_deref(),
            "cbCost": self.cost.as_deref().map(Amount::to_json),
        })
    }
}

impl ToJson for Amount {
    fn to_json(&self) -> Value {
        json!({
            "acommodity": &*self.commodity,
            "aquantity": self.quantity.to_json(),
            "astyle": self.style.to_json(),
            "acost": self.cost.to_json(),
            "acostbasis": self.cost_basis.as_deref().map(CostBasis::to_json),
        })
    }
}

/// The amounts as they are stored, zeros included (amountsRaw).
impl ToJson for MixedAmount {
    fn to_json(&self) -> Value {
        self.amounts_raw().to_json()
    }
}

impl ToJson for SourcePos {
    fn to_json(&self) -> Value {
        json!({
            "sourceName": &*self.file,
            "sourceLine": self.line,
            "sourceColumn": self.col,
        })
    }
}

impl ToJson for Status {
    fn to_json(&self) -> Value {
        json!(match self {
            Status::Unmarked => "Unmarked",
            Status::Pending => "Pending",
            Status::Cleared => "Cleared",
        })
    }
}

impl ToJson for BalanceAssertion {
    fn to_json(&self) -> Value {
        json!({
            "baamount": self.amount.to_json(),
            "batotal": self.total,
            "bainclusive": self.inclusive,
            "baposition": self.pos.to_json(),
        })
    }
}

/// A posting in this transaction, if it has one (postingKV): the
/// transaction as its index in a dummy field, to avoid a cycle, and no
/// original posting.
fn posting_json(p: &Posting, t: Option<&Transaction>) -> Value {
    json!({
        "pdate": p.date.map(|d| d.to_string()),
        "pdate2": p.date2.map(|d| d.to_string()),
        "pstatus": p.status.to_json(),
        "paccount": &*p.account,
        "pamount": p.amount.to_json(),
        "pcomment": p.comment,
        "preal": match p.realness {
            Realness::Real => "RealPosting",
            Realness::Virtual => "VirtualPosting",
            Realness::BalancedVirtual => "BalancedVirtualPosting",
        },
        "ptags": p.tags,
        "pbalanceassertion": p.assertion.as_deref().map(BalanceAssertion::to_json),
        "ptransaction_": t.map_or(String::new(), |t| t.index.to_string()),
        "poriginal": null,
    })
}

impl ToJson for Transaction {
    fn to_json(&self) -> Value {
        json!({
            "tindex": self.index,
            "tprecedingcomment": self.preceding_comment,
            "tsourcepos": [self.pos.0.to_json(), self.pos.1.to_json()],
            "tdate": self.date.to_string(),
            "tdate2": self.date2.map(|d| d.to_string()),
            "tstatus": self.status.to_json(),
            "tcode": self.code,
            "tdescription": self.description,
            "tcomment": self.comment,
            "ttags": self.tags,
            "tpostings": self.postings.iter().map(|p| posting_json(p, Some(self))).collect::<Vec<_>>(),
        })
    }
}

/// Generic encoding: the constructor as the tag, its fields as the
/// contents.
impl ToJson for Period {
    fn to_json(&self) -> Value {
        use Period::*;
        match *self {
            DayPeriod(d) => tagged("DayPeriod", json!(d.to_string())),
            WeekPeriod(d) => tagged("WeekPeriod", json!(d.to_string())),
            MonthPeriod(y, m) => tagged("MonthPeriod", json!([y, m])),
            QuarterPeriod(y, q) => tagged("QuarterPeriod", json!([y, q])),
            YearPeriod(y) => tagged("YearPeriod", json!(y)),
            PeriodBetween(b, e) => tagged("PeriodBetween", json!([b.to_string(), e.to_string()])),
            PeriodFrom(b) => tagged("PeriodFrom", json!(b.to_string())),
            PeriodTo(e) => tagged("PeriodTo", json!(e.to_string())),
            PeriodAll => json!({"tag": "PeriodAll"}),
        }
    }
}

/// A postings report line as a tuple: the date, period and description
/// if shown, the posting, the running total.
impl ToJson for PostingsReportItem<'_> {
    fn to_json(&self) -> Value {
        json!([
            self.date.map(|d| d.to_string()),
            self.period.to_json(),
            self.description,
            posting_json(&self.posting, self.txn),
            self.total.to_json(),
        ])
    }
}

/// An account transactions report line as a tuple: the transaction, the
/// transaction as seen from the account, whether it is a split, the other
/// accounts, the change, the balance.
impl ToJson for AccountTransactionsReportItem {
    fn to_json(&self) -> Value {
        json!([
            self.txn.to_json(),
            self.acct_txn.to_json(),
            self.split,
            self.other_accounts
                .iter()
                .map(|a| &**a)
                .collect::<Vec<&str>>(),
            self.amount.to_json(),
            self.balance.to_json(),
        ])
    }
}

impl ToJson for EfDay {
    fn to_json(&self) -> Value {
        tagged(
            if self.exact { "Exact" } else { "Flex" },
            json!(self.day.to_string()),
        )
    }
}

impl ToJson for DateSpan {
    fn to_json(&self) -> Value {
        json!([self.0.to_json(), self.1.to_json()])
    }
}

/// A report row's account: its full name.
impl ToJson for DisplayName {
    fn to_json(&self) -> Value {
        json!(&*self.full)
    }
}

/// The totals row's name, none (an empty tuple).
impl ToJson for () {
    fn to_json(&self) -> Value {
        json!([])
    }
}

impl<N: ToJson, A: ToJson> ToJson for PeriodicReportRow<N, A> {
    fn to_json(&self) -> Value {
        json!({
            "prrName": self.name.to_json(),
            "prrAmounts": self.amounts.to_json(),
            "prrTotal": self.total.to_json(),
            "prrAverage": self.average.to_json(),
        })
    }
}

impl<N: ToJson, A: ToJson> ToJson for PeriodicReport<N, A> {
    fn to_json(&self) -> Value {
        json!({
            "prDates": self.dates.to_json(),
            "prRows": self.rows.to_json(),
            "prTotals": self.totals.to_json(),
        })
    }
}

impl<N: ToJson, A: ToJson> ToJson for CompoundPeriodicReport<N, A> {
    fn to_json(&self) -> Value {
        let subreports: Vec<Value> = (self.subreports.iter())
            .map(|(title, r, increases)| json!([title, r.to_json(), increases]))
            .collect();
        json!({
            "cbrTitle": self.title,
            "cbrDates": self.dates.to_json(),
            "cbrSubreports": subreports,
            "cbrTotals": self.totals.to_json(),
        })
    }
}

/// The single-column report as a pair: its items, each the account's full
/// name, shown name, indent and amount, and the total.
impl ToJson for BalanceReport {
    fn to_json(&self) -> Value {
        let items: Vec<Value> = (self.items.iter())
            .map(|(n, m)| json!([&*n.full, &*n.name, n.indent, m.to_json()]))
            .collect();
        json!([items, self.total.to_json()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(v: Value) -> String {
        let mut out = String::new();
        write_value(&mut out, 0, &v);
        out
    }

    #[test]
    fn numbers() {
        let d = |x: f64| shown(double(x));
        assert_eq!(d(1.0), "1");
        assert_eq!(d(-1000.0), "-1000");
        assert_eq!(d(0.05), "5.0e-2");
        assert_eq!(d(1234.56), "1234.56");
        assert_eq!(d(10000000.5), "1.00000005e7");
        assert_eq!(d(0.1), "0.1");
        assert_eq!(d(0.0001), "1.0e-4");
        assert_eq!(d(9999999.9), "9999999.9");
        assert_eq!(d(0.3333333333), "0.3333333333");
        assert_eq!(d(12345678901234567890.5), "1.2345678901234567e19");
        assert_eq!(d(1.2345678901234568e29), "1.2345678901234568e29");
        assert_eq!(d(12345678.0), "12345678");
        assert_eq!(d(f64::INFINITY), "\"+inf\"");
        assert_eq!(d(f64::NEG_INFINITY), "\"-inf\"");
        let i = |s: &str| shown(integer(&s.parse().unwrap()));
        assert_eq!(i("10"), "10");
        assert_eq!(i("10000000000000000000"), "10000000000000000000");
        assert_eq!(i("123456789012345678905"), "1.23456789012345678905e20");
        assert_eq!(
            i("-123456789012345678901234567890"),
            "-1.2345678901234567890123456789e29"
        );
    }

    #[test]
    fn decimals() {
        let d = |m: i64, p: u8| shown(Decimal::new(IBig::from(m), p).to_json());
        assert_eq!(
            d(-1, 12),
            "{\n  \"decimalMantissa\": 0,\n  \"decimalPlaces\": 10,\n  \"floatingPoint\": 0\n}"
        );
        assert_eq!(
            d(30000000000000004, 17),
            "{\n  \"decimalMantissa\": 3000000000,\n  \"decimalPlaces\": 10,\n  \"floatingPoint\": 0.3\n}"
        );
    }

    #[test]
    fn layout_and_strings() {
        assert_eq!(shown(json!([])), "[]");
        assert_eq!(shown(json!({})), "{}");
        assert_eq!(
            shown(json!({"b": [1, {"c": null}], "a": "x\"\\\n\u{8}\u{1b}é"})),
            "{\n  \"a\": \"x\\\"\\\\\\n\\u0008\\u001bé\",\n  \"b\": [\n    1,\n    {\n      \"c\": null\n    }\n  ]\n}"
        );
    }
}
