//! Queries (Hledger.Query): search criteria for postings, transactions,
//! accounts and more, parsed from query arguments such as `acct:food`,
//! `date:2024` or `expr:'cash and amt:>0'`, and the functions matching
//! things against them.

use std::cell::RefCell;

use crate::account::{AccountName, AccountType, DepthSpec, account_name_level, escape_name};
use crate::amount::render::show_mixed_amount_one_line;
use crate::amount::{Amount, MixedAmount};
use crate::dates::{DateSpan, Day, Interval, parse_period_expr, spans_intersect, spans_union};
use crate::decimal::Decimal;
use crate::model::{PostingRef, PriceDirective, Status, Tag, Transaction};
use crate::parse::{Alts, PResult, Parser, choice_try, is_non_newline_space, or};
use crate::regex::{Regexp, to_regex, to_regex_ci};
use crate::text::{show_string, strip, strip_quotes};

/// Criteria to match postings, transactions, accounts and more against
/// (Query).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Query {
    /// Match anything.
    #[default]
    Any,
    /// Match nothing.
    None,
    /// Primary dates in this span.
    Date(DateSpan),
    /// Secondary dates in this span.
    Date2(DateSpan),
    StatusQ(Status),
    /// Transaction codes this regex matches part of.
    Code(Regexp),
    /// Transaction descriptions this regex matches part of.
    Desc(Regexp),
    /// A tag whose name, and value if given, these regexes match part of.
    Tag(Regexp, Option<Regexp>),
    /// Account names this regex matches part of.
    Acct(Regexp),
    /// Accounts of one of these types, or their subtypes.
    Type(Vec<AccountType>),
    /// Accounts at most this deep.
    Depth(usize),
    /// Accounts at most this deep, among those the regex matches.
    DepthAcct(Regexp, usize),
    /// Postings that are real, or not.
    Real(bool),
    /// Amounts whose quantity compares so with this number.
    Amt(OrdPlus, Decimal),
    /// Commodity symbols this regex matches all of.
    Cur(Regexp),
    /// Any visible text this regex matches part of, or a date in the span,
    /// given when the pattern is also a period expression.
    Find(Regexp, Option<DateSpan>),
    Not(Box<Query>),
    And(Vec<Query>),
    Or(Vec<Query>),
    /// For a posting, any posting of its transaction matched by all of
    /// these; elsewhere, like And.
    AnyPosting(Vec<Query>),
    /// For a posting, all postings of its transaction (one at least)
    /// matched by all of these; elsewhere, like And.
    AllPostings(Vec<Query>),
}

/// A comparison for `amt:` queries: less than, at most, more than, at
/// least, or equal to a number, with the Abs forms comparing absolute
/// values (OrdPlus).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrdPlus {
    Lt,
    LtEq,
    Gt,
    GtEq,
    Eq,
    AbsLt,
    AbsLtEq,
    AbsGt,
    AbsGtEq,
    AbsEq,
}

/// Something a query term asks of the report rather than of the data
/// (QueryOpt): an account register of one account (`inacctonly:`), or of
/// it and its subaccounts (`inacct:`), or the report interval of a
/// `date:` term.
#[derive(Clone, Debug, PartialEq)]
pub enum QueryOpt {
    InAcctOnly(AccountName),
    InAcct(AccountName),
    Interval(Interval),
}

/// A query for the `payee:` tag (payeeTag).
pub fn payee_tag(value: Option<&str>) -> Result<Query, String> {
    special_tag("payee", value)
}

/// A query for the `note:` tag (noteTag).
pub fn note_tag(value: Option<&str>) -> Result<Query, String> {
    special_tag("note", value)
}

fn special_tag(name: &str, value: Option<&str>) -> Result<Query, String> {
    let value = value.map(to_query_regex).transpose()?;
    Ok(Query::Tag(to_regex_ci(name)?, value))
}

/// A query for the hidden tag of generated transactions
/// (generatedTransactionTag).
pub fn generated_transaction_tag() -> Query {
    Query::Tag(
        to_regex_ci("generated-transaction").expect("a valid regex"),
        None,
    )
}

/// The query term prefixes, apart from `not:` (queryprefixes).
pub const QUERY_PREFIXES: [&str; 21] = [
    "inacctonly:",
    "inacct:",
    "amt:",
    "code:",
    "desc:",
    "payee:",
    "note:",
    "acct:",
    "date:",
    "date2:",
    "status:",
    "cur:",
    "real:",
    "depth:",
    "tag:",
    "type:",
    "expr:",
    "any:",
    "all:",
    "find:",
    "::",
];

/// The query and query options of a text of space-separated terms, which
/// may be quoted as a shell would (parseQuery). Relative dates are resolved
/// against `today`.
pub fn parse_query(today: Day, s: &str) -> Result<(Query, Vec<QueryOpt>), String> {
    parse_query_list(today, &query_words(&QUERY_PREFIXES, s))
}

/// The query and query options of these terms (parseQueryList): account
/// terms are ORed, as are description terms and status terms, then all are
/// ANDed.
pub fn parse_query_list(
    today: Day,
    terms: &[impl AsRef<str>],
) -> Result<(Query, Vec<QueryOpt>), String> {
    let mut queries = Vec::with_capacity(terms.len());
    let mut opts = Vec::new();
    for t in terms {
        let (q, o) = parse_query_term(today, t.as_ref())?;
        queries.push(q);
        opts.extend(o);
    }
    Ok((combine_queries_by_type(queries), opts))
}

/// Queries combined as separate terms are (combineQueriesByType): the
/// account ones ORed, the description ones ORed, the status ones ORed, then
/// those and the rest ANDed.
pub fn combine_queries_by_type(queries: Vec<Query>) -> Query {
    let (descs, rest): (Vec<Query>, Vec<Query>) = queries.into_iter().partition(Query::is_desc);
    let (accts, rest): (Vec<Query>, Vec<Query>) = rest.into_iter().partition(Query::is_acct);
    let (statuses, others): (Vec<Query>, Vec<Query>) = rest.into_iter().partition(Query::is_status);
    let mut all = vec![Query::Or(accts), Query::Or(descs), Query::Or(statuses)];
    all.extend(others);
    Query::And(all).simplify()
}

/// A text split into query terms at spaces, except within quotes, which
/// are removed (words''). A quoted term may have one of these prefixes, and
/// `not:`, before its opening quote. Splitting stops at a newline, or at
/// spaces before a closing parenthesis.
pub fn query_words(prefixes: &[&str], s: &str) -> Vec<String> {
    let mut p = Parser::new(s);
    p.sep_by(
        |p| {
            choice_try(
                p,
                &mut [
                    &mut |p| prefixed_quoted(p, prefixes),
                    &mut |p| quoted(p, '\''),
                    &mut |p| quoted(p, '"'),
                    &mut |p| {
                        Ok(p.take_while(None, |c| !matches!(c, ' ' | '\n' | '\r'))
                            .to_string())
                    },
                ],
            )
        },
        |p| {
            p.not_followed_by(|p| {
                p.skip_spaces();
                p.char(')')
            })?;
            p.take_while1(None, is_non_newline_space)
        },
    )
    .expect("query words always parse")
}

/// A quoted pattern after a prefix (and `not:`), as prefix and pattern
/// without the quotes (prefixedQuotedPattern).
fn prefixed_quoted(p: &mut Parser, prefixes: &[&str]) -> PResult<String> {
    let not = p.optional(|p| p.string("not:"))?.unwrap_or("");
    let mut allowed = prefixes.to_vec();
    if !not.is_empty() {
        allowed.push("");
    }
    let next = one_of_strings(p, &allowed)?;
    let pattern = or(p, |p| quoted(p, '\''), |p| quoted(p, '"'))?;
    Ok(format!("{not}{next}{}", strip_quotes(&pattern)))
}

/// Text between two of these quote characters, without them, and without
/// a further pair of quotes around all of it.
fn quoted(p: &mut Parser, q: char) -> PResult<String> {
    p.char(q)?;
    let inner = p.take_while(None, |c| c != q);
    p.char(q)?;
    Ok(strip_quotes(inner).to_string())
}

/// One of these texts (a `choice'` of `string`s).
fn one_of_strings<'a>(p: &mut Parser<'a>, words: &[&str]) -> PResult<&'a str> {
    let mut tried = Alts::new(p);
    for w in words {
        if let Some(r) = tried.run(p, |p| p.string(w)) {
            return r;
        }
    }
    Err(tried.choice_error(p))
}

/// One query term, as a query or query options (parseQueryTerm); a term
/// with no prefix matches account names. Relative dates are resolved
/// against `today`.
pub fn parse_query_term(today: Day, s: &str) -> Result<(Query, Vec<QueryOpt>), String> {
    let query = |q: Query| Ok((q, vec![]));
    let prefixed = |prefix: &str| s.strip_prefix(prefix);
    if let Some(a) = prefixed("inacctonly:") {
        return Ok((Query::Any, vec![QueryOpt::InAcctOnly(a.into())]));
    }
    if let Some(a) = prefixed("inacct:") {
        return Ok((Query::Any, vec![QueryOpt::InAcct(a.into())]));
    }
    if let Some(rest) = prefixed("not:") {
        let (q, opts) = parse_query_term(today, rest)?;
        return Ok((Query::Not(Box::new(q)), opts));
    }
    if let Some(r) = prefixed("code:") {
        return query(Query::Code(to_query_regex(r)?));
    }
    if let Some(r) = prefixed("desc:") {
        return query(Query::Desc(to_query_regex(r)?));
    }
    if let Some(r) = prefixed("payee:") {
        return query(payee_tag(Some(r))?);
    }
    if let Some(r) = prefixed("note:") {
        return query(note_tag(Some(r))?);
    }
    if let Some(r) = prefixed("acct:") {
        return query(Query::Acct(to_query_regex(r)?));
    }
    if let Some(r) = prefixed("find:").or_else(|| prefixed("::")) {
        return query(parse_find_query(today, r)?);
    }
    if let Some(e) = prefixed("date2:") {
        let (_, span) = parse_period_expr(today, e)
            .map_err(|err| format!("could not parse \"date2:{e}\": {err}"))?;
        return query(Query::Date2(span));
    }
    if let Some(e) = prefixed("date:") {
        let (interval, span) = parse_period_expr(today, e)
            .map_err(|err| format!("could not parse \"date:{e}\": {err}"))?;
        let opts = match interval {
            Interval::NoInterval => vec![],
            i => vec![QueryOpt::Interval(i)],
        };
        return Ok((Query::Date(span), opts));
    }
    if let Some(st) = prefixed("status:") {
        let status =
            parse_status(st).map_err(|e| format!("\"status:{st}\" gave a parse error: {e}"))?;
        return query(Query::StatusQ(status));
    }
    if let Some(b) = prefixed("real:") {
        return query(Query::Real(b == "1" || b.is_empty()));
    }
    if let Some(a) = prefixed("amt:") {
        let (ord, q) = parse_amount_query_term(a)?;
        return query(Query::Amt(ord, q));
    }
    if let Some(d) = prefixed("depth:") {
        let DepthSpec { flat, regexps } = parse_depth_spec(d)?;
        let mut qs: Vec<Query> = flat.map(Query::Depth).into_iter().collect();
        qs.extend(regexps.into_iter().map(|(r, d)| Query::DepthAcct(r, d)));
        return query(Query::And(qs));
    }
    if let Some(c) = prefixed("cur:") {
        return query(Query::Cur(to_regex_ci(&format!("^{c}$"))?));
    }
    if let Some(t) = prefixed("tag:") {
        return query(parse_tag(t)?);
    }
    if let Some(t) = prefixed("type:") {
        return query(parse_type_codes(t)?);
    }
    if let Some(e) = prefixed("expr:") {
        return parse_boolean_query(today, e);
    }
    if let Some(e) = prefixed("any:") {
        let (q, opts) = parse_boolean_query(today, e)?;
        return Ok((Query::AnyPosting(vec![q]), opts));
    }
    if let Some(e) = prefixed("all:") {
        let (q, opts) = parse_boolean_query(today, e)?;
        return Ok((Query::AllPostings(vec![q]), opts));
    }
    if s.is_empty() {
        return query(Query::Any);
    }
    parse_query_term(today, &format!("acct:{s}"))
}

/// The state of a boolean query parse: the reference date, and the error
/// of a term that failed to parse, which ends the whole parse.
struct BooleanQuery {
    today: Day,
    error: RefCell<Option<String>>,
}

/// A boolean query expression (parseBooleanQuery): terms combined by
/// `not`, `and` and `or` (binding in that order), parentheses, and spaces,
/// which combine terms as separate query arguments do. Dates may not be
/// ORed. Text after the expression is ignored, as upstream ignores it.
pub fn parse_boolean_query(today: Day, s: &str) -> Result<(Query, Vec<QueryOpt>), String> {
    let ctx = BooleanQuery {
        today,
        error: RefCell::new(None),
    };
    let mut p = Parser::new(s);
    let r = spaced_exprs(&mut p, &ctx);
    if let Some(e) = ctx.error.into_inner() {
        return Err(e);
    }
    r.map_err(|e| format!("failed to parse query:{}", e.pretty("", s)))
}

type QueryAndOpts = (Query, Vec<QueryOpt>);

/// Expressions separated by spaces, combined by type (spacedExprsP).
fn spaced_exprs(p: &mut Parser, ctx: &BooleanQuery) -> PResult<QueryAndOpts> {
    let exprs = p.sep_by(|p| or_exprs(p, ctx), spaces1)?;
    Ok(combine_with(combine_queries_by_type, exprs))
}

fn spaces1(p: &mut Parser) -> PResult<()> {
    p.take_while1(None, is_non_newline_space).map(drop)
}

/// The queries combined by `combine`, simplified, and the options
/// (combineWith).
fn combine_with(
    combine: impl FnOnce(Vec<Query>) -> Query,
    exprs: Vec<QueryAndOpts>,
) -> QueryAndOpts {
    let (mut queries, opts): (Vec<Query>, Vec<Vec<QueryOpt>>) = exprs.into_iter().unzip();
    let q = match queries.len() {
        0 => Query::Any,
        1 => queries.pop().expect("one query").simplify(),
        _ => combine(queries).simplify(),
    };
    (q, opts.concat())
}

/// Expressions separated by `or`, none of them about dates if there are
/// several (orExprsP).
fn or_exprs(p: &mut Parser, ctx: &BooleanQuery) -> PResult<QueryAndOpts> {
    let exprs = p.sep_by(|p| and_exprs(p, ctx), |p| keyword_between_spaces(p, "or"))?;
    if exprs.len() > 1
        && exprs
            .iter()
            .any(|(q, _)| q.clone().filter(Query::is_date_or_date2) != Query::Any)
    {
        return p.fail("sorry, using date: in OR expressions is not supported.");
    }
    Ok(combine_with(Query::Or, exprs))
}

/// Expressions separated by `and` (andExprsP).
fn and_exprs(p: &mut Parser, ctx: &BooleanQuery) -> PResult<QueryAndOpts> {
    let exprs = p.sep_by(
        |p| maybe_not_expr(p, ctx),
        |p| keyword_between_spaces(p, "and"),
    )?;
    Ok(combine_with(Query::And, exprs))
}

/// `or` or `and`, in any case, with spaces after it and maybe before it.
fn keyword_between_spaces(p: &mut Parser, word: &str) -> PResult<()> {
    p.attempt(|p| {
        p.skip_spaces();
        p.string_ci(word)?;
        spaces1(p)
    })
}

/// An expression, perhaps after `not` (maybeNotExprP).
fn maybe_not_expr(p: &mut Parser, ctx: &BooleanQuery) -> PResult<QueryAndOpts> {
    let not = p.optional(|p| {
        p.attempt(|p| {
            p.string_ci("not")?;
            p.not_followed_by(|p| p.char(':'))?;
            spaces1(p)
        })
    })?;
    let (q, opts) = term_or_parenthesised(p, ctx)?;
    Ok(match not {
        Some(()) => (Query::Not(Box::new(q)), opts),
        None => (q, opts),
    })
}

/// An expression in parentheses, or a term (termOrParenthesisedExprP).
fn term_or_parenthesised(p: &mut Parser, ctx: &BooleanQuery) -> PResult<QueryAndOpts> {
    or(
        p,
        |p| {
            p.char('(')?;
            p.skip_spaces();
            let r = spaced_exprs(p, ctx)?;
            p.attempt(|p| {
                p.skip_spaces();
                p.char(')')
            })?;
            Ok(r)
        },
        |p| query_term(p, ctx),
    )
}

/// A query term: an optional prefix and an argument (queryTermP). A term
/// that does not parse as a query ends the whole parse with its error.
fn query_term(p: &mut Parser, ctx: &BooleanQuery) -> PResult<QueryAndOpts> {
    let prefix = p.optional(query_prefix)?.unwrap_or_default();
    let arg = query_arg(p)?;
    match parse_query_term(ctx.today, &format!("{prefix}{arg}")) {
        Ok(r) => Ok(r),
        Err(e) => {
            ctx.error.borrow_mut().get_or_insert(e);
            Err(p.empty())
        }
    }
}

/// A query prefix, perhaps after one or more `not:` (queryPrefixP).
fn query_prefix(p: &mut Parser) -> PResult<String> {
    p.label("query prefix", |p| {
        or(
            p,
            |p| {
                p.string("not:")?;
                let rest = p.optional(query_prefix)?.unwrap_or_default();
                Ok(format!("not:{rest}"))
            },
            |p| one_of_strings(p, &QUERY_PREFIXES).map(str::to_string),
        )
    })
}

/// A term's argument: quoted text, or text up to a space, closing
/// parenthesis or newline that is not `not`, `and` or `or` and a space
/// (queryArgP).
fn query_arg(p: &mut Parser) -> PResult<String> {
    choice_try(
        p,
        &mut [
            &mut |p| quoted(p, '\''),
            &mut |p| quoted(p, '"'),
            &mut |p| {
                p.not_followed_by(|p| {
                    let mut tried = Alts::new(p);
                    for w in ["not ", "and ", "or "] {
                        if let Some(r) = tried.run(p, |p| p.attempt(|p| p.string_ci(w))) {
                            return r;
                        }
                    }
                    Err(tried.choice_error(p))
                })?;
                Ok(
                    p.take_while(None, |c| !matches!(c, ')' | ' ' | '\n' | '\r'))
                        .to_string(),
                )
            },
        ],
    )
}

/// A `find:` query (parseFindQuery): the pattern as a regex, and if it is
/// also a period expression giving a start or end, its span.
fn parse_find_query(today: Day, s: &str) -> Result<Query, String> {
    let span = match parse_period_expr(today, s) {
        Ok((_, span)) if span != DateSpan::default() => Some(span),
        _ => None,
    };
    Ok(Query::Find(to_query_regex(s)?, span))
}

/// The argument of an `amt:` term, `[OP][SIGN]NUM`, as a comparison and a
/// number (parseAmountQueryTerm). With a sign, the comparison is signed;
/// without one it compares absolute values, except with zero.
pub fn parse_amount_query_term(arg: &str) -> Result<(OrdPlus, Decimal), String> {
    use OrdPlus::*;
    // Strip outer white space, require and remove the prefix, remove all
    // spaces, and read a number.
    let parse = |prefix: &str| -> Option<Decimal> {
        let rest: String = strip(arg)
            .strip_prefix(prefix)?
            .chars()
            .filter(|c| *c != ' ')
            .collect();
        read_decimal(&rest)
    };
    let signed = [
        ("<=+", LtEq, false),
        ("<+", Lt, false),
        (">=+", GtEq, false),
        (">+", Gt, false),
        ("=+", Eq, false),
        ("+", Eq, false),
        ("<-", Lt, true),
        ("<=-", LtEq, true),
        (">-", Gt, true),
        (">=-", GtEq, true),
        ("=-", Eq, true),
        ("-", Eq, true),
    ];
    for (prefix, ord, negate) in signed {
        if let Some(q) = parse(prefix) {
            return Ok((ord, if negate { -q } else { q }));
        }
    }
    for (prefix, ord) in [("<=", LtEq), ("<", Lt), (">=", GtEq), (">", Gt)] {
        if parse(prefix).is_some_and(|q| q.is_zero()) {
            return Ok((ord, Decimal::ZERO));
        }
    }
    let unsigned = [
        ("<=", AbsLtEq),
        ("<", AbsLt),
        (">=", AbsGtEq),
        (">", AbsGt),
        ("=", AbsEq),
        ("", AbsEq),
    ];
    for (prefix, ord) in unsigned {
        if let Some(q) = parse(prefix) {
            return Ok((ord, q));
        }
    }
    Err(format!(
        "could not parse as a comparison operator followed by an optionally-signed number: {arg}"
    ))
}

/// A number as Haskell's `readMay` reads a Decimal, white space around it
/// allowed.
fn read_decimal(s: &str) -> Option<Decimal> {
    strip(s).parse().ok()
}

/// A query term's pattern as a case-insensitive regex; an empty one
/// matches anything (toQueryRegex).
fn to_query_regex(s: &str) -> Result<Regexp, String> {
    to_regex_ci(if s.is_empty() { "()" } else { s })
}

/// The argument of a `tag:` term, `NAME[=VALUE]`, as regexes (parseTag).
fn parse_tag(s: &str) -> Result<Query, String> {
    match s.split_once('=') {
        None => Ok(Query::Tag(to_query_regex(s)?, None)),
        Some((name, value)) => Ok(Query::Tag(
            to_query_regex(name)?,
            Some(to_query_regex(value)?),
        )),
    }
}

/// The argument of a `depth:` term, `[ACCTREGEX=]N` (parseDepthSpec).
pub fn parse_depth_spec(s: &str) -> Result<DepthSpec, String> {
    let (regex, depth) = match s.split_once('=') {
        None => (None, s),
        Some((r, d)) => (Some(r), d),
    };
    let depth = read_int(depth)
        .and_then(|d| usize::try_from(d).ok())
        .ok_or_else(|| {
            format!(
                "could not parse depth \"{s}\": it should be a whole number, 0 or more, \
                 optionally preceded by ACCTREGEX="
            )
        })?;
    Ok(match regex {
        None => DepthSpec {
            flat: Some(depth),
            regexps: vec![],
        },
        Some(r) => DepthSpec {
            flat: None,
            regexps: vec![(to_query_regex(r)?, depth)],
        },
    })
}

/// Text read as Haskell's `readMay` reads an Int: decimal, `0x` or `0o`
/// digits, perhaps after a minus sign, perhaps in parentheses, with white
/// space around allowed; wrapping at 64 bits.
fn read_int(s: &str) -> Option<i64> {
    let s = strip(s);
    if let Some(inner) = s.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
        return read_int(inner);
    }
    if let Some(rest) = s.strip_prefix('-') {
        return read_natural(strip(rest)).map(i64::wrapping_neg);
    }
    read_natural(s)
}

fn read_natural(s: &str) -> Option<i64> {
    let (digits, radix) = match s.get(..2) {
        Some("0x" | "0X") => (&s[2..], 16),
        Some("0o" | "0O") => (&s[2..], 8),
        _ => (s, 10),
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    Some(digits.chars().fold(0i64, |n, c| {
        n.wrapping_mul(i64::from(radix))
            .wrapping_add(i64::from(c.to_digit(radix).expect("a digit")))
    }))
}

/// The argument of a `type:` term: account type code letters
/// (parseTypeCodes).
fn parse_type_codes(s: &str) -> Result<Query, String> {
    let help = format!(
        "type:'s argument should be one or more of {}",
        AccountType::choices(false)
    );
    let mut types = Vec::new();
    for c in s.chars() {
        match AccountType::parse(false, &c.to_string()) {
            Ok(t) => types.push(t),
            Err(e) => {
                return Err(format!(
                    "could not parse {} as an account type code.\n{help}",
                    show_string(&e)
                ));
            }
        }
    }
    if types.is_empty() {
        return Err(help);
    }
    Ok(Query::Type(types))
}

/// The argument of a `status:` term (parseStatus).
fn parse_status(s: &str) -> Result<Status, String> {
    match s {
        "*" | "1" => Ok(Status::Cleared),
        "" | "0" => Ok(Status::Unmarked),
        "!" => Ok(Status::Pending),
        _ => Err(format!(
            "could not parse {} as a status (should be *, ! or empty)",
            show_string(s)
        )),
    }
}

impl Query {
    /// The query rewritten bottom-up by `f`: each compound query's parts
    /// first, then the query itself (transformQuery).
    pub fn transform(self, f: &impl Fn(Query) -> Query) -> Query {
        let all = |qs: Vec<Query>| qs.into_iter().map(|q| q.transform(f)).collect();
        f(match self {
            Query::Not(q) => Query::Not(Box::new(q.transform(f))),
            Query::And(qs) => Query::And(all(qs)),
            Query::Or(qs) => Query::Or(all(qs)),
            Query::AnyPosting(qs) => Query::AnyPosting(all(qs)),
            Query::AllPostings(qs) => Query::AllPostings(all(qs)),
            q => q,
        })
    }

    /// The query with redundant parts removed, until that changes nothing
    /// (simplifyQuery). The parts of Not, AnyPosting and AllPostings are
    /// left as they are.
    pub fn simplify(self) -> Query {
        let mut q = self;
        loop {
            let q1 = simplify_once(q.clone());
            if q1 == q {
                return q;
            }
            q = q1;
        }
    }

    /// The query with the terms not satisfying `p` replaced by Any, then
    /// simplified (filterQuery). And and Or are looked inside; Not is not.
    pub fn filter(self, p: impl Fn(&Query) -> bool + Copy) -> Query {
        fn go(p: impl Fn(&Query) -> bool + Copy, q: Query) -> Query {
            match q {
                Query::And(qs) => Query::And(qs.into_iter().map(|q| go(p, q)).collect()),
                Query::Or(qs) => Query::Or(qs.into_iter().map(|q| go(p, q)).collect()),
                q if p(&q) => q,
                _ => Query::Any,
            }
        }
        go(p, self).simplify()
    }

    /// Like [`filter`](Query::filter), but keeping the negations of the
    /// terms satisfying `p` too (filterQueryOrNotQuery).
    pub fn filter_or_not(self, p: impl Fn(&Query) -> bool + Copy) -> Query {
        let q = match self {
            Query::And(qs) => Query::And(qs.into_iter().map(|q| q.filter_or_not(p)).collect()),
            Query::Or(qs) => Query::Or(qs.into_iter().map(|q| q.filter_or_not(p)).collect()),
            Query::Not(q) if p(&q) => Query::Not(Box::new(q.filter_or_not(p))),
            q if p(&q) => q,
            _ => Query::Any,
        };
        q.simplify()
    }

    /// Whether `p` holds for any term of this query, looking inside And and
    /// Or, and at the query in a Not (matchesQuery).
    pub fn matches_any_term(&self, p: impl Fn(&Query) -> bool + Copy) -> bool {
        match self {
            Query::And(qs) | Query::Or(qs) => qs.iter().any(|q| q.matches_any_term(p)),
            Query::Not(q) => p(q),
            q => p(q),
        }
    }

    /// Whether the query matches everything (queryIsNull).
    pub fn is_null(&self) -> bool {
        match self {
            Query::Any => true,
            Query::And(qs) => qs.is_empty(),
            Query::Not(q) => matches!(&**q, Query::Or(qs) if qs.is_empty()),
            _ => false,
        }
    }

    /// Whether this is a `date:` term, not a compound query (queryIsDate).
    pub fn is_date(&self) -> bool {
        matches!(self, Query::Date(_))
    }

    pub fn is_date2(&self) -> bool {
        matches!(self, Query::Date2(_))
    }

    pub fn is_date_or_date2(&self) -> bool {
        matches!(self, Query::Date(_) | Query::Date2(_))
    }

    pub fn is_status(&self) -> bool {
        matches!(self, Query::StatusQ(_))
    }

    pub fn is_code(&self) -> bool {
        matches!(self, Query::Code(_))
    }

    pub fn is_desc(&self) -> bool {
        matches!(self, Query::Desc(_))
    }

    pub fn is_tag(&self) -> bool {
        matches!(self, Query::Tag(..))
    }

    pub fn is_acct(&self) -> bool {
        matches!(self, Query::Acct(_))
    }

    pub fn is_find(&self) -> bool {
        matches!(self, Query::Find(..))
    }

    pub fn is_type(&self) -> bool {
        matches!(self, Query::Type(_))
    }

    /// Whether this is a `depth:` term, overall or per account.
    pub fn is_depth(&self) -> bool {
        matches!(self, Query::Depth(_) | Query::DepthAcct(..))
    }

    /// Whether this is an overall depth limit (queryIsFlatDepth).
    pub fn is_flat_depth(&self) -> bool {
        matches!(self, Query::Depth(_))
    }

    pub fn is_real(&self) -> bool {
        matches!(self, Query::Real(_))
    }

    pub fn is_amt(&self) -> bool {
        matches!(self, Query::Amt(..))
    }

    pub fn is_cur(&self) -> bool {
        matches!(self, Query::Cur(_))
    }

    pub fn is_amt_or_cur(&self) -> bool {
        self.is_amt() || self.is_cur()
    }

    /// Whether the query gives a start date (a secondary one with
    /// `secondary`) and nothing else (queryIsStartDateOnly).
    pub fn is_start_date_only(&self, secondary: bool) -> bool {
        match self {
            Query::Or(qs) | Query::And(qs) => qs.iter().all(|q| q.is_start_date_only(secondary)),
            Query::Date(DateSpan(Some(_), _)) => !secondary,
            Query::Date2(DateSpan(Some(_), _)) => secondary,
            _ => false,
        }
    }

    /// Whether the query is about transactions or postings, so that it
    /// cannot apply to account declarations (queryIsTransactionRelated).
    pub fn is_transaction_related(&self) -> bool {
        self.matches_any_term(|q| {
            q.is_date()
                || q.is_date2()
                || q.is_status()
                || q.is_code()
                || q.is_desc()
                || q.is_real()
                || q.is_amt()
                || q.is_cur()
        })
    }

    /// The start date (or secondary date) the query gives: the earliest of
    /// an Or's, an open one counting as earliest; the latest of an And's;
    /// Not ignored (queryStartDate).
    pub fn start_date(&self, secondary: bool) -> Option<Day> {
        match self {
            Query::Or(qs) => qs.iter().map(|q| q.start_date(secondary)).min().flatten(),
            Query::And(qs) => qs.iter().map(|q| q.start_date(secondary)).max().flatten(),
            Query::Date(span) if !secondary => span.start(),
            Query::Date2(span) if secondary => span.start(),
            _ => None,
        }
    }

    /// The end date (or secondary date) the query gives: the latest of an
    /// Or's, an open one counting as latest; the earliest given one of an
    /// And's; Not ignored (queryEndDate).
    pub fn end_date(&self, secondary: bool) -> Option<Day> {
        match self {
            Query::Or(qs) => {
                let ends: Vec<Option<Day>> = qs.iter().map(|q| q.end_date(secondary)).collect();
                if ends.contains(&None) {
                    None
                } else {
                    ends.into_iter().flatten().max()
                }
            }
            Query::And(qs) => qs.iter().filter_map(|q| q.end_date(secondary)).min(),
            Query::Date(span) if !secondary => span.end(),
            Query::Date2(span) if secondary => span.end(),
            _ => None,
        }
    }

    /// The span of dates (or secondary dates) the query gives: the union of
    /// an Or's, the intersection of an And's, Not ignored (queryDateSpan).
    pub fn date_span(&self, secondary: bool) -> DateSpan {
        let spans =
            |qs: &[Query]| -> Vec<DateSpan> { qs.iter().map(|q| q.date_span(secondary)).collect() };
        match self {
            Query::Or(qs) => spans_union(&spans(qs)),
            Query::And(qs) => spans_intersect(&spans(qs)),
            Query::Date(span) => *span,
            Query::Date2(span) if secondary => *span,
            _ => DateSpan::default(),
        }
    }

    /// Like [`date_span`](Query::date_span), with primary and secondary
    /// dates alike (queryDateSpan').
    pub fn date_span_either(&self) -> DateSpan {
        let spans =
            |qs: &[Query]| -> Vec<DateSpan> { qs.iter().map(Query::date_span_either).collect() };
        match self {
            Query::Or(qs) => spans_union(&spans(qs)),
            Query::And(qs) => spans_intersect(&spans(qs)),
            Query::Date(span) | Query::Date2(span) => *span,
            _ => DateSpan::default(),
        }
    }

    /// The depth limits the query gives (queryDepth).
    pub fn depth(&self) -> DepthSpec {
        match self {
            Query::Or(qs) | Query::And(qs) => qs
                .iter()
                .map(Query::depth)
                .fold(DepthSpec::default(), DepthSpec::combine),
            Query::Depth(d) => DepthSpec {
                flat: Some(*d),
                regexps: vec![],
            },
            Query::DepthAcct(r, d) => DepthSpec {
                flat: None,
                regexps: vec![(r.clone(), *d)],
            },
            _ => DepthSpec::default(),
        }
    }

    /// Whether the query matches this commodity symbol (matchesCommodity).
    pub fn matches_commodity(&self, s: &str) -> bool {
        match self {
            Query::Cur(r) | Query::Find(r, _) => r.is_match(s),
            Query::Any => true,
            Query::Or(qs) => qs.iter().any(|q| q.matches_commodity(s)),
            Query::And(qs) | Query::AnyPosting(qs) => qs.iter().all(|q| q.matches_commodity(s)),
            Query::AllPostings(qs) => all1(qs, |q| q.matches_commodity(s)),
            _ => false,
        }
    }

    /// Like [`matches_commodity`](Query::matches_commodity), with `tag:`
    /// terms matching the commodity's tags as `tags` gives them
    /// (matchesCommodityExtra).
    pub fn matches_commodity_extra<'t>(&self, tags: &dyn Fn(&str) -> &'t [Tag], s: &str) -> bool {
        let m = |q: &Query| q.matches_commodity_extra(tags, s);
        match self {
            Query::Not(q) => !m(q),
            Query::Or(qs) => qs.iter().any(m),
            Query::And(qs) | Query::AnyPosting(qs) => qs.iter().all(m),
            Query::AllPostings(qs) => all1(qs, m),
            Query::Tag(n, v) => patterns_match_tags(n, v.as_ref(), tags(s)),
            q => q.matches_commodity(s),
        }
    }

    /// Whether the query matches this amount, terms about other things
    /// matching (matchesAmount).
    pub fn matches_amount(&self, a: &Amount) -> bool {
        match self {
            Query::Not(q) => !q.matches_amount(a),
            Query::Any => true,
            Query::None => false,
            Query::Or(qs) => qs.iter().any(|q| q.matches_amount(a)),
            Query::And(qs) | Query::AnyPosting(qs) => qs.iter().all(|q| q.matches_amount(a)),
            Query::AllPostings(qs) => all1(qs, |q| q.matches_amount(a)),
            Query::Amt(ord, n) => compare_amount(*ord, n, a),
            Query::Cur(r) => r.is_match(&a.commodity),
            _ => true,
        }
    }

    /// Whether the query matches one of the amounts, or the null amount if
    /// there are none (matchesMixedAmount).
    pub fn matches_mixed_amount(&self, m: &MixedAmount) -> bool {
        match m.amounts_raw() {
            [] => self.matches_amount(&Amount::null()),
            amounts => amounts.iter().any(|a| self.matches_amount(a)),
        }
    }

    /// Whether the query matches this account name (matchesAccount). Only
    /// account and depth terms can; others, about things an account name
    /// does not have, do not.
    pub fn matches_account(&self, a: &str) -> bool {
        match self {
            Query::Any => true,
            Query::None => false,
            Query::Not(q) => !q.matches_account(a),
            Query::Or(qs) => qs.iter().any(|q| q.matches_account(a)),
            Query::And(qs) | Query::AnyPosting(qs) => qs.iter().all(|q| q.matches_account(a)),
            Query::AllPostings(qs) => all1(qs, |q| q.matches_account(a)),
            Query::Acct(r) | Query::Find(r, _) => r.is_match(a),
            Query::Depth(d) => account_name_level(a) <= *d,
            Query::DepthAcct(r, d) => account_name_level(a) <= *d || !r.is_match(a),
            _ => false,
        }
    }

    /// Like [`matches_account`](Query::matches_account), with `type:`
    /// terms matching the account's type and `tag:` terms its tags, as
    /// `types` and `tags` give them (matchesAccountExtra).
    pub fn matches_account_extra<'t>(
        &self,
        types: &dyn Fn(&str) -> Option<AccountType>,
        tags: &dyn Fn(&str) -> &'t [Tag],
        a: &str,
    ) -> bool {
        let m = |q: &Query| q.matches_account_extra(types, tags, a);
        match self {
            Query::Not(q) => !m(q),
            Query::Or(qs) => qs.iter().any(m),
            Query::And(qs) | Query::AnyPosting(qs) => qs.iter().all(m),
            Query::AllPostings(qs) => all1(qs, m),
            Query::Type(ts) => types(a).is_some_and(|t| is_one_of(t, ts)),
            Query::Tag(n, v) => patterns_match_tags(n, v.as_ref(), tags(a)),
            q => q.matches_account(a),
        }
    }

    /// Whether the query matches this posting (matchesPosting). Account
    /// terms match its account or, if hledger changed that, its original
    /// account; code and description terms its transaction's; tag terms
    /// its tags and its transaction's.
    pub fn matches_posting(&self, p: PostingRef) -> bool {
        match self {
            Query::Not(q) => !q.matches_posting(p),
            Query::Any => true,
            Query::None => false,
            Query::Or(qs) => qs.iter().any(|q| q.matches_posting(p)),
            Query::And(qs) => qs.iter().all(|q| q.matches_posting(p)),
            Query::AnyPosting(qs) => matches_siblings(false, qs, p, &|q, p| q.matches_posting(p)),
            Query::AllPostings(qs) => matches_siblings(true, qs, p, &|q, p| q.matches_posting(p)),
            Query::Code(r) => p.t.is_some_and(|t| r.is_match(&t.code)),
            Query::Desc(r) => p.t.is_some_and(|t| r.is_match(&t.description)),
            Query::Acct(r) => {
                r.is_match(&p.p.account)
                    || p.p
                        .original
                        .as_ref()
                        .is_some_and(|o| r.is_match(&o.account))
            }
            Query::Find(r, span) => find_matches_posting(r, span.as_ref(), p),
            Query::Date(span) => span.contains(p.date()),
            Query::Date2(span) => span.contains(p.date2()),
            Query::StatusQ(s) => p.status() == *s,
            Query::Real(v) => *v == p.p.is_real(),
            q @ (Query::Depth(_) | Query::DepthAcct(..)) => q.matches_account(&p.p.account),
            q @ Query::Amt(..) => q.matches_mixed_amount(&p.p.amount),
            Query::Cur(r) => {
                p.p.amount
                    .amounts_raw()
                    .iter()
                    .any(|a| r.is_match(&a.commodity))
            }
            Query::Tag(n, v) => match (n.src(), v) {
                ("payee", Some(v)) => p.t.is_some_and(|t| v.is_match(t.payee())),
                ("note", Some(v)) => p.t.is_some_and(|t| v.is_match(t.note())),
                (_, v) => patterns_match_tags(n, v.as_ref(), p.all_tags()),
            },
            Query::Type(_) => false,
        }
    }

    /// Like [`matches_posting`](Query::matches_posting), with `type:` terms
    /// matching the type of the posting's account, or of its original
    /// account, as `types` gives them (matchesPostingExtra).
    pub fn matches_posting_extra(
        &self,
        types: &dyn Fn(&str) -> Option<AccountType>,
        p: PostingRef,
    ) -> bool {
        let m = |q: &Query, p: PostingRef| q.matches_posting_extra(types, p);
        match self {
            Query::Not(q) => !m(q, p),
            Query::Or(qs) => qs.iter().any(|q| m(q, p)),
            Query::And(qs) => qs.iter().all(|q| m(q, p)),
            Query::AnyPosting(qs) => matches_siblings(false, qs, p, &m),
            Query::AllPostings(qs) => matches_siblings(true, qs, p, &m),
            Query::Type(ts) => {
                types(&p.p.account).is_some_and(|t| is_one_of(t, ts))
                    || p.p
                        .original
                        .as_ref()
                        .and_then(|o| types(&o.account))
                        .is_some_and(|t| is_one_of(t, ts))
            }
            q => q.matches_posting(p),
        }
    }

    /// Whether the query matches this transaction (matchesTransaction):
    /// posting terms match if they match any posting.
    pub fn matches_transaction(&self, t: &Transaction) -> bool {
        let any_posting = |q: &Query| {
            t.postings
                .iter()
                .any(|p| q.matches_posting(PostingRef::new(t, p)))
        };
        let each = |qs: &[Query], p| qs.iter().all(|q| q.matches_posting(PostingRef::new(t, p)));
        match self {
            Query::Not(q) => !q.matches_transaction(t),
            Query::Any => true,
            Query::None => false,
            Query::Or(qs) => qs.iter().any(|q| q.matches_transaction(t)),
            Query::And(qs) => qs.iter().all(|q| q.matches_transaction(t)),
            Query::AnyPosting(qs) => t.postings.iter().any(|p| each(qs, p)),
            Query::AllPostings(qs) => all1(&t.postings, |p| each(qs, p)),
            Query::Code(r) => r.is_match(&t.code),
            Query::Desc(r) => r.is_match(&t.description),
            Query::Find(r, span) => find_matches_transaction(r, span.as_ref(), t),
            Query::Date(span) => span.contains(t.date),
            Query::Date2(span) => span.contains(t.date2_or_date()),
            Query::StatusQ(s) => t.status == *s,
            Query::Real(v) => *v == t.real_postings().next().is_some(),
            q @ (Query::Acct(_)
            | Query::Amt(..)
            | Query::Depth(_)
            | Query::DepthAcct(..)
            | Query::Cur(_)) => any_posting(q),
            Query::Tag(n, v) => match (n.src(), v) {
                ("payee", Some(v)) => v.is_match(t.payee()),
                ("note", Some(v)) => v.is_match(t.note()),
                (_, v) => patterns_match_tags(n, v.as_ref(), t.all_tags()),
            },
            Query::Type(_) => false,
        }
    }

    /// Like [`matches_transaction`](Query::matches_transaction), with
    /// `type:` terms matching if a posting's account type does, as `types`
    /// gives them (matchesTransactionExtra).
    pub fn matches_transaction_extra(
        &self,
        types: &dyn Fn(&str) -> Option<AccountType>,
        t: &Transaction,
    ) -> bool {
        let m = |q: &Query| q.matches_transaction_extra(types, t);
        let each = |qs: &[Query], p| {
            qs.iter()
                .all(|q| q.matches_posting_extra(types, PostingRef::new(t, p)))
        };
        match self {
            Query::Not(q) => !m(q),
            Query::Or(qs) => qs.iter().any(m),
            Query::And(qs) => qs.iter().all(m),
            Query::AnyPosting(qs) => t.postings.iter().any(|p| each(qs, p)),
            Query::AllPostings(qs) => all1(&t.postings, |p| each(qs, p)),
            q @ Query::Type(_) => t
                .postings
                .iter()
                .any(|p| q.matches_posting_extra(types, PostingRef::new(t, p))),
            q => q.matches_transaction(t),
        }
    }

    /// Whether the query matches this payee (matchesPayee): only `payee:`
    /// (and `tag:payee=`) and `find:` terms can.
    pub fn matches_payee(&self, p: &str) -> bool {
        match self {
            Query::Not(q) => !q.matches_payee(p),
            Query::Any => true,
            Query::None => false,
            Query::Or(qs) => qs.iter().any(|q| q.matches_payee(p)),
            Query::And(qs) | Query::AnyPosting(qs) => qs.iter().all(|q| q.matches_payee(p)),
            Query::AllPostings(qs) => all1(qs, |q| q.matches_payee(p)),
            Query::Tag(n, Some(v)) if n.src() == "payee" => v.is_match(p),
            Query::Find(r, _) => r.is_match(p),
            _ => false,
        }
    }

    /// Whether the query's tag terms match this tag; other terms are
    /// ignored inside compound queries, and fail alone (matchesTag).
    pub fn matches_tag(&self, tag: &Tag) -> bool {
        let tags_in = |qs: &[Query]| -> Vec<bool> {
            qs.iter()
                .filter(|q| q.is_tag())
                .map(|q| q.matches_tag(tag))
                .collect()
        };
        match self {
            Query::Not(q) => !q.matches_tag(tag),
            Query::Any => true,
            Query::None => false,
            Query::Or(qs) => tags_in(qs).contains(&true),
            Query::And(qs) | Query::AnyPosting(qs) => !tags_in(qs).contains(&false),
            Query::AllPostings(qs) => {
                let r = tags_in(qs);
                !r.is_empty() && !r.contains(&false)
            }
            Query::Tag(n, v) => patterns_match_tags(n, v.as_ref(), [tag]),
            _ => false,
        }
    }

    /// Whether the query matches this market price declaration, terms
    /// about other things matching (matchesPriceDirective).
    pub fn matches_price_directive(&self, p: &PriceDirective) -> bool {
        match self {
            Query::None => false,
            Query::Not(q) => !q.matches_price_directive(p),
            Query::Or(qs) => qs.iter().any(|q| q.matches_price_directive(p)),
            Query::And(qs) | Query::AnyPosting(qs) => {
                qs.iter().all(|q| q.matches_price_directive(p))
            }
            Query::AllPostings(qs) => all1(qs, |q| q.matches_price_directive(p)),
            q @ Query::Amt(..) => q.matches_amount(&p.amount),
            q @ Query::Cur(_) => q.matches_commodity(&p.commodity),
            Query::Date(span) => span.contains(p.date),
            _ => true,
        }
    }
}

/// One simplification step (simplify in simplifyQuery).
fn simplify_once(q: Query) -> Query {
    match q {
        Query::And(mut qs) if qs.len() <= 1 => qs.pop().map_or(Query::Any, simplify_once),
        Query::And(qs) if same(&qs) => simplify_once(qs.into_iter().next().expect("not empty")),
        Query::And(qs) if qs.contains(&Query::None) => Query::None,
        Query::And(qs) if qs.iter().all(Query::is_date) => {
            let spans: Vec<DateSpan> = qs
                .iter()
                .filter_map(|q| match q {
                    Query::Date(s) => Some(*s),
                    _ => None,
                })
                .collect();
            Query::Date(spans_intersect(&spans))
        }
        Query::And(qs) => {
            let (dates, others): (Vec<Query>, Vec<Query>) = qs
                .into_iter()
                .filter(|q| *q != Query::Any)
                .partition(Query::is_date);
            Query::And(dates.into_iter().chain(others).map(simplify_once).collect())
        }
        Query::Or(mut qs) if qs.len() <= 1 => qs.pop().map_or(Query::Any, Query::simplify),
        Query::Or(qs) if same(&qs) => simplify_once(qs.into_iter().next().expect("not empty")),
        Query::Or(qs) if qs.contains(&Query::Any) => Query::Any,
        Query::Or(qs) => Query::Or(
            qs.into_iter()
                .filter(|q| *q != Query::None)
                .map(simplify_once)
                .collect(),
        ),
        Query::Date(DateSpan(None, None)) | Query::Date2(DateSpan(None, None)) => Query::Any,
        q => q,
    }
}

/// Whether all these queries are the same.
fn same(qs: &[Query]) -> bool {
    qs.windows(2).all(|w| w[0] == w[1])
}

/// Whether `f` holds for all of the items, of which there is at least one
/// (all1).
fn all1<I: IntoIterator>(items: I, f: impl FnMut(I::Item) -> bool) -> bool {
    let mut items = items.into_iter().peekable();
    items.peek().is_some() && items.all(f)
}

/// Whether an account type is one of these, or a subtype of one.
fn is_one_of(t: AccountType, types: &[AccountType]) -> bool {
    types.iter().any(|&u| t.is_subtype_of(u))
}

/// Whether some posting of the posting's transaction (with `all`, every
/// posting, of which there is one at least) is matched by all these
/// queries, as `m` matches a query and a posting; a posting with no
/// transaction is its only sibling (matchesSiblingPostings).
fn matches_siblings(
    all: bool,
    qs: &[Query],
    p: PostingRef,
    m: &dyn Fn(&Query, PostingRef) -> bool,
) -> bool {
    let matched = |sp: PostingRef| qs.iter().all(|q| m(q, sp));
    match p.t {
        None => matched(p),
        Some(t) => {
            let mut sibs = t.postings.iter().map(|sp| PostingRef::new(t, sp));
            if all {
                !t.postings.is_empty() && sibs.all(matched)
            } else {
                sibs.any(matched)
            }
        }
    }
}

/// Whether a `find:` query matches a posting: its date is in the span, or
/// the regex matches part of a text of it or its transaction
/// (findMatchesPosting).
fn find_matches_posting(r: &Regexp, span: Option<&DateSpan>, p: PostingRef) -> bool {
    span.is_some_and(|s| s.contains(p.date()))
        || posting_find_texts_match(r, p.p)
        || p.t.is_some_and(|t| transaction_find_texts_match(r, t))
}

/// Whether a `find:` query matches a transaction: its date is in the span,
/// or the regex matches part of a text of it or its postings
/// (findMatchesTransaction).
fn find_matches_transaction(r: &Regexp, span: Option<&DateSpan>, t: &Transaction) -> bool {
    span.is_some_and(|s| s.contains(t.date))
        || transaction_find_texts_match(r, t)
        || t.postings.iter().any(|p| posting_find_texts_match(r, p))
}

/// Whether the regex matches part of a posting's visible texts: its
/// account (and original account), comment, and amount on one line,
/// rendered last as it costs the most (postingFindTexts).
fn posting_find_texts_match(r: &Regexp, p: &crate::model::Posting) -> bool {
    r.is_match(&p.account)
        || p.original.as_ref().is_some_and(|o| r.is_match(&o.account))
        || r.is_match(&p.comment)
        || r.is_match(&show_mixed_amount_one_line(&p.amount))
}

/// Whether the regex matches part of a transaction's description, code or
/// comment (transactionFindTexts).
fn transaction_find_texts_match(r: &Regexp, t: &Transaction) -> bool {
    r.is_match(&t.description) || r.is_match(&t.code) || r.is_match(&t.comment)
}

/// Whether the regexes match part of the name, and of the value if given,
/// of one of these tags (patternsMatchTags).
pub fn patterns_match_tags<'t>(
    name: &Regexp,
    value: Option<&Regexp>,
    tags: impl IntoIterator<Item = &'t Tag>,
) -> bool {
    tags.into_iter()
        .any(|(n, v)| name.is_match(n) && value.is_none_or(|r| r.is_match(v)))
}

/// Whether the amount's quantity compares so with the number
/// (compareAmount).
pub fn compare_amount(ord: OrdPlus, q: &Decimal, a: &Amount) -> bool {
    use OrdPlus::*;
    let aq = &a.quantity;
    match ord {
        Lt => aq < q,
        LtEq => aq <= q,
        Gt => aq > q,
        GtEq => aq >= q,
        Eq => aq == q,
        AbsLt => aq.abs() < q.abs(),
        AbsLtEq => aq.abs() <= q.abs(),
        AbsGt => aq.abs() > q.abs(),
        AbsGtEq => aq.abs() >= q.abs(),
        AbsEq => aq.abs() == q.abs(),
    }
}

/// The account an account register is about, if the first such option
/// names one, and whether its subaccounts are included (inAccount).
pub fn in_account(opts: &[QueryOpt]) -> Option<(AccountName, bool)> {
    opts.iter().find_map(|o| match o {
        QueryOpt::InAcctOnly(a) => Some((a.clone(), false)),
        QueryOpt::InAcct(a) => Some((a.clone(), true)),
        QueryOpt::Interval(_) => None,
    })
}

/// A query matching the account (and subaccounts) an account register is
/// about, if an option names one (inAccountQuery).
pub fn in_account_query(opts: &[QueryOpt]) -> Option<Query> {
    let (a, subs) = in_account(opts)?;
    let end = if subs { "(:|$)" } else { "$" };
    let r = to_regex(&format!("^{}{end}", escape_name(&a)))
        .expect("an escaped account name is a valid regex");
    Some(Query::Acct(r))
}

pub(crate) mod tests {
    use super::*;
    use crate::amount::{Amount, usd};
    use crate::dates::EfDay;
    use crate::model::{Posting, Transaction};

    fn ci(s: &str) -> Regexp {
        to_regex_ci(s).unwrap()
    }

    fn cs(s: &str) -> Regexp {
        to_regex(s).unwrap()
    }

    fn acct(s: &str) -> Query {
        Query::Acct(ci(s))
    }

    fn desc(s: &str) -> Query {
        Query::Desc(ci(s))
    }

    fn day(y: i64, m: u32, d: u32) -> Day {
        Day::from_ymd(y, m, d).unwrap()
    }

    fn nulldate() -> Day {
        crate::dates::null_date()
    }

    fn q(s: &str) -> Result<(Query, Vec<QueryOpt>), String> {
        parse_query(nulldate(), s)
    }

    fn bq(s: &str) -> Result<(Query, Vec<QueryOpt>), String> {
        parse_boolean_query(nulldate(), s)
    }

    fn term(s: &str) -> Result<(Query, Vec<QueryOpt>), String> {
        parse_query_term(nulldate(), s)
    }

    fn just(q: Query) -> Result<(Query, Vec<QueryOpt>), String> {
        Ok((q, vec![]))
    }

    // The doctests of parseQuery and parseBooleanQuery.
    #[cfg_attr(test, test)]
    pub(crate) fn doctests() {
        assert_eq!(
            q("expenses:dining out"),
            just(Query::Or(vec![acct("expenses:dining"), acct("out")]))
        );
        assert_eq!(
            q("\"expenses:dining out\""),
            just(acct("expenses:dining out"))
        );
        assert_eq!(
            bq("expenses:dining AND out"),
            just(Query::And(vec![acct("expenses:dining"), acct("out")]))
        );
        assert_eq!(
            bq("expenses:dining AND desc:a OR desc:b"),
            just(Query::Or(vec![
                Query::And(vec![acct("expenses:dining"), desc("a")]),
                desc("b")
            ]))
        );
    }

    // Errors as the hledger binary shows them for these query arguments.
    #[test]
    fn query_errors() {
        assert_eq!(
            term("expr:(a").unwrap_err(),
            "failed to parse query:1:3:\n  |\n1 | (a\n  |   ^\nunexpected end of input\n\
             expecting \"and\", \"or\", or ')'\n"
        );
        assert_eq!(
            term("expr:date:2023 OR date:2024").unwrap_err(),
            "failed to parse query:1:23:\n  |\n1 | date:2023 OR date:2024\n  |                       ^\n\
             sorry, using date: in OR expressions is not supported.\n"
        );
        assert!(term("expr:date:2023 AND date:2024").is_ok());
        assert_eq!(
            term("status:\u{e9}").unwrap_err(),
            "\"status:\u{e9}\" gave a parse error: could not parse \"\\233\" as a status (should be *, ! or empty)"
        );
        assert_eq!(
            term("type:\u{e9}").unwrap_err(),
            "could not parse \"\\233\" as an account type code.\n\
             type:'s argument should be one or more of A, L, E, R, X, C, V, G, U, M"
        );
        assert_eq!(
            term("depth:x").unwrap_err(),
            "could not parse depth \"x\": it should be a whole number, 0 or more, optionally preceded by ACCTREGEX="
        );
        assert_eq!(term("depth:(3)"), just(Query::And(vec![Query::Depth(3)])));
        assert_eq!(
            term("acct:(").unwrap_err(),
            "This regular expression is invalid or unsupported, please correct it: ("
        );
        assert!(
            term("expr:date:foo")
                .unwrap_err()
                .starts_with("could not parse \"date:foo\": 1:1:\n")
        );
        assert_eq!(
            term("date:monthly in 2024"),
            Ok((
                Query::Date(DateSpan::flex(Some(day(2024, 1, 1)), Some(day(2025, 1, 1)))),
                vec![QueryOpt::Interval(Interval::Months(1))]
            ))
        );
    }

    // Account types match their subtypes, and a posting's original account
    // counts.
    #[test]
    fn matching_types() {
        use AccountType::*;
        let types = |a: &str| match a {
            "assets:cash" => Some(Cash),
            "equity:conversion" => Some(Conversion),
            "revenues:gains" => Some(Gain),
            _ => None,
        };
        let ty = |s: &str| term(&format!("type:{s}")).unwrap().0;
        let m = |q: &Query, a: &str| {
            q.matches_posting_extra(
                &types,
                PostingRef {
                    t: None,
                    p: &posting(a),
                },
            )
        };
        assert!(m(&ty("a"), "assets:cash"));
        assert!(m(&ty("c"), "assets:cash"));
        assert!(!m(&ty("l"), "assets:cash"));
        assert!(m(&ty("e"), "equity:conversion"));
        assert!(m(&ty("r"), "revenues:gains"));
        assert!(!m(&ty("x"), "other"));
        assert!(m(&Query::Not(Box::new(ty("l"))), "assets:cash"));
        let renamed = Posting {
            original: Some(Box::new(posting("assets:cash"))),
            ..posting("renamed")
        };
        assert!(ty("a").matches_posting_extra(
            &types,
            PostingRef {
                t: None,
                p: &renamed
            }
        ));
        assert!(ty("a").matches_account_extra(&types, &|_| &[], "assets:cash"));
        let t = Transaction {
            postings: vec![posting("expenses:food"), posting("assets:cash")],
            ..Transaction::default()
        };
        assert!(ty("c").matches_transaction_extra(&types, &t));
        assert!(!ty("c").matches_transaction(&t));
    }

    // tests_Query "simplifyQuery".
    #[cfg_attr(test, test)]
    pub(crate) fn simplifying() {
        use Query::{And, Any, Date, Or};
        let (b2012, e2013) = (day(2012, 1, 1), day(2013, 1, 1));
        assert_eq!(
            Or(vec![Query::Acct(cs("a"))]).simplify(),
            Query::Acct(cs("a"))
        );
        assert_eq!(Or(vec![Any, Query::None]).simplify(), Any);
        assert_eq!(And(vec![Any, Query::None]).simplify(), Query::None);
        assert_eq!(And(vec![Any, Any]).simplify(), Any);
        assert_eq!(
            And(vec![Query::Acct(cs("b")), Any]).simplify(),
            Query::Acct(cs("b"))
        );
        assert_eq!(
            And(vec![Any, And(vec![Date(DateSpan::default())])]).simplify(),
            Any
        );
        assert_eq!(
            And(vec![
                Date(DateSpan::exact(None, Some(e2013))),
                Date(DateSpan::exact(Some(b2012), None))
            ])
            .simplify(),
            Date(DateSpan::exact(Some(b2012), Some(e2013)))
        );
        assert_eq!(
            And(vec![Or(vec![]), Or(vec![Query::Desc(cs("b b"))])]).simplify(),
            Query::Desc(cs("b b"))
        );
    }

    // tests_Query "parseQuery".
    #[cfg_attr(test, test)]
    pub(crate) fn parsing_queries() {
        assert_eq!(
            q("acct:'expenses:autres d\u{e9}penses' desc:b"),
            just(Query::And(vec![
                acct("expenses:autres d\u{e9}penses"),
                desc("b")
            ]))
        );
        assert_eq!(
            q("inacct:a desc:\"b b\""),
            Ok((desc("b b"), vec![QueryOpt::InAcct("a".into())]))
        );
        assert_eq!(
            q("inacct:a inacct:b"),
            Ok((
                Query::Any,
                vec![QueryOpt::InAcct("a".into()), QueryOpt::InAcct("b".into())]
            ))
        );
        assert_eq!(q("desc:'x x'"), just(desc("x x")));
        assert_eq!(
            q("'a a' 'b"),
            just(Query::Or(vec![acct("a a"), acct("'b")]))
        );
        assert_eq!(q("\""), just(acct("\"")));
        assert_eq!(q("find:a"), just(Query::Find(ci("a"), None)));
        assert_eq!(
            q("::a ::b"),
            just(Query::And(vec![
                Query::Find(ci("a"), None),
                Query::Find(ci("b"), None)
            ]))
        );
        assert_eq!(
            q("not:::'a b'"),
            just(Query::Not(Box::new(Query::Find(ci("a b"), None))))
        );
        assert_eq!(
            q("::2024"),
            just(Query::Find(
                ci("2024"),
                Some(DateSpan::flex(Some(day(2024, 1, 1)), Some(day(2025, 1, 1))))
            ))
        );
    }

    // tests_Query "parseBooleanQuery".
    #[cfg_attr(test, test)]
    pub(crate) fn parsing_boolean_queries() {
        use Query::{And, Not, Or};
        let tag = |n: &str, v: &str| Query::Tag(ci(n), Some(ci(v)));
        let not = |q| Not(Box::new(q));
        let food = || acct("expenses:food");
        assert_eq!(bq("(tag:'atag=a')"), just(tag("atag", "a")));
        assert_eq!(bq("(  tag:\"atag=a\"  )"), just(tag("atag", "a")));
        assert_eq!(bq("(acct:'expenses:food')"), just(food()));
        assert_eq!(bq("(((acct:'expenses:food')))"), just(food()));
        assert_eq!(
            bq("acct:'expenses:food' AND desc:'b'"),
            just(And(vec![food(), desc("b")]))
        );
        assert_eq!(
            bq("((desc:'a') AND (desc:'b') OR (desc:'c'))"),
            just(Or(vec![And(vec![desc("a"), desc("b")]), desc("c")]))
        );
        assert_eq!(
            bq("((desc:'a') OR (desc:'b') AND (desc:'c'))"),
            just(Or(vec![desc("a"), And(vec![desc("b"), desc("c")])]))
        );
        assert_eq!(
            bq("((desc:'a') AND desc:'b' AND (desc:'c'))"),
            just(And(vec![desc("a"), desc("b"), desc("c")]))
        );
        assert_eq!(
            bq("(NOT (desc:'a') AND (desc:'b'))"),
            just(And(vec![not(desc("a")), desc("b")]))
        );
        assert_eq!(
            bq("((desc:'a') AND (NOT desc:'b'))"),
            just(And(vec![desc("a"), not(desc("b"))]))
        );
        assert_eq!(
            bq("(desc:'a' AND desc:'b')"),
            just(And(vec![desc("a"), desc("b")]))
        );
        assert_eq!(
            bq("(acct:'a' acct:'b')"),
            just(Or(vec![acct("a"), acct("b")]))
        );
        assert_eq!(
            bq(" acct:'a' acct:'b'"),
            just(Or(vec![acct("a"), acct("b")]))
        );
        assert_eq!(bq("not:a"), just(not(acct("a"))));
        assert_eq!(
            bq("expenses:food OR (tag:A expenses:drink)"),
            just(Or(vec![
                food(),
                And(vec![acct("expenses:drink"), Query::Tag(ci("A"), None)])
            ]))
        );
        assert_eq!(bq("not a"), just(not(acct("a"))));
        assert_eq!(bq("nota"), just(acct("nota")));
        assert_eq!(bq("not (acct:a)"), just(not(acct("a"))));
    }

    // tests_Query "words''".
    #[cfg_attr(test, test)]
    pub(crate) fn splitting_words() {
        assert_eq!(query_words(&[], "a b"), ["a", "b"]);
        assert_eq!(query_words(&[], "'a b'"), ["a b"]);
        assert_eq!(query_words(&[], "not:a b"), ["not:a", "b"]);
        assert_eq!(query_words(&[], "not:'a b'"), ["not:a b"]);
        assert_eq!(query_words(&[], "'not:a b'"), ["not:a b"]);
        assert_eq!(query_words(&["desc:"], "not:desc:'a b'"), ["not:desc:a b"]);
        assert_eq!(
            query_words(&QUERY_PREFIXES, "\"acct:expenses:autres d\u{e9}penses\""),
            ["acct:expenses:autres d\u{e9}penses"]
        );
        assert_eq!(query_words(&QUERY_PREFIXES, "\""), ["\""]);
    }

    // tests_Query "filterQuery".
    #[cfg_attr(test, test)]
    pub(crate) fn filtering() {
        use Query::{And, Any, Depth, Not, Or, StatusQ};
        assert_eq!(Any.filter(Query::is_depth), Any);
        assert_eq!(Depth(1).filter(Query::is_depth), Depth(1));
        assert_eq!(
            And(vec![And(vec![StatusQ(Status::Cleared), Depth(1)])]).filter(|q| !q.is_depth()),
            StatusQ(Status::Cleared)
        );
        assert_eq!(
            And(vec![
                Query::Date(DateSpan::default()),
                Not(Box::new(Or(vec![Any, Depth(1)])))
            ])
            .filter(Query::is_depth),
            Any
        );
    }

    // tests_Query "parseQueryTerm".
    #[cfg_attr(test, test)]
    pub(crate) fn parsing_terms() {
        let not = |q| Query::Not(Box::new(q));
        assert_eq!(term("a"), just(acct("a")));
        assert_eq!(
            term("acct:expenses:autres d\u{e9}penses"),
            just(acct("expenses:autres d\u{e9}penses"))
        );
        assert_eq!(term("not:desc:a b"), just(not(desc("a b"))));
        for (s, status) in [
            ("status:1", Status::Cleared),
            ("status:*", Status::Cleared),
            ("status:!", Status::Pending),
            ("status:0", Status::Unmarked),
            ("status:", Status::Unmarked),
        ] {
            assert_eq!(term(s), just(Query::StatusQ(status)));
        }
        assert_eq!(term("payee:x"), just(payee_tag(Some("x")).unwrap()));
        assert_eq!(term("note:x"), just(note_tag(Some("x")).unwrap()));
        assert_eq!(term("real:1"), just(Query::Real(true)));
        assert_eq!(
            term("date:2008"),
            just(Query::Date(DateSpan::flex(
                Some(day(2008, 1, 1)),
                Some(day(2009, 1, 1))
            )))
        );
        assert_eq!(
            term("date:from 2012/5/17"),
            just(Query::Date(DateSpan::exact(Some(day(2012, 5, 17)), None)))
        );
        assert_eq!(
            term("date:20180101-201804"),
            just(Query::Date(DateSpan(
                Some(EfDay::exact(day(2018, 1, 1))),
                Some(EfDay::flex(day(2018, 4, 1)))
            )))
        );
        assert_eq!(
            term("inacct:a"),
            Ok((Query::Any, vec![QueryOpt::InAcct("a".into())]))
        );
        assert_eq!(term("tag:a"), just(Query::Tag(ci("a"), None)));
        assert_eq!(
            term("tag:a=some value"),
            just(Query::Tag(ci("a"), Some(ci("some value"))))
        );
        assert_eq!(term("amt:<0"), just(Query::Amt(OrdPlus::Lt, Decimal::ZERO)));
        assert_eq!(
            term("amt:>10000.10"),
            just(Query::Amt(OrdPlus::AbsGt, "10000.1".parse().unwrap()))
        );
    }

    // tests_Query "parseAmountQueryTerm".
    #[cfg_attr(test, test)]
    pub(crate) fn parsing_amount_terms() {
        use OrdPlus::*;
        let d = |s: &str| s.parse::<Decimal>().unwrap();
        let p = parse_amount_query_term;
        assert_eq!(p("<0"), Ok((Lt, d("0"))));
        assert_eq!(p(">0"), Ok((Gt, d("0"))));
        assert_eq!(p(" > - 0 "), Ok((Gt, d("0"))));
        assert_eq!(p(">10000.10"), Ok((AbsGt, d("10000.1"))));
        assert_eq!(p("=0.23"), Ok((AbsEq, d("0.23"))));
        assert_eq!(p("0.23"), Ok((AbsEq, d("0.23"))));
        assert_eq!(p("<=+0.23"), Ok((LtEq, d("0.23"))));
        assert_eq!(p("-0.23"), Ok((Eq, d("-0.23"))));
        assert!(p("-0,23").is_err());
        assert!(p("=.23").is_err());
    }

    // tests_Query "queryStartDate" and "queryEndDate".
    #[cfg_attr(test, test)]
    pub(crate) fn start_and_end_dates() {
        use Query::{And, Date, Or};
        let (small, big) = (Some(day(2000, 1, 1)), Some(day(2000, 1, 2)));
        let from = |d| Date(DateSpan::exact(d, None));
        let to = |d| Date(DateSpan::exact(None, d));
        let null = || Date(DateSpan::default());
        assert_eq!(And(vec![from(small), from(big)]).start_date(false), big);
        assert_eq!(And(vec![from(small), null()]).start_date(false), small);
        assert_eq!(Or(vec![from(small), from(big)]).start_date(false), small);
        assert_eq!(Or(vec![from(small), null()]).start_date(false), None);
        assert_eq!(And(vec![to(small), to(big)]).end_date(false), small);
        assert_eq!(And(vec![to(small), null()]).end_date(false), small);
        assert_eq!(Or(vec![to(small), to(big)]).end_date(false), big);
        assert_eq!(Or(vec![to(small), null()]).end_date(false), None);
    }

    // tests_Query "matchesAccount" and "matchesAccountExtra".
    #[cfg_attr(test, test)]
    pub(crate) fn matching_accounts() {
        assert!(Query::Acct(cs("b:c")).matches_account("a:bb:c:d"));
        assert!(!Query::Acct(cs("^a:b")).matches_account("c:a:b"));
        assert!(Query::Depth(2).matches_account("a"));
        assert!(Query::Depth(2).matches_account("a:b"));
        assert!(!Query::Depth(2).matches_account("a:b:c"));
        assert!(!Query::Date(DateSpan::default()).matches_account("a"));
        assert!(!Query::Date2(DateSpan::default()).matches_account("a"));
        assert!(!Query::Tag(cs("a"), None).matches_account("a"));
        let tagq = Query::Tag(ci("type"), None);
        let tags = [("type".to_string(), String::new())];
        assert!(!tagq.matches_account_extra(&|_| None, &|_| &[], "a"));
        assert!(tagq.matches_account_extra(&|_| None, &|_| &tags, "a"));
    }

    fn matches(q: &Query, p: &Posting) -> bool {
        q.matches_posting(PostingRef { t: None, p })
    }

    fn amount(n: i64) -> MixedAmount {
        MixedAmount::from_amount(Amount::null().with_quantity(Decimal::from_i64(n)))
    }

    fn posting(account: &str) -> Posting {
        Posting {
            account: account.into(),
            ..Posting::default()
        }
    }

    // tests_Query "matchesPosting".
    #[cfg_attr(test, test)]
    pub(crate) fn matching_postings() {
        let cleared = Posting {
            status: Status::Cleared,
            ..Posting::default()
        };
        let unmarked = Posting::default();
        let not = |q| Query::Not(Box::new(q));
        assert!(matches(&Query::StatusQ(Status::Cleared), &cleared));
        assert!(!matches(&not(Query::StatusQ(Status::Cleared)), &cleared));
        assert!(matches(&Query::StatusQ(Status::Unmarked), &unmarked));
        assert!(!matches(&not(Query::StatusQ(Status::Unmarked)), &unmarked));
        let t = Transaction {
            status: Status::Cleared,
            ..Transaction::default()
        };
        assert!(Query::StatusQ(Status::Cleared).matches_posting(PostingRef::new(&t, &unmarked)));
        let real = |r| Posting {
            realness: r,
            ..Posting::default()
        };
        use crate::account::Realness;
        assert!(matches(&Query::Real(true), &real(Realness::Real)));
        assert!(!matches(&Query::Real(true), &real(Realness::Virtual)));
        assert!(!matches(
            &Query::Real(true),
            &real(Realness::BalancedVirtual)
        ));
        assert!(matches(&Query::Acct(cs("'b")), &posting("'b")));
        let tagged = |n: &str, v: &str| Posting {
            tags: vec![(n.to_string(), v.to_string())],
            ..Posting::default()
        };
        let tag = |n: &str, v: Option<&str>| Query::Tag(cs(n), v.map(cs));
        assert!(!matches(&tag("a", Some("r$")), &Posting::default()));
        assert!(matches(&tag("foo", None), &tagged("foo", "")));
        assert!(matches(&tag("foo", None), &tagged("foo", "baz")));
        assert!(matches(&tag("foo", Some("a")), &tagged("foo", "bar")));
        assert!(!matches(&tag("foo", Some("a$")), &tagged("foo", "bar")));
        assert!(!matches(&tag(" foo ", Some("a")), &tagged("foo", "bar")));
        assert!(!matches(
            &tag("foo foo", Some(" ar ba ")),
            &tagged("foo foo", "bar bar")
        ));
        let t = Transaction {
            tags: vec![("txntag".to_string(), String::new())],
            ..Transaction::default()
        };
        assert!(tag("txntag", None).matches_posting(PostingRef::new(&t, &unmarked)));
        let cur = |s: &str| term(&format!("cur:{s}")).unwrap().0;
        let dollars = Posting {
            amount: MixedAmount::from_amount(usd(Decimal::from_i64(1))),
            ..Posting::default()
        };
        let shekels = Posting {
            amount: MixedAmount::from_amount(Amount {
                commodity: "shekels".into(),
                ..Amount::null()
            }),
            ..Posting::default()
        };
        assert!(!matches(&cur("$"), &dollars));
        assert!(matches(&cur("\\$"), &dollars));
        assert!(matches(&cur("shekels"), &shekels));
        assert!(!matches(&cur("shek"), &shekels));
    }

    // tests_Query "any: and all: match a posting by looking at its siblings".
    #[cfg_attr(test, test)]
    pub(crate) fn matching_siblings() {
        let tie = |ps: Vec<Posting>| Transaction {
            postings: ps,
            ..Transaction::default()
        };
        let cashtxn = tie(vec![posting("expenses:food"), posting("assets:cash")]);
        let cardtxn = tie(vec![posting("expenses:food"), posting("liabilities:card")]);
        let anycash = Query::AnyPosting(vec![Query::Acct(cs("cash"))]);
        let food_posting_matches = |q: &Query, t: &Transaction| {
            let food: Vec<&Posting> = t
                .postings
                .iter()
                .filter(|p| &*p.account == "expenses:food")
                .collect();
            !food.is_empty()
                && food
                    .iter()
                    .all(|p| q.matches_posting(PostingRef::new(t, p)))
        };
        let all_match = |q: &Query, t: &Transaction| {
            !t.postings.is_empty()
                && t.postings
                    .iter()
                    .all(|p| q.matches_posting(PostingRef::new(t, p)))
        };
        assert!(food_posting_matches(&anycash, &cashtxn));
        assert!(!food_posting_matches(&anycash, &cardtxn));
        assert!(all_match(
            &Query::AllPostings(vec![Query::Acct(cs("expenses|assets"))]),
            &cashtxn
        ));
        assert!(!all_match(
            &Query::AllPostings(vec![Query::Acct(cs("assets"))]),
            &cashtxn
        ));
        assert!(matches(
            &Query::AnyPosting(vec![Query::Acct(cs("food"))]),
            &posting("expenses:food")
        ));
        assert!(!matches(&anycash, &posting("expenses:food")));
    }

    // tests_Query "matchesTransaction".
    #[cfg_attr(test, test)]
    pub(crate) fn matching_transactions() {
        let txn = |description: &str| Transaction {
            description: description.to_string(),
            ..Transaction::default()
        };
        assert!(Query::Any.matches_transaction(&Transaction::default()));
        assert!(!Query::Desc(cs("x x")).matches_transaction(&txn("x")));
        assert!(Query::Desc(cs("x x")).matches_transaction(&txn("x x")));
        let tagged = Transaction {
            tags: vec![("foo".to_string(), "bar".to_string())],
            ..Transaction::default()
        };
        assert!(Query::Tag(cs("foo"), Some(cs("a"))).matches_transaction(&tagged));
        assert!(Query::Tag(cs("payee"), Some(cs("payee"))).matches_transaction(&txn("payee|note")));
        assert!(Query::Tag(cs("note"), Some(cs("note"))).matches_transaction(&txn("payee|note")));
        let with_posting_tag = Transaction {
            postings: vec![Posting {
                tags: vec![("postingtag".to_string(), String::new())],
                ..Posting::default()
            }],
            ..Transaction::default()
        };
        assert!(Query::Tag(cs("postingtag"), None).matches_transaction(&with_posting_tag));
        let with = |ps: Vec<Posting>| Transaction {
            postings: ps,
            ..Transaction::default()
        };
        let paid = |a: &str, n| Posting {
            amount: amount(n),
            ..posting(a)
        };
        let exprq = Query::And(vec![
            Query::Acct(cs("cash")),
            Query::Amt(OrdPlus::Gt, Decimal::ZERO),
        ]);
        assert!(exprq.matches_transaction(&with(vec![paid("cash", 1)])));
        assert!(exprq.matches_transaction(&with(vec![posting("cash"), paid("food", 1)])));
        let anyq = Query::AnyPosting(vec![
            Query::Acct(cs("cash")),
            Query::Amt(OrdPlus::Gt, Decimal::ZERO),
        ]);
        assert!(anyq.matches_transaction(&with(vec![paid("cash", 1)])));
        assert!(!anyq.matches_transaction(&with(vec![posting("cash"), paid("food", 1)])));
        assert!(
            Query::AllPostings(vec![Query::Amt(OrdPlus::Eq, Decimal::ZERO)])
                .matches_transaction(&with(vec![posting("cash"), posting("food")]))
        );
        assert!(
            !Query::AllPostings(vec![
                Query::Acct(cs("cash")),
                Query::Amt(OrdPlus::Eq, Decimal::ZERO)
            ])
            .matches_transaction(&with(vec![posting("cash"), posting("food")]))
        );
    }
}
