//! Links from report cells to hledger-web's register, for html and fods
//! output with --base-url (Hledger.Cli.Anchor): a figure links to the
//! register that derives it, an account name to the account's register, a
//! period heading to the register for that period.

use crate::dates::{DateSpan, Day};
use crate::reports::opts::{BalanceAccumulation, PeriodTitles};
use crate::text::quote_if_spaced;
use crate::valuation::{ConversionOp, ValuationType};
use crate::write::spreadsheet::Cell;

/// What a report's register links need to know about the report, besides
/// its query and the base url (LinkOpts).
#[derive(Clone, Debug, PartialEq)]
pub struct LinkOpts {
    /// With historical balances, the register starts from the balance
    /// before the period, so that it ends on the figure in the cell.
    pub accum: BalanceAccumulation,
    /// The whole report span. A cumulative figure's register runs from its
    /// start.
    pub span: DateSpan,
    /// Does the report use secondary dates? Then so must the register.
    pub date2: bool,
    /// Does the row's figure include subaccounts, or not (subs=0)?
    pub includes_subs: bool,
    /// Are the figures shown with the opposite sign to the register's?
    pub negated: bool,
    /// The register's value parameter, for figures converted to cost or to
    /// market value (see [`value_param`]).
    pub value: Option<String>,
}

impl Default for LinkOpts {
    /// defaultLinkOpts.
    fn default() -> LinkOpts {
        LinkOpts {
            accum: BalanceAccumulation::PerPeriod,
            span: DateSpan::default(),
            date2: false,
            includes_subs: true,
            negated: false,
            value: None,
        }
    }
}

/// The query parameters a link carries besides q: the register's
/// accumulation mode when it is not the default, and how it converts
/// amounts when the report converts them (linkParams).
pub fn link_params(lo: &LinkOpts) -> Vec<(String, String)> {
    let mut params = Vec::new();
    if lo.accum == BalanceAccumulation::Historical {
        params.push(("accum".to_string(), "historical".to_string()));
    }
    if let Some(v) = &lo.value {
        params.push(("value".to_string(), v.clone()));
    }
    params
}

/// The value parameter asking hledger-web's register to convert amounts as
/// a report does, in --value's WHEN[,COMM] syntax: cost for -B, end for -V,
/// end,COMM for -X COMM, then, now, or a date; none for cost combined with
/// a valuation (valueParam).
pub fn value_param(conv: Option<ConversionOp>, val: Option<&ValuationType>) -> Option<String> {
    let comm =
        |c: &Option<crate::amount::Sym>| c.as_ref().map_or(String::new(), |c| format!(",{c}"));
    match (conv == Some(ConversionOp::ToCost), val) {
        (true, None) => Some("cost".to_string()),
        (true, Some(_)) | (false, None) => None,
        (false, Some(vt)) => Some(match vt {
            ValuationType::AtEnd(c) => format!("end{}", comm(c)),
            ValuationType::AtThen(c) => format!("then{}", comm(c)),
            ValuationType::AtNow(c) => format!("now{}", comm(c)),
            ValuationType::AtDate(d, c) => format!("{d}{}", comm(c)),
        }),
    }
}

/// The span a figure's register covers: the period, or for a cumulative
/// figure, from the report's start to the period's end (linkSpan).
fn link_span(lo: &LinkOpts, span: &DateSpan) -> DateSpan {
    match lo.accum {
        BalanceAccumulation::Cumulative => DateSpan(lo.span.0, span.1),
        _ => *span,
    }
}

/// The query parameters naming a link's account: with its subaccounts, or
/// with subs=0 without them; none for a depth 0 report's "..." row, whose
/// register is everything's (accountParams).
pub fn account_params(includes_subs: bool, account: &str) -> Vec<(String, String)> {
    if account == "..." {
        return Vec::new();
    }
    let mut params = vec![("account".to_string(), account.to_string())];
    if !includes_subs {
        params.push(("subs".to_string(), "0".to_string()));
    }
    params
}

/// A date: term for the span (date2: with secondary dates), or none for
/// all time (dateTerm).
pub fn date_term(date2: bool, span: &DateSpan) -> Vec<String> {
    let t = span.show_for_query();
    if t.is_empty() {
        Vec::new()
    } else {
        vec![format!("{}{t}", if date2 { "date2:" } else { "date:" })]
    }
}

/// A query term quoted so that the query parser reads it back: as it is
/// if already quoted, in single quotes (unescaped) if it has double quotes
/// and no single quotes, otherwise in double quotes if it has white space
/// (quoteForQuery).
fn quote_for_query(s: &str) -> String {
    let enclosed = |q: char| s.chars().count() >= 2 && s.starts_with(q) && s.ends_with(q);
    if enclosed('\'') || enclosed('"') {
        s.to_string()
    } else if s.contains('"') && !s.contains('\'') {
        format!("'{s}'")
    } else {
        quote_if_spaced(s)
    }
}

/// A query parameter's key or value as modern-uri writes it: a space as
/// `+`, other bytes but the unreserved characters and `!$'()*,;/?:@` as
/// `%xx` (lower-case hex) of their UTF-8 bytes.
fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b' ' => out.push('+'),
            b'a'..=b'z'
            | b'A'..=b'Z'
            | b'0'..=b'9'
            | b'-'
            | b'.'
            | b'_'
            | b'~'
            | b'!'
            | b'$'
            | b'\''
            | b'('
            | b')'
            | b'*'
            | b','
            | b';'
            | b'/'
            | b'?'
            | b':'
            | b'@' => out.push(b as char),
            b => out.push_str(&format!("%{b:02x}")),
        }
    }
    out
}

/// The register's url with this query, its terms quoted and joined by
/// spaces, as q, then these parameters (registerQueryUrl).
fn register_query_url(params: &[(String, String)], query: &[String]) -> String {
    let q = (query.iter())
        .filter(|t| !t.is_empty())
        .map(|t| quote_for_query(t))
        .collect::<Vec<_>>()
        .join(" ");
    let mut all = Vec::new();
    if !q.is_empty() {
        all.push(("q".to_string(), q));
    }
    all.extend(params.iter().cloned());
    let query = (all.iter())
        .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    if query.is_empty() {
        "register".to_string()
    } else {
        format!("register?{query}")
    }
}

/// A link to the register with this query and these parameters after q,
/// under the base url, or none without one (composeAnchorWith).
pub fn compose_anchor_with(
    params: &[(String, String)],
    base: Option<&str>,
    query: &[String],
) -> String {
    match base {
        None => String::new(),
        Some(b) => {
            let slash = if b.is_empty() || b.ends_with('/') {
                ""
            } else {
                "/"
            };
            format!("{b}{slash}{}", register_query_url(params, query))
        }
    }
}

/// The query without its date terms (removeDates).
pub fn remove_dates(query: &[String]) -> Vec<String> {
    (query.iter())
        .filter(|t| !(t.starts_with("date:") || t.starts_with("date2:")))
        .cloned()
        .collect()
}

/// The query without its terms naming an account for the register, which
/// a link names itself (removeInacct).
pub fn remove_inacct(query: &[String]) -> Vec<String> {
    (query.iter())
        .filter(|t| !(t.starts_with("inacct:") || t.starts_with("inacctonly:")))
        .cloned()
        .collect()
}

/// The cell with this link and its title, unless the link is empty
/// (withLink).
pub fn with_link<T>(anchor: String, title: &str, cell: Cell<T>) -> Cell<T> {
    if anchor.is_empty() {
        cell
    } else {
        Cell {
            anchor,
            title: title.to_string(),
            ..cell
        }
    }
}

/// The title of a link from a figure to the register that derives it,
/// noting when the report shows the opposite sign (amountPhrase).
pub fn amount_phrase(lo: &LinkOpts, is_total: bool) -> String {
    let what = match (lo.accum == BalanceAccumulation::Historical, is_total) {
        (true, false) => "Show the transactions behind this balance",
        (true, true) => "Show the transactions behind this total",
        (false, false) => "Show the transactions that make up this amount",
        (false, true) => "Show the transactions that make up this total",
    };
    if lo.negated {
        format!("{what}, which the register shows with the opposite sign")
    } else {
        what.to_string()
    }
}

/// A period's heading as the period titles say: its compact name or its
/// dates (renderPeriodHeading).
pub fn render_period_heading(titles: PeriodTitles, span: &DateSpan) -> String {
    match titles {
        PeriodTitles::Dates => span.show_full(),
        PeriodTitles::Compact => span.to_string(),
    }
}

/// A column heading for the period, linking to the register for it, by a
/// date term that reads back as the same span (headerDateSpanCell).
pub fn header_date_span_cell(
    date2: bool,
    titles: PeriodTitles,
    base: Option<&str>,
    query: &[String],
    span: &DateSpan,
) -> Cell<String> {
    let mut q = date_term(date2, span);
    q.extend(remove_dates(&remove_inacct(query)));
    with_link(
        compose_anchor_with(&[], base, &q),
        "Show the transactions in this period",
        Cell::header(&render_period_heading(titles, span)),
    )
}

/// A cell showing a period in an account's row, linking to the account's
/// register for that period (dateSpanCellWith).
pub fn date_span_cell_with(
    lo: &LinkOpts,
    titles: PeriodTitles,
    base: Option<&str>,
    query: &[String],
    account: &str,
    span: &DateSpan,
) -> Cell<String> {
    let mut params = account_params(lo.includes_subs, account);
    params.extend(link_params(lo));
    let mut q = date_term(lo.date2, &link_span(lo, span));
    q.extend(remove_dates(&remove_inacct(query)));
    with_link(
        compose_anchor_with(&params, base, &q),
        "Show transactions affecting this account in this period",
        Cell::new(render_period_heading(titles, span)),
    )
}

/// A cell showing a date in an account's row, linking to the account's
/// register for that day (dateCell).
pub fn date_cell(base: Option<&str>, query: &[String], account: &str, d: Day) -> Cell<String> {
    let mut q = vec![format!("date:{d}")];
    q.extend(remove_dates(&remove_inacct(query)));
    with_link(
        compose_anchor_with(&account_params(true, account), base, &q),
        "Show transactions affecting this account on this date",
        Cell::new(d.to_string()),
    )
}

/// A cell for a period in a totals row, linking to the register of
/// everything in the report's query, restricted by these terms (a
/// section's account types, say), for that period (totalDateSpanCell).
pub fn total_date_span_cell(
    lo: &LinkOpts,
    titles: PeriodTitles,
    base: Option<&str>,
    terms: &[String],
    query: &[String],
    span: &DateSpan,
) -> Cell<String> {
    let mut q = terms.to_vec();
    q.extend(date_term(lo.date2, &link_span(lo, span)));
    q.extend(remove_dates(&remove_inacct(query)));
    with_link(
        compose_anchor_with(&link_params(lo), base, &q),
        &amount_phrase(lo, true),
        Cell::new(render_period_heading(titles, span)),
    )
}

/// The account's cell linked to the account's register, restricted to the
/// query; the link options say whether subaccounts are included and how
/// the register accumulates (setAccountAnchorWith).
pub fn set_account_anchor_with<T>(
    lo: &LinkOpts,
    base: Option<&str>,
    query: &[String],
    account: &str,
    cell: Cell<T>,
) -> Cell<T> {
    let mut params = account_params(lo.includes_subs, account);
    params.extend(link_params(lo));
    let title = if lo.includes_subs {
        "Show transactions affecting this account and subaccounts"
    } else {
        "Show transactions affecting this account but not subaccounts"
    };
    with_link(
        compose_anchor_with(&params, base, &remove_inacct(query)),
        title,
        cell,
    )
}

/// [`set_account_anchor_with`] the default link options (setAccountAnchor).
pub fn set_account_anchor<T>(
    base: Option<&str>,
    query: &[String],
    account: &str,
    cell: Cell<T>,
) -> Cell<T> {
    set_account_anchor_with(&LinkOpts::default(), base, query, account, cell)
}

pub(crate) mod tests {
    use super::*;

    // The doctests of composeAnchor and composeAnchorWith.
    #[cfg_attr(test, test)]
    pub(crate) fn anchors() {
        let q = ["date:2024".to_string()];
        assert_eq!(compose_anchor_with(&[], None, &q), "");
        assert_eq!(
            compose_anchor_with(&[], Some(""), &q),
            "register?q=date:2024"
        );
        assert_eq!(
            compose_anchor_with(&[], Some("/"), &q),
            "/register?q=date:2024"
        );
        assert_eq!(
            compose_anchor_with(&[], Some("foo"), &q),
            "foo/register?q=date:2024"
        );
        assert_eq!(
            compose_anchor_with(&[], Some("foo/"), &q),
            "foo/register?q=date:2024"
        );
        let params = [("accum".to_string(), "historical".to_string())];
        assert_eq!(
            compose_anchor_with(&params, Some(""), &q),
            "register?q=date:2024&accum=historical"
        );
    }

    #[test]
    fn query_encoding() {
        let q = |ts: &[&str]| {
            let ts: Vec<String> = ts.iter().map(|t| t.to_string()).collect();
            compose_anchor_with(&account_params(true, "a+b é"), Some(""), &ts)
        };
        assert_eq!(
            q(&["a b", "acct:a'", "x\"y"]),
            "register?q=%22a+b%22+acct:a'+'x%22y'&account=a%2bb+%c3%a9"
        );
    }
}
