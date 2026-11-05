//! The check command (Hledger.Cli.Commands.Check): run checks on the
//! journal. The basic checks (parseable, autobalanced, assertions) run
//! whenever a journal is read.

use crate::cli::args::Mode;
use crate::cli::opts::CliOpts;
use crate::model::Journal;
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

/// Run the check command (check).
pub fn check(opts: &CliOpts, _j: &Journal) -> Result<String, String> {
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
        if c > 3 {
            // M2c: the other checks
            return Err(format!(
                "the {} check is not supported yet (hledgers milestone 2c)",
                CHECKS[c].to_lowercase()
            ));
        }
    }
    Ok(String::new())
}
