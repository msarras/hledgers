//! The journal reader (Hledger.Read.JournalReader): journal files, with
//! their directives, transactions, periodic transaction rules, auto posting
//! rules, comments, and included files.
//!
//! The parser is hand-written over a byte [`Cursor`]. Its errors are
//! megaparsec's where hledger's tests or a direct comparison show them; an
//! error in an included file is shown after the files that include it.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::InputOpts;
use super::amount::{
    AmountCtx, AmountKind, amountp, balance_assertionp, commodity_symbolp, posting_amountp,
};
use super::common::{
    Interned, account_aliasp, accountnamep, codep, datep, double_quoted_textp,
    empty_or_comment_linep, following_commentp, is_line_comment_start, multiline_commentp,
    noncomment_textp, peek_after_spaces, posting_commentp, rest_of_line, secondary_datep, statusp,
    timeofdayp, transaction_commentp,
};
use crate::account::{
    AccountAlias, AccountType, Realness, account_name_apply_aliases, account_name_posting_type,
    account_name_without_posting_type, join_account_names,
};
use crate::amount::{Amount, AmountStyle, MixedAmount, Sym};
use crate::dates::{Day, periodexprp};
use crate::model::{
    AccountDeclarationInfo, Commodity, Journal, JournalItem, PayeeDeclarationInfo,
    PeriodicTransaction, Posting, PriceDirective, SourcePos, Tag, TagDeclarationInfo,
    TmPostingRule, Transaction, TransactionModifier,
};
use crate::parse::{Cursor, Item, PResult, ParseError, Parser};
use crate::text::is_space;

/// The state that directives change and later items see (the jparse*
/// fields of upstream's Journal). An included file starts with its
/// includer's, and its changes do not return to the includer.
#[derive(Clone, Debug)]
pub struct ParseState {
    /// The year of dates written without one (Y), initially this year.
    pub year: Option<i64>,
    /// The commodity of amounts written without one, and its style (D).
    pub default_commodity: Option<(Sym, Arc<AmountStyle>)>,
    /// The decimal mark numbers must use (decimal-mark).
    pub decimal_mark: Option<char>,
    /// The parent accounts from apply account, outermost first.
    pub parent_accounts: Vec<String>,
    /// The aliases from alias directives and account alias: tags, most
    /// recent first, applied to account names as written.
    pub aliases: Vec<AccountAlias>,
    /// The aliases from --alias options, applied to full account names,
    /// or the error of a bad one, which is reported only once an account
    /// name is aliased (as upstream's lazily parsed aliases are).
    pub opt_aliases: Result<Vec<AccountAlias>, String>,
}

impl Default for ParseState {
    fn default() -> ParseState {
        ParseState {
            year: None,
            default_commodity: None,
            decimal_mark: None,
            parent_accounts: Vec::new(),
            aliases: Vec::new(),
            opt_aliases: Ok(Vec::new()),
        }
    }
}

/// Parse a journal file's text, and the files it includes, into an
/// unfinalised journal, returning also the parse state at the end (the
/// parsing part of initialiseAndParseJournal). Errors are complete
/// messages.
pub fn parse_journal(
    iopts: &InputOpts,
    file: &str,
    text: &str,
) -> Result<(Journal, ParseState), String> {
    let mut st = ParseState {
        year: Some(iopts.today.year()),
        opt_aliases: aliases_from_opts(iopts),
        ..ParseState::default()
    };
    let mut j = Journal::default();
    let mut stack = vec![(file.to_string(), canonical_path(&absolute_path(file)))];
    if let Some(parsed) = parse_in_parts(iopts, &st, &stack, file, text) {
        return Ok(parsed);
    }
    match parse_file(iopts, &mut st, &mut stack, &mut j, file, text) {
        Ok(()) => Ok((j, st)),
        Err(Failure::Here(e)) => Err(e.pretty(file, text)),
        Err(Failure::Included(msg) | Failure::Unlocated(msg)) => Err(msg),
    }
}

/// The smallest text worth parsing in parts.
const PARTS_MIN_SIZE: usize = 1 << 18;

/// A large file parsed in parts, if it can be: its directives and the
/// items among them first, then the rest, transactions, price directives
/// and comment lines only, which no directive comes between to change how
/// they read, in parts starting at a transaction after an empty line, the
/// parts read with `par` (on all cores with the parallel feature) and
/// joined in order. None if the file is small, has a multi-line comment,
/// or a part has an error, which the whole file is then parsed again to
/// report as usual.
fn parse_in_parts(
    iopts: &InputOpts,
    st: &ParseState,
    stack: &[(String, PathBuf)],
    file: &str,
    text: &str,
) -> Option<(Journal, ParseState)> {
    const PARTS: usize = 64;
    if text.len() < PARTS_MIN_SIZE {
        return None;
    }
    // where a transaction follows an empty line, after the last directive,
    // with its line number
    let mut starts: Vec<(usize, usize)> = Vec::new();
    let (mut off, mut line, mut empty_before) = (0, 1, false);
    while off < text.len() {
        let end = text[off..].find('\n').map_or(text.len(), |i| off + i);
        match text.as_bytes()[off] {
            b'0'..=b'9' if empty_before => starts.push((off, line)),
            b'0'..=b'9' | b'P' | b' ' | b'\t' | b'\n' | b';' | b'#' | b'*' => {}
            _ if text[off..].starts_with("comment") => return None,
            _ => starts.clear(),
        }
        empty_before = end == off;
        (off, line) = (end + 1, line + 1);
    }
    let &(header_end, _) = starts.first()?;
    let step = (text.len() - header_end).div_ceil(PARTS);
    let mut parts: Vec<(usize, usize, usize)> = Vec::new();
    for &(start, line) in &starts {
        if parts.last().is_none_or(|&(s, _, _)| start >= s + step) {
            if let Some(last) = parts.last_mut() {
                last.1 = start;
            }
            parts.push((start, text.len(), line));
        }
    }
    let (mut st, mut stack) = (st.clone(), stack.to_vec());
    let mut j = Journal::default();
    parse_file(
        iopts,
        &mut st,
        &mut stack,
        &mut j,
        file,
        &text[..header_end],
    )
    .ok()?;
    let file: Arc<str> = Arc::from(file);
    let declared = &j.declared_commodities;
    let parsed = crate::par::map_owned(line, parts, |(start, end, line)| {
        let (mut st, mut stack) = (st.clone(), stack.clone());
        let mut part = Journal {
            declared_commodities: declared.clone(),
            ..Journal::default()
        };
        let mut p = JournalParser {
            c: Cursor {
                src: &text[..end],
                pos: start,
            },
            file: file.clone(),
            st: &mut st,
            stack: &mut stack,
            j: &mut part,
            iopts,
            anchor: (start, line, start),
            interned: RefCell::default(),
        };
        p.journalp().ok()?;
        Some((part.items, part.txns, part.price_directives))
    });
    for part in parsed {
        let (items, txns, prices) = part?;
        j.items.extend(items);
        j.txns.extend(txns);
        j.price_directives.extend(prices);
    }
    Some((j, st))
}

/// An auto posting rule's posting written alone, `ACCOUNT  AMOUNT` or
/// `ACCOUNT  *N`, parsed as upstream parses rewrite's --add-posting values:
/// after a space, with no journal state, to the end of the text
/// (tmpostingrulep). Errors are complete messages.
pub fn parse_tm_posting_rule(iopts: &InputOpts, s: &str) -> Result<TmPostingRule, String> {
    let text = format!(" {s}\n");
    let (mut st, mut stack, mut j) = (ParseState::default(), Vec::new(), Journal::default());
    let mut p = JournalParser {
        c: Cursor::new(&text),
        file: Arc::from(""),
        st: &mut st,
        stack: &mut stack,
        j: &mut j,
        iopts,
        anchor: (0, 1, 0),
        interned: RefCell::default(),
    };
    let rule = match p.postingp(true, None) {
        Ok(Some((posting, is_multiplier))) if p.c.at_end() => Ok(TmPostingRule {
            posting: posting_round_calculated_amounts(posting),
            is_multiplier,
        }),
        Ok(_) => Err(Failure::Here(p.c.unexpected(&[Item::EndOfInput]))),
        Err(e) => Err(e),
    };
    rule.map_err(|e| match e {
        Failure::Here(e) => e.pretty("", &text),
        Failure::Included(msg) | Failure::Unlocated(msg) => msg,
    })
}

/// The account aliases of the --alias options; a bad one is an error
/// (aliasesFromOpts).
pub fn aliases_from_opts(iopts: &InputOpts) -> Result<Vec<AccountAlias>, String> {
    iopts
        .aliases
        .iter()
        .map(|a| {
            let mut c = Cursor::new(a);
            account_aliasp(&mut c).map_err(|e| e.pretty("--alias", a))
        })
        .collect()
}

/// Why parsing stopped: an error in this file; a message about an error
/// in it or a file it includes, which each includer prefixes with its
/// name; or a message about something with no place in the files, shown
/// as it is.
pub(super) enum Failure {
    Here(ParseError),
    Included(String),
    Unlocated(String),
}

impl From<ParseError> for Failure {
    fn from(e: ParseError) -> Failure {
        Failure::Here(e)
    }
}

impl Failure {
    /// The complete message, an error in this file shown in its text.
    pub(super) fn message(self, file: &str, text: &str) -> String {
        match self {
            Failure::Here(e) => e.pretty(file, text),
            Failure::Included(msg) | Failure::Unlocated(msg) => msg,
        }
    }
}

pub(super) type R<T> = Result<T, Failure>;

/// Parse one file into the journal, as if it were written where it is
/// included. An error in it, or in a file it includes, becomes a complete
/// message, prefixed with its name if it was in an included file.
fn parse_file(
    iopts: &InputOpts,
    st: &mut ParseState,
    stack: &mut Vec<(String, PathBuf)>,
    j: &mut Journal,
    file: &str,
    text: &str,
) -> R<()> {
    let mut p = JournalParser {
        c: Cursor::new(text),
        file: Arc::from(file),
        st,
        stack,
        j,
        iopts,
        anchor: (0, 1, 0),
        interned: RefCell::default(),
    };
    match p.journalp() {
        Err(Failure::Here(e)) => Err(Failure::Included(e.pretty(file, text))),
        Err(Failure::Included(msg)) => Err(Failure::Included(format!(
            "in file included from {file},\n{msg}"
        ))),
        r => r,
    }
}

struct JournalParser<'a, 's> {
    c: Cursor<'a>,
    file: Arc<str>,
    st: &'s mut ParseState,
    /// The files being read, outermost first: as named, and canonical.
    stack: &'s mut Vec<(String, PathBuf)>,
    j: &'s mut Journal,
    iopts: &'s InputOpts,
    /// A known position: offset, line, and the offset of that line's start.
    anchor: (usize, usize, usize),
    /// The account names, commodity symbols and amount styles read so far.
    interned: RefCell<Interned>,
}

/// The amount parsers' view of the parse state.
fn amount_ctx<'s>(
    st: &'s ParseState,
    j: &'s Journal,
    interned: &'s RefCell<Interned>,
) -> AmountCtx<'s> {
    AmountCtx {
        decimal_mark: st.decimal_mark,
        default_commodity: st.default_commodity.as_ref(),
        commodities: Some(&j.declared_commodities),
        year: st.year,
        interned: Some(interned),
    }
}

impl<'a> JournalParser<'a, '_> {
    /// The source position of an offset: its line, and its column counted
    /// as megaparsec counts it, tabs advancing to the next multiple of 8
    /// (getSourcePos').
    fn pos_at(&mut self, offset: usize) -> SourcePos {
        if offset < self.anchor.0 {
            self.anchor = (0, 1, 0);
        }
        let (from, mut line, mut line_start) = self.anchor;
        let bytes = &self.c.src.as_bytes()[from..offset];
        for (i, b) in bytes.iter().enumerate() {
            if *b == b'\n' {
                line += 1;
                line_start = from + i + 1;
            }
        }
        self.anchor = (offset, line, line_start);
        let col = self.c.src[line_start..offset]
            .chars()
            .fold(1, |col, ch| match ch {
                '\t' => col + 8 - (col - 1) % 8,
                _ => col + 1,
            });
        SourcePos::new(&self.file, line, col)
    }

    fn pos(&mut self) -> SourcePos {
        self.pos_at(self.c.pos)
    }

    fn year(&self) -> Option<i64> {
        self.st.year
    }

    /// Items until the end of the input (journalp).
    fn journalp(&mut self) -> R<()> {
        while !self.c.at_end() {
            self.itemp()?;
        }
        Ok(())
    }

    /// Record an item with the text from `start` to here.
    fn record(&mut self, start: usize, item: fn(String) -> JournalItem) {
        let text = self.c.src[start..self.c.pos].to_string();
        self.add_item(item(text));
    }

    /// Add an item, collapsing runs of blank lines (addJournalItem).
    fn add_item(&mut self, item: JournalItem) {
        if item == JournalItem::Blank && self.j.items.last() == Some(&JournalItem::Blank) {
            return;
        }
        self.j.items.push(item);
    }

    /// Any journal item (addJournalItemP), dispatched on its first character.
    fn itemp(&mut self) -> R<()> {
        let first = self.c.peek().unwrap_or('\n');
        if first.is_ascii_digit() {
            return self.transaction_item();
        }
        if first == 'P' {
            return self.price_item();
        }
        if is_space(first) || is_line_comment_start(first) {
            return self.blank_or_comment_item();
        }
        let start = self.c.pos;
        if self.directivep()? {
            return Ok(());
        }
        match first {
            '=' => {
                let m = self.transaction_modifierp()?;
                self.record(start, JournalItem::Directive);
                self.j.txn_modifiers.push(m);
                Ok(())
            }
            '~' => {
                let pt = self.periodic_transactionp()?;
                self.record(start, JournalItem::Directive);
                self.j.periodic_txns.push(pt);
                Ok(())
            }
            _ if self.c.rest().starts_with("comment") => {
                multiline_commentp(&mut self.c)?;
                self.record(start, JournalItem::CommentBlock);
                Ok(())
            }
            _ => Err(self
                .c
                .unexpected(&[Item::label("transaction or directive"), Item::EndOfInput])
                .into()),
        }
    }

    fn transaction_item(&mut self) -> R<()> {
        let t = self.transactionp()?;
        // comment lines just before it become its preceding comment (addTransactionItem)
        let n = self
            .j
            .items
            .iter()
            .rev()
            .take_while(|i| matches!(i, JournalItem::Comment(_)))
            .count();
        let mut t = t;
        if n > 0 {
            let at = self.j.items.len() - n;
            for i in self.j.items.drain(at..) {
                if let JournalItem::Comment(c) = i {
                    t.preceding_comment.push_str(&c);
                }
            }
        }
        self.add_item(JournalItem::Transaction(t.pos.0.clone()));
        self.j.txns.push(t);
        Ok(())
    }

    fn price_item(&mut self) -> R<()> {
        let start = self.c.pos;
        let pd = self.market_price_directivep()?;
        self.record(start, JournalItem::Directive);
        self.j.price_directives.push(pd);
        Ok(())
    }

    fn blank_or_comment_item(&mut self) -> R<()> {
        let start = self.c.pos;
        empty_or_comment_linep(&mut self.c)?;
        let text = &self.c.src[start..self.c.pos];
        let item = if text.chars().all(is_space) {
            JournalItem::Blank
        } else {
            JournalItem::Comment(text.to_string())
        };
        self.add_item(item);
        Ok(())
    }

    /// A directive, if one starts here (directivep). Returns false if none
    /// does, having consumed nothing.
    fn directivep(&mut self) -> R<bool> {
        let before = self.c.pos;
        let prefixed = self.c.eat('!') || self.c.eat('@');
        let start = self.c.pos;
        let rest = self.c.rest();
        let starts = |k: &str| rest.starts_with(k);
        // `end` directives allow any spaces between their words
        let words = |ks: &[&str]| keywords_match(rest, ks);
        let exported: Option<fn(&mut Self) -> R<()>> = if starts("include") {
            self.include_directivep()?;
            return Ok(true);
        } else if starts("account") {
            Some(Self::account_directivep)
        } else if starts("commodity") {
            Some(Self::commodity_directivep)
        } else if starts("decimal-mark") {
            Some(Self::decimal_mark_directivep)
        } else if starts("Y") || starts("year") || starts("apply year") {
            Some(Self::default_year_directivep)
        } else if starts("D") {
            Some(Self::default_commodity_directivep)
        } else if starts("payee") {
            Some(Self::payee_directivep)
        } else if starts("tag") {
            Some(Self::tag_directivep)
        } else {
            None
        };
        if let Some(p) = exported {
            p(self)?;
            self.record(start, JournalItem::Directive);
            return Ok(true);
        }
        let ignored: Option<fn(&mut Self) -> R<()>> = if starts("alias") {
            Some(Self::alias_directivep)
        } else if words(&["end", "aliases"]) {
            Some(Self::end_aliases_directivep)
        } else if words(&["apply", "account"]) {
            Some(Self::apply_account_directivep)
        } else if words(&["end", "apply", "account"]) {
            Some(Self::end_apply_account_directivep)
        } else if [
            "apply fixed",
            "apply tag",
            "assert",
            "A ",
            "bucket ",
            "capture",
            "check",
            "--",
        ]
        .iter()
        .any(|k| starts(k))
        {
            Some(Self::ignored_line)
        } else if starts("C") {
            Some(Self::commodity_conversion_directivep)
        } else if [
            "define",
            "end apply fixed",
            "end apply tag",
            "end apply year",
        ]
        .iter()
        .any(|k| starts(k))
        {
            Some(Self::ignored_line)
        } else if starts("end") {
            Some(Self::end_tag_directivep)
        } else if ["eval", "expr"].iter().any(|k| starts(k)) {
            Some(Self::ignored_line)
        } else if starts("N") {
            Some(Self::ignored_price_commodity_directivep)
        } else if starts("python") {
            Some(Self::python_directivep)
        } else if starts("value") {
            Some(Self::ignored_line)
        } else {
            None
        };
        if let Some(p) = ignored {
            p(self)?;
            self.record(start, JournalItem::NonExportedDirective);
            return Ok(true);
        }
        if prefixed {
            return Err(self.no_directive_error().into());
        }
        self.c.pos = before;
        Ok(false)
    }

    /// The error when no directive follows a `!` or `@`: every directive's
    /// first word or description expected, and as much input unexpected as
    /// the longest of those words.
    fn no_directive_error(&self) -> ParseError {
        const WORDS: [&str; 20] = [
            "--",
            "A ",
            "account",
            "alias",
            "apply fixed",
            "apply tag",
            "assert",
            "bucket ",
            "capture",
            "check",
            "commodity",
            "define",
            "end apply fixed",
            "end apply tag",
            "end apply year",
            "eval",
            "expr",
            "include",
            "python",
            "value",
        ];
        const LABELS: [&str; 11] = [
            "apply account directive",
            "commodity conversion",
            "decimal mark",
            "default commodity",
            "default year",
            "end aliases directive",
            "end apply account directive",
            "end tag or end apply tag directive",
            "ignored-price commodity",
            "payee directive",
            "tag directive",
        ];
        let expected: Vec<Item> = WORDS
            .iter()
            .map(|w| Item::tokens(w))
            .chain(LABELS.iter().map(|l| Item::label(l)))
            .collect();
        let found: String = self.c.rest().chars().take(15).collect();
        let found = if found.is_empty() {
            Item::EndOfInput
        } else {
            Item::Tokens(found)
        };
        ParseError::trivial(self.c.pos, Some(found), &expected)
    }

    /// A directive line whose content is ignored (the Ledger directives
    /// hledger accepts and ignores).
    fn ignored_line(&mut self) -> R<()> {
        rest_of_line(&mut self.c);
        Ok(())
    }

    /// One or more spaces, or an error listing these hints.
    fn spaces1(&mut self, hints: &[Item]) -> PResult<()> {
        if self.c.skip_spaces() {
            Ok(())
        } else {
            Err(self.c.unexpected(hints))
        }
    }

    /// `include PATH-OR-GLOB`, and the files it matches, parsed as if they
    /// were written here (includedirectivep).
    fn include_directivep(&mut self) -> R<()> {
        let off = self.c.pos;
        let pos = self.pos();
        let no_arg = |c: &Cursor| {
            ParseError::fail_at(
                c.src,
                off,
                "include needs a file path or glob pattern argument",
            )
        };
        self.c.eat_str("include");
        if !self.c.skip_spaces() {
            return Err(no_arg(&self.c).into());
        }
        let arg = self.c.take_while(|ch| ch != ';' && ch != '\n');
        following_commentp(&mut self.c, &[])?;
        self.record(off, JournalItem::Include);
        let prefixed_glob = arg.trim_end_matches(is_space);
        let (prefix, path) = split_reader_prefix(prefixed_glob);
        if path.is_empty() {
            return Err(no_arg(&self.c).into());
        }
        let parent = absolute_path(&pos.file);
        let files = self.find_matched_files(off, &parent, path)?;
        for f in files {
            self.parse_included_file(off, prefix, &f)?;
        }
        Ok(())
    }

    /// The files matched by an include directive's path or glob pattern,
    /// relative to the including file's directory (findMatchedFiles): with
    /// a leading `~` expanded, `**` without a slash read as `**/*`, only
    /// files (not directories), sorted; the including file itself left
    /// out, and a file that includes itself through others an error.
    fn find_matched_files(&mut self, off: usize, parent: &str, path: &str) -> R<Vec<String>> {
        let err = |c: &Cursor, msg: String| Failure::Here(ParseError::fail_at(c.src, off, msg));
        let expanded = expand_home_path(path)
            .map_err(|e| err(&self.c, format!("failed to expand ~:\nuser error ({e})")))?;
        let dir = Path::new(&canonical_path(parent))
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        if expanded.contains("***") {
            return Err(err(
                &self.c,
                "Invalid glob pattern: too many stars, use * or **".to_string(),
            ));
        }
        let pattern = expand_double_stars(&expanded);
        let glob = Glob::compile(&pattern)
            .map_err(|e| err(&self.c, format!("Invalid glob pattern: {e}")))?;
        let mut files: Vec<String> = if glob.is_literal() {
            vec![if pattern.starts_with('/') {
                pattern.clone()
            } else {
                format!("{}/{}", dir.display(), pattern)
            }]
        } else {
            glob.matches(&dir)
        };
        files.retain(|f| super::file_exists(f));
        let mut files: Vec<String> = files.iter().map(|f| normalise_path(f)).collect();
        files.sort();
        let current = self
            .stack
            .last()
            .map(|(_, c)| c.clone())
            .unwrap_or_default();
        let (mut found, mut matched_current) = (Vec::new(), false);
        for f in files {
            let canonical = canonical_path(&f);
            if canonical == current {
                matched_current = true;
                continue;
            }
            if self.stack[..self.stack.len() - 1]
                .iter()
                .any(|(_, c)| *c == canonical)
            {
                return Err(err(
                    &self.c,
                    format!("This included file forms a cycle: {f}"),
                ));
            }
            found.push(f);
        }
        if found.is_empty() && !matched_current {
            return Err(err(&self.c, format!("No files were matched by: {path}")));
        }
        Ok(found)
    }

    /// Parse an included file into the journal, as if it were written at
    /// the include directive; its parse state changes do not last beyond it.
    fn parse_included_file(&mut self, off: usize, prefix: Option<&str>, file: &str) -> R<()> {
        let format = prefix.or_else(|| {
            let ext = Path::new(file).extension()?.to_str()?;
            Some(match ext.to_ascii_lowercase().as_str() {
                "csv" => "csv",
                "tsv" => "tsv",
                "ssv" => "ssv",
                "timeclock" => "timeclock",
                "timedot" => "timedot",
                "rules" => "rules",
                _ => "journal",
            })
        });
        let sep = match format {
            Some("csv") => Some(','),
            Some("ssv") => Some(';'),
            Some("tsv") => Some('\t'),
            _ => None,
        };
        if sep.is_some() || format == Some("rules") {
            return self.include_converted_file(file, sep);
        }
        let text = super::read_file_portably(file).map_err(|e| {
            let msg = format!("failed to read a file:\n{e}");
            Failure::Here(ParseError::fail_at(self.c.src, off, msg))
        })?;
        self.j.files.push((file.to_string(), text.clone()));
        if let Some(f @ ("timeclock" | "timedot")) = format {
            // its transactions, made with the parse state in effect here
            let txns = if f == "timeclock" {
                super::timeclock::timeclock_transactions(self.iopts, self.st, file, &text)
            } else {
                super::timedot::timedot_transactions(self.st, file, &text)
            };
            let txns = txns.map_err(|e| match e {
                Failure::Here(e) => Failure::Included(e.pretty(file, &text)),
                e => e,
            })?;
            self.j.txns.extend(txns);
            return Ok(());
        }
        let mut st = self.st.clone();
        self.stack.push((file.to_string(), canonical_path(file)));
        let r = parse_file(self.iopts, &mut st, self.stack, self.j, file, &text);
        self.stack.pop();
        r
    }

    /// An included CSV, SSV or TSV file (`sep` its separator), converted by
    /// its rules file, or a rules file, converted from its data: its
    /// transactions added here, unfinalised, with the account aliases in
    /// effect applied (alias directives, then --alias options), but not
    /// other directives' effects (includeFileParser). Errors in it are shown
    /// after the include file stack.
    fn include_converted_file(&mut self, file: &str, sep: Option<char>) -> R<()> {
        let read = match sep {
            Some(sep) => super::csv::read_included_csv(self.iopts, file, sep),
            None => super::csv::read_rules_data_with(self.iopts, file, |j, _| Ok(j)),
        };
        let cj = read.map_err(Failure::Included)?;
        let opt_aliases = self
            .st
            .opt_aliases
            .as_ref()
            .map_err(|e| Failure::Unlocated(e.clone()))?;
        let aliases: Vec<AccountAlias> =
            self.st.aliases.iter().chain(opt_aliases).cloned().collect();
        let cj = super::csv::journal_apply_aliases(&aliases, cj).map_err(Failure::Included)?;
        self.j.files.push((file.to_string(), String::new()));
        self.j.txns.extend(cj.txns);
        Ok(())
    }

    /// `account NAME`, with a comment, tags (including `type:` and
    /// `alias:`), and indented subdirectives, of which `alias NAME` is
    /// kept as an alias: tag (accountdirectivep).
    fn account_directivep(&mut self) -> R<()> {
        let off = self.c.pos;
        let pos = self.pos();
        self.c.eat_str("account");
        self.spaces1(&[])?;
        if matches!(self.c.peek(), Some('(' | '[')) {
            return Err(self
                .c
                .unexpected(&[Item::label("account name without brackets")])
                .into());
        }
        let aliased = self.aliased_account_namep(true)?;
        let acct = self.apply_parent_and_option_aliases(&aliased)?;
        let comment_off = self.c.pos;
        let (comment, mut tags) = transaction_commentp(&mut self.c, &[])?;
        // subdirectives: alias NAME is kept as an alias: tag, others are ignored
        loop {
            let before = self.c.pos;
            if !self.c.skip_spaces() || self.c.at_end() {
                self.c.pos = before;
                break;
            }
            if self.c.eat_str("alias") && self.c.skip_spaces() {
                let a = rest_of_line(&mut self.c).trim_end_matches(is_space);
                tags.push(("alias".to_string(), a.to_string()));
            } else {
                rest_of_line(&mut self.c);
            }
        }
        let account_type = tags
            .iter()
            .find(|(n, _)| n == "type")
            .map(|(_, v)| parse_account_type_code(v));
        let aliases: Vec<&String> = tags
            .iter()
            .filter(|(n, _)| n.to_lowercase() == "alias")
            .map(|(_, v)| v)
            .collect();
        if aliases.iter().any(|a| a.is_empty()) {
            return Err(Failure::Here(ParseError::fail_at(
                self.c.src,
                comment_off,
                "an alias: tag on an account directive should have an account name as its value, eg alias: checking",
            )));
        }
        for a in aliases {
            let alias = AccountAlias::Basic(a.as_str().into(), aliased.as_str().into());
            self.st.aliases.insert(0, alias);
        }
        let acct: Arc<str> = acct.into();
        let order = self.j.declared_accounts.len() + 1;
        self.j.declared_accounts.push((
            acct.clone(),
            AccountDeclarationInfo {
                comment,
                tags: tags.clone(),
                order,
                pos,
            },
        ));
        if !tags.is_empty() {
            let existing = self
                .j
                .declared_account_tags
                .entry(acct.clone())
                .or_default();
            for t in tags {
                if !existing.contains(&t) {
                    existing.push(t);
                }
            }
        }
        match account_type {
            None => Ok(()),
            Some(Ok(t)) => {
                self.j
                    .declared_account_types
                    .entry(t)
                    .or_default()
                    .push(acct);
                Ok(())
            }
            Some(Err(e)) => Err(Failure::Here(ParseError::fail_at(self.c.src, off, e))),
        }
    }

    /// An account name with the alias directives applied
    /// (aliasedaccountnamep).
    fn aliased_account_namep(&mut self, allow_semicolon: bool) -> PResult<String> {
        let a = accountnamep(&mut self.c, allow_semicolon)?;
        Ok(alias_or_die(&self.st.aliases, &a))
    }

    /// An account name with the parent account and --alias options applied
    /// (applyParentAndOptionAliases).
    fn apply_parent_and_option_aliases(&mut self, a: &str) -> R<String> {
        apply_parent_and_option_aliases(self.st, a)
    }

    /// `commodity AMOUNT` declaring a commodity and its display style, or
    /// `commodity SYMBOL` and indented `format AMOUNT` subdirectives
    /// (commoditydirectivep). A style's number must have a decimal mark.
    fn commodity_directivep(&mut self) -> R<()> {
        let save = self.c;
        self.c.eat_str("commodity");
        let pos = self.pos();
        if self.c.skip_spaces() {
            let off = self.c.pos;
            let ctx = amount_ctx(self.st, self.j, &self.interned);
            if let Ok(a) = amountp(&mut self.c, &ctx, AmountKind::Style) {
                self.c.skip_spaces();
                let comment_off = self.c.pos;
                let (comment, tags) = transaction_commentp(&mut self.c, &[])?;
                let aliases = self.commodity_aliasesp(comment_off, &a.commodity, &tags)?;
                if a.style.decimal_mark.is_none() {
                    return Err(please_include_decimal_point(&self.c, off));
                }
                self.add_commodity(Commodity {
                    symbol: a.commodity.clone(),
                    format: Some((*a.style).clone()),
                    comment,
                    tags,
                    aliases: aliases.into_iter().map(|a| (a, pos.clone())).collect(),
                    pos,
                });
                return Ok(());
            }
        }
        self.c = save;
        self.c.eat_str("commodity");
        self.spaces1(&[])?;
        let symbol = Sym::from(commodity_symbolp(&mut self.c)?);
        let comment_off = self.c.pos;
        let (comment, tags) = transaction_commentp(&mut self.c, &[])?;
        let aliases = self.commodity_aliasesp(comment_off, &symbol, &tags)?;
        let mut format = None;
        loop {
            let before = self.c.pos;
            if !self.c.skip_spaces() || self.c.at_end() {
                self.c.pos = before;
                break;
            }
            if self.c.rest().starts_with("format") {
                format = Some(self.format_directivep(&symbol)?);
            } else {
                rest_of_line(&mut self.c);
            }
        }
        self.add_commodity(Commodity {
            symbol,
            format,
            comment,
            tags,
            aliases: aliases.into_iter().map(|a| (a, pos.clone())).collect(),
            pos,
        });
        Ok(())
    }

    /// `format AMOUNT`, whose symbol must be the commodity's
    /// (formatdirectivep).
    fn format_directivep(&mut self, symbol: &Sym) -> R<AmountStyle> {
        self.c.eat_str("format");
        self.spaces1(&[])?;
        let off = self.c.pos;
        let ctx = amount_ctx(self.st, self.j, &self.interned);
        let a = amountp(&mut self.c, &ctx, AmountKind::Style).map_err(|e| {
            if self.c.pos == off {
                self.c.unexpected(&[Item::label("amount")])
            } else {
                e
            }
        })?;
        following_commentp(&mut self.c, &[])?;
        if a.commodity != *symbol {
            let msg = format!(
                "commodity directive symbol \"{symbol}\" and format directive symbol \"{}\" should be the same",
                a.commodity
            );
            return Err(Failure::Here(ParseError::fail_at(self.c.src, off, msg)));
        }
        if a.style.decimal_mark.is_none() {
            return Err(please_include_decimal_point(&self.c, off));
        }
        Ok((*a.style).clone())
    }

    /// Add a commodity declaration; a commodity declared again adds to its
    /// earlier declaration (addCommodityDeclaration, commodityRedeclare).
    fn add_commodity(&mut self, new: Commodity) {
        // the later declaration's tags come first (union), so they win when a
        // tag is looked up by its first value
        if !new.tags.is_empty() {
            let existing = self
                .j
                .declared_commodity_tags
                .entry(new.symbol.clone())
                .or_default();
            let earlier = std::mem::replace(existing, new.tags.clone());
            for t in earlier {
                if !existing.contains(&t) {
                    existing.push(t);
                }
            }
        }
        let sym = new.symbol.clone();
        let combined = match self.j.declared_commodities.remove(&sym) {
            None => new,
            Some(old) => {
                let mut tags = new.tags.clone();
                for t in &old.tags {
                    if !tags.contains(t) {
                        tags.push(t.clone());
                    }
                }
                let mut aliases = old.aliases.clone();
                for (a, p) in &new.aliases {
                    if !aliases
                        .iter()
                        .any(|(b, _)| b.commodity == a.commodity && b.quantity == a.quantity)
                    {
                        aliases.push((a.clone(), p.clone()));
                    }
                }
                Commodity {
                    format: new.format.or(old.format),
                    tags,
                    aliases,
                    ..new
                }
            }
        };
        self.j.declared_commodities.insert(sym, combined);
    }

    /// The values of a commodity directive's alias: tags: each a commodity
    /// symbol, optionally with a positive quantity of it equal to one unit
    /// of this commodity (commodityaliasesp). Errors are reported at `off`.
    fn commodity_aliasesp(&mut self, off: usize, sym: &Sym, tags: &[Tag]) -> R<Vec<Amount>> {
        let err = |c: &Cursor, msg: String| Failure::Here(ParseError::fail_at(c.src, off, msg));
        let mut aliases: Vec<Amount> = Vec::new();
        for (n, v) in tags {
            if n.to_lowercase() != "alias" {
                continue;
            }
            let a = parse_commodity_alias(v).ok_or_else(|| {
                err(
                    &self.c,
                    format!(
                        "could not parse commodity alias \"{v}\".\n\
                         Each alias: tag should contain one commodity symbol, optionally with a positive quantity,\n\
                         the number of alias units equal to one {sym}. Eg: alias: $, alias: 100 cents\n"
                    ),
                )
            })?;
            let one = crate::decimal::Decimal::from_i64(1);
            if (a.commodity == *sym && a.quantity == one)
                || aliases
                    .iter()
                    .any(|b| b.commodity == a.commodity && b.quantity == a.quantity)
            {
                continue;
            }
            aliases.push(a);
        }
        if aliases.iter().any(|a| a.commodity == *sym) {
            return Err(err(
                &self.c,
                format!("\"{sym}\" can't be an alias of itself with a quantity other than 1"),
            ));
        }
        for (i, a) in aliases.iter().enumerate() {
            if aliases[..i].iter().any(|b| b.commodity == a.commodity) {
                return Err(err(
                    &self.c,
                    format!(
                        "alias \"{}\" is declared more than once, with different quantities",
                        a.commodity
                    ),
                ));
            }
        }
        if let Some(c) = self.j.declared_commodities.get(sym) {
            for a in &aliases {
                if let Some((e, epos)) = c
                    .aliases
                    .iter()
                    .find(|(e, _)| e.commodity == a.commodity && e.quantity != a.quantity)
                {
                    return Err(err(
                        &self.c,
                        format!(
                            "alias \"{}\" was declared earlier for \"{sym}\" with a different quantity ({}), at {}:{}",
                            a.commodity, e.quantity, epos.file, epos.line
                        ),
                    ));
                }
            }
        }
        Ok(aliases)
    }

    /// `decimal-mark .` or `decimal-mark ,` (decimalmarkdirectivep).
    fn decimal_mark_directivep(&mut self) -> R<()> {
        self.c.eat_str("decimal-mark");
        self.spaces1(&[])?;
        match self.c.peek() {
            Some(m @ ('.' | ',')) => {
                self.c.bump();
                self.st.decimal_mark = Some(m);
            }
            _ => return Err(self.c.unexpected(&[]).into()),
        }
        rest_of_line(&mut self.c);
        Ok(())
    }

    /// `Y YEAR`, `year YEAR` or `apply year YEAR` (defaultyeardirectivep).
    fn default_year_directivep(&mut self) -> R<()> {
        for k in ["Y", "year", "apply year"] {
            if self.c.eat_str(k) {
                break;
            }
        }
        self.c.skip_spaces();
        let mut p = Parser::new(self.c.src);
        p.pos = self.c.pos;
        let y = crate::dates::yearp(&mut p)?;
        self.c.pos = p.pos;
        rest_of_line(&mut self.c);
        self.st.year = Some(y);
        Ok(())
    }

    /// `D AMOUNT`, setting the default commodity and its style
    /// (defaultcommoditydirectivep).
    fn default_commodity_directivep(&mut self) -> R<()> {
        self.c.bump();
        self.spaces1(&[])?;
        let off = self.c.pos;
        let ctx = amount_ctx(self.st, self.j, &self.interned);
        let a = amountp(&mut self.c, &ctx, AmountKind::Style)?;
        rest_of_line(&mut self.c);
        if a.style.decimal_mark.is_none() {
            return Err(please_include_decimal_point(&self.c, off));
        }
        self.st.default_commodity = Some((a.commodity, a.style));
        Ok(())
    }

    /// `payee NAME` (or `payee "NAME"`), with a comment and tags
    /// (payeedirectivep).
    fn payee_directivep(&mut self) -> R<()> {
        self.c.eat_str("payee");
        self.spaces1(&[])?;
        let save = self.c;
        let payee = match double_quoted_textp(&mut self.c) {
            Ok(p) if self.c.peek() != Some('"') => p,
            _ => {
                self.c = save;
                let p = noncomment_textp(&mut self.c);
                if p.is_empty() {
                    return Err(self.c.unexpected(&[]).into());
                }
                p
            }
        };
        let payee = payee.trim_matches(is_space).to_string();
        let (comment, tags) = transaction_commentp(&mut self.c, &[])?;
        self.skip_indented_lines();
        self.j
            .declared_payees
            .push((payee, PayeeDeclarationInfo { comment, tags }));
        Ok(())
    }

    /// `tag NAME`, with a comment (tagdirectivep).
    fn tag_directivep(&mut self) -> R<()> {
        self.c.eat_str("tag");
        self.spaces1(&[])?;
        let name = self.c.take_while(|ch| !is_space(ch));
        if name.is_empty() {
            return Err(self.c.unexpected(&[]).into());
        }
        let (comment, _) = transaction_commentp(&mut self.c, &[])?;
        self.skip_indented_lines();
        self.j
            .declared_tags
            .push((name.to_string(), TagDeclarationInfo { comment }));
        Ok(())
    }

    /// Any indented lines (skipMany indentedlinep).
    fn skip_indented_lines(&mut self) {
        loop {
            let before = self.c.pos;
            if !self.c.skip_spaces() {
                break;
            }
            if self.c.at_end() {
                self.c.pos = before;
                break;
            }
            rest_of_line(&mut self.c);
        }
    }

    /// `alias OLD = NEW` or `alias /REGEX/ = REPLACEMENT` (aliasdirectivep).
    fn alias_directivep(&mut self) -> R<()> {
        self.c.eat_str("alias");
        self.spaces1(&[])?;
        let alias = account_aliasp(&mut self.c)?;
        self.st.aliases.insert(0, alias);
        Ok(())
    }

    /// `end aliases`, forgetting the alias directives' aliases
    /// (endaliasesdirectivep).
    fn end_aliases_directivep(&mut self) -> R<()> {
        rest_of_line(&mut self.c);
        self.st.aliases.clear();
        Ok(())
    }

    /// `apply account NAME` (applyaccountdirectivep).
    fn apply_account_directivep(&mut self) -> R<()> {
        self.c.eat_str("apply");
        self.c.skip_spaces();
        self.c.eat_str("account");
        self.spaces1(&[])?;
        let parent = accountnamep(&mut self.c, true)?;
        if !self.c.eat('\n') {
            return Err(self.c.unexpected(&[Item::label("newline")]).into());
        }
        self.st.parent_accounts.push(parent);
        Ok(())
    }

    /// `end apply account` (endapplyaccountdirectivep).
    fn end_apply_account_directivep(&mut self) -> R<()> {
        rest_of_line(&mut self.c);
        if self.st.parent_accounts.pop().is_none() {
            return Err(Failure::Here(ParseError::trivial(
                self.c.pos,
                Some(Item::tokens("End of apply account block with no beginning")),
                &[],
            )));
        }
        Ok(())
    }

    /// `end tag` or `end apply tag` (endtagdirectivep).
    fn end_tag_directivep(&mut self) -> R<()> {
        self.c.eat_str("end");
        self.spaces1(&[])?;
        let save = self.c;
        if !(self.c.eat_str("apply") && self.c.skip_spaces()) {
            self.c = save;
        }
        if !self.c.eat_str("tag") {
            return Err(self.c.unexpected(&[Item::tokens("tag")]).into());
        }
        self.c.skip_spaces();
        if !self.c.eat('\n') {
            return Err(self.c.unexpected(&[Item::label("end of line")]).into());
        }
        Ok(())
    }

    /// `C AMOUNT = AMOUNT`, which is ignored (commodityconversiondirectivep).
    fn commodity_conversion_directivep(&mut self) -> R<()> {
        self.c.bump();
        self.spaces1(&[])?;
        let ctx = amount_ctx(self.st, self.j, &self.interned);
        amountp(&mut self.c, &ctx, AmountKind::Ordinary)?;
        self.c.skip_spaces();
        if !self.c.eat('=') {
            return Err(self.c.unexpected(&[Item::tokens("=")]).into());
        }
        self.c.skip_spaces();
        amountp(&mut self.c, &ctx, AmountKind::Ordinary)?;
        rest_of_line(&mut self.c);
        Ok(())
    }

    /// `N SYMBOL`, which is ignored (ignoredpricecommoditydirectivep).
    fn ignored_price_commodity_directivep(&mut self) -> R<()> {
        self.c.bump();
        self.spaces1(&[])?;
        commodity_symbolp(&mut self.c)?;
        rest_of_line(&mut self.c);
        Ok(())
    }

    /// `python` and the indented or blank lines after it, which are
    /// ignored (pythondirectivep).
    fn python_directivep(&mut self) -> R<()> {
        rest_of_line(&mut self.c);
        loop {
            let before = self.c.pos;
            let spaced = self.c.skip_spaces();
            if self.c.eat('\n') || (spaced && !self.c.at_end()) {
                if spaced {
                    rest_of_line(&mut self.c);
                }
                continue;
            }
            self.c.pos = before;
            return Ok(());
        }
    }

    /// `P DATE [TIME] SYMBOL AMOUNT`, a market price (marketpricedirectivep).
    fn market_price_directivep(&mut self) -> R<PriceDirective> {
        let pos = self.pos();
        self.c.bump();
        self.c.skip_spaces();
        let year = self.year();
        let date = datep(&mut self.c, year)?;
        self.spaces1(&[Item::label("digit")])?;
        if self.c.peek().is_some_and(|ch| ch.is_ascii_digit()) {
            let save = self.c;
            if timeofdayp(&mut self.c).is_ok() {
                self.spaces1(&[])?;
            } else {
                self.c = save;
            }
        }
        let symbol = (self.interned.borrow_mut()).name(commodity_symbolp(&mut self.c)?);
        self.spaces1(&[])?;
        let ctx = amount_ctx(self.st, self.j, &self.interned);
        let amount = amountp(&mut self.c, &ctx, AmountKind::Ordinary)?;
        rest_of_line(&mut self.c);
        Ok(PriceDirective {
            pos,
            date,
            commodity: symbol,
            amount,
        })
    }

    /// `= QUERY` and posting rules, an auto posting rule
    /// (transactionmodifierp).
    fn transaction_modifierp(&mut self) -> R<TransactionModifier> {
        let pos = self.pos();
        self.c.bump();
        self.c.skip_spaces();
        let query = noncomment_textp(&mut self.c)
            .trim_matches(is_space)
            .to_string();
        transaction_commentp(&mut self.c, &[])?;
        let mut rules = Vec::new();
        while let Some((posting, is_multiplier)) = self.postingp(true, None)? {
            rules.push(TmPostingRule {
                posting: posting_round_calculated_amounts(posting),
                is_multiplier,
            });
        }
        Ok(TransactionModifier { query, rules, pos })
    }

    /// `~ PERIODEXPR` and a transaction's other parts, a periodic
    /// transaction rule (periodictransactionp). Partial and relative dates
    /// in the period expression are relative to January 1 of the default
    /// year.
    fn periodic_transactionp(&mut self) -> R<PeriodicTransaction> {
        let start_pos = self.pos();
        self.c.bump();
        self.c.skip_spaces();
        let refdate = match self.year() {
            Some(y) => Day::from_gregorian(y, 1, 1),
            None => self.iopts.today,
        };
        let expr_start = self.c.pos;
        let expr = accountnamep(&mut self.c, false).map_err(|_| self.c.unexpected(&[]))?;
        let expr_end = expr_start + c_len(&self.c.src[expr_start..], &expr);
        let excerpt = &self.c.src[expr_start..expr_end];
        let mut p = Parser::new(excerpt);
        let (interval, span) = periodexprp(&mut p, refdate)
            .and_then(|r| {
                if p.pos < excerpt.len() {
                    Err(ParseError::at_region(
                        excerpt,
                        p.pos,
                        excerpt.len(),
                        "remainder of period expression cannot be parsed\n\
                         perhaps you need to terminate the period expression with a double space?\n\
                         a double space is required between period expression and description/comment",
                    ))
                } else {
                    Ok(r)
                }
            })
            .map_err(|e| e.shifted(expr_start))?;
        let status = statusp(&mut self.c);
        let code = codep(&mut self.c)?.to_string();
        let description = noncomment_textp(&mut self.c)
            .trim_matches(is_space)
            .to_string();
        let (comment, tags) = transaction_commentp(&mut self.c, &[])?;
        let mut postings = Vec::new();
        while let Some((p, _)) = self.postingp(false, Some(refdate.year()))? {
            postings.push(posting_round_calculated_amounts(p));
        }
        let end_pos = self.pos();
        Ok(PeriodicTransaction {
            period_expr: excerpt.trim_matches(is_space).to_string(),
            interval,
            span,
            pos: (start_pos, end_pos),
            status,
            code,
            description,
            comment,
            tags,
            postings,
        })
    }

    /// A transaction, possibly unbalanced (transactionp).
    fn transactionp(&mut self) -> R<Transaction> {
        let start_pos = self.pos();
        let year = self.year();
        let date = datep(&mut self.c, year)?;
        let date2 = if self.c.peek() == Some('=') {
            Some(secondary_datep(&mut self.c, date)?)
        } else {
            None
        };
        if !matches!(self.c.peek(), Some(ch) if ch == '\n' || crate::parse::is_non_newline_space(ch))
        {
            return Err(self
                .c
                .unexpected(&[Item::label("digit"), Item::label("whitespace or newline")])
                .into());
        }
        let status = statusp(&mut self.c);
        let code = codep(&mut self.c)?.to_string();
        let description = noncomment_textp(&mut self.c)
            .trim_matches(is_space)
            .to_string();
        let (comment, tags) = transaction_commentp(&mut self.c, &[])?;
        let year = date.year();
        let mut postings = Vec::new();
        while let Some((p, _)) = self.postingp(false, Some(year))? {
            postings.push(p);
        }
        let end_pos = self.pos();
        Ok(super::balancing::transaction_round_calculated_amounts(
            Transaction {
                index: 0,
                preceding_comment: String::new(),
                pos: (start_pos, end_pos),
                date,
                date2,
                status,
                code,
                description,
                comment,
                tags,
                postings,
            },
        ))
    }

    /// A posting, or an auto posting rule's posting (which may have a `*`
    /// before its amount, making it a multiplier), if an indented line with
    /// one follows (postingphelper). Returns it and whether its amount is a
    /// multiplier.
    fn postingp(&mut self, is_rule: bool, year: Option<i64>) -> R<Option<(Posting, bool)>> {
        // a posting is an indented line that is not blank (postingsp)
        let start = self.c;
        if !is_rule && !matches!(peek_after_spaces(&self.c), (true, Some(ch)) if ch != '\n') {
            return Ok(None);
        }
        if !self.c.skip_spaces() {
            return Ok(None);
        }
        let status = statusp(&mut self.c);
        self.c.skip_spaces();
        let Ok(written) = accountnamep(&mut self.c, true) else {
            self.c = start;
            return Ok(None);
        };
        // with no aliases or parent account, a real posting's account is
        // the name as written
        let unchanged = self.st.aliases.is_empty()
            && self.st.parent_accounts.is_empty()
            && self.st.opt_aliases.as_ref().is_ok_and(Vec::is_empty)
            && account_name_posting_type(&written) == Realness::Real;
        let account = if unchanged {
            written
        } else {
            let aliased = alias_or_die(&self.st.aliases, &written);
            self.apply_parent_and_option_aliases(&aliased)?
        };
        let realness = account_name_posting_type(&account);
        let account =
            (self.interned.borrow_mut()).name(account_name_without_posting_type(&account));
        self.c.skip_spaces();
        let multiplier = is_rule && self.c.eat('*');
        let amount_start = self.c.pos;
        let ctx = amount_ctx(self.st, self.j, &self.interned);
        let amount = if multiplier {
            amountp(&mut self.c, &ctx, AmountKind::Multiplier)
        } else {
            posting_amountp(&mut self.c, &ctx, AmountKind::Ordinary)
        };
        let amount = match amount {
            Ok(a) => Some(a),
            Err(_) if self.c.pos == amount_start => None,
            Err(e) => return Err(e.into()),
        };
        // what an error at the comment could also have been: a missing amount
        let missing_amount = amount.is_none() && self.c.pos == amount_start;
        self.c.skip_spaces();
        let assertion = if self.c.peek() == Some('=') {
            let pos = self.pos();
            let ctx = amount_ctx(self.st, self.j, &self.interned);
            Some(Box::new(balance_assertionp(&mut self.c, &ctx, pos)?))
        } else {
            None
        };
        let mut hints = Vec::new();
        if missing_amount && assertion.is_none() && self.c.pos == amount_start {
            hints.push(Item::label("amount"));
        }
        let spaced = self.c.skip_spaces();
        hints.extend(super::common::digit_hint(&self.c));
        // an assertion's amount may be followed by a cost (amountnobasisp)
        if !spaced && assertion.as_ref().is_some_and(|a| a.amount.cost.is_none()) {
            hints.push(Item::label("transaction price"));
        }
        let (comment, tags, date, date2) = posting_commentp(&mut self.c, year, &hints)?;
        let posting = Posting {
            date,
            date2,
            status,
            account,
            amount: amount.map_or_else(MixedAmount::missing, MixedAmount::from_amount),
            comment,
            realness,
            tags,
            assertion,
            ..Posting::default()
        };
        Ok(Some((posting, multiplier)))
    }
}

/// An account name, which may contain semicolons only if `allow_semicolon`,
/// with the alias directives, parent account and --alias options in effect
/// applied (modifiedaccountnamep). Fails without consuming input if there is
/// none.
pub(super) fn modified_account_namep(
    c: &mut Cursor,
    st: &ParseState,
    allow_semicolon: bool,
) -> R<String> {
    let a = accountnamep(c, allow_semicolon)?;
    apply_parent_and_option_aliases(st, &alias_or_die(&st.aliases, &a))
}

/// An account name with the parent account and --alias options applied
/// (applyParentAndOptionAliases).
fn apply_parent_and_option_aliases(st: &ParseState, a: &str) -> R<String> {
    let parent = st.parent_accounts.join(":");
    let aliases = st
        .opt_aliases
        .as_ref()
        .map_err(|e| Failure::Unlocated(e.clone()))?;
    Ok(alias_or_die(aliases, &join_account_names(&parent, a)))
}

/// The aliases applied to an account name; a regular expression alias whose
/// replacement fails is a fatal error, as upstream's (orAliasError).
fn alias_or_die(aliases: &[AccountAlias], a: &str) -> String {
    match account_name_apply_aliases(aliases, a) {
        Ok(n) => n,
        Err(e) => panic!("problem in account alias applied to {a}: {e}"),
    }
}

/// The byte length of an account name parsed from the start of `src`
/// (spaces inside it may have been tabs, which read as spaces).
fn c_len(src: &str, parsed: &str) -> usize {
    src.char_indices()
        .nth(parsed.chars().count())
        .map_or(src.len(), |(i, _)| i)
}

/// Whether text starts with these words, separated by one or more spaces
/// (keywordsp).
fn keywords_match(s: &str, words: &[&str]) -> bool {
    let mut rest = s;
    for (i, w) in words.iter().enumerate() {
        if i > 0 {
            let trimmed = rest.trim_start_matches(crate::parse::is_non_newline_space);
            if trimmed.len() == rest.len() {
                return false;
            }
            rest = trimmed;
        }
        match rest.strip_prefix(w) {
            Some(r) => rest = r,
            None => return false,
        }
    }
    true
}

fn please_include_decimal_point(c: &Cursor, off: usize) -> Failure {
    Failure::Here(ParseError::fail_at(
        c.src,
        off,
        "Please include a decimal point or decimal comma in commodity directives,\n\
         to help us parse correctly. It may be followed by zero or more decimal digits.\n\
         Examples:\n\
         commodity $1000.            ; no thousands mark, decimal period, no decimals\n\
         commodity 1.234,00 ARS      ; period at thousands, decimal comma, 2 decimals\n\
         commodity EUR 1 000,000     ; space at thousands, decimal comma, 3 decimals\n\
         commodity INR1,23,45,678.0  ; comma at thousands/lakhs/crores, decimal period, 1 decimal",
    ))
}

/// A commodity alias: a symbol, or a positive amount (`100 cents`).
fn parse_commodity_alias(v: &str) -> Option<Amount> {
    let mut c = Cursor::new(v);
    c.skip_spaces();
    let save = c;
    if let Ok(sym) = commodity_symbolp(&mut c) {
        c.skip_spaces();
        if c.at_end() {
            return Some(Amount {
                commodity: Sym::from(sym),
                quantity: crate::decimal::Decimal::from_i64(1),
                ..Amount::null()
            });
        }
    }
    c = save;
    let a = amountp(&mut c, &AmountCtx::default(), AmountKind::Multiplier).ok()?;
    c.skip_spaces();
    (c.at_end() && !a.commodity.is_empty() && a.quantity > crate::decimal::Decimal::ZERO)
        .then_some(a)
}

/// An account directive's `type:` tag value: a one-letter code or a type
/// name (parseAccountTypeCode).
fn parse_account_type_code(s: &str) -> Result<AccountType, String> {
    AccountType::parse(true, s).map_err(|_| {
        format!(
            "invalid account type code {s}, should be one of {}",
            AccountType::choices(true)
        )
    })
}

/// Round a periodic or auto posting rule's posting amounts calculated with
/// `*` or `/` to their own precision (postingRoundCalculatedAmounts with no
/// styles).
fn posting_round_calculated_amounts(mut p: Posting) -> Posting {
    super::balancing::posting_round_calculated_amounts(&Default::default(), &mut p);
    p
}

/// A file path's reader prefix, if any, and the rest (splitReaderPrefix).
/// The rules reader has no prefix: `rules:` stays part of the path, as
/// upstream leaves it.
pub fn split_reader_prefix(f: &str) -> (Option<&str>, &str) {
    for r in ["journal", "timeclock", "timedot", "csv", "ssv", "tsv"] {
        if let Some(rest) = f.strip_prefix(r).and_then(|s| s.strip_prefix(':')) {
            return (Some(r), rest);
        }
    }
    (None, f)
}

/// A path with a leading `~` replaced by the home directory
/// (expandHomePath).
pub fn expand_home_path(p: &str) -> Result<String, String> {
    let home = crate::platform::home_dir;
    if p == "~" {
        Ok(home())
    } else if let Some(rest) = p.strip_prefix("~/") {
        Ok(format!("{}/{rest}", home()))
    } else if p.starts_with('~') {
        Err("~USERNAME in paths is not supported".to_string())
    } else {
        Ok(p.to_string())
    }
}

/// The path made absolute by prepending the current directory, if it is
/// relative (makeAbsolute).
pub fn absolute_path(p: &str) -> String {
    if p.starts_with('/') {
        normalise_path(p)
    } else {
        let cwd = std::env::current_dir().unwrap_or_default();
        normalise_path(&format!("{}/{p}", cwd.display()))
    }
}

/// The path without `.` components or repeated slashes (normalise).
fn normalise_path(p: &str) -> String {
    let absolute = p.starts_with('/');
    let parts: Vec<&str> = p
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    let joined = parts.join("/");
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

/// The path with symbolic links and `..` resolved, as far as it exists
/// (canonicalizePath).
pub fn canonical_path(p: &str) -> PathBuf {
    let path = Path::new(p);
    if let Ok(c) = std::fs::canonicalize(path) {
        return c;
    }
    match (path.parent(), path.file_name()) {
        (Some(dir), Some(name)) if !dir.as_os_str().is_empty() => {
            canonical_path(&dir.to_string_lossy()).join(name)
        }
        _ => path.to_path_buf(),
    }
}

/// The files and directories a glob pattern matches, relative to a
/// directory, sorted (for -f's patterns).
pub fn glob_matches(dir: &str, pattern: &str) -> Vec<String> {
    match Glob::compile(pattern) {
        Ok(g) => {
            let mut found: Vec<String> = g
                .matches(Path::new(dir))
                .into_iter()
                .filter(|f| Path::new(f).exists())
                .collect();
            found.sort();
            found
        }
        Err(_) => Vec::new(),
    }
}

/// `**` not followed by a slash made `**/*`, so that it also matches file
/// name parts.
fn expand_double_stars(p: &str) -> String {
    let mut out = String::with_capacity(p.len() + 4);
    let b = p.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if p[i..].starts_with("**") && b.get(i + 2).is_some_and(|&c| c != b'/' && c != b'\\') {
            out.push_str("**/*");
            i += 2;
        } else {
            let ch = p[i..].chars().next().unwrap_or_default();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// A compiled glob pattern (the parts of System.FilePath.Glob that include
/// uses): path components of literal text, `*`, `?` and `[...]` classes,
/// and `**/` matching any number of directories.
struct Glob {
    absolute: bool,
    components: Vec<GlobComponent>,
}

enum GlobComponent {
    /// `**`: any number of directories.
    AnyDirectories,
    Pattern(Vec<GlobToken>),
}

#[derive(Clone)]
enum GlobToken {
    Char(char),
    Any,
    One,
    Class(bool, Vec<(char, char)>),
}

impl Glob {
    fn compile(pattern: &str) -> Result<Glob, String> {
        let absolute = pattern.starts_with('/');
        let mut components = Vec::new();
        for comp in pattern.split('/').filter(|s| !s.is_empty()) {
            if comp == "**" {
                components.push(GlobComponent::AnyDirectories);
                continue;
            }
            let mut tokens = Vec::new();
            let mut chars = comp.chars().peekable();
            while let Some(ch) = chars.next() {
                match ch {
                    '*' => {
                        while chars.peek() == Some(&'*') {
                            chars.next();
                        }
                        tokens.push(GlobToken::Any);
                    }
                    '?' => tokens.push(GlobToken::One),
                    '[' => {
                        let negated = matches!(chars.peek(), Some('!' | '^'));
                        if negated {
                            chars.next();
                        }
                        let mut ranges = Vec::new();
                        let mut first = true;
                        loop {
                            match chars.next() {
                                None => return Err("compile :: unclosed [] in pattern".to_string()),
                                Some(']') if !first => break,
                                Some(a) => {
                                    if chars.peek() == Some(&'-') {
                                        let mut look = chars.clone();
                                        look.next();
                                        match look.peek() {
                                            Some(&b) if b != ']' => {
                                                chars.next();
                                                chars.next();
                                                ranges.push((a, b));
                                            }
                                            _ => ranges.push((a, a)),
                                        }
                                    } else {
                                        ranges.push((a, a));
                                    }
                                }
                            }
                            first = false;
                        }
                        tokens.push(GlobToken::Class(negated, ranges));
                    }
                    c => tokens.push(GlobToken::Char(c)),
                }
            }
            components.push(GlobComponent::Pattern(tokens));
        }
        Ok(Glob {
            absolute,
            components,
        })
    }

    fn is_literal(&self) -> bool {
        self.components.iter().all(|c| match c {
            GlobComponent::AnyDirectories => false,
            GlobComponent::Pattern(ts) => ts.iter().all(|t| matches!(t, GlobToken::Char(_))),
        })
    }

    /// The paths matching this pattern, relative to `dir` if it is relative
    /// (globDir1). Wildcards do not match a name's leading dot, and `**`
    /// does not enter the dot directories directly below where it starts,
    /// as with upstream's Glob library.
    fn matches(&self, dir: &Path) -> Vec<String> {
        let base = if self.absolute {
            String::new()
        } else {
            dir.display().to_string()
        };
        let mut out = Vec::new();
        self.walk(&base, 0, &mut out);
        out
    }

    fn walk(&self, path: &str, i: usize, out: &mut Vec<String>) {
        let Some(comp) = self.components.get(i) else {
            out.push(if path.is_empty() {
                "/".to_string()
            } else {
                path.to_string()
            });
            return;
        };
        match comp {
            GlobComponent::AnyDirectories => {
                self.walk(path, i + 1, out);
                self.walk_dirs(path, i + 1, true, out);
            }
            GlobComponent::Pattern(tokens) => {
                if tokens.iter().all(|t| matches!(t, GlobToken::Char(_))) {
                    let name: String = tokens
                        .iter()
                        .map(|t| match t {
                            GlobToken::Char(c) => *c,
                            _ => unreachable!(),
                        })
                        .collect();
                    self.walk(&format!("{path}/{name}"), i + 1, out);
                    return;
                }
                for name in dir_entries(path) {
                    if glob_tokens_match(tokens, &name) {
                        self.walk(&format!("{path}/{name}"), i + 1, out);
                    }
                }
            }
        }
    }

    /// The directories below `path`, for `**`, at any depth, each followed
    /// by the rest of the pattern.
    fn walk_dirs(&self, path: &str, rest: usize, first_level: bool, out: &mut Vec<String>) {
        for name in dir_entries(path) {
            if first_level && name.starts_with('.') {
                continue;
            }
            let sub = format!("{path}/{name}");
            if Path::new(&sub).is_dir() {
                self.walk(&sub, rest, out);
                self.walk_dirs(&sub, rest, false, out);
            }
        }
    }
}

/// The names in a directory, sorted (none if it can't be read).
fn dir_entries(path: &str) -> Vec<String> {
    let dir = if path.is_empty() { "/" } else { path };
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// Whether a file name matches a glob component; a leading dot must be
/// matched literally.
fn glob_tokens_match(tokens: &[GlobToken], name: &str) -> bool {
    if name.starts_with('.') && !matches!(tokens.first(), Some(GlobToken::Char('.'))) {
        return false;
    }
    fn go(tokens: &[GlobToken], s: &[char]) -> bool {
        match tokens.split_first() {
            None => s.is_empty(),
            Some((GlobToken::Any, rest)) => (0..=s.len()).any(|i| go(rest, &s[i..])),
            Some((t, rest)) => match s.split_first() {
                None => false,
                Some((c, srest)) => {
                    let ok = match t {
                        GlobToken::Char(x) => x == c,
                        GlobToken::One => true,
                        GlobToken::Class(neg, ranges) => {
                            ranges.iter().any(|(a, b)| a <= c && c <= b) != *neg
                        }
                        GlobToken::Any => unreachable!(),
                    };
                    ok && go(rest, srest)
                }
            },
        }
    }
    let chars: Vec<char> = name.chars().collect();
    go(tokens, &chars)
}

pub(crate) mod tests {
    use super::*;
    use crate::amount::{num, usd};
    use crate::dates::{DateSpan, EfDay, Interval};
    use crate::decimal::Decimal;
    use crate::model::Status;

    fn parse(s: &str) -> Result<Journal, String> {
        let iopts = InputOpts {
            today: Day::from_ymd(2026, 1, 1).unwrap(),
            ..InputOpts::default()
        };
        parse_journal(&iopts, "", s).map(|(j, _)| j)
    }

    fn day(y: i64, m: u32, d: u32) -> Day {
        Day::from_ymd(y, m, d).unwrap()
    }

    fn txn(s: &str) -> Transaction {
        let mut j = parse(s).unwrap();
        assert_eq!(j.txns.len(), 1, "{s}");
        j.txns.remove(0)
    }

    /// The single posting of a transaction with this posting line.
    fn posting(line: &str) -> Result<Posting, String> {
        parse(&format!("2000-01-01\n{line}")).map(|mut j| j.txns.remove(0).postings.remove(0))
    }

    fn periodic(s: &str) -> PeriodicTransaction {
        parse(s).unwrap().periodic_txns.remove(0)
    }

    // A commodity declared again adds to its earlier declaration, the later
    // tags first (commodityRedeclare, addCommodityDeclaration).
    #[test]
    fn redeclared_commodity_tags_later_first() {
        let j = parse(
            "commodity 1000. kt  ; alias: 1e-3 Mt, t: 1
commodity kt  ; alias: 10 users
",
        )
        .unwrap();
        let tags = |ts: &[Tag]| ts.iter().map(|(_, v)| v.clone()).collect::<Vec<_>>();
        assert_eq!(
            tags(&j.declared_commodity_tags["kt"]),
            ["10 users", "1e-3 Mt", "1"]
        );
        assert_eq!(
            tags(&j.declared_commodities["kt"].tags),
            ["10 users", "1e-3 Mt", "1"]
        );
    }

    #[cfg_attr(test, test)]
    pub(crate) fn account_names() {
        let t = txn("2000-01-01\n  a:b:c  1\n  a b:c\n");
        assert_eq!(&*t.postings[0].account, "a:b:c");
        assert_eq!(&*t.postings[1].account, "a b:c");
    }

    #[cfg_attr(test, test)]
    pub(crate) fn periodic_transactions() {
        let pos = |l| {
            (
                SourcePos::new(&Arc::from(""), 1, 1),
                SourcePos::new(&Arc::from(""), l, 1),
            )
        };
        let p = periodic("~ monthly from 2018/6 ;In 2019 we will change this\n");
        assert_eq!(p.period_expr, "monthly from 2018/6");
        assert_eq!(p.interval, Interval::Months(1));
        assert_eq!(p.span, DateSpan(Some(EfDay::flex(day(2018, 6, 1))), None));
        assert_eq!(p.pos, pos(2));
        assert_eq!(
            (p.description.as_str(), p.comment.as_str()),
            ("", "In 2019 we will change this\n")
        );
        let p = periodic("~ monthly from 2018/6   In 2019 we will change this\n");
        assert_eq!(p.period_expr, "monthly from 2018/6");
        assert_eq!(
            (p.description.as_str(), p.comment.as_str()),
            ("In 2019 we will change this", "")
        );
        let p = periodic("~ monthly  Next year blah blah\n");
        assert_eq!(
            (p.period_expr.as_str(), p.span),
            ("monthly", DateSpan(None, None))
        );
        assert_eq!(p.description, "Next year blah blah");
        let p = periodic("~ 2019-01-04\n");
        assert_eq!(p.interval, Interval::NoInterval);
        assert_eq!(
            p.span,
            DateSpan(
                Some(EfDay::exact(day(2019, 1, 4))),
                Some(EfDay::exact(day(2019, 1, 5)))
            )
        );
        assert_eq!(
            periodic("~ 2019-01-04\n  ;\n  a  1\n  b\n").postings.len(),
            2
        );
    }

    #[cfg_attr(test, test)]
    pub(crate) fn postings() {
        let p = posting("  expenses:food:dining  $10.00   ; a: a a \n   ; b: b b \n").unwrap();
        assert_eq!(&*p.account, "expenses:food:dining");
        assert_eq!(
            p.amount,
            MixedAmount::from_amount(usd(Decimal::from_i64(10)))
        );
        assert_eq!(p.comment, "a: a a\nb: b b\n");
        let tag = |n: &str, v: &str| (n.to_string(), v.to_string());
        assert_eq!(p.tags, [tag("a", "a a"), tag("b", "b b")]);
        let p = posting(" a  1. ; date:2012/11/28, date2=2012/11/29,b:b\n").unwrap();
        assert_eq!(
            p.amount.amounts_raw()[0].quantity,
            num(Decimal::from_i64(1)).quantity
        );
        assert_eq!(p.comment, "date:2012/11/28, date2=2012/11/29,b:b\n");
        assert_eq!(
            p.tags,
            [tag("date", "2012/11/28"), tag("date2=2012/11/29,b", "b")]
        );
        assert_eq!((p.date, p.date2), (Some(day(2012, 11, 28)), None));
        let p = posting(" a  1. ; [2012/11/28=2012/11/29]\n").unwrap();
        assert_eq!(
            (p.comment.as_str(), p.tags.len()),
            ("[2012/11/28=2012/11/29]\n", 0)
        );
        assert_eq!(
            (p.date, p.date2),
            (Some(day(2012, 11, 28)), Some(day(2012, 11, 29)))
        );
        for ok in [
            "  a  1 \"DE123\"\n",
            "  a  1A {1B}\n",
            "  a  1A {=1B}\n",
            "  a  1A {{1B}}\n",
            "  a  1A {{  =  1B }}\n",
            "  a  1A {1B} @ 1B\n",
            "  a  1A @ 1B {1B}\n",
            "  a  1A [2000-01-01]\n",
            "  a  1A @ 1B {1B} [2000-01-01]\n",
            "  a  1A [2000-01-01] {1B} @ 1B\n",
            "  a  $1 == $1\n",
        ] {
            assert!(posting(ok).is_ok(), "{ok}");
        }
        let e = posting("  a  1A @ 1B = 1A {1B}\n").unwrap_err();
        assert!(e.contains("unexpected '{'"), "{e}");
    }

    #[cfg_attr(test, test)]
    pub(crate) fn transaction_modifiers() {
        let m = parse("= (some value expr)\n some:postings  1.\n")
            .unwrap()
            .txn_modifiers
            .remove(0);
        assert_eq!(m.query, "(some value expr)");
        assert_eq!(m.rules.len(), 1);
        assert_eq!(&*m.rules[0].posting.account, "some:postings");
        assert!(!m.rules[0].is_multiplier);
    }

    #[cfg_attr(test, test)]
    pub(crate) fn transactions() {
        let t = txn("2015/1/1\n");
        assert_eq!((t.date, t.postings.len()), (day(2015, 1, 1), 0));
        let t = txn(concat!(
            "2012/05/14=2012/05/15 (code) desc  ; tcomment1\n",
            "    ; tcomment2\n",
            "    ; ttag1: val1\n",
            "    * a         $1.00  ; pcomment1\n",
            "    ; pcomment2\n",
            "    ; ptag1: val1\n",
            "    ; ptag2: val2\n"
        ));
        assert_eq!((t.pos.0.line, t.pos.1.line, t.pos.1.col), (1, 8, 1));
        assert_eq!(
            (t.date, t.date2),
            (day(2012, 5, 14), Some(day(2012, 5, 15)))
        );
        assert_eq!((t.code.as_str(), t.description.as_str()), ("code", "desc"));
        assert_eq!(t.comment, "tcomment1\ntcomment2\nttag1: val1\n");
        assert_eq!(t.tags, [("ttag1".to_string(), "val1".to_string())]);
        let p = &t.postings[0];
        assert_eq!((p.status, &*p.account), (Status::Cleared, "a"));
        assert_eq!(
            p.comment,
            "pcomment1\npcomment2\nptag1: val1\nptag2: val2\n"
        );
        assert_eq!(p.tags.len(), 2);
        assert!(
            parse("2007/01/28 coopportunity\n    expenses:food:groceries                   $47.18\n    assets:checking                          $-47.18\n\n")
                .is_ok()
        );
        assert_eq!(txn("2009/1/1 a ;comment\n b 1\n").description, "a");
        assert!(parse("2012/1/1\n  a  1\n  b\n \n").is_ok());
        assert!(parse("2012/1/1\n  ;\n  a  1\n  b\n \n").is_ok());
        let t = txn(concat!(
            "2009/1/1 x  ; transaction comment\n",
            " a  1  ; posting 1 comment\n",
            " ; posting 1 comment 2\n",
            " b\n",
            " ; posting 2 comment\n"
        ));
        assert_eq!(t.postings.len(), 2);
    }

    #[cfg_attr(test, test)]
    pub(crate) fn directives() {
        assert!(parse("!account a\n").is_ok());
        assert!(parse("!D 1.0\n").is_ok());
        assert!(parse("account a:b  ; a comment\n").is_ok());
        assert!(parse("account a:b  ; type:A\n").is_ok());
        let j = parse("account a:b  ; type:asset\n").unwrap();
        let (a, info) = &j.declared_accounts[0];
        assert_eq!(
            (&**a, info.comment.as_str(), info.order),
            ("a:b", "type:asset\n", 1)
        );
        assert_eq!(info.tags, [("type".to_string(), "asset".to_string())]);
        assert!(parse("C 1h = $50.00\n").is_ok());
        assert!(parse("D $1,000.0\n").is_ok());
        assert!(
            parse("D $1000\n")
                .unwrap_err()
                .contains("Please include a decimal point or decimal comma")
        );
        assert!(parse("Y 1000").is_ok());
        assert!(parse("Y 12345").is_ok());
        assert!(parse("N $\n").is_ok());
        for (s, e) in [
            (
                "include nosuchfile\n",
                "No files were matched by: nosuchfile",
            ),
            (
                "include nosuchfile*\n",
                "No files were matched by: nosuchfile*",
            ),
        ] {
            assert!(parse(s).unwrap_err().contains(e), "{s}");
        }
        let p = parse("P 2017/01/30 BTC $922.83\n")
            .unwrap()
            .price_directives
            .remove(0);
        assert_eq!((p.date, &*p.commodity), (day(2017, 1, 30), "BTC"));
        assert_eq!(p.amount, usd("922.83".parse().unwrap()));
        for ok in [
            "payee foo\n",
            "payee foo ; comment\n",
            "payee \"a b\"\n",
            "payee \"\"\n",
        ] {
            assert!(parse(ok).is_ok(), "{ok}");
        }
        assert!(parse("tag foo \n").is_ok());
        assert!(parse("end tag \n").is_ok());
        assert!(parse("end apply tag \n").is_ok());
        let j = parse("").unwrap();
        assert!(j.txns.is_empty() && j.items.is_empty());
    }
}
