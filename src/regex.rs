//! Regular expressions (Hledger.Utils.Regex), in regex-tdfa's syntax, run by
//! the regex crate.
//!
//! hledger's regular expressions are POSIX extended regular expressions as
//! regex-tdfa 1.3 reads them (Text.Regex.TDFA.ReadRegex, with the
//! defaultCompOpt options hledger uses). Each pattern is parsed with that
//! grammar, so the patterns regex-tdfa rejects are rejected here too (an
//! empty pattern or alternative, `a**`, `(?i)`, `{1}`), and rewritten in the
//! regex crate's syntax with regex-tdfa's meanings:
//!
//! - a backslash makes any character literal (`\.`, `\/`, and also `\s`, `\d`,
//!   `\n`, which are the letters s, d, n), except `` \` `` and `\'` (start and
//!   end of the text), `\<` and `\>` (start and end of a word), and `\b` and
//!   `\B` (a word boundary or not), where words are ASCII letters, digits and
//!   underscores;
//! - `{` starts a repetition bound (`{2}`, `{2,}`, `{2,5}`) only when a digit
//!   follows it, and is otherwise literal;
//! - in a bracket expression a backslash is literal, a `]` first is literal,
//!   the classes `[:alpha:]` etc. are ASCII, and a negated expression does not
//!   match a newline;
//! - `^` and `$` also match at the start and end of each line, and `.` does
//!   not match a newline.
//!
//! The main difference remaining: regex-tdfa chooses the leftmost-longest
//! match, as POSIX specifies, and the regex crate the leftmost-first, so they
//! can find different matches (and capture groups) when alternatives overlap:
//! `a|ab` on "ab" matches "ab" in hledger and "a" here. Smaller ones: without
//! case sensitivity, letters match by Unicode simple case folding rather than
//! regex-tdfa's toUpper/toLower; and when replacing, the regex crate skips an
//! empty match that immediately follows another match (`a*` on "baac" matches
//! at 0, 1-3 and 4), which regex-tdfa may not (unverified).

use std::collections::HashMap;
use std::fmt::Write;
use std::sync::{Arc, Mutex, OnceLock};

use ::regex::{Captures, Regex, RegexBuilder};

/// A compiled regular expression, case-sensitive or not. Two are equal when
/// they were made from the same text with the same case sensitivity.
#[derive(Clone, Debug)]
pub struct Regexp {
    src: Arc<str>,
    ci: bool,
    /// When the pattern is plain ASCII text, with no character special to
    /// a regular expression, the text to look for (lower-cased if case is
    /// ignored): finding it needs no compiled expression, which costs a
    /// millisecond or more to make.
    plain: Option<Box<str>>,
    /// The compiled expression; for plain text, made when first needed.
    re: OnceLock<Regex>,
}

/// A case-sensitive regular expression from this text, or an error message.
pub fn to_regex(src: &str) -> Result<Regexp, String> {
    cached(src, false)
}

/// A case-insensitive regular expression from this text, or an error message.
pub fn to_regex_ci(src: &str) -> Result<Regexp, String> {
    cached(src, true)
}

/// Each distinct pattern is compiled once per process, as upstream memoises
/// toRegex and toRegexCI.
fn cached(src: &str, ci: bool) -> Result<Regexp, String> {
    type Cache = [HashMap<Arc<str>, Result<Regexp, String>>; 2];
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let mut caches = CACHE.get_or_init(Default::default).lock().unwrap();
    let cache = &mut caches[usize::from(ci)];
    if let Some(r) = cache.get(src) {
        return r.clone();
    }
    let src: Arc<str> = src.into();
    let r = translate(&src)
        .and_then(|pattern| {
            let plain = is_plain_text(&src).then(|| {
                if ci {
                    src.to_ascii_lowercase().into()
                } else {
                    src.as_ref().into()
                }
            });
            let re = OnceLock::new();
            if plain.is_none() {
                re.set(compile(&pattern, ci)?).ok();
            }
            Some(Regexp {
                src: src.clone(),
                ci,
                plain,
                re,
            })
        })
        .ok_or_else(|| {
            format!("This regular expression is invalid or unsupported, please correct it: {src}")
        });
    cache.insert(src, r.clone());
    r
}

/// A translated pattern compiled.
fn compile(pattern: &str, ci: bool) -> Option<Regex> {
    RegexBuilder::new(pattern)
        .case_insensitive(ci)
        .multi_line(true)
        .build()
        .ok()
}

/// Whether a pattern is plain text: one to 256 ASCII letters, digits and
/// characters that stand for themselves, which translate unchanged.
fn is_plain_text(src: &str) -> bool {
    (1..=256).contains(&src.len())
        && src
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b" :-_/@,;#%&=!'\"<>~".contains(&b))
}

/// Whether the text contains this lower-case ASCII text ignoring case, as
/// the regex crate compares without case sensitivity: ASCII letters, and the
/// long s and Kelvin sign, whose simple case folds are s and k.
fn contains_ignoring_case(text: &str, lower: &str) -> bool {
    let lower = lower.as_bytes();
    if text.is_ascii() {
        return (text.as_bytes().windows(lower.len())).any(|w| {
            w.iter()
                .zip(lower)
                .all(|(t, l)| t.to_ascii_lowercase() == *l)
        });
    }
    let folded: Vec<char> = (text.chars())
        .map(|c| match c {
            '\u{17F}' => 's',
            '\u{212A}' => 'k',
            c => c.to_ascii_lowercase(),
        })
        .collect();
    (folded.windows(lower.len())).any(|w| w.iter().zip(lower).all(|(c, l)| *c == *l as char))
}

impl Regexp {
    /// The text this was made from.
    pub fn src(&self) -> &str {
        &self.src
    }

    /// Whether this matches somewhere in the text.
    pub fn is_match(&self, s: &str) -> bool {
        match &self.plain {
            Some(p) if self.ci => contains_ignoring_case(s, p),
            Some(p) => s.contains(&**p),
            None => self.regex().is_match(s),
        }
    }

    /// The compiled expression.
    fn regex(&self) -> &Regex {
        self.re.get_or_init(|| {
            translate(&self.src)
                .and_then(|pattern| compile(&pattern, self.ci))
                .expect("a plain text compiles")
        })
    }

    /// The texts matched by the capture groups in the first match, an empty
    /// text for a group that took no part in it; none if nothing matches.
    pub fn match_groups(&self, s: &str) -> Vec<String> {
        match self.regex().captures(s) {
            None => Vec::new(),
            Some(caps) => caps
                .iter()
                .skip(1)
                .map(|g| g.map_or("", |g| g.as_str()).to_string())
                .collect(),
        }
    }

    /// Replace every match in the text with the replacement, in which `\N`
    /// is the text of capture group N (`\0` the whole match, empty if the group
    /// took no part); an error if a match meets a reference to a group the
    /// expression does not have (regexReplaceUnmemo).
    pub fn replace(&self, replacement: &str, s: &str) -> Result<String, String> {
        let mut out = String::with_capacity(s.len());
        let mut done = 0;
        for caps in self.regex().captures_iter(s) {
            let m = caps.get(0).expect("group 0 is the match");
            out.push_str(&s[done..m.start()]);
            expand(replacement, &caps, &mut out)?;
            done = m.end();
        }
        out.push_str(&s[done..]);
        Ok(out)
    }
}

/// Append the replacement for one match, with its backreferences replaced.
fn expand(replacement: &str, caps: &Captures, out: &mut String) -> Result<(), String> {
    let mut rest = replacement;
    while let Some(i) = rest.find('\\') {
        let after = &rest[i + 1..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            out.push_str(&rest[..=i]);
            rest = after;
            continue;
        }
        out.push_str(&rest[..i]);
        let number = &after[..digits];
        match number.parse::<usize>().ok().filter(|&n| n < caps.len()) {
            Some(n) => out.push_str(caps.get(n).map_or("", |g| g.as_str())),
            None => {
                return Err(format!(
                    "no match group exists for backreference \"\\{number}\""
                ));
            }
        }
        rest = &after[digits..];
    }
    out.push_str(rest);
    Ok(())
}

impl PartialEq for Regexp {
    fn eq(&self, other: &Regexp) -> bool {
        self.ci == other.ci && self.src == other.src
    }
}

impl Eq for Regexp {}

/// Case-sensitive expressions first, then by their text.
impl Ord for Regexp {
    fn cmp(&self, other: &Regexp) -> std::cmp::Ordering {
        (self.ci, &self.src).cmp(&(other.ci, &other.src))
    }
}

impl PartialOrd for Regexp {
    fn partial_cmp(&self, other: &Regexp) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// This text with the characters special in a regular expression escaped,
/// so that it matches itself: `U$` becomes `U\$`.
pub fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\.^$*+?()[]{}|".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A regex-tdfa pattern in the regex crate's syntax, or None if regex-tdfa
/// would not parse it.
fn translate(src: &str) -> Option<String> {
    let mut t = Translator {
        cs: src.chars().collect(),
        i: 0,
        out: String::with_capacity(src.len() * 2),
    };
    t.alternatives()?;
    (t.i == t.cs.len()).then_some(t.out)
}

/// A recursive descent parser for ReadRegex's grammar, writing its output
/// as it goes.
struct Translator {
    cs: Vec<char>,
    i: usize,
    out: String,
}

impl Translator {
    fn peek(&self, ahead: usize) -> Option<char> {
        self.cs.get(self.i + ahead).copied()
    }

    /// branch ('|' branch)*
    fn alternatives(&mut self) -> Option<()> {
        self.branch()?;
        while self.peek(0) == Some('|') {
            self.i += 1;
            self.out.push('|');
            self.branch()?;
        }
        Some(())
    }

    /// One or more pieces: an anchor or atom, then at most one of `? + *`
    /// or a bound.
    fn branch(&mut self) -> Option<()> {
        let mut pieces = 0;
        while self.atom()? {
            pieces += 1;
            self.repetition();
        }
        (pieces > 0).then_some(())
    }

    /// An anchor or atom if one starts here (Some(true)); none (Some(false));
    /// or a syntax error (None).
    fn atom(&mut self) -> Option<bool> {
        let Some(c) = self.peek(0) else {
            return Some(false);
        };
        let digit_next = self.peek(1).is_some_and(|d| d.is_ascii_digit());
        if matches!(c, '|' | ')' | '*' | '+' | '?') || (c == '{' && digit_next) {
            return Some(false);
        }
        self.i += 1;
        match c {
            '{' => self.out.push_str(r"\{"),
            '^' | '$' | '.' => self.out.push(c),
            '(' if self.peek(0) == Some(')') => {
                self.i += 1;
                self.out.push_str("()");
            }
            '(' => {
                self.out.push('(');
                self.alternatives()?;
                if self.peek(0) != Some(')') {
                    return None;
                }
                self.i += 1;
                self.out.push(')');
            }
            '[' => self.bracket()?,
            '\\' => {
                let e = self.peek(0)?;
                self.i += 1;
                match e {
                    '`' => self.out.push_str(r"\A"),
                    '\'' => self.out.push_str(r"\z"),
                    '<' => self.out.push_str(r"(?-u:\b{start})"),
                    '>' => self.out.push_str(r"(?-u:\b{end})"),
                    'b' => self.out.push_str(r"(?-u:\b)"),
                    'B' => self.out.push_str(r"(?-u:\B)"),
                    e => literal(e, &mut self.out),
                }
            }
            c => literal(c, &mut self.out),
        }
        Some(true)
    }

    /// An optional `?`, `+`, `*` or bound `{N}`, `{N,}`, `{N,M}` (N <= M).
    fn repetition(&mut self) {
        match self.peek(0) {
            Some(c @ ('?' | '+' | '*')) => {
                self.i += 1;
                self.out.push(c);
            }
            Some('{') => {
                let body: String = self.cs[self.i + 1..]
                    .iter()
                    .take_while(|&&c| c != '}')
                    .collect();
                let close = self.i + 1 + body.chars().count();
                if close < self.cs.len() && is_bound(&body) {
                    self.out.push('{');
                    self.out.push_str(&body);
                    self.out.push('}');
                    self.i = close + 1;
                }
            }
            _ => {}
        }
    }

    /// The rest of a bracket expression, after its `[`.
    fn bracket(&mut self) -> Option<()> {
        let negated = self.peek(0) == Some('^');
        if negated {
            self.i += 1;
        }
        let mut ranges: Vec<(char, char)> = Vec::new();
        if self.peek(0) == Some(']') {
            self.i += 1;
            ranges.push((']', ']'));
        }
        let first = self.i;
        loop {
            let c = self.peek(0)?;
            if c == ']' && (self.i > first || !ranges.is_empty()) {
                self.i += 1;
                break;
            }
            if let Some((kind, name, len)) = self.bracketed(c) {
                match kind {
                    ':' => ranges.extend(posix_class(&name)),
                    '=' => ranges.extend(name.chars().map(|c| (c, c))),
                    // regex-tdfa ignores collating elements.
                    _ => {}
                }
                self.i += len;
            } else if self.peek(1) == Some('-') && self.peek(2).is_some_and(|e| e != ']') {
                let end = self.peek(2)?;
                if c > end {
                    return None;
                }
                ranges.push((c, end));
                self.i += 3;
            } else {
                ranges.push((c, c));
                self.i += 1;
            }
        }
        self.out.push('[');
        if negated {
            self.out.push_str(r"^\n");
        } else if ranges.is_empty() {
            // Only collating elements: a set that matches nothing.
            self.out.push_str(r"^\x{0}-\x{10FFFF}");
        }
        for (a, b) in ranges {
            literal(a, &mut self.out);
            if a != b {
                self.out.push('-');
                literal(b, &mut self.out);
            }
        }
        self.out.push(']');
        Some(())
    }

    /// At `[` followed by `:`, `=` or `.` (the kind), a `[:name:]`, `[=name=]`
    /// or `[.name.]` element: its kind, name and length.
    fn bracketed(&self, c: char) -> Option<(char, String, usize)> {
        let kind = self.peek(1)?;
        if c != '[' || !matches!(kind, ':' | '=' | '.') {
            return None;
        }
        let name: String = self.cs[self.i + 2..]
            .iter()
            .take_while(|&&n| n != kind && n != ']')
            .collect();
        let end = self.i + 2 + name.chars().count();
        let closed = self.cs.get(end) == Some(&kind) && self.cs.get(end + 1) == Some(&']');
        (!name.is_empty() && closed).then(|| (kind, name, end + 2 - self.i))
    }
}

/// Whether this is the inside of a repetition bound: `N`, `N,` or `N,M`
/// with N <= M.
fn is_bound(body: &str) -> bool {
    let number = |s: &str| -> Option<u64> {
        let digits = !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        digits.then(|| s.parse().ok()).flatten()
    };
    match body.split_once(',') {
        None => number(body).is_some(),
        Some((low, "")) => number(low).is_some(),
        Some((low, high)) => matches!((number(low), number(high)), (Some(l), Some(h)) if l <= h),
    }
}

/// The characters of a POSIX class as regex-tdfa defines them (all ASCII);
/// an unknown class has none.
fn posix_class(name: &str) -> Vec<(char, char)> {
    let ranges: &[(char, char)] = match name {
        "alnum" => &[('0', '9'), ('A', 'Z'), ('a', 'z')],
        "digit" => &[('0', '9')],
        "punct" => &[('!', '/'), (':', '@'), ('[', '`'), ('{', '~')],
        "alpha" => &[('A', 'Z'), ('a', 'z')],
        "graph" => &[('!', '~')],
        "space" => &[('\t', '\r'), (' ', ' ')],
        "blank" => &[('\t', '\t'), (' ', ' ')],
        "lower" => &[('a', 'z')],
        "upper" => &[('A', 'Z')],
        "cntrl" => &[('\0', '\x1f'), ('\x7f', '\x7f')],
        "print" => &[(' ', '~')],
        "xdigit" => &[('0', '9'), ('A', 'F'), ('a', 'f')],
        "word" => &[('0', '9'), ('A', 'Z'), ('_', '_'), ('a', 'z')],
        _ => &[],
    };
    ranges.to_vec()
}

/// A character matching itself, in or out of a class.
fn literal(c: char, out: &mut String) {
    if c.is_alphanumeric() {
        out.push(c);
    } else {
        write!(out, r"\x{{{:X}}}", u32::from(c)).expect("writing to a String");
    }
}

pub(crate) mod tests {
    use super::*;

    /// Plain text is found as the compiled expression finds it, with and
    /// without case sensitivity, in ASCII and other text.
    #[test]
    fn plain_text_matches_as_compiled() {
        let texts = [
            "assets:investments",
            "Assets:Investments:Brokerage",
            "ASSETS:INVESTMENTS",
            "aſſets:inveſtments",
            "ban\u{212A}",
            "BANK",
            "assets:\u{130}nvestments",
            "x assets:investmentsy",
            "assets:investment",
            "",
            "a\nassets:investments",
            "ассеты:assets:investments",
            "kelvin \u{212A}elvin",
            "income:investments",
            "a-b_c/d@e,f;g#h%i&j=k!l'm\"n<o>p~q r:s",
            "A-B_C/D@E,F;G#H%I&J=\u{212A}!L'M\"N<O>P~Q R:\u{17F}",
            "a-b_c/d@e,f;g#h%i&j=k!l'm\"n<o>p~q r:t",
        ];
        for pat in [
            "a-b_c/d@e,f;g#h%i&j=k!l'm\"n<o>p~q r:s",
            "assets:investments",
            "INVESTMENTS",
            "bank",
            "k",
            "s",
            "a b",
            "kelvin",
        ] {
            assert!(is_plain_text(pat));
            for ci in [false, true] {
                let r = cached(pat, ci).unwrap();
                let compiled = compile(&translate(pat).unwrap(), ci).unwrap();
                for t in texts {
                    assert_eq!(r.is_match(t), compiled.is_match(t), "{pat:?} {ci} {t:?}");
                }
            }
        }
        for special in [
            "a.b", "a|b", "^a", "a$", "(a)", "a*", "", "a\\b", "[a]", "a{2}", "a+",
        ] {
            assert!(!is_plain_text(special), "{special:?}");
        }
    }

    fn re(s: &str) -> Regexp {
        to_regex(s).unwrap()
    }

    #[cfg(test)]
    fn invalid(s: &str) -> bool {
        to_regex(s).is_err()
    }

    // The doctests of regexReplaceUnmemo.
    #[cfg_attr(test, test)]
    pub(crate) fn replace() {
        assert_eq!(re("a").replace("b", "banana"), Ok("bbnbnb".into()));
        assert_eq!(
            re("([0-9]+)-([0-9]+)").replace(r"\2/\1", "x 1-2 y 30-40"),
            Ok("x 2/1 y 40/30".into())
        );
        assert_eq!(re("a(x)?").replace(r"[\1]", "a ax"), Ok("[] [x]".into()));
        assert_eq!(re("x*").replace("-", "abc"), Ok("-a-b-c-".into()));
        assert_eq!(
            re("a").replace(r"\1", "a"),
            Err(r#"no match group exists for backreference "\1""#.into())
        );
    }

    #[test]
    fn replacement_backreferences() {
        assert_eq!(re("b+").replace(r"<\0>", "abbc"), Ok("a<bb>c".into()));
        assert_eq!(re("(b)").replace(r"\\1", "abc"), Ok(r"a\bc".into()));
        assert_eq!(re("(b)").replace(r"\x\", "abc"), Ok(r"a\x\c".into()));
        assert_eq!(
            re("(b)").replace(r"\12", "abc"),
            Err(r#"no match group exists for backreference "\12""#.into())
        );
        // No match, so no error from the bad reference.
        assert_eq!(re("z").replace(r"\9", "abc"), Ok("abc".into()));
    }

    #[test]
    fn errors_name_the_expression() {
        assert_eq!(
            to_regex("(").unwrap_err(),
            "This regular expression is invalid or unsupported, please correct it: ("
        );
        // From the corpus: account-aliases.test 21 and query-expr.test 9.
        assert!(invalid("{202[2-5]"));
        assert!(invalid("not(tag:transactiontag=B"));
    }

    #[test]
    fn what_regex_tdfa_rejects() {
        for s in [
            "", "a|", "|a", "a||b", "(|a)", "a**", "a+?", "*a", "(?i)a", "{1}", "a{1", "a{3,1}",
            "a{1x}", "[a", "[]", "[z-a]", "a)", "a\\", "x{2}{3}",
        ] {
            assert!(invalid(s), "{s:?}");
        }
        for s in [
            "()", "a()b", "^*", "a{,3}", "a{,3", "a{", "x{2,}", "x{02}", "[]a]", "[a-]", "}", "]",
        ] {
            assert!(!invalid(s), "{s:?}");
        }
    }

    #[test]
    fn backslash_escapes() {
        assert!(re(r"\/").is_match("a/b"));
        assert!(re(r"\.").is_match("a.b") && !re(r"\.").is_match("ab"));
        assert!(re(r"\\").is_match(r"a\b"));
        assert!(re(r"\s").is_match("s") && !re(r"\s").is_match(" "));
        assert!(re(r"\d").is_match("d") && !re(r"\d").is_match("1"));
        assert!(re(r"\n").is_match("n") && !re(r"\n").is_match("\n"));
        assert!(re(r"\`a").is_match("ab") && !re(r"\`b").is_match("ab"));
        assert!(re(r"b\'").is_match("ab") && !re(r"a\'").is_match("ab"));
    }

    #[test]
    fn word_boundaries_are_ascii() {
        let w = re(r"\<2\>");
        assert!(w.is_match("a 2 b") && !w.is_match("a 22 b") && !w.is_match("x2"));
        assert!(re(r"\bab\b").is_match("x ab.") && !re(r"\bab\b").is_match("xab"));
        assert!(re(r"\Bb").is_match("ab") && !re(r"\Bb").is_match("a b"));
        // é is not a word character for regex-tdfa.
        assert!(re(r"\<b").is_match("éb") && !re(r"\<é").is_match("é"));
    }

    #[test]
    fn braces() {
        assert!(re("{DATE}").is_match("x{DATE}"));
        assert!(re("a{2}").is_match("aa") && !re("a{2}").is_match("a"));
        assert!(re("^a{2,}$").is_match("aaa"));
        assert!(re("^a{1,2}$").is_match("aa") && !re("^a{1,2}$").is_match("aaa"));
        assert!(re("a{,3}").is_match("a{,3}"));
        assert!(re("a}").is_match("a}"));
    }

    #[test]
    fn bracket_expressions() {
        assert!(!re(r#"[^"]"#).is_match("\n") && re(r#"[^"]"#).is_match("x"));
        assert!(
            re(r"[\d]").is_match(r"\") && re(r"[\d]").is_match("d") && !re(r"[\d]").is_match("1")
        );
        assert!(re("[]a]").is_match("]") && re("[]a]").is_match("a"));
        assert!(re("[^]a]").is_match("b") && !re("[^]a]").is_match("]"));
        assert!(re("[a-]").is_match("-"));
        assert!(re("[--/]").is_match(".") && !re("[--/]").is_match("a"));
        assert!(re("[[:space:]]").is_match("\t") && !re("[[:space:]]").is_match("a"));
        assert!(re("[[:alpha:]_]").is_match("_") && !re("[[:alpha:]]").is_match("é"));
        assert!(re("[[:nosuch:]x]").is_match("x") && !re("[[:nosuch:]x]").is_match("n"));
        assert!(re("[[=e=]]").is_match("e"));
        assert!(!re("[[.e.]]").is_match("e"));
        assert!(re("[a&&b]").is_match("&") && re("[a~~b]").is_match("~"));
        assert!(re("[[a]").is_match("[") && !re("[[a]").is_match("b"));
    }

    #[test]
    fn lines_and_dots() {
        assert!(re("^b$").is_match("a\nb\nc"));
        assert!(!re("a.b").is_match("a\nb"));
    }

    #[test]
    fn case_sensitivity() {
        assert!(!re("ABC").is_match("abc"));
        assert!(to_regex_ci("ABC").unwrap().is_match("xabcx"));
        assert!(to_regex_ci("[a-c]").unwrap().is_match("B"));
    }

    #[test]
    fn identity_and_order() {
        assert_eq!(re("a"), re("a"));
        assert_ne!(re("a"), to_regex_ci("a").unwrap());
        assert!(re("b") < to_regex_ci("a").unwrap());
        assert!(re("a") < re("b"));
        assert_eq!(re("a.c").src(), "a.c");
        assert_eq!(to_regex("(").unwrap_err(), to_regex("(").unwrap_err());
    }

    #[test]
    fn match_groups() {
        assert_eq!(re("a(x)?(b)").match_groups("zab"), ["", "b"]);
        assert_eq!(re("(a)(b)").match_groups("ab ab"), ["a", "b"]);
        assert!(re("(a)").match_groups("z").is_empty());
        assert!(re("a").match_groups("a").is_empty());
    }

    #[test]
    fn escape() {
        assert_eq!(regex_escape("U$"), r"U\$");
        let nasty = r"a.b*c+d?e(f)g[h]i{j}k|l^m$n\o";
        assert!(re(&regex_escape(nasty)).is_match(nasty));
        assert!(!re(&regex_escape("a.b")).is_match("axb"));
    }

    // The regex crate takes the leftmost-first alternative where regex-tdfa
    // takes the longest; this pins the documented difference.
    #[test]
    fn leftmost_first() {
        assert_eq!(re("a|ab").replace("X", "ab"), Ok("Xb".into()));
    }
}
