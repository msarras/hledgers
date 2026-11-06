//! Command-line options (Hledger.Cli.CliOptions): the general flags every
//! command accepts, and CliOpts, the options of a command run.

use super::args::{Arity, Flag, RawOpts};
use crate::amount::CommodityStyles;
use crate::dates::{DateSpan, Day, Interval, fix_smart_date_str_ef, parse_period_expr};
use crate::parse::{PResult, ParseError, Parser};
use crate::read::{BalancingOpts, InputOpts};
use crate::reports::opts::{ReportOpts, ReportSpec, usage_error};
use crate::valuation::ConversionOp;

const NONE: Arity = Arity::None;
const REQ: Arity = Arity::Req;
const OPT: Arity = Arity::Opt;

/// The general input flags (inputflags).
pub const INPUT_FLAGS: &[Flag] = &[
    Flag::new(&["file"], REQ, "[FMT:]FILE", "Use this as the journal file (- means stdin). If not specified, $LEDGER_FILE or ~/.hledger.journal will be used. If specified more than once, the files will be read in order. Each file's format (journal, csv, timeclock, timedot, rules..) is inferred from the file extension or a FMT: prefix. Some commands (add, import) write to the (first) file, and expect it to be in journal format.").short(&['f']),
    Flag::new(&["rules"], REQ, "RULESFILE", "Use rules defined in this rules file for converting subsequent CSV/SSV/TSV files. If not specified, uses FILE.csv.rules for each FILE.csv."),
    Flag::new(&["alias"], REQ, "A=B|/RGX/=RPL", "transform account names from A to B, or by replacing regular expression matches"),
    Flag::new(&["auto"], NONE, "", "generate extra postings by applying auto posting rules (\"=\") to all transactions"),
    Flag::new(&["forecast"], OPT, "PERIOD", "Generate extra transactions from periodic rules (\"~\"), from after the latest ordinary transaction until 6 months from now. Or, during the specified PERIOD (the equals is required). Auto posting rules will also be applied to these transactions. In hledger-ui, also make future-dated transactions visible at startup."),
    Flag::new(&["ignore-assertions"], NONE, "", "don't check balance assertions by default"),
    Flag::new(&["ignore-lots"], NONE, "", "don't track lots: skip lot checks, inferred cost bases and gain postings"),
    Flag::new(&[], NONE, "", "shortcut for --ignore-assertions --ignore-lots").short(&['I']).sets(&["ignore-assertions", "ignore-lots"]),
    Flag::new(&["txn-tolerance"], REQ, "...", "how much imbalance to allow in transactions:\n'entry':   - the entry's precision (default)\n'display': - display precision (legacy)\n"),
    Flag::new(&["infer-costs"], NONE, "", "infer costs from conversion equity postings"),
    Flag::new(&["infer-equity"], NONE, "", "infer conversion equity postings from costs"),
    Flag::new(&["infer-imbalance"], NONE, "", "add postings showing tolerated imbalances"),
    Flag::new(&["infer-market-prices"], NONE, "", "infer market prices from costs"),
    Flag::new(&["pivot"], REQ, "TAGNAME", "use a different field or tag as account names"),
    Flag::new(&["strict"], NONE, "", "do extra error checks (and override -I)").short(&['s']),
];

/// The general output flags (reportflags).
pub const REPORT_FLAGS: &[Flag] = &[
    Flag::new(&["begin"], REQ, "DATE", "include postings/transactions on/after this date").short(&['b']),
    Flag::new(&["end"], REQ, "DATE", "include postings/transactions before this date (with a report interval, will be adjusted to following subperiod end)").short(&['e']),
    Flag::new(&["daily"], NONE, "", "set report interval: 1 day").short(&['D']),
    Flag::new(&["weekly"], NONE, "", "set report interval: 1 week").short(&['W']),
    Flag::new(&["monthly"], NONE, "", "set report interval: 1 month").short(&['M']),
    Flag::new(&["quarterly"], NONE, "", "set report interval: 1 quarter").short(&['Q']),
    Flag::new(&["yearly"], NONE, "", "set report interval: 1 year").short(&['Y']),
    Flag::new(&["period"], REQ, "PERIODEXP", "set begin date, end date, and/or report interval, with more flexibility").short(&['p']),
    Flag::new(&["today"], REQ, "DATE", "override today's date (affects relative dates)"),
    Flag::new(&["date2"], NONE, "", "match/use secondary dates instead (deprecated)"),
    Flag::new(&["unmarked"], NONE, "", "include only unmarked postings/transactions").short(&['U']),
    Flag::new(&["pending"], NONE, "", "include only pending postings/transactions").short(&['P']),
    Flag::new(&["cleared"], NONE, "", "include only cleared postings/transactions\n(-U/-P/-C can be combined)").short(&['C']),
    Flag::new(&["real"], NONE, "", "include only non-virtual postings").short(&['R']),
    Flag::new(&["empty"], NONE, "", "Show zero items, which are normally hidden.\nIn hledger-ui & hledger-web, do the opposite.").short(&['E']),
    Flag::new(&["depth"], REQ, "DEPTHEXP", "if a number (or -NUM): show only top NUM levels of accounts. If REGEXP=NUM, only apply limiting to accounts matching the regular expression."),
    Flag::new(&["cost"], NONE, "", "convert amounts to their cost basis ({}), or else their cost/sale amount (@/@@)").short(&['B']).sets(&["B"]),
    Flag::new(&["market"], NONE, "", "valuation mode: show amounts converted to market value at period end(s) in their default valuation commodity. Short for --value=end.").short(&['V']).sets(&["V"]),
    Flag::new(&["exchange"], REQ, "COMM", "valuation mode: show amounts converted to market value at period end(s) in the specified commodity. Short for --value=end,COMM.").short(&['X']).sets(&["X"]),
    Flag::new(&["value"], REQ, "WHEN[,COMM]", "valuation mode: show amounts converted to market value on the specified date(s) in their default valuation commodity or a specified commodity. WHEN can be:\n'then':     value on transaction dates\n'end':      value at period end(s)\n'now':      value today\nYYYY-MM-DD: value on given date\n'cost':     cost basis, or else transacted cost (same as -B)\n'transacted': transacted cost/sale amount (@/@@) only\nCOMM may be written in double quotes; '\"\"' or an empty COMM means the no-symbol commodity.\n"),
    Flag::new(&["lots"], NONE, "", "show lot subaccounts and other lot details"),
    Flag::new(&["commodity-style"], REQ, "S", "Override a commodity's display style.\nEg: -c '$1000.' or -c '1.000,00 EUR'").short(&['c']),
    Flag::new(&["pretty"], OPT, "YN", "Use box-drawing characters in text output? The optional 'y'/'yes' or 'n'/'no' arg requires =.").default("yes"),
    Flag::new(&["title"], REQ, "T", "set or customise a report title"),
    Flag::new(&["subreport-titles"], REQ, "TS", "customise subreport headings in compound reports (|-separated)"),
    Flag::new(&["period-titles"], REQ, "OPT", "customise headings in periodic reports:\n'compact': readable period names when possible\n'dates':   exact dates/date ranges always\n"),
];

/// The general help flags, and the terminal flags (helpflags).
pub const HELP_FLAGS: &[Flag] = &[
    Flag::new(&[], NONE, "", "show the hledger quick reference")
        .short(&['?'])
        .sets(&["quickref"]),
    Flag::new(&["help"], NONE, "", "show this command's usage help").short(&['h']),
    Flag::new(&["info"], NONE, "", "show this command's manual with info"),
    Flag::new(&["man"], NONE, "", "show this command's manual with man"),
    Flag::new(
        &["webman"],
        NONE,
        "",
        "show this command's manual on the web",
    ),
    Flag::new(&["examples"], NONE, "", "show examples for this command"),
    Flag::new(&["version"], NONE, "", "show version information"),
    Flag::new(
        &["test"],
        OPT,
        "PATTERN",
        "run built-in tests (those matching PATTERN) and exit",
    ),
    Flag::new(
        &["write-lang"],
        REQ,
        "LANG",
        "write LANG's PO file to the config directory, for editing translations, and exit",
    ),
    Flag::new(
        &["debug"],
        REQ,
        "[1-9]",
        "show this much debug output (default: 1)",
    ),
    Flag::new(
        &["pager"],
        REQ,
        "YN",
        "use a pager when needed ? y/yes (default) or n/no",
    ),
    Flag::new(
        &["color", "colour"],
        REQ,
        "YNA",
        "use ANSI color ? y/yes, n/no, or auto (default)",
    ),
    Flag::new(
        &["lang"],
        REQ,
        "LANG",
        "select the user interface language:\nen (default), de, es, fr, ga, haw, it, ja, pt-BR, ru, uk, zh, or auto",
    ),
];

/// Legacy flags, accepted but not shown in help (hiddenflagsformainmode).
pub const LEGACY_FLAGS: &[Flag] = &[
    Flag::new(
        &["effective", "aux-date"],
        NONE,
        "",
        "Ledger-compatible aliases for --date2",
    )
    .sets(&["date2"])
    .hidden(),
    Flag::new(&["infer-value"], NONE, "", "legacy flag that was renamed")
        .sets(&["infer-market-prices"])
        .hidden(),
    Flag::new(&["pretty-tables"], NONE, "", "legacy flag that was renamed")
        .sets(&["pretty"])
        .default("always")
        .hidden(),
    Flag::new(&["anon"], NONE, "", "deprecated, renamed to --obfuscate").hidden(),
    Flag::new(
        &["obfuscate"],
        NONE,
        "",
        "slightly obfuscate hledger's output. Warning, does not give privacy. Formerly --anon.",
    )
    .hidden(),
    Flag::new(
        &["old-timeclock", "timeclock-old"],
        NONE,
        "",
        "don't pair timeclock entries by account name",
    )
    .sets(&["oldtimeclock"])
    .hidden(),
    Flag::new(
        &["old-glob"],
        NONE,
        "",
        "deprecated, no longer used as of 1.50.4",
    )
    .sets(&["oldglob"])
    .hidden(),
    Flag::new(&["rules-file"], REQ, "RULESFILE", "was renamed to --rules")
        .sets(&["rules"])
        .hidden(),
    Flag::new(
        &["txn-balancing"],
        REQ,
        "...",
        "was renamed to --txn-tolerance",
    )
    .hidden(),
];

/// The config file flags (confflags): shown in the main help, hidden in
/// commands'.
pub const CONF_FLAGS: &[Flag] = &[
    Flag::new(
        &["conf"],
        REQ,
        "CONFFILE",
        "Use extra options defined in this config file. If not specified, searches upward and in XDG config dir for hledger.conf (or .hledger.conf in $HOME).",
    ),
    Flag::new(&["no-conf"], NONE, "", "ignore any config file").short(&['n']),
];

/// The output format flag, with the formats a command supports
/// (outputFormatFlag).
pub const fn output_format_flag(help: &'static str) -> Flag {
    Flag::new(&["output-format"], REQ, "FMT", help).short(&['O'])
}

/// The output file flag (outputFileFlag).
pub const OUTPUT_FILE_FLAG: Flag = Flag::new(
    &["output-file"],
    REQ,
    "FILE",
    "write output to FILE. A file extension matching one of the above formats selects that format.",
)
.short(&['o']);

/// The view flag (outputViewFlag).
pub const OUTPUT_VIEW_FLAG: Flag = Flag::new(
    &["view"],
    NONE,
    "",
    "write output to FILE, or to view.FMT in hledger's cache directory, and open it in the system's default viewer for its format (eg a web browser for html). The default format is html.",
);

/// The list/tree mode flags (flattreeflags), with extra help about
/// subaccount amounts for reports that show amounts.
pub const fn flat_flag(amount_help: bool) -> Flag {
    let help = if amount_help {
        "list/tree mode: show accounts as a flat list (default). Amounts exclude subaccount amounts, except where the account is depth-clipped."
    } else {
        "list/tree mode: show accounts as a flat list (default)"
    };
    Flag::new(&["flat"], NONE, "", help).short(&['l'])
}

/// See [`flat_flag`].
pub const fn tree_flag(amount_help: bool) -> Flag {
    let help = if amount_help {
        "list/tree mode: show accounts as a tree. Amounts include subaccount amounts."
    } else {
        "list/tree mode: show accounts as a tree"
    };
    Flag::new(&["tree"], NONE, "", help).short(&['t'])
}

/// Which of the things a journal can declare (accounts, commodities,
/// payees, tags) a listing command shows, when not all of them
/// (DeclarablesSelector).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Declarables {
    Used,
    Declared,
    /// Used but not declared.
    Undeclared,
    /// Declared but not used.
    Unused,
    /// The first one the first argument matches (`--find`).
    FindFirst,
}

/// The first of these items that the first argument, as a
/// case-insensitive regular expression, matches part of, for `--find`;
/// `itemtype` names the items in the errors (findMatchedByArgument).
pub fn find_matched_by_argument<'a>(
    raw: &RawOpts,
    itemtype: &str,
    items: impl IntoIterator<Item = &'a str>,
) -> Result<&'a str, String> {
    let Some(arg) = raw.args().next() else {
        return Err(format!(
            "With --find, please provide a {itemtype} name or\n\
             {itemtype} pattern (case-insensitive, infix, regexp) as first command argument."
        ));
    };
    let re = crate::regex::to_regex_ci(arg).ok();
    re.and_then(|re| items.into_iter().find(|i| re.is_match(i)))
        .ok_or_else(|| {
            format!(
                "{} did not match any {itemtype}.",
                crate::text::show_string(arg)
            )
        })
}

/// Yes, no, or decide automatically (YNA).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Yna {
    Yes,
    No,
    Auto,
}

/// The options of a command run (CliOpts).
#[derive(Clone, Debug)]
pub struct CliOpts {
    pub raw: RawOpts,
    pub command: String,
    pub files: Vec<String>,
    pub input: InputOpts,
    pub spec: ReportSpec,
    pub output_file: Option<String>,
    pub output_format: Option<String>,
    pub pager: Option<bool>,
    pub color: Option<Yna>,
    pub debug: usize,
    pub no_new_accounts: bool,
    pub width: Option<String>,
    pub available_width: usize,
    /// When the program started, nearly: when its options were made
    /// (progstarttime_).
    pub start_time: std::time::Instant,
    /// The program's name as invoked, for help.
    pub progname: String,
}

impl CliOpts {
    /// The options of a command run from its parsed command line, today's
    /// date, and whether stdout gets colour (rawOptsToCliOpts).
    pub fn from_raw_opts(raw: RawOpts, current_day: Day, color: bool) -> Result<CliOpts, String> {
        let today = match raw.string("today") {
            None => current_day,
            Some(d) => fix_smart_date_str_ef(current_day, d)
                .map(|d| d.day)
                .map_err(|_| format!("Unable to parse date \"{d}\""))?,
        };
        let ropts = ReportOpts::from_raw_opts(today, color, &raw)?;
        let input = input_opts_from_raw_opts(today, &raw, &ropts)?;
        let mut spec = ReportSpec::new(today, ropts)?;
        spec.opts.translations = std::sync::Arc::new(crate::i18n::translations_for_lang_option(
            raw.string("lang"),
            &[],
        )?);
        let yn = |name: &str| -> Result<Option<bool>, String> {
            match raw.string(name).map(str::to_lowercase).as_deref() {
                None => Ok(None),
                Some("y" | "yes" | "always") => Ok(Some(true)),
                Some("n" | "no" | "never") => Ok(Some(false)),
                Some(_) => Err(format!("{name} value should be one of y, yes, n, no")),
            }
        };
        let color_opt = match raw.string("color").map(str::to_lowercase).as_deref() {
            None => None,
            Some("y" | "yes" | "always") => Some(Yna::Yes),
            Some("n" | "no" | "never") => Some(Yna::No),
            Some("a" | "auto") => Some(Yna::Auto),
            Some(_) => {
                return Err("color value should be one of y, yes, n, no, a, auto".to_string());
            }
        };
        Ok(CliOpts {
            command: raw.string("command").unwrap_or("").to_string(),
            files: raw.list("file").map(str::to_string).collect(),
            input,
            spec,
            output_file: raw.string("output-file").map(str::to_string),
            output_format: raw.string("output-format").map(str::to_string),
            pager: yn("pager")?,
            color: color_opt,
            debug: raw.posint("debug")?.unwrap_or(0),
            no_new_accounts: raw.bool("no-new-accounts"),
            width: raw.string("width").map(str::to_string),
            // the terminal's width, or 80 (getTerminalWidth, defaultWidth)
            available_width: terminal_size::terminal_size_of(std::io::stdout())
                .map_or(80, |(w, _)| usize::from(w.0)),
            start_time: std::time::Instant::now(),
            progname: "hledger".to_string(),
            raw,
        })
    }

    /// The width of a register's lines, and of its description column if
    /// given: --width's `W` or `W,D`, or the available width
    /// (registerWidthsFromOpts). A malformed --width is an error.
    pub fn register_widths(&self) -> Result<(isize, Option<isize>), String> {
        let Some(w) = &self.width else {
            return Ok((self.available_width as isize, None));
        };
        // `read` of digits, which wraps around beyond Int's range
        let number = |p: &mut Parser| -> PResult<isize> {
            let digits = p.take_while1(Some("digit"), |c| c.is_ascii_digit())?;
            Ok(digits.bytes().fold(0isize, |n, b| {
                n.wrapping_mul(10).wrapping_add(isize::from(b - b'0'))
            }))
        };
        let mut p = Parser::new(w);
        let widths = (|| {
            let total = number(&mut p)?;
            let desc = p.optional(|p| {
                p.char(',')?;
                number(p)
            })?;
            p.eof()?;
            Ok((total, desc))
        })();
        widths.map_err(|e: ParseError| {
            usage_error(&format!(
                "could not parse width option: {}",
                e.pretty("(unknown)", w)
            ))
        })
    }

    /// The one of --used, --declared, --undeclared, --unused and --find
    /// given, if any (declarablesSelectorFromOpts).
    pub fn declarables(&self) -> Result<Option<Declarables>, String> {
        let flags = [
            ("used", Declarables::Used),
            ("declared", Declarables::Declared),
            ("undeclared", Declarables::Undeclared),
            ("unused", Declarables::Unused),
            ("find", Declarables::FindFirst),
        ];
        let mut given = flags.into_iter().filter(|(f, _)| self.raw.bool(f));
        match (given.next(), given.next()) {
            (None, _) => Ok(None),
            (Some((_, d)), None) => Ok(Some(d)),
            _ => Err(
                "please pick at most one of --used, --declared, --undeclared, --unused, --find"
                    .to_string(),
            ),
        }
    }

    /// The journal files to read: those given with -f, made absolute (but
    /// `-` for standard input), or $LEDGER_FILE, or ~/.hledger.journal
    /// (journalFilePathFromOpts).
    pub fn journal_file_paths(&self) -> Result<Vec<String>, String> {
        let paths = self.possible_journal_file_paths()?;
        if self.files.is_empty() && !std::path::Path::new(&paths[0]).exists() {
            return Err(format!(
                "data file \"{}\" was not found.\n\
                 Please create it first, eg with \"hledger add\" or a text editor.\n\
                 Or, specify an existing data file with -f or $LEDGER_FILE.\n",
                paths[0]
            ));
        }
        Ok(paths)
    }

    /// The journal files to read, as [`journal_file_paths`](Self::journal_file_paths)
    /// gives them, but the default file need not exist
    /// (journalFilePathFromOptsNoDefault, defaultJournalPath).
    pub fn possible_journal_file_paths(&self) -> Result<Vec<String>, String> {
        let cwd = std::env::current_dir()
            .map(|d| d.display().to_string())
            .unwrap_or_default();
        if !self.files.is_empty() {
            return self
                .files
                .iter()
                .map(|f| expand_path_preserving_prefix(&cwd, f))
                .collect();
        }
        let ledger_file = std::env::var("LEDGER_FILE").unwrap_or_default();
        let f = if ledger_file.is_empty() {
            let home = std::env::var("HOME").unwrap_or_default();
            format!("{home}/.hledger.journal")
        } else {
            expand_path_or_glob(&cwd, &ledger_file)?
        };
        Ok(vec![f])
    }

    /// The output format: -O's, or the -o file's extension if it is a
    /// known format, or html with --view, or txt (outputFormatFromOpts).
    pub fn output_format(&self) -> String {
        if let Some(f) = &self.output_format {
            return f.clone();
        }
        const FORMATS: [&str; 10] = [
            "txt",
            "beancount",
            "csv",
            "json",
            "html",
            "ledger",
            "sql",
            "tsv",
            "fods",
            "svg",
        ];
        if let Some(ext) = self.output_file.as_deref().and_then(file_path_extension)
            && FORMATS.contains(&ext)
        {
            return ext.to_string();
        }
        if self.raw.bool("view") {
            "html".to_string()
        } else {
            "txt".to_string()
        }
    }
}

/// A file name's extension, without the dot.
fn file_path_extension(f: &str) -> Option<&str> {
    let name = f.rsplit('/').next()?;
    name.rfind('.').map(|i| &name[i + 1..])
}

/// A possibly prefixed file path made absolute, keeping the prefix
/// (expandPathPreservingPrefix).
fn expand_path_preserving_prefix(cwd: &str, prefixed: &str) -> Result<String, String> {
    let (prefix, f) = crate::read::journal::split_reader_prefix(prefixed);
    let f = expand_path_or_glob(cwd, f)?;
    Ok(match prefix {
        Some(p) => format!("{p}:{f}"),
        None => f,
    })
}

/// A file path made absolute, with a leading `~` expanded; or the first
/// file a glob pattern matches (expandPathOrGlob). `-` is left alone.
fn expand_path_or_glob(cwd: &str, p: &str) -> Result<String, String> {
    if p == "-" {
        return Ok(p.to_string());
    }
    let expanded = crate::read::journal::expand_home_path(p)?;
    if p.contains(['*', '?', '[', '{']) {
        let matches = crate::read::journal::glob_matches(cwd, &expanded);
        return matches
            .into_iter()
            .next()
            .ok_or_else(|| format!("glob pattern \"{p}\" matched no files"));
    }
    Ok(if expanded.starts_with('/') {
        expanded
    } else {
        format!("{cwd}/{expanded}")
    })
}

/// Input options from the parsed command line (rawOptsToInputOpts).
fn input_opts_from_raw_opts(
    today: Day,
    raw: &RawOpts,
    ropts: &ReportOpts,
) -> Result<InputOpts, String> {
    let no_infer_balancing_costs = raw.bool("strict")
        || (raw.string("args") == Some("balanced") && raw.string("command") == Some("check"));
    // the report period's dates, from the flags and the query arguments
    let mut date_queries = vec![ropts.query_from_flags()];
    for a in &ropts.query_string {
        if let Ok((q, _)) = crate::query::parse_query_term(today, a) {
            date_queries.push(q);
        }
    }
    let date_query = crate::query::Query::And(date_queries)
        .filter(|q| q.is_date())
        .simplify();
    let report_span = DateSpan::exact(date_query.start_date(false), date_query.end_date(false));
    let mut txn_tolerance = None;
    for (n, v) in &raw.0 {
        match n.as_str() {
            "txn-tolerance" => txn_tolerance = Some(v.clone()),
            "txn-balancing" => {
                txn_tolerance = Some(match v.as_str() {
                    "old" => "display".to_string(),
                    "exact" => "entry".to_string(),
                    s => s.to_string(),
                })
            }
            _ => {}
        }
    }
    let txn_balancing_display = match txn_tolerance.as_deref() {
        None | Some("entry") => false,
        Some("display") => true,
        Some(s) => {
            return Err(format!(
                "could not parse --txn-tolerance: '{s}', should be one of: entry, display"
            ));
        }
    };
    let mut styles = CommodityStyles::new();
    for s in raw.list("commodity-style") {
        let a = crate::read::amount::parse_amount(s)
            .map_err(|_| format!("could not parse --commodity-style: '{s}'"))?;
        styles.insert(a.commodity, a.style);
    }
    Ok(InputOpts {
        rules_file: raw.string("rules").map(str::to_string),
        aliases: raw.list("alias").map(str::to_string).collect(),
        anon: raw.bool("obfuscate"),
        new: raw.bool("new"),
        pivot: raw.string("pivot").unwrap_or("").to_string(),
        forecast: forecast_period_from_raw_opts(today, raw)?,
        verbose_tags: raw.bool("verbose-tags"),
        report_span,
        auto: raw.bool("auto"),
        infer_equity: raw.bool("infer-equity") && ropts.conversion_op != Some(ConversionOp::ToCost),
        infer_costs: raw.bool("infer-costs"),
        ignore_lots: raw.bool("ignore-lots"),
        balancing: BalancingOpts {
            ignore_assertions: raw.bool("ignore-assertions"),
            infer_balancing_costs: !no_infer_balancing_costs,
            txn_balancing_display,
            infer_imbalance: raw.bool("infer-imbalance"),
            commodity_styles: Some(styles),
            ..BalancingOpts::default()
        },
        strict: raw.bool("strict"),
        today,
        old_timeclock: raw.bool("oldtimeclock"),
        ..InputOpts::default()
    })
}

/// The forecast period of --forecast, if given: its span, or all time if
/// it has no argument (forecastPeriodFromRawOpts).
fn forecast_period_from_raw_opts(today: Day, raw: &RawOpts) -> Result<Option<DateSpan>, String> {
    let Some(arg) = raw.string("forecast") else {
        return Ok(None);
    };
    if arg.is_empty() {
        return Ok(Some(DateSpan::default()));
    }
    let (interval, span) = parse_period_expr(today, crate::text::strip_quotes(arg))
        .map_err(|e| usage_error(&format!("could not parse forecast period: {e}")))?;
    match interval {
        Interval::NoInterval => Ok(Some(span)),
        i => Err(usage_error(&format!(
            "--forecast's argument should not contain a report interval ({i:?} in \"{arg}\")"
        ))),
    }
}
