//! The command-line interface: argument processing and command dispatch (Hledger.Cli).

pub mod args;
pub mod commands;
pub mod version;

use std::ffi::OsString;
use std::io::Write;

/// Run with these command-line arguments, program name first, and return the
/// exit status. Errors print as `PROGNAME: Error: MESSAGE` on stderr, status 1.
pub fn run(args: impl Iterator<Item = OsString>) -> i32 {
    let mut args = args.map(|a| a.to_string_lossy().into_owned());
    let argv0 = args.next().unwrap_or_default();
    let progname = argv0.rsplit('/').next().unwrap_or_default().to_string();
    let args: Vec<String> = args.collect();
    match dispatch(&args) {
        Ok(output) => {
            let mut stdout = std::io::stdout().lock();
            // A reader that stops early (hledger ... | head) is not an error.
            let _ = stdout
                .write_all(output.as_bytes())
                .and_then(|()| stdout.flush());
            0
        }
        Err(msg) => {
            eprintln!("{progname}: Error: {msg}");
            1
        }
    }
}

fn dispatch(args: &[String]) -> Result<String, String> {
    let cmd = args::command_index(args);
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
    let opts = args::process(mode, &rest)?;
    if opts.bool("version") {
        return Ok(format!("{}\n", version::PROGNAME_AND_VERSION));
    }
    match cmd {
        None => Ok(commands::quickref::render()),
        Some(_) => Err(format!("the {} command is not implemented yet", mode.name)),
    }
}
