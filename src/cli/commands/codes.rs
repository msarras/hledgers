//! The codes command (Hledger.Cli.Commands.Codes): the codes of the
//! matched transactions.

use crate::cli::args::Mode;
use crate::cli::io::lines_with_title;
use crate::cli::opts::CliOpts;
use crate::model::Journal;
use crate::reports::entries::entries_report;

pub static MODE: Mode = Mode {
    name: "codes",
    aliases: &[],
    flags: &[],
};

/// Run the codes command (codes): the codes of the transactions the query
/// matches, in date order, with duplicates; the empty ones only with
/// --empty.
pub fn codes(opts: &CliOpts, j: Journal) -> Result<String, String> {
    let txns = entries_report(&opts.spec, j)?;
    let empty = opts.spec.opts.empty;
    Ok(lines_with_title(
        opts,
        txns.iter()
            .map(|t| t.code.as_str())
            .filter(|c| empty || !c.is_empty()),
    ))
}
