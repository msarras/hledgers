//! Reading journals (Hledger.Read and its submodules): the input options,
//! and the parsers for amounts and journal files.

pub mod amount;
pub mod balancing;
pub mod checks;
pub mod common;
pub mod costs;
pub mod csv;
pub mod errors;
pub mod finalise;
pub mod journal;
pub mod modifiers;
pub mod rules;
pub mod timeclock;
pub mod timedot;

use crate::amount::CommodityStyles;
use crate::dates::{DateSpan, Day};
use crate::model::Journal;

/// How transactions are balanced (BalancingOpts). The last five fields are
/// set from the journal when it is finalised.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BalancingOpts {
    /// Don't check balance assertions (`-I`).
    pub ignore_assertions: bool,
    /// May costs be inferred to balance transactions? (Not with `--strict`
    /// or `check balanced`.)
    pub infer_balancing_costs: bool,
    /// Commodity display styles given on the command line (`-c`).
    pub commodity_styles: Option<CommodityStyles>,
    /// The precision transactions must balance to: each entry's own
    /// (`entry`), or the display precision (`display`) (`--txn-tolerance`).
    pub txn_balancing_display: bool,
    /// Show tolerated imbalances as imbalance postings (`--infer-imbalance`).
    pub infer_imbalance: bool,
    pub imbalance_account: String,
    pub account_types:
        std::collections::BTreeMap<crate::account::AccountName, crate::account::AccountType>,
    pub lotful_commodities: std::collections::BTreeSet<crate::amount::Sym>,
    pub account_lots_tags: std::collections::BTreeMap<crate::account::AccountName, String>,
    pub lenient_lots: bool,
    pub verbose_balancing_tags: bool,
}

impl Default for BalancingOpts {
    /// defbalancingopts.
    fn default() -> BalancingOpts {
        BalancingOpts {
            ignore_assertions: false,
            infer_balancing_costs: true,
            commodity_styles: None,
            txn_balancing_display: false,
            infer_imbalance: false,
            imbalance_account: crate::account::DEFAULT_IMBALANCE_ACCOUNT.to_string(),
            account_types: Default::default(),
            lotful_commodities: Default::default(),
            account_lots_tags: Default::default(),
            lenient_lots: false,
            verbose_balancing_tags: false,
        }
    }
}

/// Options for reading journals (InputOpts).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputOpts {
    /// A file format to read, unless a file says otherwise.
    pub format: Option<String>,
    /// A CSV rules file to use (`--rules`).
    pub rules_file: Option<String>,
    /// Account aliases from `--alias` options, as written.
    pub aliases: Vec<String>,
    /// Obfuscate the data (`--obfuscate`).
    pub anon: bool,
    /// Read only transactions newer than last time (`--new`).
    pub new: bool,
    /// Save the latest dates read, with `new`.
    pub new_save: bool,
    /// Use this field's value as the account name (`--pivot`).
    pub pivot: String,
    /// The span in which to generate forecast transactions (`--forecast`).
    pub forecast: Option<DateSpan>,
    /// Add visible tags to generated or modified data (`--verbose-tags`).
    pub verbose_tags: bool,
    /// The report's dates, from the query (used by forecasting).
    pub report_span: DateSpan,
    /// Apply auto posting rules (`--auto`).
    pub auto: bool,
    /// Infer equity postings from costs (`--infer-equity`).
    pub infer_equity: bool,
    /// Infer costs from equity postings (`--infer-costs`).
    pub infer_costs: bool,
    /// Skip lot tracking and its checks (`--ignore-lots`).
    pub ignore_lots: bool,
    pub balancing: BalancingOpts,
    /// Do the strict checks (`--strict`).
    pub strict: bool,
    /// Postpone checks, while several files are read.
    pub defer: bool,
    /// The import command is reading.
    pub importing: bool,
    /// `import --dry-run`.
    pub dry_run: bool,
    /// Today's date.
    pub today: Day,
    /// Pair timeclock entries the old way (`--old-timeclock`).
    pub old_timeclock: bool,
    /// The main journal file's directory.
    pub journal_dir: Option<String>,
    /// The program's command-line arguments, which can ask for checks
    /// that are done while reading (progArgs).
    pub prog_args: Vec<String>,
}

impl InputOpts {
    /// Whether the command line is a check command naming this check
    /// (journalFinalise's checking).
    pub fn checking(&self, name: &str) -> bool {
        self.prog_args.iter().any(|a| a == "check") && self.prog_args.iter().any(|a| a == name)
    }
}

impl Default for InputOpts {
    /// definputopts.
    fn default() -> InputOpts {
        InputOpts {
            format: None,
            rules_file: None,
            aliases: Vec::new(),
            anon: false,
            new: false,
            new_save: true,
            pivot: String::new(),
            forecast: None,
            verbose_tags: false,
            report_span: DateSpan::default(),
            auto: false,
            infer_equity: false,
            infer_costs: false,
            ignore_lots: false,
            balancing: BalancingOpts::default(),
            strict: false,
            defer: false,
            importing: false,
            dry_run: false,
            today: crate::dates::null_date(),
            old_timeclock: false,
            journal_dir: None,
            prog_args: Vec::new(),
        }
    }
}

/// Read, finalise and combine journal files, each perhaps with a reader
/// prefix (`journal:`, `csv:`...) or `-` for standard input
/// (readJournalFiles).
pub fn read_journal_files(iopts: &InputOpts, files: &[String]) -> Result<Journal, String> {
    let iopts = InputOpts {
        defer: true,
        ..iopts.clone()
    };
    let mut journals = Vec::new();
    for f in files {
        journals.push(read_journal_file(&iopts, f)?);
    }
    let declared: Vec<_> = journals
        .iter()
        .map(|j| {
            (
                j.declared_commodities.clone(),
                j.declared_commodity_tags.clone(),
            )
        })
        .collect();
    let mut j = journals
        .into_iter()
        .reduce(finalise::journal_concat)
        .unwrap_or_default();
    // a commodity declared in several files: the last file's declaration is preferred
    // (combineCommodityDeclarations)
    if declared.len() > 1 {
        j.declared_commodities.clear();
        j.declared_commodity_tags.clear();
        for (commodities, tags) in declared.into_iter().rev() {
            for (c, d) in commodities {
                let combined = match j.declared_commodities.remove(&c) {
                    Some(d0) => finalise::commodity_combine(d0, d),
                    None => d,
                };
                j.declared_commodities.insert(c, combined);
            }
            for (c, ts) in tags {
                let existing = j.declared_commodity_tags.entry(c).or_default();
                for t in ts {
                    if !existing.contains(&t) {
                        existing.push(t);
                    }
                }
            }
        }
    }
    finalise::journal_number_and_tie_transactions(&mut j);
    if iopts.strict {
        checks::journal_strict_checks(&j)?;
    }
    Ok(j)
}

/// Read and finalise one journal file (readJournalFile). Its format comes
/// from a reader prefix, or the input options, or its file extension, and
/// is otherwise journal.
pub fn read_journal_file(iopts: &InputOpts, prefixed: &str) -> Result<Journal, String> {
    let (prefix, f) = journal::split_reader_prefix(prefixed);
    if f != "-" && !file_exists(f) {
        return Err(format!(
            "data file \"{f}\" was not found.\n\
             Please create it first, eg with \"hledger add\" or a text editor.\n\
             Or, specify an existing data file with -f or $LEDGER_FILE.\n"
        ));
    }
    let format = prefix
        .or(iopts.format.as_deref())
        .unwrap_or_else(|| format_from_extension(f));
    let read = match format {
        "journal" | "timeclock" | "timedot" => None,
        "csv" => Some(csv::read_csv_file(iopts, f, ',')),
        "ssv" => Some(csv::read_csv_file(iopts, f, ';')),
        "tsv" => Some(csv::read_csv_file(iopts, f, '\t')),
        "rules" => Some(csv::read_rules_file(iopts, f)),
        _ => {
            return Err(format!(
                "reading {format} files is not supported yet (hledgers milestone 7): {f}"
            ));
        }
    };
    if let Some(j) = read {
        return j.map_err(with_decoding_hint);
    }
    let text = if f == "-" {
        read_stdin_portably()
    } else {
        read_file_portably(f)
    }
    .map_err(with_decoding_hint)?;
    match format {
        "timeclock" => return timeclock::read_timeclock(iopts, f, &text),
        "timedot" => return timedot::read_timedot(iopts, f, &text),
        _ => {}
    }
    let (j, st) = journal::parse_journal(iopts, f, &text)?;
    finalise::journal_finalise(iopts, f, &text, j, st)
}

/// Whether a file exists and is not a directory (doesFileExist).
pub fn file_exists(f: &str) -> bool {
    std::fs::metadata(f).is_ok_and(|m| !m.is_dir())
}

/// The format a file extension suggests, journal by default (findReader).
fn format_from_extension(f: &str) -> &'static str {
    let ext = std::path::Path::new(f)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "timeclock" => "timeclock",
        "timedot" => "timedot",
        "csv" => "csv",
        "ssv" => "ssv",
        "tsv" => "tsv",
        "rules" => "rules",
        _ => "journal",
    }
}

/// A file's text, decoded as UTF-8, without a byte order mark, with CRLF
/// line endings made LF (readFilePortably); or the error as GHC shows it,
/// after the file's name and the operation that failed.
pub fn read_file_portably(f: &str) -> Result<String, String> {
    let bytes = std::fs::read(f).map_err(|e| format!("{f}: openFile: {}", io_error_text(&e)))?;
    decode_portably(bytes).map_err(|e| format!("{f}: hGetContents: {e}"))
}

/// Standard input's text, as [`read_file_portably`] reads a file's.
pub fn read_stdin_portably() -> Result<String, String> {
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut std::io::stdin(), &mut bytes)
        .map_err(|e| format!("<stdin>: hGetContents: {e}"))?;
    decode_portably(bytes).map_err(|e| format!("<stdin>: hGetContents: {e}"))
}

/// Bytes decoded as UTF-8, without a byte order mark, with CRLF line
/// endings made LF; or GHC's error about the first byte that can not be
/// decoded (hGetContentsPortably).
fn decode_portably(bytes: Vec<u8>) -> Result<String, String> {
    let text = String::from_utf8(bytes).map_err(|e| {
        let b = e.as_bytes()[e.utf8_error().valid_up_to()];
        format!("invalid argument (cannot decode byte sequence starting from {b})")
    })?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    Ok(crlf_to_lf(text))
}

/// Text with its CRLF line endings made LF.
fn crlf_to_lf(text: &str) -> String {
    if text.contains("\r\n") {
        text.replace("\r\n", "\n")
    } else {
        text.to_string()
    }
}

/// An error reading a file, with the advice upstream's top-level handler
/// adds when text could not be decoded (exitUnicode).
pub fn with_decoding_hint(msg: String) -> String {
    if msg.contains("cannot decode byte sequence") {
        format!(
            "{msg}\nSome text could not be decoded with the system's text encoding, UTF-8\n\
             (or, the text encoding specified by CSV rules).\n\
             Please either convert the text to this encoding,\n\
             or configure a system locale which can decode this text."
        )
    } else {
        msg
    }
}

/// An I/O error as GHC shows it, after the file name and operation.
pub fn io_error_text(e: &std::io::Error) -> String {
    use std::io::ErrorKind::*;
    match e.kind() {
        NotFound => "does not exist (No such file or directory)".to_string(),
        PermissionDenied => "permission denied (Permission denied)".to_string(),
        IsADirectory => "inappropriate type (is a directory)".to_string(),
        _ => e.to_string(),
    }
}
