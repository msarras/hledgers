//! Transactions and journals as journal text (showTransaction and
//! postingsAsLines in Hledger.Data.Transaction and Hledger.Data.Posting;
//! journalItemsAsText in Hledger.Write.Journal).

use dashu_int::IBig;

use crate::account::Realness;
use crate::amount::render::{
    AmountFormat, quote_commodity_symbol_if_needed, show_amount_b, show_amount_quantity,
    show_mixed_amount_lines_b, show_mixed_amount_lines_parts_b,
};
use crate::amount::{Amount, Precision, Side};
use crate::decimal::Decimal;
use crate::model::{BalanceAssertion, JournalItem, Posting, PriceDirective, Status, Transaction};
use crate::text::{Wide, display_width, fit_text};

/// A price directive as a P directive line, a zero price with its
/// commodity (showPriceDirective).
pub fn show_price_directive(pd: &PriceDirective) -> String {
    format!(
        "P {} {} {}",
        pd.date,
        quote_commodity_symbol_if_needed(&pd.commodity),
        show_amount_b(&AmountFormat::FULL_ZERO, &pd.amount).s
    )
}

/// How posting amounts are aligned (PostingLayout): right-aligned as in
/// hledger 1, or with their decimal marks at a column, moved right where
/// the account names need more room.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PostingLayout {
    Hledger1,
    Decimal(usize),
}

/// The column of decimal marks in print's default layout
/// (defaultDecimalMarkColumn).
pub const DEFAULT_DECIMAL_MARK_COLUMN: usize = 53;

impl Default for PostingLayout {
    /// defaultPostingLayout.
    fn default() -> PostingLayout {
        PostingLayout::Decimal(DEFAULT_DECIMAL_MARK_COLUMN)
    }
}

/// A transaction as journal text, ending with a blank line
/// (showTransactionWithLayout): any preceding comment lines, the first line,
/// the comment lines, and the postings; multi-commodity amounts as one
/// posting per commodity, unless `one_line_amounts`.
pub fn show_transaction(t: &Transaction, layout: PostingLayout, one_line_amounts: bool) -> String {
    let mut out = String::with_capacity(256);
    out.push_str(&t.preceding_comment);
    out.push_str(&transaction_first_line(t));
    out.push('\n');
    for c in render_comment_lines(&t.comment).iter().skip(1) {
        out.push_str(c);
        out.push('\n');
    }
    for l in postings_as_lines(
        &AmountFormat::DEFAULT,
        one_line_amounts,
        layout,
        &t.postings,
    ) {
        out.push_str(&l);
        out.push('\n');
    }
    out.push('\n');
    out
}

/// A transaction's first line: date, status, code, description and
/// same-line comment, without trailing spaces (transactionFirstLine).
pub fn transaction_first_line(t: &Transaction) -> String {
    let mut s = show_transaction_line_first_part(t);
    if !t.description.is_empty() {
        s.push(' ');
        s.push_str(&t.description);
    }
    if let Some(c) = render_comment_lines(&t.comment).first() {
        s.push_str(c);
    }
    s.truncate(s.trim_end().len());
    s
}

/// A transaction's dates, status and code (showTransactionLineFirstPart).
pub fn show_transaction_line_first_part(t: &Transaction) -> String {
    let mut s = t.date.to_string();
    if let Some(d2) = t.date2 {
        s.push('=');
        s.push_str(&d2.to_string());
    }
    match t.status {
        Status::Cleared => s.push_str(" *"),
        Status::Pending => s.push_str(" !"),
        Status::Unmarked => {}
    }
    if !t.code.is_empty() {
        s.push_str(" (");
        s.push_str(&t.code);
        s.push(')');
    }
    s
}

/// A comment as comment lines: the first (if not empty) to follow
/// something on the same line, the others indented (renderCommentLines).
pub fn render_comment_lines(t: &str) -> Vec<String> {
    let lines: Vec<&str> = t.lines().collect();
    match lines.as_slice() {
        [] => Vec::new(),
        [l] => vec![format!("  ; {l}")],
        ["", ls @ ..] => std::iter::once(String::new())
            .chain(ls.iter().map(|l| format!("    ; {l}")))
            .collect(),
        [l, ls @ ..] => std::iter::once(format!("  ; {l}"))
            .chain(ls.iter().map(|l| format!("    ; {l}")))
            .collect(),
    }
}

/// Postings as indented lines, aligned with each other: implicit amounts
/// are not shown; a multi-commodity amount is shown as one posting per
/// commodity (its balance assertion on the last), or with
/// `one_line_amounts`, on one line (postingsAsLinesWithLayout).
pub fn postings_as_lines(
    basefmt: &AmountFormat,
    one_line_amounts: bool,
    layout: PostingLayout,
    ps: &[Posting],
) -> Vec<String> {
    match layout {
        PostingLayout::Decimal(col) => {
            postings_as_lines_decimal(basefmt, one_line_amounts, col, ps)
        }
        PostingLayout::Hledger1 => postings_as_lines_hledger1(basefmt, one_line_amounts, ps),
    }
}

/// The widths of a posting's parts that its siblings align with
/// (PostingWidths): account name, main amount before and from its decimal
/// mark, assertion operator, and assertion amount before its decimal mark.
#[derive(Clone, Copy, Debug, Default)]
struct PostingWidths {
    acct: usize,
    left: usize,
    right: usize,
    operator: usize,
    asn_left: usize,
}

/// The columns of the main amounts' decimal marks, the assertions' `=`, and
/// the assertion amounts' decimal marks (AmountCols).
#[derive(Clone, Copy, Debug, Default)]
struct AmountCols {
    dec_mark: usize,
    equals: usize,
    asn_dec_mark: usize,
}

/// The decimal-mark-aligned layout (postingsAsLinesDecimal): decimal marks
/// at the target column, or further right to leave two spaces after the
/// widest account name.
fn postings_as_lines_decimal(
    basefmt: &AmountFormat,
    one_line_amounts: bool,
    target: usize,
    ps: &[Posting],
) -> Vec<String> {
    let fmt = display_format(basefmt, one_line_amounts);
    let rendered: Vec<PostingRender> = ps.iter().map(|p| PostingRender::new(&fmt, p)).collect();
    let status_width = ps.iter().map(posting_status_width).max().unwrap_or(0);
    let max =
        |f: fn(&PostingWidths) -> usize| rendered.iter().map(|r| f(&r.widths)).max().unwrap_or(0);
    let (acct, left, right, op, asn_left) = (
        max(|w| w.acct),
        max(|w| w.left),
        max(|w| w.right),
        max(|w| w.operator),
        max(|w| w.asn_left),
    );
    let dec_mark = target.max(acct + status_width + 7 + left);
    let has_assertion = op > 0;
    let equals = if has_assertion {
        dec_mark + right + 1
    } else {
        0
    };
    let asn_dec_mark = if has_assertion {
        equals + op + 1 + asn_left
    } else {
        0
    };
    let cols = AmountCols {
        dec_mark,
        equals,
        asn_dec_mark,
    };
    ps.iter()
        .zip(&rendered)
        .flat_map(|(p, r)| r.lines(&fmt, p, status_width, acct, cols))
        .collect()
}

/// One posting's lines on its own, with these status and account widths
/// and its amount's decimal mark at this column (postingAsLines).
pub fn posting_as_lines(
    basefmt: &AmountFormat,
    one_line_amounts: bool,
    status_width: usize,
    acct_width: usize,
    dec_mark: usize,
    p: &Posting,
) -> Vec<String> {
    let fmt = display_format(basefmt, one_line_amounts);
    let cols = AmountCols {
        dec_mark,
        equals: 0,
        asn_dec_mark: 0,
    };
    PostingRender::new(&fmt, p).lines(&fmt, p, status_width, acct_width, cols)
}

/// The amount format postings are shown with: zeros with their commodity,
/// a decimal mark after digit groups, and no padding of their own.
fn display_format(basefmt: &AmountFormat, one_line_amounts: bool) -> AmountFormat {
    AmountFormat {
        zero_commodity: true,
        force_decimal_mark: true,
        one_line: one_line_amounts,
        min_width: None,
        ..basefmt.clone()
    }
}

/// A posting's status mark and space, if it has one (postingStatusWidth).
fn posting_status_width(p: &Posting) -> usize {
    if p.status == Status::Unmarked { 0 } else { 2 }
}

/// A posting's rendered amounts and the widths its siblings align with.
struct PostingRender {
    /// Each amount line, and its width before the decimal mark.
    rows: Vec<(Wide, usize)>,
    widths: PostingWidths,
}

impl PostingRender {
    fn new(fmt: &AmountFormat, p: &Posting) -> PostingRender {
        let mut rows: Vec<(Wide, usize)> = show_mixed_amount_lines_parts_b(fmt, &p.amount)
            .into_iter()
            .map(|(w, a)| {
                let lw = amount_integer_width(fmt, &a);
                (w, lw)
            })
            .collect();
        if rows.is_empty() {
            rows.push((Wide::default(), 0));
        }
        let left = rows.iter().map(|(_, lw)| *lw).max().unwrap_or(0);
        let right = rows
            .iter()
            .map(|(w, lw)| w.width.saturating_sub(*lw))
            .max()
            .unwrap_or(0);
        let (operator, asn_left) = match &p.assertion {
            None => (0, 0),
            Some(ba) => (
                balance_assertion_operator(ba).len(),
                amount_integer_width(fmt, &ba.amount),
            ),
        };
        let widths = PostingWidths {
            acct: display_width(&show_account_name(p)),
            left,
            right,
            operator,
            asn_left,
        };
        PostingRender { rows, widths }
    }

    /// The posting's lines (postingAsLines).
    fn lines(
        &self,
        fmt: &AmountFormat,
        p: &Posting,
        status_width: usize,
        acct_width: usize,
        cols: AmountCols,
    ) -> Vec<String> {
        let status_and_account = format!(
            "    {}",
            fit_text(
                Some((status_width + acct_width) as isize),
                None,
                false,
                true,
                &status_and_account(p)
            )
        );
        let assertion = p.assertion.as_ref().map(|ba| {
            let pad1 = cols
                .equals
                .saturating_sub(cols.dec_mark + self.widths.right)
                .max(1);
            let pad2 = cols
                .asn_dec_mark
                .saturating_sub(cols.equals + self.widths.operator + self.widths.asn_left)
                .max(1);
            format!(
                "{}{}{}{}",
                " ".repeat(pad1),
                balance_assertion_operator(ba),
                " ".repeat(pad2),
                show_amount_b(fmt, &ba.amount).s
            )
        });
        let comments = render_comment_lines(&p.comment);
        let (same_line, next_lines) = match comments.split_first() {
            None => ("", &[][..]),
            Some((c, cs)) => (c.as_str(), cs),
        };
        let n = self.rows.len();
        let mut out = Vec::new();
        for (i, (amt, lw)) in self.rows.iter().enumerate() {
            let pad = cols
                .dec_mark
                .saturating_sub(acct_width + status_width + 7 + lw);
            let mut line = format!("{status_and_account}  {}{}", " ".repeat(pad), amt.s);
            if i + 1 == n
                && let Some(a) = &assertion
            {
                line.push_str(a);
            }
            line.push_str(same_line);
            push_lines(&mut out, &line);
            out.extend(next_lines.iter().cloned());
        }
        out
    }
}

/// The lines of some text, each without trailing spaces.
fn push_lines(out: &mut Vec<String>, s: &str) {
    for l in s.lines() {
        out.push(l.trim_end().to_string());
    }
}

/// The hledger 1 layout: amounts right-aligned in a field of at least 12
/// columns (postingsAsLinesHledger1).
fn postings_as_lines_hledger1(
    basefmt: &AmountFormat,
    one_line_amounts: bool,
    ps: &[Posting],
) -> Vec<String> {
    let fmt = AmountFormat {
        zero_commodity: true,
        force_decimal_mark: true,
        one_line: one_line_amounts,
        ..basefmt.clone()
    };
    let shown: Vec<Vec<Wide>> = ps
        .iter()
        .map(|p| {
            let lines = show_mixed_amount_lines_b(&fmt, &p.amount);
            if lines.is_empty() {
                vec![Wide::default()]
            } else {
                lines
            }
        })
        .collect();
    let acct_width = ps
        .iter()
        .map(|p| display_width(&show_account_name(p)))
        .max()
        .unwrap_or(0);
    let amt_width = shown
        .iter()
        .flat_map(|ls| ls.iter().map(|l| l.width))
        .max()
        .unwrap_or(0);
    let mut out = Vec::new();
    for (p, amounts) in ps.iter().zip(&shown) {
        let status_and_account = format!(
            "    {}",
            fit_text(
                Some(2 + acct_width as isize),
                None,
                false,
                true,
                &status_and_account(p)
            )
        );
        let assertion = p
            .assertion
            .as_ref()
            .map(|ba| format!(" {}", show_balance_assertion(ba)));
        let comments = render_comment_lines(&p.comment);
        let (same_line, next_lines) = match comments.split_first() {
            None => ("", &[][..]),
            Some((c, cs)) => (c.as_str(), cs),
        };
        for (i, amt) in amounts.iter().enumerate() {
            let pad = amt_width.max(12) - amt.width;
            let mut line = format!("{status_and_account}  {}{}", " ".repeat(pad), amt.s);
            if i + 1 == amounts.len()
                && let Some(a) = &assertion
            {
                line.push_str(a);
            }
            line.push_str(same_line);
            push_lines(&mut out, &line);
            out.extend(next_lines.iter().cloned());
        }
    }
    out
}

/// A balance assertion's operator: `=`, `==`, `=*` or `==*`
/// (balanceAssertionOperatorB).
fn balance_assertion_operator(ba: &BalanceAssertion) -> &'static str {
    match (ba.total, ba.inclusive) {
        (false, false) => "=",
        (true, false) => "==",
        (false, true) => "=*",
        (true, true) => "==*",
    }
}

/// A balance assertion as its operator, a space and its amount
/// (showBalanceAssertion).
fn show_balance_assertion(ba: &BalanceAssertion) -> String {
    let fmt = AmountFormat {
        zero_commodity: true,
        force_decimal_mark: true,
        ..AmountFormat::DEFAULT
    };
    format!(
        "{} {}",
        balance_assertion_operator(ba),
        show_amount_b(&fmt, &ba.amount).s
    )
}

/// A posting's account name, in brackets or parentheses if it is virtual,
/// even if it is already (showAccountName).
pub fn show_account_name(p: &Posting) -> String {
    match p.realness {
        Realness::Real => p.account.to_string(),
        Realness::Virtual => format!("({})", p.account),
        Realness::BalancedVirtual => format!("[{}]", p.account),
    }
}

/// A posting's status mark and a space, if it has one, and its account
/// name.
fn status_and_account(p: &Posting) -> String {
    match p.status {
        Status::Unmarked => show_account_name(p),
        s => format!("{s} {}", show_account_name(p)),
    }
}

/// The width of a rendered amount up to its decimal mark, real or implied:
/// a symbol on the left with its space, the sign, and the integer digits
/// with their group marks (amountIntegerWidth). The missing amount has none.
fn amount_integer_width(fmt: &AmountFormat, a: &Amount) -> usize {
    if a.is_missing() {
        return 0;
    }
    let truncated = truncate(&a.quantity);
    let shows_fraction = a.style.precision != Precision::Fixed(0);
    let sign_lost = a.quantity.is_negative() && truncated.is_zero() && shows_fraction;
    let int_amount = Amount {
        quantity: truncated,
        style: std::sync::Arc::new(crate::amount::AmountStyle {
            precision: Precision::Fixed(0),
            ..(*a.style).clone()
        }),
        ..a.clone()
    };
    let quantity_width = show_amount_quantity(false, &int_amount).width + usize::from(sign_lost);
    let prefix_width = if a.style.side == Side::L {
        let comm = if fmt.quotes {
            quote_commodity_symbol_if_needed(&a.commodity)
        } else {
            a.commodity.to_string()
        };
        display_width(&comm) + usize::from(!comm.is_empty() && a.style.spaced)
    } else {
        0
    };
    prefix_width + quantity_width
}

/// The integer part of a decimal, rounded toward zero.
fn truncate(q: &Decimal) -> Decimal {
    let scale = IBig::from(10u8).pow(usize::from(q.places()));
    Decimal::new(q.mantissa() / scale, 0)
}

/// How print --export renders a journal's items (ItemRenderer): a
/// transaction, without its trailing blank line; a directive's source text,
/// or nothing to drop it; a comment line; a comment block.
pub struct ItemRenderer<'a> {
    pub transaction: &'a dyn Fn(&Transaction) -> String,
    pub directive: &'a dyn Fn(&str) -> Option<String>,
}

/// A journal's items as text, with these transactions in place of the
/// transaction items they started at (and any others at the end), and
/// blank lines normalised: one between transactions, and between groups of
/// directive and comment lines (journalItemsAsText).
pub fn journal_items_as_text(
    r: &ItemRenderer,
    items: &[JournalItem],
    txns: &[Transaction],
) -> String {
    enum Block {
        Lines(String),
        Txn(String),
        Break,
    }
    let key = |p: &crate::model::SourcePos| (p.line, p.col, p.file.clone());
    let mut by_pos: std::collections::BTreeMap<_, std::collections::VecDeque<&Transaction>> =
        Default::default();
    for t in txns {
        by_pos.entry(key(&t.pos.0)).or_default().push_back(t);
    }
    let mut blocks = Vec::new();
    for item in items {
        match item {
            JournalItem::Transaction(pos) => {
                match by_pos.get_mut(&key(pos)).and_then(|q| q.pop_front()) {
                    Some(t) => blocks.push(Block::Txn((r.transaction)(t))),
                    None => blocks.push(Block::Break),
                }
            }
            JournalItem::Blank | JournalItem::Include(_) => blocks.push(Block::Break),
            JournalItem::Comment(t) | JournalItem::CommentBlock(t) => {
                blocks.push(Block::Lines(t.clone()))
            }
            JournalItem::Directive(t) => {
                if let Some(s) = (r.directive)(t) {
                    blocks.push(Block::Lines(s));
                }
            }
            JournalItem::NonExportedDirective(_) => {}
        }
    }
    for q in by_pos.into_values() {
        for t in q {
            blocks.push(Block::Txn((r.transaction)(t)));
        }
    }
    // merge adjacent groups of lines; a break keeps them apart, and is dropped
    let mut merged: Vec<Block> = Vec::new();
    let mut after_break = false;
    for b in blocks {
        match (merged.last_mut(), b) {
            (_, Block::Break) => after_break = true,
            (Some(Block::Lines(a)), Block::Lines(b)) if !after_break => a.push_str(&b),
            (_, b) => {
                merged.push(b);
                after_break = false;
            }
        }
    }
    // each block is followed by a blank line, except final comment or directive lines
    let n = merged.len();
    let mut out = String::new();
    for (i, b) in merged.into_iter().enumerate() {
        match b {
            Block::Lines(s) => {
                out.push_str(&s);
                if i + 1 < n {
                    out.push('\n');
                }
            }
            Block::Txn(s) => {
                out.push_str(&s);
                out.push('\n');
            }
            Block::Break => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amount::{MixedAmount, hrs, num, usd};
    use crate::dates::Day;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn post(account: &str, a: Amount) -> Posting {
        let mut p = Posting::new(account, a);
        let r = crate::account::account_name_posting_type(account);
        p.realness = r;
        p.account = crate::account::account_name_without_posting_type(account).into();
        p
    }

    fn missing(account: &str) -> Posting {
        Posting {
            amount: MixedAmount::missing(),
            ..post(account, Amount::null())
        }
    }

    fn lines(ps: &[Posting]) -> Vec<String> {
        postings_as_lines(&AmountFormat::DEFAULT, false, PostingLayout::default(), ps)
    }

    fn txn(date: Day, description: &str, postings: Vec<Posting>) -> Transaction {
        Transaction {
            date,
            description: description.to_string(),
            postings,
            ..Transaction::default()
        }
    }

    // tests_Transaction "postingsAsLines".
    #[test]
    fn posting_lines() {
        let one = || usd(d("1"));
        assert!(lines(&[]).is_empty());
        assert_eq!(
            lines(&[post("a", one()), missing("b")]),
            [
                "    a                                             $1.00",
                "    b"
            ]
        );
        assert_eq!(
            lines(&[post("a", one()), post("b", usd(d("-1")))]),
            [
                "    a                                             $1.00",
                "    b                                            $-1.00"
            ]
        );
        assert_eq!(
            lines(&[post("(a)", one())]),
            ["    (a)                                           $1.00"]
        );
        assert_eq!(
            lines(&[post("a", one()), post("b", hrs(d("-1")).at(one()))]),
            [
                "    a                                             $1.00",
                "    b                                             -1.00h @ $1.00"
            ]
        );
        assert_eq!(
            lines(&[post("a", one()), post("b", hrs(d("-1")))]),
            [
                "    a                                             $1.00",
                "    b                                             -1.00h"
            ]
        );
        assert_eq!(
            lines(&[post("a", one()), missing("b"), post("c", usd(d("-1")))]),
            [
                "    a                                             $1.00",
                "    b",
                "    c                                            $-1.00"
            ]
        );
    }

    // tests_Transaction "showTransaction".
    #[test]
    fn transactions() {
        let show = |t: &Transaction| show_transaction(t, PostingLayout::default(), false);
        assert_eq!(show(&Transaction::default()), "0000-01-01\n\n");
        let p = Posting {
            status: Status::Cleared,
            amount: MixedAmount::from_amounts([usd(d("1")), hrs(d("2"))]),
            comment: "\npcomment2\n".to_string(),
            ..post("a", Amount::null())
        };
        let t = Transaction {
            date2: Some(Day::from_ymd(2012, 5, 15).unwrap()),
            code: "code".to_string(),
            comment: "tcomment1\ntcomment2\n".to_string(),
            ..txn(Day::from_ymd(2012, 5, 14).unwrap(), "desc", vec![p])
        };
        assert_eq!(
            show(&t),
            concat!(
                "2012-05-14=2012-05-15 (code) desc  ; tcomment1\n",
                "    ; tcomment2\n",
                "    * a                                           $1.00\n",
                "    ; pcomment2\n",
                "    * a                                            2.00h\n",
                "    ; pcomment2\n",
                "\n"
            )
        );
        let day = Day::from_ymd(2007, 1, 28).unwrap();
        let t = txn(
            day,
            "coopportunity",
            vec![
                post("expenses:food:groceries", usd(d("47.18"))),
                post("assets:checking", usd(d("-47.19"))),
            ],
        );
        assert_eq!(
            show(&t),
            concat!(
                "2007-01-28 coopportunity\n",
                "    expenses:food:groceries                      $47.18\n",
                "    assets:checking                             $-47.19\n",
                "\n"
            )
        );
        let t = txn(
            day,
            "coopportunity",
            vec![missing("expenses:food:groceries")],
        );
        assert_eq!(
            show(&t),
            "2007-01-28 coopportunity\n    expenses:food:groceries\n\n"
        );
        let priced = num(d("1")).at(usd(d("2")).with_precision(Precision::Fixed(0)));
        let t = txn(
            Day::from_ymd(2010, 1, 1).unwrap(),
            "x",
            vec![post("a", priced), missing("b")],
        );
        assert_eq!(
            show(&t),
            "2010-01-01 x\n    a                                              1 @ $2\n    b\n\n"
        );
    }

    #[test]
    fn account_names_are_bracketed_as_they_are() {
        let with = |account: &str, realness| Posting {
            account: account.into(),
            realness,
            ..Posting::default()
        };
        assert_eq!(show_account_name(&with("(a)", Realness::Real)), "(a)");
        assert_eq!(show_account_name(&with("(a)", Realness::Virtual)), "((a))");
        assert_eq!(
            show_account_name(&with("a", Realness::BalancedVirtual)),
            "[a]"
        );
    }
}
