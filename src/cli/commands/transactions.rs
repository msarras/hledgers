//! The transactions command (Hledger.Cli.Commands.Transactions): the
//! matched transactions' first lines.

use crate::cli::args::{Arity, Flag, Mode};
use crate::cli::commands::print::journal_apply_match_opt;
use crate::cli::io::with_title;
use crate::cli::opts::CliOpts;
use crate::model::Journal;
use crate::reports::entries::entries_report;
use crate::write::journal::transaction_first_line;

pub static MODE: Mode = Mode {
    name: "transactions",
    aliases: &["tx"],
    flags: &[Flag::new(
        &["match"],
        Arity::Req,
        "DESC",
        "fuzzy search for one recent transaction with description closest to DESC",
    )
    .short(&['m'])],
    doc: crate::command_doc!("Transactions"),
    ..Mode::DEFAULT
};

/// Run the transactions command (transactions): the first line (date,
/// status, code, description and same-line comment) of each transaction
/// the query matches, in date order, or of the one --match finds
/// (showTransactionOneLine).
pub fn transactions(opts: &CliOpts, j: Journal) -> Result<String, String> {
    let j = journal_apply_match_opt(opts, j)?;
    let mut out = String::new();
    for t in entries_report(&opts.spec, j)? {
        out.push_str(&transaction_first_line(&t));
        out.push('\n');
    }
    Ok(with_title(opts, out))
}
