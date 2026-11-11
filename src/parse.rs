//! Parse errors, reported as megaparsec reports them (the error half of
//! Hledger.Utils.Parse, and Text.Megaparsec.Error's errorBundlePretty); a
//! cursor for the hand-written journal parser; and, for the small grammars
//! whose error messages list everything that could have come next (smart
//! dates, period expressions, queries), a parser state that tracks what
//! megaparsec tracks, so that those messages come out the same.
//!
//! Offsets are byte offsets into the text being parsed. Columns are counted
//! as megaparsec counts them: in display columns, with tab stops every 8.

use std::collections::BTreeSet;

use unicode_width::UnicodeWidthChar;

/// Something a parser found or expected: some literal text, a description
/// (a megaparsec label), or the end of the input. Ordered as megaparsec's
/// ErrorItem is.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Item {
    Tokens(String),
    Label(String),
    EndOfInput,
}

impl Item {
    pub fn label(s: &str) -> Item {
        Item::Label(s.to_string())
    }

    pub fn tokens(s: &str) -> Item {
        Item::Tokens(s.to_string())
    }

    /// As megaparsec shows it: `'x'`, `"xyz"`, `space`, `newline`, a label,
    /// or `end of input` (showErrorItem).
    fn show(&self) -> String {
        match self {
            Item::Tokens(s) => string_pretty(s),
            Item::Label(l) => l.clone(),
            Item::EndOfInput => "end of input".to_string(),
        }
    }

    /// The width of the caret line under it (errorItemLength).
    fn width(&self) -> usize {
        match self {
            Item::Tokens(s) => s.chars().map(char_width).sum(),
            Item::Label(_) | Item::EndOfInput => 1,
        }
    }
}

/// A parse error at an offset: either something unexpected found where
/// something else was expected (megaparsec's TrivialError), or one or more
/// messages with the caret line spanning the text from `offset` to `end` (a
/// FancyError, such as hledger's ErrorFailAt). Several messages arise when
/// alternatives fail with different messages at the same place; they are
/// kept sorted. A fancy error is shown where its region starts, but it was
/// raised at `raised`, which is where megaparsec considers it to be when
/// errors are merged (hledger's ErrorFailAt, relocated only when shown).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseError {
    Trivial {
        offset: usize,
        unexpected: Option<Item>,
        expected: BTreeSet<Item>,
    },
    Fancy {
        offset: usize,
        end: usize,
        msgs: Vec<String>,
        raised: usize,
    },
}

impl ParseError {
    /// Found `unexpected` at `offset`, where one of `expected` should be.
    pub fn trivial(offset: usize, unexpected: Option<Item>, expected: &[Item]) -> ParseError {
        ParseError::Trivial {
            offset,
            unexpected,
            expected: expected.iter().cloned().collect(),
        }
    }

    /// A message about the text at `offset`, with a one-column caret line
    /// (megaparsec's `fail`, hledger's parseErrorAt). The caret spans one
    /// character; `src` gives its length.
    pub fn fail_at(src: &str, offset: usize, msg: impl Into<String>) -> ParseError {
        let end = offset + src[offset..].chars().next().map_or(1, char::len_utf8);
        ParseError::Fancy {
            offset,
            end,
            msgs: vec![msg.into()],
            raised: offset,
        }
    }

    /// Like [`fail_at`](ParseError::fail_at), but raised at `raised`, after
    /// the text it is about (customFailure of a parseErrorAt).
    pub fn fail_at_raised(
        src: &str,
        offset: usize,
        raised: usize,
        msg: impl Into<String>,
    ) -> ParseError {
        match ParseError::fail_at(src, offset, msg) {
            ParseError::Fancy {
                offset, end, msgs, ..
            } => ParseError::Fancy {
                offset,
                end,
                msgs,
                raised,
            },
            e => e,
        }
    }

    /// A trivial error made fancy, its message the text it would show, so
    /// that alternatives tried after it do not replace it (hledger's
    /// templateexprp `region asFancy`).
    pub fn into_fancy(self, src: &str) -> ParseError {
        match self {
            e @ ParseError::Trivial { .. } => {
                let text = e.text();
                ParseError::fail_at(src, e.offset(), text.trim_end())
            }
            e => e,
        }
    }

    /// A message about the text from `start` to `end`, shown after an empty
    /// line (parseErrorAtRegion); an empty region is one column.
    pub fn at_region(src: &str, start: usize, end: usize, msg: &str) -> ParseError {
        let end = if start < end {
            end
        } else {
            start + src[start..].chars().next().map_or(1, char::len_utf8)
        };
        ParseError::Fancy {
            offset: start,
            end,
            msgs: vec![format!("\n{msg}")],
            raised: start,
        }
    }

    /// Like [`at_region`](ParseError::at_region), but raised at `raised`,
    /// where the parser was when it found the problem (customFailure).
    pub fn at_region_raised(
        src: &str,
        start: usize,
        end: usize,
        raised: usize,
        msg: &str,
    ) -> ParseError {
        match ParseError::at_region(src, start, end, msg) {
            ParseError::Fancy {
                offset, end, msgs, ..
            } => ParseError::Fancy {
                offset,
                end,
                msgs,
                raised,
            },
            e => e,
        }
    }

    /// The error shown where it was raised, its caret line as long as its
    /// region, as megaparsec's errorBundlePretty shows an hledger error
    /// without the relocation of customErrorBundlePretty (as rewrite shows
    /// the errors of --add-posting).
    pub fn shown_where_raised(self, src: &str) -> ParseError {
        match self {
            ParseError::Fancy {
                offset,
                end,
                msgs,
                raised,
            } => {
                let n = src.get(offset..end).map_or(1, |s| s.chars().count());
                let end =
                    (src[raised..].char_indices().nth(n)).map_or(src.len(), |(i, _)| raised + i);
                ParseError::Fancy {
                    offset: raised,
                    end,
                    msgs,
                    raised,
                }
            }
            e => e,
        }
    }

    /// The error moved by `delta` bytes, for text parsed as part of a
    /// larger text that starts `delta` bytes earlier.
    pub fn shifted(self, delta: usize) -> ParseError {
        match self {
            ParseError::Trivial {
                offset,
                unexpected,
                expected,
            } => ParseError::Trivial {
                offset: offset + delta,
                unexpected,
                expected,
            },
            ParseError::Fancy {
                offset,
                end,
                msgs,
                raised,
            } => ParseError::Fancy {
                offset: offset + delta,
                end: end + delta,
                msgs,
                raised: raised + delta,
            },
        }
    }

    /// Where the error is shown.
    pub fn offset(&self) -> usize {
        match self {
            ParseError::Trivial { offset, .. } | ParseError::Fancy { offset, .. } => *offset,
        }
    }

    /// Where megaparsec considers the error to be, when merging errors.
    fn raised(&self) -> usize {
        match self {
            ParseError::Trivial { offset, .. } => *offset,
            ParseError::Fancy { raised, .. } => *raised,
        }
    }

    /// Two errors from alternatives tried at the same place combined, as
    /// megaparsec combines them: the one that got further wins; at the same
    /// offset, trivial errors merge their expectations and keep the greater
    /// unexpected item, fancy errors merge their messages, and a fancy error
    /// wins over a trivial one.
    pub fn merge(self, other: ParseError) -> ParseError {
        use std::cmp::Ordering::*;
        match self.raised().cmp(&other.raised()) {
            Less => other,
            Greater => self,
            Equal => match (self, other) {
                (
                    ParseError::Trivial {
                        offset,
                        unexpected: u1,
                        expected: mut e1,
                    },
                    ParseError::Trivial {
                        unexpected: u2,
                        expected: e2,
                        ..
                    },
                ) => {
                    e1.extend(e2);
                    ParseError::Trivial {
                        offset,
                        unexpected: u1.max(u2),
                        expected: e1,
                    }
                }
                (
                    ParseError::Fancy {
                        offset,
                        end: e1,
                        msgs: mut m1,
                        raised,
                    },
                    ParseError::Fancy {
                        end: e2, msgs: m2, ..
                    },
                ) => {
                    m1.extend(m2);
                    m1.sort();
                    m1.dedup();
                    ParseError::Fancy {
                        offset,
                        end: e1.max(e2),
                        msgs: m1,
                        raised,
                    }
                }
                (e @ ParseError::Fancy { .. }, _) => e,
                (_, e) => e,
            },
        }
    }

    /// These items expected too, if this is a trivial error (megaparsec's
    /// withHints).
    fn expect_also(&mut self, items: &BTreeSet<Item>) {
        if let ParseError::Trivial { expected, .. } = self {
            expected.extend(items.iter().cloned());
        }
    }

    /// The message lines, each ending with a newline (parseErrorTextPretty),
    /// with long lists of expectations shortened as hledger shortens them
    /// (finalizeCustomErrorBundle).
    fn text(&self) -> String {
        match self {
            ParseError::Trivial {
                unexpected,
                expected,
                ..
            } => {
                if unexpected.is_none() && expected.is_empty() {
                    return "unknown parse error\n".to_string();
                }
                let mut expected: Vec<Item> = expected.iter().cloned().collect();
                if expected.len() > 20 {
                    let more = expected.len() - 10;
                    expected.truncate(10);
                    expected.push(Item::Label(format!("other valid values ({more} more)")));
                }
                let mut out = String::new();
                if let Some(u) = unexpected {
                    out += &format!("unexpected {}\n", u.show());
                }
                let shown: BTreeSet<String> = expected.iter().map(Item::show).collect();
                if !shown.is_empty() {
                    out += &format!(
                        "expecting {}\n",
                        or_list(&shown.into_iter().collect::<Vec<_>>())
                    );
                }
                out
            }
            ParseError::Fancy { msgs, .. } => msgs.iter().map(|m| format!("{m}\n")).collect(),
        }
    }

    /// The width of the caret line, before it is clipped to the line.
    fn width(&self, src: &str) -> usize {
        match self {
            ParseError::Trivial {
                unexpected: None, ..
            } => 1,
            ParseError::Trivial {
                unexpected: Some(u),
                ..
            } => u.width(),
            ParseError::Fancy { offset, end, .. } => {
                let end = (*end).min(src.len()).max(*offset);
                src.get(*offset..end)
                    .map_or(1, |s| s.chars().count())
                    .max(1)
            }
        }
    }

    /// The error as megaparsec shows it, with the source position (`NAME:LINE:COL:`
    /// or `LINE:COL:` if the name is empty), the offending line with a caret
    /// line under it, and the message; ending with a newline
    /// (customErrorBundlePretty).
    pub fn pretty(&self, name: &str, src: &str) -> String {
        let offset = self.offset().min(src.len());
        let line_start = src[..offset].rfind('\n').map_or(0, |i| i + 1);
        let line_no = src[..line_start].matches('\n').count() + 1;
        self.pretty_as_line(name, line_no, src)
    }

    /// Like [`pretty`](ParseError::pretty), but showing the error's line as
    /// line `line_no` of the file `name`, for text parsed after other text
    /// was inlined into it.
    pub fn pretty_as_line(&self, name: &str, line_no: usize, src: &str) -> String {
        let offset = self.offset().min(src.len());
        let line_start = src[..offset].rfind('\n').map_or(0, |i| i + 1);
        let line_end = src[line_start..]
            .find('\n')
            .map_or(src.len(), |i| line_start + i);
        let col = columns(&src[line_start..offset]) + 1;
        let mut line = expand_tabs(&src[line_start..line_end]);
        if line.is_empty() {
            line = "<empty line>".to_string();
        }
        let line_len: usize = line.chars().map(char_width).sum();
        let shift = col - 1;
        let width = self.width(src);
        let pointer = if shift + width > line_len {
            (line_len + 1).saturating_sub(shift)
        } else {
            width
        }
        .max(1);
        let num = line_no.to_string();
        let pad = " ".repeat(num.len() + 1);
        let pos = if name.is_empty() {
            format!("{line_no}:{col}")
        } else {
            format!("{name}:{line_no}:{col}")
        };
        format!(
            "{pos}:\n{pad}|\n{num} | {line}\n{pad}| {}{}\n{}",
            " ".repeat(shift),
            "^".repeat(pointer),
            self.text()
        )
    }
}

/// Items in a list with commas and a final "or" (megaparsec's orList).
fn or_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [x] => x.clone(),
        [x, y] => format!("{x} or {y}"),
        [init @ .., last] => format!("{}, or {last}", init.join(", ")),
    }
}

/// The display width of a character as megaparsec counts it: control
/// characters none, wide characters two.
fn char_width(c: char) -> usize {
    if c.is_control() {
        0
    } else {
        c.width().unwrap_or(0)
    }
}

/// The column reached after this text at the start of a line, less one:
/// tabs advance to the next multiple of 8.
fn columns(s: &str) -> usize {
    s.chars().fold(0, |col, c| match c {
        '\t' => col + 8 - col % 8,
        c => col + char_width(c),
    })
}

/// The column, from 1, after this text at the start of a line, as
/// megaparsec counts it: tabs to the next stop of 8, other characters one
/// column each (sourceColumn).
pub fn megaparsec_column(line_prefix: &str) -> usize {
    line_prefix.chars().fold(1, |col, ch| match ch {
        '\t' => col + 8 - (col - 1) % 8,
        _ => col + 1,
    })
}

/// The line with its tabs replaced by spaces up to the next multiple of 8
/// (megaparsec's expandTab).
fn expand_tabs(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut col = 0;
    for c in s.chars() {
        if c == '\t' {
            let n = 8 - col % 8;
            out.extend(std::iter::repeat_n(' ', n));
            col += n;
        } else {
            out.push(c);
            col += char_width(c);
        }
    }
    out
}

/// A character as megaparsec shows it in an error: `space`, `newline`,
/// `'x'` (charPretty).
pub fn char_pretty(c: char) -> String {
    match c {
        ' ' => "space".to_string(),
        c => char_pretty_name(c).map_or_else(|| format!("'{c}'"), str::to_string),
    }
}

/// Text as megaparsec shows it in an error (stringPretty).
fn string_pretty(s: &str) -> String {
    let mut cs = s.chars();
    match (cs.next(), cs.next()) {
        (Some(c), None) => char_pretty(c),
        _ if s == "\r\n" => "crlf newline".to_string(),
        _ => {
            let inner: String = s
                .chars()
                .map(|c| char_pretty_name(c).map_or_else(|| c.to_string(), |n| format!("<{n}>")))
                .collect();
            format!("\"{inner}\"")
        }
    }
}

/// The names of the control characters (charPretty').
fn char_pretty_name(c: char) -> Option<&'static str> {
    Some(match c {
        '\0' => "null",
        '\u{1}' => "start of heading",
        '\u{2}' => "start of text",
        '\u{3}' => "end of text",
        '\u{4}' => "end of transmission",
        '\u{5}' => "enquiry",
        '\u{6}' => "acknowledge",
        '\u{7}' => "bell",
        '\u{8}' => "backspace",
        '\t' => "tab",
        '\n' => "newline",
        '\u{b}' => "vertical tab",
        '\u{c}' => "form feed",
        '\r' => "carriage return",
        '\u{e}' => "shift out",
        '\u{f}' => "shift in",
        '\u{10}' => "data link escape",
        '\u{11}' => "device control one",
        '\u{12}' => "device control two",
        '\u{13}' => "device control three",
        '\u{14}' => "device control four",
        '\u{15}' => "negative acknowledge",
        '\u{16}' => "synchronous idle",
        '\u{17}' => "end of transmission block",
        '\u{18}' => "cancel",
        '\u{19}' => "end of medium",
        '\u{1a}' => "substitute",
        '\u{1b}' => "escape",
        '\u{1c}' => "file separator",
        '\u{1d}' => "group separator",
        '\u{1e}' => "record separator",
        '\u{1f}' => "unit separator",
        '\u{7f}' => "delete",
        '\u{a0}' => "non-breaking space",
        _ => return None,
    })
}

/// A position in text being parsed, for hand-written recursive descent.
#[derive(Clone, Copy, Debug)]
pub struct Cursor<'a> {
    pub src: &'a str,
    pub pos: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(src: &'a str) -> Cursor<'a> {
        Cursor { src, pos: 0 }
    }

    /// The text not yet parsed.
    pub fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    pub fn at_end(&self) -> bool {
        self.pos >= self.src.len()
    }

    pub fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    /// The character after the next one.
    pub fn peek2(&self) -> Option<char> {
        self.rest().chars().nth(1)
    }

    /// Move past the next character.
    pub fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    /// Move past this character if it is next.
    pub fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.pos += c.len_utf8();
            true
        } else {
            false
        }
    }

    /// Move past this text if it is next.
    pub fn eat_str(&mut self, s: &str) -> bool {
        if self.rest().starts_with(s) {
            self.pos += s.len();
            true
        } else {
            false
        }
    }

    /// Move past an ASCII digit if one is next.
    pub fn eat_digit(&mut self) -> bool {
        let digit = self
            .rest()
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_digit);
        self.pos += usize::from(digit);
        digit
    }

    /// Move past the characters satisfying a predicate, returning them.
    pub fn take_while(&mut self, f: impl Fn(char) -> bool) -> &'a str {
        let rest = self.rest();
        let n = rest.find(|c| !f(c)).unwrap_or(rest.len());
        self.pos += n;
        &rest[..n]
    }

    /// Move past spaces and tabs (and other non-newline white space),
    /// returning whether there were any (skipNonNewlineSpaces').
    pub fn skip_spaces(&mut self) -> bool {
        !self.take_while(is_non_newline_space).is_empty()
    }

    /// An error here: the next character is unexpected (or the end of the
    /// input), and one of these was expected.
    pub fn unexpected(&self, expected: &[Item]) -> ParseError {
        let found = match self.peek() {
            Some(c) => Item::Tokens(c.to_string()),
            None => Item::EndOfInput,
        };
        ParseError::trivial(self.pos, Some(found), expected)
    }
}

/// White space other than a newline (isNonNewlineSpace).
pub fn is_non_newline_space(c: char) -> bool {
    c != '\n' && crate::text::is_space(c)
}

/// What a [`Parser`] function returns.
pub type PResult<T> = Result<T, ParseError>;

/// The state of a parser ported from a megaparsec parser, tracking what
/// megaparsec tracks so that its error messages come out the same.
///
/// A parser is a function from `&mut Parser` to a [`PResult`]. It has
/// consumed input if it moved `pos`. One that fails after consuming input
/// makes its callers fail too, unless an [`attempt`](Parser::attempt)
/// around it puts `pos` back; one that fails without consuming input lets
/// its caller try an alternative. A failed parser leaves `pos` where it
/// failed, which is how its callers know whether it consumed input.
///
/// The hints are what could also have been accepted at `pos`: the
/// expectations of the alternatives that just failed there before one
/// succeeded without consuming input (an absent optional part), and the
/// labels of the runs that could have been longer (a run of digits). An
/// error at `pos` lists them among its expectations.
#[derive(Clone, Debug)]
pub struct Parser<'a> {
    pub src: &'a str,
    pub pos: usize,
    hints: BTreeSet<Item>,
}

impl<'a> Parser<'a> {
    pub fn new(src: &'a str) -> Parser<'a> {
        Parser {
            src,
            pos: 0,
            hints: BTreeSet::new(),
        }
    }

    /// The text not yet parsed.
    pub fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    /// The next character as found, or the end of the input.
    fn next_item(&self) -> Item {
        self.rest()
            .chars()
            .next()
            .map_or(Item::EndOfInput, |c| Item::Tokens(c.to_string()))
    }

    /// A failure here, consuming nothing: `unexpected` where `expected` or
    /// one of the hints should be.
    fn error_here(&self, unexpected: Option<Item>, expected: Option<Item>) -> ParseError {
        let mut items = self.hints.clone();
        items.extend(expected);
        ParseError::Trivial {
            offset: self.pos,
            unexpected,
            expected: items,
        }
    }

    /// Consume `n` bytes; `hint` could have been accepted after them.
    fn advance(&mut self, n: usize, hint: Option<Item>) {
        if n > 0 {
            self.pos += n;
            self.hints.clear();
        }
        self.hints.extend(hint);
    }

    /// An error's expectations become hints, if it happened here
    /// (megaparsec's toHints).
    fn hint_from(&mut self, e: &ParseError) {
        if let ParseError::Trivial {
            offset, expected, ..
        } = e
            && *offset == self.pos
        {
            self.hints.extend(expected.iter().cloned());
        }
    }

    /// This text (megaparsec's `string`).
    pub fn string(&mut self, s: &str) -> PResult<&'a str> {
        self.tokens(s, false)
    }

    /// This text, ignoring the case of ASCII letters (`string'`).
    pub fn string_ci(&mut self, s: &str) -> PResult<&'a str> {
        self.tokens(s, true)
    }

    /// As much text as `s` has characters, if it matches; otherwise that
    /// much text is what was unexpected.
    fn tokens(&mut self, s: &str, ci: bool) -> PResult<&'a str> {
        let rest = self.rest();
        let len = rest
            .char_indices()
            .nth(s.chars().count())
            .map_or(rest.len(), |(i, _)| i);
        let found = &rest[..len];
        if s.is_empty() || (ci && found.eq_ignore_ascii_case(s)) || found == s {
            self.advance(len, None);
            return Ok(found);
        }
        let unexpected = if rest.is_empty() {
            Item::EndOfInput
        } else {
            Item::Tokens(found.to_string())
        };
        Err(self.error_here(Some(unexpected), Some(Item::tokens(s))))
    }

    /// This character (`char`).
    pub fn char(&mut self, c: char) -> PResult<char> {
        self.satisfy_or(Some(Item::Tokens(c.to_string())), |x| x == c)
    }

    /// This ASCII letter in either case (`char'`).
    pub fn char_ci(&mut self, c: char) -> PResult<char> {
        or(
            self,
            |p| p.char(c.to_ascii_lowercase()),
            |p| p.char(c.to_ascii_uppercase()),
        )
    }

    /// A character satisfying `f` (`satisfy`); with a label, what was
    /// expected (`satisfy f <?> label`).
    pub fn satisfy(&mut self, label: Option<&str>, f: impl Fn(char) -> bool) -> PResult<char> {
        self.satisfy_or(label.map(Item::label), f)
    }

    fn satisfy_or(&mut self, expected: Option<Item>, f: impl Fn(char) -> bool) -> PResult<char> {
        match self.rest().chars().next() {
            Some(c) if f(c) => {
                self.advance(c.len_utf8(), None);
                Ok(c)
            }
            _ => Err(self.error_here(Some(self.next_item()), expected)),
        }
    }

    /// One or more characters satisfying `f`, which with a label is what
    /// was expected, and could have followed (`takeWhile1P`).
    pub fn take_while1(
        &mut self,
        label: Option<&str>,
        f: impl Fn(char) -> bool,
    ) -> PResult<&'a str> {
        let rest = self.rest();
        let n = rest.find(|c| !f(c)).unwrap_or(rest.len());
        if n == 0 {
            return Err(self.error_here(Some(self.next_item()), label.map(Item::label)));
        }
        self.advance(n, label.map(Item::label));
        Ok(&rest[..n])
    }

    /// Any characters satisfying `f` (`takeWhileP`).
    pub fn take_while(&mut self, label: Option<&str>, f: impl Fn(char) -> bool) -> &'a str {
        let rest = self.rest();
        let n = rest.find(|c| !f(c)).unwrap_or(rest.len());
        self.advance(n, label.map(Item::label));
        &rest[..n]
    }

    /// Spaces and tabs, if any (skipNonNewlineSpaces).
    pub fn skip_spaces(&mut self) {
        self.take_while(None, is_non_newline_space);
    }

    /// The end of the input (`eof`).
    pub fn eof(&mut self) -> PResult<()> {
        if self.pos < self.src.len() {
            return Err(self.error_here(Some(self.next_item()), Some(Item::EndOfInput)));
        }
        Ok(())
    }

    /// Fail here with this message (`fail`).
    pub fn fail<T>(&self, msg: impl Into<String>) -> PResult<T> {
        Err(ParseError::fail_at(self.src, self.pos, msg))
    }

    /// The failure of `empty`: here, with nothing unexpected.
    pub fn empty(&self) -> ParseError {
        self.error_here(None, None)
    }

    /// `f`, but if it fails after consuming input, failing as if it had
    /// consumed none (`try`).
    pub fn attempt<T>(&mut self, f: impl FnOnce(&mut Self) -> PResult<T>) -> PResult<T> {
        let (pos, hints) = (self.pos, self.hints.clone());
        f(self).map_err(|mut e| {
            if self.pos != pos {
                // What the sequence around the attempt would add to its error.
                e.expect_also(&hints);
                self.pos = pos;
            }
            self.hints = hints;
            e
        })
    }

    /// `f`, but if it fails without consuming input, `l` is what was
    /// expected (`<?>`).
    pub fn label<T>(&mut self, l: &str, f: impl FnOnce(&mut Self) -> PResult<T>) -> PResult<T> {
        let pos = self.pos;
        let outer = std::mem::take(&mut self.hints);
        match f(self) {
            Ok(v) => {
                if self.pos == pos {
                    let had_hints = !self.hints.is_empty();
                    self.hints = outer;
                    if had_hints {
                        self.hints.insert(Item::label(l));
                    }
                }
                Ok(v)
            }
            Err(mut e) => {
                if self.pos == pos
                    && let ParseError::Trivial { expected, .. } = &mut e
                {
                    *expected = outer;
                    expected.insert(Item::label(l));
                }
                Err(e)
            }
        }
    }

    /// `f`'s result, or none if it fails without consuming input
    /// (`optional`).
    pub fn optional<T>(&mut self, f: impl FnOnce(&mut Self) -> PResult<T>) -> PResult<Option<T>> {
        let (pos, hints) = (self.pos, self.hints.clone());
        match f(self) {
            Ok(v) => Ok(Some(v)),
            Err(e) if self.pos != pos => Err(e),
            Err(e) => {
                self.hints = hints;
                self.hint_from(&e);
                Ok(None)
            }
        }
    }

    /// Succeed, consuming nothing, if `f` fails here (`notFollowedBy`).
    pub fn not_followed_by<T>(&mut self, f: impl FnOnce(&mut Self) -> PResult<T>) -> PResult<()> {
        let (pos, hints) = (self.pos, self.hints.clone());
        let found = self.next_item();
        let r = f(self);
        self.pos = pos;
        self.hints = hints;
        match r {
            Ok(_) => Err(self.error_here(Some(found), None)),
            Err(_) => Ok(()),
        }
    }

    /// `f` repeated until it fails without consuming input (`many`). A
    /// repetition that consumes nothing ends it, where megaparsec would
    /// repeat it forever.
    pub fn many<T>(&mut self, mut f: impl FnMut(&mut Self) -> PResult<T>) -> PResult<Vec<T>> {
        let mut found = Vec::new();
        loop {
            let pos = self.pos;
            match self.optional(&mut f)? {
                Some(x) => found.push(x),
                None => return Ok(found),
            }
            if self.pos == pos {
                return Ok(found);
            }
        }
    }

    /// One or more `f` separated by `sep` (`sepBy1`).
    pub fn sep_by1<T, S>(
        &mut self,
        mut f: impl FnMut(&mut Self) -> PResult<T>,
        mut sep: impl FnMut(&mut Self) -> PResult<S>,
    ) -> PResult<Vec<T>> {
        let mut found = vec![f(self)?];
        found.extend(self.many(|p| {
            sep(p)?;
            f(p)
        })?);
        Ok(found)
    }

    /// Zero or more `f` separated by `sep` (`sepBy`).
    pub fn sep_by<T, S>(
        &mut self,
        f: impl FnMut(&mut Self) -> PResult<T>,
        sep: impl FnMut(&mut Self) -> PResult<S>,
    ) -> PResult<Vec<T>> {
        Ok(self.optional(|p| p.sep_by1(f, sep))?.unwrap_or_default())
    }
}

/// Alternatives tried in turn from one place, as megaparsec's `<|>` tries
/// them: the first that succeeds, or fails after consuming input, decides;
/// the failures of the others before it are combined into its error, or
/// into hints if it succeeded without consuming input.
pub struct Alts {
    pos: usize,
    hints: BTreeSet<Item>,
    errors: Vec<ParseError>,
}

impl Alts {
    pub fn new(p: &Parser) -> Alts {
        Alts {
            pos: p.pos,
            hints: p.hints.clone(),
            errors: Vec::new(),
        }
    }

    /// Try one alternative: its result if that is final, or none if it
    /// failed without consuming input, with the parser put back.
    pub fn run<'a, T>(
        &mut self,
        p: &mut Parser<'a>,
        f: impl FnOnce(&mut Parser<'a>) -> PResult<T>,
    ) -> Option<PResult<T>> {
        match f(p) {
            Ok(v) => {
                if p.pos == self.pos {
                    for e in &self.errors {
                        p.hint_from(e);
                    }
                }
                Some(Ok(v))
            }
            Err(e) if p.pos != self.pos => {
                Some(Err(self.errors.drain(..).fold(e, ParseError::merge)))
            }
            Err(e) => {
                p.pos = self.pos;
                p.hints = self.hints.clone();
                self.errors.push(e);
                None
            }
        }
    }

    /// The error when every alternative failed without consuming input, as
    /// `choice` combines them: `a <|> (b <|> (c <|> empty))`.
    pub fn choice_error(self, p: &Parser) -> ParseError {
        let pos = self.pos;
        self.errors
            .into_iter()
            .rev()
            .fold(p.empty(), |acc, e| combine(pos, e, acc))
    }

    /// The error when every alternative failed without consuming input, as
    /// `a <|> b <|> c` combines them: `(a <|> b) <|> c`.
    pub fn or_error(self) -> ParseError {
        let pos = self.pos;
        let mut errors = self.errors.into_iter();
        let first = errors.next().expect("an alternative was tried");
        errors.fold(first, |acc, e| combine(pos, acc, e))
    }
}

/// The errors of two alternatives that both failed without consuming
/// input, combined as megaparsec 9.8 combines them: a trivial error at the
/// place they started from wins over a trivial error further on (which an
/// `attempt` backtracked from); otherwise they merge.
fn combine(at: usize, e1: ParseError, e2: ParseError) -> ParseError {
    use ParseError::Trivial;
    match (&e1, &e2) {
        (Trivial { offset: o1, .. }, Trivial { offset: o2, .. }) if *o1 == at && *o2 > at => e1,
        (Trivial { offset: o1, .. }, Trivial { offset: o2, .. }) if *o2 == at && *o1 > at => e2,
        _ => e2.merge(e1),
    }
}

/// An alternative for [`choice`].
pub type Alt<'r, 'a, T> = &'r mut dyn FnMut(&mut Parser<'a>) -> PResult<T>;

/// The first of these alternatives that succeeds (`choice`).
pub fn choice<'a, T>(p: &mut Parser<'a>, alts: &mut [Alt<'_, 'a, T>]) -> PResult<T> {
    let mut tried = Alts::new(p);
    for f in alts {
        if let Some(r) = tried.run(p, f) {
            return r;
        }
    }
    Err(tried.choice_error(p))
}

/// The first of these alternatives that succeeds, each in an
/// [`attempt`](Parser::attempt) (hledger's `choice'`).
pub fn choice_try<'a, T>(p: &mut Parser<'a>, alts: &mut [Alt<'_, 'a, T>]) -> PResult<T> {
    let mut tried = Alts::new(p);
    for f in alts {
        if let Some(r) = tried.run(p, |p| p.attempt(f)) {
            return r;
        }
    }
    Err(tried.choice_error(p))
}

/// `a`, or if it fails without consuming input, `b` (`a <|> b`).
pub fn or<'a, T>(
    p: &mut Parser<'a>,
    a: impl FnOnce(&mut Parser<'a>) -> PResult<T>,
    b: impl FnOnce(&mut Parser<'a>) -> PResult<T>,
) -> PResult<T> {
    let mut tried = Alts::new(p);
    if let Some(r) = tried.run(p, a) {
        return r;
    }
    if let Some(r) = tried.run(p, b) {
        return r;
    }
    Err(tried.or_error())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trivial_error_with_a_caret_under_the_unexpected_text() {
        let src = "2024-01-01\n  a  1 \t {\n  b\n";
        let offset = src.find(" {").unwrap() + 2;
        let e = ParseError::trivial(
            offset,
            Some(Item::tokens("\n")),
            &[
                Item::tokens("="),
                Item::tokens("{"),
                Item::tokens("}"),
                Item::label("amount"),
                Item::label("digit"),
            ],
        );
        assert_eq!(
            e.pretty("-", src),
            "-:2:11:\n  |\n2 |   a  1   {\n  |           ^\nunexpected newline\n\
             expecting '=', '{', '}', amount, or digit\n"
        );
    }

    #[test]
    fn tabs_are_expanded_and_counted_to_the_next_stop() {
        let src = "x\n\ta  1 \t {\n";
        let offset = src.find(" {").unwrap() + 2;
        let e = ParseError::trivial(offset, Some(Item::tokens("\n")), &[]);
        assert_eq!(
            e.pretty("-", src),
            "-:2:19:\n  |\n2 |         a  1     {\n  |                   ^\nunexpected newline\n"
        );
    }

    #[test]
    fn long_expectations_are_shortened_and_tokens_quoted() {
        let expected: Vec<Item> = (b'a'..=b'v')
            .map(|c| Item::tokens(&(c as char).to_string()))
            .collect();
        let e = ParseError::trivial(0, Some(Item::tokens("foo")), &expected);
        assert_eq!(
            e.pretty("", "foo"),
            "1:1:\n  |\n1 | foo\n  | ^^^\nunexpected \"foo\"\n\
             expecting 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', or other valid values (12 more)\n"
        );
        assert_eq!(string_pretty("a\tb"), "\"a<tab>b\"");
        assert_eq!(string_pretty("'"), "'''");
        assert_eq!(string_pretty(" "), "space");
        assert_eq!(string_pretty("\r\n"), "crlf newline");
    }

    #[test]
    fn fancy_errors_and_carets_clipped_to_the_line() {
        let src = "abc\n\ndef";
        let e = ParseError::at_region(src, 1, 3, "bad bc");
        assert_eq!(
            e.pretty("f", src),
            "f:1:2:\n  |\n1 | abc\n  |  ^^\n\nbad bc\n"
        );
        let e = ParseError::fail_at(src, 4, "empty");
        assert_eq!(
            e.pretty("f", src),
            "f:2:1:\n  |\n2 | <empty line>\n  | ^\nempty\n"
        );
        let e = ParseError::trivial(2, Some(Item::tokens("cdefg")), &[]);
        assert_eq!(
            e.pretty("", src),
            "1:3:\n  |\n1 | abc\n  |   ^^\nunexpected \"cdefg\"\n"
        );
        let e = ParseError::trivial(src.len(), Some(Item::EndOfInput), &[Item::label("x")]);
        assert_eq!(
            e.pretty("", src),
            "3:4:\n  |\n3 | def\n  |    ^\nunexpected end of input\nexpecting x\n"
        );
    }

    #[test]
    fn merging_alternatives() {
        let a = ParseError::trivial(3, Some(Item::tokens("f")), &[Item::label("year")]);
        let b = ParseError::trivial(3, Some(Item::tokens("foo")), &[Item::tokens("..")]);
        let c = ParseError::trivial(1, Some(Item::tokens("x")), &[Item::label("zzz")]);
        let m = a.merge(b).merge(c);
        assert_eq!(
            m,
            ParseError::trivial(
                3,
                Some(Item::tokens("foo")),
                &[Item::label("year"), Item::tokens("..")]
            )
        );
    }

    // A run of digits leaves its label as a hint, so a failure right after
    // it expects more of them; an absent optional part leaves its
    // expectations.
    #[test]
    fn hints() {
        let mut p = Parser::new("2026-13");
        p.take_while1(Some("year"), |c| c.is_ascii_digit()).unwrap();
        let e = p.eof().unwrap_err();
        assert_eq!(
            e.pretty("", p.src),
            "1:5:\n  |\n1 | 2026-13\n  |     ^\nunexpected '-'\nexpecting end of input or year\n"
        );
        let mut p = Parser::new("ab");
        assert_eq!(p.optional(|p| p.string_ci("in")).unwrap(), None);
        assert_eq!(p.optional(|p| p.char_ci('x')).unwrap(), None);
        p.skip_spaces();
        let e = p.label("integer", |p| {
            p.take_while1(Some("digit"), |c| c.is_ascii_digit())
        });
        assert_eq!(
            e.unwrap_err(),
            ParseError::trivial(
                0,
                Some(Item::tokens("a")),
                &[
                    Item::tokens("in"),
                    Item::tokens("x"),
                    Item::tokens("X"),
                    Item::label("integer")
                ]
            )
        );
    }

    // A failure after consuming input decides, unless attempted; an
    // attempted failure further on loses to one where the alternatives
    // started, and failures at the same place merge.
    #[test]
    fn alternatives() {
        let ab = |p: &mut Parser| {
            p.char('a')?;
            p.char('b')
        };
        let mut p = Parser::new("ac");
        let e = or(&mut p, ab, |p| p.char('x')).unwrap_err();
        assert_eq!(e.offset(), 1);
        let mut p = Parser::new("ac");
        let e = choice_try(&mut p, &mut [&mut |p| ab(p), &mut |p| p.char('x')]).unwrap_err();
        assert_eq!(
            e,
            ParseError::trivial(0, Some(Item::tokens("a")), &[Item::tokens("x")])
        );
        let mut p = Parser::new("ac");
        let e = choice_try(&mut p, &mut [&mut |p| ab(p)]).unwrap_err();
        assert_eq!(e, ParseError::trivial(0, None, &[]));
        let mut p = Parser::new("ac");
        let e = or(&mut p, |p| p.attempt(ab), |p| p.attempt(ab)).unwrap_err();
        assert_eq!(
            e,
            ParseError::trivial(1, Some(Item::tokens("c")), &[Item::tokens("b")])
        );
        let mut p = Parser::new("ac");
        assert_eq!(
            choice(
                &mut p,
                &mut [&mut |p| p.string("b"), &mut |p| p.string("ac")]
            ),
            Ok("ac")
        );
        let fails = |p: &mut Parser| -> PResult<()> {
            p.char('a')?;
            p.fail("no")
        };
        let mut p = Parser::new("a");
        let e = choice_try(
            &mut p,
            &mut [&mut |p| fails(p), &mut |p| {
                p.char('a')?;
                p.fail("again")
            }],
        )
        .unwrap_err();
        assert_eq!(e.pretty("", "a"), "1:2:\n  |\n1 | a\n  |  ^\nagain\nno\n");
    }

    #[test]
    fn cursor() {
        let mut c = Cursor::new("Every  2 weeks");
        assert!(c.eat_str("Every"));
        assert!(c.skip_spaces());
        assert!(!c.skip_spaces());
        assert_eq!(c.take_while(|c| c.is_ascii_digit()), "2");
        assert_eq!(c.peek2(), Some('w'));
        assert_eq!(c.unexpected(&[]).offset(), 8);
        assert!(c.eat(' ') && c.eat_str("weeks") && c.at_end());
        assert_eq!(
            c.unexpected(&[]),
            ParseError::trivial(14, Some(Item::EndOfInput), &[])
        );
    }
}
