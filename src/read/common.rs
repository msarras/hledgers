//! Parsers shared by the journal reader and the others (the lexical parsers of
//! Hledger.Read.Common): dates and times, status marks, codes, descriptions,
//! account names, comments with their tags and posting dates, blank and
//! comment lines, and account aliases.
//!
//! These work on a [`Cursor`]. One that fails without consuming input leaves
//! the cursor where it started, so its caller can treat the part as absent;
//! errors are reported as megaparsec reports them where hledger's tests look.

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use crate::account::AccountAlias;
use crate::amount::AmountStyle;
use crate::dates::Day;
use crate::model::{Status, Tag};
use crate::parse::{Cursor, Item, PResult, ParseError, is_non_newline_space};
use crate::regex::to_regex_ci;
use crate::text::is_space;

/// The account names, commodity symbols and amount styles read so far, each
/// kept once: a journal repeats a few of each many times.
#[derive(Debug, Default)]
pub struct Interned {
    names: HashSet<Arc<str>>,
    styles: BTreeSet<Arc<AmountStyle>>,
}

impl Interned {
    /// The shared copy of an account name or commodity symbol.
    pub fn name(&mut self, s: &str) -> Arc<str> {
        match self.names.get(s) {
            Some(name) => name.clone(),
            None => {
                let name: Arc<str> = Arc::from(s);
                self.names.insert(name.clone());
                name
            }
        }
    }

    /// The shared copy of an amount style.
    pub fn style(&mut self, style: AmountStyle) -> Arc<AmountStyle> {
        match self.styles.get(&style) {
            Some(s) => s.clone(),
            None => {
                let s = Arc::new(style);
                self.styles.insert(s.clone());
                s
            }
        }
    }
}

/// Whether a character can separate a date's parts (isDateSepChar).
pub fn is_date_sep_char(c: char) -> bool {
    matches!(c, '-' | '/' | '.')
}

/// A run of digits as a number, saturating rather than overflowing.
pub fn read_digits(s: &str) -> i64 {
    s.bytes().fold(0i64, |n, b| {
        n.saturating_mul(10).saturating_add(i64::from(b - b'0'))
    })
}

/// A date: YYYY-MM-DD, with `/` or `.` also allowed as separators and leading
/// zeros optional; or without the year, MM-DD, if a default year is known
/// (datep').
pub fn datep(c: &mut Cursor, year: Option<i64>) -> PResult<Day> {
    let start = c.pos;
    let d1 = c.take_while(|ch| ch.is_ascii_digit());
    if d1.is_empty() {
        return Err(c.unexpected(&[Item::label("full or partial date")]));
    }
    let Some(sep) = c.peek().filter(|&ch| is_date_sep_char(ch)) else {
        return Err(c.unexpected(&[Item::label("date separator"), Item::label("digit")]));
    };
    c.bump();
    let d2 = c.take_while(|ch| ch.is_ascii_digit());
    if d2.is_empty() {
        return Err(c.unexpected(&[Item::label("month or day")]));
    }
    let invalid = |c: &Cursor| {
        ParseError::at_region(
            c.src,
            start,
            c.pos,
            "This date has different separators, please use consistent separators.",
        )
    };
    let not_a_date = |c: &Cursor| {
        ParseError::at_region(
            c.src,
            start,
            c.pos,
            "This is not a valid date, please fix it.",
        )
    };
    if d1.len() >= 4 {
        let Some(sep2) = c.peek().filter(|&ch| is_date_sep_char(ch)) else {
            return Err(c.unexpected(&[Item::label("date separator"), Item::label("digit")]));
        };
        c.bump();
        let d3 = c.take_while(|ch| ch.is_ascii_digit());
        if d3.is_empty() {
            return Err(c.unexpected(&[Item::label("day")]));
        }
        if sep != sep2 {
            return Err(invalid(c));
        }
        valid_date(read_digits(d1), read_digits(d2), read_digits(d3)).ok_or_else(|| not_a_date(c))
    } else {
        match year {
            Some(y) => valid_date(y, read_digits(d1), read_digits(d2)).ok_or_else(|| not_a_date(c)),
            None => Err(ParseError::at_region(
                c.src,
                start,
                c.pos,
                "This partial date can not be parsed because the current year is unknown.\n\
                 Please make it a full date, or add a default year directive.",
            )),
        }
    }
}

/// The date with this year, month and day, if there is one (fromGregorianValid).
fn valid_date(y: i64, m: i64, d: i64) -> Option<Day> {
    Day::from_ymd(y, u32::try_from(m).ok()?, u32::try_from(d).ok()?)
}

/// A time of day, HH:MM or HH:MM:SS, and any time zone after it (+HHMM or
/// -HHMM), which is ignored (timeofdayp). Returns the seconds since
/// midnight.
pub fn timeofdayp(c: &mut Cursor) -> PResult<u32> {
    let two_digits = |c: &mut Cursor, what: &str, max: u32| -> PResult<u32> {
        let start = c.pos;
        let Some(d1) = c.peek().and_then(|ch| ch.to_digit(10)) else {
            return Err(c.unexpected(&[Item::label(what)]));
        };
        c.bump();
        let Some(d2) = c.peek().and_then(|ch| ch.to_digit(10)) else {
            return Err(c.unexpected(&[Item::label("a second digit")]));
        };
        c.bump();
        let n = d1 * 10 + d2;
        if n > max {
            let msg = format!("invalid time (bad {what})");
            return Err(ParseError::at_region(c.src, start, c.pos, &msg));
        }
        Ok(n)
    };
    let h = two_digits(c, "hour", 23)?;
    if !c.eat(':') {
        return Err(c.unexpected(&[Item::label("':' (hour-minute separator)")]));
    }
    let m = two_digits(c, "minute", 59)?;
    let mut s = 0;
    if c.eat(':') {
        s = two_digits(c, "second", 59)?;
    }
    if c.eat('+') || c.eat('-') {
        for _ in 0..4 {
            if !c.eat_digit() {
                return Err(c.unexpected(&[Item::label("a digit (for a time zone)")]));
            }
        }
    }
    Ok(h * 3600 + m * 60 + s)
}

/// A secondary date after `=`, taking its default year from the primary
/// date (secondarydatep).
pub fn secondary_datep(c: &mut Cursor, primary: Day) -> PResult<Day> {
    c.eat('=');
    datep(c, Some(primary.year()))
}

/// The next character after any spaces, and whether there were spaces
/// (peekAfterSpaces).
pub fn peek_after_spaces(c: &Cursor) -> (bool, Option<char>) {
    let rest = c.rest();
    let after = rest.trim_start_matches(is_non_newline_space);
    (after.len() < rest.len(), after.chars().next())
}

/// The hint megaparsec keeps after a run of digits that could have been
/// longer: "digit", if the text just before the cursor is a number's digits.
pub fn digit_hint(c: &Cursor) -> Option<Item> {
    let digit_before = c.pos > 0 && c.src.as_bytes()[c.pos - 1].is_ascii_digit();
    digit_before.then(|| Item::label("digit"))
}

/// A newline, or the end of the input (eolof). `hints` are what an error
/// here would also list.
pub fn eolof(c: &mut Cursor, hints: &[Item]) -> PResult<()> {
    if c.eat('\n') || c.at_end() {
        return Ok(());
    }
    let mut expected = vec![Item::label("newline"), Item::EndOfInput];
    expected.extend_from_slice(hints);
    Err(c.unexpected(&expected))
}

/// The rest of the line, and the newline (restofline).
pub fn rest_of_line<'a>(c: &mut Cursor<'a>) -> &'a str {
    let s = c.take_while(|ch| ch != '\n');
    c.eat('\n');
    s
}

/// A status mark after optional spaces: `*`, `!`, or none (statusp).
pub fn statusp(c: &mut Cursor) -> Status {
    match peek_after_spaces(c).1 {
        Some('*') => {
            c.skip_spaces();
            c.bump();
            Status::Cleared
        }
        Some('!') => {
            c.skip_spaces();
            c.bump();
            Status::Pending
        }
        _ => Status::Unmarked,
    }
}

/// A transaction code in parentheses after one or more spaces, or nothing
/// (codep).
pub fn codep<'a>(c: &mut Cursor<'a>) -> PResult<&'a str> {
    if peek_after_spaces(c) != (true, Some('(')) {
        return Ok("");
    }
    c.skip_spaces();
    c.bump();
    let code = c.take_while(|ch| ch != ')' && ch != '\n');
    if !c.eat(')') {
        return Err(c.unexpected(&[Item::label("closing bracket ')' for transaction code")]));
    }
    Ok(code)
}

/// Possibly empty text up to a semicolon or newline (noncommenttextp,
/// descriptionp).
pub fn noncomment_textp<'a>(c: &mut Cursor<'a>) -> &'a str {
    c.take_while(|ch| ch != ';' && ch != '\n')
}

/// An account name, without its brackets if any: one or more parts
/// separated by single spaces (a single tab or other space reads as a
/// space), ending at two spaces, a newline or the end of the input; and, if
/// `allow_semicolon` is false, at a semicolon (accountnamep,
/// accountnamenosemicolonp). Fails without consuming input if there is none.
pub fn accountnamep(c: &mut Cursor, allow_semicolon: bool) -> PResult<String> {
    let ok = |ch: char| !is_space(ch) && (allow_semicolon || ch != ';');
    let first = c.take_while(ok);
    if first.is_empty() {
        return Err(c.unexpected(&[]));
    }
    let mut name = String::from(first);
    loop {
        let mut chars = c.rest().chars();
        match (chars.next(), chars.next()) {
            (Some(s), Some(n)) if is_non_newline_space(s) && ok(n) => {
                c.bump();
                name.push(' ');
                name.push_str(c.take_while(ok));
            }
            _ => break,
        }
    }
    Ok(name)
}

/// Possibly empty text in double quotes, on one line (doublequotedtextp).
pub fn double_quoted_textp<'a>(c: &mut Cursor<'a>) -> PResult<&'a str> {
    if !c.eat('"') {
        return Err(c.unexpected(&[Item::tokens("\"")]));
    }
    let s = c.take_while(|ch| ch != '\n' && ch != '"');
    if !c.eat('"') {
        return Err(c.unexpected(&[Item::tokens("\"")]));
    }
    Ok(s)
}

/// Whether a character starts a comment line (isLineCommentStart).
pub fn is_line_comment_start(c: char) -> bool {
    matches!(c, '#' | '*' | ';')
}

/// A blank line, or a line whose first non-space character is `;`, `#` or
/// `*` (emptyorcommentlinep). The newline is optional at the end of the
/// input after a comment.
pub fn empty_or_comment_linep(c: &mut Cursor) -> PResult<()> {
    c.skip_spaces();
    if c.peek().is_some_and(is_line_comment_start) {
        rest_of_line(c);
        return Ok(());
    }
    if !c.eat('\n') {
        return Err(c.unexpected(&[Item::label("newline")]));
    }
    Ok(())
}

/// A `comment` ... `end comment` block, or to the end of the input
/// (multilinecommentp). Fails without consuming input if there is none.
pub fn multiline_commentp(c: &mut Cursor) -> PResult<()> {
    if !c.eat_str("comment") {
        return Err(c.unexpected(&[]));
    }
    let end_of_keyword_line = |c: &mut Cursor| -> PResult<()> {
        c.skip_spaces();
        if !c.eat('\n') {
            return Err(c.unexpected(&[Item::label("newline")]));
        }
        Ok(())
    };
    end_of_keyword_line(c)?;
    loop {
        if c.at_end() {
            return Ok(());
        }
        if c.eat_str("end comment") {
            return end_of_keyword_line(c);
        }
        c.take_while(|ch| ch != '\n');
        if !c.eat('\n') {
            return Err(c.unexpected(&[Item::label("newline")]));
        }
    }
}

/// A comment after a journal item: a same-line comment after `;`, and/or
/// following lines that start with indented `;`s; the text of each line,
/// stripped, followed by a newline, and what `content` makes of each
/// line's text (followingcommentpWith). If there are only following lines,
/// an empty same-line comment comes first, so that rendering keeps them on
/// following lines.
fn following_comment_with<T>(
    c: &mut Cursor,
    hints: &[Item],
    mut content: impl FnMut(&mut Cursor) -> PResult<T>,
) -> PResult<(String, Vec<T>)> {
    c.skip_spaces();
    let mut texts: Vec<&str> = Vec::new();
    let mut contents = Vec::new();
    if c.peek() == Some(';') {
        c.bump();
        c.skip_spaces();
        let start = c.pos;
        contents.push(content(c)?);
        texts.push(&c.src[start..c.pos]);
    }
    eolof(c, hints)?;
    while peek_after_spaces(c) == (true, Some(';')) {
        if texts.is_empty() {
            texts.push("");
        }
        c.skip_spaces();
        c.bump();
        c.skip_spaces();
        let start = c.pos;
        contents.push(content(c)?);
        texts.push(&c.src[start..c.pos]);
        eolof(c, &[])?;
    }
    let mut text = String::new();
    for t in texts {
        text.push_str(t.trim_matches(is_space));
        text.push('\n');
    }
    Ok((text, contents))
}

/// A following comment's text (followingcommentp).
pub fn following_commentp(c: &mut Cursor, hints: &[Item]) -> PResult<String> {
    following_comment_with(c, hints, |c| {
        c.take_while(|ch| ch != '\n');
        Ok(())
    })
    .map(|(text, _)| text)
}

/// A following comment's text and its tags (transactioncommentp).
pub fn transaction_commentp(c: &mut Cursor, hints: &[Item]) -> PResult<(String, Vec<Tag>)> {
    following_comment_with(c, hints, |c| {
        let line = c.take_while(|ch| ch != '\n');
        Ok(comment_line_tags(line))
    })
    .map(|(text, tags)| (text, tags.concat()))
}

/// The tags in a comment line's text: each a name (the word before a colon)
/// and a value up to the next comma or the end of the line, stripped
/// (commentlinetagsp).
pub fn comment_line_tags(mut s: &str) -> Vec<Tag> {
    let mut tags = Vec::new();
    while let Some(i) = s.find(':') {
        let name = s[..i].rsplit(is_space).next().unwrap_or("");
        s = &s[i + 1..];
        if name.is_empty() {
            continue;
        }
        let s2 = s.trim_start_matches(is_non_newline_space);
        let end = s2.find(',').unwrap_or(s2.len());
        tags.push((
            name.to_string(),
            s2[..end].trim_matches(is_space).to_string(),
        ));
        s = s2.get(end + 1..).unwrap_or("");
    }
    tags
}

/// A posting's comment: its text, its tags, and the dates from its `date:`
/// and `date2:` tags or bracketed dates, if any (postingcommentp).
#[allow(clippy::type_complexity)]
pub fn posting_commentp(
    c: &mut Cursor,
    year: Option<i64>,
    hints: &[Item],
) -> PResult<(String, Vec<Tag>, Option<Day>, Option<Day>)> {
    let (text, lines) = following_comment_with(c, hints, |c| comment_tags_and_dates(c, year))?;
    let (mut tags, mut dates) = (Vec::new(), Vec::new());
    for (t, d) in lines {
        tags.extend(t);
        dates.extend(d);
    }
    let find = |name: &str| dates.iter().find(|(n, _)| *n == name).map(|&(_, d)| d);
    Ok((text, tags, find("date"), find("date2")))
}

/// Posting dates found in a comment: `date` or `date2`, and the date.
type DateTags = Vec<(&'static str, Day)>;

/// The tags, and the date tags (from `date:`/`date2:` tags with a date value,
/// and bracketed dates), in the rest of a comment line
/// (commenttagsanddatesp).
fn comment_tags_and_dates(c: &mut Cursor, year: Option<i64>) -> PResult<(Vec<Tag>, DateTags)> {
    let mut tags = Vec::new();
    let mut dates = Vec::new();
    loop {
        // the text up to a colon, collecting bracketed dates on the way
        let start = c.pos;
        read_up_to(c, ':', year, &mut dates)?;
        if !c.eat(':') {
            return Ok((tags, dates));
        }
        let name = c.src[start..c.pos - 1]
            .rsplit(is_space)
            .next()
            .unwrap_or("")
            .to_string();
        c.skip_spaces();
        let value_start = c.pos;
        let is_date = name == "date" || name == "date2";
        if name.is_empty() {
        } else if is_date && !matches!(c.peek(), None | Some(',' | '\n')) {
            let d = datep(c, year).map_err(|e| {
                if c.pos == value_start {
                    c.unexpected(&[Item::label("full or partial date"), Item::EndOfInput])
                } else {
                    e
                }
            })?;
            dates.push((if name == "date" { "date" } else { "date2" }, d));
            read_up_to(c, ',', year, &mut dates)?;
            tags.push((
                name,
                c.src[value_start..c.pos].trim_matches(is_space).to_string(),
            ));
        } else {
            read_up_to(c, ',', year, &mut dates)?;
            tags.push((
                name,
                c.src[value_start..c.pos].trim_matches(is_space).to_string(),
            ));
        }
        c.eat(',');
    }
}

/// Text up to `end` or a newline, with any bracketed dates on the way
/// parsed into `dates`.
fn read_up_to(
    c: &mut Cursor,
    end: char,
    year: Option<i64>,
    dates: &mut Vec<(&'static str, Day)>,
) -> PResult<()> {
    loop {
        c.take_while(|ch| ch != end && ch != '\n' && ch != '[');
        if c.peek() != Some('[') {
            return Ok(());
        }
        let before = *c;
        if let Some(found) = bracketed_date_tagsp(c, year)? {
            dates.extend(found);
        }
        *c = before;
        c.bump();
    }
}

/// Ledger-style bracketed posting dates, `[DATE]`, `[DATE=DATE2]` or
/// `[=DATE2]`, as date and date2 tags (bracketeddatetagsp); or None if what
/// is in the brackets does not look like dates (digits, date separators and
/// `=`, with at least one digit and one separator). Something that looks
/// like dates but is not valid is an error.
fn bracketed_date_tagsp(
    c: &mut Cursor,
    year: Option<i64>,
) -> PResult<Option<Vec<(&'static str, Day)>>> {
    let rest = &c.rest()[1..];
    let n = rest
        .find(|ch: char| !(ch.is_ascii_digit() || is_date_sep_char(ch) || ch == '='))
        .unwrap_or(rest.len());
    let inside = &rest[..n];
    if n == 0
        || !rest[n..].starts_with(']')
        || !inside.contains(|ch: char| ch.is_ascii_digit())
        || !inside.contains(is_date_sep_char)
    {
        return Ok(None);
    }
    c.bump();
    let mut found = Vec::new();
    let mut year2 = year;
    if c.peek().is_some_and(|ch| ch.is_ascii_digit()) {
        let d = datep(c, year)?;
        year2 = Some(d.year());
        found.push(("date", d));
    }
    if c.eat('=') {
        found.push(("date2", datep(c, year2)?));
    }
    if !c.eat(']') {
        let mut expected = vec![Item::tokens("]")];
        expected.extend(digit_hint(c));
        return Err(c.unexpected(&expected));
    }
    Ok(Some(found))
}

/// An account alias, `OLD = NEW` or `/REGEX/ = REPLACEMENT`
/// (accountaliasp). A regular expression may contain `\/`; it is kept as
/// written. An invalid one is an error at the expression.
pub fn account_aliasp(c: &mut Cursor) -> PResult<AccountAlias> {
    if c.eat('/') {
        let re_start = c.pos;
        loop {
            match c.peek() {
                Some('\\') => {
                    c.bump();
                    c.bump();
                }
                Some('/' | '\n' | '\r') | None => break,
                Some(_) => {
                    c.bump();
                }
            }
        }
        let re_end = c.pos;
        if re_end == re_start {
            return Err(c.unexpected(&[]));
        }
        if !c.eat('/') {
            return Err(c.unexpected(&[Item::tokens("/")]));
        }
        c.skip_spaces();
        if !c.eat('=') {
            return Err(c.unexpected(&[Item::tokens("=")]));
        }
        c.skip_spaces();
        let repl = rest_of_line(c).to_string();
        return match to_regex_ci(&c.src[re_start..re_end]) {
            Ok(r) => Ok(AccountAlias::Regex(r, repl)),
            Err(e) => Err(ParseError::at_region(c.src, re_start, re_end, &e)),
        };
    }
    let old = c.take_while(|ch| ch != '=');
    if old.is_empty() {
        return Err(c.unexpected(&[]));
    }
    if !c.eat('=') {
        return Err(c.unexpected(&[Item::tokens("=")]));
    }
    c.skip_spaces();
    let new = rest_of_line(c);
    Ok(AccountAlias::Basic(
        old.trim_end_matches(is_space).into(),
        new.trim_end_matches(is_space).into(),
    ))
}

pub(crate) mod tests {
    use super::*;

    fn date(s: &str, year: Option<i64>) -> Result<Day, String> {
        let mut c = Cursor::new(s);
        datep(&mut c, year).map_err(|e| e.pretty("", s))
    }

    // tests_JournalReader "datep".
    #[cfg_attr(test, test)]
    pub(crate) fn dates() {
        assert_eq!(
            date("2018/01/01", None),
            Ok(Day::from_ymd(2018, 1, 1).unwrap())
        );
        assert!(date("2018-01-01", None).is_ok());
        assert!(date("2018.01.01", None).is_ok());
        assert!(
            date("1/1", None)
                .unwrap_err()
                .contains("current year is unknown")
        );
        assert_eq!(
            date("1/1", Some(2018)),
            Ok(Day::from_ymd(2018, 1, 1).unwrap())
        );
        assert!(date("2018/1/1", None).is_ok());
        assert!(
            date("2018/1-1", None)
                .unwrap_err()
                .contains("different separators")
        );
        assert!(
            date("2022/1/32", None)
                .unwrap_err()
                .contains("not a valid date")
        );
        assert_eq!(
            date("2018\n", None).unwrap_err(),
            "1:5:\n  |\n1 | 2018\n  |     ^\nunexpected newline\nexpecting date separator or digit\n"
        );
    }
}
