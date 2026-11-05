//! Text layout helpers (Hledger.Utils.Text, Hledger.Utils.String and
//! Text.WideString): display widths, fitting text to a width, quoting, and
//! splitting into quote-aware words.
//!
//! Widths are measured as hledger measures them, with doclayout's realLength
//! (in its narrow context, where East Asian ambiguous characters are narrow).
//! That is the unicode-width crate's width plus doclayout's two exceptions:
//! control characters are zero width, and a combining mark at the start of
//! the text counts one column. Measured over every assigned code point,
//! single characters then differ from doclayout 0.6 only in 339 characters of
//! complex scripts (such as Hangul jamo and some Indic vowel signs), none of
//! which occur in hledger's tests.

use std::sync::OnceLock;

use ::regex::Regex;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Text and its width in columns, built up piece by piece (WideBuilder).
/// Each piece's width is given by whoever adds it: usually its display width,
/// but renderers may count some characters differently.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Wide {
    pub s: String,
    pub width: usize,
}

impl Wide {
    /// Text with its display width (wbFromText).
    pub fn new(s: &str) -> Wide {
        Wide {
            s: s.to_string(),
            width: display_width(s),
        }
    }

    /// Append text counted as this many columns.
    pub fn push_str(&mut self, s: &str, width: usize) {
        self.s.push_str(s);
        self.width += width;
    }

    /// Append another piece (WideBuilder's `<>`).
    pub fn push(&mut self, other: &Wide) {
        self.push_str(&other.s, other.width);
    }
}

/// The number of columns the text occupies on a terminal (doclayout's
/// realLength): East Asian wide characters and emoji count two, emoji
/// sequences two in all, combining marks nothing unless they come first,
/// and control characters nothing.
pub fn display_width(s: &str) -> usize {
    if s.is_ascii() {
        return s.bytes().filter(|b| !b.is_ascii_control()).count();
    }
    let width: usize = s.split(char::is_control).map(UnicodeWidthStr::width).sum();
    match s.chars().next() {
        Some(c) if c != '\u{FE0F}' && c.width() == Some(0) && is_combining_mark(c) => width + 1,
        _ => width,
    }
}

/// Whether this is a nonspacing or enclosing mark (doclayout's combining
/// characters).
fn is_combining_mark(c: char) -> bool {
    static MARK: OnceLock<Regex> = OnceLock::new();
    let mark = MARK.get_or_init(|| Regex::new(r"\A[\p{Mn}\p{Me}]").expect("a valid regex"));
    mark.is_match(c.encode_utf8(&mut [0; 4]))
}

/// The width of a character on its own, not first in its text (doclayout's
/// charWidth).
fn char_width(c: char) -> usize {
    c.width().unwrap_or(0)
}

/// The widest line's display width, for text of several lines.
pub fn text_width(s: &str) -> usize {
    s.split_terminator('\n')
        .map(display_width)
        .max()
        .unwrap_or(0)
}

/// The longest start of the text whose characters' widths add up to at most
/// `width` (textTakeWidth): `take_width(3, "りんご")` is "り".
pub fn take_width(width: isize, s: &str) -> &str {
    let mut left = width;
    for (i, c) in s.char_indices() {
        let w = char_width(c) as isize;
        if left == 0 || w > left {
            return &s[..i];
        }
        left -= w;
    }
    s
}

/// Pad text with spaces to at least `min` columns, then clip it to at most
/// `max` columns, ending (or starting) with `..` if `ellipsis` is set. Both
/// happen on the right if `right` is set, otherwise on the left (fitText).
pub fn fit_text(
    min: Option<isize>,
    max: Option<isize>,
    ellipsis: bool,
    right: bool,
    s: &str,
) -> String {
    let mut t = s.to_string();
    if let Some(w) = min {
        let shortfall = w - display_width(&t) as isize;
        if shortfall > 0 {
            let pad = " ".repeat(shortfall as usize);
            t = if right { t + &pad } else { pad + &t };
        }
    }
    if let Some(w) = max
        && display_width(&t) as isize > w.max(0)
    {
        let dots = if ellipsis { ".." } else { "" };
        let keep = w - dots.len() as isize;
        t = if right {
            format!("{}{dots}", take_width(keep, &t))
        } else {
            let reversed: String = t.chars().rev().collect();
            let kept: String = take_width(keep, &reversed).chars().rev().collect();
            format!("{dots}{kept}")
        };
    }
    t
}

/// Shorten text longer than `width` characters to `width`, ending with `..`
/// (elideRight).
pub fn elide_right(width: isize, s: &str) -> String {
    if s.chars().count() as isize > width {
        let keep = (width - 2).max(0) as usize;
        s.chars().take(keep).chain("..".chars()).collect()
    } else {
        s.to_string()
    }
}

/// Shorten text longer than `width` characters to `width`, starting with `..`
/// (elideLeft).
pub fn elide_left(width: isize, s: &str) -> String {
    let len = s.chars().count();
    if len as isize > width {
        let keep = (width - 2).max(0) as usize;
        "..".chars()
            .chain(s.chars().skip(len.saturating_sub(keep)))
            .collect()
    } else {
        s.to_string()
    }
}

/// Haskell's isSpace: the ASCII white space characters, no-break space, and
/// the other Unicode space separators.
pub fn is_space(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t'..='\r' | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{202f}' | '\u{205f}' | '\u{3000}'
    )
}

/// Without leading and trailing white space.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// Without trailing newlines and carriage returns.
pub fn chomp(s: &str) -> &str {
    s.trim_end_matches(['\r', '\n'])
}

/// A string quoted for a POSIX shell command line if it needs that: in
/// single quotes (an apostrophe written `'\''`) if it has quotes, white
/// space or shell characters, `''` if it is empty (quoteForCommandLine).
pub fn quote_for_command_line(s: &str) -> String {
    if s.is_empty() {
        "''".to_string()
    } else if s
        .chars()
        .any(|c| "'\" \t\n\r<>(){}[]$&?#!~`*+\\".contains(c))
    {
        format!("'{}'", s.replace('\'', "'\\''"))
    } else {
        s.to_string()
    }
}

/// The text in double quotes, with its double quotes backslash-escaped, if it
/// contains a quote or white space (textQuoteIfNeeded).
pub fn quote_if_needed(s: &str) -> String {
    if s.contains(['\'', '"', ' ', '\t', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\\\""))
    } else {
        s.to_string()
    }
}

/// Like [`quote_if_needed`], but only for text containing white space and
/// not already enclosed in single or double quotes (quoteIfSpaced).
pub fn quote_if_spaced(s: &str) -> String {
    let enclosed = |q: char| s.chars().count() >= 2 && s.starts_with(q) && s.ends_with(q);
    if enclosed('\'') || enclosed('"') || !s.contains([' ', '\t', '\n', '\r']) {
        s.to_string()
    } else {
        quote_if_needed(s)
    }
}

/// Without the single or double quotes enclosing it, if it has them
/// (textStripQuotes).
pub fn strip_quotes(s: &str) -> &str {
    let enclosed = |q: char| s.len() >= 2 && s.starts_with(q) && s.ends_with(q);
    if enclosed('\'') || enclosed('"') {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// A number with this many decimal places, as Haskell's printf `%.Nf`
/// shows a Double or Float: its shortest decimal digits, rounded half to
/// even (showFFloat, roundTo). Rust's `{:.N}` rounds the exact binary
/// value instead, so 0.35 gives 0.3 there but 0.4 here.
pub fn show_ffloat<F: Into<f64> + std::fmt::LowerExp + Copy>(decimals: usize, x: F) -> String {
    let sign = if x.into() < 0.0 { "-" } else { "" };
    // the shortest digits (of an f32 or f64), and e such that
    // |x| = 0.DIGITS * 10^e (floatToDigits)
    let sci = format!("{x:e}");
    let sci = sci.trim_start_matches('-');
    let (mantissa, exp) = sci.split_once('e').unwrap_or((sci, "0"));
    let mut is: Vec<u8> = mantissa
        .bytes()
        .filter(u8::is_ascii_digit)
        .map(|b| b - b'0')
        .collect();
    let mut e = exp.parse::<isize>().unwrap_or(0) + 1;
    if is == [0] {
        e = 0;
    }
    // is rounded to d digits, and whether that carried into a new digit
    fn round_to(d: usize, is: &[u8]) -> (usize, Vec<u8>) {
        fn f(n: usize, even: bool, is: &[u8]) -> (u8, Vec<u8>) {
            match (n, is.split_first()) {
                (_, None) => (0, vec![0; n]),
                (0, Some((&x, rest))) => {
                    let up = x > 5 || x == 5 && !(even && rest.iter().all(|&r| r == 0));
                    (u8::from(up), vec![])
                }
                (_, Some((&i, rest))) => {
                    let (c, mut ds) = f(n - 1, i % 2 == 0, rest);
                    let i1 = c + i;
                    if i1 == 10 {
                        ds.insert(0, 0);
                        (1, ds)
                    } else {
                        ds.insert(0, i1);
                        (0, ds)
                    }
                }
            }
        }
        match f(d, true, is) {
            (0, ds) => (0, ds),
            (_, mut ds) => {
                ds.insert(0, 1);
                (1, ds)
            }
        }
    }
    let digits = |ds: &[u8]| ds.iter().map(|d| char::from(b'0' + d)).collect::<String>();
    let out = if e >= 0 {
        let (ei, ds) = round_to(decimals + e as usize, &is);
        let (ls, rs) = ds.split_at((e as usize + ei).min(ds.len()));
        let ls = if ls.is_empty() {
            "0".to_string()
        } else {
            digits(ls)
        };
        if rs.is_empty() {
            ls
        } else {
            format!("{ls}.{}", digits(rs))
        }
    } else {
        let mut padded = vec![0; e.unsigned_abs()];
        padded.append(&mut is);
        let (ei, mut ds) = round_to(decimals, &padded);
        if ei == 0 {
            ds.insert(0, 0);
        }
        let (d, rest) = ds.split_at(1);
        if rest.is_empty() {
            digits(d)
        } else {
            format!("{}.{}", digits(d), digits(rest))
        }
    };
    format!("{sign}{out}")
}

/// Text as Haskell shows a string, in double quotes with escapes: `\"`,
/// `\\`, `\n` and the like for control characters, and decimal codes for
/// characters beyond ASCII, `\&` separating a code from a following digit
/// (show).
pub fn show_string(s: &str) -> String {
    const ASCII: [&str; 32] = [
        "NUL", "SOH", "STX", "ETX", "EOT", "ENQ", "ACK", "a", "b", "t", "n", "v", "f", "r", "SO",
        "SI", "DLE", "DC1", "DC2", "DC3", "DC4", "NAK", "SYN", "ETB", "CAN", "EM", "SUB", "ESC",
        "FS", "GS", "RS", "US",
    ];
    let mut out = String::from("\"");
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        let next = chars.peek().copied();
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{7f}' => out.push_str("\\DEL"),
            c if c > '\u{7f}' => {
                out.push_str(&format!("\\{}", u32::from(c)));
                if next.is_some_and(|n| n.is_ascii_digit()) {
                    out.push_str("\\&");
                }
            }
            c if c >= ' ' => out.push(c),
            c => {
                out.push('\\');
                out.push_str(ASCII[c as usize]);
                if c == '\u{e}' && next == Some('H') {
                    out.push_str("\\&");
                }
            }
        }
    }
    out.push('"');
    out
}

/// Split text into words at white space, as a shell does: white space inside
/// single or double quotes does not split, and quotes enclosing a word or part
/// of one are removed. None if a quote is not closed (wordsmay, words').
///
/// As upstream: words end at a newline, and the rest of the text is ignored;
/// leading or trailing white space makes an empty first or last word.
pub fn words_quoted(s: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    if s.is_empty() {
        return Some(words);
    }
    let mut rest = s;
    loop {
        let mut word = String::new();
        loop {
            match rest.chars().next() {
                Some(q @ ('\'' | '"')) => {
                    let close = rest[1..].find(q)?;
                    word.push_str(&rest[1..1 + close]);
                    rest = &rest[close + 2..];
                }
                Some(c) if !matches!(c, ' ' | '\t' | '\n' | '\r') => {
                    let end = rest
                        .find(['\'', '"', ' ', '\t', '\n', '\r'])
                        .unwrap_or(rest.len());
                    word.push_str(&rest[..end]);
                    rest = &rest[end..];
                }
                _ => break,
            }
        }
        words.push(word);
        let spaces = rest.len() - rest.trim_start_matches(|c| c != '\n' && is_space(c)).len();
        if spaces == 0 {
            return Some(words);
        }
        rest = &rest[spaces..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Values and results from GHC 9.10's Text.Printf.
    #[test]
    fn ffloat_as_printf() {
        for (d, x, s) in [
            (1, 0.25, "0.2"),
            (1, 0.35, "0.4"),
            (1, 0.05, "0.0"),
            (1, 0.45, "0.4"),
            (1, 2.5, "2.5"),
            (0, 0.5, "0"),
            (0, 2.5, "2"),
            (2, 1.005, "1.00"),
            (1, 0.0, "0.0"),
            (1, 9.96, "10.0"),
            (1, 0.96, "1.0"),
            (1, 0.04, "0.0"),
            (1, 1.0 / 3.0, "0.3"),
            (1, 2.0 / 3.0, "0.7"),
            (1, 100.0, "100.0"),
            (1, 1e-5, "0.0"),
            (1, -0.35, "-0.4"),
            (2, 0.125, "0.12"),
            (2, 0.375, "0.38"),
            (0, 1.5, "2"),
            (1, 0.15, "0.2"),
            (1, 1234567.25, "1234567.2"),
            (1, 0.95, "1.0"),
            (1, 5.0 / 30.0, "0.2"),
            (2, 1.0 / 7.0, "0.14"),
        ] {
            assert_eq!(show_ffloat(d, x), s, "{d} {x}");
        }
        // a Float's own shortest digits: 0.35f32 is 0.3499999940 as a Double
        assert_eq!(show_ffloat(1, 0.35f32), "0.4");
        assert_eq!(show_ffloat(1, f64::from(0.35f32)), "0.3");
        assert_eq!(show_ffloat(2, 0.125f32), "0.12");
    }

    #[test]
    fn display_widths() {
        assert_eq!(display_width(""), 0);
        assert_eq!(display_width("abc"), 3);
        assert_eq!(display_width("🎅"), 2);
        assert_eq!(display_width("中"), 2);
        assert_eq!(display_width("e\u{301}"), 1);
        assert_eq!(display_width("\u{301}"), 1);
        assert_eq!(display_width("\u{301}\u{301}"), 1);
        assert_eq!(display_width("\t"), 0);
        assert_eq!(display_width("a\u{AD}b"), 2);
        assert_eq!(display_width("👍🏽"), 2);
        assert_eq!(display_width("👨\u{200D}👩\u{200D}👧"), 2);
        assert_eq!(display_width("☺\u{FE0F}"), 2);
        assert_eq!(display_width("\u{FE0F}"), 0);
        assert_eq!(display_width("\u{200B}x"), 1);
        assert_eq!(display_width("\r\n"), 0);
        assert_eq!(display_width("\u{1b}[31m-1\u{1b}[m"), 8);
        assert_eq!(display_width("\t\u{301}"), 0);
        assert_eq!(display_width("りんご"), 6);
        assert_eq!(display_width("€"), 1);
    }

    #[test]
    fn wide_builders() {
        let mut w = Wide::new("中");
        w.push_str(", ", 2);
        w.push(&Wide::new("x"));
        assert_eq!(
            w,
            Wide {
                s: "中, x".into(),
                width: 5
            }
        );
        assert_eq!(Wide::default().width, 0);
    }

    #[test]
    fn widths_of_lines() {
        assert_eq!(text_width(""), 0);
        assert_eq!(text_width("ab\n中中\n"), 4);
        assert_eq!(text_width("a\n\nabc"), 3);
    }

    #[test]
    fn take_by_width() {
        assert_eq!(take_width(3, "りんご"), "り");
        assert_eq!(take_width(4, "りんご"), "りん");
        assert_eq!(take_width(0, "\u{301}a"), "");
        assert_eq!(take_width(1, "a\u{301}b"), "a");
        assert_eq!(take_width(2, "a\u{301}b"), "a\u{301}b");
        assert_eq!(take_width(-1, "abc"), "");
        assert_eq!(take_width(9, "abc"), "abc");
    }

    // tests_Text "fitText", and more.
    #[test]
    fn fitting_text() {
        assert_eq!(fit_text(None, Some(-2), true, true, ""), "");
        assert_eq!(fit_text(None, Some(0), true, true, ""), "");
        assert_eq!(fit_text(None, Some(6), true, true, "Test Text"), "Test..");
        assert_eq!(fit_text(None, Some(6), true, false, "Test Text"), "..Text");
        assert_eq!(fit_text(None, Some(6), false, true, "Test Text"), "Test T");
        assert_eq!(fit_text(Some(5), None, false, true, "ab"), "ab   ");
        assert_eq!(fit_text(Some(5), None, false, false, "ab"), "   ab");
        assert_eq!(fit_text(Some(4), Some(4), true, true, "中中中"), "中..");
        assert_eq!(fit_text(Some(3), Some(3), true, false, "a中中"), "..");
        assert_eq!(fit_text(None, Some(1), true, true, "abc"), "..");
        assert_eq!(fit_text(Some(2), Some(9), true, true, "abc"), "abc");
    }

    #[test]
    fn eliding() {
        assert_eq!(elide_right(5, "abcdefg"), "abc..");
        assert_eq!(elide_right(7, "abcdefg"), "abcdefg");
        assert_eq!(elide_right(1, "abc"), "..");
        assert_eq!(elide_left(5, "abcdefg"), "..efg");
        assert_eq!(elide_left(9, "abc"), "abc");
        assert_eq!(elide_left(0, "abc"), "..");
    }

    #[test]
    fn spaces() {
        assert_eq!(strip(" \t a b \u{a0}\n"), "a b");
        assert_eq!(strip("\u{85}a\u{2028}"), "\u{85}a\u{2028}");
        assert_eq!(chomp("a\r\n\n"), "a");
        assert_eq!(chomp("a\n b"), "a\n b");
        assert!(is_space('\u{3000}') && is_space('\u{b}') && !is_space('\u{200b}'));
    }

    // tests_Text "quoteIfSpaced".
    #[test]
    fn quoting() {
        assert_eq!(quote_if_spaced("a'a"), "a'a");
        assert_eq!(quote_if_spaced("a\"a"), "a\"a");
        assert_eq!(quote_if_spaced("a a"), "\"a a\"");
        assert_eq!(quote_if_spaced("mimi's cafe"), "\"mimi's cafe\"");
        assert_eq!(quote_if_spaced("\"alex\" cafe"), "\"\\\"alex\\\" cafe\"");
        assert_eq!(quote_if_spaced("le'shan's cafe"), "\"le'shan's cafe\"");
        assert_eq!(
            quote_if_spaced("\"be'any's\" cafe"),
            "\"\\\"be'any's\\\" cafe\""
        );
        assert_eq!(quote_if_spaced("'a b'"), "'a b'");
        assert_eq!(quote_if_needed("a'b"), "\"a'b\"");
        assert_eq!(quote_if_needed("ab"), "ab");
    }

    #[test]
    fn haskell_strings() {
        assert_eq!(show_string("a b"), "\"a b\"");
        assert_eq!(show_string("x\"y\\z"), "\"x\\\"y\\\\z\"");
        assert_eq!(show_string("d\u{e9}penses"), "\"d\\233penses\"");
        assert_eq!(show_string("\u{e9}1"), "\"\\233\\&1\"");
        assert_eq!(
            show_string("\t\n\u{7}\u{0}\u{7f}'"),
            "\"\\t\\n\\a\\NUL\\DEL'\""
        );
        assert_eq!(show_string("\u{e}H\u{1b}"), "\"\\SO\\&H\\ESC\"");
        assert_eq!(strip_quotes("'a b'"), "a b");
        assert_eq!(strip_quotes("\"a\""), "a");
        assert_eq!(strip_quotes("'a\""), "'a\"");
        assert_eq!(strip_quotes("'"), "'");
        assert_eq!(strip_quotes("''"), "");
    }

    // The doctests of words' and wordsmay.
    #[test]
    fn quoted_words() {
        let words = |s: &str| words_quoted(s).unwrap();
        assert_eq!(words("a b"), ["a", "b"]);
        assert_eq!(words("'a b' c"), ["a b", "c"]);
        assert_eq!(words("date:'1 to 15' x"), ["date:1 to 15", "x"]);
        assert_eq!(words("\"it's\""), ["it's"]);
        assert_eq!(words("a '' b"), ["a", "", "b"]);
        assert_eq!(words_quoted("an unclosed 'quote"), None);
        // And upstream's edge cases.
        assert_eq!(words(""), Vec::<String>::new());
        assert_eq!(words(" a  b "), ["", "a", "b", ""]);
        assert_eq!(words("a\tb\nc d"), ["a", "b"]);
        assert_eq!(words("a\u{a0}b"), ["a\u{a0}b"]);
        assert_eq!(words("x'a'\"b\"y z"), ["xaby", "z"]);
        assert_eq!(words("a\r\nb"), ["a", ""]);
    }
}
