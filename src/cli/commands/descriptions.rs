//! The descriptions command (Hledger.Cli.Commands.Descriptions): the
//! descriptions of the matched transactions.

use std::collections::BTreeSet;

use crate::cli::args::Mode;
use crate::cli::io::lines_with_title;
use crate::cli::opts::CliOpts;
use crate::model::Journal;
use crate::reports::entries::entries_report;

pub static MODE: Mode = Mode {
    name: "descriptions",
    aliases: &["desc"],
    flags: &[],
    doc: crate::command_doc!("Descriptions"),
    ..Mode::DEFAULT
};

/// Run the descriptions command (descriptions): the descriptions of the
/// transactions the query matches, sorted, without duplicates.
pub fn descriptions(opts: &CliOpts, j: Journal) -> Result<String, String> {
    let txns = entries_report(&opts.spec, j)?;
    let descs: BTreeSet<&str> = txns.iter().map(|t| t.description.as_str()).collect();
    Ok(lines_with_title(opts, descs))
}
