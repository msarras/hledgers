//! The check command (Hledger.Cli.Commands.Check): run checks on the
//! journal. The basic checks (parseable, autobalanced, assertions) run
//! whenever a journal is read.

use crate::cli::args::Mode;
use crate::cli::opts::CliOpts;
use crate::model::Journal;
use crate::read::checks;
use crate::text::words_quoted;

pub static MODE: Mode = Mode {
    name: "check",
    aliases: &[],
    flags: &[],
};

/// The checks, in the order they run (Check).
const CHECKS: [&str; 12] = [
    "Parseable",
    "Autobalanced",
    "Assertions",
    "Balanced",
    "Commodities",
    "Accounts",
    "Lots",
    "Ordereddates",
    "Payees",
    "Tags",
    "Recentassertions",
    "Uniqueleafnames",
];

/// The check a name abbreviates, ignoring case, if any (parseCheck).
fn parse_check(s: &str) -> Option<usize> {
    let lower = s.to_lowercase();
    let mut chars = lower.chars();
    let capitalised: String = chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default();
    CHECKS.iter().position(|c| c.starts_with(&capitalised))
}

/// Reject an unknown check name while the arguments are parsed, before
/// the journal is read and can hide it with its own errors (checkmode's
/// checkArg, reported the way cmdargs reports an argument's error).
pub fn validate_check_args(raw: &crate::cli::args::RawOpts) -> Result<(), String> {
    for a in raw.args() {
        let name = words_quoted(a).and_then(|ws| ws.into_iter().next());
        if name.as_deref().and_then(parse_check).is_none() {
            return Err(format!("Unhandled argument, unknown check name: {a}"));
        }
    }
    Ok(())
}

/// Run the check command (check): the named checks, in their order. The
/// basic checks, and balanced (which forbids inferring costs), were done
/// when the journal was read.
pub fn check(opts: &CliOpts, j: &Journal) -> Result<String, String> {
    let mut checks = Vec::new();
    let mut unknown = Vec::new();
    for a in opts.raw.args() {
        let name = words_quoted(a).and_then(|ws| ws.into_iter().next());
        match name.as_deref().and_then(parse_check) {
            Some(c) => checks.push(c),
            None if name.is_none() => unknown.push(crate::text::show_string(a)),
            None => unknown.push(name.unwrap_or_default()),
        }
    }
    if !unknown.is_empty() {
        return Err(format!("These checks are unknown: {}", unknown.join(" ")));
    }
    checks.sort();
    for c in checks {
        match CHECKS[c] {
            "Accounts" => checks::journal_check_accounts(j)?,
            "Commodities" => checks::journal_check_commodities(j)?,
            "Ordereddates" => checks::journal_check_ordereddates(j)?,
            "Payees" => checks::journal_check_payees(j)?,
            "Recentassertions" => checks::journal_check_recent_assertions(j)?,
            "Tags" => checks::journal_check_tags(j)?,
            "Uniqueleafnames" => checks::journal_check_uniqueleafnames(j)?,
            // checked when reading (journalCheckLots is a no-op)
            _ => {}
        }
    }
    Ok(String::new())
}
