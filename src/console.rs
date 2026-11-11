//! Messages on stderr and whether to colour them (the parts of
//! Hledger.Utils.IO that both the reader and the commands use: printError,
//! warnIO, useColorOnStderr, useColorOnStdout, colorOption and usageError),
//! and the --debug level (Hledger.Utils.Debug's debugLevel).

use std::io::IsTerminal;

/// Print an error message on stderr as upstream's top-level handler does
/// (handleExit and printError): without trailing white space, after the
/// program name and `Error: ` (unless it starts with that already), the
/// first line in bright bold red if colour is on for stderr.
pub fn print_error(progname: &str, color_option: Option<&str>, msg: &str) {
    let msg = msg.trim_end();
    let prefix = if msg.starts_with("Error:") {
        ""
    } else {
        "Error: "
    };
    let mut text = format!("{progname}: {prefix}{msg}");
    if use_color_on_stderr(color_option) {
        let first_end = text.find('\n').unwrap_or(text.len());
        text.insert_str(first_end, "\x1b[39m\x1b[49m\x1b[22m");
        text.insert_str(0, "\x1b[91m\x1b[1m");
    }
    eprintln!("{text}");
}

/// The program's name, from the path it was run by (getProgName).
pub fn progname(argv0: &str) -> String {
    let name = argv0
        .rsplit(std::path::is_separator)
        .next()
        .unwrap_or_default();
    if cfg!(windows) {
        name.strip_suffix(".exe").unwrap_or(name)
    } else {
        name
    }
    .to_string()
}

/// Exit with an error upstream raises as an exception none of its
/// handlers catch, as GHC's runtime reports it: after the program name,
/// without `Error: ` or colour, then two empty lines (for its empty
/// exception context).
pub fn uncaught(msg: &str) -> ! {
    let argv0 = std::env::args_os().next().unwrap_or_default();
    let _ = std::io::Write::flush(&mut std::io::stdout());
    eprint!("{}: {msg}\n\n\n", progname(&argv0.to_string_lossy()));
    std::process::exit(1)
}

/// Print a warning on stderr, after `Warning: `, its first line in bright
/// bold yellow if colour is on for stderr (warnIO, formatWarning). The
/// command-line arguments can turn colour on or off.
pub fn warn(args: &[String], msg: &str) {
    let mut text = format!("Warning: {msg}");
    if use_color_on_stderr(color_option(args)) {
        let first_end = text.find('\n').unwrap_or(text.len());
        text.insert_str(first_end, "\x1b[39m\x1b[49m\x1b[22m");
        text.insert_str(0, "\x1b[93m\x1b[1m");
    }
    eprintln!("{text}");
}

/// Whether to colour stderr: yes if --color says so, otherwise if it is a
/// terminal supporting colour and NO_COLOR is not set (useColorOnStderr).
pub fn use_color_on_stderr(color_option: Option<&str>) -> bool {
    use_color_on(std::io::stderr().is_terminal(), color_option)
}

/// Whether to colour stdout, as for stderr (useColorOnStdout, without its
/// check for an output file).
pub fn use_color_on_stdout(color_option: Option<&str>) -> bool {
    use_color_on(std::io::stdout().is_terminal(), color_option)
}

/// Whether to colour output to a terminal or not (useColorOnHandle).
fn use_color_on(terminal: bool, color_option: Option<&str>) -> bool {
    match color_option.map(str::to_lowercase).as_deref() {
        Some("y" | "yes" | "always") => return true,
        Some("n" | "no" | "never") => return false,
        _ => {}
    }
    let no_color = std::env::var_os("NO_COLOR").is_some();
    let dumb = std::env::var("TERM").is_ok_and(|t| t.to_lowercase() == "dumb");
    !no_color && !dumb && terminal
}

/// The value of the last --color or --colour option in some arguments
/// (colorOption's lookup).
pub fn color_option(args: &[String]) -> Option<&str> {
    let mut found = None;
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        if a == "--" {
            break;
        }
        for name in ["--color", "--colour"] {
            if let Some(v) = a.strip_prefix(name).and_then(|r| r.strip_prefix('=')) {
                found = Some(v);
            } else if a == name {
                found = it.peek().map(|s| s.as_str());
            }
        }
    }
    found
}

/// The --color or --colour flag that ends the command line without a value,
/// if any: upstream fails to find the colour option then (getOpt, with
/// splitFlagsAndVals).
pub fn color_flag_without_value(args: &[String]) -> Option<&'static str> {
    let split: Vec<&str> = args
        .iter()
        .flat_map(|a| {
            if a.starts_with("--") && a.contains('=') {
                let (x, y) = a.split_once('=').unwrap_or((a, ""));
                vec![x, y]
            } else if a.len() > 2 && a.starts_with('-') && !a[1..].starts_with('-') {
                let c = a.char_indices().nth(2).map_or(a.len(), |(i, _)| i);
                vec![&a[..c], &a[c..]]
            } else {
                vec![a.as_str()]
            }
        })
        .collect();
    match split
        .iter()
        .rposition(|a| *a == "--color" || *a == "--colour")
    {
        Some(i) if i + 1 == split.len() => Some(if split[i] == "--color" {
            "--color"
        } else {
            "--colour"
        }),
        _ => None,
    }
}

/// An error about a command-line option, with the hint upstream adds
/// (usageError).
pub fn usage_error(msg: &str) -> String {
    let msg = msg.trim_end_matches('\n');
    let sep = if msg.contains('\n') { "\n" } else { " " };
    format!("{msg}{sep}(use -h to see usage)")
}

/// The --debug level the command line asks for: the value of the last
/// --debug, 1 if it has none, or 0 (debugLevel).
pub fn debug_level(args: &[String]) -> usize {
    let mut level = 0;
    for (i, a) in args.iter().enumerate() {
        if a == "--debug" {
            level = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(1);
        } else if let Some(v) = a.strip_prefix("--debug=") {
            level = v.parse().unwrap_or(1);
        }
    }
    level
}
