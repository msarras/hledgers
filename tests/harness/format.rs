//! Parser for shelltestrunner test files, ported from shelltestrunner's Parse.hs:
//! format 3, and the deprecated format 1 that one corpus file (balance/87.test)
//! uses. Format 2 (`$$$` commands) is not used by the corpus and not supported.
//!
//! Format 3 rules, as implemented upstream:
//! - A file is a sequence of groups. A group starts with an input section: `<` on
//!   its own line, then every line up to the next line starting with `$` or `<`,
//!   verbatim. The first group's `<` may be omitted, making its input everything
//!   from the first line that is not blank or a comment up to the first `$` line.
//!   All the tests up to the next `<` share the group's input; a group without an
//!   input section gives no stdin.
//! - A test is `$ COMMAND`, then optional sections in this order: stdout (lines
//!   right after the command, or after a `>` line, or `> /REGEX/` / `> !/REGEX/`),
//!   stderr (`>2` then lines, or `>2 /REGEX/`), exit status (`>= N`, `>= !N`,
//!   `>= /REGEX/`, or `>=` alone for any status). Absent stdout and stderr must be
//!   empty, an absent status must be 0.
//! - An output section's lines end at a line starting with `>`, or where only
//!   blank and comment lines remain before a line starting with `$` or `<` or the
//!   end of the file. So trailing blank and comment lines are not output, but blank
//!   and comment lines followed by more output, or by `>2`/`>=`, are.
//! - A regex runs from `/` to the next unescaped `/`, possibly across lines (`\/`
//!   is a literal slash), and may be followed only by blanks or a comment.

/// What a test expects of one output stream or the exit status.
#[derive(Clone, Debug, PartialEq)]
pub enum Matcher {
    /// Exactly this text, each expected line ending with a newline.
    Lines(String),
    /// Text containing a match for this regex-tdfa (POSIX extended) regex.
    Regex(String),
    NotRegex(String),
    /// An exit status, as decimal text.
    Numeric(String),
    NotNumeric(String),
}

/// The status matcher used for `>=` with nothing after it.
pub fn any() -> Matcher {
    Matcher::Regex(String::new())
}

#[derive(Clone, Debug, PartialEq)]
pub struct Case {
    /// The line the command is on, from 1.
    pub line: usize,
    pub command: String,
    pub stdin: Option<String>,
    /// None means not checked (format 1 only).
    pub stdout: Option<Matcher>,
    pub stderr: Option<Matcher>,
    pub status: Matcher,
}

/// Parse a test file's text into its cases, numbered from 1 in file order.
pub fn parse(text: &str) -> Result<Vec<Case>, String> {
    let text = text.replace("\r\n", "\n");
    if text.lines().any(|l| l.starts_with("$$$")) {
        return Err("format 2 tests ($$$) are not supported".into());
    }
    let p = P { t: &text };
    match p.format3()? {
        Some(cases) => Ok(cases),
        None => p.format1(),
    }
}

struct P<'a> {
    t: &'a str,
}

impl P<'_> {
    fn starts(&self, at: usize, s: &str) -> bool {
        self.t[at..].starts_with(s)
    }

    fn line_number(&self, at: usize) -> usize {
        1 + self.t[..at].matches('\n').count()
    }

    fn error(&self, at: usize) -> String {
        let line = self.t[at..].lines().next().unwrap_or("");
        format!("line {}: could not parse: {line}", self.line_number(at))
    }

    /// A whole line, without its newline, and where the next starts. Like
    /// shelltestrunner's `line`, this needs a newline.
    fn line(&self, at: usize) -> Option<(&str, usize)> {
        let nl = self.t[at..].find('\n')?;
        Some((&self.t[at..at + nl], at + nl + 1))
    }

    fn skip_blanks(&self, mut at: usize) -> usize {
        while self.starts(at, " ") || self.starts(at, "\t") {
            at += 1;
        }
        at
    }

    /// A blank line, or a comment line (optional blanks, then `#`); not at the end.
    fn blank_or_comment_line(&self, at: usize) -> Option<usize> {
        if at == self.t.len() {
            return None;
        }
        let rest = self.skip_blanks(at);
        if self.starts(rest, "#") || self.starts(rest, "\n") || rest == self.t.len() {
            if rest == at && rest == self.t.len() {
                return None;
            }
            return Some(self.line(rest).map_or(self.t.len(), |(_, next)| next));
        }
        None
    }

    fn skip_blank_and_comment_lines(&self, mut at: usize) -> usize {
        while let Some(next) = self.blank_or_comment_line(at) {
            at = next;
        }
        at
    }

    /// What may follow a regex or status on its line: blanks, a comment, or the end.
    fn end_of_item(&self, at: usize) -> Option<usize> {
        if at == self.t.len() {
            Some(at)
        } else {
            self.blank_or_comment_line(at)
        }
    }

    /// `/REGEX/` or `!/REGEX/`.
    fn regex(&self, at: usize) -> Option<(Matcher, usize)> {
        let (negated, start) = if self.starts(at, "!/") {
            (true, at + 2)
        } else if self.starts(at, "/") {
            (false, at + 1)
        } else {
            return None;
        };
        let mut re = String::new();
        let mut cs = self.t[start..].char_indices().peekable();
        while let Some((i, c)) = cs.next() {
            match c {
                '/' => {
                    let next = self.end_of_item(start + i + 1)?;
                    let m = if negated {
                        Matcher::NotRegex(re)
                    } else {
                        Matcher::Regex(re)
                    };
                    return Some((m, next));
                }
                '\\' if cs.next_if(|&(_, d)| d == '/').is_some() => re.push('/'),
                c => re.push(c),
            }
        }
        None
    }

    /// A status matcher after `>=`/`>>>=`, defaulting to any status.
    fn status(&self, at: usize) -> (Matcher, usize) {
        let at = self.skip_blanks(at);
        if let Some(found) = self.regex(at) {
            return found;
        }
        let (negated, digits_at) = if self.starts(at, "!") {
            (true, at + 1)
        } else {
            (false, at)
        };
        let digits = self.t[digits_at..]
            .chars()
            .take_while(char::is_ascii_digit)
            .count();
        if digits > 0
            && let Some(next) = self.end_of_item(digits_at + digits)
        {
            let n = self.t[digits_at..digits_at + digits].to_string();
            let m = if negated {
                Matcher::NotNumeric(n)
            } else {
                Matcher::Numeric(n)
            };
            return (m, next);
        }
        (any(), at)
    }

    /// Lines up to one starting with one of `ends`, or the end of the file.
    fn lines_until(&self, mut at: usize, ends: &[&str]) -> Option<(String, usize)> {
        let mut text = String::new();
        while at < self.t.len() && !ends.iter().any(|e| self.starts(at, e)) {
            let (line, next) = self.line(at)?;
            text.push_str(line);
            text.push('\n');
            at = next;
        }
        Some((text, at))
    }

    // Format 3

    fn format3(&self) -> Result<Option<Vec<Case>>, String> {
        let mut cases = Vec::new();
        let mut at = 0;
        while let Some((group, next)) = self.group3(at, cases.is_empty()) {
            cases.extend(group);
            at = next;
        }
        if cases.is_empty() {
            return Ok(None);
        }
        at = self.skip_blank_and_comment_lines(at);
        if at < self.t.len() {
            return Err(self.error(at));
        }
        Ok(Some(cases))
    }

    fn group3(&self, at: usize, first: bool) -> Option<(Vec<Case>, usize)> {
        let mut at = self.skip_blank_and_comment_lines(at);
        let input_start = match self.line(at) {
            Some((l, next))
                if l.starts_with('<') && l[1..].trim_matches([' ', '\t']).is_empty() =>
            {
                Some(next)
            }
            _ if first => Some(at),
            _ => None,
        };
        let stdin = match input_start {
            Some(start) => {
                let (input, next) = self.lines_until(start, &["$", "<"])?;
                at = next;
                Some(input)
            }
            None => None,
        };
        let mut cases = Vec::new();
        while let Some((case, next)) = self.test3(at, &stdin) {
            cases.push(case);
            at = next;
        }
        (!cases.is_empty()).then_some((cases, at))
    }

    fn test3(&self, at: usize, stdin: &Option<String>) -> Option<(Case, usize)> {
        let at = self.skip_blank_and_comment_lines(at);
        if !self.starts(at, "$") {
            return None;
        }
        let (command, mut at) = self.line(at + 1)?;
        let command = command.strip_prefix(' ').unwrap_or(command);
        let command = command.trim_start_matches([' ', '\t']).to_string();
        let line = self.line_number(at) - 1;

        let mut stdout = Matcher::Lines(String::new());
        if let Some((m, next)) = self.stdout3(at) {
            stdout = m;
            at = next;
        }
        let mut stderr = Matcher::Lines(String::new());
        if self.starts(at, ">2") {
            let after = self.skip_blanks(at + 2);
            let found = self.regex(after).or_else(|| {
                self.starts(after, "\n")
                    .then(|| self.lines3(after + 1))
                    .flatten()
            });
            if let Some((m, next)) = found {
                stderr = m;
                at = next;
            }
        }
        let mut status = Matcher::Numeric("0".into());
        if self.starts(at, ">=") {
            (status, at) = self.status(at + 2);
        }
        let case = Case {
            line,
            command,
            stdin: stdin.clone(),
            stdout: Some(stdout),
            stderr: Some(stderr),
            status,
        };
        Some((case, at))
    }

    fn stdout3(&self, at: usize) -> Option<(Matcher, usize)> {
        if self.starts(at, ">") {
            if let Some(found) = self.regex(self.skip_blanks(at + 1)) {
                return Some(found);
            }
            // `>` alone on its line, then the lines.
            let after = self.skip_blanks(at + 1);
            if after == at + 1 && !self.starts(after, "\n") {
                return None;
            }
            let (rest, next) = self.line(at + 1).unwrap_or(("", self.t.len()));
            if !rest.trim_matches([' ', '\t']).is_empty() {
                return None;
            }
            return self.lines3(next);
        }
        self.lines3(at)
    }

    /// Output lines, up to a `>` line or to blank/comment lines that precede a
    /// `$` or `<` line or the end of the file.
    fn lines3(&self, mut at: usize) -> Option<(Matcher, usize)> {
        let mut text = String::new();
        loop {
            if self.starts(at, ">") {
                break;
            }
            let after = self.skip_blank_and_comment_lines(at);
            if after == self.t.len() || self.starts(after, "$") || self.starts(after, "<") {
                break;
            }
            let (line, next) = self.line(at)?;
            text.push_str(line);
            text.push('\n');
            at = next;
        }
        Some((Matcher::Lines(text), at))
    }

    // Format 1

    fn format1(&self) -> Result<Vec<Case>, String> {
        let mut cases = Vec::new();
        let mut at = 0;
        while let Some((case, next)) = self.test1(at) {
            cases.push(case);
            at = next;
        }
        at = self.skip_blank_and_comment_lines(at);
        if at < self.t.len() {
            return Err(self.error(at));
        }
        Ok(cases)
    }

    fn test1(&self, at: usize) -> Option<(Case, usize)> {
        const DELIMITERS: &[&str] = &["$$$", "<<<", ">>>2", ">>>=", ">>>"];
        let at = self.skip_blank_and_comment_lines(at);
        let (command, mut at) = self.line(at)?;
        let line = self.line_number(at) - 1;
        let command = command.trim_start_matches([' ', '\t']).to_string();
        let mut stdin = None;
        if self.starts(at, "<<<")
            && let Some(start) = self.blank_or_comment_line(at + 3)
        {
            let (input, next) = self.lines_until(start, DELIMITERS)?;
            stdin = Some(input);
            at = next;
        }
        let section = |at: &mut usize, delimiter: &str| -> Option<Matcher> {
            if !self.starts(*at, delimiter) {
                return None;
            }
            let after = self.skip_blanks(*at + delimiter.len());
            let (m, next) = self.regex(after).or_else(|| {
                let start = self.end_of_item(after)?;
                let (text, next) = self.lines_until(start, DELIMITERS)?;
                Some((Matcher::Lines(text), next))
            })?;
            *at = next;
            Some(m)
        };
        let stdout = section(&mut at, ">>>");
        let stderr = section(&mut at, ">>>2");
        if !self.starts(at, ">>>=") {
            return None;
        }
        let (status, at) = self.status(at + 4);
        let case = Case {
            line,
            command,
            stdin,
            stdout,
            stderr,
            status,
        };
        Some((case, at))
    }
}

impl Matcher {
    /// Whether text (an output stream, or an exit status in decimal) matches.
    pub fn matches(&self, actual: &str) -> Result<bool, String> {
        Ok(match self {
            Matcher::Lines(expected) => actual == expected,
            Matcher::Regex(re) => contains(actual, re)?,
            Matcher::NotRegex(re) => !contains(actual, re)?,
            Matcher::Numeric(n) => actual == n,
            Matcher::NotNumeric(n) => actual != n,
        })
    }
}

/// regex-tdfa's `=~` with default options: a search anywhere, with `^`/`$`
/// matching at line boundaries and `.` not matching a newline. An empty regex
/// matches anything.
fn contains(text: &str, re: &str) -> Result<bool, String> {
    let translated = crate::sh::posix_re::translate(re, crate::sh::posix_re::Flavor::Extended);
    let compiled = regex::Regex::new(&format!("(?m){translated}"))
        .map_err(|e| format!("bad regex /{re}/: {e}"))?;
    Ok(compiled.is_match(text))
}

#[cfg(test)]
mod tests {
    use super::Matcher::*;
    use super::*;

    fn cases(text: &str) -> Vec<Case> {
        parse(text).unwrap()
    }

    fn lines(s: &str) -> Option<Matcher> {
        Some(Lines(s.into()))
    }

    #[test]
    fn first_input_without_delimiter_and_defaults() {
        let cs = cases("# title\n\n2020-01-01\n# kept\n\n$ hledger -f- print\nout\n\n# trailing\n");
        assert_eq!(cs.len(), 1);
        let c = &cs[0];
        assert_eq!(c.stdin.as_deref(), Some("2020-01-01\n# kept\n\n"));
        assert_eq!((c.line, c.command.as_str()), (6, "hledger -f- print"));
        assert_eq!(c.stdout, lines("out\n"));
        assert_eq!(c.stderr, lines(""));
        assert_eq!(c.status, Numeric("0".into()));
        // No input at all: an empty first input.
        assert_eq!(cases("$ hledger\n")[0].stdin.as_deref(), Some(""));
    }

    #[test]
    fn groups_share_input_until_the_next_delimiter() {
        let cs = cases("$ a\n<\nin1\n$ b\n\n$ c\n<\n# in2\n\n$  d\n");
        let inputs: Vec<_> = cs.iter().map(|c| c.stdin.as_deref()).collect();
        assert_eq!(
            inputs,
            [Some(""), Some("in1\n"), Some("in1\n"), Some("# in2\n\n")]
        );
        assert_eq!(cs[3].command, "d", "two spaces after $ are allowed");
    }

    #[test]
    fn sections_and_matchers() {
        let cs = cases(concat!(
            "$ a\n> /^x\\/y/\n>2 !/err/ # comment\n>= 1\n",
            "$ b\n>\nline\n\n>2\ne1\n>=\n",
            "$ c\n>= !0\n",
            "$ d\n>2 //\n",
        ));
        assert_eq!(cs[0].stdout, Some(Regex("^x/y".into())));
        assert_eq!(cs[0].stderr, Some(NotRegex("err".into())));
        assert_eq!(cs[0].status, Numeric("1".into()));
        assert_eq!(
            cs[1].stdout,
            lines("line\n\n"),
            "a blank line before >2 is output"
        );
        assert_eq!(cs[1].stderr, lines("e1\n"));
        assert_eq!(cs[1].status, any());
        assert_eq!(cs[2].status, NotNumeric("0".into()));
        assert_eq!(cs[3].stderr, Some(Regex("".into())));
        assert_eq!(cs[3].stdout, lines(""));
    }

    #[test]
    fn comments_inside_output_are_output() {
        let cs = cases("$ a\nx\n# not a comment here\ny\n# but this is\n\n$ b\n");
        assert_eq!(cs[0].stdout, lines("x\n# not a comment here\ny\n"));
    }

    #[test]
    fn multi_line_regex() {
        let cs = cases("$ a\n>2 /one\n(.*\n)*two/\n>=1\n");
        assert_eq!(cs[0].stderr, Some(Regex("one\n(.*\n)*two".into())));
        assert_eq!(cs[0].status, Numeric("1".into()));
    }

    #[test]
    fn format1() {
        let cs =
            cases("# c\ncmd one\n<<<\nin\n>>>\n  out\n>>>= 0\n\n# c2\ncmd two\n>>>2 /e/\n>>>=\n");
        assert_eq!(cs.len(), 2);
        assert_eq!((cs[0].line, cs[0].command.as_str()), (2, "cmd one"));
        assert_eq!(cs[0].stdin.as_deref(), Some("in\n"));
        assert_eq!(cs[0].stdout, lines("  out\n"));
        assert_eq!(cs[0].stderr, None);
        assert_eq!(cs[1].stdout, None);
        assert_eq!(cs[1].stderr, Some(Regex("e".into())));
        assert_eq!(cs[1].status, any());
    }

    #[test]
    fn errors() {
        assert!(parse("$ a\n>= 1\n< x\n").is_err());
        assert!(parse("$$$ a\n").is_err());
    }

    #[test]
    fn matching() {
        assert!(Regex("^b".into()).matches("a\nb\n").unwrap());
        assert!(!Regex("a.b".into()).matches("a\nb").unwrap());
        assert!(Regex(r"\<2\>".into()).matches("x 2 y").unwrap());
        assert!(!Regex(r"\<2\>".into()).matches("x 22 y").unwrap());
        assert!(any().matches("3").unwrap());
        assert!(Lines("a\n".into()).matches("a\n").unwrap());
        assert!(!Lines("a\n".into()).matches("a").unwrap());
    }
}
