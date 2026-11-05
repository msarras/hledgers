//! CSV conversion rules (the rules language of Hledger.Read.RulesReader):
//! the rules file parser, with its included files inlined and errors as
//! upstream gives them; and the lookups that find, for a CSV record, the
//! value assigned to a hledger field, through top-level assignments, if
//! blocks and ifs tables, matchers, field templates and template functions.
//!
//! Every assignment to a hledger field is a [`FieldAssignment`] in one of two
//! lists: the top-level assignments, and the assignments of each
//! [`ConditionalBlock`] (an if block or an ifs table row). For a record, a
//! field's assignments are the top-level ones followed by those of the blocks
//! whose matchers match, in the order written; the last one is in effect.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock};

use super::csv::CsvRecord;
use crate::parse::{Alts, Item, PResult, ParseError, Parser, choice_try, is_non_newline_space, or};
use crate::regex::{Regexp, to_regex_ci};
use crate::text::{is_space, quote_if_needed, strip};

/// Where a rule was written: its rules file and line, if known.
pub type RulesPos = Option<(Arc<str>, usize)>;

/// The file and line that each line of an include-expanded rules text came
/// from.
type LineSources = Vec<(Arc<str>, usize)>;

/// A rules file's rules (CsvRules).
#[derive(Clone, Debug, Default)]
pub struct CsvRules {
    /// The top-level directives (`skip`, `date-format`...) as name and
    /// value, in the order written.
    pub directives: Vec<(String, String)>,
    /// The CSV field names given by the latest fields list, lowercased,
    /// with their column numbers counted from 1.
    pub field_indexes: Vec<(String, usize)>,
    /// The top-level assignments to hledger fields, in the order written.
    pub assignments: Vec<FieldAssignment>,
    /// The if blocks and ifs table rows, in the order written.
    pub blocks: Vec<ConditionalBlock>,
    /// For each hledger field, the top-level assignments and the blocks
    /// that assign it (rblocksassigning).
    by_field: BTreeMap<String, (Vec<usize>, Vec<usize>)>,
}

/// An assignment of a value template to a hledger field, with the rules file
/// position where it was written (FieldAssignment). The position is not part
/// of its identity.
#[derive(Clone, Debug)]
pub struct FieldAssignment {
    pub name: String,
    pub template: Template,
    pub pos: RulesPos,
}

impl PartialEq for FieldAssignment {
    fn eq(&self, other: &FieldAssignment) -> bool {
        self.name == other.name && self.template == other.template
    }
}

impl FieldAssignment {
    pub fn new(name: &str, template: &str, pos: RulesPos) -> FieldAssignment {
        FieldAssignment {
            name: name.to_string(),
            template: Template::new(template),
            pos,
        }
    }
}

/// An if block or ifs table row: matchers, and the assignments that apply
/// when they match (ConditionalBlock). Its skip, end and merge rules are
/// assignments to the pseudo fields of those names.
#[derive(Clone, Debug, PartialEq)]
pub struct ConditionalBlock {
    pub matchers: Vec<Matcher>,
    pub assignments: Vec<FieldAssignment>,
}

/// How a matcher combines with the one before it (MatcherPrefix).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatcherPrefix {
    /// No prefix: it starts an alternative.
    Or,
    /// `&` or `&&`: the previous matcher and this one.
    And,
    /// `!`: this one does not match.
    Not,
    /// `& !` or `&& !`: the previous matcher and not this one.
    AndNot,
}

/// A test of a CSV record: a case-insensitive regular expression matched
/// against one field (`%NAME REGEX`), or against the whole record, its
/// fields joined by commas (Matcher).
#[derive(Clone, Debug, PartialEq)]
pub struct Matcher {
    pub prefix: MatcherPrefix,
    /// The field reference (`%date`, `%1`), or none for the whole record.
    pub field: Option<String>,
    pub regex: Regexp,
}

/// A field assignment's value as written, and its parts (FieldTemplate).
/// Two are equal when they were written the same.
#[derive(Clone, Debug)]
pub struct Template {
    pub text: String,
    /// Literal text and references to interpolate, or none if the template
    /// can not be read that way and is used as written (templateParts).
    parts: Option<Vec<Part>>,
}

impl PartialEq for Template {
    fn eq(&self, other: &Template) -> bool {
        self.text == other.text
    }
}

/// A part of a field template (TemplatePart).
#[derive(Clone, Debug)]
enum Part {
    Text(String),
    /// A match group reference, `\N`: the digits.
    Match(String),
    /// A `%{...}` expression.
    Expr(Expr),
    /// A CSV field reference: `%NAME`, `%N` or `%(NAME)`.
    Field(String),
}

/// A `%{...}` expression (TemplateExpr).
#[derive(Clone, Debug)]
enum Expr {
    /// A template function's name and arguments.
    Call(String, Vec<Expr>),
    /// A CSV field reference.
    Field(String),
    /// A hledger field's name.
    HledgerField(String),
    /// A match group reference, `\N`.
    Match(String),
    Str(String),
    Number(i64),
}

/// The most postings a CSV record can make.
pub const MAX_POSTINGS: usize = 99;

/// The hledger fields and pseudo fields that rules can assign, in the order
/// the parser tries them, where a name comes before any name it starts
/// with (journalfieldnames).
pub fn journal_field_names() -> &'static [String] {
    static NAMES: OnceLock<Vec<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let mut names = Vec::new();
        for i in (1..=MAX_POSTINGS).rev() {
            let numbered = [
                ("account", ""),
                ("amount", "-in"),
                ("amount", "-out"),
                ("amount", ""),
                ("balance", ""),
                ("comment", ""),
                ("currency", ""),
            ];
            names.extend(numbered.iter().map(|(f, s)| format!("{f}{i}{s}")));
        }
        for f in [
            "amount-in",
            "amount-out",
            "amount",
            "balance",
            "code",
            "comment",
            "currency",
            "date2",
            "date",
            "description",
            "status",
            "skip",
            "end",
            "merge",
        ] {
            names.push(f.to_string());
        }
        names
    })
}

/// The top-level directives (directives).
const DIRECTIVES: [&str; 11] = [
    "source",
    "archive",
    "encoding",
    "date-format",
    "decimal-mark",
    "separator",
    "skip",
    "timezone",
    "newest-first",
    "intra-day-reversed",
    "balance-type",
];

impl CsvRules {
    /// Rules from their parts, in the order written (mkrules).
    pub fn new(
        directives: Vec<(String, String)>,
        field_indexes: Vec<(String, usize)>,
        assignments: Vec<FieldAssignment>,
        blocks: Vec<ConditionalBlock>,
    ) -> CsvRules {
        let mut by_field: BTreeMap<String, (Vec<usize>, Vec<usize>)> = BTreeMap::new();
        for (i, a) in assignments.iter().enumerate() {
            by_field.entry(a.name.clone()).or_default().0.push(i);
        }
        for (i, b) in blocks.iter().enumerate() {
            for name in b
                .assignments
                .iter()
                .map(|a| &a.name)
                .collect::<BTreeSet<_>>()
            {
                by_field.entry(name.clone()).or_default().1.push(i);
            }
        }
        CsvRules {
            directives,
            field_indexes,
            assignments,
            blocks,
            by_field,
        }
    }
}

/// Read a rules file and the rules files it includes, returning the rules
/// and the paths of the files read; or an error (readRules).
pub fn read_rules(f: &str) -> Result<(CsvRules, Vec<String>), String> {
    let text = super::read_file_portably(f).map_err(|e| format!("{f}: openFile: {e}"))?;
    let (expanded, sources) = expand_includes(f, &text)?;
    let rules = parse_and_validate_rules(f, &sources, &expanded)?;
    let mut files = vec![f.to_string()];
    for (file, _) in &sources {
        if !files.iter().any(|g| **g == **file) {
            files.push(file.to_string());
        }
    }
    Ok((rules, files))
}

/// The lines of a text, as Data.Text's lines splits them.
fn text_lines(s: &str) -> impl Iterator<Item = &str> {
    let s = s.strip_suffix('\n').unwrap_or(s);
    s.split('\n').filter(move |_| !s.is_empty())
}

/// The directory part of a path, `.` if it has none (takeDirectory).
pub fn take_directory(p: &str) -> String {
    match p.trim_end_matches('/').rfind('/') {
        Some(0) => "/".to_string(),
        Some(i) => p[..i].trim_end_matches('/').to_string(),
        None if p.starts_with('/') => "/".to_string(),
        None => ".".to_string(),
    }
}

/// A path joined to a directory, unless it is absolute (`</>`).
pub fn path_join(dir: &str, p: &str) -> String {
    if p.starts_with('/') || dir.is_empty() {
        p.to_string()
    } else if dir.ends_with('/') {
        format!("{dir}{p}")
    } else {
        format!("{dir}/{p}")
    }
}

/// The rules text with the files named by its `include` lines inlined, and
/// the file and line each line came from (expandIncludes). An included file
/// that can not be read, or that includes itself, is an error shown at its
/// include line.
fn expand_includes(f: &str, text: &str) -> Result<(String, LineSources), String> {
    let mut out = String::new();
    let mut sources = Vec::new();
    let ancestors = vec![super::journal::canonical_path(f)];
    expand_lines(
        &ancestors,
        &take_directory(f),
        f,
        text,
        &mut out,
        &mut sources,
    )?;
    Ok((out, sources))
}

fn expand_lines(
    ancestors: &[std::path::PathBuf],
    dir: &str,
    file: &str,
    text: &str,
    out: &mut String,
    sources: &mut LineSources,
) -> Result<(), String> {
    let file_arc: Arc<str> = Arc::from(file);
    for (i, line) in text_lines(text).enumerate() {
        let lnum = i + 1;
        let Some(path) = line.strip_prefix("include ") else {
            out.push_str(line);
            out.push('\n');
            sources.push((file_arc.clone(), lnum));
            continue;
        };
        let included = path_join(dir, strip(path));
        let canonical = super::journal::canonical_path(&included);
        if ancestors.contains(&canonical) {
            let msg = format!("This included file forms a cycle: {included}");
            return Err(include_error_msg(file, lnum, line, &msg));
        }
        let text = super::read_file_portably(&included).map_err(|e| {
            let msg = format!("Could not read this included file:\n{included}: openFile: {e}");
            include_error_msg(file, lnum, line, &msg)
        })?;
        let mut ancestors2 = vec![canonical];
        ancestors2.extend(ancestors.iter().cloned());
        expand_lines(
            &ancestors2,
            &take_directory(&included),
            &included,
            &text,
            out,
            sources,
        )?;
    }
    Ok(())
}

/// An error about an include line, megaparsec style (includeErrorMsg).
fn include_error_msg(file: &str, lnum: usize, line: &str, msg: &str) -> String {
    let pad = " ".repeat(lnum.to_string().len());
    format!("{file}:{lnum}:1:\n{pad} |\n{lnum} | {line}\n{pad} | ^\n{msg}\n")
}

/// Parse and check the include-expanded text of a rules file, reporting
/// errors and positions at the lines' own files and lines
/// (parseAndValidateCsvRules).
fn parse_and_validate_rules(
    f: &str,
    sources: &[(Arc<str>, usize)],
    text: &str,
) -> Result<CsvRules, String> {
    let lines = Lines::new(f, text);
    let mut p = Parser::new(text);
    let mut rules = match rulesp(&mut p, &lines) {
        Ok(rules) => rules,
        Err(e) => {
            // the error's line's source, or after the last line the line after its source
            let offset = e.offset().min(text.len());
            let line = text[..offset].matches('\n').count() + 1;
            let (file, lnum) = match sources.get(line - 1) {
                Some((file, l)) => (file.to_string(), *l),
                None => sources
                    .last()
                    .map_or((f.to_string(), line), |(file, l)| (file.to_string(), l + 1)),
            };
            return Err(e.pretty_as_line(&file, lnum, text));
        }
    };
    // the rules' positions, as lines of the expanded text, translated to their sources
    let translate = |a: &mut FieldAssignment| {
        a.pos = a
            .pos
            .take()
            .and_then(|(_, l)| sources.get(l.wrapping_sub(1)).cloned());
    };
    rules.assignments.iter_mut().for_each(translate);
    for b in &mut rules.blocks {
        b.assignments.iter_mut().for_each(translate);
    }
    if rules.field_assignment(&[], "date").is_none() {
        return Err(format!(
            "{f}:\nPlease specify (at top level) the date field. Eg: date %1"
        ));
    }
    Ok(rules)
}

/// Line numbers of offsets in the text being parsed.
struct Lines {
    file: Arc<str>,
    starts: Vec<usize>,
}

impl Lines {
    fn new(file: &str, text: &str) -> Lines {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Lines {
            file: Arc::from(file),
            starts,
        }
    }

    /// The rules position of an offset: the file, and the line (getRulesPos).
    fn pos(&self, offset: usize) -> RulesPos {
        let line = self.starts.partition_point(|&s| s <= offset);
        Some((self.file.clone(), line))
    }

    /// The offset of the first non-space character on a line.
    fn first_nonspace(&self, text: &str, line: usize) -> usize {
        let start = self
            .starts
            .get(line.saturating_sub(1))
            .copied()
            .unwrap_or(text.len());
        let start = start.min(text.len());
        start
            + text[start..]
                .chars()
                .take_while(|&c| c != '\n' && is_space(c))
                .map(char::len_utf8)
                .sum::<usize>()
    }
}

/// What a rules file line is.
enum RuleItem {
    Blank,
    Directive(String, String),
    Fields(RulesPos, Vec<String>),
    Assignment(FieldAssignment),
    Blocks(Vec<ConditionalBlock>),
}

/// Rules, one per line, until the end of the input (rulesp).
fn rulesp(p: &mut Parser, lines: &Lines) -> PResult<CsvRules> {
    let items = p.many(|p| rule_itemp(p, lines))?;
    p.eof()?;
    let (mut directives, mut field_indexes, mut assignments, mut blocks) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for item in items {
        match item {
            RuleItem::Blank => {}
            RuleItem::Directive(d, v) => directives.push((d, v)),
            RuleItem::Fields(pos, fs) => {
                field_indexes = fs.iter().cloned().zip(1..).collect();
                for f in journal_field_names() {
                    if let Some(i) = fs.iter().position(|g| g == f) {
                        assignments.push(FieldAssignment::new(
                            f,
                            &format!("%{}", i + 1),
                            pos.clone(),
                        ));
                    }
                }
            }
            RuleItem::Assignment(a) => assignments.push(a),
            RuleItem::Blocks(bs) => blocks.extend(bs),
        }
    }
    let rules = CsvRules::new(directives, field_indexes, assignments, blocks);
    check_field_reference_loops(p, lines, &rules)?;
    Ok(rules)
}

/// One rules file line or block.
fn rule_itemp(p: &mut Parser, lines: &Lines) -> PResult<RuleItem> {
    let mut tried = Alts::new(p);
    if let Some(r) = tried.run(p, |p| {
        p.label("blank or comment line", blank_or_comment_linep)
            .map(|()| RuleItem::Blank)
    }) {
        return r;
    }
    if let Some(r) = tried.run(p, |p| {
        p.label("directive", directivep)
            .map(|(d, v)| RuleItem::Directive(d, v))
    }) {
        return r;
    }
    if let Some(r) = tried.run(p, |p| {
        let pos = lines.pos(p.pos);
        p.label("field name list", fieldnamelistp)
            .map(|fs| RuleItem::Fields(pos, fs))
    }) {
        return r;
    }
    if let Some(r) = tried.run(p, |p| fieldassignmentp(p, lines).map(RuleItem::Assignment)) {
        return r;
    }
    if let Some(r) = tried.run(p, |p| {
        p.label("if rule", |p| p.attempt(|p| conditionalblockp(p, lines)))
            .map(|b| RuleItem::Blocks(vec![b]))
    }) {
        return r;
    }
    if let Some(r) = tried.run(p, |p| conditionaltablep(p, lines).map(RuleItem::Blocks)) {
        return r;
    }
    Err(tried.choice_error(p))
}

/// A newline, or the end of the input (eolof).
fn eolof(p: &mut Parser) -> PResult<()> {
    or(p, |p| p.char('\n').map(drop), |p| p.eof())
}

/// The rest of the line (restofline).
fn restofline(p: &mut Parser) -> PResult<()> {
    p.take_while(None, |c| c != '\n');
    eolof(p)
}

/// Spaces, at least one (skipNonNewlineSpaces1).
fn spaces1(p: &mut Parser) -> PResult<()> {
    p.take_while1(None, is_non_newline_space).map(drop)
}

/// A blank line or a comment line (blankorcommentlinep).
fn blank_or_comment_linep(p: &mut Parser) -> PResult<()> {
    choice_try(p, &mut [&mut blanklinep, &mut commentlinep])
}

fn blanklinep(p: &mut Parser) -> PResult<()> {
    p.label("blank line", |p| {
        p.skip_spaces();
        p.char('\n').map(drop)
    })
}

/// A line whose first non-space character is `;` or `#` (commentlinep).
fn commentlinep(p: &mut Parser) -> PResult<()> {
    p.label("comment line", |p| {
        p.skip_spaces();
        p.satisfy(None, |c| c == ';' || c == '#')?;
        restofline(p)
    })
}

/// A top-level directive and its value, if any (directivep).
fn directivep(p: &mut Parser) -> PResult<(String, String)> {
    let mut tried = Alts::new(p);
    let mut d = None;
    for name in DIRECTIVES {
        if let Some(r) = tried.run(p, |p| p.attempt(|p| p.string(name))) {
            d = Some(r?.to_string());
            break;
        }
    }
    let Some(d) = d else {
        return Err(tried.choice_error(p));
    };
    let (voff, v) = or(
        p,
        |p| {
            or(
                p,
                |p| {
                    p.char(':')?;
                    p.skip_spaces();
                    Ok(())
                },
                spaces1,
            )?;
            let off = p.pos;
            let v = p.take_while(None, |c| c != '\n').to_string();
            eolof(p)?;
            Ok((off, v))
        },
        |p| {
            p.optional(|p| p.char(':'))?;
            p.skip_spaces();
            eolof(p)?;
            Ok((0, String::new()))
        },
    )?;
    check_directive_value(p, voff, &d, &v)?;
    Ok((d, v))
}

/// A directive's value checked, if it is one with a specific kind of value
/// (checkDirectiveValue). Surrounding white space is ignored.
fn check_directive_value(p: &Parser, off: usize, d: &str, v: &str) -> PResult<()> {
    let v = strip(v);
    if v.is_empty() {
        return Ok(());
    }
    let msg = match d {
        "decimal-mark" if !matches!(v, "." | ",") => {
            format!("decimal-mark's argument should be \".\" or \",\" (not \"{v}\")")
        }
        "skip" if read_int(v).is_none() => {
            format!("skip's argument should be a number of lines, or nothing (not \"{v}\")")
        }
        "balance-type" if parse_balance_assertion_type(v).is_none() => {
            format!("balance-type \"{v}\" is invalid. Use =, ==, =* or ==*.")
        }
        _ => return Ok(()),
    };
    Err(ParseError::fail_at_raised(p.src, off, p.pos, msg))
}

/// `fields NAME, NAME...`: CSV field names, lowercased (fieldnamelistp).
/// There must be at least two (a comma).
fn fieldnamelistp(p: &mut Parser) -> PResult<Vec<String>> {
    p.string("fields")?;
    p.optional(|p| p.char(':'))?;
    spaces1(p)?;
    let separator = |p: &mut Parser| {
        p.skip_spaces();
        p.char(',')?;
        p.skip_spaces();
        Ok(())
    };
    let mut fs = vec![p.optional(fieldnamep)?.unwrap_or_default()];
    let first: PResult<()> = separator(p);
    first?;
    fs.push(p.optional(fieldnamep)?.unwrap_or_default());
    fs.extend(p.many(|p| {
        separator(p)?;
        Ok(p.optional(fieldnamep)?.unwrap_or_default())
    })?);
    restofline(p)?;
    Ok(fs.iter().map(|f| f.to_lowercase()).collect())
}

/// A CSV field name, in double quotes or not (fieldnamep).
fn fieldnamep(p: &mut Parser) -> PResult<String> {
    or(
        p,
        |p| {
            p.char('"')?;
            let name = p.take_while1(None, |c| !"\"\n:;#~".contains(c))?;
            p.char('"')?;
            Ok(name.to_string())
        },
        |p| {
            p.take_while1(None, |c| !" \t\n,;#~".contains(c))
                .map(str::to_string)
        },
    )
}

/// `FIELD VALUE` or `FIELD: VALUE`, or a field with no value
/// (fieldassignmentp).
fn fieldassignmentp(p: &mut Parser, lines: &Lines) -> PResult<FieldAssignment> {
    p.label("field assignment", |p| {
        let pos = lines.pos(p.pos);
        let f = journalfieldnamep(p)?;
        let mut tried = Alts::new(p);
        let v = match tried.run(p, |p| {
            p.attempt(|p| {
                assignmentseparatorp(p)?;
                fieldvalp(p)
            })
        }) {
            Some(r) => r?,
            None => match tried.run(p, |p| p.attempt(eolof)) {
                Some(r) => r.map(|()| String::new())?,
                None => return Err(tried.choice_error(p)),
            },
        };
        Ok(FieldAssignment::new(&f, &v, pos))
    })
}

/// A hledger field name (journalfieldnamep).
fn journalfieldnamep(p: &mut Parser) -> PResult<String> {
    let mut tried = Alts::new(p);
    for name in journal_field_names() {
        if let Some(r) = tried.run(p, |p| p.attempt(|p| p.string(name))) {
            return r.map(str::to_string);
        }
    }
    Err(tried.choice_error(p))
}

/// A colon with optional spaces around it, or spaces (assignmentseparatorp).
fn assignmentseparatorp(p: &mut Parser) -> PResult<()> {
    choice_try(
        p,
        &mut [
            &mut |p: &mut Parser| {
                p.skip_spaces();
                p.char(':')?;
                p.skip_spaces();
                Ok(())
            },
            &mut spaces1,
        ],
    )
}

/// A field assignment's value: the rest of the line, as written, with its
/// `%{...}` expressions checked (fieldvalp).
fn fieldvalp(p: &mut Parser) -> PResult<String> {
    let start = p.pos;
    loop {
        let rest = p.rest();
        if rest.is_empty() {
            return Ok(p.src[start..p.pos].to_string());
        }
        if rest.starts_with('\n') {
            let v = p.src[start..p.pos].to_string();
            p.pos += 1;
            return Ok(v);
        }
        templatepartp(p, |c| c != '\n')?;
    }
}

/// Part of a template: text whose characters satisfy `ok`, or a `%{...}`
/// expression, which is checked here so that errors are reported where it
/// is written (templatepartp).
fn templatepartp(p: &mut Parser, ok: impl Fn(char) -> bool) -> PResult<()> {
    if p.rest().starts_with("%{") {
        return templateexprp(p).map(drop);
    }
    if p.take_while(None, |c| ok(c) && c != '%').is_empty() {
        p.satisfy(None, ok)?;
    }
    Ok(())
}

/// The label of a `%{...}` expression's terms.
const TERM_LABEL: &str = "function call, hledger field name, CSV field reference, match group reference, double-quoted string or number";

/// A `%{...}` expression. Unknown functions, wrong numbers of arguments,
/// bad arguments and unknown field names are errors here; syntax errors are
/// made fancy, so that alternatives tried later do not replace them
/// (templateexprp).
fn templateexprp(p: &mut Parser) -> PResult<Expr> {
    p.string("%{")?;
    let inner = |p: &mut Parser| {
        p.skip_spaces();
        let e = termp(p)?;
        p.skip_spaces();
        p.char('}')?;
        Ok(e)
    };
    inner(p).map_err(|e: ParseError| e.into_fancy(p.src))
}

fn termp(p: &mut Parser) -> PResult<Expr> {
    p.label(TERM_LABEL, |p| {
        let mut tried = Alts::new(p);
        if let Some(r) = tried.run(p, namedp) {
            return r;
        }
        if let Some(r) = tried.run(p, |p| templatefieldrefp(p).map(Expr::Field)) {
            return r;
        }
        if let Some(r) = tried.run(p, |p| templatematchrefp(p).map(Expr::Match)) {
            return r;
        }
        if let Some(r) = tried.run(p, |p| stringp(p).map(Expr::Str)) {
            return r;
        }
        if let Some(r) = tried.run(p, |p| numberp(p).map(Expr::Number)) {
            return r;
        }
        Err(tried.choice_error(p))
    })
}

/// A function call, or a bare name, which refers to a hledger field.
fn namedp(p: &mut Parser) -> PResult<Expr> {
    let off = p.pos;
    p.satisfy(None, |c| c.is_ascii_lowercase())?;
    p.take_while(None, |c| {
        c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'
    });
    let name = p.src[off..p.pos].to_string();
    let args = p.optional(|p| {
        p.attempt(|p| {
            p.skip_spaces();
            p.char('(')
        })?;
        p.skip_spaces();
        let args = p.sep_by(
            |p| {
                let t = termp(p)?;
                p.skip_spaces();
                Ok(t)
            },
            |p| {
                p.char(',')?;
                p.skip_spaces();
                Ok(())
            },
        )?;
        p.char(')')?;
        Ok(args)
    })?;
    let checked = match &args {
        Some(args) => check_template_call(&name, args),
        None => check_hledger_field_reference(&name),
    };
    if let Err(msg) = checked {
        return Err(ParseError::fail_at_raised(p.src, off, p.pos, msg));
    }
    Ok(match args {
        Some(args) => Expr::Call(name, args),
        None => Expr::HledgerField(name),
    })
}

/// Whether a character can be in a CSV field reference's name
/// (isFieldNameChar).
fn is_field_name_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

/// A CSV field reference in a template: `%NAME`, `%N`, or `%(NAME)`
/// (templatefieldrefp).
fn templatefieldrefp(p: &mut Parser) -> PResult<String> {
    or(
        p,
        |p| {
            p.attempt(|p| {
                p.string("%(")?;
                let name = p.take_while1(Some("reference"), is_field_name_char)?;
                p.char(')')?;
                Ok(format!("%({name})"))
            })
        },
        |p| {
            p.char('%')?;
            let name = p.take_while1(Some("reference"), is_field_name_char)?;
            Ok(format!("%{name}"))
        },
    )
}

/// A match group reference, `\N` (templatematchrefp).
fn templatematchrefp(p: &mut Parser) -> PResult<String> {
    p.char('\\')?;
    let digits = p.take_while1(Some("matchref"), |c| c.is_ascii_digit())?;
    Ok(digits.to_string())
}

/// A double-quoted string, in which `\"` and `\\` are escapes and other
/// backslashes are kept.
fn stringp(p: &mut Parser) -> PResult<String> {
    p.label("double-quoted string", |p| {
        p.char('"')?;
        let parts = p.many(|p| {
            or(
                p,
                |p| {
                    p.take_while1(None, |c| !matches!(c, '"' | '\\' | '\n'))
                        .map(str::to_string)
                },
                |p| {
                    p.char('\\')?;
                    Ok(p.optional(|p| p.satisfy(None, |c| c == '"' || c == '\\'))?
                        .map_or("\\".to_string(), String::from))
                },
            )
        })?;
        p.char('"')?;
        Ok(parts.concat())
    })
}

/// An integer, perhaps negative.
fn numberp(p: &mut Parser) -> PResult<i64> {
    let start = p.pos;
    p.optional(|p| p.string("-"))?;
    p.take_while1(Some("digit"), |c| c.is_ascii_digit())?;
    match read_int(&p.src[start..p.pos]) {
        Some(n) => Ok(n),
        None => p.fail("invalid number"),
    }
}

/// An if block: `if` and matchers, on its line or the next ones, then
/// indented assignments, with blank and comment lines among them
/// (conditionalblockp).
fn conditionalblockp(p: &mut Parser, lines: &Lines) -> PResult<ConditionalBlock> {
    p.label("if rule", |p| {
        let start = p.pos;
        p.string("if")?;
        let on_if_line = or(
            p,
            |p| p.char('\n').map(|_| false),
            |p| {
                spaces1(p)?;
                Ok(p.optional(|p| p.char('\n'))?.is_none())
            },
        )?;
        let moff = p.pos;
        let mut matchers = Vec::new();
        if on_if_line {
            matchers.push(matcherp(p, End::Line)?);
        }
        matchers.extend(p.many(|p| {
            p.attempt(|p| {
                p.many(|p| p.attempt(commentlinep))?;
                matcherp(p, End::Line)
            })
        })?);
        if matchers.is_empty() {
            return Err(ParseError::fail_at_raised(
                p.src,
                moff,
                p.pos,
                "start of if rule found, but no matchers afterward\n\
                 (matchers should be on the same line as \"if\", or on the following lines.\n\
                 Note: a line beginning with a comment character (# or ;) is a comment;\n\
                 to match a leading comment character, escape it, eg \\#)",
            ));
        }
        let assignments: Vec<FieldAssignment> = p
            .many(|p| {
                or(
                    p,
                    |p| blank_or_comment_linep(p).map(|()| None),
                    |p| {
                        spaces1(p)?;
                        or(
                            p,
                            |p| eolof(p).map(|()| None),
                            |p| fieldassignmentp(p, lines).map(Some),
                        )
                    },
                )
            })?
            .into_iter()
            .flatten()
            .collect();
        if assignments.is_empty() {
            return Err(ParseError::fail_at_raised(
                p.src,
                start,
                p.pos,
                "start of if rule found, but no assignment rules afterward\n\
                 (assignment rules in an if rule should be indented)",
            ));
        }
        Ok(ConditionalBlock {
            matchers,
            assignments,
        })
    })
}

/// An ifs table: `ifs` or `if`, a delimiter character, hledger field names
/// separated by it; then rows of matchers and values, separated by it,
/// with comment lines among them; until a blank line or the end. Each row
/// is a conditional block; its blank values assign nothing
/// (conditionaltablep).
fn conditionaltablep(p: &mut Parser, lines: &Lines) -> PResult<Vec<ConditionalBlock>> {
    p.label("ifs table", |p| {
        let start = p.pos;
        p.string("if")?;
        if p.optional(|p| p.char('s'))?.is_some() {
            p.skip_spaces();
        }
        let sep = p.label("delimiter (a punctuation character)", |p| {
            p.satisfy(None, |c| !(c.is_alphanumeric() || is_space(c)))
        })?;
        let fields = p.sep_by1(
            |p| {
                p.skip_spaces();
                let f = journalfieldnamep(p)?;
                p.skip_spaces();
                Ok(f)
            },
            |p| p.char(sep),
        )?;
        p.char('\n')?;
        let mut rows = Vec::new();
        loop {
            let end = p.optional(|p| or(p, |p| p.attempt(blanklinep), |p| p.eof()))?;
            if end.is_some() {
                break;
            }
            let row = or(
                p,
                |p| p.attempt(commentlinep).map(|()| None),
                |p| bodylinep(p, sep, &fields, lines).map(Some),
            )?;
            rows.extend(row);
        }
        if rows.is_empty() {
            return Err(ParseError::fail_at_raised(
                p.src,
                start,
                p.pos,
                "start of ifs table found, but no table rows afterward",
            ));
        }
        Ok(rows
            .into_iter()
            .map(|(matchers, values, pos)| ConditionalBlock {
                matchers,
                assignments: fields
                    .iter()
                    .zip(values)
                    .filter(|(_, v)| !strip(v).is_empty())
                    .map(|(f, v)| FieldAssignment::new(f, strip(&v), pos.clone()))
                    .collect(),
            })
            .collect())
    })
}

/// A row of an ifs table: matchers up to the delimiter, then as many values
/// as there are fields, separated by it.
#[allow(clippy::type_complexity)]
fn bodylinep(
    p: &mut Parser,
    sep: char,
    fields: &[String],
    lines: &Lines,
) -> PResult<(Vec<Matcher>, Vec<String>, RulesPos)> {
    let off = p.pos;
    let pos = lines.pos(off);
    let mut matchers = Vec::new();
    loop {
        if p.optional(|p| p.char(sep))?.is_some() {
            break;
        }
        matchers.push(matcherp(p, End::Sep(sep))?);
    }
    let ok = |c: char| c != sep && c != '\n';
    let values = p.sep_by(
        |p| {
            let start = p.pos;
            p.many(|p| templatepartp(p, ok))?;
            Ok(p.src[start..p.pos].to_string())
        },
        |p| p.char(sep),
    )?;
    eolof(p)?;
    if values.len() != fields.len() {
        let n = fields.len();
        let values_text = if n == 1 {
            "1 value".to_string()
        } else {
            format!("{n} values")
        };
        let msg = format!(
            "line of ifs table should have {values_text}, but this one has {}",
            values.len()
        );
        return Err(ParseError::fail_at_raised(p.src, off, p.pos, msg));
    }
    Ok((matchers, values, pos))
}

/// What ends a matcher's regular expression, besides `&&`: the end of the
/// line (which it consumes), or an ifs table's delimiter.
#[derive(Clone, Copy)]
enum End {
    Line,
    Sep(char),
}

/// A matcher: a field matcher, or if that fails a whole-record matcher
/// (matcherp').
fn matcherp(p: &mut Parser, end: End) -> PResult<Matcher> {
    or(
        p,
        |p| p.attempt(|p| fieldmatcherp(p, end)),
        |p| recordmatcherp(p, end),
    )
}

fn recordmatcherp(p: &mut Parser, end: End) -> PResult<Matcher> {
    p.label("record matcher", |p| {
        let prefix = matcherprefixp(p)?;
        let regex = regexp(p, end)?;
        Ok(Matcher {
            prefix,
            field: None,
            regex,
        })
    })
}

/// `%FIELD REGEX` (fieldmatcherp).
fn fieldmatcherp(p: &mut Parser, end: End) -> PResult<Matcher> {
    p.label("field matcher", |p| {
        let prefix = matcherprefixp(p)?;
        let field = csvfieldreferencep(p)?;
        p.skip_spaces();
        let regex = regexp(p, end)?;
        Ok(Matcher {
            prefix,
            field: Some(field),
            regex,
        })
    })
}

/// `&`, `&&`, `!`, `& !`, `&& !`, or nothing (matcherprefixp).
fn matcherprefixp(p: &mut Parser) -> PResult<MatcherPrefix> {
    let and = |p: &mut Parser| {
        p.char('&')?;
        p.optional(|p| p.char('&'))?;
        p.skip_spaces();
        let not = p.optional(|p| {
            p.char('!')?;
            p.skip_spaces();
            Ok(())
        })?;
        Ok(if not.is_some() {
            MatcherPrefix::AndNot
        } else {
            MatcherPrefix::And
        })
    };
    let not = |p: &mut Parser| {
        p.char('!')?;
        p.skip_spaces();
        Ok(MatcherPrefix::Not)
    };
    or(p, |p| or(p, and, not), |_| Ok(MatcherPrefix::Or))
}

/// `%` and a field name, which is quoted if it needs to be
/// (csvfieldreferencep).
fn csvfieldreferencep(p: &mut Parser) -> PResult<String> {
    p.char('%')?;
    Ok(format!("%{}", quote_if_needed(&fieldnamep(p)?)))
}

/// A case-insensitive regular expression: from a non-space character to
/// before `&&` or the end (regexp). Its text is stripped.
fn regexp(p: &mut Parser, end: End) -> PResult<Regexp> {
    let start = p.pos;
    p.satisfy(None, |c| !is_space(c))?;
    loop {
        let rest = p.rest();
        if rest.starts_with("&&") {
            break;
        }
        match end {
            End::Line if rest.is_empty() => break,
            End::Line if rest.starts_with('\n') => {
                p.pos += 1;
                break;
            }
            End::Sep(sep) if rest.starts_with(sep) => break,
            _ => {}
        }
        match rest.chars().next() {
            Some(c) => p.pos += c.len_utf8(),
            None => {
                let mut expected = vec![Item::tokens("&&")];
                if let End::Sep(sep) = end {
                    expected.push(Item::Tokens(sep.to_string()));
                }
                return Err(ParseError::trivial(
                    p.pos,
                    Some(Item::EndOfInput),
                    &expected,
                ));
            }
        }
    }
    let text = &p.src[start..p.pos];
    let text = text.strip_suffix('\n').unwrap_or(text);
    to_regex_ci(strip(text)).or_else(|e| p.fail(format!("CSV parser: {e}")))
}

/// An error if the hledger field references in `%{...}` expressions form a
/// loop, where a field's value would depend on itself through other fields
/// (a field's own name is its previous value, which is fine). It is
/// reported at the last-written assignment in the loop
/// (checkFieldReferenceLoops).
fn check_field_reference_loops(p: &Parser, lines: &Lines, rules: &CsvRules) -> PResult<()> {
    let all: Vec<&FieldAssignment> = rules
        .assignments
        .iter()
        .chain(rules.blocks.iter().flat_map(|b| &b.assignments))
        .collect();
    let other_refs = |a: &FieldAssignment| -> Vec<String> {
        a.template
            .hledger_field_refs()
            .into_iter()
            .filter(|f| *f != a.name)
            .collect()
    };
    // a vertex per assigned field, with edges to the fields its values refer to
    let mut graph: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for a in &all {
        let refs = graph.entry(&a.name).or_default();
        for r in other_refs(a) {
            if !refs.contains(&r) {
                refs.push(r);
            }
        }
    }
    let Some(lp) = first_cyclic_scc(&graph) else {
        return Ok(());
    };
    let mut names: Vec<&str> = lp.clone();
    names.sort();
    let in_loop = |a: &&&FieldAssignment| {
        lp.contains(&a.name.as_str()) && other_refs(a).iter().any(|r| lp.contains(&r.as_str()))
    };
    let mut candidates: Vec<&&FieldAssignment> = all.iter().filter(in_loop).collect();
    candidates.sort_by_key(|a| a.pos.as_ref().map(|(_, l)| *l));
    let line = candidates
        .last()
        .and_then(|a| a.pos.as_ref())
        .map_or(1, |(_, l)| *l);
    let msg = format!(
        "these hledger fields' values depend on each other in a loop: {}",
        names.join(", ")
    );
    Err(ParseError::fail_at_raised(
        p.src,
        lines.first_nonspace(p.src, line),
        p.pos,
        msg,
    ))
}

/// The first strongly connected component with a cycle, in the order
/// Data.Graph's stronglyConnComp gives them (vertices numbered in key order;
/// a depth-first search of the graph in the reverse post-order of the
/// transposed graph).
fn first_cyclic_scc<'a>(graph: &BTreeMap<&'a str, Vec<String>>) -> Option<Vec<&'a str>> {
    let keys: Vec<&str> = graph.keys().copied().collect();
    let n = keys.len();
    let index = |k: &str| keys.binary_search(&k).ok();
    let edges: Vec<Vec<usize>> = keys
        .iter()
        .map(|k| graph[k].iter().filter_map(|r| index(r)).collect())
        .collect();
    let mut transposed = vec![Vec::new(); n];
    for (v, es) in edges.iter().enumerate() {
        for &w in es {
            transposed[w].push(v);
        }
    }
    // post-order of the transposed graph, vertices visited in order
    fn post_order(g: &[Vec<usize>], v: usize, seen: &mut [bool], out: &mut Vec<usize>) {
        seen[v] = true;
        for &w in &g[v] {
            if !seen[w] {
                post_order(g, w, seen, out);
            }
        }
        out.push(v);
    }
    let mut seen = vec![false; n];
    let mut order = Vec::new();
    for v in 0..n {
        if !seen[v] {
            post_order(&transposed, v, &mut seen, &mut order);
        }
    }
    order.reverse();
    // each tree of a depth-first search of the graph in that order is a component
    fn collect(g: &[Vec<usize>], v: usize, seen: &mut [bool], out: &mut Vec<usize>) {
        seen[v] = true;
        out.push(v);
        for &w in &g[v] {
            if !seen[w] {
                collect(g, w, seen, out);
            }
        }
    }
    let mut seen = vec![false; n];
    for v in order {
        if seen[v] {
            continue;
        }
        let mut component = Vec::new();
        collect(&edges, v, &mut seen, &mut component);
        if component.len() > 1 || edges[v].contains(&v) {
            return Some(component.into_iter().map(|i| keys[i]).collect());
        }
    }
    None
}

/// Check a template function call's name and arguments (checkTemplateCall).
fn check_template_call(name: &str, args: &[Expr]) -> Result<(), String> {
    let Some(f) = template_function(name) else {
        let names: Vec<&str> = TEMPLATE_FUNCTIONS.iter().map(|f| f.name).collect();
        return Err(format!(
            "unknown function \"{name}\" (known functions: {})",
            names.join(", ")
        ));
    };
    let n = args.len();
    let (lo, hi) = f.arity;
    if n < lo || hi.is_some_and(|hi| n > hi) {
        let plural = |k: usize| {
            if k == 1 {
                "1 argument".to_string()
            } else {
                format!("{k} arguments")
            }
        };
        let arity = match hi {
            Some(hi) if hi == lo => plural(lo),
            Some(hi) => format!("{lo} to {hi} arguments"),
            None => format!("{lo} or more arguments"),
        };
        return Err(format!("{name} takes {arity}, but was given {n}"));
    }
    match name {
        "replace" => check_replace(args),
        "substr" => check_substr(args),
        _ => Ok(()),
    }
}

/// Check that a bare name in a `%{...}` expression is a hledger field that
/// can be referred to (checkHledgerFieldReference). skip, end and merge are
/// actions, not values.
fn check_hledger_field_reference(name: &str) -> Result<(), String> {
    if template_function(name).is_some() {
        return Err(format!(
            "{name} is a function; it should be followed by parentheses, eg {name}(...)"
        ));
    }
    if journal_field_names().iter().any(|f| f == name) && !matches!(name, "skip" | "end" | "merge")
    {
        return Ok(());
    }
    Err(format!(
        "unknown hledger field \"{name}\". To refer to a CSV field, write %{name}. The hledger \
         fields are: date, date2, status, code, description, comment, amount, balance, currency, \
         and commentN, accountN, amountN, balanceN, currencyN (N = 1-99)"
    ))
}

/// A function usable in `%{...}` expressions: its name and its minimum and
/// (if limited) maximum number of arguments (TemplateFunction).
struct TemplateFunction {
    name: &'static str,
    arity: (usize, Option<usize>),
}

const TEMPLATE_FUNCTIONS: [TemplateFunction; 12] = [
    tf("upper", 1, Some(1)),
    tf("lower", 1, Some(1)),
    tf("capitalize", 1, Some(1)),
    tf("capitalise", 1, Some(1)),
    tf("trim", 1, Some(1)),
    tf("negate", 1, Some(1)),
    tf("abs", 1, Some(1)),
    tf("concat", 1, None),
    tf("join", 2, None),
    tf("default", 1, None),
    tf("replace", 3, Some(3)),
    tf("substr", 2, Some(3)),
];

const fn tf(name: &'static str, lo: usize, hi: Option<usize>) -> TemplateFunction {
    TemplateFunction {
        name,
        arity: (lo, hi),
    }
}

fn template_function(name: &str) -> Option<&'static TemplateFunction> {
    TEMPLATE_FUNCTIONS.iter().find(|f| f.name == name)
}

/// replace's regular expression must be a string, and valid, and its
/// replacement may not refer to more groups than it has.
fn check_replace(args: &[Expr]) -> Result<(), String> {
    let [_, Expr::Str(r), repl] = args else {
        return Err(
            "replace's second argument (the regular expression) should be a double-quoted string"
                .to_string(),
        );
    };
    to_regex_ci(r)?;
    if let Expr::Str(t) = repl
        && max_backref(t) > regex_group_count(r)
    {
        return Err(format!(
            "replace's replacement refers to match group \\{}, but the regular expression has only {} group(s)",
            max_backref(t),
            regex_group_count(r)
        ));
    }
    Ok(())
}

/// substr's position must be a number other than 0, and its length a number
/// at least 0.
fn check_substr(args: &[Expr]) -> Result<(), String> {
    match args {
        [_, Expr::Number(start), rest @ ..]
            if *start != 0 && rest.iter().all(|e| matches!(e, Expr::Number(n) if *n >= 0)) =>
        {
            Ok(())
        }
        _ => Err("substr's position should be a number other than 0 (1 is the first character, -1 the last), and its length a number, at least 0".to_string()),
    }
}

/// The highest `\N` backreference in a replacement.
fn max_backref(s: &str) -> usize {
    let mut max = 0;
    let mut rest = s;
    while let Some(i) = rest.find('\\') {
        rest = &rest[i + 1..];
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 {
            max = max.max(rest[..digits].parse().unwrap_or(0));
            rest = &rest[digits..];
        }
    }
    max
}

/// The number of parenthesised groups in a regular expression, roughly:
/// open parentheses not escaped or in a bracket expression.
fn regex_group_count(s: &str) -> usize {
    let cs: Vec<char> = s.chars().collect();
    let (mut i, mut n) = (0, 0);
    while i < cs.len() {
        match cs[i] {
            '\\' if i + 1 < cs.len() => i += 2,
            '[' => {
                // a ] first (after any ^) is literal
                i += 1;
                if cs.get(i) == Some(&'^') {
                    i += 1;
                }
                i += 1;
                while i < cs.len() && cs[i] != ']' {
                    i += 1;
                }
                i += 1;
            }
            '(' => {
                n += 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    n
}

impl Template {
    /// A template as written, read into its parts if possible.
    pub fn new(text: &str) -> Template {
        Template {
            text: text.to_string(),
            parts: template_parts(text),
        }
    }

    /// The hledger fields its `%{...}` expressions refer to
    /// (templateHledgerFieldRefs).
    fn hledger_field_refs(&self) -> Vec<String> {
        fn refs(e: &Expr, out: &mut Vec<String>) {
            match e {
                Expr::Call(_, args) => args.iter().for_each(|a| refs(a, out)),
                Expr::HledgerField(f) => out.push(f.clone()),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for part in self.parts.iter().flatten() {
            if let Part::Expr(e) = part {
                refs(e, &mut out);
            }
        }
        out
    }
}

/// A template's parts: literal text, match group references, `%{...}`
/// expressions and CSV field references; or none if it can not be read so
/// (templateParts).
fn template_parts(s: &str) -> Option<Vec<Part>> {
    let mut p = Parser::new(s);
    let mut parts = Vec::new();
    let mut text = String::new();
    while let Some(c) = p.rest().chars().next() {
        let next = p.rest()[c.len_utf8()..].chars().next();
        let part = match (c, next) {
            ('\\', Some(d)) if d.is_ascii_digit() => Part::Match(templatematchrefp(&mut p).ok()?),
            ('%', Some('{')) => Part::Expr(templateexprp(&mut p).ok()?),
            ('%', Some(n)) if n == '(' || is_field_name_char(n) => {
                Part::Field(templatefieldrefp(&mut p).ok()?)
            }
            _ => {
                text.push(c);
                p.pos += c.len_utf8();
                continue;
            }
        };
        if !text.is_empty() {
            parts.push(Part::Text(std::mem::take(&mut text)));
        }
        parts.push(part);
    }
    if !text.is_empty() {
        parts.push(Part::Text(text));
    }
    Some(parts)
}

/// Which conditional blocks' matchers a template's match group references
/// use: all of them for a top-level assignment, or its own block's.
pub type Scope = Option<usize>;

impl CsvRules {
    /// A directive's value, the last one if it is written more than once
    /// (getDirective).
    pub fn directive(&self, name: &str) -> Option<&str> {
        self.directives
            .iter()
            .rev()
            .find(|(d, _)| d == name)
            .map(|(_, v)| v.as_str())
    }

    /// A directive's value, the first one if it is written more than once
    /// (getDirectiveFirstWins).
    pub fn directive_first_wins(&self, name: &str) -> Option<&str> {
        self.directives
            .iter()
            .find(|(d, _)| d == name)
            .map(|(_, v)| v.as_str())
    }

    /// The assignments to a hledger field that apply to a record, in the
    /// order they take effect: top-level ones, then those in the if blocks
    /// that match; each with the scope of its match group references
    /// (hledgerFieldAssignments).
    pub fn field_assignments(
        &self,
        record: &[CsvRecord],
        f: &str,
    ) -> Vec<(&FieldAssignment, Scope)> {
        let Some((top, blocks)) = self.by_field.get(f) else {
            return Vec::new();
        };
        let mut found: Vec<(&FieldAssignment, Scope)> =
            top.iter().map(|&i| (&self.assignments[i], None)).collect();
        for &i in blocks {
            let b = &self.blocks[i];
            if self.block_active(record, b) {
                found.extend(
                    b.assignments
                        .iter()
                        .filter(|a| a.name == f)
                        .map(|a| (a, Some(i))),
                );
            }
        }
        found
    }

    /// The assignment in effect for a hledger field (hledgerFieldAssignment).
    pub fn field_assignment(&self, record: &[CsvRecord], f: &str) -> Option<&FieldAssignment> {
        self.field_assignments(record, f).pop().map(|(a, _)| a)
    }

    /// The value template in effect for a hledger field (hledgerField).
    pub fn field_template(&self, record: &[CsvRecord], f: &str) -> Option<&str> {
        self.field_assignment(record, f)
            .map(|a| a.template.text.as_str())
    }

    /// The final value of a hledger field for a record, with CSV field
    /// references, match group references and `%{...}` expressions
    /// interpolated (hledgerFieldValue). In an assignment's expressions, the
    /// field's own name gives its value from its previous assignment, and
    /// another hledger field's name that field's final value; both are
    /// empty if there is none.
    pub fn field_value(&self, record: &[CsvRecord], f: &str) -> Option<String> {
        self.field_value_seen(record, f, &mut Vec::new())
    }

    fn field_value_seen(
        &self,
        record: &[CsvRecord],
        f: &str,
        seen: &mut Vec<String>,
    ) -> Option<String> {
        let assignments = self.field_assignments(record, f);
        seen.push(f.to_string());
        let v = self.value_from(record, f, &assignments, seen);
        seen.pop();
        v
    }

    /// The value of the last of these assignments.
    fn value_from(
        &self,
        record: &[CsvRecord],
        f: &str,
        assignments: &[(&FieldAssignment, Scope)],
        seen: &mut Vec<String>,
    ) -> Option<String> {
        let ((a, scope), earlier) = assignments.split_last()?;
        let mut lookup = |g: &str, seen: &mut Vec<String>| {
            if g == f {
                self.value_from(record, f, earlier, seen)
                    .unwrap_or_default()
            } else if seen.iter().any(|s| s == g) {
                String::new()
            } else {
                self.field_value_seen(record, g, seen).unwrap_or_default()
            }
        };
        Some(self.render(*scope, record, &a.template, &mut lookup, seen))
    }

    /// A template rendered for a record (renderTemplate). Interpolated values
    /// have their outer white space removed.
    fn render(
        &self,
        scope: Scope,
        record: &[CsvRecord],
        t: &Template,
        lookup: &mut dyn FnMut(&str, &mut Vec<String>) -> String,
        seen: &mut Vec<String>,
    ) -> String {
        let Some(parts) = &t.parts else {
            return t.text.clone();
        };
        let mut out = String::new();
        for part in parts {
            match part {
                Part::Text(s) => out.push_str(s),
                Part::Match(n) => {
                    out.push_str(&self.match_group(scope, record, n).unwrap_or_default())
                }
                Part::Expr(e) => out.push_str(strip(&self.eval(scope, record, e, lookup, seen))),
                Part::Field(r) => {
                    out.push_str(&self.field_reference(record, r).unwrap_or_default())
                }
            }
        }
        out
    }

    /// A `%{...}` expression's value (evalTemplateExpr, before stripping).
    fn eval(
        &self,
        scope: Scope,
        record: &[CsvRecord],
        e: &Expr,
        lookup: &mut dyn FnMut(&str, &mut Vec<String>) -> String,
        seen: &mut Vec<String>,
    ) -> String {
        match e {
            Expr::Call(name, args) => {
                let args: Vec<String> = args
                    .iter()
                    .map(|a| self.eval(scope, record, a, lookup, seen))
                    .collect();
                apply_template_function(name, &args)
            }
            Expr::Field(r) => self.field_reference(record, r).unwrap_or_default(),
            Expr::HledgerField(f) => strip(&lookup(f, seen)).to_string(),
            Expr::Match(n) => self.match_group(scope, record, n).unwrap_or_default(),
            Expr::Str(s) => s.clone(),
            Expr::Number(n) => n.to_string(),
        }
    }

    /// The value of match group N of the matchers of the conditional blocks
    /// in scope that match the record, counting the groups of all their
    /// matchers in order (regexMatchValue).
    fn match_group(&self, scope: Scope, record: &[CsvRecord], n: &str) -> Option<String> {
        let blocks: Vec<&ConditionalBlock> = match scope {
            Some(i) => vec![&self.blocks[i]],
            None => self.blocks.iter().collect(),
        };
        let n: usize = n.parse().ok()?;
        let mut groups = blocks
            .into_iter()
            .filter(|b| self.block_active(record, b))
            .flat_map(|b| &b.matchers)
            .flat_map(|m| m.regex.match_groups(&self.matched_text(record, m)));
        groups.nth(n.checked_sub(1)?)
    }

    /// Whether a conditional block's matchers match a record: a matcher and
    /// the `&` matchers after it all match, for some matcher without `&`
    /// (isBlockActive).
    pub fn block_active(&self, record: &[CsvRecord], b: &ConditionalBlock) -> bool {
        let mut groups: Vec<Vec<(MatcherPrefix, &Matcher)>> = Vec::new();
        for m in &b.matchers {
            match (m.prefix, groups.last_mut()) {
                (MatcherPrefix::And, Some(g)) if !g.is_empty() => g.push((MatcherPrefix::And, m)),
                (MatcherPrefix::AndNot, Some(g)) if !g.is_empty() => {
                    g.push((MatcherPrefix::Not, m))
                }
                (prefix, _) => groups.push(vec![(prefix, m)]),
            }
        }
        groups.iter().any(|g| {
            g.iter().all(|(prefix, m)| {
                let found = m.regex.is_match(&self.matched_text(record, m));
                if *prefix == MatcherPrefix::Not {
                    !found
                } else {
                    found
                }
            })
        })
    }

    /// The text a matcher is matched against: its field's value, or the
    /// whole record with its fields joined by commas.
    fn matched_text(&self, record: &[CsvRecord], m: &Matcher) -> String {
        match &m.field {
            Some(r) => self.field_reference(record, r).unwrap_or_default(),
            None => record_as_approximate_text(record),
        }
    }

    /// The value of a CSV field reference, `%NAME`, `%N` or `%(NAME)`, if
    /// the field exists (replaceCsvFieldReference).
    pub fn field_reference(&self, record: &[CsvRecord], r: &str) -> Option<String> {
        let rest = r.strip_prefix('%')?;
        let name = match rest.strip_prefix('(') {
            Some(inner) if !inner.is_empty() => {
                &inner[..inner.len() - inner.chars().last().map_or(0, char::len_utf8)]
            }
            _ => rest,
        };
        self.csv_field_value(record, name)
    }

    /// The value of a CSV field of a record group, its white space removed:
    /// named by a fields list or numbered, in the group's first record, or
    /// with a `_ROWNUM` suffix in a later one; a declared name wins over the
    /// suffix reading (csvFieldValue).
    fn csv_field_value(&self, record: &[CsvRecord], name: &str) -> Option<String> {
        let index = |f: &str| -> Option<usize> {
            if f.chars().all(|c| c.is_ascii_digit()) {
                f.parse().ok()
            } else {
                let f = f.to_lowercase();
                self.field_indexes
                    .iter()
                    .find(|(n, _)| *n == f)
                    .map(|(_, i)| *i)
            }
        };
        let value_at = |row: usize, i: usize| {
            record
                .get(row.checked_sub(1)?)?
                .get(i.checked_sub(1)?)
                .map(|v| strip(v).to_string())
        };
        match index(name) {
            Some(i) => value_at(1, i),
            None => {
                let (base, row) = split_row_suffix(name)?;
                value_at(row, index(base)?)
            }
        }
    }
}

/// A reference's name and row number, if it has a `_N` suffix with N at
/// least 2 (splitRowSuffix).
fn split_row_suffix(f: &str) -> Option<(&str, usize)> {
    let i = f.rfind('_')?;
    let (base, digits) = (&f[..i], &f[i + 1..]);
    if base.is_empty() || digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let row: usize = digits.parse().ok()?;
    (row >= 2).then_some((base, row))
}

/// A record group's field values joined by commas, approximating the
/// original record, for whole-record matching (recordAsApproximateText).
pub fn record_as_approximate_text(record: &[CsvRecord]) -> String {
    record
        .iter()
        .flatten()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(",")
}

/// A template function applied to its arguments' values.
fn apply_template_function(name: &str, args: &[String]) -> String {
    let arg1 = || args.first().cloned().unwrap_or_default();
    let non_blank = |s: &&String| !strip(s).is_empty();
    match name {
        "upper" => arg1().to_uppercase(),
        "lower" => arg1().to_lowercase(),
        "capitalize" | "capitalise" => capitalize(&arg1()),
        "trim" => strip(&arg1()).to_string(),
        "negate" => signed_part(&arg1(), |t| simplify_sign(&negate_str(t))),
        "abs" => signed_part(&arg1(), |t| {
            let s = simplify_sign(t);
            s.strip_prefix('-').map(str::to_string).unwrap_or(s)
        }),
        "concat" => args.concat(),
        "join" => match args.split_first() {
            Some((sep, ts)) => ts
                .iter()
                .filter(non_blank)
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(sep),
            None => String::new(),
        },
        "default" => args.iter().find(non_blank).cloned().unwrap_or_default(),
        "replace" => match args {
            [s, r, repl, ..] => to_regex_ci(r)
                .and_then(|re| re.replace(repl, s))
                .unwrap_or_else(|_| s.clone()),
            _ => arg1(),
        },
        "substr" => match args {
            [s, start, rest @ ..] => {
                let i = read_int(start).unwrap_or(1);
                let chars: Vec<char> = s.chars().collect();
                let from = if i < 0 {
                    chars.len().saturating_sub(i.unsigned_abs() as usize)
                } else {
                    (i.max(1) as usize - 1).min(chars.len())
                };
                let s2 = &chars[from..];
                let len = match rest.first() {
                    Some(len) => read_int(len).map_or(s2.len(), |n| n.max(0) as usize),
                    None => s2.len(),
                };
                s2[..len.min(s2.len())].iter().collect()
            }
            _ => arg1(),
        },
        _ => String::new(),
    }
}

/// Each white-space-separated word's first letter upper-cased and the rest
/// lower-cased.
fn capitalize(s: &str) -> String {
    let mut at_start = true;
    s.chars()
        .map(|c| {
            let out = if at_start {
                simple_case(c, c.to_uppercase())
            } else {
                simple_case(c, c.to_lowercase())
            };
            at_start = is_space(c);
            out
        })
        .collect()
}

/// A character's case mapping when that is one character (Data.Char's
/// toUpper and toLower), else the character.
fn simple_case(c: char, mut mapped: impl Iterator<Item = char>) -> char {
    match (mapped.next(), mapped.next()) {
        (Some(m), None) => m,
        _ => c,
    }
}

/// A sign-changing function applied to an amount text, or to the part after
/// a commodity symbol written before the sign (the -5 of $-5).
fn signed_part(t: &str, f: impl Fn(&str) -> String) -> String {
    let is_sign = |c: char| matches!(c, '-' | '+' | '(');
    let i = t
        .find(|c: char| c.is_ascii_digit() || is_sign(c))
        .unwrap_or(t.len());
    let (sym, rest) = t.split_at(i);
    match rest.chars().next() {
        Some(c) if !sym.is_empty() && is_sign(c) => format!("{sym}{}", f(rest)),
        _ => f(t),
    }
}

/// An amount text with its sign made canonical: parentheses mean negative,
/// two signs cancel out, a plus sign is dropped, and a sign with no number
/// is removed (simplifySign).
pub fn simplify_sign(s: &str) -> String {
    if let Some(t) = s.strip_prefix(' ') {
        return simplify_sign(t);
    }
    if let Some(t) = s.strip_suffix(' ') {
        return simplify_sign(t);
    }
    if let Some(amt) = s.strip_prefix('(').and_then(|t| t.strip_suffix(')')) {
        return simplify_sign(&negate_str(amt));
    }
    if let Some(amt) = s.strip_prefix("-(").and_then(|t| t.strip_suffix(')')) {
        return simplify_sign(amt);
    }
    if let Some(amt) = s.strip_prefix("--") {
        return amt.to_string();
    }
    if let Some(amt) = s.strip_prefix("-+") {
        return negate_str(amt);
    }
    if matches!(s, "-" | "+" | "()") {
        return String::new();
    }
    if let Some(amt) = s.strip_prefix('+') {
        return simplify_sign(amt);
    }
    s.to_string()
}

/// An amount text negated: a leading minus removed, or added (negateStr).
pub fn negate_str(s: &str) -> String {
    match s.strip_prefix('-') {
        Some(t) => t.to_string(),
        None => format!("-{s}"),
    }
}

/// Whether a CSV amount value has an arithmetic operator, making it an
/// amount expression: a `*` or `/` anywhere, or a `+` or `-` that is not a
/// leading sign (after only spaces, opening parentheses or other signs) or
/// an exponent's sign (after a digit and `e`); double-quoted text is ignored
/// (isAmountExpression).
pub fn is_amount_expression(s: &str) -> bool {
    let cs: Vec<char> = s.chars().collect();
    let mut prevs: Vec<char> = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c == '"' {
            prevs.push('"');
            i += 1;
            while i < cs.len() && cs[i] != '"' {
                i += 1;
            }
            i += 1;
            continue;
        }
        if c == '*' || c == '/' {
            return true;
        }
        if c == '+' || c == '-' {
            let n = prevs.len();
            let exponent =
                n >= 2 && matches!(prevs[n - 1], 'e' | 'E') && prevs[n - 2].is_ascii_digit();
            let operator = !exponent
                && prevs
                    .iter()
                    .rev()
                    .find(|&&p| p != ' ')
                    .is_some_and(|p| !matches!(p, '(' | '+' | '-'));
            if operator {
                return true;
            }
        }
        prevs.push(c);
        i += 1;
    }
    false
}

/// A balance assertion type, `=`, `==`, `=*` or `==*`, as (total, inclusive)
/// (parseBalanceAssertionType).
pub fn parse_balance_assertion_type(s: &str) -> Option<(bool, bool)> {
    match s {
        "=" => Some((false, false)),
        "==" => Some((true, false)),
        "=*" => Some((false, true)),
        "==*" => Some((true, true)),
        _ => None,
    }
}

/// An integer read as Haskell's read reads an Int: with surrounding white
/// space, a minus sign (perhaps followed by space), and decimal digits, or
/// hexadecimal or octal ones after `0x` or `0o` (readMay).
pub fn read_int(s: &str) -> Option<i64> {
    let s = strip(s);
    let (neg, s) = match s.strip_prefix('-') {
        Some(rest) => (true, rest.trim_start_matches(is_space)),
        None => (false, s),
    };
    let lower = s.to_ascii_lowercase();
    let (digits, radix) = if let Some(h) = lower.strip_prefix("0x") {
        (h.to_string(), 16)
    } else if let Some(o) = lower.strip_prefix("0o") {
        (o.to_string(), 8)
    } else {
        (s.to_string(), 10)
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    let n = i128::from_str_radix(&digits, radix).ok()?;
    let n = if neg { -n } else { n };
    Some(n as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regex::to_regex;

    fn rec(fields: &[&str]) -> Vec<CsvRecord> {
        vec![fields.iter().map(|s| s.to_string()).collect()]
    }

    fn fa(name: &str, template: &str) -> FieldAssignment {
        FieldAssignment::new(name, template, None)
    }

    fn parse(text: &str) -> PResult<CsvRules> {
        let lines = Lines::new("", text);
        rulesp(&mut Parser::new(text), &lines)
    }

    fn matcher(text: &str) -> PResult<Matcher> {
        let mut p = Parser::new(text);
        matcherp(&mut p, End::Line)
    }

    fn record_matcher(prefix: MatcherPrefix, re: &str) -> Matcher {
        Matcher {
            prefix,
            field: None,
            regex: to_regex_ci(re).unwrap(),
        }
    }

    fn field_matcher(prefix: MatcherPrefix, f: &str, re: &str) -> Matcher {
        Matcher {
            prefix,
            field: Some(f.to_string()),
            regex: to_regex_ci(re).unwrap(),
        }
    }

    // tests_RulesReader: parseCsvRules, rulesp
    #[test]
    fn rules_parsing() {
        let skip = |r: CsvRules| r.directives;
        assert!(parse("").unwrap().directives.is_empty());
        for text in ["skip\n# \n#\n", "skip\n\n  \n", "skip"] {
            assert_eq!(
                skip(parse(text).unwrap()),
                vec![("skip".to_string(), String::new())]
            );
        }
        let r = parse("account1 \nif foo\n  account2 foo\n").unwrap();
        assert_eq!(r.assignments, vec![fa("account1", "")]);
        assert_eq!(
            r.blocks,
            vec![ConditionalBlock {
                matchers: vec![record_matcher(MatcherPrefix::Or, "foo")],
                assignments: vec![fa("account2", "foo")],
            }]
        );
    }

    // tests_RulesReader: conditionalblockp, csvfieldreferencep, recordmatcherp,
    // fieldmatcherp, regexp, "Combine multiple matchers on the same line"
    #[test]
    fn matchers_and_blocks() {
        let lines = Lines::new("", "");
        let block = |s: &str| conditionalblockp(&mut Parser::new(s), &lines);
        assert_eq!(
            block("if a\n account2 b\n \n").unwrap(),
            ConditionalBlock {
                matchers: vec![record_matcher(MatcherPrefix::Or, "a")],
                assignments: vec![fa("account2", "b")],
            }
        );
        let fieldref = |s: &str| csvfieldreferencep(&mut Parser::new(s));
        assert_eq!(fieldref("%1").unwrap(), "%1");
        assert_eq!(fieldref("%date").unwrap(), "%date");
        assert_eq!(fieldref("%\"csv date\"").unwrap(), "%\"csv date\"");
        use MatcherPrefix::*;
        assert_eq!(matcher("A A\n").unwrap(), record_matcher(Or, "A A"));
        assert_eq!(matcher("& A A\n").unwrap(), record_matcher(And, "A A"));
        assert_eq!(matcher("&& A A\n").unwrap(), record_matcher(And, "A A"));
        assert_eq!(
            matcher("&& ! A A\n").unwrap(),
            record_matcher(AndNot, "A A")
        );
        assert_eq!(
            matcher("description A A\n").unwrap(),
            record_matcher(Or, "description A A")
        );
        assert_eq!(
            matcher("%description A A\n").unwrap(),
            field_matcher(Or, "%description", "A A")
        );
        assert_eq!(
            matcher("& %description A A\n").unwrap(),
            field_matcher(And, "%description", "A A")
        );
        assert_eq!(
            matcher("&& %description A A\n").unwrap(),
            field_matcher(And, "%description", "A A")
        );
        assert_eq!(
            matcher("&& ! %description A A\n").unwrap(),
            field_matcher(AndNot, "%description", "A A")
        );
        let re = |s: &str| regexp(&mut Parser::new(s), End::Line).unwrap();
        assert_eq!(re("A A && xxx"), to_regex_ci("A A").unwrap());
        assert_eq!(re("A & B"), to_regex_ci("A & B").unwrap());
        let both = ConditionalBlock {
            matchers: vec![record_matcher(Or, "A"), record_matcher(And, "B")],
            assignments: vec![fa("account2", "foo"), fa("comment2", "bar")],
        };
        assert_eq!(
            block("if A && B\n account2 foo\n comment2 bar").unwrap(),
            both
        );
        assert_eq!(
            conditionaltablep(
                &mut Parser::new("if,account2,comment2\nA && B,foo,bar"),
                &lines
            )
            .unwrap(),
            vec![both]
        );
    }

    fn rules_with(
        indexes: &[(&str, usize)],
        assignments: Vec<FieldAssignment>,
        blocks: Vec<ConditionalBlock>,
    ) -> CsvRules {
        CsvRules::new(
            Vec::new(),
            indexes.iter().map(|(n, i)| (n.to_string(), *i)).collect(),
            assignments,
            blocks,
        )
    }

    fn date_block(matchers: Vec<Matcher>) -> ConditionalBlock {
        ConditionalBlock {
            matchers,
            assignments: vec![fa("date", "%csvdate")],
        }
    }

    fn fm(prefix: MatcherPrefix, f: &str, re: &str) -> Matcher {
        Matcher {
            prefix,
            field: Some(f.to_string()),
            regex: to_regex(re).unwrap(),
        }
    }

    // tests_RulesReader: hledgerField
    #[test]
    fn field_templates() {
        use MatcherPrefix::*;
        let one = [("csvdate", 1)];
        let two = [("csvdate", 1), ("description", 2)];
        let r = rules_with(&one, vec![fa("date", "%csvdate")], vec![]);
        assert_eq!(
            r.field_template(&rec(&["a", "b"]), "date"),
            Some("%csvdate")
        );
        let r = rules_with(
            &one,
            vec![],
            vec![date_block(vec![fm(Or, "%csvdate", "a")])],
        );
        assert_eq!(
            r.field_template(&rec(&["a", "b"]), "date"),
            Some("%csvdate")
        );
        let r = rules_with(
            &one,
            vec![],
            vec![date_block(vec![fm(Not, "%csvdate", "a")])],
        );
        assert_eq!(r.field_template(&rec(&["a", "b"]), "date"), None);
        let r = rules_with(
            &one,
            vec![],
            vec![date_block(vec![fm(Not, "%csvdate", "b")])],
        );
        assert_eq!(
            r.field_template(&rec(&["a", "b"]), "date"),
            Some("%csvdate")
        );
        let or_block = date_block(vec![fm(Or, "%csvdate", "a"), fm(Or, "%description", "b")]);
        let r = rules_with(&two, vec![], vec![or_block]);
        assert_eq!(r.field_template(&rec(&["a"]), "date"), Some("%csvdate"));
        assert_eq!(
            r.field_template(&rec(&["_", "b"]), "date"),
            Some("%csvdate")
        );
        let and_block = date_block(vec![fm(Or, "%csvdate", "a"), fm(And, "%description", "b")]);
        let r = rules_with(&two, vec![], vec![and_block]);
        assert_eq!(
            r.field_template(&rec(&["a", "b"]), "date"),
            Some("%csvdate")
        );
        let and_or_block = date_block(vec![
            fm(Or, "%csvdate", "a"),
            fm(And, "%description", "b"),
            fm(Or, "%description", "c"),
        ]);
        let r = rules_with(&two, vec![], vec![and_or_block]);
        assert_eq!(
            r.field_template(&rec(&["_", "c"]), "date"),
            Some("%csvdate")
        );
    }

    // tests_RulesReader: hledgerFieldValue (scoped match groups, #2158)
    #[test]
    fn field_values_with_match_groups() {
        use MatcherPrefix::*;
        let r = rules_with(
            &[("date", 1), ("description", 2)],
            vec![fa("account2", "equity"), fa("amount1", "1")],
            vec![
                ConditionalBlock {
                    matchers: vec![fm(Or, "%description", "PREFIX (.*)")],
                    assignments: vec![fa("account1", "account:\\1"), fa("comment1", "\\1")],
                },
                ConditionalBlock {
                    matchers: vec![fm(Or, "%description", "PREFIX (.*) - (.*)")],
                    assignments: vec![fa("account1", "account:\\1:\\2")],
                },
            ],
        );
        let record = rec(&["2019-02-01", "PREFIX Text 1 - Text 2"]);
        assert_eq!(
            r.field_value(&record, "account1").as_deref(),
            Some("account:Text 1:Text 2")
        );
        assert_eq!(
            r.field_value(&record, "comment1").as_deref(),
            Some("Text 1 - Text 2")
        );
    }

    // the doctests of isAmountExpression and simplifySign
    #[test]
    fn amount_texts() {
        let inputs = [
            "5",
            "-5",
            "+5",
            "(5)",
            "-(5)",
            "--5",
            "1 + 2",
            "-(1 + 2)",
            "(1) + 2",
            "2 * 3",
            "\"A-B\" 1",
            "(1e-5)",
            "5 e-3",
        ];
        let expected = [
            false, false, false, false, false, false, true, true, true, true, false, false, true,
        ];
        let found: Vec<bool> = inputs.iter().map(|s| is_amount_expression(s)).collect();
        assert_eq!(found, expected);
        for (s, simplified) in [
            ("1", "1"),
            ("+1", "1"),
            ("-1", "-1"),
            ("(1)", "-1"),
            ("--1", "1"),
            ("-(1)", "1"),
            ("-+1", "-1"),
            ("(-1)", "1"),
            ("((1))", "1"),
            ("-", ""),
            ("()", ""),
            ("+", ""),
        ] {
            assert_eq!(simplify_sign(s), simplified, "{s}");
        }
    }

    #[test]
    fn template_functions() {
        let r = rules_with(&[("a", 1)], vec![], vec![]);
        let value = |t: &str, fields: &[&str]| {
            let r2 = CsvRules::new(
                Vec::new(),
                r.field_indexes.clone(),
                vec![fa("comment", t)],
                vec![],
            );
            r2.field_value(&rec(fields), "comment").unwrap()
        };
        assert_eq!(value("%{negate(%a)}", &["$-5"]), "$5");
        assert_eq!(value("%{abs(%a)}", &["(5 USD)"]), "5 USD");
        assert_eq!(value("%{capitalize(%a)}", &["aMAZON mKTP"]), "Amazon Mktp");
        assert_eq!(value("%{substr(%a, -8, 4)}", &["4111222233334444"]), "3333");
        assert_eq!(value("%{substr(%a, -99, 3)}", &["4111222233334444"]), "411");
        assert_eq!(
            value(
                "%{replace(%a, \".*ref#([0-9]+).*\", \"\\1\")}",
                &["x REF#123 y"]
            ),
            "123"
        );
        assert_eq!(value("%{join(\", \", %a, \" \", \"x\")}", &["a"]), "a, x");
        // a template that can not be read into parts is used as written
        assert_eq!(value("%(a b %a", &["v"]), "%(a b %a");
        assert_eq!(regex_group_count("(a)[(]\\(b(c)"), 2);
        assert_eq!(max_backref("\\1 \\\\3x\\12"), 12);
    }
}
