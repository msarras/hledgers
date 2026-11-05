//! The notes command (Hledger.Cli.Commands.Notes): the notes of the
//! matched transactions.

use std::collections::BTreeSet;

use crate::cli::args::Mode;
use crate::cli::io::lines_with_title;
use crate::cli::opts::CliOpts;
use crate::model::{Journal, Transaction};
use crate::reports::entries::entries_report;

pub static MODE: Mode = Mode {
    name: "notes",
    aliases: &[],
    flags: &[],
};

/// Run the notes command (notes): the notes (descriptions after a `|`, or
/// whole) of the transactions the query matches, sorted, without
/// duplicates.
pub fn notes(opts: &CliOpts, j: Journal) -> Result<String, String> {
    let txns = entries_report(&opts.spec, j)?;
    let notes: BTreeSet<&str> = txns.iter().map(Transaction::note).collect();
    Ok(lines_with_title(opts, notes))
}
