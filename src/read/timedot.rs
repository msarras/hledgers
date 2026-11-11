//! The timedot reader (Hledger.Read.TimedotReader): a day's date line,
//! then a line per account with the time spent on it, as dots (a quarter
//! hour each), a number of hours or other units, or letters (a quarter hour
//! each, tagged with the letter); each day becomes a transaction with a
//! virtual posting per line. Comment lines start with `#` or `;`; lines
//! before the first date, and date lines, may be org headings.
//!
//! The parser is hand-written over a [`Cursor`]; its errors list what
//! megaparsec would have expected, so that they read as upstream's.

use std::sync::Arc;

use super::InputOpts;
use super::amount::numberp;
use super::common::{datep, digit_hint, noncomment_textp, source_pos, transaction_commentp};
use super::journal::{Failure, ParseState, R, modified_account_namep};
use crate::account::Realness;
use crate::amount::{Amount, AmountStyle, MixedAmount, Precision, Sym};
use crate::dates::{Day, is_date_sep_char};
use crate::decimal::Decimal;
use crate::model::{
    Journal, Posting, SourcePos, Status, Tag, Transaction, comment_add_tag_unspaced,
};
use crate::parse::{Cursor, Item, PResult, ParseError, is_non_newline_space};
use crate::text::is_space;

/// A timedot file's text read into a finalised journal, the --alias
/// options applied after the transactions are made (TimedotReader's parse).
pub fn read_timedot(iopts: &InputOpts, f: &str, text: &str) -> Result<Journal, String> {
    let st = ParseState {
        year: Some(iopts.today.year()),
        ..ParseState::default()
    };
    let txns = timedot_transactions(&st, f, text).map_err(|e| e.message(f, text))?;
    let aliases = super::journal::aliases_from_opts(iopts)?;
    let j = super::csv::journal_apply_aliases(
        &aliases,
        Journal {
            txns,
            ..Journal::default()
        },
    )?;
    super::finalise::journal_finalise(iopts, f, text, j, st)
}

/// A timedot file's days, read with this parse state, as transactions
/// (timedotp): a preamble of comment lines, then days, to the end.
pub(super) fn timedot_transactions(st: &ParseState, file: &str, text: &str) -> R<Vec<Transaction>> {
    let mut p = TimedotParser {
        c: Cursor::new(text),
        st,
        file: Arc::from(file),
        counted: (0, 1),
    };
    p.preamblep()?;
    let mut txns = Vec::new();
    while let Some(t) = p.dayp()? {
        txns.push(t);
    }
    if p.c.at_end() {
        return Ok(txns);
    }
    Err(p
        .c
        .unexpected(&[
            Item::label(&comment_line_label("#;*")),
            Item::EndOfInput,
            Item::label("timedot day entry"),
        ])
        .into())
}

struct TimedotParser<'a, 's> {
    c: Cursor<'a>,
    st: &'s ParseState,
    file: Arc<str>,
    /// An offset whose line number is known.
    counted: (usize, usize),
}

/// What emptyorcommentlinep2 is labelled with.
fn comment_line_label(starts: &str) -> String {
    format!("empty line or comment line beginning with {starts}")
}

impl TimedotParser<'_, '_> {
    /// The source position of the cursor.
    fn pos(&mut self) -> SourcePos {
        source_pos(&self.c, &self.file, &mut self.counted)
    }

    /// Empty lines and comment lines (`#`, `;` or `*`) until a date line
    /// (preamblep).
    fn preamblep(&mut self) -> R<()> {
        loop {
            if self.date_line_attempt() {
                return Ok(());
            }
            let start = self.c.pos;
            match empty_or_comment_line2(&mut self.c, "#;*") {
                Ok(()) => {}
                Err(e) if self.c.pos != start => return Err(e.into()),
                Err(_) => return Ok(()),
            }
        }
    }

    /// Does a date line, or something that looks like one (three runs of
    /// digits separated by date separators, maybe in an org heading),
    /// start here? (datelineattemptp)
    fn date_line_attempt(&self) -> bool {
        let mut c = self.c;
        let save = c;
        if c.take_while(|ch| ch == '*').is_empty() || !c.skip_spaces() {
            c = save;
        }
        let digits = |c: &mut Cursor| !c.take_while(|ch| ch.is_ascii_digit()).is_empty();
        let looks_like_date = digits(&mut c)
            && c.bump().is_some_and(is_date_sep_char)
            && digits(&mut c)
            && c.bump().is_some_and(is_date_sep_char)
            && digits(&mut c);
        looks_like_date || datelinep(&mut self.c.clone(), self.st).is_ok()
    }

    /// A day: its date line, comment lines, and entries, as a transaction;
    /// or none if no date line starts here (dayp).
    fn dayp(&mut self) -> R<Option<Transaction>> {
        let start = self.c.pos;
        let pos = self.pos();
        let (date, description, comment, tags) = match datelinep(&mut self.c, self.st) {
            Ok(d) => d,
            Err(Failure::Here(_)) if self.c.pos == start => return Ok(None),
            Err(e) => return Err(e),
        };
        self.commentlinesp();
        let mut postings = Vec::new();
        while let Some(ps) = self.entryp()? {
            postings.extend(ps);
            self.commentlinesp();
        }
        let end = self.pos();
        Ok(Some(Transaction {
            pos: (pos, end),
            date,
            status: Status::Cleared,
            description,
            comment,
            tags,
            postings,
            ..Transaction::default()
        }))
    }

    /// Empty lines and `#` or `;` comment lines (commentlinesp).
    fn commentlinesp(&mut self) {
        loop {
            let save = self.c;
            match empty_or_comment_line2(&mut self.c, "#;") {
                Ok(()) if self.c.pos > save.pos => {}
                _ => {
                    self.c = save;
                    return;
                }
            }
        }
    }

    /// An account and the time spent on it, as postings: one, or one per
    /// letter of letter "dots"; or none if a date line or the end of the
    /// input is here (timedotentryp). An org heading prefix is ignored.
    fn entryp(&mut self) -> R<Option<Vec<Posting>>> {
        if self.date_line_attempt() {
            return Ok(None);
        }
        let start = self.c.pos;
        if self.c.peek() == Some('*') {
            org_heading_prefixp(&mut self.c)?;
        } else {
            self.c.skip_spaces();
        }
        let account = match modified_account_namep(&mut self.c, self.st, false) {
            Ok(a) => a,
            Err(Failure::Here(_)) if self.c.pos == start => return Ok(None),
            Err(e) => return Err(e),
        };
        self.c.skip_spaces();
        let (durations, hints) = durationsp(&mut self.c);
        let (comment, tags) = transaction_commentp(&mut self.c, &hints)?;
        let (commodity, style) = match &self.st.default_commodity {
            Some((c, s)) => (
                *c,
                AmountStyle {
                    precision: s.precision.max(Precision::Fixed(2)),
                    ..*s
                },
            ),
            None => (
                Sym::from(""),
                AmountStyle {
                    precision: Precision::Fixed(2),
                    ..AmountStyle::default()
                },
            ),
        };
        let postings = durations
            .into_iter()
            .map(|(hours, letter)| {
                let tag: Tag = ("t".to_string(), letter.clone());
                let (comment, tags) = if letter.is_empty() {
                    (comment.clone(), tags.clone())
                } else {
                    let mut tags = tags.clone();
                    tags.push(tag.clone());
                    (comment_add_tag_unspaced(&comment, &tag), tags)
                };
                Posting {
                    account: account.as_str().into(),
                    amount: MixedAmount::from_amount(Amount {
                        commodity,
                        quantity: hours,
                        style,
                        ..Amount::null()
                    }),
                    realness: Realness::Virtual,
                    comment,
                    tags,
                    ..Posting::default()
                }
            })
            .collect();
        Ok(Some(postings))
    }
}

/// A date line: maybe an org heading prefix, a date, a description and a
/// comment (datelinep).
fn datelinep(c: &mut Cursor, st: &ParseState) -> R<(Day, String, String, Vec<Tag>)> {
    if c.peek() == Some('*') {
        org_heading_prefixp(c)?;
    }
    let date = datep(c, st.year)?;
    let description = noncomment_textp(c).trim_matches(is_space).to_string();
    let (comment, tags) = transaction_commentp(c, &[])?;
    Ok((date, description, comment, tags))
}

/// One or more stars and one or more spaces (orgheadingprefixp).
fn org_heading_prefixp(c: &mut Cursor) -> PResult<()> {
    c.take_while(|ch| ch == '*');
    if c.skip_spaces() {
        Ok(())
    } else {
        Err(c.unexpected(&[Item::tokens("*")]))
    }
}

/// A line that is empty, all spaces, or whose first non-space character
/// is one of `starts` (emptyorcommentlinep2). The newline is optional at
/// the end of the input after a comment.
fn empty_or_comment_line2(c: &mut Cursor, starts: &str) -> PResult<()> {
    let start = c.pos;
    c.skip_spaces();
    if c.eat('\n') {
        return Ok(());
    }
    if c.peek().is_some_and(|ch| starts.contains(ch)) {
        c.take_while(|ch| ch != '\n');
        c.eat('\n');
        return Ok(());
    }
    if c.pos == start {
        return Err(c.unexpected(&[Item::label(&comment_line_label(starts))]));
    }
    let mut expected: Vec<Item> = starts
        .chars()
        .map(|ch| Item::tokens(&ch.to_string()))
        .collect();
    expected.push(Item::label("newline"));
    Err(c.unexpected(&expected))
}

/// Time units, and their conversions to hours: a month is 30 days, a year
/// 365 (timeUnits). "mo" is tried before "m".
const TIME_UNITS: [(&str, Conversion); 7] = [
    ("s", Conversion::Div(3600)),
    ("mo", Conversion::Mul(720)),
    ("m", Conversion::Div(60)),
    ("h", Conversion::Mul(1)),
    ("d", Conversion::Mul(24)),
    ("w", Conversion::Mul(168)),
    ("y", Conversion::Mul(8760)),
];

#[derive(Clone, Copy)]
enum Conversion {
    Mul(i64),
    Div(i64),
}

/// Durations in hours, each with the letter it was written as, if any
/// (durationsp): a number with an optional unit, dots, letters, or nothing
/// (zero). Also what else could have followed them, for an error after
/// them.
fn durationsp(c: &mut Cursor) -> (Vec<(Decimal, String)>, Vec<Item>) {
    let start = *c;
    let number_error = match numberp(c, None) {
        Ok((q, ..)) => {
            let mut hints: Vec<Item> = digit_hint(c).into_iter().collect();
            let q = match TIME_UNITS.iter().find(|(u, _)| c.rest().starts_with(u)) {
                Some((u, conv)) => {
                    c.pos += u.len();
                    hints.clear();
                    match conv {
                        Conversion::Mul(1) => q,
                        Conversion::Mul(n) => &q * &Decimal::from_i64(*n),
                        Conversion::Div(n) => &q / &Decimal::from_i64(*n),
                    }
                }
                None => {
                    hints.extend(TIME_UNITS.iter().map(|(u, _)| Item::tokens(u)));
                    q
                }
            };
            if c.skip_spaces() {
                hints.clear();
            }
            return (vec![(q, String::new())], hints);
        }
        Err(e) => {
            let consumed = c.pos != start.pos;
            *c = start;
            (consumed, matches!(e, ParseError::Fancy { .. }))
        }
    };
    let quarters = |n: usize| &Decimal::from_i64(n as i64) / &Decimal::from_i64(4);
    if c.eat('.') {
        let rest = c.take_while(|ch| ch == '.' || ch == ' ');
        let dots = 1 + rest.chars().filter(|&ch| ch == '.').count();
        return (vec![(quarters(dots), String::new())], Vec::new());
    }
    if c.peek().is_some_and(char::is_alphabetic) {
        let text = c.take_while(|ch| ch.is_alphabetic() || is_non_newline_space(ch));
        let mut letters: Vec<char> = text.chars().filter(|&ch| !is_space(ch)).collect();
        letters.sort_unstable();
        let mut groups: Vec<(Decimal, String)> = Vec::new();
        let mut i = 0;
        while i < letters.len() {
            let n = letters[i..]
                .iter()
                .take_while(|&&l| l == letters[i])
                .count();
            groups.push((quarters(n), letters[i].to_string()));
            i += n;
        }
        return (groups, vec![Item::label("letter")]);
    }
    let hints = match number_error {
        (_, true) => Vec::new(),
        (false, _) => vec![
            Item::label("number"),
            Item::tokens("."),
            Item::label("letter"),
        ],
        (true, _) => vec![Item::tokens("."), Item::label("letter")],
    };
    (vec![(Decimal::ZERO, String::new())], hints)
}
