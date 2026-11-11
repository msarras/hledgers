//! CSV (character-separated values) data, converted to transactions by
//! rules (the CSV half of Hledger.Read.RulesReader, and CsvReader): the
//! lexer (the cassava-megaparsec parser that hledger uses), the conversion
//! of records to transactions, and the CSV, SSV and TSV file reader.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::InputOpts;
use super::amount::{AmountCtx, AmountKind, amountp, posting_amountp};
use super::common::{posting_commentp, statusp, transaction_commentp};
use super::rules::{
    CsvRules, FieldAssignment, MAX_POSTINGS, Scope, is_amount_expression,
    parse_balance_assertion_type, path_join, read_int, record_as_approximate_text, simplify_sign,
    split_row_suffix, take_base_name, take_directory, with_rules_pos,
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
use crate::text::{hs_lines, is_space, show_string, strip};

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
    let (rules, rules_files) = super::rules::read_rules(&rules_file)?;
    let text = read_data(f, &rules_file, &rules)?;
    let (mut j, context) = read_journal_from_csv(iopts, &rules_file, &rules, f, &text, Some(sep))?;
    j.aux_files = rules_files;
    finalise_csv_journal(iopts, f, j, &context)
}

/// A rules file read as data (`-f FILE.rules`): the data its source rule
/// names, converted by it, as a finalised journal whose balance assertions
/// are not checked (RulesReader's parse).
pub fn read_rules_file(iopts: &InputOpts, rules_file: &str) -> Result<Journal, String> {
    read_rules_data_with(iopts, rules_file, |j, context| {
        finalise_csv_journal(iopts, rules_file, j, context)
    })
}

/// The data that a rules file's source rule names, converted by it,
/// unfinalised (readRulesFileWith). The source rule gives a file pattern,
/// or a command producing the data after a `|`, or both, the command then
/// cleaning the file's data; `#` starts a comment. The newest matching file
/// is read (or when importing with an archive rule, the oldest): with a
/// pattern starting `./` or `../`, relative to the rules file; with an
/// absolute one or one starting with `~`, as it is; otherwise in the data
/// directory (`data/` beside the main file), or if none match there, in
/// `~/Downloads`. A pattern matching nothing gives no data. The journal is
/// then given to `postprocess`; after that, when importing (not a dry run)
/// with an archive rule, data that was read is archived and its data file
/// removed.
pub fn read_rules_data_with(
    iopts: &InputOpts,
    rules_file: &str,
    postprocess: impl FnOnce(Journal, &CsvContext) -> Result<Journal, String>,
) -> Result<Journal, String> {
    let (rules, rules_files) = super::rules::read_rules(rules_file)?;
    let (pattern, command) = match rules.directive("source") {
        None => (None, None),
        Some(s) => {
            let s = strip(s.split('#').next().unwrap_or(""));
            let (p, c) = match s.find('|') {
                Some(i) => (&s[..i], Some(strip(&s[i + 1..]))),
                None => (s, None),
            };
            let p = strip(p);
            (
                (!p.is_empty()).then(|| p.to_string()),
                c.filter(|c| !c.is_empty()).map(str::to_string),
            )
        }
    };
    let journal_dir = iopts
        .journal_dir
        .clone()
        .unwrap_or_else(|| take_directory(rules_file));
    let data_dir = path_join(&journal_dir, "data");
    let data_file = match (&pattern, &command) {
        (None, None) => {
            return Err(format!(
                "to make {rules_file} readable,\n please add a 'source' rule with a non-empty file pattern or command"
            ));
        }
        (None, Some(_)) => None,
        (Some(pat), _) => {
            let first = pat.split('/').next().unwrap_or("");
            let anchored = first == "." || first == "..";
            let absolute_or_tilde = pat.starts_with('/') || first == "~";
            let primary = if anchored {
                take_directory(rules_file)
            } else {
                data_dir.clone()
            };
            let mut found = source_files(&primary, pat)?;
            if found.is_empty() && !anchored && !absolute_or_tilde {
                let home = crate::platform::home_dir();
                found = source_files(&path_join(&home, "Downloads"), pat)?;
            }
            if iopts.importing && rules.directive("archive").is_some() {
                found.first().cloned()
            } else {
                found.last().cloned()
            }
        }
    };
    let existing = data_file.clone().filter(|f| super::file_exists(f));
    let log_command = |c: &str| {
        let name = rules_file.rsplit('/').next().unwrap_or(rules_file);
        eprintln!("{name}: running source command: {c}");
    };
    let text = match (&pattern, &existing, &command) {
        (_, Some(f), c) => {
            let raw = read_data(f, rules_file, &rules)?;
            match c {
                Some(c) => {
                    log_command(c);
                    run_command(rules_file, c, Some(&raw))?
                }
                None => raw,
            }
        }
        (None, None, Some(c)) => {
            // a data generating command that fails gives a warning (showing
            // the error as upstream's error' makes it) and no data
            log_command(c);
            run_command(rules_file, c, None).unwrap_or_else(|e| {
                crate::console::warn(&iopts.prog_args, &format!("Error: {e}"));
                String::new()
            })
        }
        _ => String::new(),
    };
    let csv_file = data_file.as_deref().unwrap_or("(cmd)");
    let (mut j, context) = read_journal_from_csv(iopts, rules_file, &rules, csv_file, &text, None)?;
    j.aux_files = rules_files.into_iter().chain(existing).collect();
    let j = postprocess(j, &context)?;
    if !text.is_empty() && iopts.importing && rules.directive("archive").is_some() && !iopts.dry_run
    {
        archive_imported_data(&data_dir, rules_file, data_file.as_deref(), &text)?;
    }
    Ok(j)
}

/// Archive data that was imported, in `archive/` in the data directory,
/// unless the latest archive for this rules file holds the same data;
/// then remove its data file, if any, so that the next import can move on
/// to the next one.
fn archive_imported_data(
    data_dir: &str,
    rules_file: &str,
    data_file: Option<&str>,
    data: &str,
) -> Result<(), String> {
    let archive_dir = path_join(data_dir, "archive");
    let previous = match latest_archive(&archive_dir, rules_file) {
        Some(f) => Some(super::read_file_portably(&f)?),
        None => None,
    };
    if previous.as_deref() != Some(data) {
        save_to_archive(&archive_dir, rules_file, data_file, data)?;
    }
    if let Some(f) = data_file {
        std::fs::remove_file(f)
            .map_err(|e| format!("{f}: removeLink: {}", super::io_error_text(&e)))?;
    }
    Ok(())
}

/// The data file most recently archived for this rules file, if any: the
/// archive directory's newest file named after the rules file, not an
/// `.orig.` original (latestArchive).
fn latest_archive(archive_dir: &str, rules_file: &str) -> Option<String> {
    let prefix = format!("{}.", take_base_name(rules_file));
    let mut archives: Vec<(Option<std::time::SystemTime>, String)> = std::fs::read_dir(archive_dir)
        .ok()?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(&prefix) && !n.contains(".orig."))
        .map(|n| {
            let f = path_join(archive_dir, &n);
            (std::fs::metadata(&f).and_then(|m| m.modified()).ok(), f)
        })
        .collect();
    archives.sort_by_key(|(t, _)| *t);
    archives.pop().map(|(_, f)| f)
}

/// Save imported data in the archive directory, created if need be, as
/// `RULESNAME.DATE.EXT`: the data file's modification date (UTC) and
/// extension, or for generated data, today and `.csv` (saveToArchive,
/// archiveFileName).
fn save_to_archive(
    archive_dir: &str,
    rules_file: &str,
    data_file: Option<&str>,
    data: &str,
) -> Result<(), String> {
    let io_error =
        |f: &str, op: &str, e: std::io::Error| format!("{f}: {op}: {}", super::io_error_text(&e));
    std::fs::create_dir_all(archive_dir)
        .map_err(|e| io_error(archive_dir, "createDirectory", e))?;
    let base = take_base_name(rules_file);
    let name = match data_file {
        Some(f) => {
            let modified = std::fs::metadata(f)
                .and_then(|m| m.modified())
                .map_err(|e| io_error(f, "getModificationTime", e))?;
            let secs = modified
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs() as i64);
            let day = crate::dates::Day::from_ymd(1970, 1, 1)
                .expect("a valid date")
                .add_days(secs.div_euclid(86_400));
            format!("{base}.{day}{}", take_extension(f))
        }
        None => format!("{base}.{}.csv", crate::dates::current_day()),
    };
    let path = path_join(archive_dir, &name);
    eprintln!("archiving {path}");
    std::fs::write(&path, data).map_err(|e| io_error(&path, "withFile", e))
}

/// A path's last extension with its dot, or nothing (takeExtension).
fn take_extension(p: &str) -> &str {
    let name = p.rsplit('/').next().unwrap_or(p);
    name.rfind('.').map_or("", |i| &name[i..])
}

/// The files a source rule's pattern matches, its leading `~` expanded and
/// relative to a directory, oldest first (expandGlob, sortByModTime).
fn source_files(dir: &str, pattern: &str) -> Result<Vec<String>, String> {
    let expanded = crate::platform::expand_home_path(pattern)?;
    let dir = if expanded.starts_with('/') { "/" } else { dir };
    let mut found: Vec<(Option<std::time::SystemTime>, String)> =
        crate::platform::glob_matches(dir, &expanded)
            .into_iter()
            .map(|f| (std::fs::metadata(&f).and_then(|m| m.modified()).ok(), f))
            .collect();
    found.sort();
    Ok(found.into_iter().map(|(_, f)| f).collect())
}

/// The output of a shell command run in the rules file's directory, with
/// this text as its input if any; an error if it fails, showing its error
/// output. A data generating command's error output is shown as a warning
/// when it succeeds (runCommand, runCommandAsFilter).
fn run_command(rules_file: &str, cmd: &str, input: Option<&str>) -> Result<String, String> {
    use std::process::Stdio;
    let failed_pipes = || format!("in {rules_file}: failed to create pipes for command execution");
    let mut child = crate::platform::shell_command(cmd)
        .current_dir(take_directory(rules_file))
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::inherit()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| failed_pipes())?;
    let writer = match (input, child.stdin.take()) {
        (Some(text), Some(mut stdin)) => {
            let text = text.to_string();
            Some(std::thread::spawn(move || {
                let _ = std::io::Write::write_all(&mut stdin, text.as_bytes());
            }))
        }
        _ => None,
    };
    let output = child.wait_with_output().map_err(|_| failed_pipes())?;
    if let Some(w) = writer {
        let _ = w.join();
    }
    let out = String::from_utf8_lossy(&output.stdout).into_owned();
    let err = String::from_utf8_lossy(&output.stderr).into_owned();
    if !output.status.success() {
        let code = output
            .status
            .code()
            .unwrap_or_else(|| -crate::platform::exit_signal(output.status).unwrap_or(0));
        let detail = if err.is_empty() {
            String::new()
        } else {
            format!(":\n{err}")
        };
        return Err(format!(
            "in {rules_file}: command \"{cmd}\" failed with exit code {code}{detail}"
        ));
    }
    if input.is_none() && !err.is_empty() {
        crate::console::warn(&[], &err);
    }
    Ok(out)
}

/// An included CSV, SSV or TSV file's records converted by its own rules
/// file (FILE.rules, whatever `--rules` says), as an unfinalised journal
/// (CsvReader's rParser).
pub fn read_included_csv(iopts: &InputOpts, f: &str, sep: char) -> Result<Journal, String> {
    let rules_file = format!("{f}.rules");
    let (rules, _) = super::rules::read_rules(&rules_file)?;
    let text = read_data(f, &rules_file, &rules)?;
    Ok(read_journal_from_csv(iopts, &rules_file, &rules, f, &text, Some(sep))?.0)
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

/// A data file's text, or standard input's, decoded in the encoding the
/// rules name, or else as UTF-8 without a byte order mark; CRLF line endings
/// made LF (hGetContentsPortably).
fn read_data(f: &str, rules_file: &str, rules: &CsvRules) -> Result<String, String> {
    let encoding = rules_encoding(rules_file, rules)?;
    let Some(encoding) = encoding else {
        return if f == "-" {
            super::read_stdin_portably()
        } else {
            super::read_file_portably(f)
        };
    };
    let bytes = if f == "-" {
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut std::io::stdin(), &mut bytes)
            .map_err(|e| format!("<stdin>: hGetContents: {e}"))?;
        bytes
    } else {
        std::fs::read(f).map_err(|e| format!("{f}: openFile: {}", super::io_error_text(&e)))?
    };
    Ok(super::crlf_to_lf(&decode(encoding, &bytes)?))
}

/// A text encoding named by an encoding rule, as the encoding package
/// (which upstream uses) decodes it.
#[derive(Clone, Copy, Debug)]
enum TextEncoding {
    /// ASCII or ISO-8859-1: each byte is the character with its code.
    Latin1,
    /// UTF-8, decoded leniently: a sequence's leading byte decides its
    /// length, and the bits of the bytes after it are used, whatever they are.
    Utf8,
    /// UTF-16 or UTF-32: big-endian unless a byte order mark (which is
    /// dropped) says otherwise.
    Utf16,
    Utf32,
    /// An encoding as encoding_rs decodes it; with `true`, a single-byte
    /// one with extra characters in 0x80-0x9F, which are control characters
    /// in the ISO-8859 encoding wanted (windows-1252 for ISO-8859-1...).
    Rs(&'static encoding_rs::Encoding, bool),
}

/// The encoding an encoding rule names, if any; an unknown name is an error
/// (rulesEncoding).
fn rules_encoding(rules_file: &str, rules: &CsvRules) -> Result<Option<TextEncoding>, String> {
    let Some(name) = rules.directive("encoding") else {
        return Ok(None);
    };
    match encoding_from_name(name) {
        Some(Ok(e)) => Ok(Some(e)),
        Some(Err(())) => Err(format!(
            "{rules_file}: encoding {name} is not supported by hledgers yet"
        )),
        None => Err(format!("{rules_file}: Invalid encoding: {name}")),
    }
}

/// The encoding with this name, ignoring case and with runs of other
/// characters than ASCII letters and digits read as `_`; an error for one
/// that is known but not supported (encodingFromStringExplicit).
fn encoding_from_name(name: &str) -> Option<Result<TextEncoding, ()>> {
    use encoding_rs::*;
    let mut normal = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            normal.push(c.to_ascii_lowercase());
        } else if !normal.ends_with('_') || normal.is_empty() {
            normal.push('_');
        }
    }
    let rs = |e: &'static encoding_rs::Encoding| Some(Ok(TextEncoding::Rs(e, false)));
    let c1 = |e: &'static encoding_rs::Encoding| Some(Ok(TextEncoding::Rs(e, true)));
    match normal.as_str() {
        "ascii" | "646" | "ansi_x3_4_1968" | "cp367" | "csascii" | "ibm367" | "iso646_us"
        | "iso_ir_6" | "us" | "us_ascii" => Some(Ok(TextEncoding::Latin1)),
        "utf_8" | "u8" | "utf" | "utf8" | "utf8_ucs2" | "utf8_ucs4" => Some(Ok(TextEncoding::Utf8)),
        "utf_16" | "u16" | "utf16" => Some(Ok(TextEncoding::Utf16)),
        "utf_32" => Some(Ok(TextEncoding::Utf32)),
        "koi8_r" | "cskoi8r" => rs(KOI8_R),
        "koi8_u" => rs(KOI8_U),
        "iso_8859_1" | "iso8859_1" | "8859" | "cp819" | "csisolatin1" | "ibm819" | "iso8859"
        | "iso_8859_1_1987" | "iso_ir_100" | "l1" | "latin" | "latin1" => {
            Some(Ok(TextEncoding::Latin1))
        }
        "iso_8859_2" | "iso8859_2" | "csisolatin2" | "iso_8859_2_1987" | "iso_ir_101" | "l2"
        | "latin2" => rs(ISO_8859_2),
        "iso_8859_3" | "iso8859_3" | "csisolatin3" | "iso_8859_3_1988" | "iso_ir_109" | "l3"
        | "latin3" => rs(ISO_8859_3),
        "iso_8859_4" | "iso8859_4" | "csisolatin4" | "iso_8859_4_1988" | "iso_ir_110" | "l4"
        | "latin4" => rs(ISO_8859_4),
        "iso_8859_5" | "iso8859_5" | "csisolatincyrillic" | "cyrillic" | "iso_8859_5_1988"
        | "iso_ir_144" => rs(ISO_8859_5),
        "iso_8859_6" | "iso8859_6" | "arabic" | "asmo_708" | "csisolatinarabic" | "ecma_114"
        | "iso_8859_6_1987" | "iso_ir_127" => rs(ISO_8859_6),
        "iso_8859_7" | "iso8859_7" | "csisolatingreek" | "ecma_118" | "elot_928" | "greek"
        | "greek8" | "iso_8859_7_1987" | "iso_ir_126" => rs(ISO_8859_7),
        "iso_8859_8" | "iso8859_8" | "csisolatinhebrew" | "hebrew" | "iso_8859_8_1988"
        | "iso_ir_138" => rs(ISO_8859_8),
        "iso_8859_9" | "iso8859_9" | "csisolatin5" | "iso_8859_9_1989" | "iso_ir_148" | "l5"
        | "latin5" => c1(WINDOWS_1254),
        "iso_8859_10" | "iso8859_10" | "csisolatin6" | "iso_8859_10_1992" | "iso_ir_157" | "l6"
        | "latin6" => rs(ISO_8859_10),
        "iso_8859_11" | "iso8859_11" | "thai" | "iso_8859_11_2001" => c1(WINDOWS_874),
        "iso_8859_13" | "iso8859_13" => rs(ISO_8859_13),
        "iso_8859_14" | "iso8859_14" | "iso_8859_14_1998" | "iso_celtic" | "iso_ir_199" | "l8"
        | "latin8" => rs(ISO_8859_14),
        "iso_8859_15" | "iso8859_15" | "latin9" | "l9" => rs(ISO_8859_15),
        "iso_8859_16" | "iso8859_16" | "iso_8859_16_2001" | "iso_ir_226" | "l10" | "latin10" => {
            rs(ISO_8859_16)
        }
        "cp1250" | "windows_1250" => rs(WINDOWS_1250),
        "cp1251" | "windows_1251" => rs(WINDOWS_1251),
        "cp1252" | "windows_1252" => rs(WINDOWS_1252),
        "cp1253" | "windows_1253" => rs(WINDOWS_1253),
        "cp1254" | "windows_1254" => rs(WINDOWS_1254),
        "cp1255" | "windows_1255" => rs(WINDOWS_1255),
        "cp1256" | "windows_1256" => rs(WINDOWS_1256),
        "cp1257" | "windows_1257" => rs(WINDOWS_1257),
        "cp1258" | "windows_1258" => rs(WINDOWS_1258),
        "gb18030" | "gb18030_2000" => rs(GB18030),
        "macintosh" => rs(MACINTOSH),
        "iso_2022_jp" => rs(ISO_2022_JP),
        "shift_jis" | "sjis" | "cp932" => rs(SHIFT_JIS),
        "cp866" => rs(IBM866),
        "cp874" => rs(WINDOWS_874),
        "jis_x_0201" | "jis_x_0208" | "cp437" | "cp737" | "cp775" | "cp850" | "cp852" | "cp855"
        | "cp857" | "cp860" | "cp861" | "cp862" | "cp863" | "cp864" | "cp865" | "cp869" => {
            Some(Err(()))
        }
        _ => None,
    }
}

/// Bytes decoded in an encoding, or the encoding package's error.
fn decode(encoding: TextEncoding, b: &[u8]) -> Result<String, String> {
    let illegal = |byte: u8| format!("IllegalCharacter {byte}");
    let end = || "UnexpectedEnd".to_string();
    let from_code = |v: u32| char::from_u32(v).unwrap_or('\u{fffd}');
    match encoding {
        TextEncoding::Latin1 => Ok(b.iter().map(|&c| char::from(c)).collect()),
        TextEncoding::Utf8 => {
            let mut out = String::with_capacity(b.len());
            let mut i = 0;
            while i < b.len() {
                let w1 = b[i];
                let (n, lead) = match w1 {
                    0..=0x7f => (1, u32::from(w1)),
                    0x80..=0xbf => return Err(illegal(w1)),
                    0xc0..=0xdf => (2, u32::from(w1 & 0x1f)),
                    0xe0..=0xef => (3, u32::from(w1 & 0x0f)),
                    0xf0..=0xf7 => (4, u32::from(w1 & 0x07)),
                    _ => return Err(illegal(w1)),
                };
                let rest = b.get(i + 1..i + n).ok_or_else(end)?;
                let v = rest
                    .iter()
                    .fold(lead, |v, &w| (v << 6) | u32::from(w & 0x3f));
                if v > 0x10ffff {
                    return Err(format!("IllegalRepresentation {:?}", &b[i..i + n]));
                }
                out.push(from_code(v));
                i += n;
            }
            Ok(out)
        }
        TextEncoding::Utf16 => {
            let (be, start) = match b {
                [0xfe, 0xff, ..] => (true, 2),
                [0xff, 0xfe, ..] => (false, 2),
                _ => (true, 0),
            };
            let words: Vec<u16> = b[start..]
                .chunks(2)
                .map(|w| match w {
                    [x, y] if be => Ok(u16::from_be_bytes([*x, *y])),
                    [x, y] => Ok(u16::from_le_bytes([*x, *y])),
                    _ => Err(end()),
                })
                .collect::<Result<_, _>>()?;
            let mut out = String::new();
            let mut it = words.into_iter();
            while let Some(w1) = it.next() {
                match w1 {
                    0xd800..=0xdbff => {
                        let w2 = it.next().ok_or_else(end)?;
                        if !(0xdc00..=0xdfff).contains(&w2) {
                            return Err(illegal((w2 >> 8) as u8));
                        }
                        let v = ((u32::from(w1) & 0x3ff) << 10 | (u32::from(w2) & 0x3ff)) + 0x10000;
                        out.push(from_code(v));
                    }
                    0xdc00..=0xdfff => return Err(illegal((w1 >> 8) as u8)),
                    _ => out.push(from_code(u32::from(w1))),
                }
            }
            Ok(out)
        }
        TextEncoding::Utf32 => {
            let words: Vec<u32> = b
                .chunks(4)
                .map(|w| {
                    <[u8; 4]>::try_from(w)
                        .map(u32::from_be_bytes)
                        .map_err(|_| end())
                })
                .collect::<Result<_, _>>()?;
            let (le, words) = match words.first() {
                Some(0x0000_feff) => (false, &words[1..]),
                Some(0xfffe_0000) => (true, &words[1..]),
                _ => (false, &words[..]),
            };
            words
                .iter()
                .map(|&w| {
                    let v = if le { w.swap_bytes() } else { w };
                    char::from_u32(v).ok_or_else(|| "Prelude.chr: bad argument".to_string())
                })
                .collect()
        }
        TextEncoding::Rs(e, c1_controls) => {
            let (text, _) = e.decode_without_bom_handling(b);
            if !c1_controls {
                return Ok(text.into_owned());
            }
            // a single-byte encoding: one character per byte
            Ok(b.iter()
                .zip(text.chars())
                .map(|(&byte, c)| {
                    if (0x80..0xa0).contains(&byte) {
                        char::from(byte)
                    } else {
                        c
                    }
                })
                .collect())
        }
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

/// CSV data converted by rules to transactions, as an unfinalised journal,
/// with what errors about them need (readJournalFromCsv). Blank lines are
/// ignored and a top-level skip rule skips lines; skip, end, merge and pair
/// rules apply to records; the transactions are put in date order, guessing the
/// data's order. `sep` is the separator a reader prefix implies, if any.
pub fn read_journal_from_csv(
    iopts: &InputOpts,
    rules_file: &str,
    rules: &CsvRules,
    csv_file: &str,
    text: &str,
    sep: Option<char>,
) -> Result<(Journal, CsvContext), String> {
    let lines: Vec<(usize, &str)> = hs_lines(text)
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
        .and_then(|gs| apply_pair_rules(rules, gs))
        .map_err(|e| format!("{rules_file}: {e}"))?;
    validate_csv(name, &groups)?;
    // characters journal format can't represent are replaced, with a warning
    let warn = |msg: &str| crate::console::warn(&iopts.prog_args, msg);
    for (field, bad, replacement) in [("description", ';', ".,"), ("code", ')', "]")] {
        let n = groups
            .iter()
            .filter(|(_, g)| {
                rules
                    .field_value(g, field)
                    .is_ok_and(|v| v.is_some_and(|v| v.contains(bad)))
            })
            .count();
        if n > 0 {
            warn(&format!(
                "{csv_file}: replaced '{bad}' with '{replacement}' in {n} {field}(s), since journal format can't represent it"
            ));
        }
    }
    // a merge or pair rule's if block matching on a later row's field can
    // never merge or pair
    for (rule, action) in [("merge", "merging"), ("pair", "pairing")] {
        let refs: Vec<&str> = rules
            .blocks
            .iter()
            .filter(|b| b.assignments.iter().any(|a| a.name == rule))
            .flat_map(|b| b.matchers.iter().filter_map(|m| m.field.as_deref()))
            .filter(|r| split_row_suffix(r.trim_matches(|c| "%()".contains(c))).is_some())
            .collect();
        if !refs.is_empty() {
            warn(&format!(
                "{rules_file}: an if block containing a {rule} rule matches on a later row's field ({}); this can not trigger a {rule}, since these fields are empty until after {action}",
                refs.join(", ")
            ));
        }
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
    // a posting can be made only if a rule assigns its account, amount or balance
    let postings: Vec<usize> = (1..=MAX_POSTINGS)
        .filter(|n| {
            let numbered = ["account", "amount", "amount", "amount", "balance"]
                .iter()
                .zip(["", "", "-in", "-out", ""])
                .any(|(f, s)| rules.assigns(&format!("{f}{n}{s}")));
            let unnumbered = (*n <= 2
                && ["amount", "amount-in", "amount-out"]
                    .iter()
                    .any(|f| rules.assigns(f)))
                || (*n == 1 && rules.assigns("balance"));
            numbered || unnumbered
        })
        .collect();
    let conv = Converter {
        rules,
        zoned,
        tz_in,
        tz_out,
        postings,
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

/// The groups after the pair rules apply (this repo's extension). A group
/// whose pair rule applies, the initiator, takes as its later rows the first
/// other group in the file, before or after it, that has no pair rule of its
/// own, is not paired already, and for which the initiator's key template
/// renders the same text; that group then makes no transaction of its own.
/// An initiator without a partner stays as it is, its later rows' references
/// empty. The group keeps the initiator's lines.
fn apply_pair_rules(
    rules: &CsvRules,
    groups: Vec<LocatedGroup>,
) -> Result<Vec<LocatedGroup>, String> {
    let rule: Vec<Option<(&FieldAssignment, Scope)>> = groups
        .iter()
        .map(|(_, g)| rules.field_assignments(g, "pair").pop())
        .collect();
    if rule.iter().all(Option::is_none) {
        return Ok(groups);
    }
    // the possible partners' keys, per distinct key template
    let mut keys: BTreeMap<&str, Vec<Option<String>>> = BTreeMap::new();
    for (a, scope) in rule.iter().flatten() {
        if a.template.text.trim().is_empty() {
            return Err(
                "pair rule needs a value: the fields that identify the other record of the pair, eg \"pair %date %time\""
                    .to_string(),
            );
        }
        if !keys.contains_key(a.template.text.as_str()) {
            let rendered = groups
                .iter()
                .zip(&rule)
                .map(|((_, g), r)| match r {
                    Some(_) => Ok(None),
                    None => rules.render_assignment(g, a, *scope).map(Some),
                })
                .collect::<Result<Vec<_>, String>>()?;
            keys.insert(&a.template.text, rendered);
        }
    }
    let mut partner: Vec<Option<usize>> = vec![None; groups.len()];
    let mut used = vec![false; groups.len()];
    for (i, (a, scope)) in rule
        .iter()
        .enumerate()
        .filter_map(|(i, r)| r.map(|r| (i, r)))
    {
        let key = rules.render_assignment(&groups[i].1, a, scope)?;
        let candidates = &keys[a.template.text.as_str()];
        if let Some(j) =
            (0..groups.len()).find(|&j| !used[j] && candidates[j].as_deref() == Some(key.as_str()))
        {
            used[j] = true;
            partner[i] = Some(j);
        }
    }
    let mut rows: Vec<Option<Vec<CsvRecord>>> =
        groups.iter().map(|(_, g)| Some(g.clone())).collect();
    for (i, j) in partner.iter().enumerate() {
        if let Some(j) = j {
            let p = rows[*j].take().unwrap_or_default();
            rows[i].get_or_insert_with(Vec::new).extend(p);
        }
    }
    Ok(groups
        .iter()
        .zip(rows)
        .enumerate()
        .filter(|(k, _)| !used[*k])
        .filter_map(|(_, ((lines, _), r))| r.map(|r| (*lines, r)))
        .collect())
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
            crate::model::errors::csv_record_error_prefix(
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
    /// The numbers of the postings that rules could make.
    postings: Vec<usize>,
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
        let errpfx = crate::model::errors::csv_record_error_prefix(
            &pos.0.file,
            pos.0.line,
            pos.1.line,
            &record_as_approximate_text(record),
        );
        let field = |f: &str| {
            rules
                .field_value(record, f)
                .map_err(|e| self.template_error(&errpfx, record, &e))
        };
        let date_format = rules.directive("date-format");
        let parse_date =
            |s: &str| parse_date_with_format(self.zoned, self.tz_in, self.tz_out, date_format, s);
        let date_s = field("date")?.unwrap_or_default();
        let date =
            parse_date(&date_s).ok_or_else(|| self.date_error(&errpfx, record, "date", &date_s))?;
        let date2 = match field("date2")? {
            Some(s) => {
                Some(parse_date(&s).ok_or_else(|| self.date_error(&errpfx, record, "date2", &s))?)
            }
            None => None,
        };
        let status = match field("status")? {
            Some(s) => parse_status(&s).map_err(|e| {
                format!(
                    "{errpfx}could not parse status value \"{s}\" (should be *, ! or empty)\nthe parse error is:      {e}\n"
                )
            })?,
            None => Status::Unmarked,
        };
        let code = field("code")?.map_or(String::new(), |s| single_line(&s).replace(')', "]"));
        let description =
            field("description")?.map_or(String::new(), |s| single_line(&s).replace(';', ".,"));
        let comment = field("comment")?.map_or(String::new(), |s| unescape_newlines(&s));
        let tags = transaction_comment_tags(&comment);
        let p1_virtual =
            field("account1")?.is_some_and(|a| account_name_posting_type(&a) == Realness::Virtual);
        let mut postings = Vec::new();
        for &n in &self.postings {
            let comment =
                field(&format!("comment{n}"))?.map_or(String::new(), |s| unescape_newlines(&s));
            let (ptags, pdate) = posting_comment_tags_and_date(&comment);
            let currency = match field(&format!("currency{n}"))? {
                Some(c) => c,
                None => field("currency")?.unwrap_or_default(),
            };
            let account = field(&format!("account{n}"))?.map(|a| strip(&a).to_string());
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
                    return Err(crate::console::usage_error(&e.pretty("", a)));
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

    /// The error for a template function that can not make a value: split's
    /// arguments that are not an amount, weights or a part number.
    fn template_error(&self, errpfx: &str, record: &[CsvRecord], msg: &str) -> String {
        format!(
            "{errpfx}{msg}\n{}\n{}",
            self.rules.show_record_fields(record),
            self.rules.show_rules(record)
        )
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
        let mut values: Vec<(String, String)> = Vec::new();
        for f in names {
            let v = rules
                .field_value(record, &f)
                .map_err(|e| self.template_error(errpfx, record, &e))?;
            if let Some(v) = v.map(|v| strip(&v).to_string()).filter(|v| !v.is_empty()) {
                values.push((f, v));
            }
        }
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
        let field = |f: &str| {
            rules
                .field_value(record, f)
                .map_err(|e| self.template_error(errpfx, record, &e))
        };
        let value = match field(&format!("balance{n}"))? {
            Some(v) => Some(v),
            None if n == 1 => field("balance")?,
            None => None,
        };
        let Some(s) = value.map(|v| strip(&v).to_string()) else {
            return Ok(None);
        };
        if s.is_empty() {
            return Ok(None);
        }
        match self.run_amount_parser(errpfx, record, currency, &simplify_sign(&s), false)? {
            Ok(a) => Ok(Some(a)),
            Err(e) => Err(format!(
                "{errpfx}could not parse \"{s}\" as balance{n} amount\n{}\n{}\nthe parse error is:      {e}\n",
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
        let parse = |default: Option<&(Sym, AmountStyle)>| {
            parse_csv_amount(text, expr, decimal_mark, default)
        };
        if currency.is_empty() {
            return Ok(parse(None));
        }
        match parse_currency(currency, decimal_mark) {
            Some(default) => Ok(parse(Some(&default))),
            None => match parse(None) {
                Ok(a) if !a.commodity.is_empty() => Ok(Ok(a)),
                _ => Err(format!(
                    "{errpfx}could not parse \"{currency}\" as a currency\n{}\n{}\na currency rule's value should be a commodity symbol, like USD or \"Some Fund\",\nor an amount showing the symbol's position, like 1 USD\n",
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
/// A currency rule's value as a default commodity and style: an amount such
/// as `1 USD` or `USD 1`, or a commodity symbol, placed on the left with no
/// space (parseCurrency).
pub fn parse_currency(currency: &str, decimal_mark: Option<char>) -> Option<(Sym, AmountStyle)> {
    let as_default = |t: &str| {
        let ctx = AmountCtx {
            decimal_mark,
            ..AmountCtx::default()
        };
        match run_parser(t, |c| amountp(c, &ctx, AmountKind::Style)) {
            Ok(a) if !a.commodity.is_empty() => Some((a.commodity, a.style)),
            _ => None,
        }
    };
    as_default(currency).or_else(|| as_default(&format!("{currency}1")))
}

/// An amount, or an amount expression, parsed from all of a text with this
/// decimal mark and this default commodity for an amount without one; or
/// the parse error as megaparsec shows it (runCsvAmountParser's parse).
pub fn parse_csv_amount(
    text: &str,
    expr: bool,
    decimal_mark: Option<char>,
    default: Option<&(Sym, AmountStyle)>,
) -> Result<crate::amount::Amount, String> {
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
}

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
    hs_lines(s)
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
    let s: String = hs_lines(c).map(|l| format!(" ;{l}\n")).collect();
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

    #[test]
    fn encodings_as_the_encoding_package_decodes_them() {
        let enc = |n: &str| encoding_from_name(n).unwrap().unwrap();
        // UTF-8 is decoded leniently: here e9 63 6c is one character
        assert_eq!(decode(enc("UTF-8"), b"\xe9clair").unwrap(), "\u{98ec}air");
        assert_eq!(
            decode(enc("utf8"), b"\x80").unwrap_err(),
            "IllegalCharacter 128"
        );
        assert_eq!(
            decode(enc("iso8859-1"), b"\xe9\x85").unwrap(),
            "\u{e9}\u{85}"
        );
        assert_eq!(decode(enc("ascii"), b"\xe9").unwrap(), "\u{e9}");
        assert_eq!(decode(enc("latin5"), b"\x80\xfd").unwrap(), "\u{80}\u{131}");
        assert_eq!(decode(enc("cp1252"), b"\x80").unwrap(), "\u{20ac}");
        assert_eq!(decode(enc("utf-16"), b"\xff\xfea\x00").unwrap(), "a");
        assert_eq!(decode(enc("UTF16"), b"\x00a").unwrap(), "a");
        assert_eq!(decode(enc("utf_32"), b"\x00\x00\x00a").unwrap(), "a");
        assert!(encoding_from_name("cp437").unwrap().is_err());
        assert!(encoding_from_name("nosuch").is_none());
    }

    // as filepath's takeBaseName and takeExtension
    #[test]
    fn base_names_and_extensions() {
        for (p, base, ext) in [
            ("rules/bank.rules", "bank", ".rules"),
            ("data/x.tar.gz", "x.tar", ".gz"),
            ("data/noext", "noext", ""),
            ("data/.csv", "", ".csv"),
            ("/a.b/c", "c", ""),
        ] {
            assert_eq!((take_base_name(p), take_extension(p)), (base, ext), "{p}");
        }
    }
}
