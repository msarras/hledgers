//! The command-line interface: argument processing and command dispatch (Hledger.Cli).

pub mod args;
pub mod commands;
pub mod io;
pub mod opts;
pub mod version;

use std::ffi::OsString;

use crate::dates::Day;
use crate::model::Journal;
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

/// Today's date, in the local time zone.
fn current_day() -> Day {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let offset = local_utc_offset_seconds(secs);
    Day::from_ymd(1970, 1, 1)
        .expect("a valid date")
        .add_days((secs + offset).div_euclid(86400))
}

/// The local time zone's offset from UTC at this time, in seconds, from the
/// TZ variable's simple forms or /etc/localtime; UTC if unknown.
fn local_utc_offset_seconds(_utc_secs: i64) -> i64 {
    // M8: local time zones (tz-rs, per the plan); the tests run with explicit dates or in UTC.
    0
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
    if cmd.is_some() {
        raw.0
            .insert(0, ("command".to_string(), mode.name.to_string()));
    }
    let mut opts = CliOpts::from_raw_opts(raw, current_day(), false)?;
    opts.input.prog_args = prog_args.to_vec();
    if opts.raw.bool("quickref") {
        return io::write_output(&opts, &commands::quickref::render());
    }
    if opts.raw.bool("version") {
        let out = format!("{}\n", version::PROGNAME_AND_VERSION);
        return io::write_output(&opts, &out);
    }
    let Some(_) = cmd else {
        return io::write_output(&opts, &commands::quickref::render());
    };
    let run: fn(&CliOpts, Journal) -> Result<String, String> = match mode.name {
        "print" => commands::print::print,
        "check" => |o, j| commands::check::check(o, &j),
        "files" => |o, j| commands::files::files(o, &j),
        name => return Err(format!("the {name} command is not implemented yet")),
    };
    let j = with_journal(&opts)?;
    let out = run(&opts, j)?;
    io::write_output(&opts, &out)
}

/// The journal the options say to read, read and transformed for the
/// command (withJournal, journalTransform).
fn with_journal(opts: &CliOpts) -> Result<Journal, String> {
    let paths = opts.journal_file_paths()?;
    let mut iopts = opts.input.clone();
    iopts.journal_dir = paths.first().map(|p| {
        let f = crate::read::journal::split_reader_prefix(p).1;
        std::path::Path::new(f)
            .parent()
            .map(|d| d.display().to_string())
            .unwrap_or_default()
    });
    if !iopts.pivot.is_empty() {
        return Err("--pivot is not supported yet (hledgers milestone 5)".to_string());
    }
    if iopts.anon {
        return Err("--obfuscate is not supported yet (hledgers milestone 8)".to_string());
    }
    crate::read::read_journal_files(&iopts, &paths)
}
