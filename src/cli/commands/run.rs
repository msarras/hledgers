//! The run and repl commands (Hledger.Cli.Commands.Run): hledger commands
//! run one after another, from the command line, files, standard input or
//! an interactive prompt, reading each journal once.

use std::cell::{OnceCell, RefCell};
use std::io::{BufRead, IsTerminal, Write};
use std::time::SystemTime;

use crate::cli::args::{self, Arity, Flag, Mode, RawOpts};
use crate::cli::conf::{self, ResolvedCommand};
use crate::cli::opts::{CliOpts, OUTPUT_FILE_FLAG, OUTPUT_VIEW_FLAG, output_format_flag};
use crate::cli::{commands, docs, io};
use crate::model::Journal;
use crate::read::InputOpts;
use crate::text::{quote_for_command_line, strip};

/// The hidden flag marking run's inline commands: put before the first
/// `--`, which the argument parser consumes (runCommandsMarker).
const RUN_COMMANDS_MARKER: &str = "_runcommands";

pub static RUN_MODE: Mode = Mode {
    name: "run",
    flags: &[
        output_format_flag("select the output format. Supported formats:\nhtml."),
        OUTPUT_FILE_FLAG,
        OUTPUT_VIEW_FLAG,
        Flag::new(&[RUN_COMMANDS_MARKER], Arity::None, "", "").hidden(),
    ],
    args: Some(
        "[COMMANDS_FILE1 COMMANDS_FILE2 ...] OR [-- command1 args... -- command2 args... -- command3 args...]",
    ),
    doc: crate::command_doc!("Run"),
    ..Mode::DEFAULT
};

pub static REPL_MODE: Mode = Mode {
    name: "repl",
    flags: &[Flag::new(
        &["no-watch"],
        Arity::None,
        "",
        "disable automatic reloading of changed input files, config aliases and addon commands",
    )],
    args: None,
    doc: crate::command_doc!("Repl"),
    ..Mode::DEFAULT
};

/// With the first `--` in these arguments, the run commands marker put
/// before it (argsMarkRunCommands).
pub fn args_mark_run_commands(args: &[String]) -> Vec<String> {
    match args.iter().position(|a| a == "--") {
        Some(i) => {
            let mut out = args[..i].to_vec();
            out.push(format!("--{RUN_COMMANDS_MARKER}"));
            out.extend_from_slice(&args[i..]);
            out
        }
        None => args.to_vec(),
    }
}

/// The names of the general flags run and repl pass on to the commands
/// they run: the input, output and help flags, except --file and the
/// flags that just show help (propagatedGeneralFlagNames).
fn propagated_flag_name(name: &str) -> bool {
    use crate::cli::opts::{HELP_FLAGS, INPUT_FLAGS, REPORT_FLAGS};
    const EXCLUDED: [&str; 12] = [
        "file",
        "f",
        "quickref",
        "help",
        "h",
        "examples",
        "info",
        "man",
        "webman",
        "version",
        "test",
        "write-lang",
    ];
    !EXCLUDED.contains(&name)
        && (INPUT_FLAGS.iter().chain(REPORT_FLAGS).chain(HELP_FLAGS))
            .any(|f| f.longs.contains(&name) || f.shorts.iter().any(|c| c.to_string() == name))
}

/// Why a sequence of commands stops: an error, or a command that ends the
/// program with this exit status.
enum Halt {
    Error(String),
    Exit(i32),
}

impl From<String> for Halt {
    fn from(e: String) -> Halt {
        Halt::Error(e)
    }
}

type R<T> = Result<T, Halt>;

/// What run and repl know from the command line's processing, and the
/// journals they have read.
pub struct Context {
    pub progname: String,
    pub prog_args: Vec<String>,
    pub color: bool,
    /// The add-on command names, rescanned by repl when PATH changes.
    pub addons: RefCell<Vec<String>>,
    /// The command aliases, the first definition of a name counting.
    pub aliases: RefCell<Vec<(String, String)>>,
    pub shell_aliases_allowed: bool,
    /// The config file, and the options to read it again with, for repl.
    pub conf: Option<(String, RawOpts)>,
    /// The journals read, by input options and file (journalCache).
    cache: RefCell<Vec<Cached>>,
    /// Standard input's text, once read (stdinCache).
    stdin: OnceCell<Result<String, String>>,
}

impl Context {
    pub fn new(
        progname: &str,
        prog_args: &[String],
        color: bool,
        addons: Vec<String>,
        aliases: Vec<(String, String)>,
        shell_aliases_allowed: bool,
        conf: Option<(String, RawOpts)>,
    ) -> Context {
        Context {
            progname: progname.to_string(),
            prog_args: prog_args.to_vec(),
            color,
            addons: RefCell::new(addons),
            aliases: RefCell::new(aliases),
            shell_aliases_allowed,
            conf,
            cache: RefCell::default(),
            stdin: OnceCell::new(),
        }
    }
}

/// A journal read by run or repl: the input options and file it was read
/// with, and the latest modification time of its files then.
struct Cached {
    key: InputOpts,
    file: String,
    journal: Journal,
    seen: SystemTime,
}

/// A journal's files, the included ones and CSV rules files too
/// (journalAllFilePaths).
fn journal_all_files(j: &Journal) -> Vec<String> {
    (j.files.iter().map(|(f, _)| f.clone()))
        .chain(j.aux_files.iter().cloned())
        .collect()
}

/// The latest modification time of these files.
fn newest_modification(files: &[String]) -> SystemTime {
    (files.iter().filter_map(|f| modification_time(f)))
        .max()
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// Run the run command, giving the program's exit status (run).
pub fn run(ctx: &Context, opts: &CliOpts) -> Result<i32, String> {
    halt_status(run_with(ctx, None, opts))
}

/// Run the repl command, giving the program's exit status (repl).
pub fn repl(ctx: &Context, opts: &CliOpts) -> Result<i32, String> {
    let session = Session::new(ctx, journal_paths_or_default(None, opts)?, opts, None);
    let watch = !opts.raw.bool("no-watch");
    halt_status(session.repl(watch, true))
}

fn halt_status(r: R<()>) -> Result<i32, String> {
    match r {
        Ok(()) => Ok(0),
        Err(Halt::Exit(code)) => Ok(code),
        Err(Halt::Error(e)) => Err(e),
    }
}

/// Whether an input file is standard input.
fn journal_from_stdin(opts: &CliOpts) -> bool {
    (opts.files.iter()).any(|f| crate::read::journal::split_reader_prefix(f).1 == "-")
}

/// The journal files a command reads: its own -f files, or else the run or
/// repl session's, or else the default journal, which need not exist
/// (journalFilePathFromOptsOrDefault).
fn journal_paths_or_default(
    defaults: Option<&[String]>,
    opts: &CliOpts,
) -> Result<Vec<String>, String> {
    match defaults {
        Some(d) if opts.files.is_empty() => Ok(d.to_vec()),
        _ => opts.possible_journal_file_paths(),
    }
}

/// Run with these options, the session's journal files being those given,
/// or by default the command line's: the inline commands after `--`, or
/// the commands read from standard input when there are no arguments and
/// it is not a terminal, or else the commands in the files named. With
/// html output, the commands' outputs are collected into one page.
fn run_with(ctx: &Context, defaults: Option<&[String]>, opts: &CliOpts) -> R<()> {
    let jpaths = journal_paths_or_default(defaults, opts)?;
    let args: Vec<String> = opts.raw.args().map(str::to_string).collect();
    let requested =
        opts.output_file.is_some() || opts.output_format.is_some() || opts.raw.bool("view");
    let collecting = requested && opts.output_format() == "html";
    if requested && !collecting {
        return Err(
            "run can write only html output; use -O html, -o FILE.html or --view"
                .to_string()
                .into(),
        );
    }
    let sections = RefCell::new(Vec::new());
    let session = Session::new(ctx, jpaths, opts, collecting.then_some(&sections));
    if opts.raw.bool(RUN_COMMANDS_MARKER) {
        for cmd in split_at_element("--", &args) {
            session.run_command(cmd)?;
        }
    } else if args.is_empty() && !std::io::stdin().is_terminal() {
        opts.journal_file_paths()?;
        if journal_from_stdin(opts) {
            return Err(
                "'run' can't read commands from stdin, as one of the input files was stdin as well"
                    .to_string()
                    .into(),
            );
        }
        session.repl(false, false)?;
    } else {
        let mut commands = Vec::new();
        for f in &args {
            commands.extend(
                crate::read::read_file_portably(f)?
                    .lines()
                    .map(str::to_string),
            );
        }
        for line in commands {
            session.run_command(parse_command(&line)?)?;
        }
    }
    if collecting {
        let tr = |s| opts.spec.opts.translations.tr(s).to_string();
        let labels = [tr("Show:"), tr("Titles"), tr("Charts"), tr("All")];
        io::write_output(opts, &run_page_html(&labels, &sections.into_inner()))?;
    }
    Ok(())
}

/// A list split at each occurrence of an element, a leading one ignored
/// (splitAtElement).
fn split_at_element(x: &str, l: &[String]) -> Vec<Vec<String>> {
    let l = match l.first() {
        None => return Vec::new(),
        Some(e) if e == x => &l[1..],
        _ => l,
    };
    let i = l.iter().position(|e| e == x).unwrap_or(l.len());
    let mut out = vec![l[..i].to_vec()];
    out.extend(split_at_element(x, &l[i..]));
    out
}

/// A command line read from a file or standard input as words, as a shell
/// splits them, without those from a word starting with `#` on; an
/// unclosed quote is an error showing where (parseCommand).
fn parse_command(line: &str) -> Result<Vec<String>, String> {
    let line = strip(line);
    match crate::text::words_quoted(line) {
        Some(ws) => Ok(ws.into_iter().take_while(|w| !w.starts_with('#')).collect()),
        None => Err(format!(
            "could not parse this command line:\n{}",
            unclosed_quote_error(line)
        )),
    }
}

/// The parse error for a line with an unclosed quote, as megaparsec's
/// errorBundlePretty shows it: at the end of the line, expecting the quote.
fn unclosed_quote_error(line: &str) -> String {
    let mut open = None;
    for c in line.chars() {
        match open {
            None if c == '\'' || c == '"' => open = Some(c),
            Some(q) if c == q => open = None,
            _ => {}
        }
    }
    let q = open.unwrap_or('\'');
    // megaparsec's columns and line display expand tabs to stops of 8
    let mut shown = String::new();
    for c in line.chars() {
        if c == '\t' {
            let n = 8 - shown.chars().count() % 8;
            shown.push_str(&" ".repeat(n));
        } else {
            shown.push(c);
        }
    }
    let col = shown.chars().count() + 1;
    format!(
        "1:{col}:\n  |\n1 | {shown}\n  | {}^\nunexpected end of input\nexpecting '{q}'\n",
        " ".repeat(col - 1)
    )
}

/// The backslash escapes `\n`, `\t`, `\r` and `\\` interpreted, others kept
/// as written (unescape).
fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut cs = s.chars().peekable();
    while let Some(c) = cs.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match cs.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(d) => {
                out.push('\\');
                out.push(d);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Run a shell command; if it fails, end the program with its exit status
/// (runShellCommandOrExit).
fn run_shell_or_exit(cmd: &str) -> R<()> {
    let _ = std::io::stdout().flush();
    match crate::cli::run_shell(cmd)? {
        0 => Ok(()),
        code => Err(Halt::Exit(code)),
    }
}

/// A section of run's html page: a heading for an echo, or a command line
/// and its output.
type Section = (String, Option<String>);

/// A sequence of commands being run: the session's journal files, the
/// general flags passed on to each command, the -f options passed on to
/// add-ons, and the html page sections being collected, if they are.
struct Session<'a> {
    ctx: &'a Context,
    jpaths: Vec<String>,
    general: Vec<(String, String)>,
    addon_file_args: Vec<String>,
    sections: Option<&'a RefCell<Vec<Section>>>,
}

impl<'a> Session<'a> {
    fn new(
        ctx: &'a Context,
        jpaths: Vec<String>,
        opts: &CliOpts,
        sections: Option<&'a RefCell<Vec<Section>>>,
    ) -> Session<'a> {
        // generalRawOpts
        let general = (opts.raw.0.iter())
            .filter(|(n, _)| propagated_flag_name(n))
            .cloned()
            .collect();
        let addon_file_args = (opts.files.iter())
            .flat_map(|f| ["-f".to_string(), f.clone()])
            .collect();
        Session {
            ctx,
            jpaths,
            general,
            addon_file_args,
            sections,
        }
    }

    /// Run one command line (runCommand): echo's text, a shell command
    /// alias, a builtin command with the session's general flags before
    /// its own, or an add-on with the session's -f options.
    fn run_command(&self, cmdline: Vec<String>) -> R<()> {
        let Some((cmdname0, args0)) = cmdline.split_first() else {
            return Ok(());
        };
        if cmdname0 == "echo" {
            let text = unescape(&args0.join(" "));
            match self.sections {
                Some(s) => s.borrow_mut().push((text, None)),
                None => io::print_stdout(&format!("{text}\n")),
            }
            return Ok(());
        }
        let resolved = conf::expand_command_alias(
            |n| commands::find(n).is_some(),
            &self.ctx.aliases.borrow(),
            cmdname0,
        );
        let (cmdname, defargs) = match resolved {
            ResolvedCommand::Shell(shcmd) => {
                if !self.ctx.shell_aliases_allowed {
                    return Err(format!(
                        "the command alias '{cmdname0}' runs a shell command, which is only allowed from your user config file or a --conf file"
                    )
                    .into());
                }
                let mut parts = vec![shcmd];
                parts.extend(args0.iter().map(|a| quote_for_command_line(a)));
                return run_shell_or_exit(&parts.join(" "));
            }
            ResolvedCommand::Hledger(c, a) => (c, a),
        };
        let mut args = defargs;
        args.extend_from_slice(args0);
        if cmdname == "run" {
            args = args_mark_run_commands(&args);
        }
        if let Some(mode) = commands::find(&cmdname) {
            return self.run_builtin(mode, &cmdname, args, &cmdline);
        }
        if self.ctx.addons.borrow().contains(&cmdname) {
            let all: Vec<String> = (self.addon_file_args.iter().chain(&args))
                .map(|a| quote_for_command_line(a))
                .collect();
            return run_shell_or_exit(&format!("hledger-{cmdname} {}", all.join(" ")));
        }
        let aliasnote = if cmdname != *cmdname0 {
            format!(" (expanded from the {cmdname0} command alias)")
        } else {
            String::new()
        };
        let mut words = vec![cmdname];
        words.extend(args);
        Err(format!("Unrecognized command{aliasnote}: {}", words.join(" ")).into())
    }

    /// Run a builtin command with these arguments (runCommand's builtin
    /// case): its options parsed (help shown, ending the program, with
    /// -h), the session's general flags put before them; then a help or
    /// documentation flag acted on, or help and setup run without a
    /// journal, add and import with one that may not exist, and the others
    /// with the journal, read once per session for the same files and
    /// input options; run runs its commands with these files by default.
    fn run_builtin(
        &self,
        mode: &'static Mode,
        cmdname: &str,
        args: Vec<String>,
        cmdline: &[String],
    ) -> R<()> {
        let args = crate::cli::replace_numeric_flags(&args::expand_args_at(&args)?);
        let raw = args::process(mode, &args).map_err(|e| crate::console::usage_error(&e))?;
        let ctx = self.ctx;
        if raw.bool("help") {
            let opts = self.cli_opts(raw)?;
            let usage = args::mode_usage(mode, &ctx.progname);
            io::run_pager(&opts, usage.strip_suffix('\n').unwrap_or(&usage))?;
            return Err(Halt::Exit(0));
        }
        let mut all = self.general.clone();
        all.extend(raw.0);
        let opts = self.cli_opts(RawOpts(all))?;
        let raw = &opts.raw;
        let topic = Some(mode.name);
        if raw.bool("examples") {
            return Ok(docs::run_tldr_for_page(&format!("hledger-{}", mode.name))?);
        }
        if raw.bool("info") {
            return Ok(docs::run_info_for_topic("hledger", topic)?);
        }
        if raw.bool("man") {
            return Ok(docs::run_man_for_topic("hledger", topic)?);
        }
        match cmdname {
            "help" => return Ok(commands::help::help(&opts, &ctx.aliases.borrow())?),
            "setup" => return Ok(commands::setup::setup(&opts)?),
            "add" | "import" => {
                let paths = journal_paths_or_default(Some(&self.jpaths), &opts)?;
                let mut j = crate::read::read_possible_journal_files(&opts.input, &paths)?;
                crate::cli::journal_transform(&opts, &mut j)?;
                if cmdname == "add" {
                    commands::add::add(&opts, j)?;
                } else {
                    commands::import::import(&opts, j)?;
                }
                return Ok(());
            }
            _ => {}
        }
        let (mut j, jpaths) = self.journal_cached(&opts)?;
        if cmdname == "run" {
            return run_with(ctx, Some(&jpaths), &opts);
        }
        if mode.name == "get" {
            return Ok(commands::get::get(&opts, &j)?);
        }
        let Some((action, paged)) = crate::cli::journal_command(mode.name) else {
            // repl (or a command with no action) does nothing here, as upstream
            return Ok(());
        };
        match self.sections {
            None => {
                let out = action(&opts, &mut j)?;
                crate::cli::write_command_output(&opts, paged, &out)?;
            }
            Some(sections) => {
                // the output as html if the command can write that, else as text
                let html = (mode.all_flags())
                    .any(|f| f.longs.contains(&"output-format") && f.help.contains("html"));
                let mut o = opts.clone();
                o.output_file = Some("part".to_string());
                o.output_format = html.then(|| "html".to_string());
                let out = action(&o, &mut j)?;
                let label: Vec<String> =
                    cmdline.iter().map(|a| quote_for_command_line(a)).collect();
                sections.borrow_mut().push((label.join(" "), Some(out)));
            }
        }
        Ok(())
    }

    /// A command's options from its raw options, with the program's
    /// colour, arguments and name.
    fn cli_opts(&self, raw: RawOpts) -> Result<CliOpts, String> {
        let mut opts = CliOpts::from_raw_opts(raw, crate::dates::current_day(), self.ctx.color)?;
        opts.input.prog_args = self.ctx.prog_args.clone();
        opts.progname = self.ctx.progname.clone();
        Ok(opts)
    }

    /// The journal a command reads, each of its files read once per
    /// session with the same input options (forecasting aside, the report
    /// dates do not count), then combined and transformed for the command;
    /// and the files (withJournalCached, readAndCacheJournalFile).
    fn journal_cached(&self, opts: &CliOpts) -> Result<(Journal, Vec<String>), String> {
        let jpaths = journal_paths_or_default(Some(&self.jpaths), opts)?;
        let mut key = opts.input.clone();
        key.report_span = match key.forecast {
            None => Default::default(),
            Some(f) => f.valid_defaults_from(key.report_span),
        };
        let mut journals = Vec::new();
        for fp in &jpaths {
            let cached = (self.ctx.cache.borrow().iter())
                .find(|c| c.key == key && c.file == *fp)
                .map(|c| c.journal.clone());
            let j = match cached {
                Some(j) => {
                    self.debug(&format!("readAndCacheJournalFile using cache for {fp}:"));
                    j
                }
                None => {
                    self.debug(&format!(
                        "readAndCacheJournalFile reading and caching {fp}:"
                    ));
                    let stdin = if crate::read::journal::split_reader_prefix(fp).1 == "-" {
                        self.ctx
                            .stdin
                            .get_or_init(crate::read::read_stdin_portably)
                            .clone()?
                    } else {
                        String::new()
                    };
                    let j = crate::read::read_journal_file_checked(&opts.input, fp, &stdin)?;
                    let seen = newest_modification(&journal_all_files(&j));
                    self.ctx.cache.borrow_mut().push(Cached {
                        key: key.clone(),
                        file: fp.clone(),
                        journal: j.clone(),
                        seen,
                    });
                    j
                }
            };
            journals.push(j);
        }
        let mut j = (journals.into_iter())
            .reduce(crate::read::finalise::journal_concat)
            .unwrap_or_default();
        crate::cli::journal_transform(opts, &mut j)?;
        Ok((j, jpaths))
    }

    fn debug(&self, msg: &str) {
        if crate::console::debug_level(&self.ctx.prog_args) >= 1 {
            eprintln!("{msg}");
        }
    }

    /// Read commands from standard input and run them until its end or a
    /// `q`, `quit` or `exit` (runREPL): `!` and a shell command runs that,
    /// `h` is help. With `watch`, changed journal files, config file
    /// aliases and add-ons are reloaded before each command. On a terminal,
    /// when `interactive`, there is a banner and a prompt with line editing,
    /// and errors are shown without ending the session.
    fn repl(&self, watch: bool, may_be_interactive: bool) -> R<()> {
        let interactive = may_be_interactive && std::io::stdin().is_terminal();
        let ctx = self.ctx;
        let mut conf_seen = ctx
            .conf
            .as_ref()
            .and_then(|(f, _)| modification_time(f))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let mut path_seen = if may_be_interactive {
            max_search_path_mtime()
        } else {
            SystemTime::UNIX_EPOCH
        };
        let mut reload = || {
            if !watch {
                return;
            }
            self.refresh_stale_journals();
            if let Some((file, raw)) = &ctx.conf
                && let Some(mtime) = modification_time(file)
                && mtime > conf_seen
            {
                eprintln!("({})", format_args!("reloading {file}"));
                match conf::get_conf(raw, false) {
                    Ok((c, _)) => {
                        let mut aliases = c.aliases();
                        aliases.reverse();
                        *ctx.aliases.borrow_mut() = aliases;
                    }
                    Err(e) => {
                        eprintln!("could not reload {file}, keeping previous aliases:\n{e}");
                    }
                }
                conf_seen = mtime;
            }
            if may_be_interactive {
                let mtime = max_search_path_mtime();
                if mtime > path_seen {
                    path_seen = mtime;
                    let addons = crate::cli::addons::addon_command_names();
                    if addons != *ctx.addons.borrow() {
                        eprintln!("(reloaded addons)");
                    }
                    *ctx.addons.borrow_mut() = addons;
                }
            }
        };
        let mut step = |input: &str| -> R<()> {
            reload();
            match strip(input) {
                "!" => Ok(()),
                s if s.starts_with('!') => {
                    let _ = std::io::stdout().flush();
                    crate::cli::run_shell(&s[1..])?;
                    Ok(())
                }
                _ => {
                    let mut cmd = parse_command(input)?;
                    if cmd.first().is_some_and(|c| c == "h") {
                        cmd[0] = "help".to_string();
                    }
                    self.run_command(cmd)
                }
            }
        };
        if !interactive {
            let stdin = std::io::stdin();
            let mut lines = stdin.lock();
            loop {
                let mut line = String::new();
                match lines.read_line(&mut line) {
                    Ok(0) | Err(_) => return Ok(()),
                    Ok(_) => {}
                }
                let input = line.strip_suffix('\n').unwrap_or(&line);
                if matches!(input, "q" | "quit" | "exit") {
                    return Ok(());
                }
                step(input)?;
            }
        }
        let fpath = crate::read::journal::split_reader_prefix(&self.jpaths[0]).1;
        let base = std::path::Path::new(fpath)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        println!("{} {}", ctx.progname, crate::cli::version::PACKAGE_VERSION);
        println!(
            "Ready for hledger or !shell commands. For help: h or h -h. To exit: q or ctrl-d."
        );
        let prompt = format!("{base}> ");
        // haskeline's default settings complete file names
        let mut editor = rustyline::Editor::new().map_err(|e| e.to_string())?;
        editor.set_helper(Some(FileNames(
            rustyline::completion::FilenameCompleter::new(),
        )));
        loop {
            match editor.readline(&prompt) {
                Ok(input) => {
                    if matches!(input.as_str(), "q" | "quit" | "exit") {
                        return Ok(());
                    }
                    let _ = editor.add_history_entry(input.as_str());
                    match step(&input) {
                        Ok(()) | Err(Halt::Exit(_)) => {}
                        Err(Halt::Error(e)) => println!("{}", e.trim_end()),
                    }
                }
                Err(rustyline::error::ReadlineError::Interrupted) => {}
                Err(_) => return Ok(()),
            }
        }
    }

    /// Read again the cached journals whose files have changed since they
    /// were read, saying so; one that no longer reads is kept, with the
    /// error, until its files change again (refreshStaleJournals). A change
    /// is a modification time later than any the files had when read:
    /// upstream compares with the time of reading, which misses a change
    /// made within a filesystem timestamp tick of it.
    fn refresh_stale_journals(&self) {
        let n = self.ctx.cache.borrow().len();
        for i in 0..n {
            let (iopts, fp, seen, files) = {
                let c = &self.ctx.cache.borrow()[i];
                (
                    c.key.clone(),
                    c.file.clone(),
                    c.seen,
                    journal_all_files(&c.journal),
                )
            };
            let newest = newest_modification(&files);
            if newest <= seen {
                continue;
            }
            eprintln!("(reloading {fp})");
            match crate::read::read_journal_file_checked(&iopts, &fp, "") {
                Ok(j) => {
                    let seen = newest_modification(&journal_all_files(&j));
                    let c = &mut self.ctx.cache.borrow_mut()[i];
                    c.journal = j;
                    c.seen = seen;
                }
                Err(e) => {
                    eprintln!("could not reload {fp}, keeping previous version:\n{e}");
                    self.ctx.cache.borrow_mut()[i].seen = newest;
                }
            }
        }
    }
}

/// Completion of file names, as haskeline's default.
struct FileNames(rustyline::completion::FilenameCompleter);

impl rustyline::completion::Completer for FileNames {
    type Candidate = rustyline::completion::Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        ctx: &rustyline::Context<'_>,
    ) -> rustyline::Result<(usize, Vec<rustyline::completion::Pair>)> {
        self.0.complete(line, pos, ctx)
    }
}

impl rustyline::hint::Hinter for FileNames {
    type Hint = String;
}
impl rustyline::highlight::Highlighter for FileNames {}
impl rustyline::validate::Validator for FileNames {}
impl rustyline::Helper for FileNames {}

/// A file's modification time, if it is a file that exists
/// (maybeFileModificationTime).
fn modification_time(f: &str) -> Option<SystemTime> {
    std::fs::metadata(f)
        .ok()
        .filter(|m| m.is_file())
        .and_then(|m| m.modified().ok())
}

/// The latest modification time of the directories in PATH
/// (maxSearchPathMtime).
fn max_search_path_mtime() -> SystemTime {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .filter_map(|d| std::fs::metadata(d).ok())
        .filter(|m| m.is_dir())
        .filter_map(|m| m.modified().ok())
        .max()
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// run's html page (runPageHtml): a heading for each echo, and for each
/// command a folding section headed by its command line, with its html
/// output (each chart's ids made unique), its text output preformatted, or
/// "(no output)"; and links showing more or less of it.
fn run_page_html(labels: &[String; 4], sections: &[Section]) -> String {
    use crate::write::html::escape;
    let mut out = String::from(
        "<!DOCTYPE HTML>\n<html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n<title>hledger run</title>\n<style>\n",
    );
    for rule in [
        "body{margin:0 auto;max-width:980px;padding:24px 16px 48px;background:#f3f2ee;color:#0b0b0b;font:15px/1.5 system-ui,-apple-system,\"Segoe UI\",sans-serif}",
        "h2{margin:24px 0 8px;font-size:18px;font-weight:650}",
        ".hledger-run-show{margin:0 0 8px;text-align:right;font-size:13px;color:#898781}",
        ".hledger-run-show a{margin-left:10px;color:#52514e}.hledger-run-show a.current{color:#0b0b0b;font-weight:600;text-decoration:none}",
        "details.hledger-run{margin:0 0 8px;background:#fcfcfb;border:1px solid #e1e0d9;border-radius:8px}",
        "details.hledger-run>summary{padding:10px 14px;cursor:pointer;color:#52514e;font:13px/1.4 ui-monospace,SFMono-Regular,Menlo,monospace;overflow-wrap:anywhere}",
        "details.hledger-run>div{padding:8px 14px 14px;border-top:1px solid #e1e0d9;overflow-x:auto}",
        "details.hledger-run table{font-size:13px;font-variant-numeric:tabular-nums}",
        "details.hledger-run svg{max-width:100%;height:auto}",
    ] {
        out.push_str(rule);
        out.push('\n');
    }
    out.push_str("</style>\n<p class=\"hledger-run-show\">");
    out.push_str(&escape(&labels[0]));
    for (n, l) in labels[1..].iter().enumerate() {
        let current = if n == 1 { " class=\"current\"" } else { "" };
        out.push_str(&format!(
            "<a href=\"#\" data-level=\"{n}\"{current}>{}</a>",
            escape(l)
        ));
    }
    out.push_str("</p>\n<script>document.querySelectorAll('.hledger-run-show a').forEach(a => a.onclick = e => { e.preventDefault(); const level = +a.dataset.level, details = document.querySelectorAll('details.hledger-details'); if (level < 2) details.forEach(d => d.open = false); void document.body.offsetHeight; document.querySelectorAll('details.hledger-run').forEach(d => d.open = level > 0); if (level > 1) details.forEach(d => d.open = true); document.querySelectorAll('.hledger-run-show a').forEach(b => b.classList.toggle('current', b === a)); });</script>\n");
    for (n, (heading, output)) in sections.iter().enumerate() {
        match output {
            None => out.push_str(&format!("<h2>{}</h2>\n", escape(heading))),
            Some(text) => {
                out.push_str(&format!(
                    "<details class=\"hledger-run\" open=\"\"><summary>{}</summary><div>\n",
                    escape(&format!("hledger {heading}"))
                ));
                let suffix = (n + 1).to_string();
                if strip(text).is_empty() {
                    out.push_str("<p>(no output)</p>");
                } else if text
                    .trim_start_matches(crate::text::is_space)
                    .starts_with('<')
                {
                    out.push_str(
                        &text
                            .replace(
                                "hledger-chart-clip-",
                                &format!("hledger-chart-clip-{suffix}-"),
                            )
                            .replace(
                                "hledger-chart-hatch",
                                &format!("hledger-chart-hatch-{suffix}"),
                            ),
                    );
                } else {
                    out.push_str(&format!("<pre>{}</pre>", escape(text)));
                }
                out.push_str("\n</div></details>\n");
            }
        }
    }
    out.push_str("</html>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn splitting_inline_commands() {
        assert!(split_at_element("--", &[]).is_empty());
        assert_eq!(
            split_at_element("--", &strs(&["--", "a", "x", "--", "b"])),
            [strs(&["a", "x"]), strs(&["b"])]
        );
        assert_eq!(
            split_at_element("--", &strs(&["a", "--", "--", "b", "--"])),
            [strs(&["a"]), strs(&[]), strs(&["b"]), strs(&[])]
        );
    }

    #[test]
    fn command_lines() {
        assert_eq!(
            parse_command("  reg date:'1 to 15' # comment").unwrap(),
            strs(&["reg", "date:1 to 15"])
        );
        assert_eq!(
            parse_command("zzzz a \"\" b").unwrap(),
            strs(&["zzzz", "a", "", "b"])
        );
        assert_eq!(
            parse_command("bal 'f").unwrap_err(),
            "could not parse this command line:\n1:7:\n  |\n1 | bal 'f\n  |       ^\nunexpected end of input\nexpecting '''\n"
        );
    }

    #[test]
    fn echo_escapes() {
        assert_eq!(
            unescape(r"one\ntwo\tx\rback\\slash\qme\"),
            "one\ntwo\tx\rback\\slash\\qme\\"
        );
    }

    #[test]
    fn marking_inline_commands() {
        assert_eq!(
            args_mark_run_commands(&strs(&["-f", "x", "--", "bal", "--", "reg"])),
            strs(&["-f", "x", "--_runcommands", "--", "bal", "--", "reg"])
        );
        assert_eq!(args_mark_run_commands(&strs(&["a"])), strs(&["a"]));
    }
}
