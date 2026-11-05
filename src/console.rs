//! Messages on stderr and whether to colour them (the parts of
//! Hledger.Utils.IO that both the reader and the commands use: printError,
//! warnIO, useColorOnStderr and colorOption).

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
        text.insert_str(first_end, "\x1b[0m");
        text.insert_str(0, "\x1b[91m\x1b[1m");
    }
    eprintln!("{text}");
}

/// Print a warning on stderr, after `Warning: `, its first line in bright
/// bold yellow if colour is on for stderr (warnIO, formatWarning). The
/// command-line arguments can turn colour on or off.
pub fn warn(args: &[String], msg: &str) {
    let mut text = format!("Warning: {msg}");
    if use_color_on_stderr(color_option(args)) {
        let first_end = text.find('\n').unwrap_or(text.len());
        text.insert_str(first_end, "\x1b[0m");
        text.insert_str(0, "\x1b[93m\x1b[1m");
    }
    eprintln!("{text}");
}

/// Whether to colour stderr: yes if --color says so, otherwise if it is a
/// terminal supporting colour and NO_COLOR is not set (useColorOnStderr).
fn use_color_on_stderr(color_option: Option<&str>) -> bool {
    match color_option.map(str::to_lowercase).as_deref() {
        Some("y" | "yes" | "always") => return true,
        Some("n" | "no" | "never") => return false,
        _ => {}
    }
    let no_color = std::env::var_os("NO_COLOR").is_some();
    let dumb = std::env::var("TERM").is_ok_and(|t| t.to_lowercase() == "dumb");
    !no_color && !dumb && std::io::stderr().is_terminal()
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
