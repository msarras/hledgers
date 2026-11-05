//! CSV (character-separated values) data, converted to transactions by
//! rules (the CSV half of Hledger.Read.RulesReader, and CsvReader): the
//! lexer (the cassava-megaparsec parser that hledger uses), the conversion
//! of records to transactions, and the CSV, SSV and TSV file reader.

use std::collections::BTreeSet;
use std::sync::Arc;

use super::InputOpts;
use super::amount::{AmountCtx, AmountKind, amountp, posting_amountp};
use super::common::{posting_commentp, statusp, transaction_commentp};
use super::rules::{
    CsvRules, MAX_POSTINGS, is_amount_expression, parse_balance_assertion_type, read_int,
    record_as_approximate_text, simplify_sign, split_row_suffix, with_rules_pos,
};
use crate::account::{
    AccountAlias, Realness, account_name_apply_aliases, account_name_posting_type,
    account_name_without_posting_type,
};
use crate::amount::render::{AmountFormat, show_mixed_amount_b};
use crate::amount::{AmountStyle, MixedAmount, Sym};
use crate::dates::{Day, parse_date_with_format, parse_time_zone};
use crate::model::{BalanceAssertion, Journal, Posting, SourcePos, Status, Tag, Transaction};
use crate::parse::{Cursor, Item, ParseError};
use crate::text::{is_space, show_string, strip};

/// A CSV record: its field values.
pub type CsvRecord = Vec<String>;

/// A CSV record and the first and last lines it came from.
type LocatedRecord = ((usize, usize), CsvRecord);

/// A group of CSV records converted to one transaction (one, or more joined
/// by a merge rule), and the first and last lines they came from.
type LocatedGroup = ((usize, usize), Vec<CsvRecord>);

/// The records in some CSV text with this field separator, or an error as
/// megaparsec shows it, named `name` (cassava-megaparsec's decodeWith,
/// without a header). Fields are separated by the separator, records by a
/// newline or CRLF; a field in double quotes may contain separators,
/// newlines and doubled double quotes, which stand for one. Upstream parses
/// bytes, and the separator is the low byte of its character; an error shows
/// its line as bytes, as Latin-1 characters.
pub fn parse_csv(sep: char, name: &str, text: &str) -> Result<Vec<CsvRecord>, String> {
    let sep = (u32::from(sep) & 0xff) as u8;
    let b = text.as_bytes();
    let field_end = |c: u8| c == sep || c == b'"' || c == b'\n' || c == b'\r';
    let mut records = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let mut record = Vec::new();
        loop {
            // a field; afterwards, what else could have continued it
            let (field, hint) = if b[i] == b'"' {
                let mut value = Vec::new();
                i += 1;
                loop {
                    match b.get(i) {
                        None => {
                            let expected = [
                                Item::tokens("\""),
                                Item::label("escaped double-quote"),
                                Item::label("unescaped character"),
                            ];
                            let e = ParseError::trivial(i, Some(Item::EndOfInput), &expected);
                            return Err(pretty_bytes(&e, name, b));
                        }
                        Some(b'"') if b.get(i + 1) == Some(&b'"') => {
                            value.push(b'"');
                            i += 2;
                        }
                        Some(b'"') => {
                            i += 1;
                            break;
                        }
                        Some(&c) => {
                            value.push(c);
                            i += 1;
                        }
                    }
                }
                (value, None)
            } else {
                let start = i;
                while i < b.len() && !field_end(b[i]) {
                    i += 1;
                }
                let hint = if i > start {
                    "unescaped character"
                } else {
                    "field"
                };
                (b[start..i].to_vec(), Some(hint))
            };
            record.push(String::from_utf8_lossy(&field).into_owned());
            match b.get(i) {
                Some(&c) if c == sep => i += 1,
                Some(b'\n') => {
                    i += 1;
                    break;
                }
                Some(b'\r') if b.get(i + 1) == Some(&b'\n') => {
                    i += 2;
                    break;
                }
                None => break,
                Some(&c) => {
                    let mut expected = BTreeSet::from([
                        Item::Tokens(char::from(sep).to_string()),
                        Item::label("end of line"),
                        Item::EndOfInput,
                    ]);
                    expected.extend(hint.map(Item::label));
                    let e = ParseError::Trivial {
                        offset: i,
                        unexpected: Some(Item::Tokens(char::from(c).to_string())),
                        expected,
                    };
                    return Err(pretty_bytes(&e, name, b));
                }
            }
        }
        records.push(record);
    }
    Ok(records)
}

/// A parse error in bytes, shown with each byte as a Latin-1 character, as
/// megaparsec shows an error in a byte string.
fn pretty_bytes(e: &ParseError, name: &str, b: &[u8]) -> String {
    let latin1: String = b.iter().map(|&c| char::from(c)).collect();
    let at = |offset: usize| {
        latin1
            .char_indices()
            .nth(offset)
            .map_or(latin1.len(), |(i, _)| i)
    };
    let e = match e.clone() {
        ParseError::Trivial {
            offset,
            unexpected,
            expected,
        } => ParseError::Trivial {
            offset: at(offset),
            unexpected,
            expected,
        },
        e => e,
    };
    e.pretty(name, &latin1)
}

/// The CSV, SSV or TSV file `f`, or standard input (`-`), read with its
/// rules into a finalised journal, its records' balance assertions not
/// checked (CsvReader's parse). `sep` is the separator its format implies.
pub fn read_csv_file(iopts: &InputOpts, f: &str, sep: char) -> Result<Journal, String> {
    let rules_file = rules_file_for(f, iopts.rules_file.as_deref())?;
    let (rules, _) = super::rules::read_rules(&rules_file)?;
    let text = read_data(f)?;
    let (j, context) = read_journal_from_csv(iopts, &rules_file, &rules, f, &text, Some(sep))?;
    finalise_csv_journal(iopts, f, j, &context)
}

/// The rules file for a CSV file: the one given (`--rules`), or the CSV
/// file's path with `.rules` added; standard input needs one given
/// (getRulesFile).
pub fn rules_file_for(csv: &str, given: Option<&str>) -> Result<String, String> {
    match given {
        Some(f) => Ok(f.to_string()),
        None if csv == "-" => Err("please use --rules when reading CSV from stdin".to_string()),
        None => Ok(format!("{csv}.rules")),
    }
}

/// A data file's text, or standard input's (hGetContentsPortably).
fn read_data(f: &str) -> Result<String, String> {
    if f == "-" {
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut std::io::stdin(), &mut bytes)
            .map_err(|e| format!("<stdin>: hGetContents: {e}"))?;
        super::decode_portably(bytes).map_err(|e| format!("<stdin>: hGetContents: {e}"))
    } else {
        super::read_file_portably(f).map_err(|e| format!("{f}: openFile: {e}"))
    }
}

/// A journal converted from CSV data finalised, after the `--alias` options
/// are applied, without checking balance assertions; an error about an entry
/// shows the record it came from.
pub fn finalise_csv_journal(
    iopts: &InputOpts,
    f: &str,
    j: Journal,
    context: &CsvContext,
) -> Result<Journal, String> {
    let aliases = super::journal::aliases_from_opts(iopts)?;
    let j = journal_apply_aliases(&aliases, j)?;
    let mut iopts = iopts.clone();
    iopts.balancing.ignore_assertions = true;
    super::finalise::journal_finalise(&iopts, f, "", j, Default::default())
        .map_err(|e| context.add_record(e))
}

/// Account aliases applied to the postings' accounts (journalApplyAliases).
pub fn journal_apply_aliases(aliases: &[AccountAlias], mut j: Journal) -> Result<Journal, String> {
    for p in j.txns.iter_mut().flat_map(|t| &mut t.postings) {
        let a = account_name_apply_aliases(aliases, &p.account).map_err(|e| {
            format!(
                "problem while applying account aliases:\n{aliases:?}\n to account name: {}\n {e}",
                p.account
            )
        })?;
        p.account = a.into();
    }
    Ok(j)
}

/// What finalising needs to show the record an entry came from: the data's
/// name in positions, the rules, and the record groups with their lines (the
/// error context adder of readJournalFromCsv).
pub struct CsvContext {
    name: String,
    rules: CsvRules,
    groups: Vec<LocatedGroup>,
}

impl CsvContext {
    /// The message, with the record group that it says the problem is in
    /// shown after it, if it mentions a line of the data (adderrorcontext).
    pub fn add_record(&self, msg: String) -> String {
        let needle = format!("{}:", self.name);
        let Some(i) = msg.find(&needle) else {
            return msg;
        };
        let digits: String = msg[i + needle.len()..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        let Ok(n) = digits.parse::<usize>() else {
            return msg;
        };
        match self
            .groups
            .iter()
            .find(|((l1, l2), _)| *l1 <= n && n <= *l2)
        {
            Some((_, g)) => format!(
                "{msg}\nthis entry was converted from:\n{}",
                self.rules.show_record_fields(g)
            ),
            None => msg,
        }
    }
}

/// The lines of a text, as Data.Text's lines splits them.
fn text_lines(s: &str) -> impl Iterator<Item = &str> {
    let s = s.strip_suffix('\n').unwrap_or(s);
    s.split('\n').filter(move |_| !s.is_empty())
}

/// CSV data converted by rules to transactions, as an unfinalised journal,
/// with what errors about them need (readJournalFromCsv). Blank lines are
/// ignored and a top-level skip rule skips lines; skip, end and merge rules
/// apply to records; the transactions are put in date order, guessing the
/// data's order. `sep` is the separator a reader prefix implies, if any.
pub fn read_journal_from_csv(
    iopts: &InputOpts,
    rules_file: &str,
    rules: &CsvRules,
    csv_file: &str,
    text: &str,
    sep: Option<char>,
) -> Result<(Journal, CsvContext), String> {
    let lines: Vec<(usize, &str)> = text_lines(text)
        .enumerate()
        .map(|(i, l)| (i + 1, l))
        .filter(|(_, l)| !strip(l).is_empty())
        .collect();
    // a top-level skip rule (the first one written) skips non-blank lines
    let skip = match rules.directive_first_wins("skip") {
        None => 0,
        Some("") => 1,
        Some(s) => {
            read_int(s).ok_or_else(|| format!("{rules_file}: could not parse skip value: {s}"))?
        }
    };
    let lines = &lines[usize::try_from(skip).unwrap_or(0).min(lines.len())..];
    let csv_text: String = lines.iter().map(|(_, l)| format!("{l}\n")).collect();
    // the separator rule wins over the file extension, and that over the reader prefix
    let ext = std::path::Path::new(csv_file)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let separator = match rules.directive("separator").and_then(parse_separator) {
        Some(c) => c,
        None if ext == "ssv" => ';',
        None if ext == "tsv" => '\t',
        None => sep.unwrap_or(','),
    };
    let name = if csv_file == "-" { "(stdin)" } else { csv_file };
    let records = if csv_text.is_empty() {
        Vec::new()
    } else {
        parse_csv(separator, name, &csv_text)?
    };
    let line_numbers: Vec<usize> = lines.iter().map(|(n, _)| *n).collect();
    let located = locate_csv_records(&line_numbers, records);
    let groups = apply_conditional_skips(rules, located)
        .and_then(|rs| apply_merge_rules(rules, rs))
        .map_err(|e| format!("{rules_file}: {e}"))?;
    validate_csv(name, &groups)?;
    // characters journal format can't represent are replaced, with a warning
    let warn = |msg: &str| crate::console::warn(&iopts.prog_args, msg);
    for (field, bad, replacement) in [("description", ';', ".,"), ("code", ')', "]")] {
        let n = groups
            .iter()
            .filter(|(_, g)| rules.field_value(g, field).is_some_and(|v| v.contains(bad)))
            .count();
        if n > 0 {
            warn(&format!(
                "{csv_file}: replaced '{bad}' with '{replacement}' in {n} {field}(s), since journal format can't represent it"
            ));
        }
    }
    // a merge rule's if block matching on a later row's field can never merge
    let refs: Vec<&str> = rules
        .blocks
        .iter()
        .filter(|b| b.assignments.iter().any(|a| a.name == "merge"))
        .flat_map(|b| b.matchers.iter().filter_map(|m| m.field.as_deref()))
        .filter(|r| split_row_suffix(r.trim_matches(|c| "%()".contains(c))).is_some())
        .collect();
    if !refs.is_empty() {
        warn(&format!(
            "{rules_file}: an if block containing a merge rule matches on a later row's field ({}); this can not trigger a merge, since these fields are empty until after merging",
            refs.join(", ")
        ));
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let tz_out = crate::dates::local_utc_offset_seconds(now) / 60;
    let tz_in = match rules.directive("timezone") {
        None => None,
        Some(s) => Some(
            parse_time_zone(s)
                .ok_or_else(|| format!("{rules_file}: could not parse time zone: {s}"))?,
        ),
    };
    let zoned = rules
        .directive("date-format")
        .is_some_and(|f| ["%Z", "%z", "%EZ", "%Ez"].iter().any(|z| f.contains(z)));
    let conv = Converter {
        rules,
        zoned,
        tz_in,
        tz_out,
    };
    let file: Arc<str> = Arc::from(name);
    let mut txns = Vec::with_capacity(groups.len());
    for ((l1, l2), g) in &groups {
        let pos = (
            SourcePos::new(&file, *l1, 1),
            SourcePos::new(&file, l2 + 1, 1),
        );
        txns.push(conv.transaction(pos, g)?);
    }
    order_transactions(rules, &mut txns);
    let context = CsvContext {
        name: name.to_string(),
        rules: rules.clone(),
        groups,
    };
    Ok((
        Journal {
            txns,
            ..Journal::default()
        },
        context,
    ))
}

/// A separator rule's value: `space`, `tab`, or else its first character
/// (parseSeparator).
fn parse_separator(s: &str) -> Option<char> {
    match s.to_lowercase().as_str() {
        "space" => Some(' '),
        "tab" => Some('\t'),
        s => s.chars().next(),
    }
}

/// Each record with the first and last line it came from: these are the
/// non-blank lines read, one for each line of a record (counting the
/// newlines in its quoted fields), continuing after the last one known
/// (locateCsvRecords).
fn locate_csv_records(line_numbers: &[usize], records: Vec<CsvRecord>) -> Vec<LocatedRecord> {
    let mut out = Vec::with_capacity(records.len());
    let (mut next, mut rest) = (1, line_numbers);
    for r in records {
        let numlines = 1 + r.iter().map(|f| f.matches('\n').count()).sum::<usize>();
        let (consumed, rest2) = rest.split_at(numlines.min(rest.len()));
        rest = rest2;
        let l1 = consumed.first().copied().unwrap_or(next);
        let l2 = consumed.last().copied().unwrap_or(l1 + numlines - 1);
        next = l2 + 1;
        out.push(((l1, l2), r));
    }
    out
}

/// The records left after the conditional skip and end rules apply
/// (applyConditionalSkips).
fn apply_conditional_skips(
    rules: &CsvRules,
    records: Vec<LocatedRecord>,
) -> Result<Vec<LocatedRecord>, String> {
    let mut out = Vec::new();
    let mut it = records.into_iter();
    while let Some((lines, r)) = it.next() {
        let group = std::slice::from_ref(&r);
        let skip = rule_record_count(rules, group, "skip")?;
        if rules.field_template(group, "end").is_some() {
            break;
        }
        if let Some(n) = skip {
            it.by_ref().take(n - 1).for_each(drop);
            continue;
        }
        out.push((lines, r));
    }
    Ok(out)
}

/// The records grouped by the merge rules, each record whose merge rule
/// applies taking the next N (or as many as remain) (applyMergeRules).
fn apply_merge_rules(
    rules: &CsvRules,
    records: Vec<LocatedRecord>,
) -> Result<Vec<LocatedGroup>, String> {
    let mut out = Vec::new();
    let mut it = records.into_iter();
    while let Some(((l1, mut l2), r)) = it.next() {
        let merge = rule_record_count(rules, std::slice::from_ref(&r), "merge")?;
        let mut group = vec![r];
        for ((_, m2), m) in it.by_ref().take(merge.unwrap_or(0)) {
            l2 = m2;
            group.push(m);
        }
        out.push(((l1, l2), group));
    }
    Ok(out)
}

/// The record count of a skip or merge rule applying to a record: one if it
/// has no value, otherwise its positive value (ruleRecordCount).
fn rule_record_count(
    rules: &CsvRules,
    group: &[CsvRecord],
    name: &str,
) -> Result<Option<usize>, String> {
    match rules.field_template(group, name) {
        None => Ok(None),
        Some("") => Ok(Some(1)),
        Some(x) => match read_int(x) {
            Some(n) if n >= 1 => Ok(Some(n as usize)),
            _ => Err(format!(
                "could not parse {name} value as a positive number: {x}"
            )),
        },
    }
}

/// An error if a record has fewer than two fields (validateCsv).
fn validate_csv(name: &str, groups: &[LocatedGroup]) -> Result<(), String> {
    match groups
        .iter()
        .find(|(_, recs)| recs.iter().any(|r| r.len() < 2))
    {
        Some(((l1, l2), recs)) => Err(format!(
            "{}This CSV record has less than two fields.\nPerhaps the separator is wrong (it can be set with a separator rule).",
            super::errors::csv_record_error_prefix(
                name,
                *l1,
                l2 + 1,
                &record_as_approximate_text(recs)
            )
        )),
        None => Ok(()),
    }
}

/// The transactions in date order, guessing the order of the data: each
/// day's records are reversed with an intra-day-reversed rule, then all of
/// them with a newest-first rule, or if the first date is after the last;
/// then they are sorted by date, keeping that order within a date.
fn order_transactions(rules: &CsvRules, txns: &mut [Transaction]) {
    if rules.directive("intra-day-reversed").is_some() {
        let mut start = 0;
        while start < txns.len() {
            let date = txns[start].date;
            let end = start + txns[start..].iter().take_while(|t| t.date == date).count();
            txns[start..end].reverse();
            start = end;
        }
    }
    // the first and last of the dates in order of appearance
    let mut seen = BTreeSet::new();
    let mut last = None;
    for t in txns.iter() {
        if seen.insert(t.date) {
            last = Some(t.date);
        }
    }
    let looks_newest_first = txns.first().zip(last).is_some_and(|(f, l)| f.date > l);
    if rules.directive("newest-first").is_some() || looks_newest_first {
        txns.reverse();
    }
    txns.sort_by_key(|t| t.date);
}

/// Converting records to transactions: the rules, and the time zones of
/// their dates (a date format with a time zone, the timezone rule's zone
/// and the local one, in minutes).
struct Converter<'a> {
    rules: &'a CsvRules,
    zoned: bool,
    tz_in: Option<i64>,
    tz_out: i64,
}

/// The default unknown accounts, to which unassigned accounts of postings
/// with an amount go.
const UNKNOWN_EXPENSE_ACCOUNT: &str = "expenses:unknown";
const UNKNOWN_INCOME_ACCOUNT: &str = "income:unknown";

impl Converter<'_> {
    /// A record group's transaction, at these positions, or the error that
    /// converting it gives (transactionFromCsvRecord).
    fn transaction(
        &self,
        pos: (SourcePos, SourcePos),
        record: &[CsvRecord],
    ) -> Result<Transaction, String> {
        let rules = self.rules;
        let errpfx = super::errors::csv_record_error_prefix(
            &pos.0.file,
            pos.0.line,
            pos.1.line,
            &record_as_approximate_text(record),
        );
        let field = |f: &str| rules.field_value(record, f);
        let date_format = rules.directive("date-format");
        let parse_date =
            |s: &str| parse_date_with_format(self.zoned, self.tz_in, self.tz_out, date_format, s);
        let date_s = field("date").unwrap_or_default();
        let date =
            parse_date(&date_s).ok_or_else(|| self.date_error(&errpfx, record, "date", &date_s))?;
        let date2 = match field("date2") {
            Some(s) => {
                Some(parse_date(&s).ok_or_else(|| self.date_error(&errpfx, record, "date2", &s))?)
            }
            None => None,
        };
        let status = match field("status") {
            Some(s) => parse_status(&s).map_err(|e| {
                format!(
                    "{errpfx}could not parse status value \"{s}\" (should be *, ! or empty)\nthe parse error is:      {e}\n"
                )
            })?,
            None => Status::Unmarked,
        };
        let code = field("code").map_or(String::new(), |s| single_line(&s).replace(')', "]"));
        let description =
            field("description").map_or(String::new(), |s| single_line(&s).replace(';', ".,"));
        let comment = field("comment").map_or(String::new(), |s| unescape_newlines(&s));
        let tags = transaction_comment_tags(&comment);
        let p1_virtual =
            field("account1").is_some_and(|a| account_name_posting_type(&a) == Realness::Virtual);
        let mut postings = Vec::new();
        for n in 1..=MAX_POSTINGS {
            let comment =
                field(&format!("comment{n}")).map_or(String::new(), |s| unescape_newlines(&s));
            let (ptags, pdate) = posting_comment_tags_and_date(&comment);
            let currency = field(&format!("currency{n}"))
                .or_else(|| field("currency"))
                .unwrap_or_default();
            let account = field(&format!("account{n}")).map(|a| strip(&a).to_string());
            if account.as_deref() == Some("") {
                continue;
            }
            if let Some(a) = &account {
                // an account with an amount or comment written after it is an error
                let mut c = Cursor::new(a);
                let checked = super::common::accountnamep(&mut c, true).and_then(|_| {
                    if c.at_end() {
                        Ok(())
                    } else {
                        Err(c.unexpected(&[Item::EndOfInput]))
                    }
                });
                if let Err(e) = checked {
                    return Err(crate::reports::opts::usage_error(&e.pretty("", a)));
                }
            }
            let amount = self.amount(&errpfx, record, &currency, p1_virtual, n)?;
            let balance = self.balance(&errpfx, record, &currency, n)?;
            let (account, is_final) = match account {
                Some(a) => (a, true),
                None if amount.is_some() || balance.is_some() => {
                    (UNKNOWN_EXPENSE_ACCOUNT.to_string(), false)
                }
                None => continue,
            };
            let negative = amount.as_ref().and_then(MixedAmount::is_negative) == Some(true);
            let shown = if !is_final && account == UNKNOWN_EXPENSE_ACCOUNT && negative {
                UNKNOWN_INCOME_ACCOUNT
            } else {
                &account
            };
            let assertion = match balance {
                Some(b) => Some(Box::new(
                    self.balance_assertion(&errpfx, record, &pos.0, b)?,
                )),
                None => None,
            };
            postings.push(Posting {
                date: pdate,
                account: account_name_without_posting_type(shown).into(),
                amount: amount.unwrap_or_else(MixedAmount::missing),
                assertion,
                comment,
                tags: ptags,
                realness: account_name_posting_type(&account),
                ..Posting::default()
            });
        }
        let t = Transaction {
            pos,
            date,
            date2,
            status,
            code,
            description,
            comment,
            tags,
            postings,
            ..Transaction::default()
        };
        Ok(super::balancing::transaction_round_calculated_amounts(t))
    }

    /// The error for a date that can not be parsed (mkdateerror).
    fn date_error(&self, errpfx: &str, record: &[CsvRecord], field: &str, value: &str) -> String {
        let rules = self.rules;
        let format = rules.directive("date-format");
        let shown_format = format.map_or_else(
            || "\"YYYY/M/D\", \"YYYY-M-D\" or \"YYYY.M.D\"".to_string(),
            show_string,
        );
        let a = rules.field_assignment(record, field);
        let rule = with_rules_pos(
            &format!(
                "the {field} rule is:   {}",
                a.map_or("required, but missing", |a| a.template.text.as_str())
            ),
            &a.and_then(|a| a.pos.clone()),
        );
        let change_or_add = |present: bool| if present { "change your" } else { "add a" };
        format!(
            "{errpfx}could not parse \"{value}\" as a date using date format {shown_format}\n{}\n{rule}\nthe date-format is: {}\nyou may need to change your {field} rule, {} date-format rule, or {} skip rule\nfor m/d/y or d/m/y dates, use date-format %-m/%-d/%Y or date-format %-d/%-m/%Y\n",
            rules.show_record_fields(record),
            format.unwrap_or("unspecified"),
            change_or_add(format.is_some()),
            change_or_add(rules.directive("skip").is_some()),
        )
    }

    /// The amount for posting N, if any: from a non-empty value assigned to
    /// amountN, amountN-in or amountN-out, or for posting 1 (or 2, if
    /// posting 1 is not virtual), to amount, amount-in or amount-out, which
    /// posting 2 negates and converts to cost; numbered fields win over
    /// unnumbered ones, and non-zero amounts over zero ones; an -out amount
    /// is negated. More than one non-zero amount is an error (getAmount).
    fn amount(
        &self,
        errpfx: &str,
        record: &[CsvRecord],
        currency: &str,
        p1_virtual: bool,
        n: usize,
    ) -> Result<Option<MixedAmount>, String> {
        let rules = self.rules;
        let unnumbered = ["amount", "amount-in", "amount-out"];
        let mut names: Vec<String> = ["", "-in", "-out"]
            .iter()
            .map(|s| format!("amount{n}{s}"))
            .collect();
        if n == 1 || (n == 2 && !p1_virtual) {
            names.extend(unnumbered.map(String::from));
        }
        let mut values: Vec<(String, String)> = names
            .into_iter()
            .filter_map(|f| {
                let v = strip(&rules.field_value(record, &f)?).to_string();
                (!v.is_empty()).then_some((f, v))
            })
            .collect();
        if values
            .iter()
            .any(|(f, _)| f.contains(|c: char| c.is_ascii_digit()))
        {
            values.retain(|(f, _)| f.contains(|c: char| c.is_ascii_digit()));
        }
        let mut amounts = Vec::new();
        for (f, v) in values {
            let a = self.parse_amount(errpfx, record, currency, &v)?;
            let a = if n == 2 && unnumbered.contains(&f.as_str()) {
                (-a).to_cost()
            } else {
                a
            };
            amounts.push((f, a));
        }
        if amounts.iter().any(|(_, a)| !a.looks_zero()) {
            amounts.retain(|(_, a)| !a.looks_zero());
        } else {
            amounts.truncate(1);
        }
        match amounts.as_slice() {
            [] => Ok(None),
            [(f, a)] => Ok(Some(if f.ends_with("-out") {
                -a.clone()
            } else {
                a.clone()
            })),
            fs => {
                let mut lines = vec![
                    format!(
                        "Multiple non-zero amounts were assigned for an amount field, for posting {n}."
                    ),
                    rules.show_record_fields(record),
                ];
                for (f, a) in fs {
                    let assignment = rules.field_assignment(record, f);
                    lines.push(with_rules_pos(
                        &format!(
                            "rule \"{f} {}\" assigned value \"{}\"",
                            assignment.map_or("", |a| a.template.text.as_str()),
                            show_mixed_amount_b(&AmountFormat::DEFAULT, a).s
                        ),
                        &assignment.and_then(|a| a.pos.clone()),
                    ));
                }
                lines.extend(
                    [
                        "",
                        "Please ensure just one non-zero amount is assigned, perhaps with an if rule.",
                        "See also: https://hledger.org/hledger.html#setting-amounts",
                        "(hledger manual -> CSV format -> Tips -> Setting amounts)",
                    ]
                    .map(String::from),
                );
                Err(format!("{errpfx}{}", lines.join("\n")))
            }
        }
    }

    /// The expected balance for posting N, if a non-empty one is assigned
    /// to balanceN, or for posting 1 to balance (getBalance).
    fn balance(
        &self,
        errpfx: &str,
        record: &[CsvRecord],
        currency: &str,
        n: usize,
    ) -> Result<Option<crate::amount::Amount>, String> {
        let rules = self.rules;
        let value = rules
            .field_value(record, &format!("balance{n}"))
            .or_else(|| {
                (n == 1)
                    .then(|| rules.field_value(record, "balance"))
                    .flatten()
            });
        let Some(s) = value.map(|v| strip(&v).to_string()) else {
            return Ok(None);
        };
        if s.is_empty() {
            return Ok(None);
        }
        match self.run_amount_parser(errpfx, record, currency, &simplify_sign(&s), false)? {
            Ok(a) => Ok(Some(a)),
            Err(e) => Err(format!(
                "{errpfx}could not parse \"{s}\" as balance{n} amount\n{}\n{}the parse error is:      {e}\n",
                rules.show_record_fields(record),
                rules.show_rules(record)
            )),
        }
    }

    /// A CSV amount value as a journal amount, with the rules' decimal mark
    /// and default currency: an amount expression if it has an operator,
    /// otherwise an amount with its sign made canonical (parseAmount).
    fn parse_amount(
        &self,
        errpfx: &str,
        record: &[CsvRecord],
        currency: &str,
        s: &str,
    ) -> Result<MixedAmount, String> {
        let expr = is_amount_expression(s);
        let text = if expr {
            s.to_string()
        } else {
            simplify_sign(s)
        };
        match self.run_amount_parser(errpfx, record, currency, &text, expr)? {
            Ok(a) => Ok(MixedAmount::from_amount(a)),
            Err(e) => Err(format!(
                "{errpfx}could not parse \"{s}\" as an amount\n{}\n{}\nthe parse error is:      {e}\nyou may need to change your amount*, balance*, or currency* rules, or add or change your skip rule\n",
                self.rules.show_record_fields(record),
                self.rules.show_rules(record)
            )),
        }
    }

    /// An amount parsed from all of a text, with the rules' decimal mark,
    /// and the currency rule's commodity and style for an amount without a
    /// commodity, as after a D directive (runCsvAmountParser). The outer
    /// error is fatal (a bad decimal-mark or currency rule), the inner one
    /// the parse error as megaparsec shows it. A currency rule that can not
    /// be parsed is an error only when an amount needs it.
    fn run_amount_parser(
        &self,
        errpfx: &str,
        record: &[CsvRecord],
        currency: &str,
        text: &str,
        expr: bool,
    ) -> Result<Result<crate::amount::Amount, String>, String> {
        let decimal_mark = self.decimal_mark()?;
        let parse = |default: Option<&(Sym, Arc<AmountStyle>)>| {
            let ctx = AmountCtx {
                decimal_mark,
                default_commodity: default,
                ..AmountCtx::default()
            };
            run_parser(text, |c| {
                if expr {
                    posting_amountp(c, &ctx, AmountKind::Ordinary)
                } else {
                    amountp(c, &ctx, AmountKind::Ordinary)
                }
            })
        };
        if currency.is_empty() {
            return Ok(parse(None));
        }
        match self.parse_currency(currency, decimal_mark) {
            Some(default) => Ok(parse(Some(&default))),
            None => match parse(None) {
                Ok(a) if !a.commodity.is_empty() => Ok(Ok(a)),
                _ => Err(format!(
                    "{errpfx}could not parse \"{currency}\" as a currency\n{}\n{}a currency rule's value should be a commodity symbol, like USD or \"Some Fund\",\nor an amount showing the symbol's position, like 1 USD\n",
                    self.rules.show_record_fields(record),
                    self.rules.show_rules(record)
                )),
            },
        }
    }

    /// The decimal-mark rule's mark, if any; an empty one is an error
    /// (parseDecimalMark).
    fn decimal_mark(&self) -> Result<Option<char>, String> {
        let Some(v) = self.rules.directive("decimal-mark") else {
            return Ok(None);
        };
        match strip(v) {
            "." => Ok(Some('.')),
            "," => Ok(Some(',')),
            s => Err(format!(
                "decimal-mark's argument should be \".\" or \",\" (not \"{s}\")"
            )),
        }
    }

    /// A currency rule's value as a default commodity and style: an amount
    /// such as `1 USD` or `USD 1`, or a commodity symbol, placed on the left
    /// with no space (parseCurrency).
    fn parse_currency(
        &self,
        currency: &str,
        decimal_mark: Option<char>,
    ) -> Option<(Sym, Arc<AmountStyle>)> {
        let as_default = |t: &str| {
            let ctx = AmountCtx {
                decimal_mark,
                ..AmountCtx::default()
            };
            match run_parser(t, |c| amountp(c, &ctx, AmountKind::Style)) {
                Ok(a) if !a.commodity.is_empty() => Some((a.commodity.clone(), a.style.clone())),
                _ => None,
            }
        };
        as_default(currency).or_else(|| as_default(&format!("{currency}1")))
    }

    /// The balance assertion of an expected balance, of the type the
    /// balance-type rule gives, at the record's position (mkBalanceAssertion).
    fn balance_assertion(
        &self,
        errpfx: &str,
        record: &[CsvRecord],
        pos: &SourcePos,
        amount: crate::amount::Amount,
    ) -> Result<BalanceAssertion, String> {
        let (total, inclusive) = match self.rules.directive("balance-type") {
            None => (false, false),
            Some(x) => parse_balance_assertion_type(strip(x)).ok_or_else(|| {
                format!(
                    "{errpfx}balance-type \"{x}\" is invalid. Use =, ==, =* or ==*.\n{}\n{}",
                    self.rules.show_record_fields(record),
                    self.rules.show_rules(record)
                )
            })?,
        };
        Ok(BalanceAssertion {
            amount,
            total,
            inclusive,
            pos: pos.clone(),
        })
    }
}

/// A parser run on all of a text, its error as megaparsec shows it without
/// a file name.
fn run_parser<T>(
    text: &str,
    p: impl FnOnce(&mut Cursor) -> crate::parse::PResult<T>,
) -> Result<T, String> {
    let mut c = Cursor::new(text);
    let r = p(&mut c).and_then(|v| {
        if c.at_end() {
            Ok(v)
        } else {
            Err(c.unexpected(&[Item::EndOfInput]))
        }
    });
    r.map_err(|e| e.pretty("", text))
}

/// A status mark, `*` or `!` after optional spaces, or none, and nothing
/// else (statusp <* eof).
fn parse_status(s: &str) -> Result<Status, String> {
    run_parser(s, |c| Ok(statusp(c)))
}

/// Text on one line: its lines stripped, the empty ones dropped, joined by
/// spaces.
fn single_line(s: &str) -> String {
    text_lines(s)
        .map(strip)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Text with each `\n` (backslash, n) made a newline.
fn unescape_newlines(s: &str) -> String {
    s.replace("\\n", "\n")
}

/// Comment text as a following comment, each line after ` ;`, for parsing
/// its tags (textToFollowingComment).
fn to_following_comment(c: &str) -> String {
    let s: String = text_lines(c).map(|l| format!(" ;{l}\n")).collect();
    s.trim_start_matches(is_space).to_string()
}

/// The tags in a transaction comment.
fn transaction_comment_tags(comment: &str) -> Vec<Tag> {
    let text = to_following_comment(comment);
    transaction_commentp(&mut Cursor::new(&text), &[]).map_or(Vec::new(), |(_, tags)| tags)
}

/// The tags in a posting comment, and the posting date from a `date:` tag
/// or bracketed date, which must have a year.
fn posting_comment_tags_and_date(comment: &str) -> (Vec<Tag>, Option<Day>) {
    let text = to_following_comment(comment);
    posting_commentp(&mut Cursor::new(&text), None, &[])
        .map_or((Vec::new(), None), |(_, tags, date, _)| (tags, date))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_records_and_quotes() {
        let parsed = parse_csv(',', "", "a,b\n\"c,\"\"d\"\"\",\r\n\"multi\nline\",x\n").unwrap();
        assert_eq!(
            parsed,
            vec![
                vec!["a".to_string(), "b".to_string()],
                vec!["c,\"d\"".to_string(), String::new()],
                vec!["multi\nline".to_string(), "x".to_string()],
            ]
        );
        assert_eq!(
            parse_csv(';', "", "1;2\n").unwrap(),
            vec![vec!["1".to_string(), "2".to_string()]]
        );
        assert!(parse_csv(',', "", "").unwrap().is_empty());
    }

    #[test]
    fn errors_as_megaparsec_shows_them() {
        assert_eq!(
            parse_csv(',', "f", "2022-01-01,B\"B\",C\n").unwrap_err(),
            "f:1:13:\n  |\n1 | 2022-01-01,B\"B\",C\n  |             ^\nunexpected '\"'\n\
             expecting ',', end of input, end of line, or unescaped character\n"
        );
        assert_eq!(
            parse_csv(',', "f", "a,\"B\"x\n").unwrap_err(),
            "f:1:6:\n  |\n1 | a,\"B\"x\n  |      ^\nunexpected 'x'\n\
             expecting ',', end of input, or end of line\n"
        );
        assert_eq!(
            parse_csv(',', "f", "a,\rb\n").unwrap_err(),
            "f:1:3:\n  |\n1 | a,\rb\n  |   ^\nunexpected carriage return\n\
             expecting ',', end of input, end of line, or field\n"
        );
        assert_eq!(
            parse_csv(',', "f", "a,b\n\"\n").unwrap_err(),
            "f:3:1:\n  |\n3 | <empty line>\n  | ^\nunexpected end of input\n\
             expecting '\"', escaped double-quote, or unescaped character\n"
        );
        // bytes beyond ASCII show as Latin-1 characters; C1 controls take no column
        assert_eq!(
            parse_csv(',', "f", "a,é🎅B\"\n").unwrap_err(),
            "f:1:7:\n  |\n1 | a,Ã©ð\u{9f}\u{8e}\u{85}B\"\n  |       ^\nunexpected '\"'\n\
             expecting ',', end of input, end of line, or unescaped character\n"
        );
    }
}
