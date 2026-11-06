//! Parser for the subset of POSIX sh syntax used by the corpus's command lines.
//! Anything outside the subset is reported as unsupported rather than guessed at.

/// Why a command line cannot be run by this interpreter.
pub type Unsupported = String;

#[derive(Clone, Debug, PartialEq)]
pub enum Part {
    /// Literal text; quoted text is exempt from field splitting.
    Lit { text: String, quoted: bool },
    /// `$NAME`, `${NAME}`, `${NAME:-DEFAULT}`, and the special `$$` and `$?`.
    Param {
        name: String,
        quoted: bool,
        default: Option<Word>,
    },
    /// `$(pwd)` or `$(mktemp ...)`.
    Subst { cmd: Box<Simple>, quoted: bool },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Word(pub Vec<Part>);

impl Word {
    /// The word's text, if it has no expansions.
    pub fn literal(&self) -> Option<String> {
        self.0
            .iter()
            .map(|p| match p {
                Part::Lit { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    fn is_unquoted(&self, s: &str) -> bool {
        matches!(&self.0[..], [Part::Lit { text, quoted: false }] if text == s)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum RedirOp {
    /// `>FILE`
    Out,
    /// `>>FILE`
    Append,
    /// `<FILE`
    In,
    /// `>&N`
    Dup(u32),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Redir {
    pub fd: u32,
    pub op: RedirOp,
    pub target: Word,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Simple {
    pub assigns: Vec<(String, Word)>,
    pub words: Vec<Word>,
    pub redirs: Vec<Redir>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Simple(Simple),
    Subshell(List, Vec<Redir>),
    /// `for NAME in WORDS; do LIST; done`
    For {
        name: String,
        words: Vec<Word>,
        body: List,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pipeline(pub Vec<Command>);

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Connector {
    And,
    Or,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AndOr {
    pub first: Pipeline,
    pub rest: Vec<(Connector, Pipeline)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct List(pub Vec<AndOr>);

/// Parse a command line, or a script of several lines.
pub fn parse(src: &str) -> Result<List, Unsupported> {
    let mut p = Parser {
        s: src.chars().collect(),
        i: 0,
    };
    let list = p.list(false)?;
    match p.peek() {
        None => Ok(list),
        Some(c) => Err(format!("unexpected `{c}`")),
    }
}

const RESERVED: &[&str] = &[
    "!", "{", "}", "case", "do", "done", "elif", "else", "esac", "fi", "for", "function", "if",
    "in", "select", "then", "until", "while",
];

struct Parser {
    s: Vec<char>,
    i: usize,
}

fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Characters that end an unquoted word.
fn is_meta(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t' | '\n' | ';' | '&' | '|' | '(' | ')' | '<' | '>'
    )
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.s.get(self.i + n).copied()
    }

    fn eat(&mut self, s: &str) -> bool {
        let n = s.chars().count();
        if self.s.len() >= self.i + n && self.s[self.i..self.i + n].iter().copied().eq(s.chars()) {
            self.i += n;
            true
        } else {
            false
        }
    }

    /// Skip blanks, line continuations and comments, stopping at a newline.
    fn blanks(&mut self) {
        loop {
            match self.peek() {
                Some(' ' | '\t') => self.i += 1,
                Some('\\') if self.peek_at(1) == Some('\n') => self.i += 2,
                Some('#') => {
                    while self.peek().is_some_and(|c| c != '\n') {
                        self.i += 1;
                    }
                }
                _ => return,
            }
        }
    }

    fn blanks_and_newlines(&mut self) {
        loop {
            self.blanks();
            if self.peek() == Some('\n') {
                self.i += 1;
            } else {
                return;
            }
        }
    }

    /// Whether the reserved word `w` starts here.
    fn at_reserved(&self, w: &str) -> bool {
        let n = w.chars().count();
        self.s[self.i..].iter().take(n).copied().eq(w.chars())
            && self.peek_at(n).is_none_or(is_meta)
    }

    /// and-or lists separated by `;` or newlines, up to the end or (inside a
    /// subshell, command substitution or `for` loop) a `)` or `done`.
    fn list(&mut self, nested: bool) -> Result<List, Unsupported> {
        let mut items = Vec::new();
        loop {
            self.blanks_and_newlines();
            match self.peek() {
                None => break,
                Some(')') if nested => break,
                _ if nested && self.at_reserved("done") => break,
                _ => {}
            }
            items.push(self.and_or()?);
            self.blanks();
            match self.peek() {
                Some(';') if self.peek_at(1) == Some(';') => return Err("`;;`".into()),
                Some(';' | '\n') => self.i += 1,
                Some('&') => return Err("background job (`&`)".into()),
                _ => break,
            }
        }
        Ok(List(items))
    }

    fn and_or(&mut self) -> Result<AndOr, Unsupported> {
        let first = self.pipeline()?;
        let mut rest = Vec::new();
        loop {
            self.blanks();
            let op = if self.eat("&&") {
                Connector::And
            } else if self.eat("||") {
                Connector::Or
            } else {
                break;
            };
            self.blanks_and_newlines();
            rest.push((op, self.pipeline()?));
        }
        Ok(AndOr { first, rest })
    }

    fn pipeline(&mut self) -> Result<Pipeline, Unsupported> {
        let mut cmds = vec![self.command()?];
        loop {
            self.blanks();
            if self.peek() == Some('|') && self.peek_at(1) != Some('|') {
                self.i += 1;
                self.blanks_and_newlines();
                cmds.push(self.command()?);
            } else {
                return Ok(Pipeline(cmds));
            }
        }
    }

    fn command(&mut self) -> Result<Command, Unsupported> {
        self.blanks();
        if self.peek() == Some('(') {
            self.i += 1;
            let list = self.list(true)?;
            if !self.eat(")") {
                return Err("unclosed `(`".into());
            }
            let mut redirs = Vec::new();
            loop {
                self.blanks();
                match self.redir()? {
                    Some(r) => redirs.push(r),
                    None => break,
                }
            }
            return Ok(Command::Subshell(list, redirs));
        }
        if self.at_reserved("for") {
            return self.for_loop();
        }
        let mut cmd = Simple::default();
        loop {
            self.blanks();
            match self.peek() {
                None | Some(';' | '&' | '|' | ')' | '\n') => break,
                Some('(') => return Err("`(` inside a command".into()),
                _ => {}
            }
            if let Some(r) = self.redir()? {
                cmd.redirs.push(r);
                continue;
            }
            let word = self.word(false)?;
            if cmd.words.is_empty() {
                if let Some(r) = RESERVED.iter().find(|r| word.is_unquoted(r)) {
                    return Err(format!("`{r}`"));
                }
                if let Some(assign) = assignment(&word) {
                    cmd.assigns.push(assign);
                    continue;
                }
            }
            cmd.words.push(word);
        }
        if cmd.words.is_empty() && cmd.assigns.is_empty() && cmd.redirs.is_empty() {
            return Err("empty command".into());
        }
        Ok(Command::Simple(cmd))
    }

    /// `for NAME in WORDS; do LIST; done`, after `for`. The loop's own
    /// redirections, `for NAME; do` and `for NAME do` are not supported.
    fn for_loop(&mut self) -> Result<Command, Unsupported> {
        self.i += "for".len();
        self.blanks();
        let name = self.name();
        self.blanks();
        if name.is_empty() || !self.at_reserved("in") {
            return Err("`for` without `NAME in`".into());
        }
        self.i += "in".len();
        let mut words = Vec::new();
        loop {
            self.blanks();
            match self.peek() {
                Some(';' | '\n') => break,
                Some(c) if !is_meta(c) => words.push(self.word(false)?),
                _ => return Err("`for` words not ended by `;`".into()),
            }
        }
        self.i += 1;
        self.blanks_and_newlines();
        if !self.at_reserved("do") {
            return Err("`for` without `do`".into());
        }
        self.i += "do".len();
        let body = self.list(true)?;
        if !self.at_reserved("done") {
            return Err("`for` without `done`".into());
        }
        self.i += "done".len();
        Ok(Command::For { name, words, body })
    }

    /// A redirection, if one starts here.
    fn redir(&mut self) -> Result<Option<Redir>, Unsupported> {
        let start = self.i;
        let mut digits = String::new();
        while let Some(c) = self.peek().filter(char::is_ascii_digit) {
            digits.push(c);
            self.i += 1;
        }
        let (default_fd, op) = if self.eat(">>") {
            (1, RedirOp::Append)
        } else if self.eat(">&") {
            let mut n = String::new();
            while let Some(c) = self.peek().filter(char::is_ascii_digit) {
                n.push(c);
                self.i += 1;
            }
            let n = n.parse().map_err(|_| "`>&` without a file descriptor")?;
            (1, RedirOp::Dup(n))
        } else if self.eat(">|") || self.eat("<<") || self.eat("<&") || self.eat("<>") {
            return Err("redirection operator".into());
        } else if self.eat(">") {
            (1, RedirOp::Out)
        } else if self.eat("<") {
            (0, RedirOp::In)
        } else {
            self.i = start;
            return Ok(None);
        };
        let fd = if digits.is_empty() {
            default_fd
        } else {
            digits.parse().map_err(|_| "file descriptor")?
        };
        let supported = match op {
            RedirOp::In => fd == 0,
            RedirOp::Dup(n) => matches!((fd, n), (1, 2) | (2, 1)),
            _ => fd == 1 || fd == 2,
        };
        if !supported {
            return Err("redirection of an unsupported file descriptor".into());
        }
        let target = if let RedirOp::Dup(_) = op {
            Word::default()
        } else {
            self.blanks();
            match self.peek() {
                Some(c) if !is_meta(c) => self.word(false)?,
                _ => return Err("redirection without a target".into()),
            }
        };
        Ok(Some(Redir { fd, op, target }))
    }

    /// A word, up to an unquoted metacharacter; inside `${NAME:-...}`, up to `}`.
    fn word(&mut self, in_braces: bool) -> Result<Word, Unsupported> {
        let mut parts = Vec::new();
        let mut lit = String::new();
        let start = self.i;
        fn flush(parts: &mut Vec<Part>, lit: &mut String) {
            if !lit.is_empty() {
                parts.push(Part::Lit {
                    text: std::mem::take(lit),
                    quoted: false,
                });
            }
        }
        while let Some(c) = self.peek() {
            if (is_meta(c) && !in_braces) || (in_braces && c == '}') {
                break;
            }
            match c {
                '\'' => {
                    flush(&mut parts, &mut lit);
                    self.i += 1;
                    let mut text = String::new();
                    loop {
                        match self.peek() {
                            None => return Err("unclosed `'`".into()),
                            Some('\'') => break,
                            Some(c) => text.push(c),
                        }
                        self.i += 1;
                    }
                    self.i += 1;
                    parts.push(Part::Lit { text, quoted: true });
                }
                '"' => {
                    flush(&mut parts, &mut lit);
                    self.i += 1;
                    self.double_quoted(&mut parts)?;
                }
                '\\' => {
                    self.i += 1;
                    match self.peek() {
                        Some('\n') => self.i += 1,
                        Some(c) => {
                            flush(&mut parts, &mut lit);
                            parts.push(Part::Lit {
                                text: c.to_string(),
                                quoted: true,
                            });
                            self.i += 1;
                        }
                        None => lit.push('\\'),
                    }
                }
                '$' => match self.dollar(false)? {
                    Some(part) => {
                        flush(&mut parts, &mut lit);
                        parts.push(part);
                    }
                    None => lit.push('$'),
                },
                '`' => return Err("backquote command substitution".into()),
                '~' if self.i == start => return Err("tilde expansion".into()),
                c => {
                    lit.push(c);
                    self.i += 1;
                }
            }
        }
        flush(&mut parts, &mut lit);
        Ok(Word(parts))
    }

    /// The inside of a double-quoted string, after the opening `"`.
    fn double_quoted(&mut self, parts: &mut Vec<Part>) -> Result<(), Unsupported> {
        let mut text = String::new();
        // An empty "" still makes a (quoted, empty) word.
        let mut pushed_any = false;
        loop {
            match self.peek() {
                None => return Err("unclosed `\"`".into()),
                Some('"') => {
                    self.i += 1;
                    break;
                }
                Some('\\') => {
                    self.i += 1;
                    match self.peek() {
                        Some('\n') => self.i += 1,
                        Some(c @ ('$' | '`' | '"' | '\\')) => {
                            text.push(c);
                            self.i += 1;
                        }
                        _ => text.push('\\'),
                    }
                }
                Some('$') => match self.dollar(true)? {
                    Some(part) => {
                        if !text.is_empty() {
                            parts.push(Part::Lit {
                                text: std::mem::take(&mut text),
                                quoted: true,
                            });
                        }
                        parts.push(part);
                        pushed_any = true;
                    }
                    None => text.push('$'),
                },
                Some('`') => return Err("backquote command substitution".into()),
                Some(c) => {
                    text.push(c);
                    self.i += 1;
                }
            }
        }
        if !text.is_empty() || !pushed_any {
            parts.push(Part::Lit { text, quoted: true });
        }
        Ok(())
    }

    /// An expansion starting at `$`, or None if this `$` is literal.
    fn dollar(&mut self, quoted: bool) -> Result<Option<Part>, Unsupported> {
        let param = |name: &str| Part::Param {
            name: name.to_string(),
            quoted,
            default: None,
        };
        match self.peek_at(1) {
            Some(c @ ('$' | '?')) => {
                self.i += 2;
                Ok(Some(param(&c.to_string())))
            }
            Some(c) if is_name_start(c) => {
                self.i += 1;
                Ok(Some(param(&self.name())))
            }
            Some('{') => {
                self.i += 2;
                let name = self.name();
                if name.is_empty() {
                    return Err("parameter expansion".into());
                }
                let default = if self.eat(":-") {
                    Some(self.word(true)?)
                } else {
                    None
                };
                if !self.eat("}") {
                    return Err("parameter expansion".into());
                }
                Ok(Some(Part::Param {
                    name,
                    quoted,
                    default,
                }))
            }
            Some('(') if self.peek_at(2) == Some('(') => Err("arithmetic expansion".into()),
            Some('(') => {
                self.i += 2;
                let list = self.list(true)?;
                if !self.eat(")") {
                    return Err("unclosed `$(`".into());
                }
                let simple = match &list.0[..] {
                    [AndOr { first, rest }] if rest.is_empty() => match &first.0[..] {
                        [Command::Simple(s)] => Some(s),
                        _ => None,
                    },
                    _ => None,
                };
                match simple {
                    Some(s)
                        if s.assigns.is_empty()
                            && s.redirs.is_empty()
                            && (s.words[0].literal().as_deref() == Some("pwd")
                                || s.words[0].literal().as_deref() == Some("mktemp")) =>
                    {
                        Ok(Some(Part::Subst {
                            cmd: Box::new(s.clone()),
                            quoted,
                        }))
                    }
                    _ => Err("command substitution other than $(pwd) or $(mktemp)".into()),
                }
            }
            Some(c) if c.is_ascii_digit() || "@*#!-".contains(c) => {
                Err(format!("special parameter `${c}`"))
            }
            Some('\'') => Err("`$'...'` quoting".into()),
            _ => {
                self.i += 1;
                Ok(None)
            }
        }
    }

    fn name(&mut self) -> String {
        let mut name = String::new();
        while let Some(c) = self.peek().filter(|&c| is_name_char(c)) {
            name.push(c);
            self.i += 1;
        }
        name
    }
}

/// `NAME=VALUE` as an assignment, if the word has that form.
fn assignment(word: &Word) -> Option<(String, Word)> {
    let Some(Part::Lit {
        text,
        quoted: false,
    }) = word.0.first()
    else {
        return None;
    };
    let (name, value) = text.split_once('=')?;
    let mut cs = name.chars();
    if !cs.next().is_some_and(is_name_start) || !cs.all(is_name_char) {
        return None;
    }
    let mut parts = Vec::new();
    if !value.is_empty() {
        parts.push(Part::Lit {
            text: value.to_string(),
            quoted: false,
        });
    }
    parts.extend(word.0[1..].iter().cloned());
    Some((name.to_string(), Word(parts)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(s: &str, quoted: bool) -> Part {
        Part::Lit {
            text: s.into(),
            quoted,
        }
    }

    fn simple(src: &str) -> Simple {
        match parse(src).unwrap().0.remove(0).first.0.remove(0) {
            Command::Simple(s) => s,
            c => panic!("not a simple command: {c:?}"),
        }
    }

    #[test]
    fn quoting() {
        let s = simple(r#"printf 'a b' "c \"d\" \$ \n" e\ f '' """#);
        assert_eq!(
            s.words,
            [
                Word(vec![lit("printf", false)]),
                Word(vec![lit("a b", true)]),
                Word(vec![lit(r#"c "d" $ \n"#, true)]),
                Word(vec![lit("e", false), lit(" ", true), lit("f", false)]),
                Word(vec![lit("", true)]),
                Word(vec![lit("", true)]),
            ]
        );
        assert_eq!(
            simple(r#"echo '"'"'""#).words[1],
            Word(vec![lit("\"", true), lit("'", true)])
        );
    }

    #[test]
    fn expansions() {
        let s = simple(r#"x $$d "${TMPDIR:-/tmp}/a" $? $(mktemp -d) "$(pwd)/" a$ $ ,$"#);
        let w = &s.words;
        assert!(
            matches!(&w[1].0[..], [Part::Param { name, quoted: false, .. }, Part::Lit { text, .. }] if name == "$" && text == "d")
        );
        assert!(
            matches!(&w[2].0[..], [Part::Param { name, quoted: true, default: Some(d) }, _] if name == "TMPDIR" && d.literal().as_deref() == Some("/tmp"))
        );
        assert!(matches!(&w[3].0[..], [Part::Param { name, .. }] if name == "?"));
        assert!(matches!(&w[4].0[..], [Part::Subst { quoted: false, .. }]));
        assert!(matches!(&w[5].0[..], [Part::Subst { quoted: true, .. }, _]));
        assert_eq!(w[6].literal().as_deref(), Some("a$"));
        assert_eq!(w[7].literal().as_deref(), Some("$"));
        assert_eq!(w[8].literal().as_deref(), Some(",$"));
    }

    #[test]
    fn assignments_and_redirections() {
        let s = simple("A=1 B= C=$d/x hledger -f- 2>&1 >/dev/null <in >>log D=2");
        let names: Vec<_> = s.assigns.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["A", "B", "C"]);
        assert_eq!(s.assigns[1].1, Word::default());
        assert_eq!(s.words.len(), 3);
        assert_eq!(s.words[2].literal().as_deref(), Some("D=2"));
        let ops: Vec<_> = s.redirs.iter().map(|r| (r.fd, r.op.clone())).collect();
        assert_eq!(
            ops,
            [
                (2, RedirOp::Dup(1)),
                (1, RedirOp::Out),
                (0, RedirOp::In),
                (1, RedirOp::Append)
            ]
        );
    }

    #[test]
    fn lists_and_pipelines() {
        let l = parse("a | b c && d || e; (cd x && f) >o; s=$?; exit $s").unwrap();
        assert_eq!(l.0.len(), 4);
        assert_eq!(l.0[0].first.0.len(), 2);
        assert_eq!(
            l.0[0].rest.iter().map(|(c, _)| *c).collect::<Vec<_>>(),
            [Connector::And, Connector::Or]
        );
        assert!(
            matches!(&l.0[1].first.0[0], Command::Subshell(inner, r) if inner.0.len() == 1 && r.len() == 1)
        );
        let script =
            parse("#!/usr/bin/env -S sh\n# comment\ncd .. && hledger x\necho | y\n").unwrap();
        assert_eq!(script.0.len(), 2);
        let l = parse("for a in x 'y z' $b; do echo $a || c; d; done; e").unwrap();
        assert_eq!(l.0.len(), 2);
        assert!(
            matches!(&l.0[0].first.0[0], Command::For { name, words, body } if name == "a" && words.len() == 3 && body.0.len() == 2)
        );
        assert!(matches!(
            &parse("for f in a\ndo\n  g $f\ndone").unwrap().0[0].first.0[0],
            Command::For { .. }
        ));
    }

    #[test]
    fn unsupported_shapes() {
        for src in [
            "for a; do echo $a; done",
            "for a in x; do echo $a done",
            "for a in x | y; do z; done",
            "for a in x; do (echo $a; done)",
            "for a in x; do echo $a; done >f",
            "ls *.j &",
            "echo `pwd`",
            "cat <<EOF",
            "x &",
            "echo $1",
            "echo $(ls)",
            "echo $((1+1))",
            "echo ${x#y}",
            "if true; then x; fi",
            "echo 'unclosed",
            "cd ~",
            "3>f x",
        ] {
            assert!(parse(src).is_err(), "{src} should be unsupported");
        }
    }
}
