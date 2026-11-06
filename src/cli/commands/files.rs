//! The files command (Hledger.Cli.Commands.Files): list the input files.

use crate::cli::args::{General, Mode};
use crate::cli::opts::CliOpts;
use crate::model::Journal;
use crate::regex::to_regex;

pub static MODE: Mode = Mode {
    name: "files",
    aliases: &[],
    flags: &[],
    general: General::Groups2,
    args: Some("[REGEX]"),
    doc: crate::command_doc!("Files"),
    ..Mode::DEFAULT
};

/// Run the files command: the journal's files, or those a regular
/// expression argument matches (files).
pub fn files(opts: &CliOpts, j: &Journal) -> Result<String, String> {
    let re = opts.raw.args().next().map(to_regex).transpose()?;
    let mut out = String::new();
    for (f, _) in &j.files {
        if re.as_ref().is_none_or(|r| r.is_match(f)) {
            out.push_str(f);
            out.push('\n');
        }
    }
    Ok(out)
}
