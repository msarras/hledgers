//! The command-line interface: argument processing and command dispatch (Hledger.Cli).

pub mod anchor;
pub mod args;
pub mod chart;
pub mod commands;
pub mod io;
pub mod opts;
pub mod version;

use std::ffi::OsString;

use crate::amount::currency::to_currency_code;
use crate::cli::commands::compound::compound_balance_command as compound;
use crate::model::Journal;
use crate::valuation::{ConversionOp, journal_to_cost};
use opts::CliOpts;

/// Run with these command-line arguments, program name first, and return the
/// exit status. Errors print as `PROGNAME: Error: MESSAGE` on stderr, status 1.
pub fn run(args: impl Iterator<Item = OsString>) -> i32 {
    let mut args = args.map(|a| a.to_string_lossy().into_owned());
    let argv0 = args.next().unwrap_or_default();
    let progname = argv0.rsplit('/').next().unwrap_or_default().to_string();
    let args: Vec<String> = args.collect();
    match dispatch(&args) {
        Ok(()) => 0,
        Err(msg) => {
            crate::console::print_error(&progname, crate::console::color_option(&args), &msg);
            1
        }
    }
}

/// `-NUM` arguments made `--depth=NUM` (replaceNumericFlags).
fn replace_numeric_flags(args: &[String]) -> Vec<String> {
    args.iter()
        .map(|a| match a.strip_prefix('-') {
            Some(ds) if !ds.is_empty() && ds.bytes().all(|b| b.is_ascii_digit()) => {
                format!("--depth={ds}")
            }
            _ => a.clone(),
        })
        .collect()
}

/// A --debug without a numeric value made --debug=1 (ensureDebugFlagHasVal).
fn ensure_debug_flag_has_value(args: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut it = args.into_iter().peekable();
    while let Some(a) = it.next() {
        if a == "--debug" {
            match it.peek() {
                Some(v) if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) => out.push(a),
                _ => out.push("--debug=1".to_string()),
            }
        } else {
            out.push(a);
        }
    }
    out
}

fn dispatch(prog_args: &[String]) -> Result<(), String> {
    let args = ensure_debug_flag_has_value(replace_numeric_flags(prog_args));
    let cmd = args::command_index(&args);
    let mode = match cmd {
        None => &commands::MAIN_MODE,
        Some(i) => commands::find(&args[i]).ok_or_else(|| {
            format!(
                "command {} is not recognised. Run 'hledger help commands' to see a list.",
                args[i]
            )
        })?,
    };
    let rest: Vec<String> = args
        .iter()
        .enumerate()
        .filter(|&(i, _)| Some(i) != cmd)
        .map(|(_, a)| a.clone())
        .collect();
    let mut raw = args::process(mode, &rest)?;
    if mode.name == "check" {
        commands::check::validate_check_args(&raw)?;
    }
    if cmd.is_some() {
        raw.0
            .insert(0, ("command".to_string(), mode.name.to_string()));
    }
    // colour on stdout, unless writing to a file (useColorOnStdout)
    let to_file = raw.string("output-file").is_some_and(|f| f != "-");
    let color =
        !to_file && crate::console::use_color_on_stdout(crate::console::color_option(prog_args));
    let mut opts = CliOpts::from_raw_opts(raw, crate::dates::current_day(), color)?;
    opts.input.prog_args = prog_args.to_vec();
    // help and version flags first (Cli.hs 6.1-6.4.1)
    let help = || {
        let mut usage = args::mode_usage(mode);
        if color && std::io::IsTerminal::is_terminal(&std::io::stdout()) {
            usage = args::highlight_help(&usage);
        }
        io::run_pager(&usage)
    };
    let raw = &opts.raw;
    if raw.bool("quickref") {
        return io::write_output(&opts, &commands::quickref::render());
    }
    if raw.bool("help") {
        return help();
    }
    for doc in ["examples", "info", "man", "webman"] {
        if raw.bool(doc) {
            return Err(commands::not_yet(&format!("--{doc}"), "8b"));
        }
    }
    if raw.bool("version") {
        let out = format!("{}\n", version::PROGNAME_AND_VERSION);
        return io::write_output(&opts, &out);
    }
    let Some(_) = cmd else {
        return io::write_output(&opts, &commands::quickref::render());
    };
    if mode.name == "import" {
        return commands::import::import(&opts, with_possible_journal(&opts)?);
    }
    let run: fn(&CliOpts, Journal) -> Result<String, String> = match mode.name {
        "accounts" => |o, j| commands::accounts::accounts(o, &j),
        "activity" => |o, j| commands::activity::activity(o, &j),
        "aregister" => |o, j| commands::aregister::aregister(o, &j),
        "balance" => |o, j| commands::balance::balance(o, &j),
        "balancesheet" => |o, j| compound(&commands::compound::BALANCESHEET, o, &j),
        "balancesheetequity" => |o, j| compound(&commands::compound::BALANCESHEETEQUITY, o, &j),
        "cashflow" => |o, j| compound(&commands::compound::CASHFLOW, o, &j),
        "incomestatement" => |o, j| compound(&commands::compound::INCOMESTATEMENT, o, &j),
        "codes" => commands::codes::codes,
        "commodities" => |o, j| commands::commodities::commodities(o, &j),
        "descriptions" => commands::descriptions::descriptions,
        "diff" => |o, j| commands::diff::diff(o, &j),
        "notes" => commands::notes::notes,
        "payees" => |o, j| commands::payees::payees(o, &j),
        "print" => commands::print::print,
        "register" => |o, j| commands::register::register(o, &j),
        "check" => |o, j| commands::check::check(o, &j),
        "close" => |o, j| commands::close::close(o, &j),
        "files" => |o, j| commands::files::files(o, &j),
        "holdings" => |o, j| commands::holdings::holdings(o, &j),
        "prices" => |o, j| commands::prices::prices(o, &j),
        "rewrite" => commands::rewrite::rewrite,
        "roi" => |o, j| commands::roi::roi(o, &j),
        "stats" => commands::stats::stats,
        "tags" => commands::tags::tags,
        "transactions" => commands::transactions::transactions,
        name => return Err(format!("the {name} command is not implemented yet")),
    };
    let j = with_journal(&opts)?;
    let out = run(&opts, j)?;
    io::write_output(&opts, &out)
}

/// The journal the options say to read, read and transformed for the
/// command (withJournal, journalTransform), after a warning if the
/// valuation commodity can not be reached
/// (maybeWarnUnknownValuationCommodity).
fn with_journal(opts: &CliOpts) -> Result<Journal, String> {
    let paths = opts.journal_file_paths()?;
    let iopts = journal_input_opts(opts, &paths)?;
    let mut j = crate::read::read_journal_files(&iopts, &paths)?;
    journal_transform(opts, &mut j);
    maybe_warn_unknown_valuation_commodity(opts, &j);
    Ok(j)
}

/// The journal the options say to read, of which the first file need not
/// exist yet, for the commands that write to it (withPossibleJournal).
fn with_possible_journal(opts: &CliOpts) -> Result<Journal, String> {
    let paths = opts.possible_journal_file_paths()?;
    let iopts = journal_input_opts(opts, &paths)?;
    let mut j = crate::read::read_possible_journal_files(&iopts, &paths)?;
    journal_transform(opts, &mut j);
    Ok(j)
}

/// The input options for reading these journal files: the data directory
/// is beside the first.
fn journal_input_opts(opts: &CliOpts, paths: &[String]) -> Result<crate::read::InputOpts, String> {
    let mut iopts = opts.input.clone();
    iopts.journal_dir = paths.first().map(|p| {
        crate::read::rules::take_directory(crate::read::journal::split_reader_prefix(p).1)
    });
    if iopts.anon {
        return Err("--obfuscate is not supported yet (hledgers milestone 8)".to_string());
    }
    Ok(iopts)
}

/// The journal transformed for the report after reading (journalTransform):
/// pivoted with --pivot, even an empty one (maybePivot), and its lot detail
/// prepared.
fn journal_transform(opts: &CliOpts, j: &mut Journal) {
    if let Some(name) = opts.raw.string("pivot") {
        j.pivot(name);
    }
    prepare_lot_detail(opts, j);
}

/// Prepare the journal's lot detail for the report (journalPrepareLotDetail):
/// with -B, convert to cost basis, and with --gain, make lot postings' cost
/// basis their transacted cost, both before lot detail is collapsed (which
/// merges a disposal's per-lot fragments, losing their bases); then
/// collapse lot detail, unless --lots or --ignore-lots keeps it, or the
/// command (holdings) aggregates lots itself.
fn prepare_lot_detail(opts: &CliOpts, j: &mut Journal) {
    if opts.spec.opts.conversion_op == Some(ConversionOp::ToCost) {
        journal_to_cost(ConversionOp::ToCost, j);
    }
    if opts.spec.opts.balance_calc == crate::reports::opts::BalanceCalculation::Gain {
        for p in j.txns.iter_mut().flat_map(|t| &mut t.postings) {
            p.amount = p.amount.map(|a| a.clone().set_cost_to_basis());
        }
    }
    let keep_lots = opts.raw.bool("lots")
        || opts.raw.bool("ignore-lots")
        || opts.raw.string("command") == Some("holdings");
    if !keep_lots {
        crate::lots::journal_collapse_lot_detail(j);
    }
}

/// Warn if -X or --value asks for a commodity which valuation can not
/// reach: one in no P directive or cost, in which no amount is
/// denominated, and which is no commodity alias; suggesting a journal
/// commodity with the same ISO 4217 currency code, if any
/// (maybeWarnUnknownValuationCommodity).
fn maybe_warn_unknown_valuation_commodity(opts: &CliOpts, j: &Journal) {
    let Some(c) = opts.spec.opts.value.as_ref().and_then(|v| v.commodity()) else {
        return;
    };
    let is_alias = j
        .declared_commodities
        .values()
        .any(|d| d.aliases.iter().any(|(a, _)| &a.commodity == c));
    if j.commodities_from_price_directives().contains(c)
        || j.commodities_from_transactions().contains(c)
        || is_alias
    {
        return;
    }
    let code = to_currency_code(c);
    let suggestion = j
        .commodities()
        .into_iter()
        .find(|s| s != c && to_currency_code(s) == code)
        .map(|s| format!(" Did you mean \"{s}\" ?"))
        .unwrap_or_default();
    crate::console::warn(
        &opts.input.prog_args,
        &format!("no conversion prices to \"{c}\" can be found.{suggestion}"),
    );
}
