//! Error excerpts (Hledger.Data.Errors): a transaction, posting or
//! directive rendered as journal text in the style of a megaparsec error,
//! with the line number of the problem and optionally a marker under the
//! problem's columns. The columns are those of the rendered text, which is
//! regenerated, not the original.

use crate::amount::render::{AmountFormat, quote_commodity_symbol_if_needed, show_amount_b};
use crate::model::{AccountDeclarationInfo, Commodity, Posting, PriceDirective, Transaction};
use crate::text::chomp;
use crate::write::journal::{PostingLayout, show_transaction};

/// The first and (if known) last column to mark.
pub type Columns = Option<(usize, Option<usize>)>;

/// An excerpt: the file, the line number shown, and the decorated text.
pub type Excerpt = (String, usize, String);

/// Text with megaparsec's left margin: the line number before the first
/// line, then a marker line under the columns if known, then the other
/// lines (decorateExcerpt, decorateTransactionErrorExcerpt).
pub fn decorate_excerpt(line: usize, cols: Columns, txt: &str) -> String {
    let lines: Vec<&str> = txt.split_terminator('\n').collect();
    let prefix = format!("{}| ", " ".repeat(line.to_string().len() + 1));
    let mut out = String::new();
    if let Some(first) = lines.first() {
        out.push_str(&format!("{line} | {first}\n"));
    }
    if let Some((col, end)) = cols {
        // upstream's region width is 2 when the end is unknown
        let width = end.map_or(1, |e| e as isize - col as isize) + 1;
        out.push_str(&marker_line(&prefix, col, width));
    }
    for l in lines.iter().skip(1) {
        out.push_str(&format!("{prefix}{l}\n"));
    }
    out
}

/// A transaction as journal text, without the comment lines before it,
/// ending with one newline (showTransactionForExcerpt).
fn transaction_text(t: &Transaction) -> String {
    let s = show_transaction(t, PostingLayout::default(), false);
    let s = s.strip_prefix(t.preceding_comment.as_str()).unwrap_or(&s);
    format!("{}\n", chomp(s))
}

/// A transaction as an excerpt, numbered at its first line, with these
/// columns marked (makeTransactionErrorExcerpt).
pub fn transaction_error_excerpt(t: &Transaction, cols: Columns) -> Excerpt {
    let line = t.pos.0.line;
    let ex = decorate_excerpt(line, cols, &transaction_text(t));
    (t.pos.0.file.to_string(), line, ex)
}

/// The lines a comment adds to a rendering, after the first.
fn comment_extra_lines(c: &str) -> usize {
    c.split_terminator('\n').count().saturating_sub(1)
}

/// The posting's transaction as an excerpt, numbered at the posting's line,
/// with the columns that `find_cols` finds in the rendering marked under it
/// (makePostingErrorExcerpt). The posting is the first one in the
/// transaction equal to it once costs are removed, the way balance
/// assertions see postings; if there is none, the excerpt is numbered at
/// the first line. Multi-commodity amounts, which render on several lines,
/// are not allowed for.
pub fn posting_error_excerpt(
    t: &Transaction,
    p: &Posting,
    find_cols: impl FnOnce(&Posting, &Transaction, &str) -> Columns,
) -> Excerpt {
    let index = t.postings.iter().position(|q| {
        let mut q = q.clone();
        q.amount = std::mem::take(&mut q.amount).strip_costs();
        q == *p
    });
    let rel = match index {
        None => 0,
        Some(i) => {
            comment_extra_lines(&t.comment)
                + t.postings[..=i]
                    .iter()
                    .map(|q| 1 + comment_extra_lines(&q.comment))
                    .sum::<usize>()
        }
    };
    let line = clamp_to_transaction_lines(t, t.pos.0.line + rel);
    let txt = transaction_text(t);
    let cols = find_cols(p, t, &txt);
    let ex = decorate_posting_excerpt(line, rel, cols, &txt);
    (t.pos.0.file.to_string(), line, ex)
}

/// The transaction as an excerpt, numbered at the line of the posting
/// with this index, with these columns marked under it
/// (makePostingErrorExcerptByIndex).
pub fn posting_error_excerpt_by_index(t: &Transaction, idx: usize, cols: Columns) -> Excerpt {
    let rel = comment_extra_lines(&t.comment)
        + t.postings
            .iter()
            .take(idx + 1)
            .map(|q| 1 + comment_extra_lines(&q.comment))
            .sum::<usize>();
    let line = clamp_to_transaction_lines(t, t.pos.0.line + rel);
    let ex = decorate_posting_excerpt(line, rel, cols, &transaction_text(t));
    (t.pos.0.file.to_string(), line, ex)
}

/// The rendered transaction with the margin, the line number on the line
/// `rel` lines after the first, and a marker line under it
/// (decoratePostingErrorExcerpt).
fn decorate_posting_excerpt(line: usize, rel: usize, cols: Columns, txt: &str) -> String {
    let lines: Vec<&str> = txt.split_terminator('\n').collect();
    let (before, after) = lines.split_at((rel + 1).min(lines.len()));
    let prefix = format!("{}| ", " ".repeat(line.to_string().len() + 1));
    let mut out = String::new();
    if let Some((last, others)) = before.split_last() {
        for l in others {
            out.push_str(&format!("{prefix}{l}\n"));
        }
        out.push_str(&format!("{line} | {last}\n"));
    }
    if let Some((col, end)) = cols {
        let width = 1 + end.map_or(0, |e| e as isize - col as isize);
        out.push_str(&marker_line(&prefix, col, width));
    }
    for l in after {
        out.push_str(&format!("{prefix}{l}\n"));
    }
    out
}

/// The margin, then carets from a column, as many as `width` says.
fn marker_line(prefix: &str, col: usize, width: isize) -> String {
    format!(
        "{prefix}{}{}\n",
        " ".repeat(col.saturating_sub(1)),
        "^".repeat(width.max(0) as usize)
    )
}

/// A line number kept within the transaction's source lines, which a
/// rendering can outnumber (clampToTransactionLines).
fn clamp_to_transaction_lines(t: &Transaction, line: usize) -> usize {
    let (start, end) = &t.pos;
    let first = start.line;
    let last = first.max(end.line.saturating_sub(usize::from(end.col == 1)));
    line.min(last).max(first)
}

/// The posting's transaction as an excerpt with the posting's account name
/// marked (makePostingAccountErrorExcerpt).
pub fn posting_account_error_excerpt(t: &Transaction, p: &Posting) -> Excerpt {
    posting_error_excerpt(t, p, |p, _, _| {
        let col = 5 + usize::from(p.is_virtual());
        Some((col, Some(col + p.account.chars().count() - 1)))
    })
}

/// The posting's transaction as an excerpt with the posting's balance
/// assertion marked, or if the posting is not found as it is, the whole
/// posting (makeBalanceAssertionErrorExcerpt).
pub fn balance_assertion_error_excerpt(t: &Transaction, p: &Posting) -> Excerpt {
    posting_error_excerpt(t, p, |p, t, rendered| {
        let lines: Vec<&str> = rendered.split_terminator('\n').collect();
        let widest = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
        let fallback = (5, widest);
        let found = t.postings.iter().position(|q| q == p).and_then(|i| {
            let tlines = comment_line_count(&t.comment);
            let before: usize = t.postings[..i]
                .iter()
                .map(|q| comment_line_count(&q.comment))
                .sum();
            let line = lines.get(tlines + before)?;
            let col2 = line.chars().count();
            let mut rev = line.chars().rev().skip_while(|&c| c != '=').peekable();
            while rev.next_if(|&c| c == '=' || c == '*').is_some() {}
            Some((rev.count() + 1, col2))
        });
        let (col, col2) = found.unwrap_or(fallback);
        Some((col, Some(col2)))
    })
}

/// The lines a comment renders as, at least one.
fn comment_line_count(c: &str) -> usize {
    c.split_terminator('\n').count().max(1)
}

/// A directive's first line with its comment: the comment's first line
/// after `sep`, and any other lines indented below
/// (showDirectiveWithComment).
fn show_directive_with_comment(directive: &str, sep: &str, comment: &str) -> String {
    let mut lines = comment.split_terminator('\n');
    match lines.next() {
        None => directive.to_string(),
        Some(first) => {
            let mut out = if first.is_empty() {
                directive.to_string()
            } else {
                format!("{directive}{sep}{first}")
            };
            for l in lines {
                out.push_str("\n    ; ");
                out.push_str(l);
            }
            out
        }
    }
}

/// An account directive as an excerpt (makeAccountTagErrorExcerpt).
pub fn account_tag_error_excerpt(a: &str, info: &AccountDeclarationInfo) -> Excerpt {
    let line = info.pos.line;
    let txt = show_directive_with_comment(&format!("account {a}"), "    ; ", &info.comment);
    let ex = decorate_excerpt(line, None, &format!("{}\n", chomp(&txt)));
    (info.pos.file.to_string(), line, ex)
}

/// A commodity directive as an excerpt (makeCommodityTagErrorExcerpt).
pub fn commodity_tag_error_excerpt(c: &Commodity) -> Excerpt {
    let line = c.pos.line;
    let txt = show_directive_with_comment(&format!("commodity {}", c.symbol), "  ; ", &c.comment);
    let ex = decorate_excerpt(line, None, &format!("{}\n", chomp(&txt)));
    (c.pos.file.to_string(), line, ex)
}

/// A P directive as an excerpt (makePriceDirectiveErrorExcerpt).
pub fn price_directive_error_excerpt(pd: &PriceDirective) -> Excerpt {
    let line = pd.pos.line;
    let ex = decorate_excerpt(line, None, &format!("{}\n", show_price_directive(pd)));
    (pd.pos.file.to_string(), line, ex)
}

/// A P directive as journal text (showPriceDirective).
pub fn show_price_directive(pd: &PriceDirective) -> String {
    format!(
        "P {} {} {}",
        pd.date,
        quote_commodity_symbol_if_needed(&pd.commodity),
        show_amount_b(&AmountFormat::FULL_ZERO, &pd.amount).s
    )
}

/// The start of an error message about a CSV record: its file and lines
/// (the end line being the start of the line after it), the first line
/// number and the record's fields joined by commas, and a blank line
/// (csvRecordErrPrefix).
pub fn csv_record_error_prefix(file: &str, l1: usize, lend: usize, record_text: &str) -> String {
    let l2 = lend.saturating_sub(1);
    let last = if l2 > l1 {
        format!("-{l2}")
    } else {
        String::new()
    };
    format!("{file}:{l1}{last}:\n{l1} | {record_text}\n\n")
}

/// An error message with the transaction's position and text after it
/// (annotateErrorWithTransaction).
pub fn annotate_error_with_transaction(t: &Transaction, msg: &str) -> String {
    format!(
        "{}\n{msg}\n{}\n",
        crate::model::source_pos_pair_pretty(&t.pos),
        show_transaction(t, PostingLayout::default(), false)
            .trim_end_matches(crate::text::is_space)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decorations() {
        assert_eq!(decorate_excerpt(5, None, "a\nb\n"), "5 | a\n  | b\n");
        assert_eq!(
            decorate_excerpt(10, Some((2, Some(3))), "abc\n"),
            "10 | abc\n   |  ^^\n"
        );
        assert_eq!(
            decorate_excerpt(1, Some((2, None)), "abc\n"),
            "1 | abc\n  |  ^^\n"
        );
        assert_eq!(
            decorate_posting_excerpt(4, 1, Some((3, Some(5))), "t\np\nq\n"),
            "  | t\n4 | p\n  |   ^^^\n  | q\n"
        );
        assert_eq!(
            show_directive_with_comment("account a", "    ; ", "x\ny"),
            "account a    ; x\n    ; y"
        );
    }
}
