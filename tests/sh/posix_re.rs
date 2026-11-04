//! Translation of POSIX regular expressions to the `regex` crate's syntax: basic
//! (grep and sed by default) and extended (grep -E, sed -E, and the shelltest
//! `/REGEX/` expectations, which use regex-tdfa).

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Flavor {
    Basic,
    Extended,
}

/// Translate. `\<` and `\>` become word-start and word-end assertions; in basic
/// syntax `\(` `\)` `\{` `\}` `\|` `\+` `\?` become operators and their unescaped
/// forms literals. Bracket expressions keep POSIX meaning (a backslash is
/// literal), and negated ones do not match a newline, as with regex-tdfa.
pub fn translate(re: &str, flavor: Flavor) -> String {
    let cs: Vec<char> = re.chars().collect();
    let basic = flavor == Flavor::Basic;
    let mut out = String::new();
    // Whether the previous token begins an expression (start, `\(`, `\|`),
    // where a basic `^` is an anchor and `*` is literal.
    let mut at_start = true;
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        let mut starts_expr = false;
        match c {
            '\\' if i + 1 < cs.len() => {
                let d = cs[i + 1];
                i += 1;
                match d {
                    '<' => out.push_str(r"\b{start}"),
                    '>' => out.push_str(r"\b{end}"),
                    '(' | '|' if basic => {
                        out.push(d);
                        starts_expr = true;
                    }
                    ')' | '{' | '}' | '+' | '?' if basic => out.push(d),
                    _ => {
                        out.push('\\');
                        out.push(d);
                    }
                }
            }
            '\\' => out.push_str(r"\\"),
            '[' => match bracket(&cs[i..]) {
                Some((class, len)) => {
                    out.push_str(&class);
                    i += len - 1;
                }
                None => out.push_str(r"\["),
            },
            '{' if !basic => match bound_len(&cs[i..]) {
                Some(len) => {
                    out.extend(&cs[i..i + len]);
                    i += len - 1;
                }
                // regex-tdfa takes a `{` that starts no valid bound literally.
                None => out.push_str(r"\{"),
            },
            '}' if !basic => out.push_str(r"\}"),
            '(' | ')' | '{' | '}' | '|' | '+' | '?' if basic => {
                out.push('\\');
                out.push(c);
            }
            '*' if basic && at_start => out.push_str(r"\*"),
            '^' if basic && !at_start => out.push_str(r"\^"),
            '^' => {
                out.push(c);
                starts_expr = true;
            }
            '$' if basic && !ends_expr(&cs[i + 1..]) => out.push_str(r"\$"),
            c => out.push(c),
        }
        at_start = starts_expr;
        i += 1;
    }
    out
}

/// The length of an interval `{N}`, `{N,}` or `{N,M}` at the start of `cs`.
fn bound_len(cs: &[char]) -> Option<usize> {
    let close = cs.iter().position(|&c| c == '}')?;
    let inner: String = cs[1..close].iter().collect();
    let (min, max) = inner.split_once(',').unwrap_or((&inner, "0"));
    let digits = |s: &str| s.chars().all(|c| c.is_ascii_digit());
    (!min.is_empty() && digits(min) && digits(max)).then_some(close + 1)
}

/// Whether a basic `$` followed by this is at the end of an expression.
fn ends_expr(rest: &[char]) -> bool {
    matches!(rest, [] | ['\\', ')' | '|', ..])
}

/// A bracket expression at the start of `cs`, translated, and its length.
fn bracket(cs: &[char]) -> Option<(String, usize)> {
    let mut i = 1;
    let mut out = String::from("[");
    if cs.get(i) == Some(&'^') {
        out.push_str(r"^\n");
        i += 1;
    }
    let first = i;
    loop {
        let c = *cs.get(i)?;
        match c {
            ']' if i > first => {
                out.push(']');
                return Some((out, i + 1));
            }
            '[' if cs.get(i + 1) == Some(&':') => {
                let end = (i + 2..cs.len().saturating_sub(1))
                    .find(|&j| cs[j] == ':' && cs[j + 1] == ']')?;
                out.extend(&cs[i..end + 2]);
                i = end + 1;
            }
            '\\' | '[' | ']' | '&' | '~' => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::Flavor::*;
    use super::*;

    #[test]
    fn basic() {
        assert_eq!(translate(r"x\(.*\)<", Basic), r"x(.*)<");
        assert_eq!(translate(r"a\|b+c?{1}", Basic), r"a|b\+c\?\{1\}");
        assert_eq!(translate(r"^--$", Basic), r"^--$");
        assert_eq!(translate(r"a^b$c", Basic), r"a\^b\$c");
        assert_eq!(translate(r"*a", Basic), r"\*a");
        assert_eq!(translate(r"\(^a$\)", Basic), r"(^a$)");
    }

    #[test]
    fn extended() {
        assert_eq!(translate(r"\<2\>", Extended), r"\b{start}2\b{end}");
        assert_eq!(translate(r"a(b|c)+$", Extended), r"a(b|c)+$");
        assert_eq!(translate(r"\{DATE\}", Extended), r"\{DATE\}");
        assert_eq!(translate("a{2,}b{1,3}c{2}", Extended), "a{2,}b{1,3}c{2}");
        assert_eq!(
            translate("  {\n  },", Extended),
            r"  \{\n  \},".replace(r"\n", "\n")
        );
    }

    #[test]
    fn brackets() {
        assert_eq!(translate(r#"[^"]*"#, Extended), r#"[^\n"]*"#);
        assert_eq!(translate(r"[[:space:]]+", Extended), r"[[:space:]]+");
        assert_eq!(translate(r"[]a\]", Extended), r"[\]a\\]");
        assert_eq!(translate(r"[a-z0-9.]", Basic), r"[a-z0-9.]");
        assert_eq!(translate(r"a[", Extended), r"a\[");
        for re in [r#"[^"]*"#, r"[[:space:]]+", r"[]a\]", r"[a&&b]"] {
            regex::Regex::new(&translate(re, Extended)).unwrap();
        }
    }
}
