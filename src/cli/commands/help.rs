//! The help command (Hledger.Cli.Commands.Help, and help and the commands
//! list in Hledger.Cli.Commands): parts of hledger's documentation.

use crate::cli::args::{Arity, Flag, General, Mode};
use crate::cli::docs::{self, TopicResolution};
use crate::cli::io::run_pager;
use crate::cli::opts::CliOpts;

const NONE: Arity = Arity::None;

pub static MODE: Mode = Mode {
    name: "help",
    aliases: &["h"],
    flags: &[
        // the help-* names avoid a clash with the --info and --man flags
        Flag::new(&[], NONE, "", "use info when showing the manual")
            .short(&['i'])
            .sets(&["help-i"]),
        Flag::new(&[], NONE, "", "use man when showing the manual")
            .short(&['m'])
            .sets(&["help-m"]),
        Flag::new(
            &[],
            NONE,
            "",
            "use less (or $PAGER) when showing the manual",
        )
        .short(&['p'])
        .sets(&["help-p"]),
        Flag::new(&[], NONE, "", "use a web browser when showing the manual")
            .short(&['w'])
            .sets(&["help-w"]),
        Flag::new(&[], NONE, "", "just list the manual topics matching TOPIC")
            .short(&['l'])
            .sets(&["help-l"]),
        Flag::new(
            &["builtins"],
            NONE,
            "",
            "with commands: show only builtin commands",
        ),
        Flag::new(
            &["addons"],
            NONE,
            "",
            "with commands: show only addon commands",
        ),
        Flag::new(
            &["aliases"],
            NONE,
            "",
            "with commands: show only command aliases",
        ),
    ],
    general: General::Groups3,
    args: Some("[TOPIC]"),
    doc: crate::command_doc!("Help"),
    ..Mode::DEFAULT
};

/// Show the part of the documentation the first argument selects: the quick
/// reference (also with no argument), the commands list, a command's usage,
/// the manual at a topic, examples, or a hledger.org page; any other
/// argument is a manual topic. With -l, list the manual topics matching the
/// argument (after an optional `manual`) instead (help).
pub fn help(opts: &CliOpts, aliases: &[(String, String)]) -> Result<(), String> {
    let args: Vec<&str> = opts.raw.args().collect();
    if opts.raw.bool("help-l") {
        let rest = match args.as_slice() {
            ["manual", rest @ ..] => rest,
            _ => &args,
        };
        return manual(opts, rest.first().copied());
    }
    let page = |p: &str| {
        docs::open_browser_on(&format!("https://hledger.org/{p}"));
        Ok(())
    };
    match args.as_slice() {
        [] | ["quickref", ..] => run_pager(&super::quickref::render(&opts.progname)),
        ["commands", ..] => commands(opts, aliases),
        ["usage", rest @ ..] => {
            let mode = rest
                .first()
                .and_then(|c| super::find(c))
                .unwrap_or(&super::MAIN_MODE);
            run_pager(&crate::cli::args::mode_usage(mode, &opts.progname))
        }
        ["manual", rest @ ..] => manual(opts, rest.first().copied()),
        ["examples", rest @ ..] => {
            if rest.is_empty() {
                docs::run_tldr_for_page("hledger")
            } else {
                rest.iter()
                    .try_for_each(|c| docs::run_tldr_for_page(&format!("hledger-{c}")))
            }
        }
        ["home", ..] => page(""),
        ["install", ..] => page("install.html"),
        ["relnotes", ..] => page("relnotes.html"),
        ["docs", ..] => page("doc.html"),
        ["support", ..] => page("support.html"),
        ["sponsor", ..] => page("sponsor.html"),
        [topic, ..] => manual(opts, Some(topic)),
    }
}

/// Show the manual at a topic in the best viewer, or list the matching
/// topics with -l, or for an ambiguous or empty topic; a topic matching
/// nothing gets a note on stderr (manual).
fn manual(opts: &CliOpts, topic: Option<&str>) -> Result<(), String> {
    let topic = topic.unwrap_or("");
    let list_all = topic.is_empty();
    let list_topics = |heading: String, topics: Vec<(String, usize)>| {
        let mut out = format!("{heading}\n");
        for (name, level) in topics {
            let indent = if list_all { level - 1 } else { 2 };
            out.push_str(&format!("{}{name}\n", " ".repeat(indent)));
        }
        run_pager(&out)
    };
    let not_found = || {
        eprintln!(
            "\"{topic}\" does not match any manual section heading.\n\
             Run `hledger help manual` to list all topics, or `hledger help` for the quick reference."
        );
        Ok(())
    };
    let title = || title_line(opts, "HLEDGER MANUAL TOPICS");
    if opts.raw.bool("help-l") {
        return match docs::manual_topics_matching(topic) {
            ts if ts.is_empty() => not_found(),
            ts => {
                let heading = if list_all {
                    title()
                } else {
                    format!("manual topics matching \"{topic}\":")
                };
                list_topics(heading, ts)
            }
        };
    }
    match docs::resolve_manual_topic(topic) {
        TopicResolution::Found(tool, h) => {
            let at =
                (docs::manual_title(tool).as_deref() != Some(h.as_str())).then_some(h.as_str());
            show_manual_at(opts, tool, at)
        }
        TopicResolution::NotFound => not_found(),
        TopicResolution::Ambiguous(ts) => {
            let heading = if list_all {
                title()
            } else {
                format!("\"{topic}\" matches several manual sections, try a more specific topic:")
            };
            list_topics(heading, ts)
        }
    }
}

/// Show a tool's manual, at a heading if any, in the viewer the flags
/// select, or as plain text when output is not to a capable terminal, or
/// else in the first of info, man, $PAGER and less found in PATH.
fn show_manual_at(opts: &CliOpts, tool: &str, at: Option<&str>) -> Result<(), String> {
    let raw = &opts.raw;
    if raw.bool("help-w") {
        docs::open_browser_on(&docs::web_manual_url(tool, at));
        return Ok(());
    }
    if raw.bool("help-i") {
        return docs::run_info_for_topic(tool, at);
    }
    if raw.bool("help-m") {
        return docs::run_man_for_topic(tool, at);
    }
    if raw.bool("help-p") {
        return docs::run_pager_for_topic(tool, at);
    }
    let interactive = std::io::IsTerminal::is_terminal(&std::io::stdout());
    let dumb = std::env::var("TERM").is_ok_and(|t| t.to_lowercase() == "dumb");
    let emacs = std::env::var("INSIDE_EMACS").unwrap_or_default();
    let emacs_non_term = !emacs.is_empty() && !emacs.contains("term");
    if !interactive || dumb || emacs_non_term {
        return docs::print_help_for_topic(tool, at);
    }
    let exes = docs::likely_executables_in_path();
    let has = |e: &str| exes.iter().any(|x| x == e);
    let pager = std::env::var("PAGER").unwrap_or_else(|_| "less".to_string());
    if has("info") {
        docs::run_info_for_topic(tool, at)
    } else if has("man") {
        docs::run_man_for_topic(tool, at)
    } else if has(&pager) || has("less") {
        docs::run_pager_for_topic(tool, at)
    } else {
        docs::print_help_for_topic(tool, at)
    }
}

/// The builtin commands and the known add-on commands (marked +, shown only
/// when installed), with their abbreviations and descriptions, in the
/// commands list's sections (commandsListSections).
const COMMANDS_LIST_SECTIONS: [(&str, &[&str]); 5] = [
    (
        "USER INTERFACES",
        &[
            " help (h)                 show documentation and examples",
            " repl                     run multiple commands from an interactive prompt",
            " run                      run multiple commands from a file or command line",
            "+ui                       run a terminal UI",
            "+web                      run a web UI",
        ],
    ),
    (
        "ENTERING DATA",
        &[
            " add                      add transactions using terminal prompts",
            " get                      fetch transaction and price data for import",
            "+iadd                     add transactions using a TUI (hledger-iadd)",
            " import                   add new transactions from other files, eg CSV files",
        ],
    ),
    (
        "REPORTS",
        &[
            " accounts (acc)           account names",
            " activity                 transaction or posting counts, as a bar chart",
            " aregister (areg)         an account's transactions & running balance",
            " balance (bal)            balance changes, end balances, gains, budgets & more",
            " balancesheet (bs)        assets, liabilities and net worth",
            " balancesheetequity (bse) assets, liabilities and equity",
            "+bar                      simple balance/change bar charts for hledger 1",
            " cashflow (cf)            changes in liquid assets",
            " codes                    transaction codes",
            " commodities (comm)       commodity/currency symbols",
            " descriptions (desc)      transaction descriptions",
            " files                    input files used",
            " holdings                 investment holdings and performance",
            " incomestatement (is)     revenues and expenses",
            " notes                    the note part of transaction descriptions",
            " payees                   the payee part of transaction descriptions",
            "+plot                     advanced matplotlib charts, as gui/svg/png/pdf",
            " prices                   market prices",
            " print                    transaction entries, or a whole exported journal",
            " register (reg)           matched postings & running total, across accounts",
            " roi                      return on investments",
            " stats                    journal statistics",
            " tags                     tag names",
            " transactions (tx)        transactions, in compact one-line format",
        ],
    ),
    (
        "MAINTENANCE",
        &[
            " setup                    check your hledger installation",
            " check                    check that your journal is valid",
            "+check-fancyassertions    check more powerful balance assertions",
            "+check-tagfiles           check that files referenced in tag values exist",
            " diff                     compare an account's transactions in two journals",
            "+edit                     edit matched transactions with $EDITOR",
            "+git                      simple version control of the journal with git",
            "+jj                       simple version control of the journal with jj",
        ],
    ),
    (
        "GENERATING DATA",
        &[
            " close                    generate transactions to zero/restore/assert balances",
            "+interest                 generate transactions transferring accrued interest",
            " rewrite                  generate postings by rule, like print --auto",
        ],
    ),
];

/// The commands list: the categories of command that --builtins, --addons
/// and --aliases select (all with none of them): the builtin commands and
/// installed known add-ons by section, the other installed add-ons in
/// columns, and the config file's command aliases (commands, commandsList).
fn commands(opts: &CliOpts, aliases: &[(String, String)]) -> Result<(), String> {
    let raw = &opts.raw;
    let (b, a, c) = (
        raw.bool("builtins"),
        raw.bool("addons"),
        raw.bool("aliases"),
    );
    let (builtins, addons, show_aliases) = if b || a || c {
        (b, a, c)
    } else {
        (true, true, true)
    };
    let installed = if addons {
        crate::cli::addons::addon_command_names()
    } else {
        Vec::new()
    };
    let bold = |s: &str| {
        if opts.spec.opts.color {
            format!("\x1b[1m{s}\x1b[22m")
        } else {
            s.to_string()
        }
    };
    let mut out = vec![title_line(opts, "HLEDGER COMMANDS")];
    let mut known_addons = Vec::new();
    for (heading, cmds) in COMMANDS_LIST_SECTIONS {
        let mut shown = Vec::new();
        for line in cmds {
            let (prefix, cmd) = line.split_at(1);
            let name = cmd.split(' ').next().unwrap_or("");
            if prefix == "+" {
                known_addons.push(name);
            }
            let show = match prefix {
                " " => builtins,
                _ => addons && installed.iter().any(|i| i == name),
            };
            if show {
                shown.push(format!(" {cmd}"));
            }
        }
        if !shown.is_empty() {
            out.push(bold(heading));
            out.extend(shown);
        }
    }
    if addons {
        out.push(bold("OTHER ADDONS"));
        let others: Vec<String> = (installed.iter())
            .filter(|i| !known_addons.contains(&i.as_str()))
            .cloned()
            .collect();
        out.extend(
            hs_lines(&multicol(79, &others))
                .into_iter()
                .map(|l| format!(" {l}")),
        );
    }
    if show_aliases && !aliases.is_empty() {
        // each alias's last definition, in the order of their last definitions
        let mut last: Vec<&(String, String)> = Vec::new();
        for def in aliases.iter().rev() {
            if !last.iter().any(|d| d.0 == def.0) {
                last.push(def);
            }
        }
        last.reverse();
        out.push(bold("ALIASES"));
        let w = last
            .iter()
            .map(|(a, _)| a.chars().count())
            .max()
            .unwrap_or(0);
        for (alias, def) in last {
            let pad = " ".repeat(w - alias.chars().count());
            out.push(format!(" {alias}{pad} = {def}"));
        }
    }
    run_pager(&(out.join("\n") + "\n"))
}

/// A title heading, bold when colour is on (titleLine, without upstream's
/// colour gradient).
fn title_line(opts: &CliOpts, title: &str) -> String {
    if opts.spec.opts.color {
        format!("\x1b[1m{title}\x1b[22m")
    } else {
        title.to_string()
    }
}

/// Strings laid out in as many columns as fit in this width, filled column
/// by column (multicol).
fn multicol(width: usize, strs: &[String]) -> String {
    if strs.is_empty() {
        return String::new();
    }
    let len = |s: &str| unicode_width::UnicodeWidthStr::width(s);
    let maxwidth = strs.iter().map(|s| len(s)).max().unwrap_or(0);
    let numcols = strs.len().min(width / (maxwidth + 2)).max(1);
    let per_col = strs.len() / numcols;
    let cols: Vec<&[String]> = strs.chunks(per_col.max(1)).collect();
    let widths: Vec<usize> = cols
        .iter()
        .map(|c| c.iter().map(|s| len(s)).max().unwrap_or(0))
        .collect();
    let rows = cols.iter().map(|c| c.len()).max().unwrap_or(0);
    let mut out = String::new();
    for r in 0..rows {
        let mut line = String::new();
        for (i, (col, w)) in cols.iter().zip(&widths).enumerate() {
            if i > 0 {
                line.push(' ');
            }
            let cell = col.get(r).map_or("", |s| s.as_str());
            line.push_str(cell);
            line.push_str(&" ".repeat(w - len(cell)));
        }
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// The lines of a text, as Haskell's `lines` splits them.
fn hs_lines(s: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = s.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines
}
