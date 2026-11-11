//! The command-line interface: argument processing and command dispatch (Hledger.Cli).

pub mod addons;
pub mod anchor;
pub mod anon;
pub mod args;
pub mod chart;
pub mod commands;
pub mod conf;
pub mod docs;
pub mod io;
pub mod opts;
pub mod selftest;
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
    let progname = crate::console::progname(&args.next().unwrap_or_default());
    let args: Vec<String> = args.collect();
    match dispatch(&progname, &args) {
        Ok(status) => status,
        Err(msg) => {
            // finding the colour option for the error fails when --color
            // ends the command line (colorOption, getOpt)
            if let Some(flag) = crate::console::color_flag_without_value(&args) {
                eprint!("{progname}: Error: {flag} requires a value\n\n");
                return 1;
            }
            crate::console::print_error(&progname, crate::console::color_option(&args), &msg);
            1
        }
    }
}

/// `-NUM` arguments made `--depth=NUM` (replaceNumericFlags).
pub fn replace_numeric_flags(args: &[String]) -> Vec<String> {
    args.iter()
        .map(|a| match a.strip_prefix('-') {
            Some(ds) if !ds.is_empty() && ds.bytes().all(|b| b.is_ascii_digit()) => {
                format!("--depth={ds}")
            }
            _ => a.clone(),
        })
        .collect()
}

/// The first list without one occurrence of each item of the second (\\).
fn list_difference(xs: &[String], ys: &[String]) -> Vec<String> {
    let mut out = xs.to_vec();
    for y in ys {
        if let Some(i) = out.iter().position(|x| x == y) {
            out.remove(i);
        }
    }
    out
}

/// A list of strings as --debug output shows it.
fn show_list(xs: &[String]) -> String {
    if xs.is_empty() {
        return "[]".to_string();
    }
    let items: Vec<String> = xs.iter().map(|x| format!("{x:?}")).collect();
    format!("[ {} ]", items.join(" , "))
}

/// Process the command line as hledger does (main, steps 1-6): expand
/// `@FILE` arguments and `-NUM` flags, find the command and move the flags
/// written before it after it, read the config file (unless only help
/// output is wanted and it has errors), expand a command alias, find add-on
/// commands when they can matter, combine the config file's general and
/// command arguments with the alias's and the command line's, parse that
/// final command line, and act on it. Returns the exit status.
fn dispatch(progname: &str, prog_args: &[String]) -> Result<i32, String> {
    use args::{Arity, cmdargs_parse_with, drop_unsupported_opts};
    let debug = crate::console::debug_level(prog_args) >= 1;
    let dbg = |label: &str, value: &str| {
        if debug {
            eprintln!("{label}: {value}");
        }
    };
    // 1. preliminary command line parsing
    let cliargs = replace_numeric_flags(&args::expand_args_at(prog_args)?);
    let (clicmdarg, cliargswithoutcmd, cliargswithcmdfirst) =
        args::move_flags_after_command(&cliargs);
    let cliargswithcmdfirstwithoutclispecific = args::drop_cli_specific_opts(&cliargswithcmdfirst);
    let cliargsaftercmd: Vec<String> = match cliargs.iter().position(|a| *a == clicmdarg) {
        Some(i) => cliargs[i + 1..].to_vec(),
        None => Vec::new(),
    };

    // 2. read the config file if any
    let cliconfargs = drop_unsupported_opts(&args::CONF_FLAGS_MODE, &cliargswithoutcmd);
    let cliconfrawopts =
        args::cmdargs_parse_mode("for conf options", &args::CONF_FLAGS_MODE, &cliconfargs)?;
    let (conf, conffile) = if clicmdarg == "setup" {
        (conf::Conf::default(), None)
    } else {
        match conf::get_conf(&cliconfrawopts, debug) {
            Ok(c) => c,
            Err(_) if can_skip_conf(&clicmdarg, &cliargswithoutcmd)? => {
                (conf::Conf::default(), None)
            }
            Err(e) => {
                return Err(format!(
                    "{}\n(hledger {} could not read this config file. To run without it, add -n.)",
                    e.trim_end(),
                    version::PACKAGE_VERSION
                ));
            }
        }
    };
    let shell_aliases_allowed = conf::conf_file_is_trusted(&cliconfrawopts, conffile.as_deref());

    // 3. identify a command name if possible
    let confallgenargs = replace_numeric_flags(&conf.lookup("general"));
    let confothergenargs = args::drop_cli_specific_opts(&confallgenargs);
    let confdroppedgenargs = list_difference(&confallgenargs, &confothergenargs);
    let cmdarg = clicmdarg;
    let nocmdprovided = cmdarg.is_empty();
    let mut cmdaliases = conf.aliases();
    cmdaliases.reverse();
    let aliasexpansion =
        conf::expand_command_alias(|n| commands::find(n).is_some(), &cmdaliases, &cmdarg);
    let (effectivecmdarg, aliasargs) = match &aliasexpansion {
        conf::ResolvedCommand::Hledger(c, a) => (c.clone(), replace_numeric_flags(a)),
        conf::ResolvedCommand::Shell(_) => (cmdarg.clone(), Vec::new()),
    };
    let addonsneeded = !nocmdprovided
        && commands::find(&effectivecmdarg)
            .is_none_or(|m| m.names().any(|n| n == "run" || n == "repl"));
    let addons = if addonsneeded {
        addons::addon_command_names()
    } else {
        Vec::new()
    };
    let mut args0: Vec<String> = Some(effectivecmdarg.clone())
        .filter(|c| !c.is_empty())
        .into_iter()
        .collect();
    args0.extend(cliargswithcmdfirstwithoutclispecific);
    let possiblecmdarg: Vec<String> = args0
        .into_iter()
        .find(|a| !args::is_flag_arg(a))
        .into_iter()
        .collect();
    let cmdname = cmdargs_parse_with(
        &|a| args::is_flag_arg_with(Arity::Req, a),
        "for command name",
        &addons,
        &possiblecmdarg,
    )?
    .string("command")
    .unwrap_or("")
    .to_string();
    let badcmdprovided = cmdname.is_empty() && !nocmdprovided;
    let isaddoncmd = !cmdname.is_empty() && addons.contains(&cmdname);
    let builtin = commands::find(&cmdname);
    let effectivemode = builtin.unwrap_or(&commands::MAIN_MODE);
    let is_final_req_val = |a: &str| {
        if isaddoncmd {
            args::is_general_req_val_flag_arg(a)
        } else {
            args::is_mode_req_val_flag_arg(effectivemode, a)
        }
    };
    // a shell command alias runs now, if allowed, with the arguments
    // written after its name
    if let conf::ResolvedCommand::Shell(shcmd) = &aliasexpansion {
        if !shell_aliases_allowed {
            let defined = conffile
                .as_ref()
                .map(|f| format!(",\ndefined in {f}"))
                .unwrap_or_default();
            return Err(format!(
                "the command alias '{cmdarg}' runs a shell command (! {shcmd}){defined}.\n\
                 Shell command aliases are only allowed from your user config file (~/.hledger.conf or XDG),\n\
                 or a config file given with --conf; refusing to run it from an automatically-found config file."
            ));
        }
        let mut parts = vec![shcmd.clone()];
        parts.extend(
            cliargsaftercmd
                .iter()
                .map(|a| crate::text::quote_for_command_line(a)),
        );
        return run_shell(&parts.join(" "));
    }
    if badcmdprovided {
        let aliasnote = if effectivecmdarg != cmdarg {
            format!(" (expanded from the {cmdarg} command alias)")
        } else {
            String::new()
        };
        return Err(format!(
            "command {effectivecmdarg}{aliasnote} is not recognised. Run 'hledger help commands' to see a list."
        ));
    }

    // 4. get the applicable arguments from the config file
    let supportedgenargsfromconf = if cmdname == "ui" || cmdname == "web" {
        (confothergenargs.iter())
            .filter(|a| !a.starts_with("ui") && !a.starts_with("web"))
            .cloned()
            .collect()
    } else if isaddoncmd {
        Vec::new()
    } else {
        drop_unsupported_opts(effectivemode, &confothergenargs)
    };
    let excludedgenargsfromconf = list_difference(&confothergenargs, &supportedgenargsfromconf);
    let confcmdargs0 = if cmdname.is_empty() {
        Vec::new()
    } else {
        replace_numeric_flags(&conf.lookup(&cmdname))
    };
    // an add-on's own flags are hidden from parsing behind a --
    let mut confcmdargs = confcmdargs0.clone();
    if isaddoncmd {
        confcmdargs.insert(0, "--".to_string());
    }
    if conffile.is_some() {
        if !confdroppedgenargs.is_empty() {
            dbg(
                "ignored conf-selecting flags from config file",
                &show_list(&confdroppedgenargs),
            );
        }
        dbg(
            "using general args from config file",
            &show_list(&confothergenargs),
        );
        if !excludedgenargsfromconf.is_empty() {
            dbg(
                "excluded general args from config file, not supported by this command",
                &show_list(&excludedgenargsfromconf),
            );
        }
        dbg(
            "using subcommand args from config file",
            &show_list(&confcmdargs),
        );
    }

    // 5. combine the config file's and the command line's arguments
    let mut finalargs: Vec<String> = Some(effectivecmdarg.clone())
        .filter(|c| !c.is_empty())
        .into_iter()
        .collect();
    finalargs.extend(supportedgenargsfromconf.iter().cloned());
    finalargs.extend(confcmdargs);
    finalargs.extend(aliasargs.iter().cloned());
    finalargs.extend(cliargswithoutcmd.iter().cloned());
    let mut finalargs = replace_numeric_flags(&finalargs);
    // mark run's inline commands, whose -- the parse consumes
    if cmdname == "run" {
        finalargs = commands::run::args_mark_run_commands(&finalargs);
    }
    let raw = cmdargs_parse_with(&is_final_req_val, "final command line", &addons, &finalargs)?;

    // 6. select an action and run it
    // colour on stdout, unless writing to a file (useColorOnStdout)
    let to_file = raw.string("output-file").is_some_and(|f| f != "-");
    let color =
        !to_file && crate::console::use_color_on_stdout(crate::console::color_option(prog_args));
    let mut opts = CliOpts::from_raw_opts(raw, crate::dates::current_day(), color)?;
    opts.input.prog_args = prog_args.to_vec();
    opts.progname = progname.to_string();
    let raw = &opts.raw;
    let quickref = || io::run_pager(&opts, &commands::quickref::render(progname));
    let doc_flag = ["quickref", "help", "examples", "info", "man", "webman"]
        .into_iter()
        .find(|f| raw.bool(f));
    let usage = |mode: &args::Mode| {
        let mut usage = args::mode_usage(mode, progname);
        if color && std::io::IsTerminal::is_terminal(&std::io::stdout()) {
            usage = args::highlight_help(&usage);
        }
        io::run_pager(&opts, &usage)
    };
    // 6.1 no command and a help or documentation flag
    if nocmdprovided {
        match doc_flag {
            Some("quickref") => quickref()?,
            Some("help") => usage(&commands::MAIN_MODE)?,
            Some("examples") => docs::run_tldr_for_page("hledger")?,
            Some("info") => docs::run_info_for_topic("hledger", None)?,
            Some("man") => docs::run_man_for_topic("hledger", None)?,
            Some("webman") => docs::open_browser_on(&docs::web_manual_url("hledger", None)),
            _ => {}
        }
        if doc_flag.is_some() {
            return Ok(0);
        }
    }
    // 6.2 the version
    if raw.bool("version") && !isaddoncmd && doc_flag.is_none() {
        io::print_stdout(&format!("{}\n", version::progname_and_version(progname)));
        return Ok(0);
    }
    // or --test: run the self-tests, if no command is given
    if let Some(pattern) = raw.string("test").filter(|_| !isaddoncmd) {
        if nocmdprovided {
            return Ok(selftest::run_unit_tests(pattern));
        }
        return Err(format!(
            "--test can't be used with a command ({cmdarg}); to select tests, use --test=PATTERN"
        ));
    }
    // or --write-lang: write a catalog to start translating from
    if let Some(lang) = raw.string("write-lang").filter(|_| !isaddoncmd) {
        io::print_stdout(&crate::i18n::write_lang(lang)?);
        return Ok(0);
    }
    // 6.3 no command: the quick reference
    if nocmdprovided {
        quickref()?;
        return Ok(0);
    }
    // 6.4 a builtin command
    if let Some(mode) = builtin {
        let topic = Some(mode.name);
        match doc_flag {
            Some("quickref") => quickref()?,
            Some("help") => usage(mode)?,
            Some("examples") => docs::run_tldr_for_page(&format!("hledger-{}", mode.name))?,
            Some("info") => docs::run_info_for_topic("hledger", topic)?,
            Some("man") => docs::run_man_for_topic("hledger", topic)?,
            Some("webman") => docs::open_browser_on(&docs::web_manual_url("hledger", topic)),
            _ if mode.name == "run" || mode.name == "repl" => {
                // the config file and the options to read it again with,
                // so that repl can reload aliases
                let conf_info = conffile
                    .clone()
                    .filter(|_| mode.name == "repl")
                    .map(|f| (f, cliconfrawopts.clone()));
                let ctx = commands::run::Context::new(
                    progname,
                    prog_args,
                    color,
                    addons.clone(),
                    cmdaliases.clone(),
                    shell_aliases_allowed,
                    conf_info,
                );
                return if mode.name == "run" {
                    commands::run::run(&ctx, &opts)
                } else {
                    commands::run::repl(&ctx, &opts)
                };
            }
            _ => {
                run_builtin(mode, &opts, &conf)?;
            }
        }
        return Ok(0);
    }
    // 6.5 an add-on command: run hledger-NAME with the arguments from each
    // source (the config file's general arguments and its section, the
    // alias, the command line), each source's first -- consumed, and
    // hledger's own config file flags before it dropped
    if isaddoncmd {
        let mut addonargs = Vec::new();
        for source in [
            &supportedgenargsfromconf,
            &confcmdargs0,
            &aliasargs,
            &cliargswithoutcmd,
        ] {
            let (before, after) = args::break_at_first_separator(source);
            addonargs.extend(args::drop_cli_specific_opts(&before));
            addonargs.extend(after);
        }
        let exe = format!("hledger-{cmdname}");
        let status = std::process::Command::new(&exe)
            .args(&addonargs)
            .status()
            .map_err(|e| format!("{exe}: rawSystem: {}", crate::read::io_error_text(&e)))?;
        return Ok(exit_code(status));
    }
    Err(crate::console::usage_error(&format!(
        "could not understand the arguments {finalargs:?}"
    )))
}

/// Run a shell command line with sh, returning its exit status (system).
pub fn run_shell(cmd: &str) -> Result<i32, String> {
    let status = crate::platform::shell_command(cmd)
        .status()
        .map_err(|e| format!("{cmd}: {e}"))?;
    Ok(exit_code(status))
}

/// A child process's exit status as ours.
pub fn exit_code(status: std::process::ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| 128 + crate::platform::exit_signal(status).unwrap_or(0))
}

/// Whether, when the config file can not be read, this command line can do
/// without it: when it asks for help, documentation, the version or the
/// tests, with no command or one recognised without the config file
/// (canSkipConf).
fn can_skip_conf(cmdarg: &str, args: &[String]) -> Result<bool, String> {
    let mode = &args::HELP_FLAGS_MODE;
    let raw = args::cmdargs_parse_mode(
        "for help flags",
        mode,
        &args::drop_unsupported_opts(mode, args),
    )?;
    let helpflag = raw.string("test").is_some()
        || raw.string("write-lang").is_some()
        || [
            "help", "info", "man", "webman", "examples", "quickref", "version",
        ]
        .iter()
        .any(|f| raw.bool(f));
    if !helpflag {
        return Ok(false);
    }
    if cmdarg.is_empty() {
        return Ok(true);
    }
    let addons = addons::addon_command_names();
    Ok(args::process_main(&addons, &[cmdarg.to_string()])
        .is_ok_and(|raw| raw.string("command").is_some_and(|c| !c.is_empty())))
}

/// Run a builtin command (Cli.hs 6.4.2-6.4.5): help without a journal,
/// import with a journal that may not exist yet, the others with the
/// journal read.
fn run_builtin(mode: &args::Mode, opts: &CliOpts, conf: &conf::Conf) -> Result<(), String> {
    if mode.name == "help" {
        return commands::help::help(opts, &conf.aliases());
    }
    if mode.name == "import" {
        return commands::import::import(opts, with_possible_journal(opts)?);
    }
    if mode.name == "add" {
        return commands::add::add(opts, with_possible_journal(opts)?);
    }
    if mode.name == "setup" {
        return commands::setup::setup(opts);
    }
    if mode.name == "get" {
        return commands::get::get(opts, &with_journal(opts)?);
    }
    let Some((run, paged)) = journal_command(mode.name) else {
        return Err(format!("the {} command is not implemented yet", mode.name));
    };
    let mut j = with_journal(opts)?;
    let out = run(opts, &mut j)?;
    // the process ends after the output, so freeing a large journal's
    // memory piece by piece would only waste time
    std::mem::forget(j);
    write_command_output(opts, paged, &out)
}

/// A command that reads the journal and gives its output as text. Those
/// that consume the journal take it, leaving an empty one.
pub type JournalCommand = fn(&CliOpts, &mut Journal) -> Result<String, String>;

/// The builtin command of this name that reads the journal and gives its
/// output as text, and whether that output may go through a pager, as the
/// reports upstream writes with writeOutputLazyText.
pub fn journal_command(name: &str) -> Option<(JournalCommand, bool)> {
    use commands::compound::{BALANCESHEET, BALANCESHEETEQUITY, CASHFLOW, INCOMESTATEMENT};
    use std::mem::take;
    let run: (JournalCommand, bool) = match name {
        "accounts" => (|o, j| commands::accounts::accounts(o, j), false),
        "activity" => (|o, j| commands::activity::activity(o, j), false),
        "aregister" => (|o, j| commands::aregister::aregister(o, j), true),
        "balance" => (|o, j| commands::balance::balance(o, j), true),
        "balancesheet" => (|o, j| compound(&BALANCESHEET, o, j), true),
        "balancesheetequity" => (|o, j| compound(&BALANCESHEETEQUITY, o, j), true),
        "cashflow" => (|o, j| compound(&CASHFLOW, o, j), true),
        "incomestatement" => (|o, j| compound(&INCOMESTATEMENT, o, j), true),
        "codes" => (|o, j| commands::codes::codes(o, take(j)), false),
        "commodities" => (|o, j| commands::commodities::commodities(o, j), false),
        "descriptions" => (
            |o, j| commands::descriptions::descriptions(o, take(j)),
            false,
        ),
        "diff" => (|o, j| commands::diff::diff(o, j), false),
        "notes" => (|o, j| commands::notes::notes(o, take(j)), false),
        "payees" => (|o, j| commands::payees::payees(o, j), false),
        "print" => (|o, j| commands::print::print(o, take(j)), true),
        "register" => (|o, j| commands::register::register(o, j), true),
        "check" => (|o, j| commands::check::check(o, j), false),
        "close" => (|o, j| commands::close::close(o, j), false),
        "files" => (|o, j| commands::files::files(o, j), false),
        "holdings" => (|o, j| commands::holdings::holdings(o, j), true),
        "prices" => (|o, j| commands::prices::prices(o, j), false),
        "rewrite" => (|o, j| commands::rewrite::rewrite(o, take(j)), false),
        "roi" => (|o, j| commands::roi::roi(o, j), false),
        "stats" => (|o, j| commands::stats::stats(o, take(j)), false),
        "tags" => (|o, j| commands::tags::tags(o, take(j)), false),
        "transactions" => (
            |o, j| commands::transactions::transactions(o, take(j)),
            true,
        ),
        _ => return None,
    };
    Some(run)
}

/// Write a command's output, through a pager if it may be paged.
pub fn write_command_output(opts: &CliOpts, paged: bool, out: &str) -> Result<(), String> {
    if paged {
        io::write_output_paged(opts, out)
    } else {
        io::write_output(opts, out)
    }
}

/// The journal the options say to read, read and transformed for the
/// command (withJournal, journalTransform), after a warning if the
/// valuation commodity can not be reached
/// (maybeWarnUnknownValuationCommodity).
pub fn with_journal(opts: &CliOpts) -> Result<Journal, String> {
    let paths = opts.journal_file_paths()?;
    let iopts = journal_input_opts(opts, &paths);
    let mut j = crate::read::read_journal_files(&iopts, &paths)?;
    journal_transform(opts, &mut j)?;
    maybe_warn_unknown_valuation_commodity(opts, &j);
    Ok(j)
}

/// The journal the options say to read, of which the first file need not
/// exist yet, for the commands that write to it (withPossibleJournal).
pub fn with_possible_journal(opts: &CliOpts) -> Result<Journal, String> {
    let paths = opts.possible_journal_file_paths()?;
    let iopts = journal_input_opts(opts, &paths);
    let mut j = crate::read::read_possible_journal_files(&iopts, &paths)?;
    journal_transform(opts, &mut j)?;
    Ok(j)
}

/// The input options for reading these journal files: the data directory
/// is beside the first.
pub fn journal_input_opts(opts: &CliOpts, paths: &[String]) -> crate::read::InputOpts {
    let mut iopts = opts.input.clone();
    iopts.journal_dir = paths.first().map(|p| {
        crate::read::rules::take_directory(crate::read::journal::split_reader_prefix(p).1)
    });
    iopts
}

/// The journal transformed for the report after reading (journalTransform):
/// pivoted with --pivot, even an empty one (maybePivot); an error with
/// --anon (maybeWarnAboutAnon); obfuscated with --obfuscate
/// (maybeObfuscate); and its lot detail prepared.
pub fn journal_transform(opts: &CliOpts, j: &mut Journal) -> Result<(), String> {
    if let Some(name) = opts.raw.string("pivot") {
        j.pivot(name);
    }
    if opts.raw.bool("anon") {
        return Err(
            "--anon does not give privacy, and perhaps should be avoided;\n\
                    please see https://github.com/hledgerorg/hledger/issues/2133 .\n\
                    For now it has been renamed to --obfuscate (a hidden flag)."
                .to_string(),
        );
    }
    if opts.input.anon {
        anon::anon_journal(j);
    }
    prepare_lot_detail(opts, j);
    Ok(())
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
pub fn maybe_warn_unknown_valuation_commodity(opts: &CliOpts, j: &Journal) {
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
