//! Parse errors, reported as megaparsec reports them (the error half of
//! Hledger.Utils.Parse, and Text.Megaparsec.Error's errorBundlePretty), and a
//! cursor for the hand-written parsers of dates, queries and journals.
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
/// something else was expected (megaparsec's TrivialError), or a message
/// with the caret line spanning the text from `offset` to `end` (a
/// FancyError, such as hledger's ErrorFailAt).
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
        msg: String,
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
            msg: msg.into(),
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
            msg: format!("\n{msg}"),
        }
    }

    pub fn offset(&self) -> usize {
        match self {
            ParseError::Trivial { offset, .. } | ParseError::Fancy { offset, .. } => *offset,
        }
    }

    /// Two errors from alternatives tried at the same place combined, as
    /// megaparsec combines them: the one that got further wins; at the same
    /// offset, trivial errors merge their expectations and keep the greater
    /// unexpected item, and a fancy error wins over a trivial one.
    pub fn merge(self, other: ParseError) -> ParseError {
        use std::cmp::Ordering::*;
        match self.offset().cmp(&other.offset()) {
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
                (e @ ParseError::Fancy { .. }, _) => e,
                (_, e) => e,
            },
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
            ParseError::Fancy { msg, .. } => format!("{msg}\n"),
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

    /// Move past this text if it is next, ignoring the case of ASCII letters
    /// (megaparsec's string').
    pub fn eat_str_ci(&mut self, s: &str) -> bool {
        let rest = self.rest().as_bytes();
        if rest.len() >= s.len() && rest[..s.len()].eq_ignore_ascii_case(s.as_bytes()) {
            self.pos += s.len();
            true
        } else {
            false
        }
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

    #[test]
    fn cursor() {
        let mut c = Cursor::new("Every  2 weeks");
        assert!(c.eat_str_ci("every"));
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
